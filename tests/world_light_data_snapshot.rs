use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection,
        layer::{DataLayer, LAYER_BYTES},
        storage::{LightDataSnapshot, LightSectionStorage, StorageError, StorageLimits},
    },
    preparation::ChunkAddress,
};

const ORIGIN: LightSection = LightSection { x: 0, y: 0, z: 0 };
const POSITION: LightBlock = LightBlock { x: 8, y: 8, z: 8 };
fn limits() -> StorageLimits {
    StorageLimits {
        max_sections: 128,
        max_columns: 64,
        max_notifications: 1024,
        metadata_bytes: 1 << 20,
        layer_bytes: 1 << 20,
    }
}
fn storage(kind: LightKind) -> LightSectionStorage {
    LightSectionStorage::new(kind, limits()).unwrap()
}
fn initialize(storage: &mut LightSectionStorage) {
    storage.update_section_status(ORIGIN, false).unwrap();
    storage.process_inconsistencies().unwrap();
    storage.publish_visible().unwrap();
}
fn value(snapshot: &LightDataSnapshot, key: LightSection) -> i32 {
    snapshot.layer(key).unwrap().get(8, 8, 8).unwrap()
}

#[test]
fn queued_only_unsupported_layers_preserve_presence_representation_and_kind() {
    for kind in [LightKind::Block, LightKind::Sky] {
        let mut storage = storage(kind);
        let implicit = LightSection { x: -1, y: -1, z: 2 };
        let allocated = LightSection {
            x: 0,
            y: 127,
            z: -2,
        };
        let nonzero = LightSection { x: 2, y: 3, z: 0 };
        storage
            .queue_data(implicit, Some(&DataLayer::uniform(0)))
            .unwrap();
        storage
            .queue_bytes(allocated, Some(&[0; LAYER_BYTES]))
            .unwrap();
        storage
            .queue_data(nonzero, Some(&DataLayer::uniform(7)))
            .unwrap();
        let before = storage.stats();
        let data = storage.data_snapshot().unwrap();
        assert_eq!(data.kind(), kind);
        assert_eq!(
            data.sections().collect::<Vec<_>>(),
            [implicit, allocated, nonzero]
        );
        assert_eq!(storage.snapshot().sections().count(), 0);
        assert_eq!(
            storage.stats().reserved_layer_bytes,
            before.reserved_layer_bytes,
            "capture must share payloads"
        );
        for key in [implicit, allocated, nonzero] {
            assert!(!storage.storing_light(key));
        }
        assert!(data.layer(implicit).unwrap().is_empty());
        assert!(data.layer(implicit).unwrap().is_definitely_homogeneous());
        assert!(!data.layer(allocated).unwrap().is_empty());
        assert!(!data.layer(allocated).unwrap().is_definitely_homogeneous());
        assert_eq!(value(&data, allocated), 0);
        assert!(data.layer(nonzero).unwrap().is_definitely_homogeneous());
        assert_eq!(value(&data, nonzero), 7);
        storage.process_inconsistencies().unwrap();
        storage
            .retain_data(ChunkAddress { x: 2, z: 0 }, false)
            .unwrap();
        assert_eq!(storage.stats().queued, 3);
        assert!(!storage.has_inconsistencies());
        assert_eq!(storage.snapshot().sections().count(), 0);
        assert_eq!(
            storage
                .data_snapshot()
                .unwrap()
                .sections()
                .collect::<Vec<_>>(),
            [implicit, allocated, nonzero]
        );
    }
}

#[test]
fn queued_override_and_clear_use_visible_fallback_instead_of_unpublished_updating_data() {
    let mut storage = storage(LightKind::Block);
    initialize(&mut storage);
    storage.set_stored_level(POSITION, 7).unwrap();
    storage.publish_visible().unwrap();
    let published = storage.data_snapshot().unwrap();
    storage.set_stored_level(POSITION, 9).unwrap();
    assert_eq!(storage.stored_level(POSITION), Some(9));
    assert_eq!(value(&storage.data_snapshot().unwrap(), ORIGIN), 7);
    storage
        .queue_data(ORIGIN, Some(&DataLayer::uniform(3)))
        .unwrap();
    let queued = storage.data_snapshot().unwrap();
    assert_eq!(value(&queued, ORIGIN), 3);
    assert_eq!(storage.snapshot().get_level(POSITION), 7);
    storage.queue_data(ORIGIN, None).unwrap();
    assert_eq!(value(&storage.data_snapshot().unwrap(), ORIGIN), 7);
    storage.publish_visible().unwrap();
    assert_eq!(value(&storage.data_snapshot().unwrap(), ORIGIN), 9);
    assert_eq!(value(&published, ORIGIN), 7);
    assert_eq!(value(&queued, ORIGIN), 3);
}

#[test]
fn merge_retains_visible_rows_after_updating_entries_are_removed_before_publish() {
    let mut storage = storage(LightKind::Block);
    initialize(&mut storage);
    let queued_only = LightSection { x: 5, y: 1, z: 0 };
    storage
        .queue_data(queued_only, Some(&DataLayer::uniform(5)))
        .unwrap();
    storage.update_section_status(ORIGIN, true).unwrap();
    storage.process_inconsistencies().unwrap();
    assert_eq!(storage.stats().sections, 0);
    assert_eq!(storage.snapshot().sections().count(), 27);
    let old = storage.data_snapshot().unwrap();
    assert_eq!(old.sections().count(), 28);
    assert_eq!(value(&old, queued_only), 5);
    storage.publish_visible().unwrap();
    let next = storage.data_snapshot().unwrap();
    assert_eq!(next.sections().collect::<Vec<_>>(), [queued_only]);
    assert_eq!(old.sections().count(), 28);
}

#[test]
fn old_capture_keeps_payloads_and_charges_through_queue_replacement_and_cow_publication() {
    let mut storage = storage(LightKind::Block);
    storage
        .queue_data(ORIGIN, Some(&DataLayer::uniform(6)))
        .unwrap();
    let queued = storage.data_snapshot().unwrap();
    let charge = queued.retained_bytes().unwrap();
    initialize(&mut storage);
    assert_eq!(storage.snapshot().get_level(POSITION), 6);
    let before = storage.stats().reserved_layer_bytes;
    storage.set_stored_level(POSITION, 4).unwrap();
    storage.publish_visible().unwrap();
    assert!(storage.stats().reserved_layer_bytes > before);
    assert_eq!(value(&queued, ORIGIN), 6);
    assert!(queued.layer(ORIGIN).unwrap().is_definitely_homogeneous());
    assert_eq!(queued.retained_bytes().unwrap(), charge);
    let current = storage.data_snapshot().unwrap();
    assert_eq!(value(&current, ORIGIN), 4);
    storage
        .queue_bytes(ORIGIN, Some(&[0; LAYER_BYTES]))
        .unwrap();
    let replaced = storage.data_snapshot().unwrap();
    assert_eq!(value(&replaced, ORIGIN), 0);
    assert!(!replaced.layer(ORIGIN).unwrap().is_definitely_homogeneous());
    storage.queue_data(ORIGIN, None).unwrap();
    assert_eq!(value(&storage.data_snapshot().unwrap(), ORIGIN), 4);
    drop(storage);
    assert_eq!(value(&queued, ORIGIN), 6);
    assert_eq!(value(&current, ORIGIN), 4);
    assert_eq!(value(&replaced, ORIGIN), 0);
    assert_eq!(queued.retained_bytes().unwrap(), charge);
}

#[test]
fn capture_failure_at_body_or_vector_admission_rolls_back_reservations_and_preserves_data() {
    fn make(limits: StorageLimits) -> LightSectionStorage {
        let mut storage = LightSectionStorage::new(LightKind::Block, limits).unwrap();
        for x in 0..3 {
            storage
                .queue_data(
                    LightSection { x, y: 0, z: 0 },
                    Some(&DataLayer::uniform(x + 1)),
                )
                .unwrap();
        }
        storage
    }
    let baseline = make(limits());
    let base_bytes = baseline.stats().metadata_bytes;
    let captured = baseline.data_snapshot().unwrap();
    let capture_bytes = baseline.stats().metadata_bytes - base_bytes;
    drop(captured);
    for remaining in [0, capture_bytes - 1] {
        let mut limits = limits();
        limits.metadata_bytes = base_bytes + remaining;
        let storage = make(limits);
        let before = storage.stats();
        for _ in 0..3 {
            assert!(matches!(
                storage.data_snapshot(),
                Err(StorageError::MetadataLimit)
            ));
            assert_eq!(storage.stats(), before);
            assert_eq!(storage.snapshot().sections().count(), 0);
            for x in 0..3 {
                assert_eq!(
                    storage
                        .data_layer_data(LightSection { x, y: 0, z: 0 })
                        .unwrap()
                        .get(0, 0, 0)
                        .unwrap(),
                    x + 1
                );
            }
        }
    }
    let mut exact = limits();
    exact.metadata_bytes = base_bytes + capture_bytes;
    let exact = make(exact);
    let first = exact.data_snapshot().unwrap();
    assert_eq!(first.sections().count(), 3);
    assert!(matches!(
        exact.data_snapshot(),
        Err(StorageError::MetadataLimit)
    ));
    drop(first);
    assert_eq!(exact.stats().metadata_bytes, base_bytes);
    assert_eq!(exact.data_snapshot().unwrap().sections().count(), 3);
}

#[test]
fn cloned_snapshot_shares_metadata_and_refunds_only_after_last_clone_is_dropped() {
    let storage = storage(LightKind::Sky);
    let baseline = storage.stats();
    let snapshot = storage.data_snapshot().unwrap();
    let after = storage.stats();
    assert!(after.metadata_bytes > baseline.metadata_bytes);
    assert_eq!(snapshot.sections().count(), 0);
    assert!(snapshot.retained_bytes().unwrap() > size_of_val(&snapshot));
    let clone = snapshot.clone();
    assert_eq!(storage.stats(), after);
    drop(snapshot);
    assert_eq!(storage.stats(), after);
    drop(clone);
    assert_eq!(storage.stats(), baseline);
}

#[test]
fn complete_key_union_has_queued_precedence_without_duplicate_equal_keys() {
    let mut storage = storage(LightKind::Block);
    initialize(&mut storage);
    let visible = storage.snapshot();
    let original: Vec<_> = visible.sections().collect();
    for (index, &key) in original.iter().enumerate() {
        if index % 2 == 0 {
            storage
                .queue_data(key, Some(&DataLayer::uniform(11)))
                .unwrap();
        }
    }
    let negative = LightSection { x: -3, y: 2, z: 0 };
    let positive = LightSection { x: 4, y: -2, z: 1 };
    for key in [positive, negative] {
        storage
            .queue_data(key, Some(&DataLayer::uniform(8)))
            .unwrap();
    }
    let snapshot = storage.data_snapshot().unwrap();
    let keys: Vec<_> = snapshot.sections().collect();
    assert_eq!(keys.len(), 29);
    assert!(
        keys.windows(2)
            .all(|pair| (pair[0].x, pair[0].z, pair[0].y) < (pair[1].x, pair[1].z, pair[1].y))
    );
    for (index, &key) in original.iter().enumerate() {
        assert_eq!(value(&snapshot, key), if index % 2 == 0 { 11 } else { 0 });
        assert_eq!(visible.layer(key).unwrap().get(8, 8, 8).unwrap(), 0);
    }
    assert_eq!(value(&snapshot, negative), 8);
    assert_eq!(value(&snapshot, positive), 8);
}
