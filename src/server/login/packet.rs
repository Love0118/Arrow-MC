//! Concrete 26.3-pre-2 login packet payloads; framing belongs to the transport.

use super::AuthenticatedProfile;
use crate::server::packet::{PacketError, PacketReader, PacketWriter};

pub const MAX_QUERY_BYTES: usize = 1024 * 1024;
pub const MAX_COOKIE_BYTES: usize = 5120;

#[derive(Debug)]
pub enum LoginPacket<'a> {
    Hello {
        name: String,
        claimed_id: [u8; 16],
    },
    Key {
        encrypted_secret: &'a [u8],
        encrypted_challenge: &'a [u8],
    },
    QueryAnswer {
        transaction_id: i32,
    },
    Acknowledged,
    CookieResponse {
        key: String,
        payload: Option<&'a [u8]>,
    },
}

/// Decodes one complete unframed serverbound login packet, including its ID.
/// Unknown query bytes are deliberately discarded after the transaction ID:
/// this matches the locked decoder's asymmetry rather than inventing a flag.
pub fn decode(input: &[u8]) -> Result<LoginPacket<'_>, PacketError> {
    let mut reader = PacketReader::new(input);
    let packet = match reader.varint()? {
        0 => LoginPacket::Hello {
            name: reader.utf(16)?,
            claimed_id: reader.uuid()?,
        },
        1 => LoginPacket::Key {
            encrypted_secret: reader.bytes(input.len())?,
            encrypted_challenge: reader.bytes(input.len())?,
        },
        2 => {
            let transaction_id = reader.varint()?;
            reader.remaining_bytes(MAX_QUERY_BYTES)?;
            LoginPacket::QueryAnswer { transaction_id }
        }
        3 => LoginPacket::Acknowledged,
        4 => {
            let key = reader.identifier()?;
            let payload = if reader.boolean()? {
                Some(reader.bytes(MAX_COOKIE_BYTES)?)
            } else {
                None
            };
            LoginPacket::CookieResponse { key, payload }
        }
        _ => return Err(PacketError::InvalidValue("unknown login packet ID")),
    };
    reader.finish()?;
    Ok(packet)
}

/// HELLO challenge for online authentication. RSA key/challenge generation and
/// signature verification are supplied by the dedicated crypto module.
pub fn hello(
    public_key_der: &[u8],
    challenge: &[u8; 4],
    max_bytes: usize,
) -> Result<Vec<u8>, PacketError> {
    let mut writer = PacketWriter::new(max_bytes);
    writer.varint(1)?;
    writer.utf("", 20)?;
    writer.bytes(public_key_der, max_bytes)?;
    writer.bytes(challenge, 4)?;
    writer.boolean(true)?;
    Ok(writer.into_bytes())
}

pub fn finished(
    profile: &AuthenticatedProfile,
    session_id: [u8; 16],
    max_bytes: usize,
) -> Result<Vec<u8>, PacketError> {
    if profile.properties.len() > 16 {
        return Err(PacketError::InvalidValue("too many profile properties"));
    }
    let mut writer = PacketWriter::new(max_bytes);
    writer.varint(2)?;
    writer.uuid(profile.id)?;
    writer.utf(&profile.name, 16)?;
    writer.varint(profile.properties.len() as i32)?;
    // Authlib's multimap groups values by first-seen property name, retaining
    // per-name order. Sixteen entries need no temporary grouping allocation.
    for (index, first) in profile.properties.iter().enumerate() {
        if profile.properties[..index]
            .iter()
            .any(|property| property.name == first.name)
        {
            continue;
        }
        for property in profile
            .properties
            .iter()
            .filter(|property| property.name == first.name)
        {
            writer.utf(&property.name, 64)?;
            writer.utf(&property.value, 32767)?;
            writer.boolean(property.signature.is_some())?;
            if let Some(signature) = &property.signature {
                writer.utf(signature, 1024)?;
            }
        }
    }
    writer.uuid(session_id)?;
    Ok(writer.into_bytes())
}

pub fn disconnect(reason: serde_json::Value, max_bytes: usize) -> Result<Vec<u8>, PacketError> {
    let json = serde_json::to_string(&reason)
        .map_err(|_| PacketError::InvalidValue("invalid disconnect JSON"))?;
    let mut writer = PacketWriter::new(max_bytes);
    writer.varint(0)?;
    writer.utf(&json, 262144)?;
    Ok(writer.into_bytes())
}
