use super::*;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct ReleaseOnDrop(Arc<TestGate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn gate() -> (Arc<TestGate>, Receiver<()>, ReleaseOnDrop) {
    let (started, receiver) = mpsc::sync_channel(1);
    let gate = Arc::new(TestGate {
        started,
        released: Mutex::new(false),
        changed: Condvar::new(),
    });
    (Arc::clone(&gate), receiver, ReleaseOnDrop(gate))
}

fn pool(workers: usize, slots: usize) -> CpuPool {
    CpuPool::new(CpuPoolConfig {
        workers,
        max_jobs: slots,
        buffer_bytes: slots * SECTION_JOB_BUFFER_BYTES,
    })
    .unwrap()
}

fn key(revision: u64) -> SectionKey {
    SectionKey {
        world_epoch: 9,
        chunk_x: -3,
        chunk_z: 4,
        section_y: -4,
        revision,
    }
}

fn reserve(pool: &CpuPool, revision: u64) -> PendingSection {
    let mut job = pool
        .try_reserve_section(
            key(revision),
            Registry::new(16).unwrap(),
            Registry::new(4).unwrap(),
            SectionCounts {
                non_empty_blocks: 4096,
                fluid_blocks: 0,
            },
        )
        .unwrap();
    job.blocks_mut().fill(1);
    job.biomes_mut().fill(2);
    job
}

fn await_condition(check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "worker progress timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn later_completion_does_not_overtake_owner_order_or_escape_byte_accounting() {
    let pool = pool(2, 2);
    let (gate, started, _release) = gate();
    let first = reserve(&pool, 1).enqueue(Some(Arc::clone(&gate))).unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = reserve(&pool, 2).submit().unwrap();
    await_condition(|| second.is_finished());
    assert!(!first.is_finished());
    assert_eq!(pool.stats().peak_running, 2);
    assert_eq!(pool.stats().in_flight, 2);
    assert_eq!(
        pool.stats().reserved_buffer_bytes,
        2 * SECTION_JOB_BUFFER_BYTES
    );

    // A world owner would check these revisions and consume in its own order.
    // Waiting for first does not block second's worker publication.
    gate.release();
    let first = first.wait().unwrap();
    let second = second.wait().unwrap();
    assert_eq!([first.key().revision, second.key().revision], [1, 2]);
    assert_eq!(first.bytes().unwrap(), &[16, 0, 0, 0, 0, 1, 0, 2]);
    assert_eq!(first.bytes().unwrap(), second.bytes().unwrap());
    assert_eq!(pool.stats().in_flight, 2);
    drop(first);
    assert_eq!(pool.stats().in_flight, 1);
    drop(second);
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
}

#[test]
fn cancelling_and_dropping_running_receiver_retains_permit_until_worker_frees_buffers() {
    let pool = pool(1, 1);
    let (gate, started, _release) = gate();
    let task = reserve(&pool, 1).enqueue(Some(Arc::clone(&gate))).unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    task.cancel();
    drop(task);
    assert_eq!(pool.stats().running, 1);
    assert_eq!(pool.stats().reserved_buffer_bytes, SECTION_JOB_BUFFER_BYTES);
    assert!(matches!(
        pool.try_reserve_section(
            key(2),
            Registry::new(1).unwrap(),
            Registry::new(1).unwrap(),
            SectionCounts {
                non_empty_blocks: 0,
                fluid_blocks: 0
            },
        ),
        Err(AdmissionError::JobLimit)
    ));
    gate.release();
    await_condition(|| pool.stats().in_flight == 0);
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
    assert_eq!(pool.stats().completed_jobs, 1);
}

#[test]
fn queued_cancellation_and_shutdown_progress_with_one_worker_and_retained_handles() {
    let pool = pool(1, 2);
    let (gate, started, _release) = gate();
    let first = reserve(&pool, 1).enqueue(Some(Arc::clone(&gate))).unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = reserve(&pool, 2).submit().unwrap();
    second.cancel();
    assert_eq!(pool.stats().queued, 1);
    assert_eq!(
        pool.stats().reserved_buffer_bytes,
        2 * SECTION_JOB_BUFFER_BYTES
    );
    pool.close();
    gate.release();
    pool.shutdown().unwrap();
    assert!(first.wait().unwrap().bytes().is_ok());
    assert!(matches!(
        second.wait().unwrap().bytes(),
        Err(SectionJobError::Cancelled)
    ));
}

#[test]
fn cancel_after_publication_suppresses_ready_payload_and_completion_slot_is_retained() {
    let pool = pool(1, 1);
    let task = reserve(&pool, 1).submit().unwrap();
    await_condition(|| task.is_finished());
    task.cancel();
    let completion = task.wait().unwrap();
    assert!(matches!(
        completion.bytes(),
        Err(SectionJobError::Cancelled)
    ));
    assert_eq!(pool.stats().in_flight, 1);
    drop(completion);
    assert_eq!(pool.stats().in_flight, 0);
}

#[test]
fn dropping_ready_receiver_releases_completed_slot_without_another_submission() {
    let pool = pool(1, 1);
    let task = reserve(&pool, 1).submit().unwrap();
    await_condition(|| task.is_finished());
    drop(task);
    await_condition(|| pool.stats().in_flight == 0);
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
}
