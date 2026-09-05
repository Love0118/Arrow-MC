//! Bounded, iterative NbtUtils.compareNbt and exact NBT equality.
//!
//! Compound predicates are subsets. Partial lists allow each expected element
//! to reuse an actual match, but still require actual.len() >= expected.len().
//! Strict lists use exact equality throughout their descendants. Resource
//! exhaustion is an error, never a predicate non-match.

use super::{CompoundEntry, NbtString, Tag};
use std::{cmp::Ordering, fmt};

#[derive(Clone, Copy, Debug)]
pub struct CompareLimits {
    /// Maximum traversed parent/child edges. This is caller policy, not an NBT
    /// or Vanilla compareNbt limit. The root is depth zero.
    pub max_depth: usize,
    /// Shared node attempts and primitive/key/string/array comparison work.
    pub work_units: usize,
    /// Maximum requested heap bytes of the active DFS continuation stack.
    /// Allocator bookkeeping and size-class slack are not an RSS measurement.
    pub stack_bytes: usize,
}

impl Default for CompareLimits {
    fn default() -> Self {
        Self {
            max_depth: usize::MAX,
            work_units: 1_000_000,
            stack_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompareError {
    WorkLimit,
    DepthLimit,
    StackLimit,
    AllocationLimit,
    AllocationFailed,
}

impl fmt::Display for CompareError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::WorkLimit => "NBT comparison work limit exceeded",
            Self::DepthLimit => "NBT comparison depth limit exceeded",
            Self::StackLimit => "NBT comparison stack byte limit exceeded",
            Self::AllocationLimit => "NBT comparison cumulative allocation limit exceeded",
            Self::AllocationFailed => "NBT comparison stack allocation failed",
        })
    }
}

impl std::error::Error for CompareError {}

/// A work budget shared across all predicates and traversal in one operation.
/// Each comparison releases its temporary stack before returning. The work
/// counter is cumulative even when a comparison fails or returns false.
pub struct CompareBudget {
    limits: CompareLimits,
    work_remaining: usize,
}

impl CompareBudget {
    pub fn new(limits: CompareLimits) -> Self {
        Self {
            limits,
            work_remaining: limits.work_units,
        }
    }

    pub fn work_remaining(&self) -> usize {
        self.work_remaining
    }

    /// Lets a path operation charge its own traversal to the same shared budget.
    /// An exhausted counter remains exhausted; retrying cannot restore work.
    pub fn charge_work(&mut self, units: usize) -> Result<(), CompareError> {
        match self.work_remaining.checked_sub(units) {
            Some(remaining) => {
                self.work_remaining = remaining;
                Ok(())
            }
            None => {
                self.work_remaining = 0;
                Err(CompareError::WorkLimit)
            }
        }
    }

    pub fn compare(
        &mut self,
        expected: Option<&Tag>,
        actual: Option<&Tag>,
        partial_lists: bool,
    ) -> Result<bool, CompareError> {
        let mut allocation_remaining = usize::MAX;
        self.compare_accounted(expected, actual, partial_lists, &mut allocation_remaining)
    }

    /// Shares the caller's cumulative requested-allocation budget. Every stack
    /// growth charges the entire new capacity before allocation, including any
    /// replacement buffer. Work and allocation charges survive errors.
    pub fn compare_accounted(
        &mut self,
        expected: Option<&Tag>,
        actual: Option<&Tag>,
        partial_lists: bool,
        allocation_remaining: &mut usize,
    ) -> Result<bool, CompareError> {
        let mode = if partial_lists {
            Mode::PartialLists
        } else {
            Mode::StrictLists
        };
        self.run(expected, actual, mode, allocation_remaining)
    }

    /// Exact equality without recursive Tag::eq, retaining NaN equality and
    /// distinguishing signed zeros as Java record equality does.
    pub fn equal(&mut self, expected: &Tag, actual: &Tag) -> Result<bool, CompareError> {
        let mut allocation_remaining = usize::MAX;
        self.equal_accounted(expected, actual, &mut allocation_remaining)
    }

    /// Exact equality with the caller's cumulative requested-allocation budget.
    pub fn equal_accounted(
        &mut self,
        expected: &Tag,
        actual: &Tag,
        allocation_remaining: &mut usize,
    ) -> Result<bool, CompareError> {
        self.run(
            Some(expected),
            Some(actual),
            Mode::Exact,
            allocation_remaining,
        )
    }

    fn run<'a>(
        &mut self,
        expected: Option<&'a Tag>,
        actual: Option<&'a Tag>,
        mode: Mode,
        allocation_remaining: &mut usize,
    ) -> Result<bool, CompareError> {
        let mut frames = Vec::new();
        let mut current = Some((expected, actual, mode));
        let mut matched = true;
        loop {
            if let Some((expected, actual, mode)) = current.take() {
                self.charge_work(1)?;
                matched = match (expected, actual) {
                    (None, _) => true,
                    (Some(_), None) => false,
                    (Some(expected), Some(actual)) if std::ptr::eq(expected, actual) => true,
                    (Some(Tag::Compound(expected)), Some(Tag::Compound(actual))) => {
                        let expected = expected.entries();
                        let actual = actual.entries();
                        if actual.len() < expected.len()
                            || (mode == Mode::Exact && expected.len() != actual.len())
                        {
                            false
                        } else if expected.is_empty() {
                            true
                        } else {
                            let value = self.child(actual, &expected[0].name)?;
                            self.push(
                                allocation_remaining,
                                &mut frames,
                                Frame::Compound {
                                    expected,
                                    actual,
                                    next: 1,
                                    mode,
                                },
                            )?;
                            current = Some((Some(&expected[0].value), value, mode));
                            continue;
                        }
                    }
                    (Some(Tag::List(expected)), Some(Tag::List(actual))) => {
                        if expected.is_empty() {
                            actual.is_empty()
                        } else if actual.len() < expected.len()
                            || (mode != Mode::PartialLists && expected.len() != actual.len())
                        {
                            false
                        } else if mode == Mode::PartialLists {
                            self.push(
                                allocation_remaining,
                                &mut frames,
                                Frame::PartialList {
                                    expected,
                                    actual,
                                    expected_index: 0,
                                    actual_index: 0,
                                },
                            )?;
                            current = Some((Some(&expected[0]), Some(&actual[0]), mode));
                            continue;
                        } else {
                            self.push(
                                allocation_remaining,
                                &mut frames,
                                Frame::Sequence {
                                    expected,
                                    actual,
                                    next: 1,
                                },
                            )?;
                            current = Some((Some(&expected[0]), Some(&actual[0]), Mode::Exact));
                            continue;
                        }
                    }
                    (Some(expected), Some(actual)) => self.primitive_equal(expected, actual)?,
                };
            }

            match frames.pop() {
                None => return Ok(matched),
                Some(Frame::Compound {
                    expected,
                    actual,
                    next,
                    mode,
                }) => {
                    if matched && next < expected.len() {
                        let value = self.child(actual, &expected[next].name)?;
                        self.push(
                            allocation_remaining,
                            &mut frames,
                            Frame::Compound {
                                expected,
                                actual,
                                next: next + 1,
                                mode,
                            },
                        )?;
                        current = Some((Some(&expected[next].value), value, mode));
                    }
                }
                Some(Frame::Sequence {
                    expected,
                    actual,
                    next,
                }) => {
                    if matched && next < expected.len() {
                        self.push(
                            allocation_remaining,
                            &mut frames,
                            Frame::Sequence {
                                expected,
                                actual,
                                next: next + 1,
                            },
                        )?;
                        current = Some((Some(&expected[next]), Some(&actual[next]), Mode::Exact));
                    }
                }
                Some(Frame::PartialList {
                    expected,
                    actual,
                    mut expected_index,
                    mut actual_index,
                }) => {
                    if matched {
                        expected_index += 1;
                        actual_index = 0;
                    } else {
                        actual_index += 1;
                    }
                    if expected_index < expected.len() && actual_index < actual.len() {
                        self.push(
                            allocation_remaining,
                            &mut frames,
                            Frame::PartialList {
                                expected,
                                actual,
                                expected_index,
                                actual_index,
                            },
                        )?;
                        current = Some((
                            Some(&expected[expected_index]),
                            Some(&actual[actual_index]),
                            Mode::PartialLists,
                        ));
                    }
                }
            }
        }
    }

    fn push<'a>(
        &self,
        allocation_remaining: &mut usize,
        frames: &mut Vec<Frame<'a>>,
        frame: Frame<'a>,
    ) -> Result<(), CompareError> {
        if frames.len() >= self.limits.max_depth {
            return Err(CompareError::DepthLimit);
        }
        let allowed = self.limits.stack_bytes / size_of::<Frame<'a>>();
        if frames.len() >= allowed {
            return Err(CompareError::StackLimit);
        }
        if frames.len() == frames.capacity() {
            let capacity = frames.capacity().saturating_mul(2).max(4).min(allowed);
            if capacity <= frames.len() {
                return Err(CompareError::StackLimit);
            }
            let bytes = capacity * size_of::<Frame<'a>>();
            *allocation_remaining = allocation_remaining
                .checked_sub(bytes)
                .ok_or(CompareError::AllocationLimit)?;
            frames
                .try_reserve_exact(capacity - frames.len())
                .map_err(|_| CompareError::AllocationFailed)?;
        }
        frames.push(frame);
        Ok(())
    }

    fn child<'a>(
        &mut self,
        entries: &'a [CompoundEntry],
        key: &NbtString,
    ) -> Result<Option<&'a Tag>, CompareError> {
        let mut low = 0;
        let mut high = entries.len();
        while low < high {
            self.charge_work(1)?;
            let middle = low + (high - low) / 2;
            match self.string_order(key.as_utf16(), entries[middle].name.as_utf16())? {
                Ordering::Less => high = middle,
                Ordering::Greater => low = middle + 1,
                Ordering::Equal => return Ok(Some(&entries[middle].value)),
            }
        }
        Ok(None)
    }

    fn string_order(&mut self, left: &[u16], right: &[u16]) -> Result<Ordering, CompareError> {
        for (&left, &right) in left.iter().zip(right) {
            self.charge_work(1)?;
            if left != right {
                return Ok(left.cmp(&right));
            }
        }
        Ok(left.len().cmp(&right.len()))
    }

    fn primitive_equal(&mut self, expected: &Tag, actual: &Tag) -> Result<bool, CompareError> {
        Ok(match (expected, actual) {
            (Tag::End, Tag::End) => true,
            (Tag::Byte(left), Tag::Byte(right)) => left == right,
            (Tag::Short(left), Tag::Short(right)) => left == right,
            (Tag::Int(left), Tag::Int(right)) => left == right,
            (Tag::Long(left), Tag::Long(right)) => left == right,
            (Tag::Float(left), Tag::Float(right)) => {
                left.to_bits() == right.to_bits() || (left.is_nan() && right.is_nan())
            }
            (Tag::Double(left), Tag::Double(right)) => {
                left.to_bits() == right.to_bits() || (left.is_nan() && right.is_nan())
            }
            (Tag::String(left), Tag::String(right)) => {
                self.string_order(left.as_utf16(), right.as_utf16())? == Ordering::Equal
            }
            (Tag::ByteArray(left), Tag::ByteArray(right)) => {
                if left.len() != right.len() {
                    return Ok(false);
                }
                for (&left, &right) in left.iter().zip(right) {
                    self.charge_work(1)?;
                    if left != right {
                        return Ok(false);
                    }
                }
                true
            }
            (Tag::IntArray(left), Tag::IntArray(right)) => {
                if left.len() != right.len() {
                    return Ok(false);
                }
                for (&left, &right) in left.iter().zip(right) {
                    self.charge_work(1)?;
                    if left != right {
                        return Ok(false);
                    }
                }
                true
            }
            (Tag::LongArray(left), Tag::LongArray(right)) => {
                if left.len() != right.len() {
                    return Ok(false);
                }
                for (&left, &right) in left.iter().zip(right) {
                    self.charge_work(1)?;
                    if left != right {
                        return Ok(false);
                    }
                }
                true
            }
            _ => false,
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    PartialLists,
    StrictLists,
    Exact,
}

enum Frame<'a> {
    Compound {
        expected: &'a [CompoundEntry],
        actual: &'a [CompoundEntry],
        next: usize,
        mode: Mode,
    },
    Sequence {
        expected: &'a [Tag],
        actual: &'a [Tag],
        next: usize,
    },
    PartialList {
        expected: &'a [Tag],
        actual: &'a [Tag],
        expected_index: usize,
        actual_index: usize,
    },
}
