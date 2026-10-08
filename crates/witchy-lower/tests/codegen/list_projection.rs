use witchy_lower::codegen::compile_checked_module_artifact;
use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{Operation, Terminator};

fn no_expand(
    _name: &str,
    _module: &mut witchy_syntax::ast::Module,
    _siblings: &[(String, witchy_syntax::ast::Module)],
) -> Result<witchy_syntax::origin::OriginTable, String> {
    Ok(witchy_syntax::origin::OriginTable::default())
}

fn checked(source: &str) -> CheckedModule {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".into(), source.as_bytes().to_vec())],
        "main",
        witchy_syntax::linker::ComptimeExpander::new(no_expand),
    ).expect("checked projection source")
}

fn projection(element: &str, ty: &str, alternative: &str) -> CheckedModule {
    let result = if ty == "Int" { "select(true)" } else { "select(true).length()" };
    checked(&format!("fn select(flag: Bool) -> {ty}:\n    (match flag:\n        true -> [{element}]\n        false -> {alternative}\n    )[0]\n\nfn main() -> Int:\n    {result}\n"))
}

#[test]
fn singleton_match_index_removes_scalar_lists_and_preserves_empty_arm_trap() {
    let source = projection("42", "Int", "[]");
    let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&source);
    assert_eq!(candidate.functions().iter().flat_map(|f| &f.blocks)
        .flat_map(|block| &block.operations)
        .filter(|operation| matches!(operation, Operation::Alloc { .. })).count(), 0);
    assert!(candidate.functions().iter().flat_map(|f| &f.blocks)
        .any(|block| matches!(block.terminator, Terminator::Trap)));
    compile_checked_module_artifact(&source).expect_lowered("admit projected scalar match");
}

#[test]
fn singleton_match_index_admits_string_and_nested_list_snapshots() {
    for source in [projection("\"snapshot\"", "String", "[]"), projection("[1, 2]", "List(Int)", "[]")] {
        compile_checked_module_artifact(&source).expect_lowered("admit projected reference match");
    }
}

#[test]
fn singleton_match_index_admits_opaque_callable_values() {
    let source = checked("fn select(flag: Bool) -> fn(Int) -> Int:\n    (match flag:\n        true -> [fn(n: Int): n + 1]\n        false -> []\n    )[0]\n\nfn main() -> Int:\n    let selected = select(true)\n    selected(7)\n");
    compile_checked_module_artifact(&source).expect_lowered("admit projected callable match");
}

#[test]
fn projected_reference_keeps_an_independent_snapshot_when_mutated() {
    let source = checked("fn select(flag: Bool, value: List(Int)) -> List(Int):\n    (match flag:\n        true -> [value]\n        false -> []\n    )[0]\n\nfn main() -> Int:\n    let original = [1, 2]\n    var selected = select(true, original)\n    selected.set_at(0, 9)\n    original.at(0)\n");
    compile_checked_module_artifact(&source).expect_lowered("admit projected snapshot mutation");
}

#[test]
fn multi_element_match_index_retains_list_construction() {
    let source = projection("42", "Int", "[1, 2]");
    let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&source);
    assert!(candidate.functions().iter().flat_map(|f| &f.blocks)
        .flat_map(|block| &block.operations)
        .any(|operation| matches!(operation, Operation::Alloc { .. })));
    compile_checked_module_artifact(&source).expect_lowered("admit ordinary multi-element match");
}
