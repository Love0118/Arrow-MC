//! Block/sky light data and propagation over admitted immutable source snapshots.

pub mod block;
pub mod layer;
pub mod owner;
pub mod queue;
pub mod sky;
pub mod source;
pub mod sources;
pub mod storage;
pub mod work;
pub use source::{LightingChunk, LightingSource, SourceLimits, SourceStamp};

use super::preparation::ChunkAddress;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LightBlock {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}
impl LightBlock {
    pub fn section(self) -> LightSection {
        LightSection {
            x: self.x >> 4,
            y: self.y >> 4,
            z: self.z >> 4,
        }
    }
    pub fn column(self) -> ChunkAddress {
        self.section().column()
    }
    pub fn offset(self, x: i32, y: i32, z: i32) -> Self {
        Self {
            x: self.x.wrapping_add(x),
            y: self.y.wrapping_add(y),
            z: self.z.wrapping_add(z),
        }
    }
    pub fn local_index(self) -> usize {
        ((self.y & 15) << 8 | (self.z & 15) << 4 | (self.x & 15)) as usize
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LightSection {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}
impl LightSection {
    pub fn column(self) -> ChunkAddress {
        ChunkAddress {
            x: self.x,
            z: self.z,
        }
    }
    pub fn offset(self, x: i32, y: i32, z: i32) -> Self {
        Self {
            x: self.x.wrapping_add(x),
            y: self.y.wrapping_add(y),
            z: self.z.wrapping_add(z),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    Block,
    Sky,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LightDirection {
    Down,
    Up,
    North,
    South,
    West,
    East,
}
impl LightDirection {
    pub const ALL: [Self; 6] = [
        Self::Down,
        Self::Up,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];
    pub const fn opposite(self) -> Self {
        match self {
            Self::Down => Self::Up,
            Self::Up => Self::Down,
            Self::North => Self::South,
            Self::South => Self::North,
            Self::West => Self::East,
            Self::East => Self::West,
        }
    }
    pub const fn vector(self) -> (i32, i32, i32) {
        match self {
            Self::Down => (0, -1, 0),
            Self::Up => (0, 1, 0),
            Self::North => (0, 0, -1),
            Self::South => (0, 0, 1),
            Self::West => (-1, 0, 0),
            Self::East => (1, 0, 0),
        }
    }
    pub fn step(self, pos: LightBlock) -> LightBlock {
        let (x, y, z) = self.vector();
        pos.offset(x, y, z)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightError {
    MissingChunk,
    MissingStoredLight,
    InvalidColumn,
    ContextMismatch,
    InvalidState,
    InvalidFace,
    InvalidLimits,
    AllocationFailed,
    AllocationLimit,
    MissingLightMetadata,
    MissingBedrock,
    InvalidCoordinate,
    DuplicateChunk,
    StaleSource,
}
impl fmt::Display for LightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lighting source: {self:?}")
    }
}
impl std::error::Error for LightError {}
