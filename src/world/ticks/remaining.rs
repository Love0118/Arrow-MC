//! Counts for remaining selected ticks, with dense distinct-identity iteration.
//! Rebuilding the externally observed lazy set never scans empty hash buckets or
//! repeated tick identities. These counts do not replace that set's semantics.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

use super::{Identity, TickError, reserved_vec};

struct Entry {
    identity: Identity,
    count: usize,
}

pub(super) struct RemainingCounts {
    entries: Vec<Entry>,
    lookup: Vec<usize>,
    hash: RandomState,
    limit: usize,
}

impl RemainingCounts {
    pub(super) fn new(capacity: usize, remaining: &mut usize) -> Result<Self, TickError> {
        if capacity == 0 {
            return Err(TickError::InvalidLimits);
        }
        let slots = capacity
            .checked_next_power_of_two()
            .and_then(|capacity| capacity.checked_mul(2))
            .ok_or(TickError::InvalidLimits)?;
        let requested = capacity
            .checked_mul(size_of::<Entry>())
            .and_then(|entries| {
                slots
                    .checked_mul(size_of::<usize>())
                    .and_then(|lookup| entries.checked_add(lookup))
            })
            .ok_or(TickError::InvalidLimits)?;
        if requested > *remaining {
            return Err(TickError::AllocationBudget);
        }
        let mut budget = *remaining;
        let entries = reserved_vec(capacity, &mut budget)?;
        let mut lookup = reserved_vec(slots, &mut budget)?;
        lookup.resize(slots, usize::MAX);
        *remaining = budget;
        Ok(Self {
            entries,
            lookup,
            hash: RandomState::new(),
            limit: capacity,
        })
    }

    fn bucket(&self, identity: Identity) -> usize {
        self.hash.hash_one(identity) as usize & (self.lookup.len() - 1)
    }

    fn find(&self, identity: Identity) -> Result<usize, usize> {
        let mut bucket = self.bucket(identity);
        loop {
            let index = self.lookup[bucket];
            if index == usize::MAX {
                return Err(bucket);
            }
            if self.entries[index].identity == identity {
                return Ok(bucket);
            }
            bucket = (bucket + 1) & (self.lookup.len() - 1);
        }
    }

    pub(super) fn add(&mut self, identity: Identity) {
        match self.find(identity) {
            Ok(bucket) => {
                let entry = &mut self.entries[self.lookup[bucket]];
                entry.count = entry
                    .count
                    .checked_add(1)
                    .expect("remaining tick count overflow");
            }
            Err(bucket) => {
                assert!(
                    self.entries.len() < self.limit,
                    "selected tick admission must precede counting"
                );
                self.lookup[bucket] = self.entries.len();
                self.entries.push(Entry { identity, count: 1 });
            }
        }
    }

    pub(super) fn remove(&mut self, identity: Identity) {
        let Ok(bucket) = self.find(identity) else {
            return;
        };
        let index = self.lookup[bucket];
        if self.entries[index].count > 1 {
            self.entries[index].count -= 1;
            return;
        }

        // Repair probe paths while every old dense index is still valid.
        self.lookup[bucket] = usize::MAX;
        let mask = self.lookup.len() - 1;
        let mut hole = bucket;
        let mut scan = (bucket + 1) & mask;
        while self.lookup[scan] != usize::MAX {
            let home = self.bucket(self.entries[self.lookup[scan]].identity);
            if (hole.wrapping_sub(home) & mask) < (scan.wrapping_sub(home) & mask) {
                self.lookup[hole] = self.lookup[scan];
                self.lookup[scan] = usize::MAX;
                hole = scan;
            }
            scan = (scan + 1) & mask;
        }

        let last = self.entries.len() - 1;
        let moved_bucket = if index != last {
            Some(
                self.find(self.entries[last].identity)
                    .expect("last dense entry is indexed"),
            )
        } else {
            None
        };
        self.entries.swap_remove(index);
        if let Some(bucket) = moved_bucket {
            self.lookup[bucket] = index;
        }
    }

    pub(super) fn identities(&self) -> impl ExactSizeIterator<Item = Identity> + '_ {
        self.entries.iter().map(|entry| entry.identity)
    }

    pub(super) fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.entries.clear();
            self.lookup.fill(usize::MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::ticks::TickPosition;
    use std::collections::HashMap;

    fn identity(value: i32) -> Identity {
        Identity {
            position: TickPosition {
                x: value,
                y: value.wrapping_mul(7),
                z: -value,
            },
            type_id: value as u32 % 13,
        }
    }

    fn counts(capacity: usize) -> RemainingCounts {
        RemainingCounts::new(capacity, &mut (1024 * 1024)).unwrap()
    }

    fn assert_contents(actual: &RemainingCounts, expected: &HashMap<Identity, usize>) {
        assert_eq!(actual.identities().len(), expected.len());
        for entry in &actual.entries {
            assert_eq!(Some(&entry.count), expected.get(&entry.identity));
        }
        for (&identity, &count) in expected {
            let bucket = actual.find(identity).unwrap();
            assert_eq!(actual.entries[actual.lookup[bucket]].count, count);
        }
        for (index, entry) in actual.entries.iter().enumerate() {
            let bucket = actual.find(entry.identity).unwrap();
            assert_eq!(actual.lookup[bucket], index);
        }
        assert_eq!(
            actual
                .lookup
                .iter()
                .filter(|&&index| index != usize::MAX)
                .count(),
            expected.len()
        );
    }

    #[test]
    fn repeated_identities_remain_dense_until_their_last_tick_is_removed() {
        let mut counts = counts(8);
        for _ in 0..100 {
            counts.add(identity(1));
            counts.add(identity(2));
        }
        for _ in 0..99 {
            counts.remove(identity(1));
            assert_eq!(counts.identities().len(), 2);
            counts.remove(identity(2));
            assert_eq!(counts.identities().len(), 2);
        }
        counts.remove(identity(1));
        assert_eq!(counts.identities().collect::<Vec<_>>(), [identity(2)]);
        counts.remove(identity(1));
        counts.remove(identity(2));
        assert_eq!(counts.identities().len(), 0);
        counts.remove(identity(2));
        assert_eq!(counts.identities().len(), 0);
    }

    #[test]
    fn wrapped_collision_clusters_and_dense_moves_stay_indexed_through_reuse() {
        let mut counts = counts(16);
        let target = counts.lookup.len() - 1;
        let mut colliding = Vec::new();
        for value in 0..100_000 {
            let identity = identity(value);
            if counts.bucket(identity) == target {
                colliding.push(identity);
            }
            if colliding.len() == 16 {
                break;
            }
        }
        assert_eq!(
            colliding.len(),
            16,
            "keyed collision fixture search exhausted"
        );
        let mut expected = HashMap::new();
        for round in 0..8 {
            for (index, &identity) in colliding.iter().enumerate() {
                let repeats = 1 + (index + round) % 3;
                for _ in 0..repeats {
                    counts.add(identity);
                }
                expected.insert(identity, repeats);
            }
            assert_contents(&counts, &expected);
            for index in [0, 7, 15, 1, 14, 6, 2, 13, 8, 12, 3, 11, 9, 4, 10, 5] {
                let identity = colliding[index];
                let repeats = expected[&identity];
                for remaining in (0..repeats).rev() {
                    counts.remove(identity);
                    if remaining == 0 {
                        expected.remove(&identity);
                    } else {
                        expected.insert(identity, remaining);
                    }
                    assert_contents(&counts, &expected);
                }
            }
        }
    }

    #[test]
    fn mixed_add_remove_and_clear_match_reference_counts_without_capacity_growth() {
        let mut counts = counts(64);
        let capacities = (counts.entries.capacity(), counts.lookup.capacity());
        let mut expected = HashMap::new();
        for step in 0..4096 {
            let identity = identity((step * 37 + step / 11) % 64);
            if step % 5 < 3 {
                counts.add(identity);
                *expected.entry(identity).or_insert(0) += 1;
            } else {
                counts.remove(identity);
                if let Some(value) = expected.get_mut(&identity) {
                    *value -= 1;
                    if *value == 0 {
                        expected.remove(&identity);
                    }
                }
            }
            if step % 127 == 0 {
                counts.clear();
                expected.clear();
            }
            assert_contents(&counts, &expected);
            assert_eq!(
                (counts.entries.capacity(), counts.lookup.capacity()),
                capacities
            );
        }
        counts.clear();
        assert!(counts.lookup.iter().all(|&index| index == usize::MAX));
        for value in 0..64 {
            counts.add(identity(value));
        }
        assert_eq!(counts.identities().len(), 64);
    }

    #[test]
    fn constructor_preflights_both_arrays_and_only_commits_successful_budget() {
        let mut budget = 1024 * 1024;
        let counts = RemainingCounts::new(16, &mut budget).unwrap();
        let bytes = counts.entries.capacity() * size_of::<Entry>()
            + counts.lookup.capacity() * size_of::<usize>();
        assert_eq!(1024 * 1024 - budget, bytes);
        assert_eq!(counts.lookup.len(), 32);
        assert!(bytes >= 16 * (size_of::<Entry>() + 2 * size_of::<usize>()));
        let mut too_short = bytes - 1;
        assert!(matches!(
            RemainingCounts::new(16, &mut too_short),
            Err(TickError::AllocationBudget)
        ));
        assert_eq!(too_short, bytes - 1);
        let mut exact = bytes;
        RemainingCounts::new(16, &mut exact).unwrap();
        assert_eq!(exact, 0);
        for invalid in [0, usize::MAX] {
            let mut budget = usize::MAX;
            assert!(matches!(
                RemainingCounts::new(invalid, &mut budget),
                Err(TickError::InvalidLimits)
            ));
            assert_eq!(budget, usize::MAX);
        }
    }

    #[test]
    fn distinct_capacity_guard_prevents_unbudgeted_growth_before_mutating_lookup() {
        let mut counts = counts(2);
        counts.add(identity(1));
        counts.add(identity(2));
        let failed =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| counts.add(identity(3))));
        assert!(failed.is_err());
        assert_eq!(counts.identities().len(), 2);
        assert!(counts.find(identity(3)).is_err());
        counts.remove(identity(1));
        counts.add(identity(3));
        assert_contents(
            &counts,
            &HashMap::from([(identity(2), 1), (identity(3), 1)]),
        );
    }
}
