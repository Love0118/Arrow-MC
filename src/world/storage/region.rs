//! Read-only Anvil stream location, without allocating or decompressing payloads.
//!
//! Independently designed from synthetic observations of the source-exposed
//! Vanilla 26.3-pre-2 RegionFile/RegionFileStorage APIs. A caller must reserve its
//! payload budget before allocating the buffer passed to `LocatedChunk::read_into`.
//! These blocking file operations belong on the caller's bounded I/O executor.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const HEADER_BYTES: usize = 8192;
const SECTOR_BYTES: u64 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamVersion {
    Gzip,
    Zlib,
    Raw,
    Lz4,
}

#[derive(Clone, Copy, Debug)]
pub struct RegionReadLimits {
    /// Arrow resource policy, applied before a stream is returned or consumed.
    /// This is independent of the decompressed/NBT allocation budget.
    pub compressed_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnavailableReason {
    InvalidSector,
    TruncatedChunkHeader,
    MissingStream,
    TruncatedStream,
    NegativeStreamLength,
    ExternalMissing,
    ExternalNotFile,
    UnknownVersion(u8),
    UnsupportedCustomCompression,
    InvalidCustomCompressionIdentifier,
}

#[derive(Debug)]
pub enum RegionLocation {
    /// No regular region file or no allocated entry for the requested position.
    Missing,
    /// The reference returns null but reports a malformed/unavailable stream.
    Unavailable(UnavailableReason),
    Present(LocatedChunk),
}

#[derive(Debug)]
pub enum RegionError {
    Io(io::Error),
    CompressedLimit { length: u64, limit: usize },
    InvalidCustomUtf,
    OutputLength { expected: usize, actual: usize },
}

impl fmt::Display for RegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "region I/O: {error}"),
            Self::CompressedLimit { length, limit } => {
                write!(
                    f,
                    "region stream length {length} exceeds byte limit {limit}"
                )
            }
            Self::InvalidCustomUtf => f.write_str("invalid modified UTF-8 custom compression name"),
            Self::OutputLength { expected, actual } => {
                write!(f, "region output length {actual}, expected {expected}")
            }
        }
    }
}

impl std::error::Error for RegionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for RegionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
pub struct LocatedChunk {
    file: File,
    length: usize,
    version: StreamVersion,
    external_internal_length_warning: bool,
    truncated_region_header_warning: bool,
}

impl LocatedChunk {
    pub fn compressed_len(&self) -> usize {
        self.length
    }

    pub fn version(&self) -> StreamVersion {
        self.version
    }

    pub fn has_external_internal_length_warning(&self) -> bool {
        self.external_internal_length_warning
    }

    pub fn has_truncated_region_header_warning(&self) -> bool {
        self.truncated_region_header_warning
    }

    /// Reads exactly the located payload into caller-owned storage. The handle
    /// remains attached to the opened file if its path is replaced. Concurrent
    /// in-place edits are not a snapshot; shortening the file returns an I/O error.
    pub fn read_into(mut self, output: &mut [u8]) -> Result<(), RegionError> {
        if output.len() != self.length {
            return Err(RegionError::OutputLength {
                expected: self.length,
                actual: output.len(),
            });
        }
        self.file.read_exact(output)?;
        Ok(())
    }
}

/// Opens existing region/external files read-only. This never creates a region,
/// repairs its header, pads a file, or interprets a missing chunk as generation.
pub fn locate(
    region_dir: &Path,
    chunk_x: i32,
    chunk_z: i32,
    limits: RegionReadLimits,
) -> Result<RegionLocation, RegionError> {
    let path = region_dir.join(format!("r.{}.{}.mca", chunk_x >> 5, chunk_z >> 5));
    // Check before opening so an ordinary non-file entry (including a FIFO)
    // is not opened as a blocking data stream. Recheck the opened handle below.
    match fs::metadata(&path) {
        Ok(metadata) if !metadata.is_file() => return Ok(RegionLocation::Missing),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(RegionLocation::Missing);
        }
        Err(error) => return Err(error.into()),
    }
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(RegionLocation::Missing);
        }
        // Windows cannot open directory handles through File::open. Preserve
        // the same non-file classification as a successfully opened directory.
        Err(error) => match fs::metadata(&path) {
            Ok(metadata) if !metadata.is_file() => return Ok(RegionLocation::Missing),
            _ => return Err(error.into()),
        },
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(RegionLocation::Missing);
    }

    let file_length = metadata.len();
    let mut header = [0_u8; HEADER_BYTES];
    let header_length = read_available(&mut file, &mut header)?;
    let entry = ((chunk_x & 31) as usize + (chunk_z & 31) as usize * 32) * 4;
    let location = u32::from_be_bytes([
        header[entry],
        header[entry + 1],
        header[entry + 2],
        header[entry + 3],
    ]);
    if location == 0 {
        return Ok(RegionLocation::Missing);
    }
    let start = u64::from(location >> 8) * SECTOR_BYTES;
    let sector_bytes = u64::from(location & 255) * SECTOR_BYTES;
    if start < HEADER_BYTES as u64 || sector_bytes == 0 || start > file_length {
        return Ok(RegionLocation::Unavailable(
            UnavailableReason::InvalidSector,
        ));
    }

    file.seek(SeekFrom::Start(start))?;
    let mut stream_header = [0_u8; 5];
    if read_available(&mut file, &mut stream_header)? != stream_header.len() {
        return Ok(RegionLocation::Unavailable(
            UnavailableReason::TruncatedChunkHeader,
        ));
    }
    let length = i32::from_be_bytes([
        stream_header[0],
        stream_header[1],
        stream_header[2],
        stream_header[3],
    ]);
    if length == 0 {
        return Ok(RegionLocation::Unavailable(
            UnavailableReason::MissingStream,
        ));
    }
    let version = stream_header[4];
    let truncated_header = header_length != HEADER_BYTES;

    if version & 128 != 0 {
        let external_path = region_dir.join(format!("c.{chunk_x}.{chunk_z}.mcc"));
        match fs::metadata(&external_path) {
            Ok(metadata) if !metadata.is_file() => {
                return Ok(RegionLocation::Unavailable(
                    UnavailableReason::ExternalNotFile,
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RegionLocation::Unavailable(
                    UnavailableReason::ExternalMissing,
                ));
            }
            Err(error) => return Err(error.into()),
        }
        let external = match File::open(&external_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RegionLocation::Unavailable(
                    UnavailableReason::ExternalMissing,
                ));
            }
            Err(error) => match fs::metadata(&external_path) {
                Ok(metadata) if !metadata.is_file() => {
                    return Ok(RegionLocation::Unavailable(
                        UnavailableReason::ExternalNotFile,
                    ));
                }
                _ => return Err(error.into()),
            },
        };
        let metadata = external.metadata()?;
        if !metadata.is_file() {
            return Ok(RegionLocation::Unavailable(
                UnavailableReason::ExternalNotFile,
            ));
        }
        return classify_stream(
            external,
            version & 127,
            metadata.len(),
            (length != 1, truncated_header),
            limits,
            &mut header,
        );
    }

    // Signed subtraction has Java's wrapping behavior. In particular MIN_VALUE
    // becomes positive here and follows the truncated-stream classification.
    let payload_length = length.wrapping_sub(1);
    let available = file_length
        .saturating_sub(start)
        .min(sector_bytes)
        .saturating_sub(5);
    if payload_length >= 0 && payload_length as u64 > available {
        return Ok(RegionLocation::Unavailable(
            UnavailableReason::TruncatedStream,
        ));
    }
    if payload_length < 0 {
        return Ok(RegionLocation::Unavailable(
            UnavailableReason::NegativeStreamLength,
        ));
    }
    classify_stream(
        file,
        version,
        payload_length as u64,
        (false, truncated_header),
        limits,
        &mut header,
    )
}

fn classify_stream(
    mut file: File,
    version: u8,
    length: u64,
    warnings: (bool, bool),
    limits: RegionReadLimits,
    scratch: &mut [u8; HEADER_BYTES],
) -> Result<RegionLocation, RegionError> {
    let compression = match version {
        1 => StreamVersion::Gzip,
        2 => StreamVersion::Zlib,
        3 => StreamVersion::Raw,
        4 => StreamVersion::Lz4,
        127 => {
            check_length(length, limits)?;
            return Ok(RegionLocation::Unavailable(
                if custom_identifier(&mut file, length, scratch)? {
                    UnavailableReason::UnsupportedCustomCompression
                } else {
                    UnavailableReason::InvalidCustomCompressionIdentifier
                },
            ));
        }
        other => {
            return Ok(RegionLocation::Unavailable(
                UnavailableReason::UnknownVersion(other),
            ));
        }
    };
    let length = check_length(length, limits)?;
    Ok(RegionLocation::Present(LocatedChunk {
        file,
        length,
        version: compression,
        external_internal_length_warning: warnings.0,
        truncated_region_header_warning: warnings.1,
    }))
}

fn check_length(length: u64, limits: RegionReadLimits) -> Result<usize, RegionError> {
    if length > limits.compressed_bytes as u64 {
        return Err(RegionError::CompressedLimit {
            length,
            limit: limits.compressed_bytes,
        });
    }
    usize::try_from(length).map_err(|_| RegionError::CompressedLimit {
        length,
        limit: limits.compressed_bytes,
    })
}

fn read_available(file: &mut File, output: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < output.len() {
        match file.read(&mut output[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

// Custom compression is unsupported, but Java consumes and validates its
// modified-UTF identifier first. Stream through fixed scratch rather than retain
// a name which no supported decoder uses. Truncation takes precedence over UTF
// errors because DataInputStream reads the declared bytes before decoding them.
fn custom_identifier(
    file: &mut File,
    payload_length: u64,
    scratch: &mut [u8; HEADER_BYTES],
) -> Result<bool, RegionError> {
    let mut length_bytes = [0_u8; 2];
    if payload_length < 2 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
    }
    file.read_exact(&mut length_bytes)?;
    let mut remaining = usize::from(u16::from_be_bytes(length_bytes));
    if remaining as u64 > payload_length - 2 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
    }
    let mut continuations = 0;
    let mut unit = 0_u16;
    let mut invalid_utf = false;
    let mut invalid_identifier = false;
    let mut colon_seen = false;
    let mut slash_before_colon = false;
    while remaining != 0 {
        let count = remaining.min(scratch.len());
        file.read_exact(&mut scratch[..count])?;
        remaining -= count;
        for &byte in &scratch[..count] {
            if invalid_utf {
                continue;
            }
            if continuations != 0 {
                if byte & 192 != 128 {
                    invalid_utf = true;
                    continue;
                }
                unit = (unit << 6) | u16::from(byte & 63);
                continuations -= 1;
                if continuations != 0 {
                    continue;
                }
            } else {
                match byte {
                    0..=127 => unit = u16::from(byte),
                    192..=223 => {
                        unit = u16::from(byte & 31);
                        continuations = 1;
                        continue;
                    }
                    224..=239 => {
                        unit = u16::from(byte & 15);
                        continuations = 2;
                        continue;
                    }
                    _ => {
                        invalid_utf = true;
                        continue;
                    }
                }
            }
            match unit {
                58 => {
                    invalid_identifier |= colon_seen || slash_before_colon;
                    colon_seen = true;
                }
                47 => slash_before_colon |= !colon_seen,
                45 | 46 | 48..=57 | 95 | 97..=122 => {}
                _ => invalid_identifier = true,
            }
        }
    }
    if invalid_utf || continuations != 0 {
        return Err(RegionError::InvalidCustomUtf);
    }
    Ok(!invalid_identifier)
}
