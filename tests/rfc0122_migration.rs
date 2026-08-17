//! RFC-0122 migration corpus parity.

use std::process::Command;

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};
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
