//! Opt-in local lighting throughput measurement; not a Minecraft tick benchmark.
//! Run the release test alone with --ignored --nocapture --test-threads=1.

#[path = "common/lighting_fixture.rs"]
mod fixture;

use arrow_mc::{
    nbt::Tag,
    runtime::{
        CpuPool, CpuPoolConfig, LightingCompletion, MAX_LIGHTING_SLICE_UNITS, WORKER_STACK_BYTES,
    },
    world::{
        lighting::{
            LightBlock, LightKind, LightingSource, SourceLimits,
            block::BlockLightLimits,
            sky::SkyLimits,
            storage::StorageLimits,
            work::{CompletedLighting, LightingLimits, LightingWork, SkyWorkLimits},
        },
        preparation::ChunkAddress,
        storage::{chunk::DimensionHeight, registry::ChunkRegistrySnapshot},
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf, process::Command, sync::Arc, time::Instant};

const JOBS: usize = 8;
const REPETITIONS: usize = 3;
const CHUNKS: usize = 4;
const SOURCE_METADATA_BYTES: usize = 16 << 10;
const SOURCE_SECTION_BYTES: usize = 256 << 10;

fn limits() -> LightingLimits {
    let storage = StorageLimits {
        max_sections: 256,
        max_columns: 36,
        max_notifications: 512,
        metadata_bytes: 1 << 20,
        layer_bytes: 2 << 20,
    };
    LightingLimits {
        max_chunks: CHUNKS,
        metadata_bytes: CHUNKS * size_of::<ChunkAddress>(),
        block: BlockLightLimits {
            checks: 64,
            decreases: 65536,
            increases: 65536,
            queue_bytes: 4 << 20,
        },
        block_storage: storage,
        sky: Some(SkyWorkLimits {
            engine: SkyLimits {
                checks: 64,
                queue_entries: 65536,
                source_chunks: CHUNKS,
                planned_writes: 1024,
            },
            storage,
            engine_bytes: 8 << 20,
        }),
    }
}

fn sources(registry: &Arc<ChunkRegistrySnapshot>) -> Vec<LightingSource> {
    let state = |name: &str| {
        let resolved = registry.block_state(&Tag::String(name.into()));
        assert!(!resolved.used_fallback);
        resolved.id
    };
    let stone = state("minecraft:stone");
    let water = state("minecraft:water");
    let glowstone = state("minecraft:glowstone");
    let slab = state("minecraft:stone_slab");
    assert_eq!(registry.light_material(glowstone).unwrap().emission, 15);
    let source_limits = SourceLimits {
        max_chunks: CHUNKS,
        metadata_bytes: SOURCE_METADATA_BYTES,
        owned_section_bytes: SOURCE_SECTION_BYTES,
    };
    // Reserve the producer's fixed allowance before constructing any palettes.
    // Dense scratch is one 64-KiB chunk, not a parallel allocation per worker.
    let producer_budget = JOBS * (SOURCE_METADATA_BYTES + SOURCE_SECTION_BYTES);
    let mut output = Vec::with_capacity(JOBS);
    let mut retained = 0;
    for job in 0..JOBS {
        let mut chunks = Vec::with_capacity(CHUNKS);
        for cz in 0..2 {
            for cx in 0..2 {
                let address = ChunkAddress { x: cx, z: cz };
                let mut dense = vec![[registry.air_id(); 4096]; 4];
                for z in 0..16 {
                    for x in 0..16 {
                        let wx = cx * 16 + x;
                        let wz = cz * 16 + z;
                        for y in -16..48 {
                            let block =
                                if y == -16 || (y == 24 && (wx + 3 * wz + job as i32) % 11 > 1) {
                                    stone
                                } else if y == 8 && (wx + wz + job as i32) % 13 == 0 {
                                    slab
                                } else if (-2..=0).contains(&y) && wx % 9 < 3 && wz % 7 < 3 {
                                    water
                                } else if y == 4 && x == 15 && z == (job as i32 + 4) % 16 {
                                    glowstone
                                } else {
                                    registry.air_id()
                                };
                            let pos = LightBlock { x: wx, y, z: wz };
                            dense[((y + 16) / 16) as usize][pos.local_index()] = block;
                        }
                    }
                }
                chunks.push(fixture::chunk_from_dense(registry, address, &dense));
            }
        }
        let source = LightingSource::from_sections(
            Arc::clone(registry),
            DimensionHeight::new(-16, 64).unwrap(),
            chunks,
            source_limits,
        )
        .unwrap();
        retained += source.heap_bytes();
        assert!(retained <= producer_budget);
        output.push(source);
    }
    output
}

struct Run {
    record: Value,
    grids: Vec<Vec<u8>>,
}

fn grid(mut level: impl FnMut(LightKind, LightBlock) -> u8) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(34 * 34 * 96 * 2);
    // All block/sky values inside the available domain plus a one-block X/Z
    // boundary and one light-only section above/below the admitted dimension.
    for y in -32..64 {
        for z in -1..33 {
            for x in -1..33 {
                let pos = LightBlock { x, y, z };
                bytes.push(level(LightKind::Block, pos));
                bytes.push(level(LightKind::Sky, pos));
            }
        }
    }
    bytes
}

fn inline(input: Vec<LightingSource>, limits: LightingLimits) -> Run {
    let source_bytes: usize = input.iter().map(LightingSource::heap_bytes).sum();
    let admission = source_bytes + JOBS * limits.reservation_bytes().unwrap();
    let mut results = Vec::<CompletedLighting>::with_capacity(JOBS);
    let mut slices = 0u64;
    let mut units = 0u64;
    let started = Instant::now();
    for source in input {
        let mut work = LightingWork::new(source, limits).unwrap();
        loop {
            let progress = work.step(MAX_LIGHTING_SLICE_UNITS).unwrap();
            slices += 1;
            units += progress.processed as u64;
            if progress.complete {
                break;
            }
        }
        results.push(
            work.into_completed()
                .unwrap_or_else(|_| panic!("work is complete")),
        );
    }
    let elapsed = started.elapsed();
    let grids = results
        .iter()
        .map(|result| {
            grid(|kind, pos| match kind {
                LightKind::Block => result.block().get_level(pos),
                LightKind::Sky => result.sky().unwrap().get_level(pos),
            })
        })
        .collect();
    Run {
        record: json!({"workers":0,"elapsed_ns":elapsed.as_nanos(),"slices":slices,
            "processed_units":units,"global_reservation_bytes":admission,
            "source_heap_bytes":source_bytes,"peak_running":1,"worker_stack_reservation_bytes":0}),
        grids,
    }
}

fn pooled(input: Vec<LightingSource>, limits: LightingLimits, workers: usize) -> Run {
    let source_bytes: usize = input.iter().map(LightingSource::heap_bytes).sum();
    let admission = source_bytes + JOBS * limits.reservation_bytes().unwrap();
    let pool = Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers,
            max_jobs: JOBS,
            buffer_bytes: admission,
        })
        .unwrap(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut results: Vec<Option<LightingCompletion>> = (0..JOBS).map(|_| None).collect();
    let mut slices = 0u64;
    let mut units = 0u64;
    let started = Instant::now();
    runtime.block_on(async {
        let mut tasks = tokio::task::JoinSet::new();
        for (index, source) in input.into_iter().enumerate() {
            let pool = Arc::clone(&pool);
            tasks.spawn(async move {
                let mut pending = pool.try_reserve_lighting(source, limits).unwrap();
                let mut slices = 0u64;
                let mut units = 0u64;
                loop {
                    let completion = pending
                        .submit(MAX_LIGHTING_SLICE_UNITS)
                        .unwrap()
                        .wait()
                        .await
                        .unwrap();
                    let progress = completion.progress().unwrap();
                    assert!(progress.processed <= MAX_LIGHTING_SLICE_UNITS);
                    slices += 1;
                    units += progress.processed as u64;
                    if progress.complete {
                        return (index, completion, slices, units);
                    }
                    pending = completion
                        .into_pending()
                        .unwrap_or_else(|_| panic!("work is pending"));
                }
            });
        }
        while let Some(result) = tasks.join_next().await {
            let (index, completion, count, processed) = result.unwrap();
            results[index] = Some(completion);
            slices += count;
            units += processed;
        }
    });
    let elapsed = started.elapsed();
    let stats = pool.stats();
    assert_eq!(stats.in_flight, JOBS);
    assert!(stats.peak_reserved_buffer_bytes <= admission);
    assert!(stats.peak_running <= workers);
    assert_eq!(stats.completed_jobs, slices);
    let grids = results
        .iter()
        .map(|result| grid(|kind, pos| result.as_ref().unwrap().light_level(kind, pos).unwrap()))
        .collect();
    drop(results);
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
    let pool = Arc::try_unwrap(pool).unwrap_or_else(|_| panic!("all job owners are released"));
    pool.shutdown().unwrap();
    Run {
        record: json!({"workers":workers,"elapsed_ns":elapsed.as_nanos(),"slices":slices,
            "processed_units":units,"global_reservation_bytes":admission,
            "peak_reserved_buffer_bytes":stats.peak_reserved_buffer_bytes,"source_heap_bytes":source_bytes,
            "peak_running":stats.peak_running,"worker_stack_reservation_bytes":workers * WORKER_STACK_BYTES}),
        grids,
    }
}

#[test]
#[ignore = "local release benchmark using authenticated lighting-v3 data; run alone"]
#[allow(clippy::assertions_on_constants)]
fn lighting_inline_and_shared_workers() {
    // The ignored test must compile in debug CI while rejecting debug measurements.
    assert!(
        !cfg!(debug_assertions),
        "run this measurement with --release"
    );
    let reference = env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("Decompile")
        });
    let registry = fixture::load_registry(&reference);
    let limits = limits();
    // One untimed warmup also supplies the complete comparison grids.
    let expected = inline(sources(&registry), limits);
    assert!(expected.grids.iter().all(|bytes| {
        bytes.chunks_exact(2).any(|levels| levels[0] == 15)
            && bytes
                .chunks_exact(2)
                .any(|levels| levels[1] > 0 && levels[1] < 15)
    }));
    let mut hash = Sha256::new();
    for bytes in &expected.grids {
        hash.update(bytes);
    }
    let digest = format!("{:x}", hash.finalize());
    let mut measurements = Vec::new();
    for repetition in 0..REPETITIONS {
        let modes = [0, 1, 2, 4];
        for offset in 0..modes.len() {
            let workers = modes[(repetition + offset) % modes.len()];
            let input = sources(&registry);
            let mut run = if workers == 0 {
                inline(input, limits)
            } else {
                pooled(input, limits, workers)
            };
            assert_eq!(
                run.grids, expected.grids,
                "every selected block/sky value must match"
            );
            assert_eq!(run.record["slices"], expected.record["slices"]);
            assert_eq!(
                run.record["processed_units"],
                expected.record["processed_units"]
            );
            run.record["repetition"] = json!(repetition);
            measurements.push(run.record);
        }
    }
    let rustc = Command::new("rustc").arg("-vV").output().unwrap();
    assert!(rustc.status.success());
    let mut measured_sources = Vec::new();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for directory in ["src/world/lighting", "src/runtime"] {
        for entry in fs::read_dir(repository.join(directory)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|extension| extension == "rs") {
                measured_sources.push(path);
            }
        }
    }
    measured_sources.extend([
        repository.join("tests/lighting_benchmark.rs"),
        repository.join("tests/common/lighting_fixture.rs"),
        repository.join("src/world/storage/registry.rs"),
        repository.join("src/world/storage/registry/lighting.rs"),
        repository.join("src/world/section.rs"),
        repository.join("src/world/section/packed.rs"),
        repository.join("Cargo.lock"),
        repository.join("Cargo.toml"),
    ]);
    measured_sources.sort();
    let source_hashes: Vec<_> = measured_sources
        .iter()
        .map(|path| {
            json!({"path":path.strip_prefix(&repository).unwrap().to_string_lossy().replace('\\', "/"),
                "sha256":format!("{:x}",Sha256::digest(fs::read(path).unwrap()))})
        })
        .collect();
    let medians: Vec<_> = [0, 1, 2, 4]
        .into_iter()
        .map(|workers| {
            let mut elapsed: Vec<_> = measurements
                .iter()
                .filter(|record| record["workers"] == workers)
                .map(|record| record["elapsed_ns"].as_u64().unwrap())
                .collect();
            elapsed.sort_unstable();
            json!({"workers":workers,"median_ns":elapsed[REPETITIONS / 2]})
        })
        .collect();
    let report = json!({
        "format_version":1,"benchmark":"initial block+sky LightingWork across independent multichunk domains",
        "context":{"os":env::consts::OS,"arch":env::consts::ARCH,"profile":"release",
            "available_parallelism":std::thread::available_parallelism().unwrap().get(),
            "rustc":String::from_utf8(rustc.stdout).unwrap(),
            "cpu_label":env::var("ARROW_BENCHMARK_CPU").ok(),
            "registry_manifest_sha256":registry.manifest_sha256().iter().map(|byte|format!("{byte:02x}")).collect::<String>(),
            "invocation":"cargo test --locked --release --test lighting_benchmark -- --ignored --nocapture --test-threads=1",
            "build_dependencies":"Warm pinned Cargo release dependency artifacts; compilation is excluded from elapsed_ns.",
            "registry_shared_load_admission_bytes":128 << 20},
        "workload":{"jobs":JOBS,"chunks_per_job":CHUNKS,"min_y":-16,"height":64,
            "source_metadata_allowance_per_job":SOURCE_METADATA_BYTES,"source_section_allowance_per_job":SOURCE_SECTION_BYTES,
            "producer_dense_scratch_bytes":64 << 10,"kernel_reservation_per_job":limits.reservation_bytes().unwrap(),
            "max_in_flight":JOBS,"slice_units":MAX_LIGHTING_SLICE_UNITS,
            "selected_grid":{"x":[-1,32],"y":[-32,63],"z":[-1,32],"layers":["block","sky"]},
            "compared_bytes_per_mode":expected.grids.iter().map(Vec::len).sum::<usize>(),"sha256":digest},
        "measurement_scope":"Sources/registry, pool/thread startup, final grid comparison and final result/pool destruction excluded. Engine construction, 64-unit convergence/resubmission, completion handoff and temporary kernel/address/storage cleanup included. Three rotated repetitions after one inline warmup. Global reservations are conservative ownership budgets, not RSS; registry, async task/control allocations and allocator overhead are separate. This is independent-domain initial lighting, not mutable cross-domain ticking or a Vanilla performance comparison.",
        "source_hashes":source_hashes,"medians":medians,"measurements":measurements,
    });
    if let Some(path) = env::var_os("ARROW_LIGHTING_BENCHMARK_OUTPUT") {
        let mut encoded = serde_json::to_vec_pretty(&report).unwrap();
        encoded.push(b'\n');
        fs::write(path, encoded).unwrap();
    }
    println!("ARROW_LIGHTING_BENCHMARK_JSON={report}");
}
