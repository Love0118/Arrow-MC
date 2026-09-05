//! Current 5018 disk palettes and retained chunk metadata, before world activation.

use super::registry::ChunkRegistrySnapshot;
use crate::nbt::{self, Compound, Tag};
use crate::world::section::{ContainerKind, PalettedContainer, Section, SectionCounts};
use std::fmt;

pub const DATA_VERSION: i32 = 5018;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DimensionHeight {
    min_section: i8,
    max_section: i8,
}

impl DimensionHeight {
    pub fn new(min_y: i32, height: u32) -> Result<Self, ChunkDecodeError> {
        if height == 0 || min_y % 16 != 0 || !height.is_multiple_of(16) {
            return Err(ChunkDecodeError::InvalidHeight);
        }
        let min_section = min_y.div_euclid(16);
        let max_section = i64::from(min_section) + i64::from(height / 16) - 1;
        if min_section < i32::from(i8::MIN) || max_section > i64::from(i8::MAX) {
            return Err(ChunkDecodeError::InvalidHeight);
        }
        Ok(Self {
            min_section: min_section as i8,
            max_section: max_section as i8,
        })
    }
    pub fn contains(self, y: i8) -> bool {
        y >= self.min_section && y <= self.max_section
    }
    pub fn min_section(self) -> i8 {
        self.min_section
    }
    pub fn max_section(self) -> i8 {
        self.max_section
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkStatus {
    Empty,
    StructureStarts,
    StructureReferences,
    Biomes,
    Terrain,
    Features,
    InitializeLight,
    Light,
    Spawn,
    Full,
}

impl ChunkStatus {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Empty => "minecraft:empty",
            Self::StructureStarts => "minecraft:structure_starts",
            Self::StructureReferences => "minecraft:structure_references",
            Self::Biomes => "minecraft:biomes",
            Self::Terrain => "minecraft:terrain",
            Self::Features => "minecraft:features",
            Self::InitializeLight => "minecraft:initialize_light",
            Self::Light => "minecraft:light",
            Self::Spawn => "minecraft:spawn",
            Self::Full => "minecraft:full",
        }
    }
}

#[derive(Debug)]
pub enum ChunkDecodeError {
    Truncated,
    RootType,
    Nbt(nbt::Error),
    NeedsUpgrade(i32),
    UnsupportedDataVersion(i32),
    MissingLevelData,
    InvalidHeight,
    MissingPalette,
    EmptyPalette,
    InvalidPalette,
    MissingPackedData,
    PackedLength { expected: usize, actual: usize },
    PaletteIndex(u32),
    LightLength(usize),
    AllocationLimit,
    AllocationFailed,
    Section(crate::world::section::Error),
}

impl fmt::Display for ChunkDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid stored chunk: {self:?}")
    }
}
impl std::error::Error for ChunkDecodeError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeWarnings {
    pub unknown_status: bool,
    pub fallback_palette_entries: usize,
}

pub struct StoredSection {
    pub y: i8,
    pub section: Option<Section>,
    pub block_light: Option<Vec<u8>>,
    pub sky_light: Option<Vec<u8>>,
}

/// Raw NBT remains owned and intact for future tick/entity/structure activation.
/// Typed palette/light copies coexist with it under a separately admitted cap.
/// No field is silently discarded or replaced by an empty gameplay placeholder.
pub struct StoredChunkDraft {
    pub position: (i32, i32),
    pub data_version: i32,
    pub status: ChunkStatus,
    pub last_update: i64,
    pub inhabited_time: i64,
    pub light_correct: bool,
    pub warnings: DecodeWarnings,
    sections: Vec<StoredSection>,
    root: Compound,
    retained_bytes: usize,
}

impl StoredChunkDraft {
    pub fn sections(&self) -> &[StoredSection] {
        &self.sections
    }
    pub fn root(&self) -> &Compound {
        &self.root
    }
    /// Conservative backing-byte allowance: NBT cumulative allocation charges
    /// plus retained typed storage. Excludes allocator metadata and shared registry.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

impl Drop for StoredChunkDraft {
    fn drop(&mut self) {
        Tag::Compound(std::mem::take(&mut self.root)).drop_iterative();
    }
}

/// Removes the disk root name in-place (Vanilla skips, rather than decodes it)
/// and parses one compound. Bytes after its root are permitted, as by NbtIo.
pub fn decode_current_chunk(
    bytes: &mut Vec<u8>,
    registries: &ChunkRegistrySnapshot,
    height: DimensionHeight,
    nbt_limits: nbt::Limits,
    decoded_bytes: usize,
) -> Result<StoredChunkDraft, ChunkDecodeError> {
    if bytes.first().copied().ok_or(ChunkDecodeError::Truncated)? != 10 {
        return Err(ChunkDecodeError::RootType);
    }
    let name = bytes.get(1..3).ok_or(ChunkDecodeError::Truncated)?;
    let skip = 3 + usize::from(u16::from_be_bytes([name[0], name[1]]));
    if bytes.len() < skip {
        return Err(ChunkDecodeError::Truncated);
    }
    bytes.copy_within(skip.., 1);
    bytes.truncate(bytes.len() - skip + 1);
    let (tag, nbt_allocated) = nbt::read_network_accounted(&mut bytes.as_slice(), nbt_limits)
        .map_err(ChunkDecodeError::Nbt)?;
    let Tag::Compound(root) = tag else {
        unreachable!("root type checked before parse")
    };
    parse_current_chunk(root, nbt_allocated, registries, height, decoded_bytes)
}

pub(crate) fn parse_current_chunk(
    root: Compound,
    nbt_allocated: usize,
    registries: &ChunkRegistrySnapshot,
    height: DimensionHeight,
    decoded_bytes: usize,
) -> Result<StoredChunkDraft, ChunkDecodeError> {
    let mut draft = StoredChunkDraft {
        position: (0, 0),
        data_version: DATA_VERSION,
        status: ChunkStatus::Empty,
        last_update: 0,
        inhabited_time: 0,
        light_correct: false,
        warnings: DecodeWarnings::default(),
        sections: Vec::new(),
        root,
        retained_bytes: nbt_allocated,
    };
    draft.data_version = field(&draft.root, "DataVersion")
        .and_then(Tag::as_int)
        .unwrap_or(-1);
    if draft.data_version < DATA_VERSION {
        return Err(ChunkDecodeError::NeedsUpgrade(draft.data_version));
    }
    if draft.data_version > DATA_VERSION {
        return Err(ChunkDecodeError::UnsupportedDataVersion(draft.data_version));
    }
    let Some(Tag::String(status)) = field(&draft.root, "Status") else {
        return Err(ChunkDecodeError::MissingLevelData);
    };
    let statuses = [
        ChunkStatus::Empty,
        ChunkStatus::StructureStarts,
        ChunkStatus::StructureReferences,
        ChunkStatus::Biomes,
        ChunkStatus::Terrain,
        ChunkStatus::Features,
        ChunkStatus::InitializeLight,
        ChunkStatus::Light,
        ChunkStatus::Spawn,
        ChunkStatus::Full,
    ];
    draft.status = statuses
        .into_iter()
        .find(|value| {
            let bare = value.name().strip_prefix("minecraft:").unwrap();
            text_equals(status.as_utf16(), value.name())
                || text_equals(status.as_utf16(), bare)
                || (status.as_utf16().first() == Some(&u16::from(b':'))
                    && text_equals(&status.as_utf16()[1..], bare))
        })
        .unwrap_or_else(|| {
            draft.warnings.unknown_status = true;
            ChunkStatus::Empty
        });
    draft.position = (
        field(&draft.root, "xPos")
            .and_then(Tag::as_int)
            .unwrap_or(0),
        field(&draft.root, "zPos")
            .and_then(Tag::as_int)
            .unwrap_or(0),
    );
    draft.last_update = field(&draft.root, "LastUpdate")
        .and_then(Tag::as_long)
        .unwrap_or(0);
    draft.inhabited_time = field(&draft.root, "InhabitedTime")
        .and_then(Tag::as_long)
        .unwrap_or(0);
    draft.light_correct = field(&draft.root, "isLightOn")
        .and_then(Tag::as_byte)
        .unwrap_or(0)
        != 0;
    let mut budget = DecodeBudget {
        remaining: decoded_bytes,
        retained: 0,
    };
    if let Some(Tag::List(entries)) = field(&draft.root, "sections") {
        let count = entries
            .iter()
            .filter(|tag| matches!(tag, Tag::Compound(_)))
            .count();
        budget.reserve(&mut draft.sections, count)?;
        for entry in entries {
            let Tag::Compound(entry) = entry else {
                continue;
            };
            let y = field(entry, "Y").and_then(Tag::as_byte).unwrap_or(0);
            let section = if height.contains(y) {
                let blocks = palette(
                    field(entry, "block_states"),
                    ContainerKind::Blocks,
                    registries,
                    &mut budget,
                    &mut draft.warnings,
                )?;
                let biomes = palette(
                    field(entry, "biomes"),
                    ContainerKind::Biomes,
                    registries,
                    &mut budget,
                    &mut draft.warnings,
                )?;
                let mut non_empty_blocks = 0;
                let mut fluid_blocks = 0;
                for index in 0..4096 {
                    let id = blocks.get(index).map_err(ChunkDecodeError::Section)?;
                    let flags = registries
                        .state_flags(id)
                        .ok_or(ChunkDecodeError::PaletteIndex(id))?;
                    if !flags.is_air {
                        non_empty_blocks += 1;
                        if flags.has_fluid {
                            fluid_blocks += 1;
                        }
                    }
                }
                Some(Section {
                    counts: SectionCounts {
                        non_empty_blocks,
                        fluid_blocks,
                    },
                    blocks,
                    biomes,
                })
            } else {
                None
            };
            let block_light = light(field(entry, "BlockLight"), &mut budget)?;
            let sky_light = light(field(entry, "SkyLight"), &mut budget)?;
            draft.sections.push(StoredSection {
                y,
                section,
                block_light,
                sky_light,
            });
        }
    }
    draft.retained_bytes = draft
        .retained_bytes
        .checked_add(budget.retained)
        .ok_or(ChunkDecodeError::AllocationLimit)?;
    Ok(draft)
}

fn palette(
    value: Option<&Tag>,
    kind: ContainerKind,
    registries: &ChunkRegistrySnapshot,
    budget: &mut DecodeBudget,
    warnings: &mut DecodeWarnings,
) -> Result<PalettedContainer, ChunkDecodeError> {
    let (registry, default) = match kind {
        ContainerKind::Blocks => (registries.block_registry(), registries.air_id()),
        ContainerKind::Biomes => (registries.biome_registry(), registries.plains_id()),
    };
    let Some(Tag::Compound(container)) = value else {
        return PalettedContainer::single(kind, registry, default)
            .map_err(ChunkDecodeError::Section);
    };
    let Some(palette) = field(container, "palette").and_then(Collection::new) else {
        return Err(ChunkDecodeError::MissingPalette);
    };
    if palette.len() == 0 {
        return Err(ChunkDecodeError::EmptyPalette);
    }
    let mut ids = Vec::new();
    budget.reserve_temporary(&mut ids, palette.len())?;
    for index in 0..palette.len() {
        let resolved = match palette {
            Collection::Tags(entries) => match kind {
                ContainerKind::Blocks => registries.block_state(&entries[index]),
                ContainerKind::Biomes => registries.biome(&entries[index]),
            },
            // NbtOps list codecs accept primitive arrays too. Each numeric
            // palette element fails the registry codec and uses its default.
            _ => super::registry::ResolvedId {
                id: default,
                used_fallback: true,
            },
        };
        warnings.fallback_palette_entries = warnings
            .fallback_palette_entries
            .saturating_add(usize::from(resolved.used_fallback));
        ids.push(resolved.id);
    }
    if ids.len() == 1 {
        return PalettedContainer::single(kind, registry, ids[0])
            .map_err(ChunkDecodeError::Section);
    }
    let bits = (usize::BITS - (ids.len() - 1).leading_zeros()).max(match kind {
        ContainerKind::Blocks => 4,
        ContainerKind::Biomes => 1,
    }) as usize;
    if bits > 31 {
        return Err(ChunkDecodeError::InvalidPalette);
    }
    let Some(words) = field(container, "data").and_then(Collection::new) else {
        return Err(ChunkDecodeError::MissingPackedData);
    };
    if (0..words.len()).any(|index| words.long(index).is_none()) {
        return Err(ChunkDecodeError::MissingPackedData);
    }
    let per_word = 64 / bits;
    let expected = kind.len().div_ceil(per_word);
    if words.len() != expected {
        return Err(ChunkDecodeError::PackedLength {
            expected,
            actual: words.len(),
        });
    }
    let mut dense = [0; 4096];
    let mask = (1_u64 << bits) - 1;
    for (index, id) in dense[..kind.len()].iter_mut().enumerate() {
        let slot = ((words.long(index / per_word).unwrap() as u64 >> (index % per_word * bits))
            & mask) as u32;
        *id = *ids
            .get(slot as usize)
            .ok_or(ChunkDecodeError::PaletteIndex(slot))?;
    }
    let output =
        PalettedContainer::from_dense(kind, registry, &dense[..kind.len()], budget.remaining)
            .map_err(ChunkDecodeError::Section)?;
    budget.charge(output.heap_bytes())?;
    budget.retained += output.heap_bytes();
    Ok(output)
}

#[derive(Clone, Copy)]
enum Collection<'a> {
    Tags(&'a [Tag]),
    Bytes(&'a [i8]),
    Ints(&'a [i32]),
    Longs(&'a [i64]),
}
impl<'a> Collection<'a> {
    fn new(tag: &'a Tag) -> Option<Self> {
        Some(match tag {
            Tag::List(values) => Self::Tags(values),
            Tag::ByteArray(values) => Self::Bytes(values),
            Tag::IntArray(values) => Self::Ints(values),
            Tag::LongArray(values) => Self::Longs(values),
            _ => return None,
        })
    }
    fn len(self) -> usize {
        match self {
            Self::Tags(values) => values.len(),
            Self::Bytes(values) => values.len(),
            Self::Ints(values) => values.len(),
            Self::Longs(values) => values.len(),
        }
    }
    fn long(self, index: usize) -> Option<i64> {
        match self {
            Self::Bytes(values) => values.get(index).map(|&value| i64::from(value)),
            Self::Ints(values) => values.get(index).map(|&value| i64::from(value)),
            Self::Longs(values) => values.get(index).copied(),
            Self::Tags(values) => match values.get(index)? {
                // DynamicOps LONG_STREAM calls java.lang.Number.longValue,
                // which truncates both Float and Double, unlike DoubleTag.
                Tag::Float(value) => Some(*value as i64),
                Tag::Double(value) => Some(*value as i64),
                value => value.as_long(),
            },
        }
    }
}

fn light(
    value: Option<&Tag>,
    budget: &mut DecodeBudget,
) -> Result<Option<Vec<u8>>, ChunkDecodeError> {
    let Some(Tag::ByteArray(bytes)) = value else {
        return Ok(None);
    };
    if bytes.len() != 2048 {
        return Err(ChunkDecodeError::LightLength(bytes.len()));
    }
    let mut output = Vec::new();
    budget.reserve(&mut output, 2048)?;
    output.extend(bytes.iter().map(|&byte| byte as u8));
    Ok(Some(output))
}

struct DecodeBudget {
    remaining: usize,
    retained: usize,
}
impl DecodeBudget {
    fn charge(&mut self, bytes: usize) -> Result<(), ChunkDecodeError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(ChunkDecodeError::AllocationLimit)?;
        Ok(())
    }
    fn reserve<T>(&mut self, values: &mut Vec<T>, count: usize) -> Result<(), ChunkDecodeError> {
        self.reserve_temporary(values, count)?;
        self.retained += values.capacity() * size_of::<T>();
        Ok(())
    }
    fn reserve_temporary<T>(
        &mut self,
        values: &mut Vec<T>,
        count: usize,
    ) -> Result<(), ChunkDecodeError> {
        let bytes = count
            .checked_mul(size_of::<T>())
            .ok_or(ChunkDecodeError::AllocationLimit)?;
        self.charge(bytes)?;
        values
            .try_reserve_exact(count)
            .map_err(|_| ChunkDecodeError::AllocationFailed)?;
        let actual = values.capacity() * size_of::<T>();
        if actual > bytes {
            self.charge(actual - bytes)?;
        }
        Ok(())
    }
}

pub(crate) fn field<'a>(compound: &'a Compound, name: &str) -> Option<&'a Tag> {
    compound
        .entries()
        .iter()
        .find(|entry| text_equals(entry.name.as_utf16(), name))
        .map(|entry| &entry.value)
}
fn text_equals(units: &[u16], ascii: &str) -> bool {
    units.iter().copied().eq(ascii.bytes().map(u16::from))
}
