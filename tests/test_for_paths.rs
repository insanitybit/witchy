use std::path::Path;
use std::process::Command;

fn route(paths: &[&str]) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/test-for-paths.sh");
    let output = Command::new("bash")
        .arg(script)
        .args(paths)
        .output()
        .expect("run test-for-paths.sh");
    assert!(
        output.status.success(),
        "test router failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("router output is UTF-8")
}

#[test]
fn rust_integration_tests_route_to_their_binary_and_fast_gate() {
    let output = route(&["tests/differential_fuzz.rs"]);
    assert!(output.contains("./scripts/check.sh --fast"));
    assert!(output.contains("cargo nextest run --test differential_fuzz"));
    assert!(!output.contains("prose-only"));
}

#[test]
fn e2e_integration_test_keeps_its_dedicated_shard() {
    let output = route(&["tests/e2e.rs"]);
    assert!(output.contains("./scripts/check.sh --e2e"));
    assert!(!output.contains("cargo nextest run --test e2e"));
    assert!(!output.contains("./scripts/check.sh --fast"));
}

#[test]
fn router_changes_run_router_regressions() {
    let output = route(&["scripts/test-for-paths.sh"]);
    assert!(output.contains("for f in scripts/*.sh; do bash -n"));
    assert!(output.contains("cargo nextest run --test test_for_paths"));
}
