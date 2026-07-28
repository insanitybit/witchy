use crate::{interpreter, parser, typeck};

    fn ok(src: &str) {
        let result = typeck::check_str(src);
        assert!(result.is_ok(), "expected ok, got: {result:?}");
    }

    fn err(src: &str, needle: &str) {
        let error = typeck::check_str(src).expect_err("expected a type error");
        assert!(error.contains(needle), "error `{error}` should mention `{needle}`");
    }

    #[test]
    fn dir_write_is_confined_to_the_subtree() {
        let tmp = std::env::temp_dir().join(format!("witchy_dir_write_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let run = |src: &str| {
            let mods = vec![("main".to_string(), parser::parse_module(src).expect("parse"))];
            let linked = crate::pipeline::link(mods, "main").expect("link");
            interpreter::run_module(linked, &tmp, Vec::new())
        };
        // Write then read back, within the confined Dir.
        let out = run("fn main(console: Console, root: Dir):\n    root.write(\"out.txt\", \"hi\")\n    console.print(root.read(\"out.txt\"))\n")
            .expect("run");
        assert_eq!(out, vec!["hi"]);
        assert_eq!(std::fs::read_to_string(tmp.join("out.txt")).unwrap(), "hi");
        // A `..` write is refused — the capability can't escape its subtree.
        assert!(run("fn main(console: Console, root: Dir):\n    root.write(\"../escape.txt\", \"x\")\n").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `list` (enumerate, sorted) and `make_dir` (create a confined subdir) — the
    /// filesystem ops a package store/registry needs. `list` needs `Read`,
    /// `make_dir` needs `Write`, and both stay confined to the capability's subtree.
    #[test]
    fn dir_list_and_make_dir_work_and_are_rights_checked() {
        let tmp = std::env::temp_dir().join(format!("witchy_dir_list_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("store")).unwrap();
        std::fs::write(tmp.join("store/bravo"), "b").unwrap();
        std::fs::write(tmp.join("store/alpha"), "a").unwrap();
        let run = |src: &str| {
            let mods = vec![("main".to_string(), parser::parse_module(src).expect("parse"))];
            let linked = crate::pipeline::link(mods, "main").expect("link");
            interpreter::run_module(linked, &tmp, Vec::new())
        };
        // `list` enumerates a subdir's entries in sorted (deterministic) order.
        let out = run("fn main(console: Console, root: Dir):\n    console.print(list.join(root.subtree(\"store\").list(), \",\"))\n")
            .expect("run");
        assert_eq!(out, vec!["alpha,bravo"]);
        // `make_dir` creates a confined subdirectory.
        run("fn main(console: Console, root: Dir):\n    root.make_dir(\"fresh\")\n").expect("run");
        assert!(tmp.join("fresh").is_dir(), "make_dir should have created the directory");
        // Confinement: a `..` make_dir is refused.
        assert!(run("fn main(console: Console, root: Dir):\n    root.make_dir(\"../escaped\")\n").is_err());
        assert!(!tmp.parent().unwrap().join("escaped").exists(), "make_dir must not escape the subtree");

        // Rights: `list` needs Read, `make_dir` needs Write.
        assert!(typeck::check_str("fn main(c: Console, d: Dir[Write]):\n    let n = d.list()\n").is_err());
        assert!(typeck::check_str("fn main(c: Console, d: Dir[Read]):\n    d.make_dir(\"x\")\n").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn dir_write_refuses_a_symlink_leaf() {
        // A pre-existing symlink in the subtree must not let a write escape it.
        let base = std::env::temp_dir().join(format!("witchy_dir_symlink_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sandbox")).unwrap();
        std::fs::write(base.join("secret.txt"), "ORIGINAL").unwrap();
        std::os::unix::fs::symlink("../secret.txt", base.join("sandbox/link.txt")).unwrap();

        let mods = vec![(
            "main".to_string(),
            parser::parse_module(
                "fn main(console: Console, root: Dir):\n    root.subtree(\"sandbox\").write(\"link.txt\", \"PWNED\")\n",
            )
            .expect("parse"),
        )];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        assert!(interpreter::run_module(linked, &base, Vec::new()).is_err());
        // The symlink target outside the subtree is untouched.
        assert_eq!(std::fs::read_to_string(base.join("secret.txt")).unwrap(), "ORIGINAL");
        let _ = std::fs::remove_dir_all(&base);
    }


    /// `as` narrowing is the identity at runtime (rights live only in the type),
    /// so a narrowed handle still reads the same confined subtree.
    #[test]
    fn as_narrowing_is_identity_at_runtime() {
        let tmp = std::env::temp_dir().join(format!("witchy_dir_as_narrow_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("in.txt"), "narrowed").unwrap();
        let src = "fn main(console: Console, root: Dir):\n    let r = root as Dir[Read]\n    console.print(r.read(\"in.txt\"))\n";
        let mods = vec![("main".to_string(), parser::parse_module(src).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        let out = interpreter::run_module(linked, &tmp, Vec::new()).expect("run");
        assert_eq!(out, vec!["narrowed"]);
        let _ = std::fs::remove_dir_all(&tmp);
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
        ok("fn load(d: Dir[Read]) -> String:\n    d.read(\"f\")\nfn main(c: Console, root: Dir):\n    let x = load(root)\n");
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
