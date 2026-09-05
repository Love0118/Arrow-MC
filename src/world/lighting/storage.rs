//! Bounded section support, queued light and immutable visible snapshots.
//!
//! Each owned layer reserves its possible 2,048-byte backing once. Uniform
//! layers still allocate no backing bytes. Snapshot readers retain that allowance
//! until the final shared layer is dropped; this is a payload policy, not RSS.

use super::layer::{self, DataLayer, LAYER_BYTES};
use super::{LightBlock, LightKind, LightSection};
use crate::world::preparation::ChunkAddress;
use std::{
    fmt,
    ops::Deref,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone, Copy, Debug)]
pub struct StorageLimits {
    pub max_sections: usize,
    pub max_columns: usize,
    pub max_notifications: usize,
    pub metadata_bytes: usize,
    pub layer_bytes: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageError {
    Budget,
    MetadataLimit,
    SectionLimit,
    ColumnLimit,
    NotificationLimit,
    AllocationFailed,
    InvalidCoordinate,
    MissingLayer,
    InvalidLightValue,
    Layer(layer::Error),
}
impl From<layer::Error> for StorageError {
    fn from(error: layer::Error) -> Self {
        Self::Layer(error)
    }
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "light storage: {self:?}")
    }
}
impl std::error::Error for StorageError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionType {
    Empty,
    LightOnly,
    LightAndData,
}

struct Budget {
    limit: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
}
/// Opaque draft identity, retained by paused engine work without allocating a
/// second identity object or depending on a reusable stack address.
#[derive(Clone)]
pub struct StorageStamp(Arc<Budget>);
impl PartialEq for StorageStamp {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for StorageStamp {}
impl fmt::Debug for StorageStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StorageStamp(..)")
    }
}
impl Budget {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }
    fn add(&self, bytes: usize) -> Result<(), StorageError> {
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let next = used
                .checked_add(bytes)
                .filter(|&n| n <= self.limit)
                .ok_or(StorageError::Budget)?;
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => {
                    self.peak.fetch_max(next, Ordering::Relaxed);
                    return Ok(());
                }
                Err(current) => used = current,
            }
        }
    }
    fn reserve(self: &Arc<Self>, bytes: usize) -> Result<Lease, StorageError> {
        self.add(bytes)?;
        Ok(Lease {
            budget: Arc::clone(self),
            bytes,
        })
    }
}
struct Lease {
    budget: Arc<Budget>,
    bytes: usize,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
fn vector<T>(count: usize, budget: &Arc<Budget>) -> Result<(Vec<T>, Lease), StorageError> {
    let requested = count
        .checked_mul(size_of::<T>())
        .ok_or(StorageError::MetadataLimit)?;
    let mut lease = budget
        .reserve(requested)
        .map_err(|_| StorageError::MetadataLimit)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| StorageError::AllocationFailed)?;
    let actual = values
        .capacity()
        .checked_mul(size_of::<T>())
        .ok_or(StorageError::MetadataLimit)?;
    if actual > requested {
        budget
            .add(actual - requested)
            .map_err(|_| StorageError::MetadataLimit)?;
        lease.bytes = actual;
    }
    Ok((values, lease))
}
struct ChargedLayer {
    value: DataLayer,
    _lease: Lease,
}
/// Includes a conservative allowance for the shared value and Arc counters.
pub const LAYER_RESERVATION_BYTES: usize =
    LAYER_BYTES + size_of::<ChargedLayer>() + 2 * size_of::<usize>();
struct Entry {
    key: LightSection,
    state: u8,
    updating: Option<Arc<ChargedLayer>>,
    queued: Option<Arc<ChargedLayer>>,
    remove: bool,
}
#[derive(Clone, Copy)]
struct Column {
    key: ChunkAddress,
    enabled: bool,
    retain: bool,
    top: Option<i32>,
}
struct SnapshotLayer {
    key: LightSection,
    layer: Arc<ChargedLayer>,
}
#[derive(Clone, Copy)]
struct SnapshotTop {
    key: ChunkAddress,
    top: i32,
}
struct SnapshotData {
    kind: LightKind,
    layers: Vec<SnapshotLayer>,
    tops: Vec<SnapshotTop>,
    lowest: i32,
    _layers_lease: Lease,
    _tops_lease: Lease,
    _body_lease: Lease,
}
#[derive(Clone)]
pub struct LightSnapshot {
    inner: Arc<SnapshotData>,
}
impl LightSnapshot {
    pub fn kind(&self) -> LightKind {
        self.inner.kind
    }
    pub fn layer(&self, key: LightSection) -> Option<&DataLayer> {
        snapshot_index(&self.inner.layers, key)
            .ok()
            .map(|i| &self.inner.layers[i].layer.value)
    }
    pub fn get_level(&self, block: LightBlock) -> u8 {
        let key = section(block);
        if self.inner.kind == LightKind::Block {
            return self.layer(key).map_or(0, |v| nibble(v, block));
        }
        let top = self
            .inner
            .tops
            .binary_search_by_key(&column(key), |v| v.key)
            .ok()
            .map_or(self.inner.lowest, |i| self.inner.tops[i].top);
        if top == self.inner.lowest || key.y >= top {
            return 15;
        }
        match snapshot_index(&self.inner.layers, key) {
            Ok(i) => nibble(&self.inner.layers[i].layer.value, block),
            Err(i) => self
                .inner
                .layers
                .get(i)
                .filter(|next| column(next.key) == column(key))
                .map_or(15, |next| {
                    nibble(&next.layer.value, LightBlock { y: 0, ..block })
                }),
        }
    }
    pub fn sections(&self) -> impl Iterator<Item = LightSection> + '_ {
        self.inner.layers.iter().map(|entry| entry.key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageStats {
    pub sections: usize,
    pub queued: usize,
    pub notifications: usize,
    pub metadata_bytes: usize,
    pub reserved_layer_bytes: usize,
    pub peak_layer_bytes: usize,
}

pub struct LightSectionStorage {
    kind: LightKind,
    limits: StorageLimits,
    entries: Vec<Entry>,
    columns: Vec<Column>,
    notifications: Vec<LightSection>,
    published_notifications: Vec<LightSection>,
    scratch: Vec<LightSection>,
    staged: Vec<(LightSection, Arc<ChargedLayer>)>,
    visible: LightSnapshot,
    lowest: i32,
    changed: bool,
    inconsistent: bool,
    metadata: Arc<Budget>,
    layers: Arc<Budget>,
    _entries_lease: Lease,
    _columns_lease: Lease,
    _notifications_lease: Lease,
    _published_notifications_lease: Lease,
    _scratch_lease: Lease,
    _staged_lease: Lease,
}
impl LightSectionStorage {
    pub fn new(kind: LightKind, limits: StorageLimits) -> Result<Self, StorageError> {
        let metadata = Budget::new(limits.metadata_bytes);
        let layers = Budget::new(limits.layer_bytes);
        let (entries, entries_lease) = vector(limits.max_sections, &metadata)?;
        let (columns, columns_lease) = vector(limits.max_columns, &metadata)?;
        let (notifications, notifications_lease) = vector(limits.max_notifications, &metadata)?;
        let (published_notifications, published_notifications_lease) =
            vector(limits.max_notifications, &metadata)?;
        let (scratch, scratch_lease) = vector(limits.max_notifications, &metadata)?;
        let (staged, staged_lease) = vector(limits.max_sections, &metadata)?;
        let (visible_layers, vl) = vector(0, &metadata)?;
        let (tops, vt) = vector(0, &metadata)?;
        let body = metadata
            .reserve(size_of::<SnapshotData>() + 2 * size_of::<usize>())
            .map_err(|_| StorageError::MetadataLimit)?;
        let visible = LightSnapshot {
            inner: Arc::new(SnapshotData {
                kind,
                layers: visible_layers,
                tops,
                lowest: i32::MAX,
                _layers_lease: vl,
                _tops_lease: vt,
                _body_lease: body,
            }),
        };
        Ok(Self {
            kind,
            limits,
            entries,
            columns,
            notifications,
            published_notifications,
            scratch,
            staged,
            visible,
            lowest: i32::MAX,
            changed: false,
            inconsistent: false,
            metadata,
            layers,
            _entries_lease: entries_lease,
            _columns_lease: columns_lease,
            _notifications_lease: notifications_lease,
            _published_notifications_lease: published_notifications_lease,
            _scratch_lease: scratch_lease,
            _staged_lease: staged_lease,
        })
    }
    pub fn kind(&self) -> LightKind {
        self.kind
    }
    /// Admission for an owned CPU job must include both configured maxima;
    /// current stats alone omit possible COW copies and visible index growth.
    pub fn limits(&self) -> StorageLimits {
        self.limits
    }
    pub fn stamp(&self) -> StorageStamp {
        StorageStamp(Arc::clone(&self.metadata))
    }
    pub fn storing_light(&self, key: LightSection) -> bool {
        self.entry_index(key)
            .ok()
            .is_some_and(|i| self.entries[i].updating.is_some())
    }
    pub fn layer(&self, key: LightSection, updating: bool) -> Option<&DataLayer> {
        if updating {
            self.entry_index(key)
                .ok()
                .and_then(|i| self.entries[i].updating.as_ref())
                .map(|v| &v.value)
        } else {
            self.visible.layer(key)
        }
    }
    /// Queued data has precedence over the published map, as in getDataLayerData.
    pub fn data_layer_data(&self, key: LightSection) -> Option<&DataLayer> {
        self.entry_index(key)
            .ok()
            .and_then(|i| self.entries[i].queued.as_ref())
            .map(|v| &v.value)
            .or_else(|| self.visible.layer(key))
    }
    pub fn section_type(&self, key: LightSection) -> SectionType {
        match self
            .entry_index(key)
            .ok()
            .map_or(0, |i| self.entries[i].state)
        {
            0 => SectionType::Empty,
            n if n & 32 != 0 => SectionType::LightAndData,
            _ => SectionType::LightOnly,
        }
    }
    pub fn neighbor_count(&self, key: LightSection) -> u8 {
        self.entry_index(key)
            .ok()
            .map_or(0, |i| self.entries[i].state & 31)
    }
    pub fn light_enabled(&self, key: ChunkAddress) -> bool {
        self.column_index(key)
            .ok()
            .is_some_and(|i| self.columns[i].enabled)
    }
    pub fn set_enabled(&mut self, key: ChunkAddress, enabled: bool) -> Result<(), StorageError> {
        if let Ok(i) = self.column_index(key) {
            self.columns[i].enabled = enabled;
            self.prune_columns();
        } else if enabled {
            self.insert_column(key)?.enabled = true;
        }
        Ok(())
    }
    pub fn retain_data(&mut self, key: ChunkAddress, retain: bool) -> Result<(), StorageError> {
        if let Ok(i) = self.column_index(key) {
            self.columns[i].retain = retain;
            self.prune_columns();
        } else if retain {
            self.insert_column(key)?.retain = true;
        }
        Ok(())
    }
    pub fn top_section_y(&self, key: ChunkAddress) -> i32 {
        self.column_index(key)
            .ok()
            .and_then(|i| self.columns[i].top)
            .unwrap_or(self.lowest)
    }
    pub fn bottom_section_y(&self) -> i32 {
        self.lowest
    }
    pub fn has_light_data_at_or_below(&self, y: i32) -> bool {
        y >= self.lowest
    }
    pub fn is_above_data(&self, key: LightSection) -> bool {
        let top = self.top_section_y(column(key));
        top == self.lowest || key.y >= top
    }
    pub fn stored_level(&self, block: LightBlock) -> Option<u8> {
        self.layer(section(block), true).map(|v| nibble(v, block))
    }
    pub fn get_level(&self, block: LightBlock, updating: bool) -> u8 {
        if !updating {
            return self.visible.get_level(block);
        }
        let key = section(block);
        if self.kind == LightKind::Block {
            return self.stored_level(block).unwrap_or(0);
        }
        let top = self.top_section_y(column(key));
        if top == self.lowest || key.y >= top {
            return if self.light_enabled(column(key)) {
                15
            } else {
                0
            };
        }
        if let Some(layer) = self.layer(key, true) {
            return nibble(layer, block);
        }
        self.next_above(key)
            .map_or(15, |v| nibble(&v.value, LightBlock { y: 0, ..block }))
    }
    pub fn queue_data(
        &mut self,
        key: LightSection,
        data: Option<&DataLayer>,
    ) -> Result<(), StorageError> {
        self.preflight_entry(key, data.is_some())?;
        let value = match data {
            Some(data) => Some(self.copy_layer(data, false)?),
            None => None,
        };
        self.queue_owned(key, value);
        Ok(())
    }
    pub fn queue_bytes(
        &mut self,
        key: LightSection,
        bytes: Option<&[u8]>,
    ) -> Result<(), StorageError> {
        self.preflight_entry(key, bytes.is_some())?;
        let value = match bytes {
            Some(bytes) => {
                let lease = self.layers.reserve(LAYER_RESERVATION_BYTES)?;
                let value = DataLayer::from_bytes(bytes, LAYER_BYTES)?;
                Some(Arc::new(ChargedLayer {
                    value,
                    _lease: lease,
                }))
            }
            None => None,
        };
        self.queue_owned(key, value);
        Ok(())
    }
    fn queue_owned(&mut self, key: LightSection, value: Option<Arc<ChargedLayer>>) {
        if value.is_some() {
            self.inconsistent = true;
        }
        match self.entry_index(key) {
            Ok(i) => self.entries[i].queued = value,
            Err(i) if value.is_some() => self.entries.insert(
                i,
                Entry {
                    key,
                    state: 0,
                    updating: None,
                    queued: value,
                    remove: false,
                },
            ),
            Err(_) => {}
        }
        self.prune_entries();
    }

    /// A status transition changes the center and all 26 neighbors atomically.
    pub fn update_section_status(
        &mut self,
        key: LightSection,
        empty: bool,
    ) -> Result<(), StorageError> {
        let old = self
            .entry_index(key)
            .ok()
            .map_or(0, |i| self.entries[i].state);
        if (old & 32 != 0) != empty {
            return Ok(());
        }
        let mut plan: [Option<Transition>; 27] = [const { None }; 27];
        plan[0] = Some(self.transition(key, if empty { old & !32 } else { old | 32 }));
        let mut count = 1;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if dx == 0 && dy == 0 && dz == 0 {
                        continue;
                    }
                    let neighbor = offset(key, dx, dy, dz)?;
                    let state = self
                        .entry_index(neighbor)
                        .ok()
                        .map_or(0, |i| self.entries[i].state);
                    let support = i32::from(state & 31) + if empty { -1 } else { 1 };
                    if !(0..=26).contains(&support) {
                        return Err(StorageError::InvalidCoordinate);
                    }
                    plan[count] = Some(self.transition(neighbor, (state & 32) | support as u8));
                    count += 1;
                }
            }
        }
        let new_entries = plan
            .iter()
            .flatten()
            .filter(|p| p.state != 0 && self.entry_index(p.key).is_err())
            .count();
        if self.entries.len() + new_entries > self.limits.max_sections {
            return Err(StorageError::SectionLimit);
        }
        self.reset_scratch();
        let mut new_columns = [None; 27];
        let mut columns_count = 0;
        for step in plan.iter().flatten().filter(|p| p.initialize) {
            self.stage_neighbors(step.key)?;
            let col = column(step.key);
            if self.kind == LightKind::Sky
                && self.column_index(col).is_err()
                && !new_columns[..columns_count].contains(&Some(col))
            {
                new_columns[columns_count] = Some(col);
                columns_count += 1;
            }
        }
        if self.columns.len() + columns_count > self.limits.max_columns {
            return Err(StorageError::ColumnLimit);
        }
        let mut lowest = self.lowest;
        for index in 0..27 {
            let step = plan[index].as_ref().unwrap();
            if !step.initialize {
                continue;
            }
            let layer = self.create_layer(step.key, &plan[..index], lowest)?;
            lowest = lowest.min(step.key.y);
            plan[index].as_mut().unwrap().layer = Some(layer);
        }
        for step in plan.into_iter().flatten() {
            let i = match self.entry_index(step.key) {
                Ok(i) => i,
                Err(i) if step.state != 0 => {
                    self.entries.insert(
                        i,
                        Entry {
                            key: step.key,
                            state: 0,
                            updating: None,
                            queued: None,
                            remove: false,
                        },
                    );
                    i
                }
                Err(_) => continue,
            };
            let entry = &mut self.entries[i];
            let previous = entry.state;
            entry.state = step.state;
            if previous == 0 && step.state != 0 {
                if entry.remove {
                    entry.remove = false;
                } else {
                    entry.updating = step.layer;
                    self.changed = true;
                    self.inconsistent = true;
                    if self.kind == LightKind::Sky {
                        self.lowest = self.lowest.min(step.key.y);
                        let col = self
                            .insert_column(column(step.key))
                            .expect("preflight columns");
                        col.top = Some(
                            col.top
                                .map_or(step.key.y + 1, |top| top.max(step.key.y + 1)),
                        );
                    }
                }
            } else if previous != 0 && step.state == 0 {
                self.entries[i].remove = true;
                self.inconsistent = true;
            }
        }
        self.notifications.clear();
        self.notifications.extend_from_slice(&self.scratch);
        Ok(())
    }

    pub fn process_inconsistencies(&mut self) -> Result<(), StorageError> {
        if !self.inconsistent {
            return Ok(());
        }
        for index in 0..self.entries.len() {
            if !self.entries[index].remove {
                continue;
            }
            let retain = self
                .column_index(column(self.entries[index].key))
                .ok()
                .is_some_and(|i| self.columns[i].retain);
            let entry = &mut self.entries[index];
            let queued = entry.queued.take();
            let stored = entry.updating.take();
            if retain {
                entry.queued = queued.or(stored);
            }
            entry.remove = false;
            self.changed = true;
        }
        if self.kind == LightKind::Sky {
            for col in &mut self.columns {
                col.top = self
                    .entries
                    .iter()
                    .rev()
                    .find(|entry| column(entry.key) == col.key && entry.updating.is_some())
                    .map(|entry| entry.key.y + 1);
            }
        }
        for entry in &mut self.entries {
            if entry.updating.is_some()
                && let Some(queued) = entry.queued.take()
                && !Arc::ptr_eq(entry.updating.as_ref().unwrap(), &queued)
            {
                entry.updating = Some(queued);
                self.changed = true;
            }
        }
        self.inconsistent = false;
        self.prune_entries();
        self.prune_columns();
        Ok(())
    }

    /// Stages every required copy/materialization before installing any of them.
    /// Call only for actual writes after the engine's queue/fanout admission.
    pub fn prepare_writes(&mut self, blocks: &[LightBlock]) -> Result<(), StorageError> {
        self.reset_scratch();
        for &block in blocks {
            self.stage_block(block)?;
        }
        self.staged.clear();
        let result = self.stage_write_layers(blocks);
        if result.is_err() {
            self.staged.clear();
            return result;
        }
        while let Some((key, layer)) = self.staged.pop() {
            let i = self.entry_index(key).expect("staged existing section");
            let entry = &mut self.entries[i];
            let queued_alias = entry
                .queued
                .as_ref()
                .is_some_and(|q| Arc::ptr_eq(q, entry.updating.as_ref().unwrap()));
            if queued_alias {
                entry.queued = Some(Arc::clone(&layer));
            }
            entry.updating = Some(layer);
            self.changed = true;
        }
        Ok(())
    }
    fn stage_write_layers(&mut self, blocks: &[LightBlock]) -> Result<(), StorageError> {
        for &block in blocks {
            let key = section(block);
            if self.staged.iter().any(|(candidate, _)| *candidate == key) {
                continue;
            }
            let Some(layer) = self
                .entry_index(key)
                .ok()
                .and_then(|i| self.entries[i].updating.as_ref())
            else {
                continue;
            };
            if self.staged.len() == self.limits.max_sections {
                return Err(StorageError::SectionLimit);
            }
            // A queued alias will be reattached after each write. The immutable
            // visible map and external snapshots must keep the old value.
            let queued_alias = self.entry_index(key).ok().is_some_and(|i| {
                self.entries[i]
                    .queued
                    .as_ref()
                    .is_some_and(|q| Arc::ptr_eq(q, layer))
            });
            if !layer.value.is_definitely_homogeneous()
                && Arc::strong_count(layer) == 1 + usize::from(queued_alias)
            {
                continue;
            }
            let mut copy = self.copy_layer(&layer.value, false)?;
            Arc::get_mut(&mut copy)
                .unwrap()
                .value
                .materialize(LAYER_BYTES)?;
            self.staged.push((key, copy));
        }
        Ok(())
    }
    pub fn set_stored_level(&mut self, block: LightBlock, value: u8) -> Result<(), StorageError> {
        if value > 15 {
            return Err(StorageError::InvalidLightValue);
        }
        if !self.storing_light(section(block)) {
            return Err(StorageError::MissingLayer);
        }
        self.prepare_writes(&[block])?;
        {
            let mut layer = self
                .layer_to_write(section(block))?
                .ok_or(StorageError::MissingLayer)?;
            layer.set(
                local(block.x),
                local(block.y),
                local(block.z),
                i32::from(value),
                0,
            )?;
        }
        // The exact affected set was admitted before changing the light value.
        self.notifications.clear();
        self.notifications.extend_from_slice(&self.scratch);
        Ok(())
    }
    pub fn layer_to_write(
        &mut self,
        key: LightSection,
    ) -> Result<Option<LayerWrite<'_>>, StorageError> {
        let Ok(index) = self.entry_index(key) else {
            return Ok(None);
        };
        let Some(current) = self.entries[index].updating.as_ref() else {
            return Ok(None);
        };
        let queued_alias = self.entries[index]
            .queued
            .as_ref()
            .is_some_and(|q| Arc::ptr_eq(q, current));
        if Arc::strong_count(current) > 1 + usize::from(queued_alias) {
            let copy = self.copy_layer(&current.value, false)?;
            if queued_alias {
                self.entries[index].queued = Some(Arc::clone(&copy));
            }
            self.entries[index].updating = Some(copy);
        }
        if queued_alias {
            self.entries[index].queued = None;
        }
        self.changed = true;
        Ok(Some(LayerWrite {
            entry: &mut self.entries[index],
            queued_alias,
        }))
    }
    pub fn affected_sections(&self) -> &[LightSection] {
        &self.notifications
    }
    pub fn has_inconsistencies(&self) -> bool {
        self.inconsistent
    }
    /// Pending updating-map work, excluding already published notifications.
    pub fn has_updates(&self) -> bool {
        self.changed || self.inconsistent || !self.notifications.is_empty()
    }
    pub fn publish_visible(&mut self) -> Result<(), StorageError> {
        self.scratch.clear();
        self.scratch
            .extend_from_slice(&self.published_notifications);
        for index in 0..self.notifications.len() {
            self.stage_notification(self.notifications[index])?;
        }
        if self.changed {
            let count = self
                .entries
                .iter()
                .filter(|entry| entry.updating.is_some())
                .count();
            let tops_count = self.columns.iter().filter(|col| col.top.is_some()).count();
            let (mut layers, ll) = vector(count, &self.metadata)?;
            let (mut tops, tl) = vector(tops_count, &self.metadata)?;
            let body = self
                .metadata
                .reserve(size_of::<SnapshotData>() + 2 * size_of::<usize>())
                .map_err(|_| StorageError::MetadataLimit)?;
            layers.extend(self.entries.iter().filter_map(|entry| {
                entry.updating.as_ref().map(|layer| SnapshotLayer {
                    key: entry.key,
                    layer: Arc::clone(layer),
                })
            }));
            tops.extend(
                self.columns
                    .iter()
                    .filter_map(|col| col.top.map(|top| SnapshotTop { key: col.key, top })),
            );
            let next = LightSnapshot {
                inner: Arc::new(SnapshotData {
                    kind: self.kind,
                    layers,
                    tops,
                    lowest: self.lowest,
                    _layers_lease: ll,
                    _tops_lease: tl,
                    _body_lease: body,
                }),
            };
            self.visible = next;
            self.changed = false;
        }
        self.published_notifications.clear();
        self.published_notifications
            .extend_from_slice(&self.scratch);
        self.notifications.clear();
        Ok(())
    }
    /// Notifications become deliverable only after a successful visible swap.
    /// They accumulate until the owner acknowledges delivery, without silent loss.
    pub fn published_sections(&self) -> &[LightSection] {
        &self.published_notifications
    }
    pub fn clear_published_notifications(&mut self) {
        self.published_notifications.clear();
    }
    pub fn snapshot(&self) -> LightSnapshot {
        self.visible.clone()
    }
    pub fn stats(&self) -> StorageStats {
        StorageStats {
            sections: self.entries.iter().filter(|e| e.updating.is_some()).count(),
            queued: self.entries.iter().filter(|e| e.queued.is_some()).count(),
            notifications: self.notifications.len(),
            metadata_bytes: self.metadata.used.load(Ordering::Relaxed),
            reserved_layer_bytes: self.layers.used.load(Ordering::Relaxed),
            peak_layer_bytes: self.layers.peak.load(Ordering::Relaxed),
        }
    }

    fn transition(&self, key: LightSection, state: u8) -> Transition {
        let initialize = self.entry_index(key).map_or(state != 0, |i| {
            self.entries[i].state == 0 && state != 0 && !self.entries[i].remove
        });
        Transition {
            key,
            state,
            initialize,
            layer: None,
        }
    }
    fn create_layer(
        &self,
        key: LightSection,
        plan: &[Option<Transition>],
        lowest: i32,
    ) -> Result<Arc<ChargedLayer>, StorageError> {
        if let Some(queued) = self
            .entry_index(key)
            .ok()
            .and_then(|i| self.entries[i].queued.as_ref())
        {
            return Ok(Arc::clone(queued));
        }
        if self.kind == LightKind::Sky {
            let old_top = self
                .column_index(column(key))
                .ok()
                .and_then(|i| self.columns[i].top);
            let new_top = plan
                .iter()
                .flatten()
                .filter(|p| p.layer.is_some() && column(p.key) == column(key))
                .map(|p| p.key.y + 1)
                .max();
            let top = old_top.into_iter().chain(new_top).max().unwrap_or(lowest);
            if top != lowest && key.y < top {
                let old = self
                    .next_above_entry(key)
                    .map(|e| (e.key.y, e.updating.as_ref().unwrap()));
                let new = plan
                    .iter()
                    .flatten()
                    .filter(|p| p.key.y > key.y && column(p.key) == column(key))
                    .filter_map(|p| p.layer.as_ref().map(|layer| (p.key.y, layer)))
                    .min_by_key(|(y, _)| *y);
                if let Some((_, above)) = old.into_iter().chain(new).min_by_key(|(y, _)| *y) {
                    return self.copy_layer(&above.value, true);
                }
            }
        }
        let lease = self.layers.reserve(LAYER_RESERVATION_BYTES)?;
        Ok(Arc::new(ChargedLayer {
            value: DataLayer::uniform(
                if self.kind == LightKind::Sky && self.light_enabled(column(key)) {
                    15
                } else {
                    0
                },
            ),
            _lease: lease,
        }))
    }
    fn copy_layer(
        &self,
        layer: &DataLayer,
        repeat: bool,
    ) -> Result<Arc<ChargedLayer>, StorageError> {
        let lease = self.layers.reserve(LAYER_RESERVATION_BYTES)?;
        let value = if repeat {
            layer.repeat_first_layer(LAYER_BYTES)?
        } else {
            layer.try_copy(LAYER_BYTES)?
        };
        validate_light(&value)?;
        Ok(Arc::new(ChargedLayer {
            value,
            _lease: lease,
        }))
    }
    fn preflight_entry(&self, key: LightSection, needed: bool) -> Result<(), StorageError> {
        if key.y == i32::MAX {
            return Err(StorageError::InvalidCoordinate);
        }
        if needed
            && self.entry_index(key).is_err()
            && self.entries.len() == self.limits.max_sections
        {
            return Err(StorageError::SectionLimit);
        }
        Ok(())
    }
    fn entry_index(&self, key: LightSection) -> Result<usize, usize> {
        self.entries
            .binary_search_by_key(&sort_key(key), |e| sort_key(e.key))
    }
    fn column_index(&self, key: ChunkAddress) -> Result<usize, usize> {
        self.columns.binary_search_by_key(&key, |c| c.key)
    }
    fn insert_column(&mut self, key: ChunkAddress) -> Result<&mut Column, StorageError> {
        let index = match self.column_index(key) {
            Ok(i) => i,
            Err(i) => {
                if self.columns.len() == self.limits.max_columns {
                    return Err(StorageError::ColumnLimit);
                }
                self.columns.insert(
                    i,
                    Column {
                        key,
                        enabled: false,
                        retain: false,
                        top: None,
                    },
                );
                i
            }
        };
        Ok(&mut self.columns[index])
    }
    fn next_above_entry(&self, key: LightSection) -> Option<&Entry> {
        let start = match self.entry_index(key) {
            Ok(i) => i + 1,
            Err(i) => i,
        };
        self.entries[start..]
            .iter()
            .take_while(|entry| column(entry.key) == column(key))
            .find(|entry| entry.updating.is_some())
    }
    fn next_above(&self, key: LightSection) -> Option<&Arc<ChargedLayer>> {
        self.next_above_entry(key).and_then(|e| e.updating.as_ref())
    }
    fn reset_scratch(&mut self) {
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.notifications);
    }
    fn stage_notification(&mut self, key: LightSection) -> Result<(), StorageError> {
        if let Err(i) = self
            .scratch
            .binary_search_by_key(&sort_key(key), |&key| sort_key(key))
        {
            if self.scratch.len() == self.limits.max_notifications {
                return Err(StorageError::NotificationLimit);
            }
            self.scratch.insert(i, key);
        }
        Ok(())
    }
    fn stage_neighbors(&mut self, key: LightSection) -> Result<(), StorageError> {
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    self.stage_notification(offset(key, x, y, z)?)?;
                }
            }
        }
        Ok(())
    }
    fn stage_block(&mut self, block: LightBlock) -> Result<(), StorageError> {
        let min = LightBlock {
            x: block
                .x
                .checked_sub(1)
                .ok_or(StorageError::InvalidCoordinate)?,
            y: block
                .y
                .checked_sub(1)
                .ok_or(StorageError::InvalidCoordinate)?,
            z: block
                .z
                .checked_sub(1)
                .ok_or(StorageError::InvalidCoordinate)?,
        };
        let max = LightBlock {
            x: block
                .x
                .checked_add(1)
                .ok_or(StorageError::InvalidCoordinate)?,
            y: block
                .y
                .checked_add(1)
                .ok_or(StorageError::InvalidCoordinate)?,
            z: block
                .z
                .checked_add(1)
                .ok_or(StorageError::InvalidCoordinate)?,
        };
        let min = section(min);
        let max = section(max);
        for x in min.x..=max.x {
            for y in min.y..=max.y {
                for z in min.z..=max.z {
                    self.stage_notification(LightSection { x, y, z })?;
                }
            }
        }
        Ok(())
    }
    fn prune_entries(&mut self) {
        self.entries
            .retain(|e| e.state != 0 || e.updating.is_some() || e.queued.is_some() || e.remove);
    }
    fn prune_columns(&mut self) {
        self.columns
            .retain(|c| c.enabled || c.retain || c.top.is_some());
    }
}
struct Transition {
    key: LightSection,
    state: u8,
    initialize: bool,
    layer: Option<Arc<ChargedLayer>>,
}
pub struct LayerWrite<'a> {
    entry: &'a mut Entry,
    queued_alias: bool,
}
impl Deref for LayerWrite<'_> {
    type Target = DataLayer;
    fn deref(&self) -> &DataLayer {
        &self.entry.updating.as_ref().unwrap().value
    }
}
impl LayerWrite<'_> {
    fn value_mut(&mut self) -> &mut DataLayer {
        &mut Arc::get_mut(self.entry.updating.as_mut().unwrap())
            .expect("visible copies detached")
            .value
    }
    /// The storage domain accepts only ordinary light levels, even though the
    /// standalone DataLayer value can model Java's arbitrary uniform integers.
    pub fn fill(&mut self, value: i32) -> Result<(), StorageError> {
        if !(0..=15).contains(&value) {
            return Err(StorageError::InvalidLightValue);
        }
        self.value_mut().fill(value);
        Ok(())
    }
    pub fn set(
        &mut self,
        x: u8,
        y: u8,
        z: u8,
        value: i32,
        allocation_limit: usize,
    ) -> Result<(), StorageError> {
        if !(0..=15).contains(&value) {
            return Err(StorageError::InvalidLightValue);
        }
        self.value_mut()
            .set(x, y, z, value, allocation_limit.min(LAYER_BYTES))?;
        Ok(())
    }
    pub fn materialize(&mut self, allocation_limit: usize) -> Result<&[u8], StorageError> {
        Ok(self
            .value_mut()
            .materialize(allocation_limit.min(LAYER_BYTES))?)
    }
}
impl Drop for LayerWrite<'_> {
    fn drop(&mut self) {
        if self.queued_alias {
            self.entry.queued = Some(Arc::clone(self.entry.updating.as_ref().unwrap()));
        }
    }
}
fn validate_light(layer: &DataLayer) -> Result<(), StorageError> {
    if !(0..=15).contains(&layer.get(0, 0, 0)?) {
        Err(StorageError::InvalidLightValue)
    } else {
        Ok(())
    }
}
fn nibble(layer: &DataLayer, block: LightBlock) -> u8 {
    layer
        .get(local(block.x), local(block.y), local(block.z))
        .expect("local coordinates") as u8
}
fn local(value: i32) -> u8 {
    value.rem_euclid(16) as u8
}
fn section(block: LightBlock) -> LightSection {
    LightSection {
        x: block.x.div_euclid(16),
        y: block.y.div_euclid(16),
        z: block.z.div_euclid(16),
    }
}
fn column(key: LightSection) -> ChunkAddress {
    ChunkAddress { x: key.x, z: key.z }
}
fn sort_key(key: LightSection) -> (i32, i32, i32) {
    (key.x, key.z, key.y)
}
fn snapshot_index(values: &[SnapshotLayer], key: LightSection) -> Result<usize, usize> {
    values.binary_search_by_key(&sort_key(key), |e| sort_key(e.key))
}
fn offset(key: LightSection, x: i32, y: i32, z: i32) -> Result<LightSection, StorageError> {
    Ok(LightSection {
        x: key
            .x
            .checked_add(x)
            .ok_or(StorageError::InvalidCoordinate)?,
        y: key
            .y
            .checked_add(y)
            .filter(|&y| y < i32::MAX)
            .ok_or(StorageError::InvalidCoordinate)?,
        z: key
            .z
            .checked_add(z)
            .ok_or(StorageError::InvalidCoordinate)?,
    })
}
