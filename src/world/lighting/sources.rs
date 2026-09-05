//! Per-column skylight entry edges over immutable canonical block snapshots.
//! The cache stores the first blocked edge from above, not a terrain heightmap.

use super::{LightBlock, LightSection, LightingSource};
use crate::world::{
    preparation::ChunkAddress,
    storage::registry::{ChunkRegistrySnapshot, EMPTY_FACE},
};
use std::fmt;

pub const NEGATIVE_INFINITY: i32 = i32::MIN;
const COLUMNS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourcesError {
    MissingChunk,
    InvalidCoordinate,
    ContextMismatch,
    InvalidMaterial(u32),
    InvalidFace,
}
impl fmt::Display for SourcesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sky sources: {self:?}")
    }
}
impl std::error::Error for SourcesError {}

#[derive(Clone, Debug)]
pub struct SkySources {
    chunk: ChunkAddress,
    min_y: i32,
    max_y: i32,
    registry: [u8; 32],
    /// min_y - 1 represents a column open below the build range.
    edges: [i32; COLUMNS],
}

impl SkySources {
    pub fn initialize(source: &LightingSource, chunk: ChunkAddress) -> Result<Self, SourcesError> {
        if !source.has_chunk(chunk) {
            return Err(SourcesError::MissingChunk);
        }
        let min_section = i32::from(source.height().min_section());
        let max_section = i32::from(source.height().max_section());
        let min_y = min_section * 16;
        let mut result = Self {
            chunk,
            min_y,
            max_y: (max_section + 1) * 16 - 1,
            registry: source.registry().manifest_sha256(),
            edges: [min_y - 1; COLUMNS],
        };

        // Shared section traversal avoids checking the same empty-section count
        // once per column. Resolved columns leave the active set permanently.
        let air = source.registry().air_id();
        let mut above = [air; COLUMNS];
        let mut unresolved = [u64::MAX; 4];
        for section_y in (min_section..=max_section).rev() {
            if unresolved.iter().all(|&word| word == 0) {
                break;
            }
            if source.section_has_only_air(LightSection {
                x: chunk.x,
                y: section_y,
                z: chunk.z,
            }) {
                above.fill(air);
                continue;
            }
            for y in (section_y * 16..section_y * 16 + 16).rev() {
                for (word_index, word) in unresolved.iter_mut().enumerate() {
                    let mut columns = *word;
                    while columns != 0 {
                        let bit = columns.trailing_zeros() as usize;
                        let index = word_index * 64 + bit;
                        columns &= columns - 1;
                        let state = source
                            .state_in_chunk(chunk, (index & 15) as u8, y, (index >> 4) as u8)
                            .ok_or(SourcesError::MissingChunk)?;
                        if blocked(source.registry(), above[index], state)? {
                            result.edges[index] = y + 1;
                            *word &= !(1u64 << bit);
                        } else {
                            above[index] = state;
                        }
                    }
                }
            }
        }
        Ok(result)
    }

    /// The supplied snapshot already contains the block mutation. Only the two
    /// affected vertical edges can raise the cache or invalidate its old edge.
    pub fn update(
        &mut self,
        source: &LightingSource,
        pos: LightBlock,
    ) -> Result<bool, SourcesError> {
        if source.height().min_section() as i32 * 16 != self.min_y
            || (source.height().max_section() as i32 + 1) * 16 - 1 != self.max_y
            || source.registry().manifest_sha256() != self.registry
        {
            return Err(SourcesError::ContextMismatch);
        }
        if pos.x.div_euclid(16) != self.chunk.x
            || pos.z.div_euclid(16) != self.chunk.z
            || !(self.min_y..=self.max_y).contains(&pos.y)
        {
            return Err(SourcesError::InvalidCoordinate);
        }
        if !source.has_chunk(self.chunk) {
            return Err(SourcesError::MissingChunk);
        }
        let x = pos.x.rem_euclid(16) as u8;
        let z = pos.z.rem_euclid(16) as u8;
        let index = usize::from(z) * 16 + usize::from(x);
        let old = self.edges[index];
        if pos.y + 1 < old {
            return Ok(false);
        }
        let mut above = self.state(source, x, pos.y + 1, z)?;
        for edge in [pos.y + 1, pos.y] {
            let below = self.state(source, x, edge - 1, z)?;
            let occluded = blocked(source.registry(), above, below)?;
            let replacement = if occluded && edge > old {
                Some(edge)
            } else if !occluded && edge == old {
                Some(self.scan_below(source, x, edge - 1, z, below)?)
            } else {
                None
            };
            if let Some(value) = replacement {
                self.edges[index] = value;
                return Ok(true);
            }
            above = below;
        }
        Ok(false)
    }

    pub fn lowest_source_y(&self, x: u8, z: u8) -> Result<i32, SourcesError> {
        if x >= 16 || z >= 16 {
            return Err(SourcesError::InvalidCoordinate);
        }
        Ok(self.public_edge(self.edges[usize::from(z) * 16 + usize::from(x)]))
    }

    pub fn highest_lowest_source_y(&self) -> i32 {
        self.public_edge(*self.edges.iter().max().unwrap())
    }

    fn public_edge(&self, edge: i32) -> i32 {
        if edge == self.min_y - 1 {
            NEGATIVE_INFINITY
        } else {
            edge
        }
    }

    fn state(&self, source: &LightingSource, x: u8, y: i32, z: u8) -> Result<u32, SourcesError> {
        if !(self.min_y..=self.max_y).contains(&y) {
            Ok(source.registry().air_id())
        } else {
            source
                .state_in_chunk(self.chunk, x, y, z)
                .ok_or(SourcesError::MissingChunk)
        }
    }

    fn scan_below(
        &self,
        source: &LightingSource,
        x: u8,
        mut y: i32,
        z: u8,
        mut state: u32,
    ) -> Result<i32, SourcesError> {
        while y >= self.min_y {
            let lower = self.state(source, x, y - 1, z)?;
            if blocked(source.registry(), state, lower)? {
                return Ok(y);
            }
            y -= 1;
            state = lower;
        }
        Ok(self.min_y - 1)
    }
}

fn blocked(registry: &ChunkRegistrySnapshot, above: u32, below: u32) -> Result<bool, SourcesError> {
    let bottom = registry
        .light_material(below)
        .ok_or(SourcesError::InvalidMaterial(below))?;
    if bottom.dampening != 0 {
        return Ok(true);
    }
    let top = registry
        .light_material(above)
        .ok_or(SourcesError::InvalidMaterial(above))?;
    let top_face = if top.empty_shape() {
        EMPTY_FACE
    } else {
        top.faces[0]
    };
    let bottom_face = if bottom.empty_shape() {
        EMPTY_FACE
    } else {
        bottom.faces[1]
    };
    registry
        .face_occludes(top_face, bottom_face)
        .ok_or(SourcesError::InvalidFace)
}
