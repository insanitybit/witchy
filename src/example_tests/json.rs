use super::*;
use crate::{ast, codegen, interpreter, typeck};

fn assert_json_backends(src: &str, expected: &[&str], label: &str) {
    let expected: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(link_run(src), expected, "{label}: interpreter");
    assert_eq!(wasm_run(src), expected, "{label}: compiled WASM");
}

    /// (BUG-545) A decoded/built `JsonObject` reflects as the JSON object shape,
    /// not as the `JsonObject(...)` constructor. Its debug rendering should
    /// therefore look like an object, not like an accidental nameless record with
    /// a leading space.
    #[test]
    fn json_object_debug_renders_as_object_on_both_backends() {
        let src = "import json\nimport reflect\n\nfn main(console: Console):\n    let obj = json.JsonObject([(\"ok\", json.JsonBool(true)), (\"n\", json.JsonInt(2))])\n    let arr = json.JsonArray([json.JsonInt(1), json.JsonString(\"x\")])\n    console.print(reflect.debug(obj))\n    console.print(reflect.debug(arr))\n    console.print(json.stringify(obj))\n";
        let expected = [
            "{ ok: true, n: 2 }",
            "[1, \"x\"]",
            "{\"ok\":true,\"n\":2}",
        ];
        assert_eq!(link_run(src), expected, "interp: Json debug object shape");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Json debug object shape");
    }

    /// (BUG-483) JSON keys are arbitrary strings, so nested lookup needs an exact
    /// segment API in addition to the dotted-string convenience helper.
    #[test]
    fn json_get_in_reaches_literal_dot_keys_on_both_backends() {
        let src = "import json\n\nfn show(console: Console, v: Option(json.Json)):\n    match v:\n        Some(j) -> console.print(json.encode(j))\n        None -> console.print(\"missing\")\n\nfn main(console: Console):\n    let obj = json.JsonObject([(\"a.b\", json.JsonInt(1)), (\"a\", json.JsonObject([(\"b\", json.JsonInt(2))])), (\"\", json.JsonObject([(\"x.y\", json.JsonInt(3))]))])\n    show(console, json.get_in(obj, [\"a.b\"]))\n    show(console, json.get_path(obj, \"a.b\"))\n    show(console, json.get_in(obj, [\"\", \"x.y\"]))\n    show(console, json.get_in(obj, [\"missing.dot\"]))\n";
        let expected = ["1", "2", "3", "missing"];
        assert_eq!(link_run(src), expected, "interp: Json exact path segments");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Json exact path segments");
    }

    /// (BUG-262) JSON decode rejects duplicate object names, and the public
    /// helper/encoding boundaries must not let hand-built duplicate objects become
    /// signed or emitted wire JSON silently.
    #[test]
    fn json_duplicate_object_keys_fail_at_encoding_boundaries_on_both_backends() {
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

    #[test]
    fn rfc0054_json_decode_uses_typed_error_and_converts_to_string() {
        let src = "import json\nfrom json import Json\nimport show\n\nfn via_string() -> Result(Json, String):\n    let doc = json.decode(\"1 2\")?\n    Ok(doc)\n\nfn main(console: Console):\n    match json.decode(\"1 2\"):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) ->\n            console.print(json.decode_error_message(e))\n            console.print(show.render(e))\n    match via_string():\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(e)\n";
        let expected = [
            "unexpected trailing content at 2",
            "unexpected trailing content at 2",
            "unexpected trailing content at 2",
        ];
        assert_eq!(link_run(src), expected, "interp: typed json.DecodeError");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: typed json.DecodeError",
        );
    }

    #[test]
    fn rfc0054_server_json_body_uses_typed_error_and_string_bridge() {
        let src = r#"import json
import server
from http import Request
from json import Json

fn typed(req: Request) -> Result(Json, json.DecodeError):
    server.json_body(req)

fn via_string(req: Request) -> Result(Json, String):
    let doc = server.json_body(req)?
    Ok(doc)

fn main(console: Console):
    let good = Request("POST", "/", [], [], [], "{\"ok\":true}")
    let bad = Request("POST", "/", [], [], [], "1 2")
    match typed(good):
        Ok(doc) -> console.print(json.encode(doc))
        Err(e) -> console.print(json.decode_error_message(e))
    match typed(bad):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(json.decode_error_message(e))
    match via_string(bad):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match server.json_body_string(bad):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
"#;
        let expected = [
            "{\"ok\":true}",
            "unexpected trailing content at 2",
            "unexpected trailing content at 2",
            "unexpected trailing content at 2",
        ];
        assert_eq!(link_run(src), expected, "interp: server.json_body typed error");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: server.json_body typed error",
        );
    }

    /// `From`/`Into` reach `Json`: `impl From(a) for Json where a: Reflect` means any
    /// reflectable value converts — `x.into()` / `Json.from(x)` — and `server.send`
    /// serializes any reflectable response. Both backends.
    #[test]
    fn into_json_via_from() {
        let src = "import json\nfrom json import Json\n\nfn main(console: Console):\n    let j: Json = [1, 2, 3].into()\n    console.print(json.encode(j))\n    console.print(json.encode(Json.from((\"x\", 5))))";
        let want = vec!["[1,2,3]".to_string(), "[\"x\",5]".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// ANONYMOUS STRUCTS: `.{ field: expr, … }` is an ad-hoc reflectable record (a
    /// generic synthetic type carrying `derive(Reflect)`), so `json.stringify(.{…})`
    /// works on any field types — including a `List` of tuples — with no per-type
    /// boilerplate. Fields render in sorted order; `.{…}` round-trips through fmt.
    #[test]
    fn anonymous_structs_reflect_to_json() {
        let src = "import json\n\nfn main(console: Console):\n    let files = [(\"a\", \"x\"), (\"b\", \"y\")]\n    console.print(json.stringify(.{files: files}))\n    console.print(json.stringify(.{name: \"acme\", count: 5}))\n";
        let want: Vec<String> = [
            "{\"files\":[[\"a\",\"x\"],[\"b\",\"y\"]]}",
            "{\"count\":5,\"name\":\"acme\"}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        assert!(
            crate::format::reformat(src).unwrap().contains(".{files: files}"),
            "`.{{…}}` round-trips through fmt"
        );
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

    /// RECURSIVE-TYPE REFLECTION: a self-referential type (`Tree` with a `List(Tree)`
    /// arm) reflects + serializes without the monomorphizer overflowing, and an
    /// already-built `Json` reflects to its own value so it embeds verbatim inside an
    /// anonymous struct. Both backends. (The compiler's scope-name encoder is depth-
    /// guarded so recursive-type reflection can never stack-overflow the compiler.)
    #[test]
    fn recursive_types_and_json_reflect() {
        let tree = "import json\nimport reflect\n\ntype Tree derive(Reflect):\n    Leaf(Int)\n    Node(List(Tree))\n\nfn main(console: Console):\n    console.print(json.stringify(Node([Leaf(1), Node([Leaf(2)])])))\n";
        let tw = vec!["{\"$variant\":\"Node\",\"$values\":[[{\"$variant\":\"Leaf\",\"$values\":[1]},{\"$variant\":\"Node\",\"$values\":[[{\"$variant\":\"Leaf\",\"$values\":[2]}]]}]]}".to_string()];
        assert_eq!(link_run(tree), tw, "interpreter (tree)");
        assert_eq!(wasm_run(tree), tw, "wasm (tree)");
        // An already-built Json embeds verbatim in an anonymous struct.
        let embed = "import json\nfrom json import Json\n\nfn main(console: Console):\n    let rec: Json = json.decode(\"{\\\"a\\\":1}\").unwrap_or(JsonNull)\n    console.print(json.stringify(.{record: rec, ok: true}))";
        let ew = vec!["{\"ok\":true,\"record\":{\"a\":1}}".to_string()];
        assert_eq!(link_run(embed), ew, "interpreter (embed)");
        assert_eq!(wasm_run(embed), ew, "wasm (embed)");
    }

    /// COMPTIME REFLECTION (typeInfo, Phase 1 / Path 2a): a `comptime:` block reads
    /// its module's type structure via `module_types` and GENERATES a specialized
    /// `to_json` per record — direct field access, no runtime `Mirror`, written in
    /// pure witchy. This is Zig-style comptime-over-types proven end-to-end, both
    /// backends (comptime runs at link time, so the generated code is identical).
    #[test]
    fn comptime_typeinfo_generates_specialized_to_json() {
        let src = r#"import meta
import json
from json import Json

type Point:
    x: Int
    y: Int

type User:
    name: String
    age: Int
    active: Bool

comptime:
    let ctor = fn(ty: meta.TypeExpr) -> String:
        match ty:
            meta.TNamed(name, _args) ->
                if name == "Int": "JsonInt"
                else if name == "String": "JsonString"
                else if name == "Bool": "JsonBool"
                else: "JsonNull"
            _ -> "JsonNull"
    for t in module_types:
        match t.kind:
            meta.TypeRecord ->
                emit("fn to_json_${t.name}(v: ${t.name}) -> Json:")
                var pairs = []
                for f in t.fields:
                    list.push(pairs, "(\"" + f.name + "\", " + ctor(f.type_expr) + "(v." + f.name + "))")
                emit("    JsonObject([" + list.join(pairs, ", ") + "])")
                emit("")
            _ -> Nil

fn main(console: Console):
    console.print(json.encode(to_json_Point(Point(1, 2))))
    console.print(json.encode(to_json_User(User("ann", 30, true))))"#;
        let want: Vec<String> = [
            "{\"x\":1,\"y\":2}",
            "{\"name\":\"ann\",\"age\":30,\"active\":true}",
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

    /// `std/json` typed field accessors: `get_string`/`get_int`/`get_strings`/
    /// `index_string` compose `get`/`index` with the `as_*` coercions — collapsing
    /// the common "read a typed field" pattern, and yielding `[]` for an absent
    /// string array.
    #[test]
    fn json_module_typed_field_accessors() {
        let src = r#"import json

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
"#;
        assert_eq!(link_run(src), vec!["acme", "7", "Net,Console", "[]"]);
    }

    #[test]
    fn json_encode_pretty_backends_agree() {
        let client = r#"
import json
from json import Json
fn main(console: Console):
    let doc = JsonObject([("name", JsonString("witchy")), ("tags", JsonArray([JsonInt(1), JsonInt(2)])), ("empty", JsonArray([]))])
    console.print(json.encode_pretty(doc))"#;
        assert_json_backends(
            client,
            &["{\n  \"name\": \"witchy\",\n  \"tags\": [\n    1,\n    2\n  ],\n  \"empty\": []\n}"],
            "encode_pretty",
        );
    }

    #[test]
    fn json_as_object_backends_agree() {
        // as_object exposes an object's key/value pairs for iteration when the
        // keys aren't known ahead of time; a non-object yields None.
        let client = r#"
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
    console.print(if option.is_none(json.as_object(JsonInt(5))): "none" else: "some")"#;
        assert_json_backends(client, &["a", "b", "none"], "as_object");
    }

    #[test]
    fn json_merge_and_has_key_backends_agree() {
        // merge is a shallow override (b wins per-key; a's other keys kept; a
        // non-object b replaces wholesale); has_key checks top-level presence.
        let client = r#"
import json
from json import Json
fn main(console: Console):
    let a = JsonObject([("name", JsonString("a")), ("x", JsonInt(1))])
    let b = JsonObject([("x", JsonInt(2)), ("y", JsonInt(3))])
    console.print(json.encode(json.merge(a, b)))
    console.print(json.encode(json.merge(a, JsonInt(9))))
    console.print(if json.contains_key(a, "x"): "T" else: "F")
    console.print(if json.contains_key(a, "z"): "T" else: "F")
    console.print(if json.contains_key(JsonInt(5), "x"): "T" else: "F")"#;
        assert_json_backends(
            client,
            &["{\"name\":\"a\",\"x\":2,\"y\":3}", "9", "T", "F", "F"],
            "json.merge/has_key",
        );
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

    #[test]
    fn json_long_fraction_does_not_wrap_backends_agree() {
        // BUG-241: the fractional tail used to fold digits into an i64
        // (`frac * 10 + digit`), so a long input-controlled fraction wrapped to a
        // wrong value (`0.<20 nines>` parsed as ~0.0776). It now folds over the
        // digit span into a Float like the integer part, so a long fraction rounds
        // to the nearest double instead of wrapping. Identical on both backends.
        let client = r#"
import json
fn rt(s: String) -> String:
    match json.decode(s):
        Ok(j) -> json.encode(j)
        Err(e) -> "err:" + json.decode_error_message(e)
fn main(console: Console):
    console.print(rt("0.99999999999999999999"))
    console.print(rt("1.99999999999999999999999999999999999999"))
    console.print(rt("0.1234567890123456789"))
    console.print(rt("3.14159"))
"#;
        let want: Vec<String> =
            ["1.0000000000000002", "2.0", "0.1234567890123457", "3.14159"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(link_run(client), want, "interpreter");
        assert_eq!(wasm_run(client), want, "wasm");
    }

    #[test]
    fn std_json_get_path_backends_agree() {
        let client = r#"
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
        Err(e) -> console.print(json.decode_error_message(e))"#;
        assert_json_backends(client, &["witchy", "1", "none"], "std json get_path");
    }

    #[test]
    fn std_json_accessors_backends_agree() {
        let client = r#"
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
        Err(e) -> console.print(json.decode_error_message(e))"#;
        assert_json_backends(client, &["witchy", "3", "20"], "std json accessors");
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
