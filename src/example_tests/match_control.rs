use super::*;
use crate::{interpreter, typeck};

    /// (BUG-324) The lexer admits the Int.MIN magnitude as a wrapped token so
    /// expression literals can spell `-9223372036854775808`; pattern parsing must
    /// use the same wraparound negation instead of panicking in debug builds.
    #[test]
    fn int_min_literal_patterns_work_on_both_backends() {
        let src = "fn main(console: Console):\n    let n = -9223372036854775808\n    match n:\n        -9223372036854775808 -> console.print(\"min\")\n        _ -> console.print(\"other\")\n    let m = -9223372036854775807\n    match m:\n        -9223372036854775808..=-9223372036854775807 -> console.print(\"range\")\n        _ -> console.print(\"miss\")\n";
        let expected = ["min", "range"];
        assert_eq!(link_run(src), expected, "interp: Int.MIN pattern");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: Int.MIN pattern",
        );
    }

    #[test]
    fn match_soundness_exhaustiveness_and_linearity() {
        // C3: an infinite scalar domain needs a catch-all — a guard-only match is
        // non-exhaustive and would trap at runtime, so it's rejected at check time.
        let guard_only = "fn f(n: Int) -> String:\n    match n:\n        m if m > 0 -> \"p\"\n        z if z < 0 -> \"n\"\nfn main(c: Console):\n    c.print(f(1))\n";
        let e = typeck::check_str(guard_only).expect_err("guard-only Int match must be rejected");
        assert!(e.to_string().contains("non-exhaustive match on `Int`"), "{e}");

        // C2: a single-field variant matched only with a narrower sub-pattern
        // (`Circle(Red)`) is rejected when an inner case (`Circle(Blue)`) is
        // missing — the recursive coverage check catches the nested hole.
        let nested = "type Color:\n    Red\n    Blue\ntype Shape:\n    Circle(Color)\n    Square\nfn f(s: Shape) -> Int:\n    match s:\n        Circle(Red) -> 1\n        Square -> 2\nfn main(c: Console):\n    c.print(\"${f(Square)}\")\n";
        let e = typeck::check_str(nested).expect_err("nested non-exhaustive match must be rejected");
        assert!(e.to_string().contains("non-exhaustive"), "{e}");

        // ...but the idiomatic `Some(V) / None` form — `Some` covered by
        // ENUMERATING the inner variants, no wholesale `Some(_)` — must still check
        // (the conservative earlier rule wrongly rejected this; the recursion does not).
        let some_enum = "type Msg:\n    A\n    B\nfn f(o: Option(Msg)) -> Int:\n    match o:\n        Some(A) -> 1\n        Some(B) -> 2\n        None -> 0\nfn main(c: Console):\n    c.print(\"${f(Some(A))}\")\n";
        assert!(typeck::check_str(some_enum).is_ok(), "idiomatic Some(V)/None must check");

        // C5: a pattern may not bind the same name twice (no equality patterns).
        let dup = "type P:\n    P(Int, Int)\nfn f(p: P) -> Int:\n    match p:\n        P(x, x) -> x\nfn main(c: Console):\n    c.print(\"${f(P(3, 4))}\")\n";
        let e = typeck::check_str(dup).expect_err("duplicate pattern binding must be rejected");
        assert!(e.to_string().contains("more than once"), "{e}");

        // Valid exhaustive / linear matches still check (no over-rejection).
        let ok = "type Shape:\n    Circle(Int)\n    Square\nfn f(s: Shape) -> Int:\n    match s:\n        Circle(r) -> r\n        Square -> 0\nfn g(n: Int) -> Int:\n    match n:\n        0 -> 0\n        _ -> 1\nfn main(c: Console):\n    c.print(\"${f(Circle(3)) + g(5)}\")\n";
        assert!(typeck::check_str(ok).is_ok(), "valid exhaustive matches must check");
    }

    /// RFC-0048's other half: the truthy fallback is GONE. `||` on a String (or
    /// any non-Bool) is a check-time teaching error pointing at `??`, and `??`
    /// on a non-Option/Result left side is rejected too.
    #[test]
    fn or_is_bool_only_teaching_errors() {
        let err = typeck::check_str(
            "fn main(console: Console):\n    console.print(\"\" || \"default\")\n",
        )
        .expect_err("String || must be rejected");
        assert!(
            err.contains("`||` is logical-or on Bool") && err.contains("use `??`"),
            "unexpected message: {err}"
        );
        let err = typeck::check_str(
            "fn main(console: Console):\n    let n = 1 ?? 2\n    console.print(\"${n}\")\n",
        )
        .expect_err("Int ?? must be rejected");
        assert!(
            err.contains("`??` unwraps an Option or a Result"),
            "unexpected message: {err}"
        );
    }

    /// (RFC-0052) Integer range patterns `lo..hi` / `lo..=hi` as real nodes, on
    /// both backends — half-open and inclusive, with a catch-all.
    #[test]
    fn range_patterns_backends_agree() {
        let src = "fn classify(n: Int) -> String:\n    match n:\n        0..10 -> \"low\"\n        10..=20 -> \"mid\"\n        _ -> \"high\"\n\nfn main(console: Console):\n    console.print(classify(5))\n    console.print(classify(10))\n    console.print(classify(20))\n    console.print(classify(99))\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["low", "mid", "mid", "high"]);
    }

    /// (RFC-0052) Nested or-patterns `Some(1 | 2 | 3)` — impossible before this
    /// RFC (parse error) — parse, check, and run identically on both backends.
    #[test]
    fn nested_or_patterns_backends_agree() {
        let src = "fn f(o: Option(Int)) -> String:\n    match o:\n        Some(1 | 2 | 3) -> \"small\"\n        Some(n) -> \"big\"\n        None -> \"none\"\n\nfn main(console: Console):\n    console.print(f(Some(2)))\n    console.print(f(Some(9)))\n    console.print(f(None))\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["small", "big", "none"]);
    }

    /// (RFC-0052) The refutability rule and literal-pattern edges — check-time
    /// teaching errors, message-pinned.
    #[test]
    fn pattern_refutability_and_literal_edges_errors() {
        // A refutable `let` (multi-variant ctor) points at `if let`.
        let err = typeck::check_str(
            "type Shape:\n    Circle(Int)\n    Square(Int)\n\nfn main(console: Console):\n    let Circle(r) = Circle(3)\n    console.print(\"${r}\")\n",
        )
        .expect_err("refutable let must be rejected");
        assert!(
            err.contains("can fail") && err.contains("if let"),
            "unexpected message: {err}"
        );
        // Float literal patterns are rejected with the precision-trap teaching error.
        let err = typeck::check_str(
            "fn main(console: Console):\n    match 1.5:\n        1.5 -> console.print(\"a\")\n        _ -> console.print(\"b\")\n",
        )
        .expect_err("float literal pattern must be rejected");
        assert!(
            err.contains("Float literals cannot be matched"),
            "unexpected message: {err}"
        );
        // Or-pattern alternatives must bind the same names at the same types.
        let err = typeck::check_str(
            "type T:\n    A(Int)\n    B(String)\n\nfn main(console: Console):\n    match A(1):\n        A(x) | B(x) -> console.print(\"${x}\")\n",
        )
        .expect_err("inconsistent or-binding types must be rejected");
        assert!(
            err.contains("or-pattern binding") && err.contains("inconsistent"),
            "unexpected message: {err}"
        );
    }

    // `match` arm guards (`pattern if cond -> body`): a guard that fails must
    // fall through to later arms, and a wildcard catches the rest. The boundary
    // value 100 (not > 100) must fall through to the `_` arm on both backends.
    #[test]
    fn match_guards_backends_agree() {
        let src = r#"
fn classify(n: Int) -> String:
    match n:
        x if (x < 0) -> "negative"
        0 -> "zero"
        x if (x > 100) -> "big"
        _ -> "small"

fn main(console: Console):
    console.print(classify((0 - 5)))
    console.print(classify(0))
    console.print(classify(200))
    console.print(classify(50))
    console.print(classify(100))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "match guards diverged");
        assert_eq!(run_on_wasm(src), vec!["negative", "zero", "big", "small", "small"]);
    }

