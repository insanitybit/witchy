use super::*;
use crate::{interpreter, parser};

    /// The common single-yield loop is a one-pass owned suspension frame on the
    /// compiled backend: each pull resumes from `(a, b)` instead of replaying all
    /// prior yields. The syntax-level lowering test pins the `iter.unfold` ABI;
    /// this fixture proves that ABI through linking, checking, and compiled Wasm.
    #[test]
    fn generator_owned_loop_frame_runs_on_both_backends() {
        let src = "import iter\n\ngen fn fibs() -> Iter(Int):\n    var a: Int = 0\n    var b: Int = 1\n    while true:\n        yield a\n        let next: Int = a + b\n        a = b\n        b = next\n\nfn main(console: Console):\n    let xs: List(Int) = iter.collect(fibs().take(10))\n    console.print(\"${xs}\")\n";
        let expected = ["[0, 1, 1, 2, 3, 5, 8, 13, 21, 34]"];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
    }

    /// The owned frame suspends at the yield itself. An effect after a yield is
    /// delayed until the next pull, executes once, and is never run merely
    /// because a short-circuiting consumer drops the remaining iterator.
    #[test]
    fn generator_owned_frame_delays_post_yield_effect_until_resume() {
        let src = "import iter\n\ngen fn values(console: Console) -> Iter(Int):\n    var i: Int = 0\n    while i < 3:\n        yield i\n        console.print(\"after ${i}\")\n        i = i + 1\n\nfn main(console: Console):\n    match iter.next(values(console)):\n        Empty -> console.print(\"one empty\")\n        Item(one, _rest) -> console.print(\"one ${one}\")\n    match iter.next(values(console)):\n        Empty -> console.print(\"two empty\")\n        Item(first, rest) ->\n            match iter.next(rest):\n                Empty -> console.print(\"two short\")\n                Item(second, _more) -> console.print(\"two ${first} ${second}\")\n";
        let expected = ["one 0", "after 0", "two 0 1"];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
    }

    /// The flagship Collatz generator yields once before its terminal loop.
    /// The seed expression's effect proves that later pulls resume the frame
    /// instead of replaying that accepted prefix.
    #[test]
    fn generator_collatz_prefix_yield_effect_runs_exactly_once_on_both_backends() {
        let src = r#"import iter

fn observed_seed(console: Console, value: Int) -> Int:
    console.print("seed ${value}")
    value

gen fn collatz(console: Console, start: Int) -> Iter(Int):
    var n = start
    yield observed_seed(console, n)
    while n > 1:
        if n % 2 == 0:
            n = n / 2
        else:
            n = 3 * n + 1
        yield n

fn main(console: Console):
    let values: List(Int) = iter.collect(collatz(console, 6))
    console.print("${values}")
"#;
        let expected = ["seed 6", "[6, 3, 10, 5, 16, 8, 4, 2, 1]"];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
    }

    /// Finite owned frames remain lazy, carry locals introduced between yields,
    /// infer ordinary arithmetic state, and cannot capture source identifiers
    /// that deliberately resemble compiler-generated names.
    #[test]
    fn generator_finite_frame_is_lazy_hygienic_and_carries_phase_locals() {
        let src = r#"import iter

fn mark(console: Console) -> Int:
    console.print("init")
    1

gen fn values(
    console: Console,
    once: Bool,
    resume_after_yield: Int,
    frame: Int,
) -> Iter(Int):
    let marked: Int = mark(console)
    var seed = marked + resume_after_yield + frame
    yield seed
    let x: Int = 2
    yield x
    yield x + 1

fn main(console: Console):
    let unused = values(console, false, 6, 1)
    console.print("made")
    let result: List(Int) = iter.collect(values(console, false, 6, 1))
    console.print("${result}")
"#;
        let expected = ["made", "init", "[8, 2, 3]"];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
    }

    /// A list `for` generator evaluates its iterable on the first pull, carries
    /// the current index in its frame, and executes each iteration effect once.
    #[test]
    fn generator_for_frame_is_lazy_and_does_not_replay_iteration_effects() {
        let src = r#"import iter

fn observed_values(console: Console) -> List(Int):
    console.print("source")
    [1, 2, 3]

gen fn values(console: Console) -> Iter(Int):
    for value in observed_values(console):
        console.print("effect ${value}")
        yield value * 10

fn main(console: Console):
    let unused = values(console)
    console.print("made")
    let result: List(Int) = iter.collect(values(console))
    console.print("${result}")
"#;
        let expected = [
            "made",
            "source",
            "effect 1",
            "effect 2",
            "effect 3",
            "[10, 20, 30]",
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
    }

    /// Destructured loop locals that survive a yield are ordinary typed frame
    /// fields; resumption neither loses their bindings nor reruns the prefix.
    #[test]
    fn generator_tuple_pattern_local_survives_yield_without_replay() {
        let src = r#"import iter

gen fn values(console: Console) -> Iter(Int):
    var running = true
    while running:
        let (first, second) = (1, 2)
        console.print("before")
        yield first
        console.print("between ${second}")
        running = false
        yield second

fn main(console: Console):
    let result: List(Int) = iter.collect(values(console))
    console.print("${result}")
"#;
        let expected = ["before", "between 2", "[1, 2]"];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
    }

    /// A local introduced in the taken yielding branch is captured with that
    /// branch's resume phase, so its suffix runs once with the original value.
    #[test]
    fn generator_conditional_branch_local_survives_yield_without_replay() {
        let src = r#"import iter

gen fn values(console: Console) -> Iter(Int):
    var running = true
    while running:
        if running:
            let value = 7
            console.print("before ${value}")
            yield value
            console.print("after ${value}")
            running = false

fn main(console: Console):
    let result: List(Int) = iter.collect(values(console))
    console.print("${result}")
"#;
        let expected = ["before 7", "after 7", "[7]"];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
    }

    /// Direct yield sites in one loop are distinct resume states. Each segment
    /// runs once, and the tail after the final yield runs before the next loop
    /// iteration begins.
    #[test]
    fn generator_direct_multi_yield_loop_resumes_each_segment_once() {
        let src = "import iter\n\ngen fn staged(console: Console) -> Iter(Int):\n    var i: Int = 0\n    while i < 2:\n        console.print(\"before ${i}\")\n        yield i\n        console.print(\"middle ${i}\")\n        yield i + 10\n        console.print(\"after ${i}\")\n        i = i + 1\n\nfn main(console: Console):\n    match iter.next(staged(console)):\n        Empty -> console.print(\"missing-one\")\n        Item(one, rest_one) ->\n            console.print(\"one ${one}\")\n            match iter.next(rest_one):\n                Empty -> console.print(\"missing-two\")\n                Item(two, rest_two) ->\n                    console.print(\"two ${two}\")\n                    match iter.next(rest_two):\n                        Empty -> console.print(\"missing-three\")\n                        Item(three, _rest_three) -> console.print(\"three ${three}\")\n";
        let expected = [
            "before 0",
            "one 0",
            "middle 0",
            "two 10",
            "after 0",
            "before 1",
            "three 1",
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
    }

    /// A conditional yield resumes inside the branch that suspended. Its
    /// condition and prefix effects are not replayed, while iterations that do
    /// not yield continue inside the same pull.
    #[test]
    fn generator_conditional_yield_resumes_once_on_both_backends() {
        let src = "import iter\n\ngen fn evens(console: Console) -> Iter(Int):\n    var i: Int = 0\n    while i < 4:\n        console.print(\"scan ${i}\")\n        if i % 2 == 0:\n            console.print(\"before ${i}\")\n            yield i\n            console.print(\"after ${i}\")\n        else:\n            console.print(\"skip ${i}\")\n        i = i + 1\n\nfn main(console: Console):\n    let xs: List(Int) = iter.collect(evens(console))\n    console.print(\"${xs}\")\n";
        let expected = [
            "scan 0",
            "before 0",
            "after 0",
            "scan 1",
            "skip 1",
            "scan 2",
            "before 2",
            "after 2",
            "scan 3",
            "skip 3",
            "[0, 2]",
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
    }

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

    /// A finite `gen fn` lowers to an owned `__gen_*` resume helper plus an
    /// `iter.unfold` entry wrapper, and `import iter` is injected.
    #[test]
    fn gen_fn_lowers_to_helper_and_wrapper() {
        let m = parser::parse_module("gen fn nums() -> Iter(Int):\n    yield 1\n    yield 2\n")
            .expect("parse");
        let checked = witchy_syntax::source_check::check(m).expect("source check");
        let lowered = witchy_syntax::generators::lower(checked).expect("lower");
        let lowered_debug = format!("{:?}", lowered.module());
        let wrapper = lowered
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::Function(function) if function.name == "nums" => Some(function),
                _ => None,
            })
            .expect("finite generator entry wrapper");
        let helper = lowered
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::Function(function) if function.name == "__gen_nums" => {
                    Some(function)
                }
                _ => None,
            })
            .expect("finite generator resume helper");
        assert!(wrapper
            .attributes
            .iter()
            .any(|attribute| attribute == witchy_syntax::suspension::FRAME_ENTRY_ATTRIBUTE));
        assert!(helper
            .attributes
            .iter()
            .any(|attribute| attribute == witchy_syntax::suspension::FRAME_FUNCTION_ATTRIBUTE));
        assert!([wrapper, helper].iter().all(|function| !function
            .attributes
            .iter()
            .any(|attribute| attribute == witchy_syntax::suspension::FRAME_BOXED_ATTRIBUTE)));
        assert!(lowered_debug.contains("iter.unfold"), "missing owned iterator entry: {lowered_debug}");
        assert!(!lowered_debug.contains("iter.from_gen"), "finite generator selected replay: {lowered_debug}");
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
