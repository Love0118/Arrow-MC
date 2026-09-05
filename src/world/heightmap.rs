//! Stored heightmaps and synchronous kernels over borrowed chunk sections.
//!
//! Heightmaps describe columns; they do not establish collision, lighting, tick,
//! or spawn readiness. Raw restoration deliberately preserves all supplied bits.

use super::loading::ChunkLoadingOwner;
use super::preparation::ChunkAddress;
use super::section::Section;
use super::storage::chunk::{ChunkStatus, DimensionHeight};
use super::storage::registry::ChunkRegistrySnapshot;
use crate::nbt::{Compound, Tag};
use std::fmt;

const COLUMNS: usize = 256;
const MAX_SECTIONS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum HeightmapKind {
    WorldSurfaceWg = 0,
    WorldSurface = 1,
    OceanFloorWg = 2,
    OceanFloor = 3,
    MotionBlocking = 4,
    MotionBlockingNoLeaves = 5,
}

impl HeightmapKind {
    pub const ALL: [Self; 6] = [
        Self::WorldSurfaceWg,
        Self::WorldSurface,
        Self::OceanFloorWg,
        Self::OceanFloor,
        Self::MotionBlocking,
        Self::MotionBlockingNoLeaves,
    ];

    pub const fn id(self) -> u8 {
        self as u8
    }
    pub fn from_id(id: u8) -> Option<Self> {
        Self::ALL.get(usize::from(id)).copied()
    }
    pub const fn send_to_client(self) -> bool {
        matches!(
            self,
            Self::WorldSurface | Self::MotionBlocking | Self::MotionBlockingNoLeaves
        )
    }
    pub const fn keep_after_worldgen(self) -> bool {
        !matches!(self, Self::WorldSurfaceWg | Self::OceanFloorWg)
    }
    pub const fn serialization_key(self) -> &'static str {
        match self {
            Self::WorldSurfaceWg => "WORLD_SURFACE_WG",
            Self::WorldSurface => "WORLD_SURFACE",
            Self::OceanFloorWg => "OCEAN_FLOOR_WG",
            Self::OceanFloor => "OCEAN_FLOOR",
            Self::MotionBlocking => "MOTION_BLOCKING",
            Self::MotionBlockingNoLeaves => "MOTION_BLOCKING_NO_LEAVES",
        }
    }
    const fn mask(self) -> u8 {
        1 << self.id()
    }
}

/// Missing maps required by the persisted status. Present maps of other kinds
/// are restored as well; worldgen variants are not discarded during loading.
pub const fn required_mask(status: ChunkStatus) -> u8 {
    match status {
        ChunkStatus::Empty
        | ChunkStatus::StructureStarts
        | ChunkStatus::StructureReferences
        | ChunkStatus::Biomes => 0b000101,
        _ => 0b111010,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightmapError {
    AllocationLimit,
    AllocationFailed,
    MissingResident,
    SectionCount,
    InvalidSection,
    InvalidState(u32),
    InvalidColumn,
    InvalidY,
    ContextMismatch,
}
impl fmt::Display for HeightmapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "heightmap: {self:?}")
    }
}
impl std::error::Error for HeightmapError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Context {
    height: DimensionHeight,
    registry: [u8; 32],
    configuration: [u8; 32],
}

/// A fixed borrowed view, not an owned dense chunk or general world interface.
/// None entries in an explicit section snapshot mean the registry's actual air.
pub struct HeightmapSource<'a> {
    sections: [Option<&'a Section>; MAX_SECTIONS],
    registries: &'a ChunkRegistrySnapshot,
    context: Context,
    highest_section: i32,
}

impl<'a> HeightmapSource<'a> {
    pub fn from_canonical(
        owner: &'a ChunkLoadingOwner,
        address: ChunkAddress,
    ) -> Result<Self, HeightmapError> {
        let resident = owner
            .resident(address)
            .ok_or(HeightmapError::MissingResident)?;
        let mut source = Self::empty(resident.registries(), owner.height());
        for (index, section_y) in (i32::from(owner.height().min_section())
            ..=i32::from(owner.height().max_section()))
            .enumerate()
        {
            let section = owner
                .section(address, section_y)
                .ok_or(HeightmapError::InvalidSection)?;
            source.sections[index] = Some(section);
            if section.counts.non_empty_blocks != 0 {
                source.highest_section = section_y;
            }
        }
        Ok(source)
    }

    /// Builds a concrete edited/test snapshot in ascending section-Y order.
    /// IDs and counts are checked once before any map mutation. Canonical stored
    /// chunks use from_canonical, whose decoder already established these facts.
    pub fn from_sections(
        registries: &'a ChunkRegistrySnapshot,
        height: DimensionHeight,
        sections: &[Option<&'a Section>],
    ) -> Result<Self, HeightmapError> {
        if sections.len() != section_count(height) {
            return Err(HeightmapError::SectionCount);
        }
        let mut source = Self::empty(registries, height);
        for (index, section) in sections.iter().enumerate() {
            let Some(section) = section else {
                continue;
            };
            let mut non_empty = 0;
            for cell in 0..4096 {
                let id = section
                    .blocks
                    .get(cell)
                    .map_err(|_| HeightmapError::InvalidSection)?;
                let flags = registries
                    .state_flags(id)
                    .ok_or(HeightmapError::InvalidState(id))?;
                non_empty += u16::from(!flags.is_air);
            }
            if non_empty != section.counts.non_empty_blocks {
                return Err(HeightmapError::InvalidSection);
            }
            source.sections[index] = Some(section);
            if non_empty != 0 {
                source.highest_section = i32::from(height.min_section()) + index as i32;
            }
        }
        Ok(source)
    }

    fn empty(registries: &'a ChunkRegistrySnapshot, height: DimensionHeight) -> Self {
        Self {
            sections: [None; MAX_SECTIONS],
            registries,
            context: Context {
                height,
                registry: registries.manifest_sha256(),
                configuration: registries.configuration_manifest_sha256(),
            },
            highest_section: i32::from(height.min_section()),
        }
    }
    pub fn height(&self) -> DimensionHeight {
        self.context.height
    }
    pub fn min_y(&self) -> i32 {
        i32::from(self.height().min_section()) * 16
    }
    pub fn max_y(&self) -> i32 {
        (i32::from(self.height().max_section()) + 1) * 16
    }
    fn state(&self, x: u8, y: i32, z: u8) -> u32 {
        let index = (y.div_euclid(16) - i32::from(self.height().min_section())) as usize;
        self.sections[index].map_or(self.registries.air_id(), |section| {
            // Ordinary ProtoChunk/LevelChunk hide cave/void air variants in a
            // section whose non-empty block count is zero.
            if section.counts.non_empty_blocks == 0 {
                return self.registries.air_id();
            }
            section
                .blocks
                .get(usize::from(x) + 16 * usize::from(z) + 256 * y.rem_euclid(16) as usize)
                .expect("validated section cell")
        })
    }
    fn opaque(&self, kind: HeightmapKind, state: u32) -> bool {
        self.registries
            .heightmap_mask(state)
            .expect("validated state ID")
            & kind.mask()
            != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored,
    Reprimed,
}

pub struct Heightmap {
    kind: HeightmapKind,
    context: Context,
    bits: u8,
    words: Vec<u64>,
}

impl Heightmap {
    /// The caller provides a reserved backing-byte allowance. Fixed values,
    /// borrowed views and allocator metadata are excluded; this is not an RSS cap.
    pub fn new(
        kind: HeightmapKind,
        source: &HeightmapSource<'_>,
        allocation_limit: usize,
    ) -> Result<Self, HeightmapError> {
        let bits = height_bits(source.height());
        let count = COLUMNS.div_ceil(64 / usize::from(bits));
        if count * size_of::<u64>() > allocation_limit {
            return Err(HeightmapError::AllocationLimit);
        }
        let mut words = Vec::new();
        words
            .try_reserve_exact(count)
            .map_err(|_| HeightmapError::AllocationFailed)?;
        if words.capacity() * size_of::<u64>() > allocation_limit {
            return Err(HeightmapError::AllocationLimit);
        }
        words.resize(count, 0);
        Ok(Self {
            kind,
            context: source.context,
            bits,
            words,
        })
    }
    pub fn required_bytes(height: DimensionHeight) -> usize {
        COLUMNS.div_ceil(64 / usize::from(height_bits(height))) * size_of::<u64>()
    }
    pub fn kind(&self) -> HeightmapKind {
        self.kind
    }
    pub fn bits(&self) -> u8 {
        self.bits
    }
    pub fn raw(&self) -> &[u64] {
        &self.words
    }
    pub fn heap_bytes(&self) -> usize {
        self.words.capacity() * size_of::<u64>()
    }
    pub fn first_available(&self, x: u8, z: u8) -> Result<i32, HeightmapError> {
        let index = column(x, z)?;
        let (word, shift) = self.position(index);
        Ok(((self.words[word] >> shift) & self.value_mask()) as i32
            + i32::from(self.context.height.min_section()) * 16)
    }
    pub fn highest_taken(&self, x: u8, z: u8) -> Result<i32, HeightmapError> {
        Ok(self.first_available(x, z)? - 1)
    }

    /// Re-priming does not clear an existing column for which no state matches.
    pub fn prime(&mut self, source: &HeightmapSource<'_>) -> Result<(), HeightmapError> {
        self.check_context(source)?;
        for index in 0..COLUMNS {
            let (x, z) = ((index % 16) as u8, (index / 16) as u8);
            for y in (source.min_y()..source.highest_section * 16 + 16).rev() {
                let state = source.state(x, y, z);
                if state != source.registries.air_id() && source.opaque(self.kind, state) {
                    self.set(index, y + 1);
                    break;
                }
            }
        }
        Ok(())
    }
    pub fn restore(
        &mut self,
        raw: &[u64],
        source: &HeightmapSource<'_>,
    ) -> Result<RestoreOutcome, HeightmapError> {
        self.check_context(source)?;
        if raw.len() == self.words.len() {
            self.words.copy_from_slice(raw);
            Ok(RestoreOutcome::Restored)
        } else {
            self.prime(source)?;
            Ok(RestoreOutcome::Reprimed)
        }
    }
    /// The changed cell is supplied explicitly. The source must describe the
    /// current lower cells after the world mutation; it is never mutated here.
    /// This boundary accepts local X/Z in 0..16 and absolute Y inside the source's
    /// build height; arbitrary out-of-world calls to Java's method are not exposed.
    pub fn update(
        &mut self,
        x: u8,
        y: i32,
        z: u8,
        state: u32,
        source: &HeightmapSource<'_>,
    ) -> Result<bool, HeightmapError> {
        self.check_context(source)?;
        let index = column(x, z)?;
        if !(source.min_y()..source.max_y()).contains(&y) {
            return Err(HeightmapError::InvalidY);
        }
        if source.registries.heightmap_mask(state).is_none() {
            return Err(HeightmapError::InvalidState(state));
        }
        let first = self.first_available(x, z)?;
        if y <= first - 2 {
            return Ok(false);
        }
        if source.opaque(self.kind, state) {
            if y < first {
                return Ok(false);
            }
            self.set(index, y + 1);
            return Ok(true);
        }
        if y != first - 1 {
            return Ok(false);
        }
        let next = (source.min_y()..y)
            .rev()
            .find(|&lower| source.opaque(self.kind, source.state(x, lower, z)))
            .map_or(source.min_y(), |lower| lower + 1);
        self.set(index, next);
        Ok(true)
    }
    fn check_context(&self, source: &HeightmapSource<'_>) -> Result<(), HeightmapError> {
        if self.context == source.context {
            Ok(())
        } else {
            Err(HeightmapError::ContextMismatch)
        }
    }
    fn position(&self, index: usize) -> (usize, usize) {
        let per_word = 64 / usize::from(self.bits);
        (index / per_word, index % per_word * usize::from(self.bits))
    }
    fn value_mask(&self) -> u64 {
        (1 << self.bits) - 1
    }
    fn set(&mut self, index: usize, first: i32) {
        let (word, shift) = self.position(index);
        let relative = (first - i32::from(self.context.height.min_section()) * 16) as u64;
        self.words[word] = (self.words[word] & !(self.value_mask() << shift)) | (relative << shift);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightmapOrigin {
    Restored,
    Reprimed,
    MissingPrimed,
}

/// The actual stored-map consumer: restore every recognized long array, then
/// prime missing maps required by the chunk's persisted status. Raw NBT is borrowed.
pub struct HeightmapSet {
    maps: [Option<Heightmap>; 6],
    origins: [Option<HeightmapOrigin>; 6],
}
impl HeightmapSet {
    pub fn from_canonical(
        owner: &ChunkLoadingOwner,
        address: ChunkAddress,
        allocation_limit: usize,
    ) -> Result<Self, HeightmapError> {
        let source = HeightmapSource::from_canonical(owner, address)?;
        let draft = owner
            .resident(address)
            .ok_or(HeightmapError::MissingResident)?
            .draft();
        Self::from_stored(&source, draft.root(), draft.status, allocation_limit)
    }
    pub fn from_stored(
        source: &HeightmapSource<'_>,
        root: &Compound,
        status: ChunkStatus,
        allocation_limit: usize,
    ) -> Result<Self, HeightmapError> {
        let stored = match field(root, "Heightmaps") {
            Some(Tag::Compound(value)) => Some(value),
            _ => None,
        };
        let mut arrays = [None; 6];
        let mut selected = required_mask(status);
        for kind in HeightmapKind::ALL {
            if let Some(Tag::LongArray(values)) =
                stored.and_then(|value| field(value, kind.serialization_key()))
            {
                arrays[kind.id() as usize] = Some(values.as_slice());
                selected |= kind.mask();
            }
        }
        if Heightmap::required_bytes(source.height()) * selected.count_ones() as usize
            > allocation_limit
        {
            return Err(HeightmapError::AllocationLimit);
        }
        let mut result = Self {
            maps: [const { None }; 6],
            origins: [None; 6],
        };
        let mut remaining = allocation_limit;
        let mut to_prime = 0;
        for kind in HeightmapKind::ALL {
            if selected & kind.mask() == 0 {
                continue;
            }
            let mut map = Heightmap::new(kind, source, remaining)?;
            remaining -= map.heap_bytes();
            let origin = match arrays[kind.id() as usize] {
                Some(raw) if raw.len() == map.words.len() => {
                    for (word, &value) in map.words.iter_mut().zip(raw) {
                        *word = value as u64;
                    }
                    HeightmapOrigin::Restored
                }
                raw => {
                    to_prime |= kind.mask();
                    if raw.is_some() {
                        HeightmapOrigin::Reprimed
                    } else {
                        HeightmapOrigin::MissingPrimed
                    }
                }
            };
            result.origins[kind.id() as usize] = Some(origin);
            result.maps[kind.id() as usize] = Some(map);
        }
        // Query each state once while priming the selected maps together.
        // Restored maps do not participate, including their unused padding bits.
        for index in 0..COLUMNS {
            let (x, z) = ((index % 16) as u8, (index / 16) as u8);
            let mut unresolved = to_prime;
            if unresolved == 0 {
                break;
            }
            for y in (source.min_y()..source.highest_section * 16 + 16).rev() {
                let state = source.state(x, y, z);
                if state == source.registries.air_id() {
                    continue;
                }
                let found = source
                    .registries
                    .heightmap_mask(state)
                    .expect("validated state ID")
                    & unresolved;
                for kind in HeightmapKind::ALL {
                    if found & kind.mask() != 0 {
                        result.maps[kind.id() as usize]
                            .as_mut()
                            .expect("selected map")
                            .set(index, y + 1);
                    }
                }
                unresolved &= !found;
                if unresolved == 0 {
                    break;
                }
            }
        }
        Ok(result)
    }
    pub fn get(&self, kind: HeightmapKind) -> Option<&Heightmap> {
        self.maps[kind.id() as usize].as_ref()
    }
    pub fn origin(&self, kind: HeightmapKind) -> Option<HeightmapOrigin> {
        self.origins[kind.id() as usize]
    }
    pub fn heap_bytes(&self) -> usize {
        self.maps.iter().flatten().map(Heightmap::heap_bytes).sum()
    }
}

fn column(x: u8, z: u8) -> Result<usize, HeightmapError> {
    if x < 16 && z < 16 {
        Ok(usize::from(x) + 16 * usize::from(z))
    } else {
        Err(HeightmapError::InvalidColumn)
    }
}
fn section_count(height: DimensionHeight) -> usize {
    (i32::from(height.max_section()) - i32::from(height.min_section()) + 1) as usize
}
fn height_bits(height: DimensionHeight) -> u8 {
    (u32::BITS - (section_count(height) as u32 * 16).leading_zeros()) as u8
}
fn field<'a>(compound: &'a Compound, key: &str) -> Option<&'a Tag> {
    compound
        .entries()
        .iter()
        .find(|entry| entry.name.as_utf16().iter().copied().eq(key.encode_utf16()))
        .map(|entry| &entry.value)
}
