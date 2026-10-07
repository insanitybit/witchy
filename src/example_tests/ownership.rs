use super::*;
use crate::{ast, parser, pipeline, typeck};

#[test]
fn closure_value_capture_own_calls_preserve_outer_snapshots_on_all_backends() {
    for mode in ["", "mode opt\n\n"] {
        let src = format!(
            "{mode}fn take(own text: String) -> String:\n    text\n\n\
             fn main(console: Console):\n    let text = \"hello\"\n    let callback = fn() -> String: take(text)\n    let first = callback()\n    let second = callback()\n    console.print(\"${{text.length() + first.length() + second.length()}}\")\n"
        );
        let want = ["15"];
        assert_eq!(
            wasm_run(&src),
            want,
            "compiled execution preserves the captured snapshot in {mode:?}",
        );
    }
}

// RFC-0167 immutable rooted-view tests replace retired RFC-0122 carrier fixtures.

    // RFC-0167 view tests replace this retired RFC-0083 materialization fixture.

// RFC-0167 immutable views replace retired RFC-0112/RFC-0083 lifetime fixtures.

    /// A conventional result from an opt export is logically owned in normal
    /// source. It may share an internal owner only until either side mutates;
    /// normal code must observe two independent values without a loan.
    #[test]
    fn rfc0122_normal_opt_result_detaches_before_owner_mutation_on_both_backends() {
        let api = r#"
mode opt

pub fn snapshot(own values: List(Int)) -> List(Int):
    values
"#;
        let app = r#"
import api
import list

fn main(console: Console):
    var values = [1]
    var original = values
    let snapshot = api.snapshot(move values)
    original.push(2)
    console.print("${snapshot}:${original}")
"#;
        let modules = vec![
            ("api".into(), parser::parse_module(api).expect("parse opt API")),
            ("app".into(), parser::parse_module(app).expect("parse normal caller")),
        ];
        let linked = crate::pipeline::link(modules, "app")
            .expect("link normal caller with an owned opt result");
        typeck::check(&linked).expect("normal result has no reference contract");
        let expected = ["[1]:[1, 2]"];
        assert_eq!(
            run_linked_on_wasm(&[("api", api), ("app", app)], "app"),
            expected,
            "compiled code detaches the ordinary result before owner mutation",
        );
    }

    /// An aliased normal caller selects the copy-correct conventional entry for
    /// an opt `var unique` export: the callee's write-back updates the caller's
    /// variable while the pre-existing value alias keeps its original contents.
    /// Neither source module contains a reference type or borrow expression.
    #[test]
    fn rfc0122_normal_repair_preserves_alias_and_var_writeback_on_both_backends() {
        let api = r#"
mode opt

import list

pub fn append(var values: unique List(Int)) -> ():
    values.push(3)
"#;
        let app = r#"
import api

fn main(console: Console):
    var values = [1, 2]
    let before = values
    api.append(values)
    console.print("${values}:${before}")
"#;
        let modules = vec![
            ("api".into(), parser::parse_module(api).expect("parse opt API")),
            ("app".into(), parser::parse_module(app).expect("parse normal caller")),
        ];
        let linked = crate::pipeline::link(modules, "app")
            .expect("link normal repair caller");
        typeck::check(&linked).expect("normal repair caller type checks");
        let expected = ["[1, 2, 3]:[1, 2]"];
        assert_eq!(
            run_linked_on_wasm(&[("api", api), ("app", app)], "app"),
            expected,
            "compiled backend performs the same repair and write-back",
        );
    }

    /// RFC-0088 baseline: update-and-extract returns the exact old leaf while
    /// committing the repaired collection. Shared snapshots remain unchanged;
    /// empty/missing operations return None. The structural helper performs the
    /// selection and repair together on both engines.
    #[test]
    fn rfc0088_update_extract_baseline_agrees_on_both_backends() {
        let src = r#"import dict
import list

fn main(console: Console):
    var d = dict.from_pairs([("a", "one"), ("b", "two")])
    let d_snapshot = d
    let replaced = d.insert("a", "ONE")
    let inserted = d.insert("c", "three")
    let removed = d.remove("b")
    let absent = d.remove("missing")
    match replaced:
        Some(value) -> console.print("replace=${value}")
        None -> console.print("replace=none")
    match inserted:
        Some(value) -> console.print("insert=${value}")
        None -> console.print("insert=none")
    match removed:
        Some(value) -> console.print("remove=${value}")
        None -> console.print("remove=none")
    match absent:
        Some(value) -> console.print("absent=${value}")
        None -> console.print("absent=none")
    console.print("d=${dict.pairs(d)}")
    console.print("d_snapshot=${dict.pairs(d_snapshot)}")
"#;
        let want = [
            "replace=one",
            "insert=none",
            "remove=two",
            "absent=none",
            "d=[(a, ONE), (c, three)]",
            "d_snapshot=[(a, one), (b, two)]",
        ];
        assert_eq!(wasm_run(src), want, "compiled update-and-extract");
    }

    /// Shared record and ADT leaves exercise the descriptor path beyond scalar,
    /// string, and nested-container slots. Snapshots must retain their old leaf
    /// while the repaired root and ordinary result remain independently live.
    #[test]
    fn rfc0088_record_and_adt_leaves_survive_shared_extraction() {
        let src = r#"import dict

type Box:
    value: String

type Payload:
    Text(String)
    Count(Int)

fn payload_text(value: Payload) -> String:
    match value:
        Text(text) -> text
        Count(n) -> "${n}"

fn main(console: Console):
    var boxes = [Box("a"), Box("b")]
    let boxes_snapshot = boxes
    let popped = boxes.pop()

    var payloads = dict.from_pairs([("key", Text("old"))])
    let payloads_snapshot = payloads
    let replaced = payloads.insert("key", Count(7))
    let removed = payloads.remove("key")

    match popped:
        Some(value) -> console.print(value.value)
        None -> console.print("none")
    match replaced:
        Some(value) -> console.print(payload_text(value))
        None -> console.print("none")
    match removed:
        Some(value) -> console.print(payload_text(value))
        None -> console.print("none")
    console.print("${boxes_snapshot[1].value}")
    console.print(payload_text(dict.at(payloads_snapshot, "key")))
    console.print("${dict.length(payloads)}")
"#;
        let want = ["b", "old", "7", "b", "old", "0"];
        assert_eq!(wasm_run(src), want, "compiled record/ADT extraction");
    }

    #[test]
    fn rfc0087_trait_dispatch_preserves_var_conventions_on_both_backends() {
        let src = "trait Advance:\n    fn advance(var self, by: Int) -> Int\n\ntype Counter:\n    value: Int\n\nimpl Advance for Counter:\n    fn advance(var self, by: Int) -> Int:\n        self.value = self.value + by\n        self.value\n\nfn step(var value: a, by: Int) -> Int where a: Advance:\n    value.advance(by)\n\nfn main(console: Console):\n    var counter = Counter(5)\n    let result = step(counter, 7)\n    console.print(\"${counter.value} ${result}\")\n";
        let want = ["12 12"];
        assert_eq!(wasm_run(src), want, "compiled trait `var` dispatch");

        let mismatch = "trait Advance:\n    fn advance(var self, by: Int) -> Int\n\ntype Counter:\n    value: Int\n\nimpl Advance for Counter:\n    fn advance(self, by: Int) -> Int:\n        self.value + by\n";
        let linked = crate::pipeline::link(
            vec![("main".to_string(), parser::parse_module(mismatch).expect("parse"))],
            "main",
        )
        .expect("link");
        let err = typeck::check(&linked)
            .expect_err("trait implementation must retain the declared convention");
        assert!(err.message.contains("convention"), "trait mismatch: {err}");
        assert!(err.message.contains("Var"), "trait mismatch must show required `var`: {err}");
    }

    /// Nested field/index places capture their coordinates once, stage the rebuilt
    /// root, and commit the same result on both engines.
    #[test]
    fn rfc0087_nested_var_places_write_back_on_both_backends() {
        let src = "import list\n\ntype State:\n    rows: List(List(Int))\n\nfn bump(var n: Int) -> Int:\n    n = n + 10\n    n * 2\n\nfn main(console: Console):\n    var state = State([[1, 2], [3, 4]])\n    let result: Int = bump(state.rows[0][1])\n    let updated: Int = list.at(list.at(state.rows, 0), 1)\n    if updated == 12 && result == 24:\n        console.print(\"ok\")\n    else:\n        console.print(\"bad\")\n";
        let want = ["ok"];
        assert_eq!(wasm_run(src), want, "compiled nested place write-back");
    }

    #[test]
    fn rfc0087_exclusivity_and_var_place_diagnostics_are_resolved() {
        let error = |case: &str, src: &str| {
            typeck::check(&resolve_std_src(src))
                .err()
                .unwrap_or_else(|| panic!("{case}: invalid `var` place must be rejected"))
                .message
        };
        let duplicate = "fn exchange(var a: Int, var b: Int) -> ():\n    return\nfn main(console: Console):\n    var n = 1\n    exchange(n, n)\n";
        let message = error("duplicate", duplicate);
        assert!(message.contains("arguments 1 and 2"), "overlap positions: {message}");
        assert!(message.contains("rooted in `n`"), "overlap root: {message}");

        let dynamic = "import list\n\nfn exchange(var a: Int, var b: Int) -> ():\n    return\nfn main(console: Console):\n    var xs = [1, 2]\n    var i = 0\n    var j = 1\n    exchange(xs[i], xs[j])\n";
        assert!(error("dynamic", dynamic).contains("overlapping `var` places"));

        let reservation = "fn inner(var n: Int) -> Int:\n    n = n + 1\n    n\nfn outer(var n: Int, snapshot: Int) -> ():\n    return\nfn main(console: Console):\n    var n = 0\n    outer(n, inner(n))\n";
        let message = error("reservation", reservation);
        assert!(message.contains("reserves `var` place"), "reservation: {message}");
        assert!(message.contains("written evaluation order"), "reservation order: {message}");

        let moved = "fn bump(var n: Int) -> ():\n    return\nfn main(console: Console):\n    var n = 0\n    bump(move n)\n";
        let message = error("move", moved);
        assert!(message.contains("uses `move`"), "move diagnostic: {message}");
        assert!(message.contains("live mutable place"), "move fix: {message}");

        let temporary = "fn bump(var n: Int) -> ():\n    return\nfn main(console: Console):\n    bump(1)\n";
        assert!(error("temporary", temporary).contains("must be a mutable place"));

        let immutable = "fn bump(var n: Int) -> ():\n    return\nfn test(n: Int) -> ():\n    bump(n)\n    return\nfn main(console: Console):\n    test(1)\n";
        let message = error("immutable", immutable);
        assert!(message.contains("root `n` must be a mutable `var`"), "immutable: {message}");
    }

    #[test]
    fn rfc0087_expression_evaluation_order_is_identical_on_both_backends() {
        let src = r#"import list

fn mark(var log: List(Int), value: Int) -> Int:
    log.push(value)
    value

fn mark_zero(var log: List(Int), value: Int) -> Int:
    log.push(value)
    0

fn mark_bool(var log: List(Int), value: Int, result: Bool) -> Bool:
    log.push(value)
    result

fn mark_none(var log: List(Int), value: Int) -> Option(Int):
    log.push(value)
    None

fn mark_some(var log: List(Int), value: Int) -> Option(Int):
    log.push(value)
    Some(value)

fn pair(a: Int, b: Int) -> Int:
    a + b

type Marker:
    value: Int

impl Marker:
    fn combine(self, other: Int) -> Int:
        self.value + other

fn make_marker(var log: List(Int), value: Int) -> Marker:
    Marker(mark(log, value))

fn main(console: Console):
    var log: List(Int) = []
    let call = pair(mark(log, 1), mark(log, 2))
    let operator = mark(log, 3) + mark(log, 4)
    let tuple = (mark(log, 5), mark(log, 6))
    let list_value = [mark(log, 7), mark(log, 8)]
    let mapped = [mark(log, x + 2) for x in [mark(log, 9), mark(log, 10)]]
    let filtered = [mark(log, x * 2 + 12) for x in [1, 2] if mark_bool(log, x * 2 + 11, true)]
    let text = "${mark(log, 17)} ${mark(log, 18)}"
    let fallback = mark_none(log, 19) ?? mark(log, 20)
    let short_fallback = mark_some(log, 21) ?? mark(log, 22)
    let matched = match mark_some(log, 23):
        Some(value) -> mark(log, 24) + value
        None -> 0
    let selected = if mark_bool(log, 25, true):
        mark(log, 26)
    else:
        0
    var target = [0]
    target[mark_zero(log, 27)] = mark(log, 28)
    let and_value = mark_bool(log, 29, false) && mark_bool(log, 30, true)
    let or_value = mark_bool(log, 31, true) || mark_bool(log, 32, false)
    let method = make_marker(log, 33).combine(mark(log, 34))
    console.print("${log}")
"#;
        let want = ["[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 23, 24, 25, 26, 27, 28, 29, 31, 33, 34]"];
        assert_eq!(wasm_run(src), want, "compiled expression order");
    }

    #[test]
    fn rfc0087_place_coordinates_are_evaluated_once_on_both_backends() {
        let src = "import list\n\nfn next_index(var calls: Int) -> Int:\n    calls = calls + 1\n    0\n\nfn bump(var n: Int) -> ():\n    n = n + 5\n    return\n\nfn main(console: Console):\n    var calls = 0\n    var rows = [[1, 2]]\n    bump(rows[next_index(calls)][1])\n    if calls == 1 && list.at(list.at(rows, 0), 1) == 7:\n        console.print(\"ok\")\n    else:\n        console.print(\"bad\")\n";
        let want = ["ok"];
        assert_eq!(wasm_run(src), want, "compiled backend captures coordinates once");
    }

    /// A `var` shares its token with a call instead of copying for it; a write
    /// after the call copies once, so every earlier result keeps its value.
    #[test]
    fn var_bytes_slices_keep_their_snapshot_across_later_writes() {
        let src = "import bytes\n\nfn main(console: Console):\n    var seq = bytes.from_string(\"aaaa\")\n    let first = seq.slice(0, 3)\n    seq.set_at(0, 90)\n    var out = \"\"\n    var i = 1\n    while i < 4:\n        let s = seq.slice(0, 4)\n        seq.set_at(i, 66)\n        out = out + s.to_string() + \",\"\n        i = i + 1\n    console.print(first.to_string())\n    console.print(out + seq.to_string())\n";
        assert_eq!(wasm_run(src), ["aaa", "Zaaa,ZBaa,ZBBa,ZBBB"]);
    }

    #[test]
    fn var_collections_passed_to_a_call_keep_their_snapshot() {
        let src = "fn keep_list(xs: List(Int)) -> List(Int):\n    xs\n\nfn keep_dict(d: Dict(String, Int)) -> Dict(String, Int):\n    d\n\nfn keep_text(s: String) -> String:\n    s\n\nfn main(console: Console):\n    var xs = [1, 2, 3]\n    let kept = keep_list(xs)\n    xs.set_at(0, 9)\n    xs.push(4)\n    var d: Dict(String, Int) = dict.new()\n    d[\"a\"] = 1\n    let kept_d = keep_dict(d)\n    d[\"a\"] = 2\n    d[\"b\"] = 3\n    var s = \"ab\"\n    s = s + \"c\"\n    let kept_s = keep_text(s)\n    s = s + \"d\"\n    console.print(\"${kept} ${xs}\")\n    console.print(\"${kept_d.get(\"a\")} ${kept_d.length()} ${d.get(\"a\")} ${d.length()}\")\n    console.print(\"${kept_s} ${s}\")\n";
        assert_eq!(wasm_run(src), ["[1, 2, 3] [9, 2, 3, 4]", "Some(1) 1 Some(2) 2", "abc abcd"]);
    }

    /// A loop-carried `var` keeps its unique header lane: the body may write
    /// it in place and then pass it to a call on the same iteration.
    #[test]
    fn loop_carried_var_arguments_follow_in_place_writes() {
        let src = "fn total(xs: List(Int)) -> Int:\n    var t = 0\n    for x in xs:\n        t = t + x\n    t\n\nfn first(xs: List(Int)) -> Int:\n    xs[0]\n\nfn size(d: Dict(String, Int)) -> Int:\n    d.length()\n\nfn main(console: Console):\n    var xs: List(Int) = []\n    var ys = [0, 0]\n    var d: Dict(String, Int) = dict.new()\n    var acc = 0\n    var i = 0\n    while i < 3:\n        xs.push(i)\n        acc = acc + total(xs)\n        ys.set_at(0, i)\n        acc = acc + first(ys)\n        d[\"k${i}\"] = i\n        acc = acc + size(d)\n        i = i + 1\n    console.print(\"${acc} ${xs} ${ys} ${d.length()}\")\n";
        assert_eq!(wasm_run(src), ["13 [0, 1, 2] [2, 0] 3"]);
    }

    #[test]
    fn shared_var_arguments_keep_nested_projected_and_write_back_snapshots() {
        let src = "fn keep_nested(xss: List(List(Int))) -> List(List(Int)):\n    xss\n\nfn keep(xs: List(Int)) -> List(Int):\n    xs\n\nfn bump(var xs: List(Int)):\n    xs.push(9)\n\nfn main(console: Console):\n    var xss = [[1], [2]]\n    let nested = keep_nested(xss)\n    let projected = keep(xss[1])\n    xss[0].push(5)\n    xss[1].push(6)\n    var xs = [1]\n    let kept = keep(xs)\n    bump(xs)\n    console.print(\"${nested} ${projected} ${xss}\")\n    console.print(\"${kept} ${xs}\")\n";
        assert_eq!(wasm_run(src), ["[[1], [2]] [2] [[1, 5], [2, 6]]", "[1] [1, 9]"]);
    }

    /// A constructor moves a fresh temporary into its field and clones a
    /// value a binding holds, so reusing that binding still sees its value.
    #[test]
    fn constructors_move_temporaries_and_share_bound_values() {
        let src = "type Tree:\n    Leaf\n    Node(Tree, Tree)\n\ntype Box:\n    items: List(Int)\n\nfn size(t: Tree) -> Int:\n    match t:\n        Leaf -> 1\n        Node(l, r) -> size(l) + size(r)\n\nfn make(n: Int) -> List(Int):\n    [n, n]\n\nfn main(console: Console):\n    let a = Node(Leaf, Leaf)\n    let b = Node(a, a)\n    let c = Node(b, a)\n    var xs = [1, 2]\n    let kept = Box(xs)\n    xs.push(3)\n    let made = Box(make(4))\n    console.print(\"${size(a)} ${size(b)} ${size(c)}\")\n    console.print(\"${kept.items} ${xs} ${made.items}\")\n";
        assert_eq!(wasm_run(src), ["2 4 6", "[1, 2] [1, 2, 3] [4, 4]"]);
    }

    #[test]
    fn constructor_fields_from_joins_owned_parameters_and_fresh_results() {
        let src = "type Box:\n    items: List(Int)\n\nfn rewrap(own b: Box) -> Box:\n    Box(b.items)\n\nfn fresh(n: Int) -> List(Int):\n    var xs: List(Int) = []\n    xs.push(n)\n    xs\n\nfn main(console: Console):\n    let c = 3 > 2\n    let joined = Box(if c: [1] else: [2])\n    let moved = rewrap(Box([5, 6]))\n    let made = Box(fresh(7))\n    console.print(\"${joined.items} ${moved.items} ${made.items}\")\n";
        assert_eq!(wasm_run(src), ["[1] [5, 6] [7]"]);
    }

    /// Popping a reference element another binding still holds hands out a
    /// shared token: writing the popped value copies, the holder keeps its own.
    #[test]
    fn popped_reference_elements_keep_value_semantics() {
        let src = "fn main(console: Console):\n    let inner = [1, 2]\n    var xs = [inner, [3]]\n    xs.push(inner)\n    var words = [\"x\", \"y\"]\n    let w = words.pop()\n    match xs.pop():\n        Some(ys) ->\n            var zs = ys\n            zs.push(9)\n            console.print(\"${zs} ${inner} ${xs}\")\n        None -> console.print(\"none\")\n    console.print(\"${w} ${words} ${words.pop()} ${words.pop()}\")\n";
        assert_eq!(wasm_run(src), ["[1, 2, 9] [1, 2] [[1, 2], [3]]", "Some(y) [x] Some(x) None"]);
    }
