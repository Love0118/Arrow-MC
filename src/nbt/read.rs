use super::{Compound, CompoundEntry, Error, Limits, NamedTag, NbtString, Tag};
use std::mem::size_of;

/// Reads one network root. Unconsumed packet bytes remain in `input`.
/// On failure `input` is unchanged and all partial decoded data is dropped.
pub fn read_network(input: &mut &[u8], limits: Limits) -> Result<Tag, Error> {
    limits.validate()?;
    let mut reader = Reader::new(input, limits);
    let id = reader.byte()?;
    let tag = if id == 0 {
        Tag::End
    } else {
        reader.payload(id, 0)?
    };
    *input = reader.input;
    Ok(tag)
}

/// Reads a disk-style root type, modified UTF-8 name and payload. The root name
/// is preserved and validated; Vanilla's disk helper discards/skips this name.
/// End has no encoded name. Failure leaves `input` unchanged.
pub fn read_named(input: &mut &[u8], limits: Limits) -> Result<NamedTag, Error> {
    limits.validate()?;
    let mut reader = Reader::new(input, limits);
    let id = reader.byte()?;
    let result = if id == 0 {
        NamedTag {
            name: NbtString::default(),
            tag: Tag::End,
        }
    } else {
        reader.check_id(id)?;
        let name = reader.string(false)?;
        NamedTag {
            name,
            tag: reader.payload(id, 0)?,
        }
    };
    *input = reader.input;
    Ok(result)
}

struct Reader<'a> {
    input: &'a [u8],
    limits: Limits,
    quota: usize,
    allocation: usize,
}

impl<'a> Reader<'a> {
    fn new(input: &'a [u8], limits: Limits) -> Self {
        Self {
            input,
            limits,
            quota: 0,
            allocation: 0,
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let bytes = self.input.get(..count).ok_or(Error::Truncated)?;
        self.input = &self.input[count..];
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn short(&mut self) -> Result<i16, Error> {
        let bytes = self.take(2)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn int(&mut self) -> Result<i32, Error> {
        let bytes = self.take(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn long(&mut self) -> Result<i64, Error> {
        let bytes = self.take(8)?;
        Ok(i64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn count(&mut self) -> Result<usize, Error> {
        let count = self.int()?;
        usize::try_from(count).map_err(|_| Error::NegativeLength(count))
    }

    fn account(&mut self, bytes: usize) -> Result<(), Error> {
        self.quota = self
            .quota
            .checked_add(bytes)
            .ok_or(Error::VanillaQuotaExceeded)?;
        if self.quota > self.limits.vanilla_quota_bytes {
            Err(Error::VanillaQuotaExceeded)
        } else {
            Ok(())
        }
    }

    fn allocation(&mut self, bytes: usize) -> Result<(), Error> {
        self.allocation = self
            .allocation
            .checked_add(bytes)
            .ok_or(Error::AllocationBudgetExceeded)?;
        if self.allocation > self.limits.allocation_bytes {
            Err(Error::AllocationBudgetExceeded)
        } else {
            Ok(())
        }
    }

    fn check_id(&self, id: u8) -> Result<(), Error> {
        if id > 12 {
            Err(Error::UnknownTag(id))
        } else {
            Ok(())
        }
    }

    fn container(&self, depth: usize) -> Result<usize, Error> {
        if depth >= self.limits.max_depth {
            Err(Error::DepthLimit)
        } else {
            Ok(depth + 1)
        }
    }

    fn string(&mut self, account_units: bool) -> Result<NbtString, Error> {
        let length = self.short()? as u16 as usize;
        let bytes = self.take(length)?;
        // First validate/count without allocation. Java readUTF accepts literal
        // NUL and non-shortest 2/3-byte forms, but not four-byte UTF-8.
        let units = modified_utf8_count(bytes)?;
        if account_units {
            self.account(units * 2)?;
        }
        self.allocation(units * size_of::<u16>())?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(units)
            .map_err(|_| Error::AllocationFailed)?;
        let mut offset = 0;
        while offset < bytes.len() {
            result.push(modified_utf8_unit(bytes, &mut offset)?);
        }
        Ok(NbtString::from_utf16(result))
    }

    fn payload(&mut self, id: u8, depth: usize) -> Result<Tag, Error> {
        match id {
            0 => Err(Error::UnexpectedEnd),
            1 => {
                self.account(9)?;
                Ok(Tag::Byte(self.byte()? as i8))
            }
            2 => {
                self.account(10)?;
                Ok(Tag::Short(self.short()?))
            }
            3 => {
                self.account(12)?;
                Ok(Tag::Int(self.int()?))
            }
            4 => {
                self.account(16)?;
                Ok(Tag::Long(self.long()?))
            }
            5 => {
                self.account(12)?;
                let value = f32::from_bits(self.int()? as u32);
                // FloatTag.valueOf returns its positive-zero singleton.
                Ok(Tag::Float(if value == 0.0 { 0.0 } else { value }))
            }
            6 => {
                self.account(16)?;
                let value = f64::from_bits(self.long()? as u64);
                Ok(Tag::Double(if value == 0.0 { 0.0 } else { value }))
            }
            7 => self.byte_array(),
            8 => {
                self.account(36)?;
                Ok(Tag::String(self.string(true)?))
            }
            9 => self.list(depth),
            10 => self.compound(depth),
            11 => self.int_array(),
            12 => self.long_array(),
            _ => Err(Error::UnknownTag(id)),
        }
    }

    fn array_length(&mut self, width: usize) -> Result<usize, Error> {
        self.account(24)?;
        let count = self.count()?;
        let bytes = count.checked_mul(width).ok_or(Error::LengthOverflow)?;
        self.account(bytes)?;
        // Reject truncated huge arrays before reserving even if budgets allow.
        if bytes > self.input.len() {
            return Err(Error::Truncated);
        }
        self.allocation(bytes)?;
        Ok(count)
    }

    fn byte_array(&mut self) -> Result<Tag, Error> {
        let count = self.array_length(1)?;
        let bytes = self.take(count)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Error::AllocationFailed)?;
        values.extend(bytes.iter().map(|&byte| byte as i8));
        Ok(Tag::ByteArray(values))
    }

    fn int_array(&mut self) -> Result<Tag, Error> {
        let count = self.array_length(4)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Error::AllocationFailed)?;
        for _ in 0..count {
            values.push(self.int()?);
        }
        Ok(Tag::IntArray(values))
    }

    fn long_array(&mut self) -> Result<Tag, Error> {
        let count = self.array_length(8)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Error::AllocationFailed)?;
        for _ in 0..count {
            values.push(self.long()?);
        }
        Ok(Tag::LongArray(values))
    }

    fn list(&mut self, depth: usize) -> Result<Tag, Error> {
        let child_depth = self.container(depth)?;
        self.account(36)?;
        let id = self.byte()?;
        let count = self.count()?;
        if count != 0 {
            if id == 0 {
                return Err(Error::UnexpectedEnd);
            }
            self.check_id(id)?;
        }
        // Vanilla accepts even unknown raw element IDs when the list is empty.
        self.account(count.checked_mul(4).ok_or(Error::LengthOverflow)?)?;
        // A length field must not cause a large reservation when the available
        // bytes cannot even contain the smallest payloads of the declared type.
        let minimum_width = match id {
            2 | 8 => 2,
            3 | 5 | 7 | 11 | 12 => 4,
            4 | 6 => 8,
            9 => 5,
            _ => 1,
        };
        let minimum_bytes = count
            .checked_mul(minimum_width)
            .ok_or(Error::LengthOverflow)?;
        if minimum_bytes > self.input.len() {
            return Err(Error::Truncated);
        }
        self.allocation(
            count
                .checked_mul(size_of::<Tag>())
                .ok_or(Error::LengthOverflow)?,
        )?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Error::AllocationFailed)?;
        for _ in 0..count {
            let value = self.payload(id, child_depth)?;
            values.push(match value {
                Tag::Compound(mut compound) if compound.is_wrapper() => {
                    compound.0.pop().unwrap().value
                }
                value => value,
            });
        }
        Ok(Tag::List(values))
    }

    fn compound(&mut self, depth: usize) -> Result<Tag, Error> {
        let child_depth = self.container(depth)?;
        self.account(48)?;
        let mut entries: Vec<CompoundEntry> = Vec::new();
        loop {
            let id = self.byte()?;
            if id == 0 {
                break;
            }
            self.check_id(id)?;
            self.account(28)?;
            let name = self.string(true)?;
            let value = self.payload(id, child_depth)?;
            if entries.len() == entries.capacity() {
                // Charge each replacement capacity in full: old/new buffers
                // may coexist during reallocation. Growth stays amortized O(n).
                let capacity = entries
                    .len()
                    .max(4)
                    .checked_mul(2)
                    .ok_or(Error::LengthOverflow)?;
                self.allocation(
                    capacity
                        .checked_mul(size_of::<CompoundEntry>())
                        .ok_or(Error::LengthOverflow)?,
                )?;
                entries
                    .try_reserve_exact(capacity - entries.len())
                    .map_err(|_| Error::AllocationFailed)?;
            }
            entries.push(CompoundEntry {
                name,
                value,
                sequence: entries.len(),
            });
        }
        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name).then(a.sequence.cmp(&b.sequence)));
        entries.dedup_by(|later, earlier| {
            if later.name == earlier.name {
                std::mem::swap(&mut later.value, &mut earlier.value);
                true
            } else {
                false
            }
        });
        // CompoundTag charges each distinct map entry once; repeated names and
        // their replaced values still consume their full read quota above.
        self.account(entries.len().checked_mul(36).ok_or(Error::LengthOverflow)?)?;
        Ok(Tag::Compound(Compound(entries)))
    }
}

fn modified_utf8_count(bytes: &[u8]) -> Result<usize, Error> {
    let mut offset = 0;
    let mut count = 0;
    while offset < bytes.len() {
        modified_utf8_unit(bytes, &mut offset)?;
        count += 1;
    }
    Ok(count)
}

fn modified_utf8_unit(bytes: &[u8], offset: &mut usize) -> Result<u16, Error> {
    let first = bytes[*offset];
    *offset += 1;
    if first < 0x80 {
        return Ok(u16::from(first));
    }
    let width = match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => return Err(Error::InvalidModifiedUtf8),
    };
    let mut unit = u16::from(first & if width == 2 { 0x1f } else { 0x0f });
    for _ in 1..width {
        let next = *bytes.get(*offset).ok_or(Error::InvalidModifiedUtf8)?;
        if next & 0xc0 != 0x80 {
            return Err(Error::InvalidModifiedUtf8);
        }
        *offset += 1;
        unit = (unit << 6) | u16::from(next & 0x3f);
    }
    Ok(unit)
}
