//! Concrete, cooperatively sliced lighting jobs on the shared CPU admission.
//! Source snapshots arrive under their separate owner budget. All new engine,
//! queue and storage allocations are reserved before construction on a worker.

use super::{AdmissionError, CpuPool, Job, Lease, Shared, finish_job, lock};
use crate::world::lighting::{
    LightBlock, LightError, LightKind, LightingSource, SourceLimits, SourceStamp,
    work::{
        CompletedLighting, LightingError, LightingLimits, LightingMode, LightingWork, WorkProgress,
    },
};
use crate::world::{loading::ChunkLoadingOwner, preparation::ChunkAddress};
use std::{
    fmt,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

/// One submission never exceeds this cooperative work quantum. A unit can be a
/// full admitted chunk's source scan, so this is not a wall-clock latency limit.
pub const MAX_LIGHTING_SLICE_UNITS: usize = 64;

/// Explicit recovery requests, applied on a worker within the original byte
/// allowance. A blocked result is never automatically retried or enlarged.
#[derive(Clone, Copy, Debug, Default)]
pub struct LightingGrowth {
    pub block_queues: Option<[usize; 3]>,
    pub sky_queue: Option<usize>,
    pub sky_plan: Option<usize>,
}

#[derive(Debug)]
pub enum LightingJobError {
    Cancelled,
    WorkerPanicked,
    Lighting(LightingError),
}
impl fmt::Display for LightingJobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CPU lighting: {self:?}")
    }
}
impl std::error::Error for LightingJobError {}

#[derive(Debug)]
pub enum LightingReserveError {
    Source(LightError),
    Admission(AdmissionError),
}
impl fmt::Display for LightingReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lighting admission: {self:?}")
    }
}
impl std::error::Error for LightingReserveError {}

#[expect(
    clippy::large_enum_variant,
    reason = "the bounded global queue provisions the inline source control; avoid an extra allocation before CPU construction"
)]
enum Input {
    Initial {
        source: LightingSource,
        limits: LightingLimits,
        mode: LightingMode,
    },
    Resume(Box<LightingWork>),
}

/// Payload precedes its admission lease so dropping pending work first releases
/// all source/engine/storage ownership, then returns the shared slot and bytes.
pub struct PendingLighting {
    input: Input,
    growth: LightingGrowth,
    lease: Lease,
}
pub(super) struct LightingJob {
    pending: PendingLighting,
    units: usize,
    sender: oneshot::Sender<LightingCompletion>,
    #[cfg(test)]
    gate: Option<Arc<super::TestGate>>,
}
pub struct LightingTask {
    receiver: Option<oneshot::Receiver<LightingCompletion>>,
    cancelled: bool,
}

enum Payload {
    Paused(Box<LightingWork>),
    Complete(CompletedLighting),
    Failed,
}

/// Keeps the full shared admission for pauses, blocked work, errors and completed
/// results. Only the world owner's crate-private fence can access snapshots;
/// public diagnostics cannot clone their backing out of this lease.
pub struct LightingCompletion {
    payload: Payload,
    progress: Result<WorkProgress, LightingJobError>,
    lease: Lease,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResidentLightingStats {
    pub results: usize,
    pub used_bytes: usize,
    pub peak_bytes: usize,
}
struct ResidentLightingState {
    max_bytes: usize,
    stats: Mutex<ResidentLightingStats>,
}
/// Clones share one ledger; creating a budget per domain would not provide an
/// aggregate residency limit. Its leases keep this ledger alive independently.
#[derive(Clone)]
pub struct ResidentLightingBudget {
    shared: Arc<ResidentLightingState>,
}
struct ResidentLightingLease {
    shared: Arc<ResidentLightingState>,
    bytes: usize,
}
/// Immutable completed payload is dropped before its resident reservation. No
/// public snapshot/source clone or detached completed-work API is exposed.
pub struct ResidentLighting {
    completed: CompletedLighting,
    lease: ResidentLightingLease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightingAdoptionReason {
    Incomplete,
    ByteLimit,
    Overflow,
}
/// Any failed destination admission retains the exact CPU result and its lease.
pub struct LightingAdoptionError {
    reason: LightingAdoptionReason,
    completion: LightingCompletion,
}
impl LightingAdoptionError {
    pub fn reason(&self) -> LightingAdoptionReason {
        self.reason
    }
    pub fn into_completion(self) -> LightingCompletion {
        self.completion
    }
}
impl fmt::Debug for LightingAdoptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LightingAdoptionError")
            .field(&self.reason)
            .finish()
    }
}
impl fmt::Display for LightingAdoptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "resident lighting adoption: {:?}", self.reason)
    }
}
impl std::error::Error for LightingAdoptionError {}

impl ResidentLightingBudget {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            shared: Arc::new(ResidentLightingState {
                max_bytes,
                stats: Mutex::new(ResidentLightingStats::default()),
            }),
        }
    }
    pub fn stats(&self) -> ResidentLightingStats {
        *lock(&self.shared.stats)
    }
    fn reserve(&self, bytes: usize) -> Result<ResidentLightingLease, LightingAdoptionReason> {
        {
            let mut stats = lock(&self.shared.stats);
            let next_bytes = stats
                .used_bytes
                .checked_add(bytes)
                .ok_or(LightingAdoptionReason::Overflow)?;
            if next_bytes > self.shared.max_bytes {
                return Err(LightingAdoptionReason::ByteLimit);
            }
            let next_results = stats
                .results
                .checked_add(1)
                .ok_or(LightingAdoptionReason::Overflow)?;
            stats.used_bytes = next_bytes;
            stats.results = next_results;
            stats.peak_bytes = stats.peak_bytes.max(next_bytes);
        }
        Ok(ResidentLightingLease {
            shared: Arc::clone(&self.shared),
            bytes,
        })
    }
}
impl ResidentLighting {
    pub fn retained_bytes(&self) -> usize {
        self.lease.bytes
    }
    pub fn light_level(&self, kind: LightKind, pos: LightBlock) -> Option<u8> {
        match kind {
            LightKind::Block => Some(self.completed.block().get_level(pos)),
            LightKind::Sky => self.completed.sky().map(|sky| sky.get_level(pos)),
        }
    }
    pub(crate) fn completed(&self) -> &CompletedLighting {
        &self.completed
    }
}
impl Drop for ResidentLightingLease {
    fn drop(&mut self) {
        let mut stats = lock(&self.shared.stats);
        stats.results -= 1;
        stats.used_bytes -= self.bytes;
    }
}

impl CpuPool {
    pub fn try_reserve_lighting(
        &self,
        source: LightingSource,
        limits: LightingLimits,
    ) -> Result<PendingLighting, AdmissionError> {
        self.reserve_lighting(source, limits, LightingMode::Fresh)
    }
    pub fn try_reserve_lighting_restore(
        &self,
        source: LightingSource,
        limits: LightingLimits,
    ) -> Result<PendingLighting, AdmissionError> {
        self.reserve_lighting(source, limits, LightingMode::RestoreSaved)
    }
    fn reserve_lighting(
        &self,
        source: LightingSource,
        limits: LightingLimits,
        mode: LightingMode,
    ) -> Result<PendingLighting, AdmissionError> {
        let bytes = limits
            .reservation_bytes()
            .map_err(|_| AdmissionError::InvalidInput)?
            .checked_add(source.heap_bytes())
            .ok_or(AdmissionError::ByteLimit)?;
        let lease = self.try_reserve(bytes)?;
        Ok(PendingLighting {
            input: Input::Initial {
                source,
                limits,
                mode,
            },
            growth: LightingGrowth::default(),
            lease,
        })
    }

    /// Reserve canonical source metadata before capture. Canonical resident
    /// palettes keep their existing resident leases; no palette is copied here.
    pub fn try_reserve_canonical_lighting(
        &self,
        owner: &ChunkLoadingOwner,
        addresses: &[ChunkAddress],
        source_limits: SourceLimits,
        limits: LightingLimits,
    ) -> Result<PendingLighting, LightingReserveError> {
        self.reserve_canonical_lighting(
            owner,
            addresses,
            source_limits,
            limits,
            LightingMode::Fresh,
        )
    }
    pub fn try_reserve_canonical_lighting_restore(
        &self,
        owner: &ChunkLoadingOwner,
        addresses: &[ChunkAddress],
        source_limits: SourceLimits,
        limits: LightingLimits,
    ) -> Result<PendingLighting, LightingReserveError> {
        self.reserve_canonical_lighting(
            owner,
            addresses,
            source_limits,
            limits,
            LightingMode::RestoreSaved,
        )
    }
    fn reserve_canonical_lighting(
        &self,
        owner: &ChunkLoadingOwner,
        addresses: &[ChunkAddress],
        source_limits: SourceLimits,
        limits: LightingLimits,
        mode: LightingMode,
    ) -> Result<PendingLighting, LightingReserveError> {
        let bytes = limits
            .reservation_bytes()
            .map_err(|_| LightingReserveError::Admission(AdmissionError::InvalidInput))?
            .checked_add(source_limits.metadata_bytes)
            .ok_or(LightingReserveError::Admission(AdmissionError::ByteLimit))?;
        let lease = self
            .try_reserve(bytes)
            .map_err(LightingReserveError::Admission)?;
        let source = LightingSource::from_canonical(owner, addresses, source_limits)
            .map_err(LightingReserveError::Source)?;
        Ok(PendingLighting {
            input: Input::Initial {
                source,
                limits,
                mode,
            },
            growth: LightingGrowth::default(),
            lease,
        })
    }
}
impl PendingLighting {
    pub fn reserved_bytes(&self) -> usize {
        self.lease.bytes
    }
    pub(crate) fn source_stamp(&self) -> SourceStamp {
        match &self.input {
            Input::Initial { source, .. } => source.stamp(),
            Input::Resume(work) => work.source_stamp(),
        }
    }
    /// This records only fixed-size requests. Queue/scratch allocation happens
    /// after submission on the shared worker, with old and new capacity admitted.
    pub fn request_growth(&mut self, growth: LightingGrowth) {
        self.growth = growth;
    }

    /// The requested limit is capped at MAX_LIGHTING_SLICE_UNITS. Zero yields a
    /// charged pending completion; every other unfinished slice must be resumed.
    pub fn submit(self, max_units: usize) -> Result<LightingTask, AdmissionError> {
        self.enqueue(
            max_units.min(MAX_LIGHTING_SLICE_UNITS),
            #[cfg(test)]
            None,
        )
    }
    #[cfg(test)]
    fn submit_with_gate(
        self,
        max_units: usize,
        gate: Arc<super::TestGate>,
    ) -> Result<LightingTask, AdmissionError> {
        self.enqueue(max_units.min(MAX_LIGHTING_SLICE_UNITS), Some(gate))
    }
    fn enqueue(
        self,
        units: usize,
        #[cfg(test)] gate: Option<Arc<super::TestGate>>,
    ) -> Result<LightingTask, AdmissionError> {
        let (sender, receiver) = oneshot::channel();
        let shared = Arc::clone(&self.lease.shared);
        {
            let mut state = lock(&shared.state);
            if state.closed {
                return Err(AdmissionError::Closed);
            }
            debug_assert!(state.queue.len() < shared.config.max_jobs);
            state.queue.push_back(Job::Lighting(LightingJob {
                pending: self,
                units,
                sender,
                #[cfg(test)]
                gate,
            }));
            state.stats.queued += 1;
        }
        shared.work.notify_one();
        Ok(LightingTask {
            receiver: Some(receiver),
            cancelled: false,
        })
    }
}

impl LightingTask {
    pub async fn wait(mut self) -> Result<LightingCompletion, LightingJobError> {
        self.wait_mut().await
    }
    /// Cancelling the wait future leaves its receiver in this task so the same
    /// task can be awaited again. Explicit cancellation also suppresses ready data.
    pub async fn wait_mut(&mut self) -> Result<LightingCompletion, LightingJobError> {
        let receiver = self.receiver.as_mut().ok_or(LightingJobError::Cancelled)?;
        let result = receiver.await.map_err(|_| LightingJobError::Cancelled);
        self.receiver = None;
        if self.cancelled {
            drop(result);
            Err(LightingJobError::Cancelled)
        } else {
            result
        }
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
        if let Some(receiver) = &mut self.receiver {
            receiver.close();
        }
    }
}
impl Drop for LightingTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl LightingCompletion {
    pub fn progress(&self) -> Result<WorkProgress, &LightingJobError> {
        self.progress.as_ref().copied()
    }
    pub fn reserved_bytes(&self) -> usize {
        self.lease.bytes
    }
    /// Retained completed backing and conservative control allowances, excluding
    /// freed working queues. Uniform layers keep their full possible backing
    /// reservation. This is an admission charge, not an allocator/RSS measurement.
    pub fn resident_bytes(&self) -> Result<usize, LightingAdoptionReason> {
        let completed = self.completed().ok_or(LightingAdoptionReason::Incomplete)?;
        let payload = completed
            .retained_bytes()
            .map_err(|_| LightingAdoptionReason::Overflow)?;
        // The payload helper already includes the inline CompletedLighting body.
        // Conservatively count the shared ledger controls once for each result.
        payload
            .checked_add(size_of::<ResidentLighting>() - size_of::<CompletedLighting>())
            .and_then(|bytes| bytes.checked_add(size_of::<ResidentLightingState>()))
            .and_then(|bytes| bytes.checked_add(2 * size_of::<usize>()))
            .ok_or(LightingAdoptionReason::Overflow)
    }
    /// Destination admission precedes ownership movement and CPU refund. Failure
    /// returns the original result; success copies no source, layer or palette.
    #[expect(
        clippy::result_large_err,
        reason = "failed adoption returns the same charged completion without allocation"
    )]
    pub fn try_adopt(
        self,
        budget: &ResidentLightingBudget,
    ) -> Result<ResidentLighting, LightingAdoptionError> {
        let bytes = match self.resident_bytes() {
            Ok(bytes) => bytes,
            Err(reason) => {
                return Err(LightingAdoptionError {
                    reason,
                    completion: self,
                });
            }
        };
        let resident_lease = match budget.reserve(bytes) {
            Ok(lease) => lease,
            Err(reason) => {
                return Err(LightingAdoptionError {
                    reason,
                    completion: self,
                });
            }
        };
        let Self {
            payload,
            progress,
            lease,
        } = self;
        drop(progress);
        let Payload::Complete(completed) = payload else {
            unreachable!("completed admission checked")
        };
        let resident = ResidentLighting {
            completed,
            lease: resident_lease,
        };
        // Never hold the destination mutex while acquiring the CPU ledger mutex.
        drop(lease);
        Ok(resident)
    }
    pub fn light_level(&self, kind: LightKind, pos: LightBlock) -> Option<u8> {
        let completed = self.completed()?;
        match kind {
            LightKind::Block => Some(completed.block().get_level(pos)),
            LightKind::Sky => completed.sky().map(|sky| sky.get_level(pos)),
        }
    }
    pub(crate) fn completed(&self) -> Option<&CompletedLighting> {
        if let Payload::Complete(completed) = &self.payload {
            Some(completed)
        } else {
            None
        }
    }
    /// Paused and ordinary-error work can be resubmitted under the same lease.
    /// Finished or terminal failures remain owned by the returned completion.
    #[expect(
        clippy::result_large_err,
        reason = "failed resume returns the same owned admission without allocation"
    )]
    pub fn into_pending(self) -> Result<PendingLighting, Self> {
        if !matches!(self.payload, Payload::Paused(_)) {
            return Err(self);
        }
        let Self {
            payload,
            progress,
            lease,
        } = self;
        drop(progress);
        let Payload::Paused(work) = payload else {
            unreachable!()
        };
        Ok(PendingLighting {
            input: Input::Resume(work),
            growth: LightingGrowth::default(),
            lease,
        })
    }
}

pub(super) fn run(job: LightingJob, shared: &Shared) {
    #[cfg(test)]
    let gate = job.gate.clone();
    let LightingJob {
        pending,
        units,
        sender,
        ..
    } = job;
    if sender.is_closed() {
        drop(pending);
        finish_job(shared);
        return;
    }
    let PendingLighting {
        input,
        growth,
        lease,
    } = pending;
    // Box storage is covered by sizeof(LightingWork) in reservation_bytes and is
    // only allocated here. It keeps the global queue's concrete job size small.
    let constructed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match input {
        Input::Initial {
            source,
            limits,
            mode,
        } => match mode {
            LightingMode::Fresh => LightingWork::new(source, limits),
            LightingMode::RestoreSaved => LightingWork::new_restore(source, limits),
        }
        .map(Box::new),
        Input::Resume(work) => Ok(work),
    }));
    let (payload, progress) = match constructed {
        Err(_) => (Payload::Failed, Err(LightingJobError::WorkerPanicked)),
        Ok(Err(error)) => (Payload::Failed, Err(LightingJobError::Lighting(error))),
        Ok(Ok(mut work)) => {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some([checks, decreases, increases]) = growth.block_queues {
                    work.grow_block_queues(checks, decreases, increases)?;
                }
                if let Some(capacity) = growth.sky_queue {
                    work.grow_sky_queues(capacity)?;
                }
                if let Some(capacity) = growth.sky_plan {
                    work.grow_sky_plan(capacity)?;
                }
                work.step(units)
            }));
            match result {
                Err(_) => {
                    drop(work);
                    (Payload::Failed, Err(LightingJobError::WorkerPanicked))
                }
                Ok(Err(error)) => (
                    Payload::Paused(work),
                    Err(LightingJobError::Lighting(error)),
                ),
                Ok(Ok(progress)) if !progress.complete => (Payload::Paused(work), Ok(progress)),
                Ok(Ok(progress)) => {
                    // Moving out of the box frees its backing before completed
                    // data is handed off; no new result backing is allocated.
                    match work.into_completed() {
                        Ok(completed) => (Payload::Complete(completed), Ok(progress)),
                        Err(work) => (
                            Payload::Paused(Box::new(work)),
                            Err(LightingJobError::WorkerPanicked),
                        ),
                    }
                }
            }
        }
    };
    let completion = LightingCompletion {
        payload,
        progress,
        lease,
    };
    #[cfg(test)]
    if let Some(gate) = gate {
        gate.block();
    }
    finish_job(shared);
    // A closed or dropped consumer destroys the result here, including all owned
    // payloads before their lease. A live consumer retains the same reservation.
    let _ = sender.send(completion);
}

#[cfg(test)]
#[path = "lighting_tests.rs"]
mod tests;

#[cfg(test)]
mod resident_accounting_tests {
    use super::*;

    #[test]
    fn overflow_and_byte_limit_leave_both_resident_counters_unchanged() {
        let budget = ResidentLightingBudget::new(usize::MAX);
        for stats in [
            ResidentLightingStats {
                results: 1,
                used_bytes: usize::MAX,
                peak_bytes: usize::MAX,
            },
            ResidentLightingStats {
                results: usize::MAX,
                used_bytes: 0,
                peak_bytes: 0,
            },
        ] {
            *lock(&budget.shared.stats) = stats;
            assert!(matches!(
                budget.reserve(1),
                Err(LightingAdoptionReason::Overflow)
            ));
            assert_eq!(budget.stats(), stats);
        }
        let small = ResidentLightingBudget::new(1);
        assert!(matches!(
            small.reserve(2),
            Err(LightingAdoptionReason::ByteLimit)
        ));
        assert_eq!(small.stats(), ResidentLightingStats::default());
    }
}
