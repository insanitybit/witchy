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

const SHADOWED_REFERENCE: &str = "type Packet:\n    Packet(List(Int))\n\nfn main() -> Int:\n    let value = [1, 2]\n    var selected = (match Packet([7, 8]):\n        Packet(value) -> [value]\n    )[0]\n    selected.set_at(0, 9)\n    value.at(0) * 10 + selected.at(0)\n";
const EFFECTFUL_ITEM: &str = "fn append(var values: List(Int)) -> Int:\n    values.push(3)\n    values.length()\n\nfn main() -> Int:\n    var values = [1]\n    let result = (match true:\n        true -> [append(values)]\n        false -> []\n    )[0]\n    values.length() * 10 + result\n";
const EFFECT_ORDER: &str = "fn mark(var order: Int, digit: Int) -> Int:\n    order = order * 10 + digit\n    order\n\nfn main() -> Int:\n    var order = 0\n    let result = (match mark(order, 1):\n        _ -> [mark(order, 2)]\n    )[0]\n    order * 100 + result\n";

#[test]
fn irrefutable_empty_arm_cannot_hide_an_unreachable_singleton() {
    let source = "fn main() -> Int:\n    (match true:\n        _ -> []\n        true -> [42]\n    )[0]\n";
    let error = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".into(), source.as_bytes().to_vec())], "main",
        witchy_syntax::linker::ComptimeExpander::new(no_expand),
    ).err().expect("unreachable singleton source must reject");
    assert!(error.to_string().to_lowercase().contains("unreachable"), "{error}");
}

#[test]
fn projected_match_admits_shadowing_and_effectful_items() {
    for source in [SHADOWED_REFERENCE, EFFECTFUL_ITEM, EFFECT_ORDER] {
        compile_checked_module_artifact(&checked(source)).expect_lowered("admit projected match effects");
    }
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn projected_match_preserves_shadowing_snapshots_and_effect_order() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); }
    }
    let _reset = Reset;
    for optimization in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(optimization));
        for (source, expected) in [(SHADOWED_REFERENCE, 19), (EFFECTFUL_ITEM, 22), (EFFECT_ORDER, 1212)] {
            assert_eq!(super::canonical_pipeline::run_callback(source).expect("projected match result"), expected, "{source}");
        }
    }
}
