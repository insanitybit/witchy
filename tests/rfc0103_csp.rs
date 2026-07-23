#[test]
fn emitted_browser_csp_matches_concrete_capability_surfaces() {
    let output = std::process::Command::new("node")
        .arg("web/csp.test.mjs")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run RFC-0103 CSP parity probe");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
