use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempRepo(PathBuf);

impl TempRepo {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "witchy-test-for-paths-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(path.join("scripts")).expect("create fixture scripts");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/test-for-paths.sh"),
            path.join("scripts/test-for-paths.sh"),
        )
        .expect("copy path router");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(root).args(args).output().expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

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
fn spec_freshness_script_runs_its_behavior_check() {
    let output = route(&["scripts/check-spec-freshness.sh"]);
    assert!(output.contains("for f in scripts/*.sh; do bash -n"));
    assert!(output.contains("./scripts/check-spec-freshness.sh"));
}

#[test]
fn staged_change_is_discovered_without_explicit_paths() {
    let repo = TempRepo::new();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["symbolic-ref", "HEAD", "refs/heads/master"]);
    git(root, &["config", "user.name", "Witchy Test"]);
    git(root, &["config", "user.email", "witchy-test@example.invalid"]);
    fs::write(root.join("README.md"), "base\n").expect("write fixture base");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base"]);
    fs::write(root.join("README.md"), "staged\n").expect("write staged fixture");
    git(root, &["add", "README.md"]);

    let output = Command::new("bash")
        .current_dir(root)
        .arg("scripts/test-for-paths.sh")
        .output()
        .expect("run staged path router");
    assert!(
        output.status.success(),
        "router: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("router output is UTF-8");
    assert!(stdout.contains("example_tests"), "staged README route: {stdout}");
    assert!(!stdout.contains("no changed files"), "staged README was missed: {stdout}");
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

#[test]
fn glamour_runtime_changes_get_javascript_and_integration_checks() {
    assert_eq!(
        selected(&["web/witchy-runtime/glamour-dom.mjs"]),
        [
            "find web/witchy-runtime -type f -name '*.mjs' -exec node --check {} \\;",
            "cargo nextest run --test glamour_dom",
        ]
    );
}

#[test]
fn shared_browser_host_runs_every_dependent_integration_binary() {
    let commands = selected(&["web/witchy-runtime/witchy-runtime.mjs"]);
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert!(commands[0].contains("node --check"), "{commands:?}");
    assert_eq!(
        commands[1],
        "cargo nextest run --test browser_shim --test browser_encoding --test glamour_dom --test wasm_abi_catalog"
    );
}

#[test]
fn unwrapped_browser_module_still_gets_javascript_syntax_checks() {
    let output = route(&["web/witchy-runtime/hex-strict.test.mjs"]);
    assert!(output.contains("node --check"), "{output}");
    assert!(!output.contains("prose-only"), "{output}");
}
