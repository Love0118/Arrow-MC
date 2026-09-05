use super::{Diagnostic, DiagnosticArgument, Error, ErrorKind, Limits};
use crate::nbt::{Compound, CompoundEntry, NbtString, Tag};
use std::mem::size_of;
use std::ops::Range;

pub fn parse(input: &str, limits: Limits) -> Result<Tag, Error> {
    limits.validate()?;
    let count = input.encode_utf16().count();
    check_input(count, limits)?;
    let bytes = count
        .checked_mul(2)
        .ok_or(error(0, ErrorKind::AllocationBudget))?;
    if bytes > limits.allocation_bytes {
        return Err(error(0, ErrorKind::AllocationBudget));
    }
    let mut units = Vec::new();
    units
        .try_reserve_exact(count)
        .map_err(|_| error(0, ErrorKind::AllocationFailed))?;
    units.extend(input.encode_utf16());
    let mut parser = Parser::new(&units, limits, bytes);
    let result = parser.full();
    result.map_err(|failure| parser.finish_error(failure))
}

pub fn parse_utf16(input: &[u16], limits: Limits) -> Result<Tag, Error> {
    limits.validate()?;
    check_input(input.len(), limits)?;
    let mut parser = Parser::new(input, limits, 0);
    let result = parser.full();
    result.map_err(|failure| parser.finish_error(failure))
}

/// Parses one argument and returns its consumed UTF-16 offset. Trailing
/// whitespace is not consumed unless it belongs to a matched grammar token.
pub fn parse_prefix(input: &[u16], limits: Limits) -> Result<(Tag, usize), Error> {
    parse_prefix_accounted(input, limits).map(|(tag, consumed, _)| (tag, consumed))
}

pub(crate) fn parse_prefix_accounted(
    input: &[u16],
    limits: Limits,
) -> Result<(Tag, usize, usize), Error> {
    limits.validate()?;
    check_input(input.len(), limits)?;
    let mut parser = Parser::new(input, limits, 0);
    let value = parser
        .value(0)
        .map_err(|failure| parser.finish_error(failure))?;
    Ok((value, parser.pos, parser.allocation))
}

pub fn parse_compound(input: &str, limits: Limits) -> Result<Compound, Error> {
    match parse(input, limits)? {
        Tag::Compound(value) => Ok(value),
        _ => Err(error(
            input.encode_utf16().count(),
            ErrorKind::ExpectedCompound,
        )),
    }
}

pub fn parse_compound_utf16(input: &[u16], limits: Limits) -> Result<Compound, Error> {
    match parse_utf16(input, limits)? {
        Tag::Compound(value) => Ok(value),
        _ => Err(error(input.len(), ErrorKind::ExpectedCompound)),
    }
}

fn check_input(count: usize, limits: Limits) -> Result<(), Error> {
    if count > limits.input_units {
        Err(error(0, ErrorKind::InputLimit))
    } else {
        Ok(())
    }
}

fn error(offset_utf16: usize, kind: ErrorKind) -> Error {
    Error {
        offset_utf16,
        kind,
        diagnostic: default_diagnostic(kind),
    }
}

fn default_diagnostic(kind: ErrorKind) -> Option<Diagnostic> {
    let key = match kind {
        ErrorKind::Syntax => "snbt.parser.expected_unquoted_string",
        ErrorKind::NumberRange => "snbt.parser.number_parse_failure",
        ErrorKind::InvalidEscape => "snbt.parser.expected_hex_escape",
        ErrorKind::InvalidCodePoint => "snbt.parser.invalid_codepoint",
        ErrorKind::InvalidCharacterName => "snbt.parser.invalid_character_name",
        ErrorKind::ArrayElementType => "snbt.parser.invalid_array_element_type",
        ErrorKind::UnknownOperation => "snbt.parser.no_such_operation",
        ErrorKind::ExpectedNumber => "snbt.parser.expected_number_or_boolean",
        ErrorKind::InvalidUuid => "snbt.parser.expected_string_uuid",
        ErrorKind::TrailingData => "argument.nbt.trailing",
        ErrorKind::EmptyKey => "snbt.parser.empty_key",
        ErrorKind::ExpectedCompound => "argument.nbt.expected.compound",
        _ => return None,
    };
    Some(Diagnostic {
        key,
        argument: DiagnosticArgument::None,
    })
}

#[derive(Clone, Copy)]
struct Failure {
    offset_utf16: usize,
    kind: ErrorKind,
}

fn failure(offset_utf16: usize, kind: ErrorKind) -> Failure {
    Failure { offset_utf16, kind }
}

struct Parser<'a> {
    input: &'a [u16],
    pos: usize,
    limits: Limits,
    allocation: usize,
    furthest: usize,
    diagnostic: Option<Diagnostic>,
}

struct Integer {
    digits: Range<usize>,
    radix: u8,
    negative: bool,
    unsigned: bool,
    width: Option<u8>,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u16], limits: Limits, allocation: usize) -> Self {
        Self {
            input,
            pos: 0,
            limits,
            allocation,
            furthest: 0,
            diagnostic: None,
        }
    }

    fn full(&mut self) -> Result<Tag, Failure> {
        let result = self.value(0)?;
        self.space();
        if self.pos != self.input.len() {
            Err(failure(self.pos, ErrorKind::TrailingData))
        } else {
            Ok(result)
        }
    }

    fn err(&self, kind: ErrorKind) -> Failure {
        failure(self.pos.max(self.furthest), kind)
    }

    fn finish_error(&self, failure: Failure) -> Error {
        let mut result = error(failure.offset_utf16, failure.kind);
        if result.diagnostic.is_some()
            && failure.kind != ErrorKind::TrailingData
            && self.furthest >= failure.offset_utf16
        {
            result.diagnostic = self.diagnostic.or(result.diagnostic);
        }
        result
    }

    fn record(&mut self, offset: usize, key: &'static str, argument: DiagnosticArgument) {
        if offset > self.furthest {
            self.furthest = offset;
            self.diagnostic = Some(Diagnostic { key, argument });
        } else if offset == self.furthest && self.diagnostic.is_none() {
            self.diagnostic = Some(Diagnostic { key, argument });
        }
    }

    fn record_error(&mut self, failure: Failure) {
        if let Some(diagnostic) = default_diagnostic(failure.kind) {
            self.record(failure.offset_utf16, diagnostic.key, diagnostic.argument);
        }
    }

    fn terminal_failure(&mut self, first: u16, second: Option<u16>) {
        self.record(
            self.pos,
            "argument.literal.incorrect",
            DiagnosticArgument::Literal { first, second },
        );
    }

    fn semantic_error(
        &mut self,
        kind: ErrorKind,
        key: &'static str,
        argument: DiagnosticArgument,
    ) -> Failure {
        self.record(self.pos, key, argument);
        self.err(kind)
    }

    fn allocation(&mut self, bytes: usize) -> Result<(), Failure> {
        self.allocation = self
            .allocation
            .checked_add(bytes)
            .ok_or(self.err(ErrorKind::AllocationBudget))?;
        if self.allocation > self.limits.allocation_bytes {
            Err(self.err(ErrorKind::AllocationBudget))
        } else {
            Ok(())
        }
    }

    // One private reservation helper keeps every concrete decoded Vec under
    // the same pre-allocation policy; no public serialization abstraction.
    fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), Failure> {
        let needed = values
            .len()
            .checked_add(additional)
            .ok_or(self.err(ErrorKind::AllocationBudget))?;
        if needed <= values.capacity() {
            return Ok(());
        }
        let capacity = needed.max(values.capacity().saturating_mul(2)).max(4);
        self.allocation(
            capacity
                .checked_mul(size_of::<T>())
                .ok_or(self.err(ErrorKind::AllocationBudget))?,
        )?;
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| self.err(ErrorKind::AllocationFailed))
    }

    fn push_unit(&mut self, values: &mut Vec<u16>, unit: u16) -> Result<(), Failure> {
        self.reserve(values, 1)?;
        values.push(unit);
        Ok(())
    }

    fn copy_string(&mut self, range: Range<usize>) -> Result<NbtString, Failure> {
        let length = range.len();
        self.allocation(
            length
                .checked_mul(2)
                .ok_or(self.err(ErrorKind::AllocationBudget))?,
        )?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(length)
            .map_err(|_| self.err(ErrorKind::AllocationFailed))?;
        result.extend_from_slice(&self.input[range]);
        Ok(NbtString::from_utf16(result))
    }

    fn space(&mut self) {
        while self
            .input
            .get(self.pos)
            .is_some_and(|&unit| java_whitespace(unit))
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u16> {
        self.input.get(self.pos).copied()
    }

    fn eat(&mut self, unit: u16) -> bool {
        let original = self.pos;
        self.space();
        if self.peek() == Some(unit) {
            self.pos += 1;
            true
        } else {
            self.terminal_failure(unit, None);
            self.pos = original;
            false
        }
    }

    fn closing(&mut self, unit: u16) -> bool {
        let original = self.pos;
        self.space();
        if self.peek() == Some(unit) {
            self.pos += 1;
            true
        } else {
            self.pos = original;
            false
        }
    }

    fn eat_pair(&mut self, first: u16, second: u16) -> bool {
        let original = self.pos;
        self.space();
        if self
            .peek()
            .is_some_and(|unit| unit == first || unit == second)
        {
            self.pos += 1;
            true
        } else {
            self.terminal_failure(first, Some(second));
            self.pos = original;
            false
        }
    }

    fn expect(&mut self, unit: u16) -> Result<(), Failure> {
        self.space();
        if self.peek() == Some(unit) {
            self.pos += 1;
            Ok(())
        } else {
            self.terminal_failure(unit, None);
            Err(self.err(ErrorKind::Syntax))
        }
    }

    fn child(&self, depth: usize) -> Result<usize, Failure> {
        if depth >= self.limits.max_depth {
            Err(self.err(ErrorKind::DepthLimit))
        } else {
            Ok(depth + 1)
        }
    }

    fn value(&mut self, depth: usize) -> Result<Tag, Failure> {
        self.space();
        match self.peek() {
            Some(34 | 39) => Ok(Tag::String(self.quoted()?)),
            Some(123) => self.compound(depth),
            Some(91) => self.list(depth),
            Some(unit) if number_start(unit) => self.number(depth),
            _ => self.word_value(depth),
        }
    }

    fn word(&mut self) -> Result<Range<usize>, Failure> {
        self.space();
        let start = self.pos;
        while self.peek().is_some_and(unquoted) {
            self.pos += 1;
        }
        if start == self.pos {
            self.record(
                self.pos,
                "snbt.parser.expected_unquoted_string",
                DiagnosticArgument::None,
            );
            Err(self.err(ErrorKind::Syntax))
        } else {
            Ok(start..self.pos)
        }
    }

    fn word_value(&mut self, depth: usize) -> Result<Tag, Failure> {
        let name = self.word()?;
        let invalid_start = number_start(self.input[name.start]);
        let word_end = self.pos;
        if self.eat(40) {
            let child = self.child(depth)?;
            let mut arguments = Vec::new();
            let suffix: Result<(), Failure> = (|| {
                if !self.closing(41) {
                    loop {
                        let value = self.value(child)?;
                        self.reserve(&mut arguments, 1)?;
                        arguments.push(value);
                        if self.closing(41) {
                            break;
                        }
                        if !self.eat(44) {
                            self.expect(41)?;
                            break;
                        }
                        if self.closing(41) {
                            break;
                        }
                    }
                }
                Ok(())
            })();
            if let Err(failure) = suffix {
                if resource_error(failure.kind) {
                    return Err(failure);
                }
                // The pinned argument parser retains successfully parsed
                // arguments when its optional parenthesized suffix rewinds.
                // Example from the JVM: bool(1 -> Byte(1), consumed offset 4.
                self.record_error(failure);
                self.pos = word_end;
            }
            if invalid_start {
                return Err(self.semantic_error(
                    ErrorKind::Syntax,
                    "snbt.parser.invalid_unquoted_start",
                    DiagnosticArgument::None,
                ));
            }
            if arguments.len() != 1 {
                return Err(self.semantic_error(
                    ErrorKind::UnknownOperation,
                    "snbt.parser.no_such_operation",
                    DiagnosticArgument::Operation {
                        name_start: name.start,
                        name_end: name.end,
                        arity: arguments.len(),
                    },
                ));
            }
            let argument = arguments.pop().unwrap();
            if ascii_eq(&self.input[name.clone()], b"bool", false) {
                let truth = match argument {
                    Tag::Byte(value) => value != 0,
                    Tag::Short(value) => value != 0,
                    Tag::Int(value) => value != 0,
                    Tag::Long(value) => value != 0,
                    Tag::Float(value) => value != 0.0,
                    Tag::Double(value) => value != 0.0,
                    _ => return Err(self.err(ErrorKind::ExpectedNumber)),
                };
                return Ok(Tag::Byte(i8::from(truth)));
            }
            if ascii_eq(&self.input[name.clone()], b"uuid", false) {
                let Tag::String(value) = argument else {
                    return Err(self.err(ErrorKind::InvalidUuid));
                };
                let words = uuid(value.as_utf16()).ok_or(self.err(ErrorKind::InvalidUuid))?;
                self.allocation(4 * size_of::<i32>())?;
                let mut result = Vec::new();
                result
                    .try_reserve_exact(4)
                    .map_err(|_| self.err(ErrorKind::AllocationFailed))?;
                result.extend_from_slice(&words);
                return Ok(Tag::IntArray(result));
            }
            return Err(self.semantic_error(
                ErrorKind::UnknownOperation,
                "snbt.parser.no_such_operation",
                DiagnosticArgument::Operation {
                    name_start: name.start,
                    name_end: name.end,
                    arity: 1,
                },
            ));
        }
        self.pos = word_end;
        if invalid_start {
            return Err(self.semantic_error(
                ErrorKind::Syntax,
                "snbt.parser.invalid_unquoted_start",
                DiagnosticArgument::None,
            ));
        }
        if ascii_eq(&self.input[name.clone()], b"true", true) {
            Ok(Tag::Byte(1))
        } else if ascii_eq(&self.input[name.clone()], b"false", true) {
            Ok(Tag::Byte(0))
        } else {
            Ok(Tag::String(self.copy_string(name)?))
        }
    }

    fn quoted(&mut self) -> Result<NbtString, Failure> {
        let quote = self.peek().ok_or(self.err(ErrorKind::Syntax))?;
        self.pos += 1;
        let mut result = Vec::new();
        loop {
            let Some(unit) = self.peek() else {
                return Err(self.semantic_error(
                    ErrorKind::Syntax,
                    "snbt.parser.invalid_string_contents",
                    DiagnosticArgument::None,
                ));
            };
            self.pos += 1;
            if unit == quote {
                return Ok(NbtString::from_utf16(result));
            }
            if unit != 92 {
                self.push_unit(&mut result, unit)?;
                continue;
            }
            // Escape introducers are terminal tokens in the pinned grammar:
            // whitespace after the slash is skipped, after x/u/U it is not.
            self.space();
            let Some(escape) = self.peek() else {
                return Err(self.semantic_error(
                    ErrorKind::InvalidEscape,
                    "argument.literal.incorrect",
                    DiagnosticArgument::Literal {
                        first: 98,
                        second: None,
                    },
                ));
            };
            self.pos += 1;
            let code_point = match escape {
                98 => 8,
                115 => 32,
                116 => 9,
                110 => 10,
                102 => 12,
                114 => 13,
                92 | 39 | 34 => u32::from(escape),
                120 | 117 | 85 => {
                    let count = match escape {
                        120 => 2,
                        117 => 4,
                        _ => 8,
                    };
                    let start = self.pos;
                    let mut code_point = 0u32;
                    for _ in 0..count {
                        let Some(digit) = self.peek().and_then(hex_digit) else {
                            self.record(
                                start,
                                "snbt.parser.expected_hex_escape",
                                DiagnosticArgument::HexWidth(count as u8),
                            );
                            return Err(failure(self.furthest, ErrorKind::InvalidEscape));
                        };
                        code_point = (code_point << 4) | u32::from(digit);
                        self.pos += 1;
                    }
                    if code_point > 0x10ffff {
                        return Err(self.semantic_error(
                            ErrorKind::InvalidCodePoint,
                            "snbt.parser.invalid_codepoint",
                            DiagnosticArgument::CodePoint(code_point),
                        ));
                    }
                    code_point
                }
                78 => {
                    self.expect(123)?;
                    let start = self.pos;
                    while self.peek().is_some_and(name_character) {
                        self.pos += 1;
                    }
                    if start == self.pos {
                        return Err(self.err(ErrorKind::InvalidCharacterName));
                    }
                    let end = self.pos;
                    self.expect(125)?;
                    crate::unicode_names::lookup_utf16(&self.input[start..end])
                        .ok_or(self.err(ErrorKind::InvalidCharacterName))?
                }
                _ => {
                    self.pos -= 1;
                    return Err(self.semantic_error(
                        ErrorKind::InvalidEscape,
                        "argument.literal.incorrect",
                        DiagnosticArgument::Literal {
                            first: 98,
                            second: None,
                        },
                    ));
                }
            };
            if code_point <= 0xffff {
                self.push_unit(&mut result, code_point as u16)?;
            } else {
                self.push_unit(
                    &mut result,
                    0xd800 | (((code_point - 0x10000) >> 10) as u16),
                )?;
                self.push_unit(
                    &mut result,
                    0xdc00 | (((code_point - 0x10000) & 0x3ff) as u16),
                )?;
            }
        }
    }

    fn compound(&mut self, depth: usize) -> Result<Tag, Failure> {
        let child = self.child(depth)?;
        self.pos += 1;
        let mut entries = Vec::new();
        if self.closing(125) {
            return Ok(Tag::Compound(Compound::new()));
        }
        loop {
            self.space();
            let name = if matches!(self.peek(), Some(34 | 39)) {
                self.quoted()?
            } else {
                self.terminal_failure(34, None);
                let range = self.word()?;
                self.copy_string(range)?
            };
            self.expect(58)?;
            let value = self.value(child)?;
            if name.is_empty() {
                return Err(self.err(ErrorKind::EmptyKey));
            }
            self.reserve(&mut entries, 1)?;
            entries.push(CompoundEntry::new(name, value));
            if self.closing(125) {
                break;
            }
            self.expect(44)?;
            if self.closing(125) {
                break;
            }
        }
        Ok(Tag::Compound(
            Compound::from_entries(entries).map_err(|_| self.err(ErrorKind::Syntax))?,
        ))
    }

    fn list(&mut self, depth: usize) -> Result<Tag, Failure> {
        let child = self.child(depth)?;
        self.pos += 1;
        if self.closing(93) {
            return Ok(Tag::List(Vec::new()));
        }
        let start = self.pos;
        self.space();
        if self.peek() != Some(66) {
            self.terminal_failure(66, None);
        }
        let array_width = match self.peek() {
            Some(66) => Some(8),
            Some(73) => Some(32),
            Some(76) => Some(64),
            _ => None,
        };
        if let Some(width) = array_width {
            self.pos += 1;
            if self.eat(59) {
                return self.array(width);
            }
        }
        self.pos = start;
        let mut values = Vec::new();
        loop {
            let value = self.value(child)?;
            self.reserve(&mut values, 1)?;
            values.push(value);
            if self.closing(93) {
                break;
            }
            self.expect(44)?;
            if self.closing(93) {
                break;
            }
        }
        Ok(Tag::List(values))
    }

    fn array(&mut self, width: u8) -> Result<Tag, Failure> {
        let mut entries = Vec::new();
        if !self.closing(93) {
            loop {
                let entry = self.integer()?;
                self.reserve(&mut entries, 1)?;
                entries.push(entry);
                if self.closing(93) {
                    break;
                }
                self.expect(44)?;
                if self.closing(93) {
                    break;
                }
            }
        }
        // Array conversion follows the closing bracket. Preserve suffix width
        // before widening: 255ub in an IntArray becomes -1, not 255.
        let bytes = entries
            .len()
            .checked_mul(usize::from(width) / 8)
            .ok_or(self.err(ErrorKind::AllocationBudget))?;
        self.allocation(bytes)?;
        match width {
            8 => {
                let mut result = Vec::new();
                result
                    .try_reserve_exact(entries.len())
                    .map_err(|_| self.err(ErrorKind::AllocationFailed))?;
                for entry in entries {
                    result.push(self.array_integer(&entry, width)? as i8);
                }
                Ok(Tag::ByteArray(result))
            }
            32 => {
                let mut result = Vec::new();
                result
                    .try_reserve_exact(entries.len())
                    .map_err(|_| self.err(ErrorKind::AllocationFailed))?;
                for entry in entries {
                    result.push(self.array_integer(&entry, width)? as i32);
                }
                Ok(Tag::IntArray(result))
            }
            _ => {
                let mut result = Vec::new();
                result
                    .try_reserve_exact(entries.len())
                    .map_err(|_| self.err(ErrorKind::AllocationFailed))?;
                for entry in entries {
                    result.push(self.array_integer(&entry, width)?);
                }
                Ok(Tag::LongArray(result))
            }
        }
    }

    fn array_integer(&mut self, entry: &Integer, width: u8) -> Result<i64, Failure> {
        let actual_width = entry.width.unwrap_or(width);
        if actual_width > width {
            return Err(self.err(ErrorKind::ArrayElementType));
        }
        self.integer_value(entry, actual_width)
    }

    fn sign(&mut self) -> bool {
        if self.eat(43) { false } else { self.eat(45) }
    }

    fn digits(&mut self, radix: u8) -> Result<Range<usize>, Failure> {
        self.space();
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|unit| unit == 95 || hex_digit(unit).is_some_and(|digit| digit < radix))
        {
            self.pos += 1;
        }
        if self.pos == start || self.input[start] == 95 || self.input[self.pos - 1] == 95 {
            let key = if self.pos != start {
                "snbt.parser.underscore_not_allowed"
            } else {
                match radix {
                    2 => "snbt.parser.expected_binary_numeral",
                    16 => "snbt.parser.expected_hex_numeral",
                    _ => "snbt.parser.expected_decimal_numeral",
                }
            };
            self.record(start, key, DiagnosticArgument::None);
            let mut result = failure(start, ErrorKind::Syntax);
            result.offset_utf16 = self.furthest;
            return Err(result);
        }
        Ok(start..self.pos)
    }

    fn integer(&mut self) -> Result<Integer, Failure> {
        let negative = self.sign();
        self.space();
        let digits_start = self.pos;
        let (radix, digits) = if self.eat(48) {
            let after_zero = self.pos;
            if self.eat_pair(120, 88) {
                (16, self.digits(16)?)
            } else {
                let binary = if self.eat_pair(98, 66) {
                    self.digits(2).ok()
                } else {
                    None
                };
                if let Some(digits) = binary {
                    (2, digits)
                } else {
                    self.pos = after_zero;
                    if let Ok(extra) = self.digits(10) {
                        self.pos = extra.end;
                        return Err(self.semantic_error(
                            ErrorKind::Syntax,
                            "snbt.parser.leading_zero_not_allowed",
                            DiagnosticArgument::None,
                        ));
                    }
                    self.pos = after_zero;
                    (10, digits_start..after_zero)
                }
            }
        } else {
            (10, self.digits(10)?)
        };
        let suffix_start = self.pos;
        let mut unsigned = radix != 10;
        let signed_prefix = if self.eat_pair(117, 85) {
            Some(true)
        } else if self.eat_pair(115, 83) {
            Some(false)
        } else {
            None
        };
        let width = if let Some(prefix) = signed_prefix {
            if let Some(width) = self.type_suffix() {
                unsigned = prefix;
                Some(width)
            } else {
                self.pos = suffix_start;
                self.type_suffix()
            }
        } else {
            self.type_suffix()
        };
        Ok(Integer {
            digits,
            radix,
            negative,
            unsigned,
            width,
        })
    }

    fn type_suffix(&mut self) -> Option<u8> {
        for (lower, upper, width) in [(98, 66, 8), (115, 83, 16), (105, 73, 32), (108, 76, 64)] {
            if self.eat_pair(lower, upper) {
                return Some(width);
            }
        }
        None
    }

    fn integer_value(&mut self, entry: &Integer, width: u8) -> Result<i64, Failure> {
        if entry.negative && entry.unsigned {
            return Err(self.semantic_error(
                ErrorKind::NumberRange,
                "snbt.parser.expected_non_negative_number",
                DiagnosticArgument::None,
            ));
        }
        let value = self.input[entry.digits.clone()]
            .iter()
            .filter(|&&unit| unit != 95)
            .try_fold(0u64, |value, &unit| {
                value
                    .checked_mul(u64::from(entry.radix))
                    .and_then(|value| value.checked_add(u64::from(hex_digit(unit).unwrap())))
            });
        let Some(value) = value else {
            return Err(self.number_error(entry, width));
        };
        let limit = if entry.unsigned {
            if width == 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            }
        } else {
            (1u64 << (width - 1)) - u64::from(!entry.negative)
        };
        if value > limit {
            return Err(self.number_error(entry, width));
        }
        let signed = if entry.negative {
            value.wrapping_neg() as i64
        } else {
            value as i64
        };
        Ok(match width {
            8 => i64::from(signed as i8),
            16 => i64::from(signed as i16),
            32 => i64::from(signed as i32),
            _ => signed,
        })
    }

    fn number_error(&mut self, entry: &Integer, width: u8) -> Failure {
        self.semantic_error(
            ErrorKind::NumberRange,
            "snbt.parser.number_parse_failure",
            DiagnosticArgument::Number {
                digits_start: entry.digits.start,
                digits_end: entry.digits.end,
                radix: entry.radix,
                width,
                unsigned: entry.unsigned,
                negative: entry.negative,
            },
        )
    }

    fn number(&mut self, depth: usize) -> Result<Tag, Failure> {
        let start = self.pos;
        match self.float() {
            Ok(value) => return Ok(value),
            Err(failure) if resource_error(failure.kind) => return Err(failure),
            Err(failure) => {
                self.record_error(failure);
                self.pos = start;
            }
        }
        let entry = match self.integer() {
            Ok(entry) => entry,
            Err(failure) => {
                self.record_error(failure);
                self.pos = start;
                // A syntactically invalid numeric candidate can reach the
                // unquoted rule, including its optional argument suffix. An
                // already parsed integer with an out-of-range value cannot.
                return self.word_value(depth);
            }
        };
        let width = entry.width.unwrap_or(32);
        let value = self.integer_value(&entry, width)?;
        Ok(match width {
            8 => Tag::Byte(value as i8),
            16 => Tag::Short(value as i16),
            64 => Tag::Long(value),
            _ => Tag::Int(value as i32),
        })
    }

    fn exponent(&mut self) -> Option<(bool, Range<usize>)> {
        let start = self.pos;
        if self.eat_pair(101, 69) {
            let negative = self.sign();
            match self.digits(10) {
                Ok(digits) => return Some((negative, digits)),
                Err(failure) => self.record_error(failure),
            }
        }
        self.pos = start;
        None
    }

    fn float(&mut self) -> Result<Tag, Failure> {
        let negative = self.sign();
        let whole_start = self.pos;
        let whole = match self.digits(10) {
            Ok(digits) => Some(digits),
            Err(_) => {
                self.pos = whole_start;
                None
            }
        };
        let fraction = if self.eat(46) {
            let start = self.pos;
            match self.digits(10) {
                Ok(digits) => Some(digits),
                Err(failure) if whole.is_none() => return Err(failure),
                Err(_) => {
                    self.pos = start;
                    Some(start..start)
                }
            }
        } else {
            None
        };
        if whole.is_none() && fraction.is_none() {
            return Err(self.err(ErrorKind::Syntax));
        }
        let exponent = self.exponent();
        let float32 = if self.eat_pair(102, 70) {
            Some(true)
        } else if self.eat_pair(100, 68) {
            Some(false)
        } else {
            None
        };
        if fraction.is_none() && exponent.is_none() && float32.is_none() {
            return Err(self.err(ErrorKind::Syntax));
        }
        let capacity = whole.as_ref().map_or(0, Range::len)
            + fraction.as_ref().map_or(0, Range::len)
            + exponent.as_ref().map_or(0, |(_, range)| range.len())
            + 4;
        self.allocation(capacity)?;
        let mut normalized = Vec::new();
        normalized
            .try_reserve_exact(capacity)
            .map_err(|_| self.err(ErrorKind::AllocationFailed))?;
        if negative {
            normalized.push(b'-');
        }
        if let Some(whole) = whole {
            append_digits(&mut normalized, &self.input[whole]);
        }
        if let Some(fraction) = fraction {
            normalized.push(b'.');
            append_digits(&mut normalized, &self.input[fraction]);
        }
        if let Some((negative, exponent)) = exponent {
            normalized.push(b'e');
            if negative {
                normalized.push(b'-');
            }
            append_digits(&mut normalized, &self.input[exponent]);
        }
        let text = std::str::from_utf8(&normalized).map_err(|_| self.err(ErrorKind::Syntax))?;
        if float32 == Some(true) {
            let value: f32 = text.parse().map_err(|_| self.err(ErrorKind::NumberRange))?;
            if !value.is_finite() {
                return Err(self.semantic_error(
                    ErrorKind::NumberRange,
                    "snbt.parser.infinity_not_allowed",
                    DiagnosticArgument::None,
                ));
            }
            Ok(Tag::Float(if value == 0.0 { 0.0 } else { value }))
        } else {
            let value: f64 = text.parse().map_err(|_| self.err(ErrorKind::NumberRange))?;
            if !value.is_finite() {
                return Err(self.semantic_error(
                    ErrorKind::NumberRange,
                    "snbt.parser.infinity_not_allowed",
                    DiagnosticArgument::None,
                ));
            }
            Ok(Tag::Double(if value == 0.0 { 0.0 } else { value }))
        }
    }
}

fn append_digits(output: &mut Vec<u8>, input: &[u16]) {
    output.extend(
        input
            .iter()
            .filter(|&&unit| unit != 95)
            .map(|&unit| unit as u8),
    );
}

fn java_whitespace(unit: u16) -> bool {
    matches!(unit, 9..=13 | 0x1c..=0x20 | 0x1680 | 0x2000..=0x2006 | 0x2008..=0x200a | 0x2028..=0x2029 | 0x205f | 0x3000)
}

fn number_start(unit: u16) -> bool {
    matches!(unit, 43 | 45 | 46 | 48..=57)
}
fn unquoted(unit: u16) -> bool {
    matches!(unit, 43 | 45 | 46 | 48..=57 | 65..=90 | 95 | 97..=122)
}
fn name_character(unit: u16) -> bool {
    matches!(unit, 32 | 45 | 48..=57 | 65..=90 | 97..=122)
}
fn hex_digit(unit: u16) -> Option<u8> {
    match unit {
        48..=57 => Some((unit - 48) as u8),
        65..=70 => Some((unit - 55) as u8),
        97..=102 => Some((unit - 87) as u8),
        _ => None,
    }
}
fn ascii_eq(input: &[u16], word: &[u8], ignore_case: bool) -> bool {
    input.len() == word.len()
        && input.iter().zip(word).all(|(&unit, &byte)| {
            unit < 128
                && if ignore_case {
                    (unit as u8).eq_ignore_ascii_case(&byte)
                } else {
                    unit as u8 == byte
                }
        })
}
fn resource_error(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::AllocationBudget
            | ErrorKind::AllocationFailed
            | ErrorKind::DepthLimit
            | ErrorKind::InputLimit
    )
}

fn uuid(input: &[u16]) -> Option<[i32; 4]> {
    if input.len() > 36 {
        return None;
    }
    let mut parts = [0u64; 5];
    let mut index = 0;
    let mut start = 0;
    for end in 0..=input.len() {
        if end != input.len() && input[end] != 45 {
            continue;
        }
        if index == 5 {
            return None;
        }
        let mut segment = &input[start..end];
        if segment.first() == Some(&43) {
            segment = &segment[1..];
        }
        if segment.is_empty() {
            return None;
        }
        let mut value = 0u64;
        for &unit in segment {
            value = value
                .checked_mul(16)?
                .checked_add(u64::from(crate::unicode_names::hex_digit_utf16(unit)?))?;
            if value > i64::MAX as u64 {
                return None;
            }
        }
        parts[index] = value;
        index += 1;
        start = end + 1;
    }
    if index != 5 {
        return None;
    }
    let high = ((parts[0] & 0xffff_ffff) << 32) | ((parts[1] & 0xffff) << 16) | (parts[2] & 0xffff);
    let low = ((parts[3] & 0xffff) << 48) | (parts[4] & 0xffff_ffff_ffff);
    Some([
        (high >> 32) as i32,
        high as i32,
        (low >> 32) as i32,
        low as i32,
    ])
}
