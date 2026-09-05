//! Read-only current-version chunk loading. Decoded drafts are not live worlds.

pub mod chunk;
pub mod compression;
pub mod nbt_stream;
pub mod region;
pub mod registry;

use crate::runtime::{AdmissionError, ChunkDecodeOutput, ChunkReadKey, CpuPool};
use chunk::{ChunkDecodeError, DimensionHeight};
use region::{RegionError, RegionLocation, RegionReadLimits, UnavailableReason};
use registry::ChunkRegistrySnapshot;
use std::{fmt, path::PathBuf, sync::Arc};
use tokio::sync::Semaphore;

#[derive(Clone, Copy, Debug)]
pub struct StorageLimits {
    pub compressed_bytes: usize,
    pub inflated_bytes: usize,
    pub nbt_limits: crate::nbt::Limits,
    pub decoded_bytes: usize,
}

impl Default for StorageLimits {
    /// Explicit resource policy, not a claim about Vanilla's maximum valid file.
    /// Non-LZ4 jobs reserve 28 MiB plus compressed bytes: four fit a 128 MiB pool
    /// while their compressed inputs total at most 16 MiB. Larger inputs/limits
    /// reduce admission concurrency, without changing worker or view distances.
    fn default() -> Self {
        Self {
            compressed_bytes: 8 * 1024 * 1024,
            inflated_bytes: 8 * 1024 * 1024,
            nbt_limits: crate::nbt::Limits {
                vanilla_quota_bytes: usize::MAX,
                allocation_bytes: 16 * 1024 * 1024,
                max_depth: 512,
                output_bytes: 0,
            },
            decoded_bytes: 4 * 1024 * 1024,
        }
    }
}

impl StorageLimits {
    /// Worst-case simultaneous requested backing bytes for one accepted job.
    /// The returned draft later uses its measured conservative retained charge.
    pub fn job_bytes(self, compressed_len: usize) -> Result<usize, AdmissionError> {
        self.job_bytes_for(region::StreamVersion::Lz4, compressed_len)
    }

    /// Format-specific reservation; LZ4 additionally admits a separate block
    /// workspace capped at min(inflated_bytes, 32 MiB). Actual allocation can be
    /// smaller after the already-admitted compressed headers have been inspected.
    pub fn job_bytes_for(
        self,
        version: region::StreamVersion,
        compressed_len: usize,
    ) -> Result<usize, AdmissionError> {
        if !self.validate() || compressed_len > self.compressed_bytes {
            return Err(AdmissionError::InvalidInput);
        }
        compressed_len
            .checked_add(self.inflated_bytes)
            .and_then(|sum| sum.checked_add(self.nbt_limits.allocation_bytes))
            .and_then(|sum| sum.checked_add(self.decoded_bytes))
            .and_then(|sum| sum.checked_add(self.decoder_scratch_bytes(version)))
            .ok_or(AdmissionError::ByteLimit)
    }

    pub(crate) fn decoder_scratch_bytes(self, version: region::StreamVersion) -> usize {
        if matches!(version, region::StreamVersion::Lz4) {
            self.inflated_bytes.min(32 * 1024 * 1024)
        } else {
            0
        }
    }
    pub(crate) fn validate(self) -> bool {
        self.compressed_bytes != 0
            && self.inflated_bytes != 0
            && self.nbt_limits.max_depth <= 512
            && self
                .inflated_bytes
                .checked_add(self.nbt_limits.allocation_bytes)
                .and_then(|sum| sum.checked_add(self.decoded_bytes))
                .and_then(|sum| sum.checked_add(self.compressed_bytes))
                .is_some()
    }
}

pub enum ChunkReadOutcome {
    Missing,
    Unavailable(UnavailableReason),
    Decoded(ChunkDecodeOutput),
}

#[derive(Debug)]
pub enum ChunkLoadError {
    InvalidLimits,
    IoBusy,
    IoWorker,
    Region(RegionError),
    Admission(AdmissionError),
    Decode(ChunkDecodeError),
    Compression(compression::CompressionError),
    NbtStream(nbt_stream::StreamError),
    Cancelled,
    CpuWorker,
}

impl fmt::Display for ChunkLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Region(error) => write!(f, "chunk region read failed: {error}"),
            Self::Admission(error) => write!(f, "chunk admission failed: {error}"),
            Self::Decode(error) => write!(f, "chunk decode failed: {error}"),
            Self::Compression(error) => write!(f, "chunk decompression failed: {error}"),
            Self::NbtStream(error) => write!(f, "chunk NBT stream failed: {error}"),
            other => write!(f, "chunk load failed: {other:?}"),
        }
    }
}
impl std::error::Error for ChunkLoadError {}

/// I/O jobs are capped before spawn_blocking, and own their payload lease until
/// the actual read returns. Dropping an async wait cannot free in-use buffers.
pub struct ChunkStore {
    directory: Arc<PathBuf>,
    cpu: Arc<CpuPool>,
    registries: Arc<ChunkRegistrySnapshot>,
    height: DimensionHeight,
    limits: StorageLimits,
    io_slots: Arc<Semaphore>,
    #[cfg(test)]
    io_gate: Option<Arc<crate::runtime::TestGate>>,
}

enum ReadIo {
    Missing,
    Unavailable(UnavailableReason),
    Pending(crate::runtime::PendingChunkDecode),
}

impl ChunkStore {
    pub fn new(
        directory: PathBuf,
        cpu: Arc<CpuPool>,
        registries: Arc<ChunkRegistrySnapshot>,
        height: DimensionHeight,
        limits: StorageLimits,
        io_concurrency: usize,
    ) -> Result<Self, ChunkLoadError> {
        if !limits.validate() || io_concurrency == 0 || io_concurrency > Semaphore::MAX_PERMITS {
            return Err(ChunkLoadError::InvalidLimits);
        }
        Ok(Self {
            directory: Arc::new(directory),
            cpu,
            registries,
            height,
            limits,
            io_slots: Arc::new(Semaphore::new(io_concurrency)),
            #[cfg(test)]
            io_gate: None,
        })
    }

    pub async fn read(&self, key: ChunkReadKey) -> Result<ChunkReadOutcome, ChunkLoadError> {
        let io_permit = Arc::clone(&self.io_slots)
            .try_acquire_owned()
            .map_err(|_| ChunkLoadError::IoBusy)?;
        let directory = Arc::clone(&self.directory);
        let cpu = Arc::clone(&self.cpu);
        let registries = Arc::clone(&self.registries);
        let height = self.height;
        let limits = self.limits;
        #[cfg(test)]
        let io_gate = self.io_gate.clone();
        let pending = tokio::task::spawn_blocking(move || {
            let _io_permit = io_permit;
            let location = region::locate(
                &directory,
                key.chunk_x,
                key.chunk_z,
                RegionReadLimits {
                    compressed_bytes: limits.compressed_bytes,
                },
            )
            .map_err(ChunkLoadError::Region)?;
            match location {
                RegionLocation::Missing => Ok(ReadIo::Missing),
                RegionLocation::Unavailable(reason) => Ok(ReadIo::Unavailable(reason)),
                RegionLocation::Present(location) => {
                    let mut pending = cpu
                        .try_reserve_chunk_decode(
                            key,
                            location.version(),
                            location.compressed_len(),
                            registries,
                            height,
                            limits,
                        )
                        .map_err(ChunkLoadError::Admission)?;
                    #[cfg(test)]
                    if let Some(gate) = &io_gate {
                        gate.block();
                    }
                    location
                        .read_into(pending.compressed_mut())
                        .map_err(ChunkLoadError::Region)?;
                    Ok(ReadIo::Pending(pending))
                }
            }
        })
        .await
        .map_err(|_| ChunkLoadError::IoWorker)??;
        match pending {
            ReadIo::Missing => Ok(ChunkReadOutcome::Missing),
            ReadIo::Unavailable(reason) => Ok(ChunkReadOutcome::Unavailable(reason)),
            ReadIo::Pending(pending) => pending
                .submit()
                .map_err(ChunkLoadError::Admission)?
                .wait()
                .await
                .map(ChunkReadOutcome::Decoded),
        }
    }
}

#[cfg(test)]
mod tests;
