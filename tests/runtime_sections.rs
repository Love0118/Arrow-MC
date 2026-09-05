use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use arrow_mc::runtime::{
    AdmissionError, CpuPool, CpuPoolConfig, PendingSection, SECTION_JOB_BUFFER_BYTES,
    SectionJobError, SectionKey,
};
use arrow_mc::world::section::{self, Registry, SectionCounts};

fn pool(workers: usize, max_jobs: usize, buffer_bytes: usize) -> CpuPool {
    CpuPool::new(CpuPoolConfig {
        workers,
        max_jobs,
        buffer_bytes,
    })
    .unwrap()
}

fn key(index: u32) -> SectionKey {
    SectionKey {
        world_epoch: 7 + u64::from(index % 2),
        chunk_x: index as i32 - 12,
        chunk_z: 19 - index as i32,
        section_y: index as i32 - 4,
        revision: 100 + u64::from(index),
    }
}

fn counts(index: u32) -> SectionCounts {
    SectionCounts {
        non_empty_blocks: 4096 - index as u16,
        fluid_blocks: index as u16,
    }
}

fn reserve(pool: &CpuPool, index: u32) -> Result<PendingSection, AdmissionError> {
    pool.try_reserve_section(
        key(index),
        Registry::new(8192).unwrap(),
        Registry::new(128).unwrap(),
        counts(index),
    )
}

fn dense(index: u32) -> ([u32; 4096], [u32; 64]) {
    let block_values = [1, 2, 16, 17, 256, 257, 513, 4096][index as usize % 8];
    let biome_values = [1, 2, 8, 9, 16, 32, 63, 64][index as usize % 8];
    let blocks = std::array::from_fn(|position| {
        (position as u32 * 37 + index * 13) % block_values + index * 64
    });
    let biomes =
        std::array::from_fn(|position| (position as u32 * 11 + index * 7) % biome_values + index);
    (blocks, biomes)
}

fn assert_reserved(pool: &CpuPool, jobs: usize) {
    let stats = pool.stats();
    assert_eq!(stats.in_flight, jobs);
    assert_eq!(stats.reserved_buffer_bytes, jobs * SECTION_JOB_BUFFER_BYTES);
}

#[test]
fn one_two_and_four_workers_match_serial_section_bytes_by_key() {
    for workers in [1, 2, 4] {
        let pool = pool(workers, 8, 8 * SECTION_JOB_BUFFER_BYTES);
        let mut expected = Vec::new();
        let mut tasks = Vec::new();
        for index in 0..8 {
            let (blocks, biomes) = dense(index);
            let mut serial = Vec::with_capacity(section::MAX_SECTION_NETWORK_BYTES);
            section::prepare_section(
                &blocks,
                &biomes,
                Registry::new(8192).unwrap(),
                Registry::new(128).unwrap(),
                counts(index),
                &mut serial,
            )
            .unwrap();
            expected.push((key(index), serial));

            let mut pending = reserve(&pool, index).unwrap();
            pending.blocks_mut().copy_from_slice(&blocks);
            pending.biomes_mut().copy_from_slice(&biomes);
            let task = pending.submit().unwrap();
            assert_eq!(task.key(), key(index));
            tasks.push(task);
        }

        // An owner can receive in its own order without blocking other results.
        let mut completions = Vec::new();
        for task in tasks.into_iter().rev() {
            let completion = task.wait().unwrap();
            let (_, bytes) = expected
                .iter()
                .find(|(key, _)| *key == completion.key())
                .unwrap();
            assert_eq!(completion.bytes().unwrap(), bytes, "workers={workers}");
            completions.push(completion);
        }

        assert_reserved(&pool, 8);
        let stats = pool.stats();
        assert_eq!(stats.completed_jobs, 8);
        assert_eq!((stats.queued, stats.running), (0, 0));
        assert!((1..=workers).contains(&stats.peak_running));
        assert_eq!(
            stats.peak_reserved_buffer_bytes,
            8 * SECTION_JOB_BUFFER_BYTES
        );
        drop(completions);
        assert_reserved(&pool, 0);
        pool.shutdown().unwrap();
    }
}

#[test]
fn retained_output_keeps_its_slot_and_full_byte_reservation() {
    let pool = pool(1, 1, SECTION_JOB_BUFFER_BYTES);
    let completion = reserve(&pool, 0).unwrap().submit().unwrap().wait().unwrap();
    assert!(!completion.bytes().unwrap().is_empty());
    assert_reserved(&pool, 1);
    assert!(matches!(reserve(&pool, 1), Err(AdmissionError::JobLimit)));

    drop(completion);
    assert_reserved(&pool, 0);
    let replacement = reserve(&pool, 1).unwrap();
    assert_reserved(&pool, 1);
    drop(replacement);
    assert_reserved(&pool, 0);
}

#[test]
fn byte_pressure_and_slot_pressure_reject_without_an_extra_reservation() {
    for (max_jobs, bytes, error) in [
        (2, SECTION_JOB_BUFFER_BYTES, AdmissionError::ByteLimit),
        (
            2,
            2 * SECTION_JOB_BUFFER_BYTES - 1,
            AdmissionError::ByteLimit,
        ),
        (1, 2 * SECTION_JOB_BUFFER_BYTES, AdmissionError::JobLimit),
    ] {
        let pool = pool(1, max_jobs, bytes);
        let pending = reserve(&pool, 0).unwrap();
        assert_reserved(&pool, 1);
        assert!(matches!(reserve(&pool, 1), Err(actual) if actual == error));
        assert_reserved(&pool, 1);
        assert_eq!(
            pool.stats().peak_reserved_buffer_bytes,
            SECTION_JOB_BUFFER_BYTES
        );
        drop(pending);
        assert_reserved(&pool, 0);
        drop(reserve(&pool, 1).unwrap());
        assert_reserved(&pool, 0);
    }
}

#[test]
fn dropping_an_unsubmitted_section_releases_bytes_and_slot_without_work() {
    let pool = pool(1, 2, 2 * SECTION_JOB_BUFFER_BYTES);
    let mut pending = reserve(&pool, 0).unwrap();
    pending.blocks_mut()[4095] = 8191;
    pending.biomes_mut()[63] = 127;
    assert_reserved(&pool, 1);
    assert_eq!((pool.stats().queued, pool.stats().running), (0, 0));

    drop(pending);
    assert_reserved(&pool, 0);
    assert_eq!(pool.stats().completed_jobs, 0);
    drop(reserve(&pool, 1).unwrap());
    assert_reserved(&pool, 0);
}

#[test]
fn malformed_block_and_biome_ids_return_errors_and_release_on_completion_drop() {
    let pool = pool(1, 1, SECTION_JOB_BUFFER_BYTES);
    for (index, invalid_id) in [(0, 8192), (1, 128)] {
        let mut pending = reserve(&pool, index).unwrap();
        if index == 0 {
            pending.blocks_mut()[4095] = invalid_id;
        } else {
            pending.biomes_mut()[63] = invalid_id;
        }
        let completion = pending.submit().unwrap().wait().unwrap();
        assert_eq!(completion.key(), key(index));
        assert!(matches!(
            completion.bytes(),
            Err(SectionJobError::Prepare(section::Error::ValueOutOfRange(id)))
                if *id == invalid_id
        ));
        assert_reserved(&pool, 1);
        assert!(matches!(reserve(&pool, 2), Err(AdmissionError::JobLimit)));
        drop(completion);
        assert_reserved(&pool, 0);
    }

    let completion = reserve(&pool, 2).unwrap().submit().unwrap().wait().unwrap();
    assert!(completion.bytes().is_ok());
    drop(completion);
    assert_reserved(&pool, 0);
    assert_eq!(pool.stats().completed_jobs, 3);
}

#[test]
fn close_rejects_pending_submission_and_drains_with_retained_results() {
    let pool = pool(2, 3, 3 * SECTION_JOB_BUFFER_BYTES);
    let retained = reserve(&pool, 0).unwrap().submit().unwrap().wait().unwrap();
    let unreceived = reserve(&pool, 1).unwrap().submit().unwrap();
    let pending = reserve(&pool, 2).unwrap();
    pool.close();
    pool.close();
    assert!(matches!(reserve(&pool, 3), Err(AdmissionError::Closed)));
    assert!(matches!(pending.submit(), Err(AdmissionError::Closed)));
    assert_reserved(&pool, 2);

    let (finished, receiver) = mpsc::channel();
    let shutdown = thread::spawn(move || {
        pool.shutdown().unwrap();
        finished.send(()).unwrap();
    });
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("shutdown waited for the owner to drop or receive a result");
    shutdown.join().unwrap();

    assert_eq!(retained.key(), key(0));
    assert!(retained.bytes().is_ok());
    let completion = unreceived.wait().unwrap();
    assert_eq!(completion.key(), key(1));
    assert!(completion.bytes().is_ok());
}

#[test]
fn completion_preserves_stale_revision_and_epoch_for_owner_validation() {
    let pool = pool(1, 2, 2 * SECTION_JOB_BUFFER_BYTES);
    let current = key(0);
    let old_keys = [
        SectionKey {
            revision: current.revision - 1,
            ..current
        },
        SectionKey {
            world_epoch: current.world_epoch - 1,
            ..current
        },
    ];
    let tasks: Vec<_> = old_keys
        .into_iter()
        .map(|old_key| {
            let pending = pool
                .try_reserve_section(
                    old_key,
                    Registry::new(8192).unwrap(),
                    Registry::new(128).unwrap(),
                    counts(0),
                )
                .unwrap();
            let task = pending.submit().unwrap();
            assert_eq!(task.key(), old_key);
            task
        })
        .collect();

    for (task, old_key) in tasks.into_iter().zip(old_keys) {
        let completion = task.wait().unwrap();
        assert_eq!(completion.key(), old_key);
        assert_ne!(completion.key(), current);
        assert!(completion.bytes().is_ok());
    }
    assert_reserved(&pool, 0);
}

#[test]
fn dropped_receivers_release_capacity_and_do_not_block_other_jobs_or_shutdown() {
    let (finished, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let pool = pool(1, 17, 17 * SECTION_JOB_BUFFER_BYTES);
        for index in 0..16 {
            drop(reserve(&pool, index).unwrap().submit().unwrap());
        }
        // With one FIFO worker, this completion follows all abandoned jobs.
        let marker = reserve(&pool, 16)
            .unwrap()
            .submit()
            .unwrap()
            .wait()
            .unwrap();
        assert!(marker.bytes().is_ok());
        assert_reserved(&pool, 1);
        assert_eq!(pool.stats().completed_jobs, 17);
        drop(marker);
        assert_reserved(&pool, 0);
        drop(reserve(&pool, 17).unwrap());
        pool.shutdown().unwrap();
        finished.send(()).unwrap();
    });
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("dropping result receivers blocked worker progress or shutdown");
    worker.join().unwrap();
}
