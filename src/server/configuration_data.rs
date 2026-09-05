//! Immutable, validated data for the locked protocol's configuration registries.
//!
//! The local preparation tool invokes the official resource loader and network
//! element codecs. This module reads data, never recorded packet frames. The
//! caller supplies trusted reference fingerprints independently of the manifest.

use crate::nbt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

pub const REFERENCE_VERSION: &str = "26.3-pre-2";
pub const REFERENCE_PROTOCOL: i32 = 1_073_742_158;

/// Protocol registry order, from the locked public synchronization contract.
pub const REQUIRED_REGISTRIES: [&str; 32] = [
    "minecraft:worldgen/biome",
    "minecraft:chat_type",
    "minecraft:trim_pattern",
    "minecraft:trim_material",
    "minecraft:wolf_variant",
    "minecraft:wolf_sound_variant",
    "minecraft:pig_variant",
    "minecraft:pig_sound_variant",
    "minecraft:frog_variant",
    "minecraft:cat_variant",
    "minecraft:cat_sound_variant",
    "minecraft:cow_sound_variant",
    "minecraft:cow_variant",
    "minecraft:chicken_sound_variant",
    "minecraft:chicken_variant",
    "minecraft:zombie_nautilus_variant",
    "minecraft:painting_variant",
    "minecraft:sulfur_cube_archetype",
    "minecraft:dimension_type",
    "minecraft:damage_type",
    "minecraft:banner_pattern",
    "minecraft:enchantment",
    "minecraft:jukebox_song",
    "minecraft:instrument",
    "minecraft:test_environment",
    "minecraft:test_instance",
    "minecraft:dialog",
    "minecraft:world_clock",
    "minecraft:timeline",
    "minecraft:decorated_pot_pattern",
    "minecraft:block_transformer",
    "minecraft:worldgen/block_state_provider",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFingerprint {
    pub id: String,
    pub version: String,
    pub sha256: [u8; 32],
}

/// These expected values must come from verified local references/server setup,
/// not from the same untrusted manifest that is being checked.
pub struct ExpectedReference<'a> {
    /// Record this digest from the verified preparation operation, outside the
    /// bundle. Re-reading it from the bundle itself would remove its trust value.
    pub expected_manifest_sha256: [u8; 32],
    pub minecraft_version: &'a str,
    pub protocol: i32,
    pub source_jar_sha256: [u8; 32],
    pub source_jar_bytes: u64,
    pub selected_packs: &'a [PackFingerprint],
}

#[derive(Clone, Copy, Debug)]
pub struct LoadLimits {
    pub total_file_bytes: usize,
    pub file_bytes: usize,
    pub files: usize,
    /// Conservative admission policy for the pinned JSON schema/serde version,
    /// not an exact allocator/RSS ceiling. Metadata costs 128 budget bytes per input byte
    /// to cover overlapping JSON/typed collections; file/input caps are separate.
    /// One NBT decoder scratch allowance is reserved; payload bytes are retained.
    pub allocation_bytes: usize,
    pub nbt: nbt::Limits,
}

impl Default for LoadLimits {
    fn default() -> Self {
        Self {
            total_file_bytes: 64 * 1024 * 1024,
            file_bytes: 8 * 1024 * 1024,
            files: 8192,
            allocation_bytes: 256 * 1024 * 1024,
            nbt: nbt::Limits::default(),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Invalid(String),
    DigestMismatch(String),
    Limit(&'static str),
    Nbt(nbt::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "configuration snapshot I/O: {error}"),
            Self::Invalid(message) => write!(f, "invalid configuration snapshot: {message}"),
            Self::DigestMismatch(path) => {
                write!(f, "configuration snapshot SHA-256 mismatch: {path}")
            }
            Self::Limit(limit) => write!(f, "configuration snapshot limit exceeded: {limit}"),
            Self::Nbt(error) => write!(f, "invalid configuration entry NBT: {error}"),
        }
    }
}

impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
pub struct RegistryEntry {
    id: String,
    protocol_id: i32,
    known_pack: Option<KnownPack>,
    network_nbt: Box<[u8]>,
}

impl RegistryEntry {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn protocol_id(&self) -> i32 {
        self.protocol_id
    }
    pub fn known_pack(&self) -> Option<&KnownPack> {
        self.known_pack.as_ref()
    }
    /// Complete network-root NBT, available even when the client knows its pack.
    pub fn network_nbt(&self) -> &[u8] {
        &self.network_nbt
    }
}

#[derive(Debug)]
pub struct RegistryData {
    id: String,
    entries: Vec<RegistryEntry>,
}
impl RegistryData {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn entries(&self) -> &[RegistryEntry] {
        &self.entries
    }
}

#[derive(Debug)]
pub struct RegistryTag {
    id: String,
    members: Vec<i32>,
}
impl RegistryTag {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn members(&self) -> &[i32] {
        &self.members
    }
}

#[derive(Debug)]
pub struct RegistryTags {
    registry: String,
    tags: Vec<RegistryTag>,
}
impl RegistryTags {
    pub fn registry(&self) -> &str {
        &self.registry
    }
    pub fn tags(&self) -> &[RegistryTag] {
        &self.tags
    }
}

/// Share this object as a whole (for example Arc<ConfigurationSnapshot>), rather
/// than cloning entry bytes or assigning a lock/refcount to each entry.
#[derive(Debug)]
pub struct ConfigurationSnapshot {
    registries: Vec<RegistryData>,
    tags: Vec<RegistryTags>,
    known_packs: Vec<KnownPack>,
    features: Vec<String>,
    selected_packs: Vec<PackFingerprint>,
    manifest_sha256: [u8; 32],
    retained_nbt_bytes: usize,
}

/// An exact requested-list match enables omission. A subset, reordered list or
/// unknown additional pack falls back to all entry contents, without mutation.
pub struct NegotiatedPacks<'a> {
    accepted: &'a [KnownPack],
}
impl NegotiatedPacks<'_> {
    pub fn entry_contents<'a>(&self, entry: &'a RegistryEntry) -> Option<&'a [u8]> {
        if entry
            .known_pack
            .as_ref()
            .is_some_and(|pack| self.accepted.contains(pack))
        {
            None
        } else {
            Some(entry.network_nbt())
        }
    }
}

impl ConfigurationSnapshot {
    pub fn registries(&self) -> &[RegistryData] {
        &self.registries
    }
    pub fn tags(&self) -> &[RegistryTags] {
        &self.tags
    }
    pub fn known_packs(&self) -> &[KnownPack] {
        &self.known_packs
    }
    pub fn features(&self) -> &[String] {
        &self.features
    }
    pub fn selected_packs(&self) -> &[PackFingerprint] {
        &self.selected_packs
    }
    pub fn manifest_sha256(&self) -> &[u8; 32] {
        &self.manifest_sha256
    }
    pub fn retained_nbt_bytes(&self) -> usize {
        self.retained_nbt_bytes
    }
    pub fn negotiate_known_packs(&self, response: &[KnownPack]) -> NegotiatedPacks<'_> {
        NegotiatedPacks {
            accepted: if response == self.known_packs {
                &self.known_packs
            } else {
                &[]
            },
        }
    }

    pub fn load(
        root: &Path,
        expected: &ExpectedReference<'_>,
        limits: LoadLimits,
    ) -> Result<Self, Error> {
        if expected.minecraft_version != REFERENCE_VERSION
            || expected.protocol != REFERENCE_PROTOCOL
        {
            return Err(invalid("unsupported expected reference"));
        }
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(invalid("snapshot root is not a directory"));
        }
        let mut reader = SnapshotReader {
            root,
            limits,
            total_bytes: 0,
            admission: limits.nbt.allocation_bytes,
            files: BTreeMap::new(),
        };
        if reader.admission > limits.allocation_bytes {
            return Err(Error::Limit("NBT scratch admission"));
        }
        let manifest_bytes = reader.read("manifest.json", None, true)?;
        let manifest_sha256: [u8; 32] = Sha256::digest(&manifest_bytes).into();
        if manifest_sha256 != expected.expected_manifest_sha256 {
            return Err(Error::DigestMismatch("manifest.json".into()));
        }
        let manifest: Value =
            serde_json::from_slice(&manifest_bytes).map_err(|error| invalid(error.to_string()))?;
        if integer(&manifest, "format_version")? != 1
            || text(&manifest, "configuration")? != "vanilla-only"
        {
            return Err(invalid("unsupported snapshot format/configuration"));
        }
        verify_reference(&manifest, expected)?;
        let selected_packs = selected_packs(&manifest)?;
        if selected_packs != expected.selected_packs {
            return Err(invalid(
                "selected pack fingerprints/order differ from expected",
            ));
        }
        // Only this explicit prepared-pack configuration is implemented now.
        if selected_packs.len() != 1
            || selected_packs[0].id != "vanilla"
            || selected_packs[0].version != expected.minecraft_version
            || selected_packs[0].sha256 != expected.source_jar_sha256
        {
            return Err(invalid("vanilla-only pack does not match the source JAR"));
        }
        let descriptors = array(field(&manifest, "files")?)?;
        if descriptors.len() > limits.files {
            return Err(Error::Limit("file count"));
        }
        for descriptor in descriptors {
            let path = text(descriptor, "path")?;
            if !allowed_file(path) {
                return Err(invalid(format!("unexpected snapshot file {path}")));
            }
            let descriptor = FileDescriptor::parse(descriptor)?;
            if reader.files.insert(path.to_owned(), descriptor).is_some() {
                return Err(invalid("duplicate manifest file path"));
            }
        }
        drop(manifest);
        drop(manifest_bytes);

        let metadata = reader.json("export-metadata.json")?;
        verify_reference(&metadata, expected)?;
        let selected_ids = array(field(&metadata, "selected_pack_ids")?)?;
        if selected_ids.len() != 1 || selected_ids[0].as_str() != Some("vanilla") {
            return Err(invalid("export selected packs differ"));
        }
        let known_values = reader.json("known-packs.json")?;
        if field(&metadata, "known_packs")? != &known_values {
            return Err(invalid("known packs differ between metadata files"));
        }
        let known_packs: Vec<_> = array(&known_values)?
            .iter()
            .map(known_pack)
            .collect::<Result<_, _>>()?;
        if known_packs
            != [KnownPack {
                namespace: "minecraft".into(),
                id: "core".into(),
                version: expected.minecraft_version.into(),
            }]
        {
            return Err(invalid(
                "vanilla-only known pack list is not the actual core pack",
            ));
        }
        drop(metadata);
        drop(known_values);
        let feature_values = reader.json("features.json")?;
        let mut feature_set = BTreeSet::new();
        let mut features = allocated_vec(array(&feature_values)?.len())?;
        for value in array(&feature_values)? {
            let id = identifier_value(value)?;
            if !feature_set.insert(id.to_owned()) {
                return Err(invalid("duplicate enabled feature"));
            }
            features.push(id.to_owned());
        }
        if features != ["minecraft:vanilla"] {
            return Err(invalid("vanilla-only enabled features differ"));
        }
        drop(feature_values);

        let registry_values = reader.json("registries.json")?;
        let definitions = array(&registry_values)?;
        if definitions.len() != REQUIRED_REGISTRIES.len() {
            return Err(invalid("missing/extra synchronized registries"));
        }
        let mut registries = allocated_vec(definitions.len())?;
        let mut domains = BTreeMap::new();
        let mut retained_nbt_bytes = 0usize;
        for (definition, required) in definitions.iter().zip(REQUIRED_REGISTRIES) {
            let id = identifier(definition, "id")?;
            if id != required {
                return Err(invalid(format!(
                    "registry order/name: expected {required}, found {id}"
                )));
            }
            let values = array(field(definition, "entries")?)?;
            if values.is_empty() {
                return Err(invalid(format!("empty vanilla registry {id}")));
            }
            let mut names = BTreeSet::new();
            let mut entries = allocated_vec(values.len())?;
            for (index, value) in values.iter().enumerate() {
                let entry_id = identifier(value, "id")?;
                if !names.insert(entry_id) {
                    return Err(invalid(format!("duplicate entry {entry_id}")));
                }
                let protocol_id = protocol_index(value, index)?;
                let pack = field(value, "known_pack")?;
                let pack = if pack.is_null() {
                    None
                } else {
                    Some(known_pack(pack)?)
                };
                if pack
                    .as_ref()
                    .is_some_and(|pack| !known_packs.contains(pack))
                {
                    return Err(invalid("entry origin is not a selected known pack"));
                }
                let path = text(value, "network_nbt_file")?;
                if !entry_file(path) {
                    return Err(invalid("entry path is not a local NBT payload"));
                }
                let claimed = FileDescriptor::parse(value)?;
                let declared = reader
                    .files
                    .remove(path)
                    .ok_or_else(|| invalid(format!("missing/reused entry file {path}")))?;
                if claimed != declared {
                    return Err(invalid(format!("entry file descriptor differs: {path}")));
                }
                let bytes = reader.read(path, Some(&declared), false)?;
                let mut input = bytes.as_slice();
                let tag = nbt::read_network(&mut input, limits.nbt).map_err(Error::Nbt)?;
                if !input.is_empty() || matches!(tag, nbt::Tag::End) {
                    return Err(invalid("entry NBT is empty or has trailing bytes"));
                }
                drop(tag);
                retained_nbt_bytes = retained_nbt_bytes
                    .checked_add(bytes.len())
                    .ok_or(Error::Limit("payload bytes"))?;
                entries.push(RegistryEntry {
                    id: entry_id.to_owned(),
                    protocol_id,
                    known_pack: pack,
                    network_nbt: bytes.into_boxed_slice(),
                });
            }
            domains.insert(id.to_owned(), entries.len());
            registries.push(RegistryData {
                id: id.to_owned(),
                entries,
            });
        }
        drop(registry_values);

        let static_values = reader.json("static-domains.json")?;
        if array(&static_values)?.is_empty() {
            return Err(invalid("missing static tag domains"));
        }
        for definition in array(&static_values)? {
            let id = identifier(definition, "id")?;
            let values = array(field(definition, "entries")?)?;
            let mut names = BTreeSet::new();
            for (index, value) in values.iter().enumerate() {
                protocol_index(value, index)?;
                if !names.insert(identifier(value, "id")?) {
                    return Err(invalid("duplicate static entry"));
                }
            }
            if domains.insert(id.to_owned(), values.len()).is_some() {
                return Err(invalid("duplicate/static-dynamic registry domain"));
            }
        }
        drop(static_values);
        let tag_values = reader.json("tags.json")?;
        let mut tag_domains = BTreeSet::new();
        let mut tags = allocated_vec(array(&tag_values)?.len())?;
        for registry in array(&tag_values)? {
            let id = identifier(registry, "id")?;
            if !tag_domains.insert(id) {
                return Err(invalid("duplicate tag registry"));
            }
            let domain_size = *domains
                .get(id)
                .ok_or_else(|| invalid(format!("unknown tag member domain {id}")))?;
            let values = array(field(registry, "tags")?)?;
            if values.is_empty() {
                return Err(invalid("empty tag registry should be omitted"));
            }
            let mut names = BTreeSet::new();
            let mut registry_tags = allocated_vec(values.len())?;
            for value in values {
                let tag_id = identifier(value, "id")?;
                if !names.insert(tag_id) {
                    return Err(invalid("duplicate tag identifier"));
                }
                let members = array(field(value, "members")?)?;
                let mut member_ids = allocated_vec(members.len())?;
                let mut unique_members = BTreeSet::new();
                for member in members {
                    let number = member
                        .as_u64()
                        .and_then(|n| i32::try_from(n).ok())
                        .ok_or_else(|| invalid("invalid tag member ID"))?;
                    if number as usize >= domain_size {
                        return Err(invalid(format!(
                            "tag {tag_id} references ID {number} outside {id}"
                        )));
                    }
                    if !unique_members.insert(number) {
                        return Err(invalid("duplicate member in resolved tag"));
                    }
                    member_ids.push(number);
                }
                registry_tags.push(RegistryTag {
                    id: tag_id.to_owned(),
                    members: member_ids,
                });
            }
            tags.push(RegistryTags {
                registry: id.to_owned(),
                tags: registry_tags,
            });
        }
        if !reader.files.is_empty() {
            return Err(invalid("unconsumed manifest files"));
        }
        Ok(Self {
            registries,
            tags,
            known_packs,
            features,
            selected_packs,
            manifest_sha256,
            retained_nbt_bytes,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileDescriptor {
    bytes: usize,
    sha256: [u8; 32],
}
impl FileDescriptor {
    fn parse(value: &Value) -> Result<Self, Error> {
        Ok(Self {
            bytes: usize::try_from(integer(value, "bytes")?)
                .map_err(|_| invalid("file size overflow"))?,
            sha256: parse_sha256(text(value, "sha256")?)?,
        })
    }
}

struct SnapshotReader {
    root: PathBuf,
    limits: LoadLimits,
    total_bytes: usize,
    admission: usize,
    files: BTreeMap<String, FileDescriptor>,
}
impl SnapshotReader {
    fn json(&mut self, path: &str) -> Result<Value, Error> {
        let descriptor = self
            .files
            .remove(path)
            .ok_or_else(|| invalid(format!("missing metadata file {path}")))?;
        let bytes = self.read(path, Some(&descriptor), true)?;
        serde_json::from_slice(&bytes).map_err(|error| invalid(format!("{path}: {error}")))
    }

    fn read(
        &mut self,
        path: &str,
        expected: Option<&FileDescriptor>,
        metadata: bool,
    ) -> Result<Vec<u8>, Error> {
        let resolved = self.root.join(path).canonicalize()?;
        if !resolved.starts_with(&self.root) {
            return Err(invalid("snapshot path leaves its root"));
        }
        let file = File::open(&resolved)?;
        let bytes =
            usize::try_from(file.metadata()?.len()).map_err(|_| Error::Limit("file size"))?;
        if bytes > self.limits.file_bytes {
            return Err(Error::Limit("file bytes"));
        }
        if expected.is_some_and(|expected| expected.bytes != bytes) {
            return Err(invalid(format!("file size differs: {path}")));
        }
        self.total_bytes = self
            .total_bytes
            .checked_add(bytes)
            .ok_or(Error::Limit("total file bytes"))?;
        if self.total_bytes > self.limits.total_file_bytes {
            return Err(Error::Limit("total file bytes"));
        }
        let charge = bytes
            .checked_mul(if metadata { 128 } else { 2 })
            .and_then(|n| n.checked_add(1))
            .ok_or(Error::Limit("allocation admission"))?;
        self.admission = self
            .admission
            .checked_add(charge)
            .ok_or(Error::Limit("allocation admission"))?;
        if self.admission > self.limits.allocation_bytes {
            return Err(Error::Limit("allocation admission"));
        }
        let capacity = bytes.checked_add(1).ok_or(Error::Limit("file size"))?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| Error::Limit("file allocation"))?;
        file.take(capacity as u64).read_to_end(&mut output)?;
        if output.len() != bytes {
            return Err(invalid(format!("file changed while loading: {path}")));
        }
        if expected
            .is_some_and(|expected| <[u8; 32]>::from(Sha256::digest(&output)) != expected.sha256)
        {
            return Err(Error::DigestMismatch(path.to_owned()));
        }
        Ok(output)
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

fn allocated_vec<T>(capacity: usize) -> Result<Vec<T>, Error> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| Error::Limit("typed collection allocation"))?;
    Ok(values)
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, Error> {
    value
        .get(name)
        .ok_or_else(|| invalid(format!("missing {name}")))
}
fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str, Error> {
    field(value, name)?
        .as_str()
        .ok_or_else(|| invalid(format!("{name} is not a string")))
}
fn integer(value: &Value, name: &str) -> Result<u64, Error> {
    field(value, name)?
        .as_u64()
        .ok_or_else(|| invalid(format!("{name} is not an unsigned integer")))
}
fn array(value: &Value) -> Result<&[Value], Error> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("expected JSON array"))
}
fn identifier<'a>(value: &'a Value, name: &str) -> Result<&'a str, Error> {
    identifier_value(field(value, name)?)
}
fn identifier_value(value: &Value) -> Result<&str, Error> {
    let value = value
        .as_str()
        .ok_or_else(|| invalid("identifier is not a string"))?;
    let (namespace, path) = value
        .split_once(':')
        .ok_or_else(|| invalid("identifier lacks namespace"))?;
    let part = |value: &str, slash: bool| {
        !value.is_empty()
            && value.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'_' | b'-' | b'.')
                    || slash && b == b'/'
            })
    };
    if value.len() > 32767 || !part(namespace, false) || !part(path, true) {
        return Err(invalid(format!("invalid identifier {value}")));
    }
    Ok(value)
}
fn protocol_index(value: &Value, index: usize) -> Result<i32, Error> {
    let number = integer(value, "protocol_id")?;
    if number != index as u64 {
        return Err(invalid("registry ID differs from ordered entry index"));
    }
    i32::try_from(number).map_err(|_| invalid("registry ID exceeds VarInt range"))
}
fn known_pack(value: &Value) -> Result<KnownPack, Error> {
    let part = |name| -> Result<String, Error> {
        let value = text(value, name)?;
        if value.is_empty() || value.len() > 32767 {
            return Err(invalid("invalid known-pack string"));
        }
        Ok(value.to_owned())
    };
    Ok(KnownPack {
        namespace: part("namespace")?,
        id: part("id")?,
        version: part("version")?,
    })
}
fn selected_packs(value: &Value) -> Result<Vec<PackFingerprint>, Error> {
    array(field(value, "selected_packs")?)?
        .iter()
        .map(|pack| {
            if text(pack, "hash_kind")? != "source_jar_sha256" {
                return Err(invalid("unsupported pack fingerprint kind"));
            }
            Ok(PackFingerprint {
                id: text(pack, "id")?.to_owned(),
                version: text(pack, "version")?.to_owned(),
                sha256: parse_sha256(text(pack, "sha256")?)?,
            })
        })
        .collect()
}
fn verify_reference(value: &Value, expected: &ExpectedReference<'_>) -> Result<(), Error> {
    if text(value, "minecraft_version")? != expected.minecraft_version
        || integer(value, "protocol")? != expected.protocol as u64
    {
        return Err(invalid("snapshot version/protocol differs from expected"));
    }
    let jar = field(value, "source_jar")?;
    if parse_sha256(text(jar, "sha256")?)? != expected.source_jar_sha256
        || integer(jar, "bytes")? != expected.source_jar_bytes
    {
        return Err(invalid("source JAR differs from expected reference"));
    }
    Ok(())
}
pub fn parse_sha256(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(invalid("SHA-256 must be 64 lowercase hexadecimal digits"));
    }
    let mut hash = [0; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| invalid("invalid SHA-256"))?;
    }
    Ok(hash)
}
fn entry_file(path: &str) -> bool {
    path.strip_prefix("entries/")
        .and_then(|path| path.strip_suffix(".nbt"))
        .is_some_and(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()))
}
fn allowed_file(path: &str) -> bool {
    matches!(
        path,
        "registries.json"
            | "tags.json"
            | "static-domains.json"
            | "known-packs.json"
            | "features.json"
            | "export-metadata.json"
    ) || entry_file(path)
}
