//! Local release probe; deterministic live tick workload, not a game/TPS claim.

use arrow_mc::world::preparation::ChunkAddress;
use arrow_mc::world::ticks::{
    ScheduleOutcome, ScheduledTickOwner, TickDomain, TickLimits, TickPosition, TickPriority,
};
use std::time::Instant;

#[test]
#[ignore = "local release timing probe"]
fn live_schedule_collect_dispatch_cost() {
    for chunk_count in [1usize, 16, 256] {
        for duplicate in [false, true] {
            let per_chunk = 256usize;
            let ticks = chunk_count * per_chunk;
            let mut owner = ScheduledTickOwner::new(
                8,
                8,
                TickLimits {
                    max_chunks: chunk_count,
                    queued_per_chunk: per_chunk,
                    selected_per_phase: ticks,
                    allocation_bytes: 32 * 1024 * 1024,
                },
            )
            .unwrap();
            for chunk in 0..chunk_count {
                owner
                    .register_chunk(
                        ChunkAddress {
                            x: chunk as i32,
                            z: -1,
                        },
                        true,
                    )
                    .unwrap();
            }
            let retained = owner.retained_heap_bytes();
            let mut times = Vec::with_capacity(35);
            let mut checksum = 0u64;
            for round in 0..35 {
                let start = Instant::now();
                for chunk in 0..chunk_count {
                    for index in 0..per_chunk {
                        let position = TickPosition {
                            x: chunk as i32 * 16,
                            y: index as i32 - 64,
                            z: -1,
                        };
                        assert_eq!(
                            owner
                                .schedule(
                                    TickDomain::Block,
                                    position,
                                    (index % 8) as u32,
                                    round,
                                    -((index % 3) as i32),
                                    TickPriority::from_value(index as i32 % 7 - 3)
                                )
                                .unwrap(),
                            ScheduleOutcome::Added
                        );
                        if duplicate {
                            assert_eq!(
                                owner
                                    .schedule(
                                        TickDomain::Block,
                                        position,
                                        (index % 8) as u32,
                                        round,
                                        -100,
                                        TickPriority::ExtremelyHigh
                                    )
                                    .unwrap(),
                                ScheduleOutcome::Duplicate
                            );
                        }
                    }
                }
                assert_eq!(
                    owner.begin_phase(TickDomain::Block, round, ticks).unwrap(),
                    ticks
                );
                let mut dispatched = 0;
                while let Some(tick) = owner.next_due().unwrap() {
                    checksum =
                        checksum.wrapping_add(tick.sub_tick_order as u64 ^ tick.position.y as u64);
                    dispatched += 1;
                }
                assert_eq!(dispatched, ticks);
                owner.finish_phase().unwrap();
                assert_eq!(owner.queued_count(TickDomain::Block), 0);
                assert_eq!(owner.retained_heap_bytes(), retained);
                if round >= 5 {
                    times.push(start.elapsed().as_secs_f64());
                }
            }
            times.sort_by(f64::total_cmp);
            let median = times[times.len() / 2];
            println!(
                "{{\"chunks\":{chunk_count},\"ticks\":{ticks},\"duplicate_attempts\":{duplicate},\"cycle_p50_us\":{:.2},\"cycle_p95_us\":{:.2},\"ticks_per_second\":{:.2},\"retained_payload_bytes\":{retained},\"checksum\":{checksum}}}",
                median * 1e6,
                times[28] * 1e6,
                ticks as f64 / median
            );
        }
    }
}
