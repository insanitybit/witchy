use super::*;
use crate::{codegen, interpreter, parser};

    /// RFC-0005 Stage 4 / BUG-566: capability-carrying named and positional
    /// records exercise construction, spread, nesting, destructuring, and
    /// assignment across both backends without crossing the i64 slot.
    #[test]
    fn plain_cap_record_runs_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_caprecord_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("greeting.txt"), "hello-record").expect("seed");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = "type Inner:\n    dir: Dir[Read]\n    tag: String\n\ntype Workspace:\n    inner: Inner\n    label: String\n    count: Int\n\ntype RootToken:\n    RootToken(Dir[Read])\n\ntype RootHandle:\n    RootHandle(RootToken, String)\n\ntype NamedAroundPositional:\n    token: RootToken\n    name: String\n\ntype PositionalAroundNamed:\n    PositionalAroundNamed(Inner, String)\n\nfn load(w: Workspace, name: String) -> String:\n    w.inner.dir.read(name)\n\nfn load_positional(h: RootHandle) -> String:\n    match h:\n        RootHandle(RootToken(dir), name) -> dir.read(name)\n\nfn load_named_positional(h: NamedAroundPositional) -> String:\n    match h.token:\n        RootToken(dir) -> dir.read(h.name)\n\nfn load_positional_named(h: PositionalAroundNamed) -> String:\n    match h:\n        PositionalAroundNamed(inner, name) -> inner.dir.read(name)\n\nfn relabel(w: Workspace, label: String) -> Workspace:\n    Workspace(label: label, ..w)\n\nfn main(console: Console, root: Dir[Read]):\n    let w = Workspace(Inner(root, \"t\"), \"main\", 1)\n    console.print(load(w, \"greeting.txt\"))\n    console.print(load_positional(RootHandle(RootToken(root), \"greeting.txt\")))\n    console.print(load_named_positional(NamedAroundPositional(RootToken(root), \"greeting.txt\")))\n    console.print(load_positional_named(PositionalAroundNamed(Inner(root, \"v\"), \"greeting.txt\")))\n    let x = relabel(w, \"alt\")\n    console.print(\"${x.label} ${x.count}\")\n    var y = Workspace(inner: Inner(root, \"u\"), label: \"named\", count: 2)\n    y.count = 40 + y.count\n    console.print(\"${y.label} ${y.count}\")\n    match y:\n        Workspace(i, lab, n) -> console.print(\"${lab} ${n} ${i.tag}\")\n";
        let want = vec![
            "hello-record".to_string(),
            "hello-record".to_string(),
            "hello-record".to_string(),
            "hello-record".to_string(),
            "alt 1".to_string(),
            "named 42".to_string(),
            "named 42 u".to_string(),
        ];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers plain cap-carrying records");
        let mut rt = Runtime::batch().expect("runtime");
        let caps = Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        };
        let mut actor = rt.spawn(&bin, caps, 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Tuple patterns in `for` (the learning log's F4): `for (k, v) in
    /// dict.pairs(d):` destructures per element, round-trips through fmt,
    /// and agrees on both backends.
    #[test]
    fn for_tuple_patterns_destructure() {
        // Parenthesized and canonical unparenthesized tuple patterns agree.
        let head = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n";
        let paren = format!("{head}    for (k, v) in dict.pairs(d):\n        console.print(\"${{k}}=${{v}}\")\n");
        let unparen = format!("{head}    for k, v in dict.pairs(d):\n        console.print(\"${{k}}=${{v}}\")\n");
        let want: Vec<String> = ["a=1", "b=2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(&paren), want, "interpreter (paren)");
        assert_eq!(wasm_run(&paren), want, "wasm (paren)");
        assert_eq!(link_run(&unparen), want, "interpreter (unparen)");
        assert_eq!(wasm_run(&unparen), want, "wasm (unparen)");
        // fmt canonicalizes to the unparenthesized form, which round-trips.
        assert_eq!(
            crate::format::reformat(&paren).as_deref(),
            Some(unparen.as_str()),
            "paren form canonicalizes to unparenthesized"
        );
        assert_eq!(
            crate::format::reformat(&unparen).as_deref(),
            Some(unparen.as_str()),
            "unparenthesized form round-trips through fmt"
        );
    }

    /// (RFC-0052) `let` destructuring — nested tuples AND a single-variant record
    /// pattern — the same grammar as `match`, both backends.
    #[test]
    fn let_destructure_patterns_backends_agree() {
        let src = "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let ((a, b), c) = ((1, 2), 3)\n    console.print(\"${a} ${b} ${c}\")\n    let Point(px, py) = Point(10, 20)\n    console.print(\"${px} ${py}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["1 2 3", "10 20"]);
    }

    /// Generic stdlib equality over user records is structural and monomorphizes
    /// through the `where a: Eq` bound on `list.contains` and `index_of`.
    #[test]
    fn generic_equality_on_records_is_structural() {
        let src = "import list\nimport cmp\n\ntype Point derive(PartialEq, Eq):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let pts = [Point(1, 2), Point(3, 4)]\n    let probe = Point(1 + 2, 4)\n    console.print(\"${list.contains(pts, probe)}\")\n    console.print(\"${list.index_of(pts, Point(1, 2)) ?? -1}\")\n";
        let want: Vec<String> = ["true", "0"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Structural `==`/`!=` on compound values must agree across both backends;
    /// codegen derives `EqShape` and uses generated structural-equality helpers.
    #[test]
    fn structural_equality_agrees_on_both_backends() {
        let src = "type Pt:\n    x: Int\n    y: Int\ntype Bag:\n    items: List(Int)\nfn main(console: Console):\n    console.print(\"${[1, 2, 3] == [1, 2, 3]}\")\n    console.print(\"${[1, 2, 3] == [1, 9, 3]}\")\n    console.print(\"${[[1], [2]] == [[1], [2]]}\")\n    console.print(\"${(1, \"a\") == (1, \"a\")}\")\n    console.print(\"${(1, \"a\") != (1, \"b\")}\")\n    console.print(\"${Pt(1, 2) == Pt(1, 2)}\")\n    console.print(\"${Pt(1, 2) == Pt(3, 4)}\")\n    console.print(\"${[Pt(1, 2)] == [Pt(1, 2)]}\")\n    console.print(\"${Bag([1, 2]) == Bag([1, 2])}\")\n    console.print(\"${[\"a\", \"b\"] == [\"a\", \"b\"]}\")\n";
        let want = vec![
            "true".to_string(),  // [1,2,3] == [1,2,3]
            "false".to_string(), // [1,2,3] == [1,9,3]
            "true".to_string(),  // nested lists
            "true".to_string(),  // tuple ==
            "true".to_string(),  // tuple != (differs)
            "true".to_string(),  // record ==
            "false".to_string(), // record == (differs)
            "true".to_string(),  // list of records
            "true".to_string(),  // record with a List field
            "true".to_string(),  // list of strings
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
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

    /// Record field access corpus with shared parity setup and labeled expected values.
    #[test]
    fn record_field_access_shapes_agree_on_both_backends() {
        let cases: &[(&str, &[&str], bool)] = &[
            (
                "interpolation",
                &["hi (9): [1, 2, 3]"],
                false,
            ),
            (
                "direct expressions",
                &["7", "10", "4"],
                true,
            ),
            (
                "conditional and match",
                &["30", "10", "2", "5"],
                true,
            ),
            (
                "list records",
                &["30", "8", "9"],
                true,
            ),
            (
                "dict records",
                &["30", "0"],
                true,
            ),
        ];
        let sources: &[&str] = &[
            "type Post:\n    title: String\n    views: Int\n    tags: List(Int)\nfn main(console: Console):\n    let p = Post(\"hi\", 9, [1, 2, 3])\n    console.print(\"${p.title} (${p.views}): ${p.tags}\")\n",
            r#"
type Item:
    price: Int
    qty: Int

fn lookup(b: Bool) -> Item:
    if b:
        Item(3, 10)
    else:
        Item(5, 2)
fn main(console: Console):
    console.print("${(Item(7, 6)).price}")
    console.print("${(lookup(true)).qty}")
    let items = [Item(1, 2), Item(3, 4)]
    console.print("${(list.at(items, 1)).qty}")
"#,
            r#"
type Item:
    price: Int
    qty: Int

fn pick(b: Bool) -> Int:
    let x = if b: Item(3, 10) else: Item(5, 2)
    ((x).price * (x).qty)
fn from_tag(t: Int) -> Int:
    let y = match t:
        0 -> Item(1, 1)
        _ -> Item(2, 3)
    ((y).price + (y).qty)

fn main(console: Console):
    console.print("${pick(true)}")
    console.print("${pick(false)}")
    console.print("${from_tag(0)}")
    console.print("${from_tag(9)}")
"#,
            r#"
type Item:
    price: Int
    qty: Int

fn first_value(items: List(Item)) -> Int:
    let first = list.at(items, 0)
    ((first).price * (first).qty)
fn main(console: Console):
    console.print("${first_value([Item(3, 10), Item(5, 2)])}")
    let items = [Item(2, 4), Item(7, 1)]
    let second = list.at(items, 1)
    console.print("${((second).price + (second).qty)}")
    var total = 0
    for it in items:
        total = (total + (it).price)
    console.print("${total}")
"#,
            r#"
type Item:
    price: Int
    qty: Int
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, "apple", Item(3, 10))
    dict.insert(d, "bread", Item(2, 5))
    let it = dict.get_or(d, "apple", Item(0, 0))
    console.print("${((it).price * (it).qty)}")
    let missing = dict.get_or(d, "milk", Item(0, 0))
    console.print("${(missing).price}")
"#,
        ];
        for ((label, expected, parity), src) in cases.iter().zip(sources) {
            let compiled = run_on_wasm(src);
            let want: Vec<String> = expected.iter().map(|value| (*value).to_string()).collect();
            assert_eq!(compiled, want, "{label}: compiled output");
            if *parity {
                assert_eq!(interp(src), compiled, "{label}: backend parity");
            }
        }
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

    // Record fields work in comprehension expressions and filters on both backends.
    #[test]
    fn list_comprehension_over_records_backends_agree() {
        let client = r#"
import list
type Item:
    name: String
    qty: Int
fn main(console: Console):
    let cart = [Item("apple", 3), Item("bread", 1), Item("milk", 2)]
    let multi = [it.name for it in cart if it.qty > 1]
    for n in multi:
        console.print(n)
    let qtys = [it.qty * 10 for it in cart]
    for q in qtys:
        console.print("${q}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "comprehension over records diverged");
        assert_eq!(compiled, vec!["apple", "milk", "30", "10", "20"]);
    }

    // Record update accepts field and conditional expression bases, including nesting.
    #[test]
    fn record_update_on_expression_base_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Line:
    from: Point
    to: Point

fn main(console: Console):
    let l = Line(Point(1, 2), Point(3, 4))
    let p2 = Point(x: 100, ..(l).from)
    console.print("${(p2).x}")
    console.print("${(p2).y}")
    let cond = true
    let p3 = Point(y: 99, ..(if cond: (l).from else: (l).to))
    console.print("${(p3).x}")
    console.print("${(p3).y}")
    let l2 = Line(from: Point(x: 7, ..(l).to), ..l)
    console.print("${((l2).from).x}")
    console.print("${((l2).from).y}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "record update on expression base diverged");
        assert_eq!(run_on_wasm(src), vec!["100", "2", "1", "99", "7", "4"]);
    }

    // Iteration resolves record fields from calls and list literals on both backends.
    #[test]
    fn for_over_nonvar_record_list_backends_agree() {
        let src = r#"
type P:
    x: Int
    y: Int

fn mk() -> List(P):
    [P(1, 2), P(3, 4), P(5, 6)]

fn main(console: Console):
    for p in mk():
        console.print("${((p).x + (p).y)}")
    for q in [P(10, 1), P(20, 2)]:
        console.print("${(q).x}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "for over non-var record list diverged");
        assert_eq!(run_on_wasm(src), vec!["3", "7", "11", "10", "20"]);
    }

    #[test]
    fn record_with_collection_field_backends_agree() {
        // Exercise collection and string fields, including iteration over a field.
        let src = r#"
type Bag:
    items: List(Int)
    label: String

fn main(console: Console):
    let b = Bag([10, 20, 30], "nums")
    console.print((b).label)
    console.print("${list.length((b).items)}")
    var total = 0
    for x in (b).items:
        total = (total + x)
    console.print("${total}")
    console.print("${list.at((b).items, 1)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["nums", "3", "60", "20"]);
    }

    #[test]
    fn inferred_and_mixed_generic_record_fields_agree_on_both_backends() {
        let src = r#"
type Boxed:
    value: a

type Mixed(a):
    first: a
    second: b

fn main(console: Console):
    let number: Boxed(Int) = Boxed(42)
    let text: Boxed(String) = Boxed("boxed")
    let mixed: Mixed(Int, String) = Mixed(7, "mixed")
    console.print("${number.value}")
    console.print(text.value)
    console.print("${mixed.first}-${mixed.second}")
"#;
        let expected = vec!["42", "boxed", "7-mixed"];
        assert_eq!(interp(src), expected, "interpreter");
        assert_eq!(run_on_wasm(src), expected, "compiled WASM");
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
