use super::*;
use crate::{interpreter, parser};

    #[test]
    fn std_func_combinators_backends_agree() {
        // The whole `func` module links + compiles, and its combinators — built
        // on first-class functions — agree across backends: compose threads
        // named functions, flip swaps a subtraction's operands, constant
        // ignores its argument, identity is a no-op.
        let client = r#"
import func

fn double(x: Int) -> Int:
    (x * 2)

fn inc(x: Int) -> Int:
    (x + 1)

fn sub(a: Int, b: Int) -> Int:
    (a - b)

fn main(console: Console):
    let h = func.compose(double, inc)
    console.print("${h(10)}")
    console.print("${(func.flip(sub))(3, 10)}")
    console.print("${(func.constant(42))(999)}")
    console.print("${func.identity(7)}")
"#;
        let sources = [("func", crate::bundled_module("func").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "func combinators diverged");
        assert_eq!(compiled, vec!["22", "7", "42", "7"]);
    }

    // A closure that *calls* a captured function-valued variable (`f(g(x))`,
    // where f and g are captured) must thread f and g through the closure
    // environment and invoke them indirectly — not emit a direct `call $g`.
    // This is the classic `compose`; it must agree across backends.
    #[test]
    fn compose_captured_functions_backends_agree() {
        let src = r#"
fn compose(f: fn(Int) -> Int, g: fn(Int) -> Int) -> fn(Int) -> Int:
    fn(x: Int): f(g(x))

fn double(x: Int) -> Int:
    (x * 2)

fn inc(x: Int) -> Int:
    (x + 1)

fn main(console: Console):
    let h = compose(double, inc)
    console.print("${h(10)}")
    console.print("${(compose(inc, double))(10)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "compose diverged");
        assert_eq!(run_on_wasm(src), vec!["22", "21"]);
    }

    #[test]
    fn function_by_name_as_value_backends_agree() {
        // A bare top-level function name is a first-class value: bind it, call
        // it, and apply it repeatedly. Both backends materialize it as a
        // callable closure.
        let src = r#"
fn double(x: Int) -> Int:
    (x * 2)

fn inc(x: Int) -> Int:
    (x + 1)

fn main(console: Console):
    let f = double
    console.print("${f(5)}")
    let g = inc
    console.print("${g(g(g(0)))}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "function-as-value diverged");
        assert_eq!(run_on_wasm(src), vec!["10", "3"]);
    }

    #[test]
    fn named_function_passed_to_map_backends_agree() {
        // Point-free style: pass a named function (not a lambda) straight to a
        // higher-order std function. Exercises the linker qualifying a bare
        // function-name reference and codegen forwarding through a closure.
        let client = r#"
import list

fn triple(x: Int) -> Int:
    (x * 3)

fn main(console: Console):
    let ys = list.map([1, 2, 3], triple)
    for y in ys:
        console.print("${y}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "named-function-to-map diverged");
        assert_eq!(compiled, vec!["3", "6", "9"]);
    }

    #[test]
    fn module_function_as_value_via_eta_backends_agree() {
        // (RFC-0050 Part 2) A bare `module.fn` in value position is a first-class
        // function value: the linker eta-expands `list.length` to a lambda of its
        // full declared arity. Exercised three ways the RFC's parity clause pins —
        // passed to `map`, bound with `let`, and stored in a record field — all
        // through the ordinary lambda path so both backends materialize the same
        // closure. `list.length` is generic (`List(a) -> Int`), so this also proves
        // RFC-0046's fixpoint infers the eta-lambda's type-var parameter.
        let client = r#"
import list

type Box:
    op: fn(List(Int)) -> Int

fn main(console: Console):
    let xs = [[1, 2], [3], [4, 5, 6]]
    let ys = list.map(xs, list.length)
    console.print("${ys}")
    let f = list.length
    console.print("${f([9, 9, 9, 9])}")
    let b = Box(list.length)
    console.print("${(b.op)([7, 7])}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "module-function-value diverged");
        assert_eq!(compiled, vec!["[2, 1, 3]", "4", "2"]);
    }

    #[test]
    fn module_function_alias_to_method_body_backends_agree() {
        // RFC-0050 follow-up: `list.map` is no longer a source-level function
        // whose body owns the implementation. `impl List(a).map` owns it, while
        // `list.map(xs, f)` and `list.map` as a value are compiler aliases to the
        // generated method implementation. That keeps function-value compatibility
        // without hand-written forwarding wrappers.
        let client = r#"
import list

fn inc(n: Int) -> Int:
    n + 1

fn main(console: Console):
    console.print("${list.map([1, 2], inc)}")
    console.print("${[3, 4].map(inc)}")
    let f = list.map
    console.print("${f([5, 6], inc)}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "module-method alias diverged");
        assert_eq!(compiled, vec!["[2, 3]", "[4, 5]", "[6, 7]"]);
    }

    #[test]
    fn module_function_fold_and_generic_mutator_eta_backends_agree() {
        // (RFC-0050 Part 2) A 2-arity module function as a `fold` reducer, both a
        // concrete one (`math.max`) and a GENERIC RFC-0043 mutator (`list.concat`,
        // whose `var` first parameter returns `self`, so it is NOT excluded — its
        // value form is a pure call). Confirms the arity-2 eta-lambda infers and
        // runs identically on both backends.
        let client = r#"
import list
import math

fn main(console: Console):
    let nums = [3, 7, 2, 9, 4]
    console.print("${list.fold(nums, 0, math.max)}")
    let xss = [[1, 2], [3], [4, 5, 6]]
    console.print("${list.fold(xss, [], list.concat)}")
"#;
        let sources = [
            ("list", crate::bundled_module("list").unwrap()),
            ("math", crate::bundled_module("math").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "fold-with-module-function diverged");
        assert_eq!(compiled, vec!["9", "[1, 2, 3, 4, 5, 6]"]);
    }

    #[test]
    fn module_function_value_uses_full_declared_arity_backends_agree() {
        // (RFC-0050 Part 2 × RFC-0056) A function VALUE ignores keyword-argument
        // defaults, so eta-expansion uses the FULL declared arity: `greeter.greet`
        // (whose second parameter has a constant default) becomes a two-parameter
        // lambda, and every positional argument must be supplied. A cross-module
        // reference, so it also exercises the imported-module value path.
        let greeter = "pub fn greet(name: String, greeting: String = \"Hello\") -> String:\n    \"${greeting}, ${name}\"\n";
        let client = r#"
import greeter
import list

fn main(console: Console):
    let g = greeter.greet
    let out = list.map(["Ada", "Bel"], fn(nm): g(nm, "Hi"))
    for s in out:
        console.print(s)
"#;
        let sources = [("greeter", greeter), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "default-arity module-function value diverged");
        assert_eq!(compiled, vec!["Hi, Ada", "Hi, Bel"]);
    }

    #[test]
    fn module_function_typo_in_value_position_names_the_module() {
        // (RFC-0050 Part 2) When the base of a `module.fn` value reference names an
        // in-scope module (here the `list` prelude), a wrong function name reuses
        // the call-position diagnostic — "unbound variable `list`" never appears.
        let src = "import list\n\nfn main():\n    let f = list.lenght\n    f\n";
        let module = parser::parse_module(src).expect("parse");
        let err = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect_err("typo'd module function must be a link error");
        assert!(format!("{err}").contains("module `list` has no function `lenght`"), "{err}");
    }

    #[test]
    fn immediate_application_backends_agree() {
        let src = r#"
fn twice(f: fn(Int) -> Int, x: Int) -> Int:
    f(f(x))

fn main(console: Console):
    let make_adder = fn(x: Int): fn(y: Int): (x + y)
    let make_mul = fn(a: Int): fn(b: Int): fn(c: Int): ((a * b) * c)
    console.print("${(make_adder(10))(5)}")
    console.print("${((make_mul(2))(3))(4)}")
    console.print("${(fn(n: Int): (n * n))(7)}")
    console.print("${twice(make_adder(1), 10)}")
    console.print("${(make_adder(10))((make_adder(2))(3))}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "immediate application diverged");
        assert_eq!(run_on_wasm(src), vec!["15", "24", "49", "12", "15"]);
    }
