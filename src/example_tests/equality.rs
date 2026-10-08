use super::*;
use crate::typeck;

    /// (RFC-0047) `==` on a function type is a compile-time error — there is no
    /// stable equality for functions (identity is a monomorphization/inlining
    /// accident), and comparing them was a confirmed backend parity divergence
    /// (interpreter name-compares `true`, compiled pointer-compares `false`).
    /// Rejecting deletes the divergence by construction. Both the direct case and
    /// the container/tuple case must error with a teaching message.
    #[test]
    fn function_equality_is_a_compile_error() {
        let direct = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    console.print(\"${f == f}\")\n";
        let e = typeck::check_str(direct).expect_err("`f == f` must be rejected");
        assert!(e.contains("not defined on function types"), "teaching error, got: {e}");
        // Nested inside a container is caught the same way (depth-uniform).
        let in_list = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    console.print(\"${[f] == [f]}\")\n";
        let el = typeck::check_str(in_list).expect_err("`[f] == [f]` must be rejected");
        assert!(el.contains("not defined on function types"), "teaching error, got: {el}");
        let in_tuple = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    console.print(\"${(f, 1) == (f, 1)}\")\n";
        assert!(
            typeck::check_str(in_tuple).expect_err("`(f, 1) == (f, 1)` must be rejected")
                .contains("not defined on function types"),
            "a function nested in a tuple must be rejected too"
        );
    }

    /// A RECURSIVE generic ADT (`Stack(a)`, whose `Push` carries a `Stack(a)`)
    /// compares structurally on compiled Wasm: the shape is identified by its
    /// type arguments and the generated helper calls itself for the
    /// self-referential field — deep spines compare by content, through
    /// literals, declared parameter types, and nullary constructors.
    #[test]
    fn recursive_generic_adt_equality_agrees_on_both_backends() {
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\nfn same(s: Stack(Int), t: Stack(Int)) -> Bool:\n    s == t\n\nfn main(console: Console):\n    console.print(\"${Push(2, Push(1, Empty)) == Push(2, Push(1, Empty))}\")\n    console.print(\"${Push(2, Push(1, Empty)) == Push(2, Push(9, Empty))}\")\n    console.print(\"${Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"a\", Empty))}\")\n    console.print(\"${Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"z\", Empty))}\")\n    console.print(\"${same(Push(1, Empty), Push(1, Empty))}\")\n    console.print(\"${same(Push(1, Empty), Empty)}\")\n";
        let want: Vec<String> = ["true", "false", "true", "false", "true", "false"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// The boundary of structural equality stays LOUD where the payload is
    /// genuinely unresolvable. Return-position inference resolves non-empty list
    /// payloads and compares them by element. An empty-list payload still has no
    /// element evidence, so the checked pipeline rejects it before codegen —
    /// never silently pointer-comparing.
    #[test]
    fn unsupported_compound_equality_is_a_loud_error_not_silent() {
        let resolved = "import result\n\nfn wrap(x: a) -> Result(a, String):\n    Ok(x)\n\nfn main(console: Console):\n    console.print(\"${wrap([1]) == wrap([2])}\")\n";
        assert_eq!(wasm_run(resolved), vec!["false"], "backends agree");
        let empty = "import result\n\nfn wrap(x: a) -> Result(a, String):\n    Ok(x)\n\nfn main(console: Console):\n    console.print(\"${wrap([]) == wrap([])}\")\n";
        // The empty payload's element type defaults to unit, whose equality
        // is total: two empty lists are equal.
        assert_eq!(wasm_run(empty), vec!["true"], "empty payloads compare equal");
    }

    /// Ordering a NaN must FAIL, not silently return IEEE false
    /// on WASM. The compiled
    /// `<`/`<=`/`>`/`>=` on floats route through a NaN-trapping helper. Equality
    /// (`==`) is IEEE (NaN == NaN is false). Ordinary
    /// float ordering is unchanged. (Regression for a silent NaN-ordering
    /// divergence.)
    #[test]
    fn nan_ordering_errors_on_both_backends() {
        for cmp in ["nan < 1.0", "nan > 1.0", "nan <= nan", "nan >= 0.0"] {
            let src = format!(
                "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(\"${{{cmp}}}\")\n"
            );
            let error = crate::run_admitted_artifact(&admitted_artifact(&src)).expect_err(&format!("WASM must trap on `{cmp}`"));
            assert!(error.contains("line 3: cannot compare NaN"), "`{cmp}`: {error}");
        }
        // Ordinary float ordering and NaN equality still agree.
        let ok = "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(\"${1.5 < 2.5}\")\n    console.print(\"${2.5 <= 2.5}\")\n    console.print(\"${nan == nan}\")\n";
        let want = vec!["true".to_string(), "true".to_string(), "false".to_string()];
        assert_eq!(run_on_wasm(ok), want, "compiled WASM must agree");
    }
