use super::{Argument, Error, ErrorKind, Limits};
use crate::nbt::predicate::{CompareBudget, CompareError, CompareLimits};
use crate::nbt::{Compound, CompoundEntry, NbtString, Tag};
use std::mem::size_of;

pub(super) fn dispose_tag(value: Tag) {
    value.drop_iterative();
}

pub(super) struct OwnedTag(Option<Tag>);

impl OwnedTag {
    pub(super) fn new(value: Tag) -> Self {
        Self(Some(value))
    }
    pub(super) fn as_tag(&self) -> &Tag {
        self.0.as_ref().expect("owned tag not yet transferred")
    }
    pub(super) fn as_tag_mut(&mut self) -> &mut Tag {
        self.0.as_mut().expect("owned tag not yet transferred")
    }
    pub(super) fn take(&mut self) -> Tag {
        self.0.take().expect("owned tag transfers once")
    }
}

impl Drop for OwnedTag {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            dispose_tag(value);
        }
    }
}

pub(crate) struct Budget {
    limits: Limits,
    allocated: usize,
    candidates: usize,
    comparison: CompareBudget,
}

impl Budget {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            allocated: 0,
            candidates: 0,
            comparison: CompareBudget::new(CompareLimits {
                max_depth: limits.comparison_depth,
                work_units: limits.work_units,
                stack_bytes: limits.allocation_bytes.min(1024 * 1024),
            }),
        }
    }

    pub(crate) fn remaining_allocation(&self) -> usize {
        self.limits.allocation_bytes.saturating_sub(self.allocated)
    }

    pub(crate) fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        let allocated = self
            .allocated
            .checked_add(bytes)
            .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?;
        if allocated > self.limits.allocation_bytes {
            return Err(Error::resource(ErrorKind::AllocationBudget));
        }
        self.allocated = allocated;
        Ok(())
    }

    pub(crate) fn work(&mut self, units: usize) -> Result<(), Error> {
        self.comparison.charge_work(units).map_err(compare_error)
    }

    pub(crate) fn candidate(&mut self) -> Result<(), Error> {
        if self.candidates >= self.limits.candidates {
            return Err(Error::resource(ErrorKind::CandidateLimit));
        }
        self.candidates += 1;
        Ok(())
    }

    pub(crate) fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), Error> {
        let needed = values
            .len()
            .checked_add(additional)
            .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?;
        if needed <= values.capacity() {
            return Ok(());
        }
        let capacity = needed.max(values.capacity().saturating_mul(2)).max(4);
        self.charge(
            capacity
                .checked_mul(size_of::<T>())
                .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
        )?;
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| Error::resource(ErrorKind::AllocationFailed))
    }

    fn exact<T>(&mut self, capacity: usize) -> Result<Vec<T>, Error> {
        self.charge(
            capacity
                .checked_mul(size_of::<T>())
                .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
        )?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(capacity)
            .map_err(|_| Error::resource(ErrorKind::AllocationFailed))?;
        Ok(values)
    }

    pub(crate) fn clone_string(&mut self, value: &NbtString) -> Result<NbtString, Error> {
        self.work(value.as_utf16().len())?;
        let mut units = self.exact(value.as_utf16().len())?;
        units.extend_from_slice(value.as_utf16());
        Ok(NbtString::from_utf16(units))
    }

    pub(crate) fn compare(
        &mut self,
        expected: Option<&Tag>,
        actual: Option<&Tag>,
        partial: bool,
    ) -> Result<bool, Error> {
        let mut remaining = self.remaining_allocation();
        let result = self
            .comparison
            .compare_accounted(expected, actual, partial, &mut remaining);
        self.allocated = self.limits.allocation_bytes - remaining;
        result.map_err(compare_error)
    }

    pub(crate) fn equal(&mut self, expected: &Tag, actual: &Tag) -> Result<bool, Error> {
        let mut remaining = self.remaining_allocation();
        let result = self
            .comparison
            .equal_accounted(expected, actual, &mut remaining);
        self.allocated = self.limits.allocation_bytes - remaining;
        result.map_err(compare_error)
    }

    /// Caller factory allocation occurs outside the library. Account retained
    /// ownership before attaching its result, without copying the returned tree.
    pub(crate) fn admit_owned(&mut self, value: &Tag) -> Result<(), Error> {
        let mut pending = Vec::new();
        self.reserve(&mut pending, 1)?;
        pending.push(value);
        while let Some(value) = pending.pop() {
            self.work(1)?;
            match value {
                Tag::String(value) => self.charge(
                    value
                        .0
                        .capacity()
                        .checked_mul(2)
                        .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
                )?,
                Tag::ByteArray(values) => self.charge(values.capacity())?,
                Tag::IntArray(values) => self.charge(
                    values
                        .capacity()
                        .checked_mul(4)
                        .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
                )?,
                Tag::LongArray(values) => self.charge(
                    values
                        .capacity()
                        .checked_mul(8)
                        .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
                )?,
                Tag::List(values) => {
                    self.charge(
                        values
                            .capacity()
                            .checked_mul(size_of::<Tag>())
                            .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
                    )?;
                    self.work(values.len())?;
                    self.reserve(&mut pending, values.len())?;
                    pending.extend(values.iter());
                }
                Tag::Compound(value) => {
                    self.charge(
                        value
                            .0
                            .capacity()
                            .checked_mul(size_of::<CompoundEntry>())
                            .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
                    )?;
                    self.work(value.entries().len())?;
                    self.reserve(&mut pending, value.entries().len())?;
                    for entry in value.entries() {
                        self.charge(
                            entry
                                .name
                                .0
                                .capacity()
                                .checked_mul(2)
                                .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
                        )?;
                        pending.push(&entry.value);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(crate) fn too_deep(&mut self, value: &Tag, depth: usize) -> Result<bool, Error> {
        let mut pending = Vec::new();
        self.reserve(&mut pending, 1)?;
        pending.push((value, depth));
        while let Some((value, depth)) = pending.pop() {
            self.work(1)?;
            if depth >= 512 {
                return Ok(true);
            }
            match value {
                Tag::List(values) => {
                    self.work(values.len())?;
                    self.reserve(&mut pending, values.len())?;
                    pending.extend(values.iter().map(|value| (value, depth + 1)));
                }
                Tag::Compound(value) => {
                    self.work(value.entries().len())?;
                    self.reserve(&mut pending, value.entries().len())?;
                    pending.extend(
                        value
                            .entries()
                            .iter()
                            .map(|entry| (&entry.value, depth + 1)),
                    );
                }
                _ => {}
            }
        }
        Ok(false)
    }

    pub(crate) fn clone_tag(&mut self, value: &Tag) -> Result<Tag, Error> {
        let mut pending = Vec::new();
        let mut completed: Vec<OwnedTag> = Vec::new();
        self.reserve(&mut pending, 1)?;
        pending.push(CloneStep::Visit(value));
        while let Some(step) = pending.pop() {
            self.work(1)?;
            let value = match step {
                CloneStep::Visit(value) => match value {
                    Tag::End => Tag::End,
                    Tag::Byte(value) => Tag::Byte(*value),
                    Tag::Short(value) => Tag::Short(*value),
                    Tag::Int(value) => Tag::Int(*value),
                    Tag::Long(value) => Tag::Long(*value),
                    Tag::Float(value) => Tag::Float(*value),
                    Tag::Double(value) => Tag::Double(*value),
                    Tag::String(value) => Tag::String(self.clone_string(value)?),
                    Tag::ByteArray(values) => {
                        self.work(values.len())?;
                        let mut copy = self.exact(values.len())?;
                        copy.extend_from_slice(values);
                        Tag::ByteArray(copy)
                    }
                    Tag::IntArray(values) => {
                        self.work(values.len())?;
                        let mut copy = self.exact(values.len())?;
                        copy.extend_from_slice(values);
                        Tag::IntArray(copy)
                    }
                    Tag::LongArray(values) => {
                        self.work(values.len())?;
                        let mut copy = self.exact(values.len())?;
                        copy.extend_from_slice(values);
                        Tag::LongArray(copy)
                    }
                    Tag::List(values) => {
                        self.work(values.len())?;
                        self.reserve(
                            &mut pending,
                            values
                                .len()
                                .checked_add(1)
                                .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
                        )?;
                        pending.push(CloneStep::List(values.len()));
                        pending.extend(values.iter().rev().map(CloneStep::Visit));
                        continue;
                    }
                    Tag::Compound(value) => {
                        self.work(value.entries().len())?;
                        self.reserve(
                            &mut pending,
                            value
                                .entries()
                                .len()
                                .checked_add(1)
                                .ok_or_else(|| Error::resource(ErrorKind::AllocationBudget))?,
                        )?;
                        pending.push(CloneStep::Compound(value.entries()));
                        pending.extend(
                            value
                                .entries()
                                .iter()
                                .rev()
                                .map(|entry| CloneStep::Visit(&entry.value)),
                        );
                        continue;
                    }
                },
                CloneStep::List(count) => {
                    self.work(count)?;
                    let mut children = self.exact(count)?;
                    children.extend(
                        completed
                            .drain(completed.len() - count..)
                            .map(|mut value| value.take()),
                    );
                    Tag::List(children)
                }
                CloneStep::Compound(entries) => {
                    self.work(entries.len())?;
                    let mut compound =
                        OwnedTag::new(Tag::Compound(Compound(self.exact(entries.len())?)));
                    for entry in entries.iter().rev() {
                        let name = self.clone_string(&entry.name)?;
                        let mut value = completed
                            .pop()
                            .expect("one completed child per source entry");
                        let Tag::Compound(children) = compound.as_tag_mut() else {
                            unreachable!()
                        };
                        children.0.push(CompoundEntry::new(name, value.take()));
                    }
                    // Source entries are already sorted and unique.
                    let Tag::Compound(children) = compound.as_tag_mut() else {
                        unreachable!()
                    };
                    children.0.reverse();
                    compound.take()
                }
            };
            let value = OwnedTag::new(value);
            self.reserve(&mut completed, 1)?;
            completed.push(value);
        }
        completed
            .pop()
            .map(|mut value| value.take())
            .ok_or_else(|| Error::resource(ErrorKind::InvalidPath))
    }

    pub(crate) fn expected_list_error(&mut self, value: &Tag) -> Error {
        match self.clone_tag(value) {
            Ok(value) => Error::operation(
                ErrorKind::ExpectedList,
                "commands.data.modify.expected_list",
                Argument::Tag(value),
            ),
            Err(error) => error,
        }
    }
}

enum CloneStep<'a> {
    Visit(&'a Tag),
    List(usize),
    Compound(&'a [CompoundEntry]),
}

fn compare_error(error: CompareError) -> Error {
    Error::resource(match error {
        CompareError::WorkLimit => ErrorKind::WorkLimit,
        CompareError::DepthLimit => ErrorKind::DepthLimit,
        CompareError::StackLimit | CompareError::AllocationLimit => ErrorKind::AllocationBudget,
        CompareError::AllocationFailed => ErrorKind::AllocationFailed,
    })
}
