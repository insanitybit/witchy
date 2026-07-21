use super::*;
use crate::{interpreter, parser, typeck};

    /// (BUG-498, parity) `derive(PartialOrd)` must compose through each field's
    /// `partial_compare`, not through `<`/`>` tests that accidentally treat
    /// incomparable fields as equal. A Float NaN field should propagate `None`
    /// on both backends.
    #[test]
    fn derive_partial_ord_float_field_propagates_none_on_both_backends() {
        let src = "import cmp\n\ntype Reading derive(PartialEq, PartialOrd):\n    value: Float\n\nfn describe(o: Option(Ordering)) -> String:\n    match o:\n        None -> \"none\"\n        Some(Less) -> \"less\"\n        Some(Equal) -> \"equal\"\n        Some(Greater) -> \"greater\"\n\nfn main(console: Console):\n    console.print(describe(partial_compare(Reading(0.0 / 0.0), Reading(1.0))))\n    console.print(describe(partial_compare(Reading(1.0), Reading(2.0))))\n    console.print(describe(partial_compare(Reading(2.0), Reading(1.0))))\n    console.print(describe(partial_compare(Reading(2.0), Reading(2.0))))\n";
        let expected = ["none", "less", "greater", "equal"];
        assert_eq!(link_run(src), expected, "interp: derived PartialOrd propagates None");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: derived PartialOrd propagates None",
        );
    }

    /// (BUG-468) `Eq` refines `PartialEq`, so `derive(Eq)` must generate the
    /// structural `PartialEq` impl too. The explicit `derive(PartialEq, Eq)`
    /// spelling remains valid and must not generate duplicate impl heads.
    #[test]
    fn derive_eq_alone_implies_partial_eq_on_both_backends() {
        let src = "import cmp\n\ntype OnlyEq derive(Eq):\n    x: Int\n\ntype Both derive(PartialEq, Eq):\n    x: Int\n\nfn main(console: Console):\n    console.print(\"${OnlyEq(1) == OnlyEq(1)}\")\n    console.print(\"${OnlyEq(1) == OnlyEq(2)}\")\n    console.print(\"${Both(1) == Both(1)}\")\n";
        let expected = ["true", "false", "true"];
        assert_eq!(link_run(src), expected, "interp: derive(Eq) implies PartialEq");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: derive(Eq) implies PartialEq",
        );
    }

    /// (BUG-478) `derive(Eq)` is a marker when the type already has a custom
    /// `PartialEq`. It must not mark that `PartialEq` as structural, because
    /// nested equality then stops calling the hand-written semantics.
    #[test]
    fn derive_eq_marker_preserves_custom_partial_eq_at_depth_on_both_backends() {
        let src = "import cmp\n\ntype Key derive(Eq):\n    id: Int\n    cache: Int\n\nimpl PartialEq for Key:\n    fn eq(self, other: Key) -> Bool:\n        self.id == other.id\n\ntype Wrapper derive(PartialEq, Eq):\n    key: Key\n\nfn a() -> Key:\n    Key(1, 10)\n\nfn b() -> Key:\n    Key(1, 20)\n\nfn main(console: Console):\n    console.print(\"${a() == b()}\")\n    console.print(\"${[a()] == [b()]}\")\n    console.print(\"${Some(a()) == Some(b())}\")\n    console.print(\"${(a(), 1) == (b(), 1)}\")\n    console.print(\"${Wrapper(a()) == Wrapper(b())}\")\n";
        let expected = ["true", "true", "true", "true", "true"];
        assert_eq!(
            link_run(src),
            expected,
            "interp: derive(Eq) marker must preserve custom PartialEq",
        );
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: derive(Eq) marker must preserve custom PartialEq",
        );
    }

    /// (BUG-544) `Ordering` is ordinary std data: it renders through `Show`,
    /// reflects as a nullary variant, and therefore serializes through JSON
    /// reflection, including when it appears in a derived-reflect record.
    #[test]
    fn ordering_is_showable_and_reflectable_on_both_backends() {
        let src = "import cmp\nimport show\nimport reflect\nimport json\n\ntype SortStep derive(Reflect):\n    ordering: Ordering\n\nfn main(console: Console):\n    let o: Ordering = cmp.reverse(Greater)\n    show.say(console, o)\n    console.print(reflect.debug(o))\n    console.print(json.stringify(o))\n    console.print(json.stringify(SortStep(o)))\n";
        let expected = [
            "Less",
            "Less",
            "{\"$variant\":\"Less\",\"$values\":[]}",
            "{\"ordering\":{\"$variant\":\"Less\",\"$values\":[]}}",
        ];
        assert_eq!(link_run(src), expected, "interp: Ordering protocols");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Ordering protocols");
    }

    /// (BUG-370) `reflect.debug` strings escape every C0 control, matching JSON's
    /// discipline instead of emitting raw terminal controls into structural text.
    #[test]
    fn reflect_debug_escapes_all_c0_controls_on_both_backends() {
        let src = "import reflect\nimport string\n\ntype Note derive(Reflect):\n    text: String\n\nfn main(console: Console):\n    console.print(reflect.debug(\"a\" + string.from_code(8) + \"b\"))\n    console.print(reflect.debug(\"a\" + string.from_code(12) + \"b\"))\n    console.print(reflect.debug(\"a\" + string.from_code(0) + \"b\"))\n    console.print(reflect.debug(Note(\"x\" + string.from_code(27) + \"y\")))\n";
        let expected = [
            "\"a\\bb\"",
            "\"a\\fb\"",
            "\"a\\u0000b\"",
            "Note { text: \"x\\u001by\" }",
        ];
        assert_eq!(link_run(src), expected, "interp: reflect.debug C0 escapes");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: reflect.debug C0 escapes");
    }

    /// (BUG-530) Tuple values are legal beyond arity four, so the public protocol
    /// surface must not silently stop there. Witchy does not have variadic trait
    /// impls yet; the documented 0.1 contract is tuple `Show`/`Reflect` through
    /// arity 8, with wider heterogeneous values modeled as named records.
    #[test]
    fn tuple5_show_and_reflect_protocols_work_on_both_backends() {
        let src = "import show\nimport reflect\nimport json\n\ntype Box5 derive(Reflect):\n    value: (Int, Int, Int, Int, Int)\n\nfn main(console: Console):\n    let t = (1, 2, 3, 4, 5)\n    show.say(console, t)\n    console.print(\"${t}\")\n    console.print(reflect.debug(t))\n    console.print(json.stringify(t))\n    console.print(json.stringify(Box5(t)))\n    let t8 = (1, 2, 3, 4, 5, 6, 7, 8)\n    console.print(show.render(t8))\n    console.print(json.stringify(t8))\n";
        let expected = [
            "(1, 2, 3, 4, 5)",
            "(1, 2, 3, 4, 5)",
            "(1, 2, 3, 4, 5)",
            "[1,2,3,4,5]",
            "{\"value\":[1,2,3,4,5]}",
            "(1, 2, 3, 4, 5, 6, 7, 8)",
            "[1,2,3,4,5,6,7,8]",
        ];
        assert_eq!(link_run(src), expected, "interp: tuple protocol arity");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: tuple protocol arity");
    }

    /// (BUG-486) `MNil` is the reflection shape for the language's unit value,
    /// not only for JSON null. Exercise it through a Nil-returning helper so this
    /// stays independent of the separate bare-`Nil` expression backend bug.
    #[test]
    fn nil_is_reflectable_on_both_backends() {
        let src = "import reflect\nimport json\n\nfn unit() -> Nil:\n    return\n\nfn main(console: Console):\n    console.print(reflect.debug(unit()))\n    console.print(json.stringify(unit()))\n";
        let expected = ["nil", "null"];
        assert_eq!(link_run(src), expected, "interp: Nil reflection");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Nil reflection");
    }

    /// `derive(Show, Eq, Ord)`: compiler-generated impls, byte-identical in
    /// behavior to handwritten ones on both backends, additive-only (the
    /// expansion appends impls before checking; footprint analysis covers
    /// the expanded program).
    #[test]
    fn derive_show_eq_ord_generates_working_impls() {
        let src = "import show\nimport cmp\nimport list\n\ntype Point derive(Show, PartialEq, Eq, PartialOrd, Ord):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let a = Point(1, 2)\n    let b = Point(1, 3)\n    show.say(console, a)\n    console.print(\"${eq(a, Point(1, 2))} ${eq(a, b)}\")\n    console.print(\"${less(a, b)} ${less(b, a)}\")\n    console.print(\"${list.contains([a, b], Point(1, 3))}\")\n";
        let want: Vec<String> = ["Point(1, 2)", "true false", "true false", "true"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // A derive now routes to a user generator `derive_<name>`; with none in
        // scope it's a loud error at comptime (the generated call can't resolve).
        let bad = "type T derive(Serialize):\n    n: Int\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let res = crate::pipeline::link(
            vec![("main".to_string(), parser::parse_module(bad).expect("parse"))],
            "main",
        );
        let err = format!("{:?}", res.expect_err("missing derive generator must be rejected"));
        assert!(err.to_lowercase().contains("serialize"), "got: {err}");
    }

    #[test]
    fn derive_ord_on_generic_record() {
        // derive(Ord) on a GENERIC record: the generated impl and the Ord trait's
        // default methods (`greater`/`less`, used by `cmp.max_of`) must be typed
        // against the applied `Pair(a, b)`, not the bare head `Pair` — otherwise a
        // real `Pair(Int, Int)` clashes with the method's `other: Self`. Both
        // backends agree. (Regression for the bare-head `Self` substitution.)
        let src = "import cmp\n\ntype Pair(a, b) derive(PartialEq, Eq, PartialOrd, Ord):\n    first: a\n    second: b\n\nfn main(console: Console):\n    let m = cmp.max_of(Pair(1, 9), Pair(1, 4))\n    console.print(\"${m.first} ${m.second}\")\n    console.print(\"${less(Pair(1, 2), Pair(1, 3))} ${less(Pair(2, 0), Pair(1, 9))}\")\n";
        let want: Vec<String> = ["1 9", "true false"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// A `derive` (comptime) in a module that ALSO imports a project-local sibling
    /// must still link. The comptime program runs in the isolated, std-only
    /// `comptime` link, so project-local imports are filtered out of it (a comptime
    /// is a capability-free, link-time eval that cannot use sibling runtime code
    /// anyway). Regression for `comptime block: imports unknown module <sibling>`,
    /// which made `derive` unusable in any multi-module rune (e.g. its test module).
    #[test]
    fn derive_links_alongside_a_project_local_import() {
        let sibling = parser::parse_module("pub fn helper() -> Int:\n    7\n").expect("parse sibling");
        let main = parser::parse_module(
            "import sibling\nimport json\nimport result\n\ntype Foo derive(Deserialize):\n    x: Int\n\nfn main(console: Console):\n    console.print(\"${sibling.helper()}\")\n",
        )
        .expect("parse main");
        let linked = crate::pipeline::link(
            vec![("sibling".into(), sibling), ("main".into(), main)],
            "main",
        )
        .expect("a derive must link in a module that also imports a project-local sibling");
        crate::typeck::check(&linked).expect("typecheck");
        let out = interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, vec!["7".to_string()]);
    }

    /// GENERIC IMPLS COMPOSE: reflection now reaches `List`, `Option`, tuples, and
    /// generic records through ordinary `impl Reflect for List(a)` etc. — a generic
    /// consumer (`json.stringify`, `where a: Reflect`) calling a generic impl method
    /// monomorphizes per element. No builtins; identical on both backends.
    #[test]
    fn reflection_covers_lists_options_tuples_and_generic_records() {
        let src = "import json\nimport reflect\n\ntype Box(a) derive(Reflect):\n    item: a\n\nfn main(console: Console):\n    console.print(json.stringify([1, 2, 3]))\n    console.print(json.stringify(Some(\"x\")))\n    console.print(json.stringify((\"p\", 5)))\n    console.print(json.stringify([(\"a\", \"b\")]))\n    console.print(json.stringify(Box([1, 2])))\n";
        let want: Vec<String> = ["[1,2,3]", "\"x\"", "[\"p\",5]", "[[\"a\",\"b\"]]", "{\"item\":[1,2]}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Uniform `var` calls must preserve the element-type refinement that the
    /// former `xs = list.push(xs, value)` shape supplied through assignment.
    /// This is especially important for generated `Reflect` implementations,
    /// where leaving `xs` as a bare `List` produces an invalid specialization.
    #[test]
    fn var_call_refines_empty_list_for_generated_reflect_on_both_backends() {
        let src = "import json\n\nfn main(console: Console):\n    var rows = []\n    for name in [\"ada\"]:\n        rows.push(.{name: name, score: 7})\n    console.print(json.stringify(.{rows: rows}))\n";
        let want = vec!["{\"rows\":[{\"name\":\"ada\",\"score\":7}]}".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Reflection's built-in protocol matrix includes scalar-like `Duration` and
    /// common std containers `Result`/`Set`, so `json.stringify` and
    /// `reflect.debug` do not arbitrarily stop at a few older container types.
    #[test]
    fn reflection_protocol_covers_duration_result_and_set() {
        let src = "import json\nimport reflect\nimport set\nimport duration\n\nfn main(console: Console):\n    let ok: Result(Int, String) = Ok(7)\n    let err: Result(Int, String) = Err(\"bad\")\n    let s = set.from_list([2, 1, 2])\n    console.print(json.stringify(1500ms))\n    console.print(reflect.debug(duration.seconds(2)))\n    console.print(json.stringify(ok))\n    console.print(json.stringify(err))\n    console.print(json.stringify(s))\n    console.print(reflect.debug(s))\n";
        let want: Vec<String> = [
            "1500",
            "2000",
            "{\"$variant\":\"Ok\",\"$values\":[7]}",
            "{\"$variant\":\"Err\",\"$values\":[\"bad\"]}",
            "[2,1]",
            "[2, 1]",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// REFLECTION, SECOND USE CASE: `reflect.debug(x)` renders any value from the
    /// SAME `reflect` that powers `json` — proving the engine is general, not a
    /// json-specific hack. Records, lists-in-fields, and scalars, both backends.
    #[test]
    fn reflective_debug_render_other_use_case() {
        let src = "import reflect\n\ntype Point derive(Reflect):\n    x: Int\n    y: Int\n\ntype Bag derive(Reflect):\n    items: List(Int)\n    label: String\n\nfn main(console: Console):\n    console.print(reflect.debug(Point(1, 2)))\n    console.print(reflect.debug(Bag([1, 2, 3], \"nums\")))\n    console.print(reflect.debug(42))\n";
        let want: Vec<String> = [
            "Point { x: 1, y: 2 }",
            "Bag { items: [1, 2, 3], label: \"nums\" }",
            "42",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-286, BUG-034) `derive(Deserialize)` composes `Option` at any depth —
    /// inside a `List` and nested in another `Option` — decoding JSON `null` to
    /// `None`. The generated code uses prelude `Result`/`Option` names without
    /// requiring redundant `import result` / `import option` lines.
    #[test]
    fn derive_deserialize_nested_option_backends_agree() {
        let src = "import json\n\ntype Rec derive(Deserialize):\n    xs: List(Option(Int))\n    oo: Option(Option(Int))\n\nfn main(console: Console):\n    match json.decode(\"{\\\"xs\\\": [1, null, 3], \\\"oo\\\": 7}\"):\n        Ok(j) -> match Rec.from_json(j):\n            Ok(r) -> console.print(\"${r.xs} ${r.oo}\")\n            Err(e) -> console.print(\"err\")\n        Err(e) -> console.print(\"parse\")\n";
        let want = ["[Some(1), None, Some(3)] Some(Some(7))"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-496) `derive(Deserialize)` must not bind decoded fields under source
    /// field names. Fields named like generator helpers (`j`) or constructors
    /// (`Ok`/`Err`/`Some`/`None`) decode normally, and later fields still read from
    /// the original JSON object.
    #[test]
    fn derive_deserialize_field_names_are_hygienic_on_both_backends() {
        let src = "import json\n\ntype Odd derive(Deserialize):\n    j: String\n    Ok: String\n    Err: String\n    Some: String\n    None: String\n    rest: Option(List(Option(Int)))\n\nfn main(console: Console):\n    match json.decode(\"{\\\"j\\\": \\\"jay\\\", \\\"Ok\\\": \\\"ok\\\", \\\"Err\\\": \\\"err\\\", \\\"Some\\\": \\\"some\\\", \\\"None\\\": \\\"none\\\", \\\"rest\\\": [1, null, 3]}\"):\n        Ok(doc) -> match Odd.from_json(doc):\n            Ok(r) ->\n                console.print(r.j + \":\" + r.Ok + \":\" + r.Err + \":\" + r.Some + \":\" + r.None)\n                console.print(\"${r.rest}\")\n            Err(e) -> console.print(\"err \" + json.deserialize_error_message(e))\n        Err(e) -> console.print(\"parse\")\n";
        let want = ["jay:ok:err:some:none", "Some([Some(1), None, Some(3)])"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-532) Tuple fields are outside the documented `derive(Deserialize)`
    /// contract for now. Reject them at the derive boundary instead of emitting a
    /// fallback call like `(Int, String).from_json(...)` and leaking `Tuple2` in a
    /// later type error.
    #[test]
    fn derive_deserialize_rejects_tuple_fields_without_generated_fallback_leak() {
        let src = "import json\nimport result\n\ntype PairBox derive(Deserialize):\n    pair: (Int, String)\n";
        let err = try_link_std(src).expect_err("tuple field must be rejected by derive");
        assert!(err.contains("derive(Deserialize)"), "{err}");
        assert!(err.contains("tuple field `pair`"), "{err}");
        assert!(!err.contains("Tuple2"), "must not leak generated tuple fallback: {err}");
        assert!(!err.contains("from_json"), "must not leak generated from_json fallback: {err}");
    }

    /// (BUG-299) `derive(Show)` on a GENERIC type renders identically on both
    /// backends (was a check-passes/interp-runs/WASM-rejects split: the derived body
    /// routed through structural render). Now field-wise, matching interpolation
    /// byte-for-byte.
    #[test]
    fn derive_show_generic_backends_agree() {
        let src = "import show\n\ntype Box(a) derive(Show):\n    value: a\n\ntype Color derive(Show):\n    Red\n    Named(String)\n\ntype Score derive(Show):\n    n: Int\n    name: String\n\nfn main(console: Console):\n    console.print(show(Box(value: 42)))\n    console.print(show(Box(value: [1, 2, 3])))\n    console.print(show(Red))\n    console.print(show(Named(\"blue\")))\n    console.print(show(Score(n: 12, name: \"beta\")))\n";
        let want = ["Box(42)", "Box([1, 2, 3])", "Red", "Named(blue)", "Score(12, beta)"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-399) `derive(Deserialize)` on a GENERIC record reconstructs on both
    /// backends: the impl carries its type params + a per-param `Deserialize` bound,
    /// and the caller ascribes the concrete type.
    #[test]
    fn derive_deserialize_generic_backends_agree() {
        let src = "import json\nimport result\n\ntype Inner derive(Deserialize):\n    n: Int\n\ntype Box(a) derive(Deserialize):\n    value: a\n\nfn main(console: Console):\n    match json.decode(\"{\\\"value\\\": {\\\"n\\\": 7}}\"):\n        Ok(j) ->\n            let r: Result(Box(Inner), json.DeserializeError) = Box.from_json(j)\n            match r:\n                Ok(b) -> console.print(\"${b.value.n}\")\n                Err(e) -> console.print(\"err\")\n        Err(e) -> console.print(\"parse\")\n";
        let want = ["7"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (RFC-0047) The fast-path invariant: a `derive(PartialEq)` type keeps the
    /// STRUCTURAL comparison at every depth (no impl dispatch), so a program with
    /// no CUSTOM impl behaves exactly as before. A derived record differing in a
    /// field is unequal inside a container.
    #[test]
    fn derived_partial_eq_stays_structural_in_containers() {
        let src = "type Pt derive(PartialEq):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    console.print(\"${[Pt(1, 2), Pt(3, 4)] == [Pt(1, 2), Pt(3, 4)]}\")\n    console.print(\"${[Pt(1, 2)] == [Pt(9, 9)]}\")\n    console.print(\"${Some(Pt(1, 2)) == Some(Pt(1, 2))}\")\n";
        let want = vec!["true".to_string(), "false".to_string(), "true".to_string()];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), want, "compiled WASM must agree");
    }
