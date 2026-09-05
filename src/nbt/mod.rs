//! Binary NBT for the locked Java Edition 26.3-pre-2 reference.
//!
//! `read_named`/`write_named` include the disk root's name; the network functions
//! contain only the root type and payload. Lists are logical, possibly mixed
//! lists: the codec implements Vanilla's empty-key compound wrappers.
//!
//! This module deliberately does not provide SNBT, compression, NbtOps, schema
//! codecs, data fixing, or item/component behavior. References: `NbtIo.java`,
//! `ListTag.java`, `CompoundTag.java`, `NbtAccounter.java` and the primitive tag
//! classes under `Decompile/sources/26.3-pre-2/net/minecraft/nbt/`.

mod read;
mod write;

use std::fmt;

pub use read::{read_named, read_network};
pub use write::{write_named, write_network};

/// Java strings are UTF-16 code units, including unpaired surrogates.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NbtString(Vec<u16>);

impl NbtString {
    pub fn from_utf16(units: Vec<u16>) -> Self {
        Self(units)
    }

    pub fn as_utf16(&self) -> &[u16] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Fails instead of discarding an unpaired surrogate.
    pub fn to_string(&self) -> Result<String, std::string::FromUtf16Error> {
        String::from_utf16(&self.0)
    }
}

impl From<&str> for NbtString {
    fn from(value: &str) -> Self {
        Self(value.encode_utf16().collect())
    }
}

/// Sorted UTF-16 keys make output deterministic. Key order is not NBT meaning.
/// Binary decoding sorts once; it does not repeatedly insert into this vector.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Compound(Vec<CompoundEntry>);

#[derive(Clone, Debug)]
pub struct CompoundEntry {
    pub name: NbtString,
    pub value: Tag,
    // Decode-sort scratch retained to avoid a second full compound allocation.
    // Eight bytes per entry on supported 64-bit targets; not semantic state.
    sequence: usize,
}

impl CompoundEntry {
    pub fn new(name: NbtString, value: Tag) -> Self {
        Self {
            name,
            value,
            sequence: 0,
        }
    }
}

impl PartialEq for CompoundEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.value == other.value
    }
}

impl Compound {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a compound in one allocation, preserving the final occurrence of
    /// duplicate keys. This is shared by independently budgeted binary/text readers.
    pub fn from_entries(mut entries: Vec<CompoundEntry>) -> Result<Self, Error> {
        for (sequence, entry) in entries.iter_mut().enumerate() {
            if matches!(entry.value, Tag::End) {
                return Err(Error::UnexpectedEnd);
            }
            entry.sequence = sequence;
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
        Ok(Self(entries))
    }

    pub fn entries(&self) -> &[CompoundEntry] {
        &self.0
    }

    pub fn get(&self, key: &NbtString) -> Option<&Tag> {
        self.0
            .binary_search_by(|entry| entry.name.cmp(key))
            .ok()
            .map(|index| &self.0[index].value)
    }

    /// Replaces an existing key. `End` is a terminator, never a named value.
    pub fn insert(&mut self, key: NbtString, value: Tag) -> Result<Option<Tag>, Error> {
        if matches!(value, Tag::End) {
            return Err(Error::UnexpectedEnd);
        }
        match self.0.binary_search_by(|entry| entry.name.cmp(&key)) {
            Ok(index) => Ok(Some(std::mem::replace(&mut self.0[index].value, value))),
            Err(index) => {
                self.0.try_reserve(1).map_err(|_| Error::AllocationFailed)?;
                self.0.insert(
                    index,
                    CompoundEntry {
                        name: key,
                        value,
                        sequence: 0,
                    },
                );
                Ok(None)
            }
        }
    }

    fn is_wrapper(&self) -> bool {
        self.0.len() == 1 && self.0[0].name.is_empty()
    }
}

/// All binary tag IDs. Byte arrays retain signed Java byte values.
#[derive(Clone, Debug)]
pub enum Tag {
    End,
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(NbtString),
    List(Vec<Tag>),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl PartialEq for Tag {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::End, Self::End) => true,
            (Self::Byte(a), Self::Byte(b)) => a == b,
            (Self::Short(a), Self::Short(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Long(a), Self::Long(b)) => a == b,
            // Java record equality canonicalizes NaNs, distinguishes zero signs.
            (Self::Float(a), Self::Float(b)) => {
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
            }
            (Self::Double(a), Self::Double(b)) => {
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
            }
            (Self::ByteArray(a), Self::ByteArray(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::List(a), Self::List(b)) => a == b,
            (Self::Compound(a), Self::Compound(b)) => a == b,
            (Self::IntArray(a), Self::IntArray(b)) => a == b,
            (Self::LongArray(a), Self::LongArray(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Tag {}

impl Tag {
    pub fn id(&self) -> u8 {
        match self {
            Self::End => 0,
            Self::Byte(_) => 1,
            Self::Short(_) => 2,
            Self::Int(_) => 3,
            Self::Long(_) => 4,
            Self::Float(_) => 5,
            Self::Double(_) => 6,
            Self::ByteArray(_) => 7,
            Self::String(_) => 8,
            Self::List(_) => 9,
            Self::Compound(_) => 10,
            Self::IntArray(_) => 11,
            Self::LongArray(_) => 12,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NamedTag {
    pub name: NbtString,
    pub tag: Tag,
}

/// Resource policies are intentionally separate from each other.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Vanilla's logical Java heap accounting, not Rust allocated bytes.
    pub vanilla_quota_bytes: usize,
    /// Cumulative requested decoded Vec backing bytes, including temporary
    /// buffers and replacement capacities. Excludes stack/allocator metadata;
    /// this is a conservative admission budget, not an RSS measurement.
    pub allocation_bytes: usize,
    /// Number of nested list/compound containers. The supported maximum is
    /// Vanilla's 512; lower limits can be selected at an untrusted boundary.
    pub max_depth: usize,
    /// Maximum bytes appended by one encoding call, independent of decoding.
    pub output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            vanilla_quota_bytes: 2 * 1024 * 1024,
            allocation_bytes: 16 * 1024 * 1024,
            max_depth: 512,
            output_bytes: 16 * 1024 * 1024,
        }
    }
}

impl Limits {
    fn validate(self) -> Result<(), Error> {
        if self.max_depth > 512 {
            Err(Error::InvalidDepthLimit)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    UnknownTag(u8),
    NegativeLength(i32),
    UnexpectedEnd,
    InvalidModifiedUtf8,
    StringTooLong,
    LengthOverflow,
    DepthLimit,
    InvalidDepthLimit,
    VanillaQuotaExceeded,
    AllocationBudgetExceeded,
    AllocationFailed,
    OutputLimit,
    NamedEnd,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTag(id) => write!(f, "unknown NBT tag ID {id}"),
            Self::NegativeLength(length) => write!(f, "negative NBT length {length}"),
            error => f.write_str(match error {
                Self::Truncated => "truncated NBT input",
                Self::UnexpectedEnd => "NBT End is not a list or compound value",
                Self::InvalidModifiedUtf8 => "invalid Java modified UTF-8",
                Self::StringTooLong => "NBT string exceeds 65535 modified UTF-8 bytes",
                Self::LengthOverflow => "NBT length arithmetic overflow",
                Self::DepthLimit => "NBT container depth limit exceeded",
                Self::InvalidDepthLimit => "NBT depth limit must be at most 512",
                Self::VanillaQuotaExceeded => "Vanilla NBT quota exceeded",
                Self::AllocationBudgetExceeded => "NBT decoded allocation budget exceeded",
                Self::AllocationFailed => "NBT buffer allocation failed",
                Self::OutputLimit => "NBT encoded output limit exceeded",
                Self::NamedEnd => "NBT End root cannot carry a name",
                Self::UnknownTag(_) | Self::NegativeLength(_) => unreachable!(),
            }),
        }
    }
}

impl std::error::Error for Error {}
