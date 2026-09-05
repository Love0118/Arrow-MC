use arrow_mc::server::configuration_data::{
    ConfigurationSnapshot, Error, ExpectedReference, KnownPack, LoadLimits, PackFingerprint,
    REFERENCE_PROTOCOL, REFERENCE_VERSION, REQUIRED_REGISTRIES, parse_sha256,
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

fn hex(bytes: &[u8]) -> String {
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
fn core() -> KnownPack {
    KnownPack {
        namespace: "minecraft".into(),
        id: "core".into(),
        version: REFERENCE_VERSION.into(),
    }
}
fn core_json() -> Value {
    json!({"namespace":"minecraft","id":"core","version":REFERENCE_VERSION})
}

struct Fixture {
    root: PathBuf,
    packs: Vec<PackFingerprint>,
    trusted_manifest: Cell<[u8; 32]>,
}
impl Fixture {
    fn new() -> Self {
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
    fn expected(&self) -> ExpectedReference<'_> {
        ExpectedReference {
            expected_manifest_sha256: self.trusted_manifest.get(),
            minecraft_version: REFERENCE_VERSION,
            protocol: REFERENCE_PROTOCOL,
            source_jar_sha256: JAR_HASH,
            source_jar_bytes: 123,
            selected_packs: &self.packs,
        }
    }
    fn load(&self) -> Result<ConfigurationSnapshot, Error> {
        ConfigurationSnapshot::load(&self.root, &self.expected(), LoadLimits::default())
    }
    fn refresh_manifest(&self) {
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
    fn edit(&self, name: &str, edit: impl FnOnce(&mut Value)) {
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
    fn replace_payload(&self, bytes: &[u8]) {
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

#[test]
fn synthetic_snapshot_preserves_order_domains_and_negotiates_full_list() {
    let fixture = Fixture::new();
    let snapshot = fixture.load().unwrap();
    assert_eq!(snapshot.registries().len(), 32);
    assert_eq!(snapshot.retained_nbt_bytes(), 64);
    assert_eq!(snapshot.tags()[1].tags()[0].members(), [1, 0]);
    assert!(snapshot.tags()[1].tags()[1].members().is_empty());
    assert_eq!(snapshot.features(), ["minecraft:vanilla"]);
    let known = &snapshot.registries()[0].entries()[0];
    let custom = &snapshot.registries()[1].entries()[0];
    assert_eq!(known.id(), "test:synthetic");
    assert_eq!(known.protocol_id(), 0);
    assert!(
        snapshot
            .negotiate_known_packs(&[core()])
            .entry_contents(known)
            .is_none()
    );
    assert_eq!(
        snapshot
            .negotiate_known_packs(&[core()])
            .entry_contents(custom),
        Some(&[10, 0][..])
    );
    assert_eq!(
        snapshot.negotiate_known_packs(&[]).entry_contents(known),
        Some(&[10, 0][..])
    );
    let other = KnownPack {
        id: "another".into(),
        ..core()
    };
    for response in [
        vec![other.clone()],
        vec![core(), other.clone()],
        vec![other, core()],
        vec![core(), core()],
    ] {
        assert!(
            snapshot
                .negotiate_known_packs(&response)
                .entry_contents(known)
                .is_some()
        );
    }
}

#[test]
fn rejects_reference_or_selected_pack_mismatch() {
    let fixture = Fixture::new();
    let mut expected = fixture.expected();
    expected.source_jar_sha256 = [0; 32];
    assert!(ConfigurationSnapshot::load(&fixture.root, &expected, LoadLimits::default()).is_err());
    fixture.edit("manifest.json", |value| value["protocol"] = json!(777));
    assert!(fixture.load().is_err());
    let fixture = Fixture::new();
    fixture.edit("manifest.json", |value| {
        value["selected_packs"][0]["sha256"] = json!(hex(&[0; 32]))
    });
    assert!(fixture.load().is_err());
}

#[test]
fn detects_payload_and_metadata_corruption_before_use() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("entries/00000.nbt"), [8, 0]).unwrap();
    assert!(matches!(fixture.load(), Err(Error::DigestMismatch(_))));
    let fixture = Fixture::new();
    let path = fixture.root.join("tags.json");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = b' ';
    fs::write(path, bytes).unwrap();
    assert!(matches!(fixture.load(), Err(Error::DigestMismatch(_))));
}

#[test]
fn independent_manifest_fingerprint_rejects_reauthored_tags_or_known_pack_contents() {
    for replace_payload in [false, true] {
        let fixture = Fixture::new();
        let trusted_before_edit = fixture.expected();
        if replace_payload {
            fixture.replace_payload(&[8, 0, 1, b'x']);
        } else {
            fixture.edit("tags.json", |value| *value = json!([]));
        }
        // Even though the modified bundle's own file hashes are consistent,
        // callers holding the verified preparation fingerprint reject it.
        assert!(matches!(
            ConfigurationSnapshot::load(&fixture.root, &trusted_before_edit, LoadLimits::default()),
            Err(Error::DigestMismatch(path)) if path == "manifest.json"
        ));
    }
}

#[test]
fn rejects_invalid_end_or_trailing_network_nbt_even_when_hash_matches() {
    for bytes in [&[13, 0][..], &[0][..], &[10, 0, 1][..], &[10][..]] {
        let fixture = Fixture::new();
        fixture.replace_payload(bytes);
        assert!(fixture.load().is_err(), "{bytes:?}");
    }
}

#[test]
fn checks_all_registry_names_order_and_entry_ids() {
    let edits: [fn(&mut Value); 5] = [
        |value| {
            value.as_array_mut().unwrap().pop();
        },
        |value| {
            value.as_array_mut().unwrap().swap(0, 1);
        },
        |value| {
            value[0]["entries"][0]["protocol_id"] = json!(1);
        },
        |value| {
            value[0]["entries"] = json!([]);
        },
        |value| {
            value[0]["entries"][0]["id"] = json!("Bad:Identifier");
        },
    ];
    for edit in edits {
        let fixture = Fixture::new();
        fixture.edit("registries.json", edit);
        assert!(fixture.load().is_err());
    }
}

#[test]
fn validates_static_dynamic_tag_domains_and_duplicates() {
    let edits: [fn(&mut Value); 5] = [
        |value| {
            value[0]["tags"][0]["members"] = json!([1]);
        },
        |value| {
            value[1]["tags"][0]["members"] = json!([2]);
        },
        |value| {
            value[1]["tags"][0]["members"] = json!([1, 1]);
        },
        |value| {
            value[1]["id"] = json!("test:unknown_registry");
        },
        |value| {
            value[1]["tags"][1]["id"] = value[1]["tags"][0]["id"].clone();
        },
    ];
    for edit in edits {
        let fixture = Fixture::new();
        fixture.edit("tags.json", edit);
        assert!(fixture.load().is_err());
    }
    let fixture = Fixture::new();
    fixture.edit("static-domains.json", |value| {
        value[0]["entries"][1]["protocol_id"] = json!(3)
    });
    assert!(fixture.load().is_err());
}

#[test]
fn requires_manifest_files_unique_and_confined() {
    for path in [
        "../outside.nbt",
        "entries/../outside.nbt",
        "entries\\00000.nbt",
        "C:/outside.nbt",
    ] {
        let fixture = Fixture::new();
        fixture.edit("manifest.json", |value| {
            value["files"][0]["path"] = json!(path)
        });
        assert!(fixture.load().is_err());
    }
    let fixture = Fixture::new();
    fixture.edit("manifest.json", |value| {
        let duplicate = value["files"][0].clone();
        value["files"].as_array_mut().unwrap().push(duplicate);
    });
    assert!(fixture.load().is_err());
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("features.json")).unwrap();
    assert!(fixture.load().is_err());
}

#[test]
fn enforces_resource_admission_and_known_pack_feature_integrity() {
    let fixture = Fixture::new();
    for limits in [
        LoadLimits {
            total_file_bytes: 1,
            ..LoadLimits::default()
        },
        LoadLimits {
            file_bytes: 1,
            ..LoadLimits::default()
        },
        LoadLimits {
            files: 1,
            ..LoadLimits::default()
        },
        LoadLimits {
            allocation_bytes: 1,
            ..LoadLimits::default()
        },
    ] {
        assert!(matches!(
            ConfigurationSnapshot::load(&fixture.root, &fixture.expected(), limits),
            Err(Error::Limit(_))
        ));
    }
    fixture.edit("registries.json", |value| {
        value[0]["entries"][0]["known_pack"]["version"] = json!("wrong")
    });
    assert!(fixture.load().is_err());
    let fixture = Fixture::new();
    fixture.edit("features.json", |value| {
        *value = json!(["minecraft:vanilla", "minecraft:vanilla"])
    });
    assert!(fixture.load().is_err());
    assert!(parse_sha256(&"A".repeat(64)).is_err());
}

#[test]
#[ignore = "requires the user's local prepared official configuration, never shipped in Git"]
fn loads_actual_local_official_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("Decompile/bootstrap/26.3-pre-2");
    let hash =
        parse_sha256("18d6ad2986227ea55eb18f8ee6929999a4c48c0bbd623c36af3d2f64d3180e4a").unwrap();
    let packs = [PackFingerprint {
        id: "vanilla".into(),
        version: REFERENCE_VERSION.into(),
        sha256: hash,
    }];
    // Each verified preparation records its actual invocation, so its manifest
    // hash need not repeat. Supply the digest printed by that trusted run;
    // the default pins the independently verified original local preparation.
    let manifest_hash = std::env::var("ARROW_CONFIGURATION_MANIFEST_SHA256").unwrap_or_else(|_| {
        "105626403604b8a2500181c9c27bd6abeab093df23d3f65db91d16245dc8f198".into()
    });
    let expected = ExpectedReference {
        expected_manifest_sha256: parse_sha256(&manifest_hash).unwrap(),
        minecraft_version: REFERENCE_VERSION,
        protocol: REFERENCE_PROTOCOL,
        source_jar_sha256: hash,
        source_jar_bytes: 26_649_663,
        selected_packs: &packs,
    };
    let start = std::time::Instant::now();
    let snapshot = ConfigurationSnapshot::load(&root, &expected, LoadLimits::default()).unwrap();
    let first_load = start.elapsed();
    assert_eq!(snapshot.registries().len(), 32);
    assert_eq!(
        snapshot
            .registries()
            .iter()
            .map(|registry| registry.entries().len())
            .sum::<usize>(),
        432
    );
    assert_eq!(snapshot.tags().len(), 15);
    let negotiated = snapshot.negotiate_known_packs(snapshot.known_packs());
    assert!(
        snapshot
            .registries()
            .iter()
            .flat_map(|registry| registry.entries())
            .all(|entry| negotiated.entry_contents(entry).is_none())
    );
    assert!(snapshot.retained_nbt_bytes() > 100_000);
    let warm_start = std::time::Instant::now();
    let warm = ConfigurationSnapshot::load(&root, &expected, LoadLimits::default()).unwrap();
    eprintln!(
        "configuration_snapshot first_load_us={} warm_load_us={} retained_nbt_bytes={} manifest_sha256={}",
        first_load.as_micros(),
        warm_start.elapsed().as_micros(),
        snapshot.retained_nbt_bytes(),
        hex(snapshot.manifest_sha256())
    );
    assert_eq!(snapshot.manifest_sha256(), warm.manifest_sha256());
}
