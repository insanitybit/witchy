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

    /// BUG-609: A BINDING IN SCOPE SHADOWS AN INTRINSIC. A closure-typed parameter
    /// named after a bare intrinsic must be typed from its own function type, not
    /// from the intrinsic catalog. `read` (`fn(String) -> String`) previously lowered
    /// to the `file_read` WIR helper and failed wasm validation ("expected externref,
    /// found i32"); `now` (`fn() -> Int`) mis-widened to i64. No capability is
    /// involved — the defect was purely name-keyed. Both names are pinned because
    /// they exercise the two distinct kind mismatches.
    #[test]
    fn closure_param_shadowing_an_intrinsic_agrees() {
        let read_shadow = "fn use_it(console: Console, read: fn(String) -> String):\n    console.print(read(\"x\"))\n\nfn main(console: Console):\n    use_it(console, fn(s: String) -> String: s + \"!\")\n";
        let expected = ["x!"];
        assert_eq!(link_run(read_shadow), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", read_shadow)], "main"), expected, "wasm");

        let now_shadow = "fn use_it(console: Console, now: fn() -> Int):\n    console.print(\"${now()}\")\n\nfn main(console: Console):\n    use_it(console, fn() -> Int: 42)\n";
        let expected = ["42"];
        assert_eq!(link_run(now_shadow), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", now_shadow)], "main"), expected, "wasm");

        // A shadowing LOCAL (not just a parameter) resolves the same way.
        let local_shadow = "fn main(console: Console):\n    let read = fn(s: String) -> String: s + \"?\"\n    console.print(read(\"y\"))\n";
        let expected = ["y?"];
        assert_eq!(link_run(local_shadow), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", local_shadow)], "main"), expected, "wasm");
    }
