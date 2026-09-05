#[path = "common/lighting_fixture.rs"]
mod fixture;

use arrow_mc::{
    runtime::{
        AdmissionError, CpuPool, CpuPoolConfig, LightingCompletion, LightingGrowth,
        LightingJobError, MAX_LIGHTING_SLICE_UNITS, SECTION_JOB_BUFFER_BYTES, SectionKey,
    },
    world::{
        lighting::{
            LightBlock, LightKind, LightingSource,
            block::BlockLightLimits,
            sky::SkyLimits,
            storage::StorageLimits,
            work::{LightingLimits, LightingWork, SkyWorkLimits},
        },
        preparation::ChunkAddress,
        section::{Registry, SectionCounts},
        storage::chunk::DimensionHeight,
    },
};
use std::time::{Duration, Instant};

fn source() -> LightingSource {
    fixture::from_placements(
        fixture::synthetic_registry(),
        DimensionHeight::new(0, 32).unwrap(),
        &[ChunkAddress { x: 0, z: 0 }],
        &[(LightBlock { x: 8, y: 0, z: 8 }, fixture::BEDROCK)],
    )
}
fn limits() -> LightingLimits {
    let storage = StorageLimits {
        max_sections: 64,
        max_columns: 16,
        max_notifications: 128,
        metadata_bytes: 1 << 20,
        layer_bytes: 1 << 20,
    };
    LightingLimits {
        max_chunks: 1,
        metadata_bytes: 8,
        block: BlockLightLimits {
            checks: 16,
            decreases: 32768,
            increases: 32768,
            queue_bytes: 2 << 20,
        },
        block_storage: storage,
        sky: Some(SkyWorkLimits {
            engine: SkyLimits {
                checks: 16,
                queue_entries: 32768,
                source_chunks: 1,
                planned_writes: 256,
            },
            storage,
            engine_bytes: 2 << 20,
        }),
    }
}
fn pool(slots: usize, bytes: usize) -> CpuPool {
    CpuPool::new(CpuPoolConfig {
        workers: 1,
        max_jobs: slots,
        buffer_bytes: bytes,
    })
    .unwrap()
}
fn reservation(limits: LightingLimits) -> usize {
    limits.reservation_bytes().unwrap() + source().heap_bytes()
}
async fn converge(
    mut result: LightingCompletion,
    pool: &CpuPool,
    bytes: usize,
) -> LightingCompletion {
    let mut slices = 0;
    while !result.progress().unwrap().complete {
        assert!(
            result
                .light_level(LightKind::Sky, LightBlock { x: 8, y: 1, z: 8 })
                .is_none()
        );
        let pending = result
            .into_pending()
            .unwrap_or_else(|_| panic!("pending work retained"));
        assert_eq!(pending.reserved_bytes(), bytes);
        assert_eq!(pool.stats().reserved_buffer_bytes, bytes);
        result = pending.submit(usize::MAX).unwrap().wait().await.unwrap();
        assert!(result.progress().unwrap().processed <= MAX_LIGHTING_SLICE_UNITS);
        slices += 1;
        assert!(slices < 1000);
    }
    assert!(slices > 1);
    result
}

#[tokio::test]
async fn paused_resubmitted_and_held_completion_share_one_admission_and_match_sync() {
    let limits = limits();
    let bytes = reservation(limits);
    let pool = pool(1, bytes);
    let pending = pool.try_reserve_lighting(source(), limits).unwrap();
    assert_eq!(pending.reserved_bytes(), bytes);
    let result = pending.submit(0).unwrap().wait().await.unwrap();
    assert_eq!(result.progress().unwrap().processed, 0);
    let completion = converge(result, &pool, bytes).await;
    let mut serial = LightingWork::new(source(), limits).unwrap();
    while !serial.step(64).unwrap().complete {}
    let serial = serial
        .into_completed()
        .unwrap_or_else(|_| panic!("complete"));
    for y in -16..48 {
        for x in [0, 8, 15, 16] {
            let pos = LightBlock { x, y, z: 8 };
            assert_eq!(
                completion.light_level(LightKind::Block, pos),
                Some(serial.block().get_level(pos))
            );
            assert_eq!(
                completion.light_level(LightKind::Sky, pos),
                Some(serial.sky().unwrap().get_level(pos))
            );
        }
    }
    assert!(completion.into_pending().is_err_and(|held| {
        assert_eq!(pool.stats().in_flight, 1);
        assert!(matches!(
            pool.try_reserve_lighting(source(), limits),
            Err(AdmissionError::JobLimit)
        ));
        drop(held);
        true
    }));
    assert_eq!(pool.stats().in_flight, 0);
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
    pool.shutdown().unwrap();
}

#[test]
fn byte_admission_precedes_construction_and_pending_drop_releases_everything() {
    let limits = limits();
    let bytes = reservation(limits);
    let pool = pool(2, bytes);
    let pending = pool.try_reserve_lighting(source(), limits).unwrap();
    assert!(matches!(
        pool.try_reserve_lighting(source(), limits),
        Err(AdmissionError::ByteLimit)
    ));
    assert_eq!(pool.stats().queued, 0);
    assert_eq!(pool.stats().completed_jobs, 0);
    drop(pending);
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
    let pending = pool.try_reserve_lighting(source(), limits).unwrap();
    pool.close();
    assert!(matches!(pending.submit(64), Err(AdmissionError::Closed)));
    assert_eq!(pool.stats().in_flight, 0);
    pool.shutdown().unwrap();
}

#[tokio::test]
async fn blocked_work_and_constructor_errors_keep_their_lease_until_drop() {
    let mut limits = limits();
    limits.sky.as_mut().unwrap().engine.queue_entries = 1;
    let bytes = reservation(limits);
    let pool = pool(1, bytes);
    let mut result = pool
        .try_reserve_lighting(source(), limits)
        .unwrap()
        .submit(64)
        .unwrap()
        .wait()
        .await
        .unwrap();
    while result.progress().is_ok_and(|p| !p.complete) {
        result = result
            .into_pending()
            .unwrap_or_else(|_| panic!("paused"))
            .submit(64)
            .unwrap()
            .wait()
            .await
            .unwrap();
    }
    assert!(matches!(
        result.progress(),
        Err(LightingJobError::Lighting(_))
    ));
    assert!(
        result
            .light_level(LightKind::Sky, LightBlock { x: 8, y: 1, z: 8 })
            .is_none()
    );
    assert_eq!(pool.stats().reserved_buffer_bytes, bytes);
    let again = result
        .into_pending()
        .unwrap_or_else(|_| panic!("blocked work retained"))
        .submit(64)
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(again.progress().is_err());
    assert_eq!(pool.stats().in_flight, 1);
    let mut pending = again
        .into_pending()
        .unwrap_or_else(|_| panic!("blocked work"));
    pending.request_growth(LightingGrowth {
        sky_queue: Some(32768),
        ..Default::default()
    });
    let grown = pending.submit(64).unwrap().wait().await.unwrap();
    let complete = converge(grown, &pool, bytes).await;
    assert_eq!(
        complete.light_level(LightKind::Sky, LightBlock { x: 8, y: 1, z: 8 }),
        Some(15)
    );
    drop(complete);
    assert_eq!(pool.stats().in_flight, 0);
    limits.block.queue_bytes = 0;
    let failed_bytes = reservation(limits);
    let result = pool
        .try_reserve_lighting(source(), limits)
        .unwrap()
        .submit(64)
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(result.progress().is_err());
    assert_eq!(pool.stats().reserved_buffer_bytes, failed_bytes);
    let terminal = result
        .into_pending()
        .err()
        .expect("construction failure has no work to resume");
    drop(terminal);
    assert_eq!(pool.stats().in_flight, 0);
    pool.shutdown().unwrap();
}

#[tokio::test]
async fn a_paused_lighting_job_does_not_hold_the_cpu_worker_from_section_work() {
    let limits = limits();
    let bytes = reservation(limits);
    let pool = pool(2, bytes + SECTION_JOB_BUFFER_BYTES);
    let result = pool
        .try_reserve_lighting(source(), limits)
        .unwrap()
        .submit(1)
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(!result.progress().unwrap().complete);
    let section = pool
        .try_reserve_section(
            SectionKey {
                world_epoch: 1,
                chunk_x: 0,
                chunk_z: 0,
                section_y: 0,
                revision: 1,
            },
            Registry::new(16).unwrap(),
            Registry::new(4).unwrap(),
            SectionCounts {
                non_empty_blocks: 0,
                fluid_blocks: 0,
            },
        )
        .unwrap()
        .submit()
        .unwrap();
    let completion = section.wait().unwrap();
    assert!(completion.bytes().is_ok());
    assert_eq!(pool.stats().peak_running, 1);
    assert_eq!(pool.stats().in_flight, 2);
    drop(completion);
    assert_eq!(pool.stats().reserved_buffer_bytes, bytes);
    drop(result);
    pool.shutdown().unwrap();
}

#[tokio::test]
async fn cancellation_at_completion_suppresses_payload_until_worker_release() {
    let limits = limits();
    let bytes = reservation(limits);
    let pool = pool(1, bytes);
    let mut task = pool
        .try_reserve_lighting(source(), limits)
        .unwrap()
        .submit(1)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.stats().completed_jobs == 0 {
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    assert_eq!(pool.stats().in_flight, 1);
    task.cancel();
    assert!(matches!(
        task.wait().await,
        Err(LightingJobError::Cancelled)
    ));
    // completed_jobs is updated before send. Cancellation may race that final
    // publication, so release is observed rather than assumed to be immediate.
    while pool.stats().in_flight != 0 {
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
    pool.shutdown().unwrap();
}
