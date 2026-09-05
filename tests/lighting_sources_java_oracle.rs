//! Opt-in source-cache replay against actual Vanilla ProtoChunk public APIs.
//! Set ARROW_MC_JAVA_REFERENCE_ROOT to the local Decompile directory.

#[path = "common/lighting_fixture.rs"]
mod fixture;
use arrow_mc::world::{
    lighting::{LightBlock, sources::SkySources},
    preparation::ChunkAddress,
    storage::chunk::DimensionHeight,
};
use serde_json::Value;
use std::{env, fs, path::PathBuf, process::Command, sync::Arc};
fn java_observations(reference: &std::path::Path) -> Value {
    let directory = reference.join("reports/lighting-sources");
    fs::create_dir_all(&directory).unwrap();
    let java = directory.join("LightingOracle.java");
    let output = directory.join("sources.json");
    let logging = directory.join("logging.xml");
    fs::write(&java, include_str!("common/lighting_oracle.java")).unwrap();
    fs::write(&logging, "<Configuration status=\"OFF\"><Appenders/><Loggers><Root level=\"off\"/></Loggers></Configuration>").unwrap();
    let artifacts = reference.join("artifacts/26.3-pre-2");
    let execution = Command::new("java")
        .arg("-Xmx1G")
        .arg(format!("-Dlog4j2.configurationFile={}", logging.display()))
        .arg("--class-path")
        .arg(
            env::join_paths([
                artifacts.join("server-26.3-pre-2.jar"),
                artifacts.join("libraries/*"),
            ])
            .unwrap(),
        )
        .arg(&java)
        .arg(&output)
        .current_dir(&directory)
        .output()
        .unwrap();
    assert!(
        execution.status.success(),
        "Java source oracle failed:\n{}\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    serde_json::from_slice(&fs::read(output).unwrap()).unwrap()
}

fn assert_columns(cache: &SkySources, expected: &Value, context: &str) {
    let values = expected["columns"].as_array().unwrap();
    assert_eq!(values.len(), 256);
    for (index, value) in values.iter().enumerate() {
        assert_eq!(
            cache
                .lowest_source_y((index & 15) as u8, (index >> 4) as u8)
                .unwrap(),
            value.as_i64().unwrap() as i32,
            "{context}, column {index}"
        );
    }
    assert_eq!(
        cache.highest_lowest_source_y(),
        expected["highest"].as_i64().unwrap() as i32,
        "{context}, highest"
    );
}

fn apply(dense: &mut [[u32; 4096]], min_y: i32, operation: &Value) -> LightBlock {
    let pos = LightBlock {
        x: operation["x"].as_i64().unwrap() as i32,
        y: operation["y"].as_i64().unwrap() as i32,
        z: operation["z"].as_i64().unwrap() as i32,
    };
    let section = ((pos.y - min_y) / 16) as usize;
    let index = ((pos.y & 15) << 8 | (pos.z & 15) << 4 | (pos.x & 15)) as usize;
    dense[section][index] = operation["state"].as_u64().unwrap() as u32;
    pos
}

#[test]
#[ignore = "requires Java 25, locked server JAR and authenticated version-3 lighting metadata"]
fn source_initialization_and_updates_match_actual_protochunk() {
    let reference = PathBuf::from(
        env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set ARROW_MC_JAVA_REFERENCE_ROOT"),
    );
    let registry = fixture::load_registry(&reference);
    let observations = java_observations(&reference);
    assert_eq!(observations["version"], "26.3-pre-2");
    assert_eq!(observations["data_version"], 5018);
    assert_eq!(observations["state_count"], registry.state_count());
    let mut snapshots = 0;
    let scenarios = observations["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 4);
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let min_y = scenario["min_y"].as_i64().unwrap() as i32;
        let block_height = scenario["height"].as_u64().unwrap() as u32;
        let height = DimensionHeight::new(min_y, block_height).unwrap();
        let mut dense = vec![[registry.air_id(); 4096]; (block_height / 16) as usize];
        for placement in scenario["placements"].as_array().unwrap() {
            apply(&mut dense, min_y, placement);
        }
        let initial = fixture::from_dense(
            Arc::clone(&registry),
            height,
            ChunkAddress { x: 0, z: 0 },
            &dense,
        );
        let mut cache = SkySources::initialize(&initial, ChunkAddress { x: 0, z: 0 }).unwrap();
        assert_columns(&cache, &scenario["initial"], name);
        snapshots += 1;
        for (step, operation) in scenario["operations"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let pos = apply(&mut dense, min_y, operation);
            let updated = fixture::from_dense(
                Arc::clone(&registry),
                height,
                ChunkAddress { x: 0, z: 0 },
                &dense,
            );
            assert_eq!(
                cache.update(&updated, pos).unwrap(),
                operation["changed"].as_bool().unwrap(),
                "{name}, update {step}"
            );
            assert_columns(
                &cache,
                &operation["after"],
                &format!("{name}, update {step}"),
            );
            snapshots += 1;
        }
    }
    assert_eq!(snapshots, 2008);
    eprintln!(
        "Matched {snapshots} complete source snapshots ({} columns) and 2004 update return values",
        snapshots * 256
    );
}
