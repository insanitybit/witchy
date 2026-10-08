//! A capture-free lambda evaluated in a loop builds its closure record once,
//! in the entry block, not on every iteration.

use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::Operation;

fn no_expand(
    _name: &str,
    _module: &mut witchy_syntax::ast::Module,
    _siblings: &[(String, witchy_syntax::ast::Module)],
) -> Result<witchy_syntax::origin::OriginTable, String> {
    Ok(witchy_syntax::origin::OriginTable::default())
}

fn checked(source: &str) -> CheckedModule {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".to_string(), source.as_bytes().to_vec())],
        "main",
        witchy_syntax::linker::ComptimeExpander::new(no_expand),
    )
    .expect("checked source")
}

/// Closure code values materialized outside their function's entry block.
fn non_entry_function_values(source: &str) -> usize {
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked(source));
    lowered.functions().iter()
        .map(|function| function.blocks.iter()
            .filter(|block| block.id != function.entry)
            .flat_map(|block| &block.operations)
            .filter(|operation| matches!(operation, Operation::FunctionValue { .. }))
            .count())
        .sum()
}

#[test]
fn a_capture_free_lambda_in_a_loop_is_built_once() {
    let in_loop = non_entry_function_values("fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\n\
fn main() -> Int:\n    var total = 0\n    var i = 0\n    while i < 10:\n        total = total + apply(fn(c: Int) -> Int: c + 1, i)\n        i = i + 1\n    total\n");
    let hoisted = non_entry_function_values("fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\n\
fn main() -> Int:\n    let f = fn(c: Int) -> Int: c + 1\n    var total = 0\n    var i = 0\n    while i < 10:\n        total = total + apply(f, i)\n        i = i + 1\n    total\n");
    assert_eq!(in_loop, hoisted, "the loop's lambda allocates per iteration");
}

#[test]
fn a_lambda_on_a_cold_branch_is_built_only_there() {
    let branch = non_entry_function_values("fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\n\
fn pick(x: Int) -> Int:\n    if x > 100:\n        apply(fn(c: Int) -> Int: c + 1, x)\n    else:\n        x\n\n\
fn main() -> Int:\n    pick(3)\n");
    let entry = non_entry_function_values("fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\n\
fn pick(x: Int) -> Int:\n    let f = fn(c: Int) -> Int: c + 1\n    if x > 100:\n        apply(f, x)\n    else:\n        x\n\n\
fn main() -> Int:\n    pick(3)\n");
    assert_eq!(branch, entry + 1, "a cold branch's lambda moved to the entry block");
}
