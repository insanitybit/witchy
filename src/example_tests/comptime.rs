use super::*;
use crate::typeck;

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
                    emit_item(quote item:
                        fn generated_type_shape() -> String:
                            ${meta.expr_string("record:" + meta.type_source(f.type_expr))}
                    )
                _ -> ()
        if t.name == "Choice":
            match t.kind:
                meta.TypeSum -> emit_item(quote item:
                    fn generated_sum_kind() -> String:
                        "sum"
                )
                _ -> ()
        if t.name == "Never":
            match t.kind:
                meta.TypeUninhabited -> emit_item(quote item:
                    fn generated_empty_kind() -> String:
                        "uninhabited"
                )
                _ -> ()

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
        let src = r#"import list
import meta

type UserId = String

type Box:
    value: a

type Config:
    id: UserId

comptime:
    for t in module_types:
        if t.name == "Box":
            emit_item(quote item:
                fn generated_box_params() -> String:
                    ${meta.expr_string(list.join(t.params, ","))}
            )
        if t.name == "Config":
            let f = list.at(t.fields, 0)
            emit_item(quote item:
                fn generated_config_field() -> String:
                    ${meta.expr_string(meta.type_source(f.type_expr))}
            )

fn main(console: Console):
    console.print(generated_box_params())
    console.print(generated_config_field())
"#;
        let expected = ["a", "String"];
        assert_eq!(link_run(src), expected, "interp sees normalized TypeInfo");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled code generated from normalized TypeInfo must agree",
        );
    }

    /// RFC-0167 rooted views reach `module_types` as the single lifetime-free
    /// `meta.TView` form.  A nested aggregate descriptor stays structural and
    /// `meta.type_source` can regenerate only canonical `&T` source.
    #[test]
    fn comptime_typeinfo_preserves_rooted_view_descriptors_on_both_backends() {
        let src = r#"import list
import meta

type Pair(a):
    shared: &a
    aggregate: &(a, &[a])
    metadata: b

comptime:
    for t in module_types:
        if t.name == "Pair":
            emit_item(quote item:
                fn generated_pair_params() -> String:
                    ${meta.expr_string(list.join(t.params, ","))}
            )
            let first = list.at(t.fields, 0)
            match first.type_expr:
                meta.TView(inner) ->
                    emit_item(quote item:
                        fn generated_view_relation() -> String:
                            ${meta.expr_string("view:" + meta.type_source(inner))}
                    )
                _ -> emit_item(quote item:
                    fn generated_view_relation() -> String:
                        "wrong"
                )
            let second = list.at(t.fields, 1)
            match second.type_expr:
                meta.TView(inner) ->
                    emit_item(quote item:
                        fn generated_aggregate_view_relation() -> String:
                            ${meta.expr_string("view:" + meta.type_source(inner))}
                    )
                _ -> emit_item(quote item:
                    fn generated_aggregate_view_relation() -> String:
                        "wrong"
                )

fn main(console: Console):
    console.print(generated_pair_params())
    console.print(generated_view_relation())
    console.print(generated_aggregate_view_relation())
"#;
        let expected = ["a,b", "view:a", "view:(a, &[a])"];
        assert_eq!(link_run(src), expected, "interp preserves rooted-view metadata");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled code preserves rooted-view metadata",
        );
    }

    /// (BUG-518) Built-in derives consume the same normalized TypeInfo as
    /// comptime reflection. Alias-typed fields derive the aliased decoder, and an
    /// implicit generic record derives a parameterized `Reflect` impl with bounds.
    #[test]
    fn derives_use_normalized_typeinfo_on_both_backends() {
        if !is_nightly() {
            return;
        }
        let src = "import json\nimport list\nimport reflect\n\ntype UserId = String\n\ntype Person derive(Deserialize):\n    id: UserId\n\ntype Box derive(Reflect):\n    value: a\n\nfn main(console: Console):\n    match Person.from_json(json.JsonObject([(\"id\", json.JsonString(\"ada\"))])):\n        Ok(p) -> console.print(p.id)\n        Err(e) -> console.print(json.deserialize_error_message(e))\n    match Box(3).reflect():\n        reflect.MRecord(_name, fields) -> console.print(\"${list.length(fields)}\")\n        _ -> console.print(\"bad\")\n";
        let expected = ["ada", "1"];
        assert_eq!(link_run(src), expected, "interp derive sees normalized TypeInfo");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled derive output from normalized TypeInfo must agree",
        );
    }

    /// RFC-0107 Phase 2: `derive(PublicState)` recursively proves that every
    /// field may cross a public or resumable boundary. It composes through
    /// nominal records, sums, generics, and the public collection types, then
    /// serializes through the ordinary `Reflect` representation on both
    /// backends.
    #[test]
    fn public_state_derive_is_recursive_and_has_backend_parity() {
        if !is_nightly() {
            return;
        }
        let src = r#"import json
import public_state
import reflect

type Child derive(PublicState, Reflect):
    name: String

type LoadState derive(PublicState, Reflect):
    Empty
    Ready(Child)

type Model(a) derive(PublicState, Reflect):
    count: Int
    selected: Option(a)
    children: List(Child)
    load: LoadState

fn main(console: Console):
    let model = Model(3, Some("chosen"), [Child("Ada")], Ready(Child("Grace")))
    public_state.check(model)
    console.print(json.encode(public_state.to_json(model)))
"#;
        let interpreted = link_run(src);
        let compiled = run_linked_on_wasm(&[("main", src)], "main");
        assert_eq!(compiled, interpreted, "PublicState must agree on both backends");
        assert_eq!(interpreted.len(), 1);
        assert!(interpreted[0].contains("\"count\":3"), "{interpreted:?}");
        assert!(interpreted[0].contains("\"name\":\"Ada\""), "{interpreted:?}");
        assert!(interpreted[0].contains("\"Ready\""), "{interpreted:?}");
    }

    /// `Reflect` alone is deliberately insufficient for public state. `Bytes`
    /// can be reflected for diagnostics, but has no `PublicState` proof, so a
    /// nested field fails during generated-code type checking.
    #[test]
    fn public_state_derive_rejects_non_public_nested_fields() {
        let src = "import bytes\n\
                   import public_state\n\
                   \n\
                   type Model derive(PublicState):\n\
                   \x20   payloads: List(Bytes)\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   public_state.check(Model([bytes.from_string(\"private\")]))\n";
        let linked = resolve_std_src(src);
        let error = typeck::check(&linked).expect_err("Bytes must not satisfy PublicState");
        let rendered = error.to_string();
        assert!(rendered.contains("PublicState"), "{rendered}");
        assert!(rendered.contains("Bytes"), "{rendered}");
    }

    #[test]
    fn public_state_rejects_handwritten_and_user_generated_proofs() {
        let handwritten = "import bytes\n\
                           import public_state\n\
                           \n\
                           type Forged:\n\
                           \x20   payload: Bytes\n\
                           \n\
                           impl PublicState for Forged:\n\
                           \x20   fn public_state_proof(self):\n\
                           \x20       ()\n\
                           \n\
                           fn main(console: Console):\n\
                           \x20   console.print(\"unreachable\")\n";
        let linked = resolve_std_src(handwritten);
        let error = typeck::check(&linked).expect_err("a handwritten proof must not widen PublicState");
        let rendered = error.to_string();
        assert!(rendered.contains("sealed public-boundary proof"), "{rendered}");
        assert!(rendered.contains("derive(PublicState)"), "{rendered}");

        let generated = r#"import bytes
import public_state
from meta import ItemSyntax, TypeInfo

comptime fn derive_forged(_t: TypeInfo) -> ItemSyntax:
    quote item:
        impl PublicState for Forged:
            fn public_state_proof(self):
                ()

type Forged derive(Forged):
    payload: Bytes

fn main(console: Console):
    console.print("unreachable")
"#;
        let linked = resolve_std_src(generated);
        let error =
            typeck::check(&linked).expect_err("a user-generated proof must not widen PublicState");
        let rendered = error.to_string();
        assert!(rendered.contains("sealed public-boundary proof"), "{rendered}");
        assert!(rendered.contains("derive(PublicState)"), "{rendered}");
    }

    #[test]
    fn derive_deserialize_uses_typed_json_error_on_both_backends() {
        if !is_nightly() {
            return;
        }
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
        if !is_nightly() {
            return;
        }
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
        let src = "from meta import TypeInfo, expr_int\n\ntype Point:\n    x: Int\n\ncomptime:\n    let types: List(TypeInfo) = module_types\n    var n = 0\n    for _t in types:\n        n = n + 1\n    emit_item(quote item:\n        fn generated_type_count() -> Int:\n            ${expr_int(n)}\n    )\n\nfn main(console: Console):\n    console.print(\"${generated_type_count()}\")\n";
        let expected = ["1"];
        assert_eq!(link_run(src), expected, "interp comptime sees from-imported std type");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled comptime output from from-imported std type must agree",
        );
    }

    /// RFC-0080 first slice: comptime can append a typed `meta.ItemSyntax`
    /// through `emit_item`, with a compiler-owned quote payload that is linked,
    /// typechecked, and footprint-analyzed exactly like handwritten code.
    #[test]
    fn comptime_emit_item_adds_typed_generated_items_on_both_backends() {
        let src = "comptime:\n    emit_item(quote item:\n        fn generated() -> Int:\n            42\n    )\n\nfn main(console: Console):\n    console.print(\"${generated()}\")\n";
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
        if !is_nightly() {
            return;
        }
        for (source, type_name, label) in [
            ("from meta import TypeSyntax\n\nfn leak(x: TypeSyntax) -> TypeSyntax:\n    x\n", "meta.TypeSyntax", "TypeSyntax"),
            ("import meta\n\nfn main(console: Console):\n    let id = meta.ident(\"x\")\n    console.print(\"runtime ident\")\n", "meta.Ident", "meta.ident"),
            ("from meta import ItemSyntax\n\nfn leak(x: ItemSyntax) -> ItemSyntax:\n    x\n", "meta.ItemSyntax", "ItemSyntax"),
            ("from meta import ExprSyntax\n\nfn leak(x: ExprSyntax) -> ExprSyntax:\n    x\n", "meta.ExprSyntax", "ExprSyntax"),
            ("from meta import BlockSyntax\n\nfn leak(x: BlockSyntax) -> BlockSyntax:\n    x\n", "meta.BlockSyntax", "BlockSyntax"),
            ("from meta import SyntaxHole\n\nfn leak(x: SyntaxHole) -> SyntaxHole:\n    x\n", "meta.SyntaxHole", "SyntaxHole"),
        ] {
            let err = typeck::check(&resolve_std_src(source))
                .expect_err("runtime syntax values must be rejected")
                .message;
            assert!(err.contains(type_name) && err.contains("compile-time-only"), "{label}: {err}");
        }

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
        if !is_nightly() {
            return;
        }
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
        if !is_nightly() {
            return;
        }
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
    let small_arm = meta.match_arm(meta.pattern_anon_ctor(meta.ident("Small"), []), meta.expr_string("small"))
    let big_body = meta.expr_binary("+", meta.expr_string("big:"), meta.expr_name(value))
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
        let src = "from meta import ItemSyntax\n\ncomptime fn generated_item() -> ItemSyntax:\n    quote item:\n        fn generated() -> Int:\n            77\n\ncomptime:\n    emit_item(generated_item())\n\nfn main(console: Console):\n    console.print(\"${generated()}\")\n";
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
        let comptime_src = "from meta import ItemSyntax\n\ncomptime fn make_item() -> ItemSyntax:\n    quote item:\n        pub fn generated() -> Int:\n            1\n\ncomptime:\n    emit_item(make_item())\n\nfn main(console: Console):\n    console.print(\"${generated()}\")\n";
        let expected = ["1"];
        assert_eq!(link_run(comptime_src), expected, "interp comptime local helper");
        assert_eq!(
            run_linked_on_wasm(&[("main", comptime_src)], "main"),
            expected,
            "compiled comptime local helper",
        );

        let derive_src = "from meta import ItemSyntax, TypeInfo\n\ncomptime fn generated_item() -> ItemSyntax:\n    quote item:\n        pub fn generated() -> Int:\n            2\n\ncomptime fn derive_hello(_t: TypeInfo) -> ItemSyntax:\n    generated_item()\n\ntype Marker derive(Hello):\n    value: Int\n\nfn main(console: Console):\n    console.print(\"${generated()}\")\n";
        let expected = ["2"];
        assert_eq!(link_run(derive_src), expected, "interp custom derive local helper");
        assert_eq!(
            run_linked_on_wasm(&[("main", derive_src)], "main"),
            expected,
            "compiled custom derive local helper",
        );
    }

    /// RFC-0080 custom-derive migration: a local `comptime fn derive_x` may
    /// return `ItemSyntax` or `List(ItemSyntax)` through compiler-owned quotes.
    #[test]
    fn custom_derives_can_return_typed_items_on_both_backends() {
        if !is_nightly() {
            return;
        }
        let one = "from meta import TypeInfo, ItemSyntax\n\ncomptime fn derive_hello(_t: TypeInfo) -> ItemSyntax:\n    quote item:\n        pub fn generated_one() -> Int:\n            3\n\ntype Marker derive(Hello):\n    value: Int\n\nfn main(console: Console):\n    console.print(\"${generated_one()}\")\n";
        let expected = ["3"];
        assert_eq!(link_run(one), expected, "interp typed custom derive item");
        assert_eq!(
            run_linked_on_wasm(&[("main", one)], "main"),
            expected,
            "compiled typed custom derive item",
        );

        let many = "from meta import TypeInfo, ItemSyntax\n\ncomptime fn derive_pair(_t: TypeInfo) -> List(ItemSyntax):\n    let left = quote item:\n        pub fn generated_left() -> Int:\n            4\n    let right = quote item:\n        pub fn generated_right() -> Int:\n            5\n    [left, right]\n\ntype Marker derive(Pair):\n    value: Int\n\nfn main(console: Console):\n    console.print(\"${generated_left() + generated_right()}\")\n";
        let expected = ["9"];
        assert_eq!(link_run(many), expected, "interp typed custom derive item list");
        assert_eq!(
            run_linked_on_wasm(&[("main", many)], "main"),
            expected,
            "compiled typed custom derive item list",
        );
    }
