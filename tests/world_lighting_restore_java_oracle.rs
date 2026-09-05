//! Opt-in saved-light initialization through actual Java and canonical Anvil input.
//! Java phase observations describe a finite public-API transaction; Rust compares
//! its final visible and queued-first snapshots, not arbitrary Threaded callbacks.
#[path = "common/lighting_fixture.rs"]
mod fixture;

use arrow_mc::{
    nbt,
    runtime::{CpuPool, CpuPoolConfig},
    world::{
        lighting::{
            LightBlock, LightSection, LightingSource, SourceLimits,
            block::BlockLightLimits,
            sky::SkyLimits,
            storage::{LightDataSnapshot, LightSnapshot, StorageLimits as LightStorageLimits},
            work::{LightingLimits, LightingWork, SkyWorkLimits},
        },
        loading::{ChunkLoadingOwner, LoadDemand, LoadingLimits, LoadingReadOutcome},
        preparation::ChunkAddress,
        storage::{
            ChunkStore, StorageLimits,
            chunk::{ChunkDecodeError, DimensionHeight, decode_current_chunk},
            registry::ChunkRegistrySnapshot,
        },
    },
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::time::timeout;

fn height() -> DimensionHeight {
    DimensionHeight::new(0, 32).unwrap()
}

fn java_tool(name: &str) -> PathBuf {
    env::var_os("JAVA_HOME")
        .map(|root| {
            PathBuf::from(root).join("bin").join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            })
        })
        .unwrap_or_else(|| PathBuf::from(name))
}

struct Observations {
    directory: PathBuf,
    report: Value,
}
impl Drop for Observations {
    fn drop(&mut self) {
        let canonical = self.directory.canonicalize().unwrap();
        assert_eq!(
            canonical.parent(),
            Some(env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(
            canonical
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("arrow-lighting-restore-oracle-")
        );
        fs::remove_dir_all(canonical).unwrap();
    }
}

fn observations(reference: &Path) -> Observations {
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-lighting-restore-oracle-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("LightingRestoreOracle.java");
    fs::write(&source, include_str!("common/lighting_restore_oracle.java")).unwrap();
    let artifacts = reference.join("artifacts/26.3-pre-2");
    let classpath = env::join_paths([
        directory.clone(),
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();
    let compile = Command::new(java_tool("javac"))
        .arg("-cp")
        .arg(&classpath)
        .arg("-d")
        .arg(&directory)
        .arg(source)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(java_tool("java"))
        .arg("-Xmx1G")
        .arg("-cp")
        .arg(classpath)
        .arg("LightingRestoreOracle")
        .arg(&directory)
        .current_dir(&directory)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let report =
        serde_json::from_slice(&fs::read(directory.join("observations.json")).unwrap()).unwrap();
    Observations { directory, report }
}

fn limits(sky: bool) -> LightingLimits {
    let storage = LightStorageLimits {
        max_sections: 128,
        max_columns: 32,
        max_notifications: 1024,
        metadata_bytes: 2 << 20,
        layer_bytes: 2 << 20,
    };
    LightingLimits {
        max_chunks: 2,
        metadata_bytes: 4096,
        block: BlockLightLimits {
            checks: 32,
            decreases: 65536,
            increases: 65536,
            queue_bytes: 8 << 20,
        },
        block_storage: storage,
        sky: sky.then_some(SkyWorkLimits {
            engine: SkyLimits {
                checks: 32,
                queue_entries: 65536,
                source_chunks: 2,
                planned_writes: 65536,
            },
            storage,
            engine_bytes: 16 << 20,
        }),
    }
}

async fn canonical(
    observations: &Observations,
    scenario: &Value,
    registry: &Arc<ChunkRegistrySnapshot>,
    cpu: &Arc<CpuPool>,
) -> ChunkLoadingOwner {
    let name = scenario["name"].as_str().unwrap();
    let directory = observations.directory.join(format!("region-{name}"));
    fs::create_dir(&directory).unwrap();
    let mut region = vec![0u8; 8192];
    for file in scenario["chunks"].as_array().unwrap() {
        let x = file["x"].as_u64().unwrap() as usize;
        assert!(x < 2);
        assert_eq!(file["z"], 0);
        let bytes = fs::read(observations.directory.join(file["nbt"].as_str().unwrap())).unwrap();
        let sectors = (bytes.len() + 5).div_ceil(4096);
        assert!(sectors < 256);
        let start_sector = region.len() / 4096;
        region[x * 4..x * 4 + 4]
            .copy_from_slice(&(((start_sector as u32) << 8) | sectors as u32).to_be_bytes());
        region.extend_from_slice(&((bytes.len() + 1) as u32).to_be_bytes());
        region.push(3);
        region.extend(bytes);
        region.resize((start_sector + sectors) * 4096, 0);
    }
    fs::write(directory.join("r.0.0.mca"), region).unwrap();
    let store = ChunkStore::new(
        directory,
        Arc::clone(cpu),
        Arc::clone(registry),
        height(),
        StorageLimits::default(),
        1,
    )
    .unwrap();
    let mut owner = ChunkLoadingOwner::new(
        71,
        Arc::clone(registry),
        height(),
        scenario["has_sky"].as_bool().unwrap(),
        LoadingLimits {
            max_chunks: 2,
            metadata_bytes: 65536,
        },
        4 << 20,
    )
    .unwrap();
    for address in addresses(scenario) {
        let LoadDemand::Read(request) = owner.request(address).unwrap() else {
            panic!("new canonical request expected")
        };
        let LoadingReadOutcome::Decoded(result) =
            timeout(Duration::from_secs(10), request.read(&store))
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("actual Java named NBT should decode")
        };
        owner.publish(result).unwrap();
    }
    owner
}

fn addresses(scenario: &Value) -> Vec<ChunkAddress> {
    scenario["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| ChunkAddress {
            x: file["x"].as_i64().unwrap() as i32,
            z: file["z"].as_i64().unwrap() as i32,
        })
        .collect()
}

fn coordinate(row: &Value) -> LightSection {
    LightSection {
        x: row["x"].as_i64().unwrap() as i32,
        y: row["y"].as_i64().unwrap() as i32,
        z: row["z"].as_i64().unwrap() as i32,
    }
}

fn nibble(hex: &[u8], index: usize) -> u8 {
    (hex[index ^ 1] as char).to_digit(16).unwrap() as u8
}

fn compare(
    visible: &LightSnapshot,
    packet: &LightDataSnapshot,
    rows: &Value,
    label: &str,
) -> (usize, usize) {
    let mut visible_sections = BTreeSet::new();
    let mut packet_sections = BTreeSet::new();
    let mut positions = 0;
    for row in rows.as_array().unwrap() {
        let key = coordinate(row);
        if row["support"].as_str().unwrap() != "EMPTY" {
            visible_sections.insert(key);
        }
        let data = &row["data"];
        match packet.layer(key) {
            None => assert!(data.is_null(), "{label}: missing packet {key:?}"),
            Some(layer) => {
                assert!(!data.is_null(), "{label}: unexpected packet {key:?}");
                packet_sections.insert(key);
                assert_eq!(
                    layer.is_empty(),
                    data["empty"].as_bool().unwrap(),
                    "{label}: {key:?} empty"
                );
                assert_eq!(
                    layer.is_definitely_homogeneous(),
                    data["uniform"].as_bool().unwrap(),
                    "{label}: {key:?} representation"
                );
                let hex = data["bytes"].as_str().unwrap().as_bytes();
                assert_eq!(hex.len(), 4096);
                for index in 0..4096 {
                    assert_eq!(
                        layer
                            .get(
                                (index & 15) as u8,
                                (index >> 8) as u8,
                                ((index >> 4) & 15) as u8
                            )
                            .unwrap() as u8,
                        nibble(hex, index),
                        "{label}: packet {key:?} nibble {index}"
                    );
                }
            }
        }
        let hex = row["visible"].as_str().unwrap().as_bytes();
        assert_eq!(hex.len(), 4096);
        for index in 0..4096 {
            assert_eq!(
                visible.get_level(LightBlock {
                    x: key.x * 16 + (index & 15) as i32,
                    y: key.y * 16 + (index >> 8) as i32,
                    z: key.z * 16 + ((index >> 4) & 15) as i32,
                }),
                nibble(hex, index),
                "{label}: visible {key:?} nibble {index}"
            );
        }
        positions += 4096;
    }
    assert_eq!(
        visible.sections().collect::<BTreeSet<_>>(),
        visible_sections,
        "{label}: visible section presence"
    );
    assert_eq!(
        packet.sections().collect::<BTreeSet<_>>(),
        packet_sections,
        "{label}: queued-first section presence"
    );
    (positions, packet_sections.len())
}

fn check_java_boundaries(scenario: &Value) {
    let phases = scenario["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 6);
    assert_eq!(phases[0]["name"], "staged");
    assert_eq!(phases[2]["name"], "initialize_post");
    assert_eq!(phases[2]["enabled"], scenario["reuse"]);
    assert_eq!(phases[0]["light_correct"], scenario["flag"]);
    assert_eq!(phases[2]["light_correct"], scenario["flag"]);
    assert_eq!(phases[3]["light_correct"], false);
    assert_eq!(phases[4]["light_correct"], false);
    assert_eq!(phases[5]["light_correct"], true);
    assert_eq!(scenario["finished_has_work"], false);
    // These assertions concern Java observations, not Rust's private phase timing.
}

#[test]
#[ignore = "requires Java25 and authenticated lighting-v3 snapshot under ARROW_MC_JAVA_REFERENCE_ROOT"]
fn saved_light_initialization_matches_actual_vanilla() {
    let reference = PathBuf::from(
        env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set ARROW_MC_JAVA_REFERENCE_ROOT"),
    );
    let registry = fixture::load_registry(&reference);
    let observed = observations(&reference);
    assert_eq!(observed.report["version"], "26.3-pre-2");
    let scenarios = observed.report["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 23);
    let errors = observed.report["parse_errors"].as_array().unwrap();
    assert_eq!(errors.len(), 16);
    for error in errors {
        let name = error["name"].as_str().unwrap();
        assert_eq!(error["error"], "java.lang.IllegalArgumentException");
        let result = decode_current_chunk(
            &mut fs::read(observed.directory.join(format!("{name}.nbt"))).unwrap(),
            &registry,
            height(),
            nbt::Limits::default(),
            4 << 20,
        );
        assert!(
            matches!(result, Err(ChunkDecodeError::LightLength(length)) if length == error["length"].as_u64().unwrap() as usize),
            "{name}"
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let cpu = Arc::new(CpuPool::new(CpuPoolConfig { workers: 1, max_jobs: 2, buffer_bytes: 128 << 20 }).unwrap());
        let mut visible_values = 0;
        let mut packet_layers = 0;
        let mut resumes = 0;
        for scenario in scenarios {
            check_java_boundaries(scenario);
            let owner = canonical(&observed, scenario, &registry, &cpu).await;
            let name = scenario["name"].as_str().unwrap();
            for budget in [usize::MAX, 7] {
                let source = LightingSource::from_canonical(&owner, &addresses(scenario), SourceLimits::default()).unwrap();
                let limits = limits(scenario["has_sky"].as_bool().unwrap());
                // This standalone oracle admits one bounded kernel at a time;
                // the actual canonical source retains its original resident lease.
                assert!(limits.reservation_bytes().unwrap() + source.heap_bytes() < 64 << 20);
                let mut work = LightingWork::new_restore(source, limits).unwrap();
                let mut complete = false;
                for _ in 0..100_000 {
                    let progress = work.step(budget).unwrap_or_else(|error| panic!("{name}: {error}"));
                    assert!(progress.processed <= budget);
                    if progress.complete { complete = true; break; }
                    resumes += 1;
                }
                assert!(complete, "{name}: finite restore did not converge");
                let completed = work.into_completed().unwrap_or_else(|_| panic!("{name}: false completion"));
                let (values, layers) = compare(completed.block(), completed.packet_block(), &scenario["block"], &format!("{name}/{budget}/block"));
                visible_values += values;
                packet_layers += layers;
                if scenario["has_sky"].as_bool().unwrap() {
                    let (values, layers) = compare(completed.sky().unwrap(), completed.packet_sky().unwrap(), &scenario["sky"], &format!("{name}/{budget}/sky"));
                    visible_values += values;
                    packet_layers += layers;
                } else {
                    assert!(completed.sky().is_none());
                    assert!(completed.packet_sky().is_none());
                }
            }
        }
        assert!(resumes > 0);
        Arc::try_unwrap(cpu).unwrap_or_else(|_| panic!("all stores released")).shutdown().unwrap();
        eprintln!("Compared {} actual saved-light transactions at unbounded/7-unit budgets: {visible_values} visible nibbles, {packet_layers} queued-first layers ({} nibbles), {resumes} resumptions; {} Java phase observations and {} actual parse rejections", scenarios.len(), packet_layers * 4096, scenarios.len() * 6, errors.len());
    });
}
