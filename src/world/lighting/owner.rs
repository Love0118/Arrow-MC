//! Current-domain admission and a coherent block/sky publication boundary.
//!
//! This owner publishes completed initialization and lighting. It does not establish
//! chunk status, ticking, player visibility, or Vanilla's pending-task markers.
use super::{
    LightBlock, LightError, LightKind, SourceLimits, SourceStamp,
    storage::{LightDataSnapshot, LightSnapshot},
    work::{CompletedLighting, LightingLimits},
};
use crate::{
    runtime::{
        AdmissionError, CpuPool, LightingAdoptionReason, LightingCompletion, LightingReserveError,
        PendingLighting, ResidentLighting, ResidentLightingBudget,
    },
    world::{
        loading::ChunkLoadingOwner, preparation::ChunkAddress, storage::chunk::DimensionHeight,
    },
};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightingOwnerError {
    Source(LightError),
    Admission(AdmissionError),
    Adoption(LightingAdoptionReason),
    WrongSkyMode,
    StaleSource,
    Incomplete,
    AlreadyCompleted,
    MissingRequest,
}
impl fmt::Display for LightingOwnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lighting publication: {self:?}")
    }
}
impl std::error::Error for LightingOwnerError {}

/// Rejection preserves the work and its CPU reservation for the caller to
/// inspect, resume where appropriate, or drop. No payload is detached.
pub struct RejectedLighting {
    pub reason: LightingOwnerError,
    pub completion: LightingCompletion,
}
impl fmt::Debug for RejectedLighting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RejectedLighting")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

/// One explicitly selected available-for-lighting domain. Separate domains can
/// run on shared CPU workers; the caller coordinates any overlapping domains.
/// Revision and selection are fenced within each domain, not globally arbitrated.
#[derive(Default)]
pub struct LightingDomain {
    completion: Option<ResidentLighting>,
    current: Option<SourceStamp>,
}
impl LightingDomain {
    pub fn new() -> Self {
        Self::default()
    }

    /// A replacement attempt revokes the old result immediately, even if source
    /// capture or CPU admission fails. Retrying takes a new domain identity.
    pub fn begin(
        &mut self,
        owner: &ChunkLoadingOwner,
        addresses: &[ChunkAddress],
        source_limits: SourceLimits,
        limits: LightingLimits,
        cpu: &CpuPool,
    ) -> Result<PendingLighting, LightingOwnerError> {
        self.begin_mode(owner, addresses, source_limits, limits, cpu, false)
    }

    /// Restore persisted rows before initialization and conditional relighting.
    /// Saved flags alone never bypass this domain's completion/source fence.
    pub fn begin_restore(
        &mut self,
        owner: &ChunkLoadingOwner,
        addresses: &[ChunkAddress],
        source_limits: SourceLimits,
        limits: LightingLimits,
        cpu: &CpuPool,
    ) -> Result<PendingLighting, LightingOwnerError> {
        self.begin_mode(owner, addresses, source_limits, limits, cpu, true)
    }

    fn begin_mode(
        &mut self,
        owner: &ChunkLoadingOwner,
        addresses: &[ChunkAddress],
        source_limits: SourceLimits,
        limits: LightingLimits,
        cpu: &CpuPool,
        restore: bool,
    ) -> Result<PendingLighting, LightingOwnerError> {
        self.cancel();
        if addresses.is_empty() {
            return Err(LightingOwnerError::Source(LightError::InvalidLimits));
        }
        if limits.has_sky_light() != owner.has_sky_light() {
            return Err(LightingOwnerError::WrongSkyMode);
        }
        let pending = if restore {
            cpu.try_reserve_canonical_lighting_restore(owner, addresses, source_limits, limits)
        } else {
            cpu.try_reserve_canonical_lighting(owner, addresses, source_limits, limits)
        }
        .map_err(|error| match error {
            LightingReserveError::Source(error) => LightingOwnerError::Source(error),
            LightingReserveError::Admission(error) => LightingOwnerError::Admission(error),
        })?;
        self.current = Some(pending.source_stamp());
        Ok(pending)
    }

    pub fn cancel(&mut self) {
        self.completion = None;
        self.current = None;
    }

    /// Validate current source ownership, then reserve the completed payload in
    /// the shared resident budget before returning its CPU slot. Admission
    /// failure leaves this request current and returns the same result for retry.
    #[expect(
        clippy::result_large_err,
        reason = "rejection preserves the existing completion and lease without allocation"
    )]
    pub fn accept(
        &mut self,
        owner: &ChunkLoadingOwner,
        completion: LightingCompletion,
        resident_budget: &ResidentLightingBudget,
    ) -> Result<(), RejectedLighting> {
        let result = self.validate(owner, &completion);
        if let Err(reason) = result {
            return Err(RejectedLighting { reason, completion });
        }
        let resident = completion.try_adopt(resident_budget).map_err(|error| {
            let reason = LightingOwnerError::Adoption(error.reason());
            RejectedLighting {
                reason,
                completion: error.into_completion(),
            }
        })?;
        self.completion = Some(resident);
        Ok(())
    }

    fn validate(
        &self,
        owner: &ChunkLoadingOwner,
        completion: &LightingCompletion,
    ) -> Result<(), LightingOwnerError> {
        let current = self
            .current
            .as_ref()
            .ok_or(LightingOwnerError::MissingRequest)?;
        if self.completion.is_some() {
            return Err(LightingOwnerError::AlreadyCompleted);
        }
        let completed = completion
            .completed()
            .ok_or(LightingOwnerError::Incomplete)?;
        if completed.source().stamp() != *current || !completed.source().is_current(owner) {
            return Err(LightingOwnerError::StaleSource);
        }
        if completed.sky().is_some() != owner.has_sky_light() {
            return Err(LightingOwnerError::WrongSkyMode);
        }
        Ok(())
    }

    /// Borrow both owners: callers cannot replace this domain or mutate the
    /// canonical source while encoding from this capability. Source publication,
    /// removal and registry/dimension reload invalidate later reads globally.
    pub fn ready<'a>(&'a self, owner: &'a ChunkLoadingOwner) -> Option<ReadyLighting<'a>> {
        let completed = self.completion.as_ref()?.completed();
        if self.current.as_ref()? != &completed.source().stamp()
            || !completed.source().is_current(owner)
            || completed.sky().is_some() != owner.has_sky_light()
        {
            return None;
        }
        Some(ReadyLighting {
            completed,
            _owner: owner,
        })
    }
}

/// Snapshot handles remain private to the crate so their public Clone cannot
/// escape the adopted result's resident reservation. Packet builders receive
/// borrowed payloads only, tied to this capability's lifetime.
pub struct ReadyLighting<'a> {
    completed: &'a CompletedLighting,
    _owner: &'a ChunkLoadingOwner,
}
impl ReadyLighting<'_> {
    pub fn height(&self) -> DimensionHeight {
        self.completed.source().height()
    }
    pub fn has_chunk(&self, address: ChunkAddress) -> bool {
        self.completed.source().has_chunk(address)
    }
    pub fn light_level(&self, kind: LightKind, position: LightBlock) -> Option<u8> {
        if !self.has_chunk(position.column()) {
            return None;
        }
        match kind {
            LightKind::Block => Some(self.block().get_level(position)),
            LightKind::Sky => self.sky().map(|snapshot| snapshot.get_level(position)),
        }
    }
    pub(crate) fn block(&self) -> &LightSnapshot {
        self.completed.block()
    }
    pub(crate) fn sky(&self) -> Option<&LightSnapshot> {
        self.completed.sky()
    }
    pub(crate) fn packet_block(&self) -> &LightDataSnapshot {
        self.completed.packet_block()
    }
    pub(crate) fn packet_sky(&self) -> Option<&LightDataSnapshot> {
        self.completed.packet_sky()
    }
}
