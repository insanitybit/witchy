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

    /// A comparison operator (`==`/`<`/…) desugars to its trait impl by recovering
    /// the operands' concrete type. The receiver may be introduced by a PATTERN
    /// binding — a `match` arm, an `if let`, or a tuple destructure — whose type
    /// the binding scope alone can't surface (it comes from the scrutinee). Since
    /// both operands share a type, the head is recovered from EITHER side and the
    /// impl mangled directly, so `Ok(p) -> p == base` resolves the same on both
    /// backends instead of failing with "unknown function `eq`".
    #[test]
    fn comparison_on_pattern_bound_operand_backends_agree() {
        let src = "import cmp\n\ntype T derive(Show, PartialEq, Eq, PartialOrd, Ord):\n    x: Int\n    y: Int\n\nfn mk() -> Result(T, String):\n    Ok(T(1, 2))\n\nfn pair() -> (T, T):\n    (T(1, 2), T(3, 4))\n\nfn main(console: Console):\n    let base = T(1, 2)\n    match mk():\n        Ok(p) -> console.print(\"${p == base}\")\n        Err(_e) -> console.print(\"err\")\n    if let Ok(p) = mk():\n        console.print(\"${p < T(9, 9)}\")\n    let (a, b) = pair()\n    console.print(\"${a == b}\")\n    console.print(\"${a < b}\")\n";
        let expected = ["true", "true", "false", "true"];
        // The linked path (what the CLI and `witchy parity` use) — it resolves
        // `import cmp` and expands the `derive(Ord)` impls the comparisons need.
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
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

    /// `a ?? b` (RFC-0048) is THE fallback: `Option(T) ?? T -> T` and
    /// `Result(T, e) ?? T -> T`, short-circuiting (the fallback runs only on
    /// `None`/`Err`), chaining right-associatively — and `||` stays Bool-only
    /// logical-or. Both backends must agree: the wasm path is a store-once/
    /// tag-test value-if where the interpreter unwraps the runtime ctor, so
    /// this guards that they stay in sync.
    #[test]
    fn coalesce_fallback_both_backends() {
        let src = "import option\n\nfn find(b: Bool) -> Option(String):\n    if b: Some(\"hit\") else: None\n\nfn parse(s: String) -> Result(Int, String):\n    match s.parse_int():\n        Some(n) -> Ok(n)\n        None -> Err(\"bad int\")\n\nfn main(console: Console):\n    console.print(find(true) ?? \"fallback\")\n    console.print(find(false) ?? \"fallback\")\n    console.print(\"${parse(\"41\") ?? 0}\")\n    console.print(\"${parse(\"x\") ?? 9}\")\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    console.print(\"${d.get(\"a\") ?? d.get(\"b\") ?? 0}\")\n    console.print(\"${d.get(\"z\") ?? d.get(\"b\") ?? 5}\")\n    console.print(\"${Some(\"\") ?? \"x\"}\")\n    console.print(\"${false || true}\")\n";
        let want: Vec<String> = ["hit", "fallback", "41", "9", "1", "5", "", "true"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
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

    /// (RFC-0052) Binding or-patterns `Circle(n) | Square(n)` — every alternative
    /// binds the same name; the arm body sees the matched alternative's value.
    #[test]
    fn binding_or_patterns_backends_agree() {
        let src = "type Shape:\n    Circle(Int)\n    Square(Int)\n\nfn size(s: Shape) -> Int:\n    match s:\n        Circle(n) | Square(n) -> n\n\nfn main(console: Console):\n    console.print(\"${size(Circle(3))}\")\n    console.print(\"${size(Square(7))}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["3", "7"]);
    }

    /// (RFC-0052) Duration literal patterns `1s`/`-1s` — exact ms equality — and
    /// the `-1s` negative-duration lexer/typeck fix, on both backends.
    #[test]
    fn duration_patterns_backends_agree() {
        let src = "fn f(d: Duration) -> String:\n    match d:\n        1s -> \"one\"\n        -1s -> \"neg\"\n        _ -> \"other\"\n\nfn main(console: Console):\n    console.print(f(1s))\n    console.print(f(-1s))\n    console.print(f(5s))\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["one", "neg", "other"]);
    }

    /// (RFC-0052) `for` and comprehension take the SAME pattern grammar: a tuple
    /// header destructures each element, on both backends.
    #[test]
    fn for_and_comprehension_patterns_backends_agree() {
        let src = "fn main(console: Console):\n    let pairs = [(1, 2), (3, 4)]\n    for (a, b) in pairs:\n        console.print(\"${a}+${b}\")\n    let sums = [a + b for (a, b) in pairs]\n    console.print(\"${sums}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["1+2", "3+4", "[3, 7]"]);
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

    // Multi-generator comprehensions nest in source order: two `for` clauses
    // form a cartesian product, and an interleaved `if` filters using earlier
    // loop variables. Both backends agree.
    // Integration showcase: Pythagorean triples in one comprehension —
    // three nested generators over inclusive ranges with variable bounds
    // (`b in a..=20`), a filter, and tuple construction, then tuple
    // destructuring in a for-loop. Exercises ranges + multi-generator
    // comprehensions + tuples together; both backends agree.
    #[test]
    fn pythagorean_triples_comprehension_backends_agree() {
        let client = r#"
import list
fn main(console: Console):
    let triples = [(a, b, c) for a in 1..=20 for b in a..=20 for c in b..=20 if a * a + b * b == c * c]
    console.print("${list.length(triples)}")
    var total = 0
    for t in triples:
        let (a, b, c) = t
        total = total + c
    console.print("${total}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pythagorean comprehension diverged");
        assert_eq!(compiled, vec!["6", "80"]);
    }

    // break exits the innermost loop; continue skips to the next iteration —
    // in both for-loops (continue advances the index) and while-loops (continue
    // re-checks the condition). Both backends agree.
    // break/continue branching out of a result-typed `match` arm inside a loop
    // must still produce valid WASM (the branch unwinds the match's value).
    #[test]
    fn break_inside_match_in_loop_backends_agree() {
        let src = r#"
fn main(console: Console):
    var total = 0
    for x in [1, 2, 3, 4, 5]:
        match x:
            3 ->
                break
            _ ->
                total = (total + x)
    console.print("${total}")
    var kept = 0
    for y in [1, 2, 3, 4]:
        match y:
            2 ->
                continue
            _ -> 0
        kept = (kept + y)
    console.print("${kept}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "break/continue in match diverged");
        assert_eq!(run_on_wasm(src), vec!["3", "8"]);
    }
