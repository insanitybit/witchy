use super::*;
use crate::{interpreter, typeck};

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
