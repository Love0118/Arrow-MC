use arrow_mc::server::chunk_sender::{
    ChunkDeliveryQueue, ChunkSender, DeliveryLimits, DropOutcome, Error as SenderError,
    SenderLimits,
};
use arrow_mc::world::preparation::ChunkAddress;
use arrow_mc::world::view::{
    PlayerView, TrackingView, ViewChange, ViewDifference, ViewDistance, ViewError, ViewEvent,
    is_within_distance,
};

fn pos(x: i32, z: i32) -> ChunkAddress {
    ChunkAddress { x, z }
}

fn view(x: i32, z: i32, radius: i32) -> TrackingView {
    TrackingView::positioned(pos(x, z), ViewDistance::server(radius)).unwrap()
}

fn consume(player: &mut PlayerView) -> Vec<ViewEvent> {
    let mut events = Vec::new();
    while let Some(event) = player.pending_event() {
        events.push(event);
        player.acknowledge_event().unwrap();
    }
    player.finish_update().unwrap();
    events
}

#[test]
fn server_and_signed_client_ranges_preserve_every_supported_radius() {
    for requested in [i32::MIN, -128, -1, 0, 1, 2, 32, 33, 127, i32::MAX] {
        let server = ViewDistance::server(requested);
        assert_eq!(i32::from(server.get()), requested.clamp(2, 32));
        for client in i8::MIN..=i8::MAX {
            assert_eq!(
                i32::from(server.effective(i32::from(client)).get()),
                i32::from(client).clamp(2, i32::from(server.get()))
            );
        }
    }
    for radius in 2..=32 {
        assert_eq!(
            ViewDistance::server(radius).effective(32).get(),
            radius as u8
        );
        assert_eq!(
            ViewDistance::server(32).effective(radius).get(),
            radius as u8
        );
    }
}

#[test]
fn strict_edge_neighbor_border_and_curved_corners_are_distinct() {
    for radius in 2..=32 {
        let current = view(-17, 29, radius);
        for direction in [-1, 1] {
            assert!(current.contains(pos(-17 + direction * (radius + 1), 29)));
            assert!(!current.contains(pos(-17 + direction * (radius + 2), 29)));
            assert!(current.is_in_view_distance(pos(-17, 29 + direction * radius)));
            assert!(!current.is_in_view_distance(pos(-17, 29 + direction * (radius + 1))));
        }
    }
    let full = view(0, 0, 32);
    assert!(full.contains(pos(24, 24)));
    assert!(!full.contains(pos(25, 25)));
    assert!(!TrackingView::EMPTY.contains(pos(0, 0)));
    assert!(!TrackingView::EMPTY.is_in_view_distance(pos(0, 0)));
}

#[test]
fn extreme_predicate_arithmetic_does_not_panic_or_turn_abs_min_into_zero() {
    for neighbors in [false, true] {
        assert!(!is_within_distance(
            pos(0, 0),
            ViewDistance::server(32),
            pos(i32::MIN, 0),
            neighbors
        ));
        // Java subtraction wraps: opposite endpoints differ by one modulo 2^32.
        assert!(is_within_distance(
            pos(i32::MIN, i32::MAX),
            ViewDistance::server(2),
            pos(i32::MAX, i32::MIN),
            neighbors
        ));
    }
}

#[test]
fn scan_bounds_are_checked_before_a_view_can_be_admitted() {
    for radius in 2..=32 {
        let distance = ViewDistance::server(radius);
        let low = i32::MIN + radius + 1;
        let high = i32::MAX - radius - 2;
        for center in [pos(low, high), pos(high, low)] {
            let current = TrackingView::positioned(center, distance).unwrap();
            assert!(ViewDifference::new(TrackingView::EMPTY, current).count() > 0);
        }
        for center in [
            pos(low - 1, 0),
            pos(high + 1, 0),
            pos(0, low - 1),
            pos(0, high + 1),
        ] {
            assert_eq!(
                TrackingView::positioned(center, distance),
                Err(ViewError::CoordinateBounds)
            );
        }
    }
}

#[test]
fn disjoint_changes_leave_then_enter_in_x_then_z_order() {
    let previous = view(-10_000_000, 1, 32);
    let next = view(10_000_000, -1, 32);
    let changes: Vec<_> = ViewDifference::new(previous, next).collect();
    let first_enter = changes
        .iter()
        .position(|event| matches!(event, ViewChange::Enter(_)))
        .unwrap();
    assert!(first_enter > 0);
    let leaves: Vec<_> = changes[..first_enter]
        .iter()
        .map(|event| match event {
            ViewChange::Leave(chunk) => *chunk,
            _ => panic!("enter before leaves finished"),
        })
        .collect();
    let enters: Vec<_> = changes[first_enter..]
        .iter()
        .map(|event| match event {
            ViewChange::Enter(chunk) => *chunk,
            _ => panic!("leave after enters started"),
        })
        .collect();
    assert!(leaves.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(enters.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(leaves.len(), enters.len());
    assert!(changes.len() <= 2 * 67 * 67);
}

#[test]
fn overlap_preserves_interleaved_changes_in_one_coordinate_order() {
    let changes: Vec<_> = ViewDifference::new(view(0, 0, 8), view(1, 1, 8)).collect();
    let coordinates: Vec<_> = changes
        .iter()
        .map(|event| match event {
            ViewChange::Enter(chunk) | ViewChange::Leave(chunk) => *chunk,
        })
        .collect();
    assert!(coordinates.windows(2).all(|pair| pair[0] < pair[1]));
    let switches = changes
        .windows(2)
        .filter(|pair| std::mem::discriminant(&pair[0]) != std::mem::discriminant(&pair[1]))
        .count();
    assert!(
        switches > 2,
        "overlapping effects must not become two separate lists"
    );
    assert_eq!(
        ViewDifference::new(view(0, 0, 32), view(0, 0, 32)).next(),
        None
    );
}

#[test]
fn center_event_precedes_changes_and_radius_only_updates_do_not_resend_it() {
    let mut player = PlayerView::new();
    let initial = view(1, 2, 4);
    player.begin_update(initial).unwrap();
    assert_eq!(
        player.pending_event(),
        Some(ViewEvent::SetCenter(pos(1, 2)))
    );
    assert_eq!(player.current(), TrackingView::EMPTY);
    let events = consume(&mut player);
    assert!(
        events[1..]
            .iter()
            .all(|event| matches!(event, ViewEvent::Enter(_)))
    );
    assert_eq!(player.current(), initial);
    player.begin_update(view(1, 2, 2)).unwrap();
    assert!(
        consume(&mut player)
            .iter()
            .all(|event| matches!(event, ViewEvent::Leave(_)))
    );
    player.begin_update(view(2, 2, 2)).unwrap();
    assert_eq!(
        player.pending_event(),
        Some(ViewEvent::SetCenter(pos(2, 2)))
    );
    consume(&mut player);
    player.begin_update(TrackingView::EMPTY).unwrap();
    assert!(
        consume(&mut player)
            .iter()
            .all(|event| matches!(event, ViewEvent::Leave(_)))
    );
    assert_eq!(player.current(), TrackingView::EMPTY);
}

#[test]
fn unconsumed_or_partly_acknowledged_transition_cannot_be_rebased_or_installed() {
    let mut player = PlayerView::new();
    assert_eq!(player.acknowledge_event(), Err(ViewError::NoUpdate));
    assert_eq!(player.finish_update(), Err(ViewError::NoUpdate));
    player.begin_update(view(0, 0, 2)).unwrap();
    for _ in 0..3 {
        let event = player.pending_event();
        assert_eq!(
            player.begin_update(view(1, 1, 2)),
            Err(ViewError::UpdateActive)
        );
        assert_eq!(player.finish_update(), Err(ViewError::UnconsumedEvents));
        assert_eq!(player.pending_event(), event);
        assert_eq!(player.current(), TrackingView::EMPTY);
        player.acknowledge_event().unwrap();
    }
    consume(&mut player);
    let installed = player.current();
    player.begin_update(installed).unwrap();
    assert_eq!(player.pending_event(), None);
    assert_eq!(player.acknowledge_event(), Err(ViewError::NoEvent));
    player.finish_update().unwrap();
    assert_eq!(player.current(), installed);
}

#[test]
fn view_events_drive_real_sender_pending_and_remove_operations() {
    let mut player = PlayerView::new();
    let mut sender = ChunkSender::new(
        false,
        SenderLimits {
            max_pending: 67 * 67,
            control_bytes: 1 << 20,
        },
    )
    .unwrap();
    let mut delivery = ChunkDeliveryQueue::new(DeliveryLimits {
        max_groups: 1,
        max_bytes: 1024,
    })
    .unwrap();
    for next in [
        view(0, 0, 32),
        view(1, 1, 32),
        view(1, 1, 2),
        TrackingView::EMPTY,
    ] {
        player.begin_update(next).unwrap();
        while let Some(event) = player.pending_event() {
            match event {
                // Fixture assumes each entering chunk is already send-ready;
                // this does not claim a chunk payload codec or ticket engine.
                ViewEvent::Enter(chunk) => {
                    sender.mark_pending(chunk).unwrap();
                }
                ViewEvent::Leave(chunk) => assert_eq!(
                    sender.drop_chunk(chunk, true, &mut delivery).unwrap(),
                    DropOutcome::RemovedPending
                ),
                ViewEvent::SetCenter(_) => {}
            }
            player.acknowledge_event().unwrap();
        }
        player.finish_update().unwrap();
        assert_eq!(
            sender.stats().pending,
            ViewDifference::new(TrackingView::EMPTY, next).count()
        );
        assert!(delivery.front_packet().is_none());
    }
}

#[test]
fn delivery_full_retries_the_same_leave_before_installing_new_view() {
    let mut player = PlayerView::new();
    player.begin_update(view(0, 0, 2)).unwrap();
    // Entered chunks were unavailable, so no pending mark was made.
    consume(&mut player);
    let old = player.current();
    let mut sender = ChunkSender::new(
        false,
        SenderLimits {
            max_pending: 1,
            control_bytes: 4096,
        },
    )
    .unwrap();
    let mut delivery = ChunkDeliveryQueue::new(DeliveryLimits {
        max_groups: 1,
        max_bytes: 1024,
    })
    .unwrap();
    player.begin_update(TrackingView::EMPTY).unwrap();
    let mut failures = 0;
    let mut emitted = Vec::new();
    while let Some(event) = player.pending_event() {
        let ViewEvent::Leave(chunk) = event else {
            panic!("only leaves expected")
        };
        match sender.drop_chunk(chunk, true, &mut delivery) {
            Ok(DropOutcome::ForgetQueued) => {
                emitted.push(chunk);
                player.acknowledge_event().unwrap();
            }
            Err(SenderError::DeliveryFull) => {
                failures += 1;
                assert_eq!(player.pending_event(), Some(event));
                assert_eq!(player.current(), old);
                assert_eq!(player.finish_update(), Err(ViewError::UnconsumedEvents));
                delivery.packet_written().unwrap();
            }
            result => panic!("unexpected drop result {result:?}"),
        }
    }
    player.finish_update().unwrap();
    assert!(failures > 0);
    let expected: Vec<_> = ViewDifference::new(old, TrackingView::EMPTY)
        .map(|event| match event {
            ViewChange::Leave(chunk) => chunk,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(emitted, expected);
}
