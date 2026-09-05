//! Local-only actual LevelLightEngine replay. No reference data is bundled.
#[path = "common/lighting_fixture.rs"]
mod fixture;

use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection, LightingSource,
        sky::{SkyLightEngine, SkyLimits},
        storage::{LightSectionStorage, StorageLimits},
    },
    preparation::ChunkAddress,
    storage::chunk::DimensionHeight,
};
use serde_json::Value;
use std::{collections::BTreeSet, env, fs, path::Path, process::Command};

fn java_observations(reference: &Path) -> Value {
    let directory = reference.join("reports/lighting-sky");
    fs::create_dir_all(&directory).unwrap();
    let helper = directory.join("LightingOracle.java");
    let oracle = directory.join("LightingSkyOracle.java");
    fs::write(&helper, include_str!("common/lighting_oracle.java")).unwrap();
    fs::write(&oracle, include_str!("common/lighting_sky_oracle.java")).unwrap();
    let artifacts = reference.join("artifacts/26.3-pre-2");
    let classpath = env::join_paths([
        directory.clone(),
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();
    let compiled = Command::new("javac")
        .arg("-cp")
        .arg(&classpath)
        .arg("-d")
        .arg(&directory)
        .arg(&helper)
        .arg(&oracle)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let output = directory.join("sky.json");
    let run = Command::new("java")
        .arg("-Xmx1G")
        .arg("-cp")
        .arg(classpath)
        .arg("LightingSkyOracle")
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
    serde_json::from_slice(&fs::read(output).unwrap()).unwrap()
}

fn position(value: &Value) -> LightBlock {
    LightBlock {
        x: value["x"].as_i64().unwrap() as i32,
        y: value["y"].as_i64().unwrap() as i32,
        z: value["z"].as_i64().unwrap() as i32,
    }
}

fn engine(world: &LightingSource) -> SkyLightEngine {
    let storage = LightSectionStorage::new(
        LightKind::Sky,
        StorageLimits {
            max_sections: 512,
            max_columns: 64,
            max_notifications: 1024,
            metadata_bytes: 4 << 20,
            layer_bytes: 8 << 20,
        },
    )
    .unwrap();
    let mut budget = 64 << 20;
    let mut engine = SkyLightEngine::new(
        storage,
        SkyLimits {
            checks: 4096,
            queue_entries: 32_768,
            source_chunks: 16,
            planned_writes: 4096,
        },
        &mut budget,
    )
    .unwrap();
    // Initialization insertion order is z, x, y, as in the Java scenario.
    for z in 0..3 {
        for x in 0..3 {
            let chunk = ChunkAddress { x, z };
            engine.initialize_sources(world, chunk).unwrap();
            for y in 0..6 {
                let section = LightSection { x, y, z };
                if !world.section_has_only_air(section) {
                    engine
                        .storage_mut()
                        .unwrap()
                        .update_section_status(section, false)
                        .unwrap();
                }
            }
        }
    }
    engine
}

fn compare(engine: &mut SkyLightEngine, expected: &Value, name: &str) -> usize {
    let label = expected["label"].as_str().unwrap();
    let snapshot = engine.storage().snapshot();
    let layers = expected["layers"].as_array().unwrap();
    let mut wanted = BTreeSet::new();
    for row in layers {
        // JSON x/y/z values are already section coordinates.
        let key = LightSection {
            x: row["x"].as_i64().unwrap() as i32,
            y: row["y"].as_i64().unwrap() as i32,
            z: row["z"].as_i64().unwrap() as i32,
        };
        wanted.insert(key);
        let layer = snapshot
            .layer(key)
            .unwrap_or_else(|| panic!("{name}/{label}: missing {key:?}"));
        assert_eq!(
            layer.is_empty(),
            row["empty"].as_bool().unwrap(),
            "{name}/{label}: empty {key:?}"
        );
        assert_eq!(
            layer.is_definitely_homogeneous(),
            row["uniform"].as_bool().unwrap(),
            "{name}/{label}: uniform {key:?}"
        );
        let bytes = row["bytes"].as_str().unwrap().as_bytes();
        assert_eq!(bytes.len(), 4096);
        for index in 0..4096 {
            let value = layer
                .get(
                    (index & 15) as u8,
                    (index >> 8) as u8,
                    ((index >> 4) & 15) as u8,
                )
                .unwrap();
            let nibble = if index & 1 == 0 {
                bytes[index + 1]
            } else {
                bytes[index - 1]
            };
            let expected = (nibble as char).to_digit(16).unwrap() as i32;
            assert_eq!(
                value, expected,
                "{name}/{label}: {key:?} block index {index}"
            );
        }
    }
    let actual: BTreeSet<_> = snapshot.sections().collect();
    assert_eq!(actual, wanted, "{name}/{label}: layer existence");
    for probe in expected["probes"].as_array().unwrap() {
        let pos = LightBlock {
            x: probe[0].as_i64().unwrap() as i32,
            y: probe[1].as_i64().unwrap() as i32,
            z: probe[2].as_i64().unwrap() as i32,
        };
        assert_eq!(
            snapshot.get_level(pos),
            probe[3].as_u64().unwrap() as u8,
            "{name}/{label}: probe {pos:?}"
        );
    }
    let actual: BTreeSet<_> = engine
        .storage()
        .published_sections()
        .iter()
        .map(|p| format!("SKY:{},{},{}", p.x, p.y, p.z))
        .collect();
    let wanted: BTreeSet<_> = expected["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(actual, wanted, "{name}/{label}: published notifications");
    engine
        .storage_mut()
        .unwrap()
        .clear_published_notifications();
    layers.len()
}

#[test]
#[ignore = "requires Java 25, locked server JAR and authenticated lighting metadata"]
fn actual_multi_chunk_sky_layers_mutations_and_empty_bridges_match() {
    let reference = std::path::PathBuf::from(
        env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set ARROW_MC_JAVA_REFERENCE_ROOT"),
    );
    let registry = fixture::load_registry(&reference);
    let observations = java_observations(&reference);
    let chunks: Vec<_> = (0..3)
        .flat_map(|z| (0..3).map(move |x| ChunkAddress { x, z }))
        .collect();
    let mut layer_snapshots = 0;
    for scenario in observations["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let height = DimensionHeight::new(0, 96).unwrap();
        let mut placements: Vec<_> = scenario["placements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (position(row), row["state"].as_u64().unwrap() as u32))
            .collect();
        let mut world = fixture::from_placements(registry.clone(), height, &chunks, &placements);
        let mut engine = engine(&world);
        engine.run_updates(&world).unwrap();
        let states = scenario["states"].as_array().unwrap();
        layer_snapshots += compare(&mut engine, &states[0], name);
        for &chunk in &chunks {
            engine.set_light_enabled(chunk, true).unwrap();
        }
        engine.run_updates(&world).unwrap();
        layer_snapshots += compare(&mut engine, &states[1], name);
        for &chunk in &chunks {
            engine.propagate_light_sources(chunk).unwrap();
        }
        let initial_entries = engine.stats().pending_increases;
        engine.run_updates(&world).unwrap();
        layer_snapshots += compare(&mut engine, &states[2], name);
        for row in scenario["updates"].as_array().unwrap() {
            let pos = position(row);
            placements.push((pos, row["state"].as_u64().unwrap() as u32));
            world = fixture::from_placements(registry.clone(), height, &chunks, &placements);
            engine.update_sources(&world, pos).unwrap();
            engine.check_block(pos).unwrap();
            engine.run_updates(&world).unwrap();
            layer_snapshots += compare(&mut engine, &row["after"], name);
        }
        engine.set_light_enabled(chunks[0], false).unwrap();
        engine
            .check_block(LightBlock {
                x: 15,
                y: 47,
                z: 15,
            })
            .unwrap();
        engine.run_updates(&world).unwrap();
        layer_snapshots += compare(&mut engine, &states[3], name);
        engine.set_light_enabled(chunks[0], true).unwrap();
        engine.propagate_light_sources(chunks[0]).unwrap();
        engine.run_updates(&world).unwrap();
        layer_snapshots += compare(&mut engine, &states[4], name);
        eprintln!(
            "{name}: initialization pending={initial_entries}, engine={:?}, storage={:?}",
            engine.stats(),
            engine.storage().stats()
        );
    }
    eprintln!(
        "Matched {layer_snapshots} complete sky layers and 22 visible stages against actual Java"
    );
}
