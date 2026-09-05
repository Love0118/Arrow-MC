//! Owned disk bytes enter the existing CPU pool; durable world residency has a
//! separate admission lifetime so loaded chunks do not exhaust CPU job slots.

use super::{AdmissionError, CpuPool, Job, Lease, Shared, finish_job, lock};
use crate::world::storage::chunk::{DimensionHeight, StoredChunkDraft, parse_current_chunk};
use crate::world::storage::compression::{CompressionKind, StorageDecoder, lz4_scratch_required};
use crate::world::storage::nbt_stream;
use crate::world::storage::region::StreamVersion;
use crate::world::storage::registry::ChunkRegistrySnapshot;
use crate::world::storage::{ChunkLoadError, StorageLimits};
use std::{
    fmt,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Supplied by the world owner. Disk/CPU stages preserve this identity; they do
/// not know the owner's latest epoch or whether the requested load was replaced.
pub struct ChunkReadKey {
    pub world_epoch: u64,
    pub chunk_x: i32,
    pub chunk_z: i32,
    pub generation: u64,
}

pub struct PendingChunkDecode {
    compressed: Vec<u8>,
    inflated: Vec<u8>,
    registries: Arc<ChunkRegistrySnapshot>,
    key: ChunkReadKey,
    version: StreamVersion,
    height: DimensionHeight,
    limits: StorageLimits,
    lease: Lease,
}

pub(super) struct DecodeChunk {
    pending: PendingChunkDecode,
    sender: oneshot::Sender<Result<ChunkDecodeOutput, ChunkLoadError>>,
    #[cfg(test)]
    gate: Option<Arc<super::TestGate>>,
}
pub struct ChunkDecodeTask {
    receiver: Option<oneshot::Receiver<Result<ChunkDecodeOutput, ChunkLoadError>>>,
    cancelled: bool,
}

pub struct ChunkDecodeOutput {
    draft: StoredChunkDraft,
    registries: Arc<ChunkRegistrySnapshot>,
    key: ChunkReadKey,
    _lease: Lease,
}

impl CpuPool {
    pub fn try_reserve_chunk_decode(
        &self,
        key: ChunkReadKey,
        version: StreamVersion,
        compressed_len: usize,
        registries: Arc<ChunkRegistrySnapshot>,
        height: DimensionHeight,
        limits: StorageLimits,
    ) -> Result<PendingChunkDecode, AdmissionError> {
        let bytes = limits.job_bytes_for(version, compressed_len)?;
        let lease = self.try_reserve(bytes)?;
        let mut compressed = Vec::new();
        compressed
            .try_reserve_exact(compressed_len)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        if compressed.capacity() > compressed_len {
            return Err(AdmissionError::ByteLimit);
        }
        compressed.resize(compressed_len, 0);
        let mut inflated = Vec::new();
        inflated
            .try_reserve_exact(limits.inflated_bytes)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        if inflated.capacity() > limits.inflated_bytes {
            return Err(AdmissionError::ByteLimit);
        }
        Ok(PendingChunkDecode {
            compressed,
            inflated,
            registries,
            key,
            version,
            height,
            limits,
            lease,
        })
    }
}

impl PendingChunkDecode {
    pub fn compressed_mut(&mut self) -> &mut [u8] {
        &mut self.compressed
    }
    pub fn submit(self) -> Result<ChunkDecodeTask, AdmissionError> {
        self.enqueue(
            #[cfg(test)]
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn submit_with_gate(
        self,
        gate: Arc<super::TestGate>,
    ) -> Result<ChunkDecodeTask, AdmissionError> {
        self.enqueue(Some(gate))
    }

    fn enqueue(
        self,
        #[cfg(test)] gate: Option<Arc<super::TestGate>>,
    ) -> Result<ChunkDecodeTask, AdmissionError> {
        let (sender, receiver) = oneshot::channel();
        let shared = Arc::clone(&self.lease.shared);
        {
            let mut state = lock(&shared.state);
            if state.closed {
                return Err(AdmissionError::Closed);
            }
            debug_assert!(state.queue.len() < shared.config.max_jobs);
            state.queue.push_back(Job::DecodeChunk(DecodeChunk {
                pending: self,
                sender,
                #[cfg(test)]
                gate,
            }));
            state.stats.queued += 1;
        }
        shared.work.notify_one();
        Ok(ChunkDecodeTask {
            receiver: Some(receiver),
            cancelled: false,
        })
    }
}

impl ChunkDecodeTask {
    pub async fn wait(mut self) -> Result<ChunkDecodeOutput, ChunkLoadError> {
        self.wait_mut().await
    }
    pub async fn wait_mut(&mut self) -> Result<ChunkDecodeOutput, ChunkLoadError> {
        let receiver = self.receiver.as_mut().ok_or(ChunkLoadError::Cancelled)?;
        let result = receiver.await.unwrap_or(Err(ChunkLoadError::Cancelled));
        self.receiver = None;
        if self.cancelled {
            drop(result);
            Err(ChunkLoadError::Cancelled)
        } else {
            result
        }
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
        if let Some(receiver) = &mut self.receiver {
            receiver.close();
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResidentStats {
    pub chunks: usize,
    pub used_bytes: usize,
    pub peak_bytes: usize,
}
struct ResidentState {
    max_bytes: usize,
    stats: Mutex<ResidentStats>,
}
pub struct ResidentChunkBudget {
    shared: Arc<ResidentState>,
}
struct ResidentLease {
    shared: Arc<ResidentState>,
    bytes: usize,
}
pub struct ResidentChunk {
    draft: StoredChunkDraft,
    registries: Arc<ChunkRegistrySnapshot>,
    key: ChunkReadKey,
    _lease: ResidentLease,
}

impl fmt::Debug for ResidentChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResidentChunk")
            .field("key", &self.key)
            .field("retained_bytes", &self.draft.retained_bytes())
            .field("sections", &self.draft.sections().len())
            .finish_non_exhaustive()
    }
}

/// Failed adoption leaves the original decoded result and CPU lease owned here.
pub struct AdoptionError {
    output: ChunkDecodeOutput,
}
impl AdoptionError {
    pub fn into_output(self) -> ChunkDecodeOutput {
        self.output
    }
}
impl fmt::Debug for AdoptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AdoptionError: resident byte budget exhausted")
    }
}
impl fmt::Display for AdoptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("resident chunk byte budget exhausted")
    }
}
impl std::error::Error for AdoptionError {}

impl ResidentChunkBudget {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            shared: Arc::new(ResidentState {
                max_bytes,
                stats: Mutex::new(ResidentStats::default()),
            }),
        }
    }
    pub fn stats(&self) -> ResidentStats {
        *lock(&self.shared.stats)
    }
}

impl ChunkDecodeOutput {
    pub fn key(&self) -> ChunkReadKey {
        self.key
    }
    pub fn draft(&self) -> &StoredChunkDraft {
        &self.draft
    }
    pub fn retained_bytes(&self) -> usize {
        self.draft.retained_bytes()
    }
    /// Destination admission succeeds before the original job lease is released.
    /// No public API can detach the owned draft from both accounting domains.
    /// This transfers memory ownership only. The caller must validate the key
    /// and stored position before making the resident data visible in a world.
    #[expect(
        clippy::result_large_err,
        reason = "return the admitted owned draft without allocating on failed resident admission"
    )]
    pub fn try_adopt(self, budget: &ResidentChunkBudget) -> Result<ResidentChunk, AdoptionError> {
        let bytes = self.draft.retained_bytes();
        {
            let mut stats = lock(&budget.shared.stats);
            if bytes > budget.shared.max_bytes - stats.used_bytes {
                return Err(AdoptionError { output: self });
            }
            stats.chunks += 1;
            stats.used_bytes += bytes;
            stats.peak_bytes = stats.peak_bytes.max(stats.used_bytes);
        }
        let lease = ResidentLease {
            shared: Arc::clone(&budget.shared),
            bytes,
        };
        let Self {
            draft,
            registries,
            key,
            _lease,
        } = self;
        let result = ResidentChunk {
            draft,
            registries,
            key,
            _lease: lease,
        };
        drop(_lease);
        Ok(result)
    }
}

impl ResidentChunk {
    pub fn key(&self) -> ChunkReadKey {
        self.key
    }
    pub fn draft(&self) -> &StoredChunkDraft {
        &self.draft
    }
    pub fn retained_bytes(&self) -> usize {
        self.draft.retained_bytes()
    }
    pub fn registries(&self) -> &ChunkRegistrySnapshot {
        &self.registries
    }
}
impl Drop for ResidentLease {
    fn drop(&mut self) {
        let mut stats = lock(&self.shared.stats);
        stats.chunks -= 1;
        stats.used_bytes -= self.bytes;
    }
}

pub(super) fn decode_chunk(
    job: DecodeChunk,
    decoder: &mut Option<StorageDecoder>,
    shared: &Shared,
) {
    #[cfg(test)]
    if let Some(gate) = &job.gate {
        gate.block();
    }
    let DecodeChunk {
        pending, sender, ..
    } = job;
    if sender.is_closed() {
        drop(pending);
        finish_job(shared);
        return;
    }
    let PendingChunkDecode {
        compressed,
        mut inflated,
        registries,
        key,
        version,
        height,
        limits,
        lease,
    } = pending;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let kind = match version {
            StreamVersion::Gzip => CompressionKind::Gzip,
            StreamVersion::Zlib => CompressionKind::Zlib,
            StreamVersion::Raw => CompressionKind::Raw,
            StreamVersion::Lz4 => CompressionKind::Lz4,
        };
        let mut lz4_scratch = Vec::new();
        if matches!(version, StreamVersion::Lz4) {
            let required = lz4_scratch_required(&compressed, limits.inflated_bytes);
            if required > limits.decoder_scratch_bytes(version) {
                return Err(ChunkLoadError::Admission(AdmissionError::ByteLimit));
            }
            lz4_scratch
                .try_reserve_exact(required)
                .map_err(|_| ChunkLoadError::Admission(AdmissionError::AllocationFailed))?;
            if lz4_scratch.capacity() > limits.decoder_scratch_bytes(version) {
                return Err(ChunkLoadError::Admission(AdmissionError::ByteLimit));
            }
            lz4_scratch.resize(required, 0);
        }
        let mut reader = decoder
            .get_or_insert_with(StorageDecoder::new)
            .reader(kind, &compressed, &mut lz4_scratch, limits.inflated_bytes)
            .map_err(ChunkLoadError::Compression)?;
        let (root, allocated) = nbt_stream::read_disk_compound(
            &mut reader,
            &mut inflated,
            limits.inflated_bytes,
            limits.nbt_limits,
        )
        .map_err(ChunkLoadError::NbtStream)?;
        parse_current_chunk(root, allocated, &registries, height, limits.decoded_bytes)
            .map_err(ChunkLoadError::Decode)
    }))
    .unwrap_or(Err(ChunkLoadError::CpuWorker));
    drop(compressed);
    drop(inflated);
    let completion = match result {
        Ok(draft) if !sender.is_closed() => Ok(ChunkDecodeOutput {
            draft,
            registries,
            key,
            _lease: lease,
        }),
        Ok(draft) => {
            drop(draft);
            drop(registries);
            drop(lease);
            Err(ChunkLoadError::Cancelled)
        }
        Err(error) => {
            drop(registries);
            drop(lease);
            Err(error)
        }
    };
    finish_job(shared);
    let _ = sender.send(completion);
}
