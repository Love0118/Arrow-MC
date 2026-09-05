#![allow(dead_code)]
//! Synthetic authenticated registry bundles shared by registry, section, and pipeline tests.
use arrow_mc::world::storage::registry::{
    ChunkRegistrySnapshot, ExpectedRegistryReference, RegistryLoadLimits,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const VERSION: &str = "26.3-pre-2";
pub const PROTOCOL: i32 = 1_073_742_158;
pub const SOURCE_HASH: [u8; 32] = [0x38; 32];
pub const CONFIGURATION_HASH: [u8; 32] = [0x57; 32];
pub const SOURCE_BYTES: u64 = 1234;
pub const FILES: [&str; 5] = [
    "blocks.json",
    "biomes.json",
    "export-metadata.json",
    "block-entity-types.json",
    "lighting.bin",
];
static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
pub fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
pub fn digest(path: &Path) -> [u8; 32] {
    Sha256::digest(fs::read(path).unwrap()).into()
}

pub struct Fixture {
    pub root: PathBuf,
    pub expected: ExpectedRegistryReference,
}
impl Fixture {
    pub fn new() -> Self {
        Self::from_data(
            json!({"state_count":5,"state_flags":[1,0,2,0,2],"blocks":[
                {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
                {"id":"test:lamp","default_state":1,"properties":[
                    {"name":"facing","values":["north","south"],"default_index":0},
                    {"name":"lit","values":["false","true"],"default_index":1}
                ],"states":[4,1,3,2]}
            ]}),
            json!([
                {"id":"minecraft:plains","protocol_id":0},
                {"id":"minecraft:forest","protocol_id":1}
            ]),
        )
    }

    /// Derive authentication metadata for caller-authored small test domains.
    /// This does not validate the domain; malformed-domain tests use the real loader.
    pub fn from_data(mut blocks: Value, biomes: Value) -> Self {
        // Unrelated test fixtures explicitly choose empty synthetic tag sets.
        // Production schema has no default for a missing heightmap_tags field.
        for block in blocks["blocks"].as_array_mut().unwrap() {
            block
                .as_object_mut()
                .unwrap()
                .entry("heightmap_tags")
                .or_insert(json!(0));
        }
        let root = std::env::temp_dir().join(format!(
            "arrow-world-registry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let source = json!({"sha256":hex(&SOURCE_HASH),"bytes":SOURCE_BYTES});
        write_json(&root.join("blocks.json"), &blocks);
        // Unrelated fixtures explicitly choose non-emitting, shape-disabled
        // synthetic materials. Lighting tests replace these with their inputs.
        let states = blocks["state_count"].as_u64().unwrap() as u32;
        fs::write(
            root.join("lighting.bin"),
            lighting_bytes(&vec![[0; 16]; states as usize], 2, &[14]),
        )
        .unwrap();
        write_json(&root.join("biomes.json"), &biomes);
        write_json(
            &root.join("block-entity-types.json"),
            &json!([
                {"id":"test:lamp","protocol_id":0}, {"id":"minecraft:chest","protocol_id":1}
            ]),
        );
        write_json(
            &root.join("export-metadata.json"),
            &json!({"minecraft_version":VERSION,"protocol":PROTOCOL,"source_jar":source,
                "block_count":blocks["blocks"].as_array().unwrap().len(),
                "state_count":blocks["state_count"]}),
        );
        write_json(
            &root.join("manifest.json"),
            &json!({"format_version":3,"minecraft_version":VERSION,"protocol":PROTOCOL,
                "source_jar":source,"configuration_manifest_sha256":hex(&CONFIGURATION_HASH),
                "selected_packs":[{"id":"vanilla","version":VERSION,
                    "hash_kind":"source_jar_sha256","sha256":hex(&SOURCE_HASH)}],"files":[]}),
        );
        let mut fixture = Self {
            root,
            expected: ExpectedRegistryReference {
                manifest_sha256: [0; 32],
                configuration_manifest_sha256: CONFIGURATION_HASH,
                source_jar_sha256: SOURCE_HASH,
                source_jar_bytes: SOURCE_BYTES,
            },
        };
        fixture.refresh_descriptors();
        fixture.trust_current_manifest();
        fixture
    }
    pub fn refresh_descriptors(&self) {
        let mut manifest = json_file(&self.root.join("manifest.json"));
        manifest["files"] = json!(FILES.map(|name| {
            let path = self.root.join(name);
            json!({"path":name,"bytes":fs::metadata(&path).unwrap().len(),"sha256":hex(&digest(&path))})
        }));
        write_json(&self.root.join("manifest.json"), &manifest);
    }
    pub fn trust_current_manifest(&mut self) {
        self.expected.manifest_sha256 = digest(&self.root.join("manifest.json"));
    }
    pub fn edit_lighting(&mut self, edit: impl FnOnce(&mut Vec<u8>)) {
        let path = self.root.join("lighting.bin");
        let mut bytes = fs::read(&path).unwrap();
        edit(&mut bytes);
        fs::write(path, bytes).unwrap();
        self.refresh_descriptors();
        self.trust_current_manifest();
    }
    pub fn edit(&mut self, file: &str, edit: impl FnOnce(&mut Value)) {
        let path = self.root.join(file);
        let mut value = json_file(&path);
        edit(&mut value);
        write_json(&path, &value);
        if file != "manifest.json" {
            self.refresh_descriptors();
        }
        // Only semantic tests call this: each edited snapshot is explicitly re-anchored.
        self.trust_current_manifest();
    }
    pub fn load(&self) -> ChunkRegistrySnapshot {
        ChunkRegistrySnapshot::load(&self.root, &self.expected, RegistryLoadLimits::default())
            .unwrap()
    }
    pub fn rejects(&self, limits: RegistryLoadLimits) -> bool {
        ChunkRegistrySnapshot::load(&self.root, &self.expected, limits).is_err()
    }
}

pub fn lighting_bytes(materials: &[[u8; 16]], face_count: u32, pairs: &[u8]) -> Vec<u8> {
    let mut bytes = b"ARLITE3\0".to_vec();
    bytes.extend_from_slice(&(materials.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&face_count.to_le_bytes());
    for material in materials {
        bytes.extend_from_slice(material);
    }
    bytes.extend_from_slice(pairs);
    bytes
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let root = fs::canonicalize(&self.root).unwrap();
        let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
        assert_eq!(root.parent(), Some(temporary.as_path()));
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("arrow-world-registry-")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
