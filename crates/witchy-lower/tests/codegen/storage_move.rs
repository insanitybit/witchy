//! A string key stored at its source's last use moves into the slot; one
//! whose source is read again is still copied.

use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{Operation, ValueType};

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

/// Reference moves across the whole lowered module.
fn reference_moves(source: &str) -> usize {
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked(source));
    lowered.functions().iter()
        .map(|function| function.blocks.iter()
            .flat_map(|block| &block.operations)
            .filter(|operation| matches!(operation, Operation::MoveValue { source, .. }
                if matches!(function.value_types.get(source), Some(ValueType::Ref(_)))))
            .count())
        .sum()
}

fn program(after: &str) -> String {
    format!("fn main() -> Int:\n    let seq = \"ACGTACGTAC\"\n    var counts = dict.new()\n    var j = 0\n    while j < 6:\n        let kmer = seq.substring(j, j + 4)\n        counts.update(kmer, 0, fn(c: Int): c + j)\n{after}        j = j + 1\n    counts.length()\n")
}

#[test]
fn a_dying_dict_key_moves_into_its_slot() {
    let dying = reference_moves(&program(""));
    let read_again = reference_moves(&program("        let _ = kmer.length()\n"));
    assert!(dying > read_again, "dying key moves: {dying} reference moves, read again: {read_again}");
}
