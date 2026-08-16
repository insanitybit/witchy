//! RFC-0122 migration corpus parity.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};
use witchy_syntax::format::migrate_references;

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
