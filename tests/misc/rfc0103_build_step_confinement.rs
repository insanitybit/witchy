use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

use super::temp_dir::TempDir;

#[test]
fn production_build_recipe_rejects_traversal_and_other_families() {
    let dir = TempDir::new("build-recipe-negative");
    let source = dir.join("build.witchy");
    let out = dir.join("out");
    for (text, expected) in [
        ("fn build(out: BuildOut):\n    out.write_out(\"../escape.txt\", \"never\")\n", "escapes"),
        ("fn build(out: BuildOut, input: BuildRead) -> Nil:\n    Nil\n", "unsupported"),
    ] {
        std::fs::write(&source, text).unwrap();
        let result = Command::new(BIN).args(["build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap()]).output().unwrap();
        assert!(!result.status.success());
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains(expected), "{stderr}");
        assert!(!dir.join("escape.txt").exists());
    }
}

#[test]
fn production_build_child_rejects_raw_byte_request() {
    use std::io::Write;
    let mut child = Command::new(BIN).arg("@compiler:build-child")
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(br#"{"version":2,"wasm":[0,97,115,109],"grants":{}}"#).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid request"));
}

#[test]
fn production_build_step_runs_in_a_confined_child() {
    let dir = TempDir::new("rfc0103-build-child");
    let source = dir.join("build.witchy");
    let out_dir = dir.join("out");
    std::fs::write(
        &source,
        "fn build(out: BuildOut):\n    out.write_out(\"generated.witchy\", \"fn value() -> Int:\\n    42\\n\")\n",
    )
    .unwrap();

    let output = Command::new(BIN)
        .args([
            "build-step",
            source.to_str().unwrap(),
            "--out",
            out_dir.to_str().unwrap(),
        ])
        .output()
        .expect("spawn witchy build-step");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("confinement: layer="),
        "the child must report its outer-confinement providers: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(out_dir.join("generated.witchy")).unwrap(),
        "fn value() -> Int:\n    42\n"
    );
}
