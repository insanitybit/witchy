//! RFC-0122 migration corpus parity.

use std::process::Command;

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};
use witchy_types::loans;
use witchy_syntax::format::migrate_references;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

#[path = "support/temp_dir.rs"]
mod temp_dir;
use temp_dir::TempDir;

const LEGACY_SHARED_CALL: &str = r#"mode opt

import borrow

fn first(text: String('a)) -> String('a):
    text

fn caller(text: String) -> String:
    first(text).owned()

fn main(console: Console):
    console.print(caller("value"))
"#;

const LEGACY_MUTABLE_PARAMETER: &str = r#"mode opt

import borrow

fn normalize(var('a) value: String) -> &'a mut String:
    value

fn caller(var value: String) -> String:
    normalize(value).owned()

fn main(console: Console):
    var value = "value"
    console.print(caller(value))
"#;

fn compiled_output(checked: &witchy_types::pipeline::CheckedModule) -> Vec<String> {
    let bytes = codegen::compile_checked_module_binary(checked)
        .expect_lowered("compile migrated RFC-0122 fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &bytes,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn migrated RFC-0122 fixture");
    actor.run().expect("run migrated RFC-0122 fixture");
    actor.output()
}

#[test]
fn migrated_direct_shared_call_preserves_interpreter_wasm_behavior() {
    let migration = migrate_references(LEGACY_SHARED_CALL).expect("migrate legacy shared-call fixture");
    assert!(migration.ambiguities.is_empty(), "{:#?}", migration.ambiguities);
    assert!(migration.source.contains("first(&text)"), "{}", migration.source);
    assert!(migration.source.contains("text: &'a String) -> &'a String"), "{}", migration.source);
    assert!(!migration.source.contains("String('a)"), "{}", migration.source);

    let checked = witchy::resolve_std_only_checked(&migration.source)
        .expect("migrated fixture must resolve and type-check");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret migrated RFC-0122 fixture");
    assert_eq!(interpreted, ["value"]);
    assert_eq!(compiled_output(&checked), interpreted, "migrated reference semantics must agree on both backends");
}

#[test]
fn migrated_mutable_parameter_preserves_interpreter_wasm_behavior() {
    let migration = migrate_references(LEGACY_MUTABLE_PARAMETER)
        .expect("migrate legacy mutable-parameter fixture");
    assert!(migration.ambiguities.is_empty(), "{:#?}", migration.ambiguities);
    assert!(
        migration.source.contains("normalize(&mut value)"),
        "{}",
        migration.source
    );
    assert!(
        migration.source.contains("fn normalize(value: &'a mut String) -> &'a mut String"),
        "{}",
        migration.source
    );
    assert!(!migration.source.contains("var('a)"), "{}", migration.source);

    let checked = witchy::resolve_std_only_checked(&migration.source)
        .expect("migrated mutable fixture must resolve and type-check");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret migrated mutable RFC-0122 fixture");
    assert_eq!(interpreted, ["value"]);
    assert_eq!(
        compiled_output(&checked),
        interpreted,
        "migrated mutable reference semantics must agree on both backends"
    );
}

#[test]
fn migrated_fixtures_preserve_checked_loan_and_runtime_counters() {
    for (label, legacy) in [
        ("shared call", LEGACY_SHARED_CALL),
        ("mutable parameter", LEGACY_MUTABLE_PARAMETER),
    ] {
        let migration = migrate_references(legacy).expect("migrate the parity fixture");
        let checked = witchy::resolve_std_only_checked(&migration.source)
            .expect("migrated parity fixture must resolve and type-check");
        let facts = loans::facts(checked.module()).expect("publish migrated loan facts");
        let telemetry = facts.telemetry();
        let stats = witchy::stats::compute(&migration.source)
            .unwrap_or_else(|error| panic!("compute migrated {label} telemetry: {error}"));

        assert!(
            telemetry.opens > 0 || telemetry.return_transfers > 0,
            "migrated {label} fixture must publish an owner relation"
        );
        assert_eq!(stats.loan_opens, telemetry.opens, "migrated {label} loan opens drifted");
        assert_eq!(stats.loan_closes, telemetry.closes, "migrated {label} loan closes drifted");
        assert_eq!(
            stats.loan_return_transfers,
            telemetry.return_transfers,
            "migrated {label} return transfers drifted"
        );
        assert_eq!(
            stats.loan_shell_mutations,
            telemetry.shell_mutations,
            "migrated {label} shell mutations drifted"
        );
        assert_eq!(
            stats.loan_control_flow_edges,
            telemetry.control_flow_edges,
            "migrated {label} control-flow facts drifted"
        );
        assert_eq!(
            stats.loan_subset_edges,
            telemetry.subset_edges,
            "migrated {label} subset facts drifted"
        );
        assert_eq!(stats.live_cells, 0, "migrated {label} fixture must not leak runtime roots");
        assert_eq!(stats.output, ["value"], "migrated {label} output drifted");
    }
}

#[test]
fn migration_command_rewrites_then_checks_without_mutating_ambiguous_sources() {
    let temp = TempDir::new("rfc0122-migration-command");
    let legacy = temp.write("legacy.witchy", LEGACY_SHARED_CALL);
    let before = std::fs::read_to_string(&legacy).expect("read legacy fixture");

    let checked = Command::new(BIN)
        .args(["migrate", "references", "--check", legacy.to_str().unwrap()])
        .output()
        .expect("run migration check");
    assert!(!checked.status.success(), "legacy source must fail --check");
    assert!(
        String::from_utf8_lossy(&checked.stderr).contains("legacy reference syntax remains"),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&legacy).expect("read unchanged legacy fixture"),
        before,
        "--check must not mutate a legacy source"
    );

    let rewritten = Command::new(BIN)
        .args(["migrate", "references", legacy.to_str().unwrap()])
        .output()
        .expect("run migration rewrite");
    assert!(
        rewritten.status.success(),
        "migration rewrite failed: {}{}",
        String::from_utf8_lossy(&rewritten.stdout),
        String::from_utf8_lossy(&rewritten.stderr)
    );
    let current = std::fs::read_to_string(&legacy).expect("read rewritten fixture");
    assert!(current.contains("first(&text)"), "{current}");
    assert!(current.contains("text: &'a String) -> &'a String"), "{current}");

    let clean = Command::new(BIN)
        .args(["migrate", "references", "--check", legacy.to_str().unwrap()])
        .output()
        .expect("run clean migration check");
    assert!(
        clean.status.success(),
        "rewritten source should pass --check: {}{}",
        String::from_utf8_lossy(&clean.stdout),
        String::from_utf8_lossy(&clean.stderr)
    );
    assert!(String::from_utf8_lossy(&clean.stdout).contains("clean"));

    let ambiguous = temp.write(
        "ambiguous.witchy",
        "mode opt\n\nfn first(text: String('a)) -> String('a):\n    text\n\nfn caller() -> String:\n    first(\"value\")\n",
    );
    let ambiguous_before = std::fs::read_to_string(&ambiguous).expect("read ambiguous fixture");
    let rejected = Command::new(BIN)
        .args(["migrate", "references", ambiguous.to_str().unwrap()])
        .output()
        .expect("run ambiguous migration");
    assert!(!rejected.status.success(), "ambiguous source must be rejected");
    let diagnostic = String::from_utf8_lossy(&rejected.stderr);
    assert!(diagnostic.contains("argument 1"), "{diagnostic}");
    assert!(
        diagnostic.contains("needs an explicit shared borrow")
            && diagnostic.contains("unresolved"),
        "{diagnostic}"
    );
    assert_eq!(
        std::fs::read_to_string(&ambiguous).expect("read unchanged ambiguous fixture"),
        ambiguous_before,
        "ambiguous migration must not write a guessed borrow"
    );
}
