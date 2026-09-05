use arrow_mc::world::preparation::ChunkAddress;
use arrow_mc::world::ticks::{
    MAX_SCHEDULED_TICKS_PER_PHASE, ScheduleOutcome, ScheduledTick, ScheduledTickOwner, TickDomain,
    TickError, TickLimits, TickPosition, TickPriority,
};

const BLOCK: TickDomain = TickDomain::Block;
const FLUID: TickDomain = TickDomain::Fluid;

fn limits() -> TickLimits {
    TickLimits {
        max_chunks: 4,
        queued_per_chunk: 32,
        selected_per_phase: 32,
        allocation_bytes: 128 * 1024,
    }
}

fn owner() -> ScheduledTickOwner {
    ScheduledTickOwner::new(16, 8, limits()).unwrap()
}

fn position(x: i32) -> TickPosition {
    TickPosition { x, y: 64, z: 0 }
}

fn register(owner: &mut ScheduledTickOwner, x: i32, eligible: bool) {
    owner
        .register_chunk(ChunkAddress { x, z: 0 }, eligible)
        .unwrap();
}

fn schedule(owner: &mut ScheduledTickOwner, id: u32, x: i32, time: i64, priority: TickPriority) {
    assert_eq!(
        owner.schedule(BLOCK, position(x), id, time, 0, priority),
        Ok(ScheduleOutcome::Added)
    );
}

fn run(
    owner: &mut ScheduledTickOwner,
    domain: TickDomain,
    time: i64,
    cap: usize,
) -> Vec<ScheduledTick> {
    let selected = owner.begin_phase(domain, time, cap).unwrap();
    let mut result = Vec::new();
    while let Some(tick) = owner.next_due().unwrap() {
        result.push(tick);
    }
    assert_eq!(selected, result.len());
    owner.finish_phase().unwrap();
    result
}

fn ids(ticks: Vec<ScheduledTick>) -> Vec<u32> {
    ticks.into_iter().map(|tick| tick.type_id).collect()
}

#[test]
fn duplicate_identity_keeps_first_time_priority_and_order() {
    let mut owner = owner();
    register(&mut owner, 0, true);
    schedule(&mut owner, 1, 1, 100, TickPriority::Low);
    assert_eq!(
        owner.schedule(BLOCK, position(1), 1, 0, 0, TickPriority::ExtremelyHigh),
        Ok(ScheduleOutcome::Duplicate)
    );
    schedule(&mut owner, 2, 1, 100, TickPriority::Normal);
    schedule(&mut owner, 1, 2, 100, TickPriority::Normal);
    assert_eq!(owner.next_sub_tick_order(), 4);
    assert_eq!(owner.queued_count(BLOCK), 3);
    assert!(run(&mut owner, BLOCK, 99, 32).is_empty());
    let ticks = run(&mut owner, BLOCK, 100, 32);
    assert_eq!(
        ticks.iter().map(|tick| tick.type_id).collect::<Vec<_>>(),
        [2, 1, 1]
    );
    assert_eq!(ticks[2].position, position(1));
    assert_eq!(
        (
            ticks[2].trigger_tick,
            ticks[2].priority,
            ticks[2].sub_tick_order
        ),
        (100, TickPriority::Low, 0)
    );
    assert!(!owner.has_scheduled(BLOCK, position(1), 1));
    assert_eq!(owner.queued_count(BLOCK), 0);
}

#[test]
fn overdue_order_depends_on_chunk_heads_and_does_not_globally_sort() {
    for (second_x, expected) in [(2, vec![1, 2]), (17, vec![2, 1])] {
        let mut owner = owner();
        register(&mut owner, 0, true);
        register(&mut owner, 1, true);
        schedule(&mut owner, 1, 1, 1, TickPriority::Low);
        schedule(&mut owner, 2, second_x, 9, TickPriority::ExtremelyHigh);
        assert_eq!(ids(run(&mut owner, BLOCK, 10, 32)), expected);
    }
    let mut owner = owner();
    register(&mut owner, 0, true);
    register(&mut owner, 1, true);
    schedule(&mut owner, 1, 1, 1, TickPriority::Low);
    schedule(&mut owner, 2, 2, 9, TickPriority::ExtremelyHigh);
    schedule(&mut owner, 3, 17, 5, TickPriority::Normal);
    assert_eq!(ids(run(&mut owner, BLOCK, 10, 32)), [3, 1, 2]);
}

#[test]
fn same_time_priority_and_suborder_interleave_chunk_heads() {
    let mut owner = owner();
    for chunk in [0, 1, 2] {
        register(&mut owner, chunk, true);
    }
    schedule(&mut owner, 1, 33, 10, TickPriority::Normal);
    schedule(&mut owner, 2, 1, 10, TickPriority::Normal);
    schedule(&mut owner, 3, 17, 10, TickPriority::Normal);
    schedule(&mut owner, 4, 18, 10, TickPriority::High);
    schedule(&mut owner, 5, 2, 10, TickPriority::High);
    assert_eq!(ids(run(&mut owner, BLOCK, 10, 32)), [4, 5, 1, 2, 3]);
}

#[test]
fn callback_queries_and_due_now_rescheduling_observe_collect_boundary() {
    let mut owner = owner();
    register(&mut owner, 0, true);
    schedule(&mut owner, 1, 1, 5, TickPriority::Normal);
    schedule(&mut owner, 2, 2, 5, TickPriority::Normal);
    assert!(owner.has_scheduled(BLOCK, position(1), 1));
    assert!(!owner.will_tick_this_phase(BLOCK, position(1), 1));
    assert_eq!(owner.begin_phase(BLOCK, 5, 32), Ok(2));
    assert_eq!(owner.queued_count(BLOCK), 0);
    assert!(!owner.has_scheduled(BLOCK, position(2), 2));
    assert!(owner.will_tick_this_phase(BLOCK, position(2), 2));
    assert_eq!(owner.next_due().unwrap().unwrap().type_id, 1);
    assert!(!owner.will_tick_this_phase(BLOCK, position(1), 1));
    for (id, priority) in [
        (1, TickPriority::Normal),
        (2, TickPriority::High),
        (3, TickPriority::ExtremelyHigh),
    ] {
        schedule(&mut owner, id, id as i32, 5, priority);
    }
    assert_eq!(owner.queued_count(BLOCK), 3);
    assert!(owner.has_scheduled(BLOCK, position(2), 2));
    assert!(owner.will_tick_this_phase(BLOCK, position(2), 2));
    assert!(!owner.will_tick_this_phase(BLOCK, position(3), 3));
    assert_eq!(owner.next_due().unwrap().unwrap().type_id, 2);
    assert!(!owner.will_tick_this_phase(BLOCK, position(2), 2));
    assert!(owner.has_scheduled(BLOCK, position(2), 2));
    assert_eq!(owner.next_due(), Ok(None));
    owner.finish_phase().unwrap();
    assert_eq!(ids(run(&mut owner, BLOCK, 5, 32)), [3, 2, 1]);
}

#[test]
fn block_then_fluid_phases_share_counter_and_allow_same_time_forward_effects() {
    let mut owner = owner();
    register(&mut owner, 0, true);
    schedule(&mut owner, 1, 1, 5, TickPriority::Normal);
    owner
        .schedule(FLUID, position(1), 1, 5, 0, TickPriority::Normal)
        .unwrap();
    owner.begin_phase(BLOCK, 5, 32).unwrap();
    let block = owner.next_due().unwrap().unwrap();
    assert_eq!(block.sub_tick_order, 0);
    assert!(!owner.will_tick_this_phase(FLUID, position(1), 1));
    owner
        .schedule(FLUID, position(2), 2, 5, 0, TickPriority::High)
        .unwrap();
    schedule(&mut owner, 3, 3, 5, TickPriority::Normal);
    assert_eq!(owner.next_due(), Ok(None));
    owner.finish_phase().unwrap();
    let fluid = run(&mut owner, FLUID, 5, 32);
    assert_eq!(
        fluid
            .iter()
            .map(|tick| (tick.type_id, tick.sub_tick_order))
            .collect::<Vec<_>>(),
        [(2, 2), (1, 1)]
    );
    assert_eq!(ids(run(&mut owner, BLOCK, 5, 32)), [3]);
    assert_eq!(owner.next_sub_tick_order(), 4);
}

#[test]
fn cap_zero_and_small_caps_keep_unselected_ticks_scheduled() {
    let mut owner = owner();
    register(&mut owner, 0, true);
    register(&mut owner, 1, true);
    schedule(&mut owner, 1, 1, 1, TickPriority::Normal);
    schedule(&mut owner, 2, 2, 2, TickPriority::High);
    schedule(&mut owner, 3, 17, 1, TickPriority::High);
    schedule(&mut owner, 4, 18, 2, TickPriority::Low);
    schedule(&mut owner, 5, 3, 100, TickPriority::ExtremelyHigh);
    assert!(run(&mut owner, BLOCK, 10, 0).is_empty());
    assert_eq!(owner.queued_count(BLOCK), 5);
    assert_eq!(owner.begin_phase(BLOCK, 10, 2), Ok(2));
    assert_eq!(owner.queued_count(BLOCK), 3);
    assert!(!owner.will_tick_this_phase(BLOCK, position(2), 2));
    assert!(owner.has_scheduled(BLOCK, position(2), 2));
    assert_eq!(owner.next_due().unwrap().unwrap().type_id, 3);
    assert_eq!(owner.next_due().unwrap().unwrap().type_id, 1);
    owner.finish_phase().unwrap();
    assert_eq!(ids(run(&mut owner, BLOCK, 10, 1)), [2]);
    assert_eq!(ids(run(&mut owner, BLOCK, 10, 32)), [4]);
    assert_eq!(ids(run(&mut owner, BLOCK, 100, 32)), [5]);
}

#[test]
fn full_phase_limit_collects_65536_and_leaves_one_queued() {
    let mut owner = ScheduledTickOwner::new(
        1,
        1,
        TickLimits {
            max_chunks: 1,
            queued_per_chunk: MAX_SCHEDULED_TICKS_PER_PHASE + 1,
            selected_per_phase: MAX_SCHEDULED_TICKS_PER_PHASE,
            allocation_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap();
    register(&mut owner, 0, true);
    for y in 0..=MAX_SCHEDULED_TICKS_PER_PHASE {
        owner
            .schedule(
                BLOCK,
                TickPosition {
                    x: 0,
                    y: y as i32,
                    z: 0,
                },
                0,
                0,
                0,
                TickPriority::Normal,
            )
            .unwrap();
    }
    let bytes = owner.retained_heap_bytes();
    assert_eq!(
        owner.begin_phase(BLOCK, 0, MAX_SCHEDULED_TICKS_PER_PHASE + 1),
        Err(TickError::PhaseLimit)
    );
    assert_eq!(owner.queued_count(BLOCK), 65537);
    assert_eq!(
        owner.begin_phase(BLOCK, 0, MAX_SCHEDULED_TICKS_PER_PHASE),
        Ok(65536)
    );
    assert_eq!(owner.queued_count(BLOCK), 1);
    for y in 0..MAX_SCHEDULED_TICKS_PER_PHASE {
        let tick = owner.next_due().unwrap().unwrap();
        assert_eq!(tick.position.y, y as i32);
        assert_eq!(tick.sub_tick_order, y as i64);
    }
    owner.finish_phase().unwrap();
    assert_eq!(run(&mut owner, BLOCK, 0, 1)[0].position.y, 65536);
    assert_eq!(owner.retained_heap_bytes(), bytes);
}

#[test]
fn eligibility_detachment_and_reattachment_keep_pending_data_without_admission() {
    let mut owner = owner();
    register(&mut owner, 0, false);
    schedule(&mut owner, 1, 1, 5, TickPriority::Normal);
    assert!(run(&mut owner, BLOCK, 10, 32).is_empty());
    assert!(owner.has_scheduled(BLOCK, position(1), 1));
    let bytes = owner.retained_heap_bytes();
    owner.detach_chunk(position(1).chunk()).unwrap();
    assert_eq!(owner.queued_count(BLOCK), 0);
    assert!(!owner.has_scheduled(BLOCK, position(1), 1));
    assert_eq!(
        owner.schedule(BLOCK, position(2), 2, 5, 0, TickPriority::High),
        Ok(ScheduleOutcome::MissingContainer)
    );
    assert_eq!(
        owner.set_eligible(position(1).chunk(), true),
        Err(TickError::MissingChunk)
    );
    assert_eq!(owner.retained_heap_bytes(), bytes);
    register(&mut owner, 0, false);
    assert_eq!(owner.queued_count(BLOCK), 1);
    assert!(run(&mut owner, BLOCK, 10, 32).is_empty());
    owner.set_eligible(position(1).chunk(), true).unwrap();
    assert_eq!(ids(run(&mut owner, BLOCK, 10, 32)), [1]);
    assert_eq!(owner.retained_heap_bytes(), bytes);
    assert_eq!(
        owner.schedule(BLOCK, position(49), 3, 0, 0, TickPriority::Normal),
        Ok(ScheduleOutcome::MissingContainer)
    );
    register(&mut owner, 3, true);
    assert!(run(&mut owner, BLOCK, 10, 32).is_empty());
    assert_eq!(owner.next_sub_tick_order(), 3);
}

#[test]
fn detach_during_phase_keeps_selected_ticks_and_discard_releases_queue_memory() {
    let mut owner = owner();
    let base = owner.retained_heap_bytes();
    register(&mut owner, 0, true);
    schedule(&mut owner, 1, 1, 1, TickPriority::Normal);
    schedule(&mut owner, 2, 2, 2, TickPriority::Normal);
    let chunk = position(0).chunk();
    assert_eq!(
        owner.discard_detached_chunk(chunk),
        Err(TickError::AlreadyRegistered)
    );
    owner.begin_phase(BLOCK, 10, 1).unwrap();
    owner.detach_chunk(chunk).unwrap();
    assert!(owner.will_tick_this_phase(BLOCK, position(1), 1));
    assert_eq!(owner.queued_count(BLOCK), 0);
    owner.discard_detached_chunk(chunk).unwrap();
    assert_eq!(owner.retained_heap_bytes(), base);
    assert_eq!(owner.next_due().unwrap().unwrap().type_id, 1);
    owner.finish_phase().unwrap();
    register(&mut owner, 0, true);
    assert!(run(&mut owner, BLOCK, 10, 32).is_empty());
}

#[test]
fn phase_errors_preserve_unconsumed_ticks_for_the_owner() {
    let mut owner = owner();
    register(&mut owner, 0, true);
    assert_eq!(owner.next_due(), Err(TickError::NoActivePhase));
    assert_eq!(owner.finish_phase(), Err(TickError::NoActivePhase));
    schedule(&mut owner, 1, 1, 0, TickPriority::Normal);
    owner.begin_phase(BLOCK, 0, 32).unwrap();
    assert_eq!(owner.finish_phase(), Err(TickError::UnconsumedTicks));
    assert_eq!(owner.begin_phase(FLUID, 0, 32), Err(TickError::PhaseActive));
    assert!(owner.will_tick_this_phase(BLOCK, position(1), 1));
    assert_eq!(owner.next_due().unwrap().unwrap().type_id, 1);
    assert_eq!(owner.next_due(), Ok(None));
    assert_eq!(owner.next_due(), Ok(None));
    owner.finish_phase().unwrap();
    assert_eq!(run(&mut owner, BLOCK, 0, 32).len(), 0);
}

#[test]
fn invalid_types_are_atomic_and_queue_limit_is_an_explicit_failure() {
    let mut owner = ScheduledTickOwner::new(
        2,
        1,
        TickLimits {
            queued_per_chunk: 1,
            ..limits()
        },
    )
    .unwrap();
    register(&mut owner, 0, true);
    let bytes = owner.retained_heap_bytes();
    assert_eq!(
        owner.schedule(BLOCK, position(1), 2, 0, 0, TickPriority::Normal),
        Err(TickError::InvalidType)
    );
    assert_eq!(
        owner.schedule(FLUID, position(1), 1, 0, 0, TickPriority::Normal),
        Err(TickError::InvalidType)
    );
    assert_eq!(owner.next_sub_tick_order(), 0);
    assert_eq!(owner.queued_count(BLOCK), 0);
    schedule(&mut owner, 1, 1, 0, TickPriority::Normal);
    assert_eq!(
        owner.schedule(BLOCK, position(1), 1, -1, 0, TickPriority::High),
        Ok(ScheduleOutcome::Duplicate)
    );
    assert_eq!(
        owner.schedule(BLOCK, position(2), 0, 0, 0, TickPriority::Normal),
        Err(TickError::QueueFull)
    );
    assert_eq!(owner.queued_count(BLOCK), 1);
    assert!(!owner.has_scheduled(BLOCK, position(2), 0));
    assert_eq!(owner.retained_heap_bytes(), bytes);
    owner
        .schedule(FLUID, position(2), 0, 0, 0, TickPriority::Normal)
        .unwrap();
    assert_eq!(ids(run(&mut owner, BLOCK, 0, 32)), [1]);
    schedule(&mut owner, 0, 2, 0, TickPriority::Normal);
    assert_eq!(ids(run(&mut owner, BLOCK, 0, 32)), [0]);
    assert_eq!(ids(run(&mut owner, FLUID, 0, 32)), [0]);
}

#[test]
fn explicit_memory_and_chunk_limits_fail_without_partial_registration() {
    let mut measured = owner();
    let base = measured.retained_heap_bytes();
    register(&mut measured, 0, true);
    let one_chunk = measured.retained_heap_bytes();
    drop(measured);
    assert!(matches!(
        ScheduledTickOwner::new(
            16,
            8,
            TickLimits {
                allocation_bytes: base - 1,
                ..limits()
            }
        ),
        Err(TickError::AllocationBudget)
    ));
    let mut limited = ScheduledTickOwner::new(
        16,
        8,
        TickLimits {
            allocation_bytes: one_chunk - 1,
            ..limits()
        },
    )
    .unwrap();
    assert_eq!(
        limited.register_chunk(ChunkAddress { x: 0, z: 0 }, true),
        Err(TickError::AllocationBudget)
    );
    assert_eq!(limited.retained_heap_bytes(), base);
    assert_eq!(
        limited.schedule(BLOCK, position(1), 1, 0, 0, TickPriority::Normal),
        Ok(ScheduleOutcome::MissingContainer)
    );

    let mut limited = ScheduledTickOwner::new(
        16,
        8,
        TickLimits {
            max_chunks: 1,
            ..limits()
        },
    )
    .unwrap();
    register(&mut limited, 0, true);
    assert_eq!(
        limited.register_chunk(position(0).chunk(), true),
        Err(TickError::AlreadyRegistered)
    );
    limited.detach_chunk(position(0).chunk()).unwrap();
    assert_eq!(
        limited.register_chunk(position(16).chunk(), true),
        Err(TickError::ChunkLimit)
    );
    limited.discard_detached_chunk(position(0).chunk()).unwrap();
    register(&mut limited, 1, true);
}

#[test]
fn negative_coordinate_boundaries_and_negative_delay_preserve_due_positions() {
    let mut owner = owner();
    for value in [-1, -16, -17, i32::MIN] {
        let position = TickPosition {
            x: value,
            y: -64,
            z: value,
        };
        let expected = ChunkAddress {
            x: value.div_euclid(16),
            z: value.div_euclid(16),
        };
        assert_eq!(position.chunk(), expected);
        assert_eq!(
            owner.register_chunk(expected, true),
            if value == -16 {
                Err(TickError::AlreadyRegistered)
            } else {
                Ok(())
            }
        );
        owner
            .schedule(BLOCK, position, 1, 100, -7, TickPriority::Normal)
            .unwrap();
    }
    assert!(run(&mut owner, BLOCK, 92, 32).is_empty());
    let actual = run(&mut owner, BLOCK, 93, 32);
    assert_eq!(
        actual
            .iter()
            .map(|tick| tick.position.x)
            .collect::<Vec<_>>(),
        [-1, -16, -17, i32::MIN]
    );
    assert!(actual.iter().all(|tick| tick.trigger_tick == 93));
}

#[test]
fn priority_normalization_and_invalid_configuration_boundaries() {
    assert_eq!(
        TickPriority::from_value(i32::MIN),
        TickPriority::ExtremelyHigh
    );
    assert_eq!(
        TickPriority::from_value(i32::MAX),
        TickPriority::ExtremelyLow
    );
    for value in -3..=3 {
        assert_eq!(TickPriority::from_value(value) as i32, value);
    }
    for invalid in [
        TickLimits {
            max_chunks: 0,
            ..limits()
        },
        TickLimits {
            queued_per_chunk: 0,
            ..limits()
        },
        TickLimits {
            selected_per_phase: 0,
            ..limits()
        },
        TickLimits {
            selected_per_phase: MAX_SCHEDULED_TICKS_PER_PHASE + 1,
            ..limits()
        },
    ] {
        assert!(matches!(
            ScheduledTickOwner::new(16, 8, invalid),
            Err(TickError::InvalidLimits)
        ));
    }
    assert!(matches!(
        ScheduledTickOwner::new(0, 8, limits()),
        Err(TickError::InvalidLimits)
    ));
    assert!(matches!(
        ScheduledTickOwner::new(16, 0, limits()),
        Err(TickError::InvalidLimits)
    ));
}
