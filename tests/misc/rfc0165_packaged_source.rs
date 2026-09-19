//! Actual native package launch, never the external-Wasm runner.
use std::process::{Command, Output};
use super::temp_dir::TempDir;
use witchy::trusted_publisher::{self, source_inputs::SourceInputs};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
const SOURCE: &str = r#"import list
comptime:
    emit_item(quote item:
        fn package_message() -> String:
            "packaged capture"
    )
fn make(console: Console, text: String) -> fn() -> ():
    fn(): console.print(text)
fn main(console: Console, args: List(String)) -> Int:
    let report = make(console, package_message())
    report()
    console.print(args[0])
    7
"#;
const MANIFEST: &str = "[rune]\nname = \"package-owner-probe\"\nversion = \"0.1.0\"\n[dependencies]\n[targets.trusted-publisher]\nconfine = \"best-effort\"\n";

fn outcome(result: &Output, code: i32, stdout: &str) {
    assert_eq!(result.status.code(), Some(code), "stdout: {}\nstderr: {}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    assert_eq!(String::from_utf8_lossy(&result.stdout), stdout);
}

#[test]
fn packaged_owner_rechecks_closed_source_and_rejects_substitution_before_effects() {
    let folder = TempDir::new("packaged-owner-source");
    let root = folder.join("project");
    let elsewhere = folder.join("elsewhere");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let source = root.join("src/main.witchy");
    let manifest = root.join("witchy.toml");
    std::fs::write(&source, SOURCE).unwrap();
    std::fs::write(&manifest, MANIFEST).unwrap();
    let expected = "packaged capture\nlaunch-argument\n";
    let interpreted = Command::new(BIN).arg(&source).arg("launch-argument").output().unwrap();
    outcome(&interpreted, 7, expected);

    let application = folder.join("app");
    let compiled = Command::new(BIN).arg("compile").arg(&source)
        .args(["--target", "trusted-publisher", "--manifest"]).arg(&manifest)
        .arg("--out").arg(&application).output().unwrap();
    assert!(compiled.status.success(), "{}", String::from_utf8_lossy(&compiled.stderr));
    let image = std::fs::read(&application).unwrap();
    let embedded = trusted_publisher::probe(&image).unwrap().unwrap();
    assert!(embedded.wasm.windows(b"witchy.private.owner".len()).any(|bytes| bytes == b"witchy.private.owner"), "fixture must require sealed owner bridges");
    let original_inputs: SourceInputs = serde_json::from_slice(embedded.source_inputs).unwrap();
    let template = std::fs::read(BIN).unwrap();
    let permissions = std::fs::metadata(&application).unwrap().permissions();

    // All future source loading must come from the closed package. A conflicting
    // launch-directory manifest must not affect identity or source resolution.
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::write(elsewhere.join("witchy.toml"), "[rune]\nname = \"unrelated\"\nversion = \"99\"\n").unwrap();
    let executed = Command::new(&application).current_dir(&elsewhere).arg("launch-argument")
        .env("WITCHY_OPT", "invalid-must-not-be-read")
        .env("WITCHY_RAND_SEED", "9123").output().unwrap();
    // The production packaged runner requires Some(0) from the guest heap audit.
    outcome(&executed, 7, expected);

    for attack in ["source", "missing-lookup", "bundled-override", "bindings"] {
        let mut inputs = original_inputs.clone();
        match attack {
            "source" => {
                let entry = inputs.entry.clone();
                let file = inputs.files.iter_mut().find(|file| file.path == entry).unwrap();
                file.contents = Some(SOURCE.replace("packaged capture", "substituted source"));
            }
            "missing-lookup" => {
                let before = inputs.files.len();
                inputs.files.retain(|file| !file.path.ends_with("src/witchy.toml"));
                assert!(inputs.files.len() < before);
            }
            "bundled-override" => {
                let file = inputs.files.iter_mut().find(|file| file.path.ends_with("src/list.witchy")).unwrap();
                assert!(file.contents.is_none());
                file.contents = Some("pub fn forged() -> Int:\n    9\n".into());
            }
            "bindings" => {},
            _ => unreachable!(),
        }
        let bindings = if attack == "bindings" {
            let mut value: serde_json::Value = serde_json::from_slice(embedded.bindings).unwrap();
            value["confinement_required"] = serde_json::Value::Bool(true);
            serde_json::to_vec(&value).unwrap()
        } else { embedded.bindings.to_vec() };
        // The real envelope encoder recomputes EVERY descriptor/content hash.
        let forged = trusted_publisher::package(&template, embedded.wasm, &bindings, &inputs).unwrap();
        let executable = folder.join(format!("forged-{attack}"));
        std::fs::write(&executable, forged).unwrap();
        std::fs::set_permissions(&executable, permissions.clone()).unwrap();
        let result = Command::new(executable).current_dir(&elsewhere).arg("launch-argument").output().unwrap();
        outcome(&result, 1, "");
        let error = String::from_utf8_lossy(&result.stderr);
        let expected = match attack {
            "source" => "packaged Wasm differs from freshly admitted source",
            "missing-lookup" => "omitted loader lookup",
            "bundled-override" => "cannot replace bundled module",
            "bindings" => "binding plan differs from freshly checked source",
            _ => unreachable!(),
        };
        assert!(error.contains(expected), "{attack}: {error}");
        assert!(!error.contains("confinement: layer="), "must reject before confinement/guest startup: {error}");
    }
}

#[test]
fn packaged_source_rejects_custom_owner_and_unsafe_input_paths() {
    let folder = TempDir::new("packaged-owner-refusals");
    let source = folder.join("main.witchy");
    let manifest = folder.join("witchy.toml");
    std::fs::write(&source, SOURCE).unwrap();
    std::fs::write(&manifest, MANIFEST).unwrap();
    let result = Command::new(BIN).arg("compile").arg(source)
        .args(["--target", "trusted-publisher", "--manifest"]).arg(manifest)
        .arg("--out").arg(folder.join("app"))
        .args(["--package-owner", "registry:unverified", "some/package", "1", "src.main"])
        .output().unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains("project owner assertion")
        && error.contains("differs from captured source/manifest ownership"), "{error}");
    assert!(!folder.join("app").exists(), "a forged owner must not produce an artifact");
    for path in ["/absolute", "../escape", "a/../b", "a//b", "a\\b", "a\0b", "C:drive"] {
        assert!(trusted_publisher::source_inputs::validate_path(path).is_err(), "{path:?}");
    }
}
