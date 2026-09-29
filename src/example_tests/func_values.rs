use super::*;
use crate::parser;

    #[test]
    fn named_function_passed_to_map_golden_output() {
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
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(compiled, vec!["3", "6", "9"]);
    }

    #[test]
    fn module_function_as_value_via_eta_golden_output() {
        // (RFC-0050 Part 2) A bare `module.fn` in value position is a first-class
        // function value: the linker eta-expands `list.length` to a lambda of its
        // full declared arity. Exercised three ways the RFC's parity clause pins —
        // passed to `map`, bound with `let`, and stored in a record field — all
        // through the ordinary lambda path. `list.length` is generic
        // (`List(a) -> Int`), so this also proves RFC-0046's fixpoint infers
        // the eta-lambda's type-var parameter.
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
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(compiled, vec!["[2, 1, 3]", "4", "2"]);
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
