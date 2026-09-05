//! Bounded scalar packet fields for login and configuration.
//!
//! Wire behavior is source-guided by the pinned Vanilla `FriendlyByteBuf`,
//! `Utf8String`, and `Identifier` APIs. This concrete slice/cursor implementation
//! is independent of their buffer and codec class structure. Packet framing and
//! packet-specific collection limits belong to the caller.

use std::fmt;

use crate::wire;

const IDENTIFIER_UTF16_LIMIT: usize = 32767;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    UnexpectedEnd {
        needed: usize,
        remaining: usize,
    },
    VarInt(wire::DecodeError),
    NegativeLength(i32),
    LengthLimit {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    LengthOverflow,
    InvalidValue(&'static str),
    InvalidIdentifier,
    AllocationFailed,
    TrailingBytes(usize),
}

impl fmt::Display for PacketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd { needed, remaining } => write!(
                formatter,
                "packet field needs {needed} bytes; {remaining} remain"
            ),
            Self::VarInt(error) => error.fmt(formatter),
            Self::NegativeLength(length) => write!(formatter, "negative packet length {length}"),
            Self::LengthLimit {
                kind,
                actual,
                maximum,
            } => {
                write!(formatter, "{kind} length {actual} exceeds limit {maximum}")
            }
            Self::LengthOverflow => {
                formatter.write_str("packet length exceeds representable range")
            }
            Self::InvalidValue(message) => formatter.write_str(message),
            Self::InvalidIdentifier => formatter.write_str("invalid packet identifier"),
            Self::AllocationFailed => formatter.write_str("packet allocation failed"),
            Self::TrailingBytes(count) => write!(formatter, "packet has {count} trailing bytes"),
        }
    }
}

impl std::error::Error for PacketError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::VarInt(error) => Some(error),
            _ => None,
        }
    }
}

/// Borrows a complete packet body. Each field advances the cursor only on success.
#[derive(Debug)]
pub struct PacketReader<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> PacketReader<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn remaining(&self) -> usize {
        self.input.len() - self.position
    }

    pub fn finish(&self) -> Result<(), PacketError> {
        match self.remaining() {
            0 => Ok(()),
            count => Err(PacketError::TrailingBytes(count)),
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], PacketError> {
        let value = self.input[self.position..]
            .get(..count)
            .ok_or(PacketError::UnexpectedEnd {
                needed: count,
                remaining: self.remaining(),
            })?;
        self.position += count;
        Ok(value)
    }

    pub fn varint(&mut self) -> Result<i32, PacketError> {
        let (value, length) =
            wire::read_varint(&self.input[self.position..]).map_err(PacketError::VarInt)?;
        self.position += length;
        Ok(value)
    }

    pub fn boolean(&mut self) -> Result<bool, PacketError> {
        Ok(self.unsigned_byte()? != 0)
    }

    pub fn byte(&mut self) -> Result<i8, PacketError> {
        Ok(self.unsigned_byte()? as i8)
    }

    pub fn unsigned_byte(&mut self) -> Result<u8, PacketError> {
        Ok(self.take(1)?[0])
    }

    pub fn short(&mut self) -> Result<i16, PacketError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn unsigned_short(&mut self) -> Result<u16, PacketError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn int(&mut self) -> Result<i32, PacketError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn long(&mut self) -> Result<i64, PacketError> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn float(&mut self) -> Result<f32, PacketError> {
        Ok(f32::from_bits(self.int()? as u32))
    }

    pub fn double(&mut self) -> Result<f64, PacketError> {
        Ok(f64::from_bits(self.long()? as u64))
    }

    /// UUID bytes are the most-significant long followed by the least-significant
    /// long, both big-endian; no host-endian UUID representation is involved.
    pub fn uuid(&mut self) -> Result<[u8; 16], PacketError> {
        Ok(self.take(16)?.try_into().unwrap())
    }

    fn prefixed(
        &self,
        maximum: usize,
        kind: &'static str,
    ) -> Result<(&'a [u8], usize), PacketError> {
        let remaining = &self.input[self.position..];
        let (length, prefix) = wire::read_varint(remaining).map_err(PacketError::VarInt)?;
        let length = usize::try_from(length).map_err(|_| PacketError::NegativeLength(length))?;
        check_length(kind, length, maximum)?;
        let value = remaining[prefix..]
            .get(..length)
            .ok_or(PacketError::UnexpectedEnd {
                needed: length,
                remaining: remaining.len() - prefix,
            })?;
        Ok((value, prefix + length))
    }

    /// Decodes Java's replacement UTF-8, checking encoded bytes and UTF-16 units
    /// before allocating. A complete encoded surrogate becomes one replacement.
    pub fn utf(&mut self, max_utf16: usize) -> Result<String, PacketError> {
        let encoded_limit = max_utf16
            .checked_mul(3)
            .ok_or(PacketError::LengthOverflow)?;
        let (bytes, consumed) = self.prefixed(encoded_limit, "encoded UTF-8")?;
        let mut decoded_bytes = 0usize;
        let mut units = 0;
        for chunk in JavaUtf8Chunks(bytes) {
            decoded_bytes = decoded_bytes
                .checked_add(chunk.len())
                .ok_or(PacketError::LengthOverflow)?;
            units += chunk.encode_utf16().count();
            check_length("UTF-16", units, max_utf16)?;
        }
        let mut value = String::new();
        value
            .try_reserve_exact(decoded_bytes)
            .map_err(|_| PacketError::AllocationFailed)?;
        for chunk in JavaUtf8Chunks(bytes) {
            value.push_str(chunk);
        }
        self.position += consumed;
        Ok(value)
    }

    /// Validates a string and compares its Java replacement-decoded value
    /// without allocating. `None` validates and skips the field, returning false.
    /// A mismatch still validates the entire field before advancing the cursor.
    pub fn utf_equals(
        &mut self,
        expected: Option<&str>,
        max_utf16: usize,
    ) -> Result<bool, PacketError> {
        let encoded_limit = max_utf16
            .checked_mul(3)
            .ok_or(PacketError::LengthOverflow)?;
        let (bytes, consumed) = self.prefixed(encoded_limit, "encoded UTF-8")?;
        // Every UTF-16 unit consumes at least one input byte, including Java's
        // replacement characters. Non-ASCII input cannot decode to ASCII.
        if bytes.len() <= max_utf16 {
            let matches = match expected {
                None => Some(false),
                Some(value) if value.is_ascii() => Some(bytes == value.as_bytes()),
                Some(_) => None,
            };
            if let Some(matches) = matches {
                self.position += consumed;
                return Ok(matches);
            }
        }
        let mut unmatched = expected.unwrap_or("").as_bytes();
        let mut matches = expected.is_some();
        let mut units = 0;
        for chunk in JavaUtf8Chunks(bytes) {
            units += chunk.encode_utf16().count();
            check_length("UTF-16", units, max_utf16)?;
            if matches {
                match unmatched.strip_prefix(chunk.as_bytes()) {
                    Some(rest) => unmatched = rest,
                    None => matches = false,
                }
            }
        }
        self.position += consumed;
        Ok(matches && unmatched.is_empty())
    }

    pub fn bytes(&mut self, maximum: usize) -> Result<&'a [u8], PacketError> {
        let (value, consumed) = self.prefixed(maximum, "byte array")?;
        self.position += consumed;
        Ok(value)
    }

    pub fn remaining_bytes(&mut self, maximum: usize) -> Result<&'a [u8], PacketError> {
        check_length("remaining bytes", self.remaining(), maximum)?;
        self.take(self.remaining())
    }

    pub fn identifier(&mut self) -> Result<String, PacketError> {
        let mut next = Self {
            input: self.input,
            position: self.position,
        };
        let mut value = next.utf(IDENTIFIER_UTF16_LIMIT)?;
        identifier_parts(&value)?;
        if value.starts_with(':') {
            value.remove(0);
        }
        if !value.contains(':') {
            value
                .try_reserve_exact(10)
                .map_err(|_| PacketError::AllocationFailed)?;
            value.insert_str(0, "minecraft:");
        }
        self.position = next.position;
        Ok(value)
    }
}

/// Append-only packet output with a cumulative byte cap. Every write validates
/// the whole field and admits its storage before changing the output.
#[derive(Debug)]
pub struct PacketWriter {
    output: Vec<u8>,
    max_bytes: usize,
}

impl PacketWriter {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            output: Vec::new(),
            max_bytes,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.output
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.output
    }

    fn reserve(&mut self, additional: usize) -> Result<(), PacketError> {
        let required = self
            .output
            .len()
            .checked_add(additional)
            .ok_or(PacketError::LengthOverflow)?;
        check_length("packet output", required, self.max_bytes)?;
        if required > self.output.capacity() {
            // Geometric growth avoids one allocation per scalar while every
            // requested capacity remains inside the caller's packet budget.
            let capacity = self
                .output
                .capacity()
                .saturating_mul(2)
                .max(64)
                .min(self.max_bytes)
                .max(required);
            self.output
                .try_reserve_exact(capacity - self.output.len())
                .map_err(|_| PacketError::AllocationFailed)?;
        }
        Ok(())
    }

    pub fn raw(&mut self, value: &[u8]) -> Result<(), PacketError> {
        self.reserve(value.len())?;
        self.output.extend_from_slice(value);
        Ok(())
    }

    pub fn varint(&mut self, value: i32) -> Result<(), PacketError> {
        let mut bytes = [0; wire::MAX_VARINT_BYTES];
        let length = wire::write_varint(value, &mut bytes).unwrap();
        self.raw(&bytes[..length])
    }

    pub fn boolean(&mut self, value: bool) -> Result<(), PacketError> {
        self.unsigned_byte(u8::from(value))
    }

    pub fn byte(&mut self, value: i8) -> Result<(), PacketError> {
        self.unsigned_byte(value as u8)
    }

    pub fn unsigned_byte(&mut self, value: u8) -> Result<(), PacketError> {
        self.raw(&[value])
    }

    pub fn short(&mut self, value: i16) -> Result<(), PacketError> {
        self.raw(&value.to_be_bytes())
    }

    pub fn unsigned_short(&mut self, value: u16) -> Result<(), PacketError> {
        self.raw(&value.to_be_bytes())
    }

    pub fn int(&mut self, value: i32) -> Result<(), PacketError> {
        self.raw(&value.to_be_bytes())
    }

    pub fn long(&mut self, value: i64) -> Result<(), PacketError> {
        self.raw(&value.to_be_bytes())
    }

    pub fn float(&mut self, value: f32) -> Result<(), PacketError> {
        self.raw(&value.to_bits().to_be_bytes())
    }

    pub fn double(&mut self, value: f64) -> Result<(), PacketError> {
        self.raw(&value.to_bits().to_be_bytes())
    }

    pub fn uuid(&mut self, value: [u8; 16]) -> Result<(), PacketError> {
        self.raw(&value)
    }

    pub fn utf(&mut self, value: &str, max_utf16: usize) -> Result<(), PacketError> {
        check_length("UTF-16", value.encode_utf16().count(), max_utf16)?;
        let encoded_limit = max_utf16
            .checked_mul(3)
            .ok_or(PacketError::LengthOverflow)?;
        check_length("encoded UTF-8", value.len(), encoded_limit)?;
        self.bytes(value.as_bytes(), encoded_limit)
    }

    pub fn bytes(&mut self, value: &[u8], maximum: usize) -> Result<(), PacketError> {
        check_length("byte array", value.len(), maximum)?;
        self.length_prefix(value.len())?;
        self.output.extend_from_slice(value);
        Ok(())
    }

    // Reserves the prefix and payload together so a failed field is atomic.
    fn length_prefix(&mut self, length: usize) -> Result<(), PacketError> {
        let wire_length = i32::try_from(length).map_err(|_| PacketError::LengthOverflow)?;
        let total = wire::varint_len(wire_length)
            .checked_add(length)
            .ok_or(PacketError::LengthOverflow)?;
        self.reserve(total)?;
        self.varint(wire_length)
    }

    pub fn identifier(&mut self, value: &str) -> Result<(), PacketError> {
        let (namespace, path) = identifier_parts(value)?;
        let length = namespace
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_add(path.len()))
            .ok_or(PacketError::LengthOverflow)?;
        // Identifier validation admits only ASCII, so bytes equal UTF-16 units.
        check_length("UTF-16", length, IDENTIFIER_UTF16_LIMIT)?;
        self.length_prefix(length)?;
        self.output.extend_from_slice(namespace.as_bytes());
        self.output.push(b':');
        self.output.extend_from_slice(path.as_bytes());
        Ok(())
    }
}

fn check_length(kind: &'static str, actual: usize, maximum: usize) -> Result<(), PacketError> {
    if actual > maximum {
        Err(PacketError::LengthLimit {
            kind,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn identifier_parts(value: &str) -> Result<(&str, &str), PacketError> {
    let (namespace, path) = match value.split_once(':') {
        Some(("", path)) => ("minecraft", path),
        Some(parts) => parts,
        None => ("minecraft", value),
    };
    let common =
        |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte);
    if namespace == ".."
        || !namespace.bytes().all(common)
        || !path.bytes().all(|byte| common(byte) || byte == b'/')
    {
        return Err(PacketError::InvalidIdentifier);
    }
    Ok((namespace, path))
}

/// Rust and Java group malformed UTF-8 prefixes alike except encoded UTF-16
/// surrogates: Java replaces their two- or three-byte prefix as one character.
/// The standard decoder supplies validated chunks; this adapter handles that
/// one compatibility difference without an additional owned temporary buffer.
struct JavaUtf8Chunks<'a>(&'a [u8]);

impl<'a> Iterator for JavaUtf8Chunks<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        match std::str::from_utf8(self.0) {
            Ok(value) => {
                self.0 = &[];
                Some(value)
            }
            Err(error) if error.valid_up_to() > 0 => {
                let (valid, rest) = self.0.split_at(error.valid_up_to());
                self.0 = rest;
                Some(std::str::from_utf8(valid).unwrap())
            }
            Err(error) => {
                let length = if self.0[0] == 0xed
                    && self.0.get(1).is_some_and(|b| *b >= 0xa0 && *b <= 0xbf)
                {
                    2 + usize::from(self.0.get(2).is_some_and(|b| b & 0xc0 == 0x80))
                } else {
                    error.error_len().unwrap_or(self.0.len())
                };
                self.0 = &self.0[length..];
                Some("\u{fffd}")
            }
        }
    }
}
