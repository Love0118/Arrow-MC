//! Canonical, immutable ownership of requested disk chunks before activation.
//!
//! This owner does not grant a light/tick/send/spawn capability. Disk FULL and
//! prepared section bytes remain data; POI, structures, ticks, entities and light
//! dependency completion are separate future consumers. Resident palettes are
//! borrowed directly instead of duplicated into the mutable preparation owner.

use super::preparation::ChunkAddress;
use super::section::{ContainerKind, PalettedContainer, Section, SectionCounts};
use super::storage::chunk::DimensionHeight;
use super::storage::region::UnavailableReason;
use super::storage::registry::ChunkRegistrySnapshot;
use super::storage::{ChunkLoadError, ChunkReadOutcome, ChunkStore};
use crate::runtime::{
    AdmissionError, ChunkDecodeOutput, ChunkReadKey, CpuPool, ResidentChunk, ResidentChunkBudget,
    SectionCompletion, SectionKey, SectionTask,
};
use std::{fmt, sync::Arc};

/// Locked ChunkPyramid bound, including generation dependency safety margin.
/// Confirmed through the actual 26.3-pre-2 public API; MIN is rejected explicitly
/// rather than reproducing Java's accidental integer-absolute overflow acceptance.
pub const MAX_REQUEST_CHUNK_COORDINATE: i32 = 2_097_061;
const NONE: usize = usize::MAX;

#[derive(Clone, Copy, Debug)]
pub struct LoadingLimits {
    pub max_chunks: usize,
    /// Requested/returned Vec capacity for demand slots and canonical row maps.
    /// Resident payload has its own budget; fixed owner/Arc control and stack
    /// scratch are not allocator/RSS measurements.
    pub metadata_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadingError {
    InvalidLimits,
    AllocationFailed,
    ChunkLimit,
    MetadataLimit,
    ResidentLimit,
    InvalidCoordinate,
    IdentityExhausted,
    MissingDemand,
    StaleRequest,
    DuplicateCompletion,
    ContextMismatch,
    MissingResident,
    InvalidSection,
    PrepareFailed,
    ForeignPreparation,
    ForeignRead,
    Admission(AdmissionError),
}
impl fmt::Display for LoadingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chunk loading owner: {self:?}")
    }
}
impl std::error::Error for LoadingError {}

#[derive(Debug, PartialEq, Eq)]
pub enum LoadDemand {
    Read(LoadingReadRequest),
    Pending(ChunkReadKey),
    Resident(ChunkReadKey),
}

/// An opaque request provenance token. Numeric key fields alone cannot attach
/// another world's raw decoded result to this owner's publication path.
#[derive(Debug)]
pub struct LoadingReadRequest {
    key: ChunkReadKey,
    owner: Arc<()>,
}
impl PartialEq for LoadingReadRequest {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && Arc::ptr_eq(&self.owner, &other.owner)
    }
}
impl Eq for LoadingReadRequest {}
pub enum LoadingReadOutcome {
    Missing,
    Unavailable(UnavailableReason),
    Decoded(LoadingReadCompletion),
}
pub struct LoadingReadCompletion {
    output: ChunkDecodeOutput,
    owner: Arc<()>,
}
impl LoadingReadRequest {
    pub fn key(&self) -> ChunkReadKey {
        self.key
    }
    /// The only constructor of branded read completions; all I/O and CPU work
    /// remains in ChunkStore with its existing admission/cancellation ownership.
    /// The caller selects the store. Repeating this operation is permitted and
    /// separately admitted; publication accepts only the first current result.
    pub async fn read(&self, store: &ChunkStore) -> Result<LoadingReadOutcome, ChunkLoadError> {
        Ok(match store.read(self.key).await? {
            ChunkReadOutcome::Missing => LoadingReadOutcome::Missing,
            ChunkReadOutcome::Unavailable(reason) => LoadingReadOutcome::Unavailable(reason),
            ChunkReadOutcome::Decoded(output) => {
                LoadingReadOutcome::Decoded(LoadingReadCompletion {
                    output,
                    owner: Arc::clone(&self.owner),
                })
            }
        })
    }
}
impl LoadingReadCompletion {
    pub fn key(&self) -> ChunkReadKey {
        self.output.key()
    }
    pub fn retained_bytes(&self) -> usize {
        self.output.retained_bytes()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    pub stored: (i32, i32),
    pub requested: ChunkAddress,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishReport {
    pub key: ChunkReadKey,
    pub relocated: Option<Relocation>,
}

/// Failed publication returns the original output, still under its CPU lease.
pub struct PublishError {
    kind: LoadingError,
    output: LoadingReadCompletion,
}
impl PublishError {
    pub fn kind(&self) -> LoadingError {
        self.kind
    }
    pub fn into_output(self) -> LoadingReadCompletion {
        self.output
    }
}
impl fmt::Debug for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PublishError").field(&self.kind).finish()
    }
}
impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}
impl std::error::Error for PublishError {}

#[derive(Clone, Copy)]
struct CanonicalRow {
    terrain: usize,
    block_light: usize,
    sky_light: usize,
    y: i8,
}
struct CanonicalChunk {
    resident: ResidentChunk,
    rows: Vec<CanonicalRow>,
}
struct DemandSlot {
    address: ChunkAddress,
    key: ChunkReadKey,
    canonical: Option<CanonicalChunk>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadingStats {
    pub demands: usize,
    pub pending: usize,
    pub residents: usize,
    pub metadata_bytes: usize,
    pub resident_bytes: usize,
}

pub struct ChunkLoadingOwner {
    epoch: u64,
    next_generation: u64,
    registries: Arc<ChunkRegistrySnapshot>,
    height: DimensionHeight,
    has_sky_light: bool,
    limits: LoadingLimits,
    slots: Vec<DemandSlot>,
    metadata_bytes: usize,
    resident_budget: ResidentChunkBudget,
    default_section: Section,
    identity: Arc<()>,
}

impl ChunkLoadingOwner {
    pub fn new(
        epoch: u64,
        registries: Arc<ChunkRegistrySnapshot>,
        height: DimensionHeight,
        has_sky_light: bool,
        limits: LoadingLimits,
        resident_bytes: usize,
    ) -> Result<Self, LoadingError> {
        if limits.max_chunks == 0 {
            return Err(LoadingError::InvalidLimits);
        }
        let bytes = limits
            .max_chunks
            .checked_mul(size_of::<DemandSlot>())
            .ok_or(LoadingError::MetadataLimit)?;
        if bytes > limits.metadata_bytes {
            return Err(LoadingError::MetadataLimit);
        }
        let default_section = default_section(&registries)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(limits.max_chunks)
            .map_err(|_| LoadingError::AllocationFailed)?;
        let metadata_bytes = slots.capacity() * size_of::<DemandSlot>();
        if metadata_bytes > limits.metadata_bytes {
            return Err(LoadingError::MetadataLimit);
        }
        Ok(Self {
            epoch,
            next_generation: 0,
            registries,
            height,
            has_sky_light,
            limits,
            slots,
            metadata_bytes,
            resident_budget: ResidentChunkBudget::new(resident_bytes),
            default_section,
            identity: Arc::new(()),
        })
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn height(&self) -> DimensionHeight {
        self.height
    }
    pub fn has_sky_light(&self) -> bool {
        self.has_sky_light
    }

    /// One slot covers either pending demand or a resident. Repeated demand does
    /// not allocate or consume another generation. No absent-file tombstones are
    /// retained after finish_without_chunk; a retry obtains a fresh generation.
    pub fn request(&mut self, address: ChunkAddress) -> Result<LoadDemand, LoadingError> {
        if !valid_coordinate(address) {
            return Err(LoadingError::InvalidCoordinate);
        }
        match self.slot_index(address) {
            Ok(index) => {
                let slot = &self.slots[index];
                Ok(if slot.canonical.is_some() {
                    LoadDemand::Resident(slot.key)
                } else {
                    LoadDemand::Pending(slot.key)
                })
            }
            Err(index) => {
                if self.slots.len() == self.limits.max_chunks {
                    return Err(LoadingError::ChunkLimit);
                }
                let generation = self
                    .next_generation
                    .checked_add(1)
                    .ok_or(LoadingError::IdentityExhausted)?;
                let key = ChunkReadKey {
                    world_epoch: self.epoch,
                    chunk_x: address.x,
                    chunk_z: address.z,
                    generation,
                };
                self.slots.insert(
                    index,
                    DemandSlot {
                        address,
                        key,
                        canonical: None,
                    },
                );
                self.next_generation = generation;
                Ok(LoadDemand::Read(LoadingReadRequest {
                    key,
                    owner: Arc::clone(&self.identity),
                }))
            }
        }
    }

    pub fn remove_demand(&mut self, address: ChunkAddress) -> bool {
        let Ok(index) = self.slot_index(address) else {
            return false;
        };
        let slot = self.slots.remove(index);
        let bytes = slot
            .canonical
            .as_ref()
            .map_or(0, |chunk| chunk.rows.capacity() * size_of::<CanonicalRow>());
        drop(slot);
        self.metadata_bytes -= bytes;
        true
    }

    /// A matching Missing/Unavailable/error read retires just that pending
    /// request. An old error cannot remove a newer request or a current resident.
    pub fn finish_without_chunk(&mut self, key: ChunkReadKey) -> Result<(), LoadingError> {
        let index = self.check_pending(key)?;
        self.slots.remove(index);
        Ok(())
    }

    /// Identity/default setup must succeed before any old demand is removed.
    /// Callers cancel/drop their old I/O futures; invalidation itself does not
    /// release buffers still owned by blocking reads or CPU workers.
    pub fn reload(
        &mut self,
        registries: Arc<ChunkRegistrySnapshot>,
        height: DimensionHeight,
        has_sky_light: bool,
    ) -> Result<u64, LoadingError> {
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(LoadingError::IdentityExhausted)?;
        let default_section = default_section(&registries)?;
        self.slots.clear();
        self.metadata_bytes = self.slots.capacity() * size_of::<DemandSlot>();
        self.epoch = epoch;
        self.registries = registries;
        self.height = height;
        self.has_sky_light = has_sky_light;
        self.default_section = default_section;
        Ok(epoch)
    }

    #[expect(
        clippy::result_large_err,
        reason = "failure returns the already admitted output without allocating"
    )]
    pub fn publish(
        &mut self,
        output: LoadingReadCompletion,
    ) -> Result<PublishReport, PublishError> {
        if !Arc::ptr_eq(&output.owner, &self.identity) {
            return Err(PublishError {
                kind: LoadingError::ForeignRead,
                output,
            });
        }
        let key = output.key();
        let index = match self.check_pending(key) {
            Ok(index) => index,
            Err(kind) => return Err(PublishError { kind, output }),
        };
        if output.output.height() != self.height
            || output.output.registries().manifest_sha256() != self.registries.manifest_sha256()
            || output.output.registries().configuration_manifest_sha256()
                != self.registries.configuration_manifest_sha256()
        {
            return Err(PublishError {
                kind: LoadingError::ContextMismatch,
                output,
            });
        }
        let rows = match canonical_rows(
            output.output.draft().sections(),
            self.height,
            self.has_sky_light,
            self.limits.metadata_bytes - self.metadata_bytes,
        ) {
            Ok(rows) => rows,
            Err(kind) => return Err(PublishError { kind, output }),
        };
        let relocated =
            (output.output.draft().position != (key.chunk_x, key.chunk_z)).then_some(Relocation {
                stored: output.output.draft().position,
                requested: ChunkAddress {
                    x: key.chunk_x,
                    z: key.chunk_z,
                },
            });
        let LoadingReadCompletion { output: raw, owner } = output;
        let resident = match raw.try_adopt(&self.resident_budget) {
            Ok(resident) => resident,
            Err(error) => {
                return Err(PublishError {
                    kind: LoadingError::ResidentLimit,
                    output: LoadingReadCompletion {
                        output: error.into_output(),
                        owner,
                    },
                });
            }
        };
        self.metadata_bytes += rows.capacity() * size_of::<CanonicalRow>();
        self.slots[index].canonical = Some(CanonicalChunk { resident, rows });
        Ok(PublishReport { key, relocated })
    }

    pub fn resident(&self, address: ChunkAddress) -> Option<&ResidentChunk> {
        self.canonical(address).map(|chunk| &chunk.resident)
    }
    pub fn stored_position(&self, address: ChunkAddress) -> Option<(i32, i32)> {
        self.resident(address)
            .map(|resident| resident.draft().position)
    }
    pub fn section(&self, address: ChunkAddress, y: i32) -> Option<&Section> {
        let y = i8::try_from(y).ok()?;
        if !self.height.contains(y) {
            return None;
        }
        let canonical = self.canonical(address)?;
        let row = canonical.row(y)?;
        if row.terrain == NONE {
            Some(&self.default_section)
        } else {
            canonical.resident.draft().sections()[row.terrain]
                .section
                .as_ref()
        }
    }
    /// Borrow staged stored light; this is not light-engine installation or a
    /// propagation fence. Last present layer wins, including terrain-outside rows.
    pub fn block_light(&self, address: ChunkAddress, y: i32) -> Option<&[u8]> {
        let canonical = self.canonical(address)?;
        let row = canonical.row(i8::try_from(y).ok()?)?;
        if row.block_light == NONE {
            None
        } else {
            canonical.resident.draft().sections()[row.block_light]
                .block_light
                .as_deref()
        }
    }
    pub fn sky_light(&self, address: ChunkAddress, y: i32) -> Option<&[u8]> {
        let canonical = self.canonical(address)?;
        let row = canonical.row(i8::try_from(y).ok()?)?;
        if row.sky_light == NONE {
            None
        } else {
            canonical.resident.draft().sections()[row.sky_light]
                .sky_light
                .as_deref()
        }
    }

    /// Reuses the same admitted kernel as SectionPreparationOwner. Immutable
    /// loaded palettes are copied only into this one bounded worker input, not
    /// into another persistent mutable palette owner.
    pub fn prepare_section(
        &self,
        address: ChunkAddress,
        y: i32,
        pool: &CpuPool,
    ) -> Result<LoadingSectionTask, LoadingError> {
        let canonical = self
            .canonical(address)
            .ok_or(LoadingError::MissingResident)?;
        let section = self
            .section(address, y)
            .ok_or(LoadingError::InvalidSection)?;
        let key = canonical.resident.key();
        let mut pending = pool
            .try_reserve_section(
                SectionKey {
                    world_epoch: key.world_epoch,
                    chunk_x: key.chunk_x,
                    chunk_z: key.chunk_z,
                    section_y: y,
                    revision: key.generation,
                },
                self.registries.block_registry(),
                self.registries.biome_registry(),
                section.counts,
            )
            .map_err(LoadingError::Admission)?;
        for (index, value) in pending.blocks_mut().iter_mut().enumerate() {
            *value = section
                .blocks
                .get(index)
                .map_err(|_| LoadingError::PrepareFailed)?;
        }
        for (index, value) in pending.biomes_mut().iter_mut().enumerate() {
            *value = section
                .biomes
                .get(index)
                .map_err(|_| LoadingError::PrepareFailed)?;
        }
        let task = pending.submit().map_err(LoadingError::Admission)?;
        Ok(LoadingSectionTask {
            task,
            owner: Arc::clone(&self.identity),
        })
    }

    /// Accepted bytes borrow this owner so remove/reload cannot happen while
    /// they are in use. The completion's CPU buffer lease remains attached.
    pub fn accept_prepared(
        &self,
        completed: LoadingSectionCompletion,
    ) -> Result<PreparedSection<'_>, LoadingError> {
        if !Arc::ptr_eq(&completed.owner, &self.identity) {
            return Err(LoadingError::ForeignPreparation);
        }
        let completion = completed.completion;
        let key = completion.key();
        let address = ChunkAddress {
            x: key.chunk_x,
            z: key.chunk_z,
        };
        let canonical = self
            .canonical(address)
            .ok_or(LoadingError::MissingResident)?;
        let resident = canonical.resident.key();
        if key.world_epoch != resident.world_epoch || key.revision != resident.generation {
            return Err(LoadingError::StaleRequest);
        }
        if self.section(address, key.section_y).is_none() {
            return Err(LoadingError::InvalidSection);
        }
        completion
            .bytes()
            .map_err(|_| LoadingError::PrepareFailed)?;
        Ok(PreparedSection {
            completion,
            _owner: self,
        })
    }

    pub fn stats(&self) -> LoadingStats {
        let residents = self.resident_budget.stats();
        LoadingStats {
            demands: self.slots.len(),
            pending: self
                .slots
                .iter()
                .filter(|slot| slot.canonical.is_none())
                .count(),
            residents: residents.chunks,
            metadata_bytes: self.metadata_bytes,
            resident_bytes: residents.used_bytes,
        }
    }
    fn canonical(&self, address: ChunkAddress) -> Option<&CanonicalChunk> {
        self.slots
            .get(self.slot_index(address).ok()?)?
            .canonical
            .as_ref()
    }
    fn slot_index(&self, address: ChunkAddress) -> Result<usize, usize> {
        self.slots
            .binary_search_by_key(&address, |slot| slot.address)
    }
    fn check_pending(&self, key: ChunkReadKey) -> Result<usize, LoadingError> {
        let index = self
            .slot_index(ChunkAddress {
                x: key.chunk_x,
                z: key.chunk_z,
            })
            .map_err(|_| LoadingError::MissingDemand)?;
        let slot = &self.slots[index];
        if key != slot.key || key.world_epoch != self.epoch {
            return Err(LoadingError::StaleRequest);
        }
        if slot.canonical.is_some() {
            return Err(LoadingError::DuplicateCompletion);
        }
        Ok(index)
    }
}

impl CanonicalChunk {
    fn row(&self, y: i8) -> Option<&CanonicalRow> {
        self.rows
            .get(self.rows.binary_search_by_key(&y, |row| row.y).ok()?)
    }
}
pub struct PreparedSection<'a> {
    completion: SectionCompletion,
    _owner: &'a ChunkLoadingOwner,
}

/// A concrete provenance wrapper: arbitrary public CpuPool section jobs cannot
/// be presented as this owner's canonical preparation merely by copying a key.
pub struct LoadingSectionTask {
    task: SectionTask,
    owner: Arc<()>,
}
pub struct LoadingSectionCompletion {
    completion: SectionCompletion,
    owner: Arc<()>,
}
impl LoadingSectionTask {
    pub fn key(&self) -> SectionKey {
        self.task.key()
    }
    pub fn cancel(&self) {
        self.task.cancel();
    }
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
    pub fn try_take(&mut self) -> Option<LoadingSectionCompletion> {
        self.task
            .try_take()
            .map(|completion| LoadingSectionCompletion {
                completion,
                owner: Arc::clone(&self.owner),
            })
    }
    /// Blocks like SectionTask::wait. Async I/O workers should poll is_finished
    /// or try_take rather than block their executor thread.
    pub fn wait(self) -> Option<LoadingSectionCompletion> {
        self.task.wait().map(|completion| LoadingSectionCompletion {
            completion,
            owner: self.owner,
        })
    }
}
impl LoadingSectionCompletion {
    pub fn key(&self) -> SectionKey {
        self.completion.key()
    }
}
impl PreparedSection<'_> {
    pub fn bytes(&self) -> &[u8] {
        self.completion
            .bytes()
            .expect("validated before wrapper construction")
    }
    pub fn key(&self) -> SectionKey {
        self.completion.key()
    }
}

fn valid_coordinate(address: ChunkAddress) -> bool {
    (-MAX_REQUEST_CHUNK_COORDINATE..=MAX_REQUEST_CHUNK_COORDINATE).contains(&address.x)
        && (-MAX_REQUEST_CHUNK_COORDINATE..=MAX_REQUEST_CHUNK_COORDINATE).contains(&address.z)
}
fn default_section(registries: &ChunkRegistrySnapshot) -> Result<Section, LoadingError> {
    let air = registries.air_id();
    let flags = registries
        .state_flags(air)
        .ok_or(LoadingError::ContextMismatch)?;
    Ok(Section {
        counts: SectionCounts {
            non_empty_blocks: if flags.is_air { 0 } else { 4096 },
            fluid_blocks: if !flags.is_air && flags.has_fluid {
                4096
            } else {
                0
            },
        },
        blocks: PalettedContainer::single(ContainerKind::Blocks, registries.block_registry(), air)
            .map_err(|_| LoadingError::ContextMismatch)?,
        biomes: PalettedContainer::single(
            ContainerKind::Biomes,
            registries.biome_registry(),
            registries.plains_id(),
        )
        .map_err(|_| LoadingError::ContextMismatch)?,
    })
}

fn canonical_rows(
    sections: &[super::storage::chunk::StoredSection],
    height: DimensionHeight,
    has_sky: bool,
    budget: usize,
) -> Result<Vec<CanonicalRow>, LoadingError> {
    let mut terrain = [NONE; 256];
    let mut block = [NONE; 256];
    let mut sky = [NONE; 256];
    for (index, section) in sections.iter().enumerate() {
        let slot = (i16::from(section.y) - i16::from(i8::MIN)) as usize;
        if section.section.is_some() && height.contains(section.y) {
            terrain[slot] = index;
        }
        if section.block_light.is_some() {
            block[slot] = index;
        }
        if has_sky && section.sky_light.is_some() {
            sky[slot] = index;
        }
    }
    let selected = |slot: usize| {
        height.contains((slot as i16 + i16::from(i8::MIN)) as i8)
            || block[slot] != NONE
            || sky[slot] != NONE
    };
    let count = (0..256).filter(|&slot| selected(slot)).count();
    if count * size_of::<CanonicalRow>() > budget {
        return Err(LoadingError::MetadataLimit);
    }
    let mut rows = Vec::new();
    rows.try_reserve_exact(count)
        .map_err(|_| LoadingError::AllocationFailed)?;
    if rows.capacity() * size_of::<CanonicalRow>() > budget {
        return Err(LoadingError::MetadataLimit);
    }
    for slot in 0..256 {
        if selected(slot) {
            rows.push(CanonicalRow {
                y: (slot as i16 + i16::from(i8::MIN)) as i8,
                terrain: terrain[slot],
                block_light: block[slot],
                sky_light: sky[slot],
            });
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(epoch: u64) -> ChunkLoadingOwner {
        ChunkLoadingOwner::new(
            epoch,
            Arc::new(super::super::storage::registry::storage_test_snapshot()),
            DimensionHeight::new(-64, 384).unwrap(),
            true,
            LoadingLimits {
                max_chunks: 3,
                metadata_bytes: 65536,
            },
            1024 * 1024,
        )
        .unwrap()
    }

    #[test]
    fn generation_exhaustion_does_not_retire_or_renumber_existing_demand() {
        let mut owner = owner(7);
        let first = ChunkAddress { x: 0, z: 0 };
        let LoadDemand::Read(request) = owner.request(first).unwrap() else {
            panic!("first request")
        };
        let key = request.key();
        owner.next_generation = u64::MAX;
        let before = owner.stats();
        assert_eq!(owner.request(first), Ok(LoadDemand::Pending(key)));
        assert_eq!(
            owner.request(ChunkAddress { x: 1, z: 0 }),
            Err(LoadingError::IdentityExhausted)
        );
        assert_eq!(owner.stats(), before);
        assert_eq!(owner.next_generation, u64::MAX);
        owner.finish_without_chunk(key).unwrap();
        assert_eq!(owner.request(first), Err(LoadingError::IdentityExhausted));
        assert_eq!(owner.stats().demands, 0);
    }

    #[test]
    fn epoch_exhaustion_leaves_context_and_demand_unchanged() {
        let mut owner = owner(u64::MAX);
        let address = ChunkAddress { x: 4, z: -4 };
        let LoadDemand::Read(request) = owner.request(address).unwrap() else {
            panic!("first request")
        };
        let key = request.key();
        let before = owner.stats();
        assert_eq!(
            owner.reload(
                Arc::new(super::super::storage::registry::storage_test_snapshot()),
                DimensionHeight::new(0, 256).unwrap(),
                false
            ),
            Err(LoadingError::IdentityExhausted)
        );
        assert_eq!(owner.stats(), before);
        assert_eq!(owner.height(), DimensionHeight::new(-64, 384).unwrap());
        assert!(owner.has_sky_light());
        assert_eq!(owner.request(address), Ok(LoadDemand::Pending(key)));
    }

    #[test]
    fn coordinate_bound_includes_generation_margin_and_rejects_min_overflow() {
        let mut owner = owner(1);
        for value in [-MAX_REQUEST_CHUNK_COORDINATE, MAX_REQUEST_CHUNK_COORDINATE] {
            for address in [
                ChunkAddress { x: value, z: 0 },
                ChunkAddress { x: 0, z: value },
            ] {
                assert!(matches!(owner.request(address), Ok(LoadDemand::Read(_))));
                owner.remove_demand(address);
            }
        }
        let before = owner.stats();
        for value in [
            i32::MIN,
            i32::MAX,
            -MAX_REQUEST_CHUNK_COORDINATE - 1,
            MAX_REQUEST_CHUNK_COORDINATE + 1,
        ] {
            for address in [
                ChunkAddress { x: value, z: 0 },
                ChunkAddress { x: 0, z: value },
            ] {
                assert_eq!(owner.request(address), Err(LoadingError::InvalidCoordinate));
            }
        }
        assert_eq!(owner.stats(), before);
    }
}
