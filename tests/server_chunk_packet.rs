use arrow_mc::nbt::{Compound, NbtString, Tag};
use arrow_mc::server::chunk_packet::{
    self, BlockEntity, ChunkWithLight, Error, HeightmapEntry, LightData, Limits,
};
use arrow_mc::world::{heightmap::HeightmapKind, preparation::ChunkAddress};

fn position() -> ChunkAddress {
    ChunkAddress { x: -2, z: 3 }
}

fn packet() -> ChunkWithLight<'static> {
    ChunkWithLight {
        position: position(),
        heightmaps: &[],
        sections: &[],
        block_entities: &[],
        light: LightData::default(),
    }
}

#[test]
fn exact_size_and_output_admission_do_not_reserve_the_maximum_packet_size() {
    let packet = packet();
    let mut expected = vec![0x2d];
    expected.extend_from_slice(&(-2i32).to_be_bytes());
    expected.extend_from_slice(&3i32.to_be_bytes());
    expected.extend_from_slice(&[0; 9]);
    assert_eq!(expected.len(), 18);
    assert_eq!(
        chunk_packet::encoded_len(&packet, 0, Limits::default()),
        Ok(18)
    );
    let limits = Limits {
        packet_bytes: 18,
        allocation_bytes: 18,
        ..Limits::default()
    };
    let encoded = chunk_packet::encode(&packet, 0, limits).unwrap();
    assert_eq!(encoded, expected);
    assert_eq!(encoded.capacity(), 18);
    assert_eq!(
        chunk_packet::encode(
            &packet,
            0,
            Limits {
                allocation_bytes: 17,
                ..limits
            }
        ),
        Err(Error::AllocationLimit)
    );
    assert_eq!(
        chunk_packet::encode(
            &packet,
            0,
            Limits {
                packet_bytes: 17,
                ..limits
            }
        ),
        Err(Error::PacketLimit)
    );
}

#[test]
fn heightmaps_preserve_supplied_map_order_and_raw_word_bits_without_nbt() {
    let entries = [
        HeightmapEntry {
            kind: HeightmapKind::MotionBlockingNoLeaves,
            words: &[0x8000_0000_0000_0001, u64::MAX],
        },
        HeightmapEntry {
            kind: HeightmapKind::WorldSurfaceWg,
            words: &[],
        },
    ];
    let packet = ChunkWithLight {
        heightmaps: &entries,
        ..packet()
    };
    let encoded = chunk_packet::encode(&packet, 0, Limits::default()).unwrap();
    assert_eq!(&encoded[9..12], &[2, 5, 2]);
    assert_eq!(&encoded[12..20], &0x8000_0000_0000_0001u64.to_be_bytes());
    assert_eq!(&encoded[20..28], &u64::MAX.to_be_bytes());
    assert_eq!(&encoded[28..30], &[0, 0]);
    let duplicate = [entries[0], entries[0]];
    assert_eq!(
        chunk_packet::encode(
            &ChunkWithLight {
                heightmaps: &duplicate,
                ..packet
            },
            0,
            Limits::default()
        ),
        Err(Error::DuplicateHeightmap)
    );
}

#[test]
fn absent_empty_and_nonempty_update_compounds_have_distinct_network_roots() {
    let empty = Tag::Compound(Compound::new());
    let mut compound = Compound::new();
    compound
        .insert(NbtString::from("x"), Tag::Int(123))
        .unwrap();
    let present = Tag::Compound(compound);
    let entries = [
        BlockEntity {
            packed_xz: 0xf2,
            y: -64,
            type_id: 0,
            update_tag: None,
        },
        BlockEntity {
            packed_xz: 0x81,
            y: i16::MAX,
            type_id: 1,
            update_tag: Some(&empty),
        },
        BlockEntity {
            packed_xz: 0x07,
            y: i16::MIN,
            type_id: 48,
            update_tag: Some(&present),
        },
    ];
    let input = ChunkWithLight {
        block_entities: &entries,
        ..packet()
    };
    let output = chunk_packet::encode(&input, 49, Limits::default()).unwrap();
    assert_eq!(&output[11..17], &[3, 0xf2, 0xff, 0xc0, 0, 0]);
    assert_eq!(&output[17..23], &[0x81, 0x7f, 0xff, 1, 10, 0]);
    assert_eq!(
        &output[23..36],
        &[0x07, 0x80, 0, 48, 10, 3, 0, 1, b'x', 0, 0, 0, 123]
    );
    assert_eq!(output[36], 0); // Compound end, not an option Boolean.
    assert_eq!(
        chunk_packet::encoded_len(&input, 49, Limits::default()).unwrap(),
        output.len()
    );
}

#[test]
fn type_domain_compound_kind_and_nested_nbt_errors_fail_preflight() {
    let scalar = Tag::Int(3);
    let mut entity = [BlockEntity {
        packed_xz: 0,
        y: 0,
        type_id: 2,
        update_tag: None,
    }];
    assert_eq!(
        chunk_packet::encode(
            &ChunkWithLight {
                block_entities: &entity,
                ..packet()
            },
            2,
            Limits::default()
        ),
        Err(Error::InvalidBlockEntityType)
    );
    entity[0].type_id = 1;
    entity[0].update_tag = Some(&scalar);
    assert_eq!(
        chunk_packet::encode(
            &ChunkWithLight {
                block_entities: &entity,
                ..packet()
            },
            2,
            Limits::default()
        ),
        Err(Error::ExpectedCompound)
    );
    let mut invalid = Compound::new();
    invalid.insert(NbtString::from("bad"), Tag::End).unwrap();
    let invalid = Tag::Compound(invalid);
    entity[0].update_tag = Some(&invalid);
    assert_eq!(
        chunk_packet::encode(
            &ChunkWithLight {
                block_entities: &entity,
                ..packet()
            },
            2,
            Limits::default()
        ),
        Err(Error::Nbt(arrow_mc::nbt::Error::UnexpectedEnd))
    );
    let empty = Tag::Compound(Compound::new());
    entity[0].update_tag = Some(&empty);
    let limits = Limits {
        nbt: arrow_mc::nbt::Limits {
            max_depth: 0,
            ..Default::default()
        },
        ..Limits::default()
    };
    assert_eq!(
        chunk_packet::encode(
            &ChunkWithLight {
                block_entities: &entity,
                ..packet()
            },
            2,
            limits
        ),
        Err(Error::Nbt(arrow_mc::nbt::Error::DepthLimit))
    );
}

#[test]
fn light_masks_use_byte_counts_and_preserve_allocated_zero_updates() {
    let zero_data = [0; 2048];
    let updates: [&[u8]; 1] = [&zero_data];
    let input = ChunkWithLight {
        light: LightData {
            sky_mask: &[0x80, 1, 0, 0],
            block_mask: &[0, 0, 0, 0, 0, 0, 0, 0, 1],
            empty_sky_mask: &[0, 0],
            empty_block_mask: &[0x80],
            sky_updates: &updates,
            block_updates: &[],
        },
        ..packet()
    };
    let output = chunk_packet::encode(&input, 0, Limits::default()).unwrap();
    assert_eq!(&output[12..15], &[2, 0x80, 1]);
    assert_eq!(&output[15..25], &[9, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(&output[25..31], &[0, 1, 0x80, 1, 0x80, 0x10]);
    assert_eq!(&output[31..31 + 2048], &zero_data);
    assert_eq!(output[31 + 2048], 0);
}

#[test]
fn low_level_light_codec_limits_do_not_invent_mask_popcount_constraints() {
    for length in [0, 1, 2047, 2048, 2049] {
        let bytes = vec![0x5a; length];
        let updates = [bytes.as_slice()];
        let input = ChunkWithLight {
            light: LightData {
                // Deliberately overlapping masks and counts: accepted by raw codec.
                sky_mask: &[255],
                empty_sky_mask: &[255],
                sky_updates: &updates,
                ..LightData::default()
            },
            ..packet()
        };
        let result = chunk_packet::encode(&input, 0, Limits::default());
        if length <= 2048 {
            assert!(result.is_ok());
        } else {
            assert_eq!(result, Err(Error::LightUpdateLimit));
        }
    }
    let input = ChunkWithLight {
        light: LightData {
            sky_mask: &[0; 19],
            ..LightData::default()
        },
        ..packet()
    };
    assert_eq!(
        chunk_packet::encoded_len(
            &input,
            0,
            Limits {
                packet_bytes: 18,
                ..Limits::default()
            }
        ),
        Err(Error::MaskInputLimit)
    );
}

#[test]
fn section_field_bound_and_outer_frame_bound_are_separate() {
    use arrow_mc::server::compression::{
        CompressionError, CompressionLimits, CompressionScratch, CompressionState,
    };
    let sections = vec![0; chunk_packet::MAX_SECTION_BYTES];
    let input = ChunkWithLight {
        sections: &sections,
        ..packet()
    };
    let encoded = chunk_packet::encode(&input, 0, Limits::default()).unwrap();
    let mut frame = Vec::new();
    let mut allocation = 16 * 1024 * 1024;
    let mut scratch = CompressionScratch::default();
    assert!(matches!(
        CompressionState::new(-1).encode_frame(
            &encoded,
            &mut scratch,
            &mut frame,
            CompressionLimits::default(),
            &mut allocation
        ),
        Err(CompressionError::FrameTooLarge)
    ));
    assert!(frame.is_empty());
    CompressionState::new(256)
        .encode_frame(
            &encoded,
            &mut scratch,
            &mut frame,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    assert!(frame.len() < 16 * 1024);
    let too_large = vec![0; chunk_packet::MAX_SECTION_BYTES + 1];
    assert_eq!(
        chunk_packet::encode(
            &ChunkWithLight {
                sections: &too_large,
                ..packet()
            },
            0,
            Limits::default()
        ),
        Err(Error::SectionLimit)
    );
}

#[test]
fn small_control_packets_preserve_exact_ids_signed_varints_and_packed_coordinates() {
    assert_eq!(chunk_packet::batch_start().as_bytes(), &[0x0c]);
    assert_eq!(
        chunk_packet::batch_finished(-1).as_bytes(),
        &[0x0b, 255, 255, 255, 255, 15]
    );
    assert_eq!(
        chunk_packet::forget(ChunkAddress {
            x: 0x1234_5678,
            z: 0x89ab_cdefu32 as i32
        })
        .as_bytes(),
        &[0x25, 0x89, 0xab, 0xcd, 0xef, 0x12, 0x34, 0x56, 0x78]
    );
    assert_eq!(
        chunk_packet::cache_center(ChunkAddress { x: 128, z: -1 }).as_bytes(),
        &[0x5f, 0x80, 1, 255, 255, 255, 255, 15]
    );
    assert_eq!(
        chunk_packet::cache_radius(-1).as_bytes(),
        &[0x60, 255, 255, 255, 255, 15]
    );
    assert_eq!(
        chunk_packet::cache_center(ChunkAddress {
            x: i32::MIN,
            z: i32::MIN
        })
        .as_bytes()
        .len(),
        11
    );
}

#[test]
fn encoded_chunk_and_controls_flow_through_delivery_queue_and_real_tcp_transport() {
    use arrow_mc::runtime::{CpuPool, CpuPoolConfig};
    use arrow_mc::server::{
        chunk_sender::{
            ChunkDeliveryQueue, ChunkSender, DeliveryLimits, SendReadyChunk, SenderLimits,
        },
        transport::{ConnectionTransport, TransportLimits},
    };
    use arrow_mc::world::section::{
        ContainerKind, PalettedContainer, Registry, Section, SectionCounts,
    };
    use std::{sync::Arc, time::Duration};
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
        time::timeout,
    };
    // A synthetic one-section fixture uses the existing real section encoder.
    // This transport test does not claim that a world is ready or enter Play.
    let section = Section {
        counts: SectionCounts {
            non_empty_blocks: 0,
            fluid_blocks: 0,
        },
        blocks: PalettedContainer::single(ContainerKind::Blocks, Registry::new(1).unwrap(), 0)
            .unwrap(),
        biomes: PalettedContainer::single(ContainerKind::Biomes, Registry::new(1).unwrap(), 0)
            .unwrap(),
    };
    let mut sections = Vec::with_capacity(32);
    section.write_network(&mut sections).unwrap();
    let encoded = chunk_packet::encode(
        &ChunkWithLight {
            sections: &sections,
            ..packet()
        },
        0,
        Limits::default(),
    )
    .unwrap();
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
                max_groups: 2,
                max_bytes: 4096,
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
            sender.drop_chunk(position(), true, &mut queue).unwrap();
            let center = chunk_packet::cache_center(position());
            let radius = chunk_packet::cache_radius(32);
            transport.write_packet(center.as_bytes()).await.unwrap();
            transport.write_packet(radius.as_bytes()).await.unwrap();
            let mut control = chunk_packet::batch_start();
            while let Some(intent) = queue.front_packet() {
                let bytes = chunk_packet::delivery_bytes(intent, &mut control).unwrap();
                transport.write_packet(bytes).await.unwrap();
                queue.packet_written().unwrap();
            }
            let expected = [
                center.as_bytes(),
                radius.as_bytes(),
                &[0x0c],
                encoded.as_slice(),
                &[0x0b, 1],
                chunk_packet::forget(position()).as_bytes(),
            ]
            .map(<[u8]>::to_vec);
            for expected in expected {
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
            assert_eq!(queue.group_count(), 0);
            assert_eq!(sender.stats().unacknowledged_batches, 1);
        });
}
