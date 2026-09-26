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

    /// Consecutive comptime blocks are prepared as one batch, speculated from
    /// the module as it stood before the batch. A block must still see the
    /// items emitted by the blocks before it: here the second block's
    /// speculation (one type) is stale and must rerun, and the third block,
    /// re-speculated after the type exists, is served from the new batch.
    #[test]
    fn comptime_batched_blocks_see_types_emitted_by_earlier_blocks() {
        let src = r#"import meta

type Seed:
    x: Int

comptime:
    emit_item(quote item:
        type Emitted:
            y: Int
    )

comptime:
    let int = quote type:
        Int
    emit_item(meta.function(true, meta.ident("types_seen"), [], Some(int), meta.expr_int(module_types.length())))

comptime:
    let int = quote type:
        Int
    emit_item(meta.function(true, meta.ident("types_seen_again"), [], Some(int), meta.expr_int(module_types.length())))

fn main(console: Console):
    console.print("${types_seen()} ${types_seen_again()}")
"#;
        let linked = resolve_std_src(src);
        for name in ["types_seen", "types_seen_again"] {
            let body = linked
                .items
                .iter()
                .find_map(|item| match item {
                    ast::Item::Function(f) if f.name.rsplit('.').next() == Some(name) => Some(&f.body),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("`{name}` was not generated"));
            assert!(
                matches!(body.stmts.as_slice(), [ast::Stmt::Expr(ast::Expr::Int(2))]),
                "`{name}` must count Seed and Emitted: {body:?}",
            );
        }
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
