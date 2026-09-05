#![allow(dead_code)]
// Synthetic reference builder shared by data-integrity and configuration-session tests.
use arrow_mc::server::configuration_data::{
    ConfigurationSnapshot, Error, ExpectedReference, KnownPack, LoadLimits, PackFingerprint,
    REFERENCE_PROTOCOL, REFERENCE_VERSION, REQUIRED_REGISTRIES,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const JAR_HASH: [u8; 32] = [0x34; 32];

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn descriptor(path: &Path, name: &str) -> Value {
    let bytes = fs::read(path.join(name)).unwrap();
    json!({"path":name,"bytes":bytes.len(),"sha256":hex(&Sha256::digest(&bytes))})
}
pub fn core() -> KnownPack {
    KnownPack {
        namespace: "minecraft".into(),
        id: "core".into(),
        version: REFERENCE_VERSION.into(),
    }
}
fn core_json() -> Value {
    json!({"namespace":"minecraft","id":"core","version":REFERENCE_VERSION})
}

pub struct Fixture {
    pub root: PathBuf,
    packs: Vec<PackFingerprint>,
    trusted_manifest: Cell<[u8; 32]>,
}
impl Fixture {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "arrow-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("entries")).unwrap();
        let mut registries = Vec::new();
        for (index, id) in REQUIRED_REGISTRIES.iter().enumerate() {
            let name = format!("entries/{index:05}.nbt");
            fs::write(root.join(&name), [10, 0]).unwrap();
            let file = descriptor(&root, &name);
            registries.push(
                json!({"id":id,"entries":[{"id":"test:synthetic", "protocol_id":0,
                "known_pack":if index==0 {core_json()} else {Value::Null},"network_nbt_file":name,
                "bytes":file["bytes"],"sha256":file["sha256"]}]}),
            );
        }
        write_json(&root.join("registries.json"), &json!(registries));
        write_json(
            &root.join("static-domains.json"),
            &json!([{"id":"minecraft:item","entries":[{"id":"test:first","protocol_id":0},{"id":"test:second","protocol_id":1}]}]),
        );
        write_json(
            &root.join("tags.json"),
            &json!([
            {"id":REQUIRED_REGISTRIES[0],"tags":[{"id":"test:dynamic","members":[0]}]},
            {"id":"minecraft:item","tags":[{"id":"test:static","members":[1,0]},{"id":"test:empty","members":[]}]}]),
        );
        write_json(&root.join("known-packs.json"), &json!([core_json()]));
        write_json(&root.join("features.json"), &json!(["minecraft:vanilla"]));
        write_json(
            &root.join("export-metadata.json"),
            &json!({"minecraft_version":REFERENCE_VERSION,"protocol":REFERENCE_PROTOCOL,
            "source_jar":{"sha256":hex(&JAR_HASH),"bytes":123},"selected_pack_ids":["vanilla"],"known_packs":[core_json()]}),
        );
        write_json(
            &root.join("manifest.json"),
            &json!({"format_version":1,"minecraft_version":REFERENCE_VERSION,"protocol":REFERENCE_PROTOCOL,
            "configuration":"vanilla-only","source_jar":{"sha256":hex(&JAR_HASH),"bytes":123},
            "selected_packs":[{"id":"vanilla","version":REFERENCE_VERSION,"hash_kind":"source_jar_sha256","sha256":hex(&JAR_HASH)}],"files":[]}),
        );
        let this = Self {
            root,
            trusted_manifest: Cell::new([0; 32]),
            packs: vec![PackFingerprint {
                id: "vanilla".into(),
                version: REFERENCE_VERSION.into(),
                sha256: JAR_HASH,
            }],
        };
        this.refresh_manifest();
        this
    }
    pub fn expected(&self) -> ExpectedReference<'_> {
        ExpectedReference {
            expected_manifest_sha256: self.trusted_manifest.get(),
            minecraft_version: REFERENCE_VERSION,
            protocol: REFERENCE_PROTOCOL,
            source_jar_sha256: JAR_HASH,
            source_jar_bytes: 123,
            selected_packs: &self.packs,
        }
    }
    pub fn load(&self) -> Result<ConfigurationSnapshot, Error> {
        ConfigurationSnapshot::load(&self.root, &self.expected(), LoadLimits::default())
    }
    pub fn refresh_manifest(&self) {
        let mut files: Vec<String> = fs::read_dir(&self.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".json") && name != "manifest.json")
            .collect();
        files.extend(
            fs::read_dir(self.root.join("entries"))
                .unwrap()
                .map(|entry| format!("entries/{}", entry.unwrap().file_name().to_str().unwrap())),
        );
        files.sort();
        let mut manifest = read_json(&self.root.join("manifest.json"));
        manifest["files"] = json!(
            files
                .iter()
                .map(|name| descriptor(&self.root, name))
                .collect::<Vec<_>>()
        );
        write_json(&self.root.join("manifest.json"), &manifest);
        self.trust_current_manifest();
    }
    pub fn edit(&self, name: &str, edit: impl FnOnce(&mut Value)) {
        let path = self.root.join(name);
        let mut value = read_json(&path);
        edit(&mut value);
        write_json(&path, &value);
        if name != "manifest.json" {
            self.refresh_manifest();
        } else {
            self.trust_current_manifest();
        }
    }
    /// Explicitly bless a freshly authored semantic fixture. Integrity tests
    /// retain the pre-edit expected fingerprint instead of using this update.
    fn trust_current_manifest(&self) {
        self.trusted_manifest
            .set(Sha256::digest(fs::read(self.root.join("manifest.json")).unwrap()).into());
    }
    pub fn replace_payload(&self, bytes: &[u8]) {
        let name = "entries/00000.nbt";
        fs::write(self.root.join(name), bytes).unwrap();
        let file = descriptor(&self.root, name);
        self.edit("registries.json", |registries| {
            registries[0]["entries"][0]["bytes"] = file["bytes"].clone();
            registries[0]["entries"][0]["sha256"] = file["sha256"].clone();
        });
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
