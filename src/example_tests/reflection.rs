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

    /// GENERIC IMPLS COMPOSE: reflection now reaches `List`, `Option`, tuples, and
    /// generic records through ordinary `impl Reflect for List(a)` etc. — a generic
    /// consumer (`json.stringify`, `where a: Reflect`) calling a generic impl method
    /// monomorphizes per element. No builtins; identical on both backends.
    #[test]
    fn reflection_covers_lists_options_tuples_and_generic_records() {
        let src = "import json\nimport reflect\n\ntype Box(a) derive(Reflect):\n    item: a\n\nfn main(console: Console):\n    console.print(json.stringify([1, 2, 3]))\n    console.print(json.stringify(Some(\"x\")))\n    console.print(json.stringify((\"p\", 5)))\n    console.print(json.stringify([(\"a\", \"b\")]))\n    console.print(json.stringify(Box([1, 2])))\n";
        let want = vec!["[1,2,3]", "\"x\"", "[\"p\",5]", "[[\"a\",\"b\"]]", "{\"item\":[1,2]}"];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    #[test]
    fn derive_deserialize_scalar_list_option_error_and_generic_paths_agree() {
        let src = "import json\n\ntype Record derive(Deserialize):\n    n: Int\n    ratio: Float\n    ok: Bool\n    text: String\n    xs: List(Int)\n    maybe: Option(String)\n\ntype Box(a) derive(Deserialize):\n    value: a\n\nfn main(console: Console):\n    match json.decode(\"{\\\"n\\\": 4, \\\"ratio\\\": 2, \\\"ok\\\": true, \\\"text\\\": \\\"hi\\\", \\\"xs\\\": [1, 2], \\\"maybe\\\": null}\"):\n        Ok(doc) ->\n            match Record.from_json(doc):\n                Ok(r) -> console.print(\"${r.n} ${r.ratio} ${r.ok} ${r.text} ${r.xs} ${r.maybe}\")\n                Err(_) -> console.print(\"valid-error\")\n        Err(_) -> console.print(\"parse-error\")\n    match json.decode(\"{\\\"value\\\": [1, null, 3]}\"):\n        Ok(doc) ->\n            let nested: Result(Box(List(Option(Int))), json.DeserializeError) = Box.from_json(doc)\n            match nested:\n                Ok(b) -> console.print(\"${b.value}\")\n                Err(_) -> console.print(\"generic-error\")\n        Err(_) -> console.print(\"parse-error\")\n    match json.decode(\"{\\\"n\\\": 4}\"):\n        Ok(doc) ->\n            match Record.from_json(doc):\n                Ok(_) -> console.print(\"missing-not-rejected\")\n                Err(_) -> console.print(\"missing-error\")\n        Err(_) -> console.print(\"parse-error\")\n";
        let expected = ["4 2.0 true hi [1, 2] None", "[Some(1), None, Some(3)]", "missing-error"];
        witchy_interp::compiler_natives::install();
        let linked = prepared_module(src);
        let (interpreted, compiled) = std::thread::scope(|scope| {
            let interpreter = scope.spawn(|| {
                interpreter::run_module(linked.clone(), ".", Vec::new())
                    .expect("interpreter run")
            });
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            let compiled = run_bytes_print_only(&bytes);
            (interpreter.join().expect("interpreter worker"), compiled)
        });
        assert_eq!(
            interpreted, expected,
            "interp: deserialize scalar/list/option/generic/error paths"
        );
        assert_eq!(
            compiled, expected,
            "compiled: deserialize scalar/list/option/generic/error paths"
        );
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
