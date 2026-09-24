//! RFC-0167 row M: generated match arms keep compiler-owned AST nodes.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn compiler_owned_match_arms_build_admitted_wasm_without_source_reparse() {
    // The scrutinee is 1, so the first arm returns 42 rather than the wildcard 0.
    let source = r#"
import meta

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let one = meta.match_arm(meta.pattern_int(1), meta.expr_int(42))
    let fallback = meta.match_arm(meta.pattern_wildcard(), meta.expr_int(0))
    let choice = meta.expr_match(meta.expr_int(1), [one, fallback])
    let body = meta.block([], Some(choice))
    emit_item(meta.function_block(true, meta.ident("generated"), [], Some(int), body))

fn main() -> Int:
    generated()
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("compiler-owned match arms type-check");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("compiler-owned match arms admit SafeWIR");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    actor.run().expect("admitted unoptimized Wasm execution");
    assert_eq!(actor.output(), vec!["42".to_string()]);
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
