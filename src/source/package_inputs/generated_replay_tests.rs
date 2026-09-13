//! Closed replay preserves loader-derived generated namespaces, not build receipts.
use super::*;

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "witchy-generated-replay-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        path
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn closed_generated_path_dependency_replay_preserves_owner_and_rejects_forged_claim() {
    let folder = Folder::new();
    let consumer_source = "[rune]\nname = \"consumer\"\nversion = \"1\"\n\
        [dependencies]\ngenlib = { path = \"../producer\" }\n\
        [build.grants.genlib]\n";
    let producer_source = "[rune]\nname = \"actual-producer\"\nversion = \"2\"\n";
    let build_source = "fn build(out: BuildOut):\n    out.write_out(\"genmod.witchy\", \"pub fn answer() -> Int:\\n    42\\n\")\n";
    let generated_source = "pub fn answer() -> Int:\n    42\n";
    let manifest = folder.write("app/witchy.toml", consumer_source);
    let producer_manifest = folder.write("producer/witchy.toml", producer_source);
    let producer_build = folder.write("producer/src/build.witchy", build_source);
    let entry = folder.write("app/src/main.witchy",
        "import genmod\nfn main() -> Int:\n    genmod.answer()\n");
    // This fixture tests source capture, not whether a build actually produced
    // these bytes. The executable project-build test covers that separate claim.
    let output = folder.write("app/build-out/genlib/genmod.witchy", generated_source);
    let ordinary_producer_module = folder.write("producer/src/genmod.witchy", generated_source);
    folder.write("impostor/witchy.toml",
        "[rune]\nname = \"different-producer\"\nversion = \"2\"\n");
    let impostor_module = folder.write("impostor/src/genmod.witchy", generated_source);

    // Caller claims come from ordinary loader APIs, not authority constructors.
    // Matching module paths isolate the negative to its wrong producer package.
    let entry_claim = workspace_module_owner(&entry, "main").unwrap();
    let producer_claim = workspace_module_owner(&ordinary_producer_module, "genmod").unwrap();
    let forged_claim = workspace_module_owner(&impostor_module, "genmod").unwrap();
    assert_eq!(producer_claim.module_path(), forged_claim.module_path());
    assert_ne!(producer_claim.package(), forged_claim.package());
    let dependencies = HashMap::from([("genmod".to_owned(), output.clone())]);
    let claims = HashMap::from([("genmod".to_owned(), producer_claim)]);
    let (checked, stem, inputs) = capture_project_source(
        entry.to_str().unwrap(), &dependencies, Some(manifest.to_str().unwrap()),
        &entry_claim, &claims,
    ).expect("real path dependency supplies generated namespace ownership");
    assert_eq!(stem, "main");
    let captured = replay_loaded_inputs(&inputs).expect("captured loader graph");
    let digest = checked.canonical_source().unwrap().source_witness().source_bundle_digest();

    for (path, expected) in [
        (&manifest, consumer_source),
        (&producer_manifest, producer_source),
        (&producer_build, build_source),
        (&output, generated_source),
    ] {
        let coordinate = key(path).unwrap();
        let observed = inputs.files.iter().find(|file| file.path == coordinate)
            .expect("closed input retains actual source and producer provenance");
        assert_eq!(observed.contents.as_deref(), Some(expected));
    }
    let output_manifest = output.parent().unwrap().join("witchy.toml");
    let output_manifest_key = key(&output_manifest).unwrap();
    for path in [&output_manifest, &folder.0.join("app/build-out/witchy.toml")] {
        let coordinate = key(path).unwrap();
        let observed = inputs.files.iter().find(|file| file.path == coordinate)
            .expect("output-side manifest absence is a captured loader input");
        assert!(observed.contents.is_none());
    }

    let forged = HashMap::from([("genmod".to_owned(), forged_claim)]);
    let error = capture_project_source(
        entry.to_str().unwrap(), &dependencies, Some(manifest.to_str().unwrap()),
        &entry_claim, &forged,
    ).unwrap_err();
    assert!(error.contains("project owner assertion for `genmod` differs"), "{error}");

    std::fs::remove_dir_all(&folder.0).unwrap();
    assert!(!folder.0.exists(), "replay must not depend on original files");
    let replayed_graph = replay_loaded_inputs(&inputs).expect("closed ownership replay after removal");
    // Includes exact module bytes, entry, user-module set and authenticated owners.
    assert_eq!(captured, replayed_graph);
    let (replayed, replayed_stem) = recheck_packaged_source(&inputs)
        .expect("fresh source checking from closed inputs after removal");
    assert_eq!(stem, replayed_stem);
    assert_eq!(digest, replayed.canonical_source().unwrap().source_witness().source_bundle_digest());

    // Observed absence and producer build source are required replay inputs,
    // not decorative metadata that may be dropped or recovered from disk.
    for missing in [output_manifest_key, key(&producer_build).unwrap()] {
        let mut incomplete = inputs.clone();
        incomplete.files.retain(|file| file.path != missing);
        let error = replay_loaded_inputs(&incomplete).unwrap_err();
        assert!(error.contains("closed package omitted loader lookup"), "{error}");
    }
}
