//! A `var self` push into a `unique` record field takes the field from its
//! slot and updates it in place; a plain field is still copied.

use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{LoadTransfer, Operation};

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

fn field_takes(field_type: &str) -> usize {
    let source = format!("type Bag:\n    items: {field_type}\n    n: Int\n\nimpl Bag:\n    pub fn add(var self, x: Int):\n        self.items.push(x)\n        self.n = self.n + 1\n\nfn main() -> Int:\n    var b = Bag([], 0)\n    b.add(1)\n    b.n\n");
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked(&source));
    lowered.functions().iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.operations)
        .filter(|operation| matches!(operation, Operation::Load { transfer: LoadTransfer::TakeNullableUnique, .. }))
        .count()
}

#[test]
fn a_unique_field_is_taken_for_an_in_place_push() {
    assert!(field_takes("unique List(Int)") > 0, "the unique field was copied");
    assert_eq!(field_takes("List(Int)"), 0, "a plain field was taken");
}
