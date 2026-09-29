use super::*;

    /// RFC-0046 acceptance (b): a trait call resolves on ANY expression the
    /// checker types — here `list.at(string.split(...), 0)`, a builtin-call
    /// result nested in another builtin call. The string shadow system stripped
    /// `string.split`'s `List(String)` return to the bare head `"List"`, so
    /// `list.at`'s element was unrecoverable and `say`'s `Show` bound
    /// specialized to the literal type-variable `"a"` and failed. With dispatch
    /// reading typeck's real `TypeTable` first, the element is `String` and
    /// `show` resolves — identically on both backends.
    #[test]
    fn rfc0046_trait_call_on_builtin_result_resolves_show() {
        let src = "import show\nimport list\n\nfn main(console: Console):\n    let parts = \"a,b,c\".split(\",\")\n    show.say(console, list.at(parts, 1))\n    show.say(console, list.at(\"x-y\".split(\"-\"), 0))\n";
        let want = vec!["b".to_string(), "x".to_string()];
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// RFC-0046 regression: a first-class function whose name coincides with a
    /// trait method — the comparator PARAMETER `less` that `list.sort_by`/
    /// `max_by`/`min_by` take — is invoked as the passed-in function, never
    /// rewritten to the element type's `Ord::less`. Latent until `cmp` was
    /// linked everywhere (step 3 imports it into `std/list`); a `max_by` with a
    /// reversed comparator would otherwise silently ignore it and return the
    /// plain maximum. Reversed-less `max_by` must return the minimum.
    #[test]
    fn rfc0046_comparator_param_named_like_trait_method_is_not_dispatched() {
        let src = "import list\nimport option\nimport cmp\n\nfn main(console: Console):\n    let xs = [3, 1, 4, 1, 5, 9, 2]\n    console.print(\"${option.unwrap_or(list.max_by(xs, fn(a: Int, b: Int): (0 - a) < (0 - b)), 0)}\")\n    console.print(\"${option.unwrap_or(list.min_by(xs, fn(a: Int, b: Int): (0 - a) < (0 - b)), 0)}\")\n";
        // Reversed comparator: max_by finds the minimum (1), min_by the maximum (9).
        let want = vec!["1".to_string(), "9".to_string()];
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// RFC-0046 acceptance (a): `iter.collect` infers through a GENERIC helper.
    /// `firsts` is unbounded-generic over `a`; its tail `iter.collect(...)` is a
    /// bounded `FromIterator` template whose result type is the helper's own
    /// generic return `List(a)` — unresolvable while `firsts` stays generic. The
    /// fixpoint monomorphizes `firsts` at its concrete call site, re-annotates so
    /// `firsts__Int`'s `iter.collect` types as `List(Int)`, then resolves it — with
    /// no ascription at EITHER site, identically on both backends. This is the
    /// primary acceptance test and failed ("cannot infer the result type for
    /// `iter.collect`") before the fixpoint landed.
    #[test]
    fn rfc0046_accept_a_iter_collect_infers_through_generic_helper() {
        let src = "import iter\n\nfn firsts(xs: List(a)) -> List(a):\n    iter.collect(iter.from_list(xs).take(2))\n\nfn main(console: Console):\n    let ys = firsts([1, 2, 3])\n    console.print(\"${ys}\")\n";
        let want = vec!["[1, 2]".to_string()];
        assert_eq!(wasm_run(src), want, "wasm");
    }

