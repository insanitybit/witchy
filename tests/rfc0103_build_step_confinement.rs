use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn workdir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-rfc0103-build-child-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn production_build_step_runs_in_a_confined_child() {
    let dir = workdir();
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
    let _ = std::fs::remove_dir_all(dir);
}
