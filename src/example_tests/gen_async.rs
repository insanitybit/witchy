use super::*;
use crate::{interpreter, parser};

    /// (BUG-007) A `gen fn` declared as a METHOD of an inherent `impl` lowers just
    /// like a top-level one: it stays a method (`value.upto()` resolves by receiver
    /// type and returns `Iter(a)`), and its hoisted helper is named per-type so two
    /// types' identically-named generators don't collide. Both backends drive the
    /// resulting iterator to the same list.
    #[test]
    fn gen_method_in_impl_backends_agree() {
        let src = "import iter\n\ntype Counter:\n    n: Int\n\nimpl Counter:\n    gen fn upto(self) -> Iter(Int):\n        var i = 0\n        while i < self.n:\n            yield i\n            i = i + 1\n\ntype Skips:\n    step: Int\n\nimpl Skips:\n    gen fn upto(self) -> Iter(Int):\n        var i = 0\n        while i < 3:\n            yield i * self.step\n            i = i + 1\n\nfn main(console: Console):\n    let c = Counter(4)\n    let xs: List(Int) = iter.collect(c.upto())\n    console.print(\"${xs}\")\n    let s = Skips(10)\n    let ys: List(Int) = iter.collect(s.upto())\n    console.print(\"${ys}\")\n";
        let expected = ["[0, 1, 2, 3]", "[0, 10, 20]"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-306, parity) A user `return` inside a `gen fn` is re-expressed in terms of
    /// the generator's stream contract, NOT passed untranslated into the synthesized
    /// `-> Option(a)` helper. A bare `return` ENDS the stream (both backends), where it
    /// used to leak the internal `Option` type or (as `return Some(v)`) silently repeat
    /// `v` forever. `return <value>` is rejected against the declared `-> Iter(a)`.
    #[test]
    fn gen_fn_bare_return_ends_stream_on_both_backends() {
        let src = "import iter\n\ngen fn firstn(n: Int) -> Iter(Int):\n    var i = 0\n    while true:\n        if i >= n:\n            return\n        yield i\n        i = i + 1\n\nfn main(console: Console):\n    let xs: List(Int) = iter.collect(firstn(3).take(10))\n    console.print(\"${xs}\")\n";
        let expected = ["[0, 1, 2]"];
        assert_eq!(link_run(src), expected, "interp: bare return ends the stream");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: bare return must end the stream identically",
        );
    }

    /// (BUG-306) `return <value>` in a `gen fn` is a compile error naming the declared
    /// `-> Iter(a)` signature — never the synthesized internal `Option(a)`, and never a
    /// silent infinite repeat (the old `return Some(99)` bug).
    #[test]
    fn gen_fn_return_value_is_rejected() {
        for tail in ["return 5", "return Some(99)"] {
            let src = format!(
                "import iter\n\ngen fn g() -> Iter(Int):\n    yield 1\n    {tail}\n\nfn main(console: Console):\n    let xs: List(Int) = iter.collect(g().take(3))\n    console.print(\"${{xs}}\")\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let err = crate::pipeline::link(vec![("main".into(), module)], "main")
                .expect_err("`return <value>` in a gen fn must be rejected");
            assert!(
                err.message.contains("gen fn") && err.message.contains("Iter"),
                "the rejection must name the declared `-> Iter(a)` signature, got: {}",
                err.message
            );
            assert!(
                !err.message.contains("Option"),
                "the internal `Option(a)` protocol must not leak into the diagnostic: {}",
                err.message
            );
        }
    }

    /// `pascal` is an infinite generator whose state is a `List(Int)` row — each
    /// `yield` emits a row, the next built from it. Demonstrates `gen fn` carrying
    /// non-scalar state; agrees on both backends.
    #[test]
    fn pascal_generator_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/pascal/src/pascal.witchy").unwrap();
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client.as_str()),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pascal diverged");
        assert_eq!(
            compiled,
            vec!["1", "1 1", "1 2 1", "1 3 3 1", "1 4 6 4 1", "1 5 10 10 5 1"]
        );
    }

    /// `split_first` + `drop_while` let a user write their own iterator
    /// transforms — here `dedup` (drop consecutive duplicates), composed with
    /// `unfold`. Must agree on both backends.
    #[test]
    fn std_iter_split_first_dedup_backends_agree() {
        let client = std::fs::read_to_string("examples/dedup/src/dedup.witchy").unwrap();
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client.as_str()),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "dedup diverged");
        assert_eq!(compiled, vec!["1 2 3 2 4".to_string()]);
    }

    /// `iter.next` is the documented low-level pull primitive. It must be a real
    /// public API, not just an internal helper reachable only because privacy was
    /// previously unenforced.
    #[test]
    fn std_iter_next_is_public_pull_api() {
        let client = r#"
import iter
fn main(console: Console):
    match iter.from_list([1, 2]).next():
        Empty -> console.print("empty")
        Item(x, rest) ->
            console.print("${x}")
            match rest.next():
                Empty -> console.print("empty")
                Item(y, _more) -> console.print("${y}")
"#;
        let want = vec!["1", "2"];
        assert_eq!(link_run(client), want, "interpreter");
        assert_eq!(wasm_run(client), want, "wasm");
    }

    /// The `std/iter` adapters `enumerate`/`zip`/`chain`/`flat_map`/`for_each`
    /// (plus `func.first`/`second` for the pairs they produce) must agree on both
    /// backends — they compose lazily over finite and infinite iterators.
    #[test]
    fn std_iter_more_adapters_backends_agree() {
        let client = r#"
import iter
import func
fn main(console: Console):
    var es = []
    let ps: List((Int, String)) = iter.collect(iter.from_list(["a", "b", "c"]).enumerate())
    for p in ps:
        list.push(es, "${func.first(p)}" + func.second(p))
    console.print(list.join(es, " "))
    console.print("${iter.count_from(1).zip(iter.from_list([0, 0, 0])).count()}")
    console.print("${iter.range(0, 4).chain(iter.range(10, 13)).sum()}")
    console.print("${iter.range(1, 4).flat_map(fn(n: Int): iter.from_list([n, n])).sum()}")
    iter.count_from(100).take(3).for_each(fn(n: Int): console.print("${n}"))
"#;
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("func", crate::bundled_module("func").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "iter adapters diverged");
        assert_eq!(compiled, vec!["0a 1b 2c", "3", "39", "12", "100", "101", "102"]);
    }

    /// `gen fn` / `yield` (lowered by `crate::generators` to `std/iter`): an
    /// imperative generator that yields a sequence becomes a lazy iterator. The
    /// `generators` example (Fibonacci + Collatz, incl. an infinite generator and
    /// a branch inside a loop) must agree on both backends.
    #[test]
    fn gen_yield_generators_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/generators/src/generators.witchy").unwrap();
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client.as_str()),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generators diverged");
        assert_eq!(
            compiled,
            vec![
                "fib[0..10): 0, 1, 1, 2, 3, 5, 8, 13, 21, 34".to_string(),
                "collatz(6): 6, 3, 10, 5, 16, 8, 4, 2, 1".to_string(),
                "collatz(27) length: 112".to_string(),
            ]
        );
    }

    /// A bare `yield` outside a `gen fn` is a parse error. It used to pass `check`,
    /// silently no-op on the interpreter (`Stmt::Yield` ran like `Stmt::Expr`) and
    /// fail to compile — a backend divergence. Now gated at parse, mirroring the
    /// `.await`/`async fn` rule. `yield` inside a `gen fn` still parses.
    #[test]
    fn yield_outside_gen_fn_is_rejected() {
        assert!(
            parser::parse_module("fn main(console: Console):\n    yield 5\n    console.print(\"hi\")\n")
                .is_err(),
            "bare yield in a plain fn must be a parse error",
        );
        assert!(
            parser::parse_module("gen fn nums() -> Iter(Int):\n    yield 1\n    yield 2\n").is_ok(),
            "yield inside a gen fn must still parse",
        );
    }

    /// A `gen fn` lowers to a `__gen_*` helper (yield -> counter + early return)
    /// plus a wrapper calling `iter.from_gen`, and `import iter` is injected.
    #[test]
    fn gen_fn_lowers_to_helper_and_wrapper() {
        let m = parser::parse_module("gen fn nums() -> Iter(Int):\n    yield 1\n    yield 2\n")
            .expect("parse");
        let checked = witchy_syntax::source_check::check(m).expect("source check");
        let lowered = crate::generators::lower(checked).expect("lower");
        let lowered = witchy_syntax::async_lower::lower(lowered).expect("lower async");
        let lowered = witchy_syntax::records::lower_lenient(lowered)
            .expect("finish source lowering")
            .into_module();
        let fn_names: Vec<&str> = lowered
            .items
            .iter()
            .filter_map(|it| match it {
                crate::ast::Item::Function(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(fn_names.contains(&"__gen_nums"), "missing helper: {fn_names:?}");
        assert!(fn_names.contains(&"nums"), "missing wrapper: {fn_names:?}");
        assert!(lowered.imports.iter().any(|m| m == "iter"), "iter not imported");
        // No `gen fn` or `yield` survives lowering.
        assert!(lowered.items.iter().all(|it| !matches!(it, crate::ast::Item::Function(f) if f.is_gen)));
    }

    /// RFC-0046 bonus (step 5 seed): the short-circuiting `iter.any`/`iter.all`
    /// consumers — completing the combinator set — run identically on both
    /// backends. `any` stops at the first match (safe on an unbounded iterator);
    /// `all` stops at the first failure; both handle the empty iterator.
    #[test]
    fn std_iter_any_all_backends_agree() {
        let client = r#"
import iter
fn main(console: Console):
    console.print("${iter.from_list([2, 4, 6, 7]).any(fn(x: Int): x % 2 == 1)}")
    console.print("${iter.from_list([2, 4, 6]).all(fn(x: Int): x % 2 == 0)}")
    console.print("${iter.from_list([2, 4, 7]).all(fn(x: Int): x % 2 == 0)}")
    console.print("${iter.empty().any(fn(x: Int): true)}")
    console.print("${iter.empty().all(fn(x: Int): false)}")
    // any short-circuits on an unbounded iterator once a match exists
    console.print("${iter.count_from(1).any(fn(n: Int): n > 100)}")
"#;
        let sources = [("iter", crate::bundled_module("iter").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std/iter any/all diverged");
        assert_eq!(compiled, vec!["true", "true", "false", "false", "true", "true"]);
    }
