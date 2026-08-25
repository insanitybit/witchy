use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

const STAGE_DEPENDENCIES: &[(&str, &[&str])] = &[
    ("witchy-cap-model", &[]),
    ("witchy-testkit", &["witchy-cap-model"]),
    ("witchy-test-host", &["witchy-testkit"]),
    ("witchy-syntax", &["witchy-cap-model"]),
    ("witchy-types", &["witchy-cap-model", "witchy-syntax"]),
    ("witchy-wir", &["witchy-syntax"]),
    ("witchy-caps", &["witchy-cap-model", "witchy-syntax"]),
    ("witchy-confinement", &[]),
    ("witchy-lower", &["witchy-syntax", "witchy-types", "witchy-wir"]),
    (
        "witchy-runtime",
        &[
            "witchy-cap-model",
            "witchy-caps",
            "witchy-confinement",
            "witchy-syntax",
            "witchy-test-host",
            "witchy-testkit",
            "witchy-wir",
        ],
    ),
    (
        "witchy-interp",
        &[
            "witchy-caps",
            "witchy-runtime",
            "witchy-syntax",
            "witchy-test-host",
            "witchy-testkit",
            "witchy-types",
        ],
    ),
];

fn metadata() -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
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

    let expected_stages: BTreeSet<&str> = STAGE_DEPENDENCIES.iter().map(|(name, _)| *name).collect();
    let actual_stages: BTreeSet<&str> = actual.keys().map(String::as_str).collect();
    assert_eq!(
        actual_stages, expected_stages,
        "workspace stage crates changed; update spec/architecture-ledger.md and this contract together"
    );

    for (stage, allowed) in STAGE_DEPENDENCIES {
        let allowed: BTreeSet<&str> = allowed.iter().copied().collect();
        let dependencies = &actual[*stage];
        let forbidden: Vec<&str> = dependencies
            .iter()
            .map(String::as_str)
            .filter(|dependency| !allowed.contains(dependency))
            .collect();
        assert!(
            forbidden.is_empty(),
            "{stage} gained forbidden stage dependencies {forbidden:?}; dependency removal is allowed, but a new edge requires an explicit architecture-contract change"
        );
    }
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
    const BUDGET: usize = 24;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
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
