#[path = "common/configuration_fixture.rs"]
mod configuration_fixture;
use arrow_mc::{
    nbt,
    server::{
        configuration::{ConfigurationSession, SessionStage, packet::ClientInformation},
        packet::{PacketReader, PacketWriter},
    },
};
use configuration_fixture::{Fixture, core};
use std::sync::Arc;

fn make_session() -> ConfigurationSession {
    let fixture = Fixture::new();
    ConfigurationSession::new(Arc::new(fixture.load().unwrap()), "Arrow MC".into(), 0)
}
fn prefix(session: &mut ConfigurationSession) {
    for id in [1, 13, 15] {
        let packet = session.next_outbound(8192).unwrap().unwrap();
        assert_eq!(packet[0], id);
        session.outbound_written().unwrap();
    }
    assert_eq!(session.stage(), SessionStage::AwaitingKnownPacks);
}
fn reply_known(known: bool) -> Vec<u8> {
    let mut writer = PacketWriter::new(256);
    writer.varint(7).unwrap();
    writer.varint(i32::from(known)).unwrap();
    if known {
        let pack = core();
        writer.utf(&pack.namespace, 32767).unwrap();
        writer.utf(&pack.id, 32767).unwrap();
        writer.utf(&pack.version, 32767).unwrap();
    }
    writer.into_bytes()
}
fn long_packet(id: i32, value: i64) -> Vec<u8> {
    let mut out = PacketWriter::new(16);
    out.varint(id).unwrap();
    out.long(value).unwrap();
    out.into_bytes()
}

#[test]
fn registry_sequence_preserves_entries_then_tags_and_stops_at_actual_spawn() {
    for known in [false, true] {
        let mut session = make_session();
        prefix(&mut session);
        session.on_packet(&reply_known(known), 0, 8192).unwrap();
        assert_eq!(session.known_packs_matched(), known);
        for index in 0..32 {
            let bytes = session.next_outbound(8192).unwrap().unwrap();
            let mut reader = PacketReader::new(&bytes);
            assert_eq!(reader.varint().unwrap(), 7);
            assert_eq!(
                reader.identifier().unwrap(),
                session.snapshot().registries()[index].id()
            );
            assert_eq!(reader.varint().unwrap(), 1);
            assert_eq!(reader.identifier().unwrap(), "test:synthetic");
            let present = reader.boolean().unwrap();
            assert_eq!(present, !(known && index == 0));
            if present {
                let mut nbt_bytes = reader.remaining_bytes(8192).unwrap();
                assert!(matches!(
                    nbt::read_network(&mut nbt_bytes, nbt::Limits::default()).unwrap(),
                    nbt::Tag::Compound(_)
                ));
                assert!(nbt_bytes.is_empty());
            }
            reader.finish().unwrap();
            session.outbound_written().unwrap();
        }
        assert_eq!(session.stage(), SessionStage::SendingTags);
        let tags = session.next_outbound(8192).unwrap().unwrap();
        assert_eq!(tags[0], 14);
        session.outbound_written().unwrap();
        assert_eq!(session.stage(), SessionStage::AwaitingSpawn);
        assert!(session.next_outbound(8192).unwrap().is_none());
        assert!(session.on_packet(&[3], 1, 8192).is_err());
        assert_eq!(session.stage(), SessionStage::Closed);
    }
}

#[test]
fn no_state_advance_before_successful_write_or_on_encoding_error() {
    let mut session = make_session();
    assert!(session.next_outbound(1).is_err());
    assert_eq!(session.stage(), SessionStage::Initializing);
    assert_eq!(session.next_outbound(8192).unwrap().unwrap()[0], 1);
    assert!(session.next_outbound(8192).is_err());
    session.outbound_written().unwrap();
    assert_eq!(session.next_outbound(8192).unwrap().unwrap()[0], 13);
    session.close();
    assert!(session.next_outbound(8192).unwrap().is_none());
    assert!(session.outbound_written().is_err());
}

#[test]
fn duplicate_and_wrong_task_responses_fail_and_common_packets_remain_live() {
    for bytes in [&[3][..], &[9][..], &reply_known(false)[..]] {
        let mut session = make_session();
        assert!(session.on_packet(bytes, 0, 8192).is_err());
        assert_eq!(session.stage(), SessionStage::Closed);
    }
    let mut session = make_session();
    prefix(&mut session);
    session.on_packet(&reply_known(false), 0, 8192).unwrap();
    assert!(session.on_packet(&reply_known(false), 0, 8192).is_err());
    let mut session = make_session();
    prefix(&mut session);
    let mut writer = PacketWriter::new(64);
    writer.varint(0).unwrap();
    writer.utf("ko_kr", 16).unwrap();
    writer.byte(-1).unwrap();
    writer.varint(-1).unwrap();
    writer.boolean(true).unwrap();
    writer.unsigned_byte(255).unwrap();
    writer.varint(99).unwrap();
    writer.boolean(true).unwrap();
    writer.boolean(false).unwrap();
    writer.varint(3).unwrap();
    session.on_packet(writer.as_bytes(), 1, 8192).unwrap();
    assert_eq!(
        session.client_information(),
        &ClientInformation {
            language: "ko_kr".into(),
            view_distance: -1,
            chat_visibility: 2,
            chat_colors: true,
            model_customization: 255,
            main_hand: 0,
            text_filtering: true,
            allows_listing: false,
            particle_status: 0
        }
    );
    let mut pong = PacketWriter::new(8);
    pong.varint(5).unwrap();
    pong.int(4).unwrap();
    session.on_packet(pong.as_bytes(), 1, 8192).unwrap();
    for action in [3, 4] {
        let mut response = PacketWriter::new(32);
        response.varint(6).unwrap();
        response.uuid([0; 16]).unwrap();
        response.varint(action).unwrap();
        session.on_packet(response.as_bytes(), 2, 8192).unwrap();
    }
    let mut response = PacketWriter::new(32);
    response.varint(6).unwrap();
    response.uuid([0; 16]).unwrap();
    response.varint(0).unwrap();
    assert!(session.on_packet(response.as_bytes(), 3, 8192).is_err());
}

#[test]
fn keepalive_uses_its_own_fifteen_second_deadline_and_correct_challenge() {
    let mut session = make_session();
    prefix(&mut session);
    session.tick(14_999).unwrap();
    assert!(session.next_outbound(64).unwrap().is_none());
    session.tick(15_000).unwrap();
    assert_eq!(
        session.next_outbound(64).unwrap().unwrap(),
        long_packet(4, 15_000)
    );
    session.outbound_written().unwrap();
    session
        .on_packet(&long_packet(4, 15_000), 15_100, 64)
        .unwrap();
    assert_eq!(session.latency(), 25);
    session.tick(29_999).unwrap();
    assert!(session.next_outbound(64).unwrap().is_none());
    session.tick(30_000).unwrap();
    let _ = session.next_outbound(64).unwrap();
    session.outbound_written().unwrap();
    assert!(session.tick(45_000).is_err());
    assert_eq!(session.stage(), SessionStage::Closed);
    let mut session = make_session();
    prefix(&mut session);
    assert!(session.on_packet(&long_packet(4, 0), 1, 64).is_err());
    let mut session = make_session();
    prefix(&mut session);
    session.tick(15_000).unwrap();
    let _ = session.next_outbound(64).unwrap();
    session.outbound_written().unwrap();
    assert!(session.on_packet(&long_packet(4, 99), 15_001, 64).is_err());
}
