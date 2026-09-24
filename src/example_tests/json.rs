use super::*;
use crate::{ast, codegen, interpreter, typeck};

fn assert_json_backends(src: &str, expected: &[&str], label: &str) {
    let expected: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(link_run(src), expected, "{label}: interpreter");
    assert_eq!(wasm_run(src), expected, "{label}: compiled WASM");
}

fn assert_json_contract_cases(cases: &[(&str, &str, &[&str])]) {
    for (label, src, expected) in cases {
        assert_json_backends(src, expected, label);
    }
}

    /// (BUG-262) JSON decode rejects duplicate object names, and the public
    /// helper/encoding boundaries must not let hand-built duplicate objects become
    /// signed or emitted wire JSON silently.
    #[test]
    fn json_duplicate_object_keys_fail_at_encoding_boundaries_on_both_backends() {
        if !is_nightly() {
            return;
        }
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        let cases = [
            (
                "encode",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"aud\", json.JsonString(\"good\")), (\"aud\", json.JsonString(\"evil\"))])\n    console.print(json.encode(j))\n",
                "json.encode: duplicate object key `aud`",
            ),
            (
                "pretty",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"aud\", json.JsonString(\"good\")), (\"aud\", json.JsonString(\"evil\"))])\n    console.print(json.encode_pretty(j))\n",
                "json.encode_pretty: duplicate object key `aud`",
            ),
            (
                "object_sorted",
                "import json\n\nfn main(console: Console):\n    let j = json.object_sorted([(\"kid\", json.JsonString(\"a\")), (\"kid\", json.JsonString(\"b\"))])\n    console.print(json.encode(j))\n",
                "json.object_sorted: duplicate object key `kid`",
            ),
            (
                "merge left",
                "import json\n\nfn main(console: Console):\n    let left = json.JsonObject([(\"a\", json.JsonInt(1)), (\"a\", json.JsonInt(2))])\n    let right = json.JsonObject([(\"b\", json.JsonInt(3))])\n    console.print(json.encode(json.merge(left, right)))\n",
                "json.merge: duplicate object key `a`",
            ),
            (
                "merge right",
                "import json\n\nfn main(console: Console):\n    let left = json.JsonObject([(\"a\", json.JsonInt(1))])\n    let right = json.JsonObject([(\"b\", json.JsonInt(2)), (\"b\", json.JsonInt(3))])\n    console.print(json.encode(json.merge(left, right)))\n",
                "json.merge: duplicate object key `b`",
            ),
            (
                "get",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"aud\", json.JsonString(\"good\")), (\"aud\", json.JsonString(\"evil\"))])\n    let _ = json.get(j, \"aud\")\n    console.print(\"bad\")\n",
                "json.get: duplicate object key `aud`",
            ),
            (
                "contains_key",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"kid\", json.JsonString(\"a\")), (\"kid\", json.JsonString(\"b\"))])\n    console.print(\"${json.contains_key(j, \"kid\")}\")\n",
                "json.contains_key: duplicate object key `kid`",
            ),
            (
                "as_object",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"kid\", json.JsonString(\"a\")), (\"kid\", json.JsonString(\"b\"))])\n    let _ = json.as_object(j)\n    console.print(\"bad\")\n",
                "json.as_object: duplicate object key `kid`",
            ),
            (
                "reflect",
                "import json\nimport reflect\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"kid\", json.JsonString(\"a\")), (\"kid\", json.JsonString(\"b\"))])\n    console.print(reflect.debug(j))\n",
                "json.reflect: duplicate object key `kid`",
            ),
        ];
        for (label, src, expected_msg) in cases {
            let (linked, wasm) = compile(src);
            let interp_err = interpreter::run_module(linked, ".", Vec::new())
                .expect_err("interpreter must abort on duplicate JSON object keys")
                .to_string();
            assert!(interp_err.contains(expected_msg), "{label}: {interp_err}");
            let wasm_err = crate::run_wasm_bytes(&wasm)
                .expect_err("WASM must abort on duplicate JSON object keys")
                .to_string();
            assert!(wasm_err.contains(expected_msg), "{label}: {wasm_err}");
        }

        let ok = "import json\n\nfn main(console: Console):\n    let left = json.JsonObject([(\"a\", json.JsonInt(1)), (\"b\", json.JsonInt(2))])\n    let right = json.JsonObject([(\"b\", json.JsonInt(3)), (\"c\", json.JsonInt(4))])\n    console.print(json.encode(json.merge(left, right)))\n";
        let expected = ["{\"a\":1,\"b\":3,\"c\":4}"];
        assert_eq!(link_run(ok), expected, "interp: unique JSON merge still works");
        assert_eq!(run_linked_on_wasm(&[("main", ok)], "main"), expected, "compiled: unique JSON merge still works");
    }

    /// REFLECTION: `json.stringify(x)` encodes ANY value with no `derive(Json)` —
    /// only `derive(Reflect)`, the one generated impl every reflective library
    /// consumes. Covers scalars, nested records, `List`, and `Option` (Some/None),
    /// identical on both backends (the generated `reflect` is ordinary witchy code).
    #[test]
    fn reflective_json_encode_without_derive() {
        let src = "import json\nimport reflect\n\ntype Point derive(Reflect):\n    x: Int\n    y: Int\n\ntype Line derive(Reflect):\n    head: Point\n    tail: Point\n    tags: List(String)\n    note: Option(String)\n\nfn main(console: Console):\n    console.print(json.stringify(Point(1, 2)))\n    console.print(json.stringify(Line(Point(0, 0), Point(3, 4), [\"a\", \"b\"], Some(\"hi\"))))\n    console.print(json.stringify(Line(Point(5, 6), Point(7, 8), [], None)))\n";
        let want: Vec<String> = [
            "{\"x\":1,\"y\":2}",
            "{\"head\":{\"x\":0,\"y\":0},\"tail\":{\"x\":3,\"y\":4},\"tags\":[\"a\",\"b\"],\"note\":\"hi\"}",
            "{\"head\":{\"x\":5,\"y\":6},\"tail\":{\"x\":7,\"y\":8},\"tags\":[],\"note\":null}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // The generated `reflect` makes trait calls that need std/reflect linked,
        // so resolve std for the interpreter path too (link_run's single-module
        // typeck can't see it); the real `witchy run` path resolves std the same way.
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Ordinary `std/json` API contracts share one table while keeping each
    /// source fixture and expected output independent. Every row exercises both
    /// backends, including the typed field-accessor fixture.
    #[test]
    fn json_api_contracts_backends_agree() {
        if !is_nightly() {
            return;
        }
        assert_json_contract_cases(&[
            (
                "typed field accessors",
                r#"import json

fn main(console: Console):
    match json.decode("{\"name\":\"acme\",\"n\":7,\"caps\":[\"Net\",\"Console\"],\"arr\":[\"a\",\"b\"]}"):
        Ok(d) ->
            console.print(opt(json.get_string(d, "name")))
            console.print(oi(json.get_int(d, "n")))
            console.print(list.join(json.get_strings(d, "caps"), ","))
            console.print("[" + list.join(json.get_strings(d, "absent"), ",") + "]")
        Err(e) -> console.print("err")

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"

fn oi(o: Option(Int)) -> String:
    match o:
        Some(n) -> "${n}"
        None -> "?"
"#,
                &["acme", "7", "Net,Console", "[]"],
            ),
            (
                "encode_pretty",
                r#"
import json
from json import Json
fn main(console: Console):
    let doc = JsonObject([("name", JsonString("witchy")), ("tags", JsonArray([JsonInt(1), JsonInt(2)])), ("empty", JsonArray([]))])
    console.print(json.encode_pretty(doc))"#,
                &["{\n  \"name\": \"witchy\",\n  \"tags\": [\n    1,\n    2\n  ],\n  \"empty\": []\n}"],
            ),
            (
                "as_object",
                r#"
import json
import option
from json import Json
fn main(console: Console):
    match json.decode("{\"a\": 1, \"b\": 2}"):
        Ok(doc) ->
            match json.as_object(doc):
                Some(pairs) ->
                    for p in pairs:
                        let (k, _v) = p
                        console.print(k)
                None -> console.print("not object")
        Err(_e) -> console.print("err")
    console.print(if option.is_none(json.as_object(JsonInt(5))): "none" else: "some")"#,
                &["a", "b", "none"],
            ),
            (
                "json.merge/contains_key",
                r#"
import json
from json import Json
fn main(console: Console):
    let a = JsonObject([("name", JsonString("a")), ("x", JsonInt(1))])
    let b = JsonObject([("x", JsonInt(2)), ("y", JsonInt(3))])
    console.print(json.encode(json.merge(a, b)))
    console.print(json.encode(json.merge(a, JsonInt(9))))
    console.print(if json.contains_key(a, "x"): "T" else: "F")
    console.print(if json.contains_key(a, "z"): "T" else: "F")
    console.print(if json.contains_key(JsonInt(5), "x"): "T" else: "F")"#,
                &["{\"name\":\"a\",\"x\":2,\"y\":3}", "9", "T", "F", "F"],
            ),
            (
                "std json get_path",
                r#"
import json
import option
from json import Json
fn str_at(j: Json, path: String) -> String:
    match json.get_path(j, path):
        Some(v) -> option.unwrap_or(json.as_string(v), "?")
        None -> "none"
fn int_at(j: Json, path: String) -> Int:
    match json.get_path(j, path):
        Some(v) -> option.unwrap_or(json.as_int(v), 0)
        None -> 0
fn main(console: Console):
    match json.decode("{\"user\":{\"name\":\"witchy\",\"age\":1},\"tags\":[\"a\"]}"):
        Ok(j) ->
            console.print(str_at(j, "user.name"))
            console.print("${int_at(j, "user.age")}")
            console.print(str_at(j, "user.missing"))
        Err(e) -> console.print(json.decode_error_message(e))"#,
                &["witchy", "1", "none"],
            ),
            (
                "std json indexed access",
                r#"
import json
import option
from json import Json
fn field(j: Json, k: String) -> Json:
    match json.get(j, k):
        Some(v) -> v
        None -> JsonNull

fn elem_int(j: Json, k: String, i: Int) -> Int:
    match json.index(field(j, k), i):
        Some(e) -> option.unwrap_or(json.as_int(e), 0)
        None -> 0

fn main(console: Console):
    match json.decode("{\"name\":\"witchy\",\"version\":3,\"items\":[10,20,30]}"):
        Ok(j) ->
            console.print(option.unwrap_or(json.as_string(field(j, "name")), "?"))
            console.print("${option.unwrap_or(json.as_int(field(j, "version")), 0)}")
            console.print("${elem_int(j, "items", 1)}")
        Err(e) -> console.print(json.decode_error_message(e))"#,
                &["witchy", "3", "20"],
            ),
        ]);
    }

    #[test]
    fn json_decode_rejects_trailing_content_backends_agree() {
        // decode must consume the whole input: trailing whitespace is fine, but
        // any trailing non-whitespace is an error (not a silently-ignored tail).
        let client = r#"
import json
fn classify(s: String) -> String:
    match json.decode(s):
        Ok(j) ->
            match json.as_int(j):
                Some(n) -> "int:" + "${n}"
                None -> "ok"
        Err(_e) -> "err"
fn main(console: Console):
    console.print(classify("[1, 2]"))
    console.print(classify("42  "))
    console.print(classify("1 2"))
    console.print(classify("true xyz"))
    console.print(classify("{}extra"))
    console.print(classify("  7"))
"#;
        let sources = [("json", crate::bundled_module("json").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "decode trailing-content diverged");
        assert_eq!(compiled, vec!["ok", "int:42", "err", "err", "err", "int:7"]);
    }

    /// std/json: `decode` rejects an overflowing exponent (BUG-241), an invalid
    /// string escape (BUG-243), a leading-zero number and a raw control character
    /// (BUG-244), and a duplicate object key (BUG-262); `float_of` accepts an
    /// integer JSON number as a Float (BUG-356), and finite JsonFloat values still
    /// encode as JSON numbers. Both backends agree.
    #[test]
    fn json_rejects_malformed_and_handles_floats_on_both_backends() {
        let src = "import json\n\
                   from json import Json\n\
                   fn dec(label: String, text: String, console: Console):\n\
                   \x20   match json.decode(text):\n\
                   \x20       Ok(j) -> console.print(label + \": \" + json.encode(j))\n\
                   \x20       Err(e) -> console.print(label + \": ERR\")\n\
                   fn main(console: Console):\n\
                   \x20   dec(\"exp_overflow\", \"1e9223372036854775808\", console)\n\
                   \x20   dec(\"exp_inf\", \"1e400\", console)\n\
                   \x20   dec(\"bad_escape\", \"\\\"a\\\\qb\\\"\", console)\n\
                   \x20   dec(\"leading_zero\", \"01\", console)\n\
                   \x20   dec(\"neg_leading_zero\", \"-01\", console)\n\
                   \x20   dec(\"zero_ok\", \"0\", console)\n\
                   \x20   dec(\"negative\", \"-3\", console)\n\
                   \x20   dec(\"fraction\", \"3.25\", console)\n\
                   \x20   dec(\"negative_fraction\", \"-0.5\", console)\n\
                   \x20   dec(\"dup_key\", \"{\\\"a\\\":1,\\\"a\\\":2}\", console)\n\
                   \x20   dec(\"exp_ok\", \"1.5e3\", console)\n\
                   \x20   dec(\"object_fraction\", \"{\\\"pi\\\": 3.25}\", console)\n\
                   \x20   dec(\"nested_roundtrip\", \"{\\\"name\\\":\\\"witchy\\\",\\\"nums\\\":[1,2,3],\\\"ok\\\":true,\\\"nil\\\":null,\\\"neg\\\":-5,\\\"nested\\\":{\\\"a\\\":[true,false]}}\", console)\n\
                   \x20   match json.float_of(JsonInt(1)):\n\
                   \x20       Ok(f) -> console.print(\"float_of_int: ${f}\")\n\
                   \x20       Err(e) -> console.print(\"float_of_int: ERR\")\n\
                   \x20   console.print(\"encode_finite: \" + json.encode(JsonFloat(1.5)))\n";
        let expected = [
            "exp_overflow: ERR",
            "exp_inf: ERR",
            "bad_escape: ERR",
            "leading_zero: ERR",
            "neg_leading_zero: ERR",
            "zero_ok: 0",
            "negative: -3",
            "fraction: 3.25",
            "negative_fraction: -0.5",
            "dup_key: ERR",
            "exp_ok: 1500.0",
            "object_fraction: {\"pi\":3.25}",
            "nested_roundtrip: {\"name\":\"witchy\",\"nums\":[1,2,3],\"ok\":true,\"nil\":null,\"neg\":-5,\"nested\":{\"a\":[true,false]}}",
            "float_of_int: 1.0",
            "encode_finite: 1.5",
        ];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// (BUG-374) JSON has no NaN/Infinity tokens, and Witchy already uses
    /// JsonNull for intentional null / Option.None. Encoding a non-finite Float
    /// must therefore be a loud boundary error, not silent data erasure to null.
    #[test]
    fn json_encode_rejects_nonfinite_floats_on_both_backends() {
        if !is_nightly() {
            return;
        }
        let cases = [
            (
                "encode_nan",
                "import json\nfrom json import Json\nfn main(console: Console):\n    console.print(json.encode(JsonFloat(0.0 / 0.0)))\n",
            ),
            (
                "encode_inf",
                "import json\nfrom json import Json\nfn main(console: Console):\n    console.print(json.encode(JsonFloat(1.0 / 0.0)))\n",
            ),
            (
                "encode_neg_inf",
                "import json\nfrom json import Json\nfn main(console: Console):\n    console.print(json.encode(JsonFloat(0.0 - (1.0 / 0.0))))\n",
            ),
            (
                "encode_nested_object",
                "import json\nfrom json import Json\nfn main(console: Console):\n    console.print(json.encode(JsonObject([(\"ratio\", JsonFloat(0.0 / 0.0))])))\n",
            ),
            (
                "stringify_reflected",
                "import json\nimport reflect\n\ntype Reading derive(Reflect):\n    ratio: Float\n\nfn main(console: Console):\n    console.print(json.stringify(Reading(0.0 / 0.0)))\n",
            ),
        ];
        for (label, src) in cases {
            let linked = resolve_std_src(src);
            typeck::check(&linked).unwrap_or_else(|e| panic!("{label} typecheck: {e}"));
            let ierr = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter must abort")
                .to_string();
            assert!(
                ierr.contains("json.encode: non-finite Float cannot be encoded as JSON"),
                "{label} interpreter mismatch: {ierr}"
            );
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered(&format!("{label}: the binary path lowers this program"));
            let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort").to_string();
            assert!(
                cerr.contains("json.encode: non-finite Float cannot be encoded as JSON"),
                "{label} compiled mismatch: {cerr}"
            );
        }

        let interpreter_only = [
            (
                "server_send_reflected",
                "import server\nfn main(console: Console):\n    let _r = server.send(200, .{ratio: 1.0 / 0.0})\n    console.print(\"unreachable\")\n",
            ),
        ];
        for (label, src) in interpreter_only {
            let linked = resolve_std_src(src);
            typeck::check(&linked).unwrap_or_else(|e| panic!("{label} typecheck: {e}"));
            let ierr = interpreter::run_module(linked, ".", Vec::new())
                .expect_err("public helper must abort before producing JSON")
                .to_string();
            assert!(
                ierr.contains("json.encode: non-finite Float cannot be encoded as JSON"),
                "{label} interpreter mismatch: {ierr}"
            );
        }
    }
