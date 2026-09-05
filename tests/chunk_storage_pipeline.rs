//! Real file → bounded I/O → shared CPU → resident adoption → section encoding.
#[path = "common/world_registry_fixture.rs"]
mod registry_fixture;

use arrow_mc::{
    nbt::{self, Compound, NamedTag, Tag},
    runtime::{ChunkReadKey, CpuPool, CpuPoolConfig, ResidentChunkBudget, SectionKey},
    world::storage::{ChunkReadOutcome, ChunkStore, StorageLimits, chunk::DimensionHeight},
};
use std::{fs, path::Path, sync::Arc};

fn compound(entries: impl IntoIterator<Item = (&'static str, Tag)>) -> Tag {
    let mut value = Compound::new();
    for (name, entry) in entries {
        value.insert(name.into(), entry).unwrap();
    }
    Tag::Compound(value)
}

fn encoded_chunk(x: i32, z: i32) -> Vec<u8> {
    let block = compound([
        ("id", Tag::String("test:lamp".into())),
        (
            "properties",
            compound([
                ("facing", Tag::String("south".into())),
                ("lit", Tag::String("true".into())),
            ]),
        ),
    ]);
    let section = compound([
        ("Y", Tag::Byte(0)),
        (
            "block_states",
            compound([("palette", Tag::List(vec![block]))]),
        ),
        (
            "biomes",
            compound([(
                "palette",
                Tag::List(vec![Tag::String("minecraft:forest".into())]),
            )]),
        ),
    ]);
    let root = NamedTag {
        name: "unused root name".into(),
        tag: compound([
            ("DataVersion", Tag::Int(5018)),
            ("xPos", Tag::Int(x)),
            ("zPos", Tag::Int(z)),
            ("Status", Tag::String("minecraft:full".into())),
            ("sections", Tag::List(vec![section])),
            ("test:auxiliary", Tag::LongArray(vec![17, -29])),
        ]),
    };
    let mut bytes = Vec::new();
    nbt::write_named(&root, &mut bytes, nbt::Limits::default()).unwrap();
    bytes
}

fn write_raw_region(directory: &Path, x: i32, z: i32, nbt: &[u8]) {
    fs::create_dir_all(directory).unwrap();
    assert!(nbt.len() + 5 < 4096);
    let mut bytes = vec![0u8; 8192];
    let slot = (x.rem_euclid(32) + 32 * z.rem_euclid(32)) as usize * 4;
    bytes[slot..slot + 4].copy_from_slice(&((2u32 << 8) | 1).to_be_bytes());
    bytes.extend_from_slice(&((nbt.len() + 1) as i32).to_be_bytes());
    bytes.push(3); // Uncompressed Anvil version, still real disk/NBT loading.
    bytes.extend_from_slice(nbt);
    fs::write(
        directory.join(format!("r.{}.{}.mca", x.div_euclid(32), z.div_euclid(32))),
        bytes,
    )
    .unwrap();
}

#[test]
fn resident_adoption_frees_cpu_slot_before_using_same_pool_for_section_bytes() {
    let fixture = registry_fixture::Fixture::new();
    let registry = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    let key = ChunkReadKey {
        world_epoch: 9,
        chunk_x: -1,
        chunk_z: -33,
        generation: 71,
    };
    write_raw_region(
        &directory,
        key.chunk_x,
        key.chunk_z,
        &encoded_chunk(key.chunk_x, key.chunk_z),
    );
    let cpu = Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers: 2,
            max_jobs: 1,
            buffer_bytes: 64 * 1024 * 1024,
        })
        .unwrap(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = ChunkStore::new(
        directory,
        Arc::clone(&cpu),
        Arc::clone(&registry),
        DimensionHeight::new(-64, 384).unwrap(),
        StorageLimits::default(),
        2,
    )
    .unwrap();
    let ChunkReadOutcome::Decoded(output) = runtime.block_on(store.read(key)).unwrap() else {
        panic!("real chunk was not decoded")
    };
    assert_eq!(output.key(), key);
    assert_eq!(output.draft().position, (-1, -33));
    assert_eq!(cpu.stats().in_flight, 1);
    let charge = output.retained_bytes();
    assert!(
        (1..256 * 1024).contains(&charge),
        "small resident charged actual decode/storage, not its whole job cap"
    );
    let too_small = ResidentChunkBudget::new(charge - 1);
    let output = output.try_adopt(&too_small).unwrap_err().into_output();
    assert_eq!(cpu.stats().in_flight, 1);
    assert_eq!(too_small.stats().used_bytes, 0);
    let budget = ResidentChunkBudget::new(charge);
    let resident = output.try_adopt(&budget).unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
    assert_eq!(budget.stats().used_bytes, charge);
    assert_eq!(
        resident.draft().root().get(&"test:auxiliary".into()),
        Some(&Tag::LongArray(vec![17, -29]))
    );
    let section = resident.draft().sections()[0].section.as_ref().unwrap();
    assert_eq!(section.counts.non_empty_blocks, 4096);
    assert_eq!(section.counts.fluid_blocks, 4096);
    let section_key = SectionKey {
        world_epoch: key.world_epoch,
        chunk_x: key.chunk_x,
        chunk_z: key.chunk_z,
        section_y: 0,
        revision: 1,
    };
    let mut pending = cpu
        .try_reserve_section(
            section_key,
            registry.block_registry(),
            registry.biome_registry(),
            section.counts,
        )
        .unwrap();
    for (index, id) in pending.blocks_mut().iter_mut().enumerate() {
        *id = section.blocks.get(index).unwrap();
    }
    for (index, id) in pending.biomes_mut().iter_mut().enumerate() {
        *id = section.biomes.get(index).unwrap();
    }
    let result = pending.submit().unwrap().wait().unwrap();
    assert_eq!(result.bytes().unwrap(), &[0x10, 0, 0x10, 0, 0, 2, 0, 1]);
    drop(result);
    assert_eq!(cpu.stats().in_flight, 0);
    assert_eq!(budget.stats().chunks, 1);
    drop(resident);
    assert_eq!(budget.stats().used_bytes, 0);
    drop(store);
    Arc::try_unwrap(cpu).ok().unwrap().shutdown().unwrap();
}

#[test]
fn missing_chunk_does_not_create_a_region_or_consume_resident_capacity() {
    let fixture = registry_fixture::Fixture::new();
    let directory = fixture.root.join("missing-region");
    let cpu = Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers: 1,
            max_jobs: 1,
            buffer_bytes: 64 * 1024 * 1024,
        })
        .unwrap(),
    );
    let store = ChunkStore::new(
        directory.clone(),
        Arc::clone(&cpu),
        Arc::new(fixture.load()),
        DimensionHeight::new(-64, 384).unwrap(),
        StorageLimits::default(),
        1,
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(matches!(
        runtime
            .block_on(store.read(ChunkReadKey {
                world_epoch: 1,
                chunk_x: 0,
                chunk_z: 0,
                generation: 1
            }))
            .unwrap(),
        ChunkReadOutcome::Missing
    ));
    assert!(!directory.exists());
    assert_eq!(cpu.stats().in_flight, 0);
    drop(store);
    Arc::try_unwrap(cpu).ok().unwrap().shutdown().unwrap();
}
