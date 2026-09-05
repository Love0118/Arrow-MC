//! Borrowed visible lighting snapshots to a chunk's packet light fields.
//!
//! This consumes published engine storage, never RawStoredLight. from_ready
//! carries the lighting owner's coherent block/sky revision and domain fence.
//! Standalone snapshot callers must establish the equivalent fence themselves.
//! A visible snapshot alone does not establish send-sync, ticking or Play
//! readiness. Queued, unpublished layer overrides are deliberately not read.

use std::fmt;

use crate::world::{
    lighting::{LightKind, LightSection, owner::ReadyLighting, storage::LightSnapshot},
    preparation::ChunkAddress,
    storage::chunk::DimensionHeight,
};

use super::chunk_packet::{BlockEntity, ChunkWithLight, HeightmapEntry, LightData, LightUpdate};

// DimensionHeight supports at most 256 sections. Packet light additionally
// includes one section below and one above that dimension range.
const MASK_BYTES: usize = 258usize.div_ceil(8);

#[derive(Clone, Copy, Debug, Default)]
pub struct ChangedFilters<'a> {
    /// Bit zero names min_dimension_section - 1. None includes all sections;
    /// Some(empty) includes none. Bits beyond the light range are ignored.
    pub block: Option<&'a [u8]>,
    pub sky: Option<&'a [u8]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    MissingChunk,
    WrongLayer {
        expected: LightKind,
        actual: LightKind,
    },
    AllocationLimit,
    AllocationFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "packet light snapshot: {self:?}")
    }
}
impl std::error::Error for Error {}

/// Four small inline masks and exactly admitted update descriptors. No light
/// payload is cloned: allocated data borrows its leased snapshot and uniform
/// values are expanded only into the final admitted packet output.
pub struct PacketLightSnapshot<'a> {
    position: ChunkAddress,
    min_section: i32,
    sections: usize,
    mask_len: usize,
    block_mask: [u8; MASK_BYTES],
    sky_mask: [u8; MASK_BYTES],
    empty_block_mask: [u8; MASK_BYTES],
    empty_sky_mask: [u8; MASK_BYTES],
    block_updates: Vec<LightUpdate<'a>>,
    sky_updates: Vec<LightUpdate<'a>>,
    // Keep the complete snapshots borrowed even when every layer is uniform.
    _block: Option<&'a LightSnapshot>,
    _sky: Option<&'a LightSnapshot>,
}

impl<'a> PacketLightSnapshot<'a> {
    /// Uses the lighting owner's current complete result and keeps its canonical
    /// source borrow and shared CPU reservation live through packet encoding.
    /// Rejects positions outside that result's explicitly selected domain before
    /// allocating descriptors. This is lighting readiness, not Play/send-sync.
    pub fn from_ready(
        ready: &'a ReadyLighting<'a>,
        position: ChunkAddress,
        filters: ChangedFilters<'_>,
        control_bytes: usize,
    ) -> Result<Self, Error> {
        if !ready.has_chunk(position) {
            return Err(Error::MissingChunk);
        }
        Self::new(
            position,
            ready.height(),
            Some(ready.block()),
            ready.sky(),
            filters,
            control_bytes,
        )
    }

    /// `control_bytes` admits both update descriptor capacities, excluding this
    /// fixed-size value and the separately leased snapshot storage. None for a
    /// light kind represents a disabled engine (including dimensions without
    /// skylight), so that kind contributes neither data nor empty bits.
    pub fn new(
        position: ChunkAddress,
        height: DimensionHeight,
        block: Option<&'a LightSnapshot>,
        sky: Option<&'a LightSnapshot>,
        filters: ChangedFilters<'_>,
        control_bytes: usize,
    ) -> Result<Self, Error> {
        for (snapshot, expected) in [(block, LightKind::Block), (sky, LightKind::Sky)] {
            if let Some(snapshot) = snapshot
                && snapshot.kind() != expected
            {
                return Err(Error::WrongLayer {
                    expected,
                    actual: snapshot.kind(),
                });
            }
        }
        let min_section = i32::from(height.min_section()) - 1;
        let sections = (i32::from(height.max_section()) - min_section + 2) as usize;
        let mut result = Self {
            position,
            min_section,
            sections,
            mask_len: sections.div_ceil(8),
            block_mask: [0; MASK_BYTES],
            sky_mask: [0; MASK_BYTES],
            empty_block_mask: [0; MASK_BYTES],
            empty_sky_mask: [0; MASK_BYTES],
            block_updates: Vec::new(),
            sky_updates: Vec::new(),
            _block: block,
            _sky: sky,
        };
        let block_count = classify(
            block,
            filters.block,
            position,
            min_section,
            sections,
            &mut result.block_mask,
            &mut result.empty_block_mask,
        );
        let sky_count = classify(
            sky,
            filters.sky,
            position,
            min_section,
            sections,
            &mut result.sky_mask,
            &mut result.empty_sky_mask,
        );
        let requested = (block_count + sky_count)
            .checked_mul(size_of::<LightUpdate<'a>>())
            .ok_or(Error::AllocationLimit)?;
        if requested > control_bytes {
            return Err(Error::AllocationLimit);
        }
        let mut remaining = control_bytes;
        result.block_updates = allocate_updates(block_count, &mut remaining)?;
        result.sky_updates = allocate_updates(sky_count, &mut remaining)?;
        if let Some(block) = block {
            collect(
                block,
                position,
                min_section,
                sections,
                &result.block_mask,
                &mut result.block_updates,
            );
        }
        if let Some(sky) = sky {
            collect(
                sky,
                position,
                min_section,
                sections,
                &result.sky_mask,
                &mut result.sky_updates,
            );
        }
        Ok(result)
    }

    pub fn position(&self) -> ChunkAddress {
        self.position
    }

    pub fn min_light_section(&self) -> i32 {
        self.min_section
    }

    pub fn light_section_count(&self) -> usize {
        self.sections
    }

    /// Retained descriptor backing bytes; masks are fixed inline storage.
    pub fn heap_bytes(&self) -> usize {
        (self.block_updates.capacity() + self.sky_updates.capacity()) * size_of::<LightUpdate<'_>>()
    }

    pub fn light_data(&self) -> LightData<'_> {
        LightData {
            sky_mask: &self.sky_mask[..self.mask_len],
            block_mask: &self.block_mask[..self.mask_len],
            empty_sky_mask: &self.empty_sky_mask[..self.mask_len],
            empty_block_mask: &self.empty_block_mask[..self.mask_len],
            sky_updates: &self.sky_updates,
            block_updates: &self.block_updates,
        }
    }

    /// Binds the full packet's coordinates to the selected light column. These
    /// remaining inputs must describe that same current chunk revision; this
    /// convenience method does not synthesize missing world data.
    pub fn chunk_packet<'b>(
        &'b self,
        heightmaps: &'b [HeightmapEntry<'b>],
        sections: &'b [u8],
        block_entities: &'b [BlockEntity<'b>],
    ) -> ChunkWithLight<'b> {
        ChunkWithLight {
            position: self.position,
            heightmaps,
            sections,
            block_entities,
            light: self.light_data(),
        }
    }
}

fn selected(filter: Option<&[u8]>, index: usize) -> bool {
    filter.is_none_or(|bytes| {
        bytes
            .get(index / 8)
            .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
    })
}

fn key(position: ChunkAddress, min_section: i32, index: usize) -> LightSection {
    LightSection {
        x: position.x,
        y: min_section + index as i32,
        z: position.z,
    }
}

fn classify(
    snapshot: Option<&LightSnapshot>,
    filter: Option<&[u8]>,
    position: ChunkAddress,
    min_section: i32,
    sections: usize,
    data: &mut [u8; MASK_BYTES],
    empty: &mut [u8; MASK_BYTES],
) -> usize {
    let Some(snapshot) = snapshot else {
        return 0;
    };
    let mut count = 0;
    for index in 0..sections {
        if !selected(filter, index) {
            continue;
        }
        let Some(layer) = snapshot.layer(key(position, min_section, index)) else {
            continue;
        };
        if layer.is_empty() {
            empty[index / 8] |= 1 << (index % 8);
        } else {
            data[index / 8] |= 1 << (index % 8);
            count += 1;
        }
    }
    count
}

fn allocate_updates<'a>(
    count: usize,
    remaining: &mut usize,
) -> Result<Vec<LightUpdate<'a>>, Error> {
    let requested = count
        .checked_mul(size_of::<LightUpdate<'a>>())
        .ok_or(Error::AllocationLimit)?;
    if requested > *remaining {
        return Err(Error::AllocationLimit);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| Error::AllocationFailed)?;
    let actual = output
        .capacity()
        .checked_mul(size_of::<LightUpdate<'a>>())
        .ok_or(Error::AllocationLimit)?;
    *remaining = remaining
        .checked_sub(actual)
        .ok_or(Error::AllocationLimit)?;
    Ok(output)
}

fn collect<'a>(
    snapshot: &'a LightSnapshot,
    position: ChunkAddress,
    min_section: i32,
    sections: usize,
    mask: &[u8; MASK_BYTES],
    output: &mut Vec<LightUpdate<'a>>,
) {
    for index in 0..sections {
        if mask[index / 8] & (1 << (index % 8)) == 0 {
            continue;
        }
        let layer = snapshot
            .layer(key(position, min_section, index))
            .expect("immutable classified layer");
        output.push(match layer.bytes() {
            Some(bytes) => LightUpdate::Bytes(bytes),
            None => LightUpdate::Uniform(layer.get(0, 0, 0).expect("valid local coordinate") as u8),
        });
    }
}
