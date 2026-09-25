use witchy_syntax::parser::*;
use witchy_syntax::ast::*;

#[test]
fn dereference_assignment_is_not_a_canonical_source_form() {
    for source in [
        "mode opt\n\nfn replace(reference: &Int):\n    *reference = 42\n",
        "mode opt\n\nfn replace():\n    var value = 0\n    let reference = &value\n    *reference = 42\n",
    ] {
        let error = parse_module(source).expect_err("immutable views cannot be written through");
        assert!(error.message.contains("write-through dereference"), "{error}");
    }
}

#[test]
fn quote_expr_holes_are_rejected_outside_quote_expr() {
    let err = parse_module("fn f(x: Int):\n    ${x}\n")
        .expect_err("quote holes outside quote expr are invalid");
    assert!(
        err.message.contains("only valid inside `quote expr:`"),
        "{err}"
    );
}

#[test]
fn inout_sink_keyword_aliases_are_removed() {
    // `inout`/`sink` were Hylo-style alias spellings for `var`/`own`; they
    // were removed, so they now lex as ordinary identifiers and a parameter
    // written with them no longer parses as a convention.
    assert!(parse_module("fn f(inout a: Int):\n    a\n").is_err());
    assert!(parse_module("fn f(sink a: Int):\n    a\n").is_err());
}

#[test]
fn rustism_diagnostics_point_at_the_witchy_form() {
    // `let mut x` (Rust) — suggest `var`, instead of "expected `=`, found `x`".
    let let_mut =
        parse_module("fn main(console: Console):\n    let mut x = 0\n    x = x + 1\n").unwrap_err();
    assert!(let_mut.to_string().contains("var"), "{let_mut}");
    // `List<Int>` (Rust/TS) — suggest parentheses, not "expected `=`, found `<`".
    let angle = parse_module(
        "fn main(console: Console):\n    let xs: List<Int> = []\n    console.print(\"x\")\n",
    )
    .unwrap_err();
    assert!(angle.to_string().contains("List(…)"), "{angle}");
    // `var mut x` is NOT mistaken for the Rust form (mut is a real binding name here).
    parse_module("fn main(console: Console):\n    var mut = 0\n    mut = mut + 1\n    console.print(\"${mut}\")\n")
            .expect("`mut` is a valid identifier on its own");
}

// SEC-041: deeply-nested untrusted source must return a `ParseError`, never
// overflow the native stack (an uncatchable SIGABRT when the parser runs in a
// wasmtime host fn — `compiler.footprint`/`doc`/`diff` on the supply-chain gate).
//
// NOTE: the parser is exercised here on a large-stack thread. In a *debug* build
// the per-recursion frame is ~10x a release build's, so a debug test thread's
// 2 MiB stack overflows near paren-depth ~48 — below the guard at
// MAX_PARSE_DEPTH. That is a *test-harness* artifact; the shipped binary is
// release, where the guard fires with wide margin on every production stack
// (release overflows a 2 MiB worker stack only near depth ~470). Giving the test
// ample stack lets it observe the *guard* firing, which is the invariant.
fn parse_deep(src: String) -> Result<Module, ParseError> {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || parse_module(&src))
        .unwrap()
        .join()
        .expect("the parser must not overflow/abort — it must return a Result")
}

// The reported bug shape: many nested parens with no closing side. It must still
// error cleanly (via the depth guard) rather than abort the host process.
#[test]
fn unbounded_nested_parens_error_cleanly() {
    let src = format!("fn main():\n    {}", "(".repeat(10_000));
    let err = parse_deep(src).expect_err("must be a clean parse error, not an abort");
    assert!(err.to_string().contains("nests too deeply"), "{err}");
}

#[test]
fn yield_inside_a_lambda_in_a_gen_fn_is_rejected() {
    // BUG-183: a `yield` in a nested lambda used to `check` clean but fail only
    // in codegen (the generator lowering does not descend into closures). It is
    // now a parse error, so `check` is a reliable gate for both backends.
    let err = parse_module(
            "import iter\n\ngen fn bad() -> Iter(Int):\n    let f = fn():\n        yield 1\n    yield 2\n",
        )
        .unwrap_err();
    assert!(err.to_string().contains("`yield`"), "{err}");
    // A `gen fn` method with a nested-lambda yield is rejected the same way.
    assert!(parse_module(
            "type Box:\n    value: Int\n\nimpl Box:\n    gen fn bad(self) -> Iter(Int):\n        let f = fn():\n            yield self.value\n        yield 2\n"
        )
        .is_err());
    // Control: a block-nested `yield` in the same generator still parses.
    parse_module("gen fn ok() -> Iter(Int):\n    if true:\n        yield 1\n    yield 2\n")
        .expect("block-nested yield parses");
}

#[test]
fn retired_reference_spellings_reject_at_every_source_entry() {
    let cases = [
        ("fn f(x: View(String, 'a)):\n    x\n", "`View(T, 'a)` is retired"),
        ("fn f(x: let('a) String):\n    x\n", "`let('a) T` is retired"),
        ("fn f(let('a) x: String):\n    x\n", "`let('a)` is retired"),
        ("fn f(var('a) x: String):\n    x\n", "`var('a)` is retired"),
        ("fn f(x: local unique String):\n    x\n", "`local unique` is retired"),
        ("type Holder('a):\n    value: List(View(String, 'a))\n", "lifetime parameters are retired"),
        ("fn f(x: fn(let('a) String) -> String):\n    x\n", "lifetime-bearing parameter conventions are retired"),
        ("fn f(x: fn(var('a) String) -> String):\n    x\n", "lifetime-bearing parameter conventions are retired"),
        ("fn f():\n    quote type:\n        View(String, 'a)\n", "`View(T, 'a)` is retired"),
        ("fn f():\n    quote type:\n        local unique String\n", "`local unique` is retired"),
        ("comptime:\n    quote item:\n        fn f(var('a) x: String):\n            x\n", "`var('a)` is retired"),
    ];
    for (body, expected) in cases {
        for prefix in ["", "mode opt\n\n"] {
            let source = format!("{prefix}{body}");
            let error = parse_module(&source).expect_err("retired syntax is not ordinary source");
            assert!(error.message.contains(expected), "{source}: {error}");
            let spanned = parse_module_with_expression_spans(&source)
                .expect_err("span collection cannot enable retired syntax");
            assert_eq!(spanned, error);
        }
    }
}

#[test]
fn reference_type_rejects_retired_explicit_lifetimes() {
    parse_module("mode opt\n\nfn read(value: &String) -> &String:\n    value\n")
        .expect("rooted views do not require a source lifetime");
    for source in [
        "mode opt\n\nfn bad(value: &'a String) -> &String:\n    value\n",
        "mode opt\n\nfn bad(value: &mut String) -> &String:\n    value\n",
    ] {
        let err = parse_module(source).expect_err("retired reference syntax is rejected");
        assert!(err.message.contains("retired"), "{err:?}");
    }
}

#[test]
fn bare_quote_is_still_rejected() {
    // A stray `'` with no name is not a lifetime; it must still be the old
    // "not a witchy operator" style lex/parse failure, never a silent token.
    assert!(parse_module("fn f() -> Int:\n    '\n").is_err());
}

// ---- RFC-0081: existential trait types -------------------------------

#[test]
fn dyn_with_empty_argument_list_is_a_precise_parse_error() {
    let err = parse_module("fn f(x: dyn Render()) -> Int:\n    1\n")
        .expect_err("empty trait-argument list rejected");
    assert!(
        err.message
            .contains("empty trait-argument list is malformed"),
        "got: {}",
        err.message
    );
}

#[test]
fn runtime_type_rejects_value_and_multiple_type_arguments() {
    for source in [
        "import dynamic\nfn f():\n    dynamic.runtime_type()\n",
        "import dynamic\nfn f():\n    dynamic.runtime_type(Int, String)\n",
        "import dynamic\nfn f():\n    dynamic.runtime_type(1)\n",
    ] {
        parse_module(source).expect_err("runtime_type accepts exactly one type");
    }
}

#[test]
fn named_once_and_unsupported_function_kind_qualifiers_are_rejected() {
    for source in [
        "once fn run():\n    ()\n",
        "pure once fn run():\n    ()\n",
        "pure async fn run():\n    ()\n",
        "pure gen fn run():\n    ()\n",
        "pure comptime fn run():\n    ()\n",
        "fn run(callback: once pure fn() -> ()):\n    ()\n",
    ] {
        parse_module(source).expect_err(source);
    }
}

#[test]
fn unit_uses_only_the_parenthesized_source_spelling() {
    let module = parse_module("fn unit() -> ():\n    ()\n").expect("canonical unit syntax");
    let Item::Function(function) = &module.items[0] else { panic!("unit function") };
    assert_eq!(function.ret, Some(Type::Tuple(Vec::new())));
    assert_eq!(function.body.stmts, [Stmt::Expr(Expr::Tuple(Vec::new()))]);

    for source in ["fn unit() -> Nil:\n    ()\n", "fn unit() -> ():\n    Nil\n"] {
        let error = parse_module(source).expect_err("the retired Nil spelling must reject");
        assert!(error.to_string().contains("`Nil` is retired; write `()`"), "{error}");
    }
}

#[test]
fn and_or_not_diagnostics_suggest_witchy_operators() {
    let and_err = parse_module("fn f(a: Bool, b: Bool):\n    if a and b:\n        ()\n").unwrap_err();
    assert!(and_err.to_string().contains("Witchy uses `&&` for logical AND"), "{and_err}");

    let or_err = parse_module("fn f(a: Bool, b: Bool):\n    if a or b:\n        ()\n").unwrap_err();
    assert!(or_err.to_string().contains("Witchy uses `||` for logical OR"), "{or_err}");

    let not_err = parse_module("fn f(a: Bool):\n    if not a:\n        ()\n").unwrap_err();
    assert!(not_err.to_string().contains("Witchy uses `!` for logical NOT"), "{not_err}");
}

#[test]
fn string_literal_record_field_names_are_rejected() {
    let source = "type Config = .{ \"host\": String }\n";
    let err = parse_module(source).unwrap_err();
    assert!(err.to_string().contains("expected an identifier"), "{err}");

    let literal_source = "fn f():\n    .{ \"host\": 1 }\n";
    let lit_err = parse_module(literal_source).unwrap_err();
    assert!(lit_err.to_string().contains("expected an identifier"), "{lit_err}");
}

