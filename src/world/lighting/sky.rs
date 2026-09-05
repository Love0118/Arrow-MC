//! Sky-light propagation with explicit column sources and bounded pending work.
//!
//! A run plans one check/propagation step before reserving its writes and queue
//! slots. Pressure retains that step and the private updating light; only a
//! complete run publishes. Source population has a section/column cursor, so a
//! large source set can resume after an explicitly budgeted queue growth.

use super::queue::{CheckQueue, Entry, QueueError, WorkQueue};
use super::sources::{SkySources, SourcesError};
use super::storage::{LightSectionStorage, StorageError};
use super::{LightBlock, LightDirection, LightKind, LightSection, LightingSource, SourceStamp};
use crate::world::preparation::ChunkAddress;
use std::fmt;

const ALL: u8 = 63;
const NO_UP: u8 = ALL & !(1 << 1);
const DIRECTIONS: [LightDirection; 6] = [
    LightDirection::Down,
    LightDirection::Up,
    LightDirection::North,
    LightDirection::South,
    LightDirection::West,
    LightDirection::East,
];

#[derive(Clone, Copy, Debug)]
pub struct SkyLimits {
    pub checks: usize,
    pub queue_entries: usize,
    pub source_chunks: usize,
    /// One checked column or empty-section bridge must fit without partial work.
    pub planned_writes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkyRunProgress {
    pub processed: usize,
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkyStats {
    pub pending_increases: usize,
    pub pending_decreases: usize,
    pub peak_increases: usize,
    pub peak_decreases: usize,
    pub source_chunks: usize,
    /// Engine-owned backing allocations, excluding the separately charged storage/source.
    pub heap_bytes: usize,
}

#[derive(Debug)]
pub enum SkyError {
    Queue(QueueError),
    Storage(StorageError),
    Sources(SourcesError),
    AllocationLimit,
    AllocationFailed,
    InvalidMaterial,
    InvalidStorage,
    InvalidCoordinate,
    StaleSource,
    Busy,
    PlanFull,
    QueueCapacity { increase: usize, decrease: usize },
}
impl fmt::Display for SkyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sky lighting: {self:?}")
    }
}
impl std::error::Error for SkyError {}
impl From<QueueError> for SkyError {
    fn from(e: QueueError) -> Self {
        Self::Queue(e)
    }
}
impl From<StorageError> for SkyError {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}
impl From<SourcesError> for SkyError {
    fn from(e: SourcesError) -> Self {
        Self::Sources(e)
    }
}

struct ColumnSources {
    chunk: ChunkAddress,
    sources: SkySources,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceContext {
    registry: [u8; 32],
    min_section: i8,
    max_section: i8,
}
impl SourceContext {
    fn of(source: &LightingSource) -> Self {
        Self {
            registry: source.registry().manifest_sha256(),
            min_section: source.height().min_section(),
            max_section: source.height().max_section(),
        }
    }
}
#[derive(Clone, Copy)]
struct Populate {
    chunk: ChunkAddress,
    section_y: i32,
    column: u16,
    sources_below: bool,
}
#[derive(Clone, Copy)]
struct Enable {
    chunk: ChunkAddress,
    next_section: i32,
    bottom_section: i32,
}

struct Plan {
    positions: Vec<LightBlock>,
    levels: Vec<u8>,
    increase: Vec<Entry>,
    decrease: Vec<Entry>,
    limit: usize,
}
impl Plan {
    fn new(limit: usize, budget: &mut usize) -> Result<Self, SkyError> {
        Ok(Self {
            positions: reserve(limit, budget)?,
            levels: reserve(limit, budget)?,
            increase: reserve(limit, budget)?,
            decrease: reserve(limit, budget)?,
            limit,
        })
    }
    fn clear(&mut self) {
        self.positions.clear();
        self.levels.clear();
        self.increase.clear();
        self.decrease.clear();
    }
    fn write(&mut self, pos: LightBlock, level: u8) -> Result<(), SkyError> {
        if self.positions.len() == self.limit {
            return Err(SkyError::PlanFull);
        }
        self.positions.push(pos);
        self.levels.push(level);
        Ok(())
    }
    fn entry(&mut self, increase: bool, entry: Entry) -> Result<(), SkyError> {
        let queue = if increase {
            &mut self.increase
        } else {
            &mut self.decrease
        };
        if queue.len() == self.limit {
            return Err(SkyError::PlanFull);
        }
        queue.push(entry);
        Ok(())
    }
    fn latest(&self, pos: LightBlock) -> Option<u8> {
        self.positions
            .iter()
            .rposition(|p| *p == pos)
            .map(|i| self.levels[i])
    }
}

pub struct SkyLightEngine {
    storage: LightSectionStorage,
    checks: CheckQueue,
    increase: WorkQueue,
    decrease: WorkQueue,
    sources: Vec<ColumnSources>,
    source_limit: usize,
    source_context: Option<SourceContext>,
    plan: Plan,
    populate: Option<Populate>,
    enable: Option<Enable>,
    running: bool,
    active_source: Option<SourceStamp>,
    peak_increases: usize,
    peak_decreases: usize,
    dirty: bool,
}

impl SkyLightEngine {
    pub fn new(
        storage: LightSectionStorage,
        limits: SkyLimits,
        remaining: &mut usize,
    ) -> Result<Self, SkyError> {
        if limits.source_chunks == 0 || limits.planned_writes < 16 {
            return Err(SkyError::AllocationLimit);
        }
        if storage.kind() != LightKind::Sky {
            return Err(SkyError::InvalidStorage);
        }
        let mut budget = *remaining;
        let engine = Self {
            storage,
            checks: CheckQueue::new(limits.checks, &mut budget)?,
            increase: WorkQueue::new(limits.queue_entries, &mut budget)?,
            decrease: WorkQueue::new(limits.queue_entries, &mut budget)?,
            sources: reserve(limits.source_chunks, &mut budget)?,
            source_limit: limits.source_chunks,
            source_context: None,
            plan: Plan::new(limits.planned_writes, &mut budget)?,
            populate: None,
            enable: None,
            running: false,
            active_source: None,
            peak_increases: 0,
            peak_decreases: 0,
            dirty: false,
        };
        *remaining = budget;
        Ok(engine)
    }
    pub fn storage(&self) -> &LightSectionStorage {
        &self.storage
    }
    pub fn stats(&self) -> SkyStats {
        SkyStats {
            pending_increases: self.increase.len(),
            pending_decreases: self.decrease.len(),
            peak_increases: self.peak_increases,
            peak_decreases: self.peak_decreases,
            source_chunks: self.sources.len(),
            heap_bytes: self.checks.heap_bytes()
                + self.increase.heap_bytes()
                + self.decrease.heap_bytes()
                + self.sources.capacity() * size_of::<ColumnSources>()
                + self.plan.positions.capacity() * size_of::<LightBlock>()
                + self.plan.levels.capacity()
                + (self.plan.increase.capacity() + self.plan.decrease.capacity())
                    * size_of::<Entry>(),
        }
    }
    pub fn storage_mut(&mut self) -> Result<&mut LightSectionStorage, SkyError> {
        if self.running || self.populate.is_some() || self.enable.is_some() {
            Err(SkyError::Busy)
        } else {
            Ok(&mut self.storage)
        }
    }
    pub fn sources(&self, chunk: ChunkAddress) -> Option<&SkySources> {
        self.source_index(chunk)
            .ok()
            .map(|index| &self.sources[index].sources)
    }
    fn source_index(&self, chunk: ChunkAddress) -> Result<usize, usize> {
        self.sources
            .binary_search_by_key(&(chunk.x, chunk.z), |entry| (entry.chunk.x, entry.chunk.z))
    }

    pub fn initialize_sources(
        &mut self,
        world: &LightingSource,
        chunk: ChunkAddress,
    ) -> Result<(), SkyError> {
        validate_chunk(chunk)?;
        if self.running || self.populate.is_some() || self.enable.is_some() {
            return Err(SkyError::Busy);
        }
        self.require_context(world)?;
        let sources = SkySources::initialize(world, chunk)?;
        match self.source_index(chunk) {
            Ok(index) => self.sources[index].sources = sources,
            Err(index) => {
                if self.sources.len() == self.source_limit {
                    return Err(SkyError::AllocationLimit);
                }
                self.sources.insert(index, ColumnSources { chunk, sources });
            }
        }
        self.source_context = Some(SourceContext::of(world));
        Ok(())
    }
    pub fn update_sources(
        &mut self,
        world: &LightingSource,
        pos: LightBlock,
    ) -> Result<bool, SkyError> {
        validate_block(pos)?;
        if self.running || self.populate.is_some() || self.enable.is_some() {
            return Err(SkyError::Busy);
        }
        let chunk = column(pos);
        let Ok(index) = self.source_index(chunk) else {
            return Ok(false);
        };
        Ok(self.sources[index].sources.update(world, pos)?)
    }
    pub fn remove_sources(&mut self, chunk: ChunkAddress) -> Result<(), SkyError> {
        validate_chunk(chunk)?;
        if self.running || self.populate.is_some() || self.enable.is_some() {
            return Err(SkyError::Busy);
        }
        if let Ok(i) = self.source_index(chunk) {
            self.sources.remove(i);
        }
        Ok(())
    }
    pub fn check_block(&mut self, pos: LightBlock) -> Result<(), SkyError> {
        validate_block(pos)?;
        if self.running || self.populate.is_some() || self.enable.is_some() {
            return Err(SkyError::Busy);
        }
        self.checks.insert(pos)?;
        Ok(())
    }
    pub fn grow_queues(&mut self, capacity: usize, remaining: &mut usize) -> Result<(), SkyError> {
        self.increase.grow(capacity, remaining)?;
        self.decrease.grow(capacity, remaining)?;
        Ok(())
    }
    pub fn grow_checks(&mut self, capacity: usize, remaining: &mut usize) -> Result<(), SkyError> {
        self.checks.grow(capacity, remaining)?;
        Ok(())
    }
    /// Replaces scratch only between complete planned steps. The old allocation
    /// remains charged until the replacement exists; pending queues/cursors stay.
    pub fn grow_plan(&mut self, capacity: usize, remaining: &mut usize) -> Result<(), SkyError> {
        if capacity <= self.plan.limit {
            return Ok(());
        }
        let mut budget = *remaining;
        let next = Plan::new(capacity, &mut budget)?;
        let old_bytes = self.plan.positions.capacity() * size_of::<LightBlock>()
            + self.plan.levels.capacity()
            + (self.plan.increase.capacity() + self.plan.decrease.capacity()) * size_of::<Entry>();
        let old = std::mem::replace(&mut self.plan, next);
        drop(old);
        *remaining = budget
            .checked_add(old_bytes)
            .ok_or(SkyError::AllocationLimit)?;
        Ok(())
    }

    /// A failed layer reservation retains the exact enable cursor. Resume the
    /// same enabled chunk after pressure is relieved; other work cannot publish
    /// the private, partially filled layers in the meantime.
    pub fn set_light_enabled(
        &mut self,
        chunk: ChunkAddress,
        enabled: bool,
    ) -> Result<(), SkyError> {
        validate_chunk(chunk)?;
        if self.running || self.populate.is_some() {
            return Err(SkyError::Busy);
        }
        if let Some(cursor) = self.enable {
            if cursor.chunk != chunk || !enabled {
                return Err(SkyError::Busy);
            }
        } else {
            self.storage.set_enabled(chunk, enabled)?;
            self.dirty = true;
            if !enabled {
                return Ok(());
            }
            // The empty-source sentinel is MIN; Java's MIN-1 intentionally
            // wraps, so enabling alone does not fill all absent sky columns.
            let highest = self
                .sources(chunk)
                .map_or(i32::MIN, SkySources::highest_lowest_source_y);
            let bottom = (highest.wrapping_sub(1) >> 4)
                .saturating_add(1)
                .max(self.storage.bottom_section_y());
            self.enable = Some(Enable {
                chunk,
                next_section: self.storage.top_section_y(chunk).saturating_sub(1),
                bottom_section: bottom,
            });
        }
        while let Some(mut cursor) = self.enable {
            if cursor.next_section < cursor.bottom_section {
                self.enable = None;
                break;
            }
            if let Some(mut layer) = self.storage.layer_to_write(LightSection {
                x: chunk.x,
                y: cursor.next_section,
                z: chunk.z,
            })? && layer.is_empty()
            {
                layer.fill(15)?;
            }
            cursor.next_section -= 1;
            self.enable = Some(cursor);
        }
        Ok(())
    }

    /// Starts/resumes the source initialization sequence. `QueueCapacity` gives
    /// total queue sizes required for the next column; draining propagation
    /// before this cursor finishes would change its initialization order.
    pub fn propagate_light_sources(&mut self, chunk: ChunkAddress) -> Result<(), SkyError> {
        self.populate_budgeted(chunk, usize::MAX).map(|_| ())
    }

    /// At most `max_columns` existing-section columns are initialized. A false
    /// result keeps the exact cursor and requires the same chunk on the next call.
    pub fn populate_budgeted(
        &mut self,
        chunk: ChunkAddress,
        max_columns: usize,
    ) -> Result<bool, SkyError> {
        validate_chunk(chunk)?;
        if self.running || self.enable.is_some() {
            return Err(SkyError::Busy);
        }
        if let Some(cursor) = self.populate {
            if cursor.chunk != chunk {
                return Err(SkyError::Busy);
            }
        } else {
            self.storage.set_enabled(chunk, true)?;
            self.dirty = true;
            self.populate = Some(Populate {
                chunk,
                section_y: self.storage.top_section_y(chunk).saturating_sub(1),
                column: 0,
                sources_below: false,
            });
        }
        let mut processed = 0;
        while let Some(mut cursor) = self.populate {
            if cursor.section_y < self.storage.bottom_section_y() {
                self.populate = None;
                break;
            }
            let section = LightSection {
                x: chunk.x,
                y: cursor.section_y,
                z: chunk.z,
            };
            if !self.storage.storing_light(section) {
                cursor.section_y -= 1;
                cursor.column = 0;
                self.populate = Some(cursor);
                continue;
            }
            if processed == max_columns {
                return Ok(false);
            }
            self.plan.clear();
            let x = chunk.x * 16 + i32::from(cursor.column & 15);
            let z = chunk.z * 16 + i32::from(cursor.column >> 4);
            let floor = cursor.section_y * 16;
            let source = self.lowest(x, z, i32::MIN);
            let neighbors = [
                self.lowest(x, z - 1, i32::MIN),
                self.lowest(x, z + 1, i32::MIN),
                self.lowest(x - 1, z, i32::MIN),
                self.lowest(x + 1, z, i32::MIN),
            ];
            if source <= floor + 15 {
                for y in (floor.max(source)..=floor + 15).rev() {
                    let pos = LightBlock { x, y, z };
                    self.plan.write(pos, 15)?;
                    let mut dirs = if y == source { 1 } else { 0 };
                    for (i, lowest) in neighbors.iter().enumerate() {
                        if y < *lowest {
                            dirs |= 1 << (i + 2);
                        }
                    }
                    if dirs != 0 {
                        self.plan.entry(true, entry(pos, 15, dirs, false))?;
                    }
                }
                if source < floor {
                    cursor.sources_below = true;
                }
            }
            self.commit_source_plan(section)?;
            processed += 1;
            cursor.column += 1;
            if cursor.column == 256 {
                if !cursor.sources_below {
                    self.populate = None;
                    break;
                }
                cursor.section_y -= 1;
                cursor.column = 0;
                cursor.sources_below = false;
            }
            self.populate = Some(cursor);
        }
        Ok(true)
    }

    pub fn has_work(&self) -> bool {
        self.populate.is_some()
            || self.enable.is_some()
            || self.running
            || !self.checks.is_empty()
            || !self.decrease.is_empty()
            || !self.increase.is_empty()
            || self.storage.has_updates()
            || self.dirty
    }
    pub fn run_updates(&mut self, world: &LightingSource) -> Result<usize, SkyError> {
        self.run_budgeted(world, usize::MAX)
            .map(|result| result.processed)
    }

    /// Work limits yield only between atomic plans. Pending work, source identity
    /// and the private storage remain pinned until a complete result is returned.
    pub fn run_budgeted(
        &mut self,
        world: &LightingSource,
        max_steps: usize,
    ) -> Result<SkyRunProgress, SkyError> {
        if self.populate.is_some() || self.enable.is_some() {
            return Err(SkyError::Busy);
        }
        self.require_context(world)?;
        let stamp = world.stamp();
        if self
            .active_source
            .as_ref()
            .is_some_and(|active| *active != stamp)
        {
            return Err(SkyError::StaleSource);
        }
        self.active_source = Some(stamp);
        self.running = true;
        let mut count = 0usize;
        while let Some(pos) = self.checks.peek() {
            if count == max_steps {
                return Ok(SkyRunProgress {
                    processed: count,
                    complete: false,
                });
            }
            self.plan.clear();
            self.plan_check(pos)?;
            self.commit_plan()?;
            self.checks.pop();
            count += 1;
        }
        self.checks.clear();
        while let Some(task) = self.decrease.peek() {
            if count == max_steps {
                return Ok(SkyRunProgress {
                    processed: count,
                    complete: false,
                });
            }
            self.plan.clear();
            self.plan_decrease(task)?;
            self.commit_plan()?;
            self.decrease.pop();
            count += 1;
        }
        while let Some(task) = self.increase.peek() {
            if count == max_steps {
                return Ok(SkyRunProgress {
                    processed: count,
                    complete: false,
                });
            }
            self.plan.clear();
            if self.storage.stored_level(task.pos) == Some(task.level) {
                self.plan_increase(world, task)?;
                self.commit_plan()?;
            }
            self.increase.pop();
            count += 1;
        }
        self.storage.process_inconsistencies()?;
        self.storage.publish_visible()?;
        self.running = false;
        self.active_source = None;
        self.dirty = false;
        Ok(SkyRunProgress {
            processed: count,
            complete: true,
        })
    }

    fn lowest(&self, x: i32, z: i32, missing: i32) -> i32 {
        self.sources(ChunkAddress {
            x: x >> 4,
            z: z >> 4,
        })
        .map_or(missing, |s| {
            s.lowest_source_y((x & 15) as u8, (z & 15) as u8)
                .expect("masked column coordinates")
        })
    }
    fn require_context(&self, world: &LightingSource) -> Result<(), SkyError> {
        if self
            .source_context
            .is_some_and(|context| context != SourceContext::of(world))
        {
            Err(SourcesError::ContextMismatch.into())
        } else {
            Ok(())
        }
    }
    fn commit_plan(&mut self) -> Result<(), SkyError> {
        if self.plan.increase.len() > self.increase.remaining_capacity()
            || self.plan.decrease.len() > self.decrease.remaining_capacity()
        {
            return Err(SkyError::QueueCapacity {
                increase: self.increase.len() + self.plan.increase.len(),
                decrease: self.decrease.len() + self.plan.decrease.len(),
            });
        }
        self.storage.prepare_writes(&self.plan.positions)?;
        for (&pos, &value) in self.plan.positions.iter().zip(&self.plan.levels) {
            self.storage.set_stored_level(pos, value)?;
        }
        for &task in &self.plan.decrease {
            self.decrease.push(task)?;
        }
        for &task in &self.plan.increase {
            self.increase.push(task)?;
        }
        self.record_queue_peaks();
        Ok(())
    }

    fn commit_source_plan(&mut self, section: LightSection) -> Result<(), SkyError> {
        if self.plan.increase.len() > self.increase.remaining_capacity() {
            return Err(SkyError::QueueCapacity {
                increase: self.increase.len() + self.plan.increase.len(),
                decrease: self.decrease.len(),
            });
        }
        if !self.plan.positions.is_empty() {
            let mut layer = self
                .storage
                .layer_to_write(section)?
                .expect("source section exists");
            layer.materialize(super::layer::LAYER_BYTES)?;
            for (&pos, &value) in self.plan.positions.iter().zip(&self.plan.levels) {
                layer.set(
                    (pos.x & 15) as u8,
                    (pos.y & 15) as u8,
                    (pos.z & 15) as u8,
                    i32::from(value),
                    0,
                )?;
            }
        }
        for &task in &self.plan.increase {
            self.increase.push(task)?;
        }
        self.record_queue_peaks();
        Ok(())
    }
    fn record_queue_peaks(&mut self) {
        self.peak_increases = self.peak_increases.max(self.increase.len());
        self.peak_decreases = self.peak_decreases.max(self.decrease.len());
    }
    fn plan_check(&mut self, pos: LightBlock) -> Result<(), SkyError> {
        let threshold = if self.storage.light_enabled(column(pos)) {
            self.lowest(pos.x, pos.z, i32::MAX)
        } else {
            i32::MAX
        };
        if threshold != i32::MAX {
            self.plan_column(pos.x, pos.z, threshold)?;
        }
        if self.storage.storing_light(section(pos)) {
            if pos.y >= threshold {
                self.plan.entry(false, entry(pos, 15, NO_UP, false))?;
                self.plan.entry(true, entry(pos, 15, NO_UP, false))?;
            } else {
                let old = self
                    .plan
                    .latest(pos)
                    .or_else(|| self.storage.stored_level(pos))
                    .unwrap_or(0);
                if old > 0 {
                    self.plan.write(pos, 0)?;
                    self.plan.entry(false, entry(pos, old, ALL, false))?;
                } else {
                    self.plan.entry(false, entry(pos, 1, ALL, false))?;
                }
            }
        }
        Ok(())
    }
    fn plan_column(&mut self, x: i32, z: i32, threshold: i32) -> Result<(), SkyError> {
        let bottom = self.storage.bottom_section_y();
        if bottom == i32::MAX {
            return Ok(());
        }
        let bottom_y = bottom * 16;
        if threshold > bottom_y {
            'remove: for sy in (bottom..=((threshold - 1) >> 4)).rev() {
                if !self.storage.storing_light(LightSection {
                    x: x >> 4,
                    y: sy,
                    z: z >> 4,
                }) {
                    continue;
                }
                for y in (sy * 16..=(sy * 16 + 15).min(threshold - 1)).rev() {
                    let pos = LightBlock { x, y, z };
                    if self.storage.stored_level(pos) != Some(15) {
                        break 'remove;
                    }
                    self.plan.write(pos, 0)?;
                    self.plan.entry(
                        false,
                        entry(pos, 15, if y == threshold - 1 { ALL } else { NO_UP }, false),
                    )?;
                }
            }
        }
        let neighbor = [
            self.lowest(x - 1, z, i32::MIN),
            self.lowest(x + 1, z, i32::MIN),
            self.lowest(x, z - 1, i32::MIN),
            self.lowest(x, z + 1, i32::MIN),
        ]
        .into_iter()
        .max()
        .unwrap();
        let start = threshold.max(bottom_y);
        let top = self.storage.top_section_y(ChunkAddress {
            x: x >> 4,
            z: z >> 4,
        });
        'add: for sy in (start >> 4)..top {
            if !self.storage.storing_light(LightSection {
                x: x >> 4,
                y: sy,
                z: z >> 4,
            }) {
                continue;
            }
            for y in (sy * 16).max(start)..=sy * 16 + 15 {
                let pos = LightBlock { x, y, z };
                if self.storage.stored_level(pos) == Some(15) {
                    break 'add;
                }
                self.plan.write(pos, 15)?;
                if y < neighbor || y == threshold {
                    self.plan.entry(true, entry(pos, 15, NO_UP, false))?;
                }
            }
        }
        Ok(())
    }
    fn empty_below(&self, pos: LightBlock) -> i32 {
        if pos.y & 15 != 0
            || (pos.x & 15 != 0 && pos.x & 15 != 15 && pos.z & 15 != 0 && pos.z & 15 != 15)
        {
            return 0;
        }
        let s = section(pos);
        let mut count = 0;
        while self.storage.has_light_data_at_or_below(s.y - count - 1)
            && !self.storage.storing_light(LightSection {
                x: s.x,
                y: s.y - count - 1,
                z: s.z,
            })
        {
            count += 1;
        }
        count
    }
    fn bridge(
        &mut self,
        pos: LightBlock,
        dir: LightDirection,
        level: u8,
        increase: bool,
        empty: i32,
    ) -> Result<(), SkyError> {
        let crossed = match dir {
            LightDirection::North => pos.z & 15 == 15,
            LightDirection::South => pos.z & 15 == 0,
            LightDirection::West => pos.x & 15 == 15,
            LightDirection::East => pos.x & 15 == 0,
            _ => false,
        };
        if empty == 0 || !crossed {
            return Ok(());
        }
        let s = section(pos);
        for sy in ((s.y - empty)..s.y).rev() {
            if !self.storage.storing_light(LightSection {
                x: s.x,
                y: sy,
                z: s.z,
            }) {
                continue;
            }
            for y in (sy * 16..=sy * 16 + 15).rev() {
                let at = LightBlock {
                    x: pos.x,
                    y,
                    z: pos.z,
                };
                self.plan.write(at, if increase { level } else { 0 })?;
                if !increase || level > 1 {
                    self.plan.entry(
                        increase,
                        entry(at, level, ALL & !(1 << ((dir as u8) ^ 1)), increase),
                    )?;
                }
            }
        }
        Ok(())
    }
    fn plan_decrease(&mut self, task: Entry) -> Result<(), SkyError> {
        let empty = self.empty_below(task.pos);
        for dir in DIRECTIONS {
            if task.directions & (1 << dir as u8) == 0 {
                continue;
            }
            let pos = offset(task.pos, dir);
            let Some(level) = self.storage.stored_level(pos) else {
                continue;
            };
            if level == 0 {
                continue;
            }
            let back = 1 << ((dir as u8) ^ 1);
            if level < task.level {
                self.plan.write(pos, 0)?;
                self.plan
                    .entry(false, entry(pos, level, ALL & !back, false))?;
                self.bridge(pos, dir, level, false, empty)?;
            } else {
                self.plan.entry(true, entry(pos, level, back, false))?;
            }
        }
        Ok(())
    }
    fn plan_increase(&mut self, world: &LightingSource, task: Entry) -> Result<(), SkyError> {
        let empty = self.empty_below(task.pos);
        let registry = world.registry();
        for dir in DIRECTIONS {
            if task.directions & (1 << dir as u8) == 0 {
                continue;
            }
            let pos = offset(task.pos, dir);
            let Some(old) = self.storage.stored_level(pos) else {
                continue;
            };
            if task.level.saturating_sub(1) <= old {
                continue;
            }
            let target = registry
                .light_material(world.state_at(pos))
                .ok_or(SkyError::InvalidMaterial)?;
            let level = task.level.saturating_sub(target.dampening.max(1));
            if level <= old {
                continue;
            }
            let from_face = if task.empty_shape {
                0
            } else {
                let material = registry
                    .light_material(world.state_at(task.pos))
                    .ok_or(SkyError::InvalidMaterial)?;
                if material.empty_shape() {
                    0
                } else {
                    material.faces[dir as usize]
                }
            };
            let to_face = if target.empty_shape() {
                0
            } else {
                target.faces[((dir as u8) ^ 1) as usize]
            };
            if registry
                .face_occludes(from_face, to_face)
                .ok_or(SkyError::InvalidMaterial)?
            {
                continue;
            }
            self.plan.write(pos, level)?;
            if level > 1 {
                self.plan.entry(
                    true,
                    entry(
                        pos,
                        level,
                        ALL & !(1 << ((dir as u8) ^ 1)),
                        target.empty_shape(),
                    ),
                )?;
            }
            self.bridge(pos, dir, level, true, empty)?;
        }
        Ok(())
    }
}

fn validate_chunk(chunk: ChunkAddress) -> Result<(), SkyError> {
    if (-2_097_061..=2_097_061).contains(&chunk.x) && (-2_097_061..=2_097_061).contains(&chunk.z) {
        Ok(())
    } else {
        Err(SkyError::InvalidCoordinate)
    }
}

fn validate_block(pos: LightBlock) -> Result<(), SkyError> {
    let horizontal = (-2_097_061 * 16)..=(2_097_061 * 16 + 15);
    if horizontal.contains(&pos.x) && horizontal.contains(&pos.z) && (-2032..=2031).contains(&pos.y)
    {
        Ok(())
    } else {
        Err(SkyError::InvalidCoordinate)
    }
}

fn reserve<T>(count: usize, budget: &mut usize) -> Result<Vec<T>, SkyError> {
    let bytes = count
        .checked_mul(size_of::<T>())
        .ok_or(SkyError::AllocationLimit)?;
    if bytes > *budget {
        return Err(SkyError::AllocationLimit);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(count)
        .map_err(|_| SkyError::AllocationFailed)?;
    let actual = out.capacity() * size_of::<T>();
    if actual > *budget {
        return Err(SkyError::AllocationLimit);
    }
    *budget -= actual;
    Ok(out)
}
fn entry(pos: LightBlock, level: u8, directions: u8, empty_shape: bool) -> Entry {
    Entry {
        pos,
        level,
        directions,
        empty_shape,
        from_emission: false,
    }
}
fn column(pos: LightBlock) -> ChunkAddress {
    ChunkAddress {
        x: pos.x >> 4,
        z: pos.z >> 4,
    }
}
fn section(pos: LightBlock) -> LightSection {
    LightSection {
        x: pos.x >> 4,
        y: pos.y >> 4,
        z: pos.z >> 4,
    }
}
fn offset(p: LightBlock, d: LightDirection) -> LightBlock {
    d.step(p)
}
