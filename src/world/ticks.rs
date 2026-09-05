//! Live block/fluid scheduled ticks with explicit collect/run/finish boundaries.
//!
//! This is the synchronous ordering owner, not block/fluid behavior or a game
//! tick loop. The consumer checks the current block/fluid type before executing
//! each returned action. Block and fluid phases are collected separately: a
//! fluid scheduled by a block callback can enter the subsequent fluid phase.
//!
//! Runtime scheduling owns one shared signed wrapping sub-tick counter. Saved
//! tick restoration/serialization, explicit sub-order insertion, area clear/copy
//! and their historical equal-key ordering are not exposed by this first slice.
//! Requirements were researched in LevelTicks, LevelChunkTicks, ScheduledTick,
//! LevelAccessor and ServerLevel in locked 26.3-pre-2, then independently designed.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hash};

use super::preparation::ChunkAddress;

pub const MAX_SCHEDULED_TICKS_PER_PHASE: usize = 65536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TickDomain {
    Block,
    Fluid,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TickPosition {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl TickPosition {
    pub fn chunk(self) -> ChunkAddress {
        ChunkAddress {
            x: self.x >> 4,
            z: self.z >> 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TickPriority {
    ExtremelyHigh = -3,
    VeryHigh = -2,
    High = -1,
    Normal = 0,
    Low = 1,
    VeryLow = 2,
    ExtremelyLow = 3,
}

impl TickPriority {
    pub fn from_value(value: i32) -> Self {
        match value {
            ..=-3 => Self::ExtremelyHigh,
            -2 => Self::VeryHigh,
            -1 => Self::High,
            0 => Self::Normal,
            1 => Self::Low,
            2 => Self::VeryLow,
            3.. => Self::ExtremelyLow,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduledTick {
    pub position: TickPosition,
    /// Canonical ID from the world's block or fluid type registry, not state ID.
    pub type_id: u32,
    pub trigger_tick: i64,
    pub priority: TickPriority,
    pub sub_tick_order: i64,
}

impl ScheduledTick {
    fn identity(self) -> Identity {
        Identity {
            position: self.position,
            type_id: self.type_id,
        }
    }
    fn within_tick(self) -> (TickPriority, i64) {
        (self.priority, self.sub_tick_order)
    }
}

impl Ord for ScheduledTick {
    fn cmp(&self, other: &Self) -> Ordering {
        (other.trigger_tick, other.priority, other.sub_tick_order).cmp(&(
            self.trigger_tick,
            self.priority,
            self.sub_tick_order,
        ))
    }
}

impl PartialOrd for ScheduledTick {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TickLimits {
    pub max_chunks: usize,
    /// Per domain, per retained chunk (including detached chunks).
    pub queued_per_chunk: usize,
    pub selected_per_phase: usize,
    /// Requested/returned Vec backing capacity, including retained empty heaps.
    /// Stack and allocator metadata are not included; this is not RSS.
    pub allocation_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TickError {
    InvalidLimits,
    AllocationFailed,
    AllocationBudget,
    ChunkLimit,
    QueueFull,
    AlreadyRegistered,
    MissingChunk,
    PhaseActive,
    NoActivePhase,
    UnconsumedTicks,
    PhaseLimit,
    InvalidType,
}

impl fmt::Display for TickError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "scheduled tick owner: {self:?}")
    }
}
impl std::error::Error for TickError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduleOutcome {
    Added,
    Duplicate,
    MissingContainer,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Identity {
    position: TickPosition,
    type_id: u32,
}

/// Fixed open addressing makes dedup memory explicit and avoids scanning every
/// queued tick. Its iteration order is never used to determine execution order.
struct Identities {
    slots: Vec<Option<Identity>>,
    count: usize,
    hash: RandomState,
}

impl Identities {
    fn new(capacity: usize, remaining: &mut usize) -> Result<Self, TickError> {
        let slots = capacity
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(TickError::InvalidLimits)?;
        let mut values = reserved_vec(slots, remaining)?;
        values.resize(slots, None);
        Ok(Self {
            slots: values,
            count: 0,
            hash: RandomState::new(),
        })
    }
    fn index(&self, value: Identity) -> usize {
        self.hash.hash_one(value) as usize & (self.slots.len() - 1)
    }
    fn contains(&self, value: Identity) -> bool {
        let mut index = self.index(value);
        while let Some(found) = self.slots[index] {
            if found == value {
                return true;
            }
            index = (index + 1) & (self.slots.len() - 1);
        }
        false
    }
    fn insert(&mut self, value: Identity) {
        let mut index = self.index(value);
        while self.slots[index].is_some() {
            index = (index + 1) & (self.slots.len() - 1);
        }
        self.slots[index] = Some(value);
        self.count += 1;
    }
    fn remove(&mut self, value: Identity) {
        let mut index = self.index(value);
        while self.slots[index] != Some(value) {
            if self.slots[index].is_none() {
                return;
            }
            index = (index + 1) & (self.slots.len() - 1);
        }
        self.slots[index] = None;
        self.count -= 1;
        // Reinsert the following cluster; no tombstones accumulate across ticks.
        index = (index + 1) & (self.slots.len() - 1);
        while let Some(displaced) = self.slots[index].take() {
            self.count -= 1;
            self.insert(displaced);
            index = (index + 1) & (self.slots.len() - 1);
        }
    }
    fn clear(&mut self) {
        if self.count != 0 {
            self.slots.fill(None);
            self.count = 0;
        }
    }
    fn heap_bytes(&self) -> usize {
        self.slots.capacity() * size_of::<Option<Identity>>()
    }
}

struct ChunkQueue {
    heap: BinaryHeap<ScheduledTick>,
    identities: Identities,
}

impl ChunkQueue {
    fn new(capacity: usize, remaining: &mut usize) -> Result<Self, TickError> {
        Ok(Self {
            heap: BinaryHeap::from(reserved_vec(capacity, remaining)?),
            identities: Identities::new(capacity, remaining)?,
        })
    }
    fn pop(&mut self) -> Option<ScheduledTick> {
        let tick = self.heap.pop()?;
        self.identities.remove(tick.identity());
        Some(tick)
    }
    fn heap_bytes(&self) -> usize {
        self.heap.capacity() * size_of::<ScheduledTick>() + self.identities.heap_bytes()
    }
}

struct ChunkQueues {
    address: ChunkAddress,
    registered: bool,
    eligible: bool,
    blocks: ChunkQueue,
    fluids: ChunkQueue,
}

impl ChunkQueues {
    fn queue(&self, domain: TickDomain) -> &ChunkQueue {
        match domain {
            TickDomain::Block => &self.blocks,
            TickDomain::Fluid => &self.fluids,
        }
    }
    fn queue_mut(&mut self, domain: TickDomain) -> &mut ChunkQueue {
        match domain {
            TickDomain::Block => &mut self.blocks,
            TickDomain::Fluid => &mut self.fluids,
        }
    }
    fn heap_bytes(&self) -> usize {
        self.blocks.heap_bytes() + self.fluids.heap_bytes()
    }
}

#[derive(Eq, PartialEq)]
struct ReadyChunk {
    index: usize,
    priority: TickPriority,
    sub_order: i64,
}

impl Ord for ReadyChunk {
    fn cmp(&self, other: &Self) -> Ordering {
        (other.priority, other.sub_order).cmp(&(self.priority, self.sub_order))
    }
}
impl PartialOrd for ReadyChunk {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// One instance per world: block/fluid scheduling shares a sub-tick counter.
/// Registered containers and eligibility are distinct; detach retains pending
/// ticks and their memory until reattachment or explicit discard.
pub struct ScheduledTickOwner {
    limits: TickLimits,
    block_types: u32,
    fluid_types: u32,
    next_sub_tick: i64,
    chunks: Vec<ChunkQueues>,
    ready: BinaryHeap<ReadyChunk>,
    selected: Vec<ScheduledTick>,
    selected_ids: Identities,
    cursor: usize,
    active: Option<TickDomain>,
    allocated_bytes: usize,
}

impl ScheduledTickOwner {
    pub fn new(block_types: u32, fluid_types: u32, limits: TickLimits) -> Result<Self, TickError> {
        if block_types == 0
            || fluid_types == 0
            || limits.max_chunks == 0
            || limits.queued_per_chunk == 0
            || limits.selected_per_phase == 0
            || limits.selected_per_phase > MAX_SCHEDULED_TICKS_PER_PHASE
        {
            return Err(TickError::InvalidLimits);
        }
        let mut remaining = limits.allocation_bytes;
        let chunks = reserved_vec(limits.max_chunks, &mut remaining)?;
        let ready = BinaryHeap::from(reserved_vec(limits.max_chunks, &mut remaining)?);
        let selected = reserved_vec(limits.selected_per_phase, &mut remaining)?;
        let selected_ids = Identities::new(limits.selected_per_phase, &mut remaining)?;
        Ok(Self {
            limits,
            block_types,
            fluid_types,
            next_sub_tick: 0,
            chunks,
            ready,
            selected,
            selected_ids,
            cursor: 0,
            active: None,
            allocated_bytes: limits.allocation_bytes - remaining,
        })
    }

    pub fn retained_heap_bytes(&self) -> usize {
        self.allocated_bytes
    }
    pub fn next_sub_tick_order(&self) -> i64 {
        self.next_sub_tick
    }

    pub fn register_chunk(
        &mut self,
        address: ChunkAddress,
        eligible: bool,
    ) -> Result<(), TickError> {
        match self.find(address) {
            Ok(index) => {
                if self.chunks[index].registered {
                    return Err(TickError::AlreadyRegistered);
                }
                self.chunks[index].registered = true;
                self.chunks[index].eligible = eligible;
            }
            Err(index) => {
                if self.chunks.len() == self.limits.max_chunks {
                    return Err(TickError::ChunkLimit);
                }
                let mut remaining = self.limits.allocation_bytes - self.allocated_bytes;
                let blocks = ChunkQueue::new(self.limits.queued_per_chunk, &mut remaining)?;
                let fluids = ChunkQueue::new(self.limits.queued_per_chunk, &mut remaining)?;
                self.chunks.insert(
                    index,
                    ChunkQueues {
                        address,
                        registered: true,
                        eligible,
                        blocks,
                        fluids,
                    },
                );
                self.allocated_bytes = self.limits.allocation_bytes - remaining;
            }
        }
        Ok(())
    }

    pub fn set_eligible(&mut self, address: ChunkAddress, eligible: bool) -> Result<(), TickError> {
        let index = self.registered(address)?;
        self.chunks[index].eligible = eligible;
        Ok(())
    }

    /// Already-collected actions remain selected, matching container removal's
    /// boundary. The game owner rechecks current state before running an action.
    pub fn detach_chunk(&mut self, address: ChunkAddress) -> Result<(), TickError> {
        let index = self.registered(address)?;
        self.chunks[index].registered = false;
        Ok(())
    }

    /// Explicitly destroys retained queue data. Persistence must capture it first
    /// once saved-tick support is implemented; this is not an automatic unload.
    pub fn discard_detached_chunk(&mut self, address: ChunkAddress) -> Result<(), TickError> {
        let index = self.find(address).map_err(|_| TickError::MissingChunk)?;
        if self.chunks[index].registered {
            return Err(TickError::AlreadyRegistered);
        }
        self.allocated_bytes -= self.chunks[index].heap_bytes();
        self.chunks.remove(index);
        Ok(())
    }

    /// Duplicate and missing-container attempts consume a sub-order, as creating
    /// the Vanilla tick precedes queue admission. Resource failures are explicit;
    /// callers must handle them rather than silently lose a necessary game tick.
    pub fn schedule(
        &mut self,
        domain: TickDomain,
        position: TickPosition,
        type_id: u32,
        game_time: i64,
        delay: i32,
        priority: TickPriority,
    ) -> Result<ScheduleOutcome, TickError> {
        let count = match domain {
            TickDomain::Block => self.block_types,
            TickDomain::Fluid => self.fluid_types,
        };
        if type_id >= count {
            return Err(TickError::InvalidType);
        }
        let sub_tick_order = self.next_sub_tick;
        self.next_sub_tick = self.next_sub_tick.wrapping_add(1);
        let Ok(index) = self.registered(position.chunk()) else {
            return Ok(ScheduleOutcome::MissingContainer);
        };
        let queue = self.chunks[index].queue_mut(domain);
        let tick = ScheduledTick {
            position,
            type_id,
            trigger_tick: game_time.wrapping_add(i64::from(delay)),
            priority,
            sub_tick_order,
        };
        if queue.identities.contains(tick.identity()) {
            return Ok(ScheduleOutcome::Duplicate);
        }
        if queue.heap.len() == self.limits.queued_per_chunk {
            return Err(TickError::QueueFull);
        }
        queue.identities.insert(tick.identity());
        queue.heap.push(tick);
        Ok(ScheduleOutcome::Added)
    }

    pub fn has_scheduled(&self, domain: TickDomain, position: TickPosition, type_id: u32) -> bool {
        self.registered(position.chunk()).is_ok_and(|index| {
            self.chunks[index]
                .queue(domain)
                .identities
                .contains(Identity { position, type_id })
        })
    }

    /// Like LevelTicks.count, excludes selected actions and detached containers.
    pub fn queued_count(&self, domain: TickDomain) -> usize {
        self.chunks
            .iter()
            .filter(|chunk| chunk.registered)
            .map(|chunk| chunk.queue(domain).heap.len())
            .sum()
    }

    pub fn begin_phase(
        &mut self,
        domain: TickDomain,
        game_time: i64,
        max_ticks: usize,
    ) -> Result<usize, TickError> {
        if self.active.is_some() {
            return Err(TickError::PhaseActive);
        }
        if max_ticks > self.limits.selected_per_phase {
            return Err(TickError::PhaseLimit);
        }
        self.active = Some(domain);
        for (index, chunk) in self.chunks.iter().enumerate() {
            if chunk.registered
                && chunk.eligible
                && let Some(tick) = chunk
                    .queue(domain)
                    .heap
                    .peek()
                    .filter(|tick| tick.trigger_tick <= game_time)
            {
                self.ready.push(ReadyChunk {
                    index,
                    priority: tick.priority,
                    sub_order: tick.sub_tick_order,
                });
            }
        }
        while self.selected.len() < max_ticks {
            let Some(candidate) = self.ready.pop() else {
                break;
            };
            loop {
                let tick = self.chunks[candidate.index]
                    .queue_mut(domain)
                    .pop()
                    .expect("ready chunk contains tick");
                self.selected_ids.insert(tick.identity());
                self.selected.push(tick);
                if self.selected.len() == max_ticks {
                    break;
                }
                let Some(next) = self.chunks[candidate.index].queue(domain).heap.peek() else {
                    break;
                };
                if next.trigger_tick > game_time {
                    break;
                }
                if self
                    .ready
                    .peek()
                    .is_some_and(|other| next.within_tick() > (other.priority, other.sub_order))
                {
                    break;
                }
            }
            if let Some(tick) = self.chunks[candidate.index]
                .queue(domain)
                .heap
                .peek()
                .filter(|tick| tick.trigger_tick <= game_time)
            {
                self.ready.push(ReadyChunk {
                    index: candidate.index,
                    priority: tick.priority,
                    sub_order: tick.sub_tick_order,
                });
            }
        }
        self.ready.clear();
        Ok(self.selected.len())
    }

    pub fn will_tick_this_phase(
        &self,
        domain: TickDomain,
        position: TickPosition,
        type_id: u32,
    ) -> bool {
        self.active == Some(domain) && self.selected_ids.contains(Identity { position, type_id })
    }

    /// Advance before executing the returned callback: it is no longer included
    /// by will_tick_this_phase. Scheduling during callbacks affects later collects.
    pub fn next_due(&mut self) -> Result<Option<ScheduledTick>, TickError> {
        if self.active.is_none() {
            return Err(TickError::NoActivePhase);
        }
        let Some(&tick) = self.selected.get(self.cursor) else {
            return Ok(None);
        };
        self.cursor += 1;
        self.selected_ids.remove(tick.identity());
        Ok(Some(tick))
    }

    pub fn finish_phase(&mut self) -> Result<(), TickError> {
        if self.active.is_none() {
            return Err(TickError::NoActivePhase);
        }
        if self.cursor != self.selected.len() {
            return Err(TickError::UnconsumedTicks);
        }
        self.selected.clear();
        self.selected_ids.clear();
        self.cursor = 0;
        self.active = None;
        Ok(())
    }

    fn find(&self, address: ChunkAddress) -> Result<usize, usize> {
        self.chunks
            .binary_search_by_key(&address, |chunk| chunk.address)
    }
    fn registered(&self, address: ChunkAddress) -> Result<usize, TickError> {
        let index = self.find(address).map_err(|_| TickError::MissingChunk)?;
        if !self.chunks[index].registered {
            return Err(TickError::MissingChunk);
        }
        Ok(index)
    }
}

fn reserved_vec<T>(capacity: usize, remaining: &mut usize) -> Result<Vec<T>, TickError> {
    let requested = capacity
        .checked_mul(size_of::<T>())
        .ok_or(TickError::InvalidLimits)?;
    if requested > *remaining {
        return Err(TickError::AllocationBudget);
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| TickError::AllocationFailed)?;
    let actual = values
        .capacity()
        .checked_mul(size_of::<T>())
        .ok_or(TickError::AllocationBudget)?;
    if actual > *remaining {
        return Err(TickError::AllocationBudget);
    }
    *remaining -= actual;
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_clusters_remain_searchable_without_accumulated_tombstones() {
        let mut budget = 65536;
        let mut set = Identities::new(64, &mut budget).unwrap();
        let identities: Vec<_> = (0..64)
            .map(|x| Identity {
                position: TickPosition { x, y: x * 7, z: -x },
                type_id: 3,
            })
            .collect();
        for _ in 0..64 {
            for &identity in &identities {
                set.insert(identity);
            }
            for index in (0..64).step_by(2) {
                set.remove(identities[index]);
            }
            for (index, &identity) in identities.iter().enumerate() {
                assert_eq!(set.contains(identity), index % 2 == 1);
            }
            for index in (1..64).step_by(2) {
                set.remove(identities[index]);
            }
            assert_eq!(set.count, 0);
            assert!(set.slots.iter().all(Option::is_none));
        }
    }

    #[test]
    fn trigger_and_shared_suborder_use_signed_java_wrapping() {
        let mut owner = ScheduledTickOwner::new(
            8,
            8,
            TickLimits {
                max_chunks: 1,
                queued_per_chunk: 8,
                selected_per_phase: 8,
                allocation_bytes: 8192,
            },
        )
        .unwrap();
        owner
            .register_chunk(ChunkAddress { x: 0, z: 0 }, true)
            .unwrap();
        owner.next_sub_tick = i64::MAX;
        let a = TickPosition { x: 0, y: 0, z: 0 };
        let b = TickPosition { x: 1, y: 0, z: 0 };
        owner
            .schedule(TickDomain::Block, a, 1, i64::MAX, 1, TickPriority::Normal)
            .unwrap();
        owner
            .schedule(TickDomain::Fluid, b, 2, 0, 0, TickPriority::Normal)
            .unwrap();
        assert_eq!(owner.next_sub_tick_order(), i64::MIN + 1);
        owner.begin_phase(TickDomain::Block, 0, 8).unwrap();
        let tick = owner.next_due().unwrap().unwrap();
        assert_eq!(tick.trigger_tick, i64::MIN);
        assert_eq!(tick.sub_tick_order, i64::MAX);
        owner.finish_phase().unwrap();
        owner.begin_phase(TickDomain::Fluid, 0, 8).unwrap();
        assert_eq!(owner.next_due().unwrap().unwrap().sub_tick_order, i64::MIN);
    }
}
