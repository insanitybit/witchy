    use super::*;

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
    fn capability_firewall_drops_and_retains() {
        // `without c:` drops `c` inside the block — using it is an error, but the
        // sibling capability is untouched.
        let drop_clock =
            "fn main(console: Console, clock: Clock):\n    without clock:\n        print(console, \"ok\")\n";
        check_str(drop_clock).expect("console still usable when only clock is dropped");
        let use_dropped =
            "fn main(console: Console, clock: Clock):\n    without clock:\n        let t = now(clock)\n        print(console, __render(t))\n";
        let err = check_str(use_dropped).expect_err("using a dropped capability must fail");
        assert!(err.contains("walled off"), "got: {err}");

        // `retain c:` keeps only `c`; every other capability is hidden.
        let retain_console =
            "fn main(console: Console, clock: Clock):\n    retain console:\n        print(console, \"ok\")\n";
        check_str(retain_console).expect("the retained capability is usable");
        let retain_drops_rest =
            "fn main(console: Console, clock: Clock):\n    retain console:\n        let t = now(clock)\n        print(console, __render(t))\n";
        let err = check_str(retain_drops_rest).expect_err("a non-retained capability must be hidden");
        assert!(err.contains("walled off"), "got: {err}");

        // `retain:` with no names is a full sandbox — even `console` is gone.
        let sandbox = "fn main(console: Console):\n    retain:\n        print(console, \"nope\")\n";
        let err = check_str(sandbox).expect_err("an empty retain drops every capability");
        assert!(err.contains("walled off"), "got: {err}");
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
    fn capability_firewall_validates_its_names() {
        // Naming a non-capability binding is rejected — it almost certainly means
        // the author misremembered what authority the block holds.
        let not_cap = "fn main(console: Console):\n    let x = 5\n    without x:\n        print(console, \"hi\")\n";
        let err = check_str(not_cap).expect_err("a non-capability can't be firewalled");
        assert!(err.contains("not a capability"), "got: {err}");
        // Naming something not in scope at all is rejected too.
        let absent = "fn main(console: Console):\n    without clock:\n        print(console, \"hi\")\n";
        let err = check_str(absent).expect_err("an out-of-scope name can't be firewalled");
        assert!(err.contains("no capability `clock` is in scope"), "got: {err}");
    }

    #[test]
    fn capability_firewall_is_sealed_against_outer_caps() {
        // The point of the firewall: a `retain` block sees exactly the named
        // capabilities even though the outer scope holds more. Adding `clock` to
        // `main` must NOT make it reachable inside `retain console`.
        let src =
            "fn main(console: Console, clock: Clock):\n    retain console:\n        now(clock)\n        print(console, \"x\")\n";
        let err = check_str(src).expect_err("retain must seal the block from outer clock");
        assert!(err.contains("walled off"), "got: {err}");
        // A nested re-binding legitimately shadows the firewall: re-using the name
        // for a fresh value is fine (you still can't reach the dropped capability).
        let shadow =
            "fn use_int(n: Int):\n    fail(__render(n))\nfn main(console: Console):\n    without console:\n        let console = 42\n        use_int(console)\n";
        check_str(shadow).expect("re-binding a dropped name to a fresh value is allowed");
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
    dict.size(d)
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
