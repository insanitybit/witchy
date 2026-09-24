use super::*;
use crate::{interpreter, parser, typeck};

    /// (BUG-310/BUG-311) Channel close is quiescence-based, not sender-refcount
    /// based. A parked recv resumes as `None` when every live task is parked; a
    /// retained sender may still send later. Likewise a bounded parked send and a
    /// parked join are released by the close pass. This pins the shipped executor
    /// contract the docs describe.
    #[test]
    fn channel_quiescence_close_contract_backends_agree() {
        if !is_nightly() {
            return;
        }
        let recv_then_send = "import chan\n\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(0).await\n    let r1 = chan.recv(rx).await\n    console.print(\"${r1}\")\n    chan.send(tx, 42).await\n    let r2 = chan.recv(rx).await\n    console.print(\"${r2}\")\n";
        let recv_expected = ["None", "Some(42)"];
        assert_eq!(link_run(recv_then_send), recv_expected, "interp recv quiescence");
        assert_eq!(
            run_linked_on_wasm(&[("main", recv_then_send)], "main"),
            recv_expected,
            "wasm recv quiescence",
        );

        let bounded_release = "import chan\n\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(1).await\n    chan.send(tx, 1).await\n    chan.send(tx, 2).await\n    let a = chan.recv(rx).await\n    let b = chan.recv(rx).await\n    console.print(\"${a}\")\n    console.print(\"${b}\")\n";
        let bounded_expected = ["Some(1)", "Some(2)"];
        assert_eq!(link_run(bounded_release), bounded_expected, "interp bounded release");
        assert_eq!(
            run_linked_on_wasm(&[("main", bounded_release)], "main"),
            bounded_expected,
            "wasm bounded release",
        );

        let join_release = "import chan\nfrom chan import Sender\n\nasync fn producer(console: Console, tx: Sender(Int)) -> ():\n    chan.send(tx, 1).await\n    chan.send(tx, 2).await\n    console.print(\"producer finished\")\n\nasync fn main(console: Console):\n    let (tx, _rx) = chan.channel(1).await\n    let h = chan.spawn(producer(console, tx)).await\n    chan.join(h).await\n    console.print(\"join returned\")\n";
        let join_expected = ["join returned", "producer finished"];
        assert_eq!(link_run(join_release), join_expected, "interp join release");
        assert_eq!(
            run_linked_on_wasm(&[("main", join_release)], "main"),
            join_expected,
            "wasm join release",
        );
    }

    /// (BUG-396) Structured channel helpers sequence multi-handle lists without
    /// routing join/cancel through the recursive generic `for_each` helper.
    #[test]
    fn channel_structured_join_cancel_indexed_fanouts_backends_agree() {
        let cases = [
            (
                "scope",
                "import chan\nimport list\n\nasync fn noop(_n: Int) -> ():\n    return\n\nasync fn main(console: Console):\n    let items = list.range(8)\n    chan.scope(list.map(items, fn(n): noop(n))).await\n    console.print(\"scoped ${list.length(items)}\")\n",
                ["scoped 8"],
            ),
            (
                "race_n",
                "import chan\nimport list\nimport option\n\nasync fn value(n: Int) -> Int:\n    n\n\nasync fn main(console: Console):\n    let items = list.range(8)\n    let raced = chan.race_n(list.map(items, fn(n): value(n))).await\n    let winner = option.unwrap_or(raced, 0 - 1)\n    console.print(\"${winner}\")\n",
                ["0"],
            ),
        ];
        for (label, src, expected) in cases {
            assert_eq!(link_run(src), expected, "interp: {label}");
            assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: {label}");
        }
    }

    /// (BUG-007) The trait-method edge is rejected LOUDLY at parse time rather than
    /// half-supported: the current trait machinery can't express a `gen`/`async`
    /// method as a trait method (async's inferred phantom-`Task` return has no
    /// declarable trait signature; a `gen` impl emits a helper the trait can't
    /// name). A `gen`/`async` method is supported only in an inherent `impl Type:`.
    #[test]
    fn gen_async_trait_methods_are_rejected() {
        // `gen`/`async` in a trait DECLARATION.
        let trait_decl = "trait Seq:\n    gen fn items(self) -> Iter(Int)\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let err = parser::parse_module(trait_decl).expect_err("gen trait method must be rejected");
        assert!(format!("{err:?}").contains("`gen`/`async` trait method"), "{err:?}");

        // A `gen`/`async` method IMPLEMENTING a trait method (an `impl Trait for T`).
        let impl_gen = "trait Seq:\n    fn items(self) -> Iter(Int)\n\ntype Nums:\n    n: Int\n\nimpl Seq for Nums:\n    gen fn items(self) -> Iter(Int):\n        yield self.n\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let err = parser::parse_module(impl_gen).expect_err("gen trait-impl method must be rejected");
        assert!(format!("{err:?}").contains("cannot implement a trait method"), "{err:?}");

        let impl_async = "trait Fetcher:\n    fn go(self, x: Int) -> Int\n\ntype Api:\n    base: Int\n\nimpl Fetcher for Api:\n    async fn go(self, x: Int) -> Int:\n        self.base + x\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let err = parser::parse_module(impl_async).expect_err("async trait-impl method must be rejected");
        assert!(format!("{err:?}").contains("cannot implement a trait method"), "{err:?}");

        // The inherent form (no `for`) is ACCEPTED — the supported case.
        let inherent = "import iter\n\ntype Nums:\n    n: Int\n\nimpl Nums:\n    gen fn items(self) -> Iter(Int):\n        yield self.n\n\nfn main(console: Console):\n    let xs: List(Int) = iter.collect(Nums(7).items())\n    console.print(\"${xs}\")\n";
        assert_eq!(link_run(inherent), ["[7]"], "inherent gen method is supported");
    }

    /// (BUG-429) Async lowering runs before type checking, so it must not erase
    /// tail-position `region:` blocks. Until async preserves region copy-out
    /// semantics, these shapes are rejected before flattening.
    #[test]
    fn async_tail_region_blocks_are_rejected_before_lowering() {
        for body in [
            "region -> String:\n        \"x\"",
            "return region -> String:\n        \"x\"",
            "if true:\n        region -> String:\n            \"x\"\n    else:\n        \"y\"",
        ] {
            let src = format!(
                "async fn build() -> String:\n    {body}\n\nfn main(console: Console):\n    console.print(\"ok\")\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let err = crate::pipeline::link(vec![("main".into(), module)], "main")
                .expect_err("async tail region must be rejected before lowering erases it");
            assert!(
                err.message.contains("region") && err.message.contains("async tail"),
                "diagnostic should name the async tail region limitation, got: {}",
                err.message
            );
        }
    }

