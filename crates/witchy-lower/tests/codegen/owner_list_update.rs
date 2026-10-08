use witchy_lower::codegen::compile_checked_module_artifact;
use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{Operation, closure_owner_contract::OwnerOperation};

fn no_expand(
    _name: &str,
    _module: &mut witchy_syntax::ast::Module,
    _siblings: &[(String, witchy_syntax::ast::Module)],
) -> Result<witchy_syntax::origin::OriginTable, String> {
    Ok(witchy_syntax::origin::OriginTable::default())
}

fn checked(source: &str) -> CheckedModule {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".into(), source.as_bytes().to_vec())], "main",
        witchy_syntax::linker::ComptimeExpander::new(no_expand),
    ).expect("checked owner-list source")
}

fn program(body: &str) -> String {
    format!("fn make(own value: Int) -> &Int:\n    &value\n\nfn main() -> Int:\n    let view = make(11)\n    let first = fn() -> Int: *view\n    let replacement = fn() -> Int: *view + 1\n    var values = [first, first]\n{body}\n")
}

fn replacement_counts(source: &str) -> (usize, usize) {
    let input = checked(source);
    let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&input);
    let source = input.canonical_source().expect("source");
    let id = candidate.source_abi_bindings().entries().iter().find_map(|binding| {
        let declaration = source.view().closed_callable_declaration(binding.source_declaration_ordinal())?;
        (declaration.name == "main").then_some(binding.function())
    }).expect("replace binding");
    let function = candidate.functions().iter().find(|function| function.id == id).expect("replace function");
    let operations = function.blocks.iter().flat_map(|block| &block.operations).collect::<Vec<_>>();
    let sets = operations.iter().filter(|op| matches!(op, Operation::ClosureOwner(OwnerOperation::ListSet { .. }))).count();
    let clones = operations.iter().filter(|op| matches!(op, Operation::ClosureOwner(OwnerOperation::CloneList { .. }))).count();
    compile_checked_module_artifact(&input).expect_lowered("admit owner-list replacement");
    (sets, clones)
}

#[test]
fn owned_owner_list_replacement_consumes_original_list_token() {
    let counts = replacement_counts(&program("    values[0] = replacement\n    12"));
    assert_eq!(counts, (1, 0));
}

#[test]
fn captured_owner_list_snapshot_consumes_the_mutation_root() {
    let counts = replacement_counts(&program("    let snapshot = values\n    values[0] = replacement\n    let first = snapshot[0]\n    first()"));
    assert_eq!(counts.0, 1);
    assert_eq!(counts.1, 1, "only the snapshot retains another list token: {counts:?}");
}

#[test]
fn owner_list_same_element_replacement_consumes_the_retained_root() {
    let counts = replacement_counts(&program("    values[0] = values[0]\n    11"));
    assert_eq!(counts.0, 1);
    assert_eq!(counts.1, 0, "replacement consumes its shared-origin parent token: {counts:?}");
}

#[test]
fn owner_list_read_then_replace_consumes_the_shared_origin_root() {
    let counts = replacement_counts(&program("    let first = values[0]\n    let before = first()\n    values[0] = replacement\n    before"));
    assert_eq!(counts, (1, 0));
}

#[test]
fn owner_list_loop_read_then_replace_keeps_shared_origins_copy_free() {
    let counts = replacement_counts(&program("    var index = 0\n    while index < 3:\n        let first = values[0]\n        let _ = first()\n        values[0] = replacement\n        index = index + 1\n    12"));
    assert_eq!(counts, (1, 0));
}

#[test]
fn borrowed_owner_list_cannot_be_rebound_as_an_owned_mutation_target() {
    let source = "fn change(let values: List(fn() -> Int), replacement: fn() -> Int):\n    var changed = values\n    changed[0] = replacement\n\nfn main():\n    return\n";
    let error = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".into(), source.as_bytes().to_vec())], "main",
        witchy_syntax::linker::ComptimeExpander::new(no_expand),
    ).err().expect("borrowed mutation target must reject");
    assert!(error.to_string().contains("call-scoped `let` parameter access"), "{error}");
}
