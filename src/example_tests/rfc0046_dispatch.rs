use super::*;

    // Traits: an `impl` provides a method per type, and a trait-method call
    // resolves to the impl for the receiver's concrete type — at a literal
    // receiver, a `let`-bound one, and across two implementing types. The trait
    // is lowered to ordinary functions, so both backends agree.
    #[test]
    fn traits_concrete_dispatch_backends_agree() {
        let src = r#"
trait Describe:
    fn describe(self) -> String

impl Describe for Int:
    fn describe(self) -> String:
        "${self}"

impl Describe for Bool:
    fn describe(self) -> String:
        if self:
            "yes"
        else:
            "no"

fn main(console: Console):
    console.print(describe(42))
    console.print(describe(true))
    let n = 7
    console.print(describe(n))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "trait dispatch diverged");
        assert_eq!(run_on_wasm(src), vec!["42", "yes", "7"]);
    }

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
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// RFC-0046 acceptance (c): the `Eq`-bounded `list.*` search functions
    /// (`unique`/`contains`/`index_of`/`position`) monomorphize per element
    /// type, so they COMPILE ON WASM for a user RECORD element type — where the
    /// unbounded generic `==` could not (the compiled backend has no structural
    /// equality through an unresolved type variable). `Point` derives `Eq`; the
    /// bound discharges to that impl, and both backends agree.
    #[test]
    fn rfc0046_eq_bounded_list_search_compiles_for_records_on_wasm() {
        let src = "import list\nimport cmp\n\ntype Point derive(PartialEq, Eq):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let ps = [Point(1, 2), Point(1, 2), Point(3, 4)]\n    let u = list.unique(ps)\n    console.print(\"${list.length(u)}\")\n    console.print(\"${list.contains(ps, Point(3, 4))}\")\n    console.print(\"${list.index_of(ps, Point(3, 4)) ?? -1}\")\n";
        let want = vec!["2".to_string(), "true".to_string(), "2".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
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
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// BUG-001: the SECOND comparator-hijack site. A user `where a: Ord` generic
    /// whose body calls a `fn`-typed parameter named like a trait method (`less`)
    /// is monomorphized; `Mono::specialize`'s `rename_calls_block` used to blanket-
    /// rename every `less(…)` call to `Ord__Int__less`, silently discarding the
    /// passed comparator (both backends agreed on the WRONG answer, so the
    /// differential net was blind). The rename now skips bound locals, so the
    /// reversed comparator makes `pick` return the maximum (3), not the minimum.
    #[test]
    fn rfc0046_bug001_mono_rename_skips_local_comparator_param() {
        let src = "fn pick(xs: List(a), less: fn(a, a) -> Bool) -> a where a: Ord:\n    var best = list.at(xs, 0)\n    for x in xs:\n        if less(x, best):\n            best = x\n    best\n\nfn rev(a: Int, b: Int) -> Bool:\n    a > b\n\nfn main(console: Console):\n    console.print(\"${pick([3, 1, 2], rev)}\")\n";
        // `rev` reverses order, so `pick` returns the max (3), not the min (1).
        let want = vec!["3".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
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
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// RFC-0046 acceptance (a), reused at two element types: the same generic
    /// helper is monomorphized once per concrete instantiation (a record type and
    /// `String`), each re-annotated and resolved independently — the transitive-
    /// monomorphization fixpoint is not single-shot.
    #[test]
    fn rfc0046_accept_a_generic_helper_specializes_per_element_type() {
        let src = "import iter\n\ntype Point derive(Show):\n    Point(Int, Int)\n\nfn firsts(xs: List(a)) -> List(a):\n    iter.collect(iter.from_list(xs).take(2))\n\nfn main(console: Console):\n    let ps = firsts([Point(1, 2), Point(3, 4), Point(5, 6)])\n    console.print(\"${ps}\")\n    let ss = firsts([\"a\", \"b\", \"c\"])\n    console.print(\"${ss}\")\n";
        let want = vec!["[Point(1, 2), Point(3, 4)]".to_string(), "[a, b]".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// BUG-013 guard: the RFC-0046 lowering fixpoint reuses `first_table` (skips
    /// the redundant FINAL re-annotate) whenever a `lower_with` call monomorphizes
    /// nothing — the case for every `derive(...)` comptime block. This program
    /// stresses BOTH branches of that decision in one module: six `derive` comptime
    /// blocks (two enums × three derives) take the reuse path, while the Eq-bounded
    /// generic `dedup_count` is monomorphized at two record element types (Color and
    /// Tag), so ITS `lower_with` still takes the final re-annotate. If the reused
    /// table were ever stale, the derived `Show`/`Eq` dispatch would render or dedup
    /// wrong — so identical output on both backends pins the optimization to zero
    /// behavior change. (Also a bounded-work regression tripwire: a program this
    /// shape must compile without the fixpoint fanning out.)
    #[test]
    fn rfc0046_bug013_derive_and_bounded_generic_lower_without_stale_table() {
        let src = "import list\n\ntype Color derive(PartialEq, Eq, Show):\n    Red\n    Green\n    Blue\n\ntype Tag derive(PartialEq, Eq, Show):\n    Tag(Int)\n\nfn dedup_count(xs: List(a), y: a) -> Int where a: Eq:\n    list.count(list.unique(xs), y)\n\nfn main(console: Console):\n    let cs = [Red, Green, Red, Blue, Green, Red]\n    console.print(\"${list.unique(cs)}\")\n    console.print(\"${dedup_count(cs, Red)}\")\n    let ts = [Tag(1), Tag(2), Tag(1), Tag(3)]\n    console.print(\"${list.unique(ts)}\")\n    console.print(\"${dedup_count(ts, Tag(1))}\")\n";
        let want = vec![
            "[Red, Green, Blue]".to_string(),
            "1".to_string(),
            "[Tag(1), Tag(2), Tag(3)]".to_string(),
            "1".to_string(),
        ];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// RFC-0046 step 5 (acceptance d, first clause): the new `Iter` combinators —
    /// `min`/`max` (Ord-bounded), `last`, `position`, `scan` (lazy stateful map),
    /// and `flatten` — exist and produce identical results on both backends,
    /// including `min` over `String` (Ord dispatch through a `where a: Ord` iter
    /// combinator). Each of these was previously unwritable because its bounded /
    /// generic-return signature did not survive inference; the step-1 fixpoint is
    /// what lets them monomorphize.
    #[test]
    fn rfc0046_iter_combinators_min_max_last_position_scan_flatten() {
        let src = "import iter\nimport option\n\nfn main(console: Console):\n    console.print(\"${option.unwrap_or(iter.from_list([3, 1, 4, 1, 5]).min(), 0)}\")\n    console.print(\"${option.unwrap_or(iter.from_list([3, 1, 4, 1, 5]).max(), 0)}\")\n    console.print(\"${option.unwrap_or(iter.from_list([10, 20, 30]).last(), 0)}\")\n    console.print(\"${option.unwrap_or(iter.from_list([3, 1, 4, 1, 5]).position(fn(n: Int): n == 4), 0 - 1)}\")\n    let sums: List(Int) = iter.collect(iter.from_list([1, 2, 3, 4]).scan(0, fn(s: Int, x: Int): (s + x, s + x)))\n    console.print(\"${sums}\")\n    let flat: List(Int) = iter.collect(iter.from_list([iter.from_list([1, 2]), iter.from_list([3, 4])]).flatten())\n    console.print(\"${flat}\")\n    console.print(\"${option.unwrap_or(iter.from_list([\"pear\", \"apple\", \"kiwi\"]).min(), \"?\")}\")\n";
        let want = vec![
            "1".to_string(),
            "5".to_string(),
            "30".to_string(),
            "2".to_string(),
            "[1, 3, 6, 10]".to_string(),
            "[1, 2, 3, 4]".to_string(),
            "apple".to_string(),
        ];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// RFC-0046 step 1/4 regression (caught by the full gate on examples/diff +
    /// examples/life): a LOCAL bound to a GENERIC CALL RESULT is a method-call
    /// receiver — `table: List(List(Int))`, `let above = table.at(i - 1)`, then
    /// `above.at(j - 1)`. The deleted string `recover_generic_call` used to type
    /// `above`; its typed replacement `declared_call_result` must judge the
    /// binding from the callee's DECLARED signature (unify `List(a)` against
    /// `List<List<Int>>` -> `a = List<Int>`), because the QUIET pre-mono pass has
    /// an empty table and annotate hard-errors on any unresolved MethodCall.
    /// Covers the nested-container `.at` chain, `.length()` on a bound result,
    /// and a subscript receiver `xs[i]` — identical on both backends.
    #[test]
    fn rfc0046_method_call_on_let_bound_generic_call_result() {
        let src = "fn main(console: Console):\n    let table = [[1, 2, 3], [4, 5, 6]]\n    let above = table.at(1)\n    console.print(\"${above.at(2)}\")\n    console.print(\"${above.length()}\")\n    let row = table[0]\n    console.print(\"${row.at(0)}\")\n";
        let want = vec!["6".to_string(), "3".to_string(), "1".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// RFC-0046 step 5 (acceptance d, second clause): std modules can consume
    /// `Iter` internally — `path` (`drop_last` via `iter.take`) and `semver`
    /// (`best` via `iter.filter` + `iter.fold`) dogfood the lazy layer. The
    /// observable output is unchanged AND identical on both backends (the generic
    /// iter pipelines monomorphize for the compiled path). Before RFC-0046, no std
    /// module imported iter because inference through it was unreliable.
    #[test]
    fn rfc0046_std_dogfoods_iter_in_path_semver() {
        let src = "import path\nimport semver\n\nfn main(console: Console):\n    console.print(path.normalize(\"a/b/c/../../d\"))\n    let vs = [semver.version(1, 2, 0), semver.version(1, 5, 3), semver.version(2, 0, 0)]\n    match semver.parse_req(\"^1.0.0\"):\n        Ok(req) ->\n            match semver.best(vs, req):\n                Some(v) -> console.print(semver.format(v))\n                None -> console.print(\"none\")\n        Err(e) -> console.print(semver.semver_error_message(e))\n";
        let want = vec!["a/d".to_string(), "1.5.3".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }
