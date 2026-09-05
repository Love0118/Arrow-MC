use super::*;
use crate::{
    nbt::{self, Compound, NamedTag, Tag},
    runtime::{CpuPoolConfig, TestGate},
    world::storage::region::StreamVersion,
};
use std::{
    fs,
    sync::{Condvar, Mutex, mpsc},
    time::{Duration, SystemTime},
};

struct TempDirectory(PathBuf);
impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "arrow-storage-gate-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDirectory {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        fs::remove_dir_all(&self.0).unwrap();
    }
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

fn bytes() -> Vec<u8> {
    let mut root = Compound::new();
    root.insert("DataVersion".into(), Tag::Int(5018)).unwrap();
    root.insert("Status".into(), Tag::String("minecraft:full".into()))
        .unwrap();
    let mut result = Vec::new();
    nbt::write_named(
        &NamedTag {
            name: "".into(),
            tag: Tag::Compound(root),
        },
        &mut result,
        nbt::Limits::default(),
    )
    .unwrap();
    result
}
fn limits() -> StorageLimits {
    StorageLimits {
        compressed_bytes: 4096,
        inflated_bytes: 16 * 1024,
        nbt_limits: nbt::Limits {
            allocation_bytes: 128 * 1024,
            ..nbt::Limits::default()
        },
        decoded_bytes: 16 * 1024,
    }
}
fn key() -> ChunkReadKey {
    ChunkReadKey {
        world_epoch: 3,
        chunk_x: 0,
        chunk_z: 0,
        generation: 9,
    }
}
fn pool() -> Arc<CpuPool> {
    Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers: 1,
            max_jobs: 2,
            buffer_bytes: 1024 * 1024,
        })
        .unwrap(),
    )
}
async fn until(check: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !check() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_disk_wait_retains_io_slot_and_cpu_bytes_until_actual_read_returns() {
    let directory = TempDirectory::new();
    let payload = bytes();
    let mut file = vec![0; 8192];
    file[..4].copy_from_slice(&513_u32.to_be_bytes());
    file.extend_from_slice(&((payload.len() + 1) as i32).to_be_bytes());
    file.push(3);
    file.extend_from_slice(&payload);
    fs::write(directory.0.join("r.0.0.mca"), file).unwrap();
    let cpu = pool();
    let (gate, started, _release) = gate();
    let mut store = ChunkStore::new(
        directory.0.clone(),
        Arc::clone(&cpu),
        Arc::new(registry::storage_test_snapshot()),
        DimensionHeight::new(-64, 384).unwrap(),
        limits(),
        1,
    )
    .unwrap();
    store.io_gate = Some(Arc::clone(&gate));
    let store = Arc::new(store);
    let reader = tokio::spawn({
        let store = Arc::clone(&store);
        async move { store.read(key()).await }
    });
    tokio::task::yield_now().await;
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        cpu.stats().running,
        0,
        "disk wait did not occupy a CPU worker"
    );
    assert_eq!(cpu.stats().in_flight, 1);
    assert_eq!(store.io_slots.available_permits(), 0);
    let reserved = limits()
        .job_bytes_for(StreamVersion::Raw, payload.len())
        .unwrap();
    assert_eq!(cpu.stats().reserved_buffer_bytes, reserved);
    reader.abort();
    assert!(reader.await.is_err());
    assert_eq!(cpu.stats().reserved_buffer_bytes, reserved);
    assert_eq!(store.io_slots.available_permits(), 0);
    assert!(matches!(
        store.read(key()).await,
        Err(ChunkLoadError::IoBusy)
    ));
    gate.release();
    until(|| cpu.stats().in_flight == 0 && store.io_slots.available_permits() == 1).await;
    assert_eq!(cpu.stats().reserved_buffer_bytes, 0);
    assert_eq!(cpu.stats().completed_jobs, 0);
    drop(store);
    Arc::try_unwrap(cpu).ok().unwrap().shutdown().unwrap();
}

#[tokio::test]
async fn running_decode_cancellation_and_cancelled_ready_result_keep_correct_ownership() {
    let cpu = pool();
    let payload = bytes();
    let registries = Arc::new(registry::storage_test_snapshot());
    let reserve = || {
        let mut pending = cpu
            .try_reserve_chunk_decode(
                key(),
                StreamVersion::Raw,
                payload.len(),
                Arc::clone(&registries),
                DimensionHeight::new(-64, 384).unwrap(),
                limits(),
            )
            .unwrap();
        pending.compressed_mut().copy_from_slice(&payload);
        pending
    };
    let (gate, started, _release) = gate();
    let task = reserve().submit_with_gate(Arc::clone(&gate)).unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(task);
    assert_eq!(cpu.stats().running, 1);
    assert_eq!(cpu.stats().in_flight, 1);
    gate.release();
    until(|| cpu.stats().in_flight == 0).await;
    // Stop exactly after finish_job but before sender.send: cancellation wakes
    // the receiver while the worker still owns the constructed result's lease.
    let (publication_gate, publication_started, _publication_release) = self::gate();
    let mut racing = reserve()
        .submit_with_publication_gate(Arc::clone(&publication_gate))
        .unwrap();
    publication_started
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(cpu.stats().completed_jobs, 2);
    assert_eq!(cpu.stats().in_flight, 1);
    let reserved = limits()
        .job_bytes_for(StreamVersion::Raw, payload.len())
        .unwrap();
    assert_eq!(cpu.stats().reserved_buffer_bytes, reserved);
    racing.cancel();
    assert!(matches!(
        racing.wait().await,
        Err(ChunkLoadError::Cancelled)
    ));
    assert_eq!(cpu.stats().in_flight, 1);
    assert_eq!(cpu.stats().reserved_buffer_bytes, reserved);
    publication_gate.release();
    until(|| cpu.stats().in_flight == 0).await;
    assert_eq!(cpu.stats().reserved_buffer_bytes, 0);

    // Separately establish actual delivery before checking cancellation of an
    // already-ready result. Cancelling wait must drop its retained lease now.
    let mut ready = reserve().submit().unwrap();
    cpu.close();
    until(|| ready.has_delivered_result_for_test()).await;
    assert_eq!(cpu.stats().completed_jobs, 3);
    assert_eq!(cpu.stats().in_flight, 1);
    ready.cancel();
    assert!(matches!(ready.wait().await, Err(ChunkLoadError::Cancelled)));
    assert_eq!(cpu.stats().in_flight, 0);
    assert_eq!(cpu.stats().reserved_buffer_bytes, 0);
    Arc::try_unwrap(cpu).ok().unwrap().shutdown().unwrap();
}
