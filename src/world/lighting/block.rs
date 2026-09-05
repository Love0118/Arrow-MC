//! Synchronous block-light convergence over an immutable admitted source.
//!
//! Independently designed from locked 26.3-pre-2 LightEngine/BlockLightEngine
//! requirements and actual-JAR observations. Check batches, decreases, increases,
//! queued storage changes and visible publication remain separate phases.

use std::fmt;

use super::queue::{CheckQueue, Entry, QueueError, WorkQueue};
use super::storage::{LightSectionStorage, StorageError, StorageStamp};
use super::{LightBlock, LightDirection, LightKind, LightingSource, SourceStamp};
use crate::world::preparation::ChunkAddress;

const ALL: u8 = 0b11_1111;
const ZERO: LightBlock = LightBlock { x: 0, y: 0, z: 0 };
const BLANK: Entry = Entry {
    pos: ZERO,
    level: 0,
    directions: 0,
    empty_shape: true,
    from_emission: false,
};

#[derive(Clone, Copy, Debug)]
pub struct BlockLightLimits {
    pub checks: usize,
    pub decreases: usize,
    pub increases: usize,
    /// All retained check/index/FIFO backing capacity. Stack plans and the
    /// separate source/layer owners are not included; this is not process RSS.
    pub queue_bytes: usize,
}

#[derive(Debug)]
pub enum BlockLightError {
    Queue(QueueError),
    Storage(StorageError),
    RunActive,
    SourceMismatch,
    StorageMismatch,
    InvalidCoordinate,
    WrongLayer,
}
impl fmt::Display for BlockLightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "block lighting: {self:?}")
    }
}
impl std::error::Error for BlockLightError {}
impl From<QueueError> for BlockLightError {
    fn from(value: QueueError) -> Self {
        Self::Queue(value)
    }
}
impl From<StorageError> for BlockLightError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunProgress {
    /// Check/propagation work units, not Vanilla's incidental FIFO update count.
    pub processed: usize,
    pub complete: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Idle,
    Checks,
    Decreases,
    Increases,
    Storage,
    Publish,
}

pub struct BlockLightEngine {
    checks: CheckQueue,
    decreases: WorkQueue,
    increases: WorkQueue,
    phase: Phase,
    source_stamp: Option<SourceStamp>,
    storage_stamp: Option<StorageStamp>,
}

impl BlockLightEngine {
    pub fn new(limits: BlockLightLimits) -> Result<Self, BlockLightError> {
        let mut remaining = limits.queue_bytes;
        Ok(Self {
            checks: CheckQueue::new(limits.checks, &mut remaining)?,
            decreases: WorkQueue::new(limits.decreases, &mut remaining)?,
            increases: WorkQueue::new(limits.increases, &mut remaining)?,
            phase: Phase::Idle,
            source_stamp: None,
            storage_stamp: None,
        })
    }

    pub fn heap_bytes(&self) -> usize {
        self.checks.heap_bytes() + self.decreases.heap_bytes() + self.increases.heap_bytes()
    }

    /// Engine queues/phase only. The storage owner must also flush pending layer
    /// changes by calling `run`, which is valid even when these queues are empty.
    pub fn has_work(&self) -> bool {
        self.phase != Phase::Idle
            || !self.checks.is_empty()
            || !self.decreases.is_empty()
            || !self.increases.is_empty()
    }

    pub fn check_block(&mut self, pos: LightBlock) -> Result<bool, BlockLightError> {
        self.require_idle()?;
        // Match the admitted source domain, leaving room for propagation into
        // padding without crossing Java BlockPos's 26/12/26-bit boundaries.
        // Unsupported inputs are rejected instead of silently aliasing a node.
        let horizontal = (-2_097_061 * 16)..=(2_097_061 * 16 + 15);
        if !horizontal.contains(&pos.x)
            || !horizontal.contains(&pos.z)
            || !(-2032..=2031).contains(&pos.y)
        {
            return Err(BlockLightError::InvalidCoordinate);
        }
        Ok(self.checks.insert(pos)?)
    }

    /// Explicit budget/capacity adjustment for a blocked run. Each replacement
    /// is admitted while its old backing remains live; earlier successful growth
    /// may remain after a later allocation fails. Pending work is never removed.
    pub fn grow_queues(&mut self, limits: BlockLightLimits) -> Result<(), BlockLightError> {
        if limits.checks == 0 || limits.decreases == 0 || limits.increases == 0 {
            return Err(QueueError::InvalidCapacity.into());
        }
        let mut remaining = limits
            .queue_bytes
            .checked_sub(self.heap_bytes())
            .ok_or(QueueError::AllocationLimit)?;
        self.checks.grow(limits.checks, &mut remaining)?;
        self.decreases.grow(limits.decreases, &mut remaining)?;
        self.increases.grow(limits.increases, &mut remaining)?;
        Ok(())
    }

    /// Enables the column, then seeds its actual sources in section/y/z/x order.
    /// Full queue admission is checked before changing the enabled-column state.
    pub fn propagate_light_sources(
        &mut self,
        source: &LightingSource,
        storage: &mut LightSectionStorage,
        chunk: ChunkAddress,
    ) -> Result<(), BlockLightError> {
        self.require_idle()?;
        require_block(storage)?;
        self.require_source(source)?;
        self.require_storage(storage)?;
        let needed = source.emission_sources(chunk).count();
        if needed > self.increases.remaining_capacity() {
            return Err(QueueError::Full.into());
        }
        for (pos, _) in source.emission_sources(chunk) {
            if !storage.storing_light(pos.section()) {
                return Err(StorageError::MissingLayer.into());
            }
        }
        storage.set_enabled(chunk, true)?;
        if needed != 0 {
            self.source_stamp = Some(source.stamp());
            self.storage_stamp = Some(storage.stamp());
        }
        for (pos, state) in source.emission_sources(chunk) {
            let material = source
                .registry()
                .light_material(state)
                .expect("source state belongs to admitted registry");
            self.increases
                .push(emission(pos, material.emission, material.empty_shape()))
                .expect("source batch admitted");
        }
        Ok(())
    }

    /// Ordinary pressure retains the current entry and all remaining work.
    /// Keep the same immutable source and private storage draft on every retry;
    /// no source/status changes or external publication may occur in between.
    /// Only a complete run publishes. Its caller still validates the source's
    /// owner/revision fence before exposing that snapshot to the world/sender.
    pub fn run(
        &mut self,
        source: &LightingSource,
        storage: &mut LightSectionStorage,
        work_budget: usize,
    ) -> Result<RunProgress, BlockLightError> {
        require_block(storage)?;
        self.require_source(source)?;
        self.require_storage(storage)?;
        if self.source_stamp.is_none() {
            self.source_stamp = Some(source.stamp());
            self.storage_stamp = Some(storage.stamp());
        }
        if self.phase == Phase::Idle {
            self.phase = Phase::Checks;
        }
        let mut processed = 0;
        loop {
            match self.phase {
                Phase::Checks if self.checks.is_empty() => {
                    self.checks.clear();
                    self.phase = Phase::Decreases;
                }
                Phase::Decreases if self.decreases.is_empty() => self.phase = Phase::Increases,
                Phase::Increases if self.increases.is_empty() => self.phase = Phase::Storage,
                Phase::Storage => {
                    storage.process_inconsistencies()?;
                    self.phase = Phase::Publish;
                }
                Phase::Publish => {
                    storage.publish_visible()?;
                    self.phase = Phase::Idle;
                    self.source_stamp = None;
                    self.storage_stamp = None;
                    return Ok(RunProgress {
                        processed,
                        complete: true,
                    });
                }
                _ if processed == work_budget => {
                    return Ok(RunProgress {
                        processed,
                        complete: false,
                    });
                }
                Phase::Checks => {
                    let plan =
                        check_plan(source, storage, self.checks.peek().expect("pending check"));
                    self.prepare(&plan, storage, 0, 0)?;
                    self.checks.pop();
                    self.apply(plan, storage);
                    processed += 1;
                }
                Phase::Decreases => {
                    let plan = decrease_plan(
                        source,
                        storage,
                        self.decreases.peek().expect("pending decrease"),
                    );
                    self.prepare(&plan, storage, 1, 0)?;
                    self.decreases.pop();
                    self.apply(plan, storage);
                    processed += 1;
                }
                Phase::Increases => {
                    let plan = increase_plan(
                        source,
                        storage,
                        self.increases.peek().expect("pending increase"),
                    )?;
                    self.prepare(&plan, storage, 0, 1)?;
                    self.increases.pop();
                    self.apply(plan, storage);
                    processed += 1;
                }
                Phase::Idle => unreachable!("run starts a check phase"),
            }
        }
    }

    fn prepare(
        &self,
        plan: &Plan,
        storage: &mut LightSectionStorage,
        decrease_credit: usize,
        increase_credit: usize,
    ) -> Result<(), BlockLightError> {
        if plan.decrease_count > self.decreases.remaining_capacity() + decrease_credit
            || plan.increase_count > self.increases.remaining_capacity() + increase_credit
        {
            return Err(QueueError::Full.into());
        }
        let mut positions = [ZERO; 7];
        for (position, write) in positions.iter_mut().zip(&plan.writes[..plan.write_count]) {
            *position = write.pos;
        }
        storage.prepare_writes(&positions[..plan.write_count])?;
        Ok(())
    }

    fn apply(&mut self, plan: Plan, storage: &mut LightSectionStorage) {
        for write in &plan.writes[..plan.write_count] {
            storage
                .set_stored_level(write.pos, write.level)
                .expect("exact writes were prepared before consuming entry");
        }
        for &entry in &plan.decreases[..plan.decrease_count] {
            self.decreases
                .push(entry)
                .expect("decrease fanout admitted");
        }
        for &entry in &plan.increases[..plan.increase_count] {
            self.increases
                .push(entry)
                .expect("increase fanout admitted");
        }
    }

    fn require_idle(&self) -> Result<(), BlockLightError> {
        if self.phase == Phase::Idle {
            Ok(())
        } else {
            Err(BlockLightError::RunActive)
        }
    }

    fn require_source(&self, source: &LightingSource) -> Result<(), BlockLightError> {
        if self
            .source_stamp
            .as_ref()
            .is_some_and(|stamp| *stamp != source.stamp())
        {
            Err(BlockLightError::SourceMismatch)
        } else {
            Ok(())
        }
    }

    fn require_storage(&self, storage: &LightSectionStorage) -> Result<(), BlockLightError> {
        if self
            .storage_stamp
            .as_ref()
            .is_some_and(|stamp| *stamp != storage.stamp())
        {
            Err(BlockLightError::StorageMismatch)
        } else {
            Ok(())
        }
    }
}

fn require_block(storage: &LightSectionStorage) -> Result<(), BlockLightError> {
    if storage.kind() == LightKind::Block {
        Ok(())
    } else {
        Err(BlockLightError::WrongLayer)
    }
}

#[derive(Clone, Copy)]
struct Write {
    pos: LightBlock,
    level: u8,
}
struct Plan {
    writes: [Write; 7],
    write_count: usize,
    decreases: [Entry; 6],
    decrease_count: usize,
    increases: [Entry; 6],
    increase_count: usize,
}
impl Plan {
    fn new() -> Self {
        Self {
            writes: [Write {
                pos: ZERO,
                level: 0,
            }; 7],
            write_count: 0,
            decreases: [BLANK; 6],
            decrease_count: 0,
            increases: [BLANK; 6],
            increase_count: 0,
        }
    }
    fn write(&mut self, pos: LightBlock, level: u8) {
        self.writes[self.write_count] = Write { pos, level };
        self.write_count += 1;
    }
    fn decrease(&mut self, entry: Entry) {
        self.decreases[self.decrease_count] = entry;
        self.decrease_count += 1;
    }
    fn increase(&mut self, entry: Entry) {
        self.increases[self.increase_count] = entry;
        self.increase_count += 1;
    }
}

fn check_plan(source: &LightingSource, storage: &LightSectionStorage, pos: LightBlock) -> Plan {
    let mut plan = Plan::new();
    let Some(old) = storage.stored_level(pos) else {
        return plan;
    };
    let material = source
        .registry()
        .light_material(source.state_at(pos))
        .expect("admitted state");
    let emitted = if storage.light_enabled(pos.column()) {
        material.emission
    } else {
        0
    };
    if emitted < old {
        plan.write(pos, 0);
        plan.decrease(Entry {
            pos,
            level: old,
            directions: ALL,
            ..BLANK
        });
    } else {
        plan.decrease(Entry {
            pos,
            level: 1,
            directions: ALL,
            ..BLANK
        });
    }
    if emitted != 0 {
        plan.increase(emission(pos, emitted, material.empty_shape()));
    }
    plan
}

fn decrease_plan(source: &LightingSource, storage: &LightSectionStorage, entry: Entry) -> Plan {
    let mut plan = Plan::new();
    for direction in LightDirection::ALL {
        if entry.directions & mask(direction) == 0 {
            continue;
        }
        let pos = direction.step(entry.pos);
        let Some(old) = storage.stored_level(pos).filter(|&value| value != 0) else {
            continue;
        };
        let backwards = mask(direction.opposite());
        if i16::from(old) < i16::from(entry.level) {
            let material = source
                .registry()
                .light_material(source.state_at(pos))
                .expect("admitted state");
            let emitted = if storage.light_enabled(pos.column()) {
                material.emission
            } else {
                0
            };
            plan.write(pos, 0);
            if emitted < old {
                plan.decrease(Entry {
                    pos,
                    level: old,
                    directions: ALL ^ backwards,
                    ..BLANK
                });
            }
            if emitted != 0 {
                plan.increase(emission(pos, emitted, material.empty_shape()));
            }
        } else {
            plan.increase(Entry {
                pos,
                level: old,
                directions: backwards,
                empty_shape: false,
                from_emission: false,
            });
        }
    }
    plan
}

fn increase_plan(
    source: &LightingSource,
    storage: &LightSectionStorage,
    entry: Entry,
) -> Result<Plan, StorageError> {
    let mut plan = Plan::new();
    let old = storage
        .stored_level(entry.pos)
        .ok_or(StorageError::MissingLayer)?;
    let level = if entry.from_emission && old < entry.level {
        plan.write(entry.pos, entry.level);
        entry.level
    } else {
        old
    };
    if level != entry.level {
        return Ok(plan);
    }
    for direction in LightDirection::ALL {
        if entry.directions & mask(direction) == 0 {
            continue;
        }
        let pos = direction.step(entry.pos);
        let Some(old) = storage.stored_level(pos) else {
            continue;
        };
        if level.saturating_sub(1) <= old {
            continue;
        }
        let material = source
            .registry()
            .light_material(source.state_at(pos))
            .expect("admitted state");
        let new = level.saturating_sub(material.dampening.max(1));
        if new <= old {
            continue;
        }
        let from_face = if entry.empty_shape {
            0
        } else {
            let from = source
                .registry()
                .light_material(source.state_at(entry.pos))
                .expect("admitted source state");
            if from.empty_shape() {
                0
            } else {
                from.faces[direction as usize]
            }
        };
        let to_face = if material.empty_shape() {
            0
        } else {
            material.faces[direction.opposite() as usize]
        };
        if source
            .registry()
            .face_occludes(from_face, to_face)
            .expect("admitted face IDs")
        {
            continue;
        }
        plan.write(pos, new);
        if new > 1 {
            plan.increase(Entry {
                pos,
                level: new,
                directions: ALL ^ mask(direction.opposite()),
                empty_shape: material.empty_shape(),
                from_emission: false,
            });
        }
    }
    Ok(plan)
}

fn emission(pos: LightBlock, level: u8, empty_shape: bool) -> Entry {
    Entry {
        pos,
        level,
        directions: ALL,
        empty_shape,
        from_emission: true,
    }
}
fn mask(direction: LightDirection) -> u8 {
    1 << direction as u8
}
