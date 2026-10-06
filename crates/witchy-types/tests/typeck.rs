//! Black-box typeck tests: every test here goes through the crate's public
//! API (`check_str` / `check`). Moved out of src/typeck_tests.rs (audit
//! 2026-08-08): integration tests don't need crate internals and don't belong
//! in the library's compilation unit. Tests exercising private helpers
//! (message surgery, pipeline internals, structural-impl markers, TypeTable
//! facts) remain in src/typeck_tests.rs. The body keeps its original
//! one-level indentation inside `mod tests` so embedded witchy source
//! literals (indentation-sensitive) stay byte-identical to the pre-move file.

mod tests {
    #[allow(unused_imports)]
    use witchy_syntax::ast::{self, Block, Expr, Item, Stmt};
    #[allow(unused_imports)]
    use witchy_types::typeck::{check, check_str, gc_cap_aggregate_names};

    #[test]
    fn anonymous_union_patterns_check_closed_tags() {
        check_str(
            "fn describe(e: .[BadPort(Int) | Missing(String) | NotFound]) -> String:\n    match e:\n        .BadPort(p) -> \"${p}\"\n        .Missing(k) -> k\n        .NotFound -> \"missing\"\n\nfn nested(r: Result(Int, .[BadPort(Int) | Missing(String) | NotFound])) -> String:\n    match r:\n        Ok(n) -> \"${n}\"\n        Err(.BadPort(p)) -> \"${p}\"\n        Err(.Missing(k)) -> k\n        Err(.NotFound) -> \"missing\"\n"
        )
        .expect("anonymous union patterns check against their scrutinee");

        check_str("fn only(e: .[Only(Int)]) -> Int:\n    let .Only(n) = e\n    n\n")
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
        assert!(
            wrong_arity.contains("takes 1 payload pattern"),
            "{wrong_arity}"
        );

        let missing = check_str(
            "fn describe(e: .[BadPort(Int) | NotFound]) -> String:\n    match e:\n        .BadPort(p) -> \"${p}\"\n"
        )
        .unwrap_err();
        assert!(
            missing.contains("non-exhaustive match") && missing.contains("`.NotFound`"),
            "{missing}"
        );
    }

    #[test]
    fn structural_types_reject_capabilities_at_every_depth() {
        let union =
            check_str("fn bad(net: Net) -> .[Got(Net) | Missing]:\n    .Got(net)\n").unwrap_err();
        assert!(
            union.contains("anonymous union") && union.contains("Net"),
            "{union}"
        );

        let record = check_str("fn read(r: .{net: Net}) -> Int:\n    0\n").unwrap_err();
        assert!(
            record.contains("anonymous record") && record.contains("Net"),
            "{record}"
        );

        let inferred_record = check_str("fn capture(net: Net):\n    .{net: net}\n").unwrap_err();
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
            "type Row = .{x: Int}\nimpl Row:\n    fn label(self) -> String:\n        \"row\"\n",
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
        assert!(
            nested.contains("packed") && nested.contains("`p`"),
            "{nested}"
        );
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
        assert!(
            direct.contains("BARE") && direct.contains("Net"),
            "{direct}"
        );
        // ... and transitively, through a nested user cap that wraps a host cap.
        let transitive = check_str(
            "capability Inner:\n    net: Net[Connect]\n\ngrantable capability Bad2:\n    inner: Inner\n"
        )
        .unwrap_err();
        assert!(
            transitive.contains("BARE") && transitive.contains("Net"),
            "{transitive}"
        );
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
        assert!(
            err.contains("export_step") && err.contains("grantable"),
            "{err}"
        );
        // A migrated host capability such as `File` is not a grantable UI cap and
        // has no export-wrapper minting ABI in RFC-0005 Stage 2.
        let file = check_str("pub fn export_step(f: File, input: String) -> String:\n    input\n")
            .unwrap_err();
        assert!(
            file.contains("export_step") && file.contains("File") && file.contains("grantable"),
            "{file}"
        );
        // A plain single-String export is unaffected.
        assert!(check_str("pub fn export_step(input: String) -> String:\n    input\n").is_ok());
    }

    #[test]
    fn retired_reference_surfaces_fail_closed_before_loan_analysis() {
        let cases = [
            (
                "explicit reference lifetime",
                "fn read(value: &'a String) -> Int:\n    value.length()\n",
            ),
            (
                "mutable reference",
                "fn read(value: &mut String) -> Int:\n    value.length()\n",
            ),
            (
                "legacy View carrier",
                "fn read(value: View(String, 'a)) -> Int:\n    0\n",
            ),
        ];

        for (surface, source) in cases {
            let error = check_str(source).expect_err(surface);
            assert!(
                error.contains("retired") || error.contains("lifetime"),
                "{surface} was not rejected fail-closed: {error}"
            );
        }

        check_str("fn read(value: &String) -> String:\n    *value\n")
            .expect("the lifetime-free immutable view syntax remains available");
    }

    #[test]
    fn recursive_list_view_can_escape_through_an_inferred_closure() {
        check_str(
            "mode opt\n\nfn escape(owner: List(List(List(Int)))) -> fn() -> &List(List(List(Int))):\n    let view = &owner\n    fn():\n        view\n",
        )
        .expect("the recursive carrier admits a depth-three rooted view capture");

        check_str("fn outer() -> Int:\n    let value = 7\n    let f = fn(): value\n    0\n")
            .expect("ordinary scalar closure captures remain valid");
    }

    #[test]
    fn borrowed_nominal_rejects_own_conventions_at_every_callable_surface() {
        check_str("mode opt\n\ntype Holder:\n    view: &String\n\nfn consume(own holder: Holder) -> Int:\n    0\n")
            .expect("own transfers an immutable-view aggregate as a value");

        check_str("fn consume(own value: String) -> Int:\n    value.length()\n")
            .expect("own remains valid for ordinary owned values");
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
            let source = format!("mode opt\n\nfn bad(value: &{ty}) -> Int:\n    0\n");
            let direct =
                check_str(&source).expect_err("immutable views cannot borrow capabilities");
            assert!(
                direct.contains(name) && (direct.contains("capability") || direct.contains("carrier")),
                "{direct}"
            );
        }

        let branded = check_str(
            "mode opt\n\ncapability Redis from Net[Connect]\n\nfn bad(redis: &Redis) -> Int:\n    0\n",
        )
        .expect_err("user-defined capabilities also require lease-bearing borrows");
        assert!(
            branded.contains("Redis") && (branded.contains("capability") || branded.contains("carrier")),
            "{branded}"
        );

        let field = check_str("mode opt\n\ntype Holder:\n    dir: &Dir[Read]\n")
            .expect_err("borrowed nominal fields cannot invent a capability lease");
        assert!(
            field.contains("Dir") && (field.contains("capability") || field.contains("carrier")),
            "{field}"
        );

        let substituted = check_str(
            "mode opt\n\ntype Inner(a):\n    value: a\n\ntype Outer(a):\n    inner: Inner(a)\n\nfn bad(value: &Outer(Console)) -> Int:\n    0\n",
        )
        .expect_err("a realized generic field that stores a capability still requires a lease");
        assert!(
            substituted.contains("Console") && (substituted.contains("capability") || substituted.contains("carrier")),
            "{substituted}"
        );

        check_str(
            "mode opt\n\ntype Callback:\n    run: fn(Dir[Read]) -> Int\n\nfn inspect(callback: &Callback) -> Int:\n    0\n",
        )
        .expect("a callback signature may mention caller-supplied authority without storing it");

        check_str(
            "mode opt\n\ntype Phantom(a):\n    value: Int\n\nfn inspect(value: &Phantom(Dir[Read])) -> Int:\n    0\n",
        )
        .expect("an unused capability type argument does not become stored authority");

        check_str("fn inspect(let dir: Dir[Read]) -> Int:\n    0\n")
            .expect("an ordinary non-lifetime capability parameter remains valid");
    }

    #[test]
    fn retired_local_unique_is_rejected_before_typechecking() {
        let ret = check_str("fn f() -> local unique List(Int):\n    [1, 2]\n").unwrap_err();
        assert!(
            ret.contains("local unique") && ret.contains("retired"),
            "{ret}"
        );
        // `unique` (returnable) is fine.
        check_str("fn f() -> unique List(Int):\n    [1, 2]\n").expect("a unique return is valid");
        check_str(
            "fn build() -> unique List(Int):\n    [1]\n\nfn forward() -> unique List(Int):\n    build()\n",
        )
        .expect("a direct unique result forwards its checked root owner");
        // Normal mode repairs a shared result with a copy at the return;
        // opt mode must prove the root owner in source.
        check_str("fn copied(xs: List(Int)) -> unique List(Int):\n    xs\n")
            .expect("normal mode repairs a shared unique result with a copy");
        // The return repairs only storage it can copy; a dictionary must
        // still prove its root owner in source.
        let dictionary = check_str("fn shared(d: Dict(String, Int)) -> unique Dict(String, Int):\n    d\n")
            .expect_err("normal mode cannot repair a shared unique Dict result");
        assert!(dictionary.contains("no root-owner proof"), "{dictionary}");
        let unproved = check_str("mode opt\n\nfn bad(xs: List(Int)) -> unique List(Int):\n    xs\n")
            .expect_err("an arbitrary shared value cannot claim a unique root owner in opt mode");
        assert!(unproved.contains("no root-owner proof"), "{unproved}");

        check_str(
            "fn choose(flag: Bool) -> unique List(Int):\n    if flag:\n        return [1]\n    return [2]\n",
        )
        .expect("exhaustive explicit returns each carry their own token");

        check_str(
            "fn choose(flag: Bool) -> unique List(Int):\n    if flag:\n        [1]\n    else:\n        [2]\n",
        )
        .expect("every completing control-flow arm establishes a fresh root owner");

        let method = check_str(
            "mode opt\n\ntype Holder:\n    values: List(Int)\n\nimpl Holder:\n    fn expose(let self: Holder) -> unique List(Int):\n        self.values\n",
        )
        .expect_err("an opt-mode impl method cannot relabel a shared field as unique");
        assert!(method.contains("no root-owner proof"), "{method}");
    }

    // Normal mode cannot repair a `unique Dict` result, so these exercise the
    // source proof without opt mode's affine bindings rejecting the aliases
    // first.
    #[test]
    fn a_local_that_solely_owns_fresh_storage_proves_a_unique_result() {
        let build = |body: &str| {
            format!("fn put(var d: unique Dict(Int, Int), k: Int):\n    return\n\nfn size(let d: Dict(Int, Int)) -> Int:\n    0\n\nfn fresh() -> unique Dict(Int, Int):\n    dict.new()\n\nfn build(n: Int, d: Dict(Int, Int)) -> unique Dict(Int, Int):\n{body}")
        };
        check_str(&build(
            "    var out = dict.new()\n    var i = 0\n    while i < n:\n        if i % 2 == 1:\n            put(out, i)\n        i = i + 1\n    out\n",
        ))
        .expect("a `var unique` write-back keeps the local the sole owner across a loop");
        check_str(&build(
            "    var out = dict.new()\n    put(out, n)\n    let moved = move out\n    moved\n",
        ))
        .expect("a move hands the proof to the new binding");
        check_str(&build(
            "    var out = dict.new()\n    if n > 0:\n        out = fresh()\n    out\n",
        ))
        .expect("a reassignment from a unique result keeps a proof on every arm");
    }

    #[test]
    fn a_local_that_may_be_aliased_does_not_prove_a_unique_result() {
        let rejected = |body: &str, why: &str| {
            let source = format!("fn put(var d: unique Dict(Int, Int), k: Int):\n    return\n\nfn size(let d: Dict(Int, Int)) -> Int:\n    0\n\nfn fresh() -> unique Dict(Int, Int):\n    dict.new()\n\nfn build(n: Int, d: Dict(Int, Int)) -> unique Dict(Int, Int):\n{body}");
            let error = check_str(&source).expect_err(why);
            assert!(error.contains("no root-owner proof"), "{why}: {error}");
        };
        rejected(
            "    var out = dict.new()\n    let alias = out\n    let _ = size(alias)\n    out\n",
            "a copy into another binding may share the root",
        );
        rejected(
            "    var out = dict.new()\n    var i = 0\n    var last = dict.new()\n    while i < n:\n        last = out\n        put(out, i)\n        i = i + 1\n    let _ = size(last)\n    out\n",
            "a loop back edge carries the alias to the return",
        );
        rejected(
            "    var out = dict.new()\n    if n > 0:\n        out = d\n    out\n",
            "one arm rebinds the local to shared storage",
        );
        rejected(
            "    var out = dict.new()\n    let count = fn(): size(out)\n    let _ = count()\n    out\n",
            "a closure captures the local",
        );
        rejected(
            "    var out = dict.new()\n    let pair = (out, 1)\n    let _ = pair\n    out\n",
            "an aggregate element may share the root",
        );
        rejected(
            "    var out = dict.new()\n    let _ = size(out)\n    out\n",
            "a `let` borrow receives a shared token",
        );
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
        check_str("type Tree:\n    Leaf\n    Node(Tree, Int, Tree)\n")
            .expect("recursive type is valid");
        check_str("type Box:\n    Box(a)\n").expect("generic type is valid");
    }

    #[test]
    fn type_argument_arity_is_checked() {
        let scalar = check_str("fn f(x: Int(String)) -> Int:\n    x\n").unwrap_err();
        assert!(
            scalar.contains("type `Int` expects 0 type argument(s) but got 1"),
            "{scalar}"
        );

        let missing_list = check_str("fn f(xs: List) -> Int:\n    0\n").unwrap_err();
        assert!(
            missing_list.contains("type `List` expects 1 type argument(s) but got 0"),
            "{missing_list}"
        );

        let extra_list = check_str("fn f(xs: List(Int, String)) -> Int:\n    0\n").unwrap_err();
        assert!(
            extra_list.contains("type `List` expects 1 type argument(s) but got 2"),
            "{extra_list}"
        );

        let result = check_str("fn f(r: Result(Int)) -> Int:\n    0\n").unwrap_err();
        assert!(
            result.contains("type `Result` expects 2 type argument(s) but got 1"),
            "{result}"
        );

        let dict = check_str("fn f(d: Dict(String, Int, Bool)) -> Int:\n    0\n").unwrap_err();
        assert!(
            dict.contains("type `Dict` expects 2 type argument(s) but got 3"),
            "{dict}"
        );

        let user_missing =
            check_str("type Pair(a, b):\n    Pair(a, b)\nfn f(p: Pair(Int)) -> Int:\n    0\n")
                .unwrap_err();
        assert!(
            user_missing.contains("type `Pair` expects 2 type argument(s) but got 1"),
            "{user_missing}"
        );

        let user_extra =
            check_str("type Box:\n    Box(a)\nfn f(b: Box(Int, String)) -> Int:\n    0\n")
                .unwrap_err();
        assert!(
            user_extra.contains("type `Box` expects 1 type argument(s) but got 2"),
            "{user_extra}"
        );

        let local = check_str(
            "fn main(console: Console):\n    let xs: List = []\n    console.print(\"bad\")\n",
        )
        .unwrap_err();
        assert!(
            local.contains("type `List` expects 1 type argument(s) but got 0"),
            "{local}"
        );

        check_str(
            "fn f(xs: List(Int), r: Result(Int, String), d: Dict(String, Int)) -> Int:\n    0\n",
        )
        .expect("valid builtin generic arities pass");
        check_str("type Pair(a, b):\n    Pair(a, b)\nfn f(p: Pair(Int, String)) -> Int:\n    0\n")
            .expect("valid explicit ADT arity passes");
        check_str("type Box:\n    Box(a)\nfn f(b: Box(Int)) -> Int:\n    0\n")
            .expect("valid inferred ADT arity passes");
        check_str(
            "fn f(dir: Dir[Read], file: File[Read, Write], net: Net[Connect]) -> Int:\n    0\n",
        )
        .expect("capability right markers are not ordinary type arguments");
    }

    #[test]
    fn unknown_trait_names_are_rejected() {
        let impl_head = check_str(
            "type Box:\n    Box(Int)\nimpl Missing for Box:\n    fn label(self) -> String:\n        \"x\"\n",
        )
        .unwrap_err();
        assert!(
            impl_head.contains("unknown trait `Missing` in impl head"),
            "{impl_head}"
        );

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
        assert!(
            missing.contains("missing required method `compare`"),
            "{missing}"
        );

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
            wrong_param.contains(
                "method `combine` parameter 2 has type `Int`, but the trait requires `R`"
            ),
            "{wrong_param}"
        );

        check_str(
            "type R:\n    R(Int)\ntrait Ranked:\n    fn compare(self, other: Self) -> Int\n    fn greater(self, other: Self) -> Bool:\n        compare(self, other) > 0\nimpl Ranked for R:\n    fn compare(self, other: R) -> Int:\n        1\n    fn greater(self, other: R) -> Bool:\n        true\n",
        )
        .expect("required methods plus default overrides are valid");

        // (BUG-604) An OMITTED return annotation inherits the trait's declared
        // return type — like unannotated parameters — instead of being read as
        // unit and rejected as a contract mismatch.
        check_str(
            "type R:\n    R(Int)\ntrait Label:\n    fn label(self) -> String\nimpl Label for R:\n    fn label(self):\n        \"r\"\n",
        )
        .expect("an unannotated impl return inherits the trait's return type");

        // The inherited return type still binds the body: a wrong body fails
        // the ordinary function-body check, not silently.
        let wrong_body = check_str(
            "type R:\n    R(Int)\ntrait Label:\n    fn label(self) -> String\nimpl Label for R:\n    fn label(self):\n        1\n",
        )
        .unwrap_err();
        assert!(
            wrong_body.contains("expected `String`, found `Int`"),
            "{wrong_body}"
        );
    }

    #[test]
    fn build_entrypoint_takes_only_build_capabilities() {
        // A valid build step: build caps only.
        check_str("fn build(out: BuildOut, schema: BuildRead):\n    out.write_out(\"x.witchy\", schema.read_build(\"a.proto\"))\n")
            .expect("a build step taking build caps is valid");
        // A runtime capability in `build` is rejected — the build sandbox grants
        // only build-time authority.
        let err =
            check_str("fn build(out: BuildOut, net: Net):\n    out.write_out(\"x\", \"y\")\n")
                .expect_err("a runtime cap in build must be rejected");
        assert!(
            err.contains("build step may only take build-time capabilities"),
            "{err}"
        );
        // And `main` may not take a build capability.
        let err =
            check_str("fn main(console: Console, out: BuildOut):\n    console.print(\"no\")\n")
                .expect_err("a build cap in main must be rejected");
        assert!(
            err.contains("`main` may only take host capabilities"),
            "{err}"
        );
        // A `build` function with no build cap is an ordinary function, not the
        // entrypoint, so it isn't subject to the build-signature rule.
        check_str("fn build(x: Int) -> Int:\n    x + 1\n")
            .expect("a plain `build` function is not the build entrypoint");
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

        check_str(
            "fn open(console: Console, r: Result(File, String)):\n    console.print(\"x\")\n",
        )
        .expect("Result(File, _) uses a closed typed GC sum");

        // Dict(String, File) — the value is slot-boxed.
        let err = check_str(
            "fn table(console: Console, d: Dict(String, File)):\n    console.print(\"x\")\n",
        )
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

        check_str("fn keep(pair: (File, Int)) -> (File, Int):\n    pair\n")
            .expect("a concrete capability tuple preserves its GC shape");

        check_str("fn maybe(pair: (Option(File), Int)) -> (Option(File), Int):\n    pair\n")
            .expect("a nullable direct externref remains reference-typed in a tuple");

        check_str("fn main(console: Console, f: File[Read]):\n    let read_later = fn() -> String: f.read() ?? \"\"\n    console.print(\"x\")\n")
            .expect("a closure capture of File uses a typed GC environment");

        check_str(
            "fn main(console: Console, f: File):\n    let xs = [f]\n    console.print(\"x\")\n",
        )
        .expect("an inferred List(File) literal receives an externref-typed GC array");

        check_str("fn main(console: Console, f: File):\n    let pair = (f, 1)\n    console.print(\"x\")\n")
            .expect("an inferred concrete File tuple uses typed GC-struct storage");

        let err = check_str("fn id(x: a) -> a:\n    x\nfn main(console: Console, f: File):\n    let pair = id((f, 1))\n    console.print(\"x\")\n")
            .expect_err("a capability tuple cannot instantiate the scalar generic ABI");
        assert!(
            err.contains("generic") && err.contains("File"),
            "got: {err}"
        );

        check_str(
            "fn collect(console: Console, xs: List((File, Int))):\n    console.print(\"x\")\n",
        )
        .expect("a list of capability tuples uses a typed GC array");

        let err =
            check_str("fn main(console: Console, f: File):\n    console.print(\"${(f, 1)}\")\n")
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
        assert!(
            err.contains("pixels") && err.contains("packed"),
            "got: {err}"
        );

        check_str("fn id(x: (File, Int)) -> (File, Int):\n    x\nfn main(console: Console, f: File):\n    let local = id\n    let pair = local((f, 1))\n    console.print(\"x\")\n")
            .expect("a concrete capability tuple crosses the typed indirect-call ABI");

        check_str("fn main(console: Console, f: File):\n    let local = fn(x: File) -> (File, Int): (x, 1)\n    console.print(\"x\")\n")
            .expect("a lambda may expose a capability through its typed signature");

        let err = check_str("fn main(console: Console, f: File):\n    let d = dict.__insert(dict.new(), \"cfg\", f)\n    console.print(\"x\")\n")
            .expect_err("an inferred Dict(String, File) needs the GC-struct aggregate path");
        assert!(
            err.contains("generic") && err.contains("File"),
            "got: {err}"
        );

        check_str("type Box(a):\n    Box(a)\nfn main(console: Console, f: File):\n    let b: Box(File) = Box(f)\n    console.print(\"x\")\n")
            .expect("a closed generic user aggregate instantiated with File receives a GC layout");

        check_str("fn get(console: Console, o: Option(Secret)):\n    console.print(\"x\")\n")
            .expect("Secret is externref-backed, so direct nullable Option(Secret) is allowed");

        check_str("fn collect(console: Console, xs: List(Secret)):\n    console.print(\"x\")\n")
            .expect("List(Secret) uses an externref-typed GC array");

        check_str(
            "fn maybe_dir(console: Console, out: Option(Dir[Write])):\n    console.print(\"x\")\n",
        )
        .expect("Dir is externref-backed this stage, so direct nullable Option(Dir) is allowed");

        let branded_dir = r#"
capability ConfigDir from Dir[Read]

fn load(c: ConfigDir, name: String) -> String:
    match c:
        ConfigDir(dir) -> dir.read(name) ?? ""
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
    w.dir.read(name) ?? ""

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
    o.inner.dir.read(name) ?? ""
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
            let src =
                format!("fn hold(x: {ty}):\n    let later = fn():\n        let captured = x\n");
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
        let error = check_str("fn hold(x: Dict(String, fn(Int) -> Int)):\n    let captured = x\n")
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
        assert!(
            bad.contains("`main` returns `Result(Int, String)`"),
            "{bad}"
        );
        assert!(bad.contains("exit code"), "{bad}");
        // `Option` is the same trap (a dropped `None`).
        let bad_opt =
            check_str("fn main(console: Console) -> Option(Int):\n    None\n").unwrap_err();
        assert!(
            bad_opt.contains("`main` returns `Option(Int)`"),
            "{bad_opt}"
        );
        // Plain value returns are NOT rejected — the value sink surfaces them: an
        // `Int` exit code, a printed `Float`, an explicit `Nil`, and no annotation
        // (implicit unit) all pass. (The `Float`-returning main is a tested feature.)
        check_str("fn main(console: Console) -> Int:\n    0\n").expect("Int exit code is valid");
        check_str("import math\nfn main() -> Float:\n    math.sqrt(4.0)\n")
            .expect("Float main is valid");
        check_str("fn main(console: Console) -> ():\n    console.print(\"x\")\n")
            .expect("explicit unit is valid");
        check_str("fn main(console: Console):\n    console.print(\"x\")\n")
            .expect("no annotation is valid");
    }

    #[test]
    fn derive_eq_ord_rejects_float_fields() {
        let eq = check_str("import cmp\n\ntype Reading derive(PartialEq, Eq):\n    value: Float\n")
            .expect_err("Float is not Eq");
        assert!(
            eq.contains("derive(Eq)") && eq.contains("Float is not Eq"),
            "{eq}"
        );

        let ord = check_str(
            "import cmp\n\ntype Reading derive(PartialEq, PartialOrd, Ord):\n    value: Float\n",
        )
        .expect_err("Float is not Ord");
        assert!(
            ord.contains("derive(Ord)") && ord.contains("Float is not Ord"),
            "{ord}"
        );

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

        let value = check_str(
            "type Marker:\n\nfn main(console: Console):\n    console.print(\"${Marker()}\")\n",
        )
        .expect_err("a fieldless type has no constructor");
        assert!(value.contains("type `Marker` is not a value"), "{value}");

        for derive_name in [
            "Show",
            "PartialEq",
            "Eq",
            "PartialOrd",
            "Ord",
            "Reflect",
            "Deserialize",
        ] {
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
        let net = check_str(
            r#"
fn f(c: Console) -> ():
    c.connect("host")
"#,
        )
        .unwrap_err();
        assert!(
            net.contains("no method `connect` on `Console`"),
            "expected a receiver-aware cap-op rejection, got: {net}"
        );
        let dir = check_str(
            r#"
fn f(c: Console) -> String:
    c.read("/etc/passwd")
"#,
        )
        .unwrap_err();
        assert!(
            dir.contains("no method `read` on `Console`"),
            "expected a receiver-aware cap-op rejection, got: {dir}"
        );
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

    /// Capability operations are methods at the source level: old bare spellings
    /// are rejected before ordinary capability type-checking.
    #[test]
    fn rejects_bare_capability_ops() {
        let src = r#"
fn leak(s: String) -> ():
    print(s, s)
"#;
        let e = check_str(src).unwrap_err();
        assert!(
            e.contains("method-only") && e.contains("console.print"),
            "got: {e}"
        );
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
    fn own_arguments_and_explicit_move_consume_in_both_modes() {
        let normal = r#"
fn take(own s: String) -> String:
    s

fn main() -> Int:
    let x = "hi"
    let a = take(x)
    let b = take(x)
    a.length() + b.length() + x.length()
"#;
        for mode in ["", "mode opt\n\n"] {
            let source = format!("{mode}{normal}");
            let error = check_str(&source)
                .expect_err("own transfers the source binding in every mode");
            assert!(error.contains("moved"), "got: {error}");

            let explicit = source.replace("let a = take(x)", "let a = take(move x)");
            let error = check_str(&explicit)
                .expect_err("explicit move consumes its source in every mode");
            assert!(error.contains("moved"), "got: {error}");
        }
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
        assert!(
            err.contains("`listen` needs `Listen`") && err.contains("Net[Connect]"),
            "got: {err}"
        );
        // A listen-only handle accepts inbound.
        check_str("fn f(console: Console, net: Net[Listen]):\n    let l = net.listen(\"0.0.0.0:80\")\n    console.print(\"ok\")\n")
            .expect("Net[Listen] can listen");
        // ... but it cannot dial out (`connect`).
        let err = check_str("fn f(net: Net[Listen]):\n    net.connect(\"example.com:443\")\n")
            .expect_err("Net[Listen] must not connect");
        assert!(
            err.contains("`connect` needs `Connect`") && err.contains("Net[Listen]"),
            "got: {err}"
        );
        // The transport axis attenuates independently: `connect`/`listen` are TCP-only,
        // so a UDP-only handle (full verbs, no TCP) cannot dial.
        let err = check_str("fn f(net: Net[Udp]):\n    net.connect(\"example.com:443\")\n")
            .expect_err("Net[Udp] must not connect (TCP-only op)");
        assert!(
            err.contains("only implemented over `Tcp`") && err.contains("Net[Udp]"),
            "got: {err}"
        );
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
        check_str("fn f(console: Console, d: Dir[Read]):\n    let s = d.read(\"a.txt\") ?? \"\"\n    console.print(s)\n")
            .expect("Dir[Read] can read");
        // ... but it cannot `write`, `append`, or `make_dir` (all `Write` verbs).
        let err = check_str("fn f(d: Dir[Read]):\n    d.write(\"a.txt\", \"x\")\n")
            .expect_err("Dir[Read] must not write");
        assert!(
            err.contains("`write` needs `Write`") && err.contains("Dir[Read]"),
            "got: {err}"
        );
        let err = check_str("fn f(d: Dir[Read]):\n    d.make_dir(\"sub\")\n")
            .expect_err("Dir[Read] must not make_dir");
        assert!(
            err.contains("`make_dir` needs `Write`") && err.contains("Dir[Read]"),
            "got: {err}"
        );
        // A write-only handle writes ...
        check_str("fn f(d: Dir[Write]):\n    d.write(\"a.txt\", \"x\")\n")
            .expect("Dir[Write] can write");
        // ... but cannot `read` or `list` (both `Read` verbs). This is the converse
        // the File slice never asserted.
        let err = check_str("fn f(d: Dir[Write]):\n    d.read(\"a.txt\")\n")
            .expect_err("Dir[Write] must not read");
        assert!(
            err.contains("`read` needs `Read`") && err.contains("Dir[Write]"),
            "got: {err}"
        );
        let err = check_str("fn f(d: Dir[Write]):\n    d.list()\n")
            .expect_err("Dir[Write] must not list");
        assert!(
            err.contains("`list` needs `Read`") && err.contains("Dir[Write]"),
            "got: {err}"
        );
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
    fn policy_narrowing_preserves_rights_and_cannot_rewiden() {
        // `net.only(policy)` keeps the receiver's rights: the result is still
        // connect-capable, so `connect` on it type-checks.
        check_str("type NetPolicy:\n    NetPolicy(String)\nfn f(console: Console, net: Net[Connect]):\n    let scoped = net.only(NetPolicy(\"example.com:443\")) ?? net\n    let s = scoped.connect(\"example.com:443\")\n    console.print(\"ok\")\n")
            .expect("net.only preserves the Connect right");
        // ... and because the rights are preserved (still connect-only, not full),
        // the narrowed handle cannot be re-widened to a full `Net` by a cast.
        let err = check_str("type NetPolicy:\n    NetPolicy(String)\nfn f(console: Console, net: Net[Connect]):\n    let scoped = net.only(NetPolicy(\"example.com:443\")) ?? net\n    let wide = scoped as Net\n    console.print(\"no\")\n")
            .expect_err("a policy-narrowed Net[Connect] must not re-widen to full Net");
        assert!(err.contains("can only drop rights"), "got: {err}");
        // `dir.only(policy)` likewise keeps `Read` ...
        check_str("type DirPolicy:\n    DirPolicy(String)\nfn f(console: Console, d: Dir[Read]):\n    let scoped = d.only(DirPolicy(\"ext:txt\"))\n    let s = scoped.read(\"a.txt\") ?? \"\"\n    console.print(s)\n")
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
    fn equality_on_fn_or_cap_fields_of_records_and_enums_is_rejected() {
        // (BUG-302) a record/enum carrying a fn or capability field must reject `==`
        // at check time, so both backends agree (interp compared by closure identity,
        // compiled rejected — a parity divergence).
        let rec = check_str(
            "type H:\n    run: fn(Int) -> Int\nfn add1(x: Int) -> Int:\n    x + 1\nfn f() -> Bool:\n    H(add1) == H(add1)\n",
        )
        .unwrap_err();
        assert!(
            rec.contains("not defined on function types") && rec.contains("H"),
            "{rec}"
        );
        let en = check_str(
            "type W:\n    Func(fn(Int) -> Int)\nfn add1(x: Int) -> Int:\n    x + 1\nfn f() -> Bool:\n    Func(add1) == Func(add1)\n",
        )
        .unwrap_err();
        assert!(en.contains("not defined on function types"), "{en}");
        let cap =
            check_str("type Hold:\n    c: Console\nfn f(a: Hold, b: Hold) -> Bool:\n    a == b\n")
                .unwrap_err();
        assert!(cap.contains("not defined on capability types"), "{cap}");
        // A plain data record still compares.
        check_str("type P:\n    x: Int\n    y: Int\nfn f(a: P, b: P) -> Bool:\n    a == b\n")
            .expect("a plain data record is comparable");
    }

    #[test]
    fn duplicate_declarations_are_rejected() {
        // (BUG-230) types, constructors, and methods get the same "defined more than
        // once" error the function namespace already gives.
        let ty = check_str("type T:\n    A\ntype T:\n    B\n").unwrap_err();
        assert!(ty.contains("type `T` is defined more than once"), "{ty}");
        let identical = check_str("type T:\n    A\ntype T:\n    A\n").unwrap_err();
        assert!(
            identical.contains("type `T` is defined more than once"),
            "{identical}"
        );
        let type_param = check_str("type Pair(a, a):\n    Pair(a, a)\n").unwrap_err();
        assert!(
            type_param.contains("type parameter `a` is declared more than once in type `Pair`"),
            "{type_param}"
        );
        let trait_param =
            check_str("trait Codec(a, a):\n    fn encode(self) -> String\n").unwrap_err();
        assert!(
            trait_param.contains("type parameter `a` is declared more than once in trait `Codec`"),
            "{trait_param}"
        );
        let konst = check_str("let ANSWER = 1\nlet ANSWER = 2\nfn main(console: Console):\n    console.print(\"${ANSWER}\")\n")
            .unwrap_err();
        assert!(
            konst.contains("constant `ANSWER` is defined more than once"),
            "{konst}"
        );
        let alias =
            check_str("type Id = Int\ntype Id = String\nfn f(x: Id) -> Id:\n    x\n").unwrap_err();
        assert!(
            alias.contains("type alias `Id` is defined more than once"),
            "{alias}"
        );
        let alias_param = check_str("type Pair(a, a) = (a, a)\n").unwrap_err();
        assert!(
            alias_param
                .contains("type parameter `a` is declared more than once in type alias `Pair`"),
            "{alias_param}"
        );
        let alias_unbound_param = check_str("type Bad(a) = (a, b)\n").unwrap_err();
        assert!(
            alias_unbound_param
                .contains("type alias `Bad` uses type parameter `b` but does not declare it"),
            "{alias_unbound_param}"
        );
        let alias_type = check_str("type Id = Int\ntype Id:\n    Id(String)\n").unwrap_err();
        assert!(
            alias_type.contains("type `Id` conflicts with type alias `Id`"),
            "{alias_type}"
        );
        let fields = check_str("type Point:\n    x: Int\n    x: String\nfn main(console: Console):\n    console.print(\"ok\")\n")
            .unwrap_err();
        assert!(
            fields.contains("field `x` is declared more than once in type `Point`"),
            "{fields}"
        );
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
        assert!(
            meth.contains("method `value` is defined more than once"),
            "{meth}"
        );
        let trait_dup =
            check_str("trait Two:\n    fn m(self) -> Int\n    fn m(self) -> Int\n").unwrap_err();
        assert!(
            trait_dup.contains("method `m` is declared more than once"),
            "{trait_dup}"
        );
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
    fn generic_dict_key_operation_requires_hash_bound() {
        // (BUG-395 / RFC-0047) A generic helper performing a `Dict` key operation
        // must carry a `where k: Hash` bound — the key is hashed and compared.
        for op in [
            "dict.get_or(d, key, fallback)",
            "dict.__insert(d, key, fallback)",
            "dict.contains_key(d, key)",
            "dict.__remove(d, key)",
        ] {
            let src =
                format!("fn f(d: Dict(k, v), key: k, fallback: v) -> v:\n    {op}\n    fallback\n");
            let err = check_str(&src).unwrap_err();
            assert!(err.contains("requires `k: Hash`"), "{op}: {err}");
        }
        // With the bound it type-checks (checked through monomorphization).
        check_str("fn f(d: Dict(k, v), key: k, fallback: v) -> v where k: Hash:\n    dict.get_or(d, key, fallback)\n")
            .expect("a `where k: Hash` generic dict helper is accepted");
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
    fn rfc0087_function_values_retain_and_enforce_conventions() {
        let immutable = check_str(
            "fn bump(var n: Int) -> Int:\n    n\n\nfn main():\n    let n = 1\n    let f: fn(var Int) -> Int = bump\n    let _ = f(n)\n",
        )
        .expect_err("an indirect var call must reject an immutable argument");
        assert!(immutable.contains("mutable `var`"), "{immutable}");

        let mismatch = check_str(
            "fn pure(n: Int) -> Int:\n    n\n\nfn use(f: fn(var Int) -> Int) -> ():\n    return\n\nfn main():\n    use(pure)\n",
        )
        .expect_err("function conventions are part of type identity");
        assert!(
            mismatch.contains("fn(var Int) -> Int") && mismatch.contains("fn(Int) -> Int"),
            "{mismatch}"
        );
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
        assert!(
            err.contains("unknown trait `Render` in `dyn Render`"),
            "{err}"
        );
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
        .expect_err("the false branch is unit, not an existential");
        assert!(
            err.contains("`if` without `else` has an implicit `()` path")
                && err.contains("add an explicit `else` branch"),
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
                error.contains(method) && (error.contains("Render") || error.contains("trait")),
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
    fn target_availability_rejects_cross_target_calls_and_function_values() {
        check_str(
            "@browser\nfn browser_api() -> Int:\n    1\n\n\
             @browser\nfn browser_feature() -> Int:\n    browser_api()\n\n\
             fn shared() -> Int:\n    2\n\n\
             @server\nfn server_feature() -> Int:\n    shared()\n",
        )
        .expect("same-target and shared references are available");

        let cross_call = check_str(
            "@server\nfn load_secret() -> String:\n    \"secret\"\n\n\
             @browser\nfn render() -> String:\n    load_secret()\n",
        )
        .expect_err("browser code must not call server-only code");
        assert!(
            cross_call.contains("browser")
                && cross_call.contains("server-only")
                && cross_call.contains("load_secret"),
            "{cross_call}",
        );

        let shared_reference = check_str(
            "@browser\nfn browser_api() -> Int:\n    1\n\n\
             fn shared_callback() -> fn() -> Int:\n    browser_api\n",
        )
        .expect_err("shared code must not retain a browser-only function value");
        assert!(
            shared_reference.contains("shared")
                && shared_reference.contains("browser-only")
                && shared_reference.contains("browser_api"),
            "{shared_reference}",
        );

        check_str(
            "@browser\nfn browser_api() -> Int:\n    1\n\n\
             fn apply(callback: fn() -> Int) -> Int:\n    callback()\n\n\
             @browser\nfn render() -> Int:\n    apply(browser_api)\n",
        )
        .expect(
            "a shared higher-order helper preserves availability checked at the reference site",
        );

        let method_body = check_str(
            "@browser\nfn browser_api() -> Int:\n    1\n\n\
             type View:\n    View\n\n\
             impl View:\n    fn render(self) -> Int:\n        browser_api()\n\n\
             fn shared(view: View) -> Int:\n    view.render()\n",
        )
        .expect_err("lowered shared methods must not call target-only functions");
        assert!(
            method_body.contains("shared")
                && method_body.contains("browser-only")
                && method_body.contains("browser_api"),
            "{method_body}",
        );
    }

    #[test]
    fn bare_slice_and_str_rejected_by_typeck() {
        let err_slice = check_str("mode opt\n\nfn process(xs: [Int]) -> Int:\n    0\n")
            .expect_err("bare [Int] must be rejected");
        assert!(
            err_slice.contains("slice type `[Int]` cannot appear as a bare value"),
            "{err_slice}"
        );

        let err_str = check_str("mode opt\n\nfn process(s: str) -> Int:\n    0\n")
            .expect_err("bare str must be rejected");
        assert!(
            err_str.contains("string slice `str` cannot appear as a bare value"),
            "{err_str}"
        );
    }

}

// The crate's library target is `test = false`: every in-crate unit test that
// survived the 2026-09-24 cut lives in these modules, compiled against the
// ordinary (non-`cfg(test)`) library build.
#[path = "typeck/access.rs"]
mod access;
#[path = "typeck/async_view.rs"]
mod async_view;
#[path = "typeck/async_view_results.rs"]
mod async_view_results;
#[path = "typeck/async_contract.rs"]
mod async_contract;
#[path = "typeck/capabilities.rs"]
mod capabilities;
#[path = "typeck/checker.rs"]
mod checker;
#[path = "typeck/dict_view_diagnostic.rs"]
mod dict_view_diagnostic;
#[path = "typeck/dir_metadata.rs"]
mod dir_metadata;
#[path = "typeck/frozen_view_matrix.rs"]
mod frozen_view_matrix;
#[path = "typeck/loans.rs"]
mod loans;
#[path = "typeck/list_push_value.rs"]
mod list_push_value;
#[path = "typeck/nullable_capabilities.rs"]
mod nullable_capabilities;
#[path = "typeck/own.rs"]
mod own;
#[path = "typeck/own_view_consumption.rs"]
mod own_view_consumption;
#[path = "typeck/optimization_readiness.rs"]
mod optimization_readiness;
#[path = "typeck/opt_callable_provenance.rs"]
mod opt_callable_provenance;
#[path = "typeck/opt_stored_callback_invocation.rs"]
mod opt_stored_callback_invocation;
#[path = "typeck/nested_opt_callable_contracts.rs"]
mod nested_opt_callable_contracts;
#[path = "typeck/opt_callable_erasure.rs"]
mod opt_callable_erasure;
#[path = "typeck/consuming_record_reconstruction.rs"]
mod consuming_record_reconstruction;
#[path = "typeck/reference_erasure.rs"]
mod reference_erasure;
#[path = "typeck/reference_contract_matrix.rs"]
mod reference_contract_matrix;
#[path = "typeck/region_parent_view_results.rs"]
mod region_parent_view_results;
#[path = "typeck/resource_composition.rs"]
mod resource_composition;
#[path = "typeck/resource_fallback.rs"]
mod resource_fallback;
#[path = "typeck/source.rs"]
mod source;
#[path = "typeck/std_pruning.rs"]
mod std_pruning;
#[path = "typeck/unit_results.rs"]
mod unit_results;
#[path = "typeck/unique_writeback_matrix.rs"]
mod unique_writeback_matrix;
#[path = "typeck/views.rs"]
mod views;
#[path = "typeck/view_witnesses.rs"]
mod view_witnesses;
#[path = "typeck/vm_service.rs"]
mod vm_service;
#[path = "typeck/vm_worker_isolation.rs"]
mod vm_worker_isolation;
#[path = "typeck/lazy_operand_ownership.rs"]
mod lazy_operand_ownership;
#[path = "typeck/consuming_sum_operands.rs"]
mod consuming_sum_operands;

#[path = "typeck/deferred_task_roles.rs"]
mod deferred_task_roles;

#[path = "typeck/match_guard_disposition.rs"]
mod match_guard_disposition;

#[path = "typeck/resource_exit_disposition.rs"]
mod resource_exit_disposition;

#[path = "typeck/expression_exit_disposition.rs"]
mod expression_exit_disposition;

#[path = "typeck/expression_operand_specialization.rs"]
mod expression_operand_specialization;
