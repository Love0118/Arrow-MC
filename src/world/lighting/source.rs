//! Immutable input for lighting. Palette data is shared from resident chunks or
//! explicitly adopted from a producer that already budgeted its owned sections.
use super::{LightBlock, LightError, LightSection};
use crate::{
    runtime::ResidentChunk,
    world::{
        loading::ChunkLoadingOwner,
        preparation::ChunkAddress,
        section::Section,
        storage::{chunk::DimensionHeight, registry::ChunkRegistrySnapshot},
    },
};
use std::sync::Arc;

#[derive(Clone)]
pub struct SourceStamp(Arc<()>);
impl PartialEq for SourceStamp {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for SourceStamp {}
impl std::fmt::Debug for SourceStamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceStamp")
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SourceLimits {
    pub max_chunks: usize,
    pub metadata_bytes: usize,
    pub owned_section_bytes: usize,
}
impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_chunks: 1024,
            metadata_bytes: 8 * 1024 * 1024,
            owned_section_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Already owned and admitted producer input. Empty slots mean real default air
/// sections in this available chunk, not an invented available missing chunk.
pub struct LightingChunk {
    pub address: ChunkAddress,
    pub sections: Vec<Option<Section>>,
}
enum Data {
    Resident {
        resident: Arc<ResidentChunk>,
        indices: Vec<usize>,
    },
    Owned(Vec<Option<Section>>),
}
struct Chunk {
    address: ChunkAddress,
    data: Data,
}
pub struct LightingSource {
    registry: Arc<ChunkRegistrySnapshot>,
    height: DimensionHeight,
    chunks: Vec<Chunk>,
    air: u32,
    bedrock: u32,
    stamp: SourceStamp,
    owner_revision: Option<Arc<()>>,
    metadata_bytes: usize,
    owned_section_bytes: usize,
}
impl LightingSource {
    fn validate(
        registry: &ChunkRegistrySnapshot,
        height: DimensionHeight,
    ) -> Result<u32, LightError> {
        if i32::from(height.min_section()) * 16 < -2032
            || (i32::from(height.max_section()) + 1) * 16 > 2032
        {
            return Err(LightError::InvalidLimits);
        }
        let bedrock = registry.bedrock_id().ok_or(LightError::MissingBedrock)?;
        if registry.light_material(registry.air_id()).is_none()
            || registry.light_material(bedrock).is_none()
        {
            return Err(LightError::MissingLightMetadata);
        }
        Ok(bedrock)
    }
    /// Captures the supplied available-for-lighting domain. Other chunks are
    /// unavailable, even if a broader owner holds unrelated data. Any owner
    /// publication/removal/reload invalidates the stamp, including absence reads.
    pub fn from_canonical(
        owner: &ChunkLoadingOwner,
        addresses: &[ChunkAddress],
        limits: SourceLimits,
    ) -> Result<Self, LightError> {
        let registry = owner.source_registry();
        let height = owner.height();
        let bedrock = Self::validate(&registry, height)?;
        if addresses.len() > limits.max_chunks {
            return Err(LightError::InvalidLimits);
        }
        let mut remaining = limits.metadata_bytes;
        let mut chunks = reserve(addresses.len(), &mut remaining)?;
        let section_count =
            (i32::from(height.max_section()) - i32::from(height.min_section()) + 1) as usize;
        for &address in addresses {
            validate_address(address)?;
            let resident = owner
                .snapshot_data(address)
                .ok_or(LightError::MissingChunk)?;
            let mut indices = reserve(section_count, &mut remaining)?;
            indices.resize(section_count, usize::MAX);
            for (index, section) in resident.draft().sections().iter().enumerate() {
                if height.contains(section.y) && section.section.is_some() {
                    indices[(i32::from(section.y) - i32::from(height.min_section())) as usize] =
                        index;
                }
            }
            chunks.push(Chunk {
                address,
                data: Data::Resident { resident, indices },
            });
        }
        sort_unique(&mut chunks)?;
        Ok(Self {
            air: registry.air_id(),
            registry,
            height,
            chunks,
            bedrock,
            stamp: SourceStamp(Arc::new(())),
            owner_revision: Some(owner.source_revision()),
            metadata_bytes: limits.metadata_bytes - remaining,
            owned_section_bytes: 0,
        })
    }
    /// Adopts producer-owned sections after validating both retained allowances.
    /// The producer is responsible for admission before constructing its input.
    pub fn from_sections(
        registry: Arc<ChunkRegistrySnapshot>,
        height: DimensionHeight,
        input: Vec<LightingChunk>,
        limits: SourceLimits,
    ) -> Result<Self, LightError> {
        let bedrock = Self::validate(&registry, height)?;
        if input.len() > limits.max_chunks {
            return Err(LightError::InvalidLimits);
        }
        let expected =
            (i32::from(height.max_section()) - i32::from(height.min_section()) + 1) as usize;
        let input_bytes = input
            .capacity()
            .checked_mul(size_of::<LightingChunk>())
            .ok_or(LightError::AllocationLimit)?;
        let mut remaining = limits
            .metadata_bytes
            .checked_sub(input_bytes)
            .ok_or(LightError::AllocationLimit)?;
        let mut chunks = reserve(input.len(), &mut remaining)?;
        let mut owned = 0usize;
        for chunk in input {
            validate_address(chunk.address)?;
            if chunk.sections.len() != expected {
                return Err(LightError::InvalidLimits);
            }
            let bytes = chunk
                .sections
                .capacity()
                .checked_mul(size_of::<Option<Section>>())
                .ok_or(LightError::AllocationLimit)?;
            remaining = remaining
                .checked_sub(bytes)
                .ok_or(LightError::AllocationLimit)?;
            for section in chunk.sections.iter().flatten() {
                if section.counts.non_empty_blocks > 4096 || section.counts.fluid_blocks > 4096 {
                    return Err(LightError::InvalidState);
                }
                owned = owned
                    .checked_add(section.blocks.heap_bytes())
                    .and_then(|v| v.checked_add(section.biomes.heap_bytes()))
                    .ok_or(LightError::AllocationLimit)?;
                if owned > limits.owned_section_bytes {
                    return Err(LightError::AllocationLimit);
                }
                let mut non_empty = 0;
                let mut fluids = 0;
                for index in 0..4096 {
                    let id = section
                        .blocks
                        .get(index)
                        .map_err(|_| LightError::InvalidState)?;
                    if registry.light_material(id).is_none() {
                        return Err(LightError::InvalidState);
                    }
                    let flags = registry.state_flags(id).ok_or(LightError::InvalidState)?;
                    non_empty += u16::from(!flags.is_air);
                    fluids += u16::from(flags.has_fluid);
                }
                if non_empty != section.counts.non_empty_blocks
                    || fluids != section.counts.fluid_blocks
                {
                    return Err(LightError::InvalidState);
                }
            }
            chunks.push(Chunk {
                address: chunk.address,
                data: Data::Owned(chunk.sections),
            });
        }
        sort_unique(&mut chunks)?;
        Ok(Self {
            air: registry.air_id(),
            registry,
            height,
            chunks,
            bedrock,
            stamp: SourceStamp(Arc::new(())),
            owner_revision: None,
            metadata_bytes: limits.metadata_bytes - remaining - input_bytes,
            owned_section_bytes: owned,
        })
    }
    pub fn registry(&self) -> &ChunkRegistrySnapshot {
        &self.registry
    }
    pub fn height(&self) -> DimensionHeight {
        self.height
    }
    pub fn stamp(&self) -> SourceStamp {
        self.stamp.clone()
    }
    pub fn metadata_bytes(&self) -> usize {
        self.metadata_bytes
    }
    pub fn owned_section_bytes(&self) -> usize {
        self.owned_section_bytes
    }
    pub fn heap_bytes(&self) -> usize {
        self.metadata_bytes + self.owned_section_bytes
    }
    pub fn is_current(&self, owner: &ChunkLoadingOwner) -> bool {
        self.owner_revision
            .as_ref()
            .is_some_and(|v| Arc::ptr_eq(v, &owner.source_revision()))
    }
    pub fn has_chunk(&self, address: ChunkAddress) -> bool {
        self.chunk(address).is_some()
    }
    pub fn chunk_addresses(&self) -> impl ExactSizeIterator<Item = ChunkAddress> + '_ {
        self.chunks.iter().map(|v| v.address)
    }
    fn chunk(&self, address: ChunkAddress) -> Option<&Chunk> {
        self.chunks
            .binary_search_by_key(&address, |v| v.address)
            .ok()
            .map(|i| &self.chunks[i])
    }
    fn section(&self, address: ChunkAddress, y: i32) -> Option<&Section> {
        let y = i8::try_from(y).ok()?;
        if !self.height.contains(y) {
            return None;
        }
        let index = (i32::from(y) - i32::from(self.height.min_section())) as usize;
        match &self.chunk(address)?.data {
            Data::Owned(sections) => sections[index].as_ref(),
            Data::Resident { resident, indices } => resident
                .draft()
                .sections()
                .get(indices[index])?
                .section
                .as_ref(),
        }
    }
    pub fn section_has_only_air(&self, section: LightSection) -> bool {
        self.section(section.column(), section.y)
            .is_none_or(|s| s.counts.non_empty_blocks == 0)
    }
    /// Lighting-only lookup. Padding AIR is light-equivalent to ProtoChunk's
    /// VOID_AIR in the pinned cache; this is not a generic block-query interface.
    pub fn state_in_chunk(&self, address: ChunkAddress, x: u8, y: i32, z: u8) -> Option<u32> {
        if x >= 16 || z >= 16 || !self.has_chunk(address) {
            return None;
        }
        let Some(section) = self.section(address, y >> 4) else {
            return Some(self.air);
        };
        if section.counts.non_empty_blocks == 0 {
            return Some(self.air);
        }
        Some(
            section
                .blocks
                .get(((y & 15) << 8 | i32::from(z) << 4 | i32::from(x)) as usize)
                .expect("validated immutable section"),
        )
    }
    pub fn state_at(&self, pos: LightBlock) -> u32 {
        self.state_in_chunk(pos.column(), (pos.x & 15) as u8, pos.y, (pos.z & 15) as u8)
            .unwrap_or(self.bedrock)
    }
    pub fn emission_sources(&self, address: ChunkAddress) -> EmissionSources<'_> {
        EmissionSources {
            source: self,
            address,
            section: i32::from(self.height.min_section()),
            index: 0,
        }
    }
}
fn validate_address(address: ChunkAddress) -> Result<(), LightError> {
    if !(-2_097_061..=2_097_061).contains(&address.x)
        || !(-2_097_061..=2_097_061).contains(&address.z)
    {
        return Err(LightError::InvalidCoordinate);
    }
    Ok(())
}
pub struct EmissionSources<'a> {
    source: &'a LightingSource,
    address: ChunkAddress,
    section: i32,
    index: usize,
}
impl Iterator for EmissionSources<'_> {
    type Item = (LightBlock, u32);
    fn next(&mut self) -> Option<Self::Item> {
        while self.section <= i32::from(self.source.height.max_section()) {
            let section = self.source.section(self.address, self.section);
            if section.is_none_or(|s| s.counts.non_empty_blocks == 0) {
                self.section += 1;
                self.index = 0;
                continue;
            }
            let section = section.unwrap();
            while self.index < 4096 {
                let i = self.index;
                self.index += 1;
                let id = section.blocks.get(i).expect("validated immutable section");
                if self
                    .source
                    .registry
                    .light_material(id)
                    .expect("validated light material")
                    .emission
                    > 0
                {
                    return Some((
                        LightBlock {
                            x: self
                                .address
                                .x
                                .wrapping_mul(16)
                                .wrapping_add((i & 15) as i32),
                            y: self.section * 16 + (i >> 8) as i32,
                            z: self
                                .address
                                .z
                                .wrapping_mul(16)
                                .wrapping_add(((i >> 4) & 15) as i32),
                        },
                        id,
                    ));
                }
            }
            self.section += 1;
            self.index = 0;
        }
        None
    }
}
fn reserve<T>(count: usize, remaining: &mut usize) -> Result<Vec<T>, LightError> {
    let requested = count
        .checked_mul(size_of::<T>())
        .ok_or(LightError::AllocationLimit)?;
    if requested > *remaining {
        return Err(LightError::AllocationLimit);
    }
    let mut v = Vec::new();
    v.try_reserve_exact(count)
        .map_err(|_| LightError::AllocationFailed)?;
    let bytes = v
        .capacity()
        .checked_mul(size_of::<T>())
        .ok_or(LightError::AllocationLimit)?;
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or(LightError::AllocationLimit)?;
    Ok(v)
}
fn sort_unique(chunks: &mut [Chunk]) -> Result<(), LightError> {
    chunks.sort_unstable_by_key(|v| v.address);
    if chunks.windows(2).any(|v| v[0].address == v[1].address) {
        Err(LightError::DuplicateChunk)
    } else {
        Ok(())
    }
}
