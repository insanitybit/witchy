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
    // These independent CLI compiles use separate roots, so they can overlap
    // without sharing source or generated output.
    let valid_dir = TempDir::new("build-admitted-comptime-valid");
    let valid_source = valid_dir.join("build.witchy");
    let valid_out = valid_dir.join("out");
    std::fs::write(&valid_source, "comptime:\n    emit_item(quote item:\n        fn generated_text() -> String:\n            \"fn value() -> Int:\\n    42\\n\"\n    )\nfn build(out: BuildOut):\n    out.write_out(\"generated.witchy\", generated_text())\n").unwrap();

    let invalid_dir = TempDir::new("build-admitted-comptime-forged");
    let invalid_source = invalid_dir.join("build.witchy");
    let invalid_out = invalid_dir.join("out");
    std::fs::write(&invalid_source, "comptime:\n    emit_item(quote item:\n        fn build(out: BuildOut):\n            out.write_out(\"forged.witchy\", \"bad\")\n    )\n").unwrap();

    let (valid, forged) = std::thread::scope(|scope| {
        let valid = scope.spawn(|| {
            Command::new(BIN).args([
                "build-step", valid_source.to_str().unwrap(), "--out", valid_out.to_str().unwrap(),
            ]).output().unwrap()
        });
        let forged = scope.spawn(|| {
            Command::new(BIN).args([
                "build-step", invalid_source.to_str().unwrap(), "--out", invalid_out.to_str().unwrap(),
            ]).output().unwrap()
        });
        (
            valid.join().expect("valid build recipe process"),
            forged.join().expect("forged build recipe process"),
        )
    });

    assert!(valid.status.success(), "{}", String::from_utf8_lossy(&valid.stderr));
    assert_eq!(std::fs::read_to_string(valid_out.join("generated.witchy")).unwrap(),
        "fn value() -> Int:\n    42\n");
    assert!(!forged.status.success(), "generated syntax cannot select a build declaration");
    assert!(String::from_utf8_lossy(&forged.stderr).contains("exactly one `build` declaration"));
    assert!(!invalid_out.join("forged.witchy").exists());
}

#[test]
fn build_recipe_uses_admitted_record_show_derive() {
    let dir = TempDir::new("build-admitted-record-show");
    let source = dir.join("build.witchy");
    let out = dir.join("out");
    std::fs::write(&source,
        "type Foo derive(Show):\n    value: Int\n\nfn build(out: BuildOut):\n    out.write_out(\"generated.witchy\", show(Foo(value: 7)))\n",
    ).unwrap();
    let result = Command::new(BIN).args([
        "build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap(),
    ]).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read_to_string(out.join("generated.witchy")).unwrap(), "Foo(7)");
}

#[test]
fn build_recipe_uses_admitted_imported_comptime_output() {
    // These CLI compiles are independent; separate roots let them run in
    // parallel without sharing generated files.
    let valid_dir = TempDir::new("build-imported-admitted-comptime");
    let valid_source = valid_dir.join("build.witchy");
    let valid_producer = valid_dir.join("dep.witchy");
    let valid_relay = valid_dir.join("relay.witchy");
    let valid_out = valid_dir.join("out");
    std::fs::write(
        &valid_producer,
        "comptime:\n    emit_item(quote item:\n        pub fn generated_text() -> String:\n            \"fn value() -> Int:\\n    42\\n\"\n    )\n",
    )
    .unwrap();
    std::fs::write(
        &valid_relay,
        "import dep\ncomptime:\n    if dep.generated_text().length() > 0:\n        emit_item(quote item:\n            pub fn generated_text() -> String:\n                dep.generated_text()\n        )\n",
    )
    .unwrap();
    std::fs::write(
        &valid_source,
        "import relay\nfn build(out: BuildOut):\n    out.write_out(\"generated.witchy\", relay.generated_text())\n",
    )
    .unwrap();

    let invalid_dir = TempDir::new("build-imported-invalid-comptime");
    let invalid_source = invalid_dir.join("build.witchy");
    let invalid_producer = invalid_dir.join("dep.witchy");
    let invalid_out = invalid_dir.join("out");
    std::fs::write(
        &invalid_producer,
        "comptime:\n    emit_item(quote item:\n        pub fn generated_text() -> String:\n            42\n    )\n",
    )
    .unwrap();
    std::fs::write(
        &invalid_source,
        "import dep\nfn build(out: BuildOut):\n    out.write_out(\"generated.witchy\", dep.generated_text())\n",
    )
    .unwrap();

    let (valid, invalid) = std::thread::scope(|scope| {
        let valid = scope.spawn(|| {
            Command::new(BIN)
                .args([
                    "build-step",
                    valid_source.to_str().unwrap(),
                    "--out",
                    valid_out.to_str().unwrap(),
                ])
                .output()
                .unwrap()
        });
        let invalid = scope.spawn(|| {
            Command::new(BIN)
                .args([
                    "build-step",
                    invalid_source.to_str().unwrap(),
                    "--out",
                    invalid_out.to_str().unwrap(),
                ])
                .output()
                .unwrap()
        });
        (
            valid.join().expect("valid build recipe process"),
            invalid.join().expect("invalid build recipe process"),
        )
    });

    assert!(valid.status.success(), "{}", String::from_utf8_lossy(&valid.stderr));
    assert_eq!(
        std::fs::read_to_string(valid_out.join("generated.witchy")).unwrap(),
        "fn value() -> Int:\n    42\n"
    );
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

/// `BuildEnv` reads the real process environment through its allow-list:
/// present, empty and unset keys keep their distinct answers, and a key
/// outside the list is refused without disclosing its value.
#[test]
fn build_env_preserves_present_empty_unset_and_refuses_unlisted_keys() {
    let dir = TempDir::new("build-env-states");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let source = dir.join("build.witchy");
    std::fs::write(&source, "fn build(env: BuildEnv, out: BuildOut):\n    let present = env.get_build_env(\"WITCHY_BE_PRESENT\") ?? \"unset\"\n    let empty = env.get_build_env(\"WITCHY_BE_EMPTY\") ?? \"unset\"\n    let unset = env.get_build_env(\"WITCHY_BE_UNSET\") ?? \"unset\"\n    out.write_out(\"env.txt\", present + \"|\" + empty + \"|\" + unset)\n").unwrap();
    let granted = Command::new(BIN)
        .args(["build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap(),
            "--env", "WITCHY_BE_PRESENT", "--env", "WITCHY_BE_EMPTY", "--env", "WITCHY_BE_UNSET"])
        .env("WITCHY_BE_PRESENT", "caf\u{e9}").env("WITCHY_BE_EMPTY", "").env_remove("WITCHY_BE_UNSET")
        .output().unwrap();
    assert!(granted.status.success(), "{}", String::from_utf8_lossy(&granted.stderr));
    assert_eq!(std::fs::read_to_string(out.join("env.txt")).unwrap(), "caf\u{e9}||unset");

    std::fs::write(&source, "fn build(env: BuildEnv, out: BuildOut):\n    out.write_out(\"denied.txt\", env.get_build_env(\"WITCHY_BE_SECRET\") ?? \"unset\")\n").unwrap();
    let denied = Command::new(BIN)
        .args(["build-step", source.to_str().unwrap(), "--out", out.to_str().unwrap(), "--env", "WITCHY_BE_PRESENT"])
        .env("WITCHY_BE_SECRET", "ungranted-environment-secret")
        .output().unwrap();
    let trace = format!("{}{}", String::from_utf8_lossy(&denied.stdout), String::from_utf8_lossy(&denied.stderr));
    assert!(!denied.status.success(), "{trace}");
    assert!(trace.contains("`WITCHY_BE_SECRET` is not in this BuildEnv grant's allow-list"), "{trace}");
    assert!(!trace.contains("ungranted-environment-secret"), "{trace}");
    assert!(!out.join("denied.txt").exists());
}
