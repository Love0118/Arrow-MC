use arrow_mc::server::chunk_sender::{
    AdmissionOutcome, ChunkDeliveryQueue, ChunkPacket, ChunkSender, DeliveryLimits, DropOutcome,
    Error, SendReadyChunk, SenderLimits,
};
use arrow_mc::world::preparation::ChunkAddress;

fn pos(x: i32, z: i32) -> ChunkAddress {
    ChunkAddress { x, z }
}

fn sender(memory: bool, max_pending: usize) -> ChunkSender {
    ChunkSender::new(
        memory,
        SenderLimits {
            max_pending,
            control_bytes: 1 << 20,
        },
    )
    .unwrap()
}

fn queue(groups: usize) -> ChunkDeliveryQueue {
    ChunkDeliveryQueue::new(DeliveryLimits {
        max_groups: groups,
        max_bytes: 1 << 20,
    })
    .unwrap()
}

fn admit_all(sender: &mut ChunkSender, queue: &mut ChunkDeliveryQueue, tick: u64) -> usize {
    let mut plan = sender.begin_tick(tick, pos(0, 0)).unwrap();
    let ready: Vec<_> = plan
        .candidates()
        .iter()
        .map(|position| {
            Some(SendReadyChunk {
                position: *position,
                packet_bytes: &[42, 7],
            })
        })
        .collect();
    match plan.try_admit(queue, &ready).unwrap() {
        AdmissionOutcome::NoReadyChunks => 0,
        AdmissionOutcome::Admitted { chunks, .. } => chunks,
    }
}

fn drain(queue: &mut ChunkDeliveryQueue) {
    while queue.front_packet().is_some() {
        queue.packet_written().unwrap();
    }
}

#[test]
fn initial_rate_outstanding_gate_and_unsolicited_ack_are_exact() {
    let mut sender = sender(false, 100);
    let initial = sender.stats();
    assert_eq!(initial.desired_chunks_per_tick, 9.0);
    assert_eq!(initial.batch_quota, 0.0);
    assert_eq!(
        (
            initial.unacknowledged_batches,
            initial.max_unacknowledged_batches
        ),
        (0, 1)
    );
    for x in 0..30 {
        sender.mark_pending(pos(x, 0)).unwrap();
    }
    let mut queue = queue(20);
    assert_eq!(admit_all(&mut sender, &mut queue, 1), 9);
    assert_eq!(sender.stats().batch_quota, 0.0);
    assert_eq!(admit_all(&mut sender, &mut queue, 2), 0);
    assert_eq!(sender.stats().batch_quota, 0.0);
    sender.acknowledge(2.5);
    assert_eq!(sender.stats().batch_quota, 1.0);
    assert_eq!(sender.stats().max_unacknowledged_batches, 10);
    assert_eq!(admit_all(&mut sender, &mut queue, 3), 2);
    assert_eq!(sender.stats().batch_quota, 0.5);
    sender.acknowledge(2.5);
    sender.acknowledge(64.0);
    assert_eq!(sender.stats().unacknowledged_batches, 0);
    assert_eq!(sender.stats().batch_quota, 1.0);
    assert_eq!(sender.stats().desired_chunks_per_tick, 64.0);
}

#[test]
fn all_float_rate_edges_and_fractional_accumulation_use_f32() {
    let mut sender = sender(false, 10);
    for (input, expected) in [
        (f32::NAN, 0.01f32),
        (f32::from_bits(0xffc0_0001), 0.01),
        (f32::NEG_INFINITY, 0.01),
        (-1.0, 0.01),
        (-0.0, 0.01),
        (0.0, 0.01),
        (0.009, 0.01),
        (0.01, 0.01),
        (63.5, 63.5),
        (64.0, 64.0),
        (64.1, 64.0),
        (f32::INFINITY, 64.0),
    ] {
        sender.acknowledge(input);
        assert_eq!(
            sender.stats().desired_chunks_per_tick.to_bits(),
            expected.to_bits()
        );
    }
    sender.mark_pending(pos(0, 0)).unwrap();
    sender.acknowledge(0.01);
    let mut queue = queue(10);
    assert_eq!(admit_all(&mut sender, &mut queue, 0), 1);
    for tick in 1..=100 {
        assert!(
            sender
                .begin_tick(tick, pos(0, 0))
                .unwrap()
                .candidates()
                .is_empty()
        );
    }
    assert_eq!(sender.stats().batch_quota.to_bits(), 0x3f7f_fff5);
    let _ = sender.begin_tick(101, pos(0, 0)).unwrap();
    assert_eq!(sender.stats().batch_quota, 1.0);
}

#[test]
fn cap_ten_pauses_accrual_and_ack_with_other_batches_does_not_reset_quota() {
    let mut sender = sender(false, 20);
    sender.acknowledge(1.5);
    for x in 0..20 {
        sender.mark_pending(pos(x, 0)).unwrap();
    }
    let mut queue = queue(11);
    for tick in 0..10 {
        assert_eq!(admit_all(&mut sender, &mut queue, tick), 1);
    }
    assert_eq!(sender.stats().unacknowledged_batches, 10);
    assert_eq!(sender.stats().batch_quota, 0.5);
    assert_eq!(admit_all(&mut sender, &mut queue, 10), 0);
    assert_eq!(sender.stats().batch_quota, 0.5);
    sender.acknowledge(0.01);
    assert_eq!(sender.stats().unacknowledged_batches, 9);
    assert_eq!(sender.stats().batch_quota, 0.5);
    assert_eq!(admit_all(&mut sender, &mut queue, 11), 0);
    assert_eq!(sender.stats().batch_quota, 0.51);
}

#[test]
fn unavailable_near_candidates_are_not_replaced_by_far_ready_chunks() {
    let mut sender = sender(false, 8);
    sender.acknowledge(2.0);
    for x in 0..5 {
        sender.mark_pending(pos(x, 0)).unwrap();
    }
    let mut queue = queue(4);
    let mut plan = sender.begin_tick(1, pos(0, 0)).unwrap();
    assert_eq!(plan.candidates(), &[pos(0, 0), pos(1, 0)]);
    assert_eq!(
        plan.try_admit(&mut queue, &[None, None]),
        Ok(AdmissionOutcome::NoReadyChunks)
    );
    assert_eq!(
        plan.try_admit(
            &mut queue,
            &[
                None,
                Some(SendReadyChunk {
                    position: pos(4, 0),
                    packet_bytes: &[1]
                })
            ]
        ),
        Err(Error::InvalidReadiness)
    );
    assert_eq!(
        plan.try_admit(
            &mut queue,
            &[
                None,
                Some(SendReadyChunk {
                    position: pos(1, 0),
                    packet_bytes: &[9]
                })
            ]
        ),
        Ok(AdmissionOutcome::Admitted {
            chunks: 1,
            packet_bytes: 1
        })
    );
    // The synchronous plan borrow ends here.
    assert!(sender.is_pending(pos(0, 0)));
    assert!(!sender.is_pending(pos(1, 0)));
    assert!(sender.is_pending(pos(4, 0)));
    assert_eq!(sender.stats().batch_quota, 1.0);
}

#[test]
fn actual_guava_tie_membership_and_k_one_partition_are_preserved() {
    let mut sender = sender(false, 8);
    for position in [
        pos(1, -1),
        pos(-2, 0),
        pos(0, -2),
        pos(0, -1),
        pos(1, 2),
        pos(-1, -3),
    ] {
        sender.mark_pending(position).unwrap();
    }
    sender.acknowledge(3.0);
    assert_eq!(
        sender.begin_tick(1, pos(0, 0)).unwrap().candidates(),
        &[pos(0, -1), pos(1, -1), pos(0, -2)]
    );
    // In this observed table order (-2,0) precedes the nearer (0,-1).
    let mut sender = self::sender(false, 2);
    sender.mark_pending(pos(-2, 0)).unwrap();
    sender.mark_pending(pos(0, -1)).unwrap();
    sender.acknowledge(1.0);
    assert_eq!(
        sender.begin_tick(1, pos(0, 0)).unwrap().candidates(),
        &[pos(0, -1)]
    );
}

#[test]
fn java_wrapped_distance_is_not_wide_euclidean_distance() {
    let mut sender = sender(false, 3);
    for position in [pos(0, 0), pos(50_000, 0), pos(1, 0)] {
        sender.mark_pending(position).unwrap();
    }
    sender.acknowledge(1.0);
    assert_eq!(
        sender.begin_tick(1, pos(0, 0)).unwrap().candidates(),
        &[pos(50_000, 0)]
    );
}

#[test]
fn memory_connection_sends_all_ready_even_beyond_quota_and_can_go_negative() {
    let mut sender = sender(true, 100);
    for x in 0..70 {
        sender.mark_pending(pos(x, 0)).unwrap();
    }
    let mut queue = queue(2);
    assert_eq!(admit_all(&mut sender, &mut queue, 0), 70);
    assert_eq!(sender.stats().batch_quota, -61.0);
    assert_eq!(sender.stats().unacknowledged_batches, 1);
    assert_eq!(admit_all(&mut sender, &mut queue, 1), 0);
    assert_eq!(sender.stats().batch_quota, -61.0);
    sender.acknowledge(64.0);
    assert_eq!(sender.stats().batch_quota, 1.0);
}

#[test]
fn full_queue_retry_is_atomic_and_never_accrues_a_second_time() {
    let mut sender = sender(false, 4);
    sender.mark_pending(pos(0, 0)).unwrap();
    let mut queue = queue(1);
    assert_eq!(
        sender.drop_chunk(pos(9, 9), true, &mut queue),
        Ok(DropOutcome::ForgetQueued)
    );
    let mut plan = sender.begin_tick(10, pos(0, 0)).unwrap();
    let ready = [Some(SendReadyChunk {
        position: pos(0, 0),
        packet_bytes: &[1, 2, 3],
    })];
    assert_eq!(plan.try_admit(&mut queue, &ready), Err(Error::DeliveryFull));
    assert_eq!(plan.candidates(), &[pos(0, 0)]);
    queue.packet_written().unwrap();
    assert_eq!(
        plan.try_admit(&mut queue, &ready),
        Ok(AdmissionOutcome::Admitted {
            chunks: 1,
            packet_bytes: 3
        })
    );
    assert_eq!(
        plan.try_admit(&mut queue, &ready),
        Err(Error::AlreadyAdmitted)
    );
    // The synchronous plan borrow ends here.
    assert_eq!(sender.stats().batch_quota, 8.0);
    assert_eq!(sender.stats().pending, 0);
    assert_eq!(sender.stats().unacknowledged_batches, 1);
    assert!(matches!(
        sender.begin_tick(10, pos(0, 0)),
        Err(Error::TickAlreadyStarted)
    ));
    assert!(matches!(
        sender.begin_tick(9, pos(0, 0)),
        Err(Error::TickAlreadyStarted)
    ));
}

#[test]
fn byte_backpressure_keeps_whole_memory_batch_and_does_not_publish_a_prefix() {
    let mut sender = sender(true, 20);
    for x in 0..20 {
        sender.mark_pending(pos(x, 0)).unwrap();
    }
    let mut queue = ChunkDeliveryQueue::new(DeliveryLimits {
        max_groups: 1,
        max_bytes: 200,
    })
    .unwrap();
    let before = queue.retained_bytes();
    let mut plan = sender.begin_tick(1, pos(0, 0)).unwrap();
    let ready: Vec<_> = plan
        .candidates()
        .iter()
        .map(|position| {
            Some(SendReadyChunk {
                position: *position,
                packet_bytes: &[1; 100],
            })
        })
        .collect();
    assert_eq!(
        plan.try_admit(&mut queue, &ready),
        Err(Error::DeliveryBytes)
    );
    // The synchronous plan borrow ends here.
    assert_eq!(queue.retained_bytes(), before);
    assert_eq!(queue.group_count(), 0);
    assert_eq!(sender.stats().pending, 20);
    assert_eq!(sender.stats().unacknowledged_batches, 0);
    assert_eq!(sender.stats().batch_quota, 9.0);
}

#[test]
fn ordering_owned_payload_and_slow_write_retention_cover_entire_batch() {
    let mut sender = sender(false, 2);
    sender.mark_pending(pos(0, 0)).unwrap();
    sender.mark_pending(pos(1, 0)).unwrap();
    let mut queue = queue(2);
    let base = queue.retained_bytes();
    let mut bytes = vec![9, 8, 7];
    {
        let mut plan = sender.begin_tick(0, pos(0, 0)).unwrap();
        plan.try_admit(
            &mut queue,
            &[
                Some(SendReadyChunk {
                    position: pos(0, 0),
                    packet_bytes: &bytes,
                }),
                Some(SendReadyChunk {
                    position: pos(1, 0),
                    packet_bytes: &[6],
                }),
            ],
        )
        .unwrap();
    }
    bytes.fill(0);
    let charged = queue.retained_bytes();
    assert!(charged > base + 4);
    assert_eq!(queue.front_packet(), Some(ChunkPacket::Start));
    queue.packet_written().unwrap();
    assert_eq!(
        queue.front_packet(),
        Some(ChunkPacket::Data {
            position: pos(0, 0),
            packet_bytes: &[9, 8, 7]
        })
    );
    assert_eq!(queue.retained_bytes(), charged);
    queue.packet_written().unwrap();
    assert_eq!(
        queue.front_packet(),
        Some(ChunkPacket::Data {
            position: pos(1, 0),
            packet_bytes: &[6]
        })
    );
    queue.packet_written().unwrap();
    assert_eq!(
        queue.front_packet(),
        Some(ChunkPacket::Finish { chunks: 2 })
    );
    assert_eq!(queue.retained_bytes(), charged);
    queue.packet_written().unwrap();
    assert_eq!(queue.front_packet(), None);
    assert_eq!(queue.retained_bytes(), base);
    assert_eq!(queue.packet_written(), Err(Error::NoPacket));
}

#[test]
fn forget_conditions_follow_pending_and_alive_without_a_sent_ledger() {
    let mut sender = sender(false, 2);
    let mut queue = queue(1);
    sender.mark_pending(pos(1, 1)).unwrap();
    assert_eq!(
        sender.drop_chunk(pos(1, 1), true, &mut queue),
        Ok(DropOutcome::RemovedPending)
    );
    assert!(queue.front_packet().is_none());
    assert_eq!(
        sender.drop_chunk(pos(2, 2), false, &mut queue),
        Ok(DropOutcome::NoPacket)
    );
    assert_eq!(
        sender.drop_chunk(pos(2, 2), true, &mut queue),
        Ok(DropOutcome::ForgetQueued)
    );
    assert_eq!(
        queue.front_packet(),
        Some(ChunkPacket::Forget {
            position: pos(2, 2)
        })
    );
    assert_eq!(
        sender.drop_chunk(pos(3, 3), true, &mut queue),
        Err(Error::DeliveryFull)
    );
    drain(&mut queue);
    assert_eq!(
        sender.drop_chunk(pos(3, 3), true, &mut queue),
        Ok(DropOutcome::ForgetQueued)
    );
}

#[test]
fn failed_write_closes_queue_and_releases_owned_batch_without_retry() {
    let mut sender = sender(false, 2);
    let mut queue = queue(1);
    let base = queue.retained_bytes();
    sender.mark_pending(pos(0, 0)).unwrap();
    assert_eq!(admit_all(&mut sender, &mut queue, 1), 1);
    queue.packet_written().unwrap();
    queue.fail();
    assert!(queue.is_closed());
    assert_eq!(queue.retained_bytes(), base);
    assert_eq!(queue.front_packet(), None);
    assert_eq!(queue.packet_written(), Err(Error::Closed));
    assert_eq!(
        sender.drop_chunk(pos(1, 1), true, &mut queue),
        Err(Error::Closed)
    );
    // Admission was real; a physical write failure closes the connection. It
    // does not manufacture an ACK or put half-written chunks back in pending.
    assert_eq!(sender.stats().unacknowledged_batches, 1);
    assert!(!sender.is_pending(pos(0, 0)));
}

#[test]
fn pending_and_control_limits_are_explicit_and_duplicate_marks_are_free() {
    assert!(matches!(
        ChunkSender::new(
            false,
            SenderLimits {
                max_pending: 0,
                control_bytes: 0
            }
        ),
        Err(Error::InvalidLimits)
    ));
    assert!(matches!(
        ChunkSender::new(
            false,
            SenderLimits {
                max_pending: 10,
                control_bytes: 1
            }
        ),
        Err(Error::ControlBudget)
    ));
    let mut sender = sender(false, 1);
    let bytes = sender.stats().control_bytes;
    assert_eq!(sender.mark_pending(pos(0, 0)), Ok(true));
    assert_eq!(sender.mark_pending(pos(0, 0)), Ok(false));
    assert_eq!(sender.mark_pending(pos(1, 0)), Err(Error::PendingFull));
    assert!(sender.is_pending(pos(0, 0)));
    assert!(!sender.is_pending(pos(1, 0)));
    assert_eq!(sender.stats().control_bytes, bytes);
}

#[test]
fn abandoned_plan_preserves_pending_and_once_observed_tick_accrual() {
    let mut sender = sender(false, 1);
    sender.mark_pending(pos(0, 0)).unwrap();
    let _ = sender.begin_tick(2, pos(0, 0)).unwrap();
    assert!(sender.is_pending(pos(0, 0)));
    assert_eq!(sender.stats().batch_quota, 9.0);
    assert_eq!(sender.stats().unacknowledged_batches, 0);
    assert!(matches!(
        sender.begin_tick(2, pos(0, 0)),
        Err(Error::TickAlreadyStarted)
    ));
    let mut plan = sender.begin_tick(3, pos(0, 0)).unwrap();
    let mut queue = queue(1);
    assert_eq!(
        plan.try_admit(&mut queue, &[]),
        Err(Error::InvalidReadiness)
    );
    assert_eq!(
        plan.try_admit(
            &mut queue,
            &[Some(SendReadyChunk {
                position: pos(0, 0),
                packet_bytes: &[]
            })]
        ),
        Err(Error::InvalidReadiness)
    );
}
