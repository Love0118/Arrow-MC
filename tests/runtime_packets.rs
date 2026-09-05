use arrow_mc::runtime::{
    AdmissionError, CpuPool, CpuPoolConfig, PacketJobError, PacketJobOutput, PacketOperation,
    PacketTask, PendingPacket, SECTION_JOB_BUFFER_BYTES, SectionKey,
};
use arrow_mc::server::compression::{
    CompressionError, CompressionLimits, CompressionScratch, CompressionState,
    MAX_FRAME_BODY_BYTES, MAX_UNCOMPRESSED_BYTES,
};
use arrow_mc::world::section::{Registry, SectionCounts};
use std::time::Duration;
use tokio::time::timeout;

fn pool(workers: usize, max_jobs: usize, buffer_bytes: usize) -> CpuPool {
    CpuPool::new(CpuPoolConfig {
        workers,
        max_jobs,
        buffer_bytes: buffer_bytes.max(SECTION_JOB_BUFFER_BYTES),
    })
    .unwrap()
}

fn limits() -> CompressionLimits {
    CompressionLimits {
        max_frame_body_bytes: 8192,
        max_uncompressed_bytes: 16384,
    }
}

fn reserve(
    pool: &CpuPool,
    operation: PacketOperation,
    input: &[u8],
    limits: CompressionLimits,
) -> PendingPacket {
    let mut pending = pool
        .try_reserve_packet(operation, input.len(), limits)
        .unwrap();
    assert_eq!(pending.input_mut().len(), input.len());
    pending.input_mut().copy_from_slice(input);
    pending
}

fn encode(threshold: i32, input: &[u8], limits: CompressionLimits) -> Vec<u8> {
    let mut output = Vec::new();
    let mut allocation_remaining = usize::MAX;
    CompressionState::new(threshold)
        .encode_frame(
            input,
            &mut CompressionScratch::default(),
            &mut output,
            limits,
            &mut allocation_remaining,
        )
        .unwrap();
    output
}

fn decode(threshold: i32, frame: &[u8], limits: CompressionLimits) -> Vec<u8> {
    let mut output = Vec::new();
    let mut input = frame;
    let mut allocation_remaining = usize::MAX;
    CompressionState::new(threshold)
        .decode_frame(
            &mut input,
            &mut CompressionScratch::default(),
            &mut output,
            limits,
            &mut allocation_remaining,
        )
        .unwrap();
    assert!(input.is_empty());
    output
}

async fn wait(task: PacketTask) -> Result<PacketJobOutput, PacketJobError> {
    timeout(Duration::from_secs(10), task.wait())
        .await
        .expect("packet worker did not complete")
}

fn assert_reserved(pool: &CpuPool, jobs: usize, bytes: usize) {
    let stats = pool.stats();
    assert_eq!(stats.in_flight, jobs);
    assert_eq!(stats.reserved_buffer_bytes, bytes);
}

#[tokio::test(flavor = "current_thread")]
async fn worker_encode_and_decode_match_synchronous_codec_across_modes() {
    for workers in [1, 2, 4] {
        let pool = pool(workers, 24, 24 * SECTION_JOB_BUFFER_BYTES);
        let mut cases = Vec::new();
        for threshold in [-1, 0, 32, 4096] {
            for packet in [vec![0x02], vec![0x5a; 32], vec![0x7f; 4096]] {
                let expected = encode(threshold, &packet, limits());
                let encoded = reserve(
                    &pool,
                    PacketOperation::Encode { threshold },
                    &packet,
                    limits(),
                )
                .submit()
                .unwrap();
                let decoded = reserve(
                    &pool,
                    PacketOperation::Decode { threshold },
                    &expected,
                    limits(),
                )
                .submit()
                .unwrap();
                cases.push((threshold, packet, expected, encoded, decoded));
            }
        }
        for (threshold, packet, expected, encoded, decoded) in cases {
            let encoded = wait(encoded).await.unwrap();
            assert_eq!(
                encoded.bytes(),
                expected,
                "workers={workers}, threshold={threshold}"
            );
            assert_eq!(decode(threshold, encoded.bytes(), limits()), packet);
            let decoded = wait(decoded).await.unwrap();
            assert_eq!(decoded.bytes(), packet);
        }
        assert_reserved(&pool, 0, 0);
        assert_eq!(pool.stats().completed_jobs, 24);
        assert!(pool.stats().peak_running <= workers);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn raw_envelope_above_inflated_limit_uses_the_outer_frame_budget() {
    let pool = pool(1, 1, SECTION_JOB_BUFFER_BYTES);
    let packet = [0x3c; 128];
    let frame = encode(256, &packet, limits());
    let decode_limits = CompressionLimits {
        max_frame_body_bytes: 2048,
        max_uncompressed_bytes: 16,
    };
    let output = wait(
        reserve(
            &pool,
            PacketOperation::Decode { threshold: 256 },
            &frame,
            decode_limits,
        )
        .submit()
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(output.bytes(), packet);
    assert_reserved(&pool, 1, frame.len() + decode_limits.max_frame_body_bytes);
    drop(output);
    assert_reserved(&pool, 0, 0);
}

#[test]
fn reservation_charges_input_and_worst_case_output_before_filling_input() {
    let pool = pool(1, 2, 4 * SECTION_JOB_BUFFER_BYTES);
    let encode_bytes = 31 + limits().max_frame_body_bytes + 3;
    let decode_bytes = 19 + limits().max_uncompressed_bytes;
    let mut encoder = pool
        .try_reserve_packet(PacketOperation::Encode { threshold: 0 }, 31, limits())
        .unwrap();
    assert_eq!(encoder.input_mut(), &[0; 31]);
    assert_reserved(&pool, 1, encode_bytes);
    let mut decoder = pool
        .try_reserve_packet(PacketOperation::Decode { threshold: 0 }, 19, limits())
        .unwrap();
    assert_eq!(decoder.input_mut(), &[0; 19]);
    assert_reserved(&pool, 2, encode_bytes + decode_bytes);
    assert!(matches!(
        pool.try_reserve_packet(PacketOperation::Encode { threshold: -1 }, 1, limits()),
        Err(AdmissionError::JobLimit)
    ));
    drop(encoder);
    assert_reserved(&pool, 1, decode_bytes);
    drop(decoder);
    assert_reserved(&pool, 0, 0);

    let raw_limits = CompressionLimits {
        max_frame_body_bytes: 2048,
        max_uncompressed_bytes: 16,
    };
    let raw = pool
        .try_reserve_packet(PacketOperation::Decode { threshold: 256 }, 256, raw_limits)
        .unwrap();
    assert_reserved(&pool, 1, 256 + 2048);
    drop(raw);
    assert_reserved(&pool, 0, 0);

    let disabled_encoder = pool
        .try_reserve_packet(PacketOperation::Encode { threshold: -1 }, 31, limits())
        .unwrap();
    assert_reserved(&pool, 1, 31 + 1 + 31);
    drop(disabled_encoder);
    let below_threshold_encoder = pool
        .try_reserve_packet(PacketOperation::Encode { threshold: 256 }, 127, limits())
        .unwrap();
    // DataLength=0 makes the body 128 bytes, requiring two outer length bytes.
    assert_reserved(&pool, 1, 127 + 2 + 1 + 127);
    drop(below_threshold_encoder);
    let disabled_decoder = pool
        .try_reserve_packet(PacketOperation::Decode { threshold: -1 }, 32, limits())
        .unwrap();
    assert_reserved(&pool, 1, 32 + 32);
    drop(disabled_decoder);
    assert_reserved(&pool, 0, 0);
}

#[test]
fn byte_budget_rejects_before_fill_and_is_reusable_after_pending_drop() {
    let limits = CompressionLimits {
        max_frame_body_bytes: SECTION_JOB_BUFFER_BYTES,
        max_uncompressed_bytes: SECTION_JOB_BUFFER_BYTES,
    };
    let charge = 32 + limits.max_frame_body_bytes + 3;
    let pool = pool(1, 3, charge);
    let first = pool
        .try_reserve_packet(PacketOperation::Encode { threshold: 0 }, 32, limits)
        .unwrap();
    assert_reserved(&pool, 1, charge);
    assert!(matches!(
        pool.try_reserve_packet(PacketOperation::Encode { threshold: 0 }, 0, limits),
        Err(AdmissionError::ByteLimit)
    ));
    assert_reserved(&pool, 1, charge);
    drop(first);
    assert_reserved(&pool, 0, 0);
    let replacement = pool
        .try_reserve_packet(PacketOperation::Encode { threshold: 0 }, 32, limits)
        .unwrap();
    assert_reserved(&pool, 1, charge);
    drop(replacement);
    assert_eq!(pool.stats().peak_reserved_buffer_bytes, charge);
}

#[test]
fn invalid_limits_and_oversize_inputs_never_acquire_a_lease() {
    let pool = pool(1, 4, 4 * SECTION_JOB_BUFFER_BYTES);
    for invalid in [
        CompressionLimits {
            max_frame_body_bytes: 0,
            ..limits()
        },
        CompressionLimits {
            max_frame_body_bytes: MAX_FRAME_BODY_BYTES + 1,
            ..limits()
        },
        CompressionLimits {
            max_uncompressed_bytes: MAX_UNCOMPRESSED_BYTES + 1,
            ..limits()
        },
    ] {
        for operation in [
            PacketOperation::Encode { threshold: 0 },
            PacketOperation::Decode { threshold: 0 },
        ] {
            assert!(matches!(
                pool.try_reserve_packet(operation, 1, invalid),
                Err(AdmissionError::InvalidInput)
            ));
            assert_reserved(&pool, 0, 0);
        }
    }
    for (operation, length) in [
        (
            PacketOperation::Encode { threshold: -1 },
            limits().max_frame_body_bytes + 1,
        ),
        (
            PacketOperation::Encode { threshold: 0 },
            limits().max_uncompressed_bytes + 1,
        ),
        (
            PacketOperation::Decode { threshold: 0 },
            limits().max_frame_body_bytes + 4,
        ),
        (PacketOperation::Encode { threshold: 0 }, usize::MAX),
        (PacketOperation::Decode { threshold: 0 }, usize::MAX),
    ] {
        assert!(matches!(
            pool.try_reserve_packet(operation, length, limits()),
            Err(AdmissionError::InvalidInput)
        ));
        assert_reserved(&pool, 0, 0);
    }
    assert_eq!(pool.stats().peak_reserved_buffer_bytes, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn retained_output_holds_full_reservation_until_it_is_dropped() {
    let pool = pool(1, 1, SECTION_JOB_BUFFER_BYTES);
    let packet = [0x7b; 2048];
    let charge = packet.len() + limits().max_frame_body_bytes + 3;
    let output = wait(
        reserve(
            &pool,
            PacketOperation::Encode { threshold: 0 },
            &packet,
            limits(),
        )
        .submit()
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(decode(0, output.bytes(), limits()), packet);
    assert!(output.bytes().len() < packet.len());
    assert_reserved(&pool, 1, charge);
    assert!(matches!(
        pool.try_reserve_packet(PacketOperation::Encode { threshold: 0 }, 1, limits()),
        Err(AdmissionError::JobLimit)
    ));
    drop(output);
    assert_reserved(&pool, 0, 0);
    let next = reserve(
        &pool,
        PacketOperation::Encode { threshold: -1 },
        &[0x01],
        limits(),
    );
    drop(next);
    assert_reserved(&pool, 0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn codec_errors_release_slots_and_do_not_poison_worker_scratch() {
    let pool = pool(1, 1, SECTION_JOB_BUFFER_BYTES);
    for (threshold, frame, expected) in [
        (0, &[5, 1, 0][..], CompressionError::Truncated),
        (0, &[3, 1, 0, 0][..], CompressionError::InvalidZlib),
        (256, &[2, 1, 0][..], CompressionError::BelowThreshold),
    ] {
        let result = wait(
            reserve(
                &pool,
                PacketOperation::Decode { threshold },
                frame,
                limits(),
            )
            .submit()
            .unwrap(),
        )
        .await;
        match result {
            Err(PacketJobError::Codec(error)) => assert_eq!(
                std::mem::discriminant(&error),
                std::mem::discriminant(&expected),
                "expected {expected:?}, got {error:?}"
            ),
            _ => panic!("expected packet codec failure: {expected:?}"),
        }
        assert_reserved(&pool, 0, 0);
    }
    let tiny_output = CompressionLimits {
        max_frame_body_bytes: 1,
        max_uncompressed_bytes: 256,
    };
    let result = wait(
        reserve(
            &pool,
            PacketOperation::Encode { threshold: 0 },
            &[42; 64],
            tiny_output,
        )
        .submit()
        .unwrap(),
    )
    .await;
    assert!(matches!(
        result,
        Err(PacketJobError::Codec(CompressionError::FrameTooLarge))
    ));
    assert_reserved(&pool, 0, 0);

    let packet = [11; 1024];
    let output = wait(
        reserve(
            &pool,
            PacketOperation::Encode { threshold: 0 },
            &packet,
            limits(),
        )
        .submit()
        .unwrap(),
    )
    .await
    .unwrap();
    let frame = output.bytes().to_vec();
    assert_eq!(frame, encode(0, &packet, limits()));
    drop(output);
    let decoded = wait(
        reserve(
            &pool,
            PacketOperation::Decode { threshold: 0 },
            &frame,
            limits(),
        )
        .submit()
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(decoded.bytes(), packet);
    drop(decoded);
    assert_reserved(&pool, 0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn decode_rejects_a_second_frame_and_releases_the_whole_reservation() {
    let pool = pool(1, 1, SECTION_JOB_BUFFER_BYTES);
    for threshold in [-1, 0, 256] {
        let mut frames = encode(threshold, &[1, 2, 3], limits());
        frames.extend_from_slice(&encode(threshold, &[4, 5, 6], limits()));
        let result = wait(
            reserve(
                &pool,
                PacketOperation::Decode { threshold },
                &frames,
                limits(),
            )
            .submit()
            .unwrap(),
        )
        .await;
        assert!(matches!(result, Err(PacketJobError::TrailingFrameBytes)));
        assert_reserved(&pool, 0, 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_receivers_release_leases_before_a_later_single_worker_completion() {
    let pool = pool(1, 17, 17 * SECTION_JOB_BUFFER_BYTES);
    for value in 0..16 {
        let task = reserve(
            &pool,
            PacketOperation::Encode { threshold: 0 },
            &[value; 4096],
            limits(),
        )
        .submit()
        .unwrap();
        drop(task);
    }
    // FIFO with one worker makes this completion a barrier for earlier cleanup,
    // regardless of whether each abandoned receiver was queued, running or done.
    let output = wait(
        reserve(
            &pool,
            PacketOperation::Encode { threshold: -1 },
            &[0x2a],
            limits(),
        )
        .submit()
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(output.bytes(), &[1, 0x2a]);
    assert_reserved(&pool, 1, 1 + 2);
    drop(output);
    assert_reserved(&pool, 0, 0);
    assert_eq!(pool.stats().completed_jobs, 17);
}

#[test]
fn close_rejects_reservation_and_submission_and_releases_pending_buffers() {
    let pool = pool(1, 2, 2 * SECTION_JOB_BUFFER_BYTES);
    let pending = reserve(
        &pool,
        PacketOperation::Encode { threshold: 0 },
        &[0; 1024],
        limits(),
    );
    pool.close();
    assert!(matches!(
        pool.try_reserve_packet(PacketOperation::Encode { threshold: 0 }, 1, limits()),
        Err(AdmissionError::Closed)
    ));
    assert!(matches!(pending.submit(), Err(AdmissionError::Closed)));
    assert_reserved(&pool, 0, 0);
    pool.shutdown().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_drains_without_waiting_for_async_receivers_to_take_outputs() {
    let pool = pool(1, 16, 16 * SECTION_JOB_BUFFER_BYTES);
    let mut tasks = Vec::new();
    for value in 0..16 {
        let task = reserve(
            &pool,
            PacketOperation::Encode { threshold: 0 },
            &[value; 4096],
            limits(),
        )
        .submit()
        .unwrap();
        if value % 2 == 0 {
            drop(task);
        } else {
            tasks.push((value, task));
        }
    }
    let (finished, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = finished.send(pool.shutdown());
    });
    timeout(Duration::from_secs(10), receiver)
        .await
        .expect("shutdown waited for packet consumers")
        .unwrap()
        .unwrap();
    for (value, task) in tasks {
        let output = wait(task).await.unwrap();
        assert_eq!(decode(0, output.bytes(), limits()), vec![value; 4096]);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_cancel_suppresses_a_result_already_published_by_a_stopped_pool() {
    let pool = pool(1, 1, SECTION_JOB_BUFFER_BYTES);
    let mut task = reserve(
        &pool,
        PacketOperation::Encode { threshold: 0 },
        &[0x4a; 1024],
        limits(),
    )
    .submit()
    .unwrap();
    pool.close();
    pool.shutdown().unwrap();
    // Shutdown joins the worker, so cancellation always follows publication.
    task.cancel();
    assert!(matches!(wait(task).await, Err(PacketJobError::Cancelled)));
}

#[tokio::test(flavor = "current_thread")]
async fn section_and_packet_work_share_job_and_byte_admission_and_retained_leases() {
    let packet = [0x5a; 1024];
    let packet_bytes = packet.len() + limits().max_frame_body_bytes + 3;
    let total_bytes = SECTION_JOB_BUFFER_BYTES + packet_bytes;
    let pool = pool(2, 2, total_bytes);
    let mut section = pool
        .try_reserve_section(
            SectionKey {
                world_epoch: 7,
                chunk_x: -3,
                chunk_z: 11,
                section_y: -4,
                revision: 17,
            },
            Registry::new(16).unwrap(),
            Registry::new(4).unwrap(),
            SectionCounts {
                non_empty_blocks: 4096,
                fluid_blocks: 0,
            },
        )
        .unwrap();
    section.blocks_mut().fill(1);
    section.biomes_mut().fill(2);
    assert_reserved(&pool, 1, SECTION_JOB_BUFFER_BYTES);
    assert!(matches!(
        pool.try_reserve_packet(
            PacketOperation::Encode { threshold: 0 },
            packet.len() + 1,
            limits()
        ),
        Err(AdmissionError::ByteLimit)
    ));
    let pending = reserve(
        &pool,
        PacketOperation::Encode { threshold: 0 },
        &packet,
        limits(),
    );
    assert_reserved(&pool, 2, total_bytes);
    assert!(matches!(
        pool.try_reserve_packet(PacketOperation::Encode { threshold: 0 }, 1, limits()),
        Err(AdmissionError::JobLimit)
    ));
    let section_task = section.submit().unwrap();
    let packet_task = pending.submit().unwrap();
    let output = wait(packet_task).await.unwrap();
    let section = section_task.wait().unwrap();
    assert!(!section.bytes().unwrap().is_empty());
    assert_eq!(section.key().revision, 17);
    assert_eq!(decode(0, output.bytes(), limits()), packet);
    assert_reserved(&pool, 2, total_bytes);
    assert_eq!(pool.stats().completed_jobs, 2);
    drop(output);
    assert_reserved(&pool, 1, SECTION_JOB_BUFFER_BYTES);
    drop(section);
    assert_reserved(&pool, 0, 0);
    assert_eq!(pool.stats().peak_reserved_buffer_bytes, total_bytes);
}
