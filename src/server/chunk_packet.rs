//! Locked 26.3-pre-2 clientbound chunk/light and chunk-control packet encoding.
//!
//! Borrowed, already computed world data is encoded after exact size admission.
//! This module does not turn disk data into live heightmaps, run light updates,
//! produce block-entity update tags, or establish permission to enter Play/send
//! a chunk. Requirements were inspected in the corresponding official packet
//! codecs; the Rust input and bounded output design are independently written.

use std::fmt;

use crate::nbt::{self, Tag};
use crate::wire;
use crate::world::heightmap::HeightmapKind;
use crate::world::preparation::ChunkAddress;

use super::chunk_sender::ChunkPacket;

pub const CHUNK_WITH_LIGHT_ID: i32 = 0x2d;
pub const BATCH_START_ID: i32 = 0x0c;
pub const BATCH_FINISHED_ID: i32 = 0x0b;
pub const FORGET_CHUNK_ID: i32 = 0x25;
pub const CACHE_CENTER_ID: i32 = 0x5f;
pub const CACHE_RADIUS_ID: i32 = 0x60;
pub const MAX_SECTION_BYTES: usize = 2_097_152;
pub const MAX_LIGHT_UPDATE_BYTES: usize = 2_048;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Total packet ID + body, excluding outer framing/compression/encryption.
    /// Also caps each source mask's byte length before trailing-zero removal.
    pub packet_bytes: usize,
    /// Requested and actual capacity of the one newly allocated output Vec.
    /// Borrowed world/cache inputs and subsequent delivery copies are accounted
    /// by their owners; they overlap this allocation until explicitly released.
    pub allocation_bytes: usize,
    pub nbt: nbt::Limits,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            packet_bytes: 8 * 1024 * 1024,
            allocation_bytes: 8 * 1024 * 1024,
            nbt: nbt::Limits::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    PacketLimit,
    AllocationLimit,
    AllocationFailed,
    SectionLimit,
    LightUpdateLimit,
    MaskInputLimit,
    DuplicateHeightmap,
    InvalidBlockEntityType,
    ExpectedCompound,
    LengthOverflow,
    Nbt(nbt::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chunk packet: {self:?}")
    }
}
impl std::error::Error for Error {}

impl From<nbt::Error> for Error {
    fn from(error: nbt::Error) -> Self {
        Self::Nbt(error)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HeightmapEntry<'a> {
    pub kind: HeightmapKind,
    /// Raw packed bits, encoded as a VarInt count and big-endian 64-bit words.
    pub words: &'a [u64],
}

#[derive(Clone, Copy, Debug)]
pub struct BlockEntity<'a> {
    /// Upper nibble is section-local x, lower nibble section-local z.
    pub packed_xz: u8,
    pub y: i16,
    /// ID from the active verified block_entity_type registry, not a block ID.
    pub type_id: u32,
    /// The actual network update compound supplied by block-entity behavior.
    /// Raw disk block_entities cannot be forwarded as update tags. None emits
    /// End; Some(empty compound) remains present at this low-level codec. The
    /// live Vanilla producer elides an empty getUpdateTag result before encoding.
    pub update_tag: Option<&'a Tag>,
}

/// One light array without materializing a uniform layer into a temporary Vec.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LightUpdate<'a> {
    Bytes(&'a [u8]),
    /// Java DataLayer's low default byte, before its repeated-nibble expansion.
    /// Ordinary light values are 0..15; the raw representation also preserves
    /// values such as 16, which materialize as byte 0x10 rather than zero.
    /// A snapshot producer omits an implicit zero layer into its empty mask;
    /// Uniform(0) here explicitly encodes a present 2048-byte data array.
    Uniform(u8),
}

impl LightUpdate<'_> {
    fn len(self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Uniform(_) => MAX_LIGHT_UPDATE_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LightData<'a> {
    /// BitSet bytes: low-order bits/bytes first; trailing zero bytes are omitted.
    /// These are BYTE arrays in 26.3, not the older long-array representation.
    pub sky_mask: &'a [u8],
    pub block_mask: &'a [u8],
    pub empty_sky_mask: &'a [u8],
    pub empty_block_mask: &'a [u8],
    /// The raw codec allows lengths 0..=2048, independent of mask popcounts.
    /// Live DataLayer production requires real layer state and normally emits
    /// 2048 bytes. Allocated all-zero data is different from an empty DataLayer;
    /// this encoder does not infer layer absence or emptiness from byte content.
    pub sky_updates: &'a [LightUpdate<'a>],
    pub block_updates: &'a [LightUpdate<'a>],
}

#[derive(Clone, Copy, Debug)]
pub struct ChunkWithLight<'a> {
    pub position: ChunkAddress,
    /// Unique map keys, in the caller's intended map iteration order. All six
    /// types are representable by the raw codec. The live producer chooses
    /// send_to_client kinds 1/4/5. A decoded Java EnumMap sorts by type ID, while
    /// a live producer's HashMap has different iteration; no order is invented.
    pub heightmaps: &'a [HeightmapEntry<'a>],
    /// Complete section serialization in dimension section order. This codec
    /// checks the field bound; section/registry semantics belong to its producer.
    pub sections: &'a [u8],
    pub block_entities: &'a [BlockEntity<'a>],
    pub light: LightData<'a>,
}

/// Exact allocation-free sizing and validation. The registry count must come
/// from the active verified ChunkRegistrySnapshot::block_entity_type_count;
/// its ID domain is contiguous. No registry lookup or snapshot clone is needed
/// for already resolved IDs. Empty block-entity lists do not require a domain.
pub fn encoded_len(
    packet: &ChunkWithLight<'_>,
    block_entity_type_count: u32,
    limits: Limits,
) -> Result<usize, Error> {
    let mut length = Length {
        value: 0,
        limit: limits.packet_bytes,
    };
    length.add(wire::varint_len(CHUNK_WITH_LIGHT_ID) + 8)?;
    length.count(packet.heightmaps.len())?;
    let mut seen = 0u8;
    for heightmap in packet.heightmaps {
        let bit = 1 << heightmap.kind.id();
        if seen & bit != 0 {
            return Err(Error::DuplicateHeightmap);
        }
        seen |= bit;
        length.add(wire::varint_len(i32::from(heightmap.kind.id())))?;
        length.count(heightmap.words.len())?;
        length.add(
            heightmap
                .words
                .len()
                .checked_mul(8)
                .ok_or(Error::LengthOverflow)?,
        )?;
    }
    if packet.sections.len() > MAX_SECTION_BYTES {
        return Err(Error::SectionLimit);
    }
    length.array(packet.sections.len())?;
    length.count(packet.block_entities.len())?;
    // At least five bytes per entry prevents an unbounded metadata-only scan
    // before discovering that even absent tags cannot fit the output budget.
    if packet.block_entities.len() > limits.packet_bytes / 5 {
        return Err(Error::PacketLimit);
    }
    for entity in packet.block_entities {
        if entity.type_id >= block_entity_type_count || entity.type_id > i32::MAX as u32 {
            return Err(Error::InvalidBlockEntityType);
        }
        length.add(3 + wire::varint_len(entity.type_id as i32))?;
        match entity.update_tag {
            None => length.add(1)?,
            Some(tag @ Tag::Compound(_)) => {
                let mut tag_limits = limits.nbt;
                tag_limits.output_bytes = tag_limits.output_bytes.min(length.remaining());
                length.add(nbt::network_encoded_len(tag, tag_limits)?)?;
            }
            Some(_) => return Err(Error::ExpectedCompound),
        }
    }
    for mask in [
        packet.light.sky_mask,
        packet.light.block_mask,
        packet.light.empty_sky_mask,
        packet.light.empty_block_mask,
    ] {
        if mask.len() > limits.packet_bytes {
            return Err(Error::MaskInputLimit);
        }
        length.array(mask_length(mask))?;
    }
    for updates in [packet.light.sky_updates, packet.light.block_updates] {
        length.count(updates.len())?;
        if updates.len() > length.remaining() {
            return Err(Error::PacketLimit);
        }
        for update in updates {
            if update.len() > MAX_LIGHT_UPDATE_BYTES {
                return Err(Error::LightUpdateLimit);
            }
            length.array(update.len())?;
        }
    }
    Ok(length.value)
}

/// Encodes packet ID + body into one precisely admitted allocation. Perform
/// this synchronous kernel on the shared admitted CPU path for large packets,
/// with world/cache borrows held stable until return. Compression and framing
/// remain separate, and may reject an otherwise valid packet for their limits.
pub fn encode(
    packet: &ChunkWithLight<'_>,
    block_entity_type_count: u32,
    limits: Limits,
) -> Result<Vec<u8>, Error> {
    let length = encoded_len(packet, block_entity_type_count, limits)?;
    if length > limits.allocation_bytes {
        return Err(Error::AllocationLimit);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| Error::AllocationFailed)?;
    if output.capacity() > limits.allocation_bytes {
        return Err(Error::AllocationLimit);
    }
    put_varint(&mut output, CHUNK_WITH_LIGHT_ID);
    output.extend_from_slice(&packet.position.x.to_be_bytes());
    output.extend_from_slice(&packet.position.z.to_be_bytes());
    put_count(&mut output, packet.heightmaps.len());
    for heightmap in packet.heightmaps {
        put_varint(&mut output, i32::from(heightmap.kind.id()));
        put_count(&mut output, heightmap.words.len());
        for word in heightmap.words {
            output.extend_from_slice(&word.to_be_bytes());
        }
    }
    put_array(&mut output, packet.sections);
    put_count(&mut output, packet.block_entities.len());
    for entity in packet.block_entities {
        output.push(entity.packed_xz);
        output.extend_from_slice(&entity.y.to_be_bytes());
        put_varint(&mut output, entity.type_id as i32);
        if let Some(tag) = entity.update_tag {
            // Exact sizing above includes this root's ID and payload. The whole
            // output capacity is present, so the existing NBT writer cannot
            // acquire another allocation for this valid immutable input.
            nbt::write_network(tag, &mut output, limits.nbt)?;
        } else {
            output.push(0);
        }
    }
    for mask in [
        packet.light.sky_mask,
        packet.light.block_mask,
        packet.light.empty_sky_mask,
        packet.light.empty_block_mask,
    ] {
        put_array(&mut output, &mask[..mask_length(mask)]);
    }
    for updates in [packet.light.sky_updates, packet.light.block_updates] {
        put_count(&mut output, updates.len());
        for update in updates {
            match update {
                LightUpdate::Bytes(bytes) => put_array(&mut output, bytes),
                LightUpdate::Uniform(value) => {
                    put_count(&mut output, MAX_LIGHT_UPDATE_BYTES);
                    output.resize(output.len() + MAX_LIGHT_UPDATE_BYTES, value | (value << 4));
                }
            }
        }
    }
    debug_assert_eq!(output.len(), length);
    Ok(output)
}

struct Length {
    value: usize,
    limit: usize,
}

impl Length {
    fn add(&mut self, count: usize) -> Result<(), Error> {
        self.value = self.value.checked_add(count).ok_or(Error::LengthOverflow)?;
        if self.value > self.limit {
            return Err(Error::PacketLimit);
        }
        Ok(())
    }
    fn count(&mut self, count: usize) -> Result<(), Error> {
        let count = i32::try_from(count).map_err(|_| Error::LengthOverflow)?;
        self.add(wire::varint_len(count))
    }
    fn array(&mut self, count: usize) -> Result<(), Error> {
        self.count(count)?;
        self.add(count)
    }
    fn remaining(&self) -> usize {
        self.limit - self.value
    }
}

fn mask_length(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1)
}

fn put_varint(output: &mut Vec<u8>, value: i32) {
    let mut bytes = [0; 5];
    let count = wire::write_varint(value, &mut bytes).unwrap();
    output.extend_from_slice(&bytes[..count]);
}

fn put_count(output: &mut Vec<u8>, count: usize) {
    put_varint(output, count as i32);
}

fn put_array(output: &mut Vec<u8>, bytes: &[u8]) {
    put_count(output, bytes.len());
    output.extend_from_slice(bytes);
}

/// Stack storage for the largest control packet here: ID + two signed VarInts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmallPacket {
    bytes: [u8; 11],
    len: usize,
}

impl SmallPacket {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn new(id: i32) -> Self {
        let mut packet = Self {
            bytes: [0; 11],
            len: 0,
        };
        packet.varint(id);
        packet
    }
    fn varint(&mut self, value: i32) {
        self.len += wire::write_varint(value, &mut self.bytes[self.len..]).unwrap();
    }
}

pub fn batch_start() -> SmallPacket {
    SmallPacket::new(BATCH_START_ID)
}

/// The codec supports every signed i32. The scheduling producer emits the
/// actual positive count; negative values are not silently clamped here.
pub fn batch_finished(chunks: i32) -> SmallPacket {
    let mut output = SmallPacket::new(BATCH_FINISHED_ID);
    output.varint(chunks);
    output
}

pub fn forget(position: ChunkAddress) -> SmallPacket {
    let mut output = SmallPacket::new(FORGET_CHUNK_ID);
    let packed = u64::from(position.x as u32) | (u64::from(position.z as u32) << 32);
    output.bytes[output.len..output.len + 8].copy_from_slice(&packed.to_be_bytes());
    output.len += 8;
    output
}

pub fn cache_center(position: ChunkAddress) -> SmallPacket {
    let mut output = SmallPacket::new(CACHE_CENTER_ID);
    output.varint(position.x);
    output.varint(position.z);
    output
}

/// View configuration owns the 2..32 producer range. This raw packet codec
/// preserves signed VarInt values for exact field compatibility.
pub fn cache_radius(radius: i32) -> SmallPacket {
    let mut output = SmallPacket::new(CACHE_RADIUS_ID);
    output.varint(radius);
    output
}

/// Converts an already admitted queue intent into real packet bytes while
/// retaining its existing data borrow. The control scratch lives on the caller
/// stack; no copy or second allocation is needed for a queued chunk packet.
/// Feed the result to the existing transport's ordered write_packet; advance
/// the queue only after successful write, and close/fail its owner on failure.
pub fn delivery_bytes<'a>(
    packet: ChunkPacket<'a>,
    scratch: &'a mut SmallPacket,
) -> Result<&'a [u8], Error> {
    *scratch = match packet {
        ChunkPacket::Data { packet_bytes, .. } => return Ok(packet_bytes),
        ChunkPacket::Start => batch_start(),
        ChunkPacket::Finish { chunks } => {
            batch_finished(i32::try_from(chunks).map_err(|_| Error::LengthOverflow)?)
        }
        ChunkPacket::Forget { position } => forget(position),
    };
    Ok(scratch.as_bytes())
}
