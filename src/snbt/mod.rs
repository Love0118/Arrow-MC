//! Modern SNBT text compatibility for the locked Java Edition 26.3-pre-2.
//!
//! The parser uses UTF-16 offsets and preserves Java strings, including isolated
//! surrogates. `parse` is a convenience for valid Unicode Rust strings. Limits
//! are Arrow admission policies, not claims that Vanilla's grammar has a fixed
//! nesting or text-size cap. The writer follows the compact Java tag visitor;
//! its End, nonfinite numbers and empty compound keys are not parse round trips.
//!
//! Independently authored from inspected grammar behavior and JVM probes. Local
//! references: SnbtGrammar, SnbtOperations, TagParser, StringTagVisitor and
//! StringTag under Decompile/sources/26.3-pre-2/net/minecraft/nbt.

mod diagnostic;
mod read;
mod write;

use std::fmt;

pub(crate) use read::parse_prefix_accounted;
pub use read::{parse, parse_compound, parse_compound_utf16, parse_prefix, parse_utf16};
pub use write::{write, write_pretty};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum UTF-16 units in supplied input, including an unparsed suffix.
    pub input_units: usize,
    /// Cumulative requested decoded/scratch Vec backing bytes, including full
    /// replacement capacities. Excludes stack and allocator metadata, not RSS.
    pub allocation_bytes: usize,
    /// Arrow's tested stack policy counts lists, maps and builtin calls.
    /// Configurable through 512; Vanilla's SNBT grammar itself has no such cap.
    pub max_depth: usize,
    /// Maximum UTF-16 units appended by one writer call. Acquired Vec capacity
    /// may persist after a failed write; original output units remain intact.
    pub output_units: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            input_units: 2 * 1024 * 1024,
            allocation_bytes: 32 * 1024 * 1024,
            max_depth: 512,
            output_units: 4 * 1024 * 1024,
        }
    }
}

impl Limits {
    pub(crate) fn validate(self) -> Result<(), Error> {
        if self.max_depth > 512 {
            Err(Error {
                offset_utf16: 0,
                kind: ErrorKind::InvalidLimits,
                diagnostic: None,
            })
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    pub offset_utf16: usize,
    pub kind: ErrorKind,
    pub diagnostic: Option<Diagnostic>,
}

impl Error {
    pub fn translation_key(&self) -> Option<&'static str> {
        self.diagnostic.map(|diagnostic| diagnostic.key)
    }
}

/// Parser metadata for a future translated command message. Source spans use
/// UTF-16 offsets into the original input and never retain or clone that input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub key: &'static str,
    pub argument: DiagnosticArgument,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticArgument {
    None,
    Literal {
        first: u16,
        second: Option<u16>,
    },
    HexWidth(u8),
    CodePoint(u32),
    Operation {
        name_start: usize,
        name_end: usize,
        arity: usize,
    },
    Number {
        digits_start: usize,
        digits_end: usize,
        radix: u8,
        width: u8,
        unsigned: bool,
        negative: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Syntax,
    NumberRange,
    InvalidEscape,
    InvalidCodePoint,
    InvalidCharacterName,
    ArrayElementType,
    UnknownOperation,
    ExpectedNumber,
    InvalidUuid,
    TrailingData,
    EmptyKey,
    DepthLimit,
    InputLimit,
    AllocationBudget,
    AllocationFailed,
    OutputLimit,
    InvalidLimits,
    ExpectedCompound,
    InvalidDiagnostic,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SNBT {:?} at UTF-16 offset {}",
            self.kind, self.offset_utf16
        )
    }
}

impl std::error::Error for Error {}
