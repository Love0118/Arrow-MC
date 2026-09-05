//! Fixed binary lighting tables prepared from initialized official state APIs.
//! Face pairs are ordered and retain the exact public Java predicate result.

use super::{Error, invalid};

pub const EMPTY_FACE: u16 = 0;
pub const FULL_FACE: u16 = 1;
const HEADER_BYTES: usize = 16;
const MATERIAL_BYTES: usize = 16;
const MAGIC: &[u8; 8] = b"ARLITE3\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LightMaterial {
    pub emission: u8,
    pub dampening: u8,
    pub can_occlude: bool,
    pub use_shape_for_light_occlusion: bool,
    /// Cached faces in DOWN, UP, NORTH, SOUTH, WEST, EAST order.
    /// These are raw cached faces; empty_shape applies the engine's flag gate.
    pub faces: [u16; 6],
}
impl LightMaterial {
    pub fn empty_shape(self) -> bool {
        !self.can_occlude || !self.use_shape_for_light_occlusion
    }
}
const _: () = assert!(std::mem::size_of::<LightMaterial>() == MATERIAL_BYTES);

#[derive(Debug)]
pub(super) struct Lighting {
    materials: Vec<LightMaterial>,
    face_count: usize,
    pairs: Vec<u8>,
}
impl Lighting {
    pub(super) fn parse(bytes: &[u8], state_count: u32, face_limit: usize) -> Result<Self, Error> {
        if bytes.get(..8) != Some(MAGIC) || bytes.len() < HEADER_BYTES {
            return Err(invalid("invalid lighting magic/header"));
        }
        let states = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let face_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        if states != state_count || !(2..=65536).contains(&face_count) {
            return Err(invalid("invalid lighting state/face counts"));
        }
        if face_count > face_limit {
            return Err(Error::Limit("lighting face count"));
        }
        let material_end = (states as usize)
            .checked_mul(MATERIAL_BYTES)
            .and_then(|length| length.checked_add(HEADER_BYTES))
            .ok_or(Error::Limit("lighting materials"))?;
        let pair_bits = face_count
            .checked_mul(face_count)
            .ok_or(Error::Limit("lighting face pairs"))?;
        let pair_bytes = pair_bits.div_ceil(8);
        if material_end.checked_add(pair_bytes) != Some(bytes.len()) {
            return Err(invalid("lighting payload length differs from its counts"));
        }
        let mut materials = Vec::new();
        materials
            .try_reserve_exact(states as usize)
            .map_err(|_| Error::Limit("lighting material allocation"))?;
        for encoded in bytes[HEADER_BYTES..material_end].chunks_exact(MATERIAL_BYTES) {
            if encoded[0] > 15 || encoded[1] > 15 || encoded[2] > 3 || encoded[3] != 0 {
                return Err(invalid("invalid lighting scalar/flags/reserved byte"));
            }
            let mut faces = [0; 6];
            for (index, face) in faces.iter_mut().enumerate() {
                *face =
                    u16::from_le_bytes(encoded[4 + index * 2..6 + index * 2].try_into().unwrap());
                if *face as usize >= face_count {
                    return Err(invalid("lighting material references unknown face"));
                }
            }
            if encoded[2] & 1 == 0 && faces != [EMPTY_FACE; 6] {
                return Err(invalid("non-occluding state has nonempty cached face"));
            }
            materials.push(LightMaterial {
                emission: encoded[0],
                dampening: encoded[1],
                can_occlude: encoded[2] & 1 != 0,
                use_shape_for_light_occlusion: encoded[2] & 2 != 0,
                faces,
            });
        }
        let encoded_pairs = &bytes[material_end..];
        let padding_bits = pair_bits % 8;
        if padding_bits != 0 && encoded_pairs.last().unwrap() >> padding_bits != 0 {
            return Err(invalid("nonzero lighting face-pair padding"));
        }
        let mut pairs = Vec::new();
        pairs
            .try_reserve_exact(pair_bytes)
            .map_err(|_| Error::Limit("lighting pair allocation"))?;
        pairs.extend_from_slice(encoded_pairs);
        let retained_capacity = materials
            .capacity()
            .checked_mul(MATERIAL_BYTES)
            .and_then(|bytes| bytes.checked_add(pairs.capacity()))
            .ok_or(Error::Limit("lighting retained capacity"))?;
        if retained_capacity > bytes.len() - HEADER_BYTES {
            return Err(Error::Limit("lighting retained capacity"));
        }
        let result = Self {
            materials,
            face_count,
            pairs,
        };
        if result.face_occludes(EMPTY_FACE, EMPTY_FACE) != Some(false)
            || (0..face_count).any(|id| {
                result.face_occludes(FULL_FACE, id as u16) != Some(true)
                    || result.face_occludes(id as u16, FULL_FACE) != Some(true)
            })
        {
            return Err(invalid("invalid canonical empty/full face-pair results"));
        }
        Ok(result)
    }

    pub(super) fn material(&self, id: u32) -> Option<LightMaterial> {
        self.materials.get(id as usize).copied()
    }
    pub(super) fn face_count(&self) -> usize {
        self.face_count
    }
    pub(super) fn face_occludes(&self, first: u16, second: u16) -> Option<bool> {
        if first as usize >= self.face_count || second as usize >= self.face_count {
            return None;
        }
        let bit = first as usize * self.face_count + second as usize;
        Some(self.pairs[bit / 8] & (1 << (bit % 8)) != 0)
    }

    #[cfg(test)]
    pub(super) fn air_only() -> Self {
        Self {
            materials: vec![LightMaterial {
                emission: 0,
                dampening: 0,
                can_occlude: false,
                use_shape_for_light_occlusion: false,
                faces: [EMPTY_FACE; 6],
            }],
            face_count: 2,
            pairs: vec![14],
        }
    }
}
