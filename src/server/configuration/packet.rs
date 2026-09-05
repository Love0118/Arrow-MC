//! Independently designed codecs for the locked configuration packet fields.

use crate::{
    nbt,
    server::{
        configuration_data::{ConfigurationSnapshot, KnownPack, NegotiatedPacks, RegistryData},
        packet::{PacketError, PacketReader, PacketWriter},
    },
};

pub const DEFAULT_PACKET_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientInformation {
    pub language: String,
    /// Signed request; the world/chunk consumer later applies its actual limits.
    pub view_distance: i8,
    pub chat_visibility: u8,
    pub chat_colors: bool,
    pub model_customization: u8,
    pub main_hand: u8,
    pub text_filtering: bool,
    pub allows_listing: bool,
    pub particle_status: u8,
}

impl Default for ClientInformation {
    fn default() -> Self {
        Self {
            language: "en_us".into(),
            view_distance: 2,
            chat_visibility: 0,
            chat_colors: true,
            model_customization: 0,
            main_hand: 1,
            text_filtering: false,
            allows_listing: false,
            particle_status: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourcePackAction {
    SuccessfullyLoaded,
    Declined,
    FailedDownload,
    Accepted,
    Downloaded,
    InvalidUrl,
    FailedReload,
    Discarded,
}
impl ResourcePackAction {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Accepted | Self::Downloaded)
    }
    pub fn protocol_id(self) -> i32 {
        match self {
            Self::SuccessfullyLoaded => 0,
            Self::Declined => 1,
            Self::FailedDownload => 2,
            Self::Accepted => 3,
            Self::Downloaded => 4,
            Self::InvalidUrl => 5,
            Self::FailedReload => 6,
            Self::Discarded => 7,
        }
    }
}

#[derive(Debug)]
pub enum Serverbound<'a> {
    ClientInformation(ClientInformation),
    CookieResponse {
        key: String,
        payload: Option<&'a [u8]>,
    },
    CustomPayload {
        channel: String,
        brand: Option<String>,
        data: &'a [u8],
    },
    FinishConfiguration,
    KeepAlive(i64),
    Pong(i32),
    ResourcePack {
        id: [u8; 16],
        action: ResourcePackAction,
    },
    SelectKnownPacks(KnownPackResponse<'a>),
    CustomClick {
        id: String,
        payload: Option<nbt::Tag>,
    },
    AcceptCodeOfConduct,
}

/// A validated view avoids retaining up to 64 × 3 decoded pack strings per
/// connection. Mismatching list lengths require no second string scan.
#[derive(Debug)]
pub struct KnownPackResponse<'a> {
    fields: &'a [u8],
    count: usize,
}
impl KnownPackResponse<'_> {
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn matches(&self, expected: &[KnownPack]) -> Result<bool, PacketError> {
        if self.count != expected.len() {
            return Ok(false);
        }
        let mut reader = PacketReader::new(self.fields);
        let mut matches = true;
        for pack in expected {
            matches &= reader.utf_equals(Some(&pack.namespace), 32767)?;
            matches &= reader.utf_equals(Some(&pack.id), 32767)?;
            matches &= reader.utf_equals(Some(&pack.version), 32767)?;
        }
        reader.finish()?;
        Ok(matches)
    }
}

pub fn decode(input: &[u8], max_packet_bytes: usize) -> Result<Serverbound<'_>, PacketError> {
    if input.len() > max_packet_bytes {
        return Err(PacketError::LengthLimit {
            kind: "configuration packet",
            actual: input.len(),
            maximum: max_packet_bytes,
        });
    }
    let mut reader = PacketReader::new(input);
    let result = match reader.varint()? {
        0 => {
            let language = reader.utf(16)?;
            let view_distance = reader.byte()?;
            let chat_visibility = reader.varint()?.rem_euclid(3) as u8;
            let chat_colors = reader.boolean()?;
            let model_customization = reader.unsigned_byte()?;
            let main_hand = if reader.varint()? == 1 { 1 } else { 0 };
            let text_filtering = reader.boolean()?;
            let allows_listing = reader.boolean()?;
            let particle_status = reader.varint()?.rem_euclid(3) as u8;
            Serverbound::ClientInformation(ClientInformation {
                language,
                view_distance,
                chat_visibility,
                chat_colors,
                model_customization,
                main_hand,
                text_filtering,
                allows_listing,
                particle_status,
            })
        }
        1 => Serverbound::CookieResponse {
            key: reader.identifier()?,
            payload: if reader.boolean()? {
                Some(reader.bytes(5120)?)
            } else {
                None
            },
        },
        2 => {
            let channel = reader.identifier()?;
            if channel == "minecraft:brand" {
                Serverbound::CustomPayload {
                    channel,
                    brand: Some(reader.utf(32767)?),
                    data: &[],
                }
            } else {
                Serverbound::CustomPayload {
                    channel,
                    brand: None,
                    data: reader.remaining_bytes(32767)?,
                }
            }
        }
        3 => Serverbound::FinishConfiguration,
        4 => Serverbound::KeepAlive(reader.long()?),
        5 => Serverbound::Pong(reader.int()?),
        6 => {
            let id = reader.uuid()?;
            let action = match reader.varint()? {
                0 => ResourcePackAction::SuccessfullyLoaded,
                1 => ResourcePackAction::Declined,
                2 => ResourcePackAction::FailedDownload,
                3 => ResourcePackAction::Accepted,
                4 => ResourcePackAction::Downloaded,
                5 => ResourcePackAction::InvalidUrl,
                6 => ResourcePackAction::FailedReload,
                7 => ResourcePackAction::Discarded,
                _ => {
                    return Err(PacketError::InvalidValue(
                        "invalid resource pack response action",
                    ));
                }
            };
            Serverbound::ResourcePack { id, action }
        }
        7 => {
            let count = reader.varint()?;
            if !(0..=64).contains(&count) {
                return Err(PacketError::InvalidValue(
                    "known pack response must contain at most 64 entries",
                ));
            }
            let start = reader.position();
            for _ in 0..count {
                for _ in 0..3 {
                    reader.utf_equals(None, 32767)?;
                }
            }
            Serverbound::SelectKnownPacks(KnownPackResponse {
                fields: &input[start..reader.position()],
                count: count as usize,
            })
        }
        8 => {
            let id = reader.identifier()?;
            let mut payload = reader.bytes(65536)?;
            let value = nbt::read_network(
                &mut payload,
                nbt::Limits {
                    vanilla_quota_bytes: 32768,
                    allocation_bytes: 1024 * 1024,
                    max_depth: 16,
                    output_bytes: 65536,
                },
            )
            .map_err(|_| PacketError::InvalidValue("invalid bounded custom click NBT"))?;
            // Vanilla's length-prefixed codec advances the outer cursor by the
            // full slice, while the inner tag decoder may leave trailing bytes.
            Serverbound::CustomClick {
                id,
                payload: if matches!(value, nbt::Tag::End) {
                    None
                } else {
                    Some(value)
                },
            }
        }
        9 => Serverbound::AcceptCodeOfConduct,
        _ => return Err(PacketError::InvalidValue("unknown configuration packet ID")),
    };
    reader.finish()?;
    Ok(result)
}

/// Writes exactly one packet body (including its packet ID), not a frame.
pub enum Clientbound<'a> {
    Brand(&'a str),
    EnabledFeatures(&'a [String]),
    SelectKnownPacks(&'a [KnownPack]),
    Registry {
        registry: &'a RegistryData,
        negotiated: &'a NegotiatedPacks<'a>,
    },
    UpdateTags(&'a ConfigurationSnapshot),
    KeepAlive(i64),
    /// A literal server-authored reason, encoded as context-free component NBT.
    Disconnect(&'a str),
}

fn count(writer: &mut PacketWriter, value: usize) -> Result<(), PacketError> {
    writer.varint(i32::try_from(value).map_err(|_| PacketError::LengthOverflow)?)
}

fn registry_header(output: &mut PacketWriter, id: &str, entries: usize) -> Result<(), PacketError> {
    output.varint(7)?;
    output.identifier(id)?;
    count(output, entries)
}
fn registry_entry(
    output: &mut PacketWriter,
    id: &str,
    contents: Option<&[u8]>,
) -> Result<(), PacketError> {
    output.identifier(id)?;
    output.boolean(contents.is_some())?;
    if let Some(contents) = contents {
        output.raw(contents)?;
    }
    Ok(())
}
fn tag_registry(output: &mut PacketWriter, id: &str, tags: usize) -> Result<(), PacketError> {
    output.identifier(id)?;
    count(output, tags)
}
fn tag_entry(output: &mut PacketWriter, id: &str, members: &[i32]) -> Result<(), PacketError> {
    output.identifier(id)?;
    count(output, members.len())?;
    for member in members {
        output.varint(*member)?;
    }
    Ok(())
}

pub fn encode(packet: Clientbound<'_>, max_packet_bytes: usize) -> Result<Vec<u8>, PacketError> {
    let mut output = PacketWriter::new(max_packet_bytes);
    match packet {
        Clientbound::Brand(brand) => {
            output.varint(1)?;
            output.identifier("minecraft:brand")?;
            output.utf(brand, 32767)?;
        }
        Clientbound::EnabledFeatures(features) => {
            output.varint(13)?;
            count(&mut output, features.len())?;
            for feature in features {
                output.identifier(feature)?;
            }
        }
        Clientbound::SelectKnownPacks(packs) => {
            output.varint(15)?;
            count(&mut output, packs.len())?;
            for pack in packs {
                output.utf(&pack.namespace, 32767)?;
                output.utf(&pack.id, 32767)?;
                output.utf(&pack.version, 32767)?;
            }
        }
        Clientbound::Registry {
            registry,
            negotiated,
        } => {
            registry_header(&mut output, registry.id(), registry.entries().len())?;
            for entry in registry.entries() {
                let contents = negotiated.entry_contents(entry);
                registry_entry(&mut output, entry.id(), contents)?;
            }
        }
        Clientbound::UpdateTags(snapshot) => {
            output.varint(14)?;
            count(&mut output, snapshot.tags().len())?;
            for registry in snapshot.tags() {
                tag_registry(&mut output, registry.registry(), registry.tags().len())?;
                for tag in registry.tags() {
                    tag_entry(&mut output, tag.id(), tag.members())?;
                }
            }
        }
        Clientbound::KeepAlive(id) => {
            output.varint(4)?;
            output.long(id)?;
        }
        Clientbound::Disconnect(reason) => {
            output.varint(2)?;
            let mut bytes = Vec::new();
            nbt::write_network(
                &nbt::Tag::String(reason.into()),
                &mut bytes,
                nbt::Limits {
                    output_bytes: max_packet_bytes.saturating_sub(1),
                    ..nbt::Limits::default()
                },
            )
            .map_err(|_| PacketError::InvalidValue("disconnect component exceeds packet limit"))?;
            output.raw(&bytes)?;
        }
    }
    Ok(output.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_and_tag_field_writers_match_public_java_codec_oracle() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/configuration_packet_oracle.json"
        ))
        .unwrap();
        let mut registry = PacketWriter::new(512);
        registry_header(&mut registry, "test:registry", 2).unwrap();
        // Independently selected tiny network compound {answer:42}.
        registry_entry(
            &mut registry,
            "test:present",
            Some(&[
                10, 3, 0, 6, b'a', b'n', b's', b'w', b'e', b'r', 0, 0, 0, 42, 0,
            ]),
        )
        .unwrap();
        registry_entry(&mut registry, "test:omitted", None).unwrap();
        let mut tags = PacketWriter::new(512);
        tags.varint(14).unwrap();
        count(&mut tags, 1).unwrap();
        tag_registry(&mut tags, "test:registry", 1).unwrap();
        tag_entry(&mut tags, "test:tag", &[0, 2, 128]).unwrap();
        for (name, output) in [
            ("clientbound_registry", registry),
            ("clientbound_tags", tags),
        ] {
            let case = fixture["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["name"] == name)
                .unwrap();
            let encoded = output.as_bytes()[1..]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(encoded, case["payload_hex"].as_str().unwrap());
            assert_eq!(
                output.as_bytes()[0],
                case["packet_id"].as_u64().unwrap() as u8
            );
        }
    }
}
