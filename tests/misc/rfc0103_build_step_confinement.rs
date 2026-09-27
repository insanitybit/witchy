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
        ("fn build(out: BuildOut, input: BuildRead) -> ():\n    ()\n", "no read grant"),
        ("fn build(out: BuildOut, net: Net) -> ():\n    ()\n", "a build step may only take build-time capabilities"),
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

#[test]
fn build_recipe_uses_admitted_original_comptime_output() {
    let dir = TempDir::new("build-admitted-comptime");
    let source = dir.join("build.witchy");
    let out = dir.join("out");
    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        fn generated_text() -> String:\n            \"fn value() -> Int:\\n    42\\n\"\n    )\nfn build(out: BuildOut):\n    out.write_out(\"generated.witchy\", generated_text())\n").unwrap();
    let result = Command::new(BIN).args([
        "build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap(),
    ]).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read_to_string(out.join("generated.witchy")).unwrap(),
        "fn value() -> Int:\n    42\n");

    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        fn build(out: BuildOut):\n            out.write_out(\"forged.witchy\", \"bad\")\n    )\n").unwrap();
    let forged = Command::new(BIN).args([
        "build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap(),
    ]).output().unwrap();
    assert!(!forged.status.success(), "generated syntax cannot select a build declaration");
    assert!(String::from_utf8_lossy(&forged.stderr).contains("exactly one `build` declaration"));
    assert!(!out.join("forged.witchy").exists());
}

#[test]
fn build_recipe_uses_admitted_imported_comptime_output() {
    let dir = TempDir::new("build-imported-admitted-comptime");
    let source = dir.join("build.witchy");
    let producer = dir.join("dep.witchy");
    let out = dir.join("out");
    std::fs::write(&producer, "comptime:\n    emit_item(quote item:\n        pub fn generated_text() -> String:\n            \"fn value() -> Int:\\n    42\\n\"\n    )\n").unwrap();
    std::fs::write(&source, "import dep\nfn build(out: BuildOut):\n    out.write_out(\"generated.witchy\", dep.generated_text())\n").unwrap();
    let run = Command::new(BIN).args([
        "build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap(),
    ]).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(out.join("generated.witchy")).unwrap(),
        "fn value() -> Int:\n    42\n");

    std::fs::write(&producer, "comptime:\n    emit_item(quote item:\n        pub fn generated_text() -> String:\n            42\n    )\n").unwrap();
    let invalid = Command::new(BIN).args([
        "build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap(),
    ]).output().unwrap();
    assert!(!invalid.status.success(), "invalid imported output reached build execution");
    let error = String::from_utf8_lossy(&invalid.stderr);
    assert!(error.contains("String") && error.contains("Int"), "{error}");
}

#[test]
fn production_build_read_uses_ordered_explicit_roots_in_the_confined_child() {
    let dir = TempDir::new("build-read-child");
    let first = dir.mkdir("first");
    let second = dir.mkdir("second");
    std::fs::write(first.join("input.txt"), [0xff]).unwrap();
    let contents = "caf\u{e9}".repeat(100);
    std::fs::write(second.join("input.txt"), &contents).unwrap();
    let source = dir.write("build.witchy", "fn build(input: BuildRead, out: BuildOut):\n    let read = fn(): input.read_build(\"input.txt\")\n    out.write_out(\"result.txt\", read())\n");
    let out = dir.join("out");
    let result = Command::new(BIN).args([
        "build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap(),
        "--read", first.to_str().unwrap(), "--read", second.to_str().unwrap(),
    ]).output().unwrap();
    assert!(result.status.success(), "stdout={} stderr={}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    assert!(String::from_utf8_lossy(&result.stderr).contains("confinement: layer="));
    assert_eq!(std::fs::read_to_string(out.join("result.txt")).unwrap(), contents);
}

#[test]
fn production_build_read_refuses_escape_without_writing_output() {
    let dir = TempDir::new("build-read-escape");
    let root = dir.mkdir("root");
    dir.write("outside.txt", "secret");
    let source = dir.write("build.witchy", "fn build(out: BuildOut, input: BuildRead):\n    out.write_out(\"result.txt\", input.read_build(\"../outside.txt\"))\n");
    let out = dir.join("out");
    let result = Command::new(BIN).args([
        "build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap(), "--read", root.to_str().unwrap(),
    ]).output().unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("read_build") && stderr.contains("escapes"), "{stderr}");
    assert!(!out.join("result.txt").exists());
}
