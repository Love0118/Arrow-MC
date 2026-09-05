//! Region-file stream codecs, independently composed from their wire formats.
//!
//! This decodes a complete logical stream, including its trailer/checksum. The
//! Vanilla NBT consumer may stop before EOF; acceptance of an unread damaged
//! suffix is not promised here. Zlib/LZ4 suffixes after the stream terminator are
//! ignored like the corresponding Java streams; gzip joins valid members.

use flate2::{Crc, Decompress, FlushDecompress, Status};
use std::fmt;
use std::io::{self, Read};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CompressionKind {
    Gzip = 1,
    Zlib = 2,
    Raw = 3,
    Lz4 = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressionError {
    Unsupported(u8),
    OutputLimit,
    OutputNotReserved,
    Truncated,
    InvalidHeader,
    InvalidLength,
    CorruptData,
    Checksum,
}

impl fmt::Display for CompressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(id) => write!(f, "unsupported region compression {id}"),
            other => f.write_str(match other {
                Self::OutputLimit => "inflated region data exceeds its byte limit",
                Self::OutputNotReserved => "inflated output capacity was not reserved",
                Self::Truncated => "truncated region compression stream",
                Self::InvalidHeader => "invalid region compression header",
                Self::InvalidLength => "invalid region compression block length",
                Self::CorruptData => "corrupt region compressed data",
                Self::Checksum => "region compression checksum mismatch",
                Self::Unsupported(_) => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for CompressionError {}
impl TryFrom<u8> for CompressionKind {
    type Error = CompressionError;
    fn try_from(id: u8) -> Result<Self, Self::Error> {
        match id {
            1 => Ok(Self::Gzip),
            2 => Ok(Self::Zlib),
            3 => Ok(Self::Raw),
            4 => Ok(Self::Lz4),
            _ => Err(CompressionError::Unsupported(id)),
        }
    }
}

/// One decoder per shared CPU worker. The backend's retained inflate workspace
/// belongs to that worker's budget; no per-job output buffer is allocated here.
pub struct StorageDecoder {
    inflate: Decompress,
}

impl Default for StorageDecoder {
    fn default() -> Self {
        Self::new()
    }
}
impl StorageDecoder {
    pub fn new() -> Self {
        Self {
            inflate: Decompress::new(true),
        }
    }

    /// Pull input for the disk NBT consumer. Its caller must stop when the root
    /// completes, without draining EOF. LZ4 scratch must be admitted separately
    /// and large enough for each needed block before that block is decoded.
    pub fn reader<'a>(
        &'a mut self,
        kind: CompressionKind,
        input: &'a [u8],
        lz4_scratch: &'a mut [u8],
        max_decoded: usize,
    ) -> Result<StorageReader<'a>, CompressionError> {
        self.inflate.reset(kind == CompressionKind::Zlib);
        let position = if kind == CompressionKind::Gzip {
            gzip_header(input)?
        } else {
            0
        };
        Ok(StorageReader {
            inner: DecodeStream {
                inflate: &mut self.inflate,
                kind,
                input,
                position,
                window_end: position,
                deflate_finished: false,
                finished: false,
                crc: Crc::new(),
                lz4_scratch,
                block_position: 0,
                block_length: 0,
                decoded: 0,
                max_decoded,
            },
            buffer: [0; 8192],
            position: 0,
            limit: 0,
        })
    }

    /// Appends at most `max_output` bytes. The caller reserves that entire append
    /// capacity before admission. Errors restore the original logical output.
    pub fn decompress(
        &mut self,
        kind: CompressionKind,
        input: &[u8],
        output: &mut Vec<u8>,
        max_output: usize,
    ) -> Result<(), CompressionError> {
        let original = output.len();
        let end = original
            .checked_add(max_output)
            .ok_or(CompressionError::OutputLimit)?;
        if output.capacity() < end {
            return Err(CompressionError::OutputNotReserved);
        }
        let result = match kind {
            CompressionKind::Raw => {
                if input.len() > max_output {
                    Err(CompressionError::OutputLimit)
                } else {
                    output.extend_from_slice(input);
                    Ok(())
                }
            }
            CompressionKind::Zlib => self.deflate(input, output, end, true).map(|_| ()),
            CompressionKind::Gzip => self.gzip(input, output, end),
            CompressionKind::Lz4 => lz4(input, output, end),
        };
        if result.is_err() {
            output.truncate(original);
        }
        result
    }

    fn deflate(
        &mut self,
        input: &[u8],
        output: &mut Vec<u8>,
        end: usize,
        zlib: bool,
    ) -> Result<usize, CompressionError> {
        self.inflate.reset(zlib);
        let mut consumed = 0usize;
        loop {
            let old_in = self.inflate.total_in();
            let old_out = self.inflate.total_out();
            let old_len = output.len();
            let available = end - old_len;
            let mut overflow = [0; 1];
            let result = if available == 0 {
                self.inflate
                    .decompress(&input[consumed..], &mut overflow, FlushDecompress::None)
            } else {
                // Bounded windows avoid zeroing an entire large reservation for
                // tiny chunks and never allocate beyond caller-owned capacity.
                output.resize(old_len + available.min(64 * 1024), 0);
                self.inflate.decompress(
                    &input[consumed..],
                    &mut output[old_len..],
                    FlushDecompress::None,
                )
            };
            let added = (self.inflate.total_out() - old_out) as usize;
            let read = (self.inflate.total_in() - old_in) as usize;
            consumed += read;
            if available == 0 && added != 0 {
                return Err(CompressionError::OutputLimit);
            }
            output.truncate(old_len + added);
            let status = result.map_err(|_| CompressionError::CorruptData)?;
            if status == Status::StreamEnd {
                return Ok(consumed);
            }
            if read == 0 && added == 0 {
                return Err(if consumed == input.len() {
                    CompressionError::Truncated
                } else {
                    CompressionError::CorruptData
                });
            }
        }
    }

    fn gzip(
        &mut self,
        mut input: &[u8],
        output: &mut Vec<u8>,
        end: usize,
    ) -> Result<(), CompressionError> {
        let mut first = true;
        loop {
            let header = match gzip_header(input) {
                Ok(header) => header,
                Err(error) if first => return Err(error),
                // JDK GZIPInputStream treats a failed subsequent member header
                // as trailing bytes, but failures inside a valid member matter.
                Err(_) => return Ok(()),
            };
            first = false;
            input = &input[header..];
            let start = output.len();
            let consumed = self.deflate(input, output, end, false)?;
            input = &input[consumed..];
            let trailer = input.get(..8).ok_or(CompressionError::Truncated)?;
            let mut crc = Crc::new();
            crc.update(&output[start..]);
            if read_u32(&trailer[..4]) != crc.sum() || read_u32(&trailer[4..]) != crc.amount() {
                return Err(CompressionError::Checksum);
            }
            input = &input[8..];
            if input.is_empty() {
                return Ok(());
            }
        }
    }
}

/// Concrete equivalent of the disk reader's 8 KiB Java buffering contract.
/// A large read bypasses an empty buffer; a small read performs only one fill.
pub struct StorageReader<'a> {
    inner: DecodeStream<'a>,
    buffer: [u8; 8192],
    position: usize,
    limit: usize,
}

impl Read for StorageReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.position == self.limit {
            if output.len() >= self.buffer.len() {
                return self.inner.read(output).map_err(stream_error);
            }
            self.position = 0;
            self.limit = self.inner.read(&mut self.buffer).map_err(stream_error)?;
        }
        let count = output.len().min(self.limit - self.position);
        output[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

impl StorageReader<'_> {
    /// Mirrors one FastBufferedInputStream.skip call. DataInput.skipBytes loops
    /// this operation when it needs to skip an entire encoded root name.
    pub fn skip(&mut self, count: usize) -> Result<usize, CompressionError> {
        if self.position != self.limit {
            let skipped = count.min(self.limit - self.position);
            self.position += skipped;
            return Ok(skipped);
        }
        let mut scratch = [0u8; 512];
        let mut skipped = 0;
        while skipped < count {
            let wanted = (count - skipped).min(scratch.len());
            let read = self.inner.read(&mut scratch[..wanted])?;
            if read == 0 {
                break;
            }
            skipped += read;
        }
        Ok(skipped)
    }
}

fn stream_error(error: CompressionError) -> io::Error {
    io::Error::new(
        if error == CompressionError::Truncated {
            io::ErrorKind::UnexpectedEof
        } else {
            io::ErrorKind::InvalidData
        },
        error,
    )
}

struct DecodeStream<'a> {
    inflate: &'a mut Decompress,
    kind: CompressionKind,
    input: &'a [u8],
    position: usize,
    window_end: usize,
    deflate_finished: bool,
    finished: bool,
    crc: Crc,
    lz4_scratch: &'a mut [u8],
    block_position: usize,
    block_length: usize,
    decoded: usize,
    max_decoded: usize,
}

impl DecodeStream<'_> {
    fn read(&mut self, output: &mut [u8]) -> Result<usize, CompressionError> {
        if output.is_empty() || self.finished {
            return Ok(0);
        }
        match self.kind {
            CompressionKind::Raw => {
                let count = output.len().min(self.input.len() - self.position);
                if count > self.max_decoded - self.decoded {
                    return Err(CompressionError::OutputLimit);
                }
                output[..count].copy_from_slice(&self.input[self.position..self.position + count]);
                self.position += count;
                self.decoded += count;
                Ok(count)
            }
            CompressionKind::Lz4 => self.read_lz4(output),
            CompressionKind::Gzip | CompressionKind::Zlib => self.read_deflate(output),
        }
    }

    fn read_deflate(&mut self, output: &mut [u8]) -> Result<usize, CompressionError> {
        loop {
            if self.deflate_finished {
                if self.kind == CompressionKind::Zlib {
                    self.finished = true;
                    return Ok(0);
                }
                let tail = self
                    .input
                    .get(self.position..self.position + 8)
                    .ok_or(CompressionError::Truncated)?;
                if read_u32(&tail[..4]) != self.crc.sum()
                    || read_u32(&tail[4..]) != self.crc.amount()
                {
                    return Err(CompressionError::Checksum);
                }
                self.position += 8;
                match gzip_header(&self.input[self.position..]) {
                    Ok(header) => {
                        self.position += header;
                        self.window_end = self.position;
                        self.deflate_finished = false;
                        self.crc.reset();
                        self.inflate.reset(false);
                    }
                    Err(_) => {
                        self.finished = true;
                        return Ok(0);
                    }
                }
            }
            if self.position == self.window_end {
                self.window_end = (self.position + 512).min(self.input.len());
            }
            let remaining = self.max_decoded - self.decoded;
            let wanted = output.len().min(remaining);
            let mut overflow = [0u8; 1];
            let destination = if wanted == 0 {
                &mut overflow[..]
            } else {
                &mut output[..wanted]
            };
            let before_in = self.inflate.total_in();
            let before_out = self.inflate.total_out();
            let status = self.inflate.decompress(
                &self.input[self.position..self.window_end],
                destination,
                FlushDecompress::None,
            );
            let consumed = (self.inflate.total_in() - before_in) as usize;
            let produced = (self.inflate.total_out() - before_out) as usize;
            self.position += consumed;
            if produced > remaining {
                return Err(CompressionError::OutputLimit);
            }
            let status = status.map_err(|_| CompressionError::CorruptData)?;
            self.deflate_finished = status == Status::StreamEnd;
            if produced > 0 {
                self.decoded += produced;
                if self.kind == CompressionKind::Gzip {
                    self.crc.update(&output[..produced]);
                }
                // GZIP checks its trailer only when a subsequent read observes
                // the inflater has no more output, even at an exact NBT boundary.
                return Ok(produced);
            }
            if self.deflate_finished {
                continue;
            }
            if self.position == self.input.len() {
                return Err(CompressionError::Truncated);
            }
            if consumed == 0 && self.position != self.window_end {
                return Err(CompressionError::CorruptData);
            }
        }
    }

    fn read_lz4(&mut self, output: &mut [u8]) -> Result<usize, CompressionError> {
        if self.block_position == self.block_length {
            let header = Lz4Header::parse(&self.input[self.position..])?;
            let original = header.original;
            let compressed = header.compressed;
            self.position += 21;
            if original == 0 {
                self.finished = true;
                return Ok(0);
            }
            if original > self.max_decoded - self.decoded {
                return Err(CompressionError::OutputLimit);
            }
            if original > self.lz4_scratch.len() {
                return Err(CompressionError::OutputNotReserved);
            }
            let block = self
                .input
                .get(self.position..self.position + compressed)
                .ok_or(CompressionError::Truncated)?;
            let destination = &mut self.lz4_scratch[..original];
            header.decode(block, destination)?;
            self.position += compressed;
            self.block_position = 0;
            self.block_length = original;
            self.decoded += original;
        }
        let count = output.len().min(self.block_length - self.block_position);
        output[..count]
            .copy_from_slice(&self.lz4_scratch[self.block_position..self.block_position + count]);
        self.block_position += count;
        Ok(count)
    }
}

fn gzip_header(input: &[u8]) -> Result<usize, CompressionError> {
    let header = input.get(..10).ok_or(CompressionError::Truncated)?;
    if header[..3] != [0x1f, 0x8b, 8] {
        return Err(CompressionError::InvalidHeader);
    }
    let flags = header[3];
    let mut cursor = 10usize;
    if flags & 4 != 0 {
        let length = input
            .get(cursor..cursor + 2)
            .ok_or(CompressionError::Truncated)?;
        cursor += 2 + usize::from(u16::from_le_bytes([length[0], length[1]]));
        if cursor > input.len() {
            return Err(CompressionError::Truncated);
        }
    }
    for flag in [8, 16] {
        if flags & flag != 0 {
            let length = input
                .get(cursor..)
                .and_then(|tail| tail.iter().position(|&b| b == 0))
                .ok_or(CompressionError::Truncated)?;
            cursor += length + 1;
        }
    }
    if flags & 2 != 0 {
        let checksum = input
            .get(cursor..cursor + 2)
            .ok_or(CompressionError::Truncated)?;
        let mut crc = Crc::new();
        crc.update(&input[..cursor]);
        if crc.sum() as u16 != u16::from_le_bytes([checksum[0], checksum[1]]) {
            return Err(CompressionError::Checksum);
        }
        cursor += 2;
    }
    Ok(cursor)
}

fn lz4(mut input: &[u8], output: &mut Vec<u8>, end: usize) -> Result<(), CompressionError> {
    loop {
        let header = Lz4Header::parse(input)?;
        input = &input[21..];
        let original = header.original;
        if original == 0 {
            return Ok(());
        }
        if original > end - output.len() {
            return Err(CompressionError::OutputLimit);
        }
        let block = input
            .get(..header.compressed)
            .ok_or(CompressionError::Truncated)?;
        let start = output.len();
        output.resize(start + original, 0);
        header.decode(block, &mut output[start..])?;
        input = &input[header.compressed..];
    }
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Upper bound for scratch of blocks reachable inside this admitted input and
/// decoded-byte limit. This inspects only framing: it does not reject malformed
/// unread tails or claim that their checksums are valid. Charge the separate
/// `min(max_decoded, 32 MiB)` scratch budget before allocating the returned size.
pub fn lz4_scratch_required(mut input: &[u8], max_decoded: usize) -> usize {
    let mut largest = 0usize;
    let mut decoded = 0usize;
    while let Ok(header) = Lz4Header::parse(input) {
        let original = header.original;
        let compressed = header.compressed;
        if original == 0 || original > max_decoded - decoded {
            break;
        }
        largest = largest.max(original);
        decoded += original;
        let Some(tail) = input.get(21..).and_then(|rest| rest.get(compressed..)) else {
            break;
        };
        input = tail;
    }
    largest
}

struct Lz4Header {
    compressed: usize,
    original: usize,
    checksum: u32,
    raw: bool,
}
impl Lz4Header {
    fn parse(input: &[u8]) -> Result<Self, CompressionError> {
        let header = input.get(..21).ok_or(CompressionError::Truncated)?;
        let method = header[8] & 0xf0;
        if &header[..8] != b"LZ4Block" || ![0x10, 0x20].contains(&method) {
            return Err(CompressionError::InvalidHeader);
        }
        let compressed = read_u32(&header[9..13]);
        let original = read_u32(&header[13..17]);
        let checksum = read_u32(&header[17..21]);
        let raw = method == 0x10;
        if compressed > i32::MAX as u32
            || original > 1u32 << (10 + (header[8] & 15))
            || (compressed == 0) != (original == 0)
            || (raw && compressed != original)
        {
            return Err(CompressionError::InvalidLength);
        }
        if original == 0 && checksum != 0 {
            return Err(CompressionError::Checksum);
        }
        Ok(Self {
            compressed: compressed as usize,
            original: original as usize,
            checksum,
            raw,
        })
    }
    fn decode(&self, input: &[u8], output: &mut [u8]) -> Result<(), CompressionError> {
        if self.raw {
            output.copy_from_slice(input);
        } else if lz4_flex::block::decompress_into(input, output)
            .map_err(|_| CompressionError::CorruptData)?
            != self.original
        {
            return Err(CompressionError::InvalidLength);
        }
        if xxhash_rust::xxh32::xxh32(output, 0x9747_b28c) & 0x0fff_ffff != self.checksum {
            return Err(CompressionError::Checksum);
        }
        Ok(())
    }
}
