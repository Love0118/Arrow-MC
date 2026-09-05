#[path = "common/world_registry_fixture.rs"]
mod fixture;

use arrow_mc::server::chunk_packet::{self, LightUpdate};
use arrow_mc::server::light_snapshot::{ChangedFilters, Error, PacketLightSnapshot};
use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection,
        layer::DataLayer,
        storage::{LightSectionStorage, StorageLimits},
    },
    preparation::ChunkAddress,
    storage::chunk::DimensionHeight,
};

fn position() -> ChunkAddress {
    ChunkAddress { x: 0, z: 0 }
}
fn section(y: i32) -> LightSection {
    LightSection { x: 0, y, z: 0 }
}
fn height() -> DimensionHeight {
    DimensionHeight::new(-16, 64).unwrap()
}
fn storage(kind: LightKind) -> LightSectionStorage {
    LightSectionStorage::new(
        kind,
        StorageLimits {
            max_sections: 128,
            max_columns: 32,
            max_notifications: 1024,
            metadata_bytes: 2 * 1024 * 1024,
            layer_bytes: 2 * 1024 * 1024,
        },
    )
    .unwrap()
}
fn publish(storage: &mut LightSectionStorage) {
    storage.process_inconsistencies().unwrap();
    storage.publish_visible().unwrap();
}
fn mixed() -> LightSectionStorage {
    let mut storage = storage(LightKind::Block);
    storage.update_section_status(section(0), false).unwrap();
    storage
        .queue_data(
            section(0),
            Some(&DataLayer::from_bytes(&[0; 2048], 2048).unwrap()),
        )
        .unwrap();
    storage
        .queue_data(section(1), Some(&DataLayer::uniform(7)))
        .unwrap();
    publish(&mut storage);
    storage
}

#[test]
fn snapshot_representation_preserves_missing_empty_allocated_zero_and_uniform_data() {
    let storage = mixed();
    let snapshot = storage.snapshot();
    let before = storage.stats();
    let bridge = PacketLightSnapshot::new(
        position(),
        height(),
        Some(&snapshot),
        None,
        ChangedFilters::default(),
        1024,
    )
    .unwrap();
    assert_eq!(bridge.min_light_section(), -2);
    assert_eq!(bridge.light_section_count(), 6);
    assert_eq!(bridge.position(), position());
    let light = bridge.light_data();
    assert_eq!(light.block_mask, &[0b001100]); // y0 allocated zero, y1 uniform7.
    assert_eq!(light.empty_block_mask, &[0b000010]); // y-1 implicit zero.
    assert_eq!(light.sky_mask, &[0]);
    assert_eq!(light.empty_sky_mask, &[0]);
    assert!(light.sky_updates.is_empty());
    assert_eq!(light.block_updates.len(), 2);
    let LightUpdate::Bytes(bytes) = light.block_updates[0] else {
        panic!("allocated zero was elided")
    };
    assert_eq!(bytes.len(), 2048);
    assert!(bytes.iter().all(|byte| *byte == 0));
    assert!(std::ptr::eq(
        bytes.as_ptr(),
        snapshot
            .layer(section(0))
            .unwrap()
            .bytes()
            .unwrap()
            .as_ptr()
    ));
    assert_eq!(light.block_updates[1], LightUpdate::Uniform(7));
    assert_eq!(
        storage.stats(),
        before,
        "bridge must not copy or materialize layers"
    );
    assert_eq!(bridge.heap_bytes(), 2 * size_of::<LightUpdate<'_>>());
    assert_eq!(bridge.chunk_packet(&[], &[], &[]).position, position());
}

#[test]
fn filters_are_per_kind_relative_to_light_min_and_ignore_out_of_range_bits() {
    let block = mixed();
    let block_snapshot = block.snapshot();
    let mut sky = storage(LightKind::Sky);
    sky.update_section_status(section(0), false).unwrap();
    sky.queue_data(section(0), Some(&DataLayer::uniform(15)))
        .unwrap();
    publish(&mut sky);
    let sky_snapshot = sky.snapshot();
    let bridge = PacketLightSnapshot::new(
        position(),
        height(),
        Some(&block_snapshot),
        Some(&sky_snapshot),
        ChangedFilters {
            block: Some(&[0b10001010, 255]),
            sky: Some(&[]),
        },
        1024,
    )
    .unwrap();
    let light = bridge.light_data();
    assert_eq!(light.block_mask, &[0b001000]);
    assert_eq!(light.empty_block_mask, &[0b000010]);
    assert_eq!(light.block_updates, &[LightUpdate::Uniform(7)]);
    assert_eq!(light.sky_mask, &[0]);
    assert_eq!(light.empty_sky_mask, &[0]);
    let bridge = PacketLightSnapshot::new(
        position(),
        height(),
        Some(&block_snapshot),
        Some(&sky_snapshot),
        ChangedFilters {
            block: Some(&[]),
            sky: Some(&[1 << 2]),
        },
        1024,
    )
    .unwrap();
    assert_eq!(bridge.light_data().sky_mask, &[1 << 2]);
    assert_eq!(bridge.light_data().sky_updates, &[LightUpdate::Uniform(15)]);
    assert!(bridge.light_data().block_updates.is_empty());
}

#[test]
fn exact_metadata_budget_wrong_kind_and_disabled_engines_fail_without_mutation() {
    let storage = mixed();
    let snapshot = storage.snapshot();
    let required = 2 * size_of::<LightUpdate<'_>>();
    let before = storage.stats();
    assert!(matches!(
        PacketLightSnapshot::new(
            position(),
            height(),
            Some(&snapshot),
            None,
            ChangedFilters::default(),
            required - 1
        ),
        Err(Error::AllocationLimit)
    ));
    assert_eq!(storage.stats(), before);
    assert_eq!(
        PacketLightSnapshot::new(
            position(),
            height(),
            Some(&snapshot),
            None,
            ChangedFilters::default(),
            required
        )
        .unwrap()
        .heap_bytes(),
        required
    );
    assert!(matches!(
        PacketLightSnapshot::new(
            position(),
            height(),
            None,
            Some(&snapshot),
            ChangedFilters::default(),
            1024
        ),
        Err(Error::WrongLayer {
            expected: LightKind::Sky,
            actual: LightKind::Block
        })
    ));
    let empty = PacketLightSnapshot::new(
        position(),
        height(),
        None,
        None,
        ChangedFilters::default(),
        0,
    )
    .unwrap();
    assert_eq!(empty.heap_bytes(), 0);
    assert!(empty.light_data().block_updates.is_empty());
    let filtered = PacketLightSnapshot::new(
        position(),
        height(),
        Some(&snapshot),
        None,
        ChangedFilters {
            block: Some(&[]),
            sky: None,
        },
        0,
    )
    .unwrap();
    assert_eq!(filtered.heap_bytes(), 0);
}

#[test]
fn full_dimension_range_includes_both_border_layers_and_bit257() {
    let height = DimensionHeight::new(-2048, 4096).unwrap();
    let mut storage = storage(LightKind::Block);
    storage.update_section_status(section(-128), false).unwrap();
    storage.update_section_status(section(127), false).unwrap();
    storage
        .queue_data(section(-129), Some(&DataLayer::uniform(1)))
        .unwrap();
    storage
        .queue_data(section(128), Some(&DataLayer::uniform(15)))
        .unwrap();
    publish(&mut storage);
    let snapshot = storage.snapshot();
    let bridge = PacketLightSnapshot::new(
        position(),
        height,
        Some(&snapshot),
        None,
        ChangedFilters::default(),
        1024,
    )
    .unwrap();
    assert_eq!(bridge.min_light_section(), -129);
    assert_eq!(bridge.light_section_count(), 258);
    let light = bridge.light_data();
    assert_eq!(light.block_mask.len(), 33);
    assert_eq!(light.block_mask[0], 1);
    assert_eq!(light.block_mask[32], 2);
    assert!(light.block_mask[1..32].iter().all(|byte| *byte == 0));
    assert_eq!(light.empty_block_mask[0], 6);
    assert_eq!(light.empty_block_mask[31], 128);
    assert_eq!(light.empty_block_mask[32], 1);
    assert_eq!(
        light.block_updates,
        &[LightUpdate::Uniform(1), LightUpdate::Uniform(15)]
    );
}

#[test]
fn old_snapshot_payload_and_budget_stay_alive_during_visible_replacement() {
    let mut storage = mixed();
    let snapshot = storage.snapshot();
    let before = storage.stats().reserved_layer_bytes;
    let bridge = PacketLightSnapshot::new(
        position(),
        height(),
        Some(&snapshot),
        None,
        ChangedFilters::default(),
        1024,
    )
    .unwrap();
    let old_packet = chunk_packet::encode(
        &bridge.chunk_packet(&[], &[], &[]),
        0,
        chunk_packet::Limits::default(),
    )
    .unwrap();
    storage
        .set_stored_level(LightBlock { x: 0, y: 0, z: 0 }, 11)
        .unwrap();
    storage.publish_visible().unwrap();
    assert!(storage.stats().reserved_layer_bytes > before);
    assert_eq!(snapshot.get_level(LightBlock { x: 0, y: 0, z: 0 }), 0);
    assert_eq!(
        storage
            .snapshot()
            .get_level(LightBlock { x: 0, y: 0, z: 0 }),
        11
    );
    assert_eq!(
        chunk_packet::encode(
            &bridge.chunk_packet(&[], &[], &[]),
            0,
            chunk_packet::Limits::default()
        )
        .unwrap(),
        old_packet
    );
    drop(bridge);
    drop(snapshot);
    assert_eq!(storage.stats().reserved_layer_bytes, before);
}

#[test]
fn unpublished_queued_override_is_not_a_completed_visible_snapshot() {
    let mut storage = mixed();
    storage
        .queue_data(section(1), Some(&DataLayer::uniform(12)))
        .unwrap();
    assert!(
        storage
            .data_layer_data(section(1))
            .unwrap()
            .is_filled_with(12)
    );
    let snapshot = storage.snapshot();
    let bridge = PacketLightSnapshot::new(
        position(),
        height(),
        Some(&snapshot),
        None,
        ChangedFilters::default(),
        1024,
    )
    .unwrap();
    assert_eq!(
        bridge.light_data().block_updates[1],
        LightUpdate::Uniform(7)
    );
    publish(&mut storage);
    let snapshot = storage.snapshot();
    let bridge = PacketLightSnapshot::new(
        position(),
        height(),
        Some(&snapshot),
        None,
        ChangedFilters::default(),
        1024,
    )
    .unwrap();
    assert_eq!(
        bridge.light_data().block_updates[1],
        LightUpdate::Uniform(12)
    );
}

#[test]
fn converged_block_light_reaches_full_chunk_packet_queue_and_real_tcp() {
    use arrow_mc::{
        runtime::{CpuPool, CpuPoolConfig},
        server::{
            chunk_sender::{
                ChunkDeliveryQueue, ChunkSender, DeliveryLimits, SendReadyChunk, SenderLimits,
            },
            transport::{ConnectionTransport, TransportLimits},
        },
        world::{
            lighting::{
                LightingChunk, LightingSource, SourceLimits,
                block::{BlockLightEngine, BlockLightLimits},
            },
            section::{ContainerKind, PalettedContainer, Section, SectionCounts},
        },
    };
    use serde_json::json;
    use std::{sync::Arc, time::Duration};
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
        time::timeout,
    };
    let mut fixture = fixture::Fixture::from_data(
        json!({
            "state_count":3,"state_flags":[1,0,0],"blocks":[
                {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
                {"id":"minecraft:bedrock","default_state":1,"properties":[],"states":[1]},
                {"id":"test:emitter","default_state":2,"properties":[],"states":[2]}
            ]
        }),
        json!([{"id":"minecraft:plains","protocol_id":0}]),
    );
    let mut materials = [[0u8; 16]; 3];
    materials[1][1] = 15;
    materials[2][0] = 15;
    materials[2][1] = 15;
    fixture.edit_lighting(|bytes| *bytes = fixture::lighting_bytes(&materials, 2, &[14]));
    let registry = Arc::new(fixture.load());
    let mut blocks = [0u32; 4096];
    blocks[8 * 256 + 8 * 16 + 8] = 2;
    let section_data = Section {
        counts: SectionCounts {
            non_empty_blocks: 1,
            fluid_blocks: 0,
        },
        blocks: PalettedContainer::from_dense(
            ContainerKind::Blocks,
            registry.block_registry(),
            &blocks,
            65536,
        )
        .unwrap(),
        biomes: PalettedContainer::single(
            ContainerKind::Biomes,
            registry.biome_registry(),
            registry.plains_id(),
        )
        .unwrap(),
    };
    let mut section_bytes = Vec::with_capacity(65536);
    section_data.write_network(&mut section_bytes).unwrap();
    let dimension = DimensionHeight::new(0, 16).unwrap();
    let source = LightingSource::from_sections(
        Arc::clone(&registry),
        dimension,
        vec![LightingChunk {
            address: position(),
            sections: vec![Some(section_data)],
        }],
        SourceLimits {
            max_chunks: 1,
            metadata_bytes: 65536,
            owned_section_bytes: 65536,
        },
    )
    .unwrap();
    let mut storage = storage(LightKind::Block);
    storage.update_section_status(section(0), false).unwrap();
    publish(&mut storage);
    let mut engine = BlockLightEngine::new(BlockLightLimits {
        checks: 16,
        decreases: 16384,
        increases: 16384,
        queue_bytes: 2 * 1024 * 1024,
    })
    .unwrap();
    engine
        .propagate_light_sources(&source, &mut storage, position())
        .unwrap();
    let mut complete = false;
    for _ in 0..100_000 {
        if engine.run(&source, &mut storage, 256).unwrap().complete {
            complete = true;
            break;
        }
    }
    assert!(complete, "finite admitted light propagation must converge");
    let snapshot = storage.snapshot();
    assert_eq!(snapshot.get_level(LightBlock { x: 8, y: 8, z: 8 }), 15);
    assert_eq!(snapshot.get_level(LightBlock { x: 9, y: 8, z: 8 }), 14);
    let bridge = PacketLightSnapshot::new(
        position(),
        dimension,
        Some(&snapshot),
        None,
        ChangedFilters::default(),
        1024,
    )
    .unwrap();
    let light = bridge.light_data();
    assert_ne!(light.block_mask[0] & 2, 0);
    let index = usize::from(light.block_mask[0] & 1);
    let LightUpdate::Bytes(bytes) = light.block_updates[index] else {
        panic!("computed light must retain allocated data")
    };
    assert_eq!(bytes[(8 * 256 + 8 * 16 + 8) / 2], 0xef);
    let packet = bridge.chunk_packet(&[], &section_bytes, &[]);
    let encoded = chunk_packet::encode(
        &packet,
        registry.block_entity_type_count(),
        chunk_packet::Limits::default(),
    )
    .unwrap();
    assert_eq!(
        chunk_packet::encoded_len(
            &packet,
            registry.block_entity_type_count(),
            chunk_packet::Limits::default()
        )
        .unwrap(),
        encoded.len()
    );
    // This tests completed light publication and bytes, not whole-world send or
    // Play readiness. No listener is activated and no readiness token is forged.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (peer, accepted) = tokio::join!(
                TcpStream::connect(listener.local_addr().unwrap()),
                listener.accept()
            );
            let mut peer = peer.unwrap();
            let pool = Arc::new(
                CpuPool::new(CpuPoolConfig {
                    workers: 1,
                    max_jobs: 2,
                    buffer_bytes: 16 * 1024 * 1024,
                })
                .unwrap(),
            );
            let mut transport =
                ConnectionTransport::new(accepted.unwrap().0, pool, TransportLimits::default());
            let mut sender = ChunkSender::new(
                false,
                SenderLimits {
                    max_pending: 1,
                    control_bytes: 4096,
                },
            )
            .unwrap();
            let mut queue = ChunkDeliveryQueue::new(DeliveryLimits {
                max_groups: 1,
                max_bytes: 65536,
            })
            .unwrap();
            sender.mark_pending(position()).unwrap();
            {
                let mut plan = sender.begin_tick(1, position()).unwrap();
                plan.try_admit(
                    &mut queue,
                    &[Some(SendReadyChunk {
                        position: position(),
                        packet_bytes: &encoded,
                    })],
                )
                .unwrap();
            }
            let mut control = chunk_packet::batch_start();
            while let Some(intent) = queue.front_packet() {
                transport
                    .write_packet(chunk_packet::delivery_bytes(intent, &mut control).unwrap())
                    .await
                    .unwrap();
                queue.packet_written().unwrap();
            }
            for expected in [&[0x0c][..], encoded.as_slice(), &[0x0b, 1][..]] {
                let actual = timeout(Duration::from_secs(2), async {
                    let mut length = 0usize;
                    for offset in 0..3 {
                        let byte = peer.read_u8().await.unwrap();
                        length |= usize::from(byte & 127) << (7 * offset);
                        if byte & 128 == 0 {
                            break;
                        }
                        assert!(offset < 2);
                    }
                    let mut bytes = vec![0; length];
                    peer.read_exact(&mut bytes).await.unwrap();
                    bytes
                })
                .await
                .unwrap();
                assert_eq!(actual, expected);
            }
        });
}
