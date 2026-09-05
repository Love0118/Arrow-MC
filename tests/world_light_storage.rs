use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection,
        layer::{DataLayer, LAYER_BYTES},
        storage::{
            LAYER_RESERVATION_BYTES, LightSectionStorage, SectionType, StorageError, StorageLimits,
        },
    },
    preparation::ChunkAddress,
};

fn limits() -> StorageLimits {
    StorageLimits {
        max_sections: 512,
        max_columns: 128,
        max_notifications: 8192,
        metadata_bytes: 4 * 1024 * 1024,
        layer_bytes: 16 * 1024 * 1024,
    }
}
fn storage(kind: LightKind) -> LightSectionStorage {
    LightSectionStorage::new(kind, limits()).unwrap()
}
fn section(x: i32, y: i32, z: i32) -> LightSection {
    LightSection { x, y, z }
}
fn block(x: i32, y: i32, z: i32) -> LightBlock {
    LightBlock { x, y, z }
}
fn column(x: i32, z: i32) -> ChunkAddress {
    ChunkAddress { x, z }
}
fn initialize(storage: &mut LightSectionStorage) {
    storage
        .update_section_status(section(0, 0, 0), false)
        .unwrap();
    storage.process_inconsistencies().unwrap();
    storage.publish_visible().unwrap();
    storage.clear_published_notifications();
}

#[test]
fn support_covers_all_26_neighbors_and_notifications_cover_initialization_halo() {
    let mut value = storage(LightKind::Block);
    value
        .update_section_status(section(0, 0, 0), false)
        .unwrap();
    assert_eq!(value.stats().sections, 27);
    assert_eq!(value.affected_sections().len(), 125);
    assert_eq!(
        value.section_type(section(0, 0, 0)),
        SectionType::LightAndData
    );
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                if (x, y, z) != (0, 0, 0) {
                    assert_eq!(value.section_type(section(x, y, z)), SectionType::LightOnly);
                    assert_eq!(value.neighbor_count(section(x, y, z)), 1);
                }
            }
        }
    }
    assert_eq!(value.section_type(section(2, 0, 0)), SectionType::Empty);
    let before = value.stats();
    value
        .update_section_status(section(0, 0, 0), false)
        .unwrap();
    assert_eq!(value.stats(), before);
    value.process_inconsistencies().unwrap();
    value.publish_visible().unwrap();
    assert_eq!(value.published_sections().len(), 125);
    assert!(value.affected_sections().is_empty());
    value.update_section_status(section(0, 0, 0), true).unwrap();
    assert_eq!(
        value.stats().sections,
        27,
        "removal remains pending until reconciliation"
    );
    assert!(value.affected_sections().is_empty());
    value.process_inconsistencies().unwrap();
    assert_eq!(value.stats().sections, 0);
    assert_eq!(value.snapshot().sections().count(), 27);
    value.publish_visible().unwrap();
    assert_eq!(value.snapshot().sections().count(), 0);
    assert_eq!(
        value.published_sections().len(),
        125,
        "unacknowledged notifications are retained"
    );
}

#[test]
fn overlapping_support_counts_reach26_and_remove_readd_cancels_pending_removal() {
    let mut value = storage(LightKind::Block);
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                if (x, y, z) != (0, 0, 0) {
                    value
                        .update_section_status(section(x, y, z), false)
                        .unwrap();
                }
            }
        }
    }
    assert_eq!(value.neighbor_count(section(0, 0, 0)), 26);
    assert_eq!(value.section_type(section(0, 0, 0)), SectionType::LightOnly);
    value
        .update_section_status(section(0, 0, 0), false)
        .unwrap();
    assert_eq!(value.neighbor_count(section(0, 0, 0)), 26);
    let mut value = storage(LightKind::Block);
    initialize(&mut value);
    value.set_stored_level(block(2, 3, 4), 9).unwrap();
    value.publish_visible().unwrap();
    value.clear_published_notifications();
    let before = value.stats().reserved_layer_bytes;
    value.update_section_status(section(0, 0, 0), true).unwrap();
    value
        .update_section_status(section(0, 0, 0), false)
        .unwrap();
    assert!(value.affected_sections().is_empty());
    assert_eq!(value.stored_level(block(2, 3, 4)), Some(9));
    value.process_inconsistencies().unwrap();
    value.publish_visible().unwrap();
    assert_eq!(value.stats().reserved_layer_bytes, before);
    assert!(value.published_sections().is_empty());
}

#[test]
fn queue_override_cancellation_and_retention_remain_distinct_from_visible_data() {
    let mut value = storage(LightKind::Block);
    let key = section(0, 0, 0);
    value.queue_data(key, Some(&DataLayer::uniform(7))).unwrap();
    assert!(value.layer(key, true).is_none());
    assert!(value.layer(key, false).is_none());
    assert!(value.data_layer_data(key).unwrap().is_filled_with(7));
    value.update_section_status(key, false).unwrap();
    value.set_stored_level(block(0, 0, 0), 10).unwrap();
    assert_eq!(
        value.data_layer_data(key).unwrap().get(0, 0, 0),
        Ok(10),
        "storage-owned queued alias tracks writes"
    );
    value.process_inconsistencies().unwrap();
    value.publish_visible().unwrap();
    value.clear_published_notifications();
    value.queue_data(key, Some(&DataLayer::uniform(3))).unwrap();
    assert!(value.data_layer_data(key).unwrap().is_filled_with(3));
    assert_eq!(value.snapshot().get_level(block(0, 0, 0)), 10);
    value.queue_data(key, None).unwrap();
    assert_eq!(value.data_layer_data(key).unwrap().get(0, 0, 0), Ok(10));
    value.retain_data(column(0, 0), true).unwrap();
    value.update_section_status(key, true).unwrap();
    value.process_inconsistencies().unwrap();
    value.publish_visible().unwrap();
    assert!(!value.storing_light(key));
    assert_eq!(value.data_layer_data(key).unwrap().get(0, 0, 0), Ok(10));
    value.retain_data(column(0, 0), false).unwrap();
    assert!(
        value.data_layer_data(key).is_some(),
        "removing retain flag does not delete queued data"
    );
    value.update_section_status(key, false).unwrap();
    value.process_inconsistencies().unwrap();
    assert_eq!(value.stored_level(block(0, 0, 0)), Some(10));
}

#[test]
fn implicit_zero_allocated_zero_and_visible_cow_keep_their_observable_representation() {
    let mut value = storage(LightKind::Block);
    initialize(&mut value);
    let key = section(0, 0, 0);
    let old = value.snapshot();
    assert!(old.layer(key).unwrap().is_empty());
    value.set_stored_level(block(1, 1, 1), 0).unwrap();
    assert!(!value.layer(key, true).unwrap().is_empty());
    assert!(old.layer(key).unwrap().is_empty());
    value.publish_visible().unwrap();
    let allocated = value.snapshot();
    assert!(!allocated.layer(key).unwrap().is_empty());
    assert_eq!(
        allocated.layer(key).unwrap().bytes(),
        Some(&[0; LAYER_BYTES][..])
    );
    value
        .layer_to_write(key)
        .unwrap()
        .unwrap()
        .fill(15)
        .unwrap();
    assert!(value.layer(key, true).unwrap().is_filled_with(15));
    assert!(!allocated.layer(key).unwrap().is_definitely_homogeneous());
    value.publish_visible().unwrap();
    assert_eq!(old.get_level(block(1, 1, 1)), 0);
    assert_eq!(allocated.get_level(block(1, 1, 1)), 0);
    assert_eq!(value.snapshot().get_level(block(1, 1, 1)), 15);
}

#[test]
fn snapshot_lifetime_keeps_layers_charged_and_releasing_reader_allows_retry() {
    let mut cap = limits();
    cap.layer_bytes = 28 * LAYER_RESERVATION_BYTES;
    let mut value = LightSectionStorage::new(LightKind::Block, cap).unwrap();
    initialize(&mut value);
    let old = value.snapshot();
    value.set_stored_level(block(1, 1, 1), 4).unwrap();
    value.publish_visible().unwrap();
    value.clear_published_notifications();
    assert_eq!(
        value.stats().reserved_layer_bytes,
        28 * LAYER_RESERVATION_BYTES
    );
    assert_eq!(
        value.set_stored_level(block(1, 1, 1), 8),
        Err(StorageError::Budget)
    );
    assert_eq!(value.stored_level(block(1, 1, 1)), Some(4));
    assert!(value.affected_sections().is_empty());
    drop(old);
    value.set_stored_level(block(1, 1, 1), 8).unwrap();
    assert_eq!(value.stored_level(block(1, 1, 1)), Some(8));
}

#[test]
fn fanout_preflight_failure_preserves_all_layers_and_does_not_lose_reservations() {
    let mut cap = limits();
    cap.layer_bytes = 28 * LAYER_RESERVATION_BYTES;
    let mut value = LightSectionStorage::new(LightKind::Block, cap).unwrap();
    initialize(&mut value);
    let before = value.stats();
    assert_eq!(
        value.prepare_writes(&[block(0, 0, 0), block(16, 0, 0)]),
        Err(StorageError::Budget)
    );
    assert!(value.layer(section(0, 0, 0), true).unwrap().is_empty());
    assert!(value.layer(section(1, 0, 0), true).unwrap().is_empty());
    assert_eq!(
        value.stats().reserved_layer_bytes,
        before.reserved_layer_bytes
    );
    assert!(value.affected_sections().is_empty());
}

#[test]
fn support_and_notification_resource_failures_are_atomic() {
    for (section_cap, notification_cap, expected) in [
        (26, 8192, StorageError::SectionLimit),
        (512, 124, StorageError::NotificationLimit),
    ] {
        let mut cap = limits();
        cap.max_sections = section_cap;
        cap.max_notifications = notification_cap;
        let mut value = LightSectionStorage::new(LightKind::Block, cap).unwrap();
        assert_eq!(
            value.update_section_status(section(0, 0, 0), false),
            Err(expected)
        );
        assert_eq!(value.section_type(section(0, 0, 0)), SectionType::Empty);
        assert_eq!(value.stats().sections, 0);
        assert_eq!(value.stats().reserved_layer_bytes, 0);
        assert!(value.affected_sections().is_empty());
    }
    let mut cap = limits();
    cap.layer_bytes = 26 * LAYER_RESERVATION_BYTES;
    let mut value = LightSectionStorage::new(LightKind::Block, cap).unwrap();
    assert_eq!(
        value.update_section_status(section(0, 0, 0), false),
        Err(StorageError::Budget)
    );
    assert_eq!(value.stats().sections, 0);
    assert_eq!(value.stats().reserved_layer_bytes, 0);
}

#[test]
fn sky_missing_lookup_top_and_monotonic_bottom_follow_section_storage() {
    let mut value = storage(LightKind::Sky);
    let key = section(0, 3, 0);
    assert_eq!(value.get_level(block(0, 0, 0), false), 15);
    assert_eq!(value.get_level(block(0, 0, 0), true), 0);
    value.set_enabled(column(0, 0), true).unwrap();
    assert_eq!(value.get_level(block(0, 0, 0), true), 15);
    value.update_section_status(key, false).unwrap();
    assert_eq!(value.top_section_y(column(0, 0)), 5);
    assert_eq!(value.bottom_section_y(), 2);
    value
        .update_section_status(section(0, 0, 0), false)
        .unwrap();
    assert_eq!(value.top_section_y(column(0, 0)), 5);
    assert_eq!(value.bottom_section_y(), -1);
    value.process_inconsistencies().unwrap();
    value.publish_visible().unwrap();
    value.update_section_status(key, true).unwrap();
    value.process_inconsistencies().unwrap();
    assert_eq!(value.top_section_y(column(0, 0)), 2);
    value.update_section_status(section(0, 0, 0), true).unwrap();
    value.process_inconsistencies().unwrap();
    assert_eq!(value.top_section_y(column(0, 0)), -1);
    assert_eq!(value.bottom_section_y(), -1);
}

#[test]
fn sky_new_lower_layers_repeat_only_first_plane_and_missing_samples_use_plane_zero() {
    let mut value = storage(LightKind::Sky);
    let upper = section(0, 3, 0);
    let mut bytes = [0; LAYER_BYTES];
    bytes[..128].fill(0x72);
    bytes[128..256].fill(0x98);
    value.queue_bytes(upper, Some(&bytes)).unwrap();
    value.update_section_status(upper, false).unwrap();
    value.process_inconsistencies().unwrap();
    value
        .update_section_status(section(0, 0, 0), false)
        .unwrap();
    value.process_inconsistencies().unwrap();
    assert_eq!(value.stored_level(block(0, 0, 0)), Some(2));
    assert_eq!(value.stored_level(block(1, 15, 0)), Some(7));
    assert_eq!(value.get_level(block(0, -50, 0), true), 2);
    assert_eq!(value.get_level(block(1, -50, 0), true), 7);
    value.publish_visible().unwrap();
    assert_eq!(value.snapshot().get_level(block(1, -50, 0)), 7);
}

#[test]
fn writes_notify_only_boundary_neighbors_and_direct_layer_writes_do_not_notify() {
    let mut value = storage(LightKind::Block);
    initialize(&mut value);
    value
        .layer_to_write(section(0, 0, 0))
        .unwrap()
        .unwrap()
        .fill(4)
        .unwrap();
    assert!(value.affected_sections().is_empty());
    value.set_stored_level(block(0, 0, 0), 1).unwrap();
    value.set_stored_level(block(15, 15, 15), 2).unwrap();
    assert_eq!(value.affected_sections().len(), 15);
    value.publish_visible().unwrap();
    assert_eq!(value.published_sections().len(), 15);
    value.clear_published_notifications();
    assert!(value.published_sections().is_empty());
}

#[test]
fn publication_budget_failure_preserves_visible_map_and_pending_notifications() {
    let base = storage(LightKind::Block).stats().metadata_bytes;
    let mut cap = limits();
    cap.metadata_bytes = base;
    let mut value = LightSectionStorage::new(LightKind::Block, cap).unwrap();
    value
        .update_section_status(section(0, 0, 0), false)
        .unwrap();
    value.process_inconsistencies().unwrap();
    assert_eq!(value.publish_visible(), Err(StorageError::MetadataLimit));
    assert_eq!(value.snapshot().sections().count(), 0);
    assert_eq!(value.affected_sections().len(), 125);
    assert!(value.published_sections().is_empty());
}

#[test]
fn unacknowledged_notifications_apply_backpressure_without_losing_the_next_swap() {
    let mut cap = limits();
    cap.max_notifications = 125;
    let mut value = LightSectionStorage::new(LightKind::Block, cap).unwrap();
    value
        .update_section_status(section(0, 0, 0), false)
        .unwrap();
    value.process_inconsistencies().unwrap();
    value.publish_visible().unwrap();
    value
        .update_section_status(section(10, 0, 0), false)
        .unwrap();
    value.process_inconsistencies().unwrap();
    assert_eq!(
        value.publish_visible(),
        Err(StorageError::NotificationLimit)
    );
    assert_eq!(value.snapshot().sections().count(), 27);
    assert_eq!(value.affected_sections().len(), 125);
    assert_eq!(value.published_sections().len(), 125);
    value.clear_published_notifications();
    value.publish_visible().unwrap();
    assert_eq!(value.snapshot().sections().count(), 54);
    assert_eq!(value.published_sections().len(), 125);
    assert!(value.affected_sections().is_empty());
}

#[test]
fn mutable_storage_guard_rejects_non_nibble_values_and_stamp_is_unique() {
    let mut value = storage(LightKind::Block);
    let other = storage(LightKind::Block);
    assert_eq!(value.stamp(), value.stamp());
    assert_ne!(value.stamp(), other.stamp());
    let stamp = value.stamp();
    initialize(&mut value);
    assert_eq!(stamp, value.stamp());
    let key = section(0, 0, 0);
    {
        let mut writer = value.layer_to_write(key).unwrap().unwrap();
        for invalid in [16, -1, i32::MIN, i32::MAX] {
            assert_eq!(writer.fill(invalid), Err(StorageError::InvalidLightValue));
            assert_eq!(
                writer.set(0, 0, 0, invalid, LAYER_BYTES),
                Err(StorageError::InvalidLightValue)
            );
            assert!(writer.is_empty());
        }
        writer.fill(15).unwrap();
    }
    assert_eq!(value.stored_level(block(0, 0, 0)), Some(15));
    let moved = value;
    assert_eq!(stamp, moved.stamp());
}
