use super::*;
use crate::{interpreter, parser};

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

    /// The selected match phase carries its pattern payload directly. Mutating
    /// the original scrutinee before suspension cannot change the resumed arm.
    #[test]
    fn generator_match_binding_survives_scrutinee_mutation_without_replay() {
        let src = r#"import iter

gen fn values(console: Console) -> Iter(Int):
    var current: Option(Int) = Some(9)
    while true:
        match current:
            Some(value) ->
                current = None
                console.print("before ${value}")
                yield value
                console.print("after ${value}")
            None -> return

fn main(console: Console):
    let result: List(Int) = iter.collect(values(console))
    console.print("${result}")
"#;
        let expected = ["before 9", "after 9", "[9]"];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled Wasm",
        );
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

