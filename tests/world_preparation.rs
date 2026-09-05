use std::thread;
use std::time::{Duration, Instant};

use arrow_mc::runtime::{AdmissionError, CpuPool, CpuPoolConfig, SECTION_JOB_BUFFER_BYTES};
use arrow_mc::world::preparation::{
    ChunkAddress, DriveReport, Error, PreparationLimits, SectionAddress, SectionPreparationOwner,
};
use arrow_mc::world::section::{
    ContainerKind, Error as SectionError, PalettedContainer, Registry, Section, SectionCounts,
};

const COUNTS: SectionCounts = SectionCounts {
    non_empty_blocks: 4096,
    fluid_blocks: 3,
};

fn address(x: i32, y: i32) -> SectionAddress {
    SectionAddress {
        chunk: ChunkAddress { x, z: -x },
        y,
    }
}

fn limits() -> PreparationLimits {
    PreparationLimits {
        max_chunks: 4,
        max_sections: 8,
        max_pending: 4,
        max_cached: 4,
        source_heap_bytes: 128 * 1024,
    }
}

fn owner(limits: PreparationLimits) -> SectionPreparationOwner {
    SectionPreparationOwner::new(
        17,
        Registry::new(8192).unwrap(),
        Registry::new(128).unwrap(),
        limits,
    )
    .unwrap()
}

fn pool(max_jobs: usize, byte_jobs: usize) -> CpuPool {
    CpuPool::new(CpuPoolConfig {
        workers: 2,
        max_jobs,
        buffer_bytes: byte_jobs * SECTION_JOB_BUFFER_BYTES,
    })
    .unwrap()
}

fn load(owner: &mut SectionPreparationOwner, address: SectionAddress, value: u32) {
    if owner.chunk_generation(address.chunk).is_none() {
        owner.load_chunk(address.chunk).unwrap();
    }
    owner
        .load_section(address, &[value; 4096], &[0; 64], COUNTS)
        .unwrap();
}

fn drive_until(
    owner: &mut SectionPreparationOwner,
    pool: &CpuPool,
    done: impl Fn(&SectionPreparationOwner) -> bool,
) -> DriveReport {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut total = DriveReport::default();
    loop {
        let report = owner.drive(pool).unwrap();
        total.submitted += report.submitted;
        total.published += report.published;
        total.discarded += report.discarded;
        total.failed += report.failed;
        total.evicted += report.evicted;
        total.backpressure = report.backpressure.or(total.backpressure);
        assert_eq!(
            report.failed, 0,
            "unexpected preparation failure: {report:?}"
        );
        if done(owner) {
            return total;
        }
        assert!(
            Instant::now() < deadline,
            "owner stalled: {:?}; pool: {:?}",
            owner.stats(),
            pool.stats()
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_cached(
    owner: &mut SectionPreparationOwner,
    pool: &CpuPool,
    address: SectionAddress,
) -> DriveReport {
    drive_until(owner, pool, |owner| {
        owner.cached(address).is_some() && owner.stats().pending == 0 && owner.stats().dirty == 0
    })
}

fn decode(bytes: &[u8], block_registry: Registry, biome_registry: Registry) -> Section {
    let mut cursor = bytes;
    let section =
        Section::read_network(&mut cursor, block_registry, biome_registry, 64 * 1024).unwrap();
    assert!(cursor.is_empty());
    section
}

fn cached_section(owner: &SectionPreparationOwner, address: SectionAddress) -> Section {
    let (key, bytes) = owner.cached(address).unwrap();
    assert_eq!(Some(key), owner.current_key(address));
    decode(
        bytes,
        Registry::new(8192).unwrap(),
        Registry::new(128).unwrap(),
    )
}

#[test]
fn source_block_and_biome_mutations_automatically_prepare_latest_bytes() {
    let mut owner = owner(limits());
    let pool = pool(4, 4);
    let address = address(-3, -4);
    load(&mut owner, address, 0);
    let initial_key = owner.request(address).unwrap();
    wait_cached(&mut owner, &pool, address);
    assert_eq!(owner.stats().source_heap_bytes, 0);

    let changed_counts = SectionCounts {
        non_empty_blocks: 1,
        fluid_blocks: 1,
    };
    assert!(
        owner
            .set_block(address, 4095, 8191, changed_counts)
            .unwrap()
    );
    assert!(owner.set_biome(address, 63, 127).unwrap());
    let latest_key = owner.current_key(address).unwrap();
    assert!(latest_key.revision > initial_key.revision);
    assert!(owner.cached(address).is_none());
    assert_eq!(owner.stats().dirty, 1);
    let source = owner.section(address).unwrap();
    assert_eq!(source.blocks.bits(), 4);
    assert_eq!(source.biomes.bits(), 1);
    assert_eq!(source.blocks.get(4095), Ok(8191));
    assert_eq!(source.biomes.get(63), Ok(127));
    assert_eq!(
        owner.stats().source_heap_bytes,
        source.blocks.heap_bytes() + source.biomes.heap_bytes()
    );

    let report = wait_cached(&mut owner, &pool, address);
    assert_eq!((report.submitted, report.published), (1, 1));
    let prepared = cached_section(&owner, address);
    assert_eq!(prepared.counts, changed_counts);
    for index in 0..4096 {
        assert_eq!(
            prepared.blocks.get(index),
            Ok(if index == 4095 { 8191 } else { 0 })
        );
    }
    for index in 0..64 {
        assert_eq!(
            prepared.biomes.get(index),
            Ok(if index == 63 { 127 } else { 0 })
        );
    }
    assert_eq!(owner.cached(address).unwrap().0, latest_key);
}

#[test]
fn counts_only_changes_reprepare_and_true_noops_preserve_the_cache_lease() {
    let mut owner = owner(limits());
    let pool = pool(1, 1);
    let address = address(2, 3);
    load(&mut owner, address, 5);
    owner.request(address).unwrap();
    wait_cached(&mut owner, &pool, address);
    let before = owner.current_key(address).unwrap();
    let changed = SectionCounts {
        non_empty_blocks: 2500,
        fluid_blocks: 11,
    };
    assert!(owner.set_counts(address, changed).unwrap());
    wait_cached(&mut owner, &pool, address);
    assert_eq!(cached_section(&owner, address).counts, changed);
    assert!(owner.current_key(address).unwrap().revision > before.revision);

    let metadata_only = SectionCounts {
        non_empty_blocks: 2499,
        fluid_blocks: 10,
    };
    assert!(owner.set_block(address, 0, 5, metadata_only).unwrap());
    wait_cached(&mut owner, &pool, address);
    let key = owner.current_key(address).unwrap();
    let cache_pointer = owner.cached(address).unwrap().1.as_ptr();
    let stats = owner.stats();
    let completed = pool.stats().completed_jobs;
    assert!(!owner.set_block(address, 0, 5, metadata_only).unwrap());
    assert!(!owner.set_biome(address, 0, 0).unwrap());
    assert!(!owner.set_counts(address, metadata_only).unwrap());
    assert_eq!(owner.request(address), Ok(key));
    assert_eq!(owner.drive(&pool).unwrap(), DriveReport::default());
    assert_eq!(owner.current_key(address), Some(key));
    assert_eq!(owner.cached(address).unwrap().1.as_ptr(), cache_pointer);
    assert_eq!(owner.stats(), stats);
    assert_eq!(pool.stats().completed_jobs, completed);
    assert_eq!(pool.stats().in_flight, 1);
    assert_eq!(pool.stats().reserved_buffer_bytes, SECTION_JOB_BUFFER_BYTES);
    let prepared = cached_section(&owner, address);
    assert_eq!(prepared.counts, metadata_only);
    assert_eq!(prepared.blocks.get(0), Ok(5));
}

#[test]
fn invalid_mutations_leave_source_counts_revision_and_cached_bytes_unchanged() {
    let mut owner = owner(limits());
    let pool = pool(1, 1);
    let address = address(0, 0);
    load(&mut owner, address, 0);
    owner.request(address).unwrap();
    wait_cached(&mut owner, &pool, address);
    let key = owner.current_key(address);
    let stats = owner.stats();
    let pointer = owner.cached(address).unwrap().1.as_ptr();
    let bytes = owner.cached(address).unwrap().1.to_vec();
    for invalid in [
        SectionCounts {
            non_empty_blocks: 4097,
            fluid_blocks: 0,
        },
        SectionCounts {
            non_empty_blocks: 1,
            fluid_blocks: 2,
        },
    ] {
        assert_eq!(
            owner.set_block(address, 0, 1, invalid),
            Err(Error::Section(SectionError::InvalidCounts))
        );
        assert_eq!(
            owner.set_counts(address, invalid),
            Err(Error::Section(SectionError::InvalidCounts))
        );
    }
    assert_eq!(
        owner.set_block(address, 0, 8192, COUNTS),
        Err(Error::Section(SectionError::ValueOutOfRange(8192)))
    );
    assert_eq!(
        owner.set_biome(address, 0, 128),
        Err(Error::Section(SectionError::ValueOutOfRange(128)))
    );
    assert_eq!(
        owner.set_block(address, 4096, 1, COUNTS),
        Err(Error::Section(SectionError::IndexOutOfBounds))
    );
    assert_eq!(
        owner.set_biome(address, 64, 1),
        Err(Error::Section(SectionError::IndexOutOfBounds))
    );
    assert_eq!(owner.current_key(address), key);
    assert_eq!(owner.stats(), stats);
    assert_eq!(owner.cached(address).unwrap().1.as_ptr(), pointer);
    assert_eq!(owner.cached(address).unwrap().1, bytes);
    let source = owner.section(address).unwrap();
    assert_eq!(source.counts, COUNTS);
    assert_eq!(source.blocks.get(0), Ok(0));
    assert_eq!(source.biomes.get(0), Ok(0));
    assert_eq!(owner.drive(&pool).unwrap(), DriveReport::default());
}

#[test]
fn aggregate_source_budget_and_growth_failure_are_atomic() {
    let blocks = std::array::from_fn(|index| (index % 16) as u32);
    let measured = PalettedContainer::from_dense(
        ContainerKind::Blocks,
        Registry::new(8192).unwrap(),
        &blocks,
        64 * 1024,
    )
    .unwrap();
    let budget = measured.heap_bytes();
    drop(measured);
    let mut owner = owner(PreparationLimits {
        source_heap_bytes: budget,
        ..limits()
    });
    let pool = pool(1, 1);
    let first = address(0, 0);
    let second = address(0, 1);
    owner.load_chunk(first.chunk).unwrap();
    owner
        .load_section(first, &blocks, &[0; 64], COUNTS)
        .unwrap();
    load(&mut owner, second, 0);
    owner.request(first).unwrap();
    wait_cached(&mut owner, &pool, first);
    let key = owner.current_key(first);
    let stats = owner.stats();
    let cache_pointer = owner.cached(first).unwrap().1.as_ptr();
    let changed_counts = SectionCounts {
        non_empty_blocks: 3000,
        fluid_blocks: 0,
    };
    let expected = Err(Error::Section(SectionError::AllocationBudgetExceeded));
    assert_eq!(owner.set_block(first, 0, 16, changed_counts), expected);
    assert_eq!(owner.set_block(second, 0, 1, changed_counts), expected);
    assert_eq!(owner.set_biome(first, 0, 1), expected);
    assert_eq!(owner.stats(), stats);
    assert_eq!(owner.current_key(first), key);
    assert_eq!(owner.cached(first).unwrap().1.as_ptr(), cache_pointer);
    assert_eq!(owner.section(first).unwrap().counts, COUNTS);
    assert_eq!(owner.section(second).unwrap().counts, COUNTS);
    assert_eq!(owner.section(second).unwrap().blocks.get(0), Ok(0));
    for (index, value) in blocks.into_iter().enumerate() {
        assert_eq!(owner.section(first).unwrap().blocks.get(index), Ok(value));
    }
    assert_eq!(owner.drive(&pool).unwrap(), DriveReport::default());
}

#[test]
fn load_limits_and_missing_or_duplicate_addresses_preserve_loaded_state() {
    let mut owner = owner(PreparationLimits {
        max_chunks: 1,
        max_sections: 1,
        max_cached: 1,
        ..limits()
    });
    let first = address(0, 0);
    let second = address(0, 1);
    assert_eq!(
        owner.load_section(first, &[0; 4096], &[0; 64], COUNTS),
        Err(Error::MissingChunk)
    );
    assert_eq!(owner.request(first), Err(Error::MissingSection));
    assert_eq!(
        owner.set_block(first, 0, 1, COUNTS),
        Err(Error::MissingSection)
    );
    assert_eq!(owner.set_biome(first, 0, 1), Err(Error::MissingSection));
    assert_eq!(owner.set_counts(first, COUNTS), Err(Error::MissingSection));
    assert_eq!(owner.unload_chunk(first.chunk), Err(Error::MissingChunk));
    let generation = owner.load_chunk(first.chunk).unwrap();
    assert_eq!(
        owner.load_chunk(first.chunk),
        Err(Error::ChunkAlreadyLoaded)
    );
    assert_eq!(
        owner.load_chunk(address(1, 0).chunk),
        Err(Error::ChunkLimit)
    );
    let key = owner
        .load_section(first, &[0; 4096], &[0; 64], COUNTS)
        .unwrap();
    let stats = owner.stats();
    assert_eq!(
        owner.load_section(first, &[1; 4096], &[1; 64], COUNTS),
        Err(Error::SectionAlreadyLoaded)
    );
    assert_eq!(
        owner.load_section(second, &[0; 4096], &[0; 64], COUNTS),
        Err(Error::SectionLimit)
    );
    assert_eq!(owner.chunk_generation(first.chunk), Some(generation));
    assert_eq!(owner.current_key(first), Some(key));
    assert_eq!(owner.stats(), stats);
    owner.unload_chunk(first.chunk).unwrap();
    assert_eq!((owner.stats().chunks, owner.stats().sections), (0, 0));
    assert!(owner.load_chunk(address(1, 0).chunk).unwrap() > generation);
}

#[test]
fn failed_load_releases_partial_palettes_without_consuming_a_revision() {
    let blocks = std::array::from_fn(|index| (index % 2) as u32);
    let biomes = std::array::from_fn(|index| (index % 2) as u32);
    let measured = PalettedContainer::from_dense(
        ContainerKind::Blocks,
        Registry::new(8192).unwrap(),
        &blocks,
        64 * 1024,
    )
    .unwrap();
    let mut owner = owner(PreparationLimits {
        source_heap_bytes: measured.heap_bytes(),
        ..limits()
    });
    drop(measured);
    let first = address(0, 0);
    let second = address(0, 1);
    load(&mut owner, first, 0);
    let first_key = owner.current_key(first).unwrap();
    let stats = owner.stats();
    assert_eq!(
        owner.load_section(second, &blocks, &biomes, COUNTS),
        Err(Error::Section(SectionError::AllocationBudgetExceeded))
    );
    assert_eq!(owner.stats(), stats);
    assert!(owner.section(second).is_none());
    assert_eq!(
        owner.load_section(second, &[8192; 4096], &[0; 64], COUNTS),
        Err(Error::Section(SectionError::ValueOutOfRange(8192)))
    );
    assert_eq!(
        owner.load_section(second, &blocks, &[128; 64], COUNTS),
        Err(Error::Section(SectionError::ValueOutOfRange(128)))
    );
    assert_eq!(
        owner.load_section(
            second,
            &[0; 4096],
            &[0; 64],
            SectionCounts {
                non_empty_blocks: 0,
                fluid_blocks: 1
            }
        ),
        Err(Error::Section(SectionError::InvalidCounts))
    );
    assert_eq!(owner.stats(), stats);
    assert_eq!(owner.current_key(first), Some(first_key));
    let loaded = owner
        .load_section(second, &[0; 4096], &[0; 64], COUNTS)
        .unwrap();
    assert_eq!(loaded.revision, first_key.revision + 1);
    assert_eq!(owner.stats().source_heap_bytes, 0);
}

#[test]
fn repeated_requests_and_pending_mutations_coalesce_to_latest_revision() {
    let mut owner = owner(PreparationLimits {
        max_pending: 1,
        ..limits()
    });
    let pool = pool(2, 2);
    let address = address(1, -2);
    load(&mut owner, address, 0);
    let original = owner.request(address).unwrap();
    for _ in 0..20 {
        assert_eq!(owner.request(address), Ok(original));
    }
    assert_eq!(owner.stats().dirty, 1);
    assert_eq!(owner.drive(&pool).unwrap().submitted, 1);
    assert_eq!(owner.stats().pending, 1);
    for value in 1..=20 {
        owner.set_block(address, 123, value, COUNTS).unwrap();
        owner.set_biome(address, 11, value).unwrap();
        owner.request(address).unwrap();
    }
    let latest = owner.current_key(address).unwrap();
    assert!(latest.revision > original.revision);
    assert_eq!(owner.stats().dirty, 1);
    assert_eq!(owner.stats().pending, 1);
    let report = wait_cached(&mut owner, &pool, address);
    assert_eq!(
        (report.submitted, report.published, report.discarded),
        (1, 1, 1)
    );
    assert_eq!(owner.cached(address).unwrap().0, latest);
    let prepared = cached_section(&owner, address);
    assert_eq!(prepared.blocks.get(123), Ok(20));
    assert_eq!(prepared.biomes.get(11), Ok(20));
    assert_eq!(pool.stats().completed_jobs, 2);
    assert_eq!(owner.failure(address), None);
}

#[test]
fn unloading_and_reusing_coordinates_rejects_old_generation_work() {
    let mut owner = owner(limits());
    let pool = pool(2, 2);
    let address = address(-9, 7);
    load(&mut owner, address, 0);
    let generation = owner.chunk_generation(address.chunk).unwrap();
    owner.request(address).unwrap();
    wait_cached(&mut owner, &pool, address);
    owner.set_block(address, 12, 7, COUNTS).unwrap();
    let old_key = owner.current_key(address).unwrap();
    assert_eq!(owner.drive(&pool).unwrap().submitted, 1);
    owner.unload_chunk(address.chunk).unwrap();
    assert!(owner.section(address).is_none());
    assert!(owner.cached(address).is_none());
    assert_eq!(
        (
            owner.stats().sections,
            owner.stats().dirty,
            owner.stats().source_heap_bytes
        ),
        (0, 0, 0)
    );
    load(&mut owner, address, 9);
    assert!(owner.chunk_generation(address.chunk).unwrap() > generation);
    let new_key = owner.request(address).unwrap();
    assert!(new_key.revision > old_key.revision);
    let report = wait_cached(&mut owner, &pool, address);
    assert_eq!(report.discarded, 1);
    assert_eq!(owner.cached(address).unwrap().0, new_key);
    let prepared = cached_section(&owner, address);
    assert_eq!(prepared.blocks.get(12), Ok(9));
    assert_eq!(prepared.blocks.get(4095), Ok(9));
    assert_eq!(owner.stats().cached, 1);
    assert_eq!(pool.stats().in_flight, 1);
}

#[test]
fn world_reload_advances_epoch_clears_sources_and_uses_new_registries() {
    let mut owner = owner(limits());
    let pool = pool(3, 3);
    let first = address(0, 0);
    let second = address(1, 0);
    load(&mut owner, first, 0);
    load(&mut owner, second, 0);
    let generation = owner.chunk_generation(first.chunk).unwrap();
    owner.request(first).unwrap();
    wait_cached(&mut owner, &pool, first);
    let key = owner.request(second).unwrap();
    assert_eq!(owner.drive(&pool).unwrap().submitted, 1);
    let epoch = owner
        .reload(Registry::new(16).unwrap(), Registry::new(4).unwrap())
        .unwrap();
    assert_eq!(epoch, key.world_epoch + 1);
    assert_eq!(owner.epoch(), epoch);
    let stats = owner.stats();
    assert_eq!(
        (
            stats.chunks,
            stats.sections,
            stats.pending,
            stats.dirty,
            stats.cached,
            stats.source_heap_bytes
        ),
        (0, 0, 0, 0, 0, 0)
    );
    assert!(owner.cached(first).is_none());
    assert!(owner.section(second).is_none());
    assert!(owner.load_chunk(first.chunk).unwrap() > generation);
    let new_key = owner
        .load_section(first, &[15; 4096], &[3; 64], COUNTS)
        .unwrap();
    assert_eq!(new_key.world_epoch, epoch);
    assert!(new_key.revision > key.revision);
    assert_eq!(
        owner.set_block(first, 0, 16, COUNTS),
        Err(Error::Section(SectionError::ValueOutOfRange(16)))
    );
    assert_eq!(
        owner.set_biome(first, 0, 4),
        Err(Error::Section(SectionError::ValueOutOfRange(4)))
    );
    owner.request(first).unwrap();
    wait_cached(&mut owner, &pool, first);
    let (prepared_key, bytes) = owner.cached(first).unwrap();
    assert_eq!(prepared_key, new_key);
    let prepared = decode(bytes, Registry::new(16).unwrap(), Registry::new(4).unwrap());
    assert_eq!(prepared.blocks.get(4095), Ok(15));
    assert_eq!(prepared.biomes.get(63), Ok(3));
}

#[test]
fn owned_cache_eviction_unblocks_a_full_pool_and_keeps_sources_usable() {
    let mut owner = owner(PreparationLimits {
        max_cached: 2,
        ..limits()
    });
    let pool = pool(2, 2);
    let addresses = [address(0, 0), address(0, 1), address(0, 2)];
    for (index, address) in addresses.into_iter().enumerate() {
        load(&mut owner, address, index as u32 + 1);
    }
    for address in &addresses[..2] {
        owner.request(*address).unwrap();
        wait_cached(&mut owner, &pool, *address);
    }
    assert_eq!(owner.stats().cached, 2);
    assert_eq!(
        owner.stats().cached_reserved_buffer_bytes,
        2 * SECTION_JOB_BUFFER_BYTES
    );
    assert_eq!(pool.stats().in_flight, 2);
    owner.request(addresses[2]).unwrap();
    let report = owner.drive(&pool).unwrap();
    assert_eq!((report.evicted, report.submitted), (1, 1));
    assert!(owner.cached(addresses[0]).is_none());
    assert_eq!(owner.section(addresses[0]).unwrap().blocks.get(0), Ok(1));
    wait_cached(&mut owner, &pool, addresses[2]);
    assert_eq!(cached_section(&owner, addresses[2]).blocks.get(4095), Ok(3));
    assert_eq!(owner.stats().sections, 3);

    owner.request(addresses[0]).unwrap();
    let report = wait_cached(&mut owner, &pool, addresses[0]);
    assert_eq!(report.evicted, 1);
    assert!(owner.cached(addresses[1]).is_none());
    assert_eq!(cached_section(&owner, addresses[0]).blocks.get(4095), Ok(1));
    assert_eq!(owner.stats().cached, 2);
    assert_eq!(
        pool.stats().reserved_buffer_bytes,
        2 * SECTION_JOB_BUFFER_BYTES
    );
    drop(owner);
    assert_eq!(pool.stats().in_flight, 0);
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
}

#[test]
fn shared_pool_slot_and_byte_backpressure_preserve_dirty_latest_source() {
    for (slots, expected) in [
        (1, AdmissionError::JobLimit),
        (2, AdmissionError::ByteLimit),
    ] {
        let pool = pool(slots, 1);
        let mut first_owner = owner(limits());
        let mut second_owner = owner(limits());
        let first = address(0, 0);
        let second = address(1, 0);
        load(&mut first_owner, first, 1);
        first_owner.request(first).unwrap();
        wait_cached(&mut first_owner, &pool, first);
        load(&mut second_owner, second, 0);
        second_owner.request(second).unwrap();
        let report = second_owner.drive(&pool).unwrap();
        assert_eq!(report.backpressure, Some(expected));
        assert_eq!((report.submitted, report.evicted), (0, 0));
        assert_eq!(
            (second_owner.stats().dirty, second_owner.stats().pending),
            (1, 0)
        );
        second_owner.set_block(second, 42, 12, COUNTS).unwrap();
        let latest = second_owner.current_key(second).unwrap();
        assert_eq!(
            second_owner.drive(&pool).unwrap().backpressure,
            Some(expected)
        );
        assert_eq!(second_owner.stats().dirty, 1);
        drop(first_owner);
        assert_eq!(pool.stats().in_flight, 0);
        wait_cached(&mut second_owner, &pool, second);
        assert_eq!(second_owner.cached(second).unwrap().0, latest);
        assert_eq!(cached_section(&second_owner, second).blocks.get(42), Ok(12));
    }
}

#[test]
fn closed_pool_leaves_demand_dirty_for_a_replacement_pool() {
    let mut owner = owner(limits());
    let closed = pool(1, 1);
    let address = address(0, 0);
    load(&mut owner, address, 0);
    owner.request(address).unwrap();
    closed.close();
    for value in 1..=3 {
        owner.set_block(address, 23, value, COUNTS).unwrap();
        let report = owner.drive(&closed).unwrap();
        assert_eq!(report.backpressure, Some(AdmissionError::Closed));
        assert_eq!(report.submitted, 0);
        assert_eq!((owner.stats().dirty, owner.stats().pending), (1, 0));
        assert_eq!(owner.failure(address), None);
    }
    let latest = owner.current_key(address).unwrap();
    let replacement = pool(1, 1);
    wait_cached(&mut owner, &replacement, address);
    assert_eq!(owner.cached(address).unwrap().0, latest);
    assert_eq!(cached_section(&owner, address).blocks.get(23), Ok(3));
    assert_eq!(closed.stats().completed_jobs, 0);
}
