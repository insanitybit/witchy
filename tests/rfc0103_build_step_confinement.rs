use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

#[path = "support/temp_dir.rs"]
mod temp_dir;
use temp_dir::TempDir;

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
