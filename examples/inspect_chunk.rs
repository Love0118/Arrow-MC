//! Read one Anvil chunk through the real I/O/CPU/adoption path. This does not
//! start a world, modify its files, or establish spawn/gameplay readiness.
use arrow_mc::{
    runtime::{ChunkReadKey, CpuPool, CpuPoolConfig, ResidentChunkBudget},
    server::configuration_data::parse_sha256,
    world::{
        section::MAX_SECTION_NETWORK_BYTES,
        storage::{
            ChunkReadOutcome, ChunkStore, StorageLimits,
            chunk::DimensionHeight,
            registry::{ChunkRegistrySnapshot, ExpectedRegistryReference, RegistryLoadLimits},
        },
    },
};
use std::{env, error::Error, io, path::PathBuf, sync::Arc, time::Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<_> = env::args().skip(1).collect();
    if arguments.len() != 8 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "Usage: inspect_chunk REGISTRY_DIR REGISTRY_MANIFEST_SHA256 CONFIGURATION_MANIFEST_SHA256 REGION_DIR CHUNK_X CHUNK_Z MIN_Y HEIGHT").into());
    }
    let expected = ExpectedRegistryReference {
        manifest_sha256: parse_sha256(&arguments[1])?,
        configuration_manifest_sha256: parse_sha256(&arguments[2])?,
        source_jar_sha256: parse_sha256(
            "18d6ad2986227ea55eb18f8ee6929999a4c48c0bbd623c36af3d2f64d3180e4a",
        )?,
        source_jar_bytes: 26_649_663,
    };
    let registries = Arc::new(ChunkRegistrySnapshot::load(
        &PathBuf::from(&arguments[0]),
        &expected,
        RegistryLoadLimits::default(),
    )?);
    let cpu = Arc::new(CpuPool::new(CpuPoolConfig {
        workers: 2,
        max_jobs: 4,
        buffer_bytes: 128 * 1024 * 1024,
    })?);
    let resident_budget = ResidentChunkBudget::new(64 * 1024 * 1024);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let store = ChunkStore::new(
        PathBuf::from(&arguments[3]),
        Arc::clone(&cpu),
        registries,
        DimensionHeight::new(arguments[6].parse()?, arguments[7].parse()?)?,
        StorageLimits::default(),
        2,
    )?;
    let key = ChunkReadKey {
        world_epoch: 1,
        chunk_x: arguments[4].parse()?,
        chunk_z: arguments[5].parse()?,
        generation: 1,
    };
    let start = Instant::now();
    let outcome = runtime.block_on(store.read(key))?;
    match outcome {
        ChunkReadOutcome::Missing => println!("{}", serde_json::json!({"result":"missing"})),
        ChunkReadOutcome::Unavailable(reason) => println!(
            "{}",
            serde_json::json!({"result":"unavailable", "reason":format!("{reason:?}")})
        ),
        ChunkReadOutcome::Decoded(output) => {
            let resident = output
                .try_adopt(&resident_budget)
                .map_err(|_| io::Error::other("resident byte budget could not adopt this chunk"))?;
            let draft = resident.draft();
            let mut encoded = Vec::new();
            encoded.try_reserve_exact(MAX_SECTION_NETWORK_BYTES)?;
            let mut payload_bytes = 0;
            let mut sections = 0;
            for stored in draft.sections() {
                if let Some(section) = &stored.section {
                    encoded.clear();
                    section.write_network(&mut encoded)?;
                    payload_bytes += encoded.len();
                    sections += 1;
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "result":"decoded", "position":draft.position, "data_version":draft.data_version,
                    "status":format!("{:?}",draft.status), "stored_sections":draft.sections().len(),
                    "network_sections":sections, "section_payload_bytes":payload_bytes,
                    "read_decode_adopt_encode_us":start.elapsed().as_micros(),
                    "cpu_peak_requested_buffer_bytes":cpu.stats().peak_reserved_buffer_bytes,
                    "resident_requested_backing_bytes":resident.retained_bytes(),
                    "resident_budget_used_bytes":resident_budget.stats().used_bytes,
                    "scope":"one chunk draft and section encoding; no spawn/lighting/player readiness or RSS claim"
                })
            );
            drop(resident);
        }
    }
    drop(store);
    Arc::try_unwrap(cpu)
        .map_err(|_| io::Error::other("outstanding CPU owner"))?
        .shutdown()
        .map_err(|_| io::Error::other("CPU worker failed"))?;
    Ok(())
}
