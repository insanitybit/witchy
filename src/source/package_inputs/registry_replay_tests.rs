use super::*;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

struct Fixture {
    root: PathBuf,
    public: String,
}

impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

fn resign_record(record: serde_json::Value) -> String {
    resign_record_with_key(record, &SigningKey::from_bytes(&[37; 32]))
}

fn resign_record_with_key(mut record: serde_json::Value, key: &SigningKey) -> String {
    let payload = format!("coven-v1\nname={}\nversion={}\nstate={}\nhash={}\nrt=[]\nbuild=\nuploaded_by=\npromoted_by=\nfactor=\nprovenance=\nreleased_at=0",
        record["name"].as_str().unwrap(), record["version"].as_str().unwrap(),
        record["state"].as_str().unwrap(), record["hash"].as_str().unwrap());
    record["sig"] = serde_json::Value::String(hex(&key.sign(payload.as_bytes()).to_bytes()));
    record.to_string()
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("witchy-pinned-vendor-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("vendor/helper/src")).unwrap();
        std::fs::write(root.join("witchy.toml"),
            "[rune]\nname=\"registry-app\"\nversion=\"1.0.0\"\n[dependencies]\nhelper={version=\"1.0.0\"}\n[targets.trusted-publisher]\n").unwrap();
        std::fs::write(root.join("src/main.witchy"),
            "import helper\nfn main(console: Console):\n    let value = helper.message()\n    let report = fn(): console.print(value)\n    report()\n").unwrap();
        let sources = BTreeMap::from([
            ("witchy.toml", "[rune]\nname=\"acme/helper\"\nversion=\"1.0.0\"\n"),
            ("src/helper.witchy", "import extra\npub fn message() -> String:\n    extra.message()\n"),
            ("src/extra.witchy", "pub fn message() -> String:\n    \"signed registry source\"\n"),
        ]);
        // Independent construction of the existing rune wire hash/signature.
        // Production replay invokes gm_core and the existing native crypto,
        // not this fixture's serializer or supplied digest.
        let mut digest = Sha256::new();
        for (path, text) in &sources {
            std::fs::write(root.join("vendor/helper").join(path), text).unwrap();
            let normalized = text.replace("\r\n", "\n");
            digest.update((path.len() as u64).to_le_bytes());
            digest.update(path.as_bytes());
            digest.update((normalized.len() as u64).to_le_bytes());
            digest.update(normalized.as_bytes());
        }
        let hash = format!("sha256:{}", hex(&digest.finalize()));
        let key = SigningKey::from_bytes(&[37; 32]);
        let public = hex(key.verifying_key().as_bytes());
        let payload = format!("coven-v1\nname=acme/helper\nversion=1.0.0\nstate=released\nhash={hash}\nrt=[]\nbuild=\nuploaded_by=\npromoted_by=\nfactor=\nprovenance=\nreleased_at=0");
        let record = serde_json::json!({
            "name": "acme/helper", "version": "1.0.0", "state": "released", "hash": hash,
            "runtime_footprint": [], "released_at": 0, "sig": hex(&key.sign(payload.as_bytes()).to_bytes()),
        });
        std::fs::write(root.join("vendor/helper/coven.json"), record.to_string()).unwrap();
        std::fs::write(root.join("witchy.lock"), format!(
            "registry_snapshot_version=1\nregistry_rootpub=\"{public}\"\n[[rune]]\nname=\"acme/helper\"\nalias=\"helper\"\nversion=\"1.0.0\"\nsource=\"coven\"\nhash=\"{hash}\"\nruntime_footprint=[]\n"
        )).unwrap();
        Self { root, public }
    }

    fn dependencies(&self) -> HashMap<String, PathBuf> {
        HashMap::from([("helper".into(), self.root.join("vendor/helper/src/helper.witchy"))])
    }

    fn capture(&self) -> Result<(pipeline::CheckedModule, String, SourceInputs), String> {
        capture_packaged_source(self.root.join("src/main.witchy").to_str().unwrap(), &self.dependencies(),
            self.root.join("witchy.toml").to_str().unwrap())
    }

    fn replace_signed_body(&self) {
        std::fs::write(self.root.join("vendor/helper/src/extra.witchy"),
            "pub fn message() -> String:\n    \"different signed source\"\n").unwrap();
        let mut digest = Sha256::new();
        for path in ["src/extra.witchy", "src/helper.witchy", "witchy.toml"] {
            let text = std::fs::read_to_string(self.root.join("vendor/helper").join(path)).unwrap().replace("\r\n", "\n");
            digest.update((path.len() as u64).to_le_bytes());
            digest.update(path.as_bytes());
            digest.update((text.len() as u64).to_le_bytes());
            digest.update(text.as_bytes());
        }
        let hash = format!("sha256:{}", hex(&digest.finalize()));
        let path = self.root.join("vendor/helper/coven.json");
        let mut record: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let old = record["hash"].as_str().unwrap().to_owned();
        record["hash"] = serde_json::Value::String(hash.clone());
        std::fs::write(path, resign_record(record)).unwrap();
        let path = self.root.join("witchy.lock");
        std::fs::write(&path, std::fs::read_to_string(&path).unwrap().replace(&old, &hash)).unwrap();
    }
}

fn replace_input(inputs: &mut SourceInputs, suffix: &str, change: impl FnOnce(&str) -> String) {
    let file = inputs.files.iter_mut().find(|file| file.path.ends_with(suffix)).expect("captured raw input");
    file.contents = Some(change(file.contents.as_deref().expect("present input")));
}

#[test]
fn pinned_vendor_package_replays_and_executes_after_original_sources_are_removed() {
    use witchy::trusted_publisher::{self, OwnedEmbeddedApplication};
    let fixture = Fixture::new();
    let (checked, _, inputs) = fixture.capture().expect("capture authenticated vendor");
    let before = checked.canonical_source().unwrap().source_witness().source_bundle_digest();
    assert!(inputs.files.iter().any(|file| file.path.ends_with("/witchy.lock") && file.contents.is_some()));
    assert!(inputs.files.iter().any(|file| file.path.ends_with("/vendor/helper/coven.json") && file.contents.is_some()));
    assert!(inputs.files.iter().any(|file| file.path.ends_with("/vendor/helper/src/extra.witchy") && file.contents.is_some()));
    let expected = vec!["signed registry source".to_owned()];
    assert_eq!(witchy_interp::interpreter::run_checked_module(&checked, ".", Vec::new()).unwrap(), expected);
    let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked).expect_lowered("admitted pinned vendor");
    assert!(!artifact.owner_bridges().is_empty());
    let manifest = std::fs::read_to_string(fixture.root.join("witchy.toml")).unwrap();
    let bindings = trusted_publisher::build_binding_plan(checked.module(), &manifest).unwrap();
    let image = trusted_publisher::package(b"pinned-vendor-test-launcher", artifact.wasm(), &bindings, &inputs).unwrap();
    std::fs::remove_dir_all(&fixture.root).unwrap();
    let embedded = trusted_publisher::probe(&image).unwrap().unwrap();
    let application = OwnedEmbeddedApplication {
        wasm: embedded.wasm.to_vec(), bindings: embedded.bindings.to_vec(),
        source_inputs: serde_json::from_slice(embedded.source_inputs).unwrap(),
    };
    let (rechecked, _) = recheck_packaged_source(&application.source_inputs).unwrap();
    assert_eq!(before, rechecked.canonical_source().unwrap().source_witness().source_bundle_digest());
    let restored = restore_packaged_artifact(&application).expect("replay fresh admission, not captured authority");
    let mut runtime = witchy_runtime::runtime::Runtime::batch().unwrap();
    let caps = witchy_runtime::runtime::Capabilities { print: true, print_int: true, quiet: true,
        ..witchy_runtime::runtime::Capabilities::none() };
    let mut vm = runtime.spawn_admitted(&restored, caps, 1024).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn pinned_vendor_replay_rejects_substituted_record_source_pin_and_coordinate() {
    let fixture = Fixture::new();
    let (_, _, inputs) = fixture.capture().unwrap();
    std::fs::remove_dir_all(&fixture.root).unwrap();
    for case in 0..13 {
        let mut hostile = inputs.clone();
        match case {
            0 => replace_input(&mut hostile, "/vendor/helper/coven.json", |text| {
                let mut record: serde_json::Value = serde_json::from_str(text).unwrap();
                record["sig"] = serde_json::Value::String("00".repeat(64));
                record.to_string()
            }),
            1 => replace_input(&mut hostile, "/vendor/helper/src/extra.witchy", |text| text.replace("signed registry source", "unsigned replacement")),
            2 => replace_input(&mut hostile, "/witchy.lock", |text| text.replace(&fixture.public,
                &hex(SigningKey::from_bytes(&[38; 32]).verifying_key().as_bytes()))),
            3 => replace_input(&mut hostile, "/witchy.lock", |text| text.replace("acme/helper", "attacker/helper")),
            4 => hostile.files.retain(|file| !file.path.ends_with("/vendor/helper/src/extra.witchy")),
            5 => replace_input(&mut hostile, "/witchy.lock", |text| text.replace("registry_snapshot_version=1\n", "")),
            6 => replace_input(&mut hostile, "/witchy.lock", |text| text.replace(&fixture.public, "not-a-public-key")),
            7 => replace_input(&mut hostile, "/witchy.lock", |text| {
                text.lines().filter(|line| !line.starts_with("registry_")).collect::<Vec<_>>().join("\n")
            }),
            8..=10 => replace_input(&mut hostile, "/vendor/helper/coven.json", |text| {
                let mut record: serde_json::Value = serde_json::from_str(text).unwrap();
                let (field, value) = match case {
                    8 => ("name", "other/helper"),
                    9 => ("version", "2.0.0"),
                    _ => ("hash", "sha256:0000000000000000000000000000000000000000000000000000000000000000"),
                };
                record[field] = serde_json::Value::String(value.into());
                resign_record(record)
            }),
            11 => replace_input(&mut hostile, "/witchy.lock", |text| {
                format!("{text}\n[[rune]]{}", text.split_once("[[rune]]").unwrap().1)
            }),
            12 => {
                hostile.dependencies[0].path = hostile.dependencies[0].path
                    .replace("/src/helper.witchy", "/src/../src/helper.witchy");
            }
            _ => unreachable!(),
        }
        let error = recheck_packaged_source(&hostile).expect_err("altered raw resolver inputs must not recover admission");
        assert!(error.contains("registry") || (case == 12 && error.contains("invalid closed package path")), "case {case}: {error}");
    }
}

#[test]
fn pinned_vendor_project_owner_flags_are_assertions_not_registry_authority() {
    let fixture = Fixture::new();
    let entry = ModuleLoadIdentity::new(PackageCoordinate::new(PackageSource::Workspace, "registry-app", "1.0.0").unwrap(), ["src", "main"]).unwrap();
    let owner = |source, name| ModuleLoadIdentity::new(PackageCoordinate::new(source, name, "1.0.0").unwrap(), ["helper"]).unwrap();
    for (claim, success) in [
        (owner(PackageSource::Registry(fixture.public.clone()), "acme/helper"), true),
        (owner(PackageSource::Registry("caller-asserted-root".into()), "acme/helper"), false),
        (owner(PackageSource::Registry(fixture.public.clone()), "attacker/helper"), false),
        (owner(PackageSource::Workspace, "acme/helper"), false),
        (owner(PackageSource::Toolchain, "acme/helper"), false),
    ] {
        let result = capture_project_source(fixture.root.join("src/main.witchy").to_str().unwrap(), &fixture.dependencies(),
            Some(fixture.root.join("witchy.toml").to_str().unwrap()), &entry, &HashMap::from([("helper".into(), claim)]));
        assert_eq!(result.is_ok(), success, "unexpected owner assertion result: {result:?}");
    }
}

#[test]
fn pinned_vendor_unsigned_sibling_cannot_inherit_registry_owner() {
    let fixture = Fixture::new();
    let (_, _, mut inputs) = fixture.capture().unwrap();
    // The signed caller requests extra, but changing the explicit resolver to
    // select an unsigned same-named file must not borrow helper's provenance.
    let file = inputs.files.iter().find(|file| file.path.ends_with("/vendor/helper/src/extra.witchy")).unwrap().clone();
    let mut forged = file;
    forged.path = forged.path.replace("/extra.witchy", "/unsigned.witchy");
    inputs.files.push(forged);
    let error = recheck_packaged_source(&inputs).unwrap_err();
    assert!(error.contains("source hash"), "{error}");
}

#[test]
fn pinned_vendor_valid_source_b_with_wasm_a_rejects_rehashed_envelope_before_execution() {
    use witchy::trusted_publisher::{self, OwnedEmbeddedApplication};
    let a = Fixture::new();
    let b = Fixture::new();
    b.replace_signed_body();
    let (checked_a, _, _) = a.capture().unwrap();
    let (_, _, inputs_b) = b.capture().expect("source B has its own valid signed record and pin");
    let artifact_a = witchy_lower::codegen::compile_checked_module_artifact(&checked_a).expect_lowered("source A admitted");
    let manifest = std::fs::read_to_string(a.root.join("witchy.toml")).unwrap();
    let bindings = trusted_publisher::build_binding_plan(checked_a.module(), &manifest).unwrap();
    // Recompute every public envelope digest, rather than relying on a stale
    // checksum to reject the independent valid-source/valid-Wasm substitution.
    let image = trusted_publisher::package(b"pinned-vendor-substitution-launcher", artifact_a.wasm(), &bindings, &inputs_b).unwrap();
    std::fs::remove_dir_all(&a.root).unwrap();
    std::fs::remove_dir_all(&b.root).unwrap();
    let embedded = trusted_publisher::probe(&image).unwrap().unwrap();
    let application = OwnedEmbeddedApplication {
        wasm: embedded.wasm.to_vec(), bindings: embedded.bindings.to_vec(),
        source_inputs: serde_json::from_slice(embedded.source_inputs).unwrap(),
    };
    let error = match restore_packaged_artifact(&application) {
        Err(error) => error,
        Ok(_) => panic!("valid source B cannot authenticate Wasm A"),
    };
    assert!(error.contains("packaged Wasm differs from freshly admitted source"), "{error}");
}

#[test]
fn pinned_vendor_consistent_key_b_cannot_satisfy_unchanged_registry_a_assertion() {
    let fixture = Fixture::new();
    let key_b = SigningKey::from_bytes(&[38; 32]);
    let public_b = hex(key_b.verifying_key().as_bytes());
    let record_path = fixture.root.join("vendor/helper/coven.json");
    let record: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
    std::fs::write(record_path, resign_record_with_key(record, &key_b)).unwrap();
    let lock_path = fixture.root.join("witchy.lock");
    std::fs::write(&lock_path, std::fs::read_to_string(&lock_path).unwrap().replace(&fixture.public, &public_b)).unwrap();
    let entry = ModuleLoadIdentity::new(PackageCoordinate::new(PackageSource::Workspace, "registry-app", "1.0.0").unwrap(), ["src", "main"]).unwrap();
    for (asserted_root, should_pass) in [(public_b, true), (fixture.public.clone(), false)] {
        let claim = ModuleLoadIdentity::new(PackageCoordinate::new(PackageSource::Registry(asserted_root), "acme/helper", "1.0.0").unwrap(), ["helper"]).unwrap();
        let result = capture_project_source(fixture.root.join("src/main.witchy").to_str().unwrap(), &fixture.dependencies(),
            Some(fixture.root.join("witchy.toml").to_str().unwrap()), &entry, &HashMap::from([("helper".into(), claim)]));
        if should_pass {
            result.expect("consistent key B, pin B and Registry(B) must verify");
        } else {
            let error = result.unwrap_err();
            assert!(error.contains("owner assertion"), "valid B signature must not authenticate Registry(A): {error}");
        }
    }
}

#[test]
fn pinned_vendor_explicit_path_dependency_wins_over_registry_lock_and_directory_name() {
    let fixture = Fixture::new();
    let manifest_path = fixture.root.join("witchy.toml");
    std::fs::write(&manifest_path, std::fs::read_to_string(&manifest_path).unwrap()
        .replace("helper={version=\"1.0.0\"}", "helper={path=\"vendor/helper\"}")).unwrap();
    // The old registry entry still exists, but an explicit path selection is
    // workspace source. A stale signature must neither grant Registry nor
    // prevent this intentionally editable source from loading.
    std::fs::write(fixture.root.join("vendor/helper/coven.json"), "stale registry metadata").unwrap();
    std::fs::write(fixture.root.join("vendor/helper/src/extra.witchy"),
        "pub fn message() -> String:\n    \"workspace path under vendor\"\n").unwrap();
    let entry = ModuleLoadIdentity::new(PackageCoordinate::new(PackageSource::Workspace, "registry-app", "1.0.0").unwrap(), ["src", "main"]).unwrap();
    let claim = ModuleLoadIdentity::new(PackageCoordinate::new(PackageSource::Workspace, "acme/helper", "1.0.0").unwrap(), ["helper"]).unwrap();
    let (checked, _, inputs) = capture_project_source(fixture.root.join("src/main.witchy").to_str().unwrap(), &fixture.dependencies(),
        Some(manifest_path.to_str().unwrap()), &entry, &HashMap::from([("helper".into(), claim)])).expect("manifest path wins");
    assert!(!inputs.files.iter().any(|file| file.path.ends_with("/coven.json") || file.path.ends_with("/witchy.lock")),
        "a path selection must not consult stale registry proofs");
    let digest = checked.canonical_source().unwrap().source_witness().source_bundle_digest();
    std::fs::remove_dir_all(&fixture.root).unwrap();
    let (replayed, _) = recheck_packaged_source(&inputs).unwrap();
    assert_eq!(digest, replayed.canonical_source().unwrap().source_witness().source_bundle_digest());
    let files = ReplayFiles(&inputs);
    let mut verified = registry::Files::new(&files);
    let dependency = &inputs.dependencies[0];
    let owner = verified.module_owner(Path::new(&dependency.path), &dependency.alias, Path::new(&inputs.entry)).unwrap();
    assert_eq!(owner.package().source(), &PackageSource::Workspace);
    assert_eq!(witchy_interp::interpreter::run_checked_module(&replayed, ".", Vec::new()).unwrap(),
        vec!["workspace path under vendor".to_owned()]);
}

#[test]
fn pinned_vendor_offline_replay_uses_locked_version_without_manifest_reresolution() {
    let fixture = Fixture::new();
    let path = fixture.root.join("witchy.toml");
    std::fs::write(&path, std::fs::read_to_string(&path).unwrap()
        .replace("helper={version=\"1.0.0\"}", "helper={version=\"9.0.0\"}")).unwrap();
    let (_, _, inputs) = fixture.capture().expect("offline integrity checks the retained lock, not a fresh resolution");
    std::fs::remove_dir_all(&fixture.root).unwrap();
    recheck_packaged_source(&inputs).expect("closed offline locked-version replay");
}
