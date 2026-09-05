use arrow_mc::server::{
    login::{
        AuthenticatedProfile, ProfileProperty,
        packet::{self, LoginPacket},
        session::{LoginPhase, LoginSession, SessionError},
    },
    packet::{PacketReader, PacketWriter},
};

fn profile() -> AuthenticatedProfile {
    AuthenticatedProfile {
        id: [7; 16],
        name: "Player!".into(),
        properties: vec![ProfileProperty {
            name: "textures".into(),
            value: "payload".into(),
            signature: Some("signature".into()),
        }],
    }
}

#[test]
fn hello_key_query_and_cookie_decode_exact_payloads() {
    let mut writer = PacketWriter::new(100);
    writer.varint(0).unwrap();
    writer.utf("Player!", 16).unwrap();
    writer.uuid([42; 16]).unwrap();
    match packet::decode(writer.as_bytes()).unwrap() {
        LoginPacket::Hello { name, claimed_id } => {
            assert_eq!(name, "Player!");
            assert_eq!(claimed_id, [42; 16]);
        }
        _ => panic!(),
    }
    let key = [1, 2, 4, 5, 1, 6];
    match packet::decode(&key).unwrap() {
        LoginPacket::Key {
            encrypted_secret,
            encrypted_challenge,
        } => {
            assert_eq!(encrypted_secret, [4, 5]);
            assert_eq!(encrypted_challenge, [6]);
        }
        _ => panic!(),
    }
    assert!(matches!(
        packet::decode(&[2, 7, 0, 99, 88]),
        Ok(LoginPacket::QueryAnswer { transaction_id: 7 })
    ));
    assert!(matches!(
        packet::decode(&[2, 7]),
        Ok(LoginPacket::QueryAnswer { transaction_id: 7 })
    ));
    assert!(matches!(
        packet::decode(&[3]),
        Ok(LoginPacket::Acknowledged)
    ));
    assert!(packet::decode(&[3, 0]).is_err());
    let mut writer = PacketWriter::new(100);
    writer.varint(4).unwrap();
    writer.identifier("minecraft:test").unwrap();
    writer.boolean(true).unwrap();
    writer.bytes(&[1, 2], 5120).unwrap();
    match packet::decode(writer.as_bytes()).unwrap() {
        LoginPacket::CookieResponse { key, payload } => {
            assert_eq!(key, "minecraft:test");
            assert_eq!(payload.unwrap(), [1, 2]);
        }
        _ => panic!(),
    }
}

#[test]
fn login_finished_includes_profile_properties_then_separate_session_uuid() {
    let profile = profile();
    let encoded = packet::finished(&profile, [11; 16], 4096).unwrap();
    let mut reader = PacketReader::new(&encoded);
    assert_eq!(reader.varint().unwrap(), 2);
    assert_eq!(reader.uuid().unwrap(), profile.id);
    assert_eq!(reader.utf(16).unwrap(), profile.name);
    assert_eq!(reader.varint().unwrap(), 1);
    assert_eq!(reader.utf(64).unwrap(), "textures");
    assert_eq!(reader.utf(32767).unwrap(), "payload");
    assert!(reader.boolean().unwrap());
    assert_eq!(reader.utf(1024).unwrap(), "signature");
    assert_eq!(reader.uuid().unwrap(), [11; 16]);
    reader.finish().unwrap();
    let mut too_many = profile;
    too_many.properties = vec![too_many.properties[0].clone(); 17];
    assert!(packet::finished(&too_many, [1; 16], 10000).is_err());
}

#[test]
fn login_state_requires_crypto_auth_admission_written_finish_then_ack() {
    let mut session = LoginSession::new(true);
    assert_eq!(
        session.acknowledge().err(),
        Some(SessionError::UnexpectedPacket)
    );
    session.receive_hello("Player!".into()).unwrap();
    assert_eq!(session.phase(), LoginPhase::Key);
    assert!(session.authenticated(profile()).is_err());
    session.begin_key_verification().unwrap();
    assert!(session.begin_key_verification().is_err());
    session.key_verified().unwrap();
    session.authenticated(profile()).unwrap();
    session.admitted([9; 16], true).unwrap();
    assert!(session.begin_finished_write().is_err());
    session.duplicate_removed().unwrap();
    assert_eq!(session.begin_finished_write().unwrap(), [9; 16]);
    assert!(session.acknowledge().is_err());
    session.finished_written().unwrap();
    let accepted = session.acknowledge().unwrap();
    assert_eq!(accepted.profile.id, [7; 16]);
    assert_eq!(accepted.session_id, [9; 16]);
    assert!(accepted.transferred);
    assert!(session.acknowledge().is_err());
}

#[test]
fn vanilla_name_validation_and_601st_tick_boundary() {
    for name in ["", "Player!", "a.b+c@d", "1234567890123456"] {
        let mut session = LoginSession::new(false);
        session.receive_hello(name.into()).unwrap();
    }
    for name in ["has space", "한글", "\t", "12345678901234567", "\u{7f}"] {
        let mut session = LoginSession::new(false);
        assert_eq!(
            session.receive_hello(name.into()),
            Err(SessionError::InvalidName)
        );
    }
    let mut session = LoginSession::new(false);
    for _ in 0..600 {
        session.tick().unwrap();
    }
    assert_eq!(session.tick(), Err(SessionError::SlowLogin));
    assert_eq!(session.phase(), LoginPhase::Closed);
    assert_eq!(session.receive_hello("a".into()), Err(SessionError::Closed));
}

#[test]
fn repeated_profile_property_names_match_java_group_order() {
    let profile = AuthenticatedProfile {
        id: [0; 16],
        name: "p".into(),
        properties: [("a", "one"), ("b", "two"), ("a", "three")]
            .into_iter()
            .map(|(name, value)| ProfileProperty {
                name: name.into(),
                value: value.into(),
                signature: None,
            })
            .collect(),
    };
    let packet = packet::finished(&profile, [1; 16], 4096).unwrap();
    let mut reader = PacketReader::new(&packet);
    reader.varint().unwrap();
    reader.uuid().unwrap();
    reader.utf(16).unwrap();
    assert_eq!(reader.varint().unwrap(), 3);
    for (name, value) in [("a", "one"), ("a", "three"), ("b", "two")] {
        assert_eq!(reader.utf(64).unwrap(), name);
        assert_eq!(reader.utf(32767).unwrap(), value);
        assert!(!reader.boolean().unwrap());
    }
    assert_eq!(reader.uuid().unwrap(), [1; 16]);
    reader.finish().unwrap();
}
