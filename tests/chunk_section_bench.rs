//! Opt-in local measurements, not a cross-platform performance claim.
use arrow_mc::world::section::{
    ContainerKind, MAX_SECTION_NETWORK_BYTES, PalettedContainer, Registry, SectionCounts,
    prepare_section,
};
use std::{hint::black_box, time::Instant};

#[test]
#[ignore = "local timing/heap-payload measurements; run release with --ignored --nocapture"]
fn measure_section_preparation_and_retained_payload() {
    let registry = Registry::new(65536).unwrap();
    let biomes = std::array::from_fn(|i| (i % 9) as u32);
    let counts = SectionCounts {
        non_empty_blocks: 4096,
        fluid_blocks: 0,
    };
    println!(
        "distinct,container_stack_bytes,container_heap_bytes,wire_bytes,dense_input_bytes,prepare_p50_ns,prepare_p95_ns,prepare_p99_ns,packed_read_total_ns,dense_read_total_ns"
    );
    for distinct in [1, 2, 16, 17, 64, 256, 257, 4096] {
        let blocks = std::array::from_fn(|i| (i % distinct) as u32);
        let container =
            PalettedContainer::from_dense(ContainerKind::Blocks, registry, &blocks, 65536).unwrap();
        let mut output = Vec::with_capacity(MAX_SECTION_NETWORK_BYTES);
        let mut timings = Vec::with_capacity(200);
        for iteration in 0..220 {
            output.clear();
            let start = Instant::now();
            prepare_section(
                black_box(&blocks),
                &biomes,
                registry,
                registry,
                counts,
                &mut output,
            )
            .unwrap();
            let elapsed = start.elapsed().as_nanos();
            black_box(&output);
            if iteration >= 20 {
                timings.push(elapsed);
            }
        }
        timings.sort_unstable();
        let start = Instant::now();
        for _ in 0..100 {
            for i in 0..4096 {
                black_box(container.get(black_box(i)).unwrap());
            }
        }
        let packed_reads = start.elapsed().as_nanos();
        let start = Instant::now();
        for _ in 0..100 {
            for i in 0..4096 {
                black_box(blocks[black_box(i)]);
            }
        }
        let dense_reads = start.elapsed().as_nanos();
        println!(
            "{distinct},{},{},{},{},{},{},{},{packed_reads},{dense_reads}",
            size_of::<PalettedContainer>(),
            container.heap_bytes(),
            container.network_len(),
            size_of_val(&blocks),
            timings[99],
            timings[189],
            timings[197]
        );
    }
    let mut container = PalettedContainer::single(ContainerKind::Blocks, registry, 0).unwrap();
    for i in 1..16 {
        container.set(i, i as u32, 65536).unwrap();
    }
    let old = container.heap_bytes();
    container.set(16, 16, 65536).unwrap();
    println!(
        "growth_4_to_5_old_payload={old},replacement_payload={},coexisting_payload={}",
        container.heap_bytes(),
        old + container.heap_bytes()
    );
    println!(
        "Values are Vec-capacity payload and synthetic local timings, excluding allocator metadata/RSS, worker stacks, queues, and OS effects."
    );
}
