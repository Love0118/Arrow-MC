use arrow_mc::server::chunk_packet::LightUpdate;
use arrow_mc::server::light_snapshot::{ChangedFilters, Error, PacketLightSnapshot};
use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection,
        layer::{DataLayer, LAYER_BYTES},
        storage::{LAYER_RESERVATION_BYTES, LightSectionStorage, StorageError, StorageLimits},
    },
    preparation::ChunkAddress,
    storage::chunk::DimensionHeight,
};

fn limits() -> StorageLimits {
    StorageLimits {
        max_sections: 128,
        max_columns: 32,
        max_notifications: 1024,
        metadata_bytes: 2 * 1024 * 1024,
        layer_bytes: 2 * 1024 * 1024,
    }
}

fn storage(kind: LightKind) -> LightSectionStorage {
    LightSectionStorage::new(kind, limits()).unwrap()
}

fn position() -> ChunkAddress {
    ChunkAddress { x: 0, z: 0 }
}

fn section(y: i32) -> LightSection {
    LightSection { x: 0, y, z: 0 }
}

fn height() -> DimensionHeight {
    DimensionHeight::new(0, 32).unwrap()
}

fn publish(storage: &mut LightSectionStorage) {
    storage.process_inconsistencies().unwrap();
    storage.publish_visible().unwrap();
}

#[test]
fn unsupported_queued_allocated_zero_is_packet_data_without_visible_publication() {
    let mut storage = storage(LightKind::Block);
    storage
        .queue_bytes(section(0), Some(&[0; LAYER_BYTES]))
        .unwrap();
    assert!(!storage.storing_light(section(0)));
    assert!(storage.layer(section(0), true).is_none());
    assert!(storage.layer(section(0), false).is_none());
    let before = storage.stats();
    let captured = storage.data_snapshot().unwrap();
    assert_eq!(captured.kind(), LightKind::Block);
    assert_eq!(captured.sections().collect::<Vec<_>>(), [section(0)]);
    assert_eq!(storage.stats().sections, 0);
    assert_eq!(storage.stats().queued, 1);
    assert_eq!(
        storage.stats().reserved_layer_bytes,
        before.reserved_layer_bytes
    );
    assert_eq!(storage.stats().peak_layer_bytes, before.peak_layer_bytes);
    assert!(storage.stats().metadata_bytes > before.metadata_bytes);

    let visible = storage.snapshot();
    let visible_packet = PacketLightSnapshot::new(
        position(),
        height(),
        Some(&visible),
        None,
        ChangedFilters::default(),
        0,
    )
    .unwrap();
    assert_eq!(visible_packet.light_data().block_mask, &[0]);
    assert_eq!(visible_packet.light_data().empty_block_mask, &[0]);

    let before_bridge = storage.stats();
    let packet = PacketLightSnapshot::from_data(
        position(),
        height(),
        Some(&captured),
        None,
        ChangedFilters::default(),
        size_of::<LightUpdate<'_>>(),
    )
    .unwrap();
    let light = packet.light_data();
    assert_eq!(light.block_mask, &[0b0010]);
    assert_eq!(light.empty_block_mask, &[0]);
    assert_eq!(light.sky_mask, &[0]);
    assert!(light.sky_updates.is_empty());
    let [LightUpdate::Bytes(bytes)] = light.block_updates else {
        panic!("queued allocated zero must remain one data update");
    };
    assert_eq!(*bytes, &[0; LAYER_BYTES]);
    assert!(std::ptr::eq(
        bytes.as_ptr(),
        captured
            .layer(section(0))
            .unwrap()
            .bytes()
            .unwrap()
            .as_ptr(),
    ));
    assert!(std::ptr::eq(
        bytes.as_ptr(),
        storage
            .data_layer_data(section(0))
            .unwrap()
            .bytes()
            .unwrap()
            .as_ptr(),
    ));
    assert_eq!(storage.stats(), before_bridge);
    assert_eq!(packet.heap_bytes(), size_of::<LightUpdate<'_>>());
}

#[test]
fn queued_override_and_cancellation_reveal_visible_not_unpublished_updating_data() {
    let mut storage = storage(LightKind::Block);
    storage.update_section_status(section(0), false).unwrap();
    storage
        .queue_data(section(0), Some(&DataLayer::uniform(3)))
        .unwrap();
    publish(&mut storage);
    let visible = storage.snapshot();
    storage
        .queue_data(section(0), Some(&DataLayer::uniform(12)))
        .unwrap();
    storage
        .queue_data(section(2), Some(&DataLayer::uniform(9)))
        .unwrap();
    let captured = storage.data_snapshot().unwrap();
    assert_eq!(
        captured.sections().filter(|key| *key == section(0)).count(),
        1
    );
    assert!(captured.layer(section(0)).unwrap().is_filled_with(12));
    assert!(captured.layer(section(2)).unwrap().is_filled_with(9));
    assert!(visible.layer(section(0)).unwrap().is_filled_with(3));
    assert!(visible.layer(section(2)).is_none());
    let packet = PacketLightSnapshot::from_data(
        position(),
        height(),
        Some(&captured),
        None,
        ChangedFilters::default(),
        1024,
    )
    .unwrap();
    assert_eq!(packet.light_data().block_mask, &[0b1010]);
    assert_eq!(packet.light_data().empty_block_mask, &[0b0101]);
    assert_eq!(
        packet.light_data().block_updates,
        &[LightUpdate::Uniform(12), LightUpdate::Uniform(9)]
    );

    storage.queue_bytes(section(0), None).unwrap();
    storage.queue_bytes(section(2), None).unwrap();
    let fallback = storage.data_snapshot().unwrap();
    assert!(fallback.layer(section(0)).unwrap().is_filled_with(3));
    assert!(fallback.layer(section(2)).is_none());
    storage
        .set_stored_level(LightBlock { x: 0, y: 0, z: 0 }, 7)
        .unwrap();
    assert_eq!(storage.layer(section(0), true).unwrap().get(0, 0, 0), Ok(7));
    let unpublished = storage.data_snapshot().unwrap();
    assert!(unpublished.layer(section(0)).unwrap().is_filled_with(3));
    storage.publish_visible().unwrap();
    let published = storage.data_snapshot().unwrap();
    assert_eq!(published.layer(section(0)).unwrap().get(0, 0, 0), Ok(7));
    assert_eq!(
        packet.light_data().block_updates,
        &[LightUpdate::Uniform(12), LightUpdate::Uniform(9)]
    );
    assert!(captured.layer(section(2)).unwrap().is_filled_with(9));
}

#[test]
fn held_capture_shares_payload_and_retains_layer_and_metadata_leases_until_last_drop() {
    let mut storage = storage(LightKind::Block);
    storage
        .queue_bytes(section(0), Some(&[0x12; LAYER_BYTES]))
        .unwrap();
    let baseline = storage.stats();
    assert_eq!(baseline.reserved_layer_bytes, LAYER_RESERVATION_BYTES);
    let captured = storage.data_snapshot().unwrap();
    let snapshot_stats = storage.stats();
    let retained = captured.retained_bytes().unwrap();
    assert!(retained > LAYER_RESERVATION_BYTES);
    let clone = captured.clone();
    assert_eq!(storage.stats(), snapshot_stats);
    assert_eq!(clone.retained_bytes().unwrap(), retained);
    assert_eq!(storage.stats(), snapshot_stats);
    let packet = PacketLightSnapshot::from_data(
        position(),
        height(),
        Some(&captured),
        None,
        ChangedFilters::default(),
        1024,
    )
    .unwrap();
    let [LightUpdate::Bytes(old)] = packet.light_data().block_updates else {
        panic!("captured allocated layer must be borrowed");
    };
    let old_pointer = old.as_ptr();
    storage
        .queue_bytes(section(0), Some(&[0x34; LAYER_BYTES]))
        .unwrap();
    assert_eq!(
        storage.stats().reserved_layer_bytes,
        2 * LAYER_RESERVATION_BYTES
    );
    assert_ne!(
        storage
            .data_layer_data(section(0))
            .unwrap()
            .bytes()
            .unwrap()
            .as_ptr(),
        old_pointer
    );
    assert_eq!(*old, &[0x12; LAYER_BYTES]);
    assert_eq!(captured.retained_bytes().unwrap(), retained);
    storage.queue_bytes(section(0), None).unwrap();
    assert_eq!(storage.stats().queued, 0);
    assert_eq!(
        storage.stats().reserved_layer_bytes,
        LAYER_RESERVATION_BYTES
    );
    assert_eq!(packet.light_data().block_mask, &[0b0010]);
    assert!(std::ptr::eq(
        clone.layer(section(0)).unwrap().bytes().unwrap().as_ptr(),
        old_pointer
    ));
    drop(packet);
    drop(captured);
    assert_eq!(
        storage.stats().reserved_layer_bytes,
        LAYER_RESERVATION_BYTES
    );
    assert_eq!(
        storage.stats().metadata_bytes,
        snapshot_stats.metadata_bytes
    );
    drop(clone);
    assert_eq!(storage.stats().reserved_layer_bytes, 0);
    assert_eq!(storage.stats().metadata_bytes, baseline.metadata_bytes);
}

#[test]
fn queued_uniform_zero_and_nonzero_have_distinct_masks_without_materialization() {
    let mut storage = storage(LightKind::Block);
    for (y, value) in [(-1, 0), (0, 15), (2, 7)] {
        storage
            .queue_data(section(y), Some(&DataLayer::uniform(value)))
            .unwrap();
    }
    let captured = storage.data_snapshot().unwrap();
    let before = storage.stats();
    let packet = PacketLightSnapshot::from_data(
        position(),
        height(),
        Some(&captured),
        None,
        ChangedFilters::default(),
        2 * size_of::<LightUpdate<'_>>(),
    )
    .unwrap();
    assert_eq!(packet.light_data().empty_block_mask, &[0b0001]);
    assert_eq!(packet.light_data().block_mask, &[0b1010]);
    assert_eq!(
        packet.light_data().block_updates,
        &[LightUpdate::Uniform(15), LightUpdate::Uniform(7)]
    );
    for key in captured.sections() {
        assert!(captured.layer(key).unwrap().bytes().is_none());
        assert_eq!(captured.layer(key).unwrap().heap_bytes(), 0);
    }
    assert_eq!(captured.layer(section(0)).unwrap().get(0, 0, 0), Ok(15));
    assert_eq!(storage.stats(), before);
}

#[test]
fn queued_border_layers_obey_per_kind_filters_and_maximum_dimension_range() {
    let mut block = storage(LightKind::Block);
    let mut sky = storage(LightKind::Sky);
    for (y, value) in [
        (-130, 9),
        (-129, 1),
        (-128, 0),
        (0, 2),
        (127, 3),
        (128, 4),
        (129, 10),
    ] {
        block
            .queue_data(section(y), Some(&DataLayer::uniform(value)))
            .unwrap();
        sky.queue_data(section(y), Some(&DataLayer::uniform(value)))
            .unwrap();
    }
    block
        .queue_data(
            LightSection {
                x: 1,
                y: -129,
                z: 0,
            },
            Some(&DataLayer::uniform(15)),
        )
        .unwrap();
    let block_snapshot = block.data_snapshot().unwrap();
    let sky_snapshot = sky.data_snapshot().unwrap();
    let dimension = DimensionHeight::new(-2048, 4096).unwrap();
    let mut filter = [0; 34];
    filter[0] = 1;
    filter[16] = 2;
    filter[32] = 0xfe;
    filter[33] = 0xff;
    let packet = PacketLightSnapshot::from_data(
        position(),
        dimension,
        Some(&block_snapshot),
        Some(&sky_snapshot),
        ChangedFilters {
            block: Some(&filter),
            sky: Some(&[]),
        },
        1024,
    )
    .unwrap();
    assert_eq!(packet.min_light_section(), -129);
    assert_eq!(packet.light_section_count(), 258);
    let mut expected = [0; 33];
    expected[0] = 1;
    expected[16] = 2;
    expected[32] = 2;
    assert_eq!(packet.light_data().block_mask, &expected);
    assert_eq!(packet.light_data().empty_block_mask, &[0; 33]);
    assert_eq!(
        packet.light_data().block_updates,
        &[
            LightUpdate::Uniform(1),
            LightUpdate::Uniform(2),
            LightUpdate::Uniform(4)
        ]
    );
    assert_eq!(packet.light_data().sky_mask, &[0; 33]);
    assert_eq!(packet.light_data().empty_sky_mask, &[0; 33]);
    let sky_empty = PacketLightSnapshot::from_data(
        position(),
        dimension,
        Some(&block_snapshot),
        Some(&sky_snapshot),
        ChangedFilters {
            block: Some(&[]),
            sky: Some(&[2]),
        },
        0,
    )
    .unwrap();
    let mut empty = [0; 33];
    empty[0] = 2;
    assert_eq!(sky_empty.light_data().empty_sky_mask, &empty);
    assert!(sky_empty.light_data().sky_updates.is_empty());
    assert_eq!(sky_empty.heap_bytes(), 0);
}

#[test]
fn snapshot_and_packet_budgets_fail_without_losing_queued_or_visible_state() {
    let mut storage = storage(LightKind::Block);
    storage
        .queue_data(section(0), Some(&DataLayer::uniform(7)))
        .unwrap();
    let baseline = storage.stats();
    let captured = storage.data_snapshot().unwrap();
    let capture_bytes = storage.stats().metadata_bytes - baseline.metadata_bytes;
    let before = storage.stats();
    let required = size_of::<LightUpdate<'_>>();
    assert!(matches!(
        PacketLightSnapshot::from_data(
            position(),
            height(),
            Some(&captured),
            None,
            ChangedFilters::default(),
            required - 1,
        ),
        Err(Error::AllocationLimit)
    ));
    assert_eq!(storage.stats(), before);
    assert_eq!(
        PacketLightSnapshot::from_data(
            position(),
            height(),
            Some(&captured),
            None,
            ChangedFilters::default(),
            required,
        )
        .unwrap()
        .heap_bytes(),
        required
    );
    assert!(matches!(
        PacketLightSnapshot::from_data(
            position(),
            height(),
            None,
            Some(&captured),
            ChangedFilters::default(),
            0,
        ),
        Err(Error::WrongLayer {
            expected: LightKind::Sky,
            actual: LightKind::Block
        })
    ));
    let disabled = PacketLightSnapshot::from_data(
        position(),
        height(),
        None,
        None,
        ChangedFilters::default(),
        0,
    )
    .unwrap();
    assert_eq!(disabled.heap_bytes(), 0);
    assert_eq!(disabled.light_data().empty_block_mask, &[0]);
    assert_eq!(disabled.light_data().empty_sky_mask, &[0]);

    for shortage in [1, 0] {
        let mut cap = limits();
        cap.metadata_bytes = baseline.metadata_bytes + capture_bytes - shortage;
        let mut tight = LightSectionStorage::new(LightKind::Block, cap).unwrap();
        tight
            .queue_data(section(0), Some(&DataLayer::uniform(7)))
            .unwrap();
        let before_capture = tight.stats();
        let result = tight.data_snapshot();
        if shortage == 1 {
            assert!(matches!(result, Err(StorageError::MetadataLimit)));
            assert_eq!(tight.stats(), before_capture);
            assert!(tight.data_layer_data(section(0)).unwrap().is_filled_with(7));
            assert!(tight.snapshot().layer(section(0)).is_none());
        } else {
            let exact = result.unwrap();
            assert!(exact.layer(section(0)).unwrap().is_filled_with(7));
            assert_eq!(tight.stats().metadata_bytes, cap.metadata_bytes);
            drop(exact);
            assert_eq!(tight.stats(), before_capture);
        }
    }
}
