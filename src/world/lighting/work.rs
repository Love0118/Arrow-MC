//! Fresh initial relighting of one admitted immutable available-chunk domain.
//!
//! This coordinator does not restore saved light, replay ThreadedLevelLightEngine
//! callbacks, or implement world tickets. Intermediate engine publications remain
//! private until both layers converge. The caller then checks the source fence.

use std::fmt;

use super::block::{BlockLightEngine, BlockLightError, BlockLightLimits};
use super::sky::{SkyError, SkyLightEngine, SkyLimits};
use super::storage::{LightSectionStorage, LightSnapshot, StorageError, StorageLimits};
use super::{LightKind, LightSection, LightingSource, SourceStamp};
use crate::world::preparation::ChunkAddress;

#[derive(Clone, Copy, Debug)]
pub struct SkyWorkLimits {
    pub engine: SkyLimits,
    pub storage: StorageLimits,
    pub engine_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct LightingLimits {
    pub max_chunks: usize,
    /// Backing capacity of the coordinator's chunk-address cursor array.
    pub metadata_bytes: usize,
    pub block: BlockLightLimits,
    pub block_storage: StorageLimits,
    pub sky: Option<SkyWorkLimits>,
}

impl LightingLimits {
    pub fn has_sky_light(self) -> bool {
        self.sky.is_some()
    }
    /// Conservative admission BEFORE `LightingWork::new`: fixed owner body plus
    /// all configured backing allowances, including storage COW/visible maps.
    /// Source/registry/resident buffers were admitted by their producer and must
    /// retain those reservations separately throughout queued/running/completed
    /// ownership. No source palette is copied by this coordinator.
    pub fn reservation_bytes(self) -> Result<usize, LightingError> {
        if self.max_chunks == 0
            || self
                .max_chunks
                .checked_mul(size_of::<ChunkAddress>())
                .is_none()
            || self.block.checks == 0
            || self.block.decreases == 0
            || self.block.increases == 0
        {
            return Err(LightingError::InvalidLimits);
        }
        let mut bytes = size_of::<LightingWork>();
        for amount in [
            self.metadata_bytes,
            self.block.queue_bytes,
            self.block_storage.metadata_bytes,
            self.block_storage.layer_bytes,
        ] {
            bytes = bytes
                .checked_add(amount)
                .ok_or(LightingError::AllocationLimit)?;
        }
        if let Some(sky) = self.sky {
            for amount in [
                sky.engine_bytes,
                sky.storage.metadata_bytes,
                sky.storage.layer_bytes,
            ] {
                bytes = bytes
                    .checked_add(amount)
                    .ok_or(LightingError::AllocationLimit)?;
            }
        }
        Ok(bytes)
    }
}

#[derive(Debug)]
pub enum LightingError {
    InvalidLimits,
    AllocationLimit,
    AllocationFailed,
    Block(BlockLightError),
    Sky(SkyError),
    Storage(StorageError),
}

impl fmt::Display for LightingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "initial lighting work: {self:?}")
    }
}
impl std::error::Error for LightingError {}
impl From<BlockLightError> for LightingError {
    fn from(error: BlockLightError) -> Self {
        Self::Block(error)
    }
}
impl From<SkyError> for LightingError {
    fn from(error: SkyError) -> Self {
        Self::Sky(error)
    }
}
impl From<StorageError> for LightingError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkProgress {
    pub processed: usize,
    pub complete: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    SupportBlock,
    SupportSky,
    SkySources,
    SkyEnable,
    SkyPopulate,
    BlockSources,
    BlockRun,
    SkyRun,
    Complete,
}

pub struct LightingWork {
    source: LightingSource,
    addresses: Vec<ChunkAddress>,
    block: BlockLightEngine,
    block_storage: LightSectionStorage,
    sky: Option<SkyLightEngine>,
    phase: Phase,
    chunk: usize,
    section_y: i32,
    reservation_bytes: usize,
    limits: LightingLimits,
}

impl LightingWork {
    /// Adopts an already admitted source. The runtime reserves
    /// `limits.reservation_bytes()` before calling this allocating constructor.
    /// Standalone callers must provide the equivalent external admission.
    pub fn new(source: LightingSource, limits: LightingLimits) -> Result<Self, LightingError> {
        let reservation_bytes = limits.reservation_bytes()?;
        let count = source.chunk_addresses().len();
        if count > limits.max_chunks {
            return Err(LightingError::InvalidLimits);
        }
        if count
            .checked_mul(size_of::<ChunkAddress>())
            .ok_or(LightingError::AllocationLimit)?
            > limits.metadata_bytes
        {
            return Err(LightingError::AllocationLimit);
        }
        let mut addresses = Vec::new();
        addresses
            .try_reserve_exact(count)
            .map_err(|_| LightingError::AllocationFailed)?;
        if addresses
            .capacity()
            .checked_mul(size_of::<ChunkAddress>())
            .ok_or(LightingError::AllocationLimit)?
            > limits.metadata_bytes
        {
            return Err(LightingError::AllocationLimit);
        }
        addresses.extend(source.chunk_addresses());
        let block = BlockLightEngine::new(limits.block)?;
        let block_storage = LightSectionStorage::new(LightKind::Block, limits.block_storage)?;
        let sky = if let Some(limits) = limits.sky {
            let storage = LightSectionStorage::new(LightKind::Sky, limits.storage)?;
            let mut remaining = limits.engine_bytes;
            Some(SkyLightEngine::new(storage, limits.engine, &mut remaining)?)
        } else {
            None
        };
        let section_y = i32::from(source.height().min_section());
        Ok(Self {
            source,
            addresses,
            block,
            block_storage,
            sky,
            phase: Phase::SupportBlock,
            chunk: 0,
            section_y,
            reservation_bytes,
            limits,
        })
    }

    pub fn reservation_bytes(&self) -> usize {
        self.reservation_bytes
    }

    pub fn source_stamp(&self) -> SourceStamp {
        self.source.stamp()
    }

    /// Changes only queue capacities within the ORIGINAL global reservation.
    /// Failure retains pending work and any earlier successfully grown queue.
    pub fn grow_block_queues(
        &mut self,
        checks: usize,
        decreases: usize,
        increases: usize,
    ) -> Result<(), LightingError> {
        self.block.grow_queues(BlockLightLimits {
            checks,
            decreases,
            increases,
            queue_bytes: self.limits.block.queue_bytes,
        })?;
        Ok(())
    }

    pub fn grow_sky_queues(&mut self, capacity: usize) -> Result<(), LightingError> {
        let limit = self
            .limits
            .sky
            .ok_or(LightingError::InvalidLimits)?
            .engine_bytes;
        let sky = self.sky.as_mut().ok_or(LightingError::InvalidLimits)?;
        let mut remaining = limit
            .checked_sub(sky.stats().heap_bytes)
            .ok_or(LightingError::AllocationLimit)?;
        sky.grow_queues(capacity, &mut remaining)?;
        Ok(())
    }

    pub fn grow_sky_plan(&mut self, capacity: usize) -> Result<(), LightingError> {
        let limit = self
            .limits
            .sky
            .ok_or(LightingError::InvalidLimits)?
            .engine_bytes;
        let sky = self.sky.as_mut().ok_or(LightingError::InvalidLimits)?;
        let mut remaining = limit
            .checked_sub(sky.stats().heap_bytes)
            .ok_or(LightingError::AllocationLimit)?;
        sky.grow_plan(capacity, &mut remaining)?;
        Ok(())
    }

    /// Actual retained engine backing plus storage reservations; excludes the
    /// separately admitted source/registry and fixed owner body. Storage reserves
    /// full layer payload capacity even when its current representation is implicit.
    pub fn retained_bytes(&self) -> usize {
        let block = self.block_storage.stats();
        self.addresses.capacity() * size_of::<ChunkAddress>()
            + self.block.heap_bytes()
            + block.metadata_bytes
            + block.reserved_layer_bytes
            + self.sky.as_ref().map_or(0, |sky| {
                let storage = sky.storage().stats();
                sky.stats().heap_bytes + storage.metadata_bytes + storage.reserved_layer_bytes
            })
    }

    /// A unit is one section status, one whole chunk's 256-column source scan,
    /// one bounded vertical enable, one populate column, one emission enumeration,
    /// or one propagation entry. These costs differ: max_units is not a latency
    /// guarantee. Source initialization is bounded by the admitted dimension.
    /// Zero does no work. A runtime can cancel between calls by dropping this
    /// owner before releasing its lease. Errors keep every buffer and exact phase
    /// for retry; no completion or partial layer snapshot escapes.
    pub fn step(&mut self, max_units: usize) -> Result<WorkProgress, LightingError> {
        let mut processed = 0;
        while processed < max_units && self.phase != Phase::Complete {
            match self.phase {
                Phase::SupportBlock => {
                    if self.chunk == self.addresses.len() {
                        self.chunk = 0;
                        self.phase = if self.sky.is_some() {
                            Phase::SkySources
                        } else {
                            Phase::BlockSources
                        };
                        continue;
                    }
                    let section = LightSection {
                        x: self.addresses[self.chunk].x,
                        y: self.section_y,
                        z: self.addresses[self.chunk].z,
                    };
                    if !self.source.section_has_only_air(section) {
                        self.block_storage.update_section_status(section, false)?;
                        self.phase = Phase::SupportSky;
                    } else {
                        self.advance_section();
                    }
                    processed += 1;
                }
                Phase::SupportSky => {
                    if let Some(sky) = &mut self.sky {
                        sky.storage_mut()?.update_section_status(
                            LightSection {
                                x: self.addresses[self.chunk].x,
                                y: self.section_y,
                                z: self.addresses[self.chunk].z,
                            },
                            false,
                        )?;
                    }
                    self.advance_section();
                    self.phase = Phase::SupportBlock;
                    processed += 1;
                }
                Phase::SkySources => {
                    if self.chunk == self.addresses.len() {
                        self.chunk = 0;
                        self.phase = Phase::SkyEnable;
                        continue;
                    }
                    self.sky
                        .as_mut()
                        .unwrap()
                        .initialize_sources(&self.source, self.addresses[self.chunk])?;
                    self.chunk += 1;
                    processed += 1;
                }
                Phase::SkyEnable => {
                    if self.chunk == self.addresses.len() {
                        self.chunk = 0;
                        self.phase = Phase::BlockSources;
                        continue;
                    }
                    self.sky
                        .as_mut()
                        .unwrap()
                        .set_light_enabled(self.addresses[self.chunk], true)?;
                    self.phase = Phase::SkyPopulate;
                    processed += 1;
                }
                Phase::SkyPopulate => {
                    if self
                        .sky
                        .as_mut()
                        .unwrap()
                        .populate_budgeted(self.addresses[self.chunk], 1)?
                    {
                        self.chunk += 1;
                        self.phase = Phase::SkyEnable;
                    }
                    processed += 1;
                }
                Phase::BlockSources => {
                    if self.chunk == self.addresses.len() {
                        self.phase = Phase::BlockRun;
                        continue;
                    }
                    self.block.propagate_light_sources(
                        &self.source,
                        &mut self.block_storage,
                        self.addresses[self.chunk],
                    )?;
                    self.chunk += 1;
                    processed += 1;
                }
                Phase::BlockRun => {
                    let result = self.block.run(
                        &self.source,
                        &mut self.block_storage,
                        max_units - processed,
                    )?;
                    processed += result.processed;
                    if result.complete {
                        self.phase = if self.sky.is_some() {
                            Phase::SkyRun
                        } else {
                            Phase::Complete
                        };
                    } else {
                        return Ok(WorkProgress {
                            processed,
                            complete: false,
                        });
                    }
                }
                Phase::SkyRun => {
                    let result = self
                        .sky
                        .as_mut()
                        .unwrap()
                        .run_budgeted(&self.source, max_units - processed)?;
                    processed += result.processed;
                    if result.complete {
                        self.phase = Phase::Complete;
                    } else {
                        return Ok(WorkProgress {
                            processed,
                            complete: false,
                        });
                    }
                }
                Phase::Complete => unreachable!(),
            }
        }
        Ok(WorkProgress {
            processed,
            complete: self.phase == Phase::Complete,
        })
    }

    fn advance_section(&mut self) {
        if self.section_y == i32::from(self.source.height().max_section()) {
            self.chunk += 1;
            self.section_y = i32::from(self.source.height().min_section());
        } else {
            self.section_y += 1;
        }
    }

    /// Returns the existing owner intact if either layer is still pending. An
    /// extra Box allocation would undermine failure-path reservation ownership.
    #[allow(clippy::result_large_err)]
    pub fn into_completed(self) -> Result<CompletedLighting, Self> {
        if self.phase != Phase::Complete {
            return Err(self);
        }
        let Self {
            source,
            block_storage,
            sky,
            ..
        } = self;
        let block = block_storage.snapshot();
        let sky = sky.as_ref().map(|sky| sky.storage().snapshot());
        Ok(CompletedLighting { source, block, sky })
    }
}

/// Both snapshots describe the same completed immutable source domain. Standalone
/// users retain storage budgets in snapshot backing. Shared-runtime delivery must
/// additionally retain its CPU or adopted resident reservation around this value
/// and avoid exposing clones that could outlive that reservation; the runtime
/// getter is crate-private.
pub struct CompletedLighting {
    source: LightingSource,
    block: LightSnapshot,
    sky: Option<LightSnapshot>,
}
impl CompletedLighting {
    /// Conservative resident allowance for this inline owner and its reachable
    /// source/snapshot backing, with checked arithmetic and no allocation.
    /// SourceStamp and optional canonical-owner revision retain two small Arc
    /// controls; reserving both is conservative for a producer-owned source.
    /// The shared registry and canonical ResidentChunk palette allocations keep
    /// their original leases and are not duplicated here. Freed engine queues,
    /// source caches, working storage arrays and configured maxima are excluded.
    pub fn retained_bytes(&self) -> Result<usize, LightingError> {
        let source = self
            .source
            .metadata_bytes()
            .checked_add(self.source.owned_section_bytes())
            .ok_or(LightingError::AllocationLimit)?;
        let block = self.block.retained_bytes()?;
        let mut bytes = size_of::<Self>()
            .checked_add(source)
            .and_then(|bytes| bytes.checked_add(4 * size_of::<usize>()))
            .and_then(|bytes| bytes.checked_add(block))
            .ok_or(LightingError::AllocationLimit)?;
        if let Some(sky) = &self.sky {
            bytes = bytes
                .checked_add(sky.retained_bytes()?)
                .ok_or(LightingError::AllocationLimit)?;
        }
        Ok(bytes)
    }

    pub fn source(&self) -> &LightingSource {
        &self.source
    }
    pub fn block(&self) -> &LightSnapshot {
        &self.block
    }
    pub fn sky(&self) -> Option<&LightSnapshot> {
        self.sky.as_ref()
    }
}
