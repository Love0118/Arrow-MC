//! Reusable bounded storage for the concrete block/sky propagation entries.

use std::{collections::hash_map::RandomState, fmt, hash::BuildHasher};

use super::LightBlock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueError {
    InvalidCapacity,
    AllocationLimit,
    AllocationFailed,
    Full,
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "light work queue: {self:?}")
    }
}
impl std::error::Error for QueueError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub pos: LightBlock,
    pub level: u8,
    pub directions: u8,
    pub empty_shape: bool,
    pub from_emission: bool,
}

const EMPTY: Entry = Entry {
    pos: LightBlock { x: 0, y: 0, z: 0 },
    level: 0,
    directions: 0,
    empty_shape: true,
    from_emission: false,
};

pub struct WorkQueue {
    entries: Vec<Entry>,
    start: usize,
    len: usize,
}

impl WorkQueue {
    pub fn new(capacity: usize, remaining: &mut usize) -> Result<Self, QueueError> {
        if capacity == 0 {
            return Err(QueueError::InvalidCapacity);
        }
        let mut entries = reserve(capacity, remaining)?;
        entries.resize(capacity, EMPTY);
        Ok(Self {
            entries,
            start: 0,
            len: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn remaining_capacity(&self) -> usize {
        self.entries.len() - self.len
    }
    pub fn heap_bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Entry>()
    }
    pub fn peek(&self) -> Option<Entry> {
        (self.len != 0).then(|| self.entries[self.start])
    }

    pub fn push(&mut self, entry: Entry) -> Result<(), QueueError> {
        if self.remaining_capacity() == 0 {
            return Err(QueueError::Full);
        }
        let tail = (self.start + self.len) % self.entries.len();
        self.entries[tail] = entry;
        self.len += 1;
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Entry> {
        let entry = self.peek()?;
        self.start = (self.start + 1) % self.entries.len();
        self.len -= 1;
        Some(entry)
    }

    /// The old allocation remains charged until the replacement is allocated
    /// and populated. Failure preserves both queue order and caller budget.
    pub fn grow(&mut self, capacity: usize, remaining: &mut usize) -> Result<(), QueueError> {
        if capacity <= self.entries.len() {
            return Ok(());
        }
        let mut budget = *remaining;
        let mut replacement = Self::new(capacity, &mut budget)?;
        for offset in 0..self.len {
            replacement.entries[offset] = self.entries[(self.start + offset) % self.entries.len()];
        }
        replacement.len = self.len;
        let next_budget = budget
            .checked_add(self.heap_bytes())
            .ok_or(QueueError::AllocationLimit)?;
        let old = std::mem::replace(self, replacement);
        drop(old);
        *remaining = next_budget;
        Ok(())
    }
}

/// Deduplicates the current check batch only. Pending insertion order is an
/// internal choice; publication follows complete decrease/increase convergence.
/// No new checks may be inserted while a run consumes this immutable batch.
pub struct CheckQueue {
    positions: Vec<LightBlock>,
    slots: Vec<usize>,
    hash: RandomState,
    limit: usize,
    cursor: usize,
}

impl CheckQueue {
    pub fn new(capacity: usize, remaining: &mut usize) -> Result<Self, QueueError> {
        if capacity == 0 {
            return Err(QueueError::InvalidCapacity);
        }
        let count = capacity
            .checked_next_power_of_two()
            .and_then(|v| v.checked_mul(2))
            .ok_or(QueueError::InvalidCapacity)?;
        let mut budget = *remaining;
        let positions = reserve(capacity, &mut budget)?;
        let mut slots = reserve(count, &mut budget)?;
        slots.resize(count, usize::MAX);
        *remaining = budget;
        Ok(Self {
            positions,
            slots,
            hash: RandomState::new(),
            limit: capacity,
            cursor: 0,
        })
    }

    fn slot(&self, pos: LightBlock) -> Result<usize, usize> {
        let mut slot = self.hash.hash_one((pos.x, pos.y, pos.z)) as usize & (self.slots.len() - 1);
        loop {
            let index = self.slots[slot];
            if index == usize::MAX {
                return Err(slot);
            }
            if self.positions[index] == pos {
                return Ok(slot);
            }
            slot = (slot + 1) & (self.slots.len() - 1);
        }
    }

    pub fn insert(&mut self, pos: LightBlock) -> Result<bool, QueueError> {
        let Err(slot) = self.slot(pos) else {
            return Ok(false);
        };
        if self.positions.len() == self.limit {
            return Err(QueueError::Full);
        }
        self.slots[slot] = self.positions.len();
        self.positions.push(pos);
        Ok(true)
    }

    pub fn peek(&self) -> Option<LightBlock> {
        self.positions.get(self.cursor).copied()
    }
    pub fn pop(&mut self) -> Option<LightBlock> {
        let pos = self.peek()?;
        self.cursor += 1;
        Some(pos)
    }
    pub fn is_empty(&self) -> bool {
        self.cursor == self.positions.len()
    }
    pub fn heap_bytes(&self) -> usize {
        self.positions.capacity() * size_of::<LightBlock>()
            + self.slots.capacity() * size_of::<usize>()
    }
    pub fn clear(&mut self) {
        if !self.positions.is_empty() {
            self.positions.clear();
            self.slots.fill(usize::MAX);
        }
        self.cursor = 0;
    }

    pub fn grow(&mut self, capacity: usize, remaining: &mut usize) -> Result<(), QueueError> {
        if capacity <= self.limit {
            return Ok(());
        }
        let mut budget = *remaining;
        let mut replacement = Self::new(capacity, &mut budget)?;
        for &pos in &self.positions {
            replacement
                .insert(pos)
                .expect("replacement fits original checks");
        }
        replacement.cursor = self.cursor;
        let next_budget = budget
            .checked_add(self.heap_bytes())
            .ok_or(QueueError::AllocationLimit)?;
        let old = std::mem::replace(self, replacement);
        drop(old);
        *remaining = next_budget;
        Ok(())
    }
}

fn reserve<T>(capacity: usize, remaining: &mut usize) -> Result<Vec<T>, QueueError> {
    let bytes = capacity
        .checked_mul(size_of::<T>())
        .ok_or(QueueError::InvalidCapacity)?;
    if bytes > *remaining {
        return Err(QueueError::AllocationLimit);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| QueueError::AllocationFailed)?;
    let actual = output
        .capacity()
        .checked_mul(size_of::<T>())
        .ok_or(QueueError::AllocationLimit)?;
    if actual > *remaining {
        return Err(QueueError::AllocationLimit);
    }
    *remaining -= actual;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(x: i32) -> Entry {
        Entry {
            pos: LightBlock { x, y: 0, z: 0 },
            ..EMPTY
        }
    }

    #[test]
    fn wrapped_fifo_growth_retains_order_and_accounts_old_plus_new() {
        let mut budget = 4096;
        let mut queue = WorkQueue::new(3, &mut budget).unwrap();
        for x in 0..3 {
            queue.push(entry(x)).unwrap();
        }
        assert_eq!(queue.push(entry(3)), Err(QueueError::Full));
        assert_eq!(queue.pop(), Some(entry(0)));
        assert_eq!(queue.pop(), Some(entry(1)));
        for x in 3..5 {
            queue.push(entry(x)).unwrap();
        }
        let old_bytes = queue.heap_bytes();
        let before = budget;
        let mut short = 6 * size_of::<Entry>() - 1;
        assert_eq!(queue.grow(6, &mut short), Err(QueueError::AllocationLimit));
        assert_eq!(short, 6 * size_of::<Entry>() - 1);
        queue.grow(6, &mut budget).unwrap();
        assert_eq!(budget, before - queue.heap_bytes() + old_bytes);
        for x in 2..5 {
            assert_eq!(queue.pop(), Some(entry(x)));
        }
        assert!(queue.is_empty());
        for round in 0..128 {
            for x in 0..6 {
                queue.push(entry(round + x)).unwrap();
            }
            for x in 0..6 {
                assert_eq!(queue.pop(), Some(entry(round + x)));
            }
        }
    }

    #[test]
    fn check_dedup_is_only_for_the_pending_batch_and_growth_preserves_cursor() {
        let mut budget = 4096;
        let mut checks = CheckQueue::new(2, &mut budget).unwrap();
        assert!(checks.insert(entry(1).pos).unwrap());
        assert!(checks.insert(entry(2).pos).unwrap());
        assert!(!checks.insert(entry(1).pos).unwrap());
        assert_eq!(checks.insert(entry(3).pos), Err(QueueError::Full));
        assert_eq!(checks.pop(), Some(entry(1).pos));
        checks.grow(4, &mut budget).unwrap();
        assert_eq!(checks.pop(), Some(entry(2).pos));
        assert!(checks.is_empty());
        checks.clear();
        assert!(checks.insert(entry(1).pos).unwrap());
        assert_eq!(checks.pop(), Some(entry(1).pos));
        assert_eq!(4096 - budget, checks.heap_bytes());
    }

    #[test]
    fn failed_check_allocation_does_not_charge_the_first_array() {
        let mut budget = 16 * size_of::<LightBlock>();
        assert!(matches!(
            CheckQueue::new(16, &mut budget),
            Err(QueueError::AllocationLimit)
        ));
        assert_eq!(budget, 16 * size_of::<LightBlock>());
    }
}
