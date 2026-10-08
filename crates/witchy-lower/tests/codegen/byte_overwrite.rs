//! An in-place byte overwrite stores below the count, so it reuses the
//! tracked extent instead of reloading the allocator's capacity: a loop of
//! overwrites keeps one capacity lane and its extent guards stay invariant.

use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{HeaderField, Operation, Projection};

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

fn capacity_loads(source: &str) -> usize {
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked(source));
    lowered.functions().iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.operations)
        .filter(|operation| matches!(operation, Operation::Load { place, .. }
            if *place.projection() == Projection::Header(HeaderField::Capacity)))
        .count()
}

#[test]
fn byte_overwrites_in_a_loop_reload_no_capacity() {
    let overwrite = capacity_loads("import bytes\n\n\
fn main() -> Int:\n    var seq = bytes.repeat(0, 8)\n    var i = 0\n    while i < 8:\n        seq.set_at(i, i)\n        seq.set_at(7 - i, i)\n        i = i + 1\n    seq.at(3)\n");
    let read_only = capacity_loads("import bytes\n\n\
fn main() -> Int:\n    var seq = bytes.repeat(0, 8)\n    var i = 0\n    while i < 8:\n        i = i + 1\n    seq.at(3)\n");
    assert_eq!(overwrite, read_only, "an overwrite reloaded the buffer's capacity");
}
