use super::*;
use crate::{parser, typeck};

    /// (BUG-341) A type error in comptime-EMITTED code must report a real, in-file
    /// location, not a phantom line number relative to the invisible emitted blob
    /// (which could point PAST the file's EOF). The emitted items' line numbers are
    /// now re-stamped to the `comptime:` block's own source line.
    #[test]
    fn comptime_body_type_error_reports_in_file_location() {
        // 6-line file; the `comptime:` block (line 1) emits `broken`, whose Bool body
        // type-errors against its declared `-> Int`. The reported line must be the
        // block's real line, within the file — never a phantom offset past EOF.
        let src = "comptime:\n    console.print(\"fn broken() -> Int:\")\n    console.print(\"    true\")\n\nfn main(console: Console):\n    console.print(\"${broken()}\")\n";
        let line_count = src.lines().count() as u32;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked)
            .expect_err("a type error in emitted code must be reported")
            .message;
        let reported: u32 = err
            .split("line ")
            .nth(1)
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("the diagnostic must carry a line number: {err}"));
        assert!(
            (1..=line_count).contains(&reported),
            "reported line {reported} must be within the {line_count}-line file, not a phantom \
             offset past EOF: {err}",
        );
    }

    /// (RFC-0069) `module_types` exposes both declaration kind and field types as
    /// structured facts. Rendering occurs only at the generated-source boundary.
    #[test]
    fn comptime_typeinfo_exposes_structured_type_expr_on_both_backends() {
        let src = r#"import list
import meta

type Config:
    values: List(Option(Int))

type Choice:
    First(Int)
    Second

type Never:

comptime:
    for t in module_types:
        if t.name == "Config":
            match t.kind:
                meta.TypeRecord ->
                    let f = list.at(t.fields, 0)
                    emit("fn generated_type_shape() -> String:")
                    emit("    \"record:" + meta.type_source(f.type_expr) + "\"")
                _ -> Nil
        if t.name == "Choice":
            match t.kind:
                meta.TypeSum -> emit("fn generated_sum_kind() -> String:\n    \"sum\"")
                _ -> Nil
        if t.name == "Never":
            match t.kind:
                meta.TypeUninhabited -> emit("fn generated_empty_kind() -> String:\n    \"uninhabited\"")
                _ -> Nil

fn main(console: Console):
    console.print(generated_type_shape())
    console.print(generated_sum_kind())
    console.print(generated_empty_kind())
"#;
        let expected = ["record:List(Option(Int))", "sum", "uninhabited"];
        assert_eq!(link_run(src), expected, "interp reads structured TypeExpr in comptime");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled code generated from structured TypeExpr must agree",
        );
    }

    /// (BUG-518) `module_types` is a normalized type fact, not a pre-typeck parse
    /// artifact: aliases are expanded and implicit record type parameters are
    /// inferred before TypeInfo reaches comptime code.
    #[test]
    fn comptime_typeinfo_normalizes_aliases_and_implicit_params_on_both_backends() {
        let src = "import list\nimport meta\n\ntype UserId = String\n\ntype Box:\n    value: a\n\ntype Config:\n    id: UserId\n\ncomptime:\n    for t in module_types:\n        if t.name == \"Box\":\n            emit(\"fn generated_box_params() -> String:\")\n            emit(\"    \\\"\" + list.join(t.params, \",\") + \"\\\"\")\n        if t.name == \"Config\":\n            let f = list.at(t.fields, 0)\n            emit(\"fn generated_config_field() -> String:\")\n            emit(\"    \\\"\" + meta.type_source(f.type_expr) + \"\\\"\")\n\nfn main(console: Console):\n    console.print(generated_box_params())\n    console.print(generated_config_field())\n";
        let expected = ["a", "String"];
        assert_eq!(link_run(src), expected, "interp sees normalized TypeInfo");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled code generated from normalized TypeInfo must agree",
        );
    }

    /// (BUG-518) Built-in derives consume the same normalized TypeInfo as
    /// comptime reflection. Alias-typed fields derive the aliased decoder, and an
    /// implicit generic record derives a parameterized `Reflect` impl with bounds.
    #[test]
    fn derives_use_normalized_typeinfo_on_both_backends() {
        let src = "import json\nimport list\nimport reflect\n\ntype UserId = String\n\ntype Person derive(Deserialize):\n    id: UserId\n\ntype Box derive(Reflect):\n    value: a\n\nfn main(console: Console):\n    match Person.from_json(json.JsonObject([(\"id\", json.JsonString(\"ada\"))])):\n        Ok(p) -> console.print(p.id)\n        Err(e) -> console.print(json.deserialize_error_message(e))\n    match Box(3).reflect():\n        reflect.MRecord(_name, fields) -> console.print(\"${list.length(fields)}\")\n        _ -> console.print(\"bad\")\n";
        let expected = ["ada", "1"];
        assert_eq!(link_run(src), expected, "interp derive sees normalized TypeInfo");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled derive output from normalized TypeInfo must agree",
        );
    }

    #[test]
    fn derive_deserialize_uses_typed_json_error_on_both_backends() {
        let src = r#"import json
import list

type Pet derive(Deserialize):
    name: String
    ages: List(Int)
    tag: Option(String)

fn via_string(j: json.Json) -> Result(Pet, String):
    let pet = Pet.from_json(j)?
    Ok(pet)

fn classify(j: json.Json) -> String:
    match Pet.from_json(j):
        Ok(p) -> p.name + ":" + "${list.length(p.ages)}"
        Err(e) ->
            match e:
                json.DeserializeMissingField(name) -> "missing:" + name
                json.DeserializeExpected(shape) -> "expected:" + shape

fn main(console: Console):
    let ok = json.JsonObject([("name", json.JsonString("kit")), ("ages", json.JsonArray([json.JsonInt(1), json.JsonInt(2)]))])
    let missing = json.JsonObject([("name", json.JsonString("kit"))])
    let wrong = json.JsonObject([("name", json.JsonString("kit")), ("ages", json.JsonString("old"))])
    console.print(classify(ok))
    console.print(classify(missing))
    console.print(classify(wrong))
    match via_string(missing):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
"#;
        let expected = [
            "kit:2",
            "missing:ages",
            "expected:an array",
            "missing field `ages`",
        ];
        assert_eq!(link_run(src), expected, "interp: typed derived JSON errors");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: typed derived JSON errors",
        );
    }

    /// (RFC-0046/RFC-0069) A constructor expression carries its concrete generic
    /// arguments into generated free helpers. Direct methods (`Box(3).reflect()`)
    /// already had a receiver to bind; helpers such as `show.render(Box(3))` and
    /// `json.stringify(Box(3))` need the constructor itself to resolve as
    /// `Box<Int>`, not the bare generic head `Box`.
    #[test]
    fn generic_constructor_calls_specialize_generated_helpers_on_both_backends() {
        let src = "import json\nimport reflect\nimport show\n\ntype Box derive(Reflect, Show):\n    value: a\n\nfn main(console: Console):\n    console.print(show.render(Box(3)))\n    console.print(json.stringify(Box(3)))\n";
        let expected = ["Box(3)", "{\"value\":3}"];
        assert_eq!(link_run(src), expected, "interp generated helpers keep constructor type args");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled generated helpers keep constructor type args",
        );
    }

    /// (BUG-533) A `comptime:` block runs in a synthetic std-only module, but it
    /// must keep the source module's std `from X import Y` bindings. Otherwise
    /// comptime code is a smaller import language than ordinary Witchy source.
    #[test]
    fn comptime_preserves_std_from_imports_on_both_backends() {
        let src = "from meta import TypeInfo\n\ntype Point:\n    x: Int\n\ncomptime:\n    let types: List(TypeInfo) = module_types\n    var n = 0\n    for _t in types:\n        n = n + 1\n    emit(\"fn generated_type_count() -> Int:\")\n    emit(\"    ${n}\")\n\nfn main(console: Console):\n    console.print(\"${generated_type_count()}\")\n";
        let expected = ["1"];
        assert_eq!(link_run(src), expected, "interp comptime sees from-imported std type");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled comptime output from from-imported std type must agree",
        );
    }

    /// RFC-0080 first slice: comptime can append a typed `meta.ItemSyntax`
    /// through `emit_item`, so the compiler-facing generation boundary is no
    /// longer only `emit(String)`. The payload is still source-backed in this
    /// slice, then parsed/typechecked/footprint-analyzed exactly like handwritten
    /// code.
    #[test]
    fn comptime_emit_item_adds_typed_generated_items_on_both_backends() {
        let src = "comptime:\n    let generated = item(\"fn generated() -> Int:\\n    42\")\n    emit_item(generated)\n\nfn main(console: Console):\n    console.print(\"${generated()}\")\n";
        let expected = ["42"];
        assert_eq!(link_run(src), expected, "interp emits typed item");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled emits typed item",
        );
    }

    /// RFC-0080 second slice: built-in derive generators return `ItemSyntax`
    /// directly. Source assembly still lives in `std/meta`, but the public
    /// generator boundary is no longer `String`.
    #[test]
    fn builtin_derive_generators_return_typed_items_on_both_backends() {
        let src = "import show\nimport meta\n\ntype Point:\n    x: Int\n\ncomptime:\n    for t in module_types:\n        if t.name == \"Point\":\n            let generated: ItemSyntax = meta.derive_show(t)\n            emit_item(generated)\n\nfn main(console: Console):\n    console.print(\"${Point(7)}\")\n";
        let expected = ["Point(7)"];
        assert_eq!(link_run(src), expected, "interp emits builtin typed derive item");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled emits builtin typed derive item",
        );
    }

    /// RFC-0080 fourth slice: compiler syntax values are compile-time-only. The
    /// public migration seam is `emit_item(ItemSyntax)` inside `comptime:`, not
    /// ordinary runtime construction or storage of `meta.*Syntax`.
    #[test]
    fn item_syntax_is_compile_time_only_outside_comptime() {
        let runtime_value = "import meta\n\nfn main(console: Console):\n    let generated = meta.item(\"fn hidden() -> Int:\\n    1\")\n    console.print(\"runtime item\")\n";
        let err = typeck::check(&resolve_std_src(runtime_value))
            .expect_err("runtime meta.item must be rejected")
            .message;
        assert!(err.contains("meta.ItemSyntax") && err.contains("compile-time-only"), "got: {err}");

        let runtime_type_signature = "from meta import TypeSyntax\n\nfn leak(x: TypeSyntax) -> TypeSyntax:\n    x\n";
        let err = typeck::check(&resolve_std_src(runtime_type_signature))
            .expect_err("runtime signatures must not expose TypeSyntax")
            .message;
        assert!(err.contains("meta.TypeSyntax") && err.contains("compile-time-only"), "got: {err}");

        let runtime_ident_value = "import meta\n\nfn main(console: Console):\n    let id = meta.ident(\"x\")\n    console.print(\"runtime ident\")\n";
        let err = typeck::check(&resolve_std_src(runtime_ident_value))
            .expect_err("runtime meta.ident must be rejected")
            .message;
        assert!(err.contains("meta.Ident") && err.contains("compile-time-only"), "got: {err}");

        let runtime_signature = "from meta import ItemSyntax\n\nfn leak(x: ItemSyntax) -> ItemSyntax:\n    x\n";
        let err = typeck::check(&resolve_std_src(runtime_signature))
            .expect_err("runtime signatures must not expose ItemSyntax")
            .message;
        assert!(err.contains("meta.ItemSyntax") && err.contains("compile-time-only"), "got: {err}");

        let runtime_expr_signature = "from meta import ExprSyntax\n\nfn leak(x: ExprSyntax) -> ExprSyntax:\n    x\n";
        let err = typeck::check(&resolve_std_src(runtime_expr_signature))
            .expect_err("runtime signatures must not expose ExprSyntax")
            .message;
        assert!(err.contains("meta.ExprSyntax") && err.contains("compile-time-only"), "got: {err}");

        let runtime_block_signature = "from meta import BlockSyntax\n\nfn leak(x: BlockSyntax) -> BlockSyntax:\n    x\n";
        let err = typeck::check(&resolve_std_src(runtime_block_signature))
            .expect_err("runtime signatures must not expose BlockSyntax")
            .message;
        assert!(err.contains("meta.BlockSyntax") && err.contains("compile-time-only"), "got: {err}");

        let runtime_hole_signature = "from meta import SyntaxHole\n\nfn leak(x: SyntaxHole) -> SyntaxHole:\n    x\n";
        let err = typeck::check(&resolve_std_src(runtime_hole_signature))
            .expect_err("runtime signatures must not expose SyntaxHole")
            .message;
        assert!(err.contains("meta.SyntaxHole") && err.contains("compile-time-only"), "got: {err}");

        let local_runtime_type = "type ItemSyntax:\n    value: Int\n\nfn main(console: Console):\n    let x = ItemSyntax(11)\n    console.print(\"${x.value}\")\n";
        let expected = ["11"];
        assert_eq!(link_run(local_runtime_type), expected, "local type name is ordinary");
        assert_eq!(
            run_linked_on_wasm(&[("main", local_runtime_type)], "main"),
            expected,
            "compiled local type name is ordinary",
        );

        let local_ident_type = "type Ident:\n    value: Int\n\nfn main(console: Console):\n    let x = Ident(12)\n    console.print(\"${x.value}\")\n";
        let expected = ["12"];
        assert_eq!(link_run(local_ident_type), expected, "local Ident type is ordinary");
        assert_eq!(
            run_linked_on_wasm(&[("main", local_ident_type)], "main"),
            expected,
            "compiled local Ident type is ordinary",
        );
    }

    /// RFC-0080 seventh/eighth slices: source-backed syntax builders make
    /// generated item structure explicit, and identifier positions take validated
    /// `meta.Ident` values even before full quotation and hygiene land.
    #[test]
    fn meta_syntax_builders_emit_function_items_on_both_backends() {
        let src = "import meta\n\nfn plus_one(x: Int) -> Int:\n    x + 1\n\ncomptime:\n    let x = meta.ident(\"x\")\n    let int = meta.type_named(meta.ident(\"Int\"), [])\n    emit_item(meta.function(true, meta.ident(\"generated\"), [meta.param(x, int)], Some(int), meta.expr_call(meta.expr_name(meta.ident(\"plus_one\")), [meta.expr_name(x)])))\n\nfn main(console: Console):\n    console.print(\"${generated(7)}\")\n";
        let expected = ["8"];
        assert_eq!(link_run(src), expected, "interp syntax-builder generated item");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled syntax-builder generated item",
        );

        let invalid = "import meta\n\ncomptime:\n    emit_item(meta.function(true, meta.ident(\"bad-name\"), [], None, meta.expr_int(1)))\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let err = try_link_std(invalid).expect_err("invalid generated identifier must fail during comptime expansion");
        assert!(err.contains("meta.ident") && err.contains("bad-name"), "got: {err}");
    }

    /// RFC-0080 ninth slice: generated function bodies and matches can be
    /// assembled from source-backed statement, block, pattern, and match-arm
    /// values instead of falling back to one whole-body string template.
    #[test]
    fn meta_body_and_pattern_builders_emit_block_items_on_both_backends() {
        let src = r#"import meta

type Size = .[Small | Big(Int)]

fn classify(n: Int) -> Size:
    if n < 10:
        .Small
    else:
        .Big(n)

comptime:
    let n = meta.ident("n")
    let value = meta.ident("value")
    let int = meta.type_named(meta.ident("Int"), [])
    let string_ty = meta.type_named(meta.ident("String"), [])
    let small_arm = meta.match_arm(meta.pattern_anon_ctor(meta.ident("Small"), []), meta.expr_raw("\"small\""))
    let big_body = meta.expr_raw("\"big:\" + \"$" + "{value}\"")
    let big_arm = meta.match_arm(meta.pattern_anon_ctor(meta.ident("Big"), [meta.pattern_var(value)]), big_body)
    let matched = meta.expr_match(meta.expr_call(meta.expr_name(meta.ident("classify")), [meta.expr_name(n)]), [small_arm, big_arm])
    let body = meta.block([meta.stmt_let(false, n, Some(int), meta.expr_int(12))], Some(matched))
    emit_item(meta.function_block(true, meta.ident("generated"), [], Some(string_ty), body))

fn main(console: Console):
    console.print(generated())
"#;
        let expected = ["big:12"];
        assert_eq!(link_run(src), expected, "interp block/pattern syntax-builder generated item");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled block/pattern syntax-builder generated item",
        );

        let empty_body = r#"import meta

comptime:
    emit_item(meta.function_block(false, meta.ident("bad"), [], None, meta.block([], None)))

fn main(console: Console):
    console.print("x")
"#;
        let err = try_link_std(empty_body).expect_err("empty generated block must fail during comptime expansion");
        assert!(err.contains("meta.block") && err.contains("body"), "got: {err}");
    }

    /// RFC-0080 fifth slice: `comptime fn` is a compile-time-only helper form.
    /// It can return compiler syntax values to `comptime:` blocks, and is removed
    /// before the runtime module is linked/type-checked.
    #[test]
    fn comptime_fn_helpers_emit_typed_items_and_do_not_escape_runtime() {
        let src = "comptime fn generated_item() -> ItemSyntax:\n    item(\"fn generated() -> Int:\\n    77\")\n\ncomptime:\n    emit_item(generated_item())\n\nfn main(console: Console):\n    console.print(\"${generated()}\")\n";
        let expected = ["77"];
        assert_eq!(link_run(src), expected, "interp comptime fn typed helper");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled comptime fn typed helper",
        );

        let runtime_call = "comptime fn hidden() -> String:\n    \"nope\"\n\nfn main(console: Console):\n    console.print(hidden())\n";
        match try_link_std(runtime_call) {
            Ok(linked) => {
                let err = typeck::check(&linked)
                    .expect_err("runtime code must not call a stripped comptime fn")
                    .message;
                assert!(err.contains("hidden"), "got: {err}");
            }
            Err(err) => assert!(err.contains("hidden"), "got: {err}"),
        }
    }

    /// (BUG-180) Comptime is isolated from authority, but not from pure local
    /// helper code. A block can call a same-module generator helper, and a custom
    /// derive routes through the same reachable helper closure.
    #[test]
    fn comptime_and_custom_derives_can_call_local_helpers_on_both_backends() {
        let comptime_src = "fn make_source() -> String:\n    \"pub fn generated() -> Int:\\n    1\"\n\ncomptime:\n    emit(make_source())\n\nfn main(console: Console):\n    console.print(\"${generated()}\")\n";
        let expected = ["1"];
        assert_eq!(link_run(comptime_src), expected, "interp comptime local helper");
        assert_eq!(
            run_linked_on_wasm(&[("main", comptime_src)], "main"),
            expected,
            "compiled comptime local helper",
        );

        let derive_src = "from meta import TypeInfo\n\nfn derive_hello(t: TypeInfo) -> String:\n    \"pub fn generated() -> Int:\\n    2\"\n\ntype Marker derive(Hello):\n    value: Int\n\nfn main(console: Console):\n    console.print(\"${generated()}\")\n";
        let expected = ["2"];
        assert_eq!(link_run(derive_src), expected, "interp custom derive local helper");
        assert_eq!(
            run_linked_on_wasm(&[("main", derive_src)], "main"),
            expected,
            "compiled custom derive local helper",
        );
    }

    /// RFC-0080 custom-derive migration: a local `comptime fn derive_x` may
    /// return `ItemSyntax` or `List(ItemSyntax)`. Legacy string-returning derives
    /// remain supported by the previous test.
    #[test]
    fn custom_derives_can_return_typed_items_on_both_backends() {
        let one = "from meta import TypeInfo, ItemSyntax\n\ncomptime fn derive_hello(t: TypeInfo) -> ItemSyntax:\n    item(\"pub fn generated_one() -> Int:\\n    3\")\n\ntype Marker derive(Hello):\n    value: Int\n\nfn main(console: Console):\n    console.print(\"${generated_one()}\")\n";
        let expected = ["3"];
        assert_eq!(link_run(one), expected, "interp typed custom derive item");
        assert_eq!(
            run_linked_on_wasm(&[("main", one)], "main"),
            expected,
            "compiled typed custom derive item",
        );

        let many = "from meta import TypeInfo, ItemSyntax\n\ncomptime fn derive_pair(t: TypeInfo) -> List(ItemSyntax):\n    [item(\"pub fn generated_left() -> Int:\\n    4\"), item(\"pub fn generated_right() -> Int:\\n    5\")]\n\ntype Marker derive(Pair):\n    value: Int\n\nfn main(console: Console):\n    console.print(\"${generated_left() + generated_right()}\")\n";
        let expected = ["9"];
        assert_eq!(link_run(many), expected, "interp typed custom derive item list");
        assert_eq!(
            run_linked_on_wasm(&[("main", many)], "main"),
            expected,
            "compiled typed custom derive item list",
        );
    }
