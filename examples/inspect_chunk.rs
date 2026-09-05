//! Read one Anvil chunk through the real I/O/CPU/canonical owner path. This does not
//! start a world, modify its files, or establish spawn/gameplay readiness.
use arrow_mc::{
    runtime::{CpuPool, CpuPoolConfig},
    server::configuration_data::parse_sha256,
    world::{
        loading::{ChunkLoadingOwner, LoadDemand, LoadingLimits, LoadingReadOutcome},
        preparation::ChunkAddress,
        storage::{
            ChunkStore, StorageLimits,
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
    let height = DimensionHeight::new(arguments[6].parse()?, arguments[7].parse()?)?;
    let mut owner = ChunkLoadingOwner::new(
        1,
        Arc::clone(&registries),
        height,
        true,
        LoadingLimits {
            max_chunks: 1,
            metadata_bytes: 64 * 1024,
        },
        64 * 1024 * 1024,
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let store = ChunkStore::new(
        PathBuf::from(&arguments[3]),
        Arc::clone(&cpu),
        registries,
        height,
        StorageLimits::default(),
        2,
    )?;
    let address = ChunkAddress {
        x: arguments[4].parse()?,
        z: arguments[5].parse()?,
    };
    let LoadDemand::Read(request) = owner.request(address)? else {
        return Err(io::Error::other("new owner unexpectedly retained this request").into());
    };
    let key = request.key();
    let start = Instant::now();
    let outcome = runtime.block_on(request.read(&store))?;
    match outcome {
        LoadingReadOutcome::Missing => {
            owner.finish_without_chunk(key)?;
            println!("{}", serde_json::json!({"result":"missing"}));
        }
        LoadingReadOutcome::Unavailable(reason) => {
            owner.finish_without_chunk(key)?;
            println!(
                "{}",
                serde_json::json!({"result":"unavailable", "reason":format!("{reason:?}")})
            );
        }
        LoadingReadOutcome::Decoded(output) => {
            let publication = owner.publish(output)?;
            let resident = owner
                .resident(address)
                .ok_or_else(|| io::Error::other("publication did not retain its chunk"))?;
            let draft = resident.draft();
            let mut payload_bytes = 0;
            let mut sections = 0;
            for y in i32::from(height.min_section())..=i32::from(height.max_section()) {
                let completion = owner
                    .prepare_section(address, y, &cpu)?
                    .wait()
                    .ok_or_else(|| io::Error::other("section preparation was cancelled"))?;
                let prepared = owner.accept_prepared(completion)?;
                payload_bytes += prepared.bytes().len();
                sections += 1;
            }
            println!(
                "{}",
                serde_json::json!({
                    "result":"resident", "requested_position":[address.x,address.z],
                    "stored_position":draft.position, "relocated":publication.relocated.is_some(),
                    "data_version":draft.data_version,
                    "status":format!("{:?}",draft.status), "stored_sections":draft.sections().len(),
                    "network_sections":sections, "section_payload_bytes":payload_bytes,
                    "read_decode_publish_prepare_us":start.elapsed().as_micros(),
                    "cpu_peak_requested_buffer_bytes":cpu.stats().peak_reserved_buffer_bytes,
                    "resident_requested_backing_bytes":resident.retained_bytes(),
                    "resident_budget_used_bytes":owner.stats().resident_bytes,
                    "owner_metadata_bytes":owner.stats().metadata_bytes,
                    "scope":"one canonical chunk and shared CPU section encoding; no spawn/lighting/player readiness or RSS claim"
                })
            );
        }
    }
    drop(owner);
    drop(store);
    Arc::try_unwrap(cpu)
        .map_err(|_| io::Error::other("outstanding CPU owner"))?
        .shutdown()
        .map_err(|_| io::Error::other("CPU worker failed"))?;
    Ok(())
}
