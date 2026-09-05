//! Owned section revisions and bounded publication of prepared section bytes.
//!
//! This is a concrete consumer of the shared CPU pool, not a gameplay tick loop.
//! Source data stays on the caller's synchronous owner. Independent sections can
//! publish as soon as ready; connections impose their own packet causality later.
//! ChunkMap.scheduleUnload's holder/future checks and GenerationChunkHolder's
//! lifetime checks informed the requirements. This revision model is separately
//! designed and does not reproduce their Java implementation/thread structure.

use std::collections::VecDeque;
use std::fmt;

use crate::runtime::{
    AdmissionError, CpuPool, SECTION_JOB_BUFFER_BYTES, SectionCompletion, SectionJobError,
    SectionKey, SectionTask,
};

use super::section::{
    BIOMES_PER_SECTION, BLOCKS_PER_SECTION, ContainerKind, Error as SectionError,
    PalettedContainer, Registry, Section, SectionCounts,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ChunkAddress {
    pub x: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SectionAddress {
    pub chunk: ChunkAddress,
    pub y: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct PreparationLimits {
    pub max_chunks: usize,
    pub max_sections: usize,
    pub max_pending: usize,
    /// Each cached completion retains SECTION_JOB_BUFFER_BYTES in the pool.
    pub max_cached: usize,
    /// Aggregate palette backing bytes, including old+new storage during growth.
    /// Fixed metadata, stack scratch and the CPU pool budget are separate.
    pub source_heap_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidLimits,
    AllocationFailed,
    ChunkLimit,
    SectionLimit,
    ChunkAlreadyLoaded,
    SectionAlreadyLoaded,
    MissingChunk,
    MissingSection,
    IdentityExhausted,
    Section(SectionError),
}

impl From<SectionError> for Error {
    fn from(error: SectionError) -> Self {
        Self::Section(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "section preparation owner: {self:?}")
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationFailure {
    Cancelled,
    Section(SectionError),
    WorkerPanicked,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DriveReport {
    pub submitted: usize,
    pub published: usize,
    pub discarded: usize,
    pub failed: usize,
    pub evicted: usize,
    pub backpressure: Option<AdmissionError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparationStats {
    pub chunks: usize,
    pub sections: usize,
    pub pending: usize,
    pub dirty: usize,
    pub cached: usize,
    pub source_heap_bytes: usize,
    pub cached_reserved_buffer_bytes: usize,
}

struct LoadedChunk {
    address: ChunkAddress,
    generation: u64,
}

struct ResidentSection {
    address: SectionAddress,
    generation: u64,
    revision: u64,
    source: Section,
    wanted: bool,
    dirty: bool,
    cached: Option<SectionCompletion>,
    failure: Option<PreparationFailure>,
}

struct RequestedSection {
    address: SectionAddress,
    generation: u64,
    task: SectionTask,
}

pub struct SectionPreparationOwner {
    limits: PreparationLimits,
    block_registry: Registry,
    biome_registry: Registry,
    epoch: u64,
    next_generation: u64,
    next_revision: u64,
    source_heap_bytes: usize,
    chunks: Vec<LoadedChunk>,
    sections: Vec<ResidentSection>,
    pending: Vec<RequestedSection>,
    dirty: VecDeque<SectionAddress>,
    cache_order: VecDeque<SectionAddress>,
}

impl SectionPreparationOwner {
    pub fn new(
        epoch: u64,
        block_registry: Registry,
        biome_registry: Registry,
        limits: PreparationLimits,
    ) -> Result<Self, Error> {
        if limits.max_chunks == 0
            || limits.max_sections == 0
            || limits.max_pending == 0
            || limits.max_cached == 0
            || limits.max_cached > limits.max_sections
            || limits
                .max_cached
                .checked_mul(SECTION_JOB_BUFFER_BYTES)
                .is_none()
        {
            return Err(Error::InvalidLimits);
        }
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(limits.max_chunks)
            .map_err(|_| Error::AllocationFailed)?;
        let mut sections = Vec::new();
        sections
            .try_reserve_exact(limits.max_sections)
            .map_err(|_| Error::AllocationFailed)?;
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(limits.max_pending)
            .map_err(|_| Error::AllocationFailed)?;
        let mut dirty = VecDeque::new();
        dirty
            .try_reserve_exact(limits.max_sections)
            .map_err(|_| Error::AllocationFailed)?;
        let mut cache_order = VecDeque::new();
        cache_order
            .try_reserve_exact(limits.max_cached)
            .map_err(|_| Error::AllocationFailed)?;
        Ok(Self {
            limits,
            block_registry,
            biome_registry,
            epoch,
            next_generation: 0,
            next_revision: 0,
            source_heap_bytes: 0,
            chunks,
            sections,
            pending,
            dirty,
            cache_order,
        })
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn chunk_generation(&self, address: ChunkAddress) -> Option<u64> {
        self.chunk_index(address)
            .ok()
            .map(|index| self.chunks[index].generation)
    }

    pub fn load_chunk(&mut self, address: ChunkAddress) -> Result<u64, Error> {
        let index = match self.chunk_index(address) {
            Ok(_) => return Err(Error::ChunkAlreadyLoaded),
            Err(index) => index,
        };
        if self.chunks.len() == self.limits.max_chunks {
            return Err(Error::ChunkLimit);
        }
        let generation = self
            .next_generation
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        self.chunks.insert(
            index,
            LoadedChunk {
                address,
                generation,
            },
        );
        self.next_generation = generation;
        Ok(generation)
    }

    /// Builds the actual source palettes inside the remaining aggregate heap
    /// allowance. A failed load leaves existing sources and identities intact.
    /// The supplied counts must come from the caller's registry metadata.
    pub fn load_section(
        &mut self,
        address: SectionAddress,
        blocks: &[u32; BLOCKS_PER_SECTION],
        biomes: &[u32; BIOMES_PER_SECTION],
        counts: SectionCounts,
    ) -> Result<SectionKey, Error> {
        validate_counts(counts)?;
        let generation = self
            .chunk_generation(address.chunk)
            .ok_or(Error::MissingChunk)?;
        let index = match self.section_index(address) {
            Ok(_) => return Err(Error::SectionAlreadyLoaded),
            Err(index) => index,
        };
        if self.sections.len() == self.limits.max_sections {
            return Err(Error::SectionLimit);
        }
        let revision = self
            .next_revision
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        let remaining = self.limits.source_heap_bytes - self.source_heap_bytes;
        let blocks = PalettedContainer::from_dense(
            ContainerKind::Blocks,
            self.block_registry,
            blocks,
            remaining,
        )?;
        let biomes = PalettedContainer::from_dense(
            ContainerKind::Biomes,
            self.biome_registry,
            biomes,
            remaining - blocks.heap_bytes(),
        )?;
        self.source_heap_bytes += blocks.heap_bytes() + biomes.heap_bytes();
        self.sections.insert(
            index,
            ResidentSection {
                address,
                generation,
                revision,
                source: Section {
                    counts,
                    blocks,
                    biomes,
                },
                wanted: false,
                dirty: false,
                cached: None,
                failure: None,
            },
        );
        self.next_revision = revision;
        Ok(self.key_at(index))
    }

    pub fn section(&self, address: SectionAddress) -> Option<&Section> {
        self.section_index(address)
            .ok()
            .map(|index| &self.sections[index].source)
    }

    pub fn current_key(&self, address: SectionAddress) -> Option<SectionKey> {
        self.section_index(address)
            .ok()
            .map(|index| self.key_at(index))
    }

    /// Validates the new metadata before changing the palette. Metadata-only
    /// changes also advance the revision; a true no-op preserves the cache.
    pub fn set_block(
        &mut self,
        address: SectionAddress,
        index: usize,
        value: u32,
        counts: SectionCounts,
    ) -> Result<bool, Error> {
        validate_counts(counts)?;
        let section = self
            .section_index(address)
            .map_err(|_| Error::MissingSection)?;
        let source = &self.sections[section].source;
        if source.blocks.get(index)? == value && source.counts == counts {
            return Ok(false);
        }
        let revision = self
            .next_revision
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        let previous_bytes = source.blocks.heap_bytes();
        let budget = self.limits.source_heap_bytes - (self.source_heap_bytes - previous_bytes);
        self.sections[section]
            .source
            .blocks
            .set(index, value, budget)?;
        self.sections[section].source.counts = counts;
        self.source_heap_bytes = self.source_heap_bytes - previous_bytes
            + self.sections[section].source.blocks.heap_bytes();
        self.changed(section, revision);
        Ok(true)
    }

    pub fn set_biome(
        &mut self,
        address: SectionAddress,
        index: usize,
        value: u32,
    ) -> Result<bool, Error> {
        let section = self
            .section_index(address)
            .map_err(|_| Error::MissingSection)?;
        let source = &self.sections[section].source;
        if source.biomes.get(index)? == value {
            return Ok(false);
        }
        let revision = self
            .next_revision
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        let previous_bytes = source.biomes.heap_bytes();
        let budget = self.limits.source_heap_bytes - (self.source_heap_bytes - previous_bytes);
        self.sections[section]
            .source
            .biomes
            .set(index, value, budget)?;
        self.source_heap_bytes = self.source_heap_bytes - previous_bytes
            + self.sections[section].source.biomes.heap_bytes();
        self.changed(section, revision);
        Ok(true)
    }

    pub fn set_counts(
        &mut self,
        address: SectionAddress,
        counts: SectionCounts,
    ) -> Result<bool, Error> {
        validate_counts(counts)?;
        let section = self
            .section_index(address)
            .map_err(|_| Error::MissingSection)?;
        if self.sections[section].source.counts == counts {
            return Ok(false);
        }
        let revision = self
            .next_revision
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        self.sections[section].source.counts = counts;
        self.changed(section, revision);
        Ok(true)
    }

    /// Expresses demand without allocating a snapshot. Subsequent changes to a
    /// requested section coalesce into a latest-revision job in drive().
    pub fn request(&mut self, address: SectionAddress) -> Result<SectionKey, Error> {
        let index = self
            .section_index(address)
            .map_err(|_| Error::MissingSection)?;
        self.sections[index].wanted = true;
        self.sections[index].failure = None;
        let key = self.key_at(index);
        if self.sections[index].cached.is_none()
            && !self.sections[index].dirty
            && !self.pending.iter().any(|pending| pending.task.key() == key)
        {
            self.mark_dirty(index);
        }
        Ok(key)
    }

    /// Nonblocking owner progress. A slow/stale job never gates other sections'
    /// ready results. Cache entries may be evicted to make room for admission;
    /// the loaded source stays resident and can be requested again.
    pub fn drive(&mut self, pool: &CpuPool) -> Result<DriveReport, Error> {
        let mut report = DriveReport::default();
        let mut index = 0;
        while index < self.pending.len() {
            if let Some(completion) = self.pending[index].task.try_take() {
                let pending = self.pending.swap_remove(index);
                self.publish(pending.generation, completion, &mut report);
            } else {
                index += 1;
            }
        }
        while let Some(&address) = self.dirty.front() {
            if self.pending.len() == self.limits.max_pending {
                report.backpressure = Some(AdmissionError::JobLimit);
                break;
            }
            let index = self
                .section_index(address)
                .expect("dirty entry has loaded source");
            let key = self.key_at(index);
            let mut job = loop {
                match pool.try_reserve_section(
                    key,
                    self.block_registry,
                    self.biome_registry,
                    self.sections[index].source.counts,
                ) {
                    Ok(job) => break Some(job),
                    Err(error @ (AdmissionError::JobLimit | AdmissionError::ByteLimit)) => {
                        if self.evict_oldest() {
                            report.evicted += 1;
                        } else {
                            report.backpressure = Some(error);
                            break None;
                        }
                    }
                    Err(error) => {
                        report.backpressure = Some(error);
                        break None;
                    }
                }
            };
            let Some(mut job) = job.take() else {
                break;
            };
            let source = &self.sections[index].source;
            for (position, value) in job.blocks_mut().iter_mut().enumerate() {
                *value = source.blocks.get(position)?;
            }
            for (position, value) in job.biomes_mut().iter_mut().enumerate() {
                *value = source.biomes.get(position)?;
            }
            match job.submit() {
                Ok(task) => {
                    self.pending.push(RequestedSection {
                        address,
                        generation: self.sections[index].generation,
                        task,
                    });
                    self.dirty.pop_front();
                    self.sections[index].dirty = false;
                    report.submitted += 1;
                }
                Err(error) => {
                    report.backpressure = Some(error);
                    break;
                }
            }
        }
        Ok(report)
    }

    /// Borrows current cached bytes with their live identity. Cache insertion or
    /// eviction is not a client delivery acknowledgement; a cache miss may be
    /// requested again without unloading or recreating the source section.
    pub fn cached(&self, address: SectionAddress) -> Option<(SectionKey, &[u8])> {
        let index = self.section_index(address).ok()?;
        let completion = self.sections[index].cached.as_ref()?;
        Some((completion.key(), completion.bytes().ok()?))
    }

    pub fn failure(&self, address: SectionAddress) -> Option<PreparationFailure> {
        self.section_index(address)
            .ok()
            .and_then(|index| self.sections[index].failure)
    }

    pub fn unload_chunk(&mut self, address: ChunkAddress) -> Result<(), Error> {
        let index = self.chunk_index(address).map_err(|_| Error::MissingChunk)?;
        for pending in &self.pending {
            if pending.address.chunk == address {
                pending.task.cancel();
            }
        }
        self.dirty.retain(|section| section.chunk != address);
        self.cache_order.retain(|section| section.chunk != address);
        self.sections.retain(|section| {
            if section.address.chunk == address {
                self.source_heap_bytes -=
                    section.source.blocks.heap_bytes() + section.source.biomes.heap_bytes();
                false
            } else {
                true
            }
        });
        self.chunks.remove(index);
        Ok(())
    }

    /// Drops sources/caches and cancels old tasks. Running worker memory remains
    /// owned and budgeted by the shared pool until actual work/buffer destruction.
    /// Generation/revision counters are never reset, even when coordinates recur.
    pub fn reload(
        &mut self,
        block_registry: Registry,
        biome_registry: Registry,
    ) -> Result<u64, Error> {
        let epoch = self.epoch.checked_add(1).ok_or(Error::IdentityExhausted)?;
        self.pending.clear();
        self.sections.clear();
        self.chunks.clear();
        self.dirty.clear();
        self.cache_order.clear();
        self.source_heap_bytes = 0;
        self.block_registry = block_registry;
        self.biome_registry = biome_registry;
        self.epoch = epoch;
        Ok(epoch)
    }

    pub fn stats(&self) -> PreparationStats {
        PreparationStats {
            chunks: self.chunks.len(),
            sections: self.sections.len(),
            pending: self.pending.len(),
            dirty: self.dirty.len(),
            cached: self.cache_order.len(),
            source_heap_bytes: self.source_heap_bytes,
            cached_reserved_buffer_bytes: self.cache_order.len() * SECTION_JOB_BUFFER_BYTES,
        }
    }

    fn chunk_index(&self, address: ChunkAddress) -> Result<usize, usize> {
        self.chunks
            .binary_search_by_key(&address, |chunk| chunk.address)
    }

    fn section_index(&self, address: SectionAddress) -> Result<usize, usize> {
        self.sections
            .binary_search_by_key(&address, |section| section.address)
    }

    fn key_at(&self, index: usize) -> SectionKey {
        let section = &self.sections[index];
        SectionKey {
            world_epoch: self.epoch,
            chunk_x: section.address.chunk.x,
            chunk_z: section.address.chunk.z,
            section_y: section.address.y,
            revision: section.revision,
        }
    }

    fn mark_dirty(&mut self, index: usize) {
        if !self.sections[index].dirty {
            self.dirty.push_back(self.sections[index].address);
            self.sections[index].dirty = true;
        }
    }

    fn changed(&mut self, index: usize, revision: u64) {
        self.next_revision = revision;
        let address = self.sections[index].address;
        self.sections[index].revision = revision;
        self.sections[index].failure = None;
        if self.sections[index].cached.take().is_some() {
            self.cache_order.retain(|cached| *cached != address);
        }
        if self.sections[index].wanted {
            // Multiple writes before the next snapshot share one dirty entry.
            // The first write cancelled older revisions; submitting a new job
            // clears dirty, so subsequent writes will cancel that job as well.
            if !self.sections[index].dirty {
                for pending in &self.pending {
                    if pending.address == address {
                        pending.task.cancel();
                    }
                }
            }
            self.mark_dirty(index);
        }
    }

    fn evict_oldest(&mut self) -> bool {
        if let Some(address) = self.cache_order.pop_front() {
            let index = self
                .section_index(address)
                .expect("cached entry has loaded source");
            self.sections[index].cached = None;
            true
        } else {
            false
        }
    }

    fn publish(
        &mut self,
        generation: u64,
        completion: SectionCompletion,
        report: &mut DriveReport,
    ) {
        let key = completion.key();
        let address = SectionAddress {
            chunk: ChunkAddress {
                x: key.chunk_x,
                z: key.chunk_z,
            },
            y: key.section_y,
        };
        let Ok(index) = self.section_index(address) else {
            report.discarded += 1;
            return;
        };
        if generation != self.sections[index].generation || key != self.key_at(index) {
            report.discarded += 1;
            return;
        }
        if let Err(error) = completion.bytes() {
            self.sections[index].failure = Some(match error {
                SectionJobError::Cancelled => PreparationFailure::Cancelled,
                SectionJobError::Prepare(error) => PreparationFailure::Section(*error),
                SectionJobError::WorkerPanicked => PreparationFailure::WorkerPanicked,
            });
            report.failed += 1;
            return;
        }
        if self.sections[index].cached.is_some() {
            self.cache_order.retain(|cached| *cached != address);
            self.sections[index].cached = None;
        }
        if self.cache_order.len() == self.limits.max_cached {
            self.evict_oldest();
            report.evicted += 1;
        }
        self.sections[index].failure = None;
        self.sections[index].cached = Some(completion);
        self.cache_order.push_back(address);
        report.published += 1;
    }
}

fn validate_counts(counts: SectionCounts) -> Result<(), Error> {
    if usize::from(counts.non_empty_blocks) > BLOCKS_PER_SECTION
        || counts.fluid_blocks > counts.non_empty_blocks
    {
        Err(Error::Section(SectionError::InvalidCounts))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::CpuPoolConfig;
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::{Duration, Instant};

    struct ReleaseGate(Arc<crate::runtime::TestGate>);

    impl Drop for ReleaseGate {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    fn await_condition(check: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(Instant::now() < deadline, "owner progress timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn address(x: i32) -> SectionAddress {
        SectionAddress {
            chunk: ChunkAddress { x, z: -2 },
            y: -4,
        }
    }

    fn owner() -> SectionPreparationOwner {
        SectionPreparationOwner::new(
            7,
            Registry::new(32).unwrap(),
            Registry::new(8).unwrap(),
            PreparationLimits {
                max_chunks: 4,
                max_sections: 4,
                max_pending: 4,
                max_cached: 4,
                source_heap_bytes: 65536,
            },
        )
        .unwrap()
    }

    fn pool() -> CpuPool {
        CpuPool::new(CpuPoolConfig {
            workers: 2,
            max_jobs: 4,
            buffer_bytes: 4 * SECTION_JOB_BUFFER_BYTES,
        })
        .unwrap()
    }

    fn load(owner: &mut SectionPreparationOwner, address: SectionAddress) {
        owner.load_chunk(address.chunk).unwrap();
        owner
            .load_section(
                address,
                &[1; 4096],
                &[2; 64],
                SectionCounts {
                    non_empty_blocks: 4096,
                    fluid_blocks: 0,
                },
            )
            .unwrap();
    }

    #[test]
    fn older_real_completion_delivered_after_new_revision_cannot_replace_latest_bytes() {
        let pool = pool();
        let mut owner = owner();
        let address = address(1);
        load(&mut owner, address);
        owner.request(address).unwrap();
        assert_eq!(owner.drive(&pool).unwrap().submitted, 1);
        // Hold delivery, not a synthetic payload. Removing this handle from the
        // owner also ensures mutation's cancellation cannot mask a key bug.
        let older = owner.pending.pop().unwrap();
        owner
            .set_block(
                address,
                7,
                3,
                SectionCounts {
                    non_empty_blocks: 4096,
                    fluid_blocks: 1,
                },
            )
            .unwrap();
        assert_eq!(owner.drive(&pool).unwrap().submitted, 1);
        let newer = owner.pending.pop().unwrap();
        let mut report = DriveReport::default();
        owner.publish(newer.generation, newer.task.wait().unwrap(), &mut report);
        let current_key = owner.cached(address).unwrap().0;
        let mut input = owner.cached(address).unwrap().1;
        let decoded = Section::read_network(
            &mut input,
            Registry::new(32).unwrap(),
            Registry::new(8).unwrap(),
            65536,
        )
        .unwrap();
        assert_eq!(decoded.blocks.get(7).unwrap(), 3);
        assert_eq!(decoded.counts.fluid_blocks, 1);
        assert!(input.is_empty());
        assert_eq!(report.published, 1);
        owner.publish(older.generation, older.task.wait().unwrap(), &mut report);
        assert_eq!(report.discarded, 1);
        assert_eq!(owner.cached(address).unwrap().0, current_key);
        assert_eq!(pool.stats().in_flight, 1);
    }

    #[test]
    fn reused_coordinates_and_reload_reject_actual_retained_old_completions() {
        let pool = pool();
        let mut owner = owner();
        let address = address(1);
        load(&mut owner, address);
        let original_generation = owner.chunk_generation(address.chunk).unwrap();
        let original_key = owner.request(address).unwrap();
        owner.drive(&pool).unwrap();
        let original = owner.pending.pop().unwrap();
        owner.unload_chunk(address.chunk).unwrap();
        load(&mut owner, address);
        assert_ne!(
            owner.chunk_generation(address.chunk).unwrap(),
            original_generation
        );
        assert_ne!(owner.current_key(address).unwrap(), original_key);
        let mut report = DriveReport::default();
        owner.publish(
            original.generation,
            original.task.wait().unwrap(),
            &mut report,
        );
        assert_eq!(report.discarded, 1);
        assert!(owner.cached(address).is_none());

        let before_reload = owner.request(address).unwrap();
        owner.drive(&pool).unwrap();
        let prior_epoch = owner.pending.pop().unwrap();
        owner
            .reload(Registry::new(32).unwrap(), Registry::new(8).unwrap())
            .unwrap();
        load(&mut owner, address);
        assert_ne!(
            owner.current_key(address).unwrap().world_epoch,
            before_reload.world_epoch
        );
        owner.publish(
            prior_epoch.generation,
            prior_epoch.task.wait().unwrap(),
            &mut report,
        );
        assert_eq!(report.discarded, 2);
        assert!(owner.cached(address).is_none());
        assert_eq!(pool.stats().reserved_buffer_bytes, 0);
    }

    #[test]
    fn identity_exhaustion_is_reported_before_source_or_lifecycle_changes() {
        let mut owner = owner();
        let address = address(1);
        load(&mut owner, address);
        let initial = owner.current_key(address).unwrap();
        owner.next_revision = u64::MAX;
        assert_eq!(
            owner.set_biome(address, 0, 4),
            Err(Error::IdentityExhausted)
        );
        assert_eq!(owner.current_key(address).unwrap(), initial);
        assert_eq!(owner.section(address).unwrap().biomes.get(0).unwrap(), 2);
        assert_eq!(
            owner.set_counts(
                address,
                SectionCounts {
                    non_empty_blocks: 4096,
                    fluid_blocks: 1
                }
            ),
            Err(Error::IdentityExhausted)
        );
        assert_eq!(owner.section(address).unwrap().counts.fluid_blocks, 0);
        owner.next_generation = u64::MAX;
        assert_eq!(
            owner.load_chunk(ChunkAddress { x: 2, z: -2 }),
            Err(Error::IdentityExhausted)
        );
        assert_eq!(owner.stats().chunks, 1);
        owner.epoch = u64::MAX;
        assert_eq!(
            owner.reload(Registry::new(2).unwrap(), Registry::new(2).unwrap()),
            Err(Error::IdentityExhausted)
        );
        assert_eq!(owner.stats().sections, 1);
        assert_eq!(owner.section(address).unwrap().blocks.get(0).unwrap(), 1);
    }

    #[test]
    fn delayed_old_worker_does_not_block_unrelated_or_new_revision_publication() {
        let pool = pool();
        let mut owner = owner();
        let slow = address(1);
        let fast = address(2);
        load(&mut owner, slow);
        load(&mut owner, fast);
        let key = owner.request(slow).unwrap();
        let (started, receiver) = mpsc::sync_channel(1);
        let gate = Arc::new(crate::runtime::TestGate {
            started,
            released: Mutex::new(false),
            changed: Condvar::new(),
        });
        // This guard is created after the pool, so every assertion unwind opens
        // the test-only gate before pool Drop attempts to join its workers.
        let _release = ReleaseGate(Arc::clone(&gate));
        let mut pending = pool
            .try_reserve_section(
                key,
                owner.block_registry,
                owner.biome_registry,
                owner.section(slow).unwrap().counts,
            )
            .unwrap();
        for (index, value) in pending.blocks_mut().iter_mut().enumerate() {
            *value = owner.section(slow).unwrap().blocks.get(index).unwrap();
        }
        for (index, value) in pending.biomes_mut().iter_mut().enumerate() {
            *value = owner.section(slow).unwrap().biomes.get(index).unwrap();
        }
        let task = pending.submit_with_gate(Arc::clone(&gate)).unwrap();
        owner.pending.push(RequestedSection {
            address: slow,
            generation: owner.chunk_generation(slow.chunk).unwrap(),
            task,
        });
        assert_eq!(owner.dirty.pop_front(), Some(slow));
        let slow_index = owner.section_index(slow).unwrap();
        owner.sections[slow_index].dirty = false;
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();

        owner.request(fast).unwrap();
        assert_eq!(owner.drive(&pool).unwrap().submitted, 1);
        await_condition(|| {
            owner
                .pending
                .iter()
                .any(|job| job.address == fast && job.task.is_finished())
        });
        let report = owner.drive(&pool).unwrap();
        assert_eq!(report.published, 1);
        assert!(owner.cached(fast).is_some());
        assert!(owner.cached(slow).is_none());

        owner.set_biome(slow, 0, 4).unwrap();
        let latest = owner.current_key(slow).unwrap();
        assert_eq!(owner.drive(&pool).unwrap().submitted, 1);
        await_condition(|| {
            owner
                .pending
                .iter()
                .any(|job| job.task.key() == latest && job.task.is_finished())
        });
        assert_eq!(owner.drive(&pool).unwrap().published, 1);
        assert_eq!(owner.cached(slow).unwrap().0, latest);
        assert_eq!(pool.stats().running, 1);
        assert_eq!(pool.stats().in_flight, 3);
        gate.release();
        await_condition(|| owner.pending.iter().all(|job| job.task.is_finished()));
        assert_eq!(owner.drive(&pool).unwrap().discarded, 1);
        assert_eq!(owner.cached(slow).unwrap().0, latest);
        assert_eq!(pool.stats().in_flight, 2);
    }
}
