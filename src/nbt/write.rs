use super::{Error, Limits, NamedTag, NbtString, Tag};

/// Appends one network root. On any error the original output bytes are intact;
/// the Vec may retain capacity acquired during the failed attempt.
pub fn write_network(tag: &Tag, output: &mut Vec<u8>, limits: Limits) -> Result<(), Error> {
    limits.validate()?;
    let start = output.len();
    let mut writer = Writer {
        output,
        start,
        limits,
    };
    let result = writer.byte(tag.id()).and_then(|()| writer.payload(tag, 0));
    if result.is_err() {
        writer.output.truncate(start);
    }
    result
}

/// Appends one named disk root. End has no encoded name and requires an empty
/// `NamedTag.name`. Errors never silently replace oversized strings with empty
/// values (unlike Vanilla's optional `StringFallbackDataOutput`).
pub fn write_named(tag: &NamedTag, output: &mut Vec<u8>, limits: Limits) -> Result<(), Error> {
    limits.validate()?;
    let start = output.len();
    let mut writer = Writer {
        output,
        start,
        limits,
    };
    let result = (|| {
        if matches!(tag.tag, Tag::End) && !tag.name.is_empty() {
            return Err(Error::NamedEnd);
        }
        writer.byte(tag.tag.id())?;
        if !matches!(tag.tag, Tag::End) {
            writer.string(&tag.name)?;
        }
        writer.payload(&tag.tag, 0)
    })();
    if result.is_err() {
        writer.output.truncate(start);
    }
    result
}

struct Writer<'a> {
    output: &'a mut Vec<u8>,
    start: usize,
    limits: Limits,
}

impl Writer<'_> {
    fn reserve(&mut self, bytes: usize) -> Result<(), Error> {
        let length = (self.output.len() - self.start)
            .checked_add(bytes)
            .ok_or(Error::OutputLimit)?;
        if length > self.limits.output_bytes {
            return Err(Error::OutputLimit);
        }
        self.output
            .try_reserve(bytes)
            .map_err(|_| Error::AllocationFailed)
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.reserve(bytes.len())?;
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    fn byte(&mut self, byte: u8) -> Result<(), Error> {
        self.bytes(&[byte])
    }

    fn length(&mut self, count: usize) -> Result<(), Error> {
        self.bytes(
            &i32::try_from(count)
                .map_err(|_| Error::LengthOverflow)?
                .to_be_bytes(),
        )
    }

    fn container(&self, depth: usize) -> Result<usize, Error> {
        if depth >= self.limits.max_depth {
            Err(Error::DepthLimit)
        } else {
            Ok(depth + 1)
        }
    }

    fn string(&mut self, value: &NbtString) -> Result<(), Error> {
        let mut length = 0usize;
        for &unit in value.as_utf16() {
            length += match unit {
                1..=0x7f => 1,
                0..=0x7ff => 2,
                _ => 3,
            };
            if length > u16::MAX as usize {
                return Err(Error::StringTooLong);
            }
        }
        self.reserve(length + 2)?;
        self.output
            .extend_from_slice(&(length as u16).to_be_bytes());
        for &unit in value.as_utf16() {
            match unit {
                1..=0x7f => self.output.push(unit as u8),
                0..=0x7ff => {
                    self.output.push(0xc0 | (unit >> 6) as u8);
                    self.output.push(0x80 | (unit & 0x3f) as u8);
                }
                _ => {
                    self.output.push(0xe0 | (unit >> 12) as u8);
                    self.output.push(0x80 | ((unit >> 6) & 0x3f) as u8);
                    self.output.push(0x80 | (unit & 0x3f) as u8);
                }
            }
        }
        Ok(())
    }

    fn payload(&mut self, tag: &Tag, depth: usize) -> Result<(), Error> {
        match tag {
            Tag::End => Ok(()),
            Tag::Byte(value) => self.byte(*value as u8),
            Tag::Short(value) => self.bytes(&value.to_be_bytes()),
            Tag::Int(value) => self.bytes(&value.to_be_bytes()),
            Tag::Long(value) => self.bytes(&value.to_be_bytes()),
            Tag::Float(value) => {
                // Java DataOutput uses floatToIntBits/doubleToLongBits.
                let bits = if value.is_nan() {
                    0x7fc00000
                } else {
                    value.to_bits()
                };
                self.bytes(&bits.to_be_bytes())
            }
            Tag::Double(value) => {
                let bits = if value.is_nan() {
                    0x7ff8000000000000
                } else {
                    value.to_bits()
                };
                self.bytes(&bits.to_be_bytes())
            }
            Tag::ByteArray(values) => {
                self.length(values.len())?;
                self.reserve(values.len())?;
                self.output.extend(values.iter().map(|&value| value as u8));
                Ok(())
            }
            Tag::String(value) => self.string(value),
            Tag::List(values) => self.list(values, depth),
            Tag::Compound(compound) => {
                let child_depth = self.container(depth)?;
                for entry in compound.entries() {
                    if matches!(entry.value, Tag::End) {
                        return Err(Error::UnexpectedEnd);
                    }
                    self.byte(entry.value.id())?;
                    self.string(&entry.name)?;
                    self.payload(&entry.value, child_depth)?;
                }
                self.byte(0)
            }
            Tag::IntArray(values) => {
                self.length(values.len())?;
                self.reserve(values.len().checked_mul(4).ok_or(Error::LengthOverflow)?)?;
                for value in values {
                    self.output.extend_from_slice(&value.to_be_bytes());
                }
                Ok(())
            }
            Tag::LongArray(values) => {
                self.length(values.len())?;
                self.reserve(values.len().checked_mul(8).ok_or(Error::LengthOverflow)?)?;
                for value in values {
                    self.output.extend_from_slice(&value.to_be_bytes());
                }
                Ok(())
            }
        }
    }

    fn list(&mut self, values: &[Tag], depth: usize) -> Result<(), Error> {
        let child_depth = self.container(depth)?;
        let mut raw_type = 0;
        for value in values {
            if matches!(value, Tag::End) {
                return Err(Error::UnexpectedEnd);
            }
            if raw_type == 0 {
                raw_type = value.id();
            } else if raw_type != value.id() {
                raw_type = 10;
            }
        }
        self.byte(raw_type)?;
        self.length(values.len())?;
        for value in values {
            let needs_wrapper = raw_type == 10
                && !matches!(value, Tag::Compound(compound) if !compound.is_wrapper());
            if needs_wrapper {
                // Write wrappers directly. Never clone payloads or allocate a
                // temporary Compound, including the escape of {"": value}.
                let wrapped_depth = self.container(child_depth)?;
                self.byte(value.id())?;
                self.bytes(&[0, 0])?;
                self.payload(value, wrapped_depth)?;
                self.byte(0)?;
            } else {
                self.payload(value, child_depth)?;
            }
        }
        Ok(())
    }
}
