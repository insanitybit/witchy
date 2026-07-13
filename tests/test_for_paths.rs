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

fn selected(paths: &[&str]) -> Vec<String> {
    route(paths)
        .lines()
        .filter_map(|line| line.strip_prefix("  ").map(str::to_owned))
        .collect()
}

#[test]
fn rust_integration_tests_use_fast_without_redundant_binary_run() {
    let output = route(&["tests/differential_fuzz.rs"]);
    assert!(output.contains("./scripts/check.sh --fast"));
    assert!(!output.contains("cargo nextest run --test differential_fuzz"));
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

#[test]
fn rust_path_drops_nextest_checks_covered_by_fast() {
    assert_eq!(
        selected(&["crates/witchy-interp/src/comptime.rs"]),
        ["./scripts/check.sh --fast"]
    );
}

#[test]
fn lower_path_keeps_wasm_but_drops_covered_nextest_checks() {
    assert_eq!(
        selected(&["crates/witchy-lower/src/lib.rs"]),
        ["./scripts/check.sh --fast", "./scripts/check.sh --wasm"]
    );
}

#[test]
fn std_path_without_rust_keeps_its_focused_checks() {
    let commands = selected(&["std/list.witchy"]);
    assert_eq!(commands.len(), 3, "{commands:?}");
    assert!(commands[0].contains("example_tests"), "{commands:?}");
    assert!(commands[1].contains("stdlib_docs_are_current"), "{commands:?}");
    assert!(commands[2].contains("witchy fmt --check"), "{commands:?}");
}

#[test]
fn mixed_rust_and_std_keeps_only_fast_and_std_formatting() {
    let commands = selected(&["crates/witchy-syntax/src/parser.rs", "std/meta.witchy"]);
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert_eq!(commands[0], "./scripts/check.sh --fast");
    assert!(commands[1].contains("witchy fmt --check"), "{commands:?}");
}
