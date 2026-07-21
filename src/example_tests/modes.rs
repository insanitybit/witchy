use super::*;
use crate::{codegen, parser};

    /// `mode opt` parses into `Module.modes`; any other word (including the former
    /// `strict` synonym) is a parse error — `opt` is the only performance mode.
    #[test]
    fn mode_directive_parses() {
        let m = parser::parse_module("mode opt\n\nfn main(console: Console):\n    console.print(\"hi\")\n")
            .expect("parse");
        assert_eq!(m.modes, vec!["opt".to_string()]);
        assert!(parser::parse_module("mode strict\n\nfn main():\n    nil\n").is_err());
        assert!(parser::parse_module("mode turbo\n\nfn main():\n    nil\n").is_err());
        // `mode` stays usable as an ordinary identifier (contextual keyword).
        assert!(parser::parse_module("fn main(console: Console):\n    let mode = 3\n    console.print(\"${mode}\")\n").is_ok());
    }

    /// `mode opt` is transitive: an `opt` module may import the std library
    /// (exempt) and other `opt` modules, but importing a non-`opt` user module is
    /// a link error.
    #[test]
    fn opt_mode_propagates_across_imports() {
        let opt_main = parser::parse_module(
            "mode opt\nimport helper\n\nfn main(console: Console):\n    console.print(\"${helper.double(21)}\")\n",
        ).expect("parse main");
        let opt_helper = parser::parse_module("mode opt\n\npub fn double(n: Int) -> Int:\n    n + n\n")
            .expect("parse opt helper");
        let plain_helper = parser::parse_module("pub fn double(n: Int) -> Int:\n    n + n\n")
            .expect("parse plain helper");

        // opt main + opt helper links.
        crate::pipeline::link(
            vec![("main".into(), opt_main.clone()), ("helper".into(), opt_helper)],
            "main",
        ).expect("opt importing opt links");

        // opt main + NON-opt helper is rejected, naming both modules.
        let err = crate::pipeline::link(
            vec![("main".into(), opt_main), ("helper".into(), plain_helper)],
            "main",
        ).map(|_| ()).expect_err("opt importing non-opt must fail");
        assert!(
            err.message.contains("not `mode opt`") && err.message.contains("helper"),
            "{}", err.message,
        );

        // Importing the bundled std library from an opt module is exempt.
        let opt_std = parser::parse_module(
            "mode opt\nimport list\n\nfn main(console: Console):\n    console.print(\"${list.length([1, 2, 3])}\")\n",
        ).expect("parse opt+std");
        crate::pipeline::link(vec![("main".into(), opt_std)], "main").expect("opt importing std is exempt");
    }

    /// In a `mode opt` file, an ownership-relevant parameter (a heap buffer) must
    /// carry an explicit `let`/`var`/`own` convention; scalars and capabilities are
    /// exempt; an ordinary file is never enforced.
    #[test]
    fn mode_requires_ownership_conventions() {
        let unannotated = "mode opt\n\nfn tag(xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    console.print(\"${tag([1, 2, 3])}\")\n";
        let err = crate::enforce_performance_modes(&link_mode(unannotated), "t")
            .expect_err("unannotated List param must be rejected in a mode file");
        assert!(err.contains("ownership convention"), "{err}");

        // The same code with `let` is accepted.
        let annotated = unannotated.replace("fn tag(xs:", "fn tag(let xs:");
        crate::enforce_performance_modes(&link_mode(&annotated), "t").expect("annotated param passes");

        // A scalar param needs no annotation even in a mode file.
        let scalar = "mode opt\n\nfn twice(n: Int) -> Int:\n    n + n\n\nfn main(console: Console):\n    console.print(\"${twice(3)}\")\n";
        crate::enforce_performance_modes(&link_mode(scalar), "t").expect("scalar param is exempt");

        // Bare capability values are authority tokens, not heap buffers; adding
        // `let`/`own`/`var` to them would be pure annotation noise. Keep this
        // list behind the shared capability predicate so new caps don't drift.
        let caps = "mode opt\n\nfn use_caps(console: Console, clock: Clock, rand: Rand, env: Env, exec: Exec, dir: Dir, file: File, net: Net, secret: Secret, store: SecretStore, sock: Socket, listener: Listener) -> Int:\n    1\n\nfn main(console: Console):\n    console.print(\"${1}\")\n";
        crate::enforce_performance_modes(&link_mode(caps), "t").expect("bare capabilities are exempt");

        // An aggregate that carries a capability is still a heap value; the
        // convention matters for the aggregate even though the bare cap is exempt.
        let cap_aggregate = "mode opt\n\nfn keep(maybe: Option(Secret)) -> Int:\n    1\n\nfn main(console: Console):\n    console.print(\"${1}\")\n";
        let err = crate::enforce_performance_modes(&link_mode(cap_aggregate), "t")
            .expect_err("cap-carrying aggregate still needs an ownership convention");
        assert!(err.contains("ownership convention") && err.contains("maybe"), "{err}");

        // Without a mode directive, the unannotated param is fine.
        let plain = unannotated.replacen("mode opt\n\n", "", 1);
        crate::enforce_performance_modes(&link_mode(&plain), "t").expect("non-mode file is not enforced");
    }

    /// In a mode file, an accumulator that reverts to the copying path inside a
    /// loop (a `Cliff`) is a hard error; in an ordinary file the same shape is
    /// accepted silently.
    #[test]
    fn mode_rejects_accumulator_cliff() {
        let cliff = "mode opt\n\nfn main(console: Console):\n    var xs = []\n    var snaps = []\n    for i in [1, 2, 3]:\n        list.push(snaps, xs)\n        list.push(xs, i)\n    console.print(\"${list.length(xs)}\")\n";
        let err = crate::enforce_performance_modes(&link_mode(cliff), "t")
            .expect_err("a repeated copy-revert in a mode file must be rejected");
        assert!(err.contains("rebuilt by copy"), "{err}");

        // The same body without the mode directive is accepted silently.
        let plain = cliff.replacen("mode opt\n\n", "", 1);
        crate::enforce_performance_modes(&link_mode(&plain), "t").expect("non-mode file is accepted");
    }

    /// A clean `mode opt` program — properly annotated, accumulator stays
    /// in-place — passes enforcement and runs.
    #[test]
    fn clean_mode_program_passes_and_runs() {
        let src = "mode opt\n\nfn main(console: Console):\n    var xs = []\n    for i in [1, 2, 3]:\n        list.push(xs, i)\n    console.print(\"${list.length(xs)}\")\n";
        crate::enforce_performance_modes(&link_mode(src), "t").expect("clean mode program passes");
        assert_eq!(link_run(src), vec!["3"]);
    }

    /// THE FORCED-COPY DIFFERENTIAL: `WITCHY_OPT=-inplace` compiles with the
    /// in-place machinery off (the copying paths ARE the semantics). Outputs
    /// must be identical — any divergence is an analysis soundness bug.
    #[test]
    fn forced_copy_mode_is_differential() {
        let src = "fn tag(let prefix: String, n: Int) -> String:\n    prefix + \"${n}\"\n\nfn main(console: Console):\n    var xs = []\n    let alias = xs\n    var s = \"\"\n    var d = dict.new()\n    var i = 0\n    while i < 800:\n        list.push(xs, i)\n        s = s + tag(\"x\", i)\n        dict.update(d, i % 7, 0, fn(n: Int): n + 1)\n        i = i + 1\n    console.print(\"${list.length(xs)}\")\n    console.print(\"${list.length(alias)}\")\n    console.print(\"${s.length()}\")\n    console.print(\"${dict.get_or(d, 3, 0)}\")\n";
        let optimized = wasm_run(src);
        codegen::set_force_copy_for_tests(Some(true));
        let forced = wasm_run(src);
        codegen::set_force_copy_for_tests(None);
        assert_eq!(optimized, forced, "forced-copy output must match the optimized build");
        assert_eq!(link_run(src), optimized, "and both must match the interpreter");
    }

    /// RFC-0030 DIFFERENTIAL DE-OPT SWEEP: a program's output must be identical
    /// under every `WITCHY_OPT` setting — `none`, `all`, the production default,
    /// and the default with each optimization individually removed — and must
    /// match the interpreter oracle. Toggling an optimization changes *how* a
    /// program runs, never *what* it computes; any divergence is a soundness bug
    /// in that optimization. As optimizations join the registry they are covered
    /// here automatically (the loop walks `Opt::ALL`).
    #[test]
    fn witchy_opt_sweep_is_differential() {
        use crate::opt::{self, Opt, OptSet};
        let src = "fn tag(let prefix: String, n: Int) -> String:\n    prefix + \"${n}\"\n\nfn main(console: Console):\n    var xs = []\n    let alias = xs\n    var s = \"\"\n    var d = dict.new()\n    var i = 0\n    while i < 600:\n        list.push(xs, i)\n        s = s + tag(\"x\", i)\n        dict.update(d, i % 7, 0, fn(n: Int): n + 1)\n        i = i + 1\n    console.print(\"${list.length(xs)}\")\n    console.print(\"${list.length(alias)}\")\n    console.print(\"${s.length()}\")\n    console.print(\"${dict.get_or(d, 3, 0)}\")\n";
        let oracle = link_run(src);

        let mut settings: Vec<(String, OptSet)> = vec![
            ("none".into(), OptSet::none()),
            ("all".into(), OptSet::all()),
            ("default".into(), OptSet::default_set()),
        ];
        for o in Opt::ALL {
            settings.push((format!("-{}", o.name()), OptSet::default_set().without(o)));
        }
        for (label, set) in settings {
            opt::set_for_tests(Some(set));
            let out = wasm_run(src);
            opt::set_for_tests(None);
            assert_eq!(out, oracle, "WITCHY_OPT={label} diverged from the interpreter oracle");
        }
    }
