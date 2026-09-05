#[path = "common/lighting_fixture.rs"]
mod fixture;

use arrow_mc::{
    runtime::{
        AdmissionError, CpuPool, CpuPoolConfig, LightingAdoptionReason, LightingCompletion,
        PacketOperation, ResidentLightingBudget,
    },
    server::compression::CompressionLimits,
    world::{
        lighting::{
            LightBlock, LightKind, LightingSource,
            block::BlockLightLimits,
            sky::SkyLimits,
            storage::StorageLimits,
            work::{LightingLimits, SkyWorkLimits},
        },
        preparation::ChunkAddress,
        storage::{chunk::DimensionHeight, registry::ChunkRegistrySnapshot},
    },
};
use std::sync::Arc;

const PROBE: LightBlock = LightBlock { x: 8, y: 1, z: 8 };
fn source_from(registry: Arc<ChunkRegistrySnapshot>) -> LightingSource {
    fixture::from_placements(
        registry,
        DimensionHeight::new(0, 32).unwrap(),
        &[ChunkAddress { x: 0, z: 0 }],
        &[(LightBlock { x: 8, y: 0, z: 8 }, fixture::BEDROCK)],
    )
}
fn source() -> LightingSource {
    source_from(fixture::synthetic_registry())
}
fn limits() -> LightingLimits {
    let storage = StorageLimits {
        max_sections: 64,
        max_columns: 16,
        max_notifications: 128,
        metadata_bytes: 1 << 20,
        layer_bytes: 1 << 20,
    };
    LightingLimits {
        max_chunks: 1,
        metadata_bytes: 8,
        block: BlockLightLimits {
            checks: 16,
            decreases: 32768,
            increases: 32768,
            queue_bytes: 2 << 20,
        },
        block_storage: storage,
        sky: Some(SkyWorkLimits {
            engine: SkyLimits {
                checks: 16,
                queue_entries: 32768,
                source_chunks: 1,
                planned_writes: 256,
            },
            storage,
            engine_bytes: 2 << 20,
        }),
    }
}
fn pool(source: &LightingSource, limits: LightingLimits) -> CpuPool {
    CpuPool::new(CpuPoolConfig {
        workers: 1,
        max_jobs: 1,
        buffer_bytes: limits.reservation_bytes().unwrap() + source.heap_bytes(),
    })
    .unwrap()
}
async fn completed(
    cpu: &CpuPool,
    source: LightingSource,
    limits: LightingLimits,
) -> LightingCompletion {
    let mut result = cpu
        .try_reserve_lighting(source, limits)
        .unwrap()
        .submit(64)
        .unwrap()
        .wait()
        .await
        .unwrap();
    let mut slices = 0;
    while !result.progress().unwrap().complete {
        result = result
            .into_pending()
            .unwrap_or_else(|_| panic!("pending"))
            .submit(64)
            .unwrap()
            .wait()
            .await
            .unwrap();
        slices += 1;
        assert!(slices < 1000);
    }
    result
}
fn packet_limits() -> CompressionLimits {
    CompressionLimits {
        max_frame_body_bytes: 64,
        max_uncompressed_bytes: 64,
    }
}

#[tokio::test]
async fn completed_adoption_releases_the_only_cpu_slot_for_packet_encoding() {
    let source = source();
    let cpu = pool(&source, limits());
    let result = completed(&cpu, source, limits()).await;
    let cpu_bytes = result.reserved_bytes();
    let resident_bytes = result.resident_bytes().unwrap();
    let before = result.light_level(LightKind::Sky, PROBE);
    assert!(resident_bytes < cpu_bytes);
    assert!(matches!(
        cpu.try_reserve_packet(
            PacketOperation::Encode { threshold: -1 },
            2,
            packet_limits()
        ),
        Err(AdmissionError::JobLimit)
    ));
    let budget = ResidentLightingBudget::new(resident_bytes);
    let resident = result.try_adopt(&budget).unwrap();
    assert_eq!(
        (cpu.stats().in_flight, cpu.stats().reserved_buffer_bytes),
        (0, 0)
    );
    assert_eq!(resident.retained_bytes(), resident_bytes);
    assert_eq!(resident.light_level(LightKind::Sky, PROBE), before);
    assert_eq!(budget.stats().results, 1);
    assert_eq!(budget.stats().used_bytes, resident_bytes);
    let mut packet = cpu
        .try_reserve_packet(
            PacketOperation::Encode { threshold: -1 },
            2,
            packet_limits(),
        )
        .unwrap();
    packet.input_mut().copy_from_slice(&[6, 7]);
    let output = packet.submit().unwrap().wait().await.unwrap();
    assert_eq!(output.bytes(), &[2, 6, 7]);
    assert_eq!(budget.stats().used_bytes, resident_bytes);
    drop(output);
    drop(resident);
    assert_eq!(budget.stats().used_bytes, 0);
    assert_eq!(budget.stats().results, 0);
    assert_eq!(budget.stats().peak_bytes, resident_bytes);
    eprintln!(
        "completed lighting: CPU reservation={cpu_bytes}, resident charge={resident_bytes}; adoption moves existing payload"
    );
    cpu.shutdown().unwrap();
}

#[tokio::test]
async fn one_byte_short_adoption_preserves_completion_and_cpu_reservation_for_retry() {
    let source = source();
    let cpu = pool(&source, limits());
    let result = completed(&cpu, source, limits()).await;
    let bytes = result.resident_bytes().unwrap();
    let cpu_bytes = result.reserved_bytes();
    let progress = result.progress().unwrap();
    let before = result.light_level(LightKind::Sky, PROBE);
    let short = ResidentLightingBudget::new(bytes - 1);
    let error = result.try_adopt(&short).err().expect("one byte short");
    assert_eq!(error.reason(), LightingAdoptionReason::ByteLimit);
    assert_eq!(short.stats().used_bytes, 0);
    assert_eq!(short.stats().results, 0);
    assert_eq!(
        (cpu.stats().in_flight, cpu.stats().reserved_buffer_bytes),
        (1, cpu_bytes)
    );
    let result = error.into_completion();
    assert_eq!(result.progress().unwrap(), progress);
    assert_eq!(result.light_level(LightKind::Sky, PROBE), before);
    let exact = ResidentLightingBudget::new(bytes);
    let resident = result.try_adopt(&exact).unwrap();
    assert_eq!(resident.light_level(LightKind::Sky, PROBE), before);
    assert_eq!(cpu.stats().in_flight, 0);
    drop(resident);
    assert_eq!(exact.stats().used_bytes, 0);
    cpu.shutdown().unwrap();
}

#[tokio::test]
async fn cloned_budget_shares_capacity_and_retries_after_another_resident_is_dropped() {
    let source = source();
    let cpu = pool(&source, limits());
    let first = completed(&cpu, source, limits()).await;
    let bytes = first.resident_bytes().unwrap();
    let budget = ResidentLightingBudget::new(bytes);
    let shared = budget.clone();
    let first = first.try_adopt(&budget).unwrap();
    let second = completed(&cpu, self::source(), limits()).await;
    assert_eq!(second.resident_bytes().unwrap(), bytes);
    let error = second
        .try_adopt(&shared)
        .err()
        .expect("shared resident capacity");
    assert_eq!(error.reason(), LightingAdoptionReason::ByteLimit);
    assert_eq!(shared.stats(), budget.stats());
    assert_eq!(shared.stats().used_bytes, bytes);
    assert_eq!(cpu.stats().in_flight, 1);
    drop(first);
    assert_eq!(shared.stats().used_bytes, 0);
    let second = error.into_completion().try_adopt(&shared).unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
    assert_eq!(budget.stats().used_bytes, bytes);
    drop(second);
    assert_eq!(budget.stats().results, 0);
    cpu.shutdown().unwrap();
}

#[tokio::test]
async fn coexisting_block_only_and_sky_residents_refund_only_their_own_charge() {
    let source = source();
    let cpu = pool(&source, limits());
    let sky = completed(&cpu, source, limits()).await;
    let sky_bytes = sky.resident_bytes().unwrap();
    let budget = ResidentLightingBudget::new(sky_bytes * 2);
    let sky = sky.try_adopt(&budget).unwrap();
    let mut block_limits = limits();
    block_limits.sky = None;
    let block = completed(&cpu, self::source(), block_limits).await;
    let block_bytes = block.resident_bytes().unwrap();
    assert!(block_bytes < sky_bytes);
    let block = block.try_adopt(&budget).unwrap();
    assert_eq!(budget.stats().results, 2);
    assert_eq!(budget.stats().used_bytes, sky_bytes + block_bytes);
    assert_eq!(block.light_level(LightKind::Sky, PROBE), None);
    assert_eq!(sky.light_level(LightKind::Sky, PROBE), Some(15));
    drop(sky);
    assert_eq!(budget.stats().results, 1);
    assert_eq!(budget.stats().used_bytes, block_bytes);
    assert_eq!(block.light_level(LightKind::Block, PROBE), Some(0));
    drop(block);
    assert_eq!(budget.stats().used_bytes, 0);
    assert_eq!(budget.stats().results, 0);
    assert_eq!(budget.stats().peak_bytes, sky_bytes + block_bytes);
    cpu.shutdown().unwrap();
}

#[tokio::test]
async fn resident_keeps_source_alive_after_the_last_budget_handle_is_dropped() {
    let registry = fixture::synthetic_registry();
    let weak = Arc::downgrade(&registry);
    let source = source_from(registry);
    let cpu = pool(&source, limits());
    let result = completed(&cpu, source, limits()).await;
    let budget = ResidentLightingBudget::new(result.resident_bytes().unwrap());
    let resident = result.try_adopt(&budget).unwrap();
    assert!(weak.upgrade().is_some());
    drop(budget);
    assert_eq!(resident.light_level(LightKind::Sky, PROBE), Some(15));
    cpu.shutdown().unwrap();
    assert!(weak.upgrade().is_some());
    drop(resident);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn incomplete_and_failed_results_cannot_enter_resident_budget() {
    let source = source();
    let cpu = pool(&source, limits());
    let result = cpu
        .try_reserve_lighting(source, limits())
        .unwrap()
        .submit(0)
        .unwrap()
        .wait()
        .await
        .unwrap();
    let bytes = result.reserved_bytes();
    let budget = ResidentLightingBudget::new(usize::MAX);
    let error = result.try_adopt(&budget).err().expect("incomplete");
    assert_eq!(error.reason(), LightingAdoptionReason::Incomplete);
    assert_eq!(budget.stats().results, 0);
    assert_eq!(cpu.stats().reserved_buffer_bytes, bytes);
    let pending = error
        .into_completion()
        .into_pending()
        .unwrap_or_else(|_| panic!("still resumable"));
    drop(pending);
    assert_eq!(cpu.stats().in_flight, 0);
    let mut invalid = limits();
    invalid.block.queue_bytes = 0;
    let result = cpu
        .try_reserve_lighting(self::source(), invalid)
        .unwrap()
        .submit(64)
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(result.progress().is_err());
    let error = result.try_adopt(&budget).err().expect("failed");
    assert_eq!(error.reason(), LightingAdoptionReason::Incomplete);
    assert_eq!(budget.stats().used_bytes, 0);
    assert_eq!(cpu.stats().in_flight, 1);
    drop(error);
    assert_eq!(cpu.stats().reserved_buffer_bytes, 0);
    cpu.shutdown().unwrap();
}
