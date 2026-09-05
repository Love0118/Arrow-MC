//! Java Edition packet compression with an ordered threshold-write boundary.
//!
//! Connection state is small; a bounded CPU worker owns and reuses the larger
//! compression scratch. Caller buffers and allocation admission stay explicit.
//! Vanilla validates produced length, not zlib stream completion: a declared
//! prefix, some truncated trailers and trailing bytes can therefore be accepted.

use crate::wire::{read_varint, write_varint};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use std::{fmt, io};
use tokio::{io::AsyncWriteExt, net::TcpStream};

pub const MAX_FRAME_BODY_BYTES: usize = 0x1f_ffff;
pub const MAX_UNCOMPRESSED_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct CompressionLimits {
    /// Outer frame body, including the compression length field when enabled.
    pub max_frame_body_bytes: usize,
    /// Declared inflated size and encoder input limit when compression is
    /// enabled. Raw DataLength=0 uses the outer-frame bound, as in Vanilla.
    pub max_uncompressed_bytes: usize,
}

impl Default for CompressionLimits {
    fn default() -> Self {
        Self {
            max_frame_body_bytes: MAX_FRAME_BODY_BYTES,
            max_uncompressed_bytes: MAX_UNCOMPRESSED_BYTES,
        }
    }
}

impl CompressionLimits {
    fn validate(self) -> Result<(), CompressionError> {
        if self.max_frame_body_bytes == 0
            || self.max_frame_body_bytes > MAX_FRAME_BODY_BYTES
            || self.max_uncompressed_bytes > MAX_UNCOMPRESSED_BYTES
        {
            Err(CompressionError::InvalidLimits)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub enum CompressionError {
    InvalidLimits,
    Truncated,
    InvalidVarInt,
    InvalidFrameLength,
    FrameTooLarge,
    NegativeDataLength,
    BelowThreshold,
    DecompressedTooLarge,
    InvalidZlib,
    LengthMismatch,
    AllocationLimit,
    AllocationFailed,
    UnusableState,
    Io(io::Error),
}

impl fmt::Display for CompressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::Io(error) = self {
            return write!(formatter, "compression transition write failed: {error}");
        }
        formatter.write_str(match self {
            Self::InvalidLimits => "compression limits exceed protocol bounds",
            Self::Truncated => "truncated compression frame",
            Self::InvalidVarInt => "invalid compression data length VarInt",
            Self::InvalidFrameLength => "invalid 21-bit frame length",
            Self::FrameTooLarge => "compressed frame exceeds admitted size",
            Self::NegativeDataLength => "negative decompressed data length",
            Self::BelowThreshold => "compressed data length is below threshold",
            Self::DecompressedTooLarge => "decompressed packet exceeds admitted size",
            Self::InvalidZlib => "invalid zlib stream",
            Self::LengthMismatch => "decompressed data did not fill the declared length",
            Self::AllocationLimit => "compression allocation budget exhausted",
            Self::AllocationFailed => "compression output allocation failed",
            Self::UnusableState => {
                "connection compression state is unusable after an interrupted write"
            }
            Self::Io(_) => unreachable!(),
        })
    }
}

impl std::error::Error for CompressionError {}

/// Provision once per bounded CPU worker, not once per idle connection.
/// The backend owns fixed compressor/inflater state; the per-call allocation
/// counter covers caller Vec growth, not those provisioned backend allocations.
pub struct CompressionScratch {
    compressor: Compress,
    decompressor: Decompress,
    chunk: [u8; 8192],
}

impl Default for CompressionScratch {
    fn default() -> Self {
        Self {
            compressor: Compress::new(Compression::default(), true),
            decompressor: Decompress::new(true),
            chunk: [0; 8192],
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Disabled,
    Enabled(i32),
    Unusable,
}

/// Select the initial negotiated state explicitly; new TCP connections use -1.
/// Later changes use prepare_threshold + write_threshold, never a naked setter.
pub struct CompressionState {
    mode: Mode,
}

impl CompressionState {
    pub fn new(threshold: i32) -> Self {
        Self {
            mode: mode(threshold),
        }
    }

    pub fn threshold(&self) -> Result<Option<i32>, CompressionError> {
        match self.mode {
            Mode::Disabled => Ok(None),
            Mode::Enabled(value) => Ok(Some(value)),
            Mode::Unusable => Err(CompressionError::UnusableState),
        }
    }

    /// Appends one complete outer frame. Input includes the packet ID. Errors
    /// preserve existing output bytes; capacities acquired before failure may
    /// remain and all full-capacity growth requests stay charged to the caller.
    pub fn encode_frame(
        &self,
        packet: &[u8],
        scratch: &mut CompressionScratch,
        output: &mut Vec<u8>,
        limits: CompressionLimits,
        allocation_remaining: &mut usize,
    ) -> Result<(), CompressionError> {
        limits.validate()?;
        let threshold = self.threshold()?;
        let start = output.len();
        let result = encode(
            packet,
            threshold,
            scratch,
            output,
            start,
            limits,
            allocation_remaining,
        );
        if result.is_err() {
            output.truncate(start);
            scratch.compressor.reset();
        }
        result
    }

    /// Decodes exactly one outer frame and leaves subsequent frames in input.
    /// An error preserves the input cursor and preexisting output bytes. Raw
    /// DataLength=0 payloads bypass the compression threshold, as in Vanilla.
    pub fn decode_frame(
        &self,
        input: &mut &[u8],
        scratch: &mut CompressionScratch,
        output: &mut Vec<u8>,
        limits: CompressionLimits,
        allocation_remaining: &mut usize,
    ) -> Result<(), CompressionError> {
        limits.validate()?;
        let threshold = self.threshold()?;
        let (body, consumed) = frame_body(input, limits.max_frame_body_bytes)?;
        let start = output.len();
        let result = decode(
            body,
            threshold,
            scratch,
            output,
            start,
            limits,
            allocation_remaining,
        );
        if result.is_err() {
            output.truncate(start);
        } else {
            *input = &input[consumed..];
        }
        result
    }

    /// Prepares Set Compression (login packet ID 3) under the old mode. The
    /// returned guard exclusively borrows connection state, but not worker
    /// scratch, so scratch is reusable while a slow peer delays the write.
    /// Pure validation/allocation errors and dropping an unstarted guard leave
    /// the old state intact. The connection owner must write earlier queued
    /// frames before this guard; no client acknowledgement is required.
    pub fn prepare_threshold<'a>(
        &'a mut self,
        threshold: i32,
        scratch: &mut CompressionScratch,
        limits: CompressionLimits,
        allocation_remaining: &mut usize,
    ) -> Result<ThresholdWrite<'a>, CompressionError> {
        let mut packet = [0; 6];
        packet[0] = 3;
        let length = write_varint(threshold, &mut packet[1..])
            .map_err(|_| CompressionError::InvalidVarInt)?;
        let mut frame = Vec::new();
        self.encode_frame(
            &packet[..length + 1],
            scratch,
            &mut frame,
            limits,
            allocation_remaining,
        )?;
        Ok(ThresholdWrite {
            state: self,
            threshold,
            frame,
        })
    }
}

pub struct ThresholdWrite<'a> {
    state: &'a mut CompressionState,
    threshold: i32,
    frame: Vec<u8>,
}

impl ThresholdWrite<'_> {
    /// Commits the new mode only after the complete old-mode frame writes.
    /// This future owns the socket, so error or cancellation also closes it.
    /// Once polled, cancellation/error poisons state. Dropping an unpolled future
    /// closes its socket while preserving the old mode. A successful OS write
    /// is the barrier, not peer acknowledgement. No worker scratch spans the await.
    pub async fn write_threshold(
        self,
        mut stream: TcpStream,
    ) -> Result<TcpStream, CompressionError> {
        self.state.mode = Mode::Unusable;
        stream
            .write_all(&self.frame)
            .await
            .map_err(CompressionError::Io)?;
        self.state.mode = mode(self.threshold);
        Ok(stream)
    }
}

fn mode(threshold: i32) -> Mode {
    if threshold < 0 {
        Mode::Disabled
    } else {
        Mode::Enabled(threshold)
    }
}

fn frame_body(input: &[u8], limit: usize) -> Result<(&[u8], usize), CompressionError> {
    let mut length = 0usize;
    for index in 0..3 {
        let byte = *input.get(index).ok_or(CompressionError::Truncated)?;
        length |= usize::from(byte & 127) << (index * 7);
        if byte & 128 == 0 {
            if length == 0 {
                return Err(CompressionError::InvalidFrameLength);
            }
            if length > limit {
                return Err(CompressionError::FrameTooLarge);
            }
            let end = index + 1 + length;
            return Ok((
                input
                    .get(index + 1..end)
                    .ok_or(CompressionError::Truncated)?,
                end,
            ));
        }
    }
    Err(CompressionError::InvalidFrameLength)
}

fn encode(
    packet: &[u8],
    threshold: Option<i32>,
    scratch: &mut CompressionScratch,
    output: &mut Vec<u8>,
    start: usize,
    limits: CompressionLimits,
    allocation_remaining: &mut usize,
) -> Result<(), CompressionError> {
    if threshold.is_some() && packet.len() > limits.max_uncompressed_bytes {
        return Err(CompressionError::DecompressedTooLarge);
    }
    let mut length = [0; 5];
    if threshold.is_none_or(|threshold| packet.len() < threshold as usize) {
        let body_length = packet
            .len()
            .checked_add(usize::from(threshold.is_some()))
            .ok_or(CompressionError::FrameTooLarge)?;
        if body_length == 0 {
            return Err(CompressionError::InvalidFrameLength);
        }
        if body_length > limits.max_frame_body_bytes {
            return Err(CompressionError::FrameTooLarge);
        }
        let count = write_varint(body_length as i32, &mut length)
            .map_err(|_| CompressionError::InvalidVarInt)?;
        append(
            output,
            &length[..count],
            start,
            limits.max_frame_body_bytes + 3,
            allocation_remaining,
        )?;
        if threshold.is_some() {
            append(
                output,
                &[0],
                start,
                limits.max_frame_body_bytes + 3,
                allocation_remaining,
            )?;
        }
        return append(
            output,
            packet,
            start,
            limits.max_frame_body_bytes + 3,
            allocation_remaining,
        );
    }
    let count = write_varint(packet.len() as i32, &mut length)
        .map_err(|_| CompressionError::InvalidVarInt)?;
    append(
        output,
        &[0; 3],
        start,
        limits.max_frame_body_bytes + 3,
        allocation_remaining,
    )?;
    append(
        output,
        &length[..count],
        start,
        limits.max_frame_body_bytes + 3,
        allocation_remaining,
    )?;
    scratch.compressor.reset();
    loop {
        let before_in = scratch.compressor.total_in();
        let before_out = scratch.compressor.total_out();
        let status = scratch
            .compressor
            .compress(
                &packet[before_in as usize..],
                &mut scratch.chunk,
                FlushCompress::Finish,
            )
            .map_err(|_| CompressionError::InvalidZlib)?;
        let written = (scratch.compressor.total_out() - before_out) as usize;
        append(
            output,
            &scratch.chunk[..written],
            start,
            limits.max_frame_body_bytes + 3,
            allocation_remaining,
        )?;
        if status == Status::StreamEnd {
            break;
        }
        if scratch.compressor.total_in() == before_in && written == 0 {
            return Err(CompressionError::InvalidZlib);
        }
    }
    let body_length = output.len() - start - 3;
    let prefix = write_varint(body_length as i32, &mut length)
        .map_err(|_| CompressionError::InvalidVarInt)?;
    output.copy_within(start + 3.., start + prefix);
    output.truncate(output.len() - (3 - prefix));
    output[start..start + prefix].copy_from_slice(&length[..prefix]);
    Ok(())
}

fn decode(
    body: &[u8],
    threshold: Option<i32>,
    scratch: &mut CompressionScratch,
    output: &mut Vec<u8>,
    start: usize,
    limits: CompressionLimits,
    allocation_remaining: &mut usize,
) -> Result<(), CompressionError> {
    let Some(threshold) = threshold else {
        return append(
            output,
            body,
            start,
            limits.max_frame_body_bytes,
            allocation_remaining,
        );
    };
    let (declared, prefix) = read_varint(body).map_err(|_| CompressionError::InvalidVarInt)?;
    let compressed = &body[prefix..];
    if declared == 0 {
        return append(
            output,
            compressed,
            start,
            limits.max_frame_body_bytes,
            allocation_remaining,
        );
    }
    if declared < 0 {
        return Err(CompressionError::NegativeDataLength);
    }
    if declared < threshold {
        return Err(CompressionError::BelowThreshold);
    }
    let length = declared as usize;
    if length > limits.max_uncompressed_bytes {
        return Err(CompressionError::DecompressedTooLarge);
    }
    reserve(
        output,
        length,
        start,
        limits.max_uncompressed_bytes,
        allocation_remaining,
    )?;
    output.resize(start + length, 0);
    scratch.decompressor.reset(true);
    let result = scratch
        .decompressor
        .decompress(compressed, &mut output[start..], FlushDecompress::None)
        .map_err(|_| CompressionError::InvalidZlib);
    let produced = scratch.decompressor.total_out();
    scratch.decompressor.reset(true);
    result?;
    if produced != length as u64 {
        return Err(CompressionError::LengthMismatch);
    }
    Ok(())
}

fn append(
    output: &mut Vec<u8>,
    bytes: &[u8],
    start: usize,
    limit: usize,
    allocation_remaining: &mut usize,
) -> Result<(), CompressionError> {
    reserve(output, bytes.len(), start, limit, allocation_remaining)?;
    output.extend_from_slice(bytes);
    Ok(())
}

fn reserve(
    output: &mut Vec<u8>,
    additional: usize,
    start: usize,
    limit: usize,
    allocation_remaining: &mut usize,
) -> Result<(), CompressionError> {
    let end = output
        .len()
        .checked_add(additional)
        .ok_or(CompressionError::FrameTooLarge)?;
    if end - start > limit {
        return Err(CompressionError::FrameTooLarge);
    }
    if end > output.capacity() {
        let capacity = output
            .capacity()
            .saturating_mul(2)
            .max(end)
            .min(start.saturating_add(limit));
        *allocation_remaining = allocation_remaining
            .checked_sub(capacity)
            .ok_or(CompressionError::AllocationLimit)?;
        output
            .try_reserve_exact(capacity - output.len())
            .map_err(|_| CompressionError::AllocationFailed)?;
    }
    Ok(())
}
