use super::{Argument, Budget, Error, ErrorKind, Limits, Node, Path};
use crate::nbt::{NbtString, Tag};
use crate::snbt;

impl Path {
    /// Reads one path argument, preserving its spelling. The returned cursor is
    /// an absolute UTF-16 offset; an ASCII space ends the argument.
    pub fn parse(input: &str, limits: Limits) -> Result<(Self, usize), Error> {
        let count = input.encode_utf16().count();
        check_input(count, 0, limits)?;
        let mut budget = Budget::new(limits);
        budget.work(count)?;
        let mut units = Vec::new();
        budget.reserve(&mut units, count)?;
        units.extend(input.encode_utf16());
        Parser::new(&units, 0, limits, budget).parse()
    }

    /// Reads from an existing Java string without converting or normalizing
    /// isolated surrogates. `start` and the result are absolute UTF-16 offsets.
    /// Diagnostic source spans also address the supplied input, not the path's
    /// retained substring. The entire input, including its suffix, is limited.
    pub fn parse_utf16(
        input: &[u16],
        start: usize,
        limits: Limits,
    ) -> Result<(Self, usize), Error> {
        check_input(input.len(), start, limits)?;
        Parser::new(input, start, limits, Budget::new(limits)).parse()
    }
}

fn check_input(count: usize, start: usize, limits: Limits) -> Result<(), Error> {
    if start > count {
        Err(Error::resource(ErrorKind::InvalidPath))
    } else if count > limits.input_units {
        Err(Error::resource(ErrorKind::InputLimit))
    } else {
        Ok(())
    }
}

struct Parser<'a> {
    input: &'a [u16],
    start: usize,
    pos: usize,
    limits: Limits,
    budget: Budget,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u16], start: usize, limits: Limits, budget: Budget) -> Self {
        Self {
            input,
            start,
            pos: start,
            limits,
            budget,
        }
    }

    fn parse(mut self) -> Result<(Path, usize), Error> {
        let mut nodes = Vec::new();
        let mut ends = Vec::new();
        let mut last_wildcard_end = 0;
        while self.peek().is_some_and(|unit| unit != u16::from(b' ')) {
            if nodes.len() >= self.limits.node_count {
                return Err(Error::resource(ErrorKind::NodeLimit));
            }
            self.budget.reserve(&mut nodes, 1)?;
            self.budget.reserve(&mut ends, 1)?;
            let node = self.node(nodes.is_empty())?;
            let end = self.pos - self.start;
            if matches!(node, Node::All) {
                // Vanilla reuses one wildcard node, so its diagnostic position
                // is the last occurrence even when an earlier wildcard fails.
                last_wildcard_end = end;
            }
            nodes.push(node);
            ends.push(end);
            if let Some(next) = self.peek()
                && !matches!(next, 0x20 | 0x5b | 0x7b)
            {
                self.expect(b'.')?;
            }
        }
        let original = self.copy_string(self.start, self.pos)?;
        Ok((
            Path {
                original,
                nodes,
                ends,
                last_wildcard_end,
            },
            self.pos,
        ))
    }

    fn node(&mut self, first: bool) -> Result<Node, Error> {
        match self.peek() {
            Some(0x5b) => {
                self.advance()?;
                match self.peek() {
                    Some(0x5d) => {
                        self.advance()?;
                        Ok(Node::All)
                    }
                    Some(0x7b) => {
                        let pattern = self.pattern()?;
                        self.expect(b']')?;
                        Ok(Node::MatchElement(pattern))
                    }
                    None => Err(Error::parse(
                        ErrorKind::InvalidPath,
                        self.pos,
                        "arguments.nbtpath.node.invalid",
                        Argument::None,
                    )),
                    _ => {
                        let index = self.index()?;
                        self.expect(b']')?;
                        Ok(Node::Index(index))
                    }
                }
            }
            Some(0x7b) if first => self.pattern().map(Node::MatchRoot),
            Some(0x7b) => Err(self.invalid_node()),
            _ => {
                let name = match self.peek() {
                    Some(delimiter @ (0x22 | 0x27)) => self.quoted_name(delimiter)?,
                    _ => self.unquoted_name()?,
                };
                if name.is_empty() {
                    return Err(self.invalid_node());
                }
                if self.peek() == Some(u16::from(b'{')) {
                    let pattern = self.pattern()?;
                    Ok(Node::MatchChild { name, pattern })
                } else {
                    Ok(Node::Child(name))
                }
            }
        }
    }

    fn unquoted_name(&mut self) -> Result<NbtString, Error> {
        let start = self.pos;
        while self.peek().is_some_and(|unit| {
            !matches!(unit, 0x20 | 0x22 | 0x27 | 0x5b | 0x5d | 0x2e | 0x7b | 0x7d)
        }) {
            self.advance()?;
        }
        if self.pos == start {
            Err(self.invalid_node())
        } else {
            self.copy_string(start, self.pos)
        }
    }

    fn quoted_name(&mut self, delimiter: u16) -> Result<NbtString, Error> {
        self.advance()?;
        let start = self.pos;
        let mut length = 0;
        let mut escaped = false;
        while let Some(unit) = self.peek() {
            self.advance()?;
            if escaped {
                if unit != delimiter && unit != u16::from(b'\\') {
                    return Err(Error::parse(
                        ErrorKind::InvalidQuotedEscape,
                        self.pos - 1,
                        "parsing.quote.escape",
                        Argument::Character(unit),
                    ));
                }
                escaped = false;
            } else if unit == u16::from(b'\\') {
                escaped = true;
                continue;
            } else if unit == delimiter {
                let content = &self.input[start..self.pos - 1];
                self.budget.work(content.len())?;
                let mut decoded = Vec::new();
                self.budget.reserve(&mut decoded, length)?;
                let mut units = content.iter().copied();
                while let Some(unit) = units.next() {
                    if unit == u16::from(b'\\') {
                        // The validation pass proved every escape has a unit.
                        if let Some(escaped) = units.next() {
                            decoded.push(escaped);
                        }
                    } else {
                        decoded.push(unit);
                    }
                }
                return Ok(NbtString::from_utf16(decoded));
            }
            length += 1;
        }
        Err(Error::parse(
            ErrorKind::UnclosedQuote,
            self.pos,
            "parsing.quote.expected.end",
            Argument::None,
        ))
    }

    fn index(&mut self) -> Result<i32, Error> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|unit| matches!(unit, 0x30..=0x39 | 0x2d | 0x2e))
        {
            self.advance()?;
        }
        if start == self.pos {
            return Err(Error::parse(
                ErrorKind::ExpectedIndex,
                start,
                "parsing.int.expected",
                Argument::None,
            ));
        }
        let end = self.pos;
        let token = &self.input[start..end];
        self.budget.work(token.len())?;
        let negative = token[0] == u16::from(b'-');
        let digits = &token[usize::from(negative)..];
        let limit = if negative {
            2_147_483_648
        } else {
            2_147_483_647
        };
        let mut value = 0_u32;
        let mut valid = !digits.is_empty();
        for &unit in digits {
            if !(0x30..=0x39).contains(&unit) {
                valid = false;
                break;
            }
            let digit = u32::from(unit - u16::from(b'0'));
            if value > (limit - digit) / 10 {
                valid = false;
                break;
            }
            value = value * 10 + digit;
        }
        if !valid {
            return Err(Error::parse(
                ErrorKind::InvalidIndex,
                start,
                "parsing.int.invalid",
                Argument::Source { start, end },
            ));
        }
        if negative {
            Ok((-(i64::from(value))) as i32)
        } else {
            Ok(value as i32)
        }
    }

    fn pattern(&mut self) -> Result<Tag, Error> {
        let start = self.pos;
        let limits = snbt::Limits {
            input_units: self.limits.input_units,
            allocation_bytes: self.budget.remaining_allocation(),
            ..snbt::Limits::default()
        };
        let (tag, consumed, allocation) =
            snbt::parse_prefix_accounted(&self.input[start..], limits)
                .map_err(|error| snbt_error(error, start))?;
        self.budget.charge(allocation)?;
        self.budget.work(consumed)?;
        self.pos += consumed;
        if matches!(tag, Tag::Compound(_)) {
            Ok(tag)
        } else {
            Err(Error::parse(
                ErrorKind::ExpectedCompound,
                self.pos,
                "argument.nbt.expected.compound",
                Argument::None,
            ))
        }
    }

    fn copy_string(&mut self, start: usize, end: usize) -> Result<NbtString, Error> {
        let length = end - start;
        self.budget.work(length)?;
        let mut units = Vec::new();
        self.budget.reserve(&mut units, length)?;
        units.extend_from_slice(&self.input[start..end]);
        Ok(NbtString::from_utf16(units))
    }

    fn invalid_node(&self) -> Error {
        Error::parse(
            ErrorKind::InvalidNode,
            self.pos,
            "arguments.nbtpath.node.invalid",
            Argument::None,
        )
    }

    fn expect(&mut self, expected: u8) -> Result<(), Error> {
        if self.peek() == Some(u16::from(expected)) {
            self.advance()
        } else {
            Err(Error::parse(
                ErrorKind::ExpectedSymbol,
                self.pos,
                "parsing.expected",
                Argument::Character(u16::from(expected)),
            ))
        }
    }

    fn peek(&self) -> Option<u16> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Result<(), Error> {
        self.budget.work(1)?;
        self.pos += 1;
        Ok(())
    }
}

fn snbt_error(error: snbt::Error, base_offset: usize) -> Error {
    let kind = match error.kind {
        snbt::ErrorKind::AllocationBudget => ErrorKind::AllocationBudget,
        snbt::ErrorKind::AllocationFailed => ErrorKind::AllocationFailed,
        snbt::ErrorKind::InputLimit => ErrorKind::InputLimit,
        snbt::ErrorKind::DepthLimit => ErrorKind::DepthLimit,
        _ => ErrorKind::Snbt,
    };
    match error.diagnostic {
        Some(diagnostic) => Error::parse(
            kind,
            base_offset + error.offset_utf16,
            diagnostic.key,
            Argument::Snbt {
                diagnostic,
                base_offset,
            },
        ),
        None => Error::resource(kind),
    }
}
