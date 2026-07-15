//! RFC-0090 criterion 9: async segments and RFC-0083 loans at proper tail edges.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

fn linked(source: &str) -> witchy::ast::Module {
    let parsed = parser::parse_module(source).expect("parse RFC-0090 criterion-9 program");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link RFC-0090 criterion-9 program");
    typeck::check(&linked).expect("typecheck RFC-0090 criterion-9 program");
    linked
}

fn optimized_wir(source: &str) -> witchy_wir::wir::WirModule {
    codegen::assemble_optimized_wir_module(&linked(source))
        .expect_lowered("criterion-9 program lowers to optimized WIR")
}

fn assert_both_backends(source: &str, expected: &[&str]) {
    let linked = linked(source);
    let want: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        want,
    );

    let wasm = codegen::compile_module_binary(&linked).expect_lowered("compile criterion-9 program");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn compiled criterion-9 program");
    actor.run().expect("run compiled criterion-9 program");
    assert_eq!(actor.output(), want);
}

fn function_debug(module: &witchy_wir::wir::WirModule, name: &str) -> String {
    let function = module
        .funcs
        .iter()
        .find(|function| {
            function.name == name || function.name.ends_with(&format!(".{name}"))
        })
        .unwrap_or_else(|| {
            panic!(
                "missing WIR function {name}; available: {:?}",
                module.funcs.iter().map(|function| &function.name).collect::<Vec<_>>()
            )
        });
    format!("{:?}", function.body)
}

#[test]
fn recursive_async_segment_scc_is_a_portable_tail_loop() {
    let source = r#"
import chan

async fn spin(n: Int) -> Int:
    var i = 0
    while i < n:
        i = i + 1
        chan.yield_now().await
    i

async fn main(console: Console):
    let n = spin(4).await
    console.print("${n}")
"#;
    let wir = optimized_wir(source);
    let segments: Vec<_> = wir
        .funcs
        .iter()
        .filter(|function| function.name.contains("__async_spin_"))
        .collect();
    assert!(!segments.is_empty(), "async lowering must emit named spin segments");
    let dispatchers: Vec<_> = wir
        .funcs
        .iter()
        .filter(|function| function.name.contains("__witchy_tail_scc"))
        .collect();
    assert_eq!(dispatchers.len(), 1, "the recursive segment cycle forms one SCC");
    let dispatcher = format!("{:?}", dispatchers[0].body);
    assert!(
        dispatcher.contains("Loop"),
        "the recursive async segment SCC must use the portable loop: {dispatcher}",
    );
    assert!(
        segments.iter().all(|function| {
            let body = format!("{:?}", function.body);
            !segments.iter().any(|candidate| {
                body.contains(&format!("func: \"{}\"", candidate.name))
            })
        }),
        "optimized async segments must contain no recursive backend edge: {segments:#?}",
    );
    assert_both_backends(source, &["4"]);
}

#[test]
fn suspension_keeps_a_continuation_and_is_not_a_tail_edge() {
    let source = r#"
import chan

async fn once(n: Int) -> Int:
    chan.yield_now().await
    n + 1

async fn main(console: Console):
    let n = once(6).await
    console.print("${n}")
"#;
    let wir = optimized_wir(source);
    let once = wir
        .funcs
        .iter()
        .find(|function| function.name == "once" || function.name.ends_with(".once"))
        .expect("lowered async entry");
    let body = format!("{:?}", once.body);
    assert!(body.contains("task.lazy") || body.contains("task.and_then"), "{body}");
    assert!(!body.contains("Loop"), "a suspension is resumable state, not a tail loop: {body}");
    assert_both_backends(source, &["7"]);
}

#[test]
fn ended_loan_allows_recursive_tail_lowering() {
    let source = r#"
mode opt
import list

fn view(xs: let('a) List(Int)) -> View(List(Int), 'a):
    xs

fn descend(xs: List(Int), n: Int) -> Int:
    let borrowed = view(xs)
    let width = list.length(borrowed)
    if n <= 0:
        width
    else:
        descend(xs, n - 1)

fn main(console: Console):
    console.print("${descend([1, 2, 3], 300000)}")
"#;
    let body = function_debug(&optimized_wir(source), "descend");
    assert!(body.contains("Loop"), "the ended loan leaves no tail continuation: {body}");
    assert!(!body.contains("func: \"descend\""), "recursive call survived: {body}");
    assert_both_backends(source, &["3"]);
}

#[test]
fn live_loan_after_recursive_call_keeps_the_edge_non_tail() {
    let source = r#"
mode opt
import list

fn view(xs: let('a) List(Int)) -> View(List(Int), 'a):
    xs

fn descend(xs: List(Int), n: Int) -> Int:
    let borrowed = view(xs)
    if n <= 0:
        list.length(borrowed)
    else:
        let rest = descend(xs, n - 1)
        list.length(borrowed) + rest

fn main(console: Console):
    console.print("${descend([1, 2, 3], 20)}")
"#;
    let body = function_debug(&optimized_wir(source), "descend");
    assert!(!body.contains("Loop"), "post-call loan use must remain in the caller: {body}");
    assert!(body.contains("func: \"main.descend\""), "the non-proper call must remain: {body}");
    assert_both_backends(source, &["63"]);
}

#[test]
fn returned_view_obligation_transfers_across_mutual_tail_edges() {
    let source = r#"
mode opt
import list

fn view(xs: let('a) List(Int)) -> View(List(Int), 'a):
    xs

fn left(xs: let('a) List(Int), n: Int) -> View(List(Int), 'a):
    if n <= 0:
        view(xs)
    else:
        right(xs, n - 1)

fn right(xs: let('a) List(Int), n: Int) -> View(List(Int), 'a):
    if n <= 0:
        view(xs)
    else:
        left(xs, n - 1)

fn main(console: Console):
    let values = [1, 2, 3, 4]
    let borrowed = left(values, 300001)
    console.print("${list.length(borrowed)}")
"#;
    let wir = optimized_wir(source);
    let dispatcher = wir
        .funcs
        .iter()
        .find(|function| function.name.contains("__witchy_tail_scc"))
        .unwrap_or_else(|| {
            panic!(
                "mutual borrowed-result edge did not form a dispatcher: {:?}",
                wir.funcs.iter().map(|function| &function.name).collect::<Vec<_>>()
            )
        });
    let body = format!("{:?}", dispatcher.body);
    assert!(body.contains("Loop"), "borrow obligation transfers through the dispatcher: {body}");
    assert!(!body.contains("func: \"left\"") && !body.contains("func: \"right\""), "{body}");
    assert_both_backends(source, &["4"]);
}
