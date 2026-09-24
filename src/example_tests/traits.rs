use super::*;
use crate::{interpreter, parser, typeck};

    // Traits over a user ADT: the receiver type comes from the constructor, and
    // the impl body matches on `self`. Both backends agree.
    #[test]
    fn traits_dispatch_on_adt_backends_agree() {
        let src = r#"
type Shape:
    Circle(Int)
    Square(Int)

trait Area:
    fn area(self) -> Int

impl Area for Shape:
    fn area(self) -> Int:
        match self:
            Circle(r) -> ((r * r) * 3)
            Square(s) -> (s * s)

fn main(console: Console):
    console.print("${area(Circle(2))}")
    console.print("${area(Square(3))}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "trait ADT dispatch diverged");
        assert_eq!(run_on_wasm(src), vec!["12", "9"]);
    }

    // Default trait methods: a method with a body in the trait is inherited by
    // impls that don't define it (calling the impl's other methods on `self`),
    // and can be overridden. Both backends agree.
    #[test]
    fn traits_default_methods_backends_agree() {
        let src = r#"
trait Label:
    fn tag(self) -> String
    fn shout(self) -> String:
        (tag(self) + "!")

impl Label for Int:
    fn tag(self) -> String:
        "int"

impl Label for Bool:
    fn tag(self) -> String:
        "bool"

    fn shout(self) -> String:
        "BOOL!!"

fn main(console: Console):
    console.print(shout(5))
    console.print(shout(true))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "trait default-method diverged");
        assert_eq!(run_on_wasm(src), vec!["int!", "BOOL!!"]);
    }

    // Cross-module traits: a trait and its impls defined in one module are used
    // from another that imports it. Desugaring runs after linking, so the
    // generated methods and their call sites resolve across the flat merged
    // namespace. Both backends agree.
    #[test]
    fn traits_cross_module_backends_agree() {
        let show_mod = r#"
trait Describe:
    fn describe(self) -> String

impl Describe for Int:
    fn describe(self) -> String:
        "${self}"

impl Describe for Bool:
    fn describe(self) -> String:
        if self:
            "Y"
        else:
            "N"
"#;
        let app = r#"
import show_mod

fn main(console: Console):
    console.print(describe(42))
    console.print(describe(false))
"#;
        let sources = [("show_mod", show_mod), ("app", app)];
        let interpreted = interpreter::run_program(&sources, "app").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "app");
        assert_eq!(interpreted, compiled, "cross-module trait diverged");
        assert_eq!(compiled, vec!["42", "N"]);
    }

    // The comparison OPERATORS (`== != < > <= >=`) desugar through the derived
    // PartialEq/PartialOrd impls of a user record — no named `eq`/`less` call —
    // and both backends agree. Also covers the `Ordering` result of `compare`,
    // `cmp.reverse`, and `list.sort` over the user type.
    #[test]
    fn comparison_operators_dispatch_on_user_types() {
        let src = "import cmp\n\ntype Coord derive(PartialEq, Eq, PartialOrd, Ord):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let a = Coord(1, 2)\n    let b = Coord(1, 5)\n    console.print(\"${a == a} ${a == b} ${a != b}\")\n    console.print(\"${a < b} ${b > a} ${a <= a} ${b >= b}\")\n    console.print(\"${compare(a, b)}\")\n    console.print(\"${cmp.reverse(compare(a, b))}\")\n    var coords = [Coord(2, 0), Coord(1, 9), Coord(1, 1)]\n    list.sort(coords)\n    console.print(\"${coords}\")\n";
        let want: Vec<String> = [
            "true false true",
            "true true true true",
            "Less",
            "Greater",
            "[Coord(1, 1), Coord(1, 9), Coord(2, 0)]",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    // `Float` implements `PartialEq` + `PartialOrd` only (NaN is unequal to itself
    // and unordered), so the operators work but an `Ord`-bounded helper rejects
    // `List(Float)` at check time — Float is not totally ordered.
    #[test]
    fn float_is_partial_ord_not_ord() {
        let ok = "import cmp\n\nfn main(console: Console):\n    console.print(\"${1.5 < 2.5} ${2.5 == 2.5} ${2.5 != 1.5}\")\n";
        assert_eq!(link_run(ok), vec!["true true true".to_string()], "Float PartialOrd works");

        let bad = "import cmp\n\nfn main(console: Console):\n    var floats = [3.0, 1.0, 2.0]\n    list.sort(floats)\n    console.print(\"${floats}\")\n";
        let module = parser::parse_module(bad).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("Float is not Ord — list.sort must reject it").message;
        assert!(err.contains("Ord"), "error should mention Ord: {err}");
    }

    // A user supertrait hierarchy (`trait Derived: Base`): a `where a: Derived`
    // bound discharges the SUPERTRAIT's methods too, so the body calls both
    // `base` (declared on `Base`) and `derived`. Both backends agree.
    #[test]
    fn supertrait_methods_resolve_through_bound() {
        let src = "trait Base:\n    fn base(self) -> Int\n\ntrait Derived: Base:\n    fn derived(self) -> Int\n\ntype W:\n    W(Int)\n\nimpl Base for W:\n    fn base(self) -> Int:\n        match self:\n            W(n) -> n\n\nimpl Derived for W:\n    fn derived(self) -> Int:\n        match self:\n            W(n) -> n * 2\n\nfn use_it(x: a) -> Int where a: Derived:\n    base(x) + derived(x)\n\nfn main(console: Console):\n    console.print(\"${use_it(W(5))}\")\n";
        let want = vec!["15".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");

        // Omitting the supertrait impl is a loud check error.
        let bad = "trait Base:\n    fn base(self) -> Int\n\ntrait Derived: Base:\n    fn derived(self) -> Int\n\ntype W:\n    W(Int)\n\nimpl Derived for W:\n    fn derived(self) -> Int:\n        match self:\n            W(n) -> n\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let module = parser::parse_module(bad).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("missing supertrait impl must be rejected").message;
        assert!(err.contains("Base"), "error should name the missing supertrait: {err}");
    }

