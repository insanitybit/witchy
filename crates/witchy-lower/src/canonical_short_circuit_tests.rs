//! Source-backed short-circuit state, snapshot, and ownership regressions.

use witchy_syntax::origin::OriginTable;
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::{check_initial_with_witness, validate_final, FunctionId};

pub(super) fn assert_execution(source_text: &str, expected: i64) {
    // Exercise the same source in both modes. These are semantic tests of the
    // initial graph; optional optimizer scheduling belongs to its own suite.
    for mode in ["", "mode opt\n\n"] {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), format!("{mode}{source_text}").into_bytes())],
            "main", |_, _, _| Ok(OriginTable::default()),
        ).expect("check source declarations, construction, and calls");
        let source = checked.canonical_source().expect("source authority");
        let layouts = crate::canonical_layouts::derive_layout_bundle(source).expect("source layouts");
        let candidate = super::lower_typed_module(source, layouts).expect("lower short-circuit state")
            .with_launch_contract_payload(source.launch_contract_payload().to_vec());
        let initial = check_initial_with_witness(candidate, &source.view()).expect("check ownership and joins");
        let validated = validate_final(initial, &source.view()).expect("final validation");
        witchy_wir::admit_canonical_with_observer(validated, &source.view(), |validated, artifact| {
            let export = artifact.entry().expect("admitted entry").export();
            let main = FunctionId::new(export.strip_prefix('f').expect("canonical function export")
                .parse().expect("canonical entry function id"));
            let oracle = interpret_program(validated, main, Vec::new(), InterpreterConfig::default())
                .expect("execute independent canonical semantics");
            assert_eq!(oracle.results, vec![InterpreterValue::Int(expected)], "{mode}{source_text}");
            assert_eq!(oracle.leaked_objects, 0, "all branch and snapshot ownership tokens must be released");
            let engine = wasmtime::Engine::default();
            let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("encoded Wasm");
            let mut store = wasmtime::Store::new(&engine, ());
            let instance = crate::canonical_pipeline::tests::fixture_diagnostic_linker(&engine)
                .instantiate(&mut store, &module).expect("instantiate admitted bytes");
            let entry = instance.get_typed_func::<(), i64>(&mut store, export)
                .expect("typed entry");
            assert_eq!(entry.call(&mut store, ()).expect("execute admitted bytes"), expected, "{mode}{source_text}");
        }).expect("encode and admit exact checked artifact");
    }
}

#[test]
fn skipped_rhs_does_not_execute_trapping_call_or_its_writeback() {
    assert_execution("fn bad(var xs: List(Int)) -> Bool:\n    xs = [2]\n    xs[99] == 0\nfn main() -> Int:\n    var xs = [1]\n    let a = false && bad(xs)\n    let b = true || bad(xs)\n    if !a && b:\n        xs[0]\n    else:\n        0\n", 1);
}
