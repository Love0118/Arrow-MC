//! Synchronous, breadth-first path traversal over disjoint owned NBT trees.

use super::budget::{OwnedTag, dispose_tag};
use super::{Argument, Budget, Error, ErrorKind, Limits, Node, Path, Selection, SelectionMut};
use crate::nbt::{Compound, CompoundEntry, NbtString, Tag};

impl Path {
    /// Returns live tree references; primitive-array elements are detached tags.
    pub fn get<'a>(&self, root: &'a Tag, limits: Limits) -> Result<Vec<Selection<'a>>, Error> {
        self.select(root, &mut Budget::new(limits), true)
    }

    pub fn count_matching(&self, root: &Tag, limits: Limits) -> Result<usize, Error> {
        Ok(self.select(root, &mut Budget::new(limits), false)?.len())
    }

    fn select<'a>(
        &self,
        root: &'a Tag,
        budget: &mut Budget,
        missing_error: bool,
    ) -> Result<Vec<Selection<'a>>, Error> {
        let mut current = Vec::new();
        push(&mut current, Selection::Borrowed(root), budget)?;
        let mut next = Vec::new();
        for (index, node) in self.nodes.iter().enumerate() {
            for candidate in current.drain(..) {
                budget.work(1)?;
                if let Selection::Borrowed(parent) = candidate {
                    read_node(node, parent, &mut next, budget)?;
                }
            }
            std::mem::swap(&mut current, &mut next);
            if current.is_empty() {
                return if missing_error {
                    Err(self.not_found(index))
                } else {
                    Ok(current)
                };
            }
        }
        Ok(current)
    }

    /// Creates only missing parents and values. Earlier changes survive errors.
    ///
    /// The factory owns allocation of its returned tag. That value is checked
    /// against the operation budget before attachment, but the library cannot
    /// reserve memory on the caller's behalf before calling the factory.
    /// Lists/compounds return live borrows; numeric arrays return detached tags.
    pub fn get_or_create<'a>(
        &self,
        root: &'a mut Tag,
        factory: &mut dyn FnMut() -> Tag,
        limits: Limits,
    ) -> Result<Vec<SelectionMut<'a>>, Error> {
        self.create(root, factory, &mut Budget::new(limits))
    }

    fn create<'a>(
        &self,
        root: &'a mut Tag,
        factory: &mut dyn FnMut() -> Tag,
        budget: &mut Budget,
    ) -> Result<Vec<SelectionMut<'a>>, Error> {
        let node = self.last_node()?;
        let parents = self.parents(root, true, budget)?;
        let mut output = Vec::new();
        let mut creation = Creation::Factory(factory);
        for parent in parents {
            budget.work(1)?;
            if let SelectionMut::Borrowed(parent) = parent {
                mutable_node(node, parent, &mut output, &mut creation, budget)?;
            }
        }
        Ok(output)
    }

    fn parents<'a>(
        &self,
        root: &'a mut Tag,
        create: bool,
        budget: &mut Budget,
    ) -> Result<Vec<SelectionMut<'a>>, Error> {
        self.last_node()?;
        let mut current = Vec::new();
        push(&mut current, SelectionMut::Borrowed(root), budget)?;
        let mut next = Vec::new();
        for index in 0..self.nodes.len() - 1 {
            let mut creation = if create {
                Creation::Preferred(&self.nodes[index + 1])
            } else {
                Creation::Disabled
            };
            for candidate in current.drain(..) {
                budget.work(1)?;
                if let SelectionMut::Borrowed(parent) = candidate {
                    mutable_node(&self.nodes[index], parent, &mut next, &mut creation, budget)?;
                }
            }
            std::mem::swap(&mut current, &mut next);
            if current.is_empty() {
                return if create {
                    Err(self.not_found(index))
                } else {
                    Ok(current)
                };
            }
        }
        Ok(current)
    }

    /// Reports Vanilla's changed count, which can differ from final byte changes.
    /// Source depth/copy checks precede mutation; later failures retain changes.
    pub fn set(&self, root: &mut Tag, value: &Tag, limits: Limits) -> Result<i32, Error> {
        let node = self.last_node()?;
        let mut budget = Budget::new(limits);
        if budget.too_deep(value, self.nodes.len())? {
            return Err(too_deep());
        }
        let mut copies = Copies {
            source: value,
            first: Some(OwnedTag::new(budget.clone_tag(value)?)),
        };
        let parents = self.parents(root, true, &mut budget)?;
        let mut changed = 0_i32;
        for mut parent in parents {
            budget.work(1)?;
            changed = changed.wrapping_add(set_node(
                node,
                parent.as_tag_mut(),
                &mut copies,
                &mut budget,
            )?);
        }
        Ok(changed)
    }

    /// Inserts sources in order and counts modified collections, not elements.
    /// Failure at a later target preserves successful earlier insertions.
    pub fn insert(
        &self,
        root: &mut Tag,
        index: i32,
        sources: &[Tag],
        limits: Limits,
    ) -> Result<i32, Error> {
        self.last_node()?;
        let mut budget = Budget::new(limits);
        let mut prepared = Vec::new();
        budget.reserve(&mut prepared, sources.len())?;
        // All source copies and depth checks precede any parent creation.
        for source in sources {
            let copy = OwnedTag::new(budget.clone_tag(source)?);
            if budget.too_deep(copy.as_tag(), self.nodes.len())? {
                return Err(too_deep());
            }
            prepared.push(copy);
        }
        let targets = self.create(root, &mut || Tag::List(Vec::new()), &mut budget)?;
        let mut changed = 0_i32;
        for (target_index, mut target) in targets.into_iter().enumerate() {
            budget.work(1)?;
            let target = target.as_tag_mut();
            let Some(length) = collection_len(target) else {
                return Err(budget.expected_list_error(target));
            };
            let mut at = if index < 0 {
                (length as i32).wrapping_add(index).wrapping_add(1)
            } else {
                index
            };
            let mut modified = false;
            for (source_index, source) in sources.iter().enumerate() {
                let value = if target_index == 0 {
                    OwnedTag::new(prepared[source_index].take())
                } else {
                    OwnedTag::new(budget.clone_tag(source)?)
                };
                if collection_add(target, at, value, &mut budget)? {
                    at = at.wrapping_add(1);
                    modified = true;
                }
            }
            changed = changed.wrapping_add(i32::from(modified));
        }
        Ok(changed)
    }

    pub fn remove(&self, root: &mut Tag, limits: Limits) -> Result<i32, Error> {
        let node = self.last_node()?;
        let mut budget = Budget::new(limits);
        let parents = self.parents(root, false, &mut budget)?;
        let mut changed = 0_i32;
        for mut parent in parents {
            budget.work(1)?;
            changed = changed.wrapping_add(remove_node(node, parent.as_tag_mut(), &mut budget)?);
        }
        Ok(changed)
    }

    fn last_node(&self) -> Result<&Node, Error> {
        self.nodes.last().ok_or_else(|| {
            Error::operation(
                ErrorKind::InvalidPath,
                "arguments.nbtpath.node.invalid",
                Argument::None,
            )
        })
    }
}

fn too_deep() -> Error {
    Error::operation(
        ErrorKind::TooDeep,
        "arguments.nbtpath.too_deep",
        Argument::None,
    )
}

fn push<T>(output: &mut Vec<T>, value: T, budget: &mut Budget) -> Result<(), Error> {
    budget.candidate()?;
    budget.reserve(output, 1)?;
    output.push(value);
    Ok(())
}

fn matches(pattern: &Tag, value: Option<&Tag>, budget: &mut Budget) -> Result<bool, Error> {
    budget.compare(Some(pattern), value, true)
}

fn read_node<'a>(
    node: &Node,
    parent: &'a Tag,
    output: &mut Vec<Selection<'a>>,
    budget: &mut Budget,
) -> Result<(), Error> {
    match node {
        Node::Child(name) | Node::MatchChild { name, .. } => {
            if let Tag::Compound(compound) = parent
                && let Ok(index) = child_index(compound, name, budget)?
                && match node {
                    Node::MatchChild { pattern, .. } => {
                        matches(pattern, Some(&compound.0[index].value), budget)?
                    }
                    _ => true,
                }
            {
                push(
                    output,
                    Selection::Borrowed(&compound.0[index].value),
                    budget,
                )?;
            }
        }
        Node::Index(index) => {
            if let Some(length) = collection_len(parent)
                && let Some(index) = existing_index(*index, length)
            {
                push(output, collection_at(parent, index), budget)?;
            }
        }
        Node::All => {
            if let Some(length) = collection_len(parent) {
                for index in 0..length {
                    budget.work(1)?;
                    push(output, collection_at(parent, index), budget)?;
                }
            }
        }
        Node::MatchElement(pattern) => {
            if let Tag::List(values) = parent {
                for value in values {
                    if matches(pattern, Some(value), budget)? {
                        push(output, Selection::Borrowed(value), budget)?;
                    }
                }
            }
        }
        Node::MatchRoot(pattern) => {
            if matches!(parent, Tag::Compound(_)) && matches(pattern, Some(parent), budget)? {
                push(output, Selection::Borrowed(parent), budget)?;
            }
        }
    }
    Ok(())
}

enum Creation<'a> {
    Disabled,
    Preferred(&'a Node),
    Factory(&'a mut dyn FnMut() -> Tag),
}

impl Creation<'_> {
    fn enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }

    fn supply(&mut self, budget: &mut Budget) -> Result<OwnedTag, Error> {
        let value = OwnedTag::new(match self {
            Self::Disabled => unreachable!("only missing-value creation requests a supplier"),
            Self::Preferred(node) => match node {
                Node::Child(_) | Node::MatchChild { .. } | Node::MatchRoot(_) => {
                    Tag::Compound(Compound::new())
                }
                Node::Index(_) | Node::All | Node::MatchElement(_) => Tag::List(Vec::new()),
            },
            Self::Factory(factory) => factory(),
        });
        budget.admit_owned(value.as_tag())?;
        Ok(value)
    }
}

fn mutable_node<'a>(
    node: &Node,
    parent: &'a mut Tag,
    output: &mut Vec<SelectionMut<'a>>,
    creation: &mut Creation<'_>,
    budget: &mut Budget,
) -> Result<(), Error> {
    match node {
        Node::Child(name) | Node::MatchChild { name, .. } => {
            if let Tag::Compound(compound) = parent {
                let index = match child_index(compound, name, budget)? {
                    Ok(index) => index,
                    Err(index) if creation.enabled() => {
                        let mut value = match node {
                            Node::MatchChild { pattern, .. } => {
                                OwnedTag::new(budget.clone_tag(pattern)?)
                            }
                            _ => creation.supply(budget)?,
                        };
                        let key = budget.clone_string(name)?;
                        budget.reserve(&mut compound.0, 1)?;
                        budget.work(compound.0.len() - index)?;
                        compound
                            .0
                            .insert(index, CompoundEntry::new(key, value.take()));
                        push(
                            output,
                            SelectionMut::Borrowed(&mut compound.0[index].value),
                            budget,
                        )?;
                        return Ok(());
                    }
                    Err(_) => return Ok(()),
                };
                let value = &mut compound.0[index].value;
                if match node {
                    Node::MatchChild { pattern, .. } => matches(pattern, Some(value), budget)?,
                    _ => true,
                } {
                    push(output, SelectionMut::Borrowed(value), budget)?;
                }
            }
        }
        Node::Index(index) => {
            if let Some(length) = collection_len(parent)
                && let Some(index) = existing_index(*index, length)
            {
                match parent {
                    Tag::List(values) => {
                        push(output, SelectionMut::Borrowed(&mut values[index]), budget)?
                    }
                    _ => push(output, detached_element(parent, index), budget)?,
                }
            }
        }
        Node::All => {
            if collection_len(parent) == Some(0) && creation.enabled() {
                let mut value = creation.supply(budget)?;
                if let Tag::List(values) = parent {
                    budget.reserve(values, 1)?;
                    values.push(value.take());
                    push(output, SelectionMut::Borrowed(&mut values[0]), budget)?;
                } else {
                    // The returned object is the original supplier value, even
                    // when the array stores a narrowed number of another type.
                    let stored = numeric_copy(value.as_tag());
                    if let Some(stored) = stored
                        && collection_add(parent, 0, OwnedTag::new(stored), budget)?
                    {
                        push(output, SelectionMut::Detached(value.take()), budget)?;
                    }
                }
            } else if let Tag::List(values) = parent {
                for value in values {
                    budget.work(1)?;
                    push(output, SelectionMut::Borrowed(value), budget)?;
                }
            } else if let Some(length) = collection_len(parent) {
                for index in 0..length {
                    budget.work(1)?;
                    push(output, detached_element(parent, index), budget)?;
                }
            }
        }
        Node::MatchElement(pattern) => {
            if let Tag::List(values) = parent {
                // Find the first match before borrowing output elements so a
                // no-match append never needs aliasing or a second predicate scan.
                let mut first = None;
                for (index, value) in values.iter().enumerate() {
                    if matches(pattern, Some(value), budget)? {
                        first = Some(index);
                        break;
                    }
                }
                if let Some(first) = first {
                    for (offset, value) in values[first..].iter_mut().enumerate() {
                        if offset == 0 || matches(pattern, Some(value), budget)? {
                            push(output, SelectionMut::Borrowed(value), budget)?;
                        }
                    }
                } else if creation.enabled() {
                    let mut value = OwnedTag::new(budget.clone_tag(pattern)?);
                    budget.reserve(values, 1)?;
                    values.push(value.take());
                    push(
                        output,
                        SelectionMut::Borrowed(values.last_mut().expect("just appended")),
                        budget,
                    )?;
                }
            }
        }
        Node::MatchRoot(pattern) => {
            if matches!(parent, Tag::Compound(_)) && matches(pattern, Some(parent), budget)? {
                push(output, SelectionMut::Borrowed(parent), budget)?;
            }
        }
    }
    Ok(())
}

fn existing_index(index: i32, length: usize) -> Option<usize> {
    let index = if index < 0 {
        (length as i32).wrapping_add(index)
    } else {
        index
    };
    usize::try_from(index).ok().filter(|index| *index < length)
}

/// The inner result follows binary_search's found/insertion-position contract.
fn child_index(
    compound: &Compound,
    name: &NbtString,
    budget: &mut Budget,
) -> Result<Result<usize, usize>, Error> {
    let mut lower = 0;
    let mut upper = compound.0.len();
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        let key = &compound.0[middle].name;
        // Charge an upper bound for each lexicographic comparison, including
        // the final length comparison. Shared prefixes cannot hide lookup work.
        budget.work(
            key.as_utf16()
                .len()
                .min(name.as_utf16().len())
                .saturating_add(1),
        )?;
        match key.cmp(name) {
            std::cmp::Ordering::Less => lower = middle + 1,
            std::cmp::Ordering::Greater => upper = middle,
            std::cmp::Ordering::Equal => return Ok(Ok(middle)),
        }
    }
    Ok(Err(lower))
}

fn collection_len(value: &Tag) -> Option<usize> {
    match value {
        Tag::List(values) => Some(values.len()),
        Tag::ByteArray(values) => Some(values.len()),
        Tag::IntArray(values) => Some(values.len()),
        Tag::LongArray(values) => Some(values.len()),
        _ => None,
    }
}

fn collection_at(value: &Tag, index: usize) -> Selection<'_> {
    match value {
        Tag::List(values) => Selection::Borrowed(&values[index]),
        Tag::ByteArray(values) => Selection::Detached(Tag::Byte(values[index])),
        Tag::IntArray(values) => Selection::Detached(Tag::Int(values[index])),
        Tag::LongArray(values) => Selection::Detached(Tag::Long(values[index])),
        _ => unreachable!("caller checks collection type"),
    }
}

fn detached_element(value: &Tag, index: usize) -> SelectionMut<'_> {
    match collection_at(value, index) {
        Selection::Detached(value) => SelectionMut::Detached(value),
        Selection::Borrowed(_) => unreachable!("list elements use live mutable borrows"),
    }
}

fn numeric_copy(value: &Tag) -> Option<Tag> {
    Some(match value {
        Tag::Byte(value) => Tag::Byte(*value),
        Tag::Short(value) => Tag::Short(*value),
        Tag::Int(value) => Tag::Int(*value),
        Tag::Long(value) => Tag::Long(*value),
        Tag::Float(value) => Tag::Float(*value),
        Tag::Double(value) => Tag::Double(*value),
        _ => return None,
    })
}

struct Copies<'a> {
    source: &'a Tag,
    first: Option<OwnedTag>,
}

impl Copies<'_> {
    fn next(&mut self, budget: &mut Budget) -> Result<OwnedTag, Error> {
        match self.first.take() {
            Some(value) => Ok(value),
            None => Ok(OwnedTag::new(budget.clone_tag(self.source)?)),
        }
    }
}

fn set_node(
    node: &Node,
    parent: &mut Tag,
    copies: &mut Copies<'_>,
    budget: &mut Budget,
) -> Result<i32, Error> {
    match node {
        Node::Child(name) | Node::MatchChild { name, .. } => {
            if let Tag::Compound(compound) = parent {
                let found = child_index(compound, name, budget)?;
                let previous = found.ok().map(|index| &compound.0[index].value);
                if let Node::MatchChild { pattern, .. } = node
                    && !matches(pattern, previous, budget)?
                {
                    return Ok(0);
                }
                let mut value = copies.next(budget)?;
                let equal = if let Some(previous) = previous {
                    budget.equal(value.as_tag(), previous)?
                } else {
                    false
                };
                if equal && matches!(node, Node::MatchChild { .. }) {
                    return Ok(0);
                }
                match found {
                    Ok(index) => replace_value(&mut compound.0[index].value, value),
                    Err(index) => {
                        let key = budget.clone_string(name)?;
                        budget.reserve(&mut compound.0, 1)?;
                        budget.work(compound.0.len() - index)?;
                        compound
                            .0
                            .insert(index, CompoundEntry::new(key, value.take()));
                    }
                }
                return Ok(i32::from(!equal));
            }
        }
        Node::Index(index) => {
            if let Some(length) = collection_len(parent)
                && let Some(index) = existing_index(*index, length)
            {
                let value = copies.next(budget)?;
                if !budget.equal(value.as_tag(), collection_at(parent, index).as_tag())?
                    && collection_set(parent, index, value)
                {
                    return Ok(1);
                }
            }
        }
        Node::All => {
            if let Some(length) = collection_len(parent) {
                let value = copies.next(budget)?;
                if length == 0 {
                    collection_add(parent, 0, value, budget)?;
                    return Ok(1);
                }
                let mut changed = 0_i32;
                for index in 0..length {
                    if !budget.equal(value.as_tag(), collection_at(parent, index).as_tag())? {
                        changed = changed.wrapping_add(1);
                    }
                }
                if changed == 0 {
                    return Ok(0);
                }
                collection_clear(parent);
                if !collection_add(parent, 0, value, budget)? {
                    return Ok(0);
                }
                for index in 1..length {
                    let value = copies.next(budget)?;
                    collection_add(parent, index as i32, value, budget)?;
                }
                return Ok(changed);
            }
        }
        Node::MatchElement(pattern) => {
            if let Tag::List(values) = parent {
                if values.is_empty() {
                    let mut value = copies.next(budget)?;
                    budget.reserve(values, 1)?;
                    values.push(value.take());
                    return Ok(1);
                }
                let mut changed = 0_i32;
                for current in values {
                    if matches(pattern, Some(current), budget)? {
                        let value = copies.next(budget)?;
                        if !budget.equal(value.as_tag(), current)? {
                            replace_value(current, value);
                            changed = changed.wrapping_add(1);
                        }
                    }
                }
                return Ok(changed);
            }
        }
        Node::MatchRoot(_) => {}
    }
    Ok(0)
}

fn replace_value(target: &mut Tag, mut value: OwnedTag) {
    dispose_tag(std::mem::replace(target, value.take()));
}

fn collection_set(target: &mut Tag, index: usize, value: OwnedTag) -> bool {
    match target {
        Tag::List(values) => replace_value(&mut values[index], value),
        Tag::ByteArray(values) => match value.as_tag().as_byte() {
            Some(value) => values[index] = value,
            None => return false,
        },
        Tag::IntArray(values) => match value.as_tag().as_int() {
            Some(value) => values[index] = value,
            None => return false,
        },
        Tag::LongArray(values) => match value.as_tag().as_long() {
            Some(value) => values[index] = value,
            None => return false,
        },
        _ => return false,
    }
    true
}

fn checked_insert_index(index: i32, length: usize) -> Result<usize, Error> {
    usize::try_from(index)
        .ok()
        .filter(|index| *index <= length)
        .ok_or_else(|| {
            Error::operation(
                ErrorKind::InvalidIndex,
                "commands.data.modify.invalid_index",
                Argument::Index(index),
            )
        })
}

fn collection_add(
    target: &mut Tag,
    index: i32,
    mut value: OwnedTag,
    budget: &mut Budget,
) -> Result<bool, Error> {
    match target {
        Tag::List(values) => {
            let index = checked_insert_index(index, values.len())?;
            budget.work(values.len() - index + 1)?;
            budget.reserve(values, 1)?;
            values.insert(index, value.take());
        }
        Tag::ByteArray(values) => {
            let Some(value) = value.as_tag().as_byte() else {
                return Ok(false);
            };
            let index = checked_insert_index(index, values.len())?;
            budget.work(values.len() - index + 1)?;
            budget.reserve(values, 1)?;
            values.insert(index, value);
        }
        Tag::IntArray(values) => {
            let Some(value) = value.as_tag().as_int() else {
                return Ok(false);
            };
            let index = checked_insert_index(index, values.len())?;
            budget.work(values.len() - index + 1)?;
            budget.reserve(values, 1)?;
            values.insert(index, value);
        }
        Tag::LongArray(values) => {
            let Some(value) = value.as_tag().as_long() else {
                return Ok(false);
            };
            let index = checked_insert_index(index, values.len())?;
            budget.work(values.len() - index + 1)?;
            budget.reserve(values, 1)?;
            values.insert(index, value);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn collection_clear(value: &mut Tag) {
    match value {
        Tag::List(values) => values.drain(..).for_each(dispose_tag),
        Tag::ByteArray(values) => values.clear(),
        Tag::IntArray(values) => values.clear(),
        Tag::LongArray(values) => values.clear(),
        _ => {}
    }
}

fn remove_node(node: &Node, parent: &mut Tag, budget: &mut Budget) -> Result<i32, Error> {
    match node {
        Node::Child(name) | Node::MatchChild { name, .. } => {
            if let Tag::Compound(compound) = parent
                && let Ok(index) = child_index(compound, name, budget)?
            {
                if let Node::MatchChild { pattern, .. } = node
                    && !matches(pattern, Some(&compound.0[index].value), budget)?
                {
                    return Ok(0);
                }
                budget.work(compound.0.len() - index)?;
                dispose_tag(compound.0.remove(index).value);
                return Ok(1);
            }
        }
        Node::Index(index) => {
            if let Some(length) = collection_len(parent)
                && let Some(index) = existing_index(*index, length)
            {
                budget.work(length - index)?;
                match parent {
                    Tag::List(values) => {
                        dispose_tag(values.remove(index));
                    }
                    Tag::ByteArray(values) => {
                        values.remove(index);
                    }
                    Tag::IntArray(values) => {
                        values.remove(index);
                    }
                    Tag::LongArray(values) => {
                        values.remove(index);
                    }
                    _ => unreachable!("validated collection"),
                }
                return Ok(1);
            }
        }
        Node::All => {
            if let Some(length) = collection_len(parent) {
                budget.work(length)?;
                collection_clear(parent);
                return Ok(length as i32);
            }
        }
        Node::MatchElement(pattern) => {
            if let Tag::List(values) = parent {
                let mut changed = 0_i32;
                // Backward removal preserves predicate/error traversal order.
                for index in (0..values.len()).rev() {
                    if matches(pattern, Some(&values[index]), budget)? {
                        budget.work(values.len() - index)?;
                        dispose_tag(values.remove(index));
                        changed = changed.wrapping_add(1);
                    }
                }
                return Ok(changed);
            }
        }
        Node::MatchRoot(_) => {}
    }
    Ok(0)
}
