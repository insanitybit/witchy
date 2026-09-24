//! Project sources must reach the admitted launcher without an external-Wasm hop.
use std::process::{Command, Output};
use super::temp_dir::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
const SOURCE: &str = r#"import helper
fn main(console: Console, args: List(String)) -> Int:
    let text = helper.message()
    let report = fn(): console.print(text)
    report()
    console.print(args[0])
    7
"#;

fn project() -> TempDir {
    let dir = TempDir::new("admitted-project-run");
    dir.write("app/witchy.toml", "[rune]\nname = \"app\"\nversion = \"1\"\n[dependencies]\nhelper = { path = \"../helper\" }\n[targets.trusted-publisher]\nconfine = \"best-effort\"\n");
    dir.write("app/src/app.witchy", SOURCE);
    dir.write("helper/witchy.toml", "[rune]\nname = \"helper\"\nversion = \"1\"\n[dependencies]\n");
    dir.write("helper/src/helper.witchy", "pub fn message() -> String:\n    \"project capture\"\n");
    // The explicit dependency must win over a same-named entry sibling.
    dir.write("app/src/helper.witchy", "pub fn message() -> String:\n    \"wrong sibling\"\n");
    dir
}

fn child(dir: &TempDir, command: &str) -> Command {
    let mut child = Command::new(BIN);
    child.current_dir(dir).args([command, "app/src/app.witchy",
        "--manifest", "app/witchy.toml", "--package-owner", "workspace", "app", "1", "app",
        "--dep", "helper=helper/src/helper.witchy", "--dep-owner", "helper", "workspace", "helper", "1", "helper"]);
    child
}

fn success(result: Output, argument: &str) {
    assert_eq!(result.status.code(), Some(7), "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    assert_eq!(String::from_utf8_lossy(&result.stdout), format!("project capture\n{argument}\n"));
}

#[test]
fn project_source_rejects_forged_owner_claims_and_registry_relabeling() {
    let dir = project();
    for (kind, name, module) in [("toolchain", "app", "app"), ("registry:forged", "app", "app"),
        ("workspace", "other", "app"), ("workspace", "app", "other")] {
        let result = Command::new(BIN).current_dir(&dir)
            .args(["run-source", "app/src/app.witchy", "--manifest", "app/witchy.toml",
                "--package-owner", kind, name, "1", module,
                "--dep", "helper=helper/src/helper.witchy", "--dep-owner", "helper", "workspace", "helper", "1", "helper",
                "--", "argument"]).output().unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains("custom/registry owner assignments") || error.contains("owner assertion"), "{error}");
        assert!(!error.contains("confinement: layer="), "must refuse before effects: {error}");
    }
    dir.write("app/witchy.toml", "[rune]\nname = \"app\"\nversion = \"1\"\n[dependencies]\nhelper = \"1\"\n");
    let result = child(&dir, "run-source").args(["--", "argument"]).output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("workspace relabeling is forbidden"));
}

#[test]
fn ordinary_project_command_executes_owner_capture_with_resolved_dependency() {
    let dir = project();
    let direct = child(&dir, "run-source")
        .args(["--dir", "app", "--", "launch-argument"]).output().unwrap();
    assert!(String::from_utf8_lossy(&direct.stderr).contains("confinement: layer="),
        "the admitted child must report confinement: {}", String::from_utf8_lossy(&direct.stderr));
    // exec.run's documented result is stdout followed by stderr, including
    // the child's real confinement report. Compare the complete capture;
    // neither strip diagnostics nor depend on platform-specific report text.
    let expected_capture = [direct.stdout.as_slice(), direct.stderr.as_slice()].concat();
    success(direct, "launch-argument");
    let result = Command::new(BIN).current_dir(&dir).env("WITCHY_HOME", dir.join("home"))
        .args(["run", "app", "launch-argument"]).output().unwrap();
    assert_eq!(result.status.code(), Some(7), "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    assert_eq!(result.stdout, expected_capture, "project Exec capture must preserve both child streams");
    assert!(!dir.join("app/target/app.wasm").exists());
}
