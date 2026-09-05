//! Bounded disk-NBT pull adapter for region-file reads.
//!
//! The scanner issues the DataInput-sized reads of one root, then decodes its
//! captured network-shaped bytes once. It never drains the compression stream
//! to EOF: an unread trailer is not an extra storage validity requirement.
//! Root names are skipped as opaque bytes, unlike named NBT string values.

use super::compression::{CompressionError, StorageReader};
use crate::nbt::{self, Compound, Tag};
use std::io::Read;

#[derive(Clone, Copy)]
struct Frame {
    // Zero identifies a compound; nonempty lists cannot have an End element.
    element: u8,
    left: u32,
}

/// Fixed scanner stack, in addition to caller-admitted captured bytes and the
/// decoder's separately admitted buffers. No per-node scanner allocation.
pub const SCANNER_SCRATCH_BYTES: usize = std::mem::size_of::<[Frame; 512]>();

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamError {
    Compression(CompressionError),
    Nbt(nbt::Error),
    RootType,
    BufferNotReserved,
    InflatedLimit,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(output, "invalid stored NBT stream: {self:?}")
    }
}
impl std::error::Error for StreamError {}

/// Reads exactly one disk-style root, requiring a compound as NbtIo.read does.
/// `captured` must already have capacity for `max_output` appended bytes. Root
/// names are omitted from captured bytes; the returned count is the existing
/// NBT decoder's cumulative requested backing allocation. On error captured
/// bytes roll back, while the pull reader's consumed input does not rewind.
pub fn read_disk_compound(
    reader: &mut StorageReader<'_>,
    captured: &mut Vec<u8>,
    max_output: usize,
    limits: nbt::Limits,
) -> Result<(Compound, usize), StreamError> {
    if limits.max_depth > 512 {
        return Err(StreamError::Nbt(nbt::Error::InvalidDepthLimit));
    }
    let start = captured.len();
    let end = start
        .checked_add(max_output)
        .ok_or(StreamError::InflatedLimit)?;
    if end > captured.capacity() {
        return Err(StreamError::BufferNotReserved);
    }
    let result = (|| {
        let mut scanner = Scanner {
            reader,
            captured,
            end,
            max_depth: limits.max_depth,
        };
        scanner.root()?;
        let mut input = &scanner.captured[start..];
        let (value, allocated) =
            nbt::read_network_accounted(&mut input, limits).map_err(StreamError::Nbt)?;
        debug_assert!(input.is_empty(), "scanner stopped at one complete NBT root");
        match value {
            Tag::Compound(compound) => Ok((compound, allocated)),
            value => {
                value.drop_iterative();
                Err(StreamError::RootType)
            }
        }
    })();
    if result.is_err() {
        captured.truncate(start);
    }
    result
}

struct Scanner<'a, 'b> {
    reader: &'a mut StorageReader<'b>,
    captured: &'a mut Vec<u8>,
    end: usize,
    max_depth: usize,
}

impl Scanner<'_, '_> {
    fn read(&mut self, count: usize) -> Result<&[u8], StreamError> {
        let start = self.captured.len();
        let end = start.checked_add(count).ok_or(StreamError::InflatedLimit)?;
        if end > self.end {
            return Err(StreamError::InflatedLimit);
        }
        self.captured.resize(end, 0);
        self.reader
            .read_exact(&mut self.captured[start..])
            .map_err(io_error)?;
        Ok(&self.captured[start..])
    }

    fn byte(&mut self) -> Result<u8, StreamError> {
        Ok(self.read(1)?[0])
    }

    fn count(&mut self) -> Result<usize, StreamError> {
        let data = self.read(4)?;
        let count = i32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        usize::try_from(count).map_err(|_| StreamError::Nbt(nbt::Error::NegativeLength(count)))
    }

    fn string(&mut self) -> Result<(), StreamError> {
        let data = self.read(2)?;
        let count = usize::from(u16::from_be_bytes([data[0], data[1]]));
        let bytes = self.read(count)?;
        validate_modified_utf8(bytes).map_err(StreamError::Nbt)
    }

    fn root(&mut self) -> Result<(), StreamError> {
        let root_type = self.byte()?;
        if root_type == 0 {
            return Ok(());
        }
        let mut length = [0; 2];
        self.reader.read_exact(&mut length).map_err(io_error)?;
        let mut remaining = usize::from(u16::from_be_bytes(length));
        // DataInput.skipBytes stops when the underlying skip returns zero. It
        // neither validates the skipped modified UTF-8 nor requires skip==len.
        while remaining != 0 {
            let skipped = self
                .reader
                .skip(remaining)
                .map_err(StreamError::Compression)?;
            if skipped == 0 {
                break;
            }
            remaining -= skipped;
        }

        let mut frames = [Frame {
            element: 0,
            left: 0,
        }; 512];
        let mut depth = 0;
        let mut next = Some(root_type);
        loop {
            if let Some(kind) = next.take() {
                match kind {
                    1 => {
                        self.read(1)?;
                    }
                    2 => {
                        self.read(2)?;
                    }
                    3 | 5 => {
                        self.read(4)?;
                    }
                    4 | 6 => {
                        self.read(8)?;
                    }
                    7 => {
                        let count = self.count()?;
                        self.read(count)?;
                    }
                    8 => self.string()?,
                    9 => {
                        if depth >= self.max_depth {
                            return Err(StreamError::Nbt(nbt::Error::DepthLimit));
                        }
                        let element = self.byte()?;
                        let count = self.count()?;
                        if count != 0 {
                            if element == 0 {
                                return Err(StreamError::Nbt(nbt::Error::UnexpectedEnd));
                            }
                            if element > 12 {
                                return Err(StreamError::Nbt(nbt::Error::UnknownTag(element)));
                            }
                            frames[depth] = Frame {
                                element,
                                left: count as u32,
                            };
                            depth += 1;
                        }
                    }
                    10 => {
                        if depth >= self.max_depth {
                            return Err(StreamError::Nbt(nbt::Error::DepthLimit));
                        }
                        frames[depth] = Frame {
                            element: 0,
                            left: 0,
                        };
                        depth += 1;
                    }
                    11 | 12 => {
                        let count = self.count()?;
                        let width = if kind == 11 { 4 } else { 8 };
                        // Read primitive elements individually; one bulk read
                        // changes FastBufferedInputStream bypass/refill behavior.
                        for _ in 0..count {
                            self.read(width)?;
                        }
                    }
                    _ => return Err(StreamError::Nbt(nbt::Error::UnknownTag(kind))),
                }
            }
            while depth != 0 {
                let frame = &mut frames[depth - 1];
                if frame.element == 0 {
                    let kind = self.byte()?;
                    if kind != 0 {
                        self.string()?;
                        next = Some(kind);
                        break;
                    }
                    depth -= 1;
                } else if frame.left != 0 {
                    frame.left -= 1;
                    next = Some(frame.element);
                    break;
                } else {
                    depth -= 1;
                }
            }
            if next.is_none() {
                return Ok(());
            }
        }
    }
}

fn io_error(error: std::io::Error) -> StreamError {
    StreamError::Compression(
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<CompressionError>())
            .copied()
            .unwrap_or(CompressionError::Truncated),
    )
}

// Validation is allocation-free and stops invalid strings at their original
// read boundary. The existing NBT reader builds the UTF-16 value exactly once.
fn validate_modified_utf8(bytes: &[u8]) -> Result<(), nbt::Error> {
    let mut cursor = 0;
    while let Some(&first) = bytes.get(cursor) {
        cursor += 1;
        let more = match first {
            0..=0x7f => 0,
            0xc0..=0xdf => 1,
            0xe0..=0xef => 2,
            _ => return Err(nbt::Error::InvalidModifiedUtf8),
        };
        for _ in 0..more {
            if bytes.get(cursor).is_none_or(|&next| next & 0xc0 != 0x80) {
                return Err(nbt::Error::InvalidModifiedUtf8);
            }
            cursor += 1;
        }
    }
    Ok(())
}
