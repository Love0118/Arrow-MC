//! Per-player chunk scheduling and bounded, ordered delivery ownership.
//!
//! Requirements come from PlayerChunkSender, ChunkHolder and ChunkPos in the
//! locked 26.3-pre-2 reference, with independently authored JVM observations of
//! its bundled collections. This is a separately designed synchronous owner;
//! it does not encode chunks, activate Play, or make a worker result send-ready.

use std::collections::VecDeque;
use std::fmt;

use crate::world::preparation::ChunkAddress;

pub const MIN_CHUNKS_PER_TICK: f32 = 0.01;
pub const MAX_CHUNKS_PER_TICK: f32 = 64.0;
const ZERO: ChunkAddress = ChunkAddress { x: 0, z: 0 };
const INITIAL_BUCKETS: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct SenderLimits {
    pub max_pending: usize,
    /// Both physical tables and selection/sort scratch, including spare capacity.
    pub control_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct DeliveryLimits {
    /// Includes the group whose packet is currently being written.
    pub max_groups: usize,
    /// Queue metadata, packet bytes and per-chunk spans, including spare capacity.
    /// Borrowed source/cache buffers have a separate owner and budget.
    pub max_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidLimits,
    AllocationFailed,
    ControlBudget,
    PendingFull,
    DeliveryFull,
    DeliveryBytes,
    Closed,
    TickAlreadyStarted,
    InvalidReadiness,
    AlreadyAdmitted,
    NoPacket,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chunk sender: {self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SenderStats {
    pub pending: usize,
    pub desired_chunks_per_tick: f32,
    pub batch_quota: f32,
    pub unacknowledged_batches: u8,
    pub max_unacknowledged_batches: u8,
    pub control_bytes: usize,
}

/// The caller asserts that the authoritative chunk has completed send-sync and
/// has a ticking LevelChunk equivalent, and that these complete packet bytes
/// describe that current chunk. Section preparation/cache availability alone is
/// insufficient. Keep the world snapshot/owner valid throughout synchronous
/// admission. The bytes remain opaque here; no chunk codec is claimed.
#[derive(Clone, Copy, Debug)]
pub struct SendReadyChunk<'a> {
    pub position: ChunkAddress,
    pub packet_bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionOutcome {
    NoReadyChunks,
    Admitted { chunks: usize, packet_bytes: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropOutcome {
    RemovedPending,
    NoPacket,
    ForgetQueued,
}

pub struct ChunkSender {
    pending: PendingPositions,
    candidates: Vec<ChunkAddress>,
    sort_scratch: Vec<ChunkAddress>,
    memory_connection: bool,
    desired: f32,
    quota: f32,
    unacknowledged: u8,
    max_unacknowledged: u8,
    last_tick: Option<u64>,
}

impl ChunkSender {
    pub fn new(memory_connection: bool, limits: SenderLimits) -> Result<Self, Error> {
        if limits.max_pending == 0 || limits.max_pending > 3 * (1 << 28) {
            return Err(Error::InvalidLimits);
        }
        let buckets = (limits.max_pending.div_ceil(3) * 4)
            .next_power_of_two()
            .max(INITIAL_BUCKETS);
        let selection = if memory_connection {
            limits.max_pending
        } else {
            limits.max_pending.min(64)
        };
        let mut remaining = limits.control_bytes;
        let slots = reserve::<u64>(buckets, &mut remaining, Error::ControlBudget)?;
        let spare = reserve::<u64>(buckets, &mut remaining, Error::ControlBudget)?;
        let candidates = reserve(selection, &mut remaining, Error::ControlBudget)?;
        let mut sort_scratch = reserve(selection, &mut remaining, Error::ControlBudget)?;
        sort_scratch.resize(selection, ZERO);
        Ok(Self {
            pending: PendingPositions::new(slots, spare, buckets, limits.max_pending),
            candidates,
            sort_scratch,
            memory_connection,
            desired: 9.0,
            quota: 0.0,
            unacknowledged: 0,
            max_unacknowledged: 1,
            last_tick: None,
        })
    }

    pub fn stats(&self) -> SenderStats {
        SenderStats {
            pending: self.pending.len,
            desired_chunks_per_tick: self.desired,
            batch_quota: self.quota,
            unacknowledged_batches: self.unacknowledged,
            max_unacknowledged_batches: self.max_unacknowledged,
            control_bytes: (self.pending.slots.capacity() + self.pending.spare.capacity())
                * size_of::<u64>()
                + (self.candidates.capacity() + self.sort_scratch.capacity())
                    * size_of::<ChunkAddress>(),
        }
    }

    pub fn is_pending(&self, position: ChunkAddress) -> bool {
        self.pending.contains(pack(position))
    }

    /// Duplicate marks do not change set history or consume additional capacity.
    pub fn mark_pending(&mut self, position: ChunkAddress) -> Result<bool, Error> {
        self.pending.insert(pack(position))
    }

    /// Absent + alive queues Forget even if this sender never sent the position.
    /// A pending removal suppresses Forget. Failed queue admission is explicit.
    pub fn drop_chunk(
        &mut self,
        position: ChunkAddress,
        alive: bool,
        delivery: &mut ChunkDeliveryQueue,
    ) -> Result<DropOutcome, Error> {
        if self.pending.remove(pack(position)) {
            return Ok(DropOutcome::RemovedPending);
        }
        if !alive {
            return Ok(DropOutcome::NoPacket);
        }
        delivery.check_group()?;
        delivery.groups.push_back(Group::Forget(position));
        Ok(DropOutcome::ForgetQueued)
    }

    /// ACK is accepted even with no outstanding batches, as in the baseline.
    pub fn acknowledge(&mut self, requested_chunks_per_tick: f32) {
        self.unacknowledged = self.unacknowledged.saturating_sub(1);
        self.desired = if requested_chunks_per_tick.is_nan() {
            MIN_CHUNKS_PER_TICK
        } else {
            requested_chunks_per_tick.clamp(MIN_CHUNKS_PER_TICK, MAX_CHUNKS_PER_TICK)
        };
        if self.unacknowledged == 0 {
            self.quota = 1.0;
        }
        self.max_unacknowledged = 10;
    }

    /// Call once per actual owner tick with a strictly increasing tick ID.
    /// Admission can be retried on the returned plan without accruing quota
    /// twice. Dropping a plan retains tick accrual but commits no delivery.
    /// Keep this scoped synchronous borrow out of socket/worker awaits.
    pub fn begin_tick(&mut self, tick: u64, center: ChunkAddress) -> Result<TickPlan<'_>, Error> {
        if self.last_tick.is_some_and(|previous| tick <= previous) {
            return Err(Error::TickAlreadyStarted);
        }
        self.last_tick = Some(tick);
        self.candidates.clear();
        if self.unacknowledged < self.max_unacknowledged {
            self.quota = (self.quota + self.desired).min(self.desired.max(1.0));
            if self.quota >= 1.0 {
                let count = self.quota.floor() as usize;
                if !self.memory_connection && self.pending.len > count {
                    let mut least = Least::new(count, center);
                    for position in self.pending.stream() {
                        least.offer(position);
                    }
                    self.candidates.extend_from_slice(least.finish());
                } else {
                    self.candidates.extend(self.pending.stream());
                    // Filtering unavailable entries after a stable full sort is
                    // equivalent to filtering first, including equal distances.
                    stable_sort(&mut self.candidates, &mut self.sort_scratch, center);
                }
            }
        }
        Ok(TickPlan {
            sender: self,
            admitted: false,
        })
    }
}

pub struct TickPlan<'a> {
    sender: &'a mut ChunkSender,
    admitted: bool,
}

impl TickPlan<'_> {
    /// Remote candidates are capped BEFORE readiness filtering. Never replace
    /// an unavailable entry with a farther ready chunk. A memory connection
    /// selects all pending positions regardless of the current quota count.
    pub fn candidates(&self) -> &[ChunkAddress] {
        &self.sender.candidates
    }

    /// Each candidate needs one corresponding readiness entry. Admission owns
    /// the entire Start/Data/Finish group before removing any pending position.
    /// Full/byte/allocation errors retain candidates, quota and ACK accounting;
    /// only the tick accrual from begin_tick has already happened.
    pub fn try_admit(
        &mut self,
        delivery: &mut ChunkDeliveryQueue,
        ready: &[Option<SendReadyChunk<'_>>],
    ) -> Result<AdmissionOutcome, Error> {
        if self.admitted {
            return Err(Error::AlreadyAdmitted);
        }
        if ready.len() != self.candidates().len() {
            return Err(Error::InvalidReadiness);
        }
        let mut count = 0usize;
        let mut bytes = 0usize;
        for (position, chunk) in self.candidates().iter().zip(ready) {
            if let Some(chunk) = chunk {
                if chunk.position != *position || chunk.packet_bytes.is_empty() {
                    return Err(Error::InvalidReadiness);
                }
                count += 1;
                bytes = bytes
                    .checked_add(chunk.packet_bytes.len())
                    .ok_or(Error::DeliveryBytes)?;
            }
        }
        if count == 0 {
            return Ok(AdmissionOutcome::NoReadyChunks);
        }
        delivery.check_group()?;
        let mut remaining = delivery.limits.max_bytes - delivery.used_bytes;
        // Check the combined request before either allocation, including the
        // transient source+destination copy through separate owner budgets.
        let required = count
            .checked_mul(size_of::<Span>())
            .and_then(|metadata| metadata.checked_add(bytes))
            .ok_or(Error::DeliveryBytes)?;
        if required > remaining {
            return Err(Error::DeliveryBytes);
        }
        let mut spans = reserve(count, &mut remaining, Error::DeliveryBytes)?;
        let mut data = reserve(bytes, &mut remaining, Error::DeliveryBytes)?;
        for chunk in ready.iter().flatten() {
            let start = data.len();
            data.extend_from_slice(chunk.packet_bytes);
            spans.push(Span {
                position: chunk.position,
                start,
                len: chunk.packet_bytes.len(),
            });
        }
        let batch = Batch {
            spans,
            data,
            cursor: 0,
        };
        delivery.used_bytes += batch.heap_bytes();
        delivery.groups.push_back(Group::Batch(batch));
        // All fallible operations precede publication and sender state changes.
        for chunk in ready.iter().flatten() {
            let removed = self.sender.pending.remove(pack(chunk.position));
            debug_assert!(removed);
        }
        self.sender.unacknowledged += 1;
        self.sender.quota -= count as f32;
        self.admitted = true;
        Ok(AdmissionOutcome::Admitted {
            chunks: count,
            packet_bytes: bytes,
        })
    }
}

struct Span {
    position: ChunkAddress,
    start: usize,
    len: usize,
}

struct Batch {
    spans: Vec<Span>,
    data: Vec<u8>,
    // 0 = Start, 1..=count = Data, count+1 = Finish.
    cursor: usize,
}

impl Batch {
    fn heap_bytes(&self) -> usize {
        self.spans.capacity() * size_of::<Span>() + self.data.capacity()
    }
}

enum Group {
    Batch(Batch),
    Forget(ChunkAddress),
}

/// Typed packet intent. Start and Finish still require the future Play packet
/// encoder; Data contains caller-supplied complete packet bytes. Ordering and
/// retained ownership are implemented without inventing a chunk body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkPacket<'a> {
    Start,
    Data {
        position: ChunkAddress,
        packet_bytes: &'a [u8],
    },
    Finish {
        chunks: usize,
    },
    Forget {
        position: ChunkAddress,
    },
}

/// One connection's bounded delivery owner. Borrow front_packet across a write;
/// call packet_written only after its successful ordered write. All batch bytes
/// remain charged until Finish completes. Any write failure/cancellation must
/// close that connection and call fail (or drop the entire owner). Never retry
/// a partially written batch on a surviving connection.
pub struct ChunkDeliveryQueue {
    limits: DeliveryLimits,
    groups: VecDeque<Group>,
    used_bytes: usize,
    closed: bool,
}

impl ChunkDeliveryQueue {
    pub fn new(limits: DeliveryLimits) -> Result<Self, Error> {
        if limits.max_groups == 0 {
            return Err(Error::InvalidLimits);
        }
        let requested = limits
            .max_groups
            .checked_mul(size_of::<Group>())
            .ok_or(Error::InvalidLimits)?;
        if requested > limits.max_bytes {
            return Err(Error::DeliveryBytes);
        }
        let mut groups = VecDeque::new();
        groups
            .try_reserve_exact(limits.max_groups)
            .map_err(|_| Error::AllocationFailed)?;
        let used_bytes = groups.capacity() * size_of::<Group>();
        if used_bytes > limits.max_bytes {
            return Err(Error::DeliveryBytes);
        }
        Ok(Self {
            limits,
            groups,
            used_bytes,
            closed: false,
        })
    }

    pub fn retained_bytes(&self) -> usize {
        self.used_bytes
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn front_packet(&self) -> Option<ChunkPacket<'_>> {
        match self.groups.front()? {
            Group::Forget(position) => Some(ChunkPacket::Forget {
                position: *position,
            }),
            Group::Batch(batch) => Some(if batch.cursor == 0 {
                ChunkPacket::Start
            } else if let Some(span) = batch.spans.get(batch.cursor - 1) {
                ChunkPacket::Data {
                    position: span.position,
                    packet_bytes: &batch.data[span.start..span.start + span.len],
                }
            } else {
                ChunkPacket::Finish {
                    chunks: batch.spans.len(),
                }
            }),
        }
    }

    pub fn packet_written(&mut self) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        let completed = match self.groups.front_mut().ok_or(Error::NoPacket)? {
            Group::Forget(_) => true,
            Group::Batch(batch) => {
                batch.cursor += 1;
                batch.cursor == batch.spans.len() + 2
            }
        };
        if completed && let Some(Group::Batch(batch)) = self.groups.pop_front() {
            self.used_bytes -= batch.heap_bytes();
        }
        Ok(())
    }

    pub fn fail(&mut self) {
        self.closed = true;
        self.groups.clear();
        self.used_bytes = self.groups.capacity() * size_of::<Group>();
    }

    fn check_group(&self) -> Result<(), Error> {
        if self.closed {
            Err(Error::Closed)
        } else if self.groups.len() == self.limits.max_groups {
            Err(Error::DeliveryFull)
        } else {
            Ok(())
        }
    }
}

fn reserve<T>(count: usize, remaining: &mut usize, error: Error) -> Result<Vec<T>, Error> {
    let requested = count.checked_mul(size_of::<T>()).ok_or(error)?;
    if requested > *remaining {
        return Err(error);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| Error::AllocationFailed)?;
    let actual = output.capacity().checked_mul(size_of::<T>()).ok_or(error)?;
    *remaining = remaining.checked_sub(actual).ok_or(error)?;
    Ok(output)
}

fn pack(position: ChunkAddress) -> u64 {
    u64::from(position.x as u32) | (u64::from(position.z as u32) << 32)
}

fn unpack(key: u64) -> ChunkAddress {
    ChunkAddress {
        x: key as i32,
        z: (key >> 32) as i32,
    }
}

fn distance(position: ChunkAddress, center: ChunkAddress) -> i32 {
    let x = position.x.wrapping_sub(center.x);
    let z = position.z.wrapping_sub(center.z);
    x.wrapping_mul(x).wrapping_add(z.wrapping_mul(z))
}

// Concrete compatibility storage. Physical capacity is admitted once; logical
// bucket count preserves historical placement, including shrink and zero-key
// handling. Sender streams traverse buckets ascending, unlike set iterators.
struct PendingPositions {
    slots: Vec<u64>,
    spare: Vec<u64>,
    buckets: usize,
    limit: usize,
    len: usize,
    zero: bool,
}

impl PendingPositions {
    fn new(mut slots: Vec<u64>, mut spare: Vec<u64>, physical: usize, limit: usize) -> Self {
        slots.resize(physical, 0);
        spare.resize(physical, 0);
        Self {
            slots,
            spare,
            buckets: INITIAL_BUCKETS,
            limit,
            len: 0,
            zero: false,
        }
    }

    fn home(key: u64, mask: usize) -> usize {
        let product = key.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let mixed = product ^ (product >> 32);
        (mixed ^ (mixed >> 16)) as usize & mask
    }

    fn bucket(&self, key: u64) -> usize {
        let mask = self.buckets - 1;
        let mut index = Self::home(key, mask);
        while self.slots[index] != 0 && self.slots[index] != key {
            index = (index + 1) & mask;
        }
        index
    }

    fn contains(&self, key: u64) -> bool {
        if key == 0 {
            self.zero
        } else {
            self.slots[self.bucket(key)] != 0
        }
    }

    fn insert(&mut self, key: u64) -> Result<bool, Error> {
        if self.contains(key) {
            return Ok(false);
        }
        if self.len == self.limit {
            return Err(Error::PendingFull);
        }
        if key == 0 {
            self.zero = true;
        } else {
            let index = self.bucket(key);
            self.slots[index] = key;
        }
        self.len += 1;
        if self.len > self.buckets * 3 / 4 {
            self.rehash(self.buckets * 2);
        }
        Ok(true)
    }

    fn remove(&mut self, key: u64) -> bool {
        if !self.contains(key) {
            return false;
        }
        if key == 0 {
            self.zero = false;
        } else {
            let mask = self.buckets - 1;
            let mut hole = self.bucket(key);
            let mut scan = (hole + 1) & mask;
            while self.slots[scan] != 0 {
                let entry = self.slots[scan];
                let home = Self::home(entry, mask);
                if (hole.wrapping_sub(home) & mask) < (scan.wrapping_sub(home) & mask) {
                    self.slots[hole] = entry;
                    hole = scan;
                }
                scan = (scan + 1) & mask;
            }
            self.slots[hole] = 0;
        }
        self.len -= 1;
        if self.buckets > INITIAL_BUCKETS && self.len < self.buckets * 3 / 16 {
            self.rehash(self.buckets / 2);
        }
        true
    }

    fn rehash(&mut self, buckets: usize) {
        self.spare[..buckets].fill(0);
        let mask = buckets - 1;
        for key in self.slots[..self.buckets]
            .iter()
            .rev()
            .copied()
            .filter(|key| *key != 0)
        {
            let mut index = Self::home(key, mask);
            while self.spare[index] != 0 {
                index = (index + 1) & mask;
            }
            self.spare[index] = key;
        }
        std::mem::swap(&mut self.slots, &mut self.spare);
        self.buckets = buckets;
    }

    fn stream(&self) -> impl Iterator<Item = ChunkAddress> + '_ {
        self.zero.then_some(ZERO).into_iter().chain(
            self.slots[..self.buckets]
                .iter()
                .copied()
                .filter(|key| *key != 0)
                .map(unpack),
        )
    }
}

// Stable bottom-up merge using caller-admitted scratch, with no hidden sort
// allocation. The comparator deliberately preserves Java i32 overflow.
fn stable_sort(values: &mut [ChunkAddress], scratch: &mut [ChunkAddress], center: ChunkAddress) {
    let mut width = 1;
    while width < values.len() {
        for start in (0..values.len()).step_by(width * 2) {
            let middle = (start + width).min(values.len());
            let end = (middle + width).min(values.len());
            let mut left = start;
            let mut right = middle;
            for output in &mut scratch[start..end] {
                if right == end
                    || (left < middle
                        && distance(values[left], center) <= distance(values[right], center))
                {
                    *output = values[left];
                    left += 1;
                } else {
                    *output = values[right];
                    right += 1;
                }
            }
        }
        values.copy_from_slice(&scratch[..values.len()]);
        width *= 2;
    }
}

// Bounded selection based on the observed offer/partition contract of the
// bundled least collector. Equal-distance membership is observable: replacing
// this with a heap or stable prefix sort changes the emitted chunk sequence.
struct Least {
    values: [ChunkAddress; 128],
    len: usize,
    count: usize,
    threshold: i32,
    center: ChunkAddress,
}

impl Least {
    fn new(count: usize, center: ChunkAddress) -> Self {
        debug_assert!((1..=64).contains(&count));
        Self {
            values: [ZERO; 128],
            len: 0,
            count,
            threshold: i32::MIN,
            center,
        }
    }

    fn offer(&mut self, position: ChunkAddress) {
        let value = distance(position, self.center);
        if self.len < self.count {
            self.values[self.len] = position;
            self.len += 1;
            self.threshold = self.threshold.max(value);
        } else if value < self.threshold {
            self.values[self.len] = position;
            self.len += 1;
            if self.len == self.count * 2 {
                self.trim();
            }
        }
    }

    fn trim(&mut self) {
        let mut left = 0usize;
        let mut right = self.len - 1;
        let mut threshold_start = 0;
        let iterations = 3 * (usize::BITS - (self.len - 2).leading_zeros());
        let mut scratch = [ZERO; 128];
        for iteration in 0..iterations.max(1) {
            if left >= right {
                break;
            }
            let pivot = (left + right).div_ceil(2);
            let value = self.values[pivot];
            let pivot_distance = distance(value, self.center);
            self.values[pivot] = self.values[right];
            let mut boundary = left;
            for scan in left..right {
                if distance(self.values[scan], self.center) < pivot_distance {
                    self.values.swap(scan, boundary);
                    boundary += 1;
                }
            }
            self.values[right] = self.values[boundary];
            self.values[boundary] = value;
            if boundary == self.count {
                break;
            }
            if boundary > self.count {
                right = boundary - 1;
            } else {
                left = boundary.max(left + 1);
                threshold_start = boundary;
            }
            if iteration + 1 >= iterations {
                stable_sort(&mut self.values[left..=right], &mut scratch, self.center);
            }
        }
        self.len = self.count;
        self.threshold = self.values[threshold_start..self.count]
            .iter()
            .map(|position| distance(*position, self.center))
            .max()
            .unwrap();
    }

    fn finish(&mut self) -> &[ChunkAddress] {
        let mut scratch = [ZERO; 128];
        stable_sort(&mut self.values[..self.len], &mut scratch, self.center);
        self.len = self.len.min(self.count);
        &self.values[..self.len]
    }
}
