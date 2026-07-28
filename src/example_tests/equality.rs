use super::*;
use crate::{codegen, interpreter, parser, typeck};

    /// (BUG-553) Container comparison protocols compose: nested `Option` and
    /// `Result` values satisfy `PartialEq`/`Eq` when their payloads do, and
    /// compiled monomorphization specializes the nested payload calls.
    #[test]
    fn nested_container_equality_satisfies_protocol_bounds_on_both_backends() {
        let src = "import cmp\nimport testing\n\ntype Key derive(Show, Eq):\n    id: Int\n    cache: Int\n\nimpl PartialEq for Key:\n    fn eq(self, other: Key) -> Bool:\n        self.id == other.id\n\nfn same(x: a, y: a) -> Bool where a: PartialEq:\n    x == y\n\nfn total_same(x: a, y: a) -> Bool where a: Eq:\n    x == y\n\nfn main(console: Console):\n    let o1: Option(List(Key)) = Some([Key(1, 10)])\n    let o2: Option(List(Key)) = Some([Key(1, 20)])\n    let r1: Result(List(Key), String) = Ok([Key(1, 10)])\n    let r2: Result(List(Key), String) = Ok([Key(1, 20)])\n    console.print(\"${same(o1, o2)}\")\n    console.print(\"${total_same(o1, o2)}\")\n    console.print(\"${same(r1, r2)}\")\n    console.print(\"${total_same(r1, r2)}\")\n    testing.assert_value_eq(o1, o2)\n    testing.assert_value_eq(r1, r2)\n";
        let expected = ["true", "true", "true", "true"];
        assert_eq!(link_run(src), expected, "interp: nested container PartialEq bounds");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: nested container PartialEq bounds",
        );
    }

    /// (RFC-0047) `==` on a function type is a compile-time error — there is no
    /// stable equality for functions (identity is a monomorphization/inlining
    /// accident), and comparing them was a confirmed backend parity divergence
    /// (interpreter name-compares `true`, compiled pointer-compares `false`).
    /// Rejecting deletes the divergence by construction. Both the direct case and
    /// the container/tuple case must error with a teaching message.
    #[test]
    fn function_equality_is_a_compile_error() {
        let direct = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    console.print(\"${f == f}\")\n";
        let e = typeck::check_str(direct).expect_err("`f == f` must be rejected");
        assert!(e.contains("not defined on function types"), "teaching error, got: {e}");
        // Nested inside a container is caught the same way (depth-uniform).
        let in_list = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    console.print(\"${[f] == [f]}\")\n";
        let el = typeck::check_str(in_list).expect_err("`[f] == [f]` must be rejected");
        assert!(el.contains("not defined on function types"), "teaching error, got: {el}");
        let in_tuple = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    console.print(\"${(f, 1) == (f, 1)}\")\n";
        assert!(
            typeck::check_str(in_tuple).expect_err("`(f, 1) == (f, 1)` must be rejected")
                .contains("not defined on function types"),
            "a function nested in a tuple must be rejected too"
        );
    }

    /// (RFC-0047) A realistic custom equality — case-insensitive strings — honored
    /// through containers on both backends. `CI("Hi") == CI("hi")` and the same
    /// inside a `List`/`Option` are `true`; genuinely different values are `false`.
    #[test]
    fn case_insensitive_custom_eq_through_containers() {
        let src = "\ntype CI:\n    CI(String)\n\nimpl PartialEq for CI:\n    fn eq(self, other: CI) -> Bool:\n        match self:\n            CI(a) -> match other:\n                CI(b) -> a.to_lower() == b.to_lower()\n\nfn main(console: Console):\n    console.print(\"${CI(\"Hello\") == CI(\"hello\")}\")\n    console.print(\"${[CI(\"Hi\"), CI(\"YO\")] == [CI(\"hi\"), CI(\"yo\")]}\")\n    console.print(\"${Some(CI(\"Ab\")) == Some(CI(\"ab\"))}\")\n    console.print(\"${CI(\"x\") == CI(\"y\")}\")\n";
        let want = vec![
            "true".to_string(),
            "true".to_string(),
            "true".to_string(),
            "false".to_string(),
        ];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), want, "compiled WASM must agree");
    }

    /// A MULTI-parameter generic ADT (`Result`, whose Ok and Err payloads are
    /// different type variables) is structural on both backends: payloads pin
    /// from constructor literals (the variant's own variables must unify with
    /// its arguments; the other variant's take a safe placeholder), from
    /// declared parameter types, and from declared function returns. (Closes
    /// the last loud equality gap.)
    #[test]
    fn result_equality_agrees_on_both_backends() {
        let src = "import result\n\nfn classify(n: Int) -> Result(Int, String):\n    if n >= 0: Ok(n) else: Err(\"negative\")\n\nfn same(a: Result(Int, String), b: Result(Int, String)) -> Bool:\n    a == b\n\nfn main(console: Console):\n    let xs: Result(List(Int), String) = Ok([1, 2])\n    let xs_same: Result(List(Int), String) = Ok([1, 2])\n    let xs_diff: Result(List(Int), String) = Ok([1, 3])\n    console.print(\"${classify(5) == Ok(5)}\")\n    console.print(\"${classify(5) == Ok(6)}\")\n    console.print(\"${classify(0 - 1) == Err(\"negative\")}\")\n    console.print(\"${classify(0 - 1) == Err(\"positive\")}\")\n    console.print(\"${classify(5) == Err(\"negative\")}\")\n    console.print(\"${same(Ok(1), Ok(1))}\")\n    console.print(\"${same(Err(\"a\"), Err(\"a\"))}\")\n    console.print(\"${same(Ok(1), Err(\"a\"))}\")\n    console.print(\"${xs == xs_same}\")\n    console.print(\"${xs == xs_diff}\")\n";
        let want: Vec<String> =
            ["true", "false", "true", "false", "false", "true", "true", "false", "true", "false"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// A RECURSIVE generic ADT (`Stack(a)`, whose `Push` carries a `Stack(a)`)
    /// compares structurally on both backends: the shape is identified by its
    /// type arguments and the generated helper calls itself for the
    /// self-referential field — deep spines compare by content, through
    /// literals, declared parameter types, and nullary constructors.
    #[test]
    fn recursive_generic_adt_equality_agrees_on_both_backends() {
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\nfn same(s: Stack(Int), t: Stack(Int)) -> Bool:\n    s == t\n\nfn main(console: Console):\n    console.print(\"${Push(2, Push(1, Empty)) == Push(2, Push(1, Empty))}\")\n    console.print(\"${Push(2, Push(1, Empty)) == Push(2, Push(9, Empty))}\")\n    console.print(\"${Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"a\", Empty))}\")\n    console.print(\"${Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"z\", Empty))}\")\n    console.print(\"${same(Push(1, Empty), Push(1, Empty))}\")\n    console.print(\"${same(Push(1, Empty), Empty)}\")\n";
        let want: Vec<String> = ["true", "false", "true", "false", "true", "false"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// The boundary of structural equality stays LOUD where the payload is
    /// genuinely unresolvable. Return-position inference resolves non-empty list
    /// payloads and compares them by element. An empty-list payload still has no
    /// element evidence, so the checked pipeline rejects it before codegen —
    /// never silently pointer-comparing.
    #[test]
    fn unsupported_compound_equality_is_a_loud_error_not_silent() {
        let resolved = "import result\n\nfn wrap(x: a) -> Result(a, String):\n    Ok(x)\n\nfn main(console: Console):\n    console.print(\"${wrap([1]) == wrap([2])}\")\n";
        assert_eq!(interp(resolved), vec!["false"]);
        assert_eq!(wasm_run(resolved), vec!["false"], "backends agree");
        let empty = "import result\n\nfn wrap(x: a) -> Result(a, String):\n    Ok(x)\n\nfn main(console: Console):\n    console.print(\"${wrap([]) == wrap([])}\")\n";
        let rm = parser::parse_module(empty).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), rm)], "main").expect("link");
        assert!(
            typeck::check(&linked).is_err(),
            "an empty generic payload must stay a loud checked-pipeline error"
        );
    }

    /// Ordering a NaN must FAIL on both backends, not silently return IEEE false
    /// on WASM. The interpreter errors ("cannot compare NaN"); the compiled
    /// `<`/`<=`/`>`/`>=` on floats route through a NaN-trapping helper. Equality
    /// (`==`) is IEEE on both (NaN == NaN is false) and still agrees. Ordinary
    /// float ordering is unchanged. (Regression for a silent NaN-ordering
    /// divergence.)
    #[test]
    fn nan_ordering_errors_on_both_backends() {
        for cmp in ["nan < 1.0", "nan > 1.0", "nan <= nan", "nan >= 0.0"] {
            let src = format!(
                "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(\"${{{cmp}}}\")\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let bytes = codegen::compile_module_binary(&module)
                .expect_lowered("the binary path lowers this program");
            assert!(interpreter::run(&src).is_err(), "interpreter must error on `{cmp}`");
            assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on `{cmp}`");
        }
        // Ordinary float ordering and NaN equality still agree.
        let ok = "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(\"${1.5 < 2.5}\")\n    console.print(\"${2.5 <= 2.5}\")\n    console.print(\"${nan == nan}\")\n";
        let want = vec!["true".to_string(), "true".to_string(), "false".to_string()];
        assert_eq!(interp(ok), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(ok), want, "compiled WASM must agree");
    }

    #[test]
    fn std_eq_member_backends_agree() {
        // The Eq trait + the bounded list `contains` / `index_of` give content-correct
        // equality on BOTH backends — even for runtime-BUILT strings, where a
        // generic `==` search does pointer comparison in compiled code and would
        // wrongly miss. A user `impl Eq` (Box) works, as does the default `ne`.
        let client = r#"
import list

type Box:
    Box(Int)

impl PartialEq for Box:
    fn eq(self, other: Self) -> Bool:
        match self:
            Box(a) -> match other:
                Box(b) -> (a == b)

impl Eq for Box

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < s.char_count()):
        acc = (acc + s.substring(i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("apple"), build("banana")]
    console.print("${list.contains(words, build("banana"))}")
    console.print("${list.contains(words, build("cherry"))}")
    console.print("${list.index_of([10, 20, 30], 20)}")
    console.print("${list.index_of([10, 20, 30], 99)}")
    console.print("${list.contains([Box(1), Box(2)], Box(2))}")
    console.print("${ne(Box(1), Box(2))}")
    console.print("${ne(Box(2), Box(2))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std eq member/index_of diverged");
        assert_eq!(
            compiled,
            vec!["true", "false", "Some(1)", "None", "true", "true", "false"]
        );
    }

    #[test]
    fn std_eq_count_unique_backends_agree() {
        // `list.count` / `list.unique` dispatch through the element type's Eq impl, so
        // they are content-correct on BOTH backends — including runtime-built
        // strings and user `impl Eq` types (Tag).
        let client = r#"
import list

type Tag:
    Tag(Int)

impl PartialEq for Tag:
    fn eq(self, other: Self) -> Bool:
        match self:
            Tag(a) -> match other:
                Tag(b) -> (a == b)

impl Eq for Tag

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < s.char_count()):
        acc = (acc + s.substring(i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("a"), build("b"), build("a"), build("c"), build("b"), build("a")]
    console.print("${list.count(words, build("a"))}")
    console.print("${list.count(words, build("z"))}")
    console.print(list.join(list.unique(words), ","))
    console.print("${list.length(list.unique([Tag(1), Tag(2), Tag(1), Tag(2), Tag(3)]))}")
    console.print("${list.count([Tag(1), Tag(2), Tag(1)], Tag(1))}")
"#;
        let sources = [
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std eq count/unique diverged");
        assert_eq!(compiled, vec!["3", "0", "a,b,c", "3", "2"]);
    }

    /// `==` on a compound whose *slots are themselves compound* must agree on
    /// both backends, whether the operands are `let`-bound or parameters. WASM
    /// previously returned `None` for the shape of such a binding and fell back to
    /// a pointer compare — a SILENT divergence (interpreter `true`, compiled
    /// `false`). The shape is now captured from the binding/declared type.
    #[test]
    fn nested_compound_equality_agrees_on_both_backends() {
        let src = "fn same(a: (List(Int), List(Int)), b: (List(Int), List(Int))) -> Bool:\n    a == b\nfn main(console: Console):\n    let v = ([1, 2], (3, 4))\n    let w = ([1, 2], (3, 4))\n    console.print(\"${v == w}\")\n    console.print(\"${same(([1], [2]), ([1], [2]))}\")\n    console.print(\"${same(([1], [2]), ([1], [9]))}\")\n";
        let want = vec!["true".to_string(), "true".to_string(), "false".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }
