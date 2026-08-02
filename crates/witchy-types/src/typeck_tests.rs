    use super::*;

    #[test]
    fn typed_module_rebuilds_address_keyed_facts_after_structural_rewrite() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        fn tail(module: &witchy_syntax::ast::Module) -> &Expr {
            let Item::Function(main) = &module.items[0] else {
                panic!("expected main function")
            };
            let Some(Stmt::Expr(expr)) = main.body.stmts.last() else {
                panic!("expected tail expression")
            };
            expr
        }

        let module = witchy_syntax::parser::parse_module("fn value() -> Int:\n    1\n")
            .expect("parse");
        let typed = annotate(module);
        assert_eq!(typed.table().type_of(tail(typed.module())), Some(&Ty::Int));

        let typed = typed.rewrite_and_reannotate(|_, module| {
            let Item::Function(main) = &mut module.items[0] else {
                panic!("expected main function")
            };
            *main.body.stmts.last_mut().expect("tail statement") =
                Stmt::Expr(Expr::Str("now a string".into()));
            main.ret = Some(witchy_syntax::ast::Type::Named("String".into(), Vec::new()));
        });

        assert_eq!(
            typed.table().type_of(tail(typed.module())),
            Some(&Ty::String)
        );
    }

    #[test]
    fn anonymous_union_injection_uses_expected_type() {
        check_str(
            "fn bad(port: Int) -> .[BadPort(Int) | NotFound]:\n    .BadPort(port)\n\nfn missing() -> .[BadPort(Int) | NotFound]:\n    .NotFound\n\nfn parse(port: Int) -> Result(Int, .[BadPort(Int) | NotFound]):\n    if port < 0:\n        Err(.BadPort(port))\n    else:\n        Ok(port)\n"
        )
        .expect("expected type supplies the anonymous union shape");

        let bare = check_str("fn f():\n    .BadPort(70000)\n").unwrap_err();
        assert!(bare.contains("needs an expected anonymous union type"), "{bare}");

        let wrong_tag = check_str(
            "fn f() -> .[BadPort(Int) | NotFound]:\n    .Timeout\n"
        )
        .unwrap_err();
        assert!(wrong_tag.contains("has no tag `.Timeout`"), "{wrong_tag}");

        let wrong_arity = check_str(
            "fn f() -> .[BadPort(Int) | NotFound]:\n    .BadPort\n"
        )
        .unwrap_err();
        assert!(wrong_arity.contains("takes 1 payload value"), "{wrong_arity}");
    }

    #[test]
    fn anonymous_union_patterns_check_closed_tags() {
        check_str(
            "fn describe(e: .[BadPort(Int) | Missing(String) | NotFound]) -> String:\n    match e:\n        .BadPort(p) -> \"${p}\"\n        .Missing(k) -> k\n        .NotFound -> \"missing\"\n\nfn nested(r: Result(Int, .[BadPort(Int) | Missing(String) | NotFound])) -> String:\n    match r:\n        Ok(n) -> \"${n}\"\n        Err(.BadPort(p)) -> \"${p}\"\n        Err(.Missing(k)) -> k\n        Err(.NotFound) -> \"missing\"\n"
        )
        .expect("anonymous union patterns check against their scrutinee");

        check_str(
            "fn only(e: .[Only(Int)]) -> Int:\n    let .Only(n) = e\n    n\n"
        )
        .expect("a single-tag anonymous union pattern is irrefutable");

        let wrong_tag = check_str(
            "fn describe(e: .[BadPort(Int) | NotFound]) -> String:\n    match e:\n        .Timeout -> \"timeout\"\n        _ -> \"other\"\n"
        )
        .unwrap_err();
        assert!(wrong_tag.contains("has no tag `.Timeout`"), "{wrong_tag}");

        let wrong_arity = check_str(
            "fn describe(e: .[BadPort(Int) | NotFound]) -> String:\n    match e:\n        .BadPort -> \"bad\"\n        _ -> \"other\"\n"
        )
        .unwrap_err();
        assert!(wrong_arity.contains("takes 1 payload pattern"), "{wrong_arity}");

        let missing = check_str(
            "fn describe(e: .[BadPort(Int) | NotFound]) -> String:\n    match e:\n        .BadPort(p) -> \"${p}\"\n"
        )
        .unwrap_err();
        assert!(missing.contains("non-exhaustive match") && missing.contains("`.NotFound`"), "{missing}");
    }

    #[test]
    fn anonymous_union_widening_is_only_at_directed_sites() {
        let let_widening = check_str(
            "fn small(n: Int) -> .[B(Int) | C]:\n    .B(n)\n\nfn f() -> .[A | B(Int) | C]:\n    let s = small(1)\n    let b: .[A | B(Int) | C] = s\n    b\n"
        )
        .unwrap_err();
        assert!(let_widening.contains("declared `"), "{let_widening}");
    }

    #[test]
    fn anonymous_record_spread_updates_existing_shape() {
        let added = check_str(
            "fn main(console: Console):\n    let p = .{x: 1}\n    let q = .{y: 2, ..p}\n    console.print(\"${q.x}\")\n"
        )
        .unwrap_err();
        assert!(added.contains("has no field `y`"), "{added}");

        let duplicate = check_str(
            "fn main(console: Console):\n    let p = .{x: 1}\n    let q = .{x: 2, x: 3, ..p}\n    console.print(\"${q.x}\")\n"
        )
        .unwrap_err();
        assert!(duplicate.contains("field `x` is set twice"), "{duplicate}");
    }

    #[test]
    fn structural_types_reject_capabilities_at_every_depth() {
        let union = check_str(
            "fn bad(net: Net) -> .[Got(Net) | Missing]:\n    .Got(net)\n"
        )
        .unwrap_err();
        assert!(union.contains("anonymous union") && union.contains("Net"), "{union}");

        let record = check_str(
            "fn read(r: .{net: Net}) -> Int:\n    0\n"
        )
        .unwrap_err();
        assert!(record.contains("anonymous record") && record.contains("Net"), "{record}");

        let inferred_record = check_str(
            "fn capture(net: Net):\n    .{net: net}\n"
        )
        .unwrap_err();
        assert!(
            inferred_record.contains("anonymous record") && inferred_record.contains("Net"),
            "{inferred_record}"
        );

        let wrapped_host_cap = check_str(
            "capability Holder:\n    net: Net\n\nfn capture(h: Holder) -> .[Wrapped(Holder)]:\n    .Wrapped(h)\n"
        )
        .unwrap_err();
        assert!(
            wrapped_host_cap.contains("anonymous union") && wrapped_host_cap.contains("Holder"),
            "{wrapped_host_cap}"
        );

        let library_cap = check_str(
            "capability Ticket:\n    label: String\n\nfn capture(t: Ticket) -> .{ticket: Ticket}:\n    .{ticket: t}\n"
        )
        .unwrap_err();
        assert!(
            library_cap.contains("anonymous record") && library_cap.contains("Ticket"),
            "{library_cap}"
        );
    }

    #[test]
    fn structural_types_reject_user_impl_targets() {
        fn check_resolved(src: &str) -> Result<(), String> {
            let module = witchy_syntax::parser::parse_module(src).map_err(|e| e.to_string())?;
            let module = witchy_syntax::aliases::resolve(module).expect("resolve aliases");
            check(&module).map_err(|e| e.to_string())
        }

        let record_trait_impl = check_resolved(
            "type Row = .{x: Int}\ntrait Label:\n    fn label(self) -> String\nimpl Label for Row:\n    fn label(self) -> String:\n        \"row\"\n"
        )
        .unwrap_err();
        assert!(
            record_trait_impl.contains("anonymous record")
                && record_trait_impl.contains(".{x: Int}")
                && record_trait_impl.contains("nominal"),
            "{record_trait_impl}"
        );

        let union_trait_impl = check_resolved(
            "type LoadError = .[BadPort(Int) | Missing]\ntrait Label:\n    fn label(self) -> String\nimpl Label for LoadError:\n    fn label(self) -> String:\n        \"err\"\n"
        )
        .unwrap_err();
        assert!(
            union_trait_impl.contains("anonymous union")
                && union_trait_impl.contains(".[BadPort(Int) | Missing]")
                && union_trait_impl.contains("nominal"),
            "{union_trait_impl}"
        );

        let generic_alias_impl = check_resolved(
            "type Row(a) = .{x: a}\ntrait Label:\n    fn label(self) -> String\nimpl Label for Row(Int):\n    fn label(self) -> String:\n        \"row\"\n"
        )
        .unwrap_err();
        assert!(
            generic_alias_impl.contains("anonymous record")
                && generic_alias_impl.contains(".{x: Int}")
                && generic_alias_impl.contains("impl target"),
            "{generic_alias_impl}"
        );

        let inherent_impl = check_resolved(
            "type Row = .{x: Int}\nimpl Row:\n    fn label(self) -> String:\n        \"row\"\n"
        )
        .unwrap_err();
        assert!(
            inherent_impl.contains("anonymous record") && inherent_impl.contains("impl target"),
            "{inherent_impl}"
        );

        check_resolved(
            "type Row:\n    x: Int\ntrait Label:\n    fn label(self) -> String\nimpl Label for Row:\n    fn label(self) -> String:\n        \"row\"\nimpl Row:\n    fn value(self) -> Int:\n        self.x\n"
        )
        .expect("nominal records can still carry trait and inherent impls");
    }

    #[test]
    fn compiler_generated_structural_impls_accept_resolved_trait_identity() {
        let record_reflect = ast::ImplDef {
            origin: ast::ImplOrigin::CompilerGenerated,
            trait_name: Some("reflect.Reflect".into()),
            trait_args: Vec::new(),
            type_name: "__anon123".into(),
            target_args: Vec::new(),
            bounds: Vec::new(),
            methods: Vec::new(),
        };
        assert!(is_compiler_generated_structural_impl(&record_reflect));

        let source_lookalike = ast::ImplDef {
            origin: ast::ImplOrigin::Source,
            ..record_reflect
        };
        assert!(!is_compiler_generated_structural_impl(&source_lookalike));
    }

    #[test]
    fn packed_type_requires_packable_fields() {
        // (RFC-0027) scalars (and nested packed types) are packable.
        assert!(check_str(
            "type Point packed:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    console.print(\"${Point(1, 2).x}\")\n"
        )
        .is_ok());
        assert!(check_str(
            "type Inner packed:\n    a: Int\n\ntype Outer packed:\n    i: Inner\n    b: Bool\n\nfn main(console: Console):\n    console.print(\"hi\")\n"
        )
        .is_ok());
        // A variable-size field (String) makes the type unpackable — error naming it.
        let err = check_str(
            "type Bad packed:\n    s: String\n    n: Int\n\nfn main(console: Console):\n    console.print(\"hi\")\n"
        )
        .unwrap_err();
        assert!(err.contains("packed") && err.contains("`s`"), "{err}");
        // A non-packed record field is also unpackable.
        let nested = check_str(
            "type Plain:\n    a: Int\n\ntype Bad2 packed:\n    p: Plain\n\nfn main(console: Console):\n    console.print(\"hi\")\n"
        )
        .unwrap_err();
        assert!(nested.contains("packed") && nested.contains("`p`"), "{nested}");
    }

    #[test]
    fn grantable_capability_must_be_bare() {
        // (RFC-0038) a bare grantable cap (policy data only) is accepted.
        assert!(check_str("grantable capability UiRoot:\n    policy: String\n").is_ok());
        // Bare-but-composed: nesting another BARE user cap is still bare.
        assert!(check_str(
            "capability UiFetch:\n    scope: String\n\ngrantable capability UiRoot:\n    fetch: UiFetch\n    policy: String\n"
        )
        .is_ok());
        // A grantable cap carrying a host capability directly is rejected.
        let direct = check_str("grantable capability Bad:\n    net: Net[Connect]\n").unwrap_err();
        assert!(direct.contains("BARE") && direct.contains("Net"), "{direct}");
        // ... and transitively, through a nested user cap that wraps a host cap.
        let transitive = check_str(
            "capability Inner:\n    net: Net[Connect]\n\ngrantable capability Bad2:\n    inner: Inner\n"
        )
        .unwrap_err();
        assert!(transitive.contains("BARE") && transitive.contains("Net"), "{transitive}");
    }

    #[test]
    fn main_accepts_a_grantable_capability() {
        // (RFC-0038) a bare grantable cap may be a root parameter of `main`.
        assert!(check_str(
            "grantable capability UiRoot:\n    policy: String\n\nfn main(console: Console, ui: UiRoot):\n    console.print(\"ok\")\n"
        )
        .is_ok());
        // An ordinary (non-grantable) user type at `main` is still rejected.
        let err = check_str(
            "type Config:\n    Config(Int)\n\nfn main(console: Console, c: Config):\n    console.print(\"hi\")\n"
        )
        .unwrap_err();
        assert!(err.contains("main") && err.contains("Config"), "{err}");
        // A non-grantable *capability* (RFC-0002 refinement) is likewise not root-grantable.
        let refined = check_str(
            "capability Redis from Net[Connect]\n\nfn main(console: Console, r: Redis):\n    console.print(\"hi\")\n"
        )
        .unwrap_err();
        assert!(refined.contains("main"), "{refined}");
    }

    #[test]
    fn cap_gated_export_must_lead_with_a_grantable_cap() {
        // (RFC-0040) a grantable leading param on an `export_*` entry is accepted.
        assert!(check_str(
            "grantable capability UiRoot:\n    policy: String\n\npub fn export_step(ui: UiRoot, input: String) -> String:\n    match ui:\n        UiRoot(p) -> p + input\n"
        )
        .is_ok());
        // A non-grantable leading param on a 2-param export is rejected.
        let err = check_str(
            "type Config:\n    Config(Int)\n\npub fn export_step(c: Config, input: String) -> String:\n    input\n"
        )
        .unwrap_err();
        assert!(err.contains("export_step") && err.contains("grantable"), "{err}");
        // A migrated host capability such as `File` is not a grantable UI cap and
        // has no export-wrapper minting ABI in RFC-0005 Stage 2.
        let file = check_str(
            "pub fn export_step(f: File, input: String) -> String:\n    input\n"
        )
        .unwrap_err();
        assert!(file.contains("export_step") && file.contains("File") && file.contains("grantable"), "{file}");
        // A plain single-String export is unaffected.
        assert!(check_str("pub fn export_step(input: String) -> String:\n    input\n").is_ok());
    }

    #[test]
    fn packed_list_can_cross_a_direct_or_stored_boundary() {
        // (RFC-0111 stage 2) closed direct signatures and ordinary record fields
        // carry the exact canonical packed layout. Trait/first-class boundaries
        // remain separately guarded until their physical signature is wired.
        check_str(
            "import list\ntype P packed:\n    x: Int\nfn f(ps: List(P)) -> Int:\n    list.length(ps)\nfn main(console: Console):\n    console.print(\"hi\")\n"
        ).unwrap();
        check_str(
            "import list\ntype P packed:\n    x: Int\nfn f() -> List(P):\n    [P(1)]\nfn main(console: Console):\n    console.print(\"hi\")\n"
        ).unwrap();
        check_str(
            "type P packed:\n    x: Int\n\ntype Holder:\n    ps: List(P)\n\nfn main(console: Console):\n    console.print(\"hi\")\n"
        ).unwrap();
    }

    #[test]
    fn frozen_value_cannot_be_declared_mutable() {
        // (RFC-0025) `frozen` is deeply immutable, so it cannot also be `var`/`own`.
        let var_let = check_str(
            "fn main(console: Console):\n    var x: frozen List(Int) = [1, 2]\n    console.print(\"hi\")\n",
        )
        .unwrap_err();
        assert!(var_let.contains("frozen") && var_let.contains("var"), "{var_let}");

        // A mutator shape (`var` first + self-typed return) so RFC-0064's row-3
        // check passes and the frozen/convention conflict is what surfaces.
        let var_param =
            check_str("fn f(var xs: frozen List(Int)) -> List(Int):\n    xs\n").unwrap_err();
        assert!(var_param.contains("frozen") && var_param.contains("mutable"), "{var_param}");

        let own_param =
            check_str("fn f(own xs: frozen List(Int)) -> Int:\n    list.length(xs)\n").unwrap_err();
        assert!(own_param.contains("frozen"), "{own_param}");

        // A `let`-bound frozen value and a read-only frozen parameter are valid.
        check_str("fn f(xs: frozen List(Int)) -> Int:\n    list.length(xs)\n")
            .expect("a read-only frozen parameter is valid");
        check_str("fn main(console: Console):\n    let x: frozen List(Int) = [1, 2]\n    console.print(\"${list.length(x)}\")\n")
            .expect("a let-bound frozen value is valid");
        // `unique`/`local unique` are compatible with mutation (FBIP) — `var` is
        // fine. A mutator shape (self-typed return) satisfies RFC-0064's row-3
        // rule (a `var` receiver must be a mutator or a procedure).
        check_str("fn f(var xs: unique List(Int)) -> List(Int):\n    xs\n")
            .expect("a unique var is valid (in-place reuse is the point)");
    }

    #[test]
    fn explicit_let_may_return_a_lifetime_related_view() {
        check_str(
            "mode opt\n\nfn view(let xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n",
        )
        .expect("the output lifetime makes the explicit borrow escape safe");

        let unrelated = check_str(
            "mode opt\n\nfn bad(let xs: let('a) List(Int)) -> View(List(Int), 'b):\n    xs\n",
        )
        .expect_err("an unrelated output lifetime must not permit the escape");
        assert!(unrelated.contains("cannot be returned") || unrelated.contains("no parameter"), "{unrelated}");

        let owned = check_str("fn bad(let xs: List(Int)) -> List(Int):\n    xs\n")
            .expect_err("an ordinary explicit borrow still cannot escape");
        assert!(owned.contains("cannot be returned"), "{owned}");
    }

    #[test]
    fn borrowed_nominal_lifetime_parameters_kind_check_in_opt_mode() {
        check_str(
            "mode opt\n\ntype Parser('a):\n    input: View(Bytes, 'a)\n    offset: Int\n\nfn inspect(let parser: let('a) Parser('a)) -> Int:\n    0\n",
        )
        .expect("named-field borrowed nominal and related signature check");
        check_str(
            "mode opt\n\ntype Pair(a, 'left, 'right):\n    Pair(View(a, 'left), View(a, 'right))\n\nfn inspect(let pair: let('left) Pair(Int, 'left, 'right), let owner: let('right) Int) -> Int:\n    0\n",
        )
        .expect("single positional borrowed nominal preserves mixed type/lifetime kinds");
        check_str(
            "mode opt\n\ntype SameSpelling(a, 'a):\n    first: a\n    second: View(a, 'a)\n",
        )
        .expect("ordinary `a` and lifetime `'a` occupy distinct kinds");
    }

    #[test]
    fn borrowed_nominal_declaration_rejections_are_precise() {
        let outside_opt = check_str(
            "type Parser('a):\n    input: View(Bytes, 'a)\n",
        )
        .expect_err("borrowed nominal declarations require mode opt");
        assert!(
            outside_opt.contains("type `Parser` declares lifetime parameters")
                && outside_opt.contains("only available in a `mode opt` module"),
            "{outside_opt}"
        );

        let duplicate = check_str(
            "mode opt\n\ntype Parser('a, 'a):\n    input: View(Bytes, 'a)\n",
        )
        .expect_err("duplicate lifetime parameters reject");
        assert!(
            duplicate.contains("lifetime parameter `'a` is declared more than once in type `Parser`")
                && duplicate.contains("lifetime parameter names must be unique"),
            "{duplicate}"
        );

        let unbound_field = check_str(
            "mode opt\n\ntype Parser('a):\n    input: View(Bytes, 'other)\n",
        )
        .expect_err("field lifetime must be declared by the nominal head");
        assert!(
            unbound_field.contains("type `Parser` uses lifetime `'other` but does not declare it")
                && unbound_field.contains("add `'other` to the nominal type parameters"),
            "{unbound_field}"
        );

        let missing_head = check_str(
            "mode opt\n\ntype Parser:\n    input: View(Bytes, 'a)\n",
        )
        .expect_err("a field lifetime cannot exist without a nominal binder");
        assert!(
            missing_head.contains("type `Parser` uses lifetime `'a` but does not declare it"),
            "{missing_head}"
        );

        let unused = check_str(
            "mode opt\n\ntype Parser('a):\n    offset: Int\n",
        )
        .expect_err("declared lifetimes must relate a field");
        assert!(
            unused.contains("declares lifetime parameter `'a` but no field uses it"),
            "{unused}"
        );

        let sum = check_str(
            "mode opt\n\ntype MaybeParser('a):\n    Empty\n    Parser(View(Bytes, 'a))\n",
        )
        .expect_err("borrowed sums are outside the fixed-shell stage");
        assert!(
            sum.contains("borrowed nominal type `MaybeParser` has 2 variants")
                && sum.contains("single-variant positional types only"),
            "{sum}"
        );
    }

    #[test]
    fn borrowed_nominal_type_arguments_are_kind_checked_and_bound() {
        let ordinary_slot = check_str(
            "mode opt\n\nfn bad(let owner: let('a) Int, values: List('a)) -> Int:\n    0\n",
        )
        .expect_err("a lifetime cannot instantiate List's type parameter");
        assert!(
            ordinary_slot.contains("lifetime argument `'a` cannot be used in ordinary type position 1 of `List`")
                || ordinary_slot.contains("lifetime argument `'a` cannot be used for ordinary type parameter"),
            "{ordinary_slot}"
        );

        let lifetime_slot = check_str(
            "mode opt\n\ntype Parser('a):\n    input: View(Bytes, 'a)\n\nfn bad(parser: Parser(Int)) -> Int:\n    0\n",
        )
        .expect_err("an ordinary type cannot instantiate a lifetime parameter");
        assert!(
            lifetime_slot.contains("type `Parser` expects a lifetime argument for parameter `'a` at position 1")
                && lifetime_slot.contains("got ordinary type `Int`"),
            "{lifetime_slot}"
        );

        let unbound_use = check_str(
            "mode opt\n\ntype Parser('a):\n    input: View(Bytes, 'a)\n\nfn bad(parser: Parser('missing)) -> Int:\n    0\n",
        )
        .expect_err("a borrowed nominal application needs an input lifetime binder");
        assert!(
            unbound_use.contains("callable `bad` uses lifetime argument `'missing`")
                && unbound_use.contains("no parameter binds that lifetime"),
            "{unbound_use}"
        );
    }

    #[test]
    fn borrowed_nominal_lifetime_relations_are_not_erased_by_type_unification() {
        let error = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn relabel(let left: let('left) String, let right: let('right) String, holder: Holder('left)) -> Holder('right):\n    holder\n",
        )
        .expect_err("a borrowed nominal lifetime cannot be relabeled");
        assert!(
            error.contains("function `relabel` body")
                && error.contains("expected `'right`, found `'left`"),
            "{error}"
        );
    }

    #[test]
    fn borrowed_nominal_construction_is_gated_until_owner_root_lowering() {
        let error = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn hold(let input: let('a) String) -> Holder('a):\n    Holder(input)\n",
        )
        .expect_err("stage 1 must not expose an ordinary owning constructor");
        assert!(
            error.contains("construction of borrowed nominal type `Holder` is not available")
                && error.contains("projection-aware loans and runtime owner-root lowering"),
            "{error}"
        );
    }

    #[test]
    fn borrowed_nominals_reject_owned_container_storage_before_descriptors() {
        let error = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\ntype Bag('a):\n    holders: List(Holder('a))\n",
        )
        .expect_err("borrowed container storage belongs to RFC-0112 stage 5");
        assert!(
            error.contains("type `Bag` stores a borrowed nominal relation inside `List`")
                && error.contains("descriptor/root-lowering stage"),
            "{error}"
        );
    }

    #[test]
    fn nested_function_lifetime_does_not_satisfy_an_outer_nominal_binder() {
        let error = check_str(
            "mode opt\n\ntype Phantom('a):\n    callback: fn(View(String, 'a)) -> Int\n",
        )
        .expect_err("nested callable lifetimes are independently quantified");
        assert!(
            error.contains("type `Phantom` declares lifetime parameter `'a` but no field uses it"),
            "{error}"
        );
    }

    #[test]
    fn borrowed_nominal_containers_reject_on_every_callable_surface() {
        let cases = [
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(let owner: let('a) String, let holders: List(Holder('a))) -> Int:\n    0\n",
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\ntrait Bad:\n    fn inspect(let owner: let('a) String, let holders: List(Holder('a))) -> Int\n",
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\ntype Subject:\n    Subject\n\ntrait Inspect:\n    fn inspect(self) -> Int\n\nimpl Inspect for Subject:\n    fn inspect(self, let owner: let('a) String, let holders: List(Holder('a))) -> Int:\n        0\n",
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn outer() -> Int:\n    let bad = fn(let owner: let('a) String, let holders: List(Holder('a))) -> Int:\n        0\n    0\n",
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn local(let owner: let('a) String, let holder: Holder('a)) -> Int:\n    let holders: List(Holder('a)) = [holder]\n    0\n",
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(let callback: fn(View(String, 'a), List(Holder('a))) -> Int) -> Int:\n    0\n",
        ];
        for source in cases {
            let error = check_str(source)
                .expect_err("borrowed containers require descriptor/root lowering");
            assert!(
                error.contains("stores a borrowed nominal relation inside `List`")
                    && error.contains("descriptor/root-lowering stage"),
                "{error}"
            );
        }
    }

    #[test]
    fn borrowed_nominal_runtime_operations_reject_until_owner_root_lowering() {
        let cases = [
            "mode opt\n\ntype Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n\nfn bad(let owner: let('a) String, let cursor: Cursor('a)) -> Int:\n    cursor.offset\n",
            "mode opt\n\ntype Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n\nfn bad(let owner: let('a) String, let cursor: Cursor('a)) -> Cursor('a):\n    Cursor(offset: cursor.offset + 1, ..cursor)\n",
            "mode opt\n\ntype Holder('a):\n    Holder(View(String, 'a))\n\nfn bad(let owner: let('a) String, let holder: Holder('a)) -> View(String, 'a):\n    let Holder(view) = holder\n    view\n",
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn sink(let owner: let('a) String, let holder: Holder('a)) -> Int:\n    0\n\nfn bad(let owner: let('a) String, let holder: Holder('a)) -> Int:\n    sink(owner, holder)\n",
            "mode opt\n\ntype Cursor('a):\n    view: View(String, 'a)\n\nimpl Cursor('a):\n    fn read(self) -> Int:\n        0\n\nfn bad(let owner: let('a) String, let cursor: Cursor('a)) -> Int:\n    cursor.read()\n",
        ];
        for source in cases {
            let error = check_str(source)
                .expect_err("borrowed nominal runtime operations require owner-root lowering");
            assert!(
                error.contains("borrowed nominal type")
                    && error.contains("runtime owner-root lowering"),
                "{error}"
            );
        }
    }

    #[test]
    fn borrowed_nominal_variable_transport_rejects_until_owner_root_lowering() {
        let binding = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(let owner: let('a) String, let holder: Holder('a)) -> Int:\n    let copy = holder\n    0\n",
        )
        .expect_err("a bare binding must not copy a borrowed nominal shell");
        assert!(
            binding.contains("binding/copy into `copy`")
                && binding.contains("borrowed nominal type `Holder`")
                && binding.contains("runtime owner-root lowering"),
            "{binding}"
        );

        for source in [
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(flag: Bool, let owner: let('a) String, let holder: Holder('a)) -> Int:\n    let copy = if flag:\n        holder\n    else:\n        holder\n    0\n",
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(flag: Bool, let owner: let('a) String, let holder: Holder('a)) -> Int:\n    let copy = match flag:\n        true -> holder\n        false -> holder\n    0\n",
        ] {
            let error = check_str(source)
                .expect_err("control flow must not hide borrowed nominal storage");
            assert!(
                error.contains("binding/copy into `copy`")
                    && error.contains("borrowed nominal type `Holder`")
                    && error.contains("runtime owner-root lowering"),
                "{error}"
            );
        }

        let moved = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(let owner: let('a) String, let holder: Holder('a)) -> Int:\n    move holder\n    0\n",
        )
        .expect_err("move must not transport a borrowed nominal shell");
        assert!(
            moved.contains("unary `move`")
                && moved.contains("borrowed nominal type `Holder`")
                && moved.contains("runtime owner-root lowering"),
            "{moved}"
        );

        let propagated = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(let owner: let('a) String, let holder: Holder('a)) -> Int:\n    holder\n    0\n",
        )
        .expect_err("a bare variable expression still performs runtime transport");
        assert!(
            propagated.contains("non-tail expression statement")
                && propagated.contains("borrowed nominal type `Holder`")
                && propagated.contains("runtime owner-root lowering"),
            "{propagated}"
        );

        for source in [
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(flag: Bool, let owner: let('a) String, let holder: Holder('a)) -> Int:\n    if flag:\n        holder\n    else:\n        holder\n    0\n",
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(flag: Bool, let owner: let('a) String, let holder: Holder('a)) -> Int:\n    match flag:\n        true -> holder\n        false -> holder\n    0\n",
        ] {
            let error = check_str(source)
                .expect_err("control flow must not hide a discarded borrowed nominal value");
            assert!(
                error.contains("non-tail expression statement")
                    && error.contains("borrowed nominal type `Holder`")
                    && error.contains("runtime owner-root lowering"),
                "{error}"
            );
        }

        let mut generated_block = witchy_syntax::parser::parse_module(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(let owner: let('a) String, let holder: Holder('a)) -> Int:\n    holder\n    0\n",
        )
        .expect("parse source before generated block rewrite");
        let function = generated_block
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "bad" => Some(function),
                _ => None,
            })
            .expect("bad function");
        function.body.stmts[0] = Stmt::Expr(Expr::Block(Block {
            stmts: vec![Stmt::Expr(Expr::Var("holder".into()))],
            lines: vec![0],
            region: None,
        }));
        let error = check(&generated_block)
            .expect_err("a generated block must not hide a discarded borrowed nominal value")
            .to_string();
        assert!(
            error.contains("non-tail expression statement")
                && error.contains("borrowed nominal type `Holder`")
                && error.contains("runtime owner-root lowering"),
            "{error}"
        );

        check_str(
            "mode opt\n\nfn copy(value: Int) -> Int:\n    if true:\n        value\n    else:\n        value\n    let duplicate = if true:\n        value\n    else:\n        value\n    duplicate\n\nfn transfer(value: String) -> String:\n    move value\n\nfn inspect(let owner: let('a) String) -> Int:\n    if true:\n        owner\n    else:\n        owner\n    let projected = if true:\n        owner\n    else:\n        owner\n    0\n",
        )
        .expect("ordinary values and borrowed View projections retain binding and move behavior");
    }

    #[test]
    fn nested_function_output_lifetime_requires_a_nested_input_owner() {
        let error = check_str(
            "mode opt\n\ntype Callback:\n    Callback(fn(View(String, 'a)) -> View(String, 'b))\n",
        )
        .expect_err("a nested callable cannot manufacture an unrelated output lifetime");
        assert!(
            error.contains("uses lifetime `'b` but does not declare it")
                || error.contains("uses lifetime `'b`") && error.contains("no parameter binds"),
            "{error}"
        );
    }

    #[test]
    fn imported_borrowed_nominal_container_signature_rejects_before_lowering() {
        fn no_comptime(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let views = witchy_syntax::parser::parse_module(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n",
        )
        .expect("parse borrowed nominal module");
        let main = witchy_syntax::parser::parse_module(
            "mode opt\n\nimport views\n\nfn bad(let owner: let('a) String, let holders: List(views.Holder('a))) -> Int:\n    0\n",
        )
        .expect("parse imported borrowed container use");
        let error = crate::pipeline::link_checked(
            vec![("views".into(), views), ("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("cross-module borrowed containers stay behind the descriptor stage");
        let error = error.to_string();
        assert!(
            error.contains("stores a borrowed nominal relation inside `List`")
                && error.contains("descriptor/root-lowering stage"),
            "{error}"
        );
    }

    #[test]
    fn imported_borrowed_nominal_runtime_use_rejects_before_lowering() {
        fn no_comptime(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let views = witchy_syntax::parser::parse_module(
            "mode opt\n\ntype Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n",
        )
        .expect("parse borrowed nominal module");
        let main = witchy_syntax::parser::parse_module(
            "mode opt\n\nimport views\n\nfn bad(let owner: let('a) String, let cursor: views.Cursor('a)) -> Int:\n    cursor.offset\n",
        )
        .expect("parse imported borrowed runtime use");
        let error = crate::pipeline::link_checked(
            vec![("views".into(), views), ("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("cross-module borrowed values stay behind owner-root lowering")
        .to_string();
        assert!(
            error.contains("borrowed nominal type")
                && error.contains("runtime owner-root lowering"),
            "{error}"
        );
    }

    #[test]
    fn borrowed_nominal_runtime_guards_recurse_through_fixed_tuples() {
        let call = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn sink(let owner: let('a) String, let pair: (Holder('a), Int)) -> Int:\n    0\n\nfn bad(let owner: let('a) String, let holder: Holder('a)) -> Int:\n    sink(owner, (holder, 0))\n",
        )
        .expect_err("a fixed tuple must not hide borrowed runtime transport");
        assert!(
            call.contains("call to `sink`")
                && call.contains("borrowed nominal type `Holder`")
                && call.contains("runtime owner-root lowering"),
            "{call}"
        );

        let destructure = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(let owner: let('a) String, let pair: (Holder('a), Int)) -> Int:\n    let (holder, _) = pair\n    0\n",
        )
        .expect_err("tuple pattern destructuring must see the nested borrowed shell");
        assert!(
            destructure.contains("pattern destructuring")
                && destructure.contains("borrowed nominal type `Holder`"),
            "{destructure}"
        );

        check_str(
            "fn sink(var left: Int, var right: Int) -> Int:\n    left + right\n\nfn main() -> Int:\n    var pair = (1, 2)\n    sink(pair.0, pair.1)\n",
        )
        .expect("ordinary fixed tuples retain their existing call/write-back behavior");
    }

    #[test]
    fn borrowed_nominal_cannot_erase_into_an_owned_existential() {
        let error = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\ntrait Mark:\n    fn mark(self) -> Int\n\nimpl Mark for Holder('a):\n    fn mark(self) -> Int:\n        0\n\nfn erase(let owner: let('a) String, let holder: Holder('a)) -> dyn Mark:\n    holder as dyn Mark\n",
        )
        .expect_err("existential packing must not erase the owner relation");
        assert!(
            error.contains("erasure to `dyn Mark`")
                && error.contains("borrowed nominal type `Holder`")
                && error.contains("runtime owner-root lowering"),
            "{error}"
        );
    }

    #[test]
    fn borrowed_nominal_cannot_escape_through_an_inferred_closure() {
        let capture = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn escape(let owner: let('a) String, let holder: Holder('a)) -> Int:\n    let f = fn():\n        holder\n    0\n",
        )
        .expect_err("an inferred closure must not capture a borrowed shell");
        assert!(
            capture.contains("closure capture `holder`")
                && capture.contains("borrowed nominal type `Holder`")
                && capture.contains("cannot prove this closure non-escaping"),
            "{capture}"
        );

        let result = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn outer() -> Int:\n    let f = fn(let owner: View(String, 'a), holder: Holder('a)):\n        holder\n    0\n",
        )
        .expect_err("an inferred callable result must retain an explicit checked relation");
        assert!(
            result.contains("inferred closure result")
                && result.contains("borrowed nominal type `Holder`"),
            "{result}"
        );

        check_str(
            "fn outer() -> Int:\n    let value = 7\n    let f = fn(): value\n    0\n",
        )
        .expect("ordinary scalar closure captures remain valid");
    }

    #[test]
    fn borrowed_nominal_rejects_own_conventions_at_every_callable_surface() {
        let direct = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn consume(let owner: let('a) String, own holder: Holder('a)) -> Int:\n    0\n",
        )
        .expect_err("own must not erase a borrowed nominal relation");
        assert!(
            direct.contains("callable `consume` passes borrowed relation `Holder` to `own`")
                && direct.contains("relation-preserving `let`"),
            "{direct}"
        );

        let nested = check_str(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn bad(callback: fn(let View(String, 'a), own Holder('a)) -> Int) -> Int:\n    0\n",
        )
        .expect_err("nested callable ownership conventions must preserve relations");
        assert!(
            nested.contains("passes borrowed relation `Holder` to `own`")
                && nested.contains("relation-preserving `let`"),
            "{nested}"
        );

        check_str("fn consume(own value: String) -> Int:\n    value.length()\n")
            .expect("own remains valid for ordinary owned values");
    }

    #[test]
    fn callable_lifetime_binders_are_alpha_equivalent() {
        let source = "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n\nfn inspect(let owner: let('a) String, let holder: let('a) Holder('a)) -> Int:\n    0\n\nfn main() -> Int:\n    let same: fn(let View(String, 'a), let View(Holder('a), 'a)) -> Int = inspect\n    let renamed: fn(let View(String, 'b), let View(Holder('b), 'b)) -> Int = inspect\n    0\n";
        check_str(source)
            .expect("renaming a universally quantified callable lifetime preserves identity");

        let different_relation = check_str(
            "mode opt\n\ntype Pair('left, 'right):\n    first: View(String, 'left)\n    second: View(String, 'right)\n\nfn inspect(let left: let('left) String, let right: let('right) String, pair: Pair('left, 'left)) -> Int:\n    0\n\nfn main() -> Int:\n    let wrong: fn(let View(String, 'a), let View(String, 'b), Pair('a, 'b)) -> Int = inspect\n    0\n",
        )
        .expect_err("alpha normalization must preserve equality between relation positions");
        assert!(
            different_relation.contains("value disagrees")
                || different_relation.contains("expected"),
            "{different_relation}"
        );
    }

    #[test]
    fn borrowed_views_of_capabilities_require_a_lease_model() {
        for (name, ty) in [
            ("Console", "Console"),
            ("Clock", "Clock"),
            ("Rand", "Rand"),
            ("Dir", "Dir[Read]"),
            ("BuildOut", "BuildOut"),
        ] {
            let source = format!(
                "mode opt\n\nfn bad(let value: let('a) {ty}) -> Int:\n    0\n"
            );
            let direct = check_str(&source)
                .expect_err("ordinary lifetimes cannot borrow capabilities");
            assert!(
                direct.contains(&format!("names capability `{name}` as an ordinary owner"))
                    && direct.contains("lease-bearing API"),
                "{direct}"
            );
        }

        let branded = check_str(
            "mode opt\n\ncapability Redis from Net[Connect]\n\nfn bad(let redis: let('a) Redis) -> Int:\n    0\n",
        )
        .expect_err("user-defined capabilities also require lease-bearing borrows");
        assert!(
            branded.contains("names capability `Redis` as an ordinary owner")
                && branded.contains("lease-bearing API"),
            "{branded}"
        );

        let field = check_str(
            "mode opt\n\ntype Holder('a):\n    dir: View(Dir[Read], 'a)\n",
        )
        .expect_err("borrowed nominal fields cannot invent a capability lease");
        assert!(
            field.contains("names capability `Dir` as an ordinary owner")
                && field.contains("ordinary lifetime cannot extend capability authority"),
            "{field}"
        );

        let substituted = check_str(
            "mode opt\n\ntype Inner(a):\n    value: a\n\ntype Outer(a):\n    inner: Inner(a)\n\nfn bad(let value: let('a) Outer(Console)) -> Int:\n    0\n",
        )
        .expect_err("a realized generic field that stores a capability still requires a lease");
        assert!(
            substituted.contains("names capability `Console` as an ordinary owner")
                && substituted.contains("lease-bearing API"),
            "{substituted}"
        );

        check_str(
            "mode opt\n\ntype Callback:\n    run: fn(let Dir[Read]) -> Int\n\nfn inspect(let callback: let('a) Callback) -> Int:\n    0\n",
        )
        .expect("a callback signature may mention caller-supplied authority without storing it");

        check_str(
            "mode opt\n\ntype Phantom(a):\n    value: Int\n\nfn inspect(let value: let('a) Phantom(Dir[Read])) -> Int:\n    0\n",
        )
        .expect("an unused capability type argument does not become stored authority");

        check_str("fn inspect(let dir: Dir[Read]) -> Int:\n    0\n")
            .expect("an ordinary non-lifetime capability parameter remains valid");
    }

    #[test]
    fn local_unique_cannot_be_a_return_type() {
        // (RFC-0026) `local unique` is valid only within the call — it cannot escape,
        // so it cannot be returned.
        let ret =
            check_str("fn f() -> local unique List(Int):\n    [1, 2]\n").unwrap_err();
        assert!(ret.contains("local unique") && ret.contains("escape"), "{ret}");
        // `unique` (returnable) is fine.
        check_str("fn f() -> unique List(Int):\n    [1, 2]\n")
            .expect("a unique return is valid");
        check_str(
            "fn build() -> unique List(Int):\n    [1]\n\nfn forward() -> unique List(Int):\n    build()\n",
        )
        .expect("a direct unique result forwards its capacity token");
        let unproved = check_str(
            "fn bad(xs: List(Int)) -> unique List(Int):\n    xs\n",
        )
        .expect_err("an arbitrary shared value cannot claim a unique result token");
        assert!(unproved.contains("capacity-token proof"), "{unproved}");

        check_str(
            "fn choose(flag: Bool) -> unique List(Int):\n    if flag:\n        return [1]\n    return [2]\n",
        )
        .expect("exhaustive explicit returns each carry their own token");

        let control_tail = check_str(
            "fn choose(flag: Bool) -> unique List(Int):\n    if flag:\n        [1]\n    else:\n        [2]\n",
        )
        .expect_err("a control-flow tail must not receive a fabricated zero token");
        assert!(control_tail.contains("capacity-token proof"), "{control_tail}");

        let method = check_str(
            "type Holder:\n    values: List(Int)\n\nimpl Holder:\n    fn expose(let self: Holder) -> unique List(Int):\n        self.values\n",
        )
        .expect_err("an impl method cannot relabel a shared field as unique");
        assert!(method.contains("capacity-token proof"), "{method}");
    }

    #[test]
    fn undeclared_type_names_are_rejected() {
        // A typo'd type used to become an opaque type that mis-unified later
        // ("expected `Flarb`, found `Int`"); now it's a clear "unknown type".
        let param = check_str("fn f(x: Flarb) -> Int:\n    1\n").unwrap_err();
        assert!(param.contains("unknown type `Flarb`"), "{param}");
        // Caught in nested positions too (this used to slip through entirely).
        let nested = check_str("fn f(xs: List(Flarb)) -> Int:\n    1\n").unwrap_err();
        assert!(nested.contains("unknown type `Flarb`"), "{nested}");
        // Builtins, capability rights, generics, Option, and declared types pass.
        check_str("fn id(x: a) -> a:\n    x\n").expect("a generic parameter is valid");
        check_str("fn g(dir: Dir[Read], o: Option(Int)) -> Int:\n    0\n")
            .expect("caps with rights and Option are valid");
        check_str("type Color:\n    Red\nfn name(c: Color) -> String:\n    \"r\"\n")
            .expect("a declared type is valid");
        // A variant field referencing an unknown type is caught too.
        let field = check_str("type Wrap:\n    Wrap(Flarb)\n").unwrap_err();
        assert!(field.contains("unknown type `Flarb`"), "{field}");
        // Recursive, generic, and Option-typed fields remain valid.
        check_str("type Tree:\n    Leaf\n    Node(Tree, Int, Tree)\n").expect("recursive type is valid");
        check_str("type Box:\n    Box(a)\n").expect("generic type is valid");
    }

    #[test]
    fn type_argument_arity_is_checked() {
        let scalar = check_str("fn f(x: Int(String)) -> Int:\n    x\n").unwrap_err();
        assert!(scalar.contains("type `Int` expects 0 type argument(s) but got 1"), "{scalar}");

        let missing_list = check_str("fn f(xs: List) -> Int:\n    0\n").unwrap_err();
        assert!(missing_list.contains("type `List` expects 1 type argument(s) but got 0"), "{missing_list}");

        let extra_list = check_str("fn f(xs: List(Int, String)) -> Int:\n    0\n").unwrap_err();
        assert!(extra_list.contains("type `List` expects 1 type argument(s) but got 2"), "{extra_list}");

        let result = check_str("fn f(r: Result(Int)) -> Int:\n    0\n").unwrap_err();
        assert!(result.contains("type `Result` expects 2 type argument(s) but got 1"), "{result}");

        let dict = check_str("fn f(d: Dict(String, Int, Bool)) -> Int:\n    0\n").unwrap_err();
        assert!(dict.contains("type `Dict` expects 2 type argument(s) but got 3"), "{dict}");

        let user_missing = check_str("type Pair(a, b):\n    Pair(a, b)\nfn f(p: Pair(Int)) -> Int:\n    0\n")
            .unwrap_err();
        assert!(
            user_missing.contains("type `Pair` expects 2 type argument(s) but got 1"),
            "{user_missing}"
        );

        let user_extra = check_str("type Box:\n    Box(a)\nfn f(b: Box(Int, String)) -> Int:\n    0\n")
            .unwrap_err();
        assert!(
            user_extra.contains("type `Box` expects 1 type argument(s) but got 2"),
            "{user_extra}"
        );

        let local = check_str("fn main(console: Console):\n    let xs: List = []\n    console.print(\"bad\")\n")
            .unwrap_err();
        assert!(local.contains("type `List` expects 1 type argument(s) but got 0"), "{local}");

        check_str("fn f(xs: List(Int), r: Result(Int, String), d: Dict(String, Int)) -> Int:\n    0\n")
            .expect("valid builtin generic arities pass");
        check_str("type Pair(a, b):\n    Pair(a, b)\nfn f(p: Pair(Int, String)) -> Int:\n    0\n")
            .expect("valid explicit ADT arity passes");
        check_str("type Box:\n    Box(a)\nfn f(b: Box(Int)) -> Int:\n    0\n")
            .expect("valid inferred ADT arity passes");
        check_str("fn f(dir: Dir[Read], file: File[Read, Write], net: Net[Connect]) -> Int:\n    0\n")
            .expect("capability right markers are not ordinary type arguments");
    }

    #[test]
    fn unknown_trait_names_are_rejected() {
        let impl_head = check_str(
            "type Box:\n    Box(Int)\nimpl Missing for Box:\n    fn label(self) -> String:\n        \"x\"\n",
        )
        .unwrap_err();
        assert!(impl_head.contains("unknown trait `Missing` in impl head"), "{impl_head}");

        let supertrait = check_str("trait Local: Missing:\n    fn f(self) -> Int\n").unwrap_err();
        assert!(
            supertrait.contains("unknown trait `Missing` in trait `Local` supertrait list"),
            "{supertrait}"
        );

        let where_bound = check_str("fn f(x: a) -> Int where a: Missing:\n    0\n").unwrap_err();
        assert!(
            where_bound.contains("unknown trait `Missing` in where clause of function `f`"),
            "{where_bound}"
        );

        let impl_trait = check_str("fn f(x: impl Missing) -> Int:\n    0\n").unwrap_err();
        assert!(
            impl_trait.contains("unknown trait `Missing` in impl-trait parameter of function `f`"),
            "{impl_trait}"
        );

        let impl_bound = check_str(
            "trait Label:\n    fn label(self) -> String\ntype Box:\n    Box(a)\nimpl Label for Box(a) where a: Missing:\n    fn label(self) -> String:\n        \"x\"\n",
        )
        .unwrap_err();
        assert!(
            impl_bound.contains("unknown trait `Missing` in impl `Box` where clause"),
            "{impl_bound}"
        );

        check_str(
            "trait Label:\n    fn label(self) -> String\ntype Box:\n    Box(Int)\nimpl Label for Box:\n    fn label(self) -> String:\n        \"ok\"\nfn f(x: a) -> Int where a: Label:\n    0\n",
        )
        .expect("locally declared traits are valid in impls and bounds");
    }

    #[test]
    fn trait_type_argument_arity_is_checked() {
        let impl_missing = check_str(
            "trait From(a):\n    fn from(x: a) -> Self\n\ntype Celsius:\n    Celsius(Int)\n\nimpl From for Celsius:\n    fn from(i: Int) -> Celsius:\n        Celsius(i)\n",
        )
        .unwrap_err();
        assert!(
            impl_missing.contains("trait `From` expects 1 type argument(s) but got 0")
                && impl_missing.contains("impl head for `Celsius`"),
            "{impl_missing}"
        );

        let impl_extra = check_str(
            "trait From(a):\n    fn from(x: a) -> Self\n\ntype Celsius:\n    Celsius(Int)\n\nimpl From(Int, String) for Celsius:\n    fn from(i: Int) -> Celsius:\n        Celsius(i)\n",
        )
        .unwrap_err();
        assert!(
            impl_extra.contains("trait `From` expects 1 type argument(s) but got 2")
                && impl_extra.contains("impl head for `Celsius`"),
            "{impl_extra}"
        );

        let where_missing =
            check_str("trait From(a):\n    fn from(x: a) -> Self\n\nfn f(x: a) -> Int where a: From:\n    0\n")
                .unwrap_err();
        assert!(
            where_missing.contains("trait `From` expects 1 type argument(s) but got 0")
                && where_missing.contains("where clause of function `f`"),
            "{where_missing}"
        );

        let where_extra = check_str(
            "trait From(a):\n    fn from(x: a) -> Self\n\nfn f(x: a) -> Int where a: From(Int, String):\n    0\n",
        )
        .unwrap_err();
        assert!(
            where_extra.contains("trait `From` expects 1 type argument(s) but got 2")
                && where_extra.contains("where clause of function `f`"),
            "{where_extra}"
        );

        let impl_trait = check_str("trait From(a):\n    fn from(x: a) -> Self\n\nfn f(x: impl From) -> Int:\n    0\n")
            .unwrap_err();
        assert!(
            impl_trait.contains("trait `From` expects 1 type argument(s) but got 0")
                && impl_trait.contains("impl-trait parameter of function `f`"),
            "{impl_trait}"
        );

        check_str(
            "trait From(a):\n    fn from(x: a) -> Self\n\ntype Celsius:\n    Celsius(Int)\n\nimpl From(Int) for Celsius:\n    fn from(i: Int) -> Celsius:\n        Celsius(i)\n\nfn f(x: a) -> Int where a: From(Int):\n    0\n",
        )
        .expect("matching trait type-argument arity passes");
    }

    #[test]
    fn trait_impl_identity_includes_trait_type_arguments() {
        check_str(
            "trait From(a):\n    fn from(x: a) -> Self\n\ntype JsonError:\n    JsonError(String)\n\ntype TomlError:\n    TomlError(String)\n\nimpl From(JsonError) for String:\n    fn from(e: JsonError) -> String:\n        \"json\"\n\nimpl From(TomlError) for String:\n    fn from(e: TomlError) -> String:\n        \"toml\"\n",
        )
        .expect("same trait and target with different trait args are distinct impl heads");

        let dup = check_str(
            "trait From(a):\n    fn from(x: a) -> Self\n\ntype JsonError:\n    JsonError(String)\n\nimpl From(JsonError) for String:\n    fn from(e: JsonError) -> String:\n        \"a\"\n\nimpl From(JsonError) for String:\n    fn from(e: JsonError) -> String:\n        \"b\"\n",
        )
        .unwrap_err();
        assert!(
            dup.contains("impl `From(JsonError)` for `String` is defined more than once"),
            "{dup}"
        );
    }

    #[test]
    fn generated_name_shape_does_not_impersonate_a_from_impl() {
        let src = r#"
type LeafError:
    Leaf

type AppError:
    App

fn From__spoof__from(value: LeafError) -> AppError:
    App

fn leaf() -> Result(Int, LeafError):
    Err(Leaf)

fn wrapper() -> Result(Int, AppError):
    leaf()?
"#;
        let error = check_str(src).expect_err("a generated-looking name is not a From impl");
        assert!(error.contains("no `From("), "{error}");
    }

    #[test]
    fn trait_impls_must_match_trait_methods() {
        let misspelled = check_str(
            "type R:\n    R(Int)\ntrait PartialLike:\n    fn partial_compare(self, other: Self) -> Option(Int)\nimpl PartialLike for R:\n    fn partial_cmp(self, other: R) -> Option(Int):\n        Some(1)\nfn main(console: Console):\n    console.print(\"ok\")\n",
        )
        .unwrap_err();
        assert!(
            misspelled.contains("`partial_cmp` is not a `PartialLike` method")
                && misspelled.contains("did you mean `partial_compare`"),
            "{misspelled}"
        );

        let missing = check_str(
            "type R:\n    R(Int)\ntrait Ranked:\n    fn compare(self, other: Self) -> Int\n    fn greater(self, other: Self) -> Bool:\n        compare(self, other) > 0\nimpl Ranked for R:\n    fn greater(self, other: R) -> Bool:\n        true\n",
        )
        .unwrap_err();
        assert!(missing.contains("missing required method `compare`"), "{missing}");

        let wrong_ret = check_str(
            "type R:\n    R(Int)\ntrait Label:\n    fn label(self) -> String\nimpl Label for R:\n    fn label(self) -> Int:\n        1\n",
        )
        .unwrap_err();
        assert!(
            wrong_ret.contains("method `label` returns `Int`, but the trait requires `String`"),
            "{wrong_ret}"
        );

        let wrong_param = check_str(
            "type R:\n    R(Int)\ntrait Combine:\n    fn combine(self, other: Self) -> Int\nimpl Combine for R:\n    fn combine(self, other: Int) -> Int:\n        other\n",
        )
        .unwrap_err();
        assert!(
            wrong_param.contains("method `combine` parameter 2 has type `Int`, but the trait requires `R`"),
            "{wrong_param}"
        );

        check_str(
            "type R:\n    R(Int)\ntrait Ranked:\n    fn compare(self, other: Self) -> Int\n    fn greater(self, other: Self) -> Bool:\n        compare(self, other) > 0\nimpl Ranked for R:\n    fn compare(self, other: R) -> Int:\n        1\n    fn greater(self, other: R) -> Bool:\n        true\n",
        )
        .expect("required methods plus default overrides are valid");
    }

    #[test]
    fn closed_generic_aggregates_accept_reference_bearing_type_arguments() {
        check_str(
            "trait Label:\n    fn label(self) -> String\n\
             type Box(a):\n    value: a\n\
             impl Label for Box(a):\n    fn label(self) -> String:\n        \"Box\"\n\
             fn id(n: Int) -> Int:\n    n\n\
             fn main(console: Console):\n    let b: Box(fn(Int) -> Int) = Box(id)\n    console.print(b.label())\n",
        )
        .expect("a closed generic function field receives a concrete GC layout");

        check_str(
            "trait Label:\n    fn label(self) -> String\n\
             type Box(a):\n    value: a\n\
             impl Label for Box(a):\n    fn label(self) -> String:\n        \"Box\"\n\
             fn id(n: Int) -> Int:\n    n\n\
             fn main(console: Console):\n    let b: Box(List(fn(Int) -> Int)) = Box([id])\n    console.print(b.label())\n",
        )
        .expect("nested reference storage receives a concrete GC layout");

        check_str(
            "trait Label:\n    fn label(self) -> String\n\
             type Box(a):\n    value: a\n\
             impl Label for Box(a):\n    fn label(self) -> String:\n        \"Box\"\n\
             fn choose(n: Int, s: String) -> Int:\n    n\n\
             fn main(console: Console):\n    let b: Box(fn(Int, String) -> Int) = Box(choose)\n    console.print(b.label())\n",
        )
        .expect("multi-parameter function references receive a concrete GC layout");

        check_str(
            "type CallbackBox(a):\n    callback: fn(Int) -> Int\n    value: a\n\
             fn add1(n: Int) -> Int:\n    n + 1\n\
             fn main():\n    let boxed = CallbackBox(add1, 1)\n",
        )
        .expect("each closed generic instance receives its own mixed-field GC layout");
    }

    #[test]
    fn static_trait_methods_on_distinct_bounds_keep_receiver_identity() {
        check_str(
            "trait Named:\n    fn tag() -> String\n\
             type A:\n    A\n\
             type B:\n    B\n\
             impl Named for A:\n    fn tag() -> String:\n        \"A\"\n\
             impl Named for B:\n    fn tag() -> String:\n        \"B\"\n\
             fn pair_tags(x: a, y: b) -> String where a: Named, b: Named:\n    \"${a.tag()} ${b.tag()}\"\n\
             fn main(console: Console):\n    console.print(pair_tags(A, B))\n",
        )
        .expect("static trait methods dispatch through the bound variable, not the method name");
    }

    #[test]
    fn same_named_trait_methods_dispatch_by_trait_identity() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = "trait Label:\n    fn name(self) -> String\n\
                   trait DebugName:\n    fn name(self) -> String\n\
                   type User:\n    User(String)\n\
                   impl Label for User:\n    fn name(self) -> String:\n        \"label\"\n\
                   impl DebugName for User:\n    fn name(self) -> String:\n        \"debug\"\n\
                   fn label(x: a) -> String where a: Label:\n    name(x)\n\
                   fn debug_name(x: a) -> String where a: DebugName:\n    name(x)\n\
                   fn main(console: Console):\n    console.print(label(User(\"u\")) + debug_name(User(\"u\")))\n";
        check_str(src).expect("same-named trait methods are scoped by the active bound");

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = crate::traits::lower_checked(module).expect("lower");
        let lowered_call = |prefix: &str| -> String {
            lowered
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Function(f) if f.name.starts_with(prefix) => match f.body.stmts.last() {
                        Some(Stmt::Expr(Expr::Call { name, .. })) => Some(name.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing lowered call for {prefix}"))
        };
        assert_eq!(lowered_call("label__"), "Label__User__name");
        assert_eq!(lowered_call("debug_name__"), "DebugName__User__name");

        let ambiguous = "trait Label:\n    fn name(self) -> String\n\
                         trait DebugName:\n    fn name(self) -> String\n\
                         type User:\n    User(String)\n\
                         impl Label for User:\n    fn name(self) -> String:\n        \"label\"\n\
                         impl DebugName for User:\n    fn name(self) -> String:\n        \"debug\"\n\
                         fn bad(u: User) -> String:\n    u.name()\n";
        let err = check_str(ambiguous).unwrap_err();
        assert!(err.contains("ambiguous") && err.contains("Label") && err.contains("DebugName"), "{err}");
    }

    #[test]
    fn trait_method_value_position_names_the_lambda_fix() {
        let err = check_str(
            "trait Showy:\n    fn show(self) -> String\n\nfn main(console: Console):\n    let f = show\n    console.print(\"x\")\n",
        )
        .expect_err("trait methods are not first-class values");
        assert!(err.contains("trait method `show`"), "{err}");
        assert!(err.contains("no single function value"), "{err}");
        assert!(err.contains("fn(x): x.show()"), "{err}");
        assert!(!err.contains("unbound variable"), "{err}");
    }

    #[test]
    fn build_entrypoint_takes_only_build_capabilities() {
        // A valid build step: build caps only.
        check_str("fn build(out: BuildOut, schema: BuildRead):\n    out.write_out(\"x.witchy\", schema.read_build(\"a.proto\"))\n")
            .expect("a build step taking build caps is valid");
        // A runtime capability in `build` is rejected — the build sandbox grants
        // only build-time authority.
        let err = check_str("fn build(out: BuildOut, net: Net):\n    out.write_out(\"x\", \"y\")\n")
            .expect_err("a runtime cap in build must be rejected");
        assert!(err.contains("build step may only take build-time capabilities"), "{err}");
        // And `main` may not take a build capability.
        let err = check_str("fn main(console: Console, out: BuildOut):\n    console.print(\"no\")\n")
            .expect_err("a build cap in main must be rejected");
        assert!(err.contains("`main` may only take host capabilities"), "{err}");
        // A `build` function with no build cap is an ordinary function, not the
        // entrypoint, so it isn't subject to the build-signature rule.
        check_str("fn build(x: Int) -> Int:\n    x + 1\n")
            .expect("a plain `build` function is not the build entrypoint");
    }

    #[test]
    fn duplicate_top_level_functions_are_rejected() {
        // Two functions with the same name silently overwrote each other; now it's
        // a check-time error that names the function and (unlinked) the lines.
        let err = check_str("fn g(x: Int) -> Int:\n    1\nfn g(x: Int) -> Int:\n    2\n").unwrap_err();
        assert!(err.contains("function `g` is defined more than once"), "{err}");
        assert!(err.contains("lines 1 and 3"), "{err}");
        // Distinct names are fine.
        check_str("fn a() -> Int:\n    1\nfn b() -> Int:\n    2\n").expect("distinct names are valid");
        // Methods with the same name on different types are dispatched by receiver,
        // not duplicates — they must still type-check.
        let methods = "type A:\n    A\ntype B:\n    B\nimpl A:\n    fn tag(self) -> Int:\n        1\nimpl B:\n    fn tag(self) -> Int:\n        2\n";
        check_str(methods).expect("same-named methods on different types are not duplicates");
    }

    #[test]
    fn ambient_std_inherent_methods_do_not_become_bare_functions() {
        let methods = "impl List(a):\n    fn push(var self, x: a) -> List(a):\n        self\n";
        check_str(&format!(
            "{methods}\nfn main(console: Console):\n    var xs = [1]\n    xs.push(2)\n"
        ))
        .expect("ambient std-owned inherent methods remain available through receiver syntax");

        let err = check_str(&format!(
            "{methods}\nfn main(console: Console):\n    let xs = push([1], 2)\n"
        ))
        .expect_err("ambient std-owned inherent methods must not resurrect bare std functions");
        assert!(err.contains("`push` moved to `list.push`"), "{err}");
    }

    #[test]
    fn method_receiver_type_survives_result_propagation() {
        let src = r#"
impl List(a):
    fn contains(self, target: a) -> Bool:
        true

fn ids() -> Result(List(String), String):
    Ok(["a"])

fn has_id() -> Result(Bool, String):
    let ids = ids()?
    Ok(ids.contains("a"))

fn optional_ids() -> Option(List(String)):
    Some(["a"])

fn has_optional_id() -> Option(Bool):
    let ids = optional_ids()?
    Some(ids.contains("a"))
"#;
        check_str(src).expect("the `?` payload keeps its concrete receiver type for method lookup");
    }

    #[test]
    fn duplicate_parameter_names_are_rejected() {
        let top = check_str("fn pick(x: Int, x: Int) -> Int:\n    x\n")
            .expect_err("duplicate function parameters must be rejected");
        assert!(top.contains("parameter `x`") && top.contains("function `pick`"), "{top}");

        let lambda = check_str(
            "fn main(console: Console):\n    let f = fn(x: Int, x: Int): x\n    console.print(\"bad\")\n",
        )
        .expect_err("duplicate lambda parameters must be rejected");
        assert!(lambda.contains("parameter `x`") && lambda.contains("lambda"), "{lambda}");

        let method = check_str(
            "type Box:\n    Box(Int)\nimpl Box:\n    fn bad(self, x: Int, x: Int) -> Int:\n        x\n",
        )
        .expect_err("duplicate method parameters must be rejected");
        assert!(method.contains("parameter `x`") && method.contains("method `bad`"), "{method}");

        let trait_method = check_str("trait T:\n    fn bad(x: Int, x: Int) -> Int\n")
            .expect_err("duplicate trait method parameters must be rejected");
        assert!(
            trait_method.contains("parameter `x`") && trait_method.contains("trait method `bad`"),
            "{trait_method}"
        );
    }

    #[test]
    fn occurs_check_rejects_infinite_types() {
        // Unifying `a` with `List(a)` (the classic omega shape) must be a clear
        // check-time error, not an infinite type silently bound in the subst.
        let omega = "fn omega(x: a) -> a:\n    omega([x])\n";
        let err = check_str(omega).expect_err("infinite type must be rejected");
        assert!(err.contains("infinite type"), "got: {err}");
        // A legitimate generic that nests its argument in a list is fine when
        // the return type grows with it.
        check_str("fn wrap(x: a) -> List(a):\n    [x]\n").expect("wrap is valid");
    }

    #[test]
    fn file_capability_cannot_cross_i64_slot_boundary() {
        // (RFC-0005 §4.4/§7) `File` is an unforgeable `externref` with no boxed
        // i64-slot representation. A bare `File` param/return stays an externref;
        // `Option(File)` is represented as nullable externref. Closed
        // `Result`/`List`/tuple/nominal shapes use typed GC storage; `Dict`
        // remains reject-first until its table layout carries references.
        check_str("fn ok(console: Console, f: File):\n    console.print(\"ok\")\nfn main(console: Console, f: File):\n    ok(console, f)\n")
            .expect("a bare File param/return is a plain externref — allowed");

        check_str("fn find(console: Console, o: Option(File)):\n    console.print(\"x\")\n")
            .expect("Option(File) is nullable externref — allowed");

        check_str("fn collect(console: Console, xs: List(File)):\n    console.print(\"x\")\n")
            .expect("List(File) uses an externref-typed GC array");

        check_str("fn callbacks(console: Console, xs: List(fn(File[Read]) -> String)):\n    console.print(\"x\")\n")
            .expect("direct List(fn) uses the typed GC function array representation");

        check_str("fn open(console: Console, r: Result(File, String)):\n    console.print(\"x\")\n")
            .expect("Result(File, _) uses a closed typed GC sum");

        // Dict(String, File) — the value is slot-boxed.
        let err = check_str("fn table(console: Console, d: Dict(String, File)):\n    console.print(\"x\")\n")
            .expect_err("Dict(_, File) slot-boxes an externref value");
        assert!(err.contains("File") && err.contains("Dict"), "got: {err}");

        check_str("capability Handle:\n    f: File\n    label: String\nfn relabel(h: Handle, label: String) -> Handle:\n    match h:\n        Handle(f, _) -> Handle(f, label)\n")
            .expect("a named capability record can carry a migrated cap via GC-struct lowering");

        // (RFC-0005 stage 4, records slice) A plain structural record carries a
        // migrated cap via the SAME GC-struct lowering as a capability record —
        // never through `$mkN`/the i64 slot.
        check_str("type Handle:\n    f: File\nfn take(console: Console, h: Handle):\n    console.print(\"ok\")\n")
            .expect("a plain cap-carrying record GC-lowers");

        check_str("type Handle:\n    Handle(File, String)\nfn take(console: Console, h: Handle):\n    match h:\n        Handle(_, label) -> console.print(label)\n")
            .expect("a positional cap-carrying nominal aggregate GC-lowers");

        check_str("type Resource:\n    Missing(String)\n    Opened(File, String)\nfn take(console: Console, r: Resource):\n    match r:\n        Missing(label) -> console.print(label)\n        Opened(_, label) -> console.print(label)\n")
            .expect("a non-generic cap-carrying sum GC-lowers");

        check_str("type Good:\n    Empty\n    Files(List(File))\nfn take(console: Console, x: Good):\n    console.print(\"x\")\n")
            .expect("reference lists compose inside a closed typed GC sum");

        check_str("type MaybeBox(a):\n    Empty\n    Boxed(a)\nfn main(console: Console, f: File):\n    let x: MaybeBox(File) = Boxed(f)\n    console.print(\"x\")\n")
            .expect("a closed generic sum instantiated with File receives a concrete GC layout");

        check_str("type Left:\n    LeftEnd\n    ToRight(Right)\ntype Right:\n    RightFile(File)\n    ToLeft(Left)\nfn take(console: Console, x: Left):\n    match x:\n        LeftEnd -> console.print(\"end\")\n        ToRight(_) -> console.print(\"right\")\n")
            .expect("mutually recursive cap-carrying sums share the GC recursion group");

        check_str("fn tupled(console: Console, pair: (File, Int)):\n    console.print(\"x\")\n")
            .expect("a concrete File tuple uses typed GC-struct storage");

        check_str("fn keep(pair: (frozen File, Int)) -> (frozen File, Int):\n    pair\n")
            .expect("qualifiers preserve a concrete capability tuple's GC shape");

        check_str("fn maybe(pair: (Option(File), Int)) -> (Option(File), Int):\n    pair\n")
            .expect("a nullable direct externref remains reference-typed in a tuple");

        check_str("fn main(console: Console, f: File[Read]):\n    let read_later = fn() -> String: f.read()\n    console.print(\"x\")\n")
            .expect("a closure capture of File uses a typed GC environment");

        check_str("fn main(console: Console, f: File):\n    let xs = [f]\n    console.print(\"x\")\n")
            .expect("an inferred List(File) literal receives an externref-typed GC array");

        check_str("fn main(console: Console, f: File):\n    let pair = (f, 1)\n    console.print(\"x\")\n")
            .expect("an inferred concrete File tuple uses typed GC-struct storage");

        let err = check_str("fn id(x: a) -> a:\n    x\nfn main(console: Console, f: File):\n    let pair = id((f, 1))\n    console.print(\"x\")\n")
            .expect_err("a capability tuple cannot instantiate the scalar generic ABI");
        assert!(err.contains("generic") && err.contains("File"), "got: {err}");

        check_str("fn collect(console: Console, xs: List((File, Int))):\n    console.print(\"x\")\n")
            .expect("a list of capability tuples uses a typed GC array");

        let err = check_str("fn main(console: Console, f: File):\n    console.print(\"${(f, 1)}\")\n")
            .expect_err("rendering a capability tuple must remain forbidden");
        assert!(err.contains("render") && err.contains("File"), "got: {err}");

        let err = check_str("fn main(console: Console, f: File):\n    let pair = region -> (File, Int):\n        (f, 1)\n    console.print(\"x\")\n")
            .expect_err("region copy-out cannot yet preserve a GC tuple reference");
        assert!(err.contains("region") && err.contains("File"), "got: {err}");

        let err = check_str("type Holder:\n    Holder(File, String)\nfn main(console: Console, f: File):\n    let holder = region -> Holder:\n        Holder(f, \"x\")\n    console.print(\"x\")\n")
            .expect_err("region copy-out cannot hide a GC tuple in a nominal aggregate");
        assert!(err.contains("region") && err.contains("File"), "got: {err}");

        check_str("fn callbacks(console: Console, pair: (File, fn(File) -> String)):\n    console.print(\"x\")\n")
            .expect("fixed tuples preserve capabilities and function values as typed GC fields");

        check_str("fn callbacks(console: Console, pair: (fn(File) -> String, Int)):\n    console.print(\"x\")\n")
            .expect("a fixed tuple may carry a typed capability-bearing function signature");

        check_str("fn callback(x: Int) -> Int:\n    x\nfn main(console: Console, f: File):\n    let pair = (f, callback)\n    console.print(\"x\")\n")
            .expect("an inferred fixed tuple keeps capability and function fields typed");

        check_str("trait Good:\n    fn call(self, value: Option(fn(File) -> String)) -> Int\n")
            .expect("trait methods accept represented nullable function storage");

        check_str("trait GoodReturn:\n    fn call(self) -> Option(fn(File) -> String)\n")
            .expect("trait returns accept represented nullable function storage");

        check_str("type Box:\n    Box\nimpl Box:\n    fn call(self, value: Result(Int, fn(File) -> String)) -> Int:\n        0\n")
            .expect("impl methods accept represented closed Result storage");

        let err = check_str("type Pixel packed:\n    Pixel(Int)\ntype Box:\n    Box\nimpl Box:\n    fn pixels(self) -> List(Pixel):\n        []\n")
            .expect_err("impl method return types reject packed-list boundaries");
        assert!(err.contains("pixels") && err.contains("packed"), "got: {err}");

        check_str("fn id(x: (File, Int)) -> (File, Int):\n    x\nfn main(console: Console, f: File):\n    let local = id\n    let pair = local((f, 1))\n    console.print(\"x\")\n")
            .expect("a concrete capability tuple crosses the typed indirect-call ABI");

        check_str("fn main(console: Console, f: File):\n    let local = fn(x: File) -> (File, Int): (x, 1)\n    console.print(\"x\")\n")
            .expect("a lambda may expose a capability through its typed signature");

        let err = check_str("fn main(console: Console, f: File):\n    let d = dict.__insert(dict.new(), \"cfg\", f)\n    console.print(\"x\")\n")
            .expect_err("an inferred Dict(String, File) needs the GC-struct aggregate path");
        assert!(err.contains("generic") && err.contains("File"), "got: {err}");

        check_str("type Box(a):\n    Box(a)\nfn main(console: Console, f: File):\n    let b: Box(File) = Box(f)\n    console.print(\"x\")\n")
            .expect("a closed generic user aggregate instantiated with File receives a GC layout");

        check_str("fn get(console: Console, o: Option(Secret)):\n    console.print(\"x\")\n")
            .expect("Secret is externref-backed, so direct nullable Option(Secret) is allowed");

        check_str("fn collect(console: Console, xs: List(Secret)):\n    console.print(\"x\")\n")
            .expect("List(Secret) uses an externref-typed GC array");

        check_str("fn maybe_dir(console: Console, out: Option(Dir[Write])):\n    console.print(\"x\")\n")
            .expect("Dir is externref-backed this stage, so direct nullable Option(Dir) is allowed");

        let branded_dir = r#"
capability ConfigDir from Dir[Read]

fn load(c: ConfigDir, name: String) -> String:
    match c:
        ConfigDir(dir) -> dir.read(name)
"#;
        check_str(branded_dir)
            .expect("Dir migration is blocked on branded-cap aggregate representation");

        let branded_net = r#"
capability Redis from Net[Connect, Tcp]

fn ping(r: Redis) -> Int:
    match r:
        Redis(net) -> 1
"#;
        check_str(branded_net)
            .expect("Net migration is blocked on branded-cap aggregate representation");

        // (RFC-0005 stage 4, records slice) A PLAIN single-variant named-field
        // record may carry a migrated capability — it GC-lowers exactly like a
        // sealed `capability` record: construction, field access, spread, and
        // place assignment all type-check.
        let plain_record = r#"
type Workspace:
    dir: Dir[Read]
    label: String

fn load(w: Workspace, name: String) -> String:
    w.dir.read(name)

fn relabel(w: Workspace, label: String) -> Workspace:
    Workspace(label: label, ..w)
"#;
        check_str(plain_record).expect("plain cap-carrying records GC-lower");

        // (BUG-566) Nesting one cap-carrying record in another classifies in
        // BOTH homes (typeck + codegen consume one classifier), for `capability`
        // and plain `type` alike.
        let nested_record = r#"
type Inner:
    dir: Dir[Read]
    tag: String

type Outer:
    inner: Inner
    label: String

fn load(o: Outer, name: String) -> String:
    o.inner.dir.read(name)
"#;
        check_str(nested_record).expect("nested cap-carrying records GC-lower");

        check_str("type W:\n    dir: Dir[Read]\n    label: String\n\nfn hold(xs: List(W)) -> Int:\n    0\n")
            .expect("a list of cap-carrying records uses a typed GC array");

        check_str("type W:\n    dir: Dir[Read]\n    label: String\n\nfn f(console: Console, w: W):\n    let g = fn() -> String: w.label\n    console.print(g())\n")
            .expect("a closure captures a cap-carrying record at its exact GC kind");
    }

    #[test]
    fn region_rejects_reference_backed_capability_aggregates() {
        check_str("fn copy(file: File) -> File:\n    region -> File:\n        file\n")
            .expect("a bare File remains a direct externref across a region boundary");

        let cases = [
            (
                "List(File)",
                "fn copy(files: List(File)) -> List(File):\n    region -> List(File):\n        files\n",
            ),
            (
                "Option(File)",
                "fn copy(file: Option(File)) -> Option(File):\n    region -> Option(File):\n        file\n",
            ),
            (
                "Box(File)",
                "type Box(a):\n    Box(a)\nfn copy(box: Box(File)) -> Box(File):\n    region -> Box(File):\n        box\n",
            ),
            (
                "nested Option(List(Box(File)))",
                "type Box(a):\n    Box(a)\nfn copy(value: Option(List(Box(File)))) -> Option(List(Box(File))):\n    region -> Option(List(Box(File))):\n        value\n",
            ),
        ];
        for (shape, source) in cases {
            let error = match check_str(source) {
                Err(error) => error,
                Ok(()) => panic!("{shape} unexpectedly crossed a region boundary"),
            };
            assert!(
                error.contains("region")
                    && error.contains("reference-backed aggregate")
                    && error.contains("File"),
                "wrong diagnostic for {shape}: {error}"
            );
        }
    }

    #[test]
    fn vm_with_dir_accepts_its_typed_bare_top_level_callback() {
        fn no_comptime(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let entry = witchy_syntax::parser::parse_module(
            "import vm\n\nfn reader(dir: Dir, input: Bytes) -> Bytes:\n    input\n\nfn invoke(dir: Dir, input: Bytes) -> Bytes:\n    vm.with_dir(dir, reader, input)\n\nfn main(console: Console):\n    console.print(\"ok\")\n",
        )
        .expect("parse isolated worker callback");
        let module = witchy_syntax::linker::link(
            vec![("main".to_string(), entry)],
            "main",
            no_comptime,
        )
        .expect("link bundled vm module");
        check(&module).expect("the dedicated Dir callback adapter is type-safe");
    }

    #[test]
    fn gc_aggregate_names_use_reference_storage_classifier() {
        let module = witchy_syntax::parser::parse_module(
            r#"
type FileAlias = File[Read]

type Mixed:
    Mixed(fn(Int) -> Int, FileAlias)

type FunctionOnly:
    FunctionOnly(fn(Int) -> Int)

capability Branded from File[Read]

type Generic(a):
    Generic(a)

type Sum:
    HasFile(File[Read])
    Empty
"#,
        )
        .expect("parse aggregate storage shapes");

        assert_eq!(
            gc_cap_aggregate_names(&module),
            vec!["Mixed".to_string(), "FunctionOnly".to_string(), "Sum".to_string()],
            "fixed function/reference layouts GC-lower; transparent and generic shapes keep their separate representations"
        );
    }

    #[test]
    fn every_externref_capability_is_allowed_in_a_typed_closure_environment() {
        let cases = [
            ("Dir[Read]", "Dir"),
            ("File[Read]", "File"),
            ("Net[Connect, Tcp]", "Net"),
            ("Socket", "Socket"),
            ("Listener", "Listener"),
            ("Secret", "Secret"),
        ];
        for (ty, label) in cases {
            let src = format!(
                "fn hold(x: {ty}):\n    let later = fn():\n        let captured = x\n"
            );
            check_str(&src).unwrap_or_else(|error| {
                panic!("{ty} ({label}) should typecheck in a typed closure environment: {error}")
            });
        }

        let branded = r#"
capability Redis from Net[Connect, Tcp]

fn hold(redis: Redis):
    let later = fn():
        let captured = redis
"#;
        check_str(branded).expect("an externref brand is captured without scalar erasure");

        let nested = r#"
type Vault:
    key: Secret

fn hold(vault: Vault):
    let later = fn():
        let captured = vault
"#;
        check_str(nested).expect("a cap-carrying nominal GC reference is a valid capture");

        for ty in [
            "Option(fn(Int) -> Int)",
            "Result(fn(Int) -> Int, String)",
            "List(List(fn(Int) -> Int))",
        ] {
            check_str(&format!("fn hold(x: {ty}):\n    let captured = x\n"))
                .unwrap_or_else(|error| panic!("{ty} should have typed GC storage: {error}"));
        }
        let error = check_str(
            "fn hold(x: Dict(String, fn(Int) -> Int)):\n    let captured = x\n",
        )
        .expect_err("Dict function storage remains reject-first");
        assert!(
            error.contains("Dict") && error.contains("function"),
            "Dict produced the wrong storage diagnostic: {error}"
        );
    }

    #[test]
    fn file_capability_rights_and_narrowing() {
        // RFC-0012: `File` is a host capability `main` may receive, the leaf of the
        // Dir/File hierarchy, right-typed like `Dir`.
        check_str("fn main(console: Console, config: File[Read], log: File[Write]):\n    console.print(\"ok\")\n")
            .expect("File[Read]/File[Write] are valid main capabilities");
        // A full `File` narrows to `File[Read]` implicitly at a call boundary.
        check_str("fn ro(console: Console, f: File[Read]):\n    console.print(\"r\")\nfn main(console: Console, f: File):\n    ro(console, f)\n")
            .expect("full File satisfies a File[Read] parameter");
        // Rights are enforced: `File[Read]` cannot stand in for `File[Write]`.
        let err = check_str("fn w(console: Console, f: File[Write]):\n    console.print(\"w\")\nfn main(console: Console, f: File[Read]):\n    w(console, f)\n")
            .expect_err("File[Read] must not satisfy File[Write]");
        assert!(err.contains("File[Write]"), "got: {err}");
        // `as` drops rights but can never add them.
        check_str("fn main(console: Console, f: File):\n    let ro = f as File[Read]\n    console.print(\"ok\")\n")
            .expect("`as` can drop File rights");
        let err = check_str("fn main(console: Console, f: File[Read]):\n    let w = f as File[Write]\n    console.print(\"no\")\n")
            .expect_err("`as` cannot add File rights");
        assert!(err.contains("can only drop rights"), "got: {err}");
    }

    #[test]
    fn main_signature_is_validated_at_check_time() {
        // A non-capability `main` parameter is a check-time error (it used to slip
        // through `witchy check` and only fail when capabilities were minted).
        let bad = check_str("fn main(x: Int):\n    print_int(x)\n").unwrap_err();
        assert!(bad.contains("`main` parameter `x` has type `Int`"), "{bad}");
        assert!(bad.contains("host capabilities"), "{bad}");
        // The args parameter must be `List(String)`, not any other list.
        let bad_args = check_str("fn main(args: List(Int)):\n    print_int(0)\n").unwrap_err();
        assert!(bad_args.contains("`List(Int)`"), "{bad_args}");
        // An untyped parameter is flagged too.
        let untyped = check_str("fn main(x):\n    x\n").unwrap_err();
        assert!(untyped.contains("has no type annotation"), "{untyped}");
        // Capabilities (with or without rights) and the args list are all valid.
        check_str("fn main(console: Console, dir: Dir[Read], args: List(String)):\n    console.print(\"ok\")\n")
            .expect("capabilities + args is a valid main");
        // A module without `main` is a library and passes.
        check_str("fn helper() -> Int:\n    5\n").expect("a library is valid");
    }

    #[test]
    fn main_returning_result_or_option_is_rejected() {
        // `main` returning a `Result` used to type-check and then be SILENTLY
        // discarded by the runtime's value sink (an `Err` neither printed nor set
        // a non-zero exit). Reject it loudly instead, pointing at the fix.
        let bad = check_str(
            "fn risky() -> Result(Int, String):\n    Err(\"boom\")\nfn main(console: Console) -> Result(Int, String):\n    let v = risky()?\n    Ok(v)\n",
        )
        .unwrap_err();
        assert!(bad.contains("`main` returns `Result(Int, String)`"), "{bad}");
        assert!(bad.contains("exit code"), "{bad}");
        // `Option` is the same trap (a dropped `None`).
        let bad_opt = check_str("fn main(console: Console) -> Option(Int):\n    None\n").unwrap_err();
        assert!(bad_opt.contains("`main` returns `Option(Int)`"), "{bad_opt}");
        // Plain value returns are NOT rejected — the value sink surfaces them: an
        // `Int` exit code, a printed `Float`, an explicit `Nil`, and no annotation
        // (implicit Nil) all pass. (The `Float`-returning main is a tested feature.)
        check_str("fn main(console: Console) -> Int:\n    0\n").expect("Int exit code is valid");
        check_str("import math\nfn main() -> Float:\n    math.sqrt(4.0)\n").expect("Float main is valid");
        check_str("fn main(console: Console) -> Nil:\n    console.print(\"x\")\n")
            .expect("explicit Nil is valid");
        check_str("fn main(console: Console):\n    console.print(\"x\")\n")
            .expect("no annotation is valid");
    }

    #[test]
    fn unknown_stdlib_function_suggests_import() {
        // Calling an unimported stdlib function points at the module to import.
        let err = check_str("fn main(console: Console):\n    console.print(\"${minimum([1], 0)}\")\n")
            .expect_err("minimum is unimported");
        assert!(err.contains("import cmp"), "{err}");
        // A genuine typo (no stdlib match) gets no misleading hint.
        let typo = check_str("fn main(console: Console):\n    frobnicate()\n")
            .expect_err("frobnicate is unknown");
        assert!(!typo.contains("did you forget"), "{typo}");
        assert!(!typo.contains("did you mean"), "{typo}");
        // A near-miss of a stdlib name suggests the correction.
        let near = check_str("fn main(console: Console):\n    let ys = mep([1], fn(x: Int): x)\n    console.print(\"ok\")\n")
            .expect_err("mep is a typo of map");
        assert!(near.contains("did you mean `map`"), "{near}");
    }

    #[test]
    fn module_qualified_call_without_import_suggests_import() {
        // `json.stringify(x)` with no `import json` parses as a method call on the
        // bare name `json`; the error should point at the missing import, not talk
        // about method resolution.
        let err = check_str("fn main(console: Console):\n    console.print(json.stringify(5))\n")
            .expect_err("json is unimported");
        assert!(err.contains("import json"), "{err}");
        assert!(!err.contains("method call"), "should not mention method resolution: {err}");
    }

    #[test]
    fn unbounded_generic_ordering_suggests_ord_bound() {
        // `<` on an unbounded type parameter resolves to a type var (renders `?`);
        // the error should suggest the `where T: Ord` bound, not a bare "found `?`".
        let err = check_str("fn smallest(xs: List(a)) -> a:\n    var m = list.at(xs, 0)\n    for x in xs:\n        if x < m:\n            m = x\n    m\nfn main(console: Console):\n    console.print(\"${smallest([3, 1, 2])}\")\n")
            .expect_err("unbounded generic comparison");
        assert!(err.contains("where T: Ord"), "{err}");
    }

    #[test]
    fn ordering_a_non_ord_type_points_at_deriving_ord() {
        // `<` on a concrete type without Ord must point at deriving `Ord`, and
        // must not leak the `less` desugar name (nor mis-suggest the list
        // function `last`, which the post-desugar unknown-function path used to).
        let err = check_str("type Foo:\n    Foo(Int)\nfn main(console: Console):\n    console.print(\"${Foo(1) < Foo(2)}\")\n")
            .expect_err("Foo has no Ord");
        assert!(err.contains("Ord"), "{err}");
        assert!(!err.contains("`less`") && !err.contains("`last`"), "should not leak desugar/typo: {err}");
    }

    #[test]
    fn derive_eq_ord_rejects_float_fields() {
        let eq = check_str("import cmp\n\ntype Reading derive(PartialEq, Eq):\n    value: Float\n")
            .expect_err("Float is not Eq");
        assert!(eq.contains("derive(Eq)") && eq.contains("Float is not Eq"), "{eq}");

        let ord = check_str(
            "import cmp\n\ntype Reading derive(PartialEq, PartialOrd, Ord):\n    value: Float\n",
        )
        .expect_err("Float is not Ord");
        assert!(ord.contains("derive(Ord)") && ord.contains("Float is not Ord"), "{ord}");

        check_str("import cmp\n\ntype Reading derive(PartialEq, PartialOrd):\n    value: Float\n")
            .expect("Float supports partial equality and partial ordering");
    }

    #[test]
    fn fieldless_types_are_uninhabited_and_builtin_derives_reject() {
        // `type Marker:` is a fieldless, uninhabited type. It has no constructor,
        // so an empty match is exhaustive, but structural built-in derives must not
        // generate empty `match self:` implementations as if it were a singleton.
        check_str("type Marker:\n\nfn absurd(m: Marker) -> Int:\n    match m:\n\nfn main(console: Console):\n    console.print(\"ok\")\n")
            .expect("empty match over an uninhabited fieldless type is exhaustive");

        let value = check_str("type Marker:\n\nfn main(console: Console):\n    console.print(\"${Marker()}\")\n")
            .expect_err("a fieldless type has no constructor");
        assert!(value.contains("type `Marker` is not a value"), "{value}");

        for derive_name in ["Show", "PartialEq", "Eq", "PartialOrd", "Ord", "Reflect", "Deserialize"] {
            let src = format!("type Marker derive({derive_name}):\n\nfn main(console: Console):\n    console.print(\"ok\")\n");
            let err = check_str(&src).expect_err("built-in derives reject fieldless types");
            assert!(
                err.contains(&format!("derive({derive_name})")) && err.contains("fieldless types"),
                "{derive_name}: {err}"
            );
        }
    }

    #[test]
    fn capabilities_do_not_leak_across_kinds() {
        // Holding one capability never confers another. A function given only a
        // Console cannot reach the network or the filesystem: receiver-aware cap
        // op lowering refuses `connect`/`read` on Console before they become host
        // calls. Authority is per-kind and (with no capability constructors)
        // unforgeable — the heart of witchy's confinement guarantee.
        let net = check_str(r#"
fn f(c: Console) -> Nil:
    c.connect("host")
"#).unwrap_err();
        assert!(
            net.contains("no method `connect` on `Console`"),
            "expected a receiver-aware cap-op rejection, got: {net}"
        );
        let dir = check_str(r#"
fn f(c: Console) -> String:
    c.read("/etc/passwd")
"#)
            .unwrap_err();
        assert!(
            dir.contains("no method `read` on `Console`"),
            "expected a receiver-aware cap-op rejection, got: {dir}"
        );
    }

    #[test]
    fn rejects_wrong_arity() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main():
    double(1, 2)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("argument"));
    }

    #[test]
    fn uncalled_bounded_generic_bodies_are_checked() {
        let err = check_str(
            r#"
fn broken(x: a) -> Int where a: Ord:
    x + "oops"

fn main(console: Console):
    console.print("ok")
"#,
        )
        .expect_err("a bad generic body is rejected at declaration time");
        assert!(err.contains("function `broken`"), "{err}");
    }

    #[test]
    fn duration_is_a_distinct_type() {
        // A Duration is not an Int and cannot combine with unrelated values.
        assert!(check_str("fn f() -> Duration:\n    30s + 5\n").is_err());
        assert!(check_str("fn f() -> Int:\n    30s\n").is_err());
        assert!(check_str("fn f() -> Duration:\n    30s + true\n").is_err());
    }

    #[test]
    fn type_errors_report_function_and_source_line() {
        // The mismatch is on the third line, inside function `f`.
        let src = r#"fn f() -> Int:
    let a = 1
    (a + "x")
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("line 3"), "expected a line number, got: {e}");
        assert!(e.contains("`f`"), "expected the function name, got: {e}");
    }

    #[test]
    fn rejects_ordering_on_non_primitives() {
        // These would type-check under bare unification but crash at runtime, so
        // the checker rejects them up front.
        assert!(check_str(r#"
fn f(a: Bool, b: Bool) -> Bool:
    (a < b)
"#).is_err());
        assert!(check_str(r#"
fn f(a: List(Int), b: List(Int)) -> Bool:
    (a < b)
"#).is_err());
        assert!(check_str(r#"
fn f(a: (Int, Int), b: (Int, Int)) -> Bool:
    (a < b)
"#).is_err());
    }

    #[test]
    fn rejects_lambda_argument_type_mismatch() {
        // Passing a `fn(Int)->Int` where a `fn(String)->String` is required fails.
        let src = r#"
fn run(f: fn(String) -> String, s: String) -> String:
    f(s)

fn main(console: Console):
    console.print(run(fn(n: Int): (n + 1), "x"))
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn record_spread_base_must_match_named_record() {
        check_str(
            "type P:\n    x: Int\n    y: Int\nfn f(p: P) -> P:\n    P(x: 5, ..p)\n",
        )
        .expect("same-type record spread remains valid");

        let err = check_str(
            "type P:\n    x: Int\n    y: Int\n\
             type Big:\n    x: Int\n    y: Int\n    z: String\n\
             fn f(big: Big) -> P:\n    P(x: 5, ..big)\n",
        )
        .expect_err("record spread base must have the named record type");
        assert!(err.contains("requires a `P` base") && err.contains("found `Big`"), "{err}");
    }

    #[test]
    fn rejects_record_update_wrong_field_type() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn bad(p: Point) -> Point:
    Point(x: "no", ..p)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_record_update_unknown_field() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn bad(p: Point) -> Point:
    Point(z: 1, ..p)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn named_field_record_construction() {
        // A missing field (no spread to supply it) is rejected.
        let miss = check_str("type Point:\n    x: Int\n    y: Int\nfn a() -> Point:\n    Point(x: 1)\n").unwrap_err();
        assert!(miss.contains("missing field `y`"), "{miss}");
        // An unknown field name is rejected.
        let unknown = check_str("type Point:\n    x: Int\nfn a(p: Point) -> Point:\n    Point(nope: 1, ..p)\n").unwrap_err();
        assert!(unknown.contains("no field `nope`"), "{unknown}");
        // A name that isn't a record type is rejected.
        let notrec = check_str("fn a() -> Int:\n    Nope(x: 1)\n").unwrap_err();
        assert!(notrec.contains("not a record type"), "{notrec}");
    }

    #[test]
    fn rejects_unknown_record_field() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn f(p: Point) -> Int:
    (p).z
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_field_access_on_non_record() {
        assert!(check_str("fn f(n: Int) -> Int { n.x }").is_err());
    }

    #[test]
    fn generic_record_field_instantiates() {
        let bad = r#"
type Box:
    value: a

fn unwrap(b: Box(Int)) -> String:
    (b).value
"#;
        assert!(check_str(bad).is_err());
    }

    #[test]
    fn rejects_list_pattern_element_misuse() {
        // Binding a list element as Int then concatenating it as a String fails.
        let src = r#"
fn f(xs: List(Int)) -> String:
    match xs:
        [] -> ""
        [head, ..] -> (head + "!")
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_for_over_non_list() {
        let src = r#"
fn main(console: Console):
    for x in 5:
        console.print("x")
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_try_when_error_types_differ() {
        // `?` yields `Err(String)`, but the function returns `Result(Int, Int)`,
        // so the error types can't match.
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn src_fn() -> Result(Int, String):
    Err("x")

fn bad() -> Result(Int, Int):
    let v = (src_fn())?
    Ok(v)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_try_on_non_result() {
        // `?` on a plain Int is meaningless.
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn bad(n: Int) -> Result(Int, String):
    Ok((n)?)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_arm_after_catchall() {
        let src = r#"
fn f(n: Int) -> Int:
    match n:
        _ -> 0
        1 -> 2
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("unreachable"), "got: {e}");
    }

    #[test]
    fn rejects_duplicate_variant_arm() {
        let src = r#"
type Opt:
    Some(a)
    None

fn f(o: Opt(Int)) -> Int:
    match o:
        Some(x) -> x
        Some(y) -> y
        None -> 0
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("unreachable"), "got: {e}");
    }

    #[test]
    fn rejects_duplicate_literal_arm() {
        let src = r#"
fn f(n: Int) -> Int:
    match n:
        1 -> 1
        1 -> 2
        _ -> 0
"#;
        assert!(check_str(src).unwrap_err().contains("unreachable"));
    }

    #[test]
    fn rejects_non_exhaustive_bool_match() {
        let src = r#"
fn f(b: Bool) -> Int:
    match b:
        true -> 1
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("non-exhaustive") && e.contains("Bool"), "got: {e}");
    }

    #[test]
    fn rejects_generic_adt_type_mismatch() {
        // `Box(Int)` and `Box(String)` are distinct: passing one for the other
        // must fail to unify their type arguments.
        let src = r#"
type Box:
    Wrap(a)

fn need_int(b: Box(Int)) -> Int:
    match b:
        Wrap(n) -> n

fn main() -> Int:
    need_int(Wrap("nope"))
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_and_on_non_bool() {
        assert!(check_str("fn f() -> Bool { 1 && true }").is_err());
    }

    #[test]
    fn rejects_non_bool_if_condition() {
        let src = r#"
fn f() -> Int:
    if 1:
        2
    else:
        3
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("if") || e.contains("Bool"));
    }

    #[test]
    fn rejects_return_type_mismatch() {
        let src = r#"
fn f() -> Int:
    "not an int"
"#;
        assert!(check_str(src).is_err());
    }

    /// Capability operations are methods at the source level: old bare spellings
    /// are rejected before ordinary capability type-checking.
    #[test]
    fn rejects_bare_capability_ops() {
        let src = r#"
fn leak(s: String) -> Nil:
    print(s, s)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("method-only") && e.contains("console.print"), "got: {e}");
    }

    #[test]
    fn rejects_bare_build_capability_ops_with_catalog_fixit() {
        let src = r#"
fn build(out: BuildOut):
    write_out(out, "x.witchy", "// generated")
"#;
        let e = check_str(src).unwrap_err();
        assert!(
            e.contains("method-only") && e.contains("out.write_out(path, contents)"),
            "got: {e}"
        );
    }

    #[test]
    fn user_functions_named_like_cap_ops_remain_callable() {
        let src = r#"
fn connect(host: String, port: Int) -> String:
    "${host}:${port}"

fn read(x: Int) -> Int:
    x + 1

fn main(console: Console):
    console.print(connect("example.com", 443))
    console.print("${read(1)}")
"#;
        check_str(src).expect("user functions named like cap ops are not host capability calls");
    }

    #[test]
    fn capability_methods_keep_method_origin_after_lowering() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = r#"
fn read(f: File[Read]) -> String:
    "shadow"

fn main(console: Console, f: File[Read]):
    console.print(f.read())
"#;
        check_str(src).expect("file read capability method should type-check");

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = crate::traits::lower_checked(module).expect("lower");
        let main = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .expect("main");
        let Some(Stmt::Expr(Expr::Call { name, args })) = main.body.stmts.last() else {
            panic!("expected lowered console.print call");
        };
        assert_eq!(name, "__capop.print");
        let Some(Expr::Call { name: inner, .. }) = args.get(1) else {
            panic!("expected lowered f.read call");
        };
        assert_eq!(inner, "__capop.read");
    }

    #[test]
    fn migrated_dir_respects_slot_and_typed_higher_order_boundaries() {
        let option_dir = r#"
type Option(a):
    Some(a)
    None

fn f(dir: Dir) -> Option(Dir):
    Some(dir)
"#;
        check_str(option_dir).expect("Option(Dir) is represented as nullable externref");

        let result_dir = r#"
type Result(a, e):
    Ok(a)
    Err(e)

fn f(dir: Dir) -> Result(Dir, String):
    Ok(dir)
"#;
        check_str(result_dir).expect("a closed Result(Dir, String) uses a typed GC sum");

        let higher_order_dir = r#"
fn consume(f: fn(Dir, Bytes) -> Bytes):
    0

fn reader(dir: Dir, input: Bytes) -> Bytes:
    input

fn demo(dir: Dir):
    consume(reader)
"#;
        check_str(higher_order_dir)
            .expect("Dir-bearing function values use the typed closure ABI");
    }

    #[test]
    fn capability_methods_prefer_host_ops_over_std_owner_modules() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = r#"
fn main(rand: Rand, net: Net[Listen, Tcp]):
    rand.rand_u64()
    let listener = net.listen("127.0.0.1:0")
    listener.serve_pool()
"#;
        assert!(check_str(src).is_ok());

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = crate::traits::lower_checked(module).expect("lower");
        let main = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .expect("main");
        let Some(Stmt::Expr(Expr::Call { name: rand_name, .. })) = main.body.stmts.first() else {
            panic!("expected lowered rand.rand_u64 call");
        };
        assert_eq!(rand_name, "__capop.rand_u64");
        let Some(Stmt::Expr(Expr::Call { name: serve_name, .. })) = main.body.stmts.last() else {
            panic!("expected lowered listener.serve_pool call");
        };
        assert_eq!(serve_name, "__capop.serve_pool");
    }

    #[test]
    fn capability_op_chains_preserve_receiver_kind() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = r#"
type DirPolicy:
    Any

fn load(dir: Dir[Read], policy: DirPolicy) -> String:
    dir.only(policy).read("config.txt")
"#;
        check_str(src).expect("Dir.only(...).read(...) should type-check");

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = crate::traits::lower_checked(module).expect("lower");
        let load = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "load" => Some(f),
                _ => None,
            })
            .expect("load");
        let Some(Stmt::Expr(Expr::Call { name: read_name, args: read_args })) =
            load.body.stmts.last()
        else {
            panic!("expected lowered dir.read call");
        };
        assert_eq!(read_name, "__capop.read");
        let Some(Expr::Call { name: only_name, .. }) = read_args.first() else {
            panic!("expected lowered dir.only receiver");
        };
        assert_eq!(only_name, "__capop.only");
    }

    #[test]
    fn capability_methods_are_receiver_aware_before_lowering() {
        let src = r#"
fn main(dir: Dir):
    dir.recv_line()
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("no method `recv_line` on `Dir`"), "got: {e}");
    }

    #[test]
    fn rejects_non_exhaustive_match() {
        let src = r#"
type Event:
    Click(Int, Int)
    Closed

fn describe(e: Event) -> String:
    match e:
        Closed -> "closed"
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("non-exhaustive"), "got: {e}");
    }

    #[test]
    fn rejects_constructor_field_type_mismatch() {
        let src = r#"
type Event:
    Click(Int, Int)
    Closed

fn f() -> Event:
    Click("not an int", 2)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_assignment_to_let() {
        let src = r#"
fn main():
    let x = 1
    x = 2
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("immutable"), "got: {e}");
    }

    #[test]
    fn rejects_var_argument_that_is_immutable() {
        let src = r#"
fn bump(var n: Int):
    n = (n + 1)

fn main():
    let x = 1
    bump(x)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("var"), "got: {e}");
    }

    #[test]
    fn rejects_use_after_own_move() {
        let src = r#"
fn take(own s: String) -> String:
    s

fn main():
    let x = "hi"
    let a = take(x)
    let b = take(x)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("moved"), "got: {e}");
    }

    // (BUG-009 / RFC-0005 hardening #4) Attenuation proof for `Net` — mirrors
    // `file_capability_rights_and_narrowing`. Under the `externref` model these
    // typeck assertions ARE the runtime defense for rights/narrowing, so they are
    // load-bearing. Rejection happens at the shared check stage, upstream of both
    // the interpreter and the WASM codegen, so a rejected program fails identically
    // on both backends — the check_str assertion is the two-backend proof.
    #[test]
    fn net_capability_rights_and_narrowing() {
        // RFC-0003: a `Net`'s verbs split `Connect` (dial out) from `Listen`
        // (accept in) — a client is not a server. A connect-only handle dials.
        check_str("fn f(console: Console, net: Net[Connect]):\n    let s = net.connect(\"example.com:443\")\n    console.print(\"ok\")\n")
            .expect("Net[Connect] can connect");
        // ... but it cannot `listen`.
        let err = check_str("fn f(net: Net[Connect]):\n    net.listen(\"0.0.0.0:80\")\n")
            .expect_err("Net[Connect] must not listen");
        assert!(err.contains("`listen` needs `Listen`") && err.contains("Net[Connect]"), "got: {err}");
        // A listen-only handle accepts inbound.
        check_str("fn f(console: Console, net: Net[Listen]):\n    let l = net.listen(\"0.0.0.0:80\")\n    console.print(\"ok\")\n")
            .expect("Net[Listen] can listen");
        // ... but it cannot dial out (`connect` or its total sibling `try_connect`).
        let err = check_str("fn f(net: Net[Listen]):\n    net.connect(\"example.com:443\")\n")
            .expect_err("Net[Listen] must not connect");
        assert!(err.contains("`connect` needs `Connect`") && err.contains("Net[Listen]"), "got: {err}");
        let err = check_str("fn f(net: Net[Listen]):\n    net.try_connect(\"example.com:443\")\n")
            .expect_err("Net[Listen] must not try_connect");
        assert!(err.contains("`try_connect` needs `Connect`") && err.contains("Net[Listen]"), "got: {err}");
        // The transport axis attenuates independently: `connect`/`listen` are TCP-only,
        // so a UDP-only handle (full verbs, no TCP) cannot dial.
        let err = check_str("fn f(net: Net[Udp]):\n    net.connect(\"example.com:443\")\n")
            .expect_err("Net[Udp] must not connect (TCP-only op)");
        assert!(err.contains("only implemented over `Tcp`") && err.contains("Net[Udp]"), "got: {err}");
        // `as` drops `Net` verbs but can never add them (mirrors the File slice).
        check_str("fn main(console: Console, net: Net):\n    let dial = net as Net[Connect]\n    console.print(\"ok\")\n")
            .expect("`as` can drop Net to Connect-only");
        let err = check_str("fn main(console: Console, net: Net[Connect]):\n    let l = net as Net[Listen]\n    console.print(\"no\")\n")
            .expect_err("`as` cannot add the Listen verb");
        assert!(err.contains("can only drop rights"), "got: {err}");
        // ... and a narrowed handle cannot be re-widened back to the full `Net`.
        let err = check_str("fn main(console: Console, net: Net[Connect]):\n    let full = net as Net\n    console.print(\"no\")\n")
            .expect_err("`as` cannot re-widen Net[Connect] to full Net");
        assert!(err.contains("can only drop rights"), "got: {err}");
    }

    // (BUG-009 / RFC-0005 hardening #4) Attenuation proof for `Dir` — the Read/Write
    // lattice and one-way `as` narrowing, mirroring the File slice.
    #[test]
    fn dir_capability_rights_and_narrowing() {
        // RFC-0012: a `Dir`'s rights split `Read` from `Write` on independent axes.
        // A read-only handle reads (and lists/exists).
        check_str("fn f(console: Console, d: Dir[Read]):\n    let s = d.read(\"a.txt\")\n    console.print(s)\n")
            .expect("Dir[Read] can read");
        // ... but it cannot `write`, `append`, or `make_dir` (all `Write` verbs).
        let err = check_str("fn f(d: Dir[Read]):\n    d.write(\"a.txt\", \"x\")\n")
            .expect_err("Dir[Read] must not write");
        assert!(err.contains("`write` needs `Write`") && err.contains("Dir[Read]"), "got: {err}");
        let err = check_str("fn f(d: Dir[Read]):\n    d.make_dir(\"sub\")\n")
            .expect_err("Dir[Read] must not make_dir");
        assert!(err.contains("`make_dir` needs `Write`") && err.contains("Dir[Read]"), "got: {err}");
        // A write-only handle writes ...
        check_str("fn f(d: Dir[Write]):\n    d.write(\"a.txt\", \"x\")\n")
            .expect("Dir[Write] can write");
        // ... but cannot `read` or `list` (both `Read` verbs). This is the converse
        // the File slice never asserted.
        let err = check_str("fn f(d: Dir[Write]):\n    d.read(\"a.txt\")\n")
            .expect_err("Dir[Write] must not read");
        assert!(err.contains("`read` needs `Read`") && err.contains("Dir[Write]"), "got: {err}");
        let err = check_str("fn f(d: Dir[Write]):\n    d.list()\n")
            .expect_err("Dir[Write] must not list");
        assert!(err.contains("`list` needs `Read`") && err.contains("Dir[Write]"), "got: {err}");
        // `as` drops Dir rights but never adds them.
        check_str("fn main(console: Console, d: Dir):\n    let ro = d as Dir[Read]\n    console.print(\"ok\")\n")
            .expect("`as` can drop Dir to Read-only");
        let err = check_str("fn main(console: Console, d: Dir[Read]):\n    let w = d as Dir[Write]\n    console.print(\"no\")\n")
            .expect_err("`as` cannot add the Write right");
        assert!(err.contains("can only drop rights"), "got: {err}");
        // ... and cannot re-widen a narrowed handle back to the full `Dir`.
        let err = check_str("fn main(console: Console, d: Dir[Read]):\n    let full = d as Dir\n    console.print(\"no\")\n")
            .expect_err("`as` cannot re-widen Dir[Read] to full Dir");
        assert!(err.contains("can only drop rights"), "got: {err}");
    }

    #[test]
    fn resolved_capability_types_preserve_rights_when_converted_to_ast() {
        let named = |name: &str| ast::Type::Named(name.to_string(), Vec::new());
        assert_eq!(
            ty_to_ast(&Ty::Dir(DirRights { read: true, write: false })),
            Some(ast::Type::Named("Dir".into(), vec![named("Read")]))
        );
        assert_eq!(
            ty_to_ast(&Ty::File(FileRights { read: false, write: true })),
            Some(ast::Type::Named("File".into(), vec![named("Write")]))
        );
        assert_eq!(
            ty_to_ast(&Ty::Net(NetRights {
                connect: true,
                listen: false,
                tcp: true,
                udp: false,
                uds: false,
            })),
            Some(ast::Type::Named(
                "Net".into(),
                vec![named("Connect"), named("Tcp")],
            ))
        );
        assert_eq!(
            ty_to_ast(&Ty::Dir(DirRights::full())),
            Some(named("Dir")),
        );
        assert_eq!(
            ty_to_ast(&Ty::Dir(DirRights { read: false, write: false })),
            None,
        );
        assert_eq!(
            ty_to_ast(&Ty::File(FileRights { read: false, write: false })),
            None,
        );
        assert_eq!(
            ty_to_ast(&Ty::Net(NetRights {
                connect: false,
                listen: false,
                tcp: true,
                udp: false,
                uds: false,
            })),
            None,
        );
        assert_eq!(
            ty_to_ast(&Ty::Net(NetRights {
                connect: true,
                listen: false,
                tcp: false,
                udp: false,
                uds: false,
            })),
            None,
        );
    }

    // (BUG-009 / RFC-0011 + RFC-0005 hardening #4) Policy narrowing preserves the
    // rights set at the type level, and a handle carrying narrowed rights cannot be
    // re-widened by a cast after passing through `net.only` / `dir.only`.
    //
    // NOTE: the *address/entry policy* that `only`/`deny` apply is enforced only at
    // runtime (host-side) and has NO type-level representation — the return type is
    // `Net[rights]` / `Dir[rights]` with the same rights, no policy component. So the
    // only type-level re-widening surface is the rights axis, which is what these
    // assertions cover; the address-set policy itself cannot be "re-widened at the
    // type level" because it is not in the type at all. (`NetPolicy`/`DirPolicy` are
    // declared locally here because `check_str` type-checks a single module without
    // linking `std/confine`; they unify by name with the builtin op expectations.)
    #[test]
    fn policy_narrowing_preserves_rights_and_cannot_rewiden() {
        // `net.only(policy)` keeps the receiver's rights: the result is still
        // connect-capable, so `connect` on it type-checks.
        check_str("type NetPolicy:\n    NetPolicy(String)\nfn f(console: Console, net: Net[Connect]):\n    let scoped = net.only(NetPolicy(\"example.com:443\"))\n    let s = scoped.connect(\"example.com:443\")\n    console.print(\"ok\")\n")
            .expect("net.only preserves the Connect right");
        // ... and because the rights are preserved (still connect-only, not full),
        // the narrowed handle cannot be re-widened to a full `Net` by a cast.
        let err = check_str("type NetPolicy:\n    NetPolicy(String)\nfn f(console: Console, net: Net[Connect]):\n    let scoped = net.only(NetPolicy(\"example.com:443\"))\n    let wide = scoped as Net\n    console.print(\"no\")\n")
            .expect_err("a policy-narrowed Net[Connect] must not re-widen to full Net");
        assert!(err.contains("can only drop rights"), "got: {err}");
        // `dir.only(policy)` likewise keeps `Read` ...
        check_str("type DirPolicy:\n    DirPolicy(String)\nfn f(console: Console, d: Dir[Read]):\n    let scoped = d.only(DirPolicy(\"ext:txt\"))\n    let s = scoped.read(\"a.txt\")\n    console.print(s)\n")
            .expect("dir.only preserves the Read right");
        // ... and the narrowed `Dir[Read]` cannot be re-widened to a full `Dir`.
        let err = check_str("type DirPolicy:\n    DirPolicy(String)\nfn f(console: Console, d: Dir[Read]):\n    let scoped = d.only(DirPolicy(\"ext:txt\"))\n    let wide = scoped as Dir\n    console.print(\"no\")\n")
            .expect_err("a policy-narrowed Dir[Read] must not re-widen to full Dir");
        assert!(err.contains("can only drop rights"), "got: {err}");
    }

    #[test]
    fn fetch_derivation_requires_connect_tcp_and_returns_fetch() {
        check_str(
            "fn derive(net: Net[Connect, Tcp]) -> Fetch:\n    net.fetch(\"https://example.com\")\n",
        )
        .expect("Net[Connect, Tcp] derives Fetch");
        for rights in ["Listen, Tcp", "Connect, Udp"] {
            let source = format!(
                "fn derive(net: Net[{rights}]) -> Fetch:\n    net.fetch(\"https://example.com\")\n"
            );
            let error = check_str(&source).expect_err("insufficient Net rights reject");
            assert!(error.contains("needs `Net[Connect, Tcp]`"), "{error}");
        }
    }

    #[test]
    fn unknown_capability_right_markers_are_rejected() {
        // (BUG-154) A misspelled or invalid bracket marker must be a clear error,
        // not silently normalized to a different authority shape.
        let net = check_str("fn f(net: Net[Conect, Tcp]) -> Bool:\n    true\n").unwrap_err();
        assert!(net.contains("unknown `Net` right `Conect`"), "{net}");
        // `Tls` is a rejected Net right (RFC-0009), not just a typo.
        let tls = check_str("fn f(net: Net[Connect, Tls]) -> Bool:\n    true\n").unwrap_err();
        assert!(tls.contains("unknown `Net` right `Tls`"), "{tls}");
        let dir = check_str("fn f(d: Dir[Reed]) -> Bool:\n    true\n").unwrap_err();
        assert!(dir.contains("unknown `Dir` right `Reed`"), "{dir}");
        let file = check_str("fn f(x: File[Reed]) -> Bool:\n    true\n").unwrap_err();
        assert!(file.contains("unknown `File` right `Reed`"), "{file}");
        // Valid vocabularies still type-check (including a bare capability).
        check_str("fn f(net: Net[Connect, Tcp]) -> Bool:\n    true\n").expect("valid Net rights");
        check_str("fn f(d: Dir[Read, Write]) -> Bool:\n    true\n").expect("valid Dir rights");
        check_str("fn f(net: Net) -> Bool:\n    true\n").expect("bare Net is full rights");
    }

    #[test]
    fn duplicate_duration_match_arm_is_unreachable() {
        // (BUG-294) A duplicate Duration literal arm is dead code, exactly as an
        // Int/Str/Bool duplicate is.
        let err = check_str(
            "fn f(d: Duration) -> Int:\n    match d:\n        1s -> 1\n        1s -> 2\n        _ -> 0\n",
        )
        .unwrap_err();
        assert!(err.contains("unreachable match arm"), "{err}");
        assert!(err.contains("`1s`"), "{err}");
        assert!(!err.contains("1000ms"), "{err}");
        // Distinct Duration arms remain reachable.
        check_str(
            "fn f(d: Duration) -> Int:\n    match d:\n        1s -> 1\n        2s -> 2\n        _ -> 0\n",
        )
        .expect("distinct Duration arms are reachable");
    }

    #[test]
    fn duration_pattern_diagnostics_use_human_units() {
        let simple = check_str("fn f(d: Duration):\n    let 1s = d\n").unwrap_err();
        assert!(simple.contains("`let 1s ="), "{simple}");
        assert!(!simple.contains("1000ms"), "{simple}");

        let compound = check_str("fn f(d: Duration):\n    let 90s = d\n").unwrap_err();
        assert!(compound.contains("`let 1m30s ="), "{compound}");
        assert!(!compound.contains("90000ms"), "{compound}");

        let negative = check_str("fn f(d: Duration):\n    let -1s = d\n").unwrap_err();
        assert!(negative.contains("`let -1s ="), "{negative}");
        assert!(!negative.contains("-1000ms"), "{negative}");
    }

    #[test]
    fn or_pattern_binding_diagnostic_names_the_actual_alt_bindings() {
        let missing = check_str(
            "type Shape:\n    Circle(Int)\n    Square(Int)\n\nfn f(s: Shape) -> Int:\n    match s:\n        Circle(r) | Square(_) -> r\n",
        )
        .unwrap_err();
        assert!(
            missing.contains("Square(_)` binds {} but another alternative binds {r}"),
            "{missing}"
        );
        assert!(
            !missing.contains("Square(_)` binds {r} but another alternative binds {}"),
            "{missing}"
        );

        let extra = check_str(
            "type Shape:\n    Circle(Int)\n    Square(Int)\n\nfn f(s: Shape) -> Int:\n    match s:\n        Circle(_) | Square(q) -> q\n",
        )
        .unwrap_err();
        assert!(
            extra.contains("Square(q)` binds {q} but another alternative binds {}"),
            "{extra}"
        );
        assert!(
            !extra.contains("Square(q)` binds {} but another alternative binds {q}"),
            "{extra}"
        );
    }

    #[test]
    fn non_exhaustive_tuple_and_list_matches_are_rejected() {
        // (BUG-293) tuple/list scrutinees are exhaustiveness-checked at check time
        // instead of trapping at runtime.
        let tup = check_str(
            "fn f(t: (Bool, Bool)) -> Int:\n    match t:\n        (true, true) -> 1\n        (true, false) -> 2\n        (false, true) -> 3\n",
        )
        .unwrap_err();
        assert!(tup.contains("non-exhaustive") && tup.contains("tuple"), "{tup}");
        // A single-arm Int-tuple literal match: Int is open, so it needs a catch-all.
        let ints = check_str("fn f(t: (Int, Int)) -> Int:\n    match t:\n        (3, 4) -> 1\n").unwrap_err();
        assert!(ints.contains("non-exhaustive"), "{ints}");
        // A list match covering only `[]` misses every non-empty list.
        let lst = check_str("fn f(xs: List(Int)) -> Int:\n    match xs:\n        [] -> 0\n").unwrap_err();
        assert!(lst.contains("non-exhaustive") && lst.contains("list"), "{lst}");
        // Fully covered tuple / list matches pass.
        check_str(
            "fn f(t: (Bool, Bool)) -> Int:\n    match t:\n        (true, true) -> 1\n        (true, false) -> 2\n        (false, true) -> 3\n        (false, false) -> 4\n",
        )
        .expect("the full (Bool, Bool) product is exhaustive");
        check_str("fn f(xs: List(Int)) -> Int:\n    match xs:\n        [] -> 0\n        [h, ..rest] -> h\n")
            .expect("`[]` + `[h, ..rest]` is exhaustive");
    }

    #[test]
    fn equality_on_fn_or_cap_fields_of_records_and_enums_is_rejected() {
        // (BUG-302) a record/enum carrying a fn or capability field must reject `==`
        // at check time, so both backends agree (interp compared by closure identity,
        // compiled rejected — a parity divergence).
        let rec = check_str(
            "type H:\n    run: fn(Int) -> Int\nfn add1(x: Int) -> Int:\n    x + 1\nfn f() -> Bool:\n    H(add1) == H(add1)\n",
        )
        .unwrap_err();
        assert!(rec.contains("not defined on function types") && rec.contains("H"), "{rec}");
        let en = check_str(
            "type W:\n    Func(fn(Int) -> Int)\nfn add1(x: Int) -> Int:\n    x + 1\nfn f() -> Bool:\n    Func(add1) == Func(add1)\n",
        )
        .unwrap_err();
        assert!(en.contains("not defined on function types"), "{en}");
        let cap = check_str("type Hold:\n    c: Console\nfn f(a: Hold, b: Hold) -> Bool:\n    a == b\n").unwrap_err();
        assert!(cap.contains("not defined on capability types"), "{cap}");
        // A plain data record still compares.
        check_str("type P:\n    x: Int\n    y: Int\nfn f(a: P, b: P) -> Bool:\n    a == b\n")
            .expect("a plain data record is comparable");
    }

    #[test]
    fn body_let_ascription_binds_the_enclosing_type_parameter() {
        // (BUG-308) `let out: List(a) = xs` refines the fn's generic `a` rather than
        // pinning it to a distinct concrete `Named("a")`.
        check_str("fn firsts(xs: List(a), k: Int) -> List(a):\n    let out: List(a) = xs\n    out\n")
            .expect("a body ascription unifies with the generic parameter");
        // A concrete ascription is unaffected.
        check_str("fn g(xs: List(Int)) -> List(Int):\n    let out: List(Int) = xs\n    out\n")
            .expect("a concrete ascription still works");
        // A *different* letter is still, correctly, a distinct parameter (pins `a`).
        let err = check_str("fn firsts(xs: List(a)) -> List(a):\n    let out: List(b) = xs\n    out\n").unwrap_err();
        assert!(err.contains("isn't generic"), "{err}");
    }

    #[test]
    fn duplicate_declarations_are_rejected() {
        // (BUG-230) types, constructors, and methods get the same "defined more than
        // once" error the function namespace already gives.
        let ty = check_str("type T:\n    A\ntype T:\n    B\n").unwrap_err();
        assert!(ty.contains("type `T` is defined more than once"), "{ty}");
        let identical = check_str("type T:\n    A\ntype T:\n    A\n").unwrap_err();
        assert!(identical.contains("type `T` is defined more than once"), "{identical}");
        let type_param = check_str("type Pair(a, a):\n    Pair(a, a)\n").unwrap_err();
        assert!(
            type_param.contains("type parameter `a` is declared more than once in type `Pair`"),
            "{type_param}"
        );
        let trait_param = check_str("trait Codec(a, a):\n    fn encode(self) -> String\n").unwrap_err();
        assert!(
            trait_param.contains("type parameter `a` is declared more than once in trait `Codec`"),
            "{trait_param}"
        );
        let konst = check_str("let ANSWER = 1\nlet ANSWER = 2\nfn main(console: Console):\n    console.print(\"${ANSWER}\")\n")
            .unwrap_err();
        assert!(konst.contains("constant `ANSWER` is defined more than once"), "{konst}");
        let alias = check_str("type Id = Int\ntype Id = String\nfn f(x: Id) -> Id:\n    x\n").unwrap_err();
        assert!(alias.contains("type alias `Id` is defined more than once"), "{alias}");
        let alias_param = check_str("type Pair(a, a) = (a, a)\n").unwrap_err();
        assert!(
            alias_param.contains("type parameter `a` is declared more than once in type alias `Pair`"),
            "{alias_param}"
        );
        let alias_unbound_param = check_str("type Bad(a) = (a, b)\n").unwrap_err();
        assert!(
            alias_unbound_param.contains("type alias `Bad` uses type parameter `b` but does not declare it"),
            "{alias_unbound_param}"
        );
        let alias_type = check_str("type Id = Int\ntype Id:\n    Id(String)\n").unwrap_err();
        assert!(alias_type.contains("type `Id` conflicts with type alias `Id`"), "{alias_type}");
        let fields = check_str("type Point:\n    x: Int\n    x: String\nfn main(console: Console):\n    console.print(\"ok\")\n")
            .unwrap_err();
        assert!(fields.contains("field `x` is declared more than once in type `Point`"), "{fields}");
        let cap_fields =
            check_str("capability Store:\n    dir: Dir[Read]\n    dir: String\nfn main(console: Console):\n    console.print(\"ok\")\n")
                .unwrap_err();
        assert!(
            cap_fields.contains("field `dir` is declared more than once in capability `Store`"),
            "{cap_fields}"
        );
        let same = check_str("type T:\n    Same(Int)\n    Same(String)\n").unwrap_err();
        assert!(same.contains("constructor `Same`"), "{same}");
        let cross = check_str("type A:\n    Same(Int)\ntype B:\n    Same(String)\n").unwrap_err();
        assert!(cross.contains("constructor `Same`"), "{cross}");
        let meth = check_str(
            "type Box:\n    Box(Int)\nimpl Box:\n    fn value(self) -> Int:\n        1\n    fn value(self) -> Int:\n        2\n",
        )
        .unwrap_err();
        assert!(meth.contains("method `value` is defined more than once"), "{meth}");
        let trait_dup = check_str("trait Two:\n    fn m(self) -> Int\n    fn m(self) -> Int\n").unwrap_err();
        assert!(trait_dup.contains("method `m` is declared more than once"), "{trait_dup}");
        let impl_head = check_str(
            "type Box:\n    Box(Int)\ntrait Label:\n    fn label(self) -> String\nimpl Label for Box:\n    fn label(self) -> String:\n        \"first\"\nimpl Label for Box:\n    fn label(self) -> String:\n        \"second\"\n",
        )
        .unwrap_err();
        assert!(
            impl_head.contains("impl `Label` for `Box` is defined more than once"),
            "{impl_head}"
        );
        // Distinct declarations (incl. same trait for different types) are fine.
        check_str(
            "trait Greet:\n    fn greet(self) -> String\ntype A:\n    A(Int)\ntype B:\n    B(Int)\nimpl Greet for A:\n    fn greet(self) -> String:\n        \"a\"\nimpl Greet for B:\n    fn greet(self) -> String:\n        \"b\"\n",
        )
        .expect("distinct declarations are accepted");
    }

    #[test]
    fn generic_dict_key_operation_requires_eq_bound() {
        // (BUG-395 / RFC-0047) A generic helper performing a `Dict` key operation
        // must carry a `where k: Eq` bound — the key is hashed and compared.
        for op in [
            "dict.get_or(d, key, fallback)",
            "dict.__insert(d, key, fallback)",
            "dict.contains_key(d, key)",
            "dict.__remove(d, key)",
        ] {
            let src = format!("fn f(d: Dict(k, v), key: k, fallback: v) -> v:\n    {op}\n    fallback\n");
            let err = check_str(&src).unwrap_err();
            assert!(err.contains("requires `k: Eq`"), "{op}: {err}");
        }
        // With the bound it type-checks (checked through monomorphization).
        check_str("fn f(d: Dict(k, v), key: k, fallback: v) -> v where k: Eq:\n    dict.get_or(d, key, fallback)\n")
            .expect("a `where k: Eq` generic dict helper is accepted");
        // A concrete key needs no bound.
        check_str("fn f(d: Dict(String, Int), key: String) -> Int:\n    dict.get_or(d, key, 0)\n")
            .expect("a concrete String key needs no bound");
    }

    #[test]
    fn bounded_generic_call_requires_wrapper_to_forward_bound() {
        // A public bounded helper's `where` clause is part of its call contract.
        // A generic wrapper must forward that obligation instead of exporting an
        // apparently unbounded signature.
        let err = check_str(
            "fn needs_eq(x: a) -> a where a: Eq:\n    x\n\nfn wrapper(x: a) -> a:\n    needs_eq(x)\n",
        )
        .expect_err("unbounded wrapper must not erase callee Eq obligation");
        assert!(err.contains("requires `a: Eq`"), "{err}");

        check_str(
            "fn needs_eq(x: a) -> a where a: Eq:\n    x\n\nfn wrapper(x: a) -> a where a: Eq:\n    needs_eq(x)\n",
        )
        .expect("forwarding the Eq bound satisfies the call contract");

        check_str(
            "fn needs_eq(x: a) -> a where a: Eq:\n    x\n\nfn wrapper(x: a) -> a where a: Ord:\n    needs_eq(x)\n",
        )
        .expect("Ord discharges its Eq supertrait obligation");
    }

    #[test]
    fn dequalify_home_strips_only_the_home_module() {
        // BUG-292: a home-module name renders bare; a cross-module name keeps its
        // qualifier (it disambiguates a same-named type from another module).
        assert_eq!(dequalify_home("t_file.Color", "t_file"), "Color");
        assert_eq!(dequalify_home("helper.Token", "t_file"), "helper.Token");
        assert_eq!(dequalify_home("Bool", "t_file"), "Bool");
        assert_eq!(dequalify_home("t_file.Color", ""), "t_file.Color");
    }

    #[test]
    fn strip_home_qualifiers_keeps_cross_module_names() {
        // Home-module type in a mismatch renders bare...
        assert_eq!(
            strip_home_qualifiers("expected `String`, found `app.Point`", "app"),
            "expected `String`, found `Point`"
        );
        // ...while two cross-module same-named types keep BOTH qualifiers (the exact
        // `expected Token, found Token` confusion RFC-0042 forbids).
        assert_eq!(
            strip_home_qualifiers("expected `helper_b.Token`, found `helper_a.Token`", "main"),
            "expected `helper_b.Token`, found `helper_a.Token`"
        );
        // The `module.fn` location prefix (lowercase suffix) is never stripped.
        assert_eq!(strip_home_qualifiers("in `app.go`: boom", "app"), "in `app.go`: boom");
        // An unknown home is a no-op.
        assert_eq!(strip_home_qualifiers("found `app.Point`", ""), "found `app.Point`");
    }

    #[test]
    fn rfc0063_list_catalog_signature_outranks_linked_placeholder() {
        use witchy_syntax::ast::Item;

        let mut module = witchy_syntax::parser::parse_module(
            "fn shadow(xs: String) -> String:\n    xs\n\nfn main() -> Int:\n    list.length([1])\n",
        )
        .expect("parse catalog precedence probe");
        let Item::Function(placeholder) = &mut module.items[0] else {
            panic!("expected placeholder function")
        };
        placeholder.name = witchy_syntax::intrinsics::LIST_LENGTH.into();

        check(&module).expect(
            "the catalog recipe, not the deliberately wrong linked placeholder signature, must type list.length",
        );
    }

    #[test]
    fn rfc0063_dict_catalog_signature_and_bounds_are_authoritative() {
        use witchy_syntax::ast::Item;

        let mut module = witchy_syntax::parser::parse_module(
            "fn shadow(value: String, extra: String) -> String:\n    value\n\nfn main() -> Int:\n    dict.length(dict.new())\n",
        )
        .expect("parse dict catalog precedence probe");
        let Item::Function(placeholder) = &mut module.items[0] else {
            panic!("expected placeholder function")
        };
        placeholder.name = witchy_syntax::intrinsics::DICT_LENGTH.into();
        check(&module).expect(
            "the catalog recipe, not the deliberately wrong linked placeholder signature, must type dict.length",
        );

        let missing = check_str(
            "fn put(d: Dict(k, Int), key: k) -> Dict(k, Int):\n    dict.__insert(d, key, 1)\n",
        )
        .expect_err("cataloged dict key operations require Eq");
        assert!(missing.contains("requires `k: Eq`"), "{missing}");

        check_str(
            "fn put(d: Dict(k, Int), key: k) -> Dict(k, Int) where k: Eq:\n    dict.__insert(d, key, 1)\n",
        )
        .expect("the cataloged Eq bound accepts an explicitly bounded key");
    }

    #[test]
    fn rfc0063_regex_catalog_signature_outranks_linked_placeholder() {
        use witchy_syntax::ast::Item;

        let mut module = witchy_syntax::parser::parse_module(
            "import regex\n\nfn shadow(value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    let spans: String = regex.match_spans(\"a\", \"cat\")\n    0\n",
        )
        .expect("parse regex catalog precedence probe");
        let Item::Function(placeholder) = &mut module.items[0] else {
            panic!("expected placeholder function")
        };
        placeholder.name = witchy_syntax::intrinsics::REGEX_MATCH_SPANS.into();

        check(&module).expect(
            "the catalog recipe, not the deliberately wrong linked placeholder signature, must type regex.match_spans",
        );
    }

    #[test]
    fn rfc0087_function_values_retain_and_enforce_conventions() {
        let immutable = check_str(
            "fn bump(var n: Int) -> Int:\n    n\n\nfn main():\n    let n = 1\n    let f: fn(var Int) -> Int = bump\n    let _ = f(n)\n",
        )
        .expect_err("an indirect var call must reject an immutable argument");
        assert!(immutable.contains("mutable `var`"), "{immutable}");

        let mismatch = check_str(
            "fn pure(n: Int) -> Int:\n    n\n\nfn use(f: fn(var Int) -> Int) -> Nil:\n    return\n\nfn main():\n    use(pure)\n",
        )
        .expect_err("function conventions are part of type identity");
        assert!(
            mismatch.contains("fn(var Int) -> Int") && mismatch.contains("fn(Int) -> Int"),
            "{mismatch}"
        );
    }

    #[test]
    fn rfc0087_discarded_var_free_call_is_allowed() {
        // RFC-0087 makes free and method forms identical: a call with a resolved
        // var convention may discard its ordinary result because write-back is
        // already an observable effect.
        //
        // The discard classifier lives in the trait/method rewrite pass, which the
        // single-module `check_str` harness only runs when the module "needs
        // lowering". The leading `impl` forces that here; a real CLI program always
        // links std (which needs lowering), so BOTH backends run this check on
        // every program (verified: even a std-free `f()` discard errors on both).
        const LOWER: &str = "type Tag:\n    v: Int\nimpl Tag:\n    fn id(self) -> Int:\n        self.v\n";

        // ANY non-`Nil` free call (not only mutators) whose result is discarded.
        let nonmut_free = check_str(&format!(
            "{LOWER}fn double(n: Int) -> Int:\n    n * 2\nfn main(console: Console):\n    double(3)\n    console.print(\"hi\")\n"
        ))
        .unwrap_err();
        assert!(nonmut_free.contains("is discarded"), "{nonmut_free}");

        // `let _ = …` is the explicit-discard escape hatch and still compiles.
        check_str(&format!(
            "{LOWER}fn double(n: Int) -> Int:\n    n * 2\nfn main(console: Console):\n    let _ = double(3)\n    console.print(\"hi\")\n"
        ))
        .expect("`let _ =` is the explicit discard escape");

        // A `Nil`-returning free call in statement position is unaffected — there
        // is no result to discard.
        check_str(&format!(
            "{LOWER}fn noop(n: Int):\n    let _ = n\nfn main(console: Console):\n    noop(3)\n    console.print(\"hi\")\n"
        ))
        .expect("a `Nil`-returning free call statement is fine");
    }

    #[test]
    fn plain_function_called_as_method_on_record_names_the_real_type() {
        // A private receiver-first free function called through method syntax on a
        // user record must name the RECEIVER's type, not the `Bool(false)`
        // placeholder the resolver swaps in on a successful rewrite. Before the
        // owner-UFCS branch was made commit-only-on-success, this reported
        // "no method `describe` on `Bool`".
        let err = check_str(
            "type Point:\n    x: Int\n    y: Int\n\n\
             fn describe(p: Point) -> Int:\n    p.x\n\n\
             fn main(console: Console):\n    let p = Point(1, 2)\n    let n = p.describe()\n    console.print(\"${n}\")\n",
        )
        .expect_err("a plain function is not a method");
        assert!(err.contains("on `Point`"), "must name the receiver type Point: {err}");
        assert!(!err.contains("`Bool`"), "must not leak the Bool placeholder: {err}");
    }

    // The builtin-receiver diagnostic and the "real owner-module method still
    // resolves" control both need the std library linked (String/Duration methods
    // live there), which `check_str` does not do. They are pinned on the real,
    // std-linked path by a runnable `book/` example instead.

    // ------------------------------------------------------------------
    // RFC-0081: existential trait values — public frontend contract.
    // ------------------------------------------------------------------

    #[test]
    fn rfc0081_unknown_trait_in_dyn() {
        let err = check_str(
            "fn f(x: dyn Render) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("dyn over an undeclared trait");
        assert!(err.contains("unknown trait `Render` in `dyn Render`"), "{err}");
    }

    #[test]
    fn rfc0081_trait_arity_mismatch_in_dyn() {
        let err = check_str(
            "trait Convert(t):\n    fn convert(let self) -> t\n\n\
             fn f(x: dyn Convert(Int, String)) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("dyn with the wrong trait arity");
        assert!(
            err.contains(
                "trait `Convert` expects 1 type argument(s) but got 2 in `dyn Convert(Int, String)`"
            ),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_unresolved_trait_type_parameter_in_dyn() {
        let err = check_str(
            "trait Convert(t):\n    fn convert(let self) -> t\n\n\
             fn f(x: dyn Convert(a)) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a dyn trait argument must be concrete");
        assert!(
            err.contains(
                "`dyn Convert(a)`: every trait type parameter must be fixed by a concrete \
                 type — `a` is an unresolved type parameter"
            ),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_safety_rule_1_receiverless_method() {
        let err = check_str(
            "trait Maker:\n    fn make() -> Int\n\n\
             fn f(x: dyn Maker) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a receiver-less associated fn blocks dyn use");
        assert!(
            err.contains("trait `Maker` is not existential-safe as `dyn Maker`")
                && err.contains("method `make` has no receiver"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_safety_rule_2_method_local_type_parameter() {
        let err = check_str(
            "trait Picker:\n    fn pick(let self, x: b) -> Int\n\n\
             fn f(x: dyn Picker) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a method-local type parameter blocks dyn use");
        assert!(
            err.contains("trait `Picker` is not existential-safe as `dyn Picker`")
                && err.contains("method `pick` introduces method-local type parameter `b`"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_safety_rule_3_bare_self_return() {
        let err = check_str(
            "trait Cloner:\n    fn duplicate(let self) -> Self\n\n\
             fn f(x: dyn Cloner) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a bare `Self` return blocks dyn use");
        assert!(
            err.contains("trait `Cloner` is not existential-safe as `dyn Cloner`")
                && err.contains("method `duplicate` returns bare `Self`"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_safety_rule_4_self_outside_receiver() {
        // A `Self`-typed non-receiver parameter…
        let err = check_str(
            "trait Merger:\n    fn merge(let self, other: Self) -> Int\n\n\
             fn f(x: dyn Merger) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a Self-typed parameter blocks dyn use");
        assert!(
            err.contains("trait `Merger` is not existential-safe as `dyn Merger`")
                && err.contains("method `merge` mentions `Self` outside the receiver"),
            "{err}"
        );

        // …and `Self` NESTED in the return (bare returns are rule 3's).
        let err = check_str(
            "trait Splitter:\n    fn split(let self) -> List(Self)\n\n\
             fn f(x: dyn Splitter) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a Self nested in the return blocks dyn use");
        assert!(
            err.contains("method `split` mentions `Self` outside the receiver"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_safety_rule_5_receiver_borrowed_result() {
        let err = check_str(
            "trait Peeker:\n    fn peek(let self) -> View(String, 'a)\n\n\
             fn f(x: dyn Peeker) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a borrowed result blocks dyn use in v1");
        assert!(
            err.contains("trait `Peeker` is not existential-safe as `dyn Peeker`")
                && err.contains("method `peek` returns a result borrowed from the hidden receiver"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_safety_rule_6_ambient_partial_eq() {
        // `check_str` links no std, so `PartialEq` is the AMBIENT built-in
        // here and takes the dedicated Self-binary diagnostic. (In a
        // std-linked program the declared `trait PartialEq` takes the general
        // rule-4 path naming `eq`/`ne`; both name the trait and the rule.)
        let err = check_str(
            "fn f(x: dyn PartialEq) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("dyn PartialEq is never existential-safe");
        assert!(
            err.contains("trait `PartialEq` is not existential-safe as `dyn PartialEq`")
                && err.contains("second `Self` parameter"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_safety_diagnostic_names_every_blocking_method() {
        // One error, listing every violating method+rule — not just the first.
        let err = check_str(
            "trait Bad:\n\
             \x20   fn make() -> Int\n\
             \x20   fn pick(let self, x: b) -> Int\n\
             \x20   fn duplicate(let self) -> Self\n\
             \x20   fn merge(let self, other: Self) -> Int\n\n\
             fn f(x: dyn Bad) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("every violation is reported at once");
        for needle in [
            "method `make` has no receiver",
            "method `pick` introduces method-local type parameter `b`",
            "method `duplicate` returns bare `Self`",
            "method `merge` mentions `Self` outside the receiver",
        ] {
            assert!(err.contains(needle), "missing `{needle}` in: {err}");
        }
    }

    #[test]
    fn rfc0081_borrowed_dyn_is_a_v1_exclusion() {
        let err = check_str(
            "trait Render:\n    fn render(let self) -> String\n\n\
             fn f(x: View(dyn Render, 'a)) -> Int:\n    1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("borrowed existentials are excluded from v1");
        assert!(
            err.contains(
                "borrowed existential values (`View(dyn Render, 'a)` / `let('a) dyn Render`) \
                 are excluded from RFC-0081 v1"
            ),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_as_dyn_rejects_record_capability_payload() {
        let err = check_str(
            "trait Render:\n    fn render(let self) -> String\n\n\
             type Holder:\n    dir: Dir\n\n\
             fn main(dir: Dir, console: Console):\n\
             \x20   let h = Holder(dir)\n\
             \x20   let r = h as dyn Render\n\
             \x20   console.print(\"hi\")\n",
        )
        .expect_err("a Dir-carrying record cannot be erased");
        assert!(
            err.contains(
                "`as dyn Render`: the concrete payload type `Holder` carries a `Dir` \
                 capability through `Holder.dir` — capability-carrying existential payloads are rejected (RFC-0081); \
                 pass the capability explicitly in method signatures instead"
            ),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_implicit_dyn_rejects_record_capability_payload() {
        let err = check_str(
            "trait Render:\n    fn render(let self) -> String\n\n\
             type Holder:\n    dir: Dir\n\n\
             fn erase(h: Holder) -> dyn Render:\n    h\n\n\
             fn main(dir: Dir, console: Console):\n\
             \x20   let h = Holder(dir)\n\
             \x20   console.print(\"hi\")\n",
        )
        .expect_err("a Dir-carrying record cannot be implicitly erased");
        assert!(
            err.contains(
                "conversion to `dyn Render`: the concrete payload type `Holder` carries a `Dir` \
                 capability through `Holder.dir` — capability-carrying existential payloads are rejected (RFC-0081); \
                 pass the capability explicitly in method signatures instead"
            ),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_as_dyn_rejects_container_capability_payloads() {
        const TRAIT: &str = "trait Render:\n    fn render(let self) -> String\n\n";

        // Tuple payload.
        let err = check_str(&format!(
            "{TRAIT}fn main(dir: Dir, console: Console):\n\
             \x20   let r = (dir, \"x\") as dyn Render\n\
             \x20   console.print(\"hi\")\n"
        ))
        .expect_err("a Dir-carrying tuple cannot be erased");
        assert!(
            err.contains("`as dyn Render`") && err.contains("carries a `Dir` capability"),
            "{err}"
        );

        // Sum/variant payload.
        let err = check_str(&format!(
            "{TRAIT}type Wrap:\n    Wrapped(Dir)\n    Empty\n\n\
             fn main(dir: Dir, console: Console):\n\
             \x20   let w = Wrapped(dir)\n\
             \x20   let r = w as dyn Render\n\
             \x20   console.print(\"hi\")\n"
        ))
        .expect_err("a Dir-carrying variant cannot be erased");
        assert!(
            err.contains("`as dyn Render`")
                && err.contains("payload type `Wrap` carries a `Dir` capability"),
            "{err}"
        );

        // Generic-container payload (`Option(Dir)` is a legal parameter form).
        let err = check_str(&format!(
            "{TRAIT}fn f(x: Option(Dir)) -> Int:\n\
             \x20   let r = x as dyn Render\n\
             \x20   1\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n"
        ))
        .expect_err("a Dir-carrying Option cannot be erased");
        assert!(
            err.contains("payload type `Option(Dir)` carries a `Dir` capability"),
            "{err}"
        );

        // A `List(Dir)` payload is unconstructible TODAY: the list literal is
        // rejected upstream (cap-carrying collections, RFC-0005), before the
        // cast is reached. Pin that the program still errors on the `Dir`
        // capability; `ty_carries_capability` covers List recursion for when
        // collections learn to carry caps.
        let err = check_str(&format!(
            "{TRAIT}fn main(dir: Dir, console: Console):\n\
             \x20   let r = [dir] as dyn Render\n\
             \x20   console.print(\"hi\")\n"
        ))
        .expect_err("a Dir-carrying list is rejected before the cast");
        assert!(err.contains("`Dir` capability"), "{err}");
    }

    #[test]
    fn rfc0081_var_dyn_argument_requires_an_existential_caller_place() {
        let err = check_str(
            "trait Render:\n    fn render(let self) -> String\n\n\
             type Plain:\n    tag: String\n\n\
             impl Render for Plain:\n    fn render(let self) -> String:\n        self.tag\n\n\
             fn replace(var value: dyn Render):\n    value = Plain(\"new\")\n\n\
             fn use(var value: Plain):\n    replace(value)\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a concrete var place cannot receive an arbitrary existential write-back");
        assert!(
            err.contains(
                "argument 1 to `var` parameter of `replace` cannot implicitly convert \
                 `Plain` to `dyn Render`"
            ) && err.contains("bind a `var` of the existential type before this call"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_if_without_else_cannot_claim_an_existential_result() {
        let err = check_str(
            "trait Render:\n    fn render(let self) -> String\n\n\
             type Plain:\n    tag: String\n\n\
             fn erase(flag: Bool, value: Plain) -> dyn Render:\n\
             \x20   if flag:\n\
             \x20       value\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("the false branch is Nil, not an existential");
        assert!(
            err.contains("`if` without `else` has an implicit `Nil` path")
                && err.contains("add an explicit `else` branch"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_redundant_dyn_cast_is_not_an_upcast() {
        let err = check_str(
            "trait Render:\n    fn render(let self) -> String\n\n\
             fn recast(value: dyn Render) -> dyn Render:\n    value as dyn Render\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("even a redundant dyn cast must not bypass preparation");
        assert!(
            err.contains("cannot cast `dyn Render` to `dyn Render`")
                && err.contains("may only upcast to one of its transitive supertraits"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_unrelated_dyn_conversion_fails_at_check_time() {
        let err = check_str(
            "trait Render:\n    fn render(let self) -> String\n\n\
             trait Inspect:\n    fn inspect(let self) -> String\n\n\
             type Label:\n    tag: String\n\n\
             impl Render for Label:\n    fn render(let self) -> String:\n        self.tag\n\n\
             impl Inspect for Label:\n    fn inspect(let self) -> String:\n        self.tag\n\n\
             fn recast(value: dyn Render) -> dyn Inspect:\n    value\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("unrelated existential conversion must be rejected by source checking");
        assert!(
            err.contains("cannot convert `dyn Render` to unrelated `dyn Inspect`"),
            "{err}"
        );
    }

    #[test]
    fn typed_capability_ascription_does_not_retain_broader_rights() {
        let err = check_str(
            "fn misuse(dir: Dir):\n\
             \x20   let read_only: Dir[Read] = dir\n\
             \x20   read_only.write(\"x\", \"y\")\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a typed binding must not retain broader authority");
        assert!(
            err.contains("declared `Dir[Read]` but the value disagrees")
                || err.contains("does not grant Write"),
            "{err}"
        );
    }

    #[test]
    fn rfc0081_operations_deliberately_absent_have_no_fallback_surface() {
        let binary = |operator: &str| {
            check_str(&format!(
                "trait Render:\n    fn render(let self) -> String\n\n\
                 fn probe(left: dyn Render, right: dyn Render) -> Bool:\n    left {operator} right\n\n\
                 fn main(console: Console):\n    console.print(\"hi\")\n"
            ))
            .expect_err("an existential has no automatic comparison protocol")
        };
        for operator in ["==", "<"] {
            let error = binary(operator);
            assert!(
                error.contains("dyn Render")
                    || error.contains("PartialEq")
                    || error.contains("ordering"),
                "operator `{operator}` unexpectedly exposed an existential fallback: {error}"
            );
        }

        let nested = check_str(
            "trait Render:\n    fn render(let self) -> String\n\n\
             fn same(left: List(dyn Render), right: List(dyn Render)) -> Bool:\n    left == right\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("containers must not restore automatic existential equality");
        assert!(
            nested.contains("List(dyn Render)")
                && nested.contains("witness identity")
                && nested.contains("not observable"),
            "{nested}"
        );

        for method in [
            "hash",
            "reflect",
            "serialize",
            "type_name",
            "address",
            "witness_id",
            "downcast",
        ] {
            let error = check_str(&format!(
                "trait Render:\n    fn render(let self) -> String\n\n\
                 fn probe(value: dyn Render) -> Int:\n    value.{method}()\n\n\
                 fn main(console: Console):\n    console.print(\"hi\")\n"
            ))
            .expect_err("an absent existential operation must fail at check time");
            assert!(
                error.contains(method)
                    && (error.contains("Render") || error.contains("trait")),
                "method `.{method}` unexpectedly exposed an existential fallback: {error}"
            );
        }

        let downcast = check_str(
            "trait Render:\n    fn render(let self) -> String\n\n\
             type Label:\n    Label(String)\n\n\
             fn narrow(value: dyn Render) -> Label:\n    value as Label\n\n\
             fn main(console: Console):\n    console.print(\"hi\")\n",
        )
        .expect_err("a dyn-to-concrete cast must not become an implicit downcast");
        assert!(
            downcast.contains("dyn Render") && downcast.contains("Label"),
            "{downcast}"
        );
    }

    #[test]
    fn rfc0081_body_type_positions_are_validated() {
        // `let` annotation.
        let err = check_str(
            "fn main(console: Console):\n\
             \x20   let x: dyn Missing = 1\n\
             \x20   console.print(\"hi\")\n",
        )
        .expect_err("a body let-annotation is validated");
        assert!(err.contains("unknown trait `Missing` in `dyn Missing`"), "{err}");

        // Lambda parameter position.
        let err = check_str(
            "fn main(console: Console):\n\
             \x20   let f = fn(x: dyn Missing) -> Int: 1\n\
             \x20   console.print(\"hi\")\n",
        )
        .expect_err("a lambda parameter type is validated");
        assert!(err.contains("unknown trait `Missing` in `dyn Missing`"), "{err}");
    }
