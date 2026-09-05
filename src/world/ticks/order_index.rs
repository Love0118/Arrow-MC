//! Scheduling-map history affects the input order of equal-priority ready ticks.
//!
//! This bounded representation was designed from the local 26.3-pre-2 ordering
//! contract and independently authored JVM probes of bundled fastutil 8.5.18.
//! It preserves the observed placement and traversal rules; no upstream method
//! bodies are included. Physical allocation does not set the logical table size.

use super::{TickError, reserved_vec};

const INITIAL_CAPACITY: usize = 32;
const MAX_CAPACITY: usize = 1 << 30;

#[derive(Clone, Copy, Default)]
struct Slot {
    // Zero denotes an empty bucket. The actual zero key is stored separately.
    key: i64,
    time: i64,
}

#[derive(Clone, Copy)]
enum Current {
    Zero,
    Bucket(usize),
    Wrapped(i64),
}

struct Scan {
    remaining: usize,
    before: usize,
    zero_pending: bool,
    wrapped_next: usize,
    current: Option<Current>,
}

pub(super) struct SchedulingIndex {
    slots: Vec<Slot>,
    rehash_scratch: Vec<Slot>,
    wrapped: Vec<i64>,
    capacity: usize,
    max_keys: usize,
    len: usize,
    zero: Option<i64>,
    scan: Option<Scan>,
}

impl SchedulingIndex {
    pub(super) fn new(max_keys: usize, allocation_limit: usize) -> Result<Self, TickError> {
        if max_keys > max_fill(MAX_CAPACITY) {
            return Err(TickError::InvalidLimits);
        }
        let mut physical_capacity = INITIAL_CAPACITY;
        while max_keys > max_fill(physical_capacity) {
            physical_capacity *= 2;
        }
        let requested = physical_capacity
            .checked_mul(size_of::<Slot>() * 2)
            .and_then(|tables| {
                max_keys
                    .checked_mul(size_of::<i64>())
                    .and_then(|wrapped| tables.checked_add(wrapped))
            })
            .ok_or(TickError::InvalidLimits)?;
        if requested > allocation_limit {
            return Err(TickError::AllocationBudget);
        }
        let mut remaining = allocation_limit;
        let mut slots = reserved_vec(physical_capacity, &mut remaining)?;
        let mut rehash_scratch = reserved_vec(physical_capacity, &mut remaining)?;
        let wrapped = reserved_vec(max_keys, &mut remaining)?;
        slots.resize(physical_capacity, Slot::default());
        rehash_scratch.resize(physical_capacity, Slot::default());
        Ok(Self {
            slots,
            rehash_scratch,
            wrapped,
            capacity: INITIAL_CAPACITY,
            max_keys,
            len: 0,
            zero: None,
            scan: None,
        })
    }

    /// Both physical tables and the retained wrapped-key scratch are charged,
    /// including capacity beyond the current logical table or scratch length.
    pub(super) fn heap_bytes(&self) -> usize {
        (self.slots.capacity() + self.rehash_scratch.capacity()) * size_of::<Slot>()
            + self.wrapped.capacity() * size_of::<i64>()
    }

    pub(super) fn get(&self, key: i64) -> Option<i64> {
        if key == 0 {
            return self.zero;
        }
        let index = self.find_bucket(key);
        (self.slots[index].key == key).then_some(self.slots[index].time)
    }

    /// Existing values may be changed during a scan. Structural insertion must
    /// occur outside it; a scan owns the map's traversal history until finished.
    pub(super) fn put(&mut self, key: i64, time: i64) -> Result<(), TickError> {
        let index = if key == 0 {
            if self.zero.is_some() {
                self.zero = Some(time);
                return Ok(());
            }
            None
        } else {
            let index = self.find_bucket(key);
            if self.slots[index].key == key {
                self.slots[index].time = time;
                return Ok(());
            }
            Some(index)
        };
        assert!(self.scan.is_none(), "insertion during a scheduling scan");
        if self.len == self.max_keys {
            return Err(TickError::ChunkLimit);
        }
        match index {
            Some(index) => self.slots[index] = Slot { key, time },
            None => self.zero = Some(time),
        }
        self.len += 1;
        if self.len > max_fill(self.capacity) {
            self.rehash(self.capacity * 2);
        }
        Ok(())
    }

    pub(super) fn remove(&mut self, key: i64) -> Option<i64> {
        assert!(
            self.scan.is_none(),
            "direct removal during a scheduling scan"
        );
        self.remove_key(key)
    }

    pub(super) fn begin_scan(&mut self) {
        assert!(self.scan.is_none(), "overlapping scheduling scans");
        self.wrapped.clear();
        self.scan = Some(Scan {
            remaining: self.len,
            before: self.capacity,
            zero_pending: self.zero.is_some(),
            wrapped_next: 0,
            current: None,
        });
    }

    pub(super) fn next_entry(&mut self) -> Option<(i64, i64)> {
        let scan = self.scan.as_mut().expect("no scheduling scan");
        if scan.remaining == 0 {
            return None;
        }
        scan.remaining -= 1;
        if scan.zero_pending {
            scan.zero_pending = false;
            scan.current = Some(Current::Zero);
            return Some((0, self.zero.expect("scanned zero key disappeared")));
        }
        while scan.before > 0 {
            scan.before -= 1;
            let entry = self.slots[scan.before];
            if entry.key != 0 {
                scan.current = Some(Current::Bucket(scan.before));
                return Some((entry.key, entry.time));
            }
        }
        let key = self.wrapped[scan.wrapped_next];
        scan.wrapped_next += 1;
        scan.current = Some(Current::Wrapped(key));
        Some((key, self.get(key).expect("scanned wrapped key disappeared")))
    }

    pub(super) fn remove_current(&mut self) {
        let current = self
            .scan
            .as_mut()
            .expect("no scheduling scan")
            .current
            .take()
            .expect("no current scheduling entry");
        match current {
            Current::Zero => {
                self.zero = None;
                self.len -= 1;
            }
            Current::Bucket(index) => {
                self.close_hole(index, true);
                self.len -= 1;
            }
            // The observed iterator delegates wrapped removal to normal map
            // removal, including its one-step shrink. Only keys remain here,
            // so rehashing cannot invalidate an unvisited bucket position.
            Current::Wrapped(key) => {
                let removed = self.remove_key(key);
                debug_assert!(removed.is_some());
            }
        }
    }

    pub(super) fn finish_scan(&mut self) {
        self.scan.take().expect("no scheduling scan");
        self.wrapped.clear();
    }

    fn find_bucket(&self, key: i64) -> usize {
        let mask = self.capacity - 1;
        let mut index = mix(key) as usize & mask;
        while self.slots[index].key != 0 && self.slots[index].key != key {
            index = (index + 1) & mask;
        }
        index
    }

    fn remove_key(&mut self, key: i64) -> Option<i64> {
        let time = if key == 0 {
            self.zero.take()?
        } else {
            let index = self.find_bucket(key);
            if self.slots[index].key == 0 {
                return None;
            }
            let time = self.slots[index].time;
            self.close_hole(index, false);
            time
        };
        self.len -= 1;
        if self.capacity > INITIAL_CAPACITY && self.len < max_fill(self.capacity) / 4 {
            self.rehash(self.capacity / 2);
        }
        Some(time)
    }

    fn close_hole(&mut self, mut hole: usize, preserve_scan: bool) {
        let mask = self.capacity - 1;
        let mut candidate = (hole + 1) & mask;
        while self.slots[candidate].key != 0 {
            let entry = self.slots[candidate];
            let home = mix(entry.key) as usize & mask;
            let distance_to_hole = hole.wrapping_sub(home) & mask;
            let distance_to_entry = candidate.wrapping_sub(home) & mask;
            if distance_to_hole < distance_to_entry {
                if preserve_scan && candidate < hole {
                    // Moving across zero puts an unvisited entry behind the
                    // descending cursor. Visit it after the bucket scan ends.
                    assert!(self.wrapped.len() < self.max_keys);
                    self.wrapped.push(entry.key);
                }
                self.slots[hole] = entry;
                hole = candidate;
            }
            candidate = (candidate + 1) & mask;
        }
        self.slots[hole] = Slot::default();
    }

    fn rehash(&mut self, capacity: usize) {
        debug_assert!(capacity <= self.rehash_scratch.len());
        self.rehash_scratch[..capacity].fill(Slot::default());
        let mask = capacity - 1;
        for entry in self.slots[..self.capacity].iter().rev().copied() {
            if entry.key == 0 {
                continue;
            }
            let mut index = mix(entry.key) as usize & mask;
            while self.rehash_scratch[index].key != 0 {
                index = (index + 1) & mask;
            }
            self.rehash_scratch[index] = entry;
        }
        std::mem::swap(&mut self.slots, &mut self.rehash_scratch);
        self.capacity = capacity;
    }
}

fn max_fill(capacity: usize) -> usize {
    capacity / 4 * 3
}

fn mix(key: i64) -> u64 {
    let product = (key as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let high_folded = product ^ (product >> 32);
    high_folded ^ (high_folded >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(max_keys: usize) -> SchedulingIndex {
        SchedulingIndex::new(max_keys, 1 << 20).unwrap()
    }

    fn keys(index: &mut SchedulingIndex) -> Vec<i64> {
        index.begin_scan();
        let mut keys = Vec::new();
        while let Some((key, _)) = index.next_entry() {
            keys.push(key);
        }
        index.finish_scan();
        keys
    }

    fn remove_matching(index: &mut SchedulingIndex, remove: impl Fn(i64) -> bool) -> Vec<i64> {
        index.begin_scan();
        let mut visited = Vec::new();
        while let Some((key, _)) = index.next_entry() {
            visited.push(key);
            if remove(key) {
                index.remove_current();
            }
        }
        index.finish_scan();
        visited
    }

    #[test]
    fn public_mix_observations() {
        for (key, expected) in [
            (0, 0),
            (1, 0x9e37_e78e_98c4_e4d1),
            (-1, 0x61c8_e78e_673b_e4d0),
            (2, 0x3c6e_cf1c_3188_c9a2),
            (3, 0xdaa6_b78a_ca55_be6a),
            (1 << 32, 0x7f4a_035f_035f_035f),
            (i64::MIN, 0x8000_8000_8000_8000),
            (i64::MAX, 0xe1c8_678e_e73b_64d0),
        ] {
            assert_eq!(mix(key), expected);
        }
    }

    #[test]
    fn budget_covers_both_physical_tables_and_wrapped_scratch() {
        let bytes = 128 * 2 * size_of::<Slot>() + 65 * size_of::<i64>();
        let mut index = SchedulingIndex::new(65, bytes).unwrap();
        assert_eq!(index.heap_bytes(), bytes);
        assert_eq!(index.capacity, 32);
        assert!(matches!(
            SchedulingIndex::new(65, bytes - 1),
            Err(TickError::AllocationBudget)
        ));
        assert!(matches!(
            SchedulingIndex::new(usize::MAX, usize::MAX),
            Err(TickError::InvalidLimits)
        ));
        let mut original_tables = [index.slots.as_ptr(), index.rehash_scratch.as_ptr()];
        original_tables.sort_unstable();
        let wrapped = index.wrapped.as_ptr();
        for _ in 0..3 {
            for key in 0..65 {
                index.put(key, key).unwrap();
            }
            for key in (0..65).rev() {
                assert_eq!(index.remove(key), Some(key));
            }
            let mut tables = [index.slots.as_ptr(), index.rehash_scratch.as_ptr()];
            tables.sort_unstable();
            assert_eq!(tables, original_tables);
            assert_eq!(index.wrapped.as_ptr(), wrapped);
            assert_eq!(index.heap_bytes(), bytes);
        }
    }

    #[test]
    fn key_limit_is_atomic_and_updates_do_not_consume_capacity() {
        let mut index = index(2);
        index.put(0, i64::MIN).unwrap();
        index.put(-1, 10).unwrap();
        let before = keys(&mut index);
        assert_eq!(index.put(1, 20), Err(TickError::ChunkLimit));
        assert_eq!(keys(&mut index), before);
        assert_eq!(index.get(1), None);
        index.begin_scan();
        assert_eq!(index.next_entry(), Some((0, i64::MIN)));
        index.put(0, i64::MAX).unwrap();
        index.put(-1, 30).unwrap();
        assert_eq!(index.next_entry(), Some((-1, 30)));
        index.remove_current();
        index.finish_scan();
        assert_eq!(index.get(0), Some(i64::MAX));
        assert_eq!(index.remove(-1), None);
        assert_eq!(index.remove(0), Some(i64::MAX));
        assert_eq!(index.remove(0), None);
        let mut empty = SchedulingIndex::new(0, 1024).unwrap();
        assert_eq!(empty.put(0, 1), Err(TickError::ChunkLimit));
        assert!(keys(&mut empty).is_empty());
    }

    #[test]
    fn observed_growth_and_direct_removal_history() {
        let mut index = index(65);
        for key in 0..65 {
            index.put(key, key).unwrap();
            assert_eq!(
                index.capacity,
                match key + 1 {
                    ..=24 => 32,
                    25..=48 => 64,
                    _ => 128,
                }
            );
            if key == 24 {
                assert_eq!(
                    keys(&mut index),
                    [
                        0, 15, 10, 12, 3, 18, 17, 13, 2, 21, 20, 22, 5, 14, 16, 9, 6, 24, 19, 1,
                        11, 7, 8, 4, 23
                    ]
                );
            }
        }
        for key in (0..65).rev() {
            assert_eq!(index.remove(key), Some(key));
            assert_eq!(
                index.capacity,
                match key {
                    ..=11 => 32,
                    12..=23 => 64,
                    _ => 128,
                }
            );
            if key == 23 {
                assert_eq!(
                    keys(&mut index),
                    [
                        0, 15, 10, 12, 18, 3, 17, 13, 2, 21, 20, 22, 5, 14, 16, 9, 6, 19, 1, 11, 7,
                        8, 4
                    ]
                );
            }
            if key == 11 {
                assert_eq!(keys(&mut index), [0, 5, 9, 6, 1, 10, 7, 8, 3, 4, 2]);
            }
        }
    }

    #[test]
    fn observed_collision_removal_and_reinsertion() {
        let cases = [
            (
                [51, 60, 97, 120, 130, 140],
                [0, 140, 130, 120, 97, 60, 51],
                [140, 130, 120, 60],
                [0, 97, 51, 140, 130, 120, 60],
            ),
            (
                [140, 130, 120, 97, 60, 51],
                [0, 51, 60, 97, 120, 130, 140],
                [60, 120, 130, 140],
                [0, 97, 51, 60, 120, 130, 140],
            ),
            (
                [62, 63, 114, 124, 164, 191],
                [0, 62, 191, 164, 124, 114, 63],
                [63, 191, 164, 124],
                [0, 63, 114, 62, 191, 164, 124],
            ),
            (
                [191, 164, 124, 114, 63, 62],
                [0, 191, 62, 63, 114, 124, 164],
                [191, 63, 124, 164],
                [0, 191, 114, 62, 63, 124, 164],
            ),
        ];
        for (insertion, visited, retained, reinserted) in cases {
            let mut index = index(7);
            for key in insertion {
                index.put(key, 10).unwrap();
            }
            index.put(0, 10).unwrap();
            assert_eq!(keys(&mut index), visited);
            let removed = if insertion.contains(&51) {
                [51, 97]
            } else {
                [62, 114]
            };
            assert_eq!(
                remove_matching(&mut index, |key| key == 0 || removed.contains(&key)),
                visited
            );
            assert_eq!(keys(&mut index), retained);
            index.put(removed[0], 20).unwrap();
            index.put(removed[1], 30).unwrap();
            index.put(0, 40).unwrap();
            assert_eq!(keys(&mut index), reinserted);
            assert_eq!(index.get(removed[0]), Some(20));
            assert_eq!(index.get(removed[1]), Some(30));
        }
    }

    #[test]
    fn wrapped_visitation_differs_from_initial_snapshot() {
        let insertion = [
            -356482285517,
            -377957122072,
            34359738246,
            -330712481896,
            -68719476846,
            -274877906880,
            515396075429,
            47244640138,
            30064771133,
        ];
        let initial = [
            -274877906880,
            -68719476846,
            515396075429,
            34359738246,
            -356482285517,
            -330712481896,
            -377957122072,
            30064771133,
            47244640138,
        ];
        let visited = [
            -274877906880,
            -68719476846,
            515396075429,
            34359738246,
            -356482285517,
            -330712481896,
            -377957122072,
            47244640138,
            30064771133,
        ];
        let mut index = index(9);
        for key in insertion {
            index.put(key, 10).unwrap();
        }
        assert_eq!(keys(&mut index), initial);
        assert_eq!(remove_matching(&mut index, |key| key & 3 != 0), visited);
        for key in insertion {
            assert_eq!(index.get(key), (key & 3 == 0).then_some(10));
        }
    }

    #[test]
    fn ordinary_iterator_removal_does_not_shrink() {
        let mut index = index(65);
        for key in 0..65 {
            index.put(key, key).unwrap();
        }
        let expected = [
            0, 63, 45, 43, 50, 15, 59, 30, 3, 17, 41, 13, 33, 56, 60, 64, 20, 49, 29, 39, 19, 9,
            47, 36, 6, 24, 34, 1, 31, 11, 54, 26, 4, 42, 25, 40, 35, 44, 61, 58, 55, 28, 57, 32,
            10, 53, 18, 12, 48, 27, 38, 2, 62, 21, 22, 16, 14, 5, 52, 7, 8, 46, 23, 37, 51,
        ];
        assert_eq!(remove_matching(&mut index, |_| true), expected);
        assert_eq!(index.capacity, 128);
        assert_eq!(index.len, 0);
    }

    #[test]
    fn wrapped_iterator_removal_uses_normal_shrink() {
        let mut index = index(65);
        for key in (1..).filter(|&key| mix(key) & 127 == 127).take(65) {
            index.put(key, 10).unwrap();
        }
        let expected = [
            63, 7557, 7349, 7190, 7136, 6723, 6477, 6447, 6160, 6130, 6001, 5689, 5645, 5575, 5392,
            5362, 5352, 2305, 2283, 2250, 2157, 2100, 1954, 1830, 1784, 1571, 1561, 1375, 1348,
            1338, 1255, 1234, 1125, 1050, 915, 897, 669, 525, 467, 456, 382, 2485, 2510, 2676,
            2696, 3065, 3122, 3142, 3568, 3595, 3866, 3908, 4227, 4252, 4500, 4579, 4610, 4823,
            4907, 4916, 5020, 5050, 5119, 5134, 5209,
        ];
        assert_eq!(keys(&mut index), expected);
        let bytes = index.heap_bytes();
        index.begin_scan();
        for (offset, expected_key) in expected.into_iter().enumerate() {
            assert_eq!(index.next_entry(), Some((expected_key, 10)));
            index.remove_current();
            assert_eq!(index.capacity, if offset == 64 { 64 } else { 128 });
        }
        assert_eq!(index.next_entry(), None);
        index.finish_scan();
        assert_eq!(index.heap_bytes(), bytes);
        assert!(keys(&mut index).is_empty());
    }

    #[test]
    fn empty_phase_reinsertion_and_detach_preserve_history() {
        let insertion = [62, 63, 114, 124, 164, 191];
        let mut index = index(6);
        for key in insertion {
            index.put(key, 100).unwrap();
        }
        assert_eq!(keys(&mut index), [62, 191, 164, 124, 114, 63]);
        // Equal-key ready insertion preserves this array order. A zero-cap
        // phase reinserts that array without any gameplay callback or pop.
        let ready_array = remove_matching(&mut index, |_| true);
        for key in ready_array {
            index.put(key, 100).unwrap();
        }
        assert_eq!(keys(&mut index), insertion);
        let mut detached = self::index(6);
        for key in insertion {
            detached.put(key, 100).unwrap();
        }
        assert_eq!(detached.remove(62), Some(100));
        detached.put(62, 100).unwrap();
        assert_eq!(keys(&mut detached), [63, 62, 191, 164, 124, 114]);
    }
}
