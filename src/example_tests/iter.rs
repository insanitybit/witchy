use super::*;
use crate::{interpreter, typeck};

    /// (BUG-366) Lazy iterator adapters must not recurse once per skipped
    /// element inside a single pull. Long rejected prefixes should behave like
    /// ordinary loop work on both backends.
    #[test]
    fn iterator_skip_adapters_handle_long_prefixes_on_both_backends() {
        let cases = [
            (
                "filter",
                "import iter\n\nfn even_after(n: Int) -> Bool:\n    n >= 1000 && n % 2 == 0\n\nfn main(console: Console):\n    match iter.range(0, 1002).filter(even_after).split_first():\n        Some(pair) ->\n            let (x, _rest) = pair\n            console.print(\"${x}\")\n        None -> console.print(\"missing\")\n",
                ["1000"],
            ),
            (
                "filter_map",
                "import iter\nimport option\n\nfn only_after(n: Int) -> Option(Int):\n    if n >= 1000:\n        Some(n + 1)\n    else:\n        None\n\nfn main(console: Console):\n    match iter.range(0, 1001).filter_map(only_after).split_first():\n        Some(pair) ->\n            let (x, _rest) = pair\n            console.print(\"${x}\")\n        None -> console.print(\"missing\")\n",
                ["1001"],
            ),
            (
                "drop_while",
                "import iter\n\nfn main(console: Console):\n    match iter.range(0, 1002).drop_while(fn(n: Int): n < 1000).split_first():\n        Some(pair) ->\n            let (x, _rest) = pair\n            console.print(\"${x}\")\n        None -> console.print(\"missing\")\n",
                ["1000"],
            ),
            (
                "flat_map",
                "import iter\n\nfn empty_until_last(n: Int) -> Iter(Int):\n    if n < 1000:\n        iter.empty()\n    else:\n        iter.once(n)\n\nfn main(console: Console):\n    match iter.range(0, 1001).flat_map(empty_until_last).split_first():\n        Some(pair) ->\n            let (x, _rest) = pair\n            console.print(\"${x}\")\n        None -> console.print(\"missing\")\n",
                ["1000"],
            ),
        ];
        for (label, src, expected) in cases {
            assert_eq!(link_run(src), expected, "interp: {label}");
            assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: {label}");
        }
    }

    /// `for x in a..b` is a counting loop on both backends — never a materialized
    /// list — with faithful `break`/`continue`, inclusive (`..=`), empty, and
    /// nested behavior. The 100_000-iteration loop proves nothing is allocated:
    /// `run_on_wasm` caps memory at 4 pages, so a materialized range would trap.
    #[test]
    fn range_for_loops_match_on_both_backends() {
        let src = r#"fn main(console: Console):
    var a = 0
    for i in 0..5:
        a = a + i
    console.print("${a}")
    var b = 0
    for i in 1..=5:
        b = b + i
    console.print("${b}")
    var c = 0
    for i in 0..100:
        if i == 10:
            break
        c = c + i
    console.print("${c}")
    var d = 0
    for i in 0..10:
        if i % 2 == 0:
            continue
        d = d + i
    console.print("${d}")
    var e = 0
    for i in 5..5:
        e = e + 1
    for i in 5..2:
        e = e + 1
    console.print("${e}")
    var f = 0
    for i in 0..3:
        for j in 0..3:
            f = f + i * j
    console.print("${f}")
    var g = 0
    for i in 0..100000:
        g = g + 1
    console.print("${g}")
"#;
        let expected = vec!["10", "15", "45", "25", "0", "9", "100000"];
        assert_eq!(interp(src), expected);
        assert_eq!(run_on_wasm(src), expected);
    }

    /// The fallback side of `??` is LAZY: it must not run when the left is
    /// `Some`/`Ok` — observable through a printing side effect, on both backends.
    #[test]
    fn coalesce_fallback_is_lazy_both_backends() {
        let src = "import option\n\nfn side(console: Console, tag: String, v: Int) -> Int:\n    console.print(\"eval ${tag}\")\n    v\n\nfn main(console: Console):\n    let a = Some(1) ?? side(console, \"unreached\", 2)\n    console.print(\"${a}\")\n    let b = None ?? side(console, \"reached\", 3)\n    console.print(\"${b}\")\n";
        let want: Vec<String> =
            ["1", "eval reached", "3"].iter().map(|s| s.to_string()).collect();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `iter.drop` must not pull from its source at construction time (the
    /// lazy-adapter contract, like take/take_while/drop_while): building
    /// `drop(explode, 1)` over an aborting generator succeeds; only consuming
    /// the returned iterator would abort.
    #[test]
    fn iter_drop_is_lazy_on_both_backends() {
        let src = r#"import iter
import option

fn explode(i: Int) -> Option(Int):
    if i >= 0:
        fail("iter was pulled at ${i}")
    None

fn main(console: Console):
    let dropped = iter.from_gen(explode).drop(1)
    console.print("constructed")
"#;
        assert_eq!(link_run(src), vec!["constructed"], "interpreter must not pull at construction");
        assert_eq!(wasm_run(src), vec!["constructed"], "compiled must not pull at construction");
    }

    #[test]
    fn std_option_combinators_backends_agree() {
        // is_none / and_then / filter behave identically in both backends.
        let client = r#"
import option

fn main(console: Console):
    let s = Some(5)
    console.print("${option.is_none(s)}")
    console.print("${option.is_none(option.filter(s, fn(n: Int): (n > 10)))}")
    let chained = option.and_then(s, fn(n: Int): Some((n * 2)))
    console.print("${option.unwrap_or(chained, 0)}")
    let kept = option.filter(s, fn(n: Int): (n > 0))
    console.print("${option.unwrap_or(kept, 0)}")
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option combinators diverged");
    }

    #[test]
    fn duration_combinators_backends_agree() {
        // max/min/is_zero/abs over the Duration type (it has no Ord impl, so the
        // generic ord helpers don't apply).
        let client = r#"
import duration
fn main(console: Console):
    console.print(duration.human(duration.max(30s, 1m)))
    console.print(duration.human(duration.min(30s, 1m)))
    console.print("${duration.is_zero(0ms)}")
    console.print("${duration.is_zero(1s)}")
    console.print(duration.human(duration.abs(0s - 5s)))
"#;
        let sources = [
            ("duration", crate::bundled_module("duration").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "duration combinators diverged");
        assert_eq!(compiled, vec!["1m0s", "30s", "true", "false", "5s"]);
    }

    #[test]
    fn std_result_combinators_backends_agree() {
        // is_err / and_then / map_err / unwrap_err behave identically in both
        // backends — including using is_err at two different error types in one
        // program (Result(Int, String) and the Result(Int, Int) that map_err
        // produces), which per-call generalization now allows.
        let client = r#"
import result

fn checked(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    console.print("${result.is_err(checked(5))}")
    console.print("${result.is_err(checked((0 - 1)))}")
    let chained = result.and_then(checked(5), fn(n: Int): Ok((n * 10)))
    console.print("${result.unwrap_or(chained, 0)}")
    let mapped = result.map_err(checked((0 - 1)), fn(s: String): s.length())
    console.print("${result.is_err(mapped)}")
"#;
        let sources = [("result", crate::bundled_module("result").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result combinators diverged");
    }

    /// `std/iter` is the lazy pull-based iterator module (witchy's answer to
    /// Rust's Iterator). Lazy `map`/`filter`/`take_while` over an *infinite*
    /// `count_from`, plus `find`/`sum`/`collect`/`count` consumers, must agree on
    /// both backends — closures-in-ADTs + recursion compile to WASM.
    #[test]
    fn std_iter_lazy_adapters_backends_agree() {
        let client = r#"
import iter
fn main(console: Console):
    // squares of 1.. while < 100, kept odd, summed: 1+9+25+49+81 = 165
    let sq = iter.count_from(1).map(fn(n: Int): n * n)
    let small = sq.take_while(fn(s: Int): s < 100)
    console.print("${small.filter(fn(s: Int): s % 2 == 1).sum()}")
    // first multiple of 7 above 50, from an infinite iterator
    match iter.count_from(51).find(fn(n: Int): n % 7 == 0):
        Some(n) -> console.print("${n}")
        None -> console.print("none")
    // a finite range, doubled and collected
    console.print("${iter.range(0, 5).count()}")
    let vs: List(Int) = iter.collect(iter.range(0, 3).map(fn(n: Int): n * 10))
    for v in vs:
        console.print("${v}")
"#;
        let sources = [("iter", crate::bundled_module("iter").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std/iter diverged");
        assert_eq!(compiled, vec!["165", "56", "5", "0", "10", "20"]);
    }
