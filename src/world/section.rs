//! Registry-ID section storage and the 26.3-pre-2 section network payload.
//!
//! Independently designed from observed palette/word-packing behavior. Network
//! longs have no length prefix in this version. Registry identities and block
//! metadata belong to the caller; this module does not invent air/fluid rules.

mod packed;

use crate::wire::{read_varint, varint_len, write_varint};
use packed::Packed;
use std::fmt;

pub const BLOCKS_PER_SECTION: usize = 4096;
pub const BIOMES_PER_SECTION: usize = 64;
/// Maximum payload for registries with positive signed VarInt IDs (up to 31 bits).
pub const MAX_SECTION_NETWORK_BYTES: usize = 4 + 1 + 2048 * 8 + 1 + 32 * 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidRegistrySize(u32),
    InvalidBits(u8),
    InvalidLength { expected: usize, actual: usize },
    IndexOutOfBounds,
    ValueOutOfRange(u32),
    InvalidPaletteLength(i32),
    InvalidPaletteIndex(u32),
    InvalidCounts,
    Truncated,
    InvalidVarInt,
    AllocationBudgetExceeded,
    AllocationFailed,
    OutputCapacity,
    NonCanonicalPadding,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid section data: {self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registry {
    state_count: u32,
}

impl Registry {
    /// IDs must be contiguous in `0..state_count`; holes require caller validation.
    pub fn new(state_count: u32) -> Result<Self, Error> {
        if state_count == 0 || state_count > 1 << 31 {
            return Err(Error::InvalidRegistrySize(state_count));
        }
        Ok(Self { state_count })
    }

    pub const fn state_count(self) -> u32 {
        self.state_count
    }

    pub const fn bits(self) -> u8 {
        (u32::BITS - (self.state_count - 1).leading_zeros()) as u8
    }

    fn validate(self, value: u32) -> Result<(), Error> {
        if value < self.state_count {
            Ok(())
        } else {
            Err(Error::ValueOutOfRange(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    Blocks,
    Biomes,
}

impl ContainerKind {
    pub const fn len(self) -> usize {
        match self {
            Self::Blocks => BLOCKS_PER_SECTION,
            Self::Biomes => BIOMES_PER_SECTION,
        }
    }

    pub const fn is_empty(self) -> bool {
        false
    }

    pub fn index(self, x: usize, y: usize, z: usize) -> Result<usize, Error> {
        let side = match self {
            Self::Blocks => 16,
            Self::Biomes => 4,
        };
        if x >= side || y >= side || z >= side {
            return Err(Error::IndexOutOfBounds);
        }
        Ok(x + side * (z + side * y))
    }

    const fn min_bits(self) -> u8 {
        match self {
            Self::Blocks => 4,
            Self::Biomes => 1,
        }
    }
    const fn max_bits(self) -> u8 {
        match self {
            Self::Blocks => 8,
            Self::Biomes => 3,
        }
    }

    fn normalized_bits(self, bits: u8, registry: Registry) -> u8 {
        if bits == 0 {
            0
        } else if bits <= self.max_bits() {
            bits.max(self.min_bits())
        } else {
            registry.bits()
        }
    }
}

enum Storage {
    Single(u32),
    Indirect { palette: Vec<u32>, values: Packed },
    Direct(Packed),
}

pub struct PalettedContainer {
    kind: ContainerKind,
    registry: Registry,
    storage: Storage,
}

impl PalettedContainer {
    pub fn single(kind: ContainerKind, registry: Registry, initial: u32) -> Result<Self, Error> {
        registry.validate(initial)?;
        Ok(Self {
            kind,
            registry,
            storage: Storage::Single(initial),
        })
    }

    /// Compacts in increasing linear index order; palette IDs follow first use.
    /// Requested backing payload is checked before allocation; returned Vec
    /// capacity is checked as well. Allocator metadata and fixed stack scratch
    /// are not part of `allocation_limit` (this is not an RSS limit).
    pub fn from_dense(
        kind: ContainerKind,
        registry: Registry,
        input: &[u32],
        allocation_limit: usize,
    ) -> Result<Self, Error> {
        Self::build(kind, registry, input, None, allocation_limit)
    }

    fn build(
        kind: ContainerKind,
        registry: Registry,
        input: &[u32],
        forced_bits: Option<u8>,
        allocation_limit: usize,
    ) -> Result<Self, Error> {
        if input.len() != kind.len() {
            return Err(Error::InvalidLength {
                expected: kind.len(),
                actual: input.len(),
            });
        }
        let mut palette = PaletteScratch::new();
        for &value in input {
            registry.validate(value)?;
            palette.insert(value);
        }
        let bits = forced_bits.unwrap_or_else(|| {
            if palette.len == 1 {
                0
            } else {
                (usize::BITS - (palette.len - 1).leading_zeros()) as u8
            }
        });
        if bits == 0 {
            return Self::single(kind, registry, input[0]);
        }
        let direct = bits > kind.max_bits();
        let bits = kind.normalized_bits(bits, registry);
        let palette_bytes = if direct {
            0
        } else {
            (1_usize << bits) * size_of::<u32>()
        };
        let words_bytes = word_count(bits, kind.len()) * size_of::<u64>();
        if palette_bytes + words_bytes > allocation_limit {
            return Err(Error::AllocationBudgetExceeded);
        }
        let mut values = Packed::new(bits, kind.len(), words_bytes)?;
        let storage = if direct {
            for (index, &id) in input.iter().enumerate() {
                values.set(index, id)?;
            }
            Storage::Direct(values)
        } else {
            let mut entries = Vec::new();
            entries
                .try_reserve_exact(1 << bits)
                .map_err(|_| Error::AllocationFailed)?;
            if entries.capacity() * size_of::<u32>() + values.heap_bytes() > allocation_limit {
                return Err(Error::AllocationBudgetExceeded);
            }
            entries.extend_from_slice(&palette.ids[..palette.len]);
            for (index, &id) in input.iter().enumerate() {
                values.set(index, palette.find(id).unwrap() as u32)?;
            }
            Storage::Indirect {
                palette: entries,
                values,
            }
        };
        Ok(Self {
            kind,
            registry,
            storage,
        })
    }

    pub fn bits(&self) -> u8 {
        match &self.storage {
            Storage::Single(_) => 0,
            Storage::Indirect { values, .. } | Storage::Direct(values) => values.bits(),
        }
    }

    pub fn heap_bytes(&self) -> usize {
        match &self.storage {
            Storage::Single(_) => 0,
            Storage::Indirect { palette, values } => {
                palette.capacity() * size_of::<u32>() + values.heap_bytes()
            }
            Storage::Direct(values) => values.heap_bytes(),
        }
    }

    pub fn get(&self, index: usize) -> Result<u32, Error> {
        if index >= self.kind.len() {
            return Err(Error::IndexOutOfBounds);
        }
        match &self.storage {
            Storage::Single(value) => Ok(*value),
            Storage::Indirect { palette, values } => {
                let key = values.get(index).ok_or(Error::IndexOutOfBounds)?;
                palette
                    .get(key as usize)
                    .copied()
                    .ok_or(Error::InvalidPaletteIndex(key))
            }
            Storage::Direct(values) => values.get(index).ok_or(Error::IndexOutOfBounds),
        }
    }

    /// Adds palette IDs in mutation order. Like Vanilla, ordinary mutations do
    /// not shrink an existing palette. Growth reindexes current contents first.
    /// Budget includes old plus replacement storage at the transition peak.
    pub fn set(&mut self, index: usize, value: u32, allocation_limit: usize) -> Result<u32, Error> {
        self.registry.validate(value)?;
        let old = self.get(index)?;
        match &mut self.storage {
            Storage::Single(current) if *current == value => return Ok(old),
            Storage::Indirect { palette, values } => {
                if let Some(key) = palette.iter().position(|&entry| entry == value) {
                    values.set(index, key as u32)?;
                    return Ok(old);
                }
                if palette.len() < 1 << values.bits() {
                    let key = palette.len();
                    palette.push(value);
                    values.set(index, key as u32)?;
                    return Ok(old);
                }
            }
            Storage::Direct(values) => {
                values.set(index, value)?;
                return Ok(old);
            }
            Storage::Single(_) => {}
        }
        let budget = allocation_limit
            .checked_sub(self.heap_bytes())
            .ok_or(Error::AllocationBudgetExceeded)?;
        let mut dense = [0; BLOCKS_PER_SECTION];
        for (i, id) in dense[..self.kind.len()].iter_mut().enumerate() {
            *id = self.get(i)?;
        }
        let next_bits = (self.bits() + 1).max(self.kind.min_bits());
        let mut replacement = Self::build(
            self.kind,
            self.registry,
            &dense[..self.kind.len()],
            Some(next_bits),
            budget,
        )?;
        // Existing contents are reindexed before the last new value is added.
        replacement.set(index, value, budget)?;
        *self = replacement;
        Ok(old)
    }

    /// Explicit compaction; unlike set, may return a section to a single value.
    pub fn repack(&mut self, allocation_limit: usize) -> Result<(), Error> {
        let budget = allocation_limit
            .checked_sub(self.heap_bytes())
            .ok_or(Error::AllocationBudgetExceeded)?;
        let mut dense = [0; BLOCKS_PER_SECTION];
        for (i, value) in dense[..self.kind.len()].iter_mut().enumerate() {
            *value = self.get(i)?;
        }
        *self = Self::from_dense(self.kind, self.registry, &dense[..self.kind.len()], budget)?;
        Ok(())
    }

    pub fn network_len(&self) -> usize {
        match &self.storage {
            Storage::Single(value) => 1 + varint_len(*value as i32),
            Storage::Indirect { palette, values } => {
                1 + varint_len(palette.len() as i32)
                    + palette
                        .iter()
                        .map(|&id| varint_len(id as i32))
                        .sum::<usize>()
                    + values.words().len() * 8
            }
            Storage::Direct(values) => 1 + values.words().len() * 8,
        }
    }

    /// Appends without allocation. The caller reserves output before submission.
    /// Insufficient spare capacity leaves the output unchanged.
    pub fn write_network(&self, output: &mut Vec<u8>) -> Result<(), Error> {
        if output.capacity() - output.len() < self.network_len() {
            return Err(Error::OutputCapacity);
        }
        output.push(self.bits());
        match &self.storage {
            Storage::Single(value) => append_varint(output, *value),
            Storage::Indirect { palette, values } => {
                append_varint(output, palette.len() as u32);
                for &id in palette {
                    append_varint(output, id);
                }
                append_words(output, values.words());
            }
            Storage::Direct(values) => append_words(output, values.words()),
        }
        Ok(())
    }

    /// Decodes one container and preserves trailing packet bytes. Input remains
    /// unchanged on failure. Vanilla's bit-header normalization is preserved;
    /// unused padding is normalized to zero. Invalid IDs/indices are rejected
    /// eagerly rather than failing on a later get. Allocation is bounded before
    /// construction, including palette spare capacity for later mutations.
    pub fn read_network(
        input: &mut &[u8],
        kind: ContainerKind,
        registry: Registry,
        allocation_limit: usize,
    ) -> Result<Self, Error> {
        let mut cursor = *input;
        let header = take(&mut cursor, 1)?[0];
        let bits = kind.normalized_bits(header, registry);
        if header == 0 {
            let value = read_id(&mut cursor)?;
            let result = Self::single(kind, registry, value)?;
            *input = cursor;
            return Ok(result);
        }
        if bits == 0 {
            return Err(Error::InvalidBits(header));
        }
        let direct = header > kind.max_bits();
        let mut scratch = [0; 256];
        let palette_len = if direct {
            0
        } else {
            let count = read_int(&mut cursor)?;
            if count < 1 || count as usize > 1 << bits {
                return Err(Error::InvalidPaletteLength(count));
            }
            for entry in &mut scratch[..count as usize] {
                *entry = read_id(&mut cursor)?;
                registry.validate(*entry)?;
            }
            count as usize
        };
        let count = word_count(bits, kind.len());
        let bytes = take(&mut cursor, count * 8)?;
        // Validate all IDs before reserving either vector.
        let per_word = 64 / usize::from(bits);
        let mask = (1_u64 << bits) - 1;
        for index in 0..kind.len() {
            let word = u64::from_be_bytes(bytes[index / per_word * 8..][..8].try_into().unwrap());
            let key = ((word >> (index % per_word * usize::from(bits))) & mask) as u32;
            if direct {
                registry.validate(key)?;
            } else if key as usize >= palette_len {
                return Err(Error::InvalidPaletteIndex(key));
            }
        }
        let palette_bytes = if direct {
            0
        } else {
            (1 << bits) * size_of::<u32>()
        };
        if count * 8 + palette_bytes > allocation_limit {
            return Err(Error::AllocationBudgetExceeded);
        }
        let mut words = Vec::new();
        words
            .try_reserve_exact(count)
            .map_err(|_| Error::AllocationFailed)?;
        if words.capacity() * size_of::<u64>() + palette_bytes > allocation_limit {
            return Err(Error::AllocationBudgetExceeded);
        }
        for (index, bytes) in bytes.chunks_exact(8).enumerate() {
            let entries = per_word.min(kind.len() - index * per_word);
            let used_bits = entries * usize::from(bits);
            let word_mask = u64::MAX >> (64 - used_bits);
            words.push(u64::from_be_bytes(bytes.try_into().unwrap()) & word_mask);
        }
        let values = Packed::from_words(bits, kind.len(), words)?;
        let storage = if direct {
            Storage::Direct(values)
        } else {
            let mut palette = Vec::new();
            palette
                .try_reserve_exact(1 << bits)
                .map_err(|_| Error::AllocationFailed)?;
            if palette.capacity() * size_of::<u32>() + values.heap_bytes() > allocation_limit {
                return Err(Error::AllocationBudgetExceeded);
            }
            palette.extend_from_slice(&scratch[..palette_len]);
            Storage::Indirect { palette, values }
        };
        *input = cursor;
        Ok(Self {
            kind,
            registry,
            storage,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionCounts {
    pub non_empty_blocks: u16,
    pub fluid_blocks: u16,
}

impl SectionCounts {
    fn validate(self) -> Result<(), Error> {
        if usize::from(self.non_empty_blocks) > BLOCKS_PER_SECTION
            || self.fluid_blocks > self.non_empty_blocks
        {
            Err(Error::InvalidCounts)
        } else {
            Ok(())
        }
    }
}

/// Caller-supplied section metadata plus its two registry-ID containers.
pub struct Section {
    pub counts: SectionCounts,
    pub blocks: PalettedContainer,
    pub biomes: PalettedContainer,
}

impl Section {
    pub fn read_network(
        input: &mut &[u8],
        block_registry: Registry,
        biome_registry: Registry,
        allocation_limit: usize,
    ) -> Result<Self, Error> {
        let mut cursor = *input;
        let bytes = take(&mut cursor, 4)?;
        let counts = SectionCounts {
            non_empty_blocks: u16::from_be_bytes([bytes[0], bytes[1]]),
            fluid_blocks: u16::from_be_bytes([bytes[2], bytes[3]]),
        };
        counts.validate()?;
        let blocks = PalettedContainer::read_network(
            &mut cursor,
            ContainerKind::Blocks,
            block_registry,
            allocation_limit,
        )?;
        let remaining = allocation_limit
            .checked_sub(blocks.heap_bytes())
            .ok_or(Error::AllocationBudgetExceeded)?;
        let biomes = PalettedContainer::read_network(
            &mut cursor,
            ContainerKind::Biomes,
            biome_registry,
            remaining,
        )?;
        *input = cursor;
        Ok(Self {
            counts,
            blocks,
            biomes,
        })
    }

    pub fn write_network(&self, output: &mut Vec<u8>) -> Result<(), Error> {
        self.counts.validate()?;
        if self.blocks.kind != ContainerKind::Blocks || self.biomes.kind != ContainerKind::Biomes {
            return Err(Error::InvalidLength {
                expected: BLOCKS_PER_SECTION,
                actual: self.blocks.kind.len(),
            });
        }
        if output.capacity() - output.len()
            < 4 + self.blocks.network_len() + self.biomes.network_len()
        {
            return Err(Error::OutputCapacity);
        }
        append_counts(output, self.counts);
        self.blocks.write_network(output)?;
        self.biomes.write_network(output)
    }
}

/// Fixed-array synchronous worker kernel: no heap allocations, under 5 KiB of
/// scratch, and at most MAX_SECTION_NETWORK_BYTES appended. The caller must
/// reserve input/output payloads before allocating or copying them. Output is
/// unchanged on every error. The two counts must come from registry metadata.
pub fn prepare_section(
    blocks: &[u32; BLOCKS_PER_SECTION],
    biomes: &[u32; BIOMES_PER_SECTION],
    block_registry: Registry,
    biome_registry: Registry,
    counts: SectionCounts,
    output: &mut Vec<u8>,
) -> Result<(), Error> {
    counts.validate()?;
    if output.capacity() - output.len() < MAX_SECTION_NETWORK_BYTES {
        return Err(Error::OutputCapacity);
    }
    for &id in blocks {
        block_registry.validate(id)?;
    }
    for &id in biomes {
        biome_registry.validate(id)?;
    }
    append_counts(output, counts);
    encode_dense(ContainerKind::Blocks, block_registry, blocks, output);
    encode_dense(ContainerKind::Biomes, biome_registry, biomes, output);
    Ok(())
}

// Fixed open-addressed scratch preserves first-use order without allocating a
// per-section HashMap. A 257th distinct value selects direct block storage.
struct PaletteScratch {
    ids: [u32; 256],
    slots: [u16; 512],
    len: usize,
}

impl PaletteScratch {
    fn new() -> Self {
        Self {
            ids: [0; 256],
            slots: [0; 512],
            len: 0,
        }
    }
    fn slot(value: u32) -> usize {
        (value.wrapping_mul(0x9e37_79b9) >> 23) as usize
    }
    fn find(&self, value: u32) -> Option<usize> {
        let mut slot = Self::slot(value);
        loop {
            let entry = self.slots[slot];
            if entry == 0 {
                return None;
            }
            let index = usize::from(entry - 1);
            if self.ids[index] == value {
                return Some(index);
            }
            slot = (slot + 1) & 511;
        }
    }
    fn insert(&mut self, value: u32) {
        if self.len > 256 || self.find(value).is_some() {
            return;
        }
        if self.len == 256 {
            self.len = 257;
            return;
        }
        let index = self.len;
        self.ids[index] = value;
        self.len += 1;
        let mut slot = Self::slot(value);
        while self.slots[slot] != 0 {
            slot = (slot + 1) & 511;
        }
        self.slots[slot] = (index + 1) as u16;
    }
}

fn encode_dense(kind: ContainerKind, registry: Registry, input: &[u32], output: &mut Vec<u8>) {
    let mut palette = PaletteScratch::new();
    for &id in input {
        palette.insert(id);
    }
    if palette.len == 1 {
        output.push(0);
        append_varint(output, input[0]);
        return;
    }
    let needed = (usize::BITS - (palette.len - 1).leading_zeros()) as u8;
    let direct = needed > kind.max_bits();
    let bits = kind.normalized_bits(needed, registry);
    output.push(bits);
    if !direct {
        append_varint(output, palette.len as u32);
        for &id in &palette.ids[..palette.len] {
            append_varint(output, id);
        }
    }
    let per_word = 64 / usize::from(bits);
    for group in input.chunks(per_word) {
        let mut word = 0_u64;
        for (index, &id) in group.iter().enumerate() {
            let value = if direct {
                id
            } else {
                palette.find(id).unwrap() as u32
            };
            word |= u64::from(value) << (index * usize::from(bits));
        }
        output.extend_from_slice(&word.to_be_bytes());
    }
}

fn word_count(bits: u8, len: usize) -> usize {
    len.div_ceil(64 / usize::from(bits))
}
fn append_counts(output: &mut Vec<u8>, counts: SectionCounts) {
    output.extend_from_slice(&counts.non_empty_blocks.to_be_bytes());
    output.extend_from_slice(&counts.fluid_blocks.to_be_bytes());
}
fn append_varint(output: &mut Vec<u8>, value: u32) {
    let mut bytes = [0; 5];
    let length = write_varint(value as i32, &mut bytes).unwrap();
    output.extend_from_slice(&bytes[..length]);
}
fn append_words(output: &mut Vec<u8>, words: &[u64]) {
    for word in words {
        output.extend_from_slice(&word.to_be_bytes());
    }
}
fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], Error> {
    let bytes = input.get(..count).ok_or(Error::Truncated)?;
    *input = &input[count..];
    Ok(bytes)
}
fn read_int(input: &mut &[u8]) -> Result<i32, Error> {
    let (value, count) = read_varint(input).map_err(|e| match e {
        crate::wire::DecodeError::Incomplete => Error::Truncated,
        crate::wire::DecodeError::TooLong => Error::InvalidVarInt,
    })?;
    *input = &input[count..];
    Ok(value)
}
fn read_id(input: &mut &[u8]) -> Result<u32, Error> {
    Ok(read_int(input)? as u32)
}
