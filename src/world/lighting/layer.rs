//! A section's 4,096 light values, retaining Vanilla's lazy representation.
//!
//! An allocated zero buffer is observably different from a uniform zero layer.
//! Allocation limits cover newly allocated backing capacity, excluding allocator
//! metadata and the fixed size of this value. Existing buffers require no new
//! allocation allowance; shared storage accounts for their lifetime separately.

use std::fmt;

pub const SIDE: u8 = 16;
pub const LAYER_BYTES: usize = 2048;
const PLANE_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidLength { expected: usize, actual: usize },
    CoordinateOutOfBounds { x: u8, y: u8, z: u8 },
    AllocationBudgetExceeded,
    AllocationFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid light layer: {self:?}")
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
enum Storage {
    Uniform(i32),
    Allocated(Vec<u8>),
}

#[derive(Debug)]
pub struct DataLayer {
    storage: Storage,
}

impl DataLayer {
    /// Keeps the entire Java `int` value until materialization, including values
    /// outside the ordinary 0..=15 light range.
    pub const fn uniform(value: i32) -> Self {
        Self {
            storage: Storage::Uniform(value),
        }
    }

    /// Copies exactly one section's bytes, retaining its allocated representation.
    pub fn from_bytes(bytes: &[u8], allocation_limit: usize) -> Result<Self, Error> {
        if bytes.len() != LAYER_BYTES {
            return Err(Error::InvalidLength {
                expected: LAYER_BYTES,
                actual: bytes.len(),
            });
        }
        let mut output = allocate(allocation_limit)?;
        output.copy_from_slice(bytes);
        Ok(Self {
            storage: Storage::Allocated(output),
        })
    }

    /// Coordinates must be section-relative (0..16 on each axis). Invalid
    /// coordinates are rejected even when the layer is uniform.
    pub fn get(&self, x: u8, y: u8, z: u8) -> Result<i32, Error> {
        let (byte, shift) = address(x, y, z)?;
        Ok(match &self.storage {
            Storage::Uniform(value) => *value,
            Storage::Allocated(bytes) => i32::from((bytes[byte] >> shift) & 0x0f),
        })
    }

    /// Setting a value always materializes a uniform layer, even if its low
    /// nibble was already equal. Failed allocation or coordinates leave it intact.
    pub fn set(
        &mut self,
        x: u8,
        y: u8,
        z: u8,
        value: i32,
        allocation_limit: usize,
    ) -> Result<(), Error> {
        let (byte, shift) = address(x, y, z)?;
        self.materialize(allocation_limit)?;
        let Storage::Allocated(bytes) = &mut self.storage else {
            unreachable!("materialize allocates a uniform layer")
        };
        let nibble = (value as u8) & 0x0f;
        bytes[byte] = (bytes[byte] & !(0x0f << shift)) | (nibble << shift);
        Ok(())
    }

    /// Replaces the representation with a uniform value and releases any buffer.
    pub fn fill(&mut self, value: i32) {
        self.storage = Storage::Uniform(value);
    }

    /// Materialization preserves Java's full low byte before adding the repeated
    /// low nibble. Thus a uniform 16 becomes alternating values 0 and 1, not zero.
    pub fn materialize(&mut self, allocation_limit: usize) -> Result<&[u8], Error> {
        if let Storage::Uniform(value) = self.storage {
            let mut bytes = allocate(allocation_limit)?;
            let low_byte = value as u8;
            bytes.fill(low_byte | (low_byte << 4));
            self.storage = Storage::Allocated(bytes);
        }
        Ok(self.bytes().expect("materialization produced a buffer"))
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        match &self.storage {
            Storage::Uniform(_) => None,
            Storage::Allocated(bytes) => Some(bytes),
        }
    }

    /// These queries describe the retained representation; they do not scan data.
    pub const fn is_empty(&self) -> bool {
        self.is_filled_with(0)
    }

    pub const fn is_definitely_homogeneous(&self) -> bool {
        matches!(self.storage, Storage::Uniform(_))
    }

    pub const fn is_filled_with(&self, value: i32) -> bool {
        matches!(self.storage, Storage::Uniform(current) if current == value)
    }

    pub fn heap_bytes(&self) -> usize {
        match &self.storage {
            Storage::Uniform(_) => 0,
            Storage::Allocated(bytes) => bytes.capacity(),
        }
    }

    pub fn try_copy(&self, allocation_limit: usize) -> Result<Self, Error> {
        match &self.storage {
            Storage::Uniform(value) => Ok(Self::uniform(*value)),
            Storage::Allocated(bytes) => Self::from_bytes(bytes, allocation_limit),
        }
    }

    /// Extends the bottom horizontal plane through the section, as needed when
    /// creating missing sky-light data below an existing section.
    pub fn repeat_first_layer(&self, allocation_limit: usize) -> Result<Self, Error> {
        let Storage::Allocated(input) = &self.storage else {
            return self.try_copy(allocation_limit);
        };
        let mut bytes = allocate(allocation_limit)?;
        for plane in bytes.chunks_exact_mut(PLANE_BYTES) {
            plane.copy_from_slice(&input[..PLANE_BYTES]);
        }
        Ok(Self {
            storage: Storage::Allocated(bytes),
        })
    }
}

fn address(x: u8, y: u8, z: u8) -> Result<(usize, u8), Error> {
    if x >= SIDE || y >= SIDE || z >= SIDE {
        return Err(Error::CoordinateOutOfBounds { x, y, z });
    }
    let byte = usize::from(y) * 128 + usize::from(z) * 8 + usize::from(x / 2);
    Ok((byte, (x % 2) * 4))
}

fn allocate(allocation_limit: usize) -> Result<Vec<u8>, Error> {
    if allocation_limit < LAYER_BYTES {
        return Err(Error::AllocationBudgetExceeded);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(LAYER_BYTES)
        .map_err(|_| Error::AllocationFailed)?;
    if bytes.capacity() > allocation_limit {
        return Err(Error::AllocationBudgetExceeded);
    }
    bytes.resize(LAYER_BYTES, 0);
    Ok(bytes)
}
