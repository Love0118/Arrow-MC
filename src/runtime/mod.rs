//! Shared, bounded CPU work for sections, packet codecs and login verification.
//!
//! One server owns one pool and budgets its workers alongside its I/O runtime.
//! This prepares immutable section bytes; it does not tick a world or publish
//! revisions into one. The owner must compare the completed key with its current
//! world/chunk revision before using the bytes.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use crate::world::section::{self, Registry, SectionCounts};

mod lighting;
mod packet;
mod storage;
pub use lighting::{
    LightingAdoptionError, LightingAdoptionReason, LightingCompletion, LightingGrowth,
    LightingJobError, LightingReserveError, LightingTask, MAX_LIGHTING_SLICE_UNITS,
    PendingLighting, ResidentLighting, ResidentLightingBudget, ResidentLightingStats,
};
pub use packet::{
    LOGIN_KEY_JOB_BUFFER_BYTES, LoginKeyJobError, LoginKeyOutput, LoginKeyTask, PendingLoginKey,
};
pub use packet::{PacketJobError, PacketJobOutput, PacketOperation, PacketTask, PendingPacket};
pub use storage::{
    AdoptionError, ChunkDecodeOutput, ChunkDecodeTask, ChunkReadKey, PendingChunkDecode,
    ResidentChunk, ResidentChunkBudget, ResidentStats,
};

pub const SECTION_INPUT_BYTES: usize = (4096 + 64) * size_of::<u32>();
pub const SECTION_JOB_BUFFER_BYTES: usize =
    SECTION_INPUT_BYTES + section::MAX_SECTION_NETWORK_BYTES;
/// Fixed worker stacks also cover the kernel's bounded, allocation-free scratch.
/// Stack reservations, queue/control storage and allocator metadata are separate
/// from the requested input/output backing-byte admission budget (not RSS).
pub const WORKER_STACK_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct CpuPoolConfig {
    pub workers: usize,
    /// Counts reservations, queued/running work and retained completions together.
    pub max_jobs: usize,
    pub buffer_bytes: usize,
}

/// The consumer supplies identities; they are not global pool ordering keys.
/// Different chunks may finish in any order. Revision checks belong to the owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SectionKey {
    pub world_epoch: u64,
    pub chunk_x: i32,
    pub chunk_z: i32,
    pub section_y: i32,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuPoolStats {
    pub in_flight: usize,
    pub queued: usize,
    pub running: usize,
    pub reserved_buffer_bytes: usize,
    pub peak_reserved_buffer_bytes: usize,
    pub peak_running: usize,
    pub completed_jobs: u64,
}

#[derive(Debug)]
pub enum StartError {
    InvalidConfig,
    AllocationFailed,
    Spawn(std::io::Error),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig => {
                f.write_str("CPU pool requires workers, slots and one job's byte budget")
            }
            Self::AllocationFailed => f.write_str("CPU pool control allocation failed"),
            Self::Spawn(error) => write!(f, "CPU worker creation failed: {error}"),
        }
    }
}

impl std::error::Error for StartError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    Closed,
    JobLimit,
    ByteLimit,
    AllocationFailed,
    InvalidInput,
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "CPU pool is closed",
            Self::JobLimit => "CPU in-flight job limit reached",
            Self::ByteLimit => "CPU buffer-byte budget exhausted",
            Self::AllocationFailed => "CPU job buffer allocation failed",
            Self::InvalidInput => "CPU job input or limits are invalid",
        })
    }
}

impl std::error::Error for AdmissionError {}

#[derive(Debug)]
pub enum SectionJobError {
    Cancelled,
    Prepare(section::Error),
    WorkerPanicked,
}

impl fmt::Display for SectionJobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("section preparation cancelled"),
            Self::Prepare(error) => write!(f, "section preparation failed: {error}"),
            Self::WorkerPanicked => f.write_str("section preparation worker panicked"),
        }
    }
}

impl std::error::Error for SectionJobError {}

pub struct CpuPool {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<PoolState>,
    work: Condvar,
    config: CpuPoolConfig,
}

struct PoolState {
    queue: VecDeque<Job>,
    closed: bool,
    stats: CpuPoolStats,
}

enum Job {
    Section(PrepareSection),
    Packet(packet::PacketJob),
    VerifyLoginKey(packet::LoginKeyJob),
    DecodeChunk(storage::DecodeChunk),
    Lighting(lighting::LightingJob),
}

/// Field order is deliberate: payloads are freed before the lease returns their
/// bytes and slot. There is no API that detaches a Vec from that lease.
pub struct PendingSection {
    input: Vec<u32>,
    output: Vec<u8>,
    key: SectionKey,
    block_registry: Registry,
    biome_registry: Registry,
    counts: SectionCounts,
    lease: Lease,
}

struct PrepareSection {
    pending: PendingSection,
    task: Arc<TaskState>,
    #[cfg(test)]
    gate: Option<Arc<TestGate>>,
}

struct TaskState {
    ready: Mutex<Option<SectionCompletion>>,
    changed: Condvar,
    cancelled: AtomicBool,
}

pub struct SectionTask {
    key: SectionKey,
    state: Option<Arc<TaskState>>,
}

/// Keeping this completion alive retains its slot and full conservative byte
/// reservation, including on errors. Drop it after copying/sending under the
/// destination's own budget. The immutable slice may be borrowed without copying.
pub struct SectionCompletion {
    key: SectionKey,
    outcome: Result<Vec<u8>, SectionJobError>,
    _lease: Lease,
}

struct Lease {
    shared: Arc<Shared>,
    bytes: usize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl CpuPool {
    pub fn new(config: CpuPoolConfig) -> Result<Self, StartError> {
        if config.workers == 0
            || config.max_jobs == 0
            || config.buffer_bytes < SECTION_JOB_BUFFER_BYTES
        {
            return Err(StartError::InvalidConfig);
        }
        let mut queue = VecDeque::new();
        queue
            .try_reserve_exact(config.max_jobs)
            .map_err(|_| StartError::AllocationFailed)?;
        let mut workers = Vec::new();
        workers
            .try_reserve_exact(config.workers)
            .map_err(|_| StartError::AllocationFailed)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(PoolState {
                queue,
                closed: false,
                stats: CpuPoolStats::default(),
            }),
            work: Condvar::new(),
            config,
        });
        let mut pool = Self { shared, workers };
        for index in 0..config.workers {
            let shared = Arc::clone(&pool.shared);
            let worker = thread::Builder::new()
                .name(format!("arrow-cpu-{index}"))
                .stack_size(WORKER_STACK_BYTES)
                .spawn(move || work(shared))
                .map_err(StartError::Spawn)?;
            pool.workers.push(worker);
        }
        Ok(pool)
    }

    /// Reserves one slot and all requested input/output backing bytes before
    /// allocating either buffer. Fill the input directly instead of cloning an
    /// unaccounted dense section. Rejection makes no payload allocation.
    pub fn try_reserve_section(
        &self,
        key: SectionKey,
        block_registry: Registry,
        biome_registry: Registry,
        counts: SectionCounts,
    ) -> Result<PendingSection, AdmissionError> {
        // This local predates both buffers, so error unwinding drops them first.
        let lease = self.try_reserve(SECTION_JOB_BUFFER_BYTES)?;
        let mut input = Vec::new();
        input
            .try_reserve_exact(4096 + 64)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        input.resize(4096 + 64, 0);
        let mut output = Vec::new();
        output
            .try_reserve_exact(section::MAX_SECTION_NETWORK_BYTES)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        Ok(PendingSection {
            input,
            output,
            key,
            block_registry,
            biome_registry,
            counts,
            lease,
        })
    }

    fn try_reserve(&self, bytes: usize) -> Result<Lease, AdmissionError> {
        let mut state = lock(&self.shared.state);
        if state.closed {
            return Err(AdmissionError::Closed);
        }
        if state.stats.in_flight == self.shared.config.max_jobs {
            return Err(AdmissionError::JobLimit);
        }
        if bytes > self.shared.config.buffer_bytes - state.stats.reserved_buffer_bytes {
            return Err(AdmissionError::ByteLimit);
        }
        state.stats.in_flight += 1;
        state.stats.reserved_buffer_bytes += bytes;
        state.stats.peak_reserved_buffer_bytes = state
            .stats
            .peak_reserved_buffer_bytes
            .max(state.stats.reserved_buffer_bytes);
        Ok(Lease {
            shared: Arc::clone(&self.shared),
            bytes,
        })
    }

    pub fn stats(&self) -> CpuPoolStats {
        lock(&self.shared.state).stats
    }

    /// Stops admission and drains all accepted queued/running jobs. Unsubmitted
    /// reservations cannot be submitted after close; drop them to release space.
    pub fn close(&self) {
        lock(&self.shared.state).closed = true;
        self.shared.work.notify_all();
    }

    /// Workers never wait for consumers to accept results, so retained task
    /// handles/completions do not prevent this drain and join from finishing.
    pub fn shutdown(mut self) -> thread::Result<()> {
        self.join()
    }

    fn join(&mut self) -> thread::Result<()> {
        self.close();
        let mut failure = None;
        for worker in self.workers.drain(..) {
            if let Err(error) = worker.join() {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for CpuPool {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = lock(&self.shared.state);
        state.stats.in_flight -= 1;
        state.stats.reserved_buffer_bytes -= self.bytes;
    }
}

impl PendingSection {
    pub fn blocks_mut(&mut self) -> &mut [u32; 4096] {
        (&mut self.input[..4096]).try_into().unwrap()
    }

    pub fn biomes_mut(&mut self) -> &mut [u32; 64] {
        (&mut self.input[4096..]).try_into().unwrap()
    }

    pub fn submit(self) -> Result<SectionTask, AdmissionError> {
        self.enqueue(
            #[cfg(test)]
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn submit_with_gate(
        self,
        gate: Arc<TestGate>,
    ) -> Result<SectionTask, AdmissionError> {
        self.enqueue(Some(gate))
    }

    fn enqueue(
        self,
        #[cfg(test)] gate: Option<Arc<TestGate>>,
    ) -> Result<SectionTask, AdmissionError> {
        let task = Arc::new(TaskState {
            ready: Mutex::new(None),
            changed: Condvar::new(),
            cancelled: AtomicBool::new(false),
        });
        let result = SectionTask {
            key: self.key,
            state: Some(Arc::clone(&task)),
        };
        let shared = Arc::clone(&self.lease.shared);
        {
            let mut state = lock(&shared.state);
            if state.closed {
                return Err(AdmissionError::Closed);
            }
            // All-stage admission bounds queue capacity; this cannot grow.
            debug_assert!(state.queue.len() < shared.config.max_jobs);
            state.queue.push_back(Job::Section(PrepareSection {
                pending: self,
                task,
                #[cfg(test)]
                gate,
            }));
            state.stats.queued += 1;
        }
        shared.work.notify_one();
        Ok(result)
    }
}

impl SectionTask {
    pub fn key(&self) -> SectionKey {
        self.key
    }

    /// Cancellation suppresses the result; it does not release running memory.
    /// A completed payload is freed when taken as Cancelled or when dropped.
    pub fn cancel(&self) {
        if let Some(state) = &self.state {
            state.cancelled.store(true, Ordering::Release);
        }
    }

    pub fn is_finished(&self) -> bool {
        self.state
            .as_ref()
            .is_none_or(|state| lock(&state.ready).is_some())
    }

    /// Returns a completion once. Independent task slots allow owners to wait in
    /// their required order without blocking worker publication of later work.
    pub fn try_take(&mut self) -> Option<SectionCompletion> {
        let state = self.state.as_ref()?;
        let mut completion = lock(&state.ready).take()?;
        if state.cancelled.load(Ordering::Acquire) {
            completion.outcome = Err(SectionJobError::Cancelled);
        }
        // A caller may retain an already-consumed handle. It need not retain the
        // per-job synchronization allocation after ownership transfers out.
        self.state = None;
        Some(completion)
    }

    /// Blocks the calling thread. Use on the synchronous owner/tooling path,
    /// never an async I/O runtime worker. Such owners can poll try_take instead.
    /// Returns None only if try_take already consumed this task's completion.
    pub fn wait(mut self) -> Option<SectionCompletion> {
        {
            let state = self.state.as_ref()?;
            let mut ready = lock(&state.ready);
            while ready.is_none() {
                ready = state
                    .changed
                    .wait(ready)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
        self.try_take()
    }
}

impl Drop for SectionTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl SectionCompletion {
    pub fn key(&self) -> SectionKey {
        self.key
    }

    pub fn bytes(&self) -> Result<&[u8], &SectionJobError> {
        self.outcome.as_deref()
    }
}

fn work(shared: Arc<Shared>) {
    // Lazily initialized once per worker. Backend state is provisioned worker
    // overhead, separately bounded by worker count, not a per-connection cache.
    let mut compression = None;
    let mut storage_decoder = None;
    loop {
        let job = {
            let mut state = lock(&shared.state);
            loop {
                if let Some(job) = state.queue.pop_front() {
                    state.stats.queued -= 1;
                    state.stats.running += 1;
                    state.stats.peak_running = state.stats.peak_running.max(state.stats.running);
                    break job;
                }
                if state.closed {
                    return;
                }
                state = shared
                    .work
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        match job {
            Job::Section(job) => run_section(job, &shared),
            Job::Packet(job) => packet::run(job, &mut compression, &shared),
            Job::VerifyLoginKey(job) => packet::verify_login_key(job, &shared),
            Job::DecodeChunk(job) => storage::decode_chunk(job, &mut storage_decoder, &shared),
            Job::Lighting(job) => lighting::run(job, &shared),
        }
    }
}

fn finish_job(shared: &Shared) {
    let mut state = lock(&shared.state);
    state.stats.running -= 1;
    state.stats.completed_jobs = state.stats.completed_jobs.saturating_add(1);
}

fn run_section(job: PrepareSection, shared: &Shared) {
    #[cfg(test)]
    if let Some(gate) = &job.gate {
        gate.block();
    }
    let PrepareSection { pending, task, .. } = job;
    let PendingSection {
        input,
        mut output,
        key,
        block_registry,
        biome_registry,
        counts,
        lease,
    } = pending;
    let outcome = if task.cancelled.load(Ordering::Acquire) {
        Err(SectionJobError::Cancelled)
    } else {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            section::prepare_section(
                (&input[..4096]).try_into().unwrap(),
                (&input[4096..]).try_into().unwrap(),
                block_registry,
                biome_registry,
                counts,
                &mut output,
            )
        }))
        .map_err(|_| SectionJobError::WorkerPanicked)
        .and_then(|result| result.map_err(SectionJobError::Prepare))
    };
    // Release input before handing the still-reserved output to the owner.
    drop(input);
    let outcome = if task.cancelled.load(Ordering::Acquire) {
        drop(output);
        Err(SectionJobError::Cancelled)
    } else {
        match outcome {
            Ok(()) => Ok(output),
            Err(error) => {
                drop(output);
                Err(error)
            }
        }
    };
    let completion = SectionCompletion {
        key,
        outcome,
        _lease: lease,
    };
    finish_job(shared);
    *lock(&task.ready) = Some(completion);
    task.changed.notify_one();
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) struct TestGate {
    pub(crate) started: std::sync::mpsc::SyncSender<()>,
    pub(crate) released: Mutex<bool>,
    pub(crate) changed: Condvar,
}

#[cfg(test)]
impl TestGate {
    pub(crate) fn block(&self) {
        self.started.send(()).unwrap();
        let mut released = lock(&self.released);
        while !*released {
            released = self.changed.wait(released).unwrap();
        }
    }

    pub(crate) fn release(&self) {
        *lock(&self.released) = true;
        self.changed.notify_all();
    }
}
