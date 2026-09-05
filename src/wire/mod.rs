//! Minecraft's signed variable-length integers.
//!
//! Behavior follows `net.minecraft.network.VarInt` and `VarLong` in the pinned
//! Vanilla 26.3-pre-2 server. These are not ZigZag integers: negative values use
//! five or ten bytes. Packet frame lengths have additional constraints and must
//! be checked by the framing layer.

use std::fmt;

pub const MAX_VARINT_BYTES: usize = 5;
pub const MAX_VARLONG_BYTES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// More input is needed, including after a maximum-length continuation byte.
    Incomplete,
    /// A sixth VarInt byte or eleventh VarLong byte was present.
    TooLong,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Incomplete => "incomplete variable-length integer",
            Self::TooLong => "variable-length integer exceeds its byte limit",
        })
    }
}

impl std::error::Error for DecodeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferTooSmall {
    pub required: usize,
    pub available: usize,
}

impl fmt::Display for BufferTooSmall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "variable-length integer needs {} bytes; output has {}",
            self.required, self.available
        )
    }
}

impl std::error::Error for BufferTooSmall {}

/// Reads one signed VarInt and returns its value and consumed byte count.
///
/// As in Vanilla, overlong encodings within five bytes are accepted, and high
/// payload bits in the fifth byte are discarded. Five continuation bytes without
/// a sixth byte are incomplete; Vanilla checks its size limit after reading the
/// next byte. Input is borrowed, so callers advance their cursor only on success.
pub fn read_varint(input: &[u8]) -> Result<(i32, usize), DecodeError> {
    let mut value = 0_u32;
    for (index, &byte) in input.iter().take(MAX_VARINT_BYTES).enumerate() {
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok((value as i32, index + 1));
        }
    }

    if input.len() > MAX_VARINT_BYTES {
        Err(DecodeError::TooLong)
    } else {
        Err(DecodeError::Incomplete)
    }
}

/// Reads one signed VarLong and returns its value and consumed byte count.
///
/// Overlong encodings within ten bytes are accepted. Only the low payload bit of
/// the tenth byte contributes to the result. Ten continuation bytes require an
/// eleventh byte before Vanilla reports the size error; see [`read_varint`].
pub fn read_varlong(input: &[u8]) -> Result<(i64, usize), DecodeError> {
    let mut value = 0_u64;
    for (index, &byte) in input.iter().take(MAX_VARLONG_BYTES).enumerate() {
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok((value as i64, index + 1));
        }
    }

    if input.len() > MAX_VARLONG_BYTES {
        Err(DecodeError::TooLong)
    } else {
        Err(DecodeError::Incomplete)
    }
}

/// Number of bytes in the canonical encoding, including five for negatives.
pub const fn varint_len(value: i32) -> usize {
    if value == 0 {
        1
    } else {
        (u32::BITS - (value as u32).leading_zeros()).div_ceil(7) as usize
    }
}

/// Number of bytes in the canonical encoding, including ten for negatives.
pub const fn varlong_len(value: i64) -> usize {
    if value == 0 {
        1
    } else {
        (u64::BITS - (value as u64).leading_zeros()).div_ceil(7) as usize
    }
}

/// Writes a canonical signed VarInt, leaving output unchanged if it is too small.
/// Bytes beyond the returned length are unchanged.
pub fn write_varint(value: i32, output: &mut [u8]) -> Result<usize, BufferTooSmall> {
    let length = varint_len(value);
    if output.len() < length {
        return Err(BufferTooSmall {
            required: length,
            available: output.len(),
        });
    }

    let mut remaining = value as u32;
    for byte in &mut output[..length - 1] {
        *byte = (remaining as u8 & 0x7f) | 0x80;
        remaining >>= 7;
    }
    output[length - 1] = remaining as u8;
    Ok(length)
}

/// Writes a canonical signed VarLong, leaving output unchanged if it is too small.
/// Bytes beyond the returned length are unchanged.
pub fn write_varlong(value: i64, output: &mut [u8]) -> Result<usize, BufferTooSmall> {
    let length = varlong_len(value);
    if output.len() < length {
        return Err(BufferTooSmall {
            required: length,
            available: output.len(),
        });
    }

    let mut remaining = value as u64;
    for byte in &mut output[..length - 1] {
        *byte = (remaining as u8 & 0x7f) | 0x80;
        remaining >>= 7;
    }
    output[length - 1] = remaining as u8;
    Ok(length)
}
