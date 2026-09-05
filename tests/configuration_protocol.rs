use arrow_mc::{
    nbt,
    server::{
        configuration::packet::{self, Clientbound, Serverbound},
        packet::PacketWriter,
    },
};
use serde_json::{Value, json};

fn unhex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
        .collect()
}
fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn payload(case: &Value) -> Vec<u8> {
    if let Some(value) = case["payload_hex"].as_str() {
        return unhex(value);
    }
    let mut value = unhex(case["payload_prefix_hex"].as_str().unwrap_or(""));
    for _ in 0..case["payload_repeat_count"].as_u64().unwrap_or(0) {
        value.extend_from_slice(&unhex(case["payload_repeat_hex"].as_str().unwrap()));
    }
    value.extend_from_slice(&unhex(case["payload_suffix_hex"].as_str().unwrap_or("")));
    value
}
fn tag_id(tag: &nbt::Tag) -> u8 {
    match tag {
        nbt::Tag::End => 0,
        nbt::Tag::Byte(_) => 1,
        nbt::Tag::Short(_) => 2,
        nbt::Tag::Int(_) => 3,
        nbt::Tag::Long(_) => 4,
        nbt::Tag::Float(_) => 5,
        nbt::Tag::Double(_) => 6,
        nbt::Tag::ByteArray(_) => 7,
        nbt::Tag::String(_) => 8,
        nbt::Tag::List(_) => 9,
        nbt::Tag::Compound(_) => 10,
        nbt::Tag::IntArray(_) => 11,
        nbt::Tag::LongArray(_) => 12,
    }
}

#[test]
fn all_synthetic_serverbound_fields_match_official_codec_acceptance_and_values() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/configuration_packet_oracle.json")).unwrap();
    let mut checked = 0;
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["direction"] == "serverbound")
    {
        let mut bytes = vec![case["packet_id"].as_u64().unwrap() as u8];
        bytes.extend_from_slice(&payload(case));
        let result = packet::decode(&bytes, packet::DEFAULT_PACKET_LIMIT);
        let expected = case["ok"] == true && case["consumed_bytes"] == case["payload_bytes"];
        assert_eq!(result.is_ok(), expected, "{}: {:?}", case["name"], result);
        if let Ok(value) = result {
            let reference = &case["result"];
            match value {
                Serverbound::ClientInformation(info) => assert_eq!(
                    json!({"language":info.language,"view_distance":info.view_distance,"chat_visibility":info.chat_visibility,"chat_colors":info.chat_colors,"model_customisation":info.model_customization,"main_hand":info.main_hand,"text_filtering":info.text_filtering,"allows_listing":info.allows_listing,"particle_status":info.particle_status}),
                    *reference,
                    "{}",
                    case["name"]
                ),
                Serverbound::KeepAlive(id) => assert_eq!(
                    id.to_string(),
                    reference["id"].as_str().unwrap(),
                    "{}",
                    case["name"]
                ),
                Serverbound::Pong(id) => assert_eq!(
                    id.to_string(),
                    reference["id"].as_str().unwrap(),
                    "{}",
                    case["name"]
                ),
                Serverbound::SelectKnownPacks(packs) => {
                    let expected = reference["known_packs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|pack| arrow_mc::server::configuration_data::KnownPack {
                            namespace: pack["namespace"].as_str().unwrap().into(),
                            id: pack["id"].as_str().unwrap().into(),
                            version: pack["version"].as_str().unwrap().into(),
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(packs.len(), expected.len());
                    assert!(packs.matches(&expected).unwrap());
                }
                Serverbound::ResourcePack { action, .. } => {
                    assert_eq!(
                        json!(action.protocol_id()),
                        reference["action"],
                        "{}",
                        case["name"]
                    );
                    assert_eq!(
                        json!(action.is_terminal()),
                        reference["terminal"],
                        "{}",
                        case["name"]
                    );
                }
                Serverbound::CustomClick { id, payload } => {
                    assert_eq!(json!(id), reference["id"]);
                    assert_eq!(json!(payload.is_some()), reference["present"]);
                    if let Some(value) = payload {
                        assert_eq!(json!(tag_id(&value)), reference["tag_id"]);
                        if let nbt::Tag::ByteArray(values) = &value {
                            assert_eq!(json!(values.len()), reference["byte_array_length"]);
                        }
                        if let Some(expected) = reference["snbt"].as_str() {
                            let mut text = Vec::new();
                            arrow_mc::snbt::write(
                                &value,
                                &mut text,
                                arrow_mc::snbt::Limits::default(),
                            )
                            .unwrap();
                            assert_eq!(String::from_utf16(&text).unwrap(), expected);
                        }
                    }
                }
                Serverbound::CustomPayload {
                    channel,
                    brand: Some(brand),
                    ..
                } => {
                    assert_eq!(json!(channel), reference["channel"]);
                    assert_eq!(
                        json!(brand.encode_utf16().count()),
                        reference["brand_utf16_length"]
                    );
                    if let Some(expected) = reference["brand"].as_str() {
                        assert_eq!(brand, expected);
                    }
                }
                Serverbound::CustomPayload { channel, .. } => {
                    assert_eq!(json!(channel), reference["channel"])
                }
                Serverbound::CookieResponse { key, payload } => {
                    assert_eq!(json!(key), reference["key"]);
                    assert_eq!(json!(payload.is_some()), reference["present"]);
                    if let Some(payload) = payload {
                        assert_eq!(json!(payload.len()), reference["payload_bytes"]);
                        if let Some(expected) = reference["data_hex"].as_str() {
                            assert_eq!(hex(payload), expected);
                        }
                    }
                }
                _ => {}
            }
        }
        checked += 1;
    }
    assert_eq!(checked, 70);
}

#[test]
fn outgoing_brand_features_known_packs_and_disconnect_match_official_bytes() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/configuration_packet_oracle.json")).unwrap();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["direction"] == "clientbound")
    {
        let name = case["name"].as_str().unwrap();
        let features = vec!["test:feature".to_owned()];
        let packs = vec![arrow_mc::server::configuration_data::KnownPack {
            namespace: "test".into(),
            id: "pack".into(),
            version: "v1".into(),
        }];
        let outgoing = match name {
            "clientbound_brand" => Some(Clientbound::Brand("arrow-oracle")),
            "clientbound_features" => Some(Clientbound::EnabledFeatures(&features)),
            "clientbound_known_packs" => Some(Clientbound::SelectKnownPacks(&packs)),
            "clientbound_disconnect" => Some(Clientbound::Disconnect("oracle complete")),
            _ => None,
        };
        if let Some(outgoing) = outgoing {
            let bytes = packet::encode(outgoing, 1024).unwrap();
            assert_eq!(hex(&bytes[1..]), hex(&payload(case)), "{name}");
            assert_eq!(bytes[0], case["packet_id"].as_u64().unwrap() as u8);
        }
    }
}

#[test]
fn bounded_packet_and_outer_trailing_validation() {
    assert!(packet::decode(&[3, 0], 8).is_err());
    assert!(packet::decode(&[3], 0).is_err());
    assert!(packet::encode(Clientbound::Brand("Arrow MC"), 2).is_err());
    let mut writer = PacketWriter::new(32);
    writer.varint(8).unwrap();
    writer.identifier("test:click").unwrap();
    writer.bytes(&[0, 0xff], 65536).unwrap();
    assert!(matches!(
        packet::decode(writer.as_bytes(), 32),
        Ok(Serverbound::CustomClick { payload: None, .. })
    ));
}
