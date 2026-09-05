//! Bounded heaps preserving the Java 25 queue's observed equal-key behavior.
//! These implementations use standard binary-heap operations and independently
//! authored API fixtures; they contain no translated JDK method bodies.

use super::{ReadyChunk, ScheduledTick, TickError, reserved_vec};

trait HeapEntry {
    fn precedes(&self, other: &Self) -> bool;
}

impl HeapEntry for ScheduledTick {
    fn precedes(&self, other: &Self) -> bool {
        (self.trigger_tick, self.priority, self.sub_tick_order)
            < (other.trigger_tick, other.priority, other.sub_tick_order)
    }
}

impl HeapEntry for ReadyChunk {
    fn precedes(&self, other: &Self) -> bool {
        (self.priority, self.sub_order) < (other.priority, other.sub_order)
    }
}

/// Only the two concrete wrappers below use this private storage helper.
struct Heap<T> {
    values: Vec<T>,
    limit: usize,
}

impl<T: HeapEntry> Heap<T> {
    fn new(capacity: usize, remaining: &mut usize) -> Result<Self, TickError> {
        Ok(Self {
            values: reserved_vec(capacity, remaining)?,
            limit: capacity,
        })
    }

    fn push(&mut self, value: T) {
        assert!(
            self.values.len() < self.limit,
            "heap admission must precede insertion"
        );
        self.values.push(value);
        self.raise(self.values.len() - 1, &mut |_, _| {});
    }

    fn pop(&mut self) -> Option<T> {
        if self.values.is_empty() {
            return None;
        }
        Some(self.remove_at(0, &mut |_, _| {}).0)
    }

    fn swap(&mut self, first: usize, second: usize, moved: &mut impl FnMut(usize, usize)) {
        self.values.swap(first, second);
        moved(first, second);
    }

    fn raise(&mut self, mut index: usize, moved: &mut impl FnMut(usize, usize)) -> usize {
        while index != 0 {
            let parent = (index - 1) / 2;
            if !self.values[index].precedes(&self.values[parent]) {
                break;
            }
            self.swap(index, parent, moved);
            index = parent;
        }
        index
    }

    fn lower(&mut self, mut index: usize, moved: &mut impl FnMut(usize, usize)) -> usize {
        while index < self.values.len() / 2 {
            let left = index * 2 + 1;
            let right = left + 1;
            let child =
                if right < self.values.len() && self.values[right].precedes(&self.values[left]) {
                    right
                } else {
                    left
                };
            if !self.values[child].precedes(&self.values[index]) {
                break;
            }
            self.swap(index, child, moved);
            index = child;
        }
        index
    }

    /// Reports where the former last entry lands so iteration can revisit an
    /// unexamined entry that moved into the already-examined array prefix.
    fn remove_at(
        &mut self,
        index: usize,
        moved: &mut impl FnMut(usize, usize),
    ) -> (T, Option<usize>) {
        let last = self.values.len() - 1;
        if index == last {
            return (self.values.pop().unwrap(), None);
        }
        self.swap(index, last, moved);
        let removed = self.values.pop().unwrap();
        let mut destination = self.lower(index, moved);
        if destination == index {
            destination = self.raise(index, moved);
        }
        (removed, Some(destination))
    }

    fn heap_bytes(&self) -> usize {
        self.values.capacity() * size_of::<T>()
    }
}

/// Two index arrays make deferred iterator visits exact even for equal-valued
/// duplicate ticks. Recording heap swaps keeps each index update constant-time.
struct Revisits {
    positions: Vec<usize>,
    tickets: Vec<usize>,
}

impl Revisits {
    fn new(capacity: usize, remaining: &mut usize) -> Result<Self, TickError> {
        let positions = reserved_vec(capacity, remaining)?;
        let mut tickets = reserved_vec(capacity, remaining)?;
        tickets.resize(capacity, usize::MAX);
        Ok(Self { positions, tickets })
    }

    fn reset(&mut self) {
        self.positions.clear();
        self.tickets.fill(usize::MAX);
    }

    fn defer(&mut self, position: usize) {
        debug_assert_eq!(self.tickets[position], usize::MAX);
        debug_assert!(self.positions.len() < self.positions.capacity());
        self.tickets[position] = self.positions.len();
        self.positions.push(position);
    }

    fn swapped(&mut self, first: usize, second: usize) {
        self.tickets.swap(first, second);
        for position in [first, second] {
            let ticket = self.tickets[position];
            if ticket != usize::MAX {
                self.positions[ticket] = position;
            }
        }
    }

    fn take(&mut self, ticket: usize) -> usize {
        let position = self.positions[ticket];
        self.tickets[position] = usize::MAX;
        position
    }

    fn heap_bytes(&self) -> usize {
        (self.positions.capacity() + self.tickets.capacity()) * size_of::<usize>()
    }
}

pub(super) struct ScheduledHeap {
    heap: Heap<ScheduledTick>,
    revisits: Revisits,
}

impl ScheduledHeap {
    pub(super) fn new(capacity: usize, remaining: &mut usize) -> Result<Self, TickError> {
        let mut budget = *remaining;
        let heap = Heap::new(capacity, &mut budget)?;
        let revisits = Revisits::new(capacity, &mut budget)?;
        *remaining = budget;
        Ok(Self { heap, revisits })
    }

    pub(super) fn len(&self) -> usize {
        self.heap.values.len()
    }
    pub(super) fn peek(&self) -> Option<&ScheduledTick> {
        self.heap.values.first()
    }
    pub(super) fn push(&mut self, tick: ScheduledTick) {
        self.heap.push(tick);
    }
    pub(super) fn pop(&mut self) -> Option<ScheduledTick> {
        self.heap.pop()
    }
    pub(super) fn as_slice(&self) -> &[ScheduledTick] {
        &self.heap.values
    }
    pub(super) fn heap_bytes(&self) -> usize {
        self.heap.heap_bytes() + self.revisits.heap_bytes()
    }

    /// Visits and removes in the queue iterator's order, including entries moved
    /// before the cursor. No predicate call is skipped or repeated by heap repair.
    /// The predicate can update the owner's dedup set for each removed entry.
    pub(super) fn remove_if(&mut self, mut predicate: impl FnMut(&ScheduledTick) -> bool) -> usize {
        self.revisits.reset();
        let original_len = self.len();
        let mut cursor = 0;
        while cursor < self.len() {
            if !predicate(&self.heap.values[cursor]) {
                cursor += 1;
                continue;
            }
            let (_, destination) = self
                .heap
                .remove_at(cursor, &mut |a, b| self.revisits.swapped(a, b));
            if let Some(destination) = destination.filter(|destination| *destination < cursor) {
                self.revisits.defer(destination);
                cursor += 1;
            }
        }
        for ticket in 0..self.revisits.positions.len() {
            let position = self.revisits.take(ticket);
            if predicate(&self.heap.values[position]) {
                self.heap
                    .remove_at(position, &mut |a, b| self.revisits.swapped(a, b));
            }
        }
        self.revisits.reset();
        original_len - self.len()
    }
}

pub(super) struct ReadyHeap {
    heap: Heap<ReadyChunk>,
}

impl ReadyHeap {
    pub(super) fn new(capacity: usize, remaining: &mut usize) -> Result<Self, TickError> {
        Ok(Self {
            heap: Heap::new(capacity, remaining)?,
        })
    }

    pub(super) fn peek(&self) -> Option<&ReadyChunk> {
        self.heap.values.first()
    }
    pub(super) fn push(&mut self, chunk: ReadyChunk) {
        self.heap.push(chunk);
    }
    pub(super) fn pop(&mut self) -> Option<ReadyChunk> {
        self.heap.pop()
    }
    pub(super) fn clear(&mut self) {
        self.heap.values.clear();
    }
    pub(super) fn as_slice(&self) -> &[ReadyChunk] {
        &self.heap.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::ticks::{TickPosition, TickPriority};

    fn tick(id: u32, order: i64) -> ScheduledTick {
        ScheduledTick {
            position: TickPosition {
                x: id as i32,
                y: 64,
                z: 0,
            },
            type_id: id,
            trigger_tick: order,
            priority: TickPriority::Normal,
            sub_tick_order: -1,
        }
    }

    fn scheduled(capacity: usize) -> ScheduledHeap {
        ScheduledHeap::new(capacity, &mut (1024 * 1024)).unwrap()
    }

    fn array(heap: &ScheduledHeap) -> Vec<u32> {
        heap.as_slice().iter().map(|tick| tick.type_id).collect()
    }

    fn drain(heap: &mut ScheduledHeap) -> Vec<u32> {
        let mut result = Vec::new();
        while let Some(tick) = heap.pop() {
            result.push(tick.type_id);
        }
        result
    }

    #[test]
    fn equal_keys_preserve_java_array_and_last_entry_pop_behavior() {
        for count in [1, 2, 3, 4, 5, 8, 17, 33] {
            let mut heap = scheduled(count);
            assert_eq!(heap.len(), 0);
            assert_eq!(heap.peek(), None);
            for id in 0..count as u32 {
                heap.push(tick(id, 10));
            }
            assert_eq!(array(&heap), (0..count as u32).collect::<Vec<_>>());
            let expected: Vec<_> = std::iter::once(0).chain((1..count as u32).rev()).collect();
            assert_eq!(drain(&mut heap), expected);
            assert_eq!(heap.pop(), None);
        }
    }

    #[test]
    fn mixed_equal_siblings_choose_observed_array_and_pop_order() {
        let mut heap = scheduled(8);
        for (id, order) in [2, 1, 1, 0, 0, 2, 1, 0].into_iter().enumerate() {
            heap.push(tick(id as u32, order));
        }
        assert_eq!(array(&heap), [3, 4, 2, 7, 1, 5, 6, 0]);
        assert_eq!(drain(&mut heap), [3, 4, 7, 6, 1, 2, 0, 5]);
    }

    #[test]
    fn iterator_deletion_then_insertion_preserves_equal_key_history() {
        for (remove, after, popped) in [
            (0, [6, 1, 2, 3, 4, 5], [6, 7, 5, 4, 3, 2, 1]),
            (1, [0, 6, 2, 3, 4, 5], [0, 7, 5, 4, 3, 2, 6]),
            (3, [0, 1, 2, 6, 4, 5], [0, 7, 5, 4, 6, 2, 1]),
            (6, [0, 1, 2, 3, 4, 5], [0, 7, 5, 4, 3, 2, 1]),
        ] {
            let mut heap = scheduled(7);
            for id in 0..7 {
                heap.push(tick(id, 10));
            }
            assert_eq!(heap.remove_if(|tick| tick.type_id == remove), 1);
            assert_eq!(array(&heap), after);
            heap.push(tick(7, 10));
            assert_eq!(drain(&mut heap), popped);
        }
        let mut heap = scheduled(7);
        for id in 0..7 {
            heap.push(tick(id, 10));
        }
        assert_eq!(
            heap.remove_if(|tick| tick.type_id == 0 || tick.type_id == 3),
            2
        );
        assert_eq!(array(&heap), [6, 1, 2, 5, 4]);
        assert_eq!(drain(&mut heap), [6, 4, 5, 2, 1]);
    }

    #[test]
    fn ready_heap_uses_only_priority_and_suborder_with_same_tie_rules() {
        let mut heap = ReadyHeap::new(8, &mut 4096).unwrap();
        let bytes = heap.heap.heap_bytes();
        for (index, order) in [2, 1, 1, 0, 0, 2, 1, 0].into_iter().enumerate() {
            heap.push(ReadyChunk {
                index,
                priority: TickPriority::Normal,
                sub_order: order,
            });
        }
        assert_eq!(
            heap.as_slice()
                .iter()
                .map(|entry| entry.index)
                .collect::<Vec<_>>(),
            [3, 4, 2, 7, 1, 5, 6, 0]
        );
        assert_eq!(heap.peek().unwrap().index, 3);
        let mut popped = Vec::new();
        while let Some(entry) = heap.pop() {
            popped.push(entry.index);
        }
        assert_eq!(popped, [3, 4, 7, 6, 1, 2, 0, 5]);
        for index in 0..8 {
            heap.push(ReadyChunk {
                index,
                priority: TickPriority::High,
                sub_order: -1,
            });
        }
        heap.clear();
        assert!(heap.as_slice().is_empty());
        assert!(heap.pop().is_none());
        assert_eq!(heap.heap.heap_bytes(), bytes);
    }

    #[test]
    fn removal_scratch_is_reserved_and_reused_and_failed_construction_is_atomic() {
        let mut budget = 1024 * 1024;
        let mut heap = ScheduledHeap::new(64, &mut budget).unwrap();
        let bytes = heap.heap_bytes();
        assert_eq!(1024 * 1024 - budget, bytes);
        assert!(bytes >= 64 * (size_of::<ScheduledTick>() + 2 * size_of::<usize>()));
        let capacity = heap.heap.values.capacity();
        for round in 0..4 {
            for id in 0..64 {
                heap.push(tick(id, i64::from((id * 23 + round) % 11)));
            }
            let mut visits = vec![0; 64];
            heap.remove_if(|tick| {
                visits[tick.type_id as usize] += 1;
                tick.type_id % 3 == 0
            });
            assert!(visits.iter().all(|&count| count == 1));
            assert_eq!(heap.heap.values.capacity(), capacity);
            assert_eq!(heap.heap_bytes(), bytes);
            let remaining = heap.len();
            assert_eq!(heap.remove_if(|_| true), remaining);
            assert_eq!(heap.len(), 0);
        }
        let mut insufficient = bytes - 1;
        assert!(matches!(
            ScheduledHeap::new(64, &mut insufficient),
            Err(TickError::AllocationBudget)
        ));
        assert_eq!(insufficient, bytes - 1);
    }

    #[test]
    fn upward_repair_revisits_the_unexamined_tail_and_removes_it() {
        let mut heap = scheduled(7);
        for (id, order) in [0, 5, 1, 6, 7, 2, 3].into_iter().enumerate() {
            heap.push(tick(id as u32, order));
        }
        let mut visited = Vec::new();
        assert_eq!(
            heap.remove_if(|tick| {
                visited.push(tick.type_id);
                matches!(tick.type_id, 3 | 6)
            }),
            2
        );
        assert_eq!(visited, [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(array(&heap), [0, 5, 2, 1, 4]);
        assert_eq!(drain(&mut heap), [0, 2, 5, 1, 4]);
    }

    // This observer invokes public Java APIs; it is not an implementation of
    // queue repair or iterator removal. Runtime compilation uses only Java 25.
    const JAVA_ITERATOR_ORACLE: &str = r#"
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.PriorityQueue;

class TickHeapIteratorOracle {
    record Item(int id, long order) {}
    static String ids(List<Integer> values) {
        return String.join(",", values.stream().map(Object::toString).toList());
    }
    static String view(PriorityQueue<Item> queue) {
        return ids(queue.stream().map(Item::id).toList());
    }
    public static void main(String[] args) throws Exception {
        if (Runtime.version().feature() != 25) throw new AssertionError("Java 25 required");
        List<String> output = new ArrayList<>();
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            String[] fields = line.split("\\|", -1);
            PriorityQueue<Item> queue = new PriorityQueue<>(Comparator.comparingLong(Item::order));
            String[] orders = fields[0].split(",");
            for (int id = 0; id < orders.length; id++) queue.add(new Item(id, Long.parseLong(orders[id])));
            List<Integer> removals = new ArrayList<>();
            if (!fields[1].isEmpty()) for (String id : fields[1].split(",")) removals.add(Integer.parseInt(id));
            String before = view(queue);
            List<Integer> visits = new ArrayList<>();
            var iterator = queue.iterator();
            while (iterator.hasNext()) {
                Item item = iterator.next();
                visits.add(item.id());
                if (removals.contains(item.id())) iterator.remove();
            }
            String after = view(queue);
            List<Integer> popped = new ArrayList<>();
            while (!queue.isEmpty()) popped.add(queue.remove().id());
            output.add(before + "|" + ids(visits) + "|" + after + "|" + ids(popped));
        }
        Files.write(Path.of(args[1]), output);
    }
}
"#;

    #[test]
    #[ignore = "requires Java25 on JAVA_HOME or PATH for actual iterator-removal comparison"]
    fn iterator_deletion_matches_actual_java25_visits_arrays_and_pop_order() {
        use std::{env, fs, path::PathBuf, process::Command, time::SystemTime};

        fn csv<T: std::fmt::Display>(values: &[T]) -> String {
            values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        }

        let mut fixtures = vec![(vec![0, 5, 1, 6, 7, 2, 3], vec![3, 6])];
        for seed in 0..128_u32 {
            let count = 7 + seed as usize % 58;
            let priorities: Vec<_> = (0..count)
                .map(|index| {
                    i64::from(((index as u32 * 37 + seed * 13) ^ (index as u32 * seed)) % 17)
                })
                .collect();
            let removals = (0..count as u32)
                .filter(|id| (id * 11 + seed) % 5 < 3)
                .collect();
            fixtures.push((priorities, removals));
        }
        let mut input = String::new();
        let mut expected = Vec::new();
        for (priorities, removals) in fixtures {
            input.push_str(&format!("{}|{}\n", csv(&priorities), csv(&removals)));
            let mut heap = scheduled(priorities.len());
            for (id, order) in priorities.into_iter().enumerate() {
                heap.push(tick(id as u32, order));
            }
            let before = csv(&array(&heap));
            let mut visits = Vec::new();
            heap.remove_if(|tick| {
                visits.push(tick.type_id);
                removals.contains(&tick.type_id)
            });
            let after = csv(&array(&heap));
            expected.push(format!(
                "{before}|{}|{after}|{}",
                csv(&visits),
                csv(&drain(&mut heap))
            ));
        }

        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = env::temp_dir().join(format!(
            "arrow-heap-iterator-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let source = directory.join("TickHeapIteratorOracle.java");
        let input_path = directory.join("input.txt");
        let output_path = directory.join("observations.txt");
        fs::write(&source, JAVA_ITERATOR_ORACLE).unwrap();
        fs::write(&input_path, input).unwrap();
        let java = env::var_os("JAVA_HOME")
            .map(|home| {
                PathBuf::from(home).join(if cfg!(windows) {
                    "bin/java.exe"
                } else {
                    "bin/java"
                })
            })
            .unwrap_or_else(|| PathBuf::from("java"));
        let process = Command::new(java)
            .arg(&source)
            .arg(&input_path)
            .arg(&output_path)
            .current_dir(&directory)
            .output()
            .expect("Java25 must be available");
        assert!(
            process.status.success(),
            "Java oracle failed: {}",
            String::from_utf8_lossy(&process.stderr)
        );
        let output = fs::read_to_string(output_path).unwrap();
        assert!(
            directory
                .canonicalize()
                .unwrap()
                .starts_with(env::temp_dir().canonicalize().unwrap())
        );
        fs::remove_dir_all(directory).unwrap();
        let actual: Vec<_> = output.lines().collect();
        assert_eq!(actual.len(), expected.len());
        for (index, (java, rust)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(java, rust, "Java iterator removal case {index}");
        }
        eprintln!(
            "Compared {} actual Java25 iterator visit/array/pop traces",
            actual.len()
        );
    }
}
