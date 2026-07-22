use super::*;
use crate::{interpreter, parser, typeck};

    /// (BUG-214) `Nil` is the language's unit value, so every backend must accept
    /// it anywhere a `Nil` expression is expected instead of treating it as an
    /// unknown nullary constructor that the compiled backend cannot lower.
    #[test]
    fn bare_nil_expression_compiles_on_both_backends() {
        let cases = [
            (
                "tail",
                "fn unit() -> Nil:\n    Nil\n\nfn main(console: Console):\n    unit()\n    console.print(\"tail\")\n",
                ["tail"],
            ),
            (
                "statement",
                "fn main(console: Console):\n    Nil\n    console.print(\"statement\")\n",
                ["statement"],
            ),
            (
                "match arm",
                "fn unit(n: Int) -> Nil:\n    match n:\n        0 -> Nil\n        _ -> Nil\n\nfn main(console: Console):\n    unit(0)\n    unit(1)\n    console.print(\"match\")\n",
                ["match"],
            ),
            (
                "let binding",
                "fn unit() -> Nil:\n    Nil\n\nfn main(console: Console):\n    let x = unit()\n    console.print(\"let\")\n",
                ["let"],
            ),
        ];

        for (label, src, expected) in cases {
            assert_eq!(link_run(src), expected, "interp: {label}");
            assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: {label}");
        }

        assert!(
            typeck::check_str("fn main(console: Console):\n    Nil(1)\n").is_err(),
            "Nil has no constructor fields"
        );
    }

    /// `comptime:` — compile-time item generation: zero capabilities
    /// reachable (deterministic by construction), `emit(line)` as the
    /// channel, output parsed as ADDITIVE items before checking — so the
    /// generated functions exist on both backends and in the footprint.
    #[test]
    fn comptime_blocks_generate_items_additively() {
        let src = "comptime:\n    var i = 0\n    while i < 3:\n        emit(\"pub fn lucky_${i}() -> Int:\")\n        emit(\"    ${i * 7}\")\n        emit(\"\")\n        i = i + 1\n\nfn main(console: Console):\n    console.print(\"${lucky_0()} ${lucky_1()} ${lucky_2()}\")\n";
        let want = vec!["0 7 14".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // Emitted garbage is a loud error carrying the emitted source.
        let bad = "comptime:\n    emit(\"fn (((\")\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let module = parser::parse_module(bad).expect("parse");
        let err = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect_err("bad emission must be loud");
        assert!(err.to_string().contains("does not parse"), "got: {err}");
    }

    /// `return X if cond` — a postfix-guard return, sugar for `if cond: return X`.
    /// It round-trips through fmt (the parser tags the desugared block with the
    /// synthetic-line marker so the formatter re-collapses exactly this shape),
    /// while an explicitly written multi-line `if cond: return X` is left untouched.
    /// Runs identically on both backends.
    #[test]
    fn postfix_guard_return() {
        let src = "fn classify(n: Int) -> String:\n    return \"neg\" if n < 0\n    return \"zero\" if n == 0\n    \"pos\"\n\nfn main(console: Console):\n    console.print(classify(-5))\n    console.print(classify(0))\n    console.print(classify(7))\n";
        let want: Vec<String> = ["neg", "zero", "pos"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // The postfix form is preserved by fmt (idempotent).
        assert_eq!(
            crate::format::reformat(src).as_deref(),
            Some(src),
            "postfix return round-trips through fmt"
        );
        // An explicitly written multi-line if-return is NOT collapsed.
        let explicit = "fn f(n: Int) -> Int:\n    if n < 0:\n        return 0\n    n\n";
        assert_eq!(
            crate::format::reformat(explicit).as_deref(),
            Some(explicit),
            "an explicit multi-line if-return is preserved"
        );
    }

    /// fmt breaks a long fluent method chain onto one call per line (witchy's layout
    /// joins the leading-`.` continuation lines back into the chain on re-parse), so
    /// a builder like a router reads vertically. Short chains stay inline, and the
    /// wrap is idempotent (the decision is the chain's own inline width, not its
    /// indented column, so `chain_wrap` and `expr_max_line` agree).
    #[test]
    fn fmt_wraps_long_method_chains() {
        let long = "fn main(net: Net):\n    let app = router().get(\"/aaaaaaaaaaaaaaaa\", h()).get(\"/bbbbbbbbbbbbbbbb\", h()).get(\"/cccccccccccccccc\", h()).get(\"/dddddddddddddddd\", h())\n    serve(net, app)\n";
        let wrapped = crate::format::reformat(long).expect("a long chain formats");
        assert!(
            wrapped.contains("let app = router()\n        .get("),
            "a long chain breaks one call per line:\n{wrapped}"
        );
        assert_eq!(
            crate::format::reformat(&wrapped).as_deref(),
            Some(wrapped.as_str()),
            "the wrap is idempotent"
        );
        // A short chain stays on one line.
        let short = "fn main(net: Net):\n    let x = a().b()\n";
        assert_eq!(
            crate::format::reformat(short).as_deref(),
            Some(short),
            "a short chain stays inline"
        );
    }

    /// (BUG-295, spec §6) An irrefutable `if let`/`while let` (Var or Wildcard) is
    /// accepted, consistently with the already-accepted irrefutable TUPLE form. A
    /// genuine duplicate arm still errors (dead-code detection preserved).
    #[test]
    fn irrefutable_if_let_while_let_accepted_consistently() {
        let iflet = "fn main(console: Console):\n    if let x = 3:\n        console.print(\"${x}\")\n";
        assert_eq!(link_run(iflet), ["3"], "interp if-let");
        assert_eq!(wasm_run(iflet), ["3"], "wasm if-let");
        let whilelet = "fn main(console: Console):\n    var n = 0\n    while let x = 3:\n        n = n + x\n        if n >= 6:\n            break\n    console.print(\"${n}\")\n";
        assert_eq!(link_run(whilelet), ["6"], "interp while-let");
        assert_eq!(wasm_run(whilelet), ["6"], "wasm while-let");
        typeck::check_str("fn main(console: Console):\n    if let _ = 3:\n        console.print(\"m\")\n").expect("if let _ ok");
        typeck::check_str("fn main(console: Console):\n    let p = (1, 2)\n    if let (a, b) = p:\n        console.print(\"${a + b}\")\n").expect("tuple if-let ok");
        assert!(typeck::check_str("fn f(d: Duration) -> Int:\n    match d:\n        1s -> 1\n        1s -> 2\n        _ -> 0\n\nfn main(console: Console):\n    console.print(\"${f(1s)}\")\n").is_err(), "duplicate arm must still error");
    }

    #[test]
    fn inline_if_else_expression_form() {
        // Brace-free inline `if c: a else: b` (chained), here inside a brace-free
        // lambda inside call parens. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    let xs = [3, (0 - 2), 0, 5]
    let signs = list.map(xs, fn(n: Int): if (n > 0): 1 else: if (n < 0): (0 - 1) else: 0)
    console.print("${list.fold(signs, 0, fn(a: Int, b: Int): (a + b))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "inline if-else diverged");
        assert_eq!(compiled, vec!["1"]);
    }

    #[test]
    fn print_trailing_newline_agrees_on_both_backends() {
        // Regression: a printed string ending in `\n` (the line terminator) must
        // produce identical output on both backends. The WASM host strips a
        // trailing newline; the interpreter now does too.
        let src = "fn main(console: Console):\n    console.print(\"ab\" + \"\\n\")\n    console.print(\"cd\")\n";
        let sources = [("main", src)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "trailing-newline print diverged");
        assert_eq!(compiled, vec!["ab", "cd"]);
    }

    // Indentation-based (off-side rule) syntax: blocks are delimited by `:` +
    // indentation rather than braces. A layout pass turns it into the brace form
    // the rest of the pipeline expects, so both backends agree — here over a
    // type, match, for-loop, let/var, and calls.
    #[test]
    fn indentation_syntax_backends_agree() {
        let src = r#"
type Shape:
    Circle(Int)
    Rect(Int, Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> 3 * r * r
        Rect(w, h) -> w * h

fn main(console: Console):
    let xs = [area(Circle(2)), area(Rect(3, 4))]
    var total = 0
    for x in xs:
        total = total + x
    console.print("${total}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "indentation backends diverged");
        assert_eq!(run_on_wasm(src), vec!["24"]);
    }

    // Regression: a `(...)` expression on the line after a block must be its own
    // statement, not an application of the block's value — the virtual closing
    // brace sits on the previous line so `} (a, n)` stays two things. (This is
    // what `list.partition`'s trailing `(yes, no)` exercises.)
    #[test]
    fn indentation_block_then_paren_expr_backends_agree() {
        let src = r#"
fn pair(n: Int) -> (Int, Int):
    var a = 0
    for i in [1, 2, 3]:
        a = a + i
    (a, n)

fn main(console: Console):
    let (x, y) = pair(10)
    console.print("${x}")
    console.print("${y}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "block-then-paren diverged");
        assert_eq!(run_on_wasm(src), vec!["6", "10"]);
    }

    #[test]
    fn bitwise_not_backends_agree() {
        // ~x = -x-1 (width-independent), so it agrees across backends.
        let src = r#"
fn main(console: Console):
    console.print("${(~0)}")
    console.print("${(~5)}")
    console.print("${(~(0 - 1))}")
    console.print("${(255 & (~15))}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["-1", "-6", "0", "240"]);
    }

    #[test]
    fn bitwise_operators_backends_agree() {
        // & | ^ << >> on Int, with precedence (& tighter than |, both tighter
        // than ==), and or-patterns still parsing (| in pattern position). Both
        // backends agree.
        let src = r#"
fn classify(n: Int) -> String:
    match n:
        1 -> "pow2"
        2 -> "pow2"
        4 -> "pow2"
        _ -> "other"

fn main(console: Console):
    console.print("${(12 & 10)}")
    console.print("${(12 | 10)}")
    console.print("${(12 ^ 10)}")
    console.print("${(1 << 4)}")
    console.print("${(256 >> 2)}")
    console.print("${((5 & 3) | 8)}")
    console.print("${((5 & 4) == 4)}")
    console.print(classify(2))
    console.print(classify(3))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["8", "14", "6", "16", "64", "9", "true", "pow2", "other"]
        );
    }

    #[test]
    fn bug307_real_body_error_surfaces_over_collect_inference_fallback() {
        // (BUG-307) A genuine body type error must surface even when the module has a
        // result-position bounded call (`iter.collect`) whose annotate fell back —
        // the false "cannot infer the result type" diagnostic must not mask it.
        let src = "import iter\n\
                   import list\n\
                   fn broken() -> Int:\n\
                   \x20   \"oops\"\n\
                   fn main(console: Console):\n\
                   \x20   let a: List(Int) = iter.collect(iter.range(0, 3))\n\
                   \x20   console.print(\"${list.length(a)}\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("broken body must fail").to_string();
        assert!(err.contains("broken") && err.contains("expected `Int`"), "{err}");
    }

    #[test]
    fn bug181_tagged_literals_in_impls_and_consts_expand() {
        // (BUG-181) a `tag"…"` in an impl method OR a top-level `let` constant must
        // be expanded before type-checking — it must not survive as an
        // `Expr::TaggedLit` (which the type checker `unreachable!`s on). The `lit`
        // tag here emits the source `"ok"`, so both sites render `ok`.
        let src = "import meta\n\
                   \n\
                   comptime fn lit(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n\
                   \x20   meta.expr_raw(\"\\\"ok\\\"\")\n\
                   type Box:\n\
                   \x20   value: Int\n\
                   impl Box:\n\
                   \x20   pub fn label(self) -> String:\n\
                   \x20       lit\"ignored\"\n\
                   let LABEL = lit\"ignored\"\n\
                   fn main(console: Console):\n\
                   \x20   console.print(Box(1).label())\n\
                   \x20   console.print(LABEL)\n";
        let expected = ["ok", "ok"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// PARITY (BUG-246): a TAB in leading indentation is rejected at the SHARED
    /// parse stage, so neither backend can silently mis-nest a tab-indented body.
    /// The old bug let a tab count as one column, so the body of `if false:` below
    /// lexed shallower than it looked and *executed*. Rejection before codegen means
    /// `witchy run` and the compiled path fail identically (parity by construction).
    #[test]
    fn tab_indentation_rejected_identically_on_both_backends() {
        let src = "fn main(console: Console):\n    if false:\n\tconsole.print(\"tab body executed\")\n    console.print(\"done\")\n";
        let err = typeck::check_str(src).expect_err("a tab-indented body must be rejected");
        assert!(
            err.to_string().contains("tab in leading indentation"),
            "unexpected error: {err}"
        );
    }

    /// PARITY (BUG-339): a multiline tagged literal keeps the raw newline in its
    /// content byte-for-byte. `tagged::parse_splice_expr` used to reindent EVERY
    /// newline when nesting the emitted source under its throwaway `fn __tagsplice()`
    /// wrapper, injecting four spaces after a newline that fell inside a string
    /// literal — so `line1\nline2` rendered as `line1\n    line2`. The fix reindents
    /// only STRUCTURAL newlines (outside string literals), so the tagged literal now
    /// matches a plain multiline string and both backends produce identical bytes.
    #[test]
    fn multiline_tag_literal_preserves_raw_newlines_on_both_backends() {
        let src = "import meta\n\ncomptime fn raw(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n    meta.expr_raw(\"\\\"\" + parts.at(0) + \"\\\"\")\n\nfn main(console: Console):\n    console.print(raw\"line1\\nline2\")\n    console.print(\"line1\\nline2\")\n";
        // The plain multiline string (line 2 of output) is the oracle; the tagged
        // literal (line 1) must match it exactly on BOTH backends.
        let expected = vec!["line1\nline2".to_string(), "line1\nline2".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }
