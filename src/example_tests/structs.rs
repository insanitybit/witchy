use super::*;
use crate::{codegen, interpreter, parser, typeck};

    /// RFC-0005 Stage 4 (records slice): plain named-field and positional
    /// nominal aggregates may carry a migrated capability, lowered to typed GC
    /// structs. Construction, spread, field access through a nested record
    /// chain, `match` destructuring, and `var` place assignment all agree between
    /// the backends, and the authority never crosses the i64 slot. Nesting is
    /// also the BUG-566 regression: the
    /// classifier lives in one home now, so typeck and codegen cannot disagree
    /// about which records GC-lower (the old codegen copy missed nested records
    /// and ICE'd the encoder).
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
        // Both the parenthesized and the unparenthesized (canonical, Python-style)
        // tuple patterns parse and run identically on both backends.
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

    /// Generic stdlib functions over USER RECORD types compare by content:
    /// typed lowering resolves the type argument (confirmed via the table),
    /// the specialization's `==` becomes structural. Previously the generic
    /// fallback pointer-compared (or, post-hotfix, refused to compile).
    /// RFC-0046 step 3: `list.contains`/`index_of` now carry a `where a: Eq`
    /// bound, so a record element type derives `Eq` (its content equality) to
    /// use them — which is exactly what makes them monomorphize on WASM.
    #[test]
    fn generic_equality_on_records_is_structural() {
        let src = "import list\nimport cmp\n\ntype Point derive(PartialEq, Eq):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let pts = [Point(1, 2), Point(3, 4)]\n    let probe = Point(1 + 2, 4)\n    console.print(\"${list.contains(pts, probe)}\")\n    console.print(\"${list.index_of(pts, Point(1, 2)) ?? -1}\")\n";
        let want: Vec<String> = ["true", "0"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-300) Field projection on a method-call-chain result compiles on both
    /// backends (`list.sort(xs).at(0).label`, `[..].at(0).label`) — the record type
    /// of the chain result comes from the type table, not a local-var-only map.
    #[test]
    fn field_projection_on_call_chain_backends_agree() {
        let src = "import cmp\n\ntype Top derive(Ord, PartialOrd, Eq, PartialEq):\n    label: String\n\nfn main(console: Console):\n    var xs = [Top(label: \"b\"), Top(label: \"a\")]\n    list.sort(xs)\n    console.print(xs.at(0).label)\n    console.print([Top(label: \"b\"), Top(label: \"a\")].at(0).label)\n    console.print(list.at([Top(label: \"b\"), Top(label: \"a\")], 0).label)\n";
        let want = ["a", "b", "b"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-318) Anonymous-record `==` and `"${…}"` work on both backends — the
    /// structural eq/render build the shape from the inline field types.
    #[test]
    fn anonymous_record_eq_and_show_backends_agree() {
        let src = "fn main(console: Console):\n    let a = .{x: 1, y: \"hi\"}\n    let b = .{x: 1, y: \"hi\"}\n    let c = .{x: 2, y: \"hi\"}\n    console.print(\"${a == b}\")\n    console.print(\"${a == c}\")\n    console.print(\"${a}\")\n";
        let want = ["true", "false", ".{x: 1, y: hi}"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Structural `==`/`!=` on compound values (lists, nested lists, tuples,
    /// records, lists of records) must agree on both backends. WASM previously
    /// compared heap POINTERS, so two equal-but-distinct values compared unequal;
    /// codegen now derives the operands' `EqShape` and routes through generated
    /// per-shape structural-equality helpers. (Regression for the silent
    /// compound-`==` pointer-compare divergence.)
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

    /// Interpolating a record field — `"${p.x}"` (scalar) and `"${p.tags}"`
    /// (compound) — renders on WASM, including inside a custom `Show` impl. A
    /// field access previously resolved to no value type, so `to_string` of it
    /// errored on the compiled backend even though the field's type is known.
    #[test]
    fn record_field_interpolation_renders_on_wasm() {
        let src = "type Post:\n    title: String\n    views: Int\n    tags: List(Int)\nfn main(console: Console):\n    let p = Post(\"hi\", 9, [1, 2, 3])\n    console.print(\"${p.title} (${p.views}): ${p.tags}\")\n";
        assert_eq!(run_on_wasm(src), vec!["hi (9): [1, 2, 3]".to_string()]);
    }

    #[test]
    fn brace_free_record_update_form() {
        // `update e: field = value ...` — brace-free record update (one or more
        // whitespace-separated `name = value` overrides). Both backends agree.
        let client = r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(1, 2)
    let q = Point(x: ((p).x + 10), ..p)
    console.print("${((q).x + (q).y)}")
    let r = Point(x: 5, y: 6, ..p)
    console.print("${((r).x + (r).y)}")
"#;
        assert_eq!(interp(client), vec!["13", "11"]);
        assert_eq!(run_on_wasm(client), vec!["13", "11"]);
    }

    /// A RecordUpdate whose base is a non-Var EXPRESSION on the binary path:
    /// `Point(x: 100, ..(l).from)` — the base `(l).from` (a field access) is
    /// evaluated ONCE into the `$TUPLE_TMP` scratch, base-first, so each
    /// un-updated field (`y`) reads it (was: the lowering required a Var base and
    /// bailed to WAT). Compared against the interpreter oracle.
    #[test]
    fn wir_record_update_expr_base_binary_path() {
        let src = "type Point:\n    x: Int\n    y: Int\n\ntype Line:\n    from: Point\n    to: Point\n\nfn main(console: Console):\n    let l = Line(Point(1, 2), Point(3, 4))\n    let p2 = Point(x: 100, ..(l).from)\n    console.print(\"${(p2).x}\")\n    console.print(\"${(p2).y}\")\n";
        let want = vec!["100".to_string(), "2".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower a RecordUpdate with an expression base");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    #[test]
    fn direct_field_access_on_expressions_backends_agree() {
        // Field access directly on a record-producing expression (no `let`): a
        // constructor literal, a record-returning call, and an `at` result.
        let src = r#"
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
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["7", "10", "4"]);
    }

    #[test]
    fn conditional_record_field_access_backends_agree() {
        // `let x = if c { A } else { B }; x.field` (and a match-bound record):
        // the binding's record type is recovered from the branch/arm.
        let src = r#"
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
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "10", "2", "5"]);
    }

    #[test]
    fn list_of_records_index_access_backends_agree() {
        // `list.at(items, i).field` via a let, for both a List(Record) parameter and a
        // let-bound list literal of records; and a for-loop over the let-bound
        // list. Both backends agree.
        let src = r#"
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
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "8", "9"]);
    }

    #[test]
    fn dict_of_records_field_access_backends_agree() {
        // Looking a record up in a Dict and accessing its field: the result of
        // get_or carries the default's record type, so `it.price` resolves.
        let src = r#"
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
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "0"]);
    }

    #[test]
    fn tuple_construct_and_destructure_on_wasm() {
        // Multiple-return-value tuples compile to WASM: divmod(17,5) = (3,2),
        // then 3*100 + 2 = 302.
        let src = r#"
fn divmod(a: Int, b: Int) -> (Int, Int):
    ((a / b), (a % b))

fn main() -> Int:
    let (q, r) = divmod(17, 5)
    ((q * 100) + r)
"#;
        assert_eq!(run_on_wasm(src), vec!["302"]);
    }

    #[test]
    fn nested_record_chained_field_access_on_wasm() {
        // `o.inner.v` — chained access through a nested record — compiles to WASM.
        let src = r#"
type Inner:
    v: Int

type Outer:
    inner: Inner
    tag: Int

fn deep(o: Outer) -> Int:
    (((o).inner).v + (o).tag)

fn main() -> Int:
    let o = Outer(Inner(42), 8)
    deep(o)
"#;
        assert_eq!(run_on_wasm(src), vec!["50"]);
    }

    #[test]
    fn record_call_and_update_results_field_access_on_wasm() {
        // Field access on a `let` bound to a record-returning call / update —
        // exercises return-record and update-result type tracking in codegen.
        let src = r#"
type Point:
    x: Int
    y: Int

fn make(a: Int, b: Int) -> Point:
    Point(a, b)

fn shift(p: Point, dx: Int) -> Point:
    Point(x: ((p).x + dx), ..p)

fn main() -> Int:
    let p = make(3, 4)
    let q = shift(p, 7)
    ((q).x + (q).y)
"#;
        assert_eq!(run_on_wasm(src), vec!["14"]);
    }

    #[test]
    fn records_example_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../../examples/records/src/records.witchy")),
            vec!["origin.x = 2", "moved = (12, 3)", "manhattan(moved) = 15"]
        );
    }

    #[test]
    fn record_typed_list_iteration_on_wasm() {
        // `for it in items` where items: List(Record) — the loop var's fields
        // resolve. total([Item(3,2), Item(5,1)]) = 3*2 + 5*1 = 11.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn total(items: List(Item)) -> Int:
    var sum = 0
    for it in items:
        sum = (sum + ((it).price * (it).qty))
    sum

fn main() -> Int:
    total([Item(3, 2), Item(5, 1)])
"#;
        assert_eq!(run_on_wasm(src), vec!["11"]);
    }

    #[test]
    fn record_field_access_and_update_run_on_wasm() {
        // Records — field access *and* update — compile and run on the WASM
        // runtime. shift_x(Point(3,4), 1) = Point(4,4); 4*4 + 4*4 = 32.
        assert_eq!(
            run_on_wasm(include_str!("../../examples/record_compiled/src/record_compiled.witchy")),
            vec!["32"]
        );
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

    #[test]
    fn record_update_example_runs_on_wasm() {
        // `update` referencing the original record, plus a String-field update;
        // the original is unchanged.
        let src = include_str!("../../examples/record_update/src/record_update.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["alice 100", "alice 150", "alice smith 150"]
        );
    }

    // Dict operations factored into helper functions: codegen picks the
    // string-vs-i32 key comparison from the static key type, so a `k: String`
    // parameter must compile to by-value comparison just like an inline String
    // key. Looking up with a freshly built string (`"ap" + "ple"`) proves the
    // match is structural, not by pointer — and both backends must agree.
    // An integration stress test for first-class functions: a list of closures
    // folded with a higher-order lambda that applies each function-typed
    // element to the accumulator (`f(acc)`). Exercises closures stored in a
    // list, a function-typed fold element, and calling a function-valued lambda
    // parameter — all of which must agree across backends.
    // Nested records: `l.from.x` requires codegen to resolve the record type of
    // the intermediate field (`l.from` is a Point) to index the next one. Record
    // update rebuilds the outer record with one field replaced, leaving the rest
    // (and the original value) untouched. Both backends must agree.
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

    // Tuple patterns in `match`: literals and wildcards in each position
    // (quadrant), plus binding tuple elements alongside a literal in another
    // position (describe). Destructuring a matched tuple must agree across
    // backends.
    // List comprehensions desugar to a block that builds the list with a for
    // loop and push: `[elem for x in xs (if cond)?]`. Mapping, filtering, and an
    // empty source all agree across backends.
    // Comprehensions compose with records: the element expression and the `if`
    // filter both access fields of the loop variable (resolved because the
    // source is a List(Record)). Both backends agree.
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

    // `update` on a base that is not a bare variable: a field access (`l.from`)
    // and an `if` expression. Codegen used to require a record-typed variable;
    // it now evaluates an arbitrary base once into a scratch slot, matching the
    // interpreter. Nested update in an override (`update p { x: update q ... }`)
    // exercises the level-scoped scratch reuse.
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

    // Iterating records produced by a non-variable expression: a call returning
    // `List(Record)` and a list literal of records. Codegen now resolves the
    // loop variable's record type (so `p.x` works in the body) for any list
    // expression, not just a bare variable — matching the interpreter.
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
        // A record holding a List(Int) and a String: field access, length on a
        // list field, and a `for` loop iterating a list *field* (the iterand is a
        // field expression, not a variable). Both backends agree.
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
    fn nested_records_backends_agree() {
        // A record containing a record: chained field access (o.inner.v), nested
        // construction, `update` on a nested field, and immutability of the
        // original. Both backends must agree.
        let src = r#"
type Inner:
    v: Int

type Outer:
    name: String
    inner: Inner

fn main(console: Console):
    let o = Outer("x", Inner(42))
    console.print("${((o).inner).v}")
    let o2 = Outer(inner: Inner((((o).inner).v + 1)), ..o)
    console.print("${((o2).inner).v}")
    console.print((o).name)
    console.print("${((o).inner).v}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "43", "x", "42"]);
    }

    #[test]
    fn records_example() {
        assert_eq!(
            interp(include_str!("../../examples/records/src/records.witchy")),
            vec![
                "origin.x = 2",
                "moved = (12, 3)",
                "manhattan(moved) = 15"
            ]
        );
    }

    #[test]
    fn record_update_example() {
        assert_eq!(
            interp(include_str!("../../examples/record_update/src/record_update.witchy")),
            vec!["alice 100", "alice 150", "alice smith 150"]
        );
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
