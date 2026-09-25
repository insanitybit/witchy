use super::*;
use crate::{interpreter, parser, typeck};

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

fn pair(n: Int) -> (Int, Int):
    var a = 0
    for i in [1, 2, 3]:
        a = a + i
    (a, n)

fn main(console: Console):
    let xs = [area(Circle(2)), area(Rect(3, 4))]
    var total = 0
    for x in xs:
        total = total + x
    console.print("${total}")
    // A tuple expression after an indented block must remain a separate
    // statement, not an application of the block's value.
    let (x, y) = pair(10)
    console.print("${x}")
    console.print("${y}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "indentation backends diverged");
        assert_eq!(run_on_wasm(src), vec!["24", "6", "10"]);
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

    /// PARITY (BUG-339): a multiline tagged literal keeps the raw newline in its
    /// content byte-for-byte. `tagged::parse_splice_expr` used to reindent EVERY
    /// newline when nesting the emitted source under its throwaway `fn __tagsplice()`
    /// wrapper, injecting four spaces after a newline that fell inside a string
    /// literal — so `line1\nline2` rendered as `line1\n    line2`. The fix reindents
    /// only STRUCTURAL newlines (outside string literals), so the tagged literal now
    /// matches a plain multiline string and both backends produce identical bytes.
    #[test]
    fn multiline_tag_literal_preserves_raw_newlines_on_both_backends() {
        let src = "import meta\n\ncomptime fn raw(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:\n    meta.expr_string(parts.at(0))\n\nfn main(console: Console):\n    console.print(raw\"line1\\nline2\")\n    console.print(\"line1\\nline2\")\n";
        // The plain multiline string (line 2 of output) is the oracle; the tagged
        // literal (line 1) must match it exactly on BOTH backends.
        let expected = vec!["line1\nline2".to_string(), "line1\nline2".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }
