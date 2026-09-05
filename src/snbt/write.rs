use std::fmt::{self, Write as _};

use crate::nbt::{Compound, CompoundEntry, NbtString, Tag};

use super::{Error, ErrorKind, Limits};

/// Appends compact Vanilla SNBT as UTF-16. On error the original units remain
/// intact; the vector may retain capacity acquired during the attempt.
pub fn write(tag: &Tag, output: &mut Vec<u16>, limits: Limits) -> Result<(), Error> {
    limits.validate()?;
    let start = output.len();
    let mut writer = Writer {
        output,
        start,
        limits,
    };
    let result = writer.tag(tag, 0);
    if result.is_err() {
        writer.output.truncate(start);
    }
    result
}

/// Appends Vanilla's structure-oriented SNBT presentation with four-space
/// indentation. Its path-specific ordering is distinct from compact SNBT.
/// Errors preserve the existing output; End appends no units in this format.
pub fn write_pretty(tag: &Tag, output: &mut Vec<u16>, limits: Limits) -> Result<(), Error> {
    limits.validate()?;
    let start = output.len();
    let mut writer = Writer {
        output,
        start,
        limits,
    };
    let result = writer.pretty_tag(tag, 0, PrettyPath::new(), false);
    if result.is_err() {
        writer.output.truncate(start);
    }
    result
}

struct Writer<'a> {
    output: &'a mut Vec<u16>,
    start: usize,
    limits: Limits,
}

impl Writer<'_> {
    fn error(&self, kind: ErrorKind) -> Error {
        Error {
            offset_utf16: self.output.len() - self.start,
            kind,
            diagnostic: None,
        }
    }

    fn reserve(&mut self, count: usize) -> Result<(), Error> {
        let length = (self.output.len() - self.start)
            .checked_add(count)
            .ok_or_else(|| self.error(ErrorKind::OutputLimit))?;
        if length > self.limits.output_units {
            return Err(self.error(ErrorKind::OutputLimit));
        }
        self.output
            .try_reserve(count)
            .map_err(|_| self.error(ErrorKind::AllocationFailed))
    }

    fn unit(&mut self, unit: u16) -> Result<(), Error> {
        self.reserve(1)?;
        self.output.push(unit);
        Ok(())
    }

    fn ascii(&mut self, text: &str) -> Result<(), Error> {
        debug_assert!(text.is_ascii());
        self.reserve(text.len())?;
        self.output.extend(text.bytes().map(u16::from));
        Ok(())
    }

    fn integer(&mut self, value: i64) -> Result<(), Error> {
        let mut text = NumberText::new();
        write!(text, "{value}").expect("i64 fits numeric scratch");
        self.ascii(text.as_str())
    }

    fn quoted(&mut self, value: &NbtString) -> Result<(), Error> {
        let units = value.as_utf16();
        let quote = match units.iter().find(|&&unit| unit == 0x22 || unit == 0x27) {
            Some(0x22) => 0x27,
            _ => 0x22,
        };
        self.unit(quote)?;
        for &unit in units {
            match unit {
                8 => self.ascii("\\b")?,
                9 => self.ascii("\\t")?,
                10 => self.ascii("\\n")?,
                12 => self.ascii("\\f")?,
                13 => self.ascii("\\r")?,
                0..=31 => {
                    const HEX: &[u8] = b"0123456789ABCDEF";
                    self.ascii("\\x")?;
                    self.unit(u16::from(HEX[(unit >> 4) as usize]))?;
                    self.unit(u16::from(HEX[(unit & 15) as usize]))?;
                }
                _ => {
                    if unit == quote || unit == 0x5c {
                        self.unit(0x5c)?;
                    }
                    self.unit(unit)?;
                }
            }
        }
        self.unit(quote)
    }

    fn key(&mut self, name: &NbtString) -> Result<(), Error> {
        let units = name.as_utf16();
        let word = |unit: u16| matches!(unit, 65..=90 | 97..=122 | 46 | 95);
        let boolean = |word: &[u8]| {
            units.len() == word.len()
                && units.iter().zip(word).all(|(&unit, &byte)| {
                    unit == u16::from(byte) || unit == u16::from(byte.to_ascii_uppercase())
                })
        };
        let unquoted = units.first().is_some_and(|&unit| word(unit))
            && units[1..]
                .iter()
                .all(|&unit| word(unit) || matches!(unit, 48..=57 | 43 | 45))
            && !boolean(b"true")
            && !boolean(b"false");
        if unquoted {
            self.reserve(units.len())?;
            self.output.extend_from_slice(units);
            Ok(())
        } else {
            self.quoted(name)
        }
    }

    fn tag(&mut self, tag: &Tag, depth: usize) -> Result<(), Error> {
        if matches!(tag, Tag::List(_) | Tag::Compound(_)) && depth >= self.limits.max_depth {
            return Err(self.error(ErrorKind::DepthLimit));
        }
        match tag {
            Tag::End => self.ascii("END"),
            Tag::Byte(value) => {
                self.integer(i64::from(*value))?;
                self.ascii("b")
            }
            Tag::Short(value) => {
                self.integer(i64::from(*value))?;
                self.ascii("s")
            }
            Tag::Int(value) => self.integer(i64::from(*value)),
            Tag::Long(value) => {
                self.integer(*value)?;
                self.ascii("L")
            }
            Tag::Float(value) => {
                self.number(Number::Float(*value))?;
                self.ascii("f")
            }
            Tag::Double(value) => {
                self.number(Number::Double(*value))?;
                self.ascii("d")
            }
            Tag::String(value) => self.quoted(value),
            Tag::ByteArray(values) => self.byte_array(values, false),
            Tag::IntArray(values) => self.int_array(values, false),
            Tag::LongArray(values) => self.long_array(values, false),
            Tag::List(values) => {
                self.ascii("[")?;
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        self.ascii(",")?;
                    }
                    self.tag(value, depth + 1)?;
                }
                self.ascii("]")
            }
            Tag::Compound(value) => {
                self.ascii("{")?;
                for (index, entry) in value.entries().iter().enumerate() {
                    if index != 0 {
                        self.ascii(",")?;
                    }
                    self.key(&entry.name)?;
                    self.ascii(":")?;
                    self.tag(&entry.value, depth + 1)?;
                }
                self.ascii("}")
            }
        }
    }

    fn array_spacing(&mut self, index: usize, pretty: bool) -> Result<(), Error> {
        if index != 0 {
            self.ascii(",")?;
        }
        if pretty {
            self.ascii(" ")?;
        }
        Ok(())
    }

    fn byte_array(&mut self, values: &[i8], pretty: bool) -> Result<(), Error> {
        self.ascii("[B;")?;
        for (index, &value) in values.iter().enumerate() {
            self.array_spacing(index, pretty)?;
            self.integer(i64::from(value))?;
            self.ascii("B")?;
        }
        self.ascii("]")
    }

    fn int_array(&mut self, values: &[i32], pretty: bool) -> Result<(), Error> {
        self.ascii("[I;")?;
        for (index, &value) in values.iter().enumerate() {
            self.array_spacing(index, pretty)?;
            self.integer(i64::from(value))?;
        }
        self.ascii("]")
    }

    fn long_array(&mut self, values: &[i64], pretty: bool) -> Result<(), Error> {
        self.ascii("[L;")?;
        for (index, &value) in values.iter().enumerate() {
            self.array_spacing(index, pretty)?;
            self.integer(value)?;
            self.ascii("L")?;
        }
        self.ascii("]")
    }

    fn indentation(&mut self, depth: usize) -> Result<(), Error> {
        let units = depth * 4;
        self.reserve(units)?;
        self.output.extend(std::iter::repeat_n(0x20, units));
        Ok(())
    }

    fn pretty_tag(
        &mut self,
        tag: &Tag,
        depth: usize,
        path: PrettyPath,
        inline: bool,
    ) -> Result<(), Error> {
        if matches!(tag, Tag::List(_) | Tag::Compound(_)) && depth >= self.limits.max_depth {
            return Err(self.error(ErrorKind::DepthLimit));
        }
        match tag {
            Tag::End => Ok(()),
            Tag::ByteArray(values) => self.byte_array(values, true),
            Tag::IntArray(values) => self.int_array(values, true),
            Tag::LongArray(values) => self.long_array(values, true),
            Tag::List(values) => {
                if values.is_empty() {
                    return self.ascii("[]");
                }
                let path = path.with_ascii("[]");
                let inline = inline || path.suppresses_indentation();
                self.ascii("[")?;
                for (index, value) in values.iter().enumerate() {
                    self.pretty_element_start(index, depth, inline)?;
                    self.pretty_tag(value, depth + 1, path, inline)?;
                }
                self.pretty_container_end("]", depth, inline)
            }
            Tag::Compound(value) => {
                self.pretty_compound(value, depth, path.with_ascii("{}"), inline)
            }
            _ => self.tag(tag, depth),
        }
    }

    fn pretty_element_start(
        &mut self,
        index: usize,
        depth: usize,
        inline: bool,
    ) -> Result<(), Error> {
        if index != 0 {
            self.ascii(",")?;
        }
        if inline {
            if index != 0 {
                self.ascii(" ")?;
            }
        } else {
            self.ascii("\n")?;
            self.indentation(depth + 1)?;
        }
        Ok(())
    }

    fn pretty_container_end(
        &mut self,
        delimiter: &str,
        depth: usize,
        inline: bool,
    ) -> Result<(), Error> {
        if !inline {
            self.ascii("\n")?;
            self.indentation(depth)?;
        }
        self.ascii(delimiter)
    }

    fn pretty_compound(
        &mut self,
        compound: &Compound,
        depth: usize,
        path: PrettyPath,
        inline: bool,
    ) -> Result<(), Error> {
        let entries = compound.entries();
        if entries.is_empty() {
            return self.ascii("{}");
        }
        let inline = inline || path.suppresses_indentation();
        let priority = path.key_priority();
        self.ascii("{")?;
        let mut index = 0;
        // Read the existing sorted storage directly: no cloned map, sorted
        // temporary key collection or per-container heap allocation is needed.
        for &name in priority {
            if let Ok(position) = entries.binary_search_by(|entry| {
                entry
                    .name
                    .as_utf16()
                    .iter()
                    .copied()
                    .cmp(name.bytes().map(u16::from))
            }) {
                self.pretty_element_start(index, depth, inline)?;
                self.pretty_entry(&entries[position], depth, path, inline)?;
                index += 1;
            }
        }
        for entry in entries {
            if priority.iter().any(|name| {
                entry
                    .name
                    .as_utf16()
                    .iter()
                    .copied()
                    .eq(name.bytes().map(u16::from))
            }) {
                continue;
            }
            self.pretty_element_start(index, depth, inline)?;
            self.pretty_entry(entry, depth, path, inline)?;
            index += 1;
        }
        self.pretty_container_end("}", depth, inline)
    }

    fn pretty_entry(
        &mut self,
        entry: &CompoundEntry,
        depth: usize,
        path: PrettyPath,
        inline: bool,
    ) -> Result<(), Error> {
        let key = entry.name.as_utf16();
        if !key.is_empty()
            && key
                .iter()
                .all(|&unit| matches!(unit, 65..=90 | 97..=122 | 48..=57 | 46 | 95 | 43 | 45))
        {
            self.reserve(key.len())?;
            self.output.extend_from_slice(key);
        } else {
            self.quoted(&entry.name)?;
        }
        self.ascii(": ")?;
        self.pretty_tag(&entry.value, depth + 1, path.with_key(key), inline)
    }

    fn number(&mut self, number: Number) -> Result<(), Error> {
        let value = number.as_double();
        if value.is_nan() {
            return self.ascii("NaN");
        }
        if value.is_sign_negative() {
            self.ascii("-")?;
        }
        if value.is_infinite() {
            return self.ascii("Infinity");
        }
        if value == 0.0 {
            return self.ascii("0.0");
        }
        let decimal = number.abs().decimal();
        let mut text = NumberText::new();
        write!(text, "{}", decimal.significand).expect("significand fits numeric scratch");
        let digits = text.as_str();
        let exponent = decimal.exponent + digits.len() as i32 - 1;
        if !(-3..7).contains(&exponent) {
            self.ascii(&digits[..1])?;
            self.ascii(".")?;
            self.ascii(if digits.len() == 1 { "0" } else { &digits[1..] })?;
            self.ascii("E")?;
            self.integer(i64::from(exponent))
        } else if exponent < 0 {
            self.ascii("0.")?;
            for _ in 0..-exponent - 1 {
                self.ascii("0")?;
            }
            self.ascii(digits)
        } else {
            let before_point = exponent as usize + 1;
            if before_point >= digits.len() {
                self.ascii(digits)?;
                for _ in digits.len()..before_point {
                    self.ascii("0")?;
                }
                self.ascii(".0")
            } else {
                self.ascii(&digits[..before_point])?;
                self.ascii(".")?;
                self.ascii(&digits[before_point..])
            }
        }
    }
}

// Only a few literal, ASCII paths affect Vanilla's presentation. Retaining a
// bounded prefix is enough: once a path is longer or non-ASCII, appending to it
// cannot equal any special path. Periods inside keys are deliberately kept, so
// keys such as "data.[]" have the same joined-path behavior as the Java printer.
#[derive(Clone, Copy)]
struct PrettyPath {
    bytes: [u8; 20],
    length: usize,
}

impl PrettyPath {
    fn new() -> Self {
        Self {
            bytes: [0; 20],
            length: 0,
        }
    }

    fn append(&mut self, unit: u16) {
        if self.length < self.bytes.len() && unit <= 0x7f {
            self.bytes[self.length] = unit as u8;
            self.length += 1;
        } else {
            self.length = self.bytes.len() + 1;
        }
    }

    fn with_ascii(mut self, component: &str) -> Self {
        if self.length != 0 {
            self.append(u16::from(b'.'));
        }
        for byte in component.bytes() {
            self.append(u16::from(byte));
        }
        self
    }

    fn with_key(mut self, key: &[u16]) -> Self {
        // Once no special path can fit, avoid scanning a potentially large key
        // merely to keep an already irrelevant presentation path.
        let separator = usize::from(self.length != 0);
        if self.length > self.bytes.len()
            || key.len() > self.bytes.len().saturating_sub(self.length + separator)
        {
            self.length = self.bytes.len() + 1;
            return self;
        }
        if self.length != 0 {
            self.append(u16::from(b'.'));
        }
        for &unit in key {
            self.append(unit);
        }
        self
    }

    fn text(&self) -> &[u8] {
        self.bytes.get(..self.length).unwrap_or(&[])
    }

    fn suppresses_indentation(&self) -> bool {
        matches!(
            self.text(),
            b"{}.size.[]" | b"{}.data.[].{}" | b"{}.palette.[].{}" | b"{}.entities.[].{}"
        )
    }

    fn key_priority(&self) -> &'static [&'static str] {
        match self.text() {
            b"{}" => &[
                "DataVersion",
                "author",
                "size",
                "data",
                "entities",
                "palette",
                "palettes",
            ],
            b"{}.data.[].{}" => &["pos", "state", "nbt"],
            b"{}.entities.[].{}" => &["blockPos", "pos"],
            _ => &[],
        }
    }
}

// All numeric scratch is bounded and stays on the stack. Decimal formatting
// uses Rust's correctly rounded scientific formatting, then validates the
// decimal candidates against the original binary type. This is intentionally
// a small baseline, rather than a second bespoke floating-point conversion
// library. Java's public toString specification defines the selection rule.
#[derive(Clone, Copy)]
enum Number {
    Float(f32),
    Double(f64),
}

impl Number {
    fn as_double(self) -> f64 {
        match self {
            Self::Float(value) => f64::from(value),
            Self::Double(value) => value,
        }
    }

    fn abs(self) -> Self {
        match self {
            Self::Float(value) => Self::Float(value.abs()),
            Self::Double(value) => Self::Double(value.abs()),
        }
    }

    fn matches(self, text: &str) -> bool {
        match self {
            Self::Float(value) => text
                .parse::<f32>()
                .is_ok_and(|parsed| parsed.to_bits() == value.to_bits()),
            Self::Double(value) => text
                .parse::<f64>()
                .is_ok_and(|parsed| parsed.to_bits() == value.to_bits()),
        }
    }

    fn decimal(self) -> Decimal {
        let maximum = match self {
            Self::Float(_) => 9,
            Self::Double(_) => 17,
        };
        // Java considers one- and two-digit decimals together. In particular,
        // the smallest subnormals require 1.4E-45 and 4.9E-324, respectively.
        for digits in 2..=maximum {
            let precision = digits - 1;
            let mut text = NumberText::new();
            match self {
                Self::Float(value) => write!(text, "{value:.precision$e}"),
                Self::Double(value) => write!(text, "{value:.precision$e}"),
            }
            .expect("finite scientific number fits numeric scratch");
            let decimal = Decimal::from_scientific(text.as_str());
            if self.matches(text.as_str()) {
                return decimal.normalized();
            }
            // Around binary powers the rounding interval is asymmetric. The
            // closest decimal can miss on its narrow side while its neighbor
            // still rounds to the original value. At most one neighbor can
            // qualify if the closest decimal itself did not.
            let lower = if decimal.significand == 10_u64.pow(precision as u32) {
                // Crossing a decimal decade changes the spacing: the predecessor
                // of 1.0E3 at two digits is 9.9E2, not 9.0E2.
                Decimal {
                    significand: decimal.significand * 10 - 1,
                    exponent: decimal.exponent - 1,
                }
            } else {
                Decimal {
                    significand: decimal.significand - 1,
                    exponent: decimal.exponent,
                }
            };
            let upper = Decimal {
                significand: decimal.significand + 1,
                exponent: decimal.exponent,
            };
            for candidate in [lower, upper] {
                let mut adjacent = NumberText::new();
                write!(adjacent, "{}e{}", candidate.significand, candidate.exponent)
                    .expect("adjacent decimal fits numeric scratch");
                if self.matches(adjacent.as_str()) {
                    return candidate.normalized();
                }
            }
        }
        unreachable!("9/17 significant digits always round trip binary32/64")
    }
}

struct Decimal {
    significand: u64,
    exponent: i32,
}

impl Decimal {
    fn from_scientific(text: &str) -> Self {
        let (mantissa, exponent) = text
            .split_once('e')
            .expect("scientific format has exponent");
        let mut significand = 0;
        let mut digits = 0;
        for byte in mantissa.bytes().filter(|&byte| byte != b'.') {
            significand = significand * 10 + u64::from(byte - b'0');
            digits += 1;
        }
        Self {
            significand,
            exponent: exponent
                .parse::<i32>()
                .expect("formatted exponent is integer")
                - digits
                + 1,
        }
    }

    fn normalized(mut self) -> Self {
        while self.significand.is_multiple_of(10) {
            self.significand /= 10;
            self.exponent += 1;
        }
        self
    }
}

struct NumberText {
    bytes: [u8; 48],
    length: usize,
}

impl NumberText {
    fn new() -> Self {
        Self {
            bytes: [0; 48],
            length: 0,
        }
    }
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.length]).expect("numeric output is ASCII")
    }
}

impl fmt::Write for NumberText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.length.checked_add(value.len()).ok_or(fmt::Error)?;
        self.bytes
            .get_mut(self.length..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(value.as_bytes());
        self.length = end;
        Ok(())
    }
}
