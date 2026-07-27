use super::*;
use crate::{codegen, interpreter, parser, typeck};

    /// (RFC-0032) `vm.par_map` stays correct when the native worker-VM fast path does
    /// NOT apply — a CAPTURING closure (here `fn(n): n + base`) would be unsound to run
    /// with a null environment in a separate worker VM, so the compiled backend must
    /// fall through to the sequential `List.map` body. Both backends must still agree.
    #[test]
    fn vm_par_map_capturing_closure_agrees() {
        let src = "import vm\n\nfn main(console: Console):\n    let base = 100\n    let ys = vm.par_map([1, 2, 3], fn(n): n + base)\n    console.print(\"${ys}\")\n";
        let expected = ["[101, 102, 103]"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    #[test]
    fn brace_free_lambda_form() {
        // `fn(params): expr` — the brace-free single-expression lambda, used
        // inline inside call parens where layout is suppressed. Both backends.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 4]
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    console.print("${list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))}")
    console.print("${list.length(list.filter(xs, fn(n: Int): ((n % 2) == 0)))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "brace-free lambda diverged");
        assert_eq!(compiled, vec!["20", "2"]);
    }

    #[test]
    fn func_on_backends_agree() {
        // on(op, f) lifts op to act on projections — here sorting (name, age)
        // pairs by age via func.on_key(lt, snd).
        let client = r#"
import func
import list
fn fst(p: (String, Int)) -> String:
    let (a, _b) = p
    a
fn snd(p: (String, Int)) -> Int:
    let (_a, b) = p
    b
fn lt(a: Int, b: Int) -> Bool:
    a < b
fn main(console: Console):
    var people = [("alice", 30), ("bob", 25), ("carol", 35)]
    list.sort_by(people, func.on_key(lt, snd))
    console.print(list.join(list.map(people, fst), ","))
    let by_age = func.on_key(lt, snd)
    console.print(if by_age(("x", 1), ("y", 2)): "lt" else: "ge")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("func", crate::bundled_module("func").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "func.on diverged");
        assert_eq!(compiled, vec!["bob,alice,carol", "lt"]);
    }

    #[test]
    fn sandbox_runs_compiled_and_captures_output() {
        // `witchy sandbox` compiles to WASM and runs in the capability sandbox,
        // returning the program's output.
        let path = std::env::temp_dir().join(format!("witchy_sandbox_smoke_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "fn main(console: Console):\n    console.print(\"${6 * 7}\")\n",
        )
        .unwrap();
        let (out, exit) =
            crate::run_file_sandboxed(
                path.to_str().unwrap(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                None,
                Vec::new(),
                witchy_confinement::EnforcementMode::Disabled,
            )
                .expect("sandbox run");
        assert_eq!(out, vec!["42"]);
        assert_eq!(exit, None, "a Nil-returning main has no exit code");
    }

    /// Regression: a for-loop in a function body followed by a closure both lower
    /// on the binary path; the loop watermark must be captured for the WHOLE
    /// function, NOT mistaking the inner loop body for the function body — a bug
    /// that silently compiled a loop to a single iteration. Closures now lower
    /// (the lifted body + closure object + `call_indirect`), so this program takes
    /// the binary path end-to-end; the loop emitting all three iterations under the
    /// binary sink is the live proof the capture is not mis-scoped.
    #[test]
    fn wir_loop_then_closure_lowers_and_keeps_loop_scope() {
        let src = "fn main(console: Console):\n    for x in [10, 20, 30]:\n        console.print(\"${x}\")\n    let f = fn(n: Int): n + 1\n    console.print(\"${f(5)}\")\n";
        let want = vec!["10".to_string(), "20".to_string(), "30".to_string(), "6".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        // Takes the binary path (closures lower) AND emits all loop iterations —
        // a mis-scoped capture would drop the loop to a single pass.
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("loop + closure must lower on the binary path");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
        assert_eq!(run_on_wasm(src), want, "WAT path");
    }

    #[test]
    fn to_string_respects_lambda_param_shadowing_on_wasm() {
        // The outer `x` is an Int; the lambda's `x` is a String param. `to_string`
        // inside the lambda must pass the String through, not run int_to_string on
        // the pointer — i.e. value-type tracking is scoped per lambda.
        let src = r#"
fn apply(f: fn(String) -> String, s: String) -> String:
    f(s)

fn main(console: Console):
    let x = 5
    console.print("${x}")
    console.print(apply(fn(x: String): "${x}", "hey"))
"#;
        assert_eq!(run_on_wasm(src), vec!["5", "hey"]);
    }

    #[test]
    fn closures_example_runs_on_wasm() {
        // Higher-order functions + closures, compiled to WASM: apply(square, 9) =
        // 81; twice(+3, 10) = ((10+3)+3) = 16; apply(adder(100), 5) = 105 (the
        // returned closure captures `by = 100`).
        assert_eq!(
            run_on_wasm(include_str!("../../examples/closures/src/closures.witchy")),
            vec!["81", "16", "105"]
        );
    }

    /// `higher_order_sum` reproduces Rust by Example's "sum of squared odd numbers
    /// under 1000" — an imperative range loop and a functional `std/list` pipeline
    /// (map / take_while / filter / sum) that must agree, on both backends.
    #[test]
    fn higher_order_sum_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/higher_order_sum/src/higher_order_sum.witchy").unwrap();
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client.as_str())];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "higher_order_sum diverged");
        assert_eq!(compiled, vec!["imperative: 5456".to_string(), "functional: 5456".to_string()]);
    }

    #[test]
    fn higher_order_example_runs_on_wasm() {
        // Closure returned from a function (make_adder) + higher-order reduce.
        let src = include_str!("../../examples/higher_order/src/higher_order.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["15", "81", "15", "120"]);
    }

    #[test]
    fn closures_capturing_loop_var_backends_agree() {
        // Closures created in a loop each capture that iteration's value of the
        // loop variable (by value), are stored in a list, and called back. Both
        // backends agree — no shared-loop-variable surprise.
        let src = r#"
fn main(console: Console):
    var fs = []
    for i in [1, 2, 3]:
        list.push(fs, fn(x: Int): (x + i))
    let f0 = list.at(fs, 0)
    let f2 = list.at(fs, 2)
    console.print("${f0(10)}")
    console.print("${f2(10)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["11", "13"]);
    }

    #[test]
    fn zero_arg_closures_backends_agree() {
        // Zero-argument closures (incl. capturing ones) compile and run.
        let src = r#"
fn call0(f: fn() -> Int) -> Int:
    f()

fn main(console: Console):
    console.print("${call0(fn(): 42)}")
    let base = 100
    console.print("${call0(fn(): (base + 1))}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "101"]);
    }

    #[test]
    fn closure_capturing_closure_backends_agree() {
        // A closure that captures another closure and calls it through a
        // higher-order function. Both backends agree.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let g = fn(x: Int): (x + 1)
    let h = fn(y: Int): (apply(g, y) * 2)
    console.print("${apply(h, 5)}")
    console.print("${apply(h, 20)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["12", "42"]); // (5+1)*2, (20+1)*2
    }

    #[test]
    fn captured_inferred_dict_keeps_key_and_value_widths() {
        let src = r#"
import dict

fn call0(f: fn() -> Int) -> Int:
    f()

fn main(console: Console):
    var d = dict.new()
    dict.insert(d, 5000000000, 9000000000)
    let captured = d
    let direct = fn():
        var total = 0
        for (k, v) in dict.pairs(captured):
            total = total + k + v
        total
    console.print("${direct()}")
    console.print("${call0(fn():
        var total = 0
        for (k, v) in dict.pairs(captured):
            total = total + k + v
        total
    )}")
"#;
        let want = vec!["14000000000", "14000000000"];
        assert_eq!(interp(src), want, "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM");
    }

    // Boundary behavior of the string builtins: an empty separator yields the
    // whole string, substrings clamp (and start>end gives ""), an empty needle
    // for index_of returns 0, a missing one returns -1, and empty-string concat
    // is identity. These clamp/empty rules are easy to get subtly different in
    // codegen, so assert the backends agree.
    // Immediately applying a function-valued expression — `make(3)(4)`,
    // `(fn(x){..})(7)`, chains, and an application nested inside another's
    // argument — must behave identically on both backends. The nested-in-arg
    // case in particular exercises codegen's per-level scratch locals (the
    // callee pointer must survive argument evaluation).
    // Function values stored in data structures and applied immediately — the
    // composition unlocked by Expr::Apply. A closure pulled from a list with
    // `at`, one selected by an `if` expression, and one held in a record field
    // (reached via `(b.f)(b.n)`) must all apply identically on both backends.
    #[test]
    fn fn_values_in_data_backends_agree() {
        let src = r#"
type Box:
    f: fn(Int) -> Int
    n: Int

fn main(console: Console):
    let fns = [fn(x: Int): (x + 1), fn(x: Int): (x * 10)]
    console.print("${(list.at(fns, 0))(5)}")
    console.print("${(list.at(fns, 1))(5)}")
    let pick = true
    console.print("${(if pick: fn(x: Int): (x + 100) else: fn(x: Int): x)(7)}")
    let b = Box(fn(x: Int): (x * 3), 7)
    console.print("${((b).f)((b).n)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "fn-values-in-data diverged");
        assert_eq!(run_on_wasm(src), vec!["6", "50", "107", "21"]);
    }

    // The classic loop-capture pitfall: each iteration creates a closure that
    // captures a fresh `let` binding. Capture is by value at creation, so the
    // three closures must remember 0, 1, 2 (giving 10, 11, 12) — not share the
    // final loop value. Both backends must agree.
    #[test]
    fn closure_captures_loop_value_backends_agree() {
        let src = r#"
fn main(console: Console):
    var fns = []
    var i = 0
    while (i < 3):
        let captured = i
        list.push(fns, fn(x: Int): (x + captured))
        i = (i + 1)
    for f in fns:
        console.print("${f(10)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "loop-captured closures diverged");
        assert_eq!(run_on_wasm(src), vec!["10", "11", "12"]);
    }

    #[test]
    fn function_pipeline_fold_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let inc = fn(x: Int): (x + 1)
    let dbl = fn(x: Int): (x * 2)
    let neg = fn(x: Int): (0 - x)
    let pipeline = [inc, dbl, neg]
    let result = list.fold(pipeline, 5, fn(acc: Int, f: fn(Int) -> Int): f(acc))
    console.print("${result}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "function-pipeline fold diverged");
        assert_eq!(compiled, vec!["-12"]);
    }

    #[test]
    fn closures_and_string_ordering_backends_agree() {
        let src = r#"
fn main(console: Console):
    let base = 10
    let add = fn(n: Int): (n + base)
    var total = 0
    var i = 0
    while (i < 5):
        total = (total + add(i))
        i = (i + 1)
    console.print("${total}")
    let make_adder = fn(x: Int): fn(y: Int): (x + y)
    let add3 = make_adder(3)
    console.print("${add3(4)}")
    console.print("${(make_adder(100))(1)}")
    if ("abc" < "abcd"):
        console.print("lt1")
    else:
        console.print("ge1")
    if ("Z" < "a"):
        console.print("lt2")
    else:
        console.print("ge2")
    if ("" < "a"):
        console.print("lt3")
    else:
        console.print("ge3")
    if ("apple" < "apply"):
        console.print("lt4")
    else:
        console.print("ge4")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "closures/ordering diverged");
    }

