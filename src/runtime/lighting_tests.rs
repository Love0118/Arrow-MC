use super::*;
use crate::runtime::{CpuPoolConfig, TestGate};
use crate::world::{
    lighting::{block::BlockLightLimits, storage::StorageLimits},
    storage::chunk::DimensionHeight,
};
use std::{
    sync::{Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

#[path = "../../tests/common/lighting_fixture.rs"]
mod fixture;

fn source() -> LightingSource {
    fixture::from_placements(
        fixture::synthetic_registry(),
        DimensionHeight::new(0, 32).unwrap(),
        &[ChunkAddress { x: 0, z: 0 }],
        &[(LightBlock { x: 8, y: 0, z: 8 }, fixture::BEDROCK)],
    )
}
fn limits() -> LightingLimits {
    LightingLimits {
        max_chunks: 1,
        metadata_bytes: 8,
        block: BlockLightLimits {
            checks: 16,
            decreases: 16,
            increases: 16,
            queue_bytes: 8192,
        },
        block_storage: StorageLimits {
            max_sections: 64,
            max_columns: 16,
            max_notifications: 128,
            metadata_bytes: 1 << 20,
            layer_bytes: 1 << 20,
        },
        sky: None,
    }
}
fn pool(slots: usize) -> (CpuPool, usize) {
    let bytes = limits().reservation_bytes().unwrap() + source().heap_bytes();
    (
        CpuPool::new(CpuPoolConfig {
            workers: 1,
            max_jobs: slots,
            buffer_bytes: bytes * slots,
        })
        .unwrap(),
        bytes,
    )
}
struct Release(Arc<TestGate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
fn gate() -> (Arc<TestGate>, mpsc::Receiver<()>, Release) {
    let (started, receiver) = mpsc::sync_channel(1);
    let gate = Arc::new(TestGate {
        started,
        released: Mutex::new(false),
        changed: Condvar::new(),
    });
    (Arc::clone(&gate), receiver, Release(gate))
}
async fn released(pool: &CpuPool, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.stats().in_flight != count {
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn running_cancel_and_drop_hold_constructed_work_until_worker_releases() {
    let (pool, bytes) = pool(1);
    let (gate, started, _release) = gate();
    let mut task = pool
        .try_reserve_lighting(source(), limits())
        .unwrap()
        .submit_with_gate(1, Arc::clone(&gate))
        .unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(pool.stats().running, 1);
    task.cancel();
    drop(task);
    assert_eq!(pool.stats().reserved_buffer_bytes, bytes);
    assert_eq!(pool.stats().in_flight, 1);
    gate.release();
    released(&pool, 0).await;
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
    pool.shutdown().unwrap();
}

#[tokio::test]
async fn cancelled_borrowed_wait_can_be_reawaited_without_losing_work() {
    let (pool, bytes) = pool(1);
    let (gate, started, _release) = gate();
    let mut task = pool
        .try_reserve_lighting(source(), limits())
        .unwrap()
        .submit_with_gate(1, Arc::clone(&gate))
        .unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(1), task.wait_mut())
            .await
            .is_err()
    );
    assert_eq!(pool.stats().reserved_buffer_bytes, bytes);
    gate.release();
    let result = task.wait_mut().await.unwrap();
    assert!(result.progress().is_ok());
    assert_eq!(pool.stats().reserved_buffer_bytes, bytes);
    assert!(matches!(
        task.wait_mut().await,
        Err(LightingJobError::Cancelled)
    ));
    drop(result);
    assert_eq!(pool.stats().in_flight, 0);
    pool.shutdown().unwrap();
}

#[tokio::test]
async fn queued_cancel_keeps_admission_until_shared_worker_drains_it() {
    let (pool, bytes) = pool(2);
    let (gate, started, _release) = gate();
    let first = pool
        .try_reserve_lighting(source(), limits())
        .unwrap()
        .submit_with_gate(1, Arc::clone(&gate))
        .unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut queued = pool
        .try_reserve_lighting(source(), limits())
        .unwrap()
        .submit(1)
        .unwrap();
    assert_eq!(pool.stats().queued, 1);
    queued.cancel();
    drop(queued);
    assert_eq!(pool.stats().reserved_buffer_bytes, bytes * 2);
    gate.release();
    let held = first.wait().await.unwrap();
    released(&pool, 1).await;
    assert_eq!(pool.stats().reserved_buffer_bytes, bytes);
    drop(held);
    pool.shutdown().unwrap();
}

#[tokio::test]
async fn cancellation_after_actual_send_suppresses_a_ready_completion() {
    let (pool, bytes) = pool(1);
    let mut task = pool
        .try_reserve_lighting(source(), limits())
        .unwrap()
        .submit(1)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    // Receiving and putting back the exact object provides a deterministic ready
    // observation without changing the production API or guessing from job stats.
    let completion = loop {
        match task.receiver.as_mut().unwrap().try_recv() {
            Ok(completion) => break completion,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline);
                tokio::task::yield_now().await;
            }
            Err(error) => panic!("unexpected receiver state {error:?}"),
        }
    };
    let (sender, receiver) = oneshot::channel();
    assert!(sender.send(completion).is_ok());
    task.receiver = Some(receiver);
    assert_eq!(pool.stats().reserved_buffer_bytes, bytes);
    task.cancel();
    assert!(matches!(
        task.wait().await,
        Err(LightingJobError::Cancelled)
    ));
    assert_eq!(pool.stats().in_flight, 0);
    pool.shutdown().unwrap();
}
