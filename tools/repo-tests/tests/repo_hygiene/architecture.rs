use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use serde_json::Value;

fn stage_dependencies() -> BTreeMap<String, BTreeSet<String>> {
    let dependencies: BTreeMap<String, Vec<String>> = serde_json::from_str(include_str!(
        "../../../../spec/stage-dependencies.json"
    ))
    .expect("parse stage dependency contract");
    dependencies
        .into_iter()
        .map(|(stage, dependencies)| (stage, dependencies.into_iter().collect()))
        .collect()
}

fn metadata() -> Value {
    let root = super::workspace_root();
    let output = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse cargo metadata JSON")
}

#[test]
fn stage_crates_follow_the_declared_dependency_dag() {
    let metadata = metadata();
    let expected = stage_dependencies();
    let packages = metadata["packages"].as_array().expect("metadata packages");
    let mut actual: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for package in packages {
        let name = package["name"].as_str().expect("package name");
        if !name.starts_with("witchy-") {
            continue;
        }
        let dependencies = package["dependencies"]
            .as_array()
            .expect("package dependencies")
            .iter()
            .filter(|dependency| !dependency["path"].is_null())
            // The stage DAG constrains the LIBRARY build graph. Dev-dependencies
            // never enter it (they exist only for the crate's own test targets;
            // Cargo forbids dev-dep cycles from affecting downstream builds), so
            // a self dev-dependency used to enable a test-only feature for
            // tests/ integration targets is not a stage edge.
            .filter(|dependency| dependency["kind"].is_null())
            .filter_map(|dependency| dependency["name"].as_str())
            .filter(|dependency| dependency.starts_with("witchy-"))
            .map(str::to_owned)
            .collect();
        actual.insert(name.to_owned(), dependencies);
    }

    let expected_stages: BTreeSet<&str> = expected.keys().map(String::as_str).collect();
    let actual_stages: BTreeSet<&str> = actual.keys().map(String::as_str).collect();
    assert_eq!(
        actual_stages, expected_stages,
        "workspace stage crates changed; update spec/architecture-ledger.md and this contract together"
    );

    for (stage, allowed) in &expected {
        let dependencies = &actual[stage];
        let forbidden: Vec<&str> = dependencies
            .iter()
            .map(String::as_str)
            .filter(|dependency| !allowed.contains(*dependency))
            .collect();
        assert!(
            forbidden.is_empty(),
            "{stage} gained forbidden stage dependencies {forbidden:?}; dependency removal is allowed, but a new edge requires an explicit architecture-contract change"
        );
    }
}

/// Compiler-only tools use `witchy-interp` without the native runtime feature.
/// Keep the two C-heavy providers behind that boundary so snapshot preparation
/// and other frontend-only builds cannot compile Wasmtime or AWS-LC.
#[test]
fn runtime_native_providers_are_optional() {
    let metadata = metadata();
    let packages = metadata["packages"].as_array().expect("metadata packages");
    let runtime = packages
        .iter()
        .find(|package| package["name"] == "witchy-runtime")
        .expect("witchy-runtime package");
    let dependencies = runtime["dependencies"].as_array().expect("runtime dependencies");

    for provider in ["aws-lc-rs", "aws-lc-sys", "rustls", "wasmtime"] {
        let dependency = dependencies
            .iter()
            .find(|dependency| dependency["name"] == provider)
            .unwrap_or_else(|| panic!("witchy-runtime dependency {provider}"));
        assert_eq!(
            dependency["optional"], true,
            "{provider} must stay optional so compiler-only builds avoid its native graph"
        );
    }

    let native = runtime["features"]["native"]
        .as_array()
        .expect("witchy-runtime native feature");
    for provider in ["aws-lc-rs", "aws-lc-sys", "rustls", "wasmtime"] {
        let selector = format!("dep:{provider}");
        assert!(
            native.iter().any(|entry| entry.as_str() == Some(selector.as_str())),
            "witchy-runtime native feature must select {provider}"
        );
    }

    let root = packages
        .iter()
        .find(|package| package["name"] == "witchy")
        .expect("witchy root package");
    let root_dependencies = root["dependencies"].as_array().expect("root dependencies");
    for provider in ["aws-lc-rs", "rustls", "wasmtime"] {
        let production_edges: Vec<_> = root_dependencies
            .iter()
            .filter(|dependency| dependency["name"] == provider && dependency["kind"].is_null())
            .collect();
        assert!(
            production_edges
                .iter()
                .all(|dependency| dependency["optional"] == true),
            "root production dependency {provider} must stay optional"
        );
    }
    assert!(
        root["features"].get("census").is_none(),
        "snapshot generation belongs in witchy-snapshot-tools, not the root compiler package"
    );

    let snapshot_tools = packages
        .iter()
        .find(|package| package["name"] == "witchy-snapshot-tools")
        .expect("witchy-snapshot-tools package");
    let snapshot_dependencies: BTreeSet<_> = snapshot_tools["dependencies"]
        .as_array()
        .expect("snapshot tool dependencies")
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .map(|dependency| dependency["name"].as_str().expect("dependency name"))
        .collect();
    assert_eq!(
        snapshot_dependencies,
        BTreeSet::from(["toml", "witchy-interp", "witchy-syntax", "witchy-types"]),
        "snapshot tools must stay on the smallest frontend graph"
    );
}

/// Every `tests/*.rs` file in the root package is a separately linked test
/// binary that nextest must also discover. Discovery is not free: a measured
/// full gate spent 240s before the first test ran -- 119s compiling and ~121s
/// discovering 50 binaries, roughly 2.4s each -- against 554s of actual test
/// execution. That is why the architecture ledger keeps the consolidated
/// driver pattern (`tests/misc.rs` + `tests/misc/`) as a KEEP decision.
///
/// The decision was previously enforced only by review, and drifted: an
/// earlier consolidation reached 43 binaries and the count grew back to 50 as
/// new RFC suites each arrived as their own top-level file. This budget is the
/// enforcement the ledger row lacked.
///
/// Adding a genuinely independent suite is fine -- raise the budget and say
/// why in the commit. What this catches is the unthinking default of one new
/// top-level file per RFC, which costs every gate for every branch.
#[test]
fn root_integration_test_binaries_stay_within_budget() {
    // The explicitly nightly differential-fuzz campaign is a first-class
    // binary, even though it is absent from the blocking profile.
    const BUDGET: usize = 18;

    let root = super::workspace_root().join("tests");
    let mut drivers: Vec<String> = std::fs::read_dir(&root)
        .expect("read tests/")
        .filter_map(|entry| {
            let path = entry.expect("read tests/ entry").path();
            if path.extension().is_some_and(|ext| ext == "rs") {
                path.file_name().map(|n| n.to_string_lossy().into_owned())
            } else {
                None
            }
        })
        .collect();
    drivers.sort();

    assert!(
        drivers.len() <= BUDGET,
        "{} top-level test binaries exceeds the budget of {BUDGET}. Each one costs \
         a link plus ~2.4s of nextest discovery on every gate. Fold the new suite \
         into an existing driver (see tests/misc.rs for the `#[path]` pattern) or \
         raise BUDGET deliberately.\nbinaries: {drivers:#?}",
        drivers.len(),
    );
}

/// Cargo does not fingerprint its own `CARGO_MANIFEST_DIR` value. Compiling it
/// into a test executable therefore binds that executable to one checkout even
/// after the build seed rebases every source dependency onto a new worktree.
/// Runtime lookup keeps linked test artifacts portable; build-seed.py retains
/// a conservative invalidation fallback for any future regression.
#[test]
fn rust_tests_do_not_compile_checkout_specific_manifest_paths() {
    let root = super::workspace_root();
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "-z", "--", "*.rs"])
        .output()
        .expect("list tracked Rust sources");
    assert!(output.status.success(), "git ls-files failed");

    let forbidden = [
        ["env", "!(\"", "CARGO_MANIFEST_DIR", "\")"].concat(),
        ["option_env", "!(\"", "CARGO_MANIFEST_DIR", "\")"].concat(),
    ];
    let mut offenders = Vec::new();
    for relative in output.stdout.split(|byte| *byte == 0).filter(|path| !path.is_empty()) {
        let relative = String::from_utf8_lossy(relative);
        let source = std::fs::read_to_string(root.join(relative.as_ref()))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        if forbidden.iter().any(|needle| source.contains(needle)) {
            offenders.push(relative.into_owned());
        }
    }

    assert!(
        offenders.is_empty(),
        "compile-time CARGO_MANIFEST_DIR makes seeded tests read the publisher checkout; use std::env::var_os at runtime instead: {offenders:#?}",
    );
}
