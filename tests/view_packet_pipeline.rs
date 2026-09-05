//! Tracking admission and actual control packet bytes share one ordered sink.
//! Every entered chunk in this synthetic fixture is explicitly considered ready;
//! this does not implement world activation or infer readiness from disk status.
use arrow_mc::{
    server::{
        chunk_packet::{self, SmallPacket},
        chunk_sender::{ChunkDeliveryQueue, ChunkSender, DeliveryLimits, Error, SenderLimits},
    },
    world::{
        preparation::ChunkAddress,
        view::{PlayerView, TrackingView, ViewDistance, ViewEvent},
    },
};

fn queue() -> ChunkDeliveryQueue {
    ChunkDeliveryQueue::new(DeliveryLimits {
        max_groups: 1,
        max_bytes: 2048,
    })
    .unwrap()
}

fn drain(queue: &mut ChunkDeliveryQueue, wire: &mut Vec<Vec<u8>>) {
    let mut scratch: SmallPacket = chunk_packet::batch_start();
    while let Some(intent) = queue.front_packet() {
        wire.push(
            chunk_packet::delivery_bytes(intent, &mut scratch)
                .unwrap()
                .to_vec(),
        );
        queue.packet_written().unwrap();
    }
}

#[test]
fn center_control_waits_for_prior_delivery_and_radius_only_change_does_not_resend_it() {
    let mut delivery = queue();
    let mut sender = ChunkSender::new(
        false,
        SenderLimits {
            max_pending: 8192,
            control_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let prior = ChunkAddress { x: -20, z: 3 };
    sender.drop_chunk(prior, true, &mut delivery).unwrap();
    let center = ChunkAddress { x: -7, z: -3 };
    let next = TrackingView::positioned(center, ViewDistance::server(2)).unwrap();
    let mut view = PlayerView::new();
    view.begin_update(next).unwrap();
    assert_eq!(view.pending_event(), Some(ViewEvent::SetCenter(center)));
    // The new control must follow already-admitted bytes on this connection.
    // While they remain, no event is acknowledged and no view is published.
    assert_eq!(delivery.group_count(), 1);
    assert_eq!(view.current(), TrackingView::EMPTY);
    let mut wire = Vec::new();
    drain(&mut delivery, &mut wire);
    assert_eq!(wire, [chunk_packet::forget(prior).as_bytes()]);
    wire.push(chunk_packet::cache_center(center).as_bytes().to_vec());
    view.acknowledge_event().unwrap();
    while let Some(event) = view.pending_event() {
        let ViewEvent::Enter(position) = event else {
            panic!("initial view only enters")
        };
        sender.mark_pending(position).unwrap();
        view.acknowledge_event().unwrap();
    }
    view.finish_update().unwrap();
    assert_eq!(view.current(), next);
    assert_eq!(wire[1][0], chunk_packet::CACHE_CENTER_ID as u8);
    assert_eq!(wire[1], chunk_packet::cache_center(center).as_bytes());

    let server_distance = ViewDistance::server(3);
    let distance = server_distance.effective(32);
    wire.push(
        chunk_packet::cache_radius(i32::from(server_distance.get()))
            .as_bytes()
            .to_vec(),
    );
    let expanded = TrackingView::positioned(center, distance).unwrap();
    view.begin_update(expanded).unwrap();
    while let Some(event) = view.pending_event() {
        match event {
            ViewEvent::SetCenter(_) => panic!("unchanged center must not be sent again"),
            ViewEvent::Enter(position) => {
                sender.mark_pending(position).unwrap();
            }
            ViewEvent::Leave(position) => {
                sender.drop_chunk(position, true, &mut delivery).unwrap();
            }
        }
        view.acknowledge_event().unwrap();
    }
    view.finish_update().unwrap();
    assert_eq!(
        wire.last().unwrap(),
        &[chunk_packet::CACHE_RADIUS_ID as u8, 3]
    );
    assert_eq!(view.current(), expanded);
}

#[test]
fn full_forget_queue_retries_same_tracking_event_and_encodes_each_drop_once() {
    let center = ChunkAddress { x: 2, z: -4 };
    let old = TrackingView::positioned(center, ViewDistance::server(2)).unwrap();
    let mut view = PlayerView::new();
    view.begin_update(old).unwrap();
    // Simulate a previously tracked/sent view, so leaves need real Forget packets.
    while view.pending_event().is_some() {
        view.acknowledge_event().unwrap();
    }
    view.finish_update().unwrap();
    view.begin_update(TrackingView::EMPTY).unwrap();
    let mut sender = ChunkSender::new(
        false,
        SenderLimits {
            max_pending: 128,
            control_bytes: 16384,
        },
    )
    .unwrap();
    let mut delivery = queue();
    let mut wire = Vec::new();
    let mut expected = Vec::new();
    let mut saw_full = false;
    while let Some(event) = view.pending_event() {
        let ViewEvent::Leave(position) = event else {
            panic!("empty view only leaves")
        };
        match sender.drop_chunk(position, true, &mut delivery) {
            Ok(_) => {
                expected.push(chunk_packet::forget(position).as_bytes().to_vec());
                view.acknowledge_event().unwrap();
            }
            Err(Error::DeliveryFull) => {
                saw_full = true;
                assert_eq!(view.pending_event(), Some(event));
                assert_eq!(view.current(), old);
                drain(&mut delivery, &mut wire);
            }
            other => panic!("unexpected admission: {other:?}"),
        }
    }
    drain(&mut delivery, &mut wire);
    view.finish_update().unwrap();
    assert!(saw_full);
    assert_eq!(wire, expected);
    assert!(
        wire.iter()
            .all(|bytes| bytes.len() == 9 && bytes[0] == chunk_packet::FORGET_CHUNK_ID as u8)
    );
    assert_eq!(view.current(), TrackingView::EMPTY);
}
