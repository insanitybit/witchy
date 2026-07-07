    use super::*;

    #[test]
    fn packed_type_requires_packable_fields() {
        // (RFC-0027) scalars (and nested packed types) are packable.
        assert!(check_str(
            "type Point packed:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    print(console, \"${Point(1, 2).x}\")\n"
        )
        .is_ok());
        assert!(check_str(
            "type Inner packed:\n    a: Int\n\ntype Outer packed:\n    i: Inner\n    b: Bool\n\nfn main(console: Console):\n    print(console, \"hi\")\n"
        )
        .is_ok());
        // A variable-size field (String) makes the type unpackable — error naming it.
        let err = check_str(
            "type Bad packed:\n    s: String\n    n: Int\n\nfn main(console: Console):\n    print(console, \"hi\")\n"
        )
        .unwrap_err();
        assert!(err.contains("packed") && err.contains("`s`"), "{err}");
        // A non-packed record field is also unpackable.
        let nested = check_str(
            "type Plain:\n    a: Int\n\ntype Bad2 packed:\n    p: Plain\n\nfn main(console: Console):\n    print(console, \"hi\")\n"
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
            "grantable capability UiRoot:\n    policy: String\n\nfn main(console: Console, ui: UiRoot):\n    print(console, \"ok\")\n"
        )
        .is_ok());
        // An ordinary (non-grantable) user type at `main` is still rejected.
        let err = check_str(
            "type Config:\n    Config(Int)\n\nfn main(console: Console, c: Config):\n    print(console, \"hi\")\n"
        )
        .unwrap_err();
        assert!(err.contains("main") && err.contains("Config"), "{err}");
        // A non-grantable *capability* (RFC-0002 refinement) is likewise not root-grantable.
        let refined = check_str(
            "capability Redis from Net[Connect]\n\nfn main(console: Console, r: Redis):\n    print(console, \"hi\")\n"
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
    fn packed_list_cannot_cross_a_boundary() {
        // (RFC-0027) a `packed` type's list is a CONFINED LOCAL flat buffer with no
        // cross-function or stored layout — a `List(P)` in a parameter, return type,
        // or stored field is a clean compile error (never a silent boxed fall-back).
        let param = check_str(
            "import list\ntype P packed:\n    x: Int\nfn f(ps: List(P)) -> Int:\n    list.length(ps)\nfn main(console: Console):\n    print(console, \"hi\")\n"
        ).unwrap_err();
        assert!(param.contains("List(P)") && param.contains("parameter"), "{param}");
        let ret = check_str(
            "import list\ntype P packed:\n    x: Int\nfn f() -> List(P):\n    [P(1)]\nfn main(console: Console):\n    print(console, \"hi\")\n"
        ).unwrap_err();
        assert!(ret.contains("List(P)") && ret.contains("return"), "{ret}");
        let field = check_str(
            "type P packed:\n    x: Int\n\ntype Holder:\n    ps: List(P)\n\nfn main(console: Console):\n    print(console, \"hi\")\n"
        ).unwrap_err();
        assert!(field.contains("List(P)") && field.contains("field"), "{field}");
    }

    #[test]
    fn frozen_value_cannot_be_declared_mutable() {
        // (RFC-0025) `frozen` is deeply immutable, so it cannot also be `var`/`own`.
        let var_let = check_str(
            "fn main(console: Console):\n    var x: frozen List(Int) = [1, 2]\n    print(console, \"hi\")\n",
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
        check_str("fn main(console: Console):\n    let x: frozen List(Int) = [1, 2]\n    print(console, __render(list.length(x)))\n")
            .expect("a let-bound frozen value is valid");
        // `unique`/`local unique` are compatible with mutation (FBIP) — `var` is
        // fine. A mutator shape (self-typed return) satisfies RFC-0064's row-3
        // rule (a `var` receiver must be a mutator or a procedure).
        check_str("fn f(var xs: unique List(Int)) -> List(Int):\n    xs\n")
            .expect("a unique var is valid (in-place reuse is the point)");
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

        let local = check_str("fn main(console: Console):\n    let xs: List = []\n    print(console, \"bad\")\n")
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
    fn trait_impls_must_match_trait_methods() {
        let misspelled = check_str(
            "type R:\n    R(Int)\ntrait PartialLike:\n    fn partial_compare(self, other: Self) -> Option(Int)\nimpl PartialLike for R:\n    fn partial_cmp(self, other: R) -> Option(Int):\n        Some(1)\nfn main(console: Console):\n    print(console, \"ok\")\n",
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
    fn generic_trait_impls_preserve_function_type_arguments() {
        check_str(
            "trait Label:\n    fn label(self) -> String\n\
             type Box(a):\n    value: a\n\
             impl Label for Box(a):\n    fn label(self) -> String:\n        \"Box\"\n\
             fn id(n: Int) -> Int:\n    n\n\
             fn main(console: Console):\n    let b: Box(fn(Int) -> Int) = Box(id)\n    print(console, b.label())\n",
        )
        .expect("generic trait dispatch preserves a function-typed type argument");

        check_str(
            "trait Label:\n    fn label(self) -> String\n\
             type Box(a):\n    value: a\n\
             impl Label for Box(a):\n    fn label(self) -> String:\n        \"Box\"\n\
             fn id(n: Int) -> Int:\n    n\n\
             fn main(console: Console):\n    let b: Box(List(fn(Int) -> Int)) = Box([id])\n    print(console, b.label())\n",
        )
        .expect("nested function-typed type arguments keep their full scope encoding");

        check_str(
            "trait Label:\n    fn label(self) -> String\n\
             type Box(a):\n    value: a\n\
             impl Label for Box(a):\n    fn label(self) -> String:\n        \"Box\"\n\
             fn choose(n: Int, s: String) -> Int:\n    n\n\
             fn main(console: Console):\n    let b: Box(fn(Int, String) -> Int) = Box(choose)\n    print(console, b.label())\n",
        )
        .expect("function-typed type arguments may contain parameter commas");
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
             fn main(console: Console):\n    print(console, pair_tags(A, B))\n",
        )
        .expect("static trait methods dispatch through the bound variable, not the method name");
    }

    #[test]
    fn build_entrypoint_takes_only_build_capabilities() {
        // A valid build step: build caps only.
        check_str("fn build(out: BuildOut, schema: BuildRead):\n    write_out(out, \"x.witchy\", read_build(schema, \"a.proto\"))\n")
            .expect("a build step taking build caps is valid");
        // A runtime capability in `build` is rejected — the build sandbox grants
        // only build-time authority.
        let err = check_str("fn build(out: BuildOut, net: Net):\n    write_out(out, \"x\", \"y\")\n")
            .expect_err("a runtime cap in build must be rejected");
        assert!(err.contains("build step may only take build-time capabilities"), "{err}");
        // And `main` may not take a build capability.
        let err = check_str("fn main(console: Console, out: BuildOut):\n    print(console, \"no\")\n")
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
    fn duplicate_parameter_names_are_rejected() {
        let top = check_str("fn pick(x: Int, x: Int) -> Int:\n    x\n")
            .expect_err("duplicate function parameters must be rejected");
        assert!(top.contains("parameter `x`") && top.contains("function `pick`"), "{top}");

        let lambda = check_str(
            "fn main(console: Console):\n    let f = fn(x: Int, x: Int): x\n    print(console, \"bad\")\n",
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
        // (RFC-0005 §4.4/§7) `File` is (to be) an unforgeable `externref` with no
        // boxed i64-slot representation, so it cannot be wrapped in `Option`/
        // `Result`/`List`/`Dict` — the containers whose payload crosses the slot.
        // A bare `File` param/return stays an `externref` and is fine.
        check_str("fn ok(console: Console, f: File):\n    print(console, \"ok\")\nfn main(console: Console, f: File):\n    ok(console, f)\n")
            .expect("a bare File param/return is a plain externref — allowed");

        // Option(File) — the payload is slot-boxed. (The reject fires on the
        // signature, so a trivial body suffices.)
        let err = check_str("fn find(console: Console, o: Option(File)):\n    print(console, \"x\")\n")
            .expect_err("Option(File) slot-boxes an externref");
        assert!(err.contains("File") && err.contains("Option"), "got: {err}");

        // List(File) — the collection stores externref elements (§7).
        let err = check_str("fn collect(console: Console, xs: List(File)):\n    print(console, \"x\")\n")
            .expect_err("List(File) stores externref elements");
        assert!(err.contains("File") && err.contains("List"), "got: {err}");

        // Result(File, String) — the Ok payload is slot-boxed.
        let err = check_str("fn open(console: Console, r: Result(File, String)):\n    print(console, \"x\")\n")
            .expect_err("Result(File, _) slot-boxes an externref");
        assert!(err.contains("File") && err.contains("Result"), "got: {err}");

        // Dict(String, File) — the value is slot-boxed.
        let err = check_str("fn table(console: Console, d: Dict(String, File)):\n    print(console, \"x\")\n")
            .expect_err("Dict(_, File) slot-boxes an externref value");
        assert!(err.contains("File") && err.contains("Dict"), "got: {err}");

        // Cap-carrying aggregates are the later GC-struct path, not part of the
        // bare-File cut. Reject them instead of silently lowering a File through
        // `$mkN`/the i64 slot.
        let err = check_str("type Handle:\n    f: File\nfn take(console: Console, h: Handle):\n    print(console, \"ok\")\n")
            .expect_err("a File record field needs the GC-struct aggregate path");
        assert!(err.contains("File") && err.contains("GC-struct"), "got: {err}");

        let err = check_str("fn tupled(console: Console, pair: (File, Int)):\n    print(console, \"x\")\n")
            .expect_err("a File tuple element needs the GC-struct aggregate path");
        assert!(err.contains("File") && err.contains("tuple"), "got: {err}");

        let err = check_str("fn main(console: Console, f: File[Read]):\n    let read_later = fn() -> String: read(f)\n    print(console, \"x\")\n")
            .expect_err("a closure capture of File needs the GC-struct aggregate path");
        assert!(
            err.contains("closure") && err.contains("f") && err.contains("File") && err.contains("GC-struct"),
            "got: {err}"
        );

        let err = check_str("fn main(console: Console, f: File):\n    let xs = [f]\n    print(console, \"x\")\n")
            .expect_err("an inferred List(File) literal needs the GC-struct aggregate path");
        assert!(err.contains("List") && err.contains("File"), "got: {err}");

        let err = check_str("fn main(console: Console, f: File):\n    let pair = (f, 1)\n    print(console, \"x\")\n")
            .expect_err("an inferred tuple carrying File needs the GC-struct aggregate path");
        assert!(err.contains("tuple") && err.contains("File"), "got: {err}");

        let err = check_str("fn main(console: Console, f: File):\n    let d = dict.insert(dict.new(), \"cfg\", f)\n    print(console, \"x\")\n")
            .expect_err("an inferred Dict(String, File) needs the GC-struct aggregate path");
        assert!(err.contains("Dict") && err.contains("File"), "got: {err}");

        let err = check_str("type Box(a):\n    Box(a)\nfn main(console: Console, f: File):\n    let b = Box(f)\n    print(console, \"x\")\n")
            .expect_err("a generic user aggregate instantiated with File needs the GC-struct aggregate path");
        assert!(err.contains("Box") && err.contains("File"), "got: {err}");

        // Sibling caps still on the i32 path may cross a slot until their own stage:
        // `std/secretstore.get -> Option(Secret)` must keep type-checking.
        check_str("fn get(console: Console, o: Option(Secret)):\n    print(console, \"x\")\n")
            .expect("Secret is still an i32 handle this stage — Option(Secret) allowed");
    }

    #[test]
    fn file_capability_rights_and_narrowing() {
        // RFC-0012: `File` is a host capability `main` may receive, the leaf of the
        // Dir/File hierarchy, right-typed like `Dir`.
        check_str("fn main(console: Console, config: File[Read], log: File[Write]):\n    print(console, \"ok\")\n")
            .expect("File[Read]/File[Write] are valid main capabilities");
        // A full `File` narrows to `File[Read]` implicitly at a call boundary.
        check_str("fn ro(console: Console, f: File[Read]):\n    print(console, \"r\")\nfn main(console: Console, f: File):\n    ro(console, f)\n")
            .expect("full File satisfies a File[Read] parameter");
        // Rights are enforced: `File[Read]` cannot stand in for `File[Write]`.
        let err = check_str("fn w(console: Console, f: File[Write]):\n    print(console, \"w\")\nfn main(console: Console, f: File[Read]):\n    w(console, f)\n")
            .expect_err("File[Read] must not satisfy File[Write]");
        assert!(err.contains("File[Write]"), "got: {err}");
        // `as` drops rights but can never add them.
        check_str("fn main(console: Console, f: File):\n    let ro = f as File[Read]\n    print(console, \"ok\")\n")
            .expect("`as` can drop File rights");
        let err = check_str("fn main(console: Console, f: File[Read]):\n    let w = f as File[Write]\n    print(console, \"no\")\n")
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
        check_str("fn main(console: Console, dir: Dir[Read], args: List(String)):\n    print(console, \"ok\")\n")
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
        check_str("fn main(console: Console) -> Nil:\n    print(console, \"x\")\n")
            .expect("explicit Nil is valid");
        check_str("fn main(console: Console):\n    print(console, \"x\")\n")
            .expect("no annotation is valid");
    }

    #[test]
    fn unknown_stdlib_function_suggests_import() {
        // Calling an unimported stdlib function points at the module to import.
        let err = check_str("fn main(console: Console):\n    print(console, __render(minimum([1], 0)))\n")
            .expect_err("minimum is unimported");
        assert!(err.contains("import cmp"), "{err}");
        // A genuine typo (no stdlib match) gets no misleading hint.
        let typo = check_str("fn main(console: Console):\n    frobnicate()\n")
            .expect_err("frobnicate is unknown");
        assert!(!typo.contains("did you forget"), "{typo}");
        assert!(!typo.contains("did you mean"), "{typo}");
        // A near-miss of a stdlib name suggests the correction.
        let near = check_str("fn main(console: Console):\n    let ys = mep([1], fn(x: Int): x)\n    print(console, \"ok\")\n")
            .expect_err("mep is a typo of map");
        assert!(near.contains("did you mean `map`"), "{near}");
    }

    #[test]
    fn module_qualified_call_without_import_suggests_import() {
        // `json.stringify(x)` with no `import json` parses as a method call on the
        // bare name `json`; the error should point at the missing import, not talk
        // about method resolution.
        let err = check_str("fn main(console: Console):\n    print(console, json.stringify(5))\n")
            .expect_err("json is unimported");
        assert!(err.contains("import json"), "{err}");
        assert!(!err.contains("method call"), "should not mention method resolution: {err}");
    }

    #[test]
    fn unbounded_generic_ordering_suggests_ord_bound() {
        // `<` on an unbounded type parameter resolves to a type var (renders `?`);
        // the error should suggest the `where T: Ord` bound, not a bare "found `?`".
        let err = check_str("fn smallest(xs: List(a)) -> a:\n    var m = list.at(xs, 0)\n    for x in xs:\n        if x < m:\n            m = x\n    m\nfn main(console: Console):\n    print(console, \"${smallest([3, 1, 2])}\")\n")
            .expect_err("unbounded generic comparison");
        assert!(err.contains("where T: Ord"), "{err}");
    }

    #[test]
    fn ordering_a_non_ord_type_points_at_deriving_ord() {
        // `<` on a concrete type without Ord must point at deriving `Ord`, and
        // must not leak the `less` desugar name (nor mis-suggest the list
        // function `last`, which the post-desugar unknown-function path used to).
        let err = check_str("type Foo:\n    Foo(Int)\nfn main(console: Console):\n    print(console, \"${Foo(1) < Foo(2)}\")\n")
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
    fn derive_ord_accepts_total_nested_fields() {
        check_str(
            "import cmp\nimport duration\n\ntype Key derive(PartialEq, Eq, PartialOrd, Ord):\n    name: String\n    age: Duration\n\ntype Reading derive(PartialEq, Eq, PartialOrd, Ord):\n    key: Key\n    count: Int\n",
        )
        .expect("derived Ord composes through total nested fields");
    }

    #[test]
    fn accepts_a_well_typed_program() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main(console: Console):
    print(console, __render(double(21)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_string_plus_int() {
        let src = r#"
fn f() -> String:
    ("a" + 1)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn capabilities_do_not_leak_across_kinds() {
        // Holding one capability never confers another. A function given only a
        // Console cannot reach the network or the filesystem: `connect` demands
        // a Net and `read` demands a Dir, and a Console can't stand in for
        // either. Authority is per-kind and (with no capability constructors)
        // unforgeable — the heart of witchy's confinement guarantee.
        let net = check_str(r#"
fn f(c: Console) -> Nil:
    connect(c, "host")
"#).unwrap_err();
        assert!(net.contains("Net"), "expected a Net mismatch, got: {net}");
        let dir = check_str(r#"
fn f(c: Console) -> String:
    read(c, "/etc/passwd")
"#)
            .unwrap_err();
        assert!(dir.contains("Dir"), "expected a Dir mismatch, got: {dir}");
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
    fn rejects_tuple_arity_mismatch() {
        assert!(check_str(r#"
fn main():
    let (a, b, c) = (1, 2)
"#).is_err());
    }

    #[test]
    fn accepts_tuple_destructure() {
        assert!(check_str(r#"
fn main():
    let (a, b) = (1, 2)
"#).is_ok());
    }

    #[test]
    fn generic_function_used_at_multiple_types() {
        let src = r#"
fn id(x: a) -> a:
    x

fn main(console: Console):
    print(console, id("hi"))
    print(console, __render(id(5)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_over_constrained_type_param() {
        // `a` can't be generic if the body forces it to Int.
        assert!(check_str("fn bad(x: a) -> a { x + 1 }").is_err());
    }

    #[test]
    fn uncalled_bounded_generic_bodies_are_checked() {
        let err = check_str(
            r#"
fn broken(x: a) -> Int where a: Ord:
    x + "oops"

fn main(console: Console):
    print(console, "ok")
"#,
        )
        .expect_err("a bad generic body is rejected at declaration time");
        assert!(err.contains("function `broken`"), "{err}");
    }

    #[test]
    fn duration_is_a_distinct_type() {
        // Durations combine with durations, scale by an Int, divide to an Int
        // ratio, and compare; mixing with a bare Int under +/- is rejected.
        assert!(check_str("fn f() -> Duration:\n    30s + 1m\n").is_ok());
        assert!(check_str("fn f() -> Duration:\n    2 * 1h\n").is_ok());
        assert!(check_str("fn f() -> Int:\n    1h / 1m\n").is_ok());
        assert!(check_str("fn f() -> Bool:\n    30s > 1m\n").is_ok());
        assert!(check_str("fn f(d: Duration) -> Duration:\n    d + 5s\n").is_ok());
        // A Duration is not an Int.
        assert!(check_str("fn f() -> Duration:\n    30s + 5\n").is_err());
        assert!(check_str("fn f() -> Int:\n    30s\n").is_err());
        assert!(check_str("fn f() -> Duration:\n    30s + true\n").is_err());
    }

    #[test]
    fn generic_adt_used_at_multiple_types() {
        // A generic `Box(a)` can be unwrapped at both Int and String.
        let src = r#"
type Box:
    Wrap(a)

fn unwrap_int(b: Box(Int)) -> Int:
    match b:
        Wrap(n) -> n

fn unwrap_str(b: Box(String)) -> String:
    match b:
        Wrap(s) -> s

fn main(console: Console):
    print(console, __render(unwrap_int(Wrap(5))))
    print(console, unwrap_str(Wrap("hi")))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn generic_function_with_binding_body_at_multiple_types() {
        // The same generic function — whose body *binds* its type parameter (here
        // by matching on it) — called at two different types in one program. This
        // regressed previously: checking the body bound the type-param var, and
        // instantiation then reused that binding instead of a fresh one per call.
        let src = r#"
type Box:
    Wrap(a)

fn unwrap(b: Box(a), default: a) -> a:
    match b:
        Wrap(v) -> v

fn main(console: Console):
    print(console, __render(unwrap(Wrap(5), 0)))
    print(console, unwrap(Wrap("hi"), "none"))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn early_return_type_checks_including_divergence() {
        // A guard `return` in an if-branch (no else) must not force the branch to
        // the function's return type — divergence is handled.
        let src = r#"
fn classify(n: Int) -> String:
    if (n < 0):
        return "neg"
    "nonneg"

fn only_return() -> Int:
    return 5
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_return_of_wrong_type() {
        assert!(check_str("fn f() -> Int { return \"x\" }").is_err());
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
    fn ordering_allows_comparable_primitives() {
        assert!(check_str(r#"
fn f(a: Int, b: Int) -> Bool:
    (a < b)
"#).is_ok());
        assert!(check_str(r#"
fn f(a: Float, b: Float) -> Bool:
    (a >= b)
"#).is_ok());
        assert!(check_str(r#"
fn f(a: String, b: String) -> Bool:
    (a < b)
"#).is_ok());
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
    fn equality_still_works_on_any_matching_type() {
        // `==` is unaffected — structural equality is defined for every value.
        assert!(check_str(r#"
fn f(a: (Int, Int), b: (Int, Int)) -> Bool:
    (a == b)
"#).is_ok());
    }

    #[test]
    fn dict_builtins_are_generic() {
        let src = r#"
fn tally(words: List(String)) -> Int:
    var d = dict.new()
    for w in words:
        d = dict.insert(d, w, (dict.get_or(d, w, 0) + 1))
    dict.length(d)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_dict_key_type_mismatch() {
        // The dict's key type is fixed by the first insert (String here), so
        // looking it up with an Int key must fail.
        let src = r#"
fn f() -> Int:
    let d = dict.insert(dict.new(), "a", 1)
    dict.get_or(d, 2, 0)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn string_builtins_type() {
        let src = r#"
fn first_field(row: String) -> String:
    list.at(string.split(row, ","), 0)

fn has(s: String, sub: String) -> Bool:
    string.contains(s, sub)

fn fix(s: String) -> String:
    string.replace(s, "a", "b")
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_split_on_non_string() {
        assert!(check_str("fn f() -> List(String) { string.split(5, \",\") }").is_err());
    }

    #[test]
    fn push_and_concat_are_generic() {
        let src = r#"
fn ints() -> List(Int):
    list.push([1, 2], 3)

fn strs() -> List(String):
    list.concat(["a"], ["b", "c"])
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_push_element_type_mismatch() {
        // Pushing a String onto a List(Int) must fail.
        assert!(check_str("fn f() -> List(Int) { list.push([1, 2], \"x\") }").is_err());
    }

    #[test]
    fn higher_order_and_lambda_type() {
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    print(console, __render(apply(fn(n: Int): (n + 1), 10)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn generic_higher_order_function() {
        // `apply` is generic over the value type `a`; the explicit fn-type
        // parameter keeps the type parameters free.
        let src = r#"
fn apply(f: fn(a) -> a, x: a) -> a:
    f(x)

fn main(console: Console):
    print(console, apply(fn(s: String): s, "hi"))
    print(console, __render(apply(fn(n: Int): n, 5)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_lambda_argument_type_mismatch() {
        // Passing a `fn(Int)->Int` where a `fn(String)->String` is required fails.
        let src = r#"
fn run(f: fn(String) -> String, s: String) -> String:
    f(s)

fn main(console: Console):
    print(console, run(fn(n: Int): (n + 1), "x"))
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn record_update_types() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn bump(p: Point) -> Point:
    Point(x: ((p).x + 1), ..p)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
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
        // Full named construction in any order is accepted (it lowers to the
        // positional constructor); positional construction still works too.
        let ok = "type Point:\n    x: Int\n    y: Int\nfn a() -> Point:\n    Point(y: 2, x: 1)\nfn b() -> Point:\n    Point(3, 4)\n";
        assert!(check_str(ok).is_ok(), "{:?}", check_str(ok));
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
    fn record_field_access_types() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn sum(p: Point) -> Int:
    ((p).x + (p).y)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
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
        // `value`'s type is the parameter `a`; reading `.value` on a `Box(Int)`
        // must yield Int (and concatenating it as a string must fail).
        let ok = r#"
type Box:
    value: a

fn unwrap(b: Box(Int)) -> Int:
    (b).value
"#;
        assert!(check_str(ok).is_ok(), "{:?}", check_str(ok));
        let bad = r#"
type Box:
    value: a

fn unwrap(b: Box(Int)) -> String:
    (b).value
"#;
        assert!(check_str(bad).is_err());
    }

    #[test]
    fn list_pattern_binds_element_and_tail() {
        // `head` is the element type, `tail` is a list of the same element type.
        let src = r#"
fn f(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [head, ..tail] -> (head + f(tail))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
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
    fn for_in_binds_element_type() {
        let src = r#"
fn main(console: Console):
    for n in [1, 2, 3]:
        print(console, __render(n))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_for_over_non_list() {
        let src = r#"
fn main(console: Console):
    for x in 5:
        print(console, "x")
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn try_operator_propagates_result() {
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn parse(s: String) -> Result(Int, String):
    Ok(string.to_int(s))

fn add(a: String, b: String) -> Result(Int, String):
    let x = (parse(a))?
    let y = (parse(b))?
    Ok((x + y))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
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
    fn allows_specific_then_general_constructor_arm() {
        // `Some(0)` is refutable, so a following `Some(n)` is still reachable —
        // the unreachable check must NOT flag this valid program.
        let src = r#"
type Opt:
    Some(a)
    None

fn f(o: Opt(Int)) -> Int:
    match o:
        Some(0) -> 1
        Some(n) -> n
        None -> 0
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn allows_guarded_arm_before_same_variant() {
        // A guarded arm may fail at runtime, so it does not cover its variant; a
        // later unguarded arm for that variant stays reachable.
        let src = r#"
type Opt:
    Some(a)
    None

fn f(o: Opt(Int)) -> Int:
    match o:
        Some(x) if (x > 0) -> 1
        Some(y) -> y
        None -> 0
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
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
    fn allows_complete_bool_match() {
        assert!(check_str(r#"
fn f(b: Bool) -> Int:
    match b:
        true -> 1
        false -> 0
"#).is_ok());
        assert!(check_str(r#"
fn f(b: Bool) -> Int:
    match b:
        true -> 1
        _ -> 0
"#).is_ok());
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

    /// Capability safety as a type error: `print` needs a `Console`, and a
    /// `String` is not one. Only a `Console`-typed parameter (ultimately from
    /// `main`) can satisfy it.
    #[test]
    fn rejects_print_without_console_capability() {
        let src = r#"
fn leak(s: String) -> Nil:
    print(s, s)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("Console"), "expected a Console error, got: {e}");
    }

    #[test]
    fn accepts_print_with_console_capability() {
        let src = r#"
fn shout(console: Console, s: String) -> Nil:
    print(console, s)
"#;
        assert!(check_str(src).is_ok());
    }

    #[test]
    fn checks_adt_constructors_and_exhaustive_match() {
        let src = r#"
type Event:
    Click(Int, Int)
    Closed

fn describe(e: Event) -> String:
    match e:
        Click(x, _) -> __render(x)
        Closed -> "closed"
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
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
    fn accepts_assignment_to_var() {
        let src = r#"
fn main():
    var x = 1
    x = 2
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
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
    fn accepts_var_argument_that_is_var() {
        let src = r#"
fn bump(var n: Int):
    n = (n + 1)

fn main():
    var x = 1
    bump(x)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
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

    #[test]
    fn accepts_reassignment_after_own_move() {
        let src = r#"
fn take(own s: String) -> String:
    s

fn main():
    var x = "hi"
    take(x)
    x = "again"
    take(x)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
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
        check_str("fn f(console: Console, net: Net[Connect]):\n    let s = connect(net, \"example.com:443\")\n    print(console, \"ok\")\n")
            .expect("Net[Connect] can connect");
        // ... but it cannot `listen`.
        let err = check_str("fn f(net: Net[Connect]):\n    listen(net, \"0.0.0.0:80\")\n")
            .expect_err("Net[Connect] must not listen");
        assert!(err.contains("`listen` needs `Listen`") && err.contains("Net[Connect]"), "got: {err}");
        // A listen-only handle accepts inbound.
        check_str("fn f(console: Console, net: Net[Listen]):\n    let l = listen(net, \"0.0.0.0:80\")\n    print(console, \"ok\")\n")
            .expect("Net[Listen] can listen");
        // ... but it cannot dial out (`connect` or its total sibling `try_connect`).
        let err = check_str("fn f(net: Net[Listen]):\n    connect(net, \"example.com:443\")\n")
            .expect_err("Net[Listen] must not connect");
        assert!(err.contains("`connect` needs `Connect`") && err.contains("Net[Listen]"), "got: {err}");
        let err = check_str("fn f(net: Net[Listen]):\n    try_connect(net, \"example.com:443\")\n")
            .expect_err("Net[Listen] must not try_connect");
        assert!(err.contains("`try_connect` needs `Connect`") && err.contains("Net[Listen]"), "got: {err}");
        // The transport axis attenuates independently: `connect`/`listen` are TCP-only,
        // so a UDP-only handle (full verbs, no TCP) cannot dial.
        let err = check_str("fn f(net: Net[Udp]):\n    connect(net, \"example.com:443\")\n")
            .expect_err("Net[Udp] must not connect (TCP-only op)");
        assert!(err.contains("only implemented over `Tcp`") && err.contains("Net[Udp]"), "got: {err}");
        // `as` drops `Net` verbs but can never add them (mirrors the File slice).
        check_str("fn main(console: Console, net: Net):\n    let dial = net as Net[Connect]\n    print(console, \"ok\")\n")
            .expect("`as` can drop Net to Connect-only");
        let err = check_str("fn main(console: Console, net: Net[Connect]):\n    let l = net as Net[Listen]\n    print(console, \"no\")\n")
            .expect_err("`as` cannot add the Listen verb");
        assert!(err.contains("can only drop rights"), "got: {err}");
        // ... and a narrowed handle cannot be re-widened back to the full `Net`.
        let err = check_str("fn main(console: Console, net: Net[Connect]):\n    let full = net as Net\n    print(console, \"no\")\n")
            .expect_err("`as` cannot re-widen Net[Connect] to full Net");
        assert!(err.contains("can only drop rights"), "got: {err}");
    }

    // (BUG-009 / RFC-0005 hardening #4) Attenuation proof for `Dir` — the Read/Write
    // lattice and one-way `as` narrowing, mirroring the File slice.
    #[test]
    fn dir_capability_rights_and_narrowing() {
        // RFC-0012: a `Dir`'s rights split `Read` from `Write` on independent axes.
        // A read-only handle reads (and lists/exists).
        check_str("fn f(console: Console, d: Dir[Read]):\n    let s = read(d, \"a.txt\")\n    print(console, s)\n")
            .expect("Dir[Read] can read");
        // ... but it cannot `write`, `append`, or `make_dir` (all `Write` verbs).
        let err = check_str("fn f(d: Dir[Read]):\n    write(d, \"a.txt\", \"x\")\n")
            .expect_err("Dir[Read] must not write");
        assert!(err.contains("`write` needs `Write`") && err.contains("Dir[Read]"), "got: {err}");
        let err = check_str("fn f(d: Dir[Read]):\n    make_dir(d, \"sub\")\n")
            .expect_err("Dir[Read] must not make_dir");
        assert!(err.contains("`make_dir` needs `Write`") && err.contains("Dir[Read]"), "got: {err}");
        // A write-only handle writes ...
        check_str("fn f(d: Dir[Write]):\n    write(d, \"a.txt\", \"x\")\n")
            .expect("Dir[Write] can write");
        // ... but cannot `read` or `list` (both `Read` verbs). This is the converse
        // the File slice never asserted.
        let err = check_str("fn f(d: Dir[Write]):\n    read(d, \"a.txt\")\n")
            .expect_err("Dir[Write] must not read");
        assert!(err.contains("`read` needs `Read`") && err.contains("Dir[Write]"), "got: {err}");
        let err = check_str("fn f(d: Dir[Write]):\n    list(d)\n")
            .expect_err("Dir[Write] must not list");
        assert!(err.contains("`list` needs `Read`") && err.contains("Dir[Write]"), "got: {err}");
        // `as` drops Dir rights but never adds them.
        check_str("fn main(console: Console, d: Dir):\n    let ro = d as Dir[Read]\n    print(console, \"ok\")\n")
            .expect("`as` can drop Dir to Read-only");
        let err = check_str("fn main(console: Console, d: Dir[Read]):\n    let w = d as Dir[Write]\n    print(console, \"no\")\n")
            .expect_err("`as` cannot add the Write right");
        assert!(err.contains("can only drop rights"), "got: {err}");
        // ... and cannot re-widen a narrowed handle back to the full `Dir`.
        let err = check_str("fn main(console: Console, d: Dir[Read]):\n    let full = d as Dir\n    print(console, \"no\")\n")
            .expect_err("`as` cannot re-widen Dir[Read] to full Dir");
        assert!(err.contains("can only drop rights"), "got: {err}");
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
        check_str("type NetPolicy:\n    NetPolicy(String)\nfn f(console: Console, net: Net[Connect]):\n    let scoped = only(net, NetPolicy(\"example.com:443\"))\n    let s = connect(scoped, \"example.com:443\")\n    print(console, \"ok\")\n")
            .expect("net.only preserves the Connect right");
        // ... and because the rights are preserved (still connect-only, not full),
        // the narrowed handle cannot be re-widened to a full `Net` by a cast.
        let err = check_str("type NetPolicy:\n    NetPolicy(String)\nfn f(console: Console, net: Net[Connect]):\n    let scoped = only(net, NetPolicy(\"example.com:443\"))\n    let wide = scoped as Net\n    print(console, \"no\")\n")
            .expect_err("a policy-narrowed Net[Connect] must not re-widen to full Net");
        assert!(err.contains("can only drop rights"), "got: {err}");
        // `dir.only(policy)` likewise keeps `Read` ...
        check_str("type DirPolicy:\n    DirPolicy(String)\nfn f(console: Console, d: Dir[Read]):\n    let scoped = only(d, DirPolicy(\"ext:txt\"))\n    let s = read(scoped, \"a.txt\")\n    print(console, s)\n")
            .expect("dir.only preserves the Read right");
        // ... and the narrowed `Dir[Read]` cannot be re-widened to a full `Dir`.
        let err = check_str("type DirPolicy:\n    DirPolicy(String)\nfn f(console: Console, d: Dir[Read]):\n    let scoped = only(d, DirPolicy(\"ext:txt\"))\n    let wide = scoped as Dir\n    print(console, \"no\")\n")
            .expect_err("a policy-narrowed Dir[Read] must not re-widen to full Dir");
        assert!(err.contains("can only drop rights"), "got: {err}");
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
        // Distinct Duration arms remain reachable.
        check_str(
            "fn f(d: Duration) -> Int:\n    match d:\n        1s -> 1\n        2s -> 2\n        _ -> 0\n",
        )
        .expect("distinct Duration arms are reachable");
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
            "dict.insert(d, key, fallback)",
            "dict.contains_key(d, key)",
            "dict.remove(d, key)",
        ] {
            let src = format!("fn f(d: Dict(k, v), key: k, fallback: v) -> v:\n    {op}\n    fallback\n");
            let err = check_str(&src).unwrap_err();
            assert!(err.contains("generic `Dict` key must be `Eq`"), "{op}: {err}");
        }
        // With the bound it type-checks (checked through monomorphization).
        check_str("fn f(d: Dict(k, v), key: k, fallback: v) -> v where k: Eq:\n    dict.get_or(d, key, fallback)\n")
            .expect("a `where k: Eq` generic dict helper is accepted");
        // A concrete key needs no bound.
        check_str("fn f(d: Dict(String, Int), key: String) -> Int:\n    dict.get_or(d, key, 0)\n")
            .expect("a concrete String key needs no bound");
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
    fn rfc0064_row3_var_shapes_are_rejected() {
        // (RFC-0064 Check 1) Row 3 of RFC-0043's table: a `var` parameter that is
        // neither a procedure channel (`-> Nil`) nor a mutator receiver (first
        // parameter, self-typed return) carries the abolished *combined*
        // write-back+return semantics and is a compile error at the shared gate —
        // rejected before either backend lowers, so parity holds by construction.

        // (a) `var` in a NON-first position with a self-typed return.
        let non_first =
            check_str("fn f(x: Int, var xs: List(Int)) -> List(Int):\n    xs\n").unwrap_err();
        assert!(
            non_first.contains("write-back channel") && non_first.contains("mutator receiver"),
            "{non_first}"
        );

        // (b) `var` FIRST with an UNRELATED (non-`Nil`, non-receiver) return type —
        // the shape the interpreter used to run with combined semantics while the
        // WASM backend rejected it; now rejected identically at type-check.
        let unrelated =
            check_str("fn f(var xs: List(Int), x: Int) -> Int:\n    x\n").unwrap_err();
        assert!(unrelated.contains("mutator receiver"), "{unrelated}");

        // Both legal shapes still compile: a procedure channel (`-> Nil`) and a
        // mutator receiver (first parameter, self-typed return).
        check_str("fn bump(var n: Int):\n    n = n + 1\n")
            .expect("a `var` procedure channel is valid");
        check_str("fn keep(var xs: List(Int), x: Int) -> List(Int):\n    xs\n")
            .expect("a mutator receiver is valid");
    }

    #[test]
    fn rfc0064_row3_var_shapes_are_rejected_in_impl_methods() {
        // (BUG-436 / RFC-0064 Check 1) Impl methods lower to ordinary functions.
        // Re-run the same row-3 validation after trait/impl lowering so method
        // surfaces cannot keep the abolished combined write-back+return channel.
        let static_method = check_str(
            "type Box:\n    Box(List(Int))\nimpl Box:\n    fn row3_static(var xs: List(Int), n: Int) -> Int:\n        xs.push(n)\n        n\n",
        )
        .unwrap_err();
        assert!(static_method.contains("write-back channel"), "{static_method}");

        let instance_method = check_str(
            "type Box:\n    Box(List(Int))\nimpl Box:\n    fn row3_method(self, var xs: List(Int)) -> List(Int):\n        xs.push(1)\n",
        )
        .unwrap_err();
        assert!(instance_method.contains("write-back channel"), "{instance_method}");
    }

    #[test]
    fn rfc0064_ambiguous_elided_var_receiver_must_annotate() {
        // (RFC-0064 Check 2) A `var` FIRST parameter with an ELIDED return whose
        // inferred tail type equals the receiver's type is ambiguous between a
        // mutator (`-> T`, statement form writes back) and a procedure (`-> Nil`).
        // Write-back is DECLARED, not inferred (RFC-0043's thesis), and this is
        // the one property whose inferred value changes call-site semantics, so
        // the author must annotate the intent.
        let err = check_str("fn bump(var xs: List(Int), by: Int):\n    xs\n").unwrap_err();
        assert!(err.contains("annotate the intent"), "{err}");

        // An EXPLICIT self-typed return declares a mutator with no extra ceremony.
        check_str("fn bump(var xs: List(Int), by: Int) -> List(Int):\n    xs\n")
            .expect("an explicit self-typed return is an unambiguous mutator");
        // `-> Nil` (with a `return`) is the other unambiguous choice — a procedure.
        check_str("fn bump(var xs: List(Int), by: Int) -> Nil:\n    xs = xs\n    return\n")
            .expect("an explicit `-> Nil` return is an unambiguous procedure");
        // A `var` first param whose inferred tail is a DIFFERENT type is untouched
        // by this rule (its non-`Nil` shape is instead caught by Check 1).
        let other =
            check_str("fn f(var xs: List(Int), x: Int):\n    x\n").unwrap_err();
        assert!(other.contains("mutator receiver"), "{other}");
    }

    #[test]
    fn rfc0064_discarded_free_call_is_an_error() {
        // (RFC-0064 Check 3) A non-`Nil` FREE call in statement position whose
        // result is discarded is the same discard error the method form already
        // raised — a free call does NOT write back (its first argument is not the
        // syntactic target), so the value is silently thrown away without this.
        //
        // The discard classifier lives in the trait/method rewrite pass, which the
        // single-module `check_str` harness only runs when the module "needs
        // lowering". The leading `impl` forces that here; a real CLI program always
        // links std (which needs lowering), so BOTH backends run this check on
        // every program (verified: even a std-free `f()` discard errors on both).
        const LOWER: &str = "type Tag:\n    v: Int\nimpl Tag:\n    fn id(self) -> Int:\n        self.v\n";

        // A user mutator called in free form as a statement: its return is lost.
        let mutator_free = check_str(&format!(
            "{LOWER}fn add(var xs: List(Int), x: Int) -> List(Int):\n    xs\nfn main(console: Console):\n    var xs: List(Int) = []\n    add(xs, 2)\n    print(console, \"hi\")\n"
        ))
        .unwrap_err();
        assert!(mutator_free.contains("is discarded"), "{mutator_free}");

        // ANY non-`Nil` free call (not only mutators) whose result is discarded.
        let nonmut_free = check_str(&format!(
            "{LOWER}fn double(n: Int) -> Int:\n    n * 2\nfn main(console: Console):\n    double(3)\n    print(console, \"hi\")\n"
        ))
        .unwrap_err();
        assert!(nonmut_free.contains("is discarded"), "{nonmut_free}");

        // `let _ = …` is the explicit-discard escape hatch and still compiles.
        check_str(&format!(
            "{LOWER}fn double(n: Int) -> Int:\n    n * 2\nfn main(console: Console):\n    let _ = double(3)\n    print(console, \"hi\")\n"
        ))
        .expect("`let _ =` is the explicit discard escape");

        // A `Nil`-returning free call in statement position is unaffected — there
        // is no result to discard.
        check_str(&format!(
            "{LOWER}fn noop(n: Int):\n    let _ = n\nfn main(console: Console):\n    noop(3)\n    print(console, \"hi\")\n"
        ))
        .expect("a `Nil`-returning free call statement is fine");
    }
