use crate::wire::{read_varint, write_varint};
use std::io;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub(super) fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

pub(super) struct TrafficBudget(usize);

impl TrafficBudget {
    pub(super) fn new(bytes: usize) -> Self {
        Self(bytes)
    }

    fn reserve(&mut self, bytes: usize) -> io::Result<()> {
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or_else(|| invalid("connection byte budget exhausted"))?;
        Ok(())
    }
}

pub(super) async fn read_frame(
    stream: &mut TcpStream,
    output: &mut [u8],
    limit: usize,
    budget: &mut TrafficBudget,
) -> io::Result<usize> {
    let mut length = 0usize;
    for index in 0..3 {
        budget.reserve(1)?;
        let byte = stream.read_u8().await?;
        length |= usize::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            if length == 0 || length > limit || length > output.len() {
                return Err(invalid("invalid frame length for connection state"));
            }
            budget.reserve(length)?;
            stream.read_exact(&mut output[..length]).await?;
            return Ok(length);
        }
    }
    Err(invalid("frame length exceeds 21 bits"))
}

/// `frame` already contains its length prefix. Reserving the complete output
/// before writing prevents a budget failure from publishing a partial frame.
pub(super) async fn write_frame(
    stream: &mut TcpStream,
    frame: &[u8],
    budget: &mut TrafficBudget,
) -> io::Result<()> {
    budget.reserve(frame.len())?;
    stream.write_all(frame).await
}

pub(super) fn json_packet(value: serde_json::Value, max_utf16: usize) -> io::Result<Vec<u8>> {
    let json = serde_json::to_string(&value).map_err(invalid)?;
    if json.encode_utf16().count() > max_utf16 || json.len() > max_utf16 * 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "encoded JSON exceeds protocol string bound",
        ));
    }
    let mut string_length = [0; 5];
    let string_length_bytes =
        write_varint(json.len() as i32, &mut string_length).map_err(invalid)?;
    let body_length = 1 + string_length_bytes + json.len();
    if body_length > 0x1f_ffff {
        return Err(invalid("JSON packet exceeds frame limit"));
    }
    let mut frame_length = [0; 5];
    let frame_length_bytes =
        write_varint(body_length as i32, &mut frame_length).map_err(invalid)?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(frame_length_bytes + body_length)
        .map_err(io::Error::other)?;
    frame.extend_from_slice(&frame_length[..frame_length_bytes]);
    frame.push(0);
    frame.extend_from_slice(&string_length[..string_length_bytes]);
    frame.extend_from_slice(json.as_bytes());
    Ok(frame)
}

pub(super) struct Handshake {
    pub(super) protocol: i32,
    pub(super) intention: i32,
}

impl Handshake {
    pub(super) fn parse(mut bytes: &[u8]) -> io::Result<Self> {
        if take_varint(&mut bytes)? != 0 {
            return Err(invalid("expected handshake packet"));
        }
        let protocol = take_varint(&mut bytes)?;
        let host_bytes = take_varint(&mut bytes)?;
        if !(0..=765).contains(&host_bytes) {
            return Err(invalid("host exceeds encoded string bound"));
        }
        let host_length = host_bytes as usize;
        let hostname = bytes
            .get(..host_length)
            .ok_or_else(|| invalid("truncated hostname"))?;
        if java_utf16_length(hostname) > 255 {
            return Err(invalid("host exceeds UTF-16 string bound"));
        }
        bytes = &bytes[host_length..];
        let port = bytes.get(..2).ok_or_else(|| invalid("truncated port"))?;
        // The advertised destination port is an unsigned short; zero and 65535
        // are legal wire values. It does not override the actual bound listener.
        let _port = u16::from_be_bytes([port[0], port[1]]);
        bytes = &bytes[2..];
        let intention = take_varint(&mut bytes)?;
        if !(1..=3).contains(&intention) || !bytes.is_empty() {
            return Err(invalid("invalid handshake intention or trailing data"));
        }
        Ok(Self {
            protocol,
            intention,
        })
    }
}

fn take_varint(input: &mut &[u8]) -> io::Result<i32> {
    let (value, length) = read_varint(input).map_err(invalid)?;
    *input = &input[length..];
    Ok(value)
}

/// Counts Java's replacement-decoded UTF-8 as UTF-16, without keeping a hostname
/// that this state never uses. Malformed prefixes consume the valid prefix;
/// a complete encoded surrogate consumes three bytes as one replacement unit.
fn java_utf16_length(bytes: &[u8]) -> usize {
    let mut index = 0;
    let mut units = 0;
    while index < bytes.len() {
        let first = bytes[index];
        let width = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 1,
        };
        let mut consumed = 1;
        if width > 1
            && let Some(&second) = bytes.get(index + 1)
            && second & 0xc0 == 0x80
            && !(first == 0xe0 && second < 0xa0)
            && !(first == 0xf0 && second < 0x90)
            && !(first == 0xf4 && second > 0x8f)
        {
            consumed = 2;
            while consumed < width
                && bytes
                    .get(index + consumed)
                    .is_some_and(|byte| byte & 0xc0 == 0x80)
            {
                consumed += 1;
            }
        }
        units += if consumed == 4 { 2 } else { 1 };
        index += consumed;
    }
    units
}

#[cfg(test)]
mod tests {
    use super::java_utf16_length;

    #[test]
    fn java_malformed_utf8_groups_and_utf16_units() {
        for (bytes, count) in [
            (&b"hello"[..], 5),
            (&[0xed, 0xa0, 0x80], 1),
            (&[0xf0, 0x80, 0x80, 0x80], 4),
            (&[0xe1, 0x80], 1),
            (&[0xf4, 0x90, 0x80, 0x80], 4),
            (&[0xf0, 0x90, 0x80], 1),
            (&[0xc0, 0x80], 2),
            (&[0xe0, 0x80, 0x80], 3),
            (&[0xed, 0xa0, b'a'], 2),
            (&[0xf0, 0x9f, 0x98, 0x80], 2),
            (&[0xe1, 0x80, b'a'], 2),
        ] {
            assert_eq!(java_utf16_length(bytes), count, "{bytes:?}");
        }
    }
}
