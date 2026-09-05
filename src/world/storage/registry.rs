//! Immutable current-version block-state/biome IDs for disk palette decoding.
//!
//! Data is prepared from official public APIs outside the repository. The
//! independent manifest digest binds the file set; JAR/configuration fingerprints
//! keep this table aligned with the separately admitted connection snapshot.

use crate::nbt::{Compound, NbtString, Tag};
use crate::world::section::Registry;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const VERSION: &str = "26.3-pre-2";
const PROTOCOL: u32 = 1_073_742_158;
const FILES: [&str; 4] = [
    "blocks.json",
    "biomes.json",
    "export-metadata.json",
    "block-entity-types.json",
];

#[derive(Clone, Copy, Debug)]
pub struct ExpectedRegistryReference {
    /// Obtain independently from the trusted preparation command, not this bundle.
    pub manifest_sha256: [u8; 32],
    pub configuration_manifest_sha256: [u8; 32],
    pub source_jar_sha256: [u8; 32],
    pub source_jar_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct RegistryLoadLimits {
    pub file_bytes: usize,
    pub total_file_bytes: usize,
    /// Fixed admission policy: 128 budget bytes per JSON input byte, covering
    /// deserialization and retained tables. This is not an allocator/RSS bound;
    /// serde/string allocations are not individually fallible or accounted.
    pub allocation_bytes: usize,
    pub blocks: usize,
    pub states: usize,
    pub biomes: usize,
    pub block_entity_types: usize,
}
impl Default for RegistryLoadLimits {
    fn default() -> Self {
        Self {
            file_bytes: 4 * 1024 * 1024,
            total_file_bytes: 8 * 1024 * 1024,
            allocation_bytes: 128 * 1024 * 1024,
            blocks: 65536,
            states: 1 << 20,
            biomes: 65536,
            block_entity_types: 4096,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Invalid(String),
    Limit(&'static str),
    DigestMismatch(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "registry snapshot I/O: {error}"),
            Self::Invalid(message) => write!(f, "invalid registry snapshot: {message}"),
            Self::Limit(limit) => write!(f, "registry snapshot admission: {limit}"),
            Self::DigestMismatch(path) => write!(f, "registry snapshot digest mismatch: {path}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedId {
    pub id: u32,
    /// A malformed/unknown value required a lossy recovery. Omitted optional
    /// properties and ignored unknown property names are ordinary codec defaults.
    pub used_fallback: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateFlags {
    pub is_air: bool,
    pub has_fluid: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Property {
    name: String,
    values: Vec<String>,
    default_index: usize,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Block {
    id: String,
    /// Bound to the manifest's configuration tags: motion and motion-no-leaves.
    heightmap_tags: u8,
    default_state: u32,
    properties: Vec<Property>,
    /// Complete mixed-radix property product, last property varying fastest.
    states: Vec<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Blocks {
    state_count: u32,
    state_flags: Vec<u8>,
    blocks: Vec<Block>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedId {
    id: String,
    protocol_id: u32,
}
#[derive(Deserialize)]
struct Jar {
    sha256: String,
    bytes: u64,
}
#[derive(Deserialize)]
struct Pack {
    id: String,
    version: String,
    hash_kind: String,
    sha256: String,
}
#[derive(Deserialize)]
struct Descriptor {
    path: String,
    bytes: usize,
    sha256: String,
}
#[derive(Deserialize)]
struct Manifest {
    format_version: u32,
    minecraft_version: String,
    protocol: u32,
    source_jar: Jar,
    selected_packs: Vec<Pack>,
    configuration_manifest_sha256: String,
    files: Vec<Descriptor>,
}
#[derive(Deserialize)]
struct Metadata {
    minecraft_version: String,
    protocol: u32,
    source_jar: Jar,
    block_count: usize,
    state_count: u32,
}

#[derive(Debug)]
pub struct ChunkRegistrySnapshot {
    blocks: Vec<Block>,
    state_flags: Vec<u8>,
    biomes: Vec<NamedId>,
    block_entity_types: Vec<NamedId>,
    blocks_domain: Registry,
    biomes_domain: Registry,
    air: u32,
    plains: u32,
    manifest_sha256: [u8; 32],
    configuration_manifest_sha256: [u8; 32],
}
impl ChunkRegistrySnapshot {
    pub fn load(
        root: &Path,
        expected: &ExpectedRegistryReference,
        limits: RegistryLoadLimits,
    ) -> Result<Self, Error> {
        let mut reader = Reader {
            root: root.canonicalize()?,
            limits,
            total: 0,
            admission: 0,
        };
        let manifest_bytes = reader.read("manifest.json", None)?;
        if <[u8; 32]>::from(Sha256::digest(&manifest_bytes)) != expected.manifest_sha256 {
            return Err(Error::DigestMismatch("manifest.json".into()));
        }
        let manifest: Manifest = json(&manifest_bytes)?;
        if manifest.format_version != 2
            || manifest.minecraft_version != VERSION
            || manifest.protocol != PROTOCOL
        {
            return Err(invalid("unsupported version/protocol/format"));
        }
        verify_jar(&manifest.source_jar, expected)?;
        if digest(&manifest.configuration_manifest_sha256)?
            != expected.configuration_manifest_sha256
        {
            return Err(invalid("configuration snapshot differs from expected"));
        }
        match manifest.selected_packs.as_slice() {
            [pack]
                if pack.id == "vanilla"
                    && pack.version == VERSION
                    && pack.hash_kind == "source_jar_sha256"
                    && digest(&pack.sha256)? == expected.source_jar_sha256 => {}
            _ => return Err(invalid("selected packs differ from locked vanilla core")),
        }
        if manifest.files.len() != FILES.len()
            || FILES
                .iter()
                .any(|name| manifest.files.iter().filter(|d| d.path == *name).count() != 1)
        {
            return Err(invalid("expected exactly the four registry data files"));
        }
        let descriptor = |name: &str| {
            manifest
                .files
                .iter()
                .find(|d| d.path == name)
                .expect("file inventory validated")
        };
        let block_bytes = reader.read(FILES[0], Some(descriptor(FILES[0])))?;
        let mut data: Blocks = json(&block_bytes)?;
        drop(block_bytes);
        let biome_bytes = reader.read(FILES[1], Some(descriptor(FILES[1])))?;
        let mut biomes: Vec<NamedId> = json(&biome_bytes)?;
        drop(biome_bytes);
        let metadata: Metadata = json(&reader.read(FILES[2], Some(descriptor(FILES[2])))?)?;
        let mut block_entity_types: Vec<NamedId> =
            json(&reader.read(FILES[3], Some(descriptor(FILES[3])))?)?;
        verify_jar(&metadata.source_jar, expected)?;
        if metadata.minecraft_version != VERSION
            || metadata.protocol != PROTOCOL
            || metadata.block_count != data.blocks.len()
            || metadata.state_count != data.state_count
        {
            return Err(invalid("export metadata differs from registry data"));
        }
        if data.blocks.is_empty()
            || data.blocks.len() > limits.blocks
            || data.state_count as usize > limits.states
            || biomes.is_empty()
            || biomes.len() > limits.biomes
            || block_entity_types.is_empty()
            || block_entity_types.len() > limits.block_entity_types
            || block_entity_types.len() > i32::MAX as usize
        {
            return Err(Error::Limit("registry entry counts"));
        }
        let blocks_domain =
            Registry::new(data.state_count).map_err(|_| invalid("invalid state count"))?;
        let biomes_domain = Registry::new(
            u32::try_from(biomes.len()).map_err(|_| invalid("biome count overflow"))?,
        )
        .map_err(|_| invalid("invalid biome count"))?;
        if data.state_flags.len() != data.state_count as usize
            || data.state_flags.iter().any(|&flags| flags > 3)
        {
            return Err(invalid("invalid state flags"));
        }
        let mut seen = Vec::new();
        seen.try_reserve_exact(data.state_count as usize)
            .map_err(|_| Error::Limit("state validation allocation"))?;
        seen.resize(data.state_count as usize, false);
        let mut value_names = Vec::new();
        for block in &data.blocks {
            if !identifier(&block.id) || block.heightmap_tags > 3 {
                return Err(invalid("invalid block identifier or heightmap tags"));
            }
            let mut combinations = 1usize;
            let mut default_ordinal = 0usize;
            let mut previous = None;
            for property in &block.properties {
                if !property_name(&property.name)
                    || previous.is_some_and(|name: &str| name >= property.name.as_str())
                    || property.values.len() < 2
                    || property.default_index >= property.values.len()
                {
                    return Err(invalid("invalid property name/order/default/domain"));
                }
                previous = Some(property.name.as_str());
                value_names.clear();
                value_names
                    .try_reserve_exact(property.values.len())
                    .map_err(|_| Error::Limit("property validation allocation"))?;
                for value in &property.values {
                    if !property_name(value) {
                        return Err(invalid("invalid/duplicate property value"));
                    }
                    value_names.push(value.as_str());
                }
                value_names.sort_unstable();
                if value_names.windows(2).any(|pair| pair[0] == pair[1]) {
                    return Err(invalid("duplicate property value"));
                }
                combinations = combinations
                    .checked_mul(property.values.len())
                    .filter(|&n| n <= limits.states)
                    .ok_or(Error::Limit("property product"))?;
                default_ordinal = default_ordinal * property.values.len() + property.default_index;
            }
            if combinations != block.states.len()
                || block.states.get(default_ordinal) != Some(&block.default_state)
            {
                return Err(invalid(
                    "incomplete property product or wrong default state",
                ));
            }
            for &id in &block.states {
                let slot = seen
                    .get_mut(id as usize)
                    .ok_or_else(|| invalid("state ID outside global domain"))?;
                if std::mem::replace(slot, true) {
                    return Err(invalid("duplicate global state ID"));
                }
                data.state_flags[id as usize] |= block.heightmap_tags << 2;
            }
        }
        if seen.iter().any(|&present| !present) {
            return Err(invalid("missing global state ID"));
        }
        data.blocks.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        if data.blocks.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(invalid("duplicate block identifier"));
        }
        validate_named_ids(&mut biomes)?;
        validate_named_ids(&mut block_entity_types)?;
        let air_block = data
            .blocks
            .iter()
            .find(|block| block.id == "minecraft:air")
            .ok_or_else(|| invalid("missing air block"))?;
        let air = air_block.default_state;
        if !air_block.properties.is_empty() || data.state_flags[air as usize] & 3 != 1 {
            return Err(invalid("invalid air state"));
        }
        let plains = biomes
            .iter()
            .find(|biome| biome.id == "minecraft:plains")
            .ok_or_else(|| invalid("missing plains biome"))?
            .protocol_id;
        Ok(Self {
            blocks: data.blocks,
            state_flags: data.state_flags,
            biomes,
            block_entity_types,
            blocks_domain,
            biomes_domain,
            air,
            plains,
            manifest_sha256: expected.manifest_sha256,
            configuration_manifest_sha256: expected.configuration_manifest_sha256,
        })
    }

    pub fn manifest_sha256(&self) -> [u8; 32] {
        self.manifest_sha256
    }
    pub fn configuration_manifest_sha256(&self) -> [u8; 32] {
        self.configuration_manifest_sha256
    }
    pub fn block_registry(&self) -> Registry {
        self.blocks_domain
    }
    pub fn biome_registry(&self) -> Registry {
        self.biomes_domain
    }
    pub fn state_count(&self) -> u32 {
        self.blocks_domain.state_count()
    }
    pub fn biome_count(&self) -> u32 {
        self.biomes_domain.state_count()
    }
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }
    pub fn air_id(&self) -> u32 {
        self.air
    }
    pub fn plains_id(&self) -> u32 {
        self.plains
    }
    pub fn state_flags(&self, id: u32) -> Option<StateFlags> {
        self.state_flags.get(id as usize).map(|flags| StateFlags {
            is_air: flags & 1 != 0,
            has_fluid: flags & 2 != 0,
        })
    }

    /// Predicate bits in current Heightmap.Types ID order (0..5). Priming's
    /// separate literal Blocks.AIR skip is the kernel's responsibility.
    pub fn heightmap_mask(&self, id: u32) -> Option<u8> {
        self.state_flags.get(id as usize).map(|&flags| {
            let surface = if flags & 1 == 0 { 0b000011 } else { 0 };
            let floor = if flags & 4 != 0 { 0b001100 } else { 0 };
            let motion = if flags & (4 | 2) != 0 { 0b010000 } else { 0 };
            let no_leaves = if flags & (8 | 2) != 0 { 0b100000 } else { 0 };
            surface | floor | motion | no_leaves
        })
    }

    pub fn block_entity_type_count(&self) -> u32 {
        self.block_entity_types.len() as u32
    }

    /// Resolves the network type domain only; disk NBT is not an update tag.
    pub fn block_entity_type_id(&self, name: &NbtString) -> Option<u32> {
        let (prefix, units) = identifier_units(name);
        self.block_entity_types
            .binary_search_by(|entry| compare_identifier(&entry.id, prefix, units))
            .ok()
            .map(|index| self.block_entity_types[index].protocol_id)
    }

    /// Current BlockState.CODEC string default or lowercase `{id,properties}`.
    /// Unknown/malformed block values recover to air at the disk palette boundary.
    /// Each known property independently defaults on invalid input; a non-map
    /// properties container uses the complete default. No temporary strings.
    pub fn block_state(&self, value: &Tag) -> ResolvedId {
        let (name, properties) = match value {
            Tag::String(name) => (name, None),
            Tag::Compound(compound) => match field(compound, "id") {
                Some(Tag::String(name)) => (name, field(compound, "properties")),
                _ => {
                    return ResolvedId {
                        id: self.air,
                        used_fallback: true,
                    };
                }
            },
            _ => {
                return ResolvedId {
                    id: self.air,
                    used_fallback: true,
                };
            }
        };
        let (prefix, units) = identifier_units(name);
        let Ok(index) = self
            .blocks
            .binary_search_by(|block| compare_identifier(&block.id, prefix, units))
        else {
            return ResolvedId {
                id: self.air,
                used_fallback: true,
            };
        };
        let block = &self.blocks[index];
        let properties = match properties {
            _ if block.properties.is_empty() => {
                return ResolvedId {
                    id: block.default_state,
                    used_fallback: false,
                };
            }
            None => {
                return ResolvedId {
                    id: block.default_state,
                    used_fallback: false,
                };
            }
            Some(Tag::Compound(properties)) => properties,
            _ => {
                return ResolvedId {
                    id: block.default_state,
                    used_fallback: true,
                };
            }
        };
        let mut ordinal = 0;
        let mut used_fallback = false;
        for property in &block.properties {
            let index = match field(properties, &property.name) {
                None => property.default_index,
                Some(Tag::String(value)) => property
                    .values
                    .iter()
                    .position(|name| compare_ascii(name, value.as_utf16()) == Ordering::Equal)
                    .unwrap_or_else(|| {
                        used_fallback = true;
                        property.default_index
                    }),
                _ => {
                    used_fallback = true;
                    property.default_index
                }
            };
            ordinal = ordinal * property.values.len() + index;
        }
        ResolvedId {
            id: block.states[ordinal],
            used_fallback,
        }
    }

    pub fn biome(&self, value: &Tag) -> ResolvedId {
        if let Tag::String(name) = value
            && let (prefix, units) = identifier_units(name)
            && let Ok(index) = self
                .biomes
                .binary_search_by(|biome| compare_identifier(&biome.id, prefix, units))
        {
            return ResolvedId {
                id: self.biomes[index].protocol_id,
                used_fallback: false,
            };
        }
        ResolvedId {
            id: self.plains,
            used_fallback: true,
        }
    }
}

fn compare_ascii(ascii: &str, units: &[u16]) -> Ordering {
    ascii.bytes().map(u16::from).cmp(units.iter().copied())
}
fn validate_named_ids(values: &mut [NamedId]) -> Result<(), Error> {
    for (index, value) in values.iter().enumerate() {
        if !identifier(&value.id) || value.protocol_id as usize != index {
            return Err(invalid("invalid registry identifier or ordered ID"));
        }
    }
    values.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    if values.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(invalid("duplicate registry identifier"));
    }
    Ok(())
}
fn identifier_units(input: &NbtString) -> (&'static [u16], &[u16]) {
    const DEFAULT_PREFIX: &[u16] = &[109, 105, 110, 101, 99, 114, 97, 102, 116, 58];
    let units = input.as_utf16();
    match units.iter().position(|&unit| unit == u16::from(b':')) {
        None => (DEFAULT_PREFIX, units),
        Some(0) => (DEFAULT_PREFIX, &units[1..]),
        Some(_) => (&[], units),
    }
}
fn compare_identifier(stored: &str, prefix: &[u16], units: &[u16]) -> Ordering {
    stored
        .bytes()
        .map(u16::from)
        .cmp(prefix.iter().chain(units).copied())
}
fn field<'a>(compound: &'a Compound, name: &str) -> Option<&'a Tag> {
    compound
        .entries()
        .binary_search_by(|entry| compare_ascii(name, entry.name.as_utf16()).reverse())
        .ok()
        .map(|index| &compound.entries()[index].value)
}
fn property_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32767
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn identifier(value: &str) -> bool {
    let Some((namespace, path)) = value.split_once(':') else {
        return false;
    };
    !namespace.is_empty()
        && !path.is_empty()
        && value.len() <= 32767
        && namespace.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
        })
        && path.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.' | b'/')
        })
}
fn verify_jar(jar: &Jar, expected: &ExpectedRegistryReference) -> Result<(), Error> {
    if jar.bytes != expected.source_jar_bytes || digest(&jar.sha256)? != expected.source_jar_sha256
    {
        return Err(invalid("source JAR differs from expected"));
    }
    Ok(())
}
fn digest(text: &str) -> Result<[u8; 32], Error> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("SHA-256 must be lowercase hexadecimal"));
    }
    let mut value = [0; 32];
    for (index, byte) in value.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| invalid("invalid SHA-256"))?;
    }
    Ok(value)
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
fn json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))
}

// Fixed four-file format: this reader deliberately has no registry/plugin
// discovery hooks. All JSON admission occurs before serde allocates its tables.
struct Reader {
    root: PathBuf,
    limits: RegistryLoadLimits,
    total: usize,
    admission: usize,
}
impl Reader {
    fn read(&mut self, path: &str, descriptor: Option<&Descriptor>) -> Result<Vec<u8>, Error> {
        let resolved = self.root.join(path).canonicalize()?;
        if !resolved.starts_with(&self.root) {
            return Err(invalid("file leaves snapshot root"));
        }
        let file = File::open(resolved)?;
        let size =
            usize::try_from(file.metadata()?.len()).map_err(|_| Error::Limit("file bytes"))?;
        if size > self.limits.file_bytes {
            return Err(Error::Limit("file bytes"));
        }
        if descriptor.is_some_and(|expected| size != expected.bytes) {
            return Err(invalid("file size differs from manifest"));
        }
        self.total = self
            .total
            .checked_add(size)
            .ok_or(Error::Limit("total file bytes"))?;
        self.admission = size
            .checked_mul(128)
            .and_then(|charge| self.admission.checked_add(charge))
            .ok_or(Error::Limit("allocation admission"))?;
        if self.total > self.limits.total_file_bytes {
            return Err(Error::Limit("total file bytes"));
        }
        if self.admission > self.limits.allocation_bytes {
            return Err(Error::Limit("allocation admission"));
        }
        let capacity = size.checked_add(1).ok_or(Error::Limit("file bytes"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| Error::Limit("file allocation"))?;
        file.take(capacity as u64).read_to_end(&mut bytes)?;
        if bytes.len() != size {
            return Err(invalid("file changed during read"));
        }
        if let Some(expected) = descriptor
            && <[u8; 32]>::from(Sha256::digest(&bytes)) != digest(&expected.sha256)?
        {
            return Err(Error::DigestMismatch(path.into()));
        }
        Ok(bytes)
    }
}

/// Internal I/O cancellation tests need a resident identity before decoding starts.
/// Public loading always uses the authenticated bundle path above.
#[cfg(test)]
pub(crate) fn storage_test_snapshot() -> ChunkRegistrySnapshot {
    ChunkRegistrySnapshot {
        blocks: vec![Block {
            id: "minecraft:air".into(),
            heightmap_tags: 0,
            default_state: 0,
            properties: Vec::new(),
            states: vec![0],
        }],
        state_flags: vec![1],
        biomes: vec![NamedId {
            id: "minecraft:plains".into(),
            protocol_id: 0,
        }],
        block_entity_types: vec![NamedId {
            id: "test:entity".into(),
            protocol_id: 0,
        }],
        blocks_domain: Registry::new(1).unwrap(),
        biomes_domain: Registry::new(1).unwrap(),
        air: 0,
        plains: 0,
        manifest_sha256: [0; 32],
        configuration_manifest_sha256: [0; 32],
    }
}
