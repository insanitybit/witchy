//! RFC-0090 criterion 7: proper calls forward the complete RFC-0087 envelope.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

fn linked(source: &str) -> witchy::ast::Module {
    let parsed = parser::parse_module(source).expect("parse criterion-7 program");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link criterion-7 program");
    typeck::check(&linked).expect("typecheck criterion-7 program");
    linked
}

fn optimized_wir(source: &str) -> witchy_wir::wir::WirModule {
    codegen::assemble_optimized_wir_module(&linked(source))
        .expect_lowered("criterion-7 program lowers to optimized WIR")
}

fn function_debug(module: &witchy_wir::wir::WirModule, name: &str) -> String {
    let function = module
        .funcs
        .iter()
        .find(|function| {
            function.name == name || function.name.ends_with(&format!(".{name}"))
        })
        .unwrap_or_else(|| panic!("missing WIR function {name}"));
    format!("{:?}", function.body)
}

fn assert_both_backends(source: &str, expected: &[&str]) {
    let linked = linked(source);
    let want: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        want,
    );

    let wasm = codegen::compile_module_binary(&linked)
        .expect_lowered("compile criterion-7 program");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn compiled criterion-7 program");
    actor.run().expect("run compiled criterion-7 program");
    assert_eq!(actor.output(), want);
}

#[test]
fn direct_var_result_envelope_is_one_portable_loop() {
    let source = r#"
fn descend(var total: Int, n: Int) -> Int:
    if n <= 0:
        total
    else:
        total = total + 1
        descend(total, n - 1)

fn main(console: Console):
    var total = 0
    let value = descend(total, 300000)
    console.print("${value} ${total}")
"#;
    let body = function_debug(&optimized_wir(source), "descend");
    assert!(
        body.contains("Loop"),
        "forwarded var envelope must become a loop: {body}"
    );
    assert!(
        !body.contains("func: \"main.descend\""),
        "forwarded var envelope retained a recursive backend call: {body}",
    );
    assert_both_backends(source, &["300000 300000"]);
}

#[test]
fn every_var_result_is_forwarded_in_declaration_order() {
    let source = r#"
fn braid(var left: Int, var right: Int, n: Int) -> Int:
    if n <= 0:
        left + right
    else:
        left = left + 1
        right = right + 2
        braid(left, right, n - 1)

fn main(console: Console):
    var left = 0
    var right = 0
    let value = braid(left, right, 300000)
    console.print("${value} ${left} ${right}")
"#;
    let body = function_debug(&optimized_wir(source), "braid");
    assert!(
        body.contains("Loop"),
        "the complete multi-var envelope must loop: {body}"
    );
    assert!(
        !body.contains("func: \"main.braid\""),
        "multi-var recursion survived lowering: {body}",
    );
    assert_both_backends(source, &["900000 300000 600000"]);
}

#[test]
fn mutual_var_envelopes_share_one_typed_dispatcher() {
    let source = r#"
fn left(var total: Int, n: Int) -> Int:
    if n <= 0:
        total
    else:
        total = total + 1
        right(total, n - 1)

fn right(var state: Int, n: Int) -> Int:
    if n <= 0:
        state
    else:
        state = state + 2
        left(state, n - 1)

fn main(console: Console):
    var total = 0
    let value = left(total, 300000)
    console.print("${value} ${total}")
"#;
    let wir = optimized_wir(source);
    let dispatcher = wir
        .funcs
        .iter()
        .find(|function| function.name.contains("__witchy_tail_envelope_scc"))
        .unwrap_or_else(|| panic!("missing multi-result dispatcher"));
    let body = format!("{:?}", dispatcher.body);
    assert!(body.contains("Loop"), "mutual var dispatcher must loop: {body}");
    assert!(
        !body.contains("func: \"main.left\"")
            && !body.contains("func: \"main.right\""),
        "mutual var dispatcher retained a recursive backend call: {body}",
    );
    assert_both_backends(source, &["450000 450000"]);
}

#[test]
fn capacity_token_is_part_of_the_forwarded_envelope() {
    let source = r#"
import list

fn overwrite(var xs: List(Int), n: Int) -> Int:
    if n <= 0:
        list.at(xs, 0)
    else:
        xs.set_at(0, n)
        overwrite(xs, n - 1)

fn main(console: Console):
    var xs = [0]
    let value = overwrite(xs, 300000)
    console.print("${value} ${xs}")
"#;
    let body = function_debug(&optimized_wir(source), "overwrite");
    assert!(
        body.contains("Loop"),
        "the value and capacity results must loop together: {body}"
    );
    assert!(
        !body.contains("func: \"main.overwrite\""),
        "capacity-bearing recursion survived lowering: {body}",
    );
    assert_both_backends(source, &["1 [1]"]);
}

#[test]
fn capability_writeback_keeps_its_externref_result_kind() {
    let source = r#"
fn thread(var current: Dir, n: Int) -> Int:
    if n <= 0:
        7
    else:
        thread(current, n - 1)

fn main(console: Console, root: Dir):
    var current = root
    console.print("${thread(current, 1)}")
"#;
    let wir = optimized_wir(source);
    let function = wir
        .funcs
        .iter()
        .find(|function| function.name == "main.thread")
        .expect("capability-tail function");
    assert_eq!(
        function
            .ret
            .iter()
            .map(|result| result.kind())
            .collect::<Vec<_>>(),
        vec![witchy_wir::wir::Kind::I64, witchy_wir::wir::Kind::ExternRef],
    );
    let body = format!("{:?}", function.body);
    assert!(
        body.contains("Loop"),
        "capability envelope must loop without boxing: {body}"
    );
    assert!(!body.contains("func: \"main.thread\""), "{body}");
}

#[test]
fn explicit_return_forwards_the_complete_var_envelope() {
    let source = r#"
fn descend(var total: Int, n: Int) -> Int:
    if n <= 0:
        return total
    total = total + 1
    return descend(total, n - 1)

fn main(console: Console):
    var total = 0
    let value = descend(total, 300000)
    console.print("${value} ${total}")
"#;
    let body = function_debug(&optimized_wir(source), "descend");
    assert!(
        body.contains("Loop"),
        "an explicit returned envelope must loop: {body}"
    );
    assert!(
        !body.contains("func: \"main.descend\""),
        "explicit-return recursion survived lowering: {body}",
    );
    assert_both_backends(source, &["300000 300000"]);
}

#[test]
fn explicit_recursive_return_preserves_a_fallthrough_base_envelope() {
    let source = r#"
fn descend(var total: Int, n: Int) -> Int:
    if n > 0:
        total = total + 1
        return descend(total, n - 1)
    total

fn main(console: Console):
    var total = 0
    let value = descend(total, 300000)
    console.print("${value} ${total}")
"#;
    let body = function_debug(&optimized_wir(source), "descend");
    assert!(
        body.contains("Loop"),
        "an explicit recursive return with a fallthrough base must loop: {body}"
    );
    assert!(!body.contains("func: \"main.descend\""), "{body}");
    assert_both_backends(source, &["300000 300000"]);
}

#[test]
fn nested_place_reconstruction_remains_non_tail() {
    let source = r#"
fn descend(var cell: Int, n: Int) -> Int:
    if n <= 0:
        cell
    else:
        var row = [cell + 1]
        descend(row[0], n - 1)

fn main(console: Console):
    var cell = 0
    let value = descend(cell, 20)
    console.print("${value} ${cell}")
"#;
    let body = function_debug(&optimized_wir(source), "descend");
    assert!(
        !body.contains("Loop"),
        "nested-place reconstruction is caller work and cannot become proper: {body}",
    );
    assert!(
        body.contains("func: \"main.descend\""),
        "the non-proper recursive call must remain explicit: {body}",
    );
    assert_both_backends(source, &["20 0"]);
}
