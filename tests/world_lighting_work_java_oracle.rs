//! Focused initial block+sky relighting comparison with actual LevelLightEngine.
//! The Java fixtures use original public-API observations, not translated bodies.
#[path = "common/lighting_fixture.rs"]
mod fixture;

use arrow_mc::world::{
    lighting::{
        LightBlock, LightSection,
        block::BlockLightLimits,
        sky::SkyLimits,
        storage::{LightSnapshot, StorageLimits},
        work::{LightingLimits, LightingWork, SkyWorkLimits},
    },
    preparation::ChunkAddress,
    storage::chunk::DimensionHeight,
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::SystemTime,
};

fn java_tool(name: &str) -> PathBuf {
    env::var_os("JAVA_HOME")
        .map(|home| {
            PathBuf::from(home).join("bin").join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            })
        })
        .unwrap_or_else(|| PathBuf::from(name))
}

fn observations(reference: &Path) -> Value {
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-lighting-work-oracle-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let oracle = directory.join("LightingWorkOracle.java");
    fs::write(&oracle, include_str!("common/lighting_work_oracle.java")).unwrap();
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
        .arg(oracle)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let output = directory.join("observations.json");
    let run = Command::new(java_tool("java"))
        .arg("-Xmx1G")
        .arg("-cp")
        .arg(classpath)
        .arg("LightingWorkOracle")
        .arg(&output)
        .current_dir(&directory)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let json = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    // Only this uniquely created test directory, including WorldLoader logs.
    let canonical = directory.canonicalize().unwrap();
    assert_eq!(
        canonical.parent(),
        Some(env::temp_dir().canonicalize().unwrap().as_path())
    );
    assert!(
        canonical
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("arrow-lighting-work-oracle-")
    );
    fs::remove_dir_all(canonical).unwrap();
    json
}

fn coordinate(row: &Value) -> LightBlock {
    LightBlock {
        x: row["x"].as_i64().unwrap() as i32,
        y: row["y"].as_i64().unwrap() as i32,
        z: row["z"].as_i64().unwrap() as i32,
    }
}

fn limits() -> LightingLimits {
    let storage = StorageLimits {
        max_sections: 256,
        max_columns: 64,
        max_notifications: 1024,
        metadata_bytes: 4 << 20,
        layer_bytes: 4 << 20,
    };
    LightingLimits {
        max_chunks: 4,
        metadata_bytes: 4 * size_of::<ChunkAddress>(),
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
                source_chunks: 4,
                planned_writes: 512,
            },
            storage,
            engine_bytes: 8 << 20,
        }),
    }
}

fn compare(snapshot: &LightSnapshot, expected: &Value, label: &str) -> usize {
    let rows = expected["layers"].as_array().unwrap();
    let mut wanted = BTreeSet::new();
    for row in rows {
        let pos = coordinate(row);
        let key = LightSection {
            x: pos.x,
            y: pos.y,
            z: pos.z,
        };
        assert!(wanted.insert(key), "duplicate {label} oracle section");
        let layer = snapshot
            .layer(key)
            .unwrap_or_else(|| panic!("missing {label} {key:?}"));
        assert_eq!(
            layer.is_empty(),
            row["empty"].as_bool().unwrap(),
            "{label} {key:?} empty"
        );
        assert_eq!(
            layer.is_definitely_homogeneous(),
            row["uniform"].as_bool().unwrap(),
            "{label} {key:?} representation"
        );
        let hex = row["bytes"].as_str().unwrap().as_bytes();
        assert_eq!(hex.len(), 4096);
        for index in 0..4096 {
            let value = (hex[index ^ 1] as char).to_digit(16).unwrap() as u8;
            let x = (index & 15) as u8;
            let z = ((index >> 4) & 15) as u8;
            let y = (index >> 8) as u8;
            assert_eq!(
                layer.get(x, y, z).unwrap() as u8,
                value,
                "{label} {key:?} nibble {index}"
            );
            assert_eq!(
                snapshot.get_level(LightBlock {
                    x: key.x * 16 + i32::from(x),
                    y: key.y * 16 + i32::from(y),
                    z: key.z * 16 + i32::from(z)
                }),
                value,
                "{label} {key:?} visible nibble {index}"
            );
        }
    }
    assert_eq!(
        snapshot.sections().collect::<BTreeSet<_>>(),
        wanted,
        "{label} complete section presence"
    );
    for probe in expected["probes"].as_array().unwrap() {
        assert_eq!(
            snapshot.get_level(LightBlock {
                x: probe[0].as_i64().unwrap() as i32,
                y: probe[1].as_i64().unwrap() as i32,
                z: probe[2].as_i64().unwrap() as i32,
            }),
            probe[3].as_u64().unwrap() as u8,
            "{label} probe {probe}"
        );
    }
    rows.len()
}

#[test]
#[ignore = "requires Java25 and authenticated lighting-v3 snapshot under ARROW_MC_JAVA_REFERENCE_ROOT"]
fn combined_initial_lighting_matches_actual_vanilla() {
    let reference = PathBuf::from(
        env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set ARROW_MC_JAVA_REFERENCE_ROOT"),
    );
    let registry = fixture::load_registry(&reference);
    let observed = observations(&reference);
    for profile in observed["profiles"].as_array().unwrap() {
        let id = profile["id"].as_u64().unwrap() as u32;
        let actual = registry.light_material(id).unwrap();
        assert_eq!(
            u64::from(actual.emission),
            profile["emission"].as_u64().unwrap()
        );
        assert_eq!(
            u64::from(actual.dampening),
            profile["dampening"].as_u64().unwrap()
        );
        assert_eq!(
            actual.can_occlude,
            profile["can_occlude"].as_bool().unwrap()
        );
        assert_eq!(
            actual.use_shape_for_light_occlusion,
            profile["use_shape"].as_bool().unwrap()
        );
        assert_eq!(
            actual.empty_shape(),
            profile["empty_shape"].as_bool().unwrap()
        );
    }
    let scenarios = observed["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 2);
    for budget in [usize::MAX, 7] {
        let mut sections = 0;
        let mut yields = 0;
        for scenario in scenarios {
            let name = scenario["name"].as_str().unwrap();
            let height = DimensionHeight::new(
                scenario["min_y"].as_i64().unwrap() as i32,
                scenario["height"].as_u64().unwrap() as u32,
            )
            .unwrap();
            let chunks: Vec<_> = scenario["chunks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| ChunkAddress {
                    x: row["x"].as_i64().unwrap() as i32,
                    z: row["z"].as_i64().unwrap() as i32,
                })
                .collect();
            let placements: Vec<_> = scenario["placements"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| (coordinate(row), row["state"].as_u64().unwrap() as u32))
                .collect();
            let source =
                fixture::from_placements(Arc::clone(&registry), height, &chunks, &placements);
            let mut work = LightingWork::new(source, limits()).unwrap();
            for _ in 0..100_000 {
                let progress = work.step(budget).unwrap();
                assert!(progress.processed <= budget);
                if progress.complete {
                    break;
                }
                yields += 1;
                work = work
                    .into_completed()
                    .err()
                    .expect("partial world returned completed layers");
            }
            let completed = work
                .into_completed()
                .unwrap_or_else(|_| panic!("{name}: work did not finish"));
            sections += compare(
                completed.block(),
                &scenario["block"],
                &format!("{name}/block"),
            );
            sections += compare(
                completed.sky().unwrap(),
                &scenario["sky"],
                &format!("{name}/sky"),
            );
        }
        if budget == 7 {
            assert!(yields > 0);
        }
        eprintln!(
            "Compared {} actual Vanilla initial block+sky domains / {sections} complete layers / {} nibbles at work budget {budget} ({yields} partial resumes)",
            scenarios.len(),
            sections * 4096
        );
    }
}
