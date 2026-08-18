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

const LEGACY_AGGREGATE_CARRIER: &str = r#"mode opt

type Pair('a):
    first: View(String, 'a)
    second: View(String, 'a)

fn make_pair(text: String('a)) -> Pair('a):
    let held: String('a) = text
    Pair(held, held)

fn caller(text: String) -> String:
    let pair = make_pair(text).owned()
    pair.first

fn main(console: Console):
    console.print(caller("first"))
"#;

const LEGACY_PARSER_SHELL: &str = r#"mode opt

type Parser('a):
    input: View(String, 'a)
    offset: Int

type TokenIter('a):
    input: View(String, 'a)
    index: Int

type Token('a):
    text: View(String, 'a)
    width: Int

fn parser(input: let('a) String) -> Parser('a):
    Parser(input, 2)

fn tokens(input: let('a) String) -> TokenIter('a):
    TokenIter(input, 3)

fn scan(input: let('a) String) -> Int:
    let p = parser(input)
    let it = tokens(input)
    let values: List(Token('a)) = [Token(input, p.offset), Token(it.input, it.index)]
    var total = 0
    for token in values:
        total = total + token.width
    total

fn caller(input: String) -> Int:
    scan(input)

fn main(console: Console):
    console.print("${caller("source")}")
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
        let expected_root = if label == "shared call" { "text" } else { "value" };
        let stats = witchy::stats::compute(&migration.source)
            .unwrap_or_else(|error| panic!("compute migrated {label} telemetry: {error}"));

        assert_eq!(facts.owner_roots().len(), 1, "migrated {label} root set changed");
        assert_eq!(facts.owner_roots()[0].local, expected_root, "migrated {label} root changed");
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
fn migrated_rfc0083_rfc0112_fixtures_preserve_full_parity() {
    for (
        label,
        legacy,
        expected_roots,
        expected_output,
        expected_live_cells,
        expected_allocations,
    ) in [
        ("shared call", LEGACY_SHARED_CALL, vec!["text"], ["value"], 0, 0),
        ("mutable parameter", LEGACY_MUTABLE_PARAMETER, vec!["value"], ["value"], 0, 0),
        ("parser shell", LEGACY_PARSER_SHELL, vec!["input"], ["5"], 3, 3),
    ] {
        let Some(migration) = migrate_references(legacy) else {
            panic!("migrate historical {label} fixture");
        };
        assert!(migration.ambiguities.is_empty(), "{label}: {:#?}", migration.ambiguities);
        let migrated_checked = witchy::resolve_std_only_checked(&migration.source)
            .unwrap_or_else(|error| panic!("resolve migrated {label} fixture: {error}"));

        let migrated_interpreted =
            interpreter::run_checked_module(&migrated_checked, ".", Vec::new())
                .unwrap_or_else(|error| panic!("interpret migrated {label} fixture: {error}"));
        assert_eq!(migrated_interpreted, expected_output, "{label} interpreter output drifted");
        assert_eq!(compiled_output(&migrated_checked), expected_output, "{label} Wasm output drifted");

        let migrated_facts =
            loans::facts(migrated_checked.module()).expect("publish migrated loan facts");
        assert_eq!(
            migrated_facts
                .owner_roots()
                .iter()
                .map(|root| root.local.as_str())
                .collect::<Vec<_>>(),
            expected_roots,
            "{label} migrated owner roots drifted"
        );
        let migrated_stats = witchy::stats::compute(&migration.source)
            .unwrap_or_else(|error| panic!("measure migrated {label} fixture: {error}"));
        assert!(
            migrated_stats.loan_opens > 0 || migrated_stats.loan_return_transfers > 0,
            "{label} migrated loan evidence missing"
        );
        assert_eq!(migrated_stats.loan_opens, migrated_facts.telemetry().opens);
        assert_eq!(migrated_stats.loan_closes, migrated_facts.telemetry().closes);
        assert_eq!(migrated_stats.output, expected_output, "{label} migrated counters lost output");
        assert_eq!(migrated_stats.loan_opens, migrated_stats.loan_closes, "{label} loan roots unbalanced");
        assert_eq!(migrated_stats.live_cells, expected_live_cells, "{label} aggregate allocation baseline drifted");
        assert_eq!(migrated_stats.reowns, 0, "{label} inserted a materialization reown");
        assert_eq!(migrated_stats.rc_alloc_calls, expected_allocations, "{label} allocation count drifted");
        assert_eq!(migrated_stats.bump_alloc_calls, expected_allocations, "{label} bump allocation count drifted");
        assert_eq!(migrated_stats.rc_reuse_calls, 0, "{label} unexpectedly reused an allocation");
        assert_eq!(migrated_stats.rc_free_calls, 0, "{label} runtime free count drifted");
    }
}

#[test]
fn migrated_aggregate_declaration_preserves_nominal_lifetime() {
    let migration = migrate_references(LEGACY_AGGREGATE_CARRIER)
        .expect("migrate the historical aggregate declaration");
    assert!(migration.ambiguities.is_empty(), "{:#?}", migration.ambiguities);
    assert!(migration.source.contains("type Pair('a):"), "{}", migration.source);
    assert!(migration.source.contains("first: &'a String"), "{}", migration.source);
    assert!(migration.source.contains("second: &'a String"), "{}", migration.source);
    assert!(migration.source.contains("fn make_pair(text: &'a String) -> Pair('a)"), "{}", migration.source);
    assert!(!migration.source.contains("View(String, 'a)"), "{}", migration.source);
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
