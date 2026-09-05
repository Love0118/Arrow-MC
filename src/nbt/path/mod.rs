//! Concrete NBT path queries and synchronous mutation for the locked reference.
//! Selections borrow owned tree values; primitive-array elements are detached
//! numeric values. Mutations preserve prior successful effects on later failure.

mod budget;
mod execute;
mod parse;

use super::{NbtString, Tag};
use crate::snbt;
use std::fmt;

pub(crate) use budget::Budget;

#[derive(Clone, Debug)]
pub enum Node {
    Child(NbtString),
    Index(i32),
    All,
    MatchElement(Tag),
    MatchChild { name: NbtString, pattern: Tag },
    MatchRoot(Tag),
}

#[derive(Debug)]
pub struct Path {
    pub(crate) original: NbtString,
    pub(crate) nodes: Vec<Node>,
    pub(crate) ends: Vec<usize>,
    pub(crate) last_wildcard_end: usize,
}

impl Path {
    pub fn as_string(&self) -> &NbtString {
        &self.original
    }
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub(crate) fn not_found(&self, index: usize) -> Error {
        let end = if matches!(self.nodes[index], Node::All) {
            self.last_wildcard_end
        } else {
            self.ends[index]
        };
        Error::operation(
            ErrorKind::NothingFound,
            "arguments.nbtpath.nothing_found",
            Argument::Source { start: 0, end },
        )
    }
}

/// Matches Vanilla's depth>=512 test; arrays' primitive elements do not add
/// child depth. Traversal work/allocation policy is independently configurable.
pub fn is_too_deep(value: &Tag, start_depth: usize, limits: Limits) -> Result<bool, Error> {
    Budget::new(limits).too_deep(value, start_depth)
}

#[derive(Debug)]
pub enum Selection<'a> {
    Borrowed(&'a Tag),
    Detached(Tag),
}

impl Selection<'_> {
    pub fn as_tag(&self) -> &Tag {
        match self {
            Self::Borrowed(value) => value,
            Self::Detached(value) => value,
        }
    }
}

#[derive(Debug)]
pub enum SelectionMut<'a> {
    Borrowed(&'a mut Tag),
    Detached(Tag),
}

impl SelectionMut<'_> {
    pub fn as_tag(&self) -> &Tag {
        match self {
            Self::Borrowed(value) => value,
            Self::Detached(value) => value,
        }
    }
    pub fn as_tag_mut(&mut self) -> &mut Tag {
        match self {
            Self::Borrowed(value) => value,
            Self::Detached(value) => value,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub input_units: usize,
    /// Resource policy; does not impose the unrelated 512 source-depth rule.
    pub node_count: usize,
    /// Cumulative requested internal backing bytes, including replacement
    /// capacities, copies and comparator scratch reservation. Excludes allocator
    /// metadata and caller factory allocation; this is not process RSS.
    pub allocation_bytes: usize,
    pub candidates: usize,
    /// Shared path/comparison work. The embedded SNBT grammar is separately
    /// bounded by input, allocation and its documented parser depth policy.
    pub work_units: usize,
    pub comparison_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            input_units: 2 * 1024 * 1024,
            node_count: 4096,
            allocation_bytes: 32 * 1024 * 1024,
            candidates: 1_000_000,
            work_units: 1_000_000,
            comparison_depth: usize::MAX,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Argument {
    None,
    Character(u16),
    Index(i32),
    Text(NbtString),
    /// Parser arguments index the input passed to parse; operation arguments
    /// index Path::as_string(). Pass that same slice to write_argument.
    Source {
        start: usize,
        end: usize,
    },
    Tag(Tag),
    Snbt {
        diagnostic: snbt::Diagnostic,
        base_offset: usize,
    },
}

impl Drop for Argument {
    fn drop(&mut self) {
        if let Self::Tag(value) = self {
            std::mem::replace(value, Tag::End).drop_iterative();
        }
    }
}

#[derive(Clone, Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub cursor: Option<usize>,
    pub key: &'static str,
    pub argument: Argument,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    InvalidPath,
    InvalidNode,
    ExpectedSymbol,
    InvalidIndex,
    ExpectedIndex,
    InvalidQuotedEscape,
    UnclosedQuote,
    ExpectedCompound,
    Snbt,
    TooDeep,
    NothingFound,
    ExpectedList,
    AllocationBudget,
    AllocationFailed,
    WorkLimit,
    CandidateLimit,
    InputLimit,
    NodeLimit,
    DepthLimit,
    LengthOverflow,
}

impl Error {
    pub(crate) fn parse(
        kind: ErrorKind,
        cursor: usize,
        key: &'static str,
        argument: Argument,
    ) -> Self {
        Self {
            kind,
            cursor: Some(cursor),
            key,
            argument,
        }
    }
    pub(crate) fn operation(kind: ErrorKind, key: &'static str, argument: Argument) -> Self {
        Self {
            kind,
            cursor: None,
            key,
            argument,
        }
    }
    pub(crate) fn resource(kind: ErrorKind) -> Self {
        Self::operation(kind, "", Argument::None)
    }
    pub fn translation_key(&self) -> Option<&'static str> {
        if self.key.is_empty() {
            None
        } else {
            Some(self.key)
        }
    }

    pub fn write_argument(
        &self,
        input: &[u16],
        output: &mut Vec<u16>,
        max_output_units: usize,
    ) -> Result<bool, snbt::Error> {
        let start = output.len();
        let result = self.argument.write(input, output, max_output_units);
        if result.is_err() {
            output.truncate(start);
        }
        result
    }
}

impl Argument {
    fn write(
        &self,
        input: &[u16],
        output: &mut Vec<u16>,
        limit: usize,
    ) -> Result<bool, snbt::Error> {
        let invalid = || snbt::Error {
            offset_utf16: 0,
            kind: snbt::ErrorKind::InvalidDiagnostic,
            diagnostic: None,
        };
        let units = match self {
            Self::None => return Ok(false),
            Self::Character(unit) => {
                return snbt::Diagnostic {
                    key: "",
                    argument: snbt::DiagnosticArgument::Literal {
                        first: *unit,
                        second: None,
                    },
                }
                .write_argument(input, output, limit);
            }
            Self::Index(index) => {
                let mut digits = [0u16; 11];
                let mut cursor = digits.len();
                let mut value = index.unsigned_abs();
                loop {
                    cursor -= 1;
                    digits[cursor] = 48 + (value % 10) as u16;
                    value /= 10;
                    if value == 0 {
                        break;
                    }
                }
                if *index < 0 {
                    cursor -= 1;
                    digits[cursor] = 45;
                }
                append_argument(&digits[cursor..], output, limit)?;
                return Ok(true);
            }
            Self::Text(text) => text.as_utf16(),
            Self::Source { start, end } => input.get(*start..*end).ok_or_else(invalid)?,
            Self::Tag(tag) => {
                snbt::write(
                    tag,
                    output,
                    snbt::Limits {
                        output_units: limit,
                        ..snbt::Limits::default()
                    },
                )?;
                return Ok(true);
            }
            Self::Snbt {
                diagnostic,
                base_offset,
            } => {
                return diagnostic.write_argument(
                    input.get(*base_offset..).ok_or_else(invalid)?,
                    output,
                    limit,
                );
            }
        };
        append_argument(units, output, limit)?;
        Ok(true)
    }
}

fn append_argument(units: &[u16], output: &mut Vec<u16>, limit: usize) -> Result<(), snbt::Error> {
    if units.len() > limit {
        return Err(snbt::Error {
            offset_utf16: 0,
            kind: snbt::ErrorKind::OutputLimit,
            diagnostic: None,
        });
    }
    output.try_reserve(units.len()).map_err(|_| snbt::Error {
        offset_utf16: 0,
        kind: snbt::ErrorKind::AllocationFailed,
        diagnostic: None,
    })?;
    output.extend_from_slice(units);
    Ok(())
}

impl fmt::Display for Error {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "NBT path {:?} at {:?}", self.kind, self.cursor)
    }
}
impl std::error::Error for Error {}
