//! Live block/fluid scheduled ticks with explicit collect/run/finish boundaries.
//!
//! This is the synchronous ordering owner, not block/fluid behavior or a game
//! tick loop. The consumer checks the current block/fluid type before executing
//! each returned action. Block and fluid phases are collected separately: a
//! fluid scheduled by a block callback can enter the subsequent fluid phase.
//!
//! Runtime scheduling owns one shared signed wrapping sub-tick counter. Saved
//! tick restoration and area copying preserve independently assigned sub-orders;
//! private heaps and scheduling indexes retain their observable tie history.
//! Requirements were researched in LevelTicks, LevelChunkTicks, ScheduledTick,
//! LevelAccessor and ServerLevel in locked 26.3-pre-2, then independently designed.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hash};

use super::preparation::ChunkAddress;

mod heap;
mod order_index;
mod remaining;

use heap::{ReadyHeap, ScheduledHeap};
use order_index::SchedulingIndex;
use remaining::RemainingCounts;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SavedTick {
    pub position: TickPosition,
    pub type_id: u32,
    pub delay: i32,
    pub priority: TickPriority,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TickBounds {
    pub min: TickPosition,
    pub max: TickPosition,
}

impl TickBounds {
    fn validate(self) -> Result<(), TickError> {
        if self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z {
            Err(TickError::InvalidBounds)
        } else {
            Ok(())
        }
    }
    fn contains(self, pos: TickPosition) -> bool {
        pos.x >= self.min.x
            && pos.x <= self.max.x
            && pos.y >= self.min.y
            && pos.y <= self.max.y
            && pos.z >= self.min.z
            && pos.z <= self.max.z
    }
    fn contains_chunk(self, chunk: ChunkAddress) -> bool {
        let min = self.min.chunk();
        let max = self.max.chunk();
        chunk.x >= min.x && chunk.x <= max.x && chunk.z >= min.z && chunk.z <= max.z
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CopyOutcome {
    pub added: usize,
    pub duplicates: usize,
    pub missing_containers: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct TickLimits {
    pub max_chunks: usize,
    /// Pending plus live entries per domain/retained chunk (including detached).
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
    InvalidBounds,
    ChunkAlreadyPresent,
    OutputCapacity,
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
        while let Some(existing) = self.slots[index] {
            if existing == value {
                return;
            }
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
    heap: ScheduledHeap,
    identities: Identities,
    pending: Option<Vec<SavedTick>>,
}

impl ChunkQueue {
    fn new(capacity: usize, remaining: &mut usize) -> Result<Self, TickError> {
        Ok(Self {
            heap: ScheduledHeap::new(capacity, remaining)?,
            identities: Identities::new(capacity, remaining)?,
            pending: None,
        })
    }
    fn pop(&mut self) -> Option<ScheduledTick> {
        let tick = self.heap.pop()?;
        self.identities.remove(tick.identity());
        Some(tick)
    }
    fn heap_bytes(&self) -> usize {
        self.heap.heap_bytes()
            + self.identities.heap_bytes()
            + self
                .pending
                .as_ref()
                .map_or(0, |pending| pending.capacity() * size_of::<SavedTick>())
    }
    fn count(&self) -> usize {
        self.heap.len() + self.pending.as_ref().map_or(0, Vec::len)
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReadyChunk {
    index: usize,
    priority: TickPriority,
    sub_order: i64,
}

#[derive(Clone, Copy)]
struct SnapshotTick {
    tick: ScheduledTick,
    sequence: usize,
    admitted: bool,
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
    ready: ReadyHeap,
    block_index: SchedulingIndex,
    fluid_index: SchedulingIndex,
    selected: Vec<ScheduledTick>,
    selected_ids: Identities,
    // Counts accelerate the source of a lazy-set rebuild. They never answer
    // queries directly: the separately observed set may intentionally be stale.
    remaining_ids: RemainingCounts,
    #[cfg(test)]
    selected_rebuilt_entries: usize,
    scratch: Vec<SnapshotTick>,
    scratch_limit: usize,
    copy_ids: Option<Identities>,
    copy_capacity: usize,
    copy_counts: Vec<usize>,
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
        let ready = ReadyHeap::new(limits.max_chunks, &mut remaining)?;
        let block_index = SchedulingIndex::new(limits.max_chunks, remaining)?;
        remaining -= block_index.heap_bytes();
        let fluid_index = SchedulingIndex::new(limits.max_chunks, remaining)?;
        remaining -= fluid_index.heap_bytes();
        let selected = reserved_vec(limits.selected_per_phase, &mut remaining)?;
        let selected_ids = Identities::new(limits.selected_per_phase, &mut remaining)?;
        let remaining_ids = RemainingCounts::new(limits.selected_per_phase, &mut remaining)?;
        let scratch_capacity = limits
            .max_chunks
            .checked_mul(limits.queued_per_chunk)
            .and_then(|queued| queued.checked_add(limits.selected_per_phase))
            .ok_or(TickError::InvalidLimits)?;
        Ok(Self {
            limits,
            block_types,
            fluid_types,
            next_sub_tick: 0,
            chunks,
            ready,
            block_index,
            fluid_index,
            selected,
            selected_ids,
            remaining_ids,
            #[cfg(test)]
            selected_rebuilt_entries: 0,
            scratch: Vec::new(),
            scratch_limit: scratch_capacity,
            copy_ids: None,
            copy_capacity: 0,
            copy_counts: Vec::new(),
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
        self.update_head(address, TickDomain::Block);
        self.update_head(address, TickDomain::Fluid);
        Ok(())
    }

    /// Copies saved data into a new detached chunk after reserving its complete
    /// payload. Positions outside the chunk are filtered without reordering or
    /// deduplicating the remaining saved entries. Mutable input aliases are not
    /// retained. Registering and starting tick execution remain separate steps.
    pub fn load_pending_chunk(
        &mut self,
        address: ChunkAddress,
        blocks: &[SavedTick],
        fluids: &[SavedTick],
    ) -> Result<(), TickError> {
        let index = self
            .find(address)
            .err()
            .ok_or(TickError::ChunkAlreadyPresent)?;
        if self.chunks.len() == self.limits.max_chunks {
            return Err(TickError::ChunkLimit);
        }
        let block_count = blocks
            .iter()
            .filter(|tick| tick.position.chunk() == address)
            .count();
        let fluid_count = fluids
            .iter()
            .filter(|tick| tick.position.chunk() == address)
            .count();
        if block_count > self.limits.queued_per_chunk
            || fluid_count > self.limits.queued_per_chunk
            || block_count > i32::MAX as usize
            || fluid_count > i32::MAX as usize
        {
            return Err(TickError::QueueFull);
        }
        for (values, count) in [(blocks, self.block_types), (fluids, self.fluid_types)] {
            if values
                .iter()
                .any(|tick| tick.position.chunk() == address && tick.type_id >= count)
            {
                return Err(TickError::InvalidType);
            }
        }
        let mut remaining = self.limits.allocation_bytes - self.allocated_bytes;
        let mut block_queue = ChunkQueue::new(self.limits.queued_per_chunk, &mut remaining)?;
        let mut fluid_queue = ChunkQueue::new(self.limits.queued_per_chunk, &mut remaining)?;
        let mut pending_blocks = reserved_vec(block_count, &mut remaining)?;
        let mut pending_fluids = reserved_vec(fluid_count, &mut remaining)?;
        for tick in blocks
            .iter()
            .filter(|tick| tick.position.chunk() == address)
        {
            pending_blocks.push(*tick);
            block_queue.identities.insert(Identity {
                position: tick.position,
                type_id: tick.type_id,
            });
        }
        for tick in fluids
            .iter()
            .filter(|tick| tick.position.chunk() == address)
        {
            pending_fluids.push(*tick);
            fluid_queue.identities.insert(Identity {
                position: tick.position,
                type_id: tick.type_id,
            });
        }
        block_queue.pending = Some(pending_blocks);
        fluid_queue.pending = Some(pending_fluids);
        self.chunks.insert(
            index,
            ChunkQueues {
                address,
                registered: false,
                eligible: false,
                blocks: block_queue,
                fluids: fluid_queue,
            },
        );
        self.allocated_bytes = self.limits.allocation_bytes - remaining;
        Ok(())
    }

    /// Materializes each saved entry, including duplicate identities. Reserved
    /// queue capacity already includes pending entries, so this does not allocate
    /// or partially fail. Each domain assigns -N..-1 independently; no live
    /// sub-order is consumed. A second unpack does nothing.
    pub fn unpack_chunk(&mut self, address: ChunkAddress, game_time: i64) -> Result<(), TickError> {
        let index = self.find(address).map_err(|_| TickError::MissingChunk)?;
        for domain in [TickDomain::Block, TickDomain::Fluid] {
            let Some(pending) = self.chunks[index].queue_mut(domain).pending.take() else {
                continue;
            };
            let reserved = pending.capacity() * size_of::<SavedTick>();
            let first_order = -(pending.len() as i64);
            for (order, saved) in pending.iter().enumerate() {
                let tick = ScheduledTick {
                    position: saved.position,
                    type_id: saved.type_id,
                    trigger_tick: game_time.wrapping_add(i64::from(saved.delay)),
                    priority: saved.priority,
                    sub_tick_order: first_order + order as i64,
                };
                self.chunks[index].queue_mut(domain).heap.push(tick);
                if self.chunks[index].registered
                    && self.chunks[index].queue(domain).heap.peek() == Some(&tick)
                {
                    self.timeline_mut(domain)
                        .put(packed_chunk(address), tick.trigger_tick)
                        .expect("registered chunks fit index");
                }
            }
            drop(pending);
            self.allocated_bytes -= reserved;
        }
        Ok(())
    }

    /// Appends a non-consuming snapshot into caller-preallocated output. Pending
    /// entries retain original delay/order; live entries follow stable sub-order
    /// sorting of the heap array. Sort scratch is lazily admitted before any
    /// output is appended, then retained until release_operation_scratch().
    pub fn pack_chunk(
        &mut self,
        address: ChunkAddress,
        domain: TickDomain,
        game_time: i64,
        output: &mut Vec<SavedTick>,
    ) -> Result<usize, TickError> {
        let index = self.find(address).map_err(|_| TickError::MissingChunk)?;
        let count = self.chunks[index].queue(domain).count();
        if output.capacity() - output.len() < count {
            return Err(TickError::OutputCapacity);
        }
        self.ensure_operation_scratch(self.chunks[index].queue(domain).heap.len(), false)?;
        let queue = self.chunks[index].queue(domain);
        self.scratch.clear();
        for (sequence, tick) in queue.heap.as_slice().iter().enumerate() {
            self.scratch.push(SnapshotTick {
                tick: *tick,
                sequence,
                admitted: false,
            });
        }
        self.scratch
            .sort_unstable_by_key(|entry| (entry.tick.sub_tick_order, entry.sequence));
        if let Some(pending) = &queue.pending {
            output.extend_from_slice(pending);
        }
        for entry in &self.scratch {
            let tick = entry.tick;
            output.push(SavedTick {
                position: tick.position,
                type_id: tick.type_id,
                delay: tick.trigger_tick.wrapping_sub(game_time) as i32,
                priority: tick.priority,
            });
        }
        self.scratch.clear();
        Ok(count)
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
        self.block_index.remove(packed_chunk(address));
        self.fluid_index.remove(packed_chunk(address));
        Ok(())
    }

    /// Explicitly destroys retained queue data. Persistence must capture it first
    /// via pack_chunk before discard; durable world-save integration is separate.
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
        let tick = ScheduledTick {
            position,
            type_id,
            trigger_tick: game_time.wrapping_add(i64::from(delay)),
            priority,
            sub_tick_order,
        };
        self.schedule_created(domain, index, tick)
    }

    fn schedule_created(
        &mut self,
        domain: TickDomain,
        index: usize,
        tick: ScheduledTick,
    ) -> Result<ScheduleOutcome, TickError> {
        let queue = self.chunks[index].queue_mut(domain);
        if queue.identities.contains(tick.identity()) {
            return Ok(ScheduleOutcome::Duplicate);
        }
        if queue.count() == self.limits.queued_per_chunk {
            return Err(TickError::QueueFull);
        }
        queue.identities.insert(tick.identity());
        queue.heap.push(tick);
        if queue.heap.peek() == Some(&tick) {
            let address = self.chunks[index].address;
            self.timeline_mut(domain)
                .put(packed_chunk(address), tick.trigger_tick)
                .expect("registered chunks fit index");
        }
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

    /// Includes pending saved entries, excludes selected and detached entries.
    pub fn queued_count(&self, domain: TickDomain) -> usize {
        self.chunks
            .iter()
            .filter(|chunk| chunk.registered)
            .map(|chunk| chunk.queue(domain).count())
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
        #[cfg(test)]
        {
            self.selected_rebuilt_entries = 0;
        }
        self.timeline_mut(domain).begin_scan();
        while let Some((key, next_time)) = self.timeline_mut(domain).next_entry() {
            if next_time > game_time {
                continue;
            }
            let address = unpacked_chunk(key);
            let Ok(index) = self.registered(address) else {
                self.timeline_mut(domain).remove_current();
                continue;
            };
            let Some(tick) = self.chunks[index].queue(domain).heap.peek().copied() else {
                self.timeline_mut(domain).remove_current();
                continue;
            };
            if tick.trigger_tick > game_time {
                self.timeline_mut(domain)
                    .put(key, tick.trigger_tick)
                    .expect("existing key update");
            } else if self.chunks[index].eligible {
                self.timeline_mut(domain).remove_current();
                self.ready.push(ReadyChunk {
                    index,
                    priority: tick.priority,
                    sub_order: tick.sub_tick_order,
                });
            }
        }
        self.timeline_mut(domain).finish_scan();
        while self.selected.len() < max_ticks {
            let Some(candidate) = self.ready.pop() else {
                break;
            };
            loop {
                let tick = self.chunks[candidate.index]
                    .queue_mut(domain)
                    .pop()
                    .expect("ready chunk contains tick");
                self.remaining_ids.add(tick.identity());
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
                .copied()
            {
                if tick.trigger_tick <= game_time && self.selected.len() < max_ticks {
                    self.ready.push(ReadyChunk {
                        index: candidate.index,
                        priority: tick.priority,
                        sub_order: tick.sub_tick_order,
                    });
                } else {
                    let address = self.chunks[candidate.index].address;
                    self.timeline_mut(domain)
                        .put(packed_chunk(address), tick.trigger_tick)
                        .expect("registered chunks fit index");
                }
            }
        }
        // Reinsert in the ready heap's array order, including cap-zero phases.
        // With equal keys this changes the later scheduling-index history.
        for position in 0..self.ready.as_slice().len() {
            let index = self.ready.as_slice()[position].index;
            let address = self.chunks[index].address;
            let next = self.chunks[index]
                .queue(domain)
                .heap
                .peek()
                .expect("ready chunk contains tick")
                .trigger_tick;
            self.timeline_mut(domain)
                .put(packed_chunk(address), next)
                .expect("registered chunks fit index");
        }
        self.ready.clear();
        Ok(self.selected.len())
    }

    pub fn will_tick_this_phase(
        &mut self,
        domain: TickDomain,
        position: TickPosition,
        type_id: u32,
    ) -> bool {
        if self.active != Some(domain) {
            return false;
        }
        if self.selected_ids.count == 0 && self.cursor < self.selected.len() {
            for identity in self.remaining_ids.identities() {
                self.selected_ids.insert(identity);
                #[cfg(test)]
                {
                    self.selected_rebuilt_entries += 1;
                }
            }
        }
        self.selected_ids.contains(Identity { position, type_id })
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
        self.remaining_ids.remove(tick.identity());
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
        self.remaining_ids.clear();
        self.cursor = 0;
        self.active = None;
        Ok(())
    }

    /// Clears live queued, remaining selected and already-returned entries in an
    /// inclusive area. Pending saved entries are intentionally untouched. A lazy
    /// will-tick set already observed by the caller retains Vanilla's stale
    /// membership until later removals or phase cleanup; it is not repaired here.
    pub fn clear_area(
        &mut self,
        domain: TickDomain,
        bounds: TickBounds,
    ) -> Result<usize, TickError> {
        bounds.validate()?;
        let mut removed = 0;
        for index in 0..self.chunks.len() {
            if !self.chunks[index].registered || !bounds.contains_chunk(self.chunks[index].address)
            {
                continue;
            }
            let queue = self.chunks[index].queue_mut(domain);
            let old_head = queue.heap.peek().copied();
            let identities = &mut queue.identities;
            removed += queue.heap.remove_if(|tick| {
                if bounds.contains(tick.position) {
                    identities.remove(tick.identity());
                    true
                } else {
                    false
                }
            });
            let new_head = queue.heap.peek().copied();
            if old_head != new_head {
                let key = packed_chunk(self.chunks[index].address);
                if let Some(tick) = new_head {
                    self.timeline_mut(domain)
                        .put(key, tick.trigger_tick)
                        .expect("registered chunks fit index");
                } else {
                    self.timeline_mut(domain).remove(key);
                }
            }
        }
        if self.active == Some(domain) {
            let original_cursor = self.cursor;
            let mut position = 0;
            self.selected.retain(|tick| {
                let keep = !bounds.contains(tick.position);
                if !keep {
                    removed += 1;
                    if position < original_cursor {
                        self.cursor -= 1;
                    } else {
                        self.remaining_ids.remove(tick.identity());
                    }
                }
                position += 1;
                keep
            });
        }
        Ok(removed)
    }

    /// Copies a fixed snapshot once, so newly copied entries cannot recursively
    /// become sources. Admission and identity decisions are checked for the whole
    /// batch before mutation; Arrow resource errors therefore leave queues intact.
    pub fn copy_area(
        &mut self,
        domain: TickDomain,
        bounds: TickBounds,
        offset: TickPosition,
    ) -> Result<CopyOutcome, TickError> {
        bounds.validate()?;
        let selected = if self.active == Some(domain) {
            &self.selected[..]
        } else {
            &[]
        };
        let needed = snapshot_count(&self.chunks, selected, domain, bounds)?;
        self.ensure_operation_scratch(needed, true)?;
        let selected = if self.active == Some(domain) {
            &self.selected[..]
        } else {
            &[]
        };
        gather_snapshot(&self.chunks, selected, domain, bounds, &mut self.scratch)?;
        self.commit_copy(domain, offset)
    }

    pub fn copy_area_from(
        &mut self,
        source: &Self,
        domain: TickDomain,
        bounds: TickBounds,
        offset: TickPosition,
    ) -> Result<CopyOutcome, TickError> {
        bounds.validate()?;
        let selected = if source.active == Some(domain) {
            &source.selected[..]
        } else {
            &[]
        };
        let needed = snapshot_count(&source.chunks, selected, domain, bounds)?;
        self.ensure_operation_scratch(needed, true)?;
        gather_snapshot(&source.chunks, selected, domain, bounds, &mut self.scratch)?;
        self.commit_copy(domain, offset)
    }

    /// Releases retained pack/copy workspace without touching queue, pending or
    /// selected data. Live-only owners never allocate this rare-operation space.
    pub fn release_operation_scratch(&mut self) {
        let bytes = self.scratch.capacity() * size_of::<SnapshotTick>()
            + self.copy_ids.as_ref().map_or(0, Identities::heap_bytes)
            + self.copy_counts.capacity() * size_of::<usize>();
        drop(std::mem::take(&mut self.scratch));
        drop(self.copy_ids.take());
        drop(std::mem::take(&mut self.copy_counts));
        self.copy_capacity = 0;
        self.allocated_bytes -= bytes;
    }

    fn ensure_operation_scratch(&mut self, needed: usize, copying: bool) -> Result<(), TickError> {
        if needed > self.scratch_limit {
            return Err(TickError::AllocationBudget);
        }
        if needed == 0 {
            return Ok(());
        }
        // Old buffers remain charged while every replacement is admitted. A
        // later allocation failure drops new locals and preserves old workspace.
        let mut remaining = self.limits.allocation_bytes - self.allocated_bytes;
        let replacement = if needed > self.scratch.capacity() {
            Some(reserved_vec(needed, &mut remaining)?)
        } else {
            None
        };
        let identities = if copying && needed > self.copy_capacity {
            Some(Identities::new(needed, &mut remaining)?)
        } else {
            None
        };
        let counts = if copying && self.chunks.len() > self.copy_counts.capacity() {
            Some(reserved_vec(self.chunks.len(), &mut remaining)?)
        } else {
            None
        };
        let mut freed = 0;
        if let Some(values) = replacement {
            freed += self.scratch.capacity() * size_of::<SnapshotTick>();
            drop(std::mem::replace(&mut self.scratch, values));
        }
        if let Some(identities) = identities {
            freed += self.copy_ids.as_ref().map_or(0, Identities::heap_bytes);
            drop(self.copy_ids.replace(identities));
            self.copy_capacity = needed;
        }
        if let Some(counts) = counts {
            freed += self.copy_counts.capacity() * size_of::<usize>();
            drop(std::mem::replace(&mut self.copy_counts, counts));
        }
        if copying {
            self.copy_counts.resize(self.chunks.len(), 0);
        }
        self.allocated_bytes = self.limits.allocation_bytes - remaining - freed;
        Ok(())
    }

    fn commit_copy(
        &mut self,
        domain: TickDomain,
        offset: TickPosition,
    ) -> Result<CopyOutcome, TickError> {
        let Some(minimum) = self
            .scratch
            .iter()
            .map(|entry| entry.tick.sub_tick_order)
            .min()
        else {
            return Ok(CopyOutcome::default());
        };
        let maximum = self
            .scratch
            .iter()
            .map(|entry| entry.tick.sub_tick_order)
            .max()
            .unwrap();
        let copy_ids = self.copy_ids.as_mut().expect("copy scratch admitted");
        copy_ids.clear();
        self.copy_counts.fill(0);
        let type_count = match domain {
            TickDomain::Block => self.block_types,
            TickDomain::Fluid => self.fluid_types,
        };
        let mut outcome = CopyOutcome::default();
        for entry in &mut self.scratch {
            let tick = &mut entry.tick;
            tick.position = TickPosition {
                x: tick.position.x.wrapping_add(offset.x),
                y: tick.position.y.wrapping_add(offset.y),
                z: tick.position.z.wrapping_add(offset.z),
            };
            tick.sub_tick_order = tick
                .sub_tick_order
                .wrapping_sub(minimum)
                .wrapping_add(maximum)
                .wrapping_add(1);
            let address = tick.position.chunk();
            let Ok(index) = self
                .chunks
                .binary_search_by_key(&address, |chunk| chunk.address)
            else {
                outcome.missing_containers += 1;
                continue;
            };
            if !self.chunks[index].registered {
                outcome.missing_containers += 1;
                continue;
            }
            if tick.type_id >= type_count {
                return Err(TickError::InvalidType);
            }
            let queue = self.chunks[index].queue(domain);
            if queue.identities.contains(tick.identity()) || copy_ids.contains(tick.identity()) {
                outcome.duplicates += 1;
                continue;
            }
            if queue.count() + self.copy_counts[index] >= self.limits.queued_per_chunk {
                return Err(TickError::QueueFull);
            }
            self.copy_counts[index] += 1;
            copy_ids.insert(tick.identity());
            entry.admitted = true;
            outcome.added += 1;
        }
        for position in 0..self.scratch.len() {
            let entry = self.scratch[position];
            if entry.admitted {
                let index = self
                    .registered(entry.tick.position.chunk())
                    .expect("copy admission validated chunk");
                let result = self
                    .schedule_created(domain, index, entry.tick)
                    .expect("copy batch was admitted");
                debug_assert_eq!(result, ScheduleOutcome::Added);
            }
        }
        self.copy_ids.as_mut().unwrap().clear();
        self.scratch.clear();
        Ok(outcome)
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

    fn timeline_mut(&mut self, domain: TickDomain) -> &mut SchedulingIndex {
        match domain {
            TickDomain::Block => &mut self.block_index,
            TickDomain::Fluid => &mut self.fluid_index,
        }
    }

    fn update_head(&mut self, address: ChunkAddress, domain: TickDomain) {
        let Ok(index) = self.registered(address) else {
            return;
        };
        if let Some(time) = self.chunks[index]
            .queue(domain)
            .heap
            .peek()
            .map(|tick| tick.trigger_tick)
        {
            self.timeline_mut(domain)
                .put(packed_chunk(address), time)
                .expect("registered chunks fit index");
        }
    }
}

fn packed_chunk(address: ChunkAddress) -> i64 {
    ((address.z as u32 as u64) << 32 | address.x as u32 as u64) as i64
}
fn unpacked_chunk(key: i64) -> ChunkAddress {
    ChunkAddress {
        x: key as i32,
        z: ((key as u64) >> 32) as i32,
    }
}

fn gather_snapshot(
    chunks: &[ChunkQueues],
    selected: &[ScheduledTick],
    domain: TickDomain,
    bounds: TickBounds,
    output: &mut Vec<SnapshotTick>,
) -> Result<(), TickError> {
    output.clear();
    let needed = snapshot_count(chunks, selected, domain, bounds)?;
    if output.capacity() < needed {
        return Err(TickError::AllocationBudget);
    }
    // selected keeps the already-returned prefix followed by remaining actions.
    for tick in selected
        .iter()
        .filter(|tick| bounds.contains(tick.position))
    {
        output.push(SnapshotTick {
            tick: *tick,
            sequence: output.len(),
            admitted: false,
        });
    }
    // ChunkAddress's x-then-z order matches the area's nested chunk traversal.
    for chunk in chunks
        .iter()
        .filter(|chunk| chunk.registered && bounds.contains_chunk(chunk.address))
    {
        for tick in chunk
            .queue(domain)
            .heap
            .as_slice()
            .iter()
            .filter(|tick| bounds.contains(tick.position))
        {
            output.push(SnapshotTick {
                tick: *tick,
                sequence: output.len(),
                admitted: false,
            });
        }
    }
    Ok(())
}

fn snapshot_count(
    chunks: &[ChunkQueues],
    selected: &[ScheduledTick],
    domain: TickDomain,
    bounds: TickBounds,
) -> Result<usize, TickError> {
    let selected_count = selected
        .iter()
        .filter(|tick| bounds.contains(tick.position))
        .count();
    let queued_count = chunks
        .iter()
        .filter(|chunk| chunk.registered && bounds.contains_chunk(chunk.address))
        .map(|chunk| {
            chunk
                .queue(domain)
                .heap
                .as_slice()
                .iter()
                .filter(|tick| bounds.contains(tick.position))
                .count()
        })
        .try_fold(0usize, usize::checked_add)
        .ok_or(TickError::AllocationBudget)?;
    selected_count
        .checked_add(queued_count)
        .ok_or(TickError::AllocationBudget)
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

    #[test]
    fn copied_extreme_suborders_wrap_without_consuming_live_counter() {
        fn owner() -> ScheduledTickOwner {
            ScheduledTickOwner::new(
                8,
                8,
                TickLimits {
                    max_chunks: 2,
                    queued_per_chunk: 8,
                    selected_per_phase: 8,
                    allocation_bytes: 65536,
                },
            )
            .unwrap()
        }
        for original in [[i64::MAX - 1, i64::MAX], [i64::MIN, i64::MAX]] {
            let mut source = owner();
            let mut target = owner();
            source
                .register_chunk(ChunkAddress { x: 0, z: 0 }, true)
                .unwrap();
            target
                .register_chunk(ChunkAddress { x: 2, z: 0 }, true)
                .unwrap();
            for (index, order) in original.into_iter().enumerate() {
                source
                    .schedule_created(
                        TickDomain::Block,
                        0,
                        ScheduledTick {
                            position: TickPosition {
                                x: index as i32 + 1,
                                y: 64,
                                z: 0,
                            },
                            type_id: index as u32 + 1,
                            trigger_tick: 5,
                            priority: TickPriority::Normal,
                            sub_tick_order: order,
                        },
                    )
                    .unwrap();
            }
            let outcome = target
                .copy_area_from(
                    &source,
                    TickDomain::Block,
                    TickBounds {
                        min: TickPosition { x: 0, y: 0, z: 0 },
                        max: TickPosition {
                            x: 15,
                            y: 128,
                            z: 15,
                        },
                    },
                    TickPosition { x: 32, y: 0, z: 0 },
                )
                .unwrap();
            assert_eq!(outcome.added, 2);
            assert_eq!(target.next_sub_tick_order(), 0);
            target.begin_phase(TickDomain::Block, 5, 8).unwrap();
            let first = target.next_due().unwrap().unwrap();
            let second = target.next_due().unwrap().unwrap();
            assert_eq!(first.sub_tick_order, i64::MIN);
            assert_eq!(
                second.sub_tick_order,
                if original[0] == i64::MIN {
                    i64::MAX
                } else {
                    i64::MIN + 1
                }
            );
            target.finish_phase().unwrap();
            assert_eq!(source.queued_count(TickDomain::Block), 2);
        }
    }

    #[test]
    fn packed_equal_suborders_preserve_heap_array_order_without_sort_allocation() {
        let mut owner = ScheduledTickOwner::new(
            8,
            8,
            TickLimits {
                max_chunks: 1,
                queued_per_chunk: 8,
                selected_per_phase: 8,
                allocation_bytes: 65536,
            },
        )
        .unwrap();
        let chunk = ChunkAddress { x: 0, z: 0 };
        owner.register_chunk(chunk, true).unwrap();
        for (type_id, time) in [(1, 50), (2, 10), (3, 30)] {
            owner
                .schedule_created(
                    TickDomain::Block,
                    0,
                    ScheduledTick {
                        position: TickPosition {
                            x: type_id as i32,
                            y: 64,
                            z: 0,
                        },
                        type_id,
                        trigger_tick: time,
                        priority: TickPriority::Normal,
                        sub_tick_order: 0,
                    },
                )
                .unwrap();
        }
        let mut output = Vec::with_capacity(3);
        let allocation = owner.retained_heap_bytes();
        owner
            .pack_chunk(chunk, TickDomain::Block, 0, &mut output)
            .unwrap();
        assert_eq!(
            output
                .iter()
                .map(|tick| (tick.type_id, tick.delay))
                .collect::<Vec<_>>(),
            [(2, 10), (1, 50), (3, 30)]
        );
        assert!(owner.retained_heap_bytes() > allocation);
        owner.release_operation_scratch();
        assert_eq!(owner.retained_heap_bytes(), allocation);
        assert_eq!(owner.queued_count(TickDomain::Block), 3);
    }

    #[test]
    fn repeated_saved_duplicate_queries_do_not_rescan_the_remaining_suffix() {
        const COUNT: usize = 2048;
        let mut owner = ScheduledTickOwner::new(
            8,
            8,
            TickLimits {
                max_chunks: 1,
                queued_per_chunk: COUNT,
                selected_per_phase: COUNT,
                allocation_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        let chunk = ChunkAddress { x: 0, z: 0 };
        let position = TickPosition { x: 1, y: 64, z: 0 };
        let pending = vec![
            SavedTick {
                position,
                type_id: 1,
                delay: 0,
                priority: TickPriority::Normal
            };
            COUNT
        ];
        owner.load_pending_chunk(chunk, &pending, &[]).unwrap();
        owner.register_chunk(chunk, true).unwrap();
        owner.unpack_chunk(chunk, 0).unwrap();
        owner.begin_phase(TickDomain::Block, 0, COUNT).unwrap();
        assert!(owner.will_tick_this_phase(TickDomain::Block, position, 1));
        for remaining in (0..COUNT).rev() {
            assert_eq!(owner.next_due().unwrap().unwrap().type_id, 1);
            assert_eq!(
                owner.will_tick_this_phase(TickDomain::Block, position, 1),
                remaining != 0
            );
        }
        assert_eq!(owner.selected_rebuilt_entries, COUNT);
        owner.finish_phase().unwrap();
        for id in 2..=3 {
            owner
                .schedule(
                    TickDomain::Block,
                    TickPosition {
                        x: id as i32,
                        ..position
                    },
                    id,
                    0,
                    0,
                    TickPriority::Normal,
                )
                .unwrap();
        }
        owner.begin_phase(TickDomain::Block, 0, COUNT).unwrap();
        for id in 2..=3 {
            assert!(owner.will_tick_this_phase(
                TickDomain::Block,
                TickPosition {
                    x: id as i32,
                    ..position
                },
                id
            ));
        }
        assert_eq!(owner.selected_rebuilt_entries, 2);
    }

    #[test]
    fn alternating_duplicate_identities_have_amortized_linear_rebuild_work() {
        const COUNT: usize = 2048;
        for distinct in [2, 4] {
            let mut owner = ScheduledTickOwner::new(
                8,
                8,
                TickLimits {
                    max_chunks: 1,
                    queued_per_chunk: COUNT,
                    selected_per_phase: COUNT,
                    allocation_bytes: 1024 * 1024,
                },
            )
            .unwrap();
            let chunk = ChunkAddress { x: 0, z: 0 };
            let pending: Vec<_> = (0..COUNT)
                .map(|index| {
                    let id = index as u32 % distinct + 1;
                    SavedTick {
                        position: TickPosition {
                            x: id as i32,
                            y: 64,
                            z: 0,
                        },
                        type_id: id,
                        delay: 0,
                        priority: TickPriority::Normal,
                    }
                })
                .collect();
            owner.load_pending_chunk(chunk, &pending, &[]).unwrap();
            owner.register_chunk(chunk, true).unwrap();
            owner.unpack_chunk(chunk, 0).unwrap();
            owner.begin_phase(TickDomain::Block, 0, COUNT).unwrap();
            while owner.next_due().unwrap().is_some() {
                for id in 1..=distinct {
                    owner.will_tick_this_phase(
                        TickDomain::Block,
                        TickPosition {
                            x: id as i32,
                            y: 64,
                            z: 0,
                        },
                        id,
                    );
                }
            }
            assert!(
                owner.selected_rebuilt_entries <= COUNT,
                "rebuild revisited {} entries",
                owner.selected_rebuilt_entries
            );
            owner.finish_phase().unwrap();
            assert_eq!(owner.queued_count(TickDomain::Block), 0);
        }
    }
}
