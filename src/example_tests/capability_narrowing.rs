use crate::typeck;

    fn ok(src: &str) {
        let result = typeck::check_str(src);
        assert!(result.is_ok(), "expected ok, got: {result:?}");
    }

    fn err(src: &str, needle: &str) {
        let error = typeck::check_str(src).expect_err("expected a type error");
        assert!(error.contains(needle), "error `{error}` should mention `{needle}`");
    }

    /// `list` needs `Read` and `make_dir` needs `Write`: a missing right is a
    /// compile error. Their confinement (an escape or a link leaf is `Denied`)
    /// runs as admitted Wasm in `capabilities::dir_capability_compiles_to_wasm_and_confines`
    /// and at the host in `dir_write_family_answers_failures_as_statuses`.
    #[test]
    fn dir_list_and_make_dir_are_rights_checked() {
        assert!(typeck::check_str("fn main(c: Console, d: Dir[Write]):\n    let n = d.list()\n").is_err());
        assert!(typeck::check_str("fn main(c: Console, d: Dir[Read]):\n    let _ = d.make_dir(\"x\")\n").is_err());
        assert!(typeck::check_str("fn main(c: Console, d: Dir[Write]):\n    let _ = d.make_dir(\"x\")\n").is_ok());
    }

    /// The `as` ascription narrows a capability to a subset of its rights, and is
    /// the single native mechanism for it (replacing the per-right `_only`
    /// builtins). It can only *drop* rights — never widen or cross capabilities.
    #[test]
    fn as_ascription_narrows_to_subsets_only() {
        // Narrowing along each axis, and an idempotent re-ascription, type-check.
        ok("fn main(c: Console, net: Net, root: Dir):\n    let a = net as Net[Connect]\n    let b = net as Net[Listen, Tcp]\n    let d = root as Dir[Read]\n    let e = (net as Net[Connect]) as Net[Connect]\n");
        // Re-widening (`Net[Connect]` back to full `Net`) is rejected.
        err(
            "fn main(c: Console, net: Net):\n    let w = (net as Net[Connect]) as Net\n",
            "`as` can only drop rights",
        );
        // `as` cannot cross capabilities (a `Net` is not a `Dir`).
        err(
            "fn main(c: Console, net: Net):\n    let x = net as Dir[Read]\n",
            "cannot ascribe",
        );
        // The retired narrowing builtins are gone — calling one is unknown.
        err(
            "fn main(c: Console, net: Net):\n    let x = connect_only(net)\n",
            "unknown function `connect_only`",
        );
    }

    /// Implicit directional narrowing wherever a value flows into a capability-
    /// typed slot — call arguments, return types, constructor fields, and actor
    /// spawn fields: a broader capability satisfies a narrower one (a full `Net`
    /// flows into a `Net[Connect]`) without an explicit `as`. The callee stays
    /// type-bounded to its declared rights, so widening is rejected everywhere.
    #[test]
    fn implicit_narrowing_at_call_boundaries() {
        // A full `Net`/`Dir` coerces into a narrowed parameter — no `as` needed.
        ok("fn fetch(n: Net[Connect]) -> Socket:\n    n.connect(\"a:1\")\nfn main(c: Console, net: Net):\n    let s = fetch(net)\n");
        ok("fn dial(n: Net[Connect, Tcp]) -> Socket:\n    n.connect(\"a:1\")\nfn main(c: Console, net: Net):\n    let s = dial(net)\n");
        ok("fn load(d: Dir[Read]) -> String:\n    d.read(\"f\") ?? \"\"\nfn main(c: Console, root: Dir):\n    let x = load(root)\n");
        // The type ceiling holds: a `Net[Connect]` cannot be re-widened to satisfy
        // a full-`Net` parameter (soundness — no laundering authority back up).
        err(
            "fn g(m: Net):\n    let l = m.listen(\"b:2\")\nfn f(n: Net[Connect]):\n    g(n)\nfn main(c: Console, net: Net):\n    f(net)\n",
            "expected `Net`, found `Net[Connect]`",
        );
        // A too-narrow argument is still rejected (Connect cannot satisfy Listen).
        err(
            "fn serve(n: Net[Listen]):\n    let l = n.listen(\"b:2\")\nfn main(c: Console, net: Net):\n    serve(net as Net[Connect])\n",
            "expected `Net[Listen]`, found `Net[Connect]`",
        );

        // The same directional narrowing holds wherever a value flows into a
        // capability-typed slot, not just call arguments:
        // (a) a return type — return a full `Net` where `Net[Connect]` is declared,
        ok("fn client(net: Net) -> Net[Connect]:\n    net\nfn main(c: Console, net: Net):\n    let s = client(net).connect(\"a:1\")\n");
        // (b) a constructor field that holds a narrowed capability.
        ok("type Client:\n    Client(Net[Connect])\nfn main(c: Console, net: Net):\n    let x = Client(net)\n");
        // Both still reject *widening* (the type ceiling holds at every position).
        err(
            "fn bad(n: Net[Connect]) -> Net:\n    n\nfn main(c: Console, net: Net):\n    bad(net as Net[Connect])\n",
            "expected `Net`, found `Net[Connect]`",
        );
        err(
            "capability Server:\n    net: Net\nfn make(n: Net[Connect]) -> Server:\n    Server(n)\nfn main(c: Console, net: Net):\n    make(net as Net[Connect])\n",
            "expected `Net`, found `Net[Connect]`",
        );
    }
