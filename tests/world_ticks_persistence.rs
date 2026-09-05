use arrow_mc::world::preparation::ChunkAddress;
use arrow_mc::world::ticks::{
    CopyOutcome, SavedTick, ScheduleOutcome, ScheduledTick, ScheduledTickOwner, TickBounds,
    TickDomain, TickError, TickLimits, TickPosition, TickPriority,
};

const BLOCK: TickDomain = TickDomain::Block;
const FLUID: TickDomain = TickDomain::Fluid;

fn limits() -> TickLimits {
    TickLimits {
        max_chunks: 8,
        queued_per_chunk: 16,
        selected_per_phase: 16,
        allocation_bytes: 256 * 1024,
    }
}

fn owner(limits: TickLimits) -> ScheduledTickOwner {
    ScheduledTickOwner::new(32, 16, limits).unwrap()
}

fn position(x: i32) -> TickPosition {
    TickPosition { x, y: 64, z: 0 }
}
fn chunk(x: i32) -> ChunkAddress {
    ChunkAddress { x, z: 0 }
}
fn bounds(first: i32, last: i32) -> TickBounds {
    TickBounds {
        min: position(first),
        max: position(last),
    }
}
fn offset(x: i32) -> TickPosition {
    TickPosition { x, y: 0, z: 0 }
}
fn saved(id: u32, x: i32, delay: i32) -> SavedTick {
    SavedTick {
        position: position(x),
        type_id: id,
        delay,
        priority: TickPriority::Normal,
    }
}

fn schedule(owner: &mut ScheduledTickOwner, id: u32, x: i32, time: i64, priority: TickPriority) {
    assert_eq!(
        owner.schedule(BLOCK, position(x), id, time, 0, priority),
        Ok(ScheduleOutcome::Added)
    );
}

fn pack(
    owner: &mut ScheduledTickOwner,
    address: ChunkAddress,
    domain: TickDomain,
    time: i64,
) -> Vec<SavedTick> {
    let mut output = Vec::with_capacity(64);
    let capacity = output.capacity();
    let count = owner
        .pack_chunk(address, domain, time, &mut output)
        .unwrap();
    assert_eq!(count, output.len());
    assert_eq!(output.capacity(), capacity);
    output
}

fn run(
    owner: &mut ScheduledTickOwner,
    domain: TickDomain,
    time: i64,
    cap: usize,
) -> Vec<ScheduledTick> {
    let count = owner.begin_phase(domain, time, cap).unwrap();
    let mut ticks = Vec::new();
    while let Some(tick) = owner.next_due().unwrap() {
        ticks.push(tick);
    }
    assert_eq!(count, ticks.len());
    owner.finish_phase().unwrap();
    ticks
}

#[test]
fn duplicate_saved_identities_survive_unpack_and_allow_readmission_after_first_selection() {
    let mut owner = owner(limits());
    owner
        .load_pending_chunk(chunk(0), &[saved(1, 1, 0), saved(1, 1, 5)], &[])
        .unwrap();
    assert_eq!(owner.queued_count(BLOCK), 0);
    assert!(!owner.has_scheduled(BLOCK, position(1), 1));
    owner.register_chunk(chunk(0), true).unwrap();
    assert_eq!(owner.queued_count(BLOCK), 2);
    assert!(owner.has_scheduled(BLOCK, position(1), 1));
    assert_eq!(
        owner.schedule(BLOCK, position(1), 1, 0, 0, TickPriority::ExtremelyHigh),
        Ok(ScheduleOutcome::Duplicate)
    );
    assert!(run(&mut owner, BLOCK, 100, 16).is_empty());
    owner.unpack_chunk(chunk(0), 100).unwrap();
    assert_eq!(owner.begin_phase(BLOCK, 100, 1), Ok(1));
    assert_eq!(owner.queued_count(BLOCK), 1);
    assert!(!owner.has_scheduled(BLOCK, position(1), 1));
    let first = owner.next_due().unwrap().unwrap();
    assert_eq!((first.trigger_tick, first.sub_tick_order), (100, -2));
    schedule(&mut owner, 1, 1, 101, TickPriority::High);
    assert_eq!(owner.queued_count(BLOCK), 2);
    owner.finish_phase().unwrap();
    let remaining = run(&mut owner, BLOCK, 105, 16);
    assert_eq!(
        remaining
            .iter()
            .map(|tick| (tick.trigger_tick, tick.sub_tick_order))
            .collect::<Vec<_>>(),
        [(101, 1), (105, -1)]
    );
    assert_eq!(owner.next_sub_tick_order(), 2);
}

#[test]
fn unpack_assigns_independent_negative_domain_orders_once_and_frees_pending_backing() {
    let mut owner = owner(limits());
    let blocks = [saved(1, 1, 0), saved(2, 2, -5), saved(3, 3, 0)];
    let fluids = [saved(4, 4, 0), saved(5, 5, 0)];
    owner
        .load_pending_chunk(chunk(0), &blocks, &fluids)
        .unwrap();
    owner.register_chunk(chunk(0), true).unwrap();
    schedule(&mut owner, 6, 6, 100, TickPriority::Normal);
    let before = owner.retained_heap_bytes();
    owner.unpack_chunk(chunk(0), 100).unwrap();
    let after = owner.retained_heap_bytes();
    assert!(before - after >= (blocks.len() + fluids.len()) * size_of::<SavedTick>());
    let packed = pack(&mut owner, chunk(0), BLOCK, 100);
    owner.release_operation_scratch();
    owner.unpack_chunk(chunk(0), 500).unwrap();
    assert_eq!(owner.retained_heap_bytes(), after);
    assert_eq!(pack(&mut owner, chunk(0), BLOCK, 100), packed);
    assert_eq!(owner.next_sub_tick_order(), 1);
    let actual = run(&mut owner, BLOCK, 100, 16);
    assert_eq!(
        actual
            .iter()
            .map(|tick| (tick.type_id, tick.trigger_tick, tick.sub_tick_order))
            .collect::<Vec<_>>(),
        [(2, 95, -2), (1, 100, -3), (3, 100, -1), (6, 100, 0)]
    );
    assert_eq!(
        run(&mut owner, FLUID, 100, 16)
            .iter()
            .map(|tick| (tick.type_id, tick.sub_tick_order))
            .collect::<Vec<_>>(),
        [(4, -2), (5, -1)]
    );
}

#[test]
fn pending_load_filters_wrong_chunks_without_reordering_or_deduplicating() {
    let mut owner = owner(limits());
    let mut input = vec![
        saved(1, -17, 1),
        saved(2, -16, 2),
        saved(3, -1, 3),
        saved(4, 0, 4),
        saved(5, 15, 5),
        saved(6, 16, 6),
        saved(4, 0, 7),
    ];
    input.push(saved(u32::MAX, 32, 8));
    for (x, expected) in [
        (-2, vec![input[0]]),
        (-1, vec![input[1], input[2]]),
        (0, vec![input[3], input[4], input[6]]),
        (1, vec![input[5]]),
    ] {
        owner.load_pending_chunk(chunk(x), &input, &[]).unwrap();
        assert_eq!(pack(&mut owner, chunk(x), BLOCK, 999), expected);
        owner.register_chunk(chunk(x), true).unwrap();
        owner.unpack_chunk(chunk(x), 100).unwrap();
    }
    assert_eq!(owner.queued_count(BLOCK), 7);
    assert_eq!(run(&mut owner, BLOCK, 200, 16).len(), 7);
    assert_eq!(owner.queued_count(BLOCK), 0);
}

#[test]
fn pack_appends_pending_then_live_suborder_and_preserves_output_on_short_capacity() {
    let mut owner = owner(limits());
    let pending = [saved(1, 1, 3), saved(2, 2, -4)];
    owner.load_pending_chunk(chunk(0), &pending, &[]).unwrap();
    owner.register_chunk(chunk(0), true).unwrap();
    schedule(&mut owner, 3, 3, 130, TickPriority::High);
    schedule(&mut owner, 4, 4, 105, TickPriority::Low);
    assert_eq!(
        pack(&mut owner, chunk(0), BLOCK, 100),
        [
            pending[0],
            pending[1],
            SavedTick {
                priority: TickPriority::High,
                ..saved(3, 3, 30)
            },
            SavedTick {
                priority: TickPriority::Low,
                ..saved(4, 4, 5)
            }
        ]
    );
    let marker = saved(31, 0, -100);
    let mut output = vec![marker];
    let before = output.clone();
    assert_eq!(
        owner.pack_chunk(chunk(0), BLOCK, 100, &mut output),
        Err(TickError::OutputCapacity)
    );
    assert_eq!(output, before);
    let mut output = Vec::with_capacity(5);
    output.push(marker);
    assert_eq!(owner.pack_chunk(chunk(0), BLOCK, 100, &mut output), Ok(4));
    assert_eq!(output[0], marker);
    assert_eq!(owner.queued_count(BLOCK), 4);
    owner.unpack_chunk(chunk(0), 100).unwrap();
    assert_eq!(
        pack(&mut owner, chunk(0), BLOCK, 102)
            .iter()
            .map(|tick| (tick.type_id, tick.delay))
            .collect::<Vec<_>>(),
        [(1, 1), (2, -6), (3, 28), (4, 3)]
    );
}

#[test]
fn pack_and_unpack_use_signed_narrowing_and_wrapping_time_arithmetic() {
    let mut owner = owner(limits());
    owner.register_chunk(chunk(0), true).unwrap();
    for (id, time) in [
        (1, (1_i64 << 31) + 3),
        (2, -(1_i64 << 31) - 3),
        (3, i64::MIN),
    ] {
        schedule(&mut owner, id, id as i32, time, TickPriority::Normal);
    }
    assert_eq!(
        pack(&mut owner, chunk(0), BLOCK, 0)
            .iter()
            .map(|tick| tick.delay)
            .collect::<Vec<_>>(),
        [-2147483645, 2147483645, 0]
    );
    assert_eq!(pack(&mut owner, chunk(0), BLOCK, i64::MAX)[2].delay, 1);
    owner
        .load_pending_chunk(chunk(1), &[saved(4, 17, 1)], &[saved(5, 18, -1)])
        .unwrap();
    owner.register_chunk(chunk(1), true).unwrap();
    owner.unpack_chunk(chunk(1), i64::MAX).unwrap();
    let selected = run(&mut owner, BLOCK, i64::MIN, 16);
    assert!(
        selected
            .iter()
            .any(|tick| tick.type_id == 4 && tick.trigger_tick == i64::MIN)
    );
    assert_eq!(
        run(&mut owner, FLUID, i64::MAX, 16)[0].trigger_tick,
        i64::MAX - 1
    );
}

#[test]
fn load_budget_failure_and_invalid_input_leave_no_partial_chunk_or_counter_change() {
    let config = TickLimits {
        max_chunks: 1,
        queued_per_chunk: 4,
        selected_per_phase: 4,
        ..limits()
    };
    let blocks = [saved(1, 1, 0), saved(2, 2, 0), saved(3, 3, 0)];
    let mut measured = owner(config);
    let baseline = measured.retained_heap_bytes();
    measured.load_pending_chunk(chunk(0), &blocks, &[]).unwrap();
    let needed = measured.retained_heap_bytes();
    drop(measured);
    let mut limited = owner(TickLimits {
        allocation_bytes: needed - 1,
        ..config
    });
    assert_eq!(
        limited.load_pending_chunk(chunk(0), &blocks, &[]),
        Err(TickError::AllocationBudget)
    );
    assert_eq!(limited.retained_heap_bytes(), baseline);
    assert_eq!(limited.next_sub_tick_order(), 0);
    assert_eq!(
        limited.unpack_chunk(chunk(0), 0),
        Err(TickError::MissingChunk)
    );
    assert_eq!(
        limited.load_pending_chunk(chunk(0), &blocks, &[saved(16, 4, 0)]),
        Err(TickError::InvalidType)
    );
    assert_eq!(limited.retained_heap_bytes(), baseline);
    assert_eq!(
        limited.load_pending_chunk(chunk(0), &[saved(1, 1, 0); 5], &[]),
        Err(TickError::QueueFull)
    );
    limited.register_chunk(chunk(0), true).unwrap();
    assert_eq!(
        limited.load_pending_chunk(chunk(0), &[], &[]),
        Err(TickError::ChunkAlreadyPresent)
    );
    assert_eq!(limited.queued_count(BLOCK), 0);
}

#[test]
fn pending_entries_occupy_queue_capacity_before_unpack_and_capacity_reopens_after_selection() {
    let mut owner = owner(TickLimits {
        queued_per_chunk: 2,
        ..limits()
    });
    owner
        .load_pending_chunk(chunk(0), &[saved(1, 1, 0), saved(1, 1, 1)], &[])
        .unwrap();
    owner.register_chunk(chunk(0), true).unwrap();
    assert_eq!(
        owner.schedule(BLOCK, position(1), 1, 0, 0, TickPriority::Normal),
        Ok(ScheduleOutcome::Duplicate)
    );
    assert_eq!(
        owner.schedule(BLOCK, position(2), 2, 0, 0, TickPriority::Normal),
        Err(TickError::QueueFull)
    );
    owner.unpack_chunk(chunk(0), 100).unwrap();
    assert_eq!(
        owner.schedule(BLOCK, position(2), 2, 0, 0, TickPriority::Normal),
        Err(TickError::QueueFull)
    );
    owner.begin_phase(BLOCK, 100, 1).unwrap();
    schedule(&mut owner, 2, 2, 100, TickPriority::Normal);
    owner.next_due().unwrap().unwrap();
    owner.finish_phase().unwrap();
    assert_eq!(
        run(&mut owner, BLOCK, 101, 16)
            .iter()
            .map(|tick| tick.type_id)
            .collect::<Vec<_>>(),
        [2, 1]
    );
}

#[test]
fn clear_area_ignores_pending_entries_and_repairs_changed_live_heads() {
    let mut owner = owner(limits());
    owner
        .load_pending_chunk(chunk(0), &[saved(1, 1, 0)], &[])
        .unwrap();
    owner.register_chunk(chunk(0), true).unwrap();
    schedule(&mut owner, 2, 2, 10, TickPriority::Normal);
    schedule(&mut owner, 3, 3, 20, TickPriority::Normal);
    assert_eq!(owner.clear_area(BLOCK, bounds(1, 2)), Ok(1));
    assert!(owner.has_scheduled(BLOCK, position(1), 1));
    assert_eq!(owner.queued_count(BLOCK), 2);
    owner.unpack_chunk(chunk(0), 10).unwrap();
    assert_eq!(
        run(&mut owner, BLOCK, 10, 16)
            .iter()
            .map(|tick| tick.type_id)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        run(&mut owner, BLOCK, 20, 16)
            .iter()
            .map(|tick| tick.type_id)
            .collect::<Vec<_>>(),
        [3]
    );
}

#[test]
fn clear_selected_and_already_run_entries_preserves_observed_lazy_query_difference() {
    for prequery in [false, true] {
        let mut source = owner(limits());
        let mut destination = owner(limits());
        source.register_chunk(chunk(0), true).unwrap();
        destination.register_chunk(chunk(2), true).unwrap();
        for id in 1..=3 {
            schedule(&mut source, id, id as i32, 10, TickPriority::Normal);
        }
        schedule(&mut source, 4, 4, 100, TickPriority::Normal);
        source.begin_phase(BLOCK, 10, 16).unwrap();
        assert_eq!(source.next_due().unwrap().unwrap().type_id, 1);
        if prequery {
            assert!(source.will_tick_this_phase(BLOCK, position(2), 2));
        }
        assert_eq!(source.clear_area(BLOCK, bounds(1, 2)), Ok(2));
        assert_eq!(source.clear_area(BLOCK, bounds(4, 4)), Ok(1));
        assert_eq!(source.will_tick_this_phase(BLOCK, position(2), 2), prequery);
        assert!(source.will_tick_this_phase(BLOCK, position(3), 3));
        assert_eq!(
            destination.copy_area_from(&source, BLOCK, bounds(1, 4), offset(32)),
            Ok(CopyOutcome {
                added: 1,
                ..CopyOutcome::default()
            })
        );
        assert_eq!(source.next_due().unwrap().unwrap().type_id, 3);
        assert_eq!(source.will_tick_this_phase(BLOCK, position(2), 2), prequery);
        assert_eq!(source.next_due(), Ok(None));
        source.finish_phase().unwrap();
        assert!(!source.will_tick_this_phase(BLOCK, position(2), 2));
        let copied = run(&mut destination, BLOCK, 10, 16);
        assert_eq!(
            (
                copied[0].type_id,
                copied[0].position.x,
                copied[0].sub_tick_order
            ),
            (3, 35, 3)
        );
        assert_eq!(source.queued_count(BLOCK), 0);
    }
}

#[test]
fn self_copy_zero_offset_selected_ticks_rerun_while_queued_ticks_deduplicate() {
    let mut owner = owner(limits());
    owner.register_chunk(chunk(0), true).unwrap();
    schedule(&mut owner, 1, 1, 5, TickPriority::Normal);
    schedule(&mut owner, 2, 2, 5, TickPriority::Normal);
    assert_eq!(
        owner.copy_area(BLOCK, bounds(1, 2), offset(0)),
        Ok(CopyOutcome {
            duplicates: 2,
            ..CopyOutcome::default()
        })
    );
    assert_eq!(owner.begin_phase(BLOCK, 5, 16), Ok(2));
    assert_eq!(owner.next_due().unwrap().unwrap().type_id, 1);
    assert_eq!(
        owner.copy_area(BLOCK, bounds(1, 2), offset(0)),
        Ok(CopyOutcome {
            added: 2,
            ..CopyOutcome::default()
        })
    );
    assert_eq!(owner.next_due().unwrap().unwrap().type_id, 2);
    owner.finish_phase().unwrap();
    assert_eq!(owner.next_sub_tick_order(), 2);
    let copied = run(&mut owner, BLOCK, 5, 16);
    assert_eq!(
        copied
            .iter()
            .map(|tick| (tick.type_id, tick.sub_tick_order))
            .collect::<Vec<_>>(),
        [(1, 2), (2, 3)]
    );
}

#[test]
fn copy_uses_already_run_remaining_and_queue_and_deduplicates_in_that_order() {
    let mut source = owner(limits());
    let mut destination = owner(limits());
    source.register_chunk(chunk(0), true).unwrap();
    destination.register_chunk(chunk(2), true).unwrap();
    for id in 1..=2 {
        schedule(&mut source, id, id as i32, 10, TickPriority::Normal);
    }
    source.begin_phase(BLOCK, 10, 16).unwrap();
    source.next_due().unwrap().unwrap();
    schedule(&mut source, 1, 1, 2, TickPriority::ExtremelyHigh);
    let before_counter = destination.next_sub_tick_order();
    assert_eq!(
        destination.copy_area_from(&source, BLOCK, bounds(1, 2), offset(32)),
        Ok(CopyOutcome {
            added: 2,
            duplicates: 1,
            missing_containers: 0
        })
    );
    assert_eq!(destination.next_sub_tick_order(), before_counter);
    let copied = run(&mut destination, BLOCK, 10, 16);
    assert_eq!(
        copied
            .iter()
            .map(|tick| (
                tick.type_id,
                tick.trigger_tick,
                tick.priority,
                tick.sub_tick_order
            ))
            .collect::<Vec<_>>(),
        [
            (1, 10, TickPriority::Normal, 3),
            (2, 10, TickPriority::Normal, 4)
        ]
    );
    source.next_due().unwrap().unwrap();
    source.finish_phase().unwrap();
    assert_eq!(
        destination.copy_area_from(&source, BLOCK, bounds(1, 2), offset(32)),
        Ok(CopyOutcome {
            added: 1,
            ..CopyOutcome::default()
        })
    );
    let after_cleanup = run(&mut destination, BLOCK, 10, 16);
    assert_eq!(
        (
            after_cleanup[0].type_id,
            after_cleanup[0].trigger_tick,
            after_cleanup[0].sub_tick_order
        ),
        (1, 2, 3)
    );
    assert_eq!(source.queued_count(BLOCK), 1);
}

#[test]
fn copy_keeps_trigger_priority_and_xyz_offset_and_ignores_destination_counter() {
    let mut source = owner(limits());
    let mut destination = owner(limits());
    source.register_chunk(chunk(0), true).unwrap();
    for x in [2, 3] {
        destination.register_chunk(chunk(x), true).unwrap();
    }
    schedule(&mut source, 1, 1, 5, TickPriority::Normal);
    schedule(&mut source, 2, 2, 5, TickPriority::Normal);
    schedule(&mut source, 3, 3, 100, TickPriority::High);
    schedule(&mut source, 8, 8, 5, TickPriority::Low);
    for _ in 0..8 {
        destination
            .schedule(BLOCK, position(96), 9, 0, 0, TickPriority::Normal)
            .unwrap();
    }
    schedule(&mut destination, 9, 49, 5, TickPriority::Normal);
    source.begin_phase(BLOCK, 5, 2).unwrap();
    source.next_due().unwrap().unwrap();
    assert_eq!(
        destination.copy_area_from(
            &source,
            BLOCK,
            bounds(1, 3),
            TickPosition { x: 32, y: 3, z: 1 }
        ),
        Ok(CopyOutcome {
            added: 3,
            ..CopyOutcome::default()
        })
    );
    assert_eq!(destination.next_sub_tick_order(), 9);
    let due = run(&mut destination, BLOCK, 5, 16);
    assert_eq!(
        due.iter()
            .map(|tick| (tick.type_id, tick.sub_tick_order))
            .collect::<Vec<_>>(),
        [(1, 3), (2, 4), (9, 8)]
    );
    assert_eq!(due[0].position, TickPosition { x: 33, y: 67, z: 1 });
    let future = run(&mut destination, BLOCK, 100, 16);
    assert_eq!(
        (
            future[0].type_id,
            future[0].trigger_tick,
            future[0].priority,
            future[0].sub_tick_order
        ),
        (3, 100, TickPriority::High, 5)
    );
    source.next_due().unwrap().unwrap();
    source.finish_phase().unwrap();
    assert_eq!(source.queued_count(BLOCK), 2);
}

#[test]
fn overlapping_self_copy_snapshots_once_and_keeps_existing_destination_identity() {
    let mut owner = owner(limits());
    owner.register_chunk(chunk(0), true).unwrap();
    schedule(&mut owner, 1, 1, 5, TickPriority::Normal);
    schedule(&mut owner, 1, 2, 10, TickPriority::High);
    assert_eq!(
        owner.copy_area(BLOCK, bounds(1, 2), offset(1)),
        Ok(CopyOutcome {
            added: 1,
            duplicates: 1,
            missing_containers: 0
        })
    );
    assert!(!owner.has_scheduled(BLOCK, position(4), 1));
    assert_eq!(
        pack(&mut owner, chunk(0), BLOCK, 0)
            .iter()
            .map(|tick| (tick.position.x, tick.delay, tick.priority))
            .collect::<Vec<_>>(),
        [
            (1, 5, TickPriority::Normal),
            (2, 10, TickPriority::High),
            (3, 10, TickPriority::High)
        ]
    );
    assert_eq!(owner.next_sub_tick_order(), 2);
}

#[test]
fn copied_equal_suborders_pack_in_heap_array_order_and_execute_java_ties() {
    let mut source = owner(limits());
    for (x, id, position) in [(0, 1, 15), (1, 2, 16)] {
        source
            .load_pending_chunk(chunk(x), &[saved(id, position, 0)], &[])
            .unwrap();
        source.register_chunk(chunk(x), true).unwrap();
        source.unpack_chunk(chunk(x), 100).unwrap();
    }
    let mut destination = owner(limits());
    destination.register_chunk(chunk(1), true).unwrap();
    schedule(&mut destination, 3, 18, 100, TickPriority::Normal);
    destination
        .copy_area_from(&source, BLOCK, bounds(15, 16), offset(1))
        .unwrap();
    assert_eq!(
        pack(&mut destination, chunk(1), BLOCK, 100)
            .iter()
            .map(|tick| tick.type_id)
            .collect::<Vec<_>>(),
        [3, 1, 2]
    );
    let ticks = run(&mut destination, BLOCK, 100, 16);
    assert_eq!(
        ticks
            .iter()
            .map(|tick| (tick.type_id, tick.sub_tick_order))
            .collect::<Vec<_>>(),
        [(3, 0), (2, 0), (1, 0)]
    );
}

#[test]
fn six_colliding_loaded_chunks_preserve_cap_zero_and_detach_order_history() {
    let xs = [62, 63, 114, 124, 164, 191];
    for (history, expected) in [
        ("fresh", [62, 63, 114, 124, 164, 191]),
        ("cap_zero", [62, 191, 164, 124, 114, 63]),
        ("detach", [63, 114, 124, 164, 191, 62]),
    ] {
        let mut owner = owner(limits());
        for x in xs {
            owner
                .load_pending_chunk(chunk(x), &[saved(1, x * 16, 0)], &[])
                .unwrap();
            owner.register_chunk(chunk(x), true).unwrap();
            owner.unpack_chunk(chunk(x), 100).unwrap();
        }
        match history {
            "cap_zero" => {
                assert!(run(&mut owner, BLOCK, 100, 0).is_empty());
            }
            "detach" => {
                owner.detach_chunk(chunk(62)).unwrap();
                owner.register_chunk(chunk(62), true).unwrap();
            }
            _ => {}
        }
        assert_eq!(
            run(&mut owner, BLOCK, 100, 16)
                .iter()
                .map(|tick| tick.position.chunk().x)
                .collect::<Vec<_>>(),
            expected,
            "history={history}"
        );
    }
}

#[test]
fn copy_queue_full_is_transactional_and_failed_batch_does_not_poison_retry() {
    let mut source = owner(limits());
    for x in [0, 1] {
        source.register_chunk(chunk(x), true).unwrap();
    }
    schedule(&mut source, 1, 1, 5, TickPriority::Normal);
    schedule(&mut source, 2, 17, 5, TickPriority::Normal);
    let mut destination = owner(TickLimits {
        max_chunks: 2,
        queued_per_chunk: 1,
        selected_per_phase: 4,
        ..limits()
    });
    for x in [2, 3] {
        destination.register_chunk(chunk(x), true).unwrap();
    }
    schedule(&mut destination, 9, 50, 5, TickPriority::Normal);
    let memory = destination.retained_heap_bytes();
    let counter = destination.next_sub_tick_order();
    assert_eq!(
        destination.copy_area_from(&source, BLOCK, bounds(1, 17), offset(32)),
        Err(TickError::QueueFull)
    );
    assert!(pack(&mut destination, chunk(2), BLOCK, 0).is_empty());
    assert_eq!(
        pack(&mut destination, chunk(3), BLOCK, 0),
        [saved(9, 50, 5)]
    );
    // Failed semantic admission may retain newly admitted reusable workspace;
    // explicit release returns it without changing source or queued ticks.
    destination.release_operation_scratch();
    assert_eq!(destination.retained_heap_bytes(), memory);
    assert_eq!(destination.next_sub_tick_order(), counter);
    destination.clear_area(BLOCK, bounds(50, 50)).unwrap();
    assert_eq!(
        destination.copy_area_from(&source, BLOCK, bounds(1, 17), offset(32)),
        Ok(CopyOutcome {
            added: 2,
            ..CopyOutcome::default()
        })
    );
    assert_eq!(
        run(&mut destination, BLOCK, 5, 4)
            .iter()
            .map(|tick| tick.type_id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn copy_snapshot_budget_failure_is_atomic_and_pending_lists_are_not_sources() {
    let mut source = owner(limits());
    source.register_chunk(chunk(0), true).unwrap();
    for id in 1..=3 {
        schedule(&mut source, id, id as i32, 5, TickPriority::Normal);
    }
    let mut destination = owner(TickLimits {
        max_chunks: 1,
        queued_per_chunk: 1,
        selected_per_phase: 1,
        ..limits()
    });
    destination.register_chunk(chunk(2), true).unwrap();
    let memory = destination.retained_heap_bytes();
    assert_eq!(
        destination.copy_area_from(&source, BLOCK, bounds(1, 3), offset(32)),
        Err(TickError::AllocationBudget)
    );
    assert_eq!(destination.queued_count(BLOCK), 0);
    assert_eq!(destination.next_sub_tick_order(), 0);
    assert_eq!(destination.retained_heap_bytes(), memory);
    assert_eq!(
        destination.copy_area_from(&source, BLOCK, bounds(1, 1), offset(32)),
        Ok(CopyOutcome {
            added: 1,
            ..CopyOutcome::default()
        })
    );
    assert_eq!(run(&mut destination, BLOCK, 5, 1)[0].type_id, 1);
    source
        .load_pending_chunk(chunk(1), &[saved(4, 17, 0)], &[])
        .unwrap();
    source.register_chunk(chunk(1), true).unwrap();
    assert_eq!(
        destination.copy_area_from(&source, BLOCK, bounds(17, 17), offset(16)),
        Ok(CopyOutcome::default())
    );
    assert_eq!(source.queued_count(BLOCK), 4);
    assert_eq!(
        destination.copy_area_from(&source, BLOCK, bounds(1, 1), offset(64)),
        Ok(CopyOutcome {
            missing_containers: 1,
            ..CopyOutcome::default()
        })
    );
}

#[test]
fn invalid_bounds_leave_phase_selected_queue_and_copy_destination_unchanged() {
    let mut owner = owner(limits());
    owner.register_chunk(chunk(0), true).unwrap();
    schedule(&mut owner, 1, 1, 5, TickPriority::Normal);
    schedule(&mut owner, 2, 2, 5, TickPriority::Normal);
    schedule(&mut owner, 3, 3, 100, TickPriority::Normal);
    owner.begin_phase(BLOCK, 5, 16).unwrap();
    owner.next_due().unwrap().unwrap();
    let bytes = owner.retained_heap_bytes();
    let counter = owner.next_sub_tick_order();
    let source = ScheduledTickOwner::new(32, 16, limits()).unwrap();
    for invalid in [
        bounds(2, 1),
        TickBounds {
            min: TickPosition { x: 0, y: 65, z: 0 },
            max: position(3),
        },
        TickBounds {
            min: TickPosition { x: 0, y: 64, z: 1 },
            max: position(3),
        },
    ] {
        assert_eq!(
            owner.clear_area(BLOCK, invalid),
            Err(TickError::InvalidBounds)
        );
        assert_eq!(
            owner.copy_area(BLOCK, invalid, offset(0)),
            Err(TickError::InvalidBounds)
        );
        assert_eq!(
            owner.copy_area_from(&source, BLOCK, invalid, offset(0)),
            Err(TickError::InvalidBounds)
        );
    }
    assert_eq!(owner.queued_count(BLOCK), 1);
    assert!(owner.will_tick_this_phase(BLOCK, position(2), 2));
    assert_eq!(owner.retained_heap_bytes(), bytes);
    assert_eq!(owner.next_sub_tick_order(), counter);
    assert_eq!(owner.next_due().unwrap().unwrap().type_id, 2);
    owner.finish_phase().unwrap();
    assert_eq!(run(&mut owner, BLOCK, 100, 16)[0].type_id, 3);
}

#[test]
fn pack_workspace_is_lazy_retained_and_explicitly_released() {
    let mut owner = owner(limits());
    owner.register_chunk(chunk(0), true).unwrap();
    let baseline = owner.retained_heap_bytes();
    schedule(&mut owner, 1, 1, 10, TickPriority::Normal);
    assert_eq!(owner.retained_heap_bytes(), baseline);
    owner.release_operation_scratch();
    assert_eq!(owner.retained_heap_bytes(), baseline);
    let first = pack(&mut owner, chunk(0), BLOCK, 0);
    let with_workspace = owner.retained_heap_bytes();
    assert!(with_workspace > baseline);
    assert_eq!(pack(&mut owner, chunk(0), BLOCK, 0), first);
    assert_eq!(owner.retained_heap_bytes(), with_workspace);
    owner.release_operation_scratch();
    assert_eq!(owner.retained_heap_bytes(), baseline);
    assert_eq!(run(&mut owner, BLOCK, 10, 16).len(), 1);
}

#[test]
fn scratch_growth_admits_old_and_replacement_capacity_before_output_mutation() {
    let settings = TickLimits {
        max_chunks: 1,
        queued_per_chunk: 4,
        selected_per_phase: 4,
        ..limits()
    };
    let mut measured = owner(settings);
    measured.register_chunk(chunk(0), true).unwrap();
    let baseline = measured.retained_heap_bytes();
    for id in 1..=3 {
        schedule(&mut measured, id, id as i32, 10, TickPriority::Normal);
    }
    pack(&mut measured, chunk(0), BLOCK, 0);
    let final_capacity = measured.retained_heap_bytes();

    let mut limited = owner(TickLimits {
        allocation_bytes: final_capacity,
        ..settings
    });
    limited.register_chunk(chunk(0), true).unwrap();
    schedule(&mut limited, 1, 1, 10, TickPriority::Normal);
    pack(&mut limited, chunk(0), BLOCK, 0);
    let with_small_scratch = limited.retained_heap_bytes();
    for id in 2..=3 {
        schedule(&mut limited, id, id as i32, 10, TickPriority::Normal);
    }
    let sentinel = saved(9, 9, 99);
    let mut output = Vec::with_capacity(4);
    output.push(sentinel);
    assert_eq!(
        limited.pack_chunk(chunk(0), BLOCK, 0, &mut output),
        Err(TickError::AllocationBudget)
    );
    assert_eq!(output, [sentinel]);
    assert_eq!(limited.retained_heap_bytes(), with_small_scratch);
    assert_eq!(limited.queued_count(BLOCK), 3);
    limited.release_operation_scratch();
    assert_eq!(limited.retained_heap_bytes(), baseline);
    assert_eq!(limited.pack_chunk(chunk(0), BLOCK, 0, &mut output), Ok(3));
    assert_eq!(limited.retained_heap_bytes(), final_capacity);
    assert_eq!(output.len(), 4);
}

#[test]
fn copy_workspace_has_a_separate_budget_and_release_does_not_discard_ticks() {
    let settings = TickLimits {
        max_chunks: 1,
        queued_per_chunk: 4,
        selected_per_phase: 4,
        ..limits()
    };
    let mut source = owner(settings);
    source.register_chunk(chunk(0), true).unwrap();
    schedule(&mut source, 1, 1, 10, TickPriority::Normal);
    let mut measured = owner(settings);
    measured.register_chunk(chunk(2), true).unwrap();
    let baseline = measured.retained_heap_bytes();
    let copy = measured
        .copy_area_from(&source, BLOCK, bounds(1, 1), offset(32))
        .unwrap();
    assert_eq!(copy.added, 1);
    let with_workspace = measured.retained_heap_bytes();
    assert!(with_workspace > baseline);
    measured.release_operation_scratch();
    assert_eq!(measured.retained_heap_bytes(), baseline);
    assert_eq!(measured.queued_count(BLOCK), 1);

    let mut limited = owner(TickLimits {
        allocation_bytes: with_workspace - 1,
        ..settings
    });
    limited.register_chunk(chunk(2), true).unwrap();
    assert_eq!(
        limited.copy_area_from(&source, BLOCK, bounds(1, 1), offset(32)),
        Err(TickError::AllocationBudget)
    );
    assert_eq!(limited.retained_heap_bytes(), baseline);
    assert_eq!(limited.queued_count(BLOCK), 0);
    assert_eq!(source.queued_count(BLOCK), 1);
    assert_eq!(run(&mut measured, BLOCK, 10, 4)[0].position, position(33));
}
