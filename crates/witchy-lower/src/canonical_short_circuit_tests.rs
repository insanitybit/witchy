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
        let initial = check_initial_with_witness(candidate, source).expect("check ownership and joins");
        let validated = validate_final(initial, source).expect("final validation");
        witchy_wir::admit_canonical_with_observer(validated, source, |validated, artifact| {
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
fn scalar_writeback_taken_and_skipped_evaluates_each_operand_once() {
    for operator in ["&&", "||"] {
        for left in [false, true] {
            for right in [false, true] {
                let taken = if operator == "&&" { left } else { !left };
                let result = if operator == "&&" { left && right } else { left || right };
                let source = format!("fn touch(var n: Int, answer: Bool) -> Bool:\n    n = n + 1\n    answer\nfn main() -> Int:\n    var left_count = 0\n    var right_count = 0\n    let answer = touch(left_count, {left}) {operator} touch(right_count, {right})\n    left_count * 100 + right_count * 10 + (if answer: 1 else: 0)\n");
                assert_execution(&source, 100 + i64::from(taken) * 10 + i64::from(result));
            }
        }
    }
}

#[test]
fn list_writeback_joins_replacement_extents_and_preserves_snapshots() {
    for operator in ["&&", "||"] {
        for left in [false, true] {
            let taken = if operator == "&&" { left } else { !left };
            let source = format!("fn revise(var xs: List(Int), var calls: Int) -> Bool:\n    calls = calls + 1\n    xs = [xs[0] + 10, 20, 30]\n    false\nfn main() -> Int:\n    var xs = [7]\n    let saved = xs\n    var calls = 0\n    let answer = {left} {operator} revise(xs, calls)\n    let after = xs\n    xs[0] = xs[0] + 1\n    calls * 10000 + saved[0] * 1000 + after[0] * 10 + xs[0] + (if answer: 100000 else: 0)\n");
            let value = if taken { 17 } else { 7 };
            let result = operator == "||" && left;
            assert_execution(&source, i64::from(taken) * 10000 + 7000 + value * 10 + value + 1 + i64::from(result) * 100000);
        }
    }
}

#[test]
fn nested_short_circuits_join_list_tokens_through_loop_backedges() {
    assert_execution("fn step(var xs: List(Int), var calls: Int, answer: Bool) -> Bool:\n    calls = calls + 1\n    xs = [xs[0] + 1]\n    answer\nfn main() -> Int:\n    var xs = [0]\n    let saved = xs\n    var calls = 0\n    var i = 0\n    while i < 4:\n        let answer = (i < 2 && step(xs, calls, false)) || (i >= 2 && step(xs, calls, true))\n        if answer:\n            xs[0] = xs[0] + 10\n        i = i + 1\n    saved[0] * 10000 + calls * 100 + xs[0]\n", 424);
}

#[test]
fn left_reference_writeback_is_visible_on_both_successors() {
    for operator in ["&&", "||"] {
        for left in [false, true] {
            let taken = if operator == "&&" { left } else { !left };
            let source = format!("fn step(var xs: List(Int), var calls: Int, amount: Int, answer: Bool) -> Bool:\n    calls = calls + 1\n    xs = [xs[0] + amount, amount]\n    answer\nfn main() -> Int:\n    var xs = [1]\n    let saved = xs\n    var calls = 0\n    let answer = step(xs, calls, 2, {left}) {operator} step(xs, calls, 4, false)\n    xs[0] * 100 + saved[0] * 10 + calls + (if answer: 10000 else: 0)\n");
            let value = if taken { 7 } else { 3 };
            let result = operator == "||" && left;
            assert_execution(&source, value * 100 + 10 + 1 + i64::from(taken) + i64::from(result) * 10000);
        }
    }
}

#[test]
fn pointer_list_writeback_releases_children_and_keeps_old_snapshot() {
    for left in [false, true] {
        let source = format!("fn revise(var xs: List(String)) -> Bool:\n    xs = [xs[0] + \"b\", \"tail\"]\n    true\nfn main() -> Int:\n    var xs = [\"a\"]\n    let saved = xs\n    let answer = {left} && revise(xs)\n    if saved[0] == \"a\" && xs[0] == (if answer: \"ab\" else: \"a\"):\n        1\n    else:\n        0\n");
        assert_execution(&source, 1);
    }
}

#[test]
fn multiple_reference_writebacks_preserve_aliased_extents_independent_of_names() {
    for operator in ["&&", "||"] {
        for left in [false, true] {
            let taken = if operator == "&&" { left } else { !left };
            let source = format!("fn revise(var a: List(Int), var b: List(Int)) -> Bool:\n    a = [2, 3]\n    b = [4]\n    false\nfn main() -> Int:\n    var a = [1]\n    var b = a\n    let z_saved = a\n    let answer = {left} {operator} revise(a, b)\n    a[0] * 100 + b[0] * 10 + z_saved[0] + (if answer: 1000 else: 0)\n");
            let result = operator == "||" && left;
            assert_execution(&source, (if taken { 241 } else { 111 }) + i64::from(result) * 1000);
        }
    }
}

#[test]
fn skipped_rhs_does_not_execute_trapping_call_or_its_writeback() {
    assert_execution("fn bad(var xs: List(Int)) -> Bool:\n    xs = [2]\n    xs[99] == 0\nfn main() -> Int:\n    var xs = [1]\n    let a = false && bad(xs)\n    let b = true || bad(xs)\n    if !a && b:\n        xs[0]\n    else:\n        0\n", 1);
}
