use super::*;
use crate::{codegen, interpreter, parser};

    /// (RFC-0052) `let` destructuring — nested tuples AND a single-variant record
    /// pattern — the same grammar as `match`, both backends.
    #[test]
    fn let_destructure_patterns_backends_agree() {
        let src = "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let ((a, b), c) = ((1, 2), 3)\n    console.print(\"${a} ${b} ${c}\")\n    let Point(px, py) = Point(10, 20)\n    console.print(\"${px} ${py}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["1 2 3", "10 20"]);
    }

    /// Structural `==` on sum types: nullary enums and concrete-field variants
    /// compare by tag (then by the matched variant's fields) on both backends.
    /// (Regression for the silent ADT pointer-compare divergence.)
    #[test]
    fn adt_structural_equality_agrees_on_both_backends() {
        let src = "type Color:\n    Red\n    Green\n    Blue\ntype Shape:\n    Circle(Int)\n    Square(Int)\nfn main(console: Console):\n    console.print(\"${Red == Red}\")\n    console.print(\"${Red == Blue}\")\n    console.print(\"${Circle(3) == Circle(3)}\")\n    console.print(\"${Circle(3) == Circle(4)}\")\n    console.print(\"${Circle(3) == Square(3)}\")\n    console.print(\"${[Red, Green] == [Red, Green]}\")\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
            "true".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A record SPREAD (`Point(x: 5, ..p)`) is validated exactly like plain
    /// construction: the named type must be a record, every override field declared,
    /// and none repeated. Skipping this let a repeated override reach the backends,
    /// where they disagreed on which wins (interpreter last, compiled first) — a
    /// silent divergence — and let an unknown type name through. A valid spread still
    /// links and runs identically on both backends.
    #[test]
    fn record_spread_rejects_duplicate_and_unknown_fields() {
        let link_err = |body: &str| -> String {
            let src = format!(
                "type Point:\n    x: Int\n    y: Int\nfn main(console: Console):\n    let p = Point(x: 1, y: 2)\n{body}"
            );
            let m = parser::parse_module(&src).expect("parse");
            crate::pipeline::link(vec![("main".into(), m)], "main")
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
        };
        assert!(
            link_err("    let q = Point(x: 7, x: 8, ..p)\n    console.print(\"${q.x}\")\n")
                .contains("set twice"),
            "a repeated override field in a spread must be rejected",
        );
        assert!(
            link_err("    let q = Bogus(x: 9, ..p)\n    console.print(\"${q.x}\")\n")
                .contains("not a record type"),
            "a spread over an unknown type name must be rejected",
        );
        let ok = "type Point:\n    x: Int\n    y: Int\nfn main(console: Console):\n    let p = Point(x: 1, y: 2)\n    let q = Point(x: 7, ..p)\n    console.print(\"${q.x}\" + \" \" + \"${q.y}\")\n";
        assert_eq!(link_run(ok), vec!["7 2"], "interpreter");
        assert_eq!(wasm_run(ok), vec!["7 2"], "wasm");
    }

    // Nested field access and record update preserve the other fields on both backends.
    #[test]
    fn nested_records_and_update_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Line:
    from: Point
    to: Point

fn main(console: Console):
    let l = Line(Point(1, 2), Point(3, 4))
    console.print("${((l).from).x}")
    console.print("${((l).to).y}")
    let l2 = Line(from: Point(10, 20), ..l)
    console.print("${((l2).from).x}")
    console.print("${((l2).to).y}")
    console.print("${((l).from).x}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "nested records / update diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "4", "10", "4", "1"]);
    }

    /// REGRESSION (BUG-253): `xs.sort()` dispatches through `Ord`, so a list of
    /// derived-`Ord` records sorts (it used to fail 'expected Int' by binding the
    /// Int-only `list.sort`); Ints still sort. Identical on both backends.
    #[test]
    fn list_sort_orders_records_through_ord_backends_agree() {
        let src = "import list\ntype V derive(PartialEq, Eq, PartialOrd, Ord):\n    major: Int\n    minor: Int\nfn main(console: Console):\n    var values = [V(3, 1), V(1, 2), V(2, 0)]\n    values.sort()\n    for v in values:\n        console.print(\"${v.major}\" + \".\" + \"${v.minor}\")\n    var ints = [3, 1, 2, 5]\n    ints.sort()\n    console.print(\"${ints}\")\n";
        let expected = ["1.2", "2.0", "3.1", "[1, 2, 3, 5]"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }
