    use crate::{ast, codegen, interpreter, parser, typeck};
    use wasmtime::{Engine, Module};

    fn validates_wasm_gc(bytes: &[u8]) -> bool {
        let features = wasmparser::WasmFeatures::default()
            | wasmparser::WasmFeatures::GC
            | wasmparser::WasmFeatures::REFERENCE_TYPES
            | wasmparser::WasmFeatures::FUNCTION_REFERENCES;
        wasmparser::Validator::new_with_features(features)
            .validate_all(bytes)
            .is_ok()
    }

    fn wasm_gc_engine() -> Engine {
        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        Engine::new(&config).expect("Wasm GC engine")
    }

    fn interp(src: &str) -> Vec<String> {
        link_run(src)
    }

    /// Link a single-`main` source (pulling in any imported std module) and run
    /// it on the interpreter — the path that resolves `import crypto`.
    fn link_run(src: &str) -> Vec<String> {
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        interpreter::run_module(linked, ".", Vec::new()).expect("run")
    }

    /// Resolve `src`'s `import`s against the bundled std and link the whole set —
    /// the source-level analog of the CLI's `link_file` / the lib's
    /// `resolve_std_only`. The COMPILED backend needs every reached std function
    /// present (only builtins are inlined), so a single-module `linker::link` is
    /// insufficient; the interpreter (`link_run`) tolerates it because it resolves
    /// std at run time, but `compile_module_binary` does not.
    fn resolve_std_src(src: &str) -> ast::Module {
        use std::collections::{HashSet, VecDeque};
        let entry = parser::parse_module(src).expect("parse");
        let mut modules: Vec<(String, ast::Module)> = vec![("main".to_string(), entry.clone())];
        let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
        let mut queue: VecDeque<ast::Module> = VecDeque::from([entry]);
        while let Some(module) = queue.pop_front() {
            for name in module.imports.clone() {
                if !loaded.insert(name.clone()) {
                    continue;
                }
                let source = crate::bundled_module(&name).expect("a bundled std module");
                let parsed = parser::parse_module(source).expect("parse std module");
                queue.push_back(parsed.clone());
                modules.push((name, parsed));
            }
        }
        crate::pipeline::link(modules, "main").expect("link")
    }

    /// Like [`resolve_std_src`] but RETURNS the link result (its error message on
    /// failure) instead of panicking — for tests that assert a link-time rejection
    /// (e.g. RFC-0065 sealed-construction). Parsing still panics (the source must
    /// be syntactically valid); only the final link may legitimately fail.
    fn try_link_std(src: &str) -> Result<ast::Module, String> {
        use std::collections::{HashSet, VecDeque};
        let entry = parser::parse_module(src).expect("parse");
        let mut modules: Vec<(String, ast::Module)> = vec![("main".to_string(), entry.clone())];
        let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
        let mut queue: VecDeque<ast::Module> = VecDeque::from([entry]);
        while let Some(module) = queue.pop_front() {
            for name in module.imports.clone() {
                if !loaded.insert(name.clone()) {
                    continue;
                }
                let source = crate::bundled_module(&name).expect("a bundled std module");
                let parsed = parser::parse_module(source).expect("parse std module");
                queue.push_back(parsed.clone());
                modules.push((name, parsed));
            }
        }
        crate::pipeline::link(modules, "main").map_err(|e| e.message)
    }

    /// Link a single source as the entry module `t`, for performance-mode tests.
    fn link_mode(src: &str) -> ast::Module {
        let module = parser::parse_module(src).expect("parse");
        crate::pipeline::link(vec![("t".into(), module)], "t").expect("link")
    }

    /// Every shipped example's entry module: the source file named by its
    /// `[rune].name` manifest field (the one bearing `main`).
    /// Skips `examples/projects/` (multi-rune workspaces, covered by the pm tests)
    /// and each rune's `*_test.witchy` modules and helper modules.
    fn example_entries() -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir("examples").expect("examples directory") {
            let dir = entry.expect("dir entry").path();
            if !dir.is_dir() {
                continue;
            }
            let Some(dir_name) = dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if dir_name == "projects" {
                continue;
            }
            let manifest_path = dir.join("witchy.toml");
            let module_name = if manifest_path.exists() {
                let source = std::fs::read_to_string(&manifest_path).expect("read example manifest");
                let manifest: ::toml::Value = ::toml::from_str(&source).expect("parse example manifest");
                manifest
                    .get("rune")
                    .and_then(|rune| rune.get("name"))
                    .and_then(::toml::Value::as_str)
                    .expect("example manifest has [rune].name")
                    .to_string()
            } else {
                dir_name.to_string()
            };
            let entry_file = dir.join("src").join(format!("{module_name}.witchy"));
            if manifest_path.exists() {
                assert!(entry_file.exists(), "example manifest entry missing: {}", entry_file.display());
                out.push(entry_file);
            } else if entry_file.exists() {
                out.push(entry_file);
            }
        }
        out.sort();
        out
    }

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

    #[test]
    fn public_sources_do_not_call_legacy_render_intrinsic() {
        fn collect(root: &std::path::Path, suffix: &str, out: &mut Vec<std::path::PathBuf>) {
            if root.is_file() {
                if root.to_string_lossy().ends_with(suffix) {
                    out.push(root.to_path_buf());
                }
                return;
            }
            for entry in std::fs::read_dir(root).unwrap_or_else(|_| panic!("read {}", root.display())) {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    collect(&path, suffix, out);
                } else if path.to_string_lossy().ends_with(suffix) {
                    out.push(path);
                }
            }
        }

        let mut paths = Vec::new();
        for root in ["std", "examples"] {
            collect(std::path::Path::new(root), ".witchy", &mut paths);
        }
        for root in ["README.md", "book", "spec", "rfcs/performance-modes.md"] {
            collect(std::path::Path::new(root), ".md", &mut paths);
        }
        paths.sort();

        let mut offenders = Vec::new();
        for path in paths {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("read {}", path.display()));
            if text.contains("__render(") {
                offenders.push(path.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "public source/docs must use interpolation or show.render, not __render:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn generic_type_aliases_resolve_on_linked_path() {
        // BUG-563: parameterized aliases were accepted at the declaration site
        // but every `Pair(Int)` use reached type resolution as an unknown type.
        let src = "type Pair(a) = (a, a)\ntype Rows(a) = List(Pair(a))\n\nfn first(p: Pair(Int)) -> Int:\n    p.0\n\nfn main(console: Console):\n    let rows: Rows(String) = [(\"a\", \"b\")]\n    console.print(\"${first((1, 2))}:${list.length(rows)}\")\n";
        assert_eq!(link_run(src), vec!["1:1"]);
    }

    /// The bundled stdlib must stay on the in-place fast path: a performance cliff
    /// (an accumulator that reverts to copy-per-iteration, i.e. O(n²)) anywhere in
    /// std silently slows every program that touches it. This applies `mode opt`'s
    /// cliff detection to ALL 42 std modules as a regression guard — the same check
    /// that turned `string.chars`/`json.decode` from O(n²) into O(n). Adding a
    /// growing-buffer loop to a std module without keeping it on the fast path fails
    /// HERE, loudly, with the function and the offending accumulator.
    #[test]
    fn stdlib_has_no_performance_cliffs() {
        // Link a synthetic entry that imports every std module, so cross-module call
        // summaries resolve exactly as in real compilation (a per-module scan would
        // false-positive on calls like `list.join` whose summary lives elsewhere).
        let imports: String =
            crate::linker::STD_MODULES.iter().map(|m| format!("import {m}\n")).collect();
        let entry_src = format!("{imports}\npub fn perfcheck() -> Int:\n    0\n");
        let entry = parser::parse_module(&entry_src).expect("parse synthetic std-import entry");
        let linked = crate::pipeline::link(vec![("perfcheck".into(), entry)], "perfcheck")
            .expect("link all std modules");
        // The whole stdlib is cliff-free: every "build a sub-list, then collect it into a
        // result list" shape (`out = push(out, move cur); cur = []`) transfers ownership with
        // `move`, so the sub-list's per-element pushes stay in place (the `move`-resets-cap fix
        // makes that sound). No allowlist — a new cliff is a hard failure.
        let offenders: Vec<String> = crate::analysis::module_cliffs(&linked)
            .into_iter()
            .map(|(func, c)| {
                format!("{func} (line {}): `{}` is rebuilt by copy each iteration — {}", c.line, c.var, c.reason)
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "stdlib performance cliffs (O(n²) accumulation) found:\n  {}",
            offenders.join("\n  ")
        );
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

    /// (BUG-007) An `async fn` declared as a METHOD of an inherent `impl` lowers in
    /// place, staying a method that returns a `Task` — so `d.scaled(5).await` drives
    /// it through the executor. Here the method itself `await`s a top-level async fn,
    /// exercising the CPS lowering inside a method body. Both backends agree.
    #[test]
    fn async_method_in_impl_backends_agree() {
        let src = "type Doubler:\n    base: Int\n\nasync fn step(n: Int) -> Int:\n    n + n\n\nimpl Doubler:\n    async fn scaled(self, x: Int) -> Int:\n        let doubled = step(x).await\n        self.base + doubled\n\nasync fn main(console: Console):\n    let d = Doubler(100)\n    let r = d.scaled(5).await\n    console.print(\"${r}\")\n";
        let expected = ["110"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-312) Async lowering runs before typeck, so synthesized wrapper blocks
    /// must preserve the source line of the statement they wrap. Otherwise type
    /// errors inside an async body lose the normal `fn`, line N prefix.
    #[test]
    fn async_lowered_type_errors_keep_source_locations() {
        let before_await = "import list\nimport chan\n\nasync fn work(console: Console) -> Nil:\n    var xs: List(Int) = []\n    list.push(xs, \"bad\")\n    chan.yield_now().await\n    return\n\nasync fn main(console: Console):\n    work(console).await\n";
        let err = typeck::check(&resolve_std_src(before_await))
            .expect_err("async type error before await must be rejected")
            .to_string();
        assert!(err.contains("`main.work`, line 6:"), "async diagnostic lost location: {err}");
        assert!(err.contains("expected `Int`, found `String`"), "{err}");

        let after_await = "import list\nimport chan\n\nasync fn work(console: Console) -> Nil:\n    var xs: List(Int) = []\n    chan.yield_now().await\n    list.push(xs, \"bad\")\n    return\n\nasync fn main(console: Console):\n    work(console).await\n";
        let err = typeck::check(&resolve_std_src(after_await))
            .expect_err("async type error after await must be rejected")
            .to_string();
        assert!(err.contains("`async_work_"), "continuation diagnostic must name segment: {err}");
        assert!(err.contains("`, line 7:"), "continuation diagnostic lost source line: {err}");
        assert!(err.contains("expected `Int`, found `String`"), "{err}");
    }

    /// (BUG-310/BUG-311) Channel close is quiescence-based, not sender-refcount
    /// based. A parked recv resumes as `None` when every live task is parked; a
    /// retained sender may still send later. Likewise a bounded parked send and a
    /// parked join are released by the close pass. This pins the shipped executor
    /// contract the docs describe.
    #[test]
    fn channel_quiescence_close_contract_backends_agree() {
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

        let join_release = "import chan\nfrom chan import Sender\n\nasync fn producer(console: Console, tx: Sender(Int)) -> Nil:\n    chan.send(tx, 1).await\n    chan.send(tx, 2).await\n    console.print(\"producer finished\")\n\nasync fn main(console: Console):\n    let (tx, _rx) = chan.channel(1).await\n    let h = chan.spawn(producer(console, tx)).await\n    chan.join(h).await\n    console.print(\"join returned\")\n";
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
                "import chan\nimport list\n\nasync fn noop(_n: Int) -> Nil:\n    return\n\nasync fn main(console: Console):\n    let items = list.range(40)\n    chan.scope(list.map(items, fn(n): noop(n))).await\n    console.print(\"scoped ${list.length(items)}\")\n",
                ["scoped 40"],
            ),
            (
                "race_n",
                "import chan\nimport list\nimport option\n\nasync fn value(n: Int) -> Int:\n    n\n\nasync fn main(console: Console):\n    let items = list.range(40)\n    let raced = chan.race_n(list.map(items, fn(n): value(n))).await\n    let winner = option.unwrap_or(raced, 0 - 1)\n    console.print(\"${winner}\")\n",
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

    /// (RFC-0032) `vm.par_map(xs, f)` maps a capture-free function over a list. On the
    /// interpreter it is the sequential oracle; on the compiled backend it runs across
    /// OS-thread VMs. Because results are collected by input index and `f` is pure, the
    /// two backends produce identical output (parity by determinism).
    #[test]
    fn vm_par_map_backends_agree() {
        let src = "import vm\n\nfn dbl(n: Int) -> Int:\n    n * 2\n\nfn main(console: Console):\n    let prior: List(fn(Int) -> Int) = [fn(n: Int): n + 1]\n    console.print(\"${list.at(prior, 0)(6)}\")\n    let ys = vm.par_map([1, 2, 3, 4, 5], dbl)\n    console.print(\"${ys}\")\n    console.print(\"${list.length(ys)}\")\n";
        let expected = ["7", "[2, 4, 6, 8, 10]", "5"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-414) `vm.par_map` takes its worker-VM fast path only for a bare
    /// top-level function. A local function value or lambda deliberately uses the
    /// byte-identical sequential body; unlike the isolation APIs, that changes
    /// performance rather than semantics or authority.
    #[test]
    fn vm_par_map_indirect_callbacks_fall_back() {
        // par_map with the function in a LOCAL and as an inline LAMBDA -> fallback.
        let par = "import vm\n\nfn dbl(n: Int) -> Int:\n    n * 2\n\nfn main(console: Console):\n    let f = dbl\n    console.print(\"${vm.par_map([1, 2, 3], f)}\")\n    console.print(\"${vm.par_map([4, 5], fn(n: Int): n + 1)}\")\n";
        let par_expected = ["[2, 4, 6]", "[5, 6]"];
        assert_eq!(link_run(par), par_expected, "interp par_map indirect");
        assert_eq!(run_linked_on_wasm(&[("main", par)], "main"), par_expected, "wasm par_map indirect");

        // A bare TOP-LEVEL function still takes the fast path and agrees.
        let direct = "import vm\n\nfn dbl(n: Int) -> Int:\n    n * 2\n\nfn main(console: Console):\n    console.print(\"${vm.par_map([1, 2, 3], dbl)}\")\n";
        assert_eq!(link_run(direct), ["[2, 4, 6]"], "interp direct");
        assert_eq!(run_linked_on_wasm(&[("main", direct)], "main"), ["[2, 4, 6]"], "wasm direct");
    }

    /// RFC-0050 Part 1: ambient builtin types whose API home is a std module are
    /// method-capable through that owner. Bytes and Duration were the motivating
    /// holes in the old hardcoded UFCS allowlist.
    #[test]
    fn rfc0050_builtin_type_owners_backends_agree() {
        let src = "import bytes\nimport duration\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hello\")\n    console.print(\"${b.length()} ${b.slice(1, 4).to_string()}\")\n    let d = duration.seconds(3661)\n    console.print(\"${d.to_seconds()} ${d.abs().to_seconds()}\")\n";
        let expected = ["5 ell", "3661 3661"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-508) `string.split_once_opt` / `rsplit_once_opt` preserve the
    /// missing-separator bit that the legacy tuple helpers erase.
    #[test]
    fn string_split_once_option_helpers_backends_agree() {
        let src = "\nfn show(console: Console, label: String, p: Option((String, String))):\n    match p:\n        Some(parts) ->\n            let (a, b) = parts\n            console.print(label + \"=Some(\" + a + \"|\" + b + \")\")\n        None -> console.print(label + \"=None\")\n\nfn main(console: Console):\n    show(console, \"missing\", \"host\".split_once_opt(\":\"))\n    show(console, \"present-empty-right\", \"host:\".split_once_opt(\":\"))\n    show(console, \"present-empty-left\", \":name\".split_once_opt(\":\"))\n    show(console, \"last\", \"a.b.c\".rsplit_once_opt(\".\"))\n    show(console, \"last-missing\", \"name\".rsplit_once_opt(\".\"))\n    let (a, b) = \"host\".split_once(\":\")\n    console.print(\"old-first=\" + a + \"|\" + b)\n    let (c, d) = \"name\".rsplit_once(\".\")\n    console.print(\"old-last=\" + c + \"|\" + d)\n";
        let expected = [
            "missing=None",
            "present-empty-right=Some(host|)",
            "present-empty-left=Some(|name)",
            "last=Some(a.b|c)",
            "last-missing=None",
            "old-first=host|",
            "old-last=|name",
        ];
        assert_eq!(link_run(src), expected, "interp: split_once_opt preserves absence");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: split_once_opt preserves absence",
        );
    }

    /// (BUG-231) Tail slices after a matched prefix are character-indexed. Code
    /// must derive the start from `char_count`, not `length`'s UTF-8 byte count,
    /// or non-ASCII keys skip past the value.
    #[test]
    fn string_tail_slices_use_character_counts_on_both_backends() {
        let src = "\nfn value_after_eq(kv: String, name: String) -> String:\n    if kv.starts_with(name + \"=\"):\n        kv.drop(name.char_count() + 1)\n    else:\n        \"\"\n\nfn main(console: Console):\n    console.print(value_after_eq(\"naïve=x\", \"naïve\"))\n    console.print(\"éclair\".drop(1))\n";
        let expected = ["x", "clair"];
        assert_eq!(link_run(src), expected, "interp: char-count tail slice");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: char-count tail slice",
        );
    }

    /// `string.join` is the module-symmetric inverse of `string.split`, while
    /// the existing `list.join`/`parts.join` spellings remain valid.
    #[test]
    fn string_join_alias_backends_agree() {
        let src = "\nfn main(console: Console):\n    let parts = \"a,b,c\".split(\",\")\n    console.print(parts.join(\"-\"))\n    console.print(parts.join(\"|\"))\n    console.print([].join(\",\"))\n";
        let expected = ["a-b-c", "a|b|c", ""];
        assert_eq!(link_run(src), expected, "interp: string.join");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: string.join",
        );
    }

    /// RFC-0050 Part 1: for ordinary module-scoped types, method ownership is
    /// derived from the canonical `module.Type` name, so package/user modules get
    /// receiver-first methods without being listed in the compiler.
    #[test]
    fn rfc0050_user_type_owner_methods_backends_agree() {
        let matrix = "type Matrix:\n    Matrix(Int)\n\npub fn value(m: Matrix) -> Int:\n    match m:\n        Matrix(n) -> n\n\npub fn shifted(m: Matrix, delta: Int) -> Matrix:\n    match m:\n        Matrix(n) -> Matrix(n + delta)\n\nfn secret(m: Matrix) -> Int:\n    99\n";
        let main = "import matrix\n\nfn main(console: Console):\n    let m = matrix.Matrix(40)\n    console.print(\"${m.value()} ${m.shifted(2).value()}\")\n";
        let sources = [("matrix", matrix), ("main", main)];
        let expected = vec!["40 42".to_string()];
        assert_eq!(interpreter::run_program(&sources, "main").expect("interp"), expected);
        assert_eq!(run_linked_on_wasm(&sources, "main"), expected, "wasm");

        let bad_main = "import matrix\n\nfn main(console: Console):\n    let m = matrix.Matrix(1)\n    console.print(\"${m.secret()}\")\n";
        let linked = crate::pipeline::link(
            vec![
                ("matrix".to_string(), parser::parse_module(matrix).expect("parse matrix")),
                ("main".to_string(), parser::parse_module(bad_main).expect("parse main")),
            ],
            "main",
        )
        .expect("link");
        let err = typeck::check(&linked).expect_err("private owner helper is not a method").message;
        assert!(err.contains("no method `secret`"), "got: {err}");
    }

    /// (BUG-305, parity) `"${f}"` on a function value is rejected at CHECK time, so
    /// BOTH backends refuse it identically. The interpreter used to render
    /// `<function/N>` while the compiled backend rejected at codegen with a misleading
    /// "generic record such as `Set`" diagnostic (there was no Set). A function has no
    /// printable form; the message now names the function operand, never `Set`.
    #[test]
    fn interpolating_a_function_value_is_rejected_on_both_backends() {
        let src = "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    console.print(\"${f}\")\n";
        let err = typeck::check_str(src)
            .expect_err("interpolating a function value must be a type error on both backends");
        assert!(err.contains("function"), "diagnostic must name the function operand: {err}");
        assert!(!err.contains("Set"), "diagnostic must not mention `Set` for a function operand: {err}");
        // Calling the function and interpolating the RESULT still renders on both.
        let ok = "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    console.print(\"${f(41)}\")\n";
        assert_eq!(link_run(ok), ["42"], "interp renders the call result");
        assert_eq!(
            run_linked_on_wasm(&[("main", ok)], "main"),
            ["42"],
            "compiled renders the call result",
        );
    }


    /// (RFC-0053, parity) Interpolation (`"${x}"`) honors a CUSTOM `Show` impl, exactly
    /// as `say` does — the typed lowering rewrites generated render to `show(x)` when
    /// x's type has a public `Show` model. Primitive-derived values may print the
    /// same bytes as the structural fallback, but they still share the `Show` path
    /// when `show` is linked. Both backends must agree byte-for-byte.
    #[test]
    fn rfc0053_interpolation_honors_custom_show_on_both_backends() {
        let src = "import show\nimport duration\n\ntype P:\n    P(Int)\n\nimpl Show for P:\n    fn show(self) -> String:\n        match self:\n            P(n) -> \"P<${n}>\"\n\ntype Q derive(Show):\n    Q(Int)\n\nfn main(console: Console):\n    console.print(\"${P(5)}\")\n    console.print(\"${[P(1), P(2)]}\")\n    console.print(\"${90000ms}\")\n    console.print(\"${Q(7)}\")\n    console.print(\"${42}\")\n";
        // custom Show honored; container recurses; Duration -> human; primitive
        // derived Show remains constructor-shaped by its generated implementation.
        let expected = ["P<5>", "[P<1>, P<2>]", "1m30s", "Q(7)", "42"];
        assert_eq!(link_run(src), expected, "interp: interpolation honors custom Show");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: interpolation must honor custom Show identically",
        );
    }

    /// RFC-0046 residual cleanup: constructor-pattern bindings retain their
    /// substituted generic field types. A bounded trait call may dispatch directly
    /// on `Some(value)`, `Ok(value)`, or `Err(error)` without routing the binding
    /// through a parameter/loop-shaped helper to recover its type.
    #[test]
    fn generic_match_bindings_dispatch_bounded_traits_on_both_backends() {
        let src = "import reflect\nimport show\nfrom reflect import Mirror\n\ntype P derive(Reflect):\n    value: Int\n\nimpl Show for P:\n    fn show(self) -> String:\n        \"<${self.value}>\"\n\nfn show_option(value: Option(a)) -> String where a: Show:\n    match value:\n        Some(inner) -> show(inner)\n        None -> \"none\"\n\nfn show_result(value: Result(a, e)) -> String where a: Show, e: Show:\n    match value:\n        Ok(inner) -> show(inner)\n        Err(error) -> show(error)\n\nfn reflect_option(value: Option(a)) -> Mirror where a: Reflect:\n    match value:\n        Some(inner) -> MVariant(\"Option\", \"Some\", [reflect(inner)])\n        None -> MVariant(\"Option\", \"None\", [])\n\nfn reflect_result(value: Result(a, e)) -> Mirror where a: Reflect, e: Reflect:\n    match value:\n        Ok(inner) -> MVariant(\"Result\", \"Ok\", [reflect(inner)])\n        Err(error) -> MVariant(\"Result\", \"Err\", [reflect(error)])\n\nfn describe(value: Mirror) -> String:\n    match value:\n        MVariant(owner, variant, payload) -> \"${owner}.${variant}:${list.length(payload)}\"\n        _ -> \"other\"\n\nfn main(console: Console):\n    let ok: Result(P, P) = Ok(P(2))\n    let err: Result(P, P) = Err(P(3))\n    let nested_ok: Result(List(P), List(P)) = Ok([P(5)])\n    let nested_err: Result(List(P), List(P)) = Err([P(7)])\n    console.print(show_option(Some(P(1))))\n    console.print(show_result(ok))\n    console.print(show_result(err))\n    console.print(show_option(Some([P(4)])))\n    console.print(show_result(nested_ok))\n    console.print(describe(reflect_option(Some(P(4)))))\n    console.print(describe(reflect_result(err)))\n    console.print(describe(reflect_option(Some([P(6)]))))\n    console.print(describe(reflect_result(nested_err)))\n    console.print(show.render(Some(P(8))))\n    let std_ok: Result(P, P) = Ok(P(9))\n    let std_err: Result(P, P) = Err(P(10))\n    console.print(show.render(std_ok))\n    console.print(show.render(std_err))\n    console.print(describe(reflect.reflect_option(Some(P(11)))))\n    let std_reflect: Result(P, P) = Err(P(12))\n    console.print(describe(reflect.reflect_result(std_reflect)))\n";
        let expected = [
            "<1>",
            "<2>",
            "<3>",
            "[<4>]",
            "[<5>]",
            "Option.Some:1",
            "Result.Err:1",
            "Option.Some:1",
            "Result.Err:1",
            "Some(<8>)",
            "Ok(<9>)",
            "Err(<10>)",
            "Option.Some:1",
            "Result.Err:1",
        ];
        assert_eq!(link_run(src), expected, "interp: bounded dispatch on generic match bindings");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: bounded dispatch on generic match bindings",
        );
    }

    /// (RFC-0053, D5) f-strings are not a second rendering mechanism. They lower
    /// to the same interpolation path, so they honor `Show` for custom values,
    /// containers, and std domain/scalar display.
    #[test]
    fn rfc0053_f_strings_honor_show_on_both_backends() {
        let src = "import show\nimport duration\n\ntype P:\n    P(Int)\n\nimpl Show for P:\n    fn show(self) -> String:\n        match self:\n            P(n) -> \"P<${n}>\"\n\nfn main(console: Console):\n    console.print(f\"p={P(5)} xs={[P(1), P(2)]} d={90000ms}\")\n";
        let expected = ["p=P<5> xs=[P<1>, P<2>] d=1m30s"];
        assert_eq!(link_run(src), expected, "interp: f-strings honor Show");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: f-strings honor Show",
        );
    }

    /// RFC-0053's interpolation flip is import-independent. `show` is preluded,
    /// so interpolation, `show.render`, and `show.say` agree on Duration's human
    /// form with or without a redundant explicit import.
    #[test]
    fn rfc0053_duration_interpolation_is_import_independent_on_both_backends() {
        let without_import = "fn main(console: Console):\n    console.print(\"${90000ms}\")\n";
        let expected = ["1m30s"];
        assert_eq!(link_run(without_import), expected, "interp: prelude Duration interpolation");
        assert_eq!(
            run_linked_on_wasm(&[("main", without_import)], "main"),
            expected,
            "compiled: prelude Duration interpolation",
        );

        let with_show = "import show\nimport duration\n\nfn main(console: Console):\n    console.print(\"${90000ms}\")\n    console.print(show.render(90000ms))\n    show.say(console, 90000ms)\n";
        let show_expected = ["1m30s", "1m30s", "1m30s"];
        assert_eq!(link_run(with_show), show_expected, "interp: Duration interpolation honors Show");
        assert_eq!(
            run_linked_on_wasm(&[("main", with_show)], "main"),
            show_expected,
            "compiled: Duration interpolation honors Show",
        );
    }

    /// (BUG-326) Whole-valued Float rendering keeps a Float marker without using
    /// Rust's exact fixed-point expansion for large magnitudes. This shared
    /// formatter feeds interpolation, `show`, JSON reflection, the interpreter,
    /// and compiled wasm.
    #[test]
    fn whole_float_rendering_uses_shortest_round_trip_on_both_backends() {
        let src = "import show\nimport json\nimport reflect\n\ntype Reading derive(Reflect):\n    value: Float\n\nfn main(console: Console):\n    let big = 1234567890123456789.0\n    console.print(\"${big}\")\n    console.print(show.render(big))\n    console.print(json.stringify(Reading(big)))\n";
        let expected = [
            "1.2345678901234568e18",
            "1.2345678901234568e18",
            "{\"value\":1.2345678901234568e18}",
        ];
        assert_eq!(link_run(src), expected, "interp: whole Float rendering");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: whole Float rendering",
        );
    }

    /// (RFC-0053, coherence) Generic container `Show` impls are part of the same
    /// rendering model as concrete custom impls. In particular, `Set(Int)` has a
    /// structural fallback (`Set([1, 2])`) but a public display form (`{1, 2}`), so
    /// interpolation, `show.render`, and `show.say` must always agree.
    #[test]
    fn rfc0053_interpolation_matches_show_for_generic_containers_on_both_backends() {
        let with_show = "import set\nimport show\n\ntype P:\n    P(Int)\n\nimpl Show for P:\n    fn show(self) -> String:\n        match self:\n            P(n) -> \"P<${n}>\"\n\nfn main(console: Console):\n    let s = set.from_list([1, 1, 2, 3])\n    console.print(\"${s}\")\n    console.print(show.render(s))\n    show.say(console, s)\n    console.print(\"${[s]}\")\n    let ps = [P(1), P(2)]\n    console.print(\"${ps}\")\n    console.print(show.render(ps))\n";
        let expected = [
            "{1, 2, 3}",
            "{1, 2, 3}",
            "{1, 2, 3}",
            "[{1, 2, 3}]",
            "[P<1>, P<2>]",
            "[P<1>, P<2>]",
        ];
        assert_eq!(link_run(with_show), expected, "interp: interpolation matches show.render/say");
        assert_eq!(
            run_linked_on_wasm(&[("main", with_show)], "main"),
            expected,
            "compiled: interpolation matches show.render/say",
        );

        let no_import = "import set\n\nfn main(console: Console):\n    let s = set.from_list([1, 1, 2, 3])\n    console.print(\"${s}\")\n";
        let public_display = ["{1, 2, 3}"];
        assert_eq!(link_run(no_import), public_display, "interp: prelude Show renders Set");
        assert_eq!(
            run_linked_on_wasm(&[("main", no_import)], "main"),
            public_display,
            "compiled: prelude Show renders Set",
        );
    }

    /// (RFC-0053, coherence) `derive(Show)` is not a second rendering protocol.
    /// Its generated body renders fields through `Show`, so interpolation must
    /// agree with `show.say` for derived values containing custom-Show fields and
    /// for containers of those derived values.
    #[test]
    fn rfc0053_derived_show_fields_use_show_in_interpolation_on_both_backends() {
        let src = "import show\n\ntype Label:\n    Label(String)\n\nimpl Show for Label:\n    fn show(self) -> String:\n        match self:\n            Label(s) -> \"<\" + s + \">\"\n\ntype Box derive(Show):\n    label: Label\n\nfn main(console: Console):\n    let b = Box(Label(\"x\"))\n    console.print(\"${Label(\"x\")}\")\n    show.say(console, Label(\"x\"))\n    console.print(\"${b}\")\n    show.say(console, b)\n    console.print(\"${[b]}\")\n    show.say(console, [b])\n";
        let expected = ["<x>", "<x>", "Box(<x>)", "Box(<x>)", "[Box(<x>)]", "[Box(<x>)]"];
        assert_eq!(
            link_run(src),
            expected,
            "interp: derived Show fields must use field Show impls",
        );
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: derived Show fields must use field Show impls",
        );
    }

    /// (BUG-321) Dict keys are any concrete `Eq` value, not just scalar slots.
    /// The compiled backend dispatches key equality through the same structural
    /// helpers as `==`, including String fields and enum payloads.
    #[test]
    fn compound_dict_keys_work_on_both_backends() {
        let src = "import cmp\nimport dict\n\ntype Pair derive(PartialEq, Eq):\n    id: Int\n    label: String\n\ntype Tag derive(PartialEq, Eq):\n    ById(Int)\n    ByName(String)\n\nfn main(console: Console):\n    var records = dict.new()\n    dict.insert(records, Pair(100000, \"a\"), 10)\n    dict.insert(records, Pair(100000, \"a\"), 20)\n    console.print(\"${dict.get_or(records, Pair(100000, \"a\"), 0)}\")\n    console.print(\"${dict.contains_key(records, Pair(100000, \"a\"))}\")\n    console.print(\"${dict.length(records)}\")\n\n    var tags = dict.new()\n    dict.insert(tags, ById(7), \"id\")\n    dict.insert(tags, ByName(\"x\"), \"name\")\n    dict.insert(tags, ById(7), \"id2\")\n    console.print(dict.get_or(tags, ById(7), \"missing\"))\n    console.print(dict.get_or(tags, ByName(\"x\"), \"missing\"))\n    console.print(\"${dict.length(tags)}\")\n";
        let expected = ["20", "true", "1", "id2", "name", "2"];
        assert_eq!(link_run(src), expected, "interp: compound dict keys");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: compound dict keys",
        );
    }

    /// (BUG-317/BUG-336) Dict subscripts are real read places, not a list-only
    /// parser desugar. That makes direct reads, compound assignment, and nested
    /// place assignment through a Dict agree on both backends.
    #[test]
    fn dict_subscript_read_and_nested_place_assignment_work_on_both_backends() {
        let src = "import dict\n\nfn main(console: Console):\n    var counts: Dict(String, Int) = dict.new()\n    counts[\"a\"] = 1\n    counts[\"a\"] += 2\n    console.print(\"${counts[\"a\"]}\")\n\n    var nested: Dict(String, Dict(String, Int)) = dict.new()\n    var inner: Dict(String, Int) = dict.new()\n    dict.insert(inner, \"inner\", 1)\n    nested[\"outer\"] = inner\n    nested[\"outer\"][\"inner\"] = 7\n    console.print(\"${nested[\"outer\"][\"inner\"]}\")\n\n    var rows: Dict(String, List(Int)) = dict.new()\n    rows[\"r\"] = [1, 2, 3]\n    rows[\"r\"][1] = 9\n    console.print(\"${rows[\"r\"]}\")\n";
        let expected = ["3", "7", "[1, 9, 3]"];
        assert_eq!(link_run(src), expected, "interp: dict subscript read");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: dict subscript read",
        );
    }

    /// Field-contained containers mutated through a `var` record parameter keep
    /// their headers intact across write-back, including the interpolation reads
    /// that previously exposed stale lengths. This pins both the free-function
    /// shape and the `var self` method shape needed by the stdlib method sweep.
    #[test]
    fn var_record_container_field_updates_keep_headers_on_both_backends() {
        let list_free = "type Buf:\n    items: List(Int)\n\nfn add(var b: Buf, x: Int):\n    list.push(b.items, x)\n\nfn main(console: Console):\n    var b = Buf([])\n    var i = 0\n    while i < 16:\n        add(b, i)\n        i = i + 1\n    console.print(\"${list.at(b.items, 15)}\")\n    console.print(\"${list.length(b.items)}\")\n";
        let list_method = "type Buf:\n    items: List(Int)\n\nimpl Buf:\n    fn add(var self, x: Int) -> Nil:\n        list.push(self.items, x)\n        return\n\nfn main(console: Console):\n    var b = Buf([])\n    var i = 0\n    while i < 16:\n        b.add(i)\n        i = i + 1\n    console.print(\"${list.at(b.items, 15)}\")\n    console.print(\"${list.length(b.items)}\")\n";
        let dict_field = "import dict\n\ntype Tally:\n    counts: Dict(Int, Int)\n\nfn bump(var t: Tally, k: Int):\n    dict.insert(t.counts, k, k * 2)\n\nfn main(console: Console):\n    var t = Tally(dict.new())\n    var i = 0\n    while i < 50:\n        bump(t, i)\n        i = i + 1\n    console.print(\"${dict.get_or(t.counts, 49, 0)}\")\n    console.print(\"${dict.length(t.counts)}\")\n";

        for (label, src, expected) in [
            ("list free function", list_free, vec!["15", "16"]),
            ("list var self method", list_method, vec!["15", "16"]),
            ("dict free function", dict_field, vec!["98", "50"]),
        ] {
            assert_eq!(link_run(src), expected, "interp: {label}");
            assert_eq!(
                run_linked_on_wasm(&[("main", src)], "main"),
                expected,
                "compiled: {label}",
            );
        }
    }

    /// (RFC-0074) Lists have the same remove-by-value affordance as set/dict,
    /// with first-occurrence semantics; all-occurrences removal remains `filter`.
    #[test]
    fn list_remove_removes_first_occurrence_on_both_backends() {
        let src = "import list\n\nfn main(console: Console):\n    var xs = [1, 2, 3, 2]\n    xs.remove(2)\n    console.print(\"${xs}\")\n    xs.remove(9)\n    console.print(\"${xs}\")\n    var words = [\"a\", \"b\", \"a\"]\n    let _removed = list.remove(words, \"a\")\n    console.print(\"${words}\")\n";
        let expected = ["[1, 3, 2]", "[1, 3, 2]", "[b, a]"];
        assert_eq!(link_run(src), expected, "interp: list.remove");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: list.remove",
        );
    }

    /// (RFC-0050/RFC-0074) The list operations that act like value-mutators have
    /// real inherent methods, so statement form writes back consistently instead
    /// of depending on the older free-function UFCS path.
    #[test]
    fn list_mutator_methods_write_back_on_both_backends() {
        let src = "import list\n\nfn main(console: Console):\n    var xs = [3, 1, 2]\n    xs.sort()\n    console.print(\"${xs}\")\n    xs.reverse()\n    console.print(\"${xs}\")\n    xs.set_at(1, 9)\n    console.print(\"${xs}\")\n    xs.update_at(2, fn(n: Int): n + 10)\n    console.print(\"${xs}\")\n    xs.remove(9)\n    console.print(\"${xs}\")\n    var ys = [3, 1, 2]\n    ys.sort_by(fn(x: Int, y: Int): x > y)\n    console.print(\"${ys}\")\n";
        let expected = ["[1, 2, 3]", "[3, 2, 1]", "[3, 9, 1]", "[3, 9, 11]", "[3, 11]", "[3, 2, 1]"];
        assert_eq!(link_run(src), expected, "interp: list mutator methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: list mutator methods",
        );
    }

    /// Dict and Set carry the same mutator-method rule as List: module
    /// functions remain the function-value surface, while statement-form methods
    /// on `var self` write back to the receiver.
    #[test]
    fn dict_and_set_mutator_methods_write_back_on_both_backends() {
        let src = "import dict\nimport set\n\nfn main(console: Console):\n    var d: Dict(String, Int) = dict.new()\n    d.insert(\"a\", 1)\n    d.insert(\"b\", 2)\n    d.update(\"b\", 0, fn(n: Int): n + 5)\n    d.remove(\"a\")\n    console.print(\"${dict.get_or(d, \"a\", 0)}\")\n    console.print(\"${dict.get_or(d, \"b\", 0)}\")\n    console.print(\"${dict.length(d)}\")\n\n    var s: Set(Int) = set.new()\n    s.insert(1)\n    s.insert(2)\n    s.insert(2)\n    s.remove(1)\n    console.print(\"${set.contains(s, 1)}\")\n    console.print(\"${set.contains(s, 2)}\")\n    console.print(\"${set.length(s)}\")\n";
        let expected = ["0", "7", "1", "false", "true", "1"];
        assert_eq!(link_run(src), expected, "interp: dict/set mutator methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: dict/set mutator methods",
        );
    }

    /// (RFC-0050) String's value-mutator surface is a real inherent-method API
    /// like List/Dict/Set. Module functions remain callable for function-value
    /// and explicit-module use.
    #[test]
    fn string_transform_methods_require_explicit_reassignment() {
        let src = "\nfn main(console: Console):\n    var s = \"  hello  \"\n    s = s.trim()\n    console.print(s)\n    s = s.to_upper()\n    console.print(s)\n    s = s.replace(\"HELLO\", \"hi\")\n    console.print(s)\n    s = s.pad_left(5, \"0\")\n    console.print(s)\n    s = s.pad_right(7, \"!\")\n    console.print(s)\n    s = s.center(9, \".\")\n    console.print(s)\n    s = s.strip_prefix(\".\")\n    s = s.strip_suffix(\".\")\n    console.print(s)\n    s = s.replace_first(\"hi\", \"bye\")\n    console.print(s)\n    var t = \"  edge  \"\n    t = t.trim_start()\n    console.print(t)\n    t = t.trim_end()\n    console.print(t)\n    console.print(\"  module  \".trim())\n    console.print(\"alpha alpha\".replace_first(\"alpha\", \"beta\"))\n";
        let expected = [
            "hello",
            "HELLO",
            "hi",
            "000hi",
            "000hi!!",
            ".000hi!!.",
            "000hi!!",
            "000bye!!",
            "edge  ",
            "edge",
            "module",
            "beta alpha",
        ];
        assert_eq!(link_run(src), expected, "interp: string mutator methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: string mutator methods",
        );
    }

    /// (RFC-0050) String's read/combinator surface is also a real method API,
    /// not just receiver-first module functions reached through UFCS.
    #[test]
    fn string_read_methods_work_on_both_backends() {
        let src = r#"
fn main(console: Console):
    let text = "alpha,beta,alpha"
    console.print("${text.length()}")
    console.print("${text.char_count()}")
    console.print("${text.contains("beta")}")
    console.print("${text.starts_with("alpha")}")
    console.print("${text.ends_with("omega")}")
    console.print("${text.split(",")}")
    console.print("${text.index_of("beta")}")
    console.print("${text.last_index_of("alpha")}")
    let (left, right) = text.split_once(",")
    console.print(left + "|" + right)
    let (rleft, rright) = text.rsplit_once(",")
    console.print(rleft + "|" + rright)
    console.print("${"cafe".substring(1, 3)}")
    console.print("${"ha".repeat(3)}")
    console.print("${"alpha beta".words()}")
    console.print("${"a\nb".lines()}")
    console.print("${"42".parse_int()}")
    console.print("${"x".parse_int()}")
    console.print("${"abc".char_at(1)}")
    console.print("${"abc".take(2)}|${"abc".drop(1)}|${"abc".reverse()}|${"banana".count("an")}")
"#;
        let expected = [
            "16",
            "16",
            "true",
            "true",
            "false",
            "[alpha, beta, alpha]",
            "Some(6)",
            "Some(11)",
            "alpha|beta,alpha",
            "alpha,beta|alpha",
            "af",
            "hahaha",
            "[alpha, beta]",
            "[a, b]",
            "Some(42)",
            "None",
            "Some(b)",
            "ab|bc|cba|2",
        ];
        assert_eq!(link_run(src), expected, "interp: string read methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: string read methods",
        );
    }

    /// (RFC-0050) Option and Result expose their primary combinators as real
    /// methods, while the module functions remain available as first-class
    /// helpers.
    #[test]
    fn option_and_result_methods_work_on_both_backends() {
        let src = r#"import option
import result
import show

fn main(console: Console):
    let some = Some(2)
    let none: Option(Int) = None
    console.print("${some.is_some()}|${none.is_none()}")
    console.print("${some.unwrap_or(0)}|${none.unwrap_or_else(fn(): 5)}")
    console.print(show(some.map(fn(x: Int): x + 3)))
    console.print("${some.map_or(0, fn(x: Int): x * 10)}|${none.map_or(9, fn(x: Int): x * 10)}")
    console.print(show(some.and_then(fn(x: Int): Some(x * 2))))
    console.print(show(some.filter(fn(x: Int): x > 1)) + "|" + show(some.filter(fn(x: Int): x > 3)))
    console.print(show(none.or(Some(7))))
    console.print(show(none.or_else(fn(): Some(8))))
    console.print(show(some.ok_or("missing")) + "|" + show(none.ok_or("missing")))
    let nested_opt: Option(Option(Int)) = Some(Some(4))
    console.print(show(nested_opt.flatten()))
    console.print(show(some.zip(Some("x"))))

    let ok: Result(Int, String) = Ok(4)
    let err: Result(Int, String) = Err("bad")
    console.print("${ok.is_ok()}|${err.is_err()}|${err.unwrap_err_or("none")}")
    console.print(show(ok.map_ok(fn(x: Int): x + 1)) + "|" + show(err.map_ok(fn(x: Int): x + 1)))
    console.print(show(err.map_err(fn(e: String): e + "!")))
    console.print("${ok.map_or(0, fn(x: Int): x * 2)}|${err.map_or(7, fn(x: Int): x * 2)}")
    console.print(show(ok.or(Ok(9))) + "|" + show(err.or(Ok(9))))
    console.print(show(err.or_else(fn(e: String): Ok(3))))
    console.print("${err.unwrap_or(12)}|${err.unwrap_or_else(fn(): 13)}")
    console.print(show(ok.ok()) + "|" + show(err.err()))
    let nested_res: Result(Result(Int, String), String) = Ok(Ok(11))
    console.print(show(nested_res.flatten()))
    console.print("${result.unwrap_or(option.ok_or(Some(6), "missing"), 0)}")
"#;
        let expected = [
            "true|true",
            "2|5",
            "Some(5)",
            "20|9",
            "Some(4)",
            "Some(2)|None",
            "Some(7)",
            "Some(8)",
            "Ok(2)|Err(missing)",
            "Some(4)",
            "Some((2, x))",
            "true|true|bad",
            "Ok(5)|Err(bad)",
            "Err(bad!)",
            "8|7",
            "Ok(4)|Ok(9)",
            "Ok(3)",
            "12|13",
            "Some(4)|Some(bad)",
            "Ok(11)",
            "6",
        ];
        assert_eq!(link_run(src), expected, "interp: option/result methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: option/result methods",
        );
    }

    /// (BUG-553) Container comparison protocols compose: nested `Option` and
    /// `Result` values satisfy `PartialEq`/`Eq` when their payloads do, and
    /// compiled monomorphization specializes the nested payload calls.
    #[test]
    fn nested_container_equality_satisfies_protocol_bounds_on_both_backends() {
        let src = "import cmp\nimport testing\n\ntype Key derive(Show, Eq):\n    id: Int\n    cache: Int\n\nimpl PartialEq for Key:\n    fn eq(self, other: Key) -> Bool:\n        self.id == other.id\n\nfn same(x: a, y: a) -> Bool where a: PartialEq:\n    x == y\n\nfn total_same(x: a, y: a) -> Bool where a: Eq:\n    x == y\n\nfn main(console: Console):\n    let o1: Option(List(Key)) = Some([Key(1, 10)])\n    let o2: Option(List(Key)) = Some([Key(1, 20)])\n    let r1: Result(List(Key), String) = Ok([Key(1, 10)])\n    let r2: Result(List(Key), String) = Ok([Key(1, 20)])\n    console.print(\"${same(o1, o2)}\")\n    console.print(\"${total_same(o1, o2)}\")\n    console.print(\"${same(r1, r2)}\")\n    console.print(\"${total_same(r1, r2)}\")\n    testing.assert_value_eq(o1, o2)\n    testing.assert_value_eq(r1, r2)\n";
        let expected = ["true", "true", "true", "true"];
        assert_eq!(link_run(src), expected, "interp: nested container PartialEq bounds");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: nested container PartialEq bounds",
        );
    }

    /// (BUG-546) Sealed domain values display through their public canonical
    /// formatter, not their private constructor-shaped representation.
    #[test]
    fn sealed_domain_values_use_canonical_show_on_both_backends() {
        let src = "import show\nimport semver\nimport url\nimport time\n\nfn main(console: Console):\n    let v = semver.version(1, 2, 3)\n    let d = time.from_unix(0)\n    match url.parse(\"https://example.com/p\"):\n        Ok(u) ->\n            show.say(console, v)\n            show.say(console, u)\n            show.say(console, d)\n            console.print(\"${v}\")\n            console.print(\"${u}\")\n            console.print(\"${d}\")\n            console.print(show.render([v, semver.version(2, 0, 0)]))\n            console.print(show.render(Some(u)))\n        Err(e) -> console.print(url.url_error_message(e))\n";
        let expected = [
            "1.2.3",
            "https://example.com/p",
            "1970-01-01T00:00:00Z",
            "1.2.3",
            "https://example.com/p",
            "1970-01-01T00:00:00Z",
            "[1.2.3, 2.0.0]",
            "Some(https://example.com/p)",
        ];
        assert_eq!(link_run(src), expected, "interp: sealed domain Show");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: sealed domain Show",
        );
    }

    /// (BUG-537) `DateTime`'s public constructors enforce the fixed-width
    /// RFC3339 year domain that `time.iso8601` and `time.parse_iso8601` share.
    #[test]
    fn datetime_rejects_years_outside_fixed_iso_domain_on_both_backends() {
        let src = "import time\n\nfn report(console: Console, r: Result(time.DateTime, time.TimeError)):\n    match r:\n        Ok(d) -> console.print(time.iso8601(d))\n        Err(e) -> console.print(time.time_error_message(e))\n\nfn main(console: Console):\n    report(console, time.civil(0, 1, 1, 0, 0, 0))\n    report(console, time.civil(10000, 1, 1, 0, 0, 0))\n    report(console, time.parse_iso8601(\"0000-01-01T00:00:00Z\"))\n    report(console, time.parse_iso8601(\"9999-12-31T23:59:59Z\"))\n";
        let expected = [
            "year 0 is out of range 1..9999",
            "year 10000 is out of range 1..9999",
            "year 0 is out of range 1..9999",
            "9999-12-31T23:59:59Z",
        ];
        assert_eq!(link_run(src), expected, "interp: DateTime fixed ISO domain");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: DateTime fixed ISO domain",
        );
    }

    /// (RFC-0054) `?` converts typed errors through `From`, so libraries can
    /// expose matchable enum errors without collapsing every layer to `String`.
    #[test]
    fn rfc0054_try_converts_errors_through_from_backends_agree() {
        let src = "import show\nimport error\nimport convert\n\ntype ParseError:\n    Bad(String)\n\nimpl Show for ParseError:\n    fn show(self) -> String:\n        match self:\n            Bad(s) -> \"parse:\" + s\n\nimpl Error for ParseError\n\ntype AppError:\n    Wrapped(String)\n\nimpl Show for AppError:\n    fn show(self) -> String:\n        match self:\n            Wrapped(s) -> \"app:\" + s\n\nimpl Error for AppError\n\nimpl From(ParseError) for AppError:\n    fn from(value: ParseError) -> Self:\n        match value:\n            Bad(s) -> Wrapped(\"wrapped \" + s)\n\nfn leaf() -> Result(Int, ParseError):\n    Err(Bad(\"nope\"))\n\nfn wrapper() -> Result(Int, AppError):\n    let x = leaf()?\n    Ok(x + 1)\n\nfn main(console: Console):\n    match wrapper():\n        Ok(n) -> console.print(\"${n}\")\n        Err(e) -> console.print(show.render(e))\n";
        let expected = ["app:wrapped nope"];
        assert_eq!(link_run(src), expected, "interp: From-converting ?");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: From-converting ?",
        );
    }

    #[test]
    fn rfc0054_try_rejects_missing_from_error_conversion() {
        let src = "import show\nimport error\n\ntype LeafError:\n    Leaf\n\nimpl Show for LeafError:\n    fn show(self) -> String:\n        \"leaf\"\n\nimpl Error for LeafError\n\ntype AppError:\n    App\n\nimpl Show for AppError:\n    fn show(self) -> String:\n        \"app\"\n\nimpl Error for AppError\n\nfn leaf() -> Result(Int, LeafError):\n    Err(Leaf)\n\nfn wrapper() -> Result(Int, AppError):\n    leaf()?\n";
        let err = typeck::check(&resolve_std_src(src)).expect_err("missing From conversion must reject");
        assert!(err.to_string().contains("no `From("), "{err}");
    }

    #[test]
    fn rfc0054_option_context_converts_through_string_from() {
        let src = "import show\nimport error\nimport convert\n\ntype AppError:\n    Message(String)\n\nimpl Show for AppError:\n    fn show(self) -> String:\n        match self:\n            Message(s) -> \"app:\" + s\n\nimpl Error for AppError\n\nimpl From(String) for AppError:\n    fn from(value: String) -> Self:\n        Message(value)\n\nfn find() -> Option(Int):\n    None\n\nfn wrapper() -> Result(Int, AppError):\n    let x = find()? \"missing value\"\n    Ok(x)\n\nfn main(console: Console):\n    match wrapper():\n        Ok(n) -> console.print(\"${n}\")\n        Err(e) -> console.print(show.render(e))\n";
        let expected = ["app:missing value"];
        assert_eq!(link_run(src), expected, "interp: Option ? context converts through From(String)");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: Option ? context converts through From(String)",
        );
    }

    #[test]
    fn rfc0054_plain_option_try_stays_option_scoped() {
        let src = "import show\nimport error\n\ntype AppError:\n    Missing\n\nimpl Show for AppError:\n    fn show(self) -> String:\n        \"missing\"\n\nimpl Error for AppError\n\nfn find() -> Option(Int):\n    None\n\nfn wrapper() -> Result(Int, AppError):\n    find()?\n";
        let err = typeck::check(&resolve_std_src(src)).expect_err("plain Option ? must not invent a typed error");
        assert!(err.to_string().contains("propagates from a `Option"), "{err}");
    }

    #[test]
    fn coven_maintainer_policy_string_array_is_strict() {
        let src = "import coven_trust\nimport list\n\nfn report(text: String) -> String:\n    match coven_trust.string_array(text):\n        Ok(xs) -> \"ok:\" + list.join(xs, \",\")\n        Err(e) -> \"err:\" + coven_trust.trust_policy_error_message(e)\n\nfn tag(text: String) -> String:\n    match coven_trust.string_array(text):\n        Ok(_xs) -> \"typed:ok\"\n        Err(e) ->\n            match e:\n                coven_trust.PolicyInvalidJson(_) -> \"typed:json\"\n                coven_trust.PolicyNotStringArray -> \"typed:shape\"\n                coven_trust.PolicyNonString(i) -> \"typed:item:\" + \"${i}\"\n\nfn main(console: Console):\n    console.print(report(\"[\\\"gha|alice\\\",\\\"gha|bob\\\"]\"))\n    console.print(report(\"{\\\"maintainers\\\":[]}\"))\n    console.print(report(\"[\\\"gha|alice\\\",7]\"))\n    console.print(report(\"[\"))\n    console.print(tag(\"[\\\"gha|alice\\\",7]\"))\n";
        let sources = [
            ("coven_json", include_str!("../projects/coven/src/coven_json.witchy")),
            ("coven_trust", include_str!("../projects/coven/src/coven_trust.witchy")),
            ("main", src),
        ];
        let modules: Vec<(String, ast::Module)> = sources
            .iter()
            .map(|(name, source)| ((*name).to_string(), parser::parse_module(source).expect("parse")))
            .collect();
        let linked = crate::pipeline::link(modules, "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interp");
        let wasm = run_linked_on_wasm(&sources, "main");
        assert_eq!(interp, wasm, "strict maintainer policy decode must be backend-stable");
        assert_eq!(
            wasm,
            [
                "ok:gha|alice,gha|bob",
                "err:expected a JSON array of strings",
                "err:expected a string at index 1",
                "err:unexpected end of input",
                "typed:item:1",
            ],
        );
    }

    #[test]
    fn native_compiler_intrinsics_reject_comptime_source_strings() {
        // The compiler-service natives live above the runtime kernel in
        // `witchy-interp`; install their vtable before resolving them here.
        witchy_interp::compiler_natives::install();
        use crate::value::NativeValue;

        let invalid = "fn main(console: Console):\n    missing(console)\n";
        let footprint = crate::native::lookup("compiler.footprint").expect("compiler.footprint native");
        let NativeValue::Str(json) = footprint(&[NativeValue::Str(invalid.into())]).expect("native call") else {
            panic!("compiler.footprint must return a JSON string");
        };
        assert!(json.contains("\"error\""), "type-invalid source must fail closed: {json}");
        assert!(json.contains("unknown function `missing`"), "error must include the type error: {json}");

        let src = "comptime:\n    emit(\"pub fn generated(net: Net) -> Int:\")\n    emit(\"    7\")\n";
        let NativeValue::Str(json) = footprint(&[NativeValue::Str(src.into())]).expect("native call") else {
            panic!("compiler.footprint must return a JSON string");
        };
        assert!(json.contains("\"error\""), "comptime source must fail closed: {json}");
        assert!(json.contains("does not support comptime"), "error must name the boundary: {json}");

        let diff = crate::native::lookup("compiler.diff").expect("compiler.diff native");
        let NativeValue::Str(json) = diff(&[
            NativeValue::Str("pub fn direct() -> Int:\n    0\n".into()),
            NativeValue::Str(invalid.into()),
        ])
        .expect("native diff")
        else {
            panic!("compiler.diff must return a JSON string");
        };
        assert!(json.contains("\"error\""), "type-invalid diff must fail closed: {json}");
        assert!(json.contains("unknown function `missing`"), "diff error must include the type error: {json}");

        let NativeValue::Str(json) =
            diff(&[NativeValue::Str("pub fn direct() -> Int:\n    0\n".into()), NativeValue::Str(src.into())])
                .expect("native diff")
        else {
            panic!("compiler.diff must return a JSON string");
        };
        assert!(json.contains("\"error\""), "comptime diff must fail closed: {json}");
        assert!(json.contains("does not support comptime"), "diff error must name the boundary: {json}");

        let doc = crate::native::lookup("compiler.doc").expect("compiler.doc native");
        let NativeValue::Str(md) =
            doc(&[NativeValue::Str("generated".into()), NativeValue::Str(src.into())]).expect("native doc")
        else {
            panic!("compiler.doc must return a markdown string");
        };
        assert!(md.contains("doc error:"), "comptime doc must return an error comment: {md}");
        assert!(md.contains("does not support comptime"), "doc error must name the boundary: {md}");
    }

    #[test]
    fn compiler_footprint_rejects_type_invalid_sources_on_both_backends() {
        let src = "import compiler\n\nfn main(console: Console):\n    let bad = \"fn main(console: Console):\\n    missing(console)\\n\"\n    let fp = compiler.footprint(bad)\n    console.print(\"${fp.contains(\"error\")}\")\n    console.print(\"${fp.contains(\"missing\")}\")\n    let diff = compiler.diff(\"pub fn direct() -> Int:\\n    0\\n\", bad)\n    console.print(\"${diff.contains(\"error\")}\")\n    console.print(\"${diff.contains(\"missing\")}\")\n";
        let expected = ["true", "true", "true", "true"];
        assert_eq!(link_run(src), expected, "interp: compiler.footprint type gate");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: compiler.footprint type gate",
        );
    }

    #[test]
    fn compiler_try_doc_reports_parse_errors_as_result_on_both_backends() {
        let src = "import compiler\n\nfn via_string() -> Result(String, String):\n    let md = compiler.try_doc(\"bad\", \"fn broken( ->\")?\n    Ok(md)\n\nfn main(console: Console):\n    match compiler.try_doc(\"bad\", \"fn broken( ->\"):\n        Ok(_) -> console.print(\"unexpected ok\")\n        Err(compiler.SourceRejected(message)) -> console.print(message)\n        Err(e) -> console.print(compiler.compiler_error_message(e))\n    match via_string():\n        Ok(_) -> console.print(\"unexpected string ok\")\n        Err(e) -> console.print(e)\n    console.print(compiler.doc(\"bad\", \"fn broken( ->\"))\n";
        let expected = [
            "parse error at 1:12: expected an identifier, found `->`",
            "parse error at 1:12: expected an identifier, found `->`",
            "<!-- doc error: parse error at 1:12: expected an identifier, found `->` -->",
        ];
        assert_eq!(link_run(src), expected, "interp: compiler.try_doc error channel");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: compiler.try_doc error channel",
        );
    }

    /// (BUG-538 / D9) Core values should compose through the public protocols,
    /// not through backend magic. This is the release gate over the protocol
    /// matrix: representative std values must have deliberate `Show`, `Reflect`,
    /// and `PartialEq`/`Eq` behavior, including when nested inside containers.
    #[test]
    fn core_protocol_matrix_composes_on_both_backends() {
        let src = r#"import bytes
import cmp
import encoding
import json
import list
import reflect
import set
import show
import testing

type Label derive(Reflect, PartialEq, Eq):
    Label(String)

impl Show for Label:
    fn show(self) -> String:
        match self:
            Label(s) -> "<" + s + ">"

type ProtocolRow derive(Reflect):
    label: Label
    payload: Bytes
    wait: Duration
    order: Ordering
    choices: Set(Int)
    outcome: Result(Bytes, String)
    tupled: (Int, String, Bool, Duration, Ordering)

fn same(x: a, y: a) -> Bool where a: PartialEq:
    x == y

fn total_same(x: a, y: a) -> Bool where a: Eq:
    x == y

fn sorted_window(xs: List(a)) -> String where a: Ord, a: Show:
    var sortable = xs
    sortable.sort()
    show.render(sortable) + "|" + show.render(list.min(sortable)) + "|" + show.render(list.max(sortable))

fn main(console: Console):
    let b = bytes.from_string("hi")
    let other_b = bytes.from_string("hi")
    let s = set.from_list([1, 2, 2])
    let other_s = set.from_list([2, 1])
    let outcome: Result(Bytes, String) = Ok(b)
    let other_outcome: Result(Bytes, String) = Ok(other_b)
    let tup = (7, "x", true, 90s, Greater)
    let other_tup = (7, "x", true, 90s, Greater)
    let labels = [Label("x"), Label("y")]
    let other_labels = [Label("x"), Label("y")]
    let row = ProtocolRow(Label("packet"), b, 90s, Greater, s, outcome, tup)

    console.print(sorted_window([3, 1, 2]))
    console.print(sorted_window(["b", "a", "c"]))
    console.print("${labels}")
    console.print(show.render(labels))
    console.print(show.render(b))
    console.print(show.render(90s))
    console.print(show.render(Greater))
    console.print(show.render(s))
    console.print(show.render(outcome))
    console.print(show.render(tup))
    let hex = encoding.hex_encode_bytes(b)
    match encoding.hex_decode_bytes(hex):
        Ok(back) -> console.print(hex + ":" + back.to_string())
        Err(e) -> console.print("bad:" + show.render(e))
    match encoding.hex_decode_bytes("zz"):
        Ok(_) -> console.print("bad")
        Err(_) -> console.print("hex-err")
    console.print(json.stringify(row))

    testing.assert_value_eq(labels, other_labels)
    console.print("${same(b, other_b)}")
    console.print("${total_same(b, other_b)}")
    console.print("${same(labels, other_labels)}")
    console.print("${same(Some(Greater), Some(Greater))}")
    console.print("${same(outcome, other_outcome)}")
    console.print("${total_same(s, other_s)}")
    console.print("${same(tup, other_tup)}")
    console.print("${total_same(tup, other_tup)}")
    console.print(show.render("nope".parse_int()))
    console.print(show.render(list.get([10, 20], 9)))
    match json.decode("1 2"):
        Ok(_) -> console.print("bad")
        Err(_) -> console.print("json-err")
"#;
        let expected = [
            "[1, 2, 3]|Some(1)|Some(3)",
            "[a, b, c]|Some(a)|Some(c)",
            "[<x>, <y>]",
            "[<x>, <y>]",
            "Bytes(len=2)",
            "1m30s",
            "Greater",
            "{1, 2}",
            "Ok(Bytes(len=2))",
            "(7, x, true, 1m30s, Greater)",
            "6869:hi",
            "hex-err",
            "{\"label\":{\"$variant\":\"Label\",\"$values\":[\"packet\"]},\"payload\":[104,105],\"wait\":90000,\"order\":{\"$variant\":\"Greater\",\"$values\":[]},\"choices\":[1,2],\"outcome\":{\"$variant\":\"Ok\",\"$values\":[[104,105]]},\"tupled\":[7,\"x\",true,90000,{\"$variant\":\"Greater\",\"$values\":[]}]}",
            "true",
            "true",
            "true",
            "true",
            "true",
            "true",
            "true",
            "true",
            "None",
            "None",
            "json-err",
        ];
        assert_eq!(link_run(src), expected, "interp: core protocol matrix");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: core protocol matrix",
        );
    }

    /// (BUG-481) Numeric duration constructors are convenience contracts, not
    /// wrapping arithmetic. Oversized counts abort before the intermediate Int
    /// multiplication/addition can wrap; ordinary negative spans remain valid.
    #[test]
    fn duration_numeric_constructors_abort_on_overflow_on_both_backends() {
        let ok = "import duration\n\nfn main(console: Console):\n    console.print(duration.human(duration.seconds(0 - 90)))\n    console.print(duration.human(duration.from_clock(1, 2, 3)))\n";
        let expected = ["-1m30s", "1h2m3s"];
        assert_eq!(link_run(ok), expected, "interp: duration constructor controls");
        assert_eq!(
            run_linked_on_wasm(&[("main", ok)], "main"),
            expected,
            "compiled: duration constructor controls",
        );

        for (label, call) in [
            ("seconds", "duration.seconds(9223372036854776)"),
            ("days", "duration.days(200000000000)"),
            ("from_clock", "duration.from_clock(2562047788015, 13, 0)"),
        ] {
            let src = format!("import duration\n\nfn main(console: Console):\n    console.print(\"${{{call}}}\")\n");
            let linked = resolve_std_src(&src);
            let interp_err = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter must abort on duration overflow")
                .to_string();
            assert!(
                interp_err.contains("duration.") && interp_err.contains("overflow"),
                "{label}: {interp_err}"
            );
            let wasm = codegen::compile_module_binary(&linked)
                .expect_lowered("duration overflow program should lower");
            let wasm_err = crate::run_wasm_bytes(&wasm)
                .expect_err("WASM must abort on duration overflow")
                .to_string();
            assert!(
                wasm_err.contains("duration.") && wasm_err.contains("overflow"),
                "{label}: {wasm_err}"
            );
        }
    }

    /// (BUG-214) `Nil` is the language's unit value, so every backend must accept
    /// it anywhere a `Nil` expression is expected instead of treating it as an
    /// unknown nullary constructor that the compiled backend cannot lower.
    #[test]
    fn bare_nil_expression_compiles_on_both_backends() {
        let cases = [
            (
                "tail",
                "fn unit() -> Nil:\n    Nil\n\nfn main(console: Console):\n    unit()\n    console.print(\"tail\")\n",
                ["tail"],
            ),
            (
                "statement",
                "fn main(console: Console):\n    Nil\n    console.print(\"statement\")\n",
                ["statement"],
            ),
            (
                "match arm",
                "fn unit(n: Int) -> Nil:\n    match n:\n        0 -> Nil\n        _ -> Nil\n\nfn main(console: Console):\n    unit(0)\n    unit(1)\n    console.print(\"match\")\n",
                ["match"],
            ),
            (
                "let binding",
                "fn unit() -> Nil:\n    Nil\n\nfn main(console: Console):\n    let x = unit()\n    console.print(\"let\")\n",
                ["let"],
            ),
        ];

        for (label, src, expected) in cases {
            assert_eq!(link_run(src), expected, "interp: {label}");
            assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: {label}");
        }

        assert!(
            typeck::check_str("fn main(console: Console):\n    Nil(1)\n").is_err(),
            "Nil has no constructor fields"
        );
    }

    /// Type heads are not runtime values. The resolver may keep ambient type
    /// names bare (`Int`, `Set`, `Tuple2`, a local sum type name), but type
    /// checking must reject them before codegen instead of letting them look like
    /// unknown constructors with fresh result types.
    #[test]
    fn type_names_are_rejected_as_values_after_linking() {
        let cases = [
            (
                "builtin constructor-looking call",
                "fn main(console: Console):\n    Int(1)\n    console.print(\"bad\")\n",
                "type `Int` is not a value",
            ),
            (
                "prelude type",
                "fn main(console: Console):\n    Result\n    console.print(\"bad\")\n",
                "type `Result` is not a value",
            ),
            (
                "synthetic tuple type",
                "fn main(console: Console):\n    Tuple2(1, 2)\n    console.print(\"bad\")\n",
                "type `Tuple2` is not a value",
            ),
            (
                "local sum type name",
                "type Color:\n    Red\n    Blue\n\nfn main(console: Console):\n    Color\n    console.print(\"bad\")\n",
                "type `Color` is not a value",
            ),
        ];

        for (label, src, want) in cases {
            let module = parser::parse_module(src).expect(label);
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main")
                .unwrap_or_else(|e| panic!("{label}: link failed: {}", e.message));
            let err = typeck::check(&linked).expect_err(label);
            assert!(
                err.message.contains(want),
                "{label}: expected `{want}`, got `{}`",
                err.message
            );
        }
    }

    /// (BUG-216) A local binding with the same name as a prelude/imported module
    /// owns dotted calls consistently. `string.to_upper("x")` below must dispatch
    /// to the local `S` method, not silently escape to std String.to_upper.
    #[test]
    fn shadowing_module_name_keeps_dotted_calls_on_local() {
        let src = "type S:\n    x: String\n\nimpl S:\n    fn to_upper(self: S, suffix: String) -> String:\n        self.x + suffix\n\nfn module_upper(s: String) -> String:\n    s.to_upper()\n\nfn main(console: Console):\n    let string = S(\"s\")\n    console.print(string.to_upper(\"x\"))\n    console.print(module_upper(\"y\"))\n";
        let expected = ["sx", "Y"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-240, parity) `math.abs(Int.MIN)` has no positive `Int`, so both backends
    /// must ABORT rather than silently wrap back to the negative `Int.MIN`. Ordinary
    /// magnitudes still agree. (Was a stable wrong answer: `-Int.MIN == Int.MIN`.)
    #[test]
    fn math_abs_int_min_aborts_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        // Int.MIN: `0 - 9223372036854775807 - 1`. Both backends must error.
        let min_src = "import math\n\nfn main(console: Console):\n    console.print(\"${math.abs(0 - 9223372036854775807 - 1)}\")\n";
        let (lmod, wasm) = compile(min_src);
        assert!(
            interpreter::run_module(lmod, ".", Vec::new()).is_err(),
            "interpreter must abort on math.abs(Int.MIN)"
        );
        assert!(crate::run_wasm_bytes(&wasm).is_err(), "WASM must abort on math.abs(Int.MIN)");
        // Ordinary magnitudes agree (negative, zero, positive, and Int.MAX).
        let ok_src = "import math\n\nfn main(console: Console):\n    console.print(\"${math.abs(0 - 5)}\")\n    console.print(\"${math.abs(0)}\")\n    console.print(\"${math.abs(7)}\")\n    console.print(\"${math.abs(9223372036854775807)}\")\n";
        let expected = ["5", "0", "7", "9223372036854775807"];
        assert_eq!(link_run(ok_src), expected, "interp math.abs of ordinary values");
        assert_eq!(
            run_linked_on_wasm(&[("main", ok_src)], "main"),
            expected,
            "compiled math.abs of ordinary values must agree",
        );
    }

    /// (BUG-466, RFC-0044) `math.to_int(NaN)` is a loud contract error on both
    /// backends. Finite values and infinities keep the existing saturating
    /// truncation behavior.
    #[test]
    fn math_to_int_nan_aborts_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        let nan_src = "import math\n\nfn main(console: Console):\n    console.print(\"${math.to_int(0.0 / 0.0)}\")\n";
        let (lmod, wasm) = compile(nan_src);
        let interp_err = interpreter::run_module(lmod, ".", Vec::new())
            .expect_err("interpreter must abort on math.to_int(NaN)")
            .to_string();
        assert!(interp_err.contains("math.to_int: NaN cannot be converted to Int"), "{interp_err}");
        let wasm_err = crate::run_wasm_bytes(&wasm)
            .expect_err("WASM must abort on math.to_int(NaN)")
            .to_string();
        assert!(wasm_err.contains("math.to_int: NaN cannot be converted to Int"), "{wasm_err}");

        let ok_src = "import math\n\nfn main(console: Console):\n    console.print(\"${math.to_int(3.9)}\")\n    console.print(\"${math.to_int(0.0 - 3.9)}\")\n    console.print(\"${math.to_int(1.0 / 0.0)}\")\n    console.print(\"${math.to_int(0.0 - (1.0 / 0.0))}\")\n";
        let expected = ["3", "-3", "9223372036854775807", "-9223372036854775808"];
        assert_eq!(link_run(ok_src), expected, "interp math.to_int non-NaN cases");
        assert_eq!(
            run_linked_on_wasm(&[("main", ok_src)], "main"),
            expected,
            "compiled math.to_int non-NaN cases",
        );
    }

    /// (BUG-276) The public hex decoders reject malformed input before it can
    /// reach the private raw byte-level primitives (`encoding.hex_decode_lossy`,
    /// `encoding.hex_to_base64url_lossy`). Invalid input is `Err` on both
    /// backends, never the old silent-drop that could hand mangled crypto
    /// material to a signature check. Valid hex still round-trips.
    #[test]
    fn hex_primitives_reject_non_hex_strictly_on_both_backends() {
        let prog = |call: &str| {
            format!(
                "import encoding\n\nfn main(console: Console):\n    match {call}:\n        Ok(x) -> console.print(x)\n        Err(e) -> console.print(\"err\")\n"
            )
        };
        for bad in [
            "encoding.hex_decode(\"68zz69\")",
            "encoding.hex_to_base64url(\"zz6869\")",
            "encoding.hex_decode(\"abc\")", // odd length
        ] {
            let src = prog(bad);
            assert_eq!(link_run(&src), ["err"], "interpreter must reject non-hex: {bad}");
            assert_eq!(
                run_linked_on_wasm(&[("main", &src)], "main"),
                ["err"],
                "WASM must reject non-hex: {bad}"
            );
        }
        // Valid hex still decodes identically on both backends.
        let ok = prog("encoding.hex_decode(\"6869\")");
        assert_eq!(link_run(&ok), ["hi"], "interp decodes valid hex");
        assert_eq!(run_linked_on_wasm(&[("main", &ok)], "main"), ["hi"], "wasm decodes valid hex");
    }

    /// WebAuthn treats authenticatorData as a trust-boundary decoder input. A
    /// malformed hex flag byte must fail as malformed input before semantic flag
    /// checks can turn it into a misleading user-presence error.
    #[test]
    fn webauthn_authenticator_data_rejects_malformed_hex_before_flags_on_both_backends() {
        let src = r#"import crypto
import webauthn

fn main(console: Console):
    let rp = "example.com"
    let client = "{\"type\":\"webauthn.get\",\"challenge\":\"c\",\"origin\":\"https://example.com\"}"
    let malformed = crypto.sha256(rp) + "zz00000000"
    match webauthn.verify_assertion("00", malformed, client, "00", "c", "https://example.com", rp, true):
        Ok(v) -> console.print("ok ${v}")
        Err(e) ->
            match e:
                webauthn.AuthenticatorDataHex(_) -> console.print("typed")
                _ -> console.print("wrong")
            console.print(webauthn.assertion_error_message(e))
"#;

        let interp = link_run(src);
        assert_eq!(interp.len(), 2, "interpreter produced unexpected output: {interp:?}");
        assert_eq!(interp[0], "typed", "interpreter must expose typed malformed-authenticatorData error");
        assert!(
            interp[1].starts_with("authenticatorData is not valid hex:"),
            "interpreter must reject malformed authenticatorData as malformed hex: {interp:?}"
        );
        assert!(
            !interp[1].contains("user-presence flag not set"),
            "interpreter must not report a semantic flag error for malformed hex: {interp:?}"
        );

        let wasm = run_linked_on_wasm(&[("main", src)], "main");
        assert_eq!(wasm.len(), 2, "WASM produced unexpected output: {wasm:?}");
        assert_eq!(wasm[0], "typed", "WASM must expose typed malformed-authenticatorData error");
        assert!(
            wasm[1].starts_with("authenticatorData is not valid hex:"),
            "WASM must reject malformed authenticatorData as malformed hex: {wasm:?}"
        );
        assert!(
            !wasm[0].contains("user-presence flag not set"),
            "WASM must not report a semantic flag error for malformed hex: {wasm:?}"
        );
    }

    /// JWT/OIDC registered claims are trust-boundary inputs. Missing or
    /// wrong-shaped claims must fail as malformed payloads after the RS256
    /// signature verifies, not default into ordinary expiry/mismatch outcomes.
    #[test]
    fn jwt_registered_claims_reject_malformed_values_on_both_backends() {
        use crate::idp::RegistryKey;

        fn b64url(bytes: &[u8]) -> String {
            const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for c in bytes.chunks(3) {
                let n = ((c[0] as u32) << 16)
                    | ((*c.get(1).unwrap_or(&0) as u32) << 8)
                    | (*c.get(2).unwrap_or(&0) as u32);
                out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
                out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
                if c.len() > 1 {
                    out.push(ALPHABET[(n >> 6 & 63) as usize] as char);
                }
                if c.len() > 2 {
                    out.push(ALPHABET[(n & 63) as usize] as char);
                }
            }
            out
        }

        fn signed_jwt(key: &RegistryKey, payload_json: &str) -> String {
            let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
            let payload = b64url(payload_json.as_bytes());
            let signing_input = format!("{header}.{payload}");
            let sig = key.sign(signing_input.as_bytes()).expect("sign JWT");
            format!("{signing_input}.{}", b64url(&sig))
        }

        let dir = std::env::temp_dir().join(format!("witchy_jwt_claims_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let key = RegistryKey::load_or_create(&dir).expect("issuer key");
        let pubkey = key.public_hex();
        let missing_exp = signed_jwt(
            &key,
            r#"{"iss":"https://issuer","sub":"s","aud":"aud","iat":1}"#,
        );
        let wrong_aud = signed_jwt(
            &key,
            r#"{"iss":"https://issuer","sub":"s","aud":123,"exp":2000000000,"iat":1}"#,
        );
        let wrong_iss = signed_jwt(
            &key,
            r#"{"iss":123,"sub":"s","aud":"aud","exp":2000000000,"iat":1}"#,
        );

        let src = format!(
            r#"import jwt

fn report(console: Console, token: String):
    match jwt.verify_oidc(token, "{pubkey}", "https://issuer", "aud", 1000):
        Ok(_) -> console.print("ok")
        Err(e) ->
            match e:
                jwt.MissingClaim(name) -> console.print("missing:" + name)
                jwt.AudienceClaimExpected -> console.print("aud-shape")
                jwt.StringClaimExpected(name) -> console.print("string:" + name)
                _ -> console.print("other")
            console.print(jwt.jwt_error_message(e))

fn main(console: Console):
    report(console, "{missing_exp}")
    report(console, "{wrong_aud}")
    report(console, "{wrong_iss}")
"#
        );
        let want = [
            "missing:exp",
            "JWT payload is missing `exp`",
            "aud-shape",
            "JWT payload `aud` must be a string or array of strings",
            "string:iss",
            "JWT payload `iss` must be a string",
        ];
        assert_eq!(link_run(&src), want, "interpreter must reject malformed registered claims");
        assert_eq!(
            run_linked_on_wasm(&[("main", &src)], "main"),
            want,
            "WASM must reject malformed registered claims"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (SEC-045) An overflowing `Content-Length` must NOT crash the server. The old
    /// `content_length` guarded with `ascii.all_digits` (which passes an arbitrarily
    /// long digit string) then called `string.to_int`, which TRAPS on i64 overflow —
    /// an unauthenticated remote crash. The fix parses totally with `string.parse_int`
    /// (returns None on overflow) and treats a rejected value as no body (0). This
    /// mirrors `server.content_length` and must agree + not trap on both backends.
    #[test]
    fn overflowing_content_length_does_not_trap_on_either_backend() {
        // `ascii.all_digits` accepts the overflowing string (the old trap trigger),
        // but the total parse yields 0 (no body) rather than aborting the VM.
        let src = "import ascii\nimport option\n\n\
                   fn content_length_val(v: String) -> Int:\n\
                   \x20   match v.parse_int():\n\
                   \x20       Some(n) -> if n > 0: n else: 0\n\
                   \x20       None -> 0\n\n\
                   fn main(console: Console):\n\
                   \x20   let big = \"99999999999999999999999999\"\n\
                   \x20   console.print(\"${ascii.all_digits(big)}\")\n\
                   \x20   console.print(\"${content_length_val(big)}\")\n\
                   \x20   console.print(\"${content_length_val(\"42\")}\")\n\
                   \x20   console.print(\"${content_length_val(\"abc\")}\")\n";
        let want = ["true", "0", "42", "0"];
        assert_eq!(link_run(src), want, "interp: overflow -> 0, valid -> value, junk -> 0");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), want, "wasm must agree and not trap");
    }

    /// (RFC-0047) `==` on a function type is a compile-time error — there is no
    /// stable equality for functions (identity is a monomorphization/inlining
    /// accident), and comparing them was a confirmed backend parity divergence
    /// (interpreter name-compares `true`, compiled pointer-compares `false`).
    /// Rejecting deletes the divergence by construction. Both the direct case and
    /// the container/tuple case must error with a teaching message.
    #[test]
    fn function_equality_is_a_compile_error() {
        let direct = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    console.print(\"${f == f}\")\n";
        let e = typeck::check_str(direct).expect_err("`f == f` must be rejected");
        assert!(e.contains("not defined on function types"), "teaching error, got: {e}");
        // Nested inside a container is caught the same way (depth-uniform).
        let in_list = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    console.print(\"${[f] == [f]}\")\n";
        let el = typeck::check_str(in_list).expect_err("`[f] == [f]` must be rejected");
        assert!(el.contains("not defined on function types"), "teaching error, got: {el}");
        let in_tuple = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    console.print(\"${(f, 1) == (f, 1)}\")\n";
        assert!(
            typeck::check_str(in_tuple).expect_err("`(f, 1) == (f, 1)` must be rejected")
                .contains("not defined on function types"),
            "a function nested in a tuple must be rejected too"
        );
    }

    /// (RFC-0032) `vm.par_map` over `String` elements: each string is a flat
    /// `[len][bytes]` value, so it crosses to a worker VM by a plain byte copy (in via
    /// the worker's `__galloc`, result back out) — no marshaling. A witchy `String` is
    /// always valid UTF-8, so the round-trip is lossless. Both backends must agree.
    #[test]
    fn vm_par_map_string_backends_agree() {
        let src = "import vm\n\nfn shout(s: String) -> String:\n    s + \"!\"\n\nfn main(console: Console):\n    let prior: List(fn(String) -> String) = [fn(s: String): s + \"?\"]\n    console.print(list.at(prior, 0)(\"warm\"))\n    let ys = vm.par_map([\"a\", \"bb\", \"ccc\"], shout)\n    console.print(\"${ys}\")\n";
        let expected = ["warm?", "[a!, bb!, ccc!]"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0032) Cross-VM channels: `vm.serve(init, requests, handler)` runs a stateful
    /// service on a long-lived isolated worker VM — it threads `state` through the request
    /// stream (here a running byte concatenation) and emits each new state. Lock-step and
    /// deterministic, so the interpreter (sequential scan) and the compiled backend
    /// (persistent worker VM) produce identical responses.
    #[test]
    fn vm_serve_stateful_service_agrees() {
        let src = "import vm\nimport bytes\n\nfn step(state: Bytes, req: Bytes) -> Bytes:\n    bytes.concat(state, req)\n\nfn main(console: Console):\n    let prior: List(fn(Bytes, Bytes) -> Bytes) = [fn(state: Bytes, _req: Bytes): state]\n    console.print(bytes.to_string(list.at(prior, 0)(bytes.from_string(\"warm\"), bytes.from_string(\"ignored\"))))\n    let reqs = [bytes.from_string(\"a\"), bytes.from_string(\"b\"), bytes.from_string(\"c\")]\n    let outs = vm.serve(bytes.from_string(\"\"), reqs, step)\n    for o in outs:\n        console.print(bytes.to_string(o))\n";
        let expected = ["warm", "a", "ab", "abc"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0032/RFC-0005) `vm.with_dir(dir, f, input)` is intentionally fail-closed while
    /// `Dir` is externref-backed but higher-order function values still use the slot-based
    /// closure ABI. The API remains declared in `std/vm`; using it with a `fn(Dir, ...)`
    /// value is rejected until the typed cap-closure boundary lands.
    #[test]
    fn vm_with_dir_rejects_slot_based_dir_callback() {
        let src = "import vm\nimport bytes\n\nfn reader(d: Dir, name: Bytes) -> Bytes:\n    bytes.from_string(d.read(bytes.to_string(name)))\n\nfn main(console: Console, dir: Dir):\n    let out = vm.with_dir(dir, reader, bytes.from_string(\"ok.txt\"))\n    console.print(bytes.to_string(out))\n";
        let err = typeck::check(&resolve_std_src(src))
            .expect_err("Dir-bearing function values cannot cross the current closure ABI");
        assert!(err.message.contains("Dir") && err.message.contains("function value"), "{}", err.message);
    }

    /// RFC-0005 Stage 3: a single-field capability brand over `Dir` is transparent
    /// at runtime, and `Option(ConfigDir)` is a nullable externref rather than a
    /// heap slot. This preserves the sealed smart-constructor idiom for root caps.
    #[test]
    fn branded_dir_option_runs_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_brandeddir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).expect("mkdir");
        std::fs::write(root.join("data").join("greeting.txt"), "hello-branded").expect("seed");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = "capability ConfigDir from Dir[Read]\n\nfn config_dir(root: Dir[Read]) -> Option(ConfigDir):\n    if root.exists(\"data\"):\n        Some(ConfigDir(root.subtree(\"data\")))\n    else:\n        None\n\nfn load(c: ConfigDir, name: String) -> String:\n    match c:\n        ConfigDir(dir) -> dir.read(name)\n\nfn main(console: Console, root: Dir[Read]):\n    match config_dir(root):\n        Some(cfg) -> console.print(load(cfg, \"greeting.txt\"))\n        None -> console.print(\"missing\")\n";
        let want = vec!["hello-branded".to_string()];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers branded Dir options");
        let mut rt = Runtime::batch().expect("runtime");
        let caps = Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        };
        let mut actor = rt.spawn(&bin, caps, 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (RFC-0032) `vm.par_map` over `Bytes`: binary payloads cross to worker VMs by a
    /// RAW (non-lossy) byte copy. Maps a top-level fn over a list of Bytes in parallel;
    /// both backends agree (the interp oracle runs the sequential `List.map` body).
    #[test]
    fn vm_par_map_bytes_backends_agree() {
        let src = "import vm\nimport bytes\n\nfn tag(b: Bytes) -> Bytes:\n    bytes.concat(b, bytes.from_string(\"!\"))\n\nfn main(console: Console):\n    let xs = [bytes.from_string(\"a\"), bytes.from_string(\"bb\"), bytes.from_string(\"ccc\")]\n    let ys = vm.par_map(xs, tag)\n    console.print(bytes.to_string(list.at(ys, 0)))\n    console.print(bytes.to_string(list.at(ys, 2)))\n    console.print(\"${bytes.length(list.at(ys, 1))}\")\n";
        let expected = ["a!", "ccc!", "3"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// RFC-0011: the raw-string `restrict` builtin is RETIRED. Address narrowing now goes
    /// only through the typed `net.only(Net...)` verb; a raw `host:port` string survives
    /// solely as a `--net`/config grant, not a language builtin. Both the free `restrict(net,
    /// …)` and the method `net.restrict(…)` forms are rejected — there is no such verb.
    #[test]
    fn retired_restrict_builtin_is_rejected() {
        assert!(
            typeck::check_str("fn main(net: Net):\n    let r = restrict(net, \"a:1\")\n").is_err(),
            "the free `restrict` builtin must be rejected after retirement",
        );
        assert!(
            typeck::check_str("fn main(net: Net):\n    let r = net.restrict(\"a:1\")\n").is_err(),
            "the `net.restrict` method form must be rejected after retirement",
        );
    }

    /// Python-style f-strings: `f"...{expr}..."` interpolates (with `{{`/`}}` for
    /// literal braces), desugaring to generated render + concat — same result on
    /// both backends.
    #[test]
    fn f_strings_interpolate() {
        let src = "fn main(console: Console):\n    let name = \"world\"\n    let n = 6\n    console.print(f\"hi {name} #{n * 7}\")\n    console.print(f\"{{braces}}\")\n";
        assert_eq!(interp(src), vec!["hi world #42", "{braces}"]);
        assert_eq!(run_on_wasm(src), vec!["hi world #42", "{braces}"]);
    }

    /// `witchy emit-wat <file>` compiles a program to WebAssembly text — the same
    /// module `sandbox` runs — for inspecting the generated code.
    #[test]
    fn emit_wat_returns_the_compiled_module() {
        let path = std::env::temp_dir().join(format!("witchy_emit_wat_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "fn fib(n: Int) -> Int:\n    if n < 2:\n        n\n    else:\n        fib(n - 1) + fib(n - 2)\nfn main(console: Console):\n    console.print(\"${fib(10)}\")\n",
        )
        .expect("write temp source");
        let wat = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat");
        let _ = std::fs::remove_file(&path);
        assert!(wat.starts_with("(module"), "expected a wasm module, got: {}", &wat[..wat.len().min(40)]);
        // The fib function is emitted, module-qualified by the file stem.
        assert!(wat.contains(".fib (param $n i64)"), "expected the fib function in the WAT");
    }

    /// Property tests: a `for` over a random range must compute exactly the same
    /// result as a Rust reference range, on BOTH backends (so they also agree
    /// with each other) — across sign, inclusive/exclusive, empty, and `continue`.
    mod range_for_properties {
        use super::{interp, run_on_wasm};
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(96))]

            #[test]
            fn sum_matches_reference(lo in -300i64..300, len in 0i64..600, inclusive in any::<bool>()) {
                let hi = lo + len;
                let op = if inclusive { "..=" } else { ".." };
                let src = format!(
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}{op}{hi}:\n        s = s + i\n    console.print(\"${{s}}\")\n"
                );
                let reference: i64 = if inclusive { (lo..=hi).sum() } else { (lo..hi).sum() };
                let want = vec![reference.to_string()];
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm(&src), want);
            }

            #[test]
            fn continue_skipping_odds_matches_reference(lo in -100i64..100, len in 0i64..300) {
                let hi = lo + len;
                let src = format!(
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}..{hi}:\n        if i % 2 != 0:\n            continue\n        s = s + i\n    console.print(\"${{s}}\")\n"
                );
                let reference: i64 = (lo..hi).filter(|x| x % 2 == 0).sum();
                let want = vec![reference.to_string()];
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm(&src), want);
            }
        }
    }

    mod stdlib_evidence;
    mod concurrency;
    mod traits;
    mod records;
    mod comptime;
    mod quote;
    mod ownership;
    mod json;
    mod capabilities;
    mod region;
    mod network;
    mod iter;
    mod reflection;
    mod keyword_args;
    mod tailcalls;
    mod bytes;
    mod crypto;
    mod toml;
    mod bigint;
    mod pm_coven;
    mod capability_narrowing;
    mod cli_std_modules;
    mod std_result_list_combinators;
    mod record_match_integration;
    mod func_values;
    mod rc_corpus;
    mod crypto_jwt_oauth;
    mod glamour;
    mod rfc0046_dispatch;
    mod gen_async;
    mod closures;
    mod collections;
    mod match_control;
    mod mutation;
    mod structs;

    fn wasm_run(src: &str) -> Vec<String> {
        witchy_interp::compiler_natives::install();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        crate::run_wasm_bytes(&bytes).expect("wasm run")
    }

    #[test]
    fn rfc0081_supertrait_upcast_backends_agree() {
        let src = r#"
trait Base:
    fn base(let self) -> Int

trait Render: Base:
    fn render(let self) -> Int

type Label:
    Label(Int)

impl Base for Label:
    fn base(let self) -> Int:
        match self:
            Label(value) -> value

impl Render for Label:
    fn render(let self) -> Int:
        match self:
            Label(value) -> value + 10

fn main(console: Console):
    let rendered: dyn Render = Label(2)
    let base: dyn Base = rendered
    console.print("${base.base()}")
"#;
        let linked = resolve_std_src(src);
        let want = vec!["2".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpreter"),
            want,
            "interpreter"
        );
        let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
        assert_eq!(crate::run_wasm_bytes(&bytes).expect("wasm"), want, "wasm");
    }

    #[test]
    fn rfc0081_rejects_unrelated_existential_upcasts_before_execution() {
        let src = r#"
trait Render:
    fn render(let self) -> Int

trait Inspect:
    fn inspect(let self) -> Int

type Label:
    Label(Int)

impl Render for Label:
    fn render(let self) -> Int:
        match self:
            Label(value) -> value

impl Inspect for Label:
    fn inspect(let self) -> Int:
        match self:
            Label(value) -> value

fn main() -> Int:
    let rendered: dyn Render = Label(2)
    let inspect: dyn Inspect = rendered
    inspect.inspect()
"#;
        let linked = resolve_std_src(src);
        let interpreter_error = interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect_err("interpreter must reject unrelated upcast")
            .to_string();
        assert!(
            interpreter_error
                .contains("invalid existential upcast request `main.Render` to `main.Inspect`"),
            "unexpected interpreter error: {interpreter_error}"
        );
        let codegen_error = codegen::compile_module_binary(&linked)
            .expect_rejected("compiled backend must reject unrelated upcast")
            .to_string();
        assert!(
            codegen_error
                .contains("invalid existential upcast request `main.Render` to `main.Inspect`"),
            "unexpected codegen error: {codegen_error}"
        );
    }

    #[test]
    fn rfc0081_receiver_and_nested_var_writebacks_agree_across_backends() {
        let src = r#"
trait CounterOps:
    fn bare(self) -> Int
    fn inspect(let self) -> Int
    fn tail(var self) -> Int
    fn explicit(var self) -> Int
    fn question(var self) -> Result(Int, String)
    fn adjust(let self, var value: Int) -> Int
    fn pair(let self, var left: Int, var right: Int) -> Int
    fn announce(let self, console: Console)
    fn take(own self) -> Int

type Counter:
    Counter(Int)

type Holder:
    item: dyn CounterOps

type Slots:
    left: Int
    right: Int

fn tail_step(var value: Counter) -> Int:
    let Counter(current) = value
    value = Counter(current + 1)
    current + 1

impl CounterOps for Counter:
    fn bare(self) -> Int:
        match self:
            Counter(value) -> value

    fn inspect(let self) -> Int:
        match self:
            Counter(value) -> value

    fn tail(var self) -> Int:
        tail_step(self)

    fn explicit(var self) -> Int:
        let Counter(current) = self
        self = Counter(current + 2)
        return current + 2

    fn question(var self) -> Result(Int, String):
        let Counter(current) = self
        self = Counter(current + 3)
        Err("stopped")?

    fn adjust(let self, var value: Int) -> Int:
        value = value + 1
        match self:
            Counter(current) -> current + value

    fn pair(let self, var left: Int, var right: Int) -> Int:
        left = left + 1
        right = right + 2
        match self:
            Counter(current) -> current + left + right

    fn announce(let self, console: Console):
        match self:
            Counter(value) -> console.print("counter=${value}")

    fn take(own self) -> Int:
        match self:
            Counter(value) -> value

fn direct(console: Console):
    var counter = Counter(1)
    var slots = Slots(3, 9)
    console.print("${counter.bare()} ${counter.inspect()}")
    console.print("${counter.tail()} ${counter.explicit()}")
    let ignored = counter.question()
    let adjusted = counter.adjust(slots.left)
    console.print("${adjusted} ${counter.pair(slots.left, slots.right)} ${counter.inspect()} ${slots.left} ${slots.right}")
    counter.announce(console)
    let consumed = Counter(12)
    console.print("${consumed.take()}")

fn dynamic(console: Console):
    var holder = Holder(Counter(1))
    var slots = Slots(3, 9)
    console.print("${holder.item.bare()} ${holder.item.inspect()}")
    console.print("${holder.item.tail()} ${holder.item.explicit()}")
    let ignored = holder.item.question()
    let adjusted = holder.item.adjust(slots.left)
    console.print("${adjusted} ${holder.item.pair(slots.left, slots.right)} ${holder.item.inspect()} ${slots.left} ${slots.right}")
    holder.item.announce(console)
    let consumed: dyn CounterOps = Counter(12)
    console.print("${consumed.take()}")

fn main(console: Console):
    direct(console)
    dynamic(console)
"#;
        let linked = resolve_std_src(src);
        let interpreter =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpreter");
        let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
        let wasm = crate::run_wasm_bytes(&bytes).expect("wasm");
        let one_backend = vec![
            "1 1".to_string(),
            "2 4".to_string(),
            "11 23 7 5 11".to_string(),
            "counter=7".to_string(),
            "12".to_string(),
        ];
        assert_eq!(interpreter, [one_backend.clone(), one_backend.clone()].concat());
        assert_eq!(wasm, interpreter);
    }

    #[test]
    fn rfc0081_rejects_aliased_var_places_and_use_after_own() {
        let aliases = r#"
trait Adjust:
    fn clash(let self, var left: Int, var right: Int) -> Int

type Counter:
    Counter(Int)

impl Adjust for Counter:
    fn clash(let self, var left: Int, var right: Int) -> Int:
        left = left + 1
        right = right + 1
        left + right

fn main() -> Int:
    let counter: dyn Adjust = Counter(1)
    var value = 3
    counter.clash(value, value)
"#;
        let aliases = resolve_std_src(aliases);
        let alias_interpreter = interpreter::run_module(aliases.clone(), ".", Vec::new())
            .expect_err("interpreter must reject overlapping var places")
            .to_string();
        let alias_codegen = codegen::compile_module_binary(&aliases)
            .expect_rejected("compiled backend must reject overlapping var places")
            .to_string();
        for alias_error in [&alias_interpreter, &alias_codegen] {
            assert!(
                alias_error.contains("overlapping `var` places rooted in `value`"),
                "{alias_error}"
            );
        }

        let moved = r#"
trait Consume:
    fn take(own self) -> Int

type Counter:
    Counter(Int)

impl Consume for Counter:
    fn take(own self) -> Int:
        match self:
            Counter(value) -> value

fn main() -> Int:
    let counter: dyn Consume = Counter(1)
    let first = counter.take()
    first + counter.take()
"#;
        let moved = resolve_std_src(moved);
        let move_interpreter = interpreter::run_module(moved.clone(), ".", Vec::new())
            .expect_err("interpreter must reject use after own")
            .to_string();
        let move_codegen = codegen::compile_module_binary(&moved)
            .expect_rejected("compiled backend must reject use after own")
            .to_string();
        for move_error in [&move_interpreter, &move_codegen] {
            assert!(
                move_error.contains("was already consumed")
                    || move_error.contains("use after move")
                    || move_error.contains("after it was moved"),
                "{move_error}"
            );
        }
    }

    #[test]
    fn rfc0081_var_receiver_traps_before_writeback_on_both_backends() {
        let src = r#"
trait Explode:
    fn explode(var self) -> Int

type Counter:
    Counter(Int)

impl Explode for Counter:
    fn explode(var self) -> Int:
        self = Counter(99)
        1 / 0

fn main() -> Int:
    var counter: dyn Explode = Counter(1)
    counter.explode()
"#;
        let linked = resolve_std_src(src);
        let interpreter_error = interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect_err("interpreter call must trap")
            .to_string();
        let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
        let wasm_error = crate::run_wasm_bytes(&bytes).expect_err("wasm call must trap");
        assert!(
            interpreter_error.contains("division by zero"),
            "{interpreter_error}"
        );
        assert!(
            wasm_error.contains("divide by zero") || wasm_error.contains("division by zero"),
            "{wasm_error}"
        );
    }

    #[test]
    fn rfc0081_normal_and_opt_modes_have_identical_values_and_traps() {
        let values = r#"
trait Render:
    fn render(let self) -> String

type Number:
    Number(Int)

type Label:
    Label(String)

impl Render for Number:
    fn render(let self) -> String:
        match self:
            Number(value) -> "number=${value}"

impl Render for Label:
    fn render(let self) -> String:
        match self:
            Label(value) -> "label=${value}"

fn main(console: Console):
    let values: List(dyn Render) = [Number(7), Label("safe")]
    for value in values:
        console.print(value.render())
"#;
        let run = |source: &str| {
            let linked = resolve_std_src(source);
            let interpreted =
                interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpreter");
            let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
            let compiled = crate::run_wasm_bytes(&bytes).expect("wasm");
            assert_eq!(compiled, interpreted);
            interpreted
        };
        let normal = run(values);
        let opt = run(&format!("mode opt\n{values}"));
        assert_eq!(normal, vec!["number=7", "label=safe"]);
        assert_eq!(opt, normal);

        let trap = r#"
trait Explode:
    fn explode(let self) -> Int

type Bomb:
    Bomb

impl Explode for Bomb:
    fn explode(let self) -> Int:
        1 / 0

fn main() -> Int:
    let value: dyn Explode = Bomb
    value.explode()
"#;
        let fail = |source: &str| {
            let linked = resolve_std_src(source);
            let interpreted = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter trap")
                .to_string();
            let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
            let compiled = crate::run_wasm_bytes(&bytes).expect_err("wasm trap");
            assert!(interpreted.contains("division by zero"), "{interpreted}");
            assert!(
                compiled.contains("divide by zero") || compiled.contains("division by zero"),
                "{compiled}"
            );
            (interpreted, compiled)
        };
        let normal_traps = fail(trap);
        let opt_traps = fail(&format!("mode opt\n{trap}"));
        assert_eq!(normal_traps.0, normal_traps.1);
        assert_eq!(opt_traps.0, opt_traps.1);
        let normal_kind = normal_traps.0.rsplit(": ").next().unwrap_or(&normal_traps.0);
        let opt_kind = opt_traps.0.rsplit(": ").next().unwrap_or(&opt_traps.0);
        assert_eq!(normal_kind, opt_kind);
    }

    /// `wasm_run` that also reads the exported `__witchy_reowns` counter —
    /// the timing-free proof of whether accumulation ran in place (O(1)
    /// re-owns) or fell to the copying path (O(n) re-owns).
    fn wasm_run_reowns(src: &str) -> (Vec<String>, i64) {
        use crate::runtime::{Capabilities, Runtime};
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        let reowns = actor.reowns().unwrap_or(0);
        (actor.output(), reowns)
    }

    /// `wasm_run` that also reads the `__heap` frontier and `__rc_reused_bytes` —
    /// the timing-free proof of whether the RC floor bounded the heap (flat frontier)
    /// by recycling freed blocks (reused > 0), rather than leaking O(iterations).
    fn wasm_run_heap(src: &str) -> (Vec<String>, i64, i64) {
        use crate::runtime::{Capabilities, Runtime};
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        let heap = actor.heap_bytes().unwrap_or(0);
        let reused = actor.rc_reused_bytes().unwrap_or(0);
        (actor.output(), heap, reused)
    }

    /// `comptime:` — compile-time item generation: zero capabilities
    /// reachable (deterministic by construction), `emit(line)` as the
    /// channel, output parsed as ADDITIVE items before checking — so the
    /// generated functions exist on both backends and in the footprint.
    #[test]
    fn comptime_blocks_generate_items_additively() {
        let src = "comptime:\n    var i = 0\n    while i < 3:\n        emit(\"pub fn lucky_${i}() -> Int:\")\n        emit(\"    ${i * 7}\")\n        emit(\"\")\n        i = i + 1\n\nfn main(console: Console):\n    console.print(\"${lucky_0()} ${lucky_1()} ${lucky_2()}\")\n";
        let want = vec!["0 7 14".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // Emitted garbage is a loud error carrying the emitted source.
        let bad = "comptime:\n    emit(\"fn (((\")\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let module = parser::parse_module(bad).expect("parse");
        let err = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect_err("bad emission must be loud");
        assert!(err.to_string().contains("does not parse"), "got: {err}");
    }

    /// `return X if cond` — a postfix-guard return, sugar for `if cond: return X`.
    /// It round-trips through fmt (the parser tags the desugared block with the
    /// synthetic-line marker so the formatter re-collapses exactly this shape),
    /// while an explicitly written multi-line `if cond: return X` is left untouched.
    /// Runs identically on both backends.
    #[test]
    fn postfix_guard_return() {
        let src = "fn classify(n: Int) -> String:\n    return \"neg\" if n < 0\n    return \"zero\" if n == 0\n    \"pos\"\n\nfn main(console: Console):\n    console.print(classify(-5))\n    console.print(classify(0))\n    console.print(classify(7))\n";
        let want: Vec<String> = ["neg", "zero", "pos"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // The postfix form is preserved by fmt (idempotent).
        assert_eq!(
            crate::format::reformat(src).as_deref(),
            Some(src),
            "postfix return round-trips through fmt"
        );
        // An explicitly written multi-line if-return is NOT collapsed.
        let explicit = "fn f(n: Int) -> Int:\n    if n < 0:\n        return 0\n    n\n";
        assert_eq!(
            crate::format::reformat(explicit).as_deref(),
            Some(explicit),
            "an explicit multi-line if-return is preserved"
        );
    }

    /// fmt breaks a long fluent method chain onto one call per line (witchy's layout
    /// joins the leading-`.` continuation lines back into the chain on re-parse), so
    /// a builder like a router reads vertically. Short chains stay inline, and the
    /// wrap is idempotent (the decision is the chain's own inline width, not its
    /// indented column, so `chain_wrap` and `expr_max_line` agree).
    #[test]
    fn fmt_wraps_long_method_chains() {
        let long = "fn main(net: Net):\n    let app = router().get(\"/aaaaaaaaaaaaaaaa\", h()).get(\"/bbbbbbbbbbbbbbbb\", h()).get(\"/cccccccccccccccc\", h()).get(\"/dddddddddddddddd\", h())\n    serve(net, app)\n";
        let wrapped = crate::format::reformat(long).expect("a long chain formats");
        assert!(
            wrapped.contains("let app = router()\n        .get("),
            "a long chain breaks one call per line:\n{wrapped}"
        );
        assert_eq!(
            crate::format::reformat(&wrapped).as_deref(),
            Some(wrapped.as_str()),
            "the wrap is idempotent"
        );
        // A short chain stays on one line.
        let short = "fn main(net: Net):\n    let x = a().b()\n";
        assert_eq!(
            crate::format::reformat(short).as_deref(),
            Some(short),
            "a short chain stays inline"
        );
    }

    /// FROM / INTO (std/convert): a user implements `From` and gets `Into` free via
    /// the blanket `impl Into(b) for a where b: From(a)`. The blanket body calls the
    /// STATIC `b.from(self)` on the bound target type (no receiver), resolved through
    /// the bound at monomorphization. Both backends.
    #[test]
    fn from_into_conversion_traits() {
        let src = "import convert\n\ntype Celsius:\n    deg: Int\n\nimpl From(Int) for Celsius:\n    fn from(value: Int) -> Celsius:\n        Celsius(value)\n\nfn main(console: Console):\n    let c: Celsius = (5).into()\n    let d = Celsius.from(9)\n    console.print(\"${c.deg} ${d.deg}\")\n";
        let want = vec!["5 9".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-534) RFC-0042's qualified type spelling composes with static trait
    /// methods: plain `import json` exposes the type as `json.Json`, and that
    /// receiver should reach the same `From(a) for Json` impl as bare `Json.from`.
    #[test]
    fn qualified_type_receiver_static_trait_method_backends_agree() {
        let src = "import json\nimport reflect\n\ntype Point derive(Reflect):\n    x: Int\n\nfn main(console: Console):\n    let p = Point(7)\n    let j = json.Json.from(p)\n    console.print(json.encode(j))\n";
        let want = vec!["{\"x\":7}".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    // ---- RFC-0052: one pattern grammar ------------------------------------

    /// (RFC-0052) A Float SCRUTINEE bound to a variable pattern now compiles (the
    /// former check-passes/codegen-fails hole) and agrees on both backends.
    #[test]
    fn float_scrutinee_binding_backends_agree() {
        let src = "fn main(console: Console):\n    let r = match 1.5:\n        x -> x + 1.0\n    console.print(\"${r}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2.5"]);
    }

    /// THE F11 FAMILY (learning log): interpolating values whose type only
    /// typed lowering knows — an ADT String payload and a generic-combinator
    /// return — renders identically on both backends.
    #[test]
    fn interpolation_of_mono_typed_values_agrees() {
        let src = "import iter\n\ntype Msg:\n    Text(String)\n    Silence\n\nfn main(console: Console):\n    match Text(\"hi\"):\n        Text(s) -> console.print(\"got: ${s}\")\n        Silence -> console.print(\"none\")\n    let collected: List(Int) = iter.collect(iter.range(1, 100).take(3))\n    console.print(\"collected: ${collected}\")\n";
        let want: Vec<String> = ["got: hi", "collected: [1, 2, 3]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    #[test]
    fn interpolation_tail_after_guard_returns_string() {
        let src = "fn checked(n: Int) -> String:\n    if n < 0:\n        fail(\"bad\")\n    \"${n}\"\n\nfn main(console: Console):\n    console.print(checked(7))\n";
        let want = vec!["7".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `say` covers every scalar out of the box (Duration in its HUMAN form
    /// — the custom rendering `Show` exists for), and a missing impl is a
    /// clean check-time error naming the trait and type, not a post-lowering
    /// "unknown function" artifact.
    #[test]
    fn show_scalars_and_missing_impl_diagnostic() {
        let src = "import show\n\nfn main(console: Console):\n    show.say(console, 42)\n    show.say(console, 3.5)\n    show.say(console, 90s)\n    show.say(console, true)\n";
        let want: Vec<String> =
            ["42", "3.5", "1m30s", "true"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        let missing = "import show\n\ntype Blob:\n    n: Int\n\nfn main(console: Console):\n    show.say(console, Blob(1))\n";
        let module = parser::parse_module(missing).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("missing impl must be rejected");
        assert!(
            err.to_string().contains("`Blob` does not implement `Show`"),
            "want a clean trait error, got: {err}"
        );
    }

    /// The formatter ROUND-TRIPS string interpolation. The lexer desugars it to
    /// a generated render chain, and `interpolation_sugar` prints that AST back
    /// to the public interpolation spelling.
    #[test]
    fn fmt_round_trips_interpolation() {
        let src = "fn main(console: Console):\n    let n = 3\n    console.print(\"n is ${n}, doubled ${n * 2}\")\n    console.print(\"cost: \\$${n}\")\n";
        assert_eq!(crate::format::reformat(src).as_deref(), Some(src), "interpolation must round-trip");
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


    // HEAP-TYPE MATRIX (RFC-0035 step 3 gate). Corpus 1-3 above only exercised RECORD/ADT
    // elements — the false confidence that let the reverted emission ship (5e9e167): it
    // assumed every i32 element was an offset-0 rc_alloc object, which was FALSE for the
    // header-less strings/lists/dicts from the direct-bump helpers. Phase A now routes every
    // value producer through $rc_alloc, so these element types are all headered. This matrix
    // is the gate that would have caught the revert: each element type the revert corrupted,
    // read-past-set_at / aliased / stored / match-on-read, must stay byte-identical across
    // interp == wasm == wasm(rc-floor). Authored FIRST, before re-applying the dup/drop
    // emission — a premature/wrong dec flips one of these red.


    /// `compiler.footprint` runs in the WASM backend (staged-JSON host bridge)
    /// and agrees byte-for-byte with the interpreter — a self-hosted package
    /// manager can compute footprints from inside the sandbox.
    #[test]
    fn compiler_footprint_runs_in_the_wasm_backend() {
        let prog = "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"pub fn read_all(d: Dir[Read]) -> String:\\n    d.read(\\\"x\\\")\\n\"))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].contains("Dir[Read]"), "{out:?}");
    }

    /// `compiler.diff` runs in the WASM backend and flags widening exactly as
    /// the interpreter does.
    #[test]
    fn compiler_diff_runs_in_the_wasm_backend() {
        let prog = "import compiler\nfn main(console: Console):\n    let old = \"pub fn pure(x: Int) -> Int:\\n    x\\n\"\n    let new = \"pub fn pure(x: Int, d: Dir) -> Int:\\n    x\\n\"\n    console.print(compiler.diff(old, new))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].contains("\"widened\":true"), "{out:?}");
    }

    /// `compiler.footprint` exposes witchy's own capability analyzer to witchy
    /// programs (the heart of a self-hosted package manager): it returns the
    /// rights-precise footprint as JSON, which composes with `std/json`.
    #[test]
    fn compiler_footprint_exposes_the_analyzer() {
        // The rights-precise footprint comes back as JSON.
        let out = link_run(
            "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"pub fn load(d: Dir[Read]) -> String:\\n    d.read(\\\"x\\\")\\n\"))\n",
        );
        assert!(out[0].contains("\"total\":[\"Dir[Read]\"]"), "total wrong: {}", out[0]);
        assert!(out[0].contains("\"name\":\"load\""), "entry missing: {}", out[0]);
        // The output is valid JSON — it round-trips through `std/json`.
        let composed = link_run(
            "import compiler\nimport json\nfn main(console: Console):\n    match json.decode(compiler.footprint(\"pub fn serve(n: Net) -> Int:\\n    0\\n\")):\n        Ok(doc) -> console.print(\"valid\")\n        Err(e) -> console.print(\"invalid: \" + json.decode_error_message(e))\n",
        );
        assert_eq!(composed, vec!["valid"]);
        // `comptime fn` helpers are not runtime entrypoints and must not widen
        // source-string footprint reports even if their helper signature mentions
        // a capability.
        let comptime_helper = link_run(
            "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"comptime fn helper(d: Dir[Read]) -> Int:\\n    0\\n\\npub fn visible() -> Int:\\n    1\\n\"))\n",
        );
        assert!(
            !comptime_helper[0].contains("Dir[Read]") && !comptime_helper[0].contains("helper"),
            "comptime helper leaked into footprint: {}",
            comptime_helper[0]
        );
        assert!(
            comptime_helper[0].contains("\"name\":\"visible\""),
            "visible function missing: {}",
            comptime_helper[0]
        );
        // Malformed source degrades to an error object, not a crash.
        let bad = link_run(
            "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"fn oops(\"))\n",
        );
        assert!(bad[0].contains("\"error\""), "expected an error object: {}", bad[0]);
    }

    /// `compiler.diff` is the rights-precise block-on-widening gate (the package
    /// manager's core safety check), exposed to witchy as JSON.
    #[test]
    fn compiler_diff_is_the_widening_gate() {
        let diff = |old: &str, new: &str| -> String {
            link_run(&format!(
                "import compiler\nfn main(console: Console):\n    console.print(compiler.diff(\"{old}\", \"{new}\"))\n"
            ))
            .remove(0)
        };
        // A connect-only client that gains `Listen` is a widening (the gate blocks).
        let widen = diff(
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect, Listen]) -> Int:\\n    0\\n",
        );
        assert!(widen.contains("\"widened\":true"), "should widen: {widen}");
        assert!(widen.contains("\"added\":[\"Net[Listen]\"]"), "added wrong: {widen}");
        // The reverse (tightening to connect-only) is a safe narrowing.
        let narrow = diff(
            "pub fn f(n: Net[Connect, Listen]) -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
        );
        assert!(narrow.contains("\"widened\":false"), "should not widen: {narrow}");
        assert!(narrow.contains("\"removed\":[\"Net[Listen]\"]"), "removed wrong: {narrow}");
    }

    /// `compiler.diff` includes build-axis and user-cap axes in the JSON report
    /// so self-hosted gates can explain which axis caused a widening.
    #[test]
    fn compiler_diff_includes_build_and_user_cap_axes() {
        let diff = |old: &str, new: &str| -> String {
            link_run(&format!(
                "import compiler\nfn main(console: Console):\n    console.print(compiler.diff(\"{old}\", \"{new}\"))\n"
            ))
            .remove(0)
        };
        // A pure function gaining a build-time BuildExec is a build-axis widening.
        let build = diff(
            "pub fn pure() -> Int:\\n    0\\n",
            "pub fn pure() -> Int:\\n    0\\npub fn build(e: BuildExec) -> Int:\\n    0\\n",
        );
        assert!(build.contains("\"widened\":true"), "build widening should set widened: {build}");
        assert!(build.contains("\"build_added\":[\"BuildExec\"]"), "build_added should list BuildExec: {build}");
        assert!(build.contains("\"build_removed\":[]"), "build_removed should be empty: {build}");
        // Every diff includes all axes.
        let runtime = diff(
            "pub fn pure() -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
        );
        assert!(runtime.contains("\"build_added\":[]"), "no-build diff should have empty build_added: {runtime}");
        assert!(runtime.contains("\"user_caps_added\":[]"), "no-user-cap diff should have empty user_caps_added: {runtime}");
        assert!(runtime.contains("\"user_caps_removed\":[]"), "no-user-cap diff should have empty user_caps_removed: {runtime}");
    }

    /// (BUG-373) The lockfile `[[rune]]` grammar has exactly ONE parser now:
    /// `toml.decode` + the structured navigation helpers. This exercises the four
    /// contract points the migration must uphold, on BOTH backends:
    ///   - two `[[rune]]` entries stay DISTINCT and in DECLARATION ORDER;
    ///   - a capability array (`runtime_footprint`) is read from the structured
    ///     table via `string_array_field`;
    ///   - a WRONG-TYPED field (`name = 42`) FAILS CLOSED with a kind error rather
    ///     than reading as empty/default;
    ///   - the entry a "verifier" checks and the entry a "resolver" enumerates are
    ///     the SAME decoded model (name+hash+caps agree entry-for-entry).
    #[test]
    fn structured_lockfile_is_the_single_rune_parser() {
        let src = r#"import toml

fn main(console: Console):
    // Two [[rune]] entries; the second repeats the `name` key shape and carries a
    // capability array. Order and repetition must be preserved.
    let lock = "[[rune]]\nname = \"money\"\nhash = \"h-money\"\nruntime_footprint = [\"Console\"]\n\n[[rune]]\nname = \"ledger\"\nhash = \"h-ledger\"\nruntime_footprint = [\"Console\", \"Dir[Read]\"]\n"
    match toml.decode(lock):
        Err(e) -> console.print("decode: " + toml.decode_error_message(e))
        Ok(doc) ->
            match toml.array_of_tables(doc, "rune"):
                Err(e) -> console.print("aot: " + toml.decode_error_message(e))
                Ok(entries) ->
                    console.print("count=${list.length(entries)}")
                    // distinct + ordered: name and hash per entry, in file order.
                    for entry in entries:
                        let name = req(entry, "name")
                        let hash = req(entry, "hash")
                        let caps = caps_of(entry)
                        console.print(name + "@" + hash + " caps=[" + caps + "]")
    // wrong-typed field fails closed (not read as empty).
    let bad = "[[rune]]\nname = 42\n"
    match toml.decode(bad):
        Err(e) -> console.print("decode: " + toml.decode_error_message(e))
        Ok(doc) ->
            match toml.array_of_tables(doc, "rune"):
                Err(e) -> console.print("aot: " + toml.decode_error_message(e))
                Ok(entries) ->
                    for entry in entries:
                        match toml.required_string(entry, "name", "a [[rune]]"):
                            Ok(s) -> console.print("wrongly accepted: " + s)
                            Err(e) -> console.print("fail-closed: " + toml.decode_error_message(e))

fn req(entry: toml.Toml, key: String) -> String:
    match toml.required_string(entry, key, "a [[rune]]"):
        Ok(s) -> s
        Err(e) -> "(err:" + toml.decode_error_message(e) + ")"

fn caps_of(entry: toml.Toml) -> String:
    match toml.string_array_field(entry, "runtime_footprint"):
        Ok(cs) -> list.join(cs, ",")
        Err(e) -> "(err:" + toml.decode_error_message(e) + ")"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "count=2",
                "money@h-money caps=[Console]",
                "ledger@h-ledger caps=[Console,Dir[Read]]",
                "fail-closed: a [[rune]]'s `name` field is not a string (found an integer)",
            ]
        );
    }

    /// `std/semver` (pure witchy) parses `major.minor.patch`, matches the `^`/`~`/
    /// exact/`>=`/`*` constraints (rights matching the Rust resolver's semver),
    /// and picks the highest satisfying version — what dependency resolution needs.
    #[test]
    fn semver_module_parses_and_matches_constraints() {
        let src = r#"import semver

fn main(console: Console):
    console.print(yes(req_matches("^1.2.0", "1.9.9")))
    console.print(yes(req_matches("^1.2.0", "2.0.0")))
    console.print(yes(req_matches("^0.4.0", "0.5.0")))
    console.print(yes(req_matches("^0", "0.9.9")))
    console.print(yes(req_matches("^0", "1.0.0")))
    console.print(yes(req_matches("^0.0", "0.0.9")))
    console.print(yes(req_matches("^0.0", "0.1.0")))
    console.print(yes(req_matches("^0.0.3", "0.0.4")))
    console.print(yes(req_matches("~1.2.0", "1.2.9")))
    console.print(yes(req_matches("~1.2.0", "1.3.0")))
    console.print(yes(req_matches("~1", "1.9.9")))
    console.print(yes(req_matches("~1", "2.0.0")))
    console.print(yes(req_matches("~1.2", "1.2.9")))
    console.print(yes(req_matches("~1.2", "1.3.0")))
    console.print(yes(req_matches(">=1.0.0", "3.0.0")))
    console.print(best_of("^1.2.0"))
    console.print(best_zero("^0"))

fn req_matches(r: String, v: String) -> Bool:
    match semver.parse_req(r):
        Ok(req) -> match semver.parse(v):
            Ok(ver) -> semver.matches(req, ver)
            Err(e) -> false
        Err(e) -> false

fn best_of(r: String) -> String:
    let vs = [semver.version(1, 0, 0), semver.version(1, 2, 0), semver.version(1, 9, 9), semver.version(2, 0, 0)]
    match semver.parse_req(r):
        Ok(req) -> match semver.best(vs, req):
            Some(v) -> semver.format(v)
            None -> "(none)"
        Err(e) -> "err"

fn best_zero(r: String) -> String:
    let vs = [semver.version(0, 0, 0), semver.version(0, 4, 2), semver.version(0, 9, 9), semver.version(1, 0, 0)]
    match semver.parse_req(r):
        Ok(req) -> match semver.best(vs, req):
            Some(v) -> semver.format(v)
            None -> "(none)"
        Err(e) -> "err"

fn yes(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "y", "n", "n", "y", "n", "y", "n", "n", "y", "n", "y", "n", "y",
                "n", "y", "1.9.9", "0.9.9"
            ]
        );
    }

    /// `std/path` does pure '/'-path surgery: base/dir/ext/stem, join (an absolute
    /// right-hand side replaces), and normalize (collapsing `.`/`..`, never
    /// escaping an absolute root, keeping leading `..` when relative).
    #[test]
    fn path_module_components_and_normalize() {
        let src = r#"import path
import option

fn main(console: Console):
    console.print(path.base("a/b/c.txt") + "|" + (path.dir("a/b/c.txt") ?? "<none>"))
    console.print((path.ext("a/b.tar.gz") ?? "<none>") + "|" + path.stem("a/b.tar.gz"))
    console.print("[" + (path.ext(".bashrc") ?? "<none>") + "]|" + path.base("a/b/"))
    console.print((path.dir("c") ?? "<none>") + "|" + (path.ext("README") ?? "<none>"))
    console.print(path.join("a/b", "c") + "|" + path.join("a", "/x"))
    console.print(path.normalize("a/./b/../c/") + "|" + path.normalize("/a/b/../../../x"))
    console.print(path.normalize("../a/../../b"))
"#;
        assert_eq!(
            link_run(src),
            vec![
                "c.txt|a/b",
                "gz|b.tar",
                "[<none>]|b",
                "<none>|<none>",
                "a/b/c|/x",
                "a/c|/x",
                "../../b",
            ]
        );
    }

    /// The committed `spec/stdlib.md` must match what `witchy doc` generates from
    /// the std sources — so a std module change that isn't re-documented fails
    /// loudly. Regenerate with: `witchy doc std/*.witchy > spec/stdlib.md`.
    #[test]
    fn stdlib_docs_are_current() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir("std")
            .expect("read std/")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("witchy"))
            .collect();
        files.sort();
        let mut generated = String::from("# API reference\n\n");
        for f in &files {
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap();
            let src = std::fs::read_to_string(f).unwrap();
            generated.push_str(&crate::doc::render(stem, &src).expect("render"));
        }
        let committed = std::fs::read_to_string("spec/stdlib.md").expect("read spec/stdlib.md");
        for (i, (g, c)) in generated.lines().zip(committed.lines()).enumerate() {
            assert_eq!(
                g,
                c,
                "spec/stdlib.md is stale at line {} — regenerate with `witchy doc std/*.witchy > spec/stdlib.md`",
                i + 1
            );
        }
        assert_eq!(
            generated.lines().count(),
            committed.lines().count(),
            "spec/stdlib.md length differs — regenerate with `witchy doc std/*.witchy > spec/stdlib.md`"
        );
    }

    /// `std/encoding` — hex + base64 over UTF-8 bytes (native, like crypto),
    /// matching the standard vectors incl. padding, and round-tripping multibyte
    /// UTF-8.
    #[test]
    fn encoding_module_hex_and_base64() {
        let src = r#"import encoding

fn main(console: Console):
    console.print(encoding.hex_encode("hello"))
    console.print(encoding.hex_decode("68656c6c6f").unwrap_or("?"))
    console.print(encoding.base64_encode("Man"))
    console.print(encoding.base64_encode("Ma"))
    console.print(encoding.base64_decode("aGVsbG8=").unwrap_or("?"))
    console.print(yn(encoding.base64_decode(encoding.base64_encode("witchy! 🧙")).unwrap_or("?") == "witchy! 🧙"))

fn yn(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec!["68656c6c6f", "hello", "TWFu", "TWE=", "hello", "y"]
        );
    }

    /// The `examples/time_and_encoding/src/time_and_encoding.witchy` showcase runs: a formatted civil
    /// date and base64/hex of a multibyte-UTF-8 payload, round-tripped — its
    /// footprint is just Console.
    #[test]
    fn time_and_encoding_example_runs() {
        assert_eq!(
            crate::execute_file("examples/time_and_encoding/src/time_and_encoding.witchy", Vec::new()).unwrap(),
            vec![
                "date:    2026-05-28T20:26:40Z (Thursday)",
                "layout:  Thursday, May 28 2026 at 20:26",
                "parsed:  2026-06-08T20:30:00Z",
                "checked: day 30 is out of range for 2026-2",
                "base64:  d2l0Y2h5IPCfp5k=",
                "hex:     77697463687920f09fa799",
                "decoded: witchy 🧙",
            ]
        );
        let src = std::fs::read_to_string("examples/time_and_encoding/src/time_and_encoding.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// `examples/calc/src/calc.witchy` — a recursive-descent arithmetic evaluator — honors
    /// operator precedence and left-associativity, and reports division-by-zero
    /// and parse errors through `Result`. A pure (Console-only) tour of recursive
    /// enums + pattern matching.
    #[test]
    fn calc_example_evaluates_with_precedence_and_errors() {
        assert_eq!(
            crate::execute_file("examples/calc/src/calc.witchy", Vec::new()).unwrap(),
            vec![
                "2 + 3 * 4       => 14",
                "(2 + 3) * 4     => 20",
                "100 - 2 - 3     => 95",
                "2 * (10 - 1)    => 18",
                "8 / (4 - 4)     => error: division by zero",
                "2 * (3 +        => error: unexpected end of input",
            ]
        );
        let src = std::fs::read_to_string("examples/calc/src/calc.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// `examples/wrap/src/wrap.witchy` — greedy word wrapping — packs space-separated
    /// words onto lines within a column width, breaking before overflow, and
    /// frames each padded line. Pure string handling; agrees on both backends.
    #[test]
    fn wrap_example_greedily_wraps_to_width() {
        assert_eq!(
            crate::execute_file("examples/wrap/src/wrap.witchy", Vec::new()).unwrap(),
            vec![
                "wrapped to 20 columns:",
                "| The quick brown fox  |",
                "| jumps over the lazy  |",
                "| dog and then keeps   |",
                "| on running far away  |",
            ]
        );
    }

    /// `examples/dijkstra/src/dijkstra.witchy` — single-source shortest paths in a weighted
    /// directed graph — settles the nearest node, relaxes edges, then prints
    /// every distance and one reconstructed path. Returns a tuple of parallel
    /// arrays, so it also covers tuple-return + `let (a, b) =` on both backends.
    #[test]
    fn dijkstra_example_finds_shortest_paths() {
        assert_eq!(
            crate::execute_file("examples/dijkstra/src/dijkstra.witchy", Vec::new()).unwrap(),
            vec![
                "shortest distances from A:",
                "  A = 0",
                "  B = 3",
                "  C = 1",
                "  D = 4",
                "  E = 7",
                "path to E: A -> C -> B -> D -> E",
            ]
        );
    }

    /// `examples/queens/src/queens.witchy` — N-queens by backtracking — counts all 92
    /// solutions for the 8x8 board and renders the first (column-order DFS). Deep
    /// recursion with an early-exit search; agrees on both backends.
    #[test]
    fn queens_example_counts_and_renders_first_board() {
        assert_eq!(
            crate::execute_file("examples/queens/src/queens.witchy", Vec::new()).unwrap(),
            vec![
                "8-queens solutions: 92",
                "first solution:",
                "Q.......",
                "....Q...",
                ".......Q",
                ".....Q..",
                "..Q.....",
                "......Q.",
                ".Q......",
                "...Q....",
            ]
        );
    }

    /// `examples/regex/src/regex_demo.witchy` — a tiny K&P-style regex matcher (literals, `.`,
    /// `*`, `^`, `$`) — matches a battery of pattern/text pairs. Every step is a
    /// two-`list.at(..)` character comparison, so it stresses content comparison on
    /// both backends.
    #[test]
    fn regex_example_matches_literals_dot_star_anchors() {
        assert_eq!(
            crate::execute_file("examples/regex/src/regex_demo.witchy", Vec::new()).unwrap(),
            vec![
                "/abc/     \"abc\"           match",
                "/a.c/     \"axc\"           match",
                "/a.c/     \"ac\"            no match",
                "/a*b/     \"aaab\"          match",
                "/a*b/     \"b\"             match",
                "/^hello/  \"hello world\"   match",
                "/world$/  \"hello world\"   match",
                "/^a.*z$/  \"abcz\"          match",
                "/^a.*z$/  \"abc\"           no match",
            ]
        );
    }

    /// (BUG-251, RFC-0047) A hand-written `impl PartialEq` is honored INSIDE
    /// containers on the compiled backend too — a `List`/`Option`/tuple of a
    /// custom-eq type compares by that impl, not by structural bytes. The
    /// case-insensitive impl proves the user fn decided the answer ("X" == "x").
    #[test]
    fn custom_partial_eq_inside_containers_backends_agree() {
        let src = "type CI:\n    s: String\n\nimpl PartialEq for CI:\n    fn eq(self, other: CI) -> Bool:\n        self.s.to_lower() == other.s.to_lower()\n\nfn main(console: Console):\n    let la = [CI(s: \"X\")]\n    let lb = [CI(s: \"x\")]\n    console.print(\"${la == lb}\")\n    let oa: Option(CI) = Some(CI(s: \"Y\"))\n    let ob: Option(CI) = Some(CI(s: \"y\"))\n    console.print(\"${oa == ob}\")\n    let ta = (CI(s: \"Z\"), 1)\n    let tb = (CI(s: \"z\"), 1)\n    console.print(\"${ta == tb}\")\n";
        let want = ["true", "true", "true"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-295, spec §6) An irrefutable `if let`/`while let` (Var or Wildcard) is
    /// accepted, consistently with the already-accepted irrefutable TUPLE form. A
    /// genuine duplicate arm still errors (dead-code detection preserved).
    #[test]
    fn irrefutable_if_let_while_let_accepted_consistently() {
        let iflet = "fn main(console: Console):\n    if let x = 3:\n        console.print(\"${x}\")\n";
        assert_eq!(link_run(iflet), ["3"], "interp if-let");
        assert_eq!(wasm_run(iflet), ["3"], "wasm if-let");
        let whilelet = "fn main(console: Console):\n    var n = 0\n    while let x = 3:\n        n = n + x\n        if n >= 6:\n            break\n    console.print(\"${n}\")\n";
        assert_eq!(link_run(whilelet), ["6"], "interp while-let");
        assert_eq!(wasm_run(whilelet), ["6"], "wasm while-let");
        typeck::check_str("fn main(console: Console):\n    if let _ = 3:\n        console.print(\"m\")\n").expect("if let _ ok");
        typeck::check_str("fn main(console: Console):\n    let p = (1, 2)\n    if let (a, b) = p:\n        console.print(\"${a + b}\")\n").expect("tuple if-let ok");
        assert!(typeck::check_str("fn f(d: Duration) -> Int:\n    match d:\n        1s -> 1\n        1s -> 2\n        _ -> 0\n\nfn main(console: Console):\n    console.print(\"${f(1s)}\")\n").is_err(), "duplicate arm must still error");
    }

    /// (BUG-335, spec §16) `main` may return only Nil/Int/Float; a String/Bool/List
    /// return is a CHECK-TIME error (the interpreter echoes it but the compiled run
    /// wrapper drops it — a silent divergence, now rejected loud by construction).
    #[test]
    fn off_spec_main_return_is_check_error() {
        assert!(typeck::check_str("fn main(console: Console) -> String:\n    \"oops\"\n").is_err(), "String main rejected");
        assert!(typeck::check_str("fn main(console: Console) -> Bool:\n    true\n").is_err(), "Bool main rejected");
        assert!(typeck::check_str("fn main(console: Console) -> List(Int):\n    [1, 2]\n").is_err(), "List main rejected");
        typeck::check_str("fn main(console: Console) -> Int:\n    0\n").expect("Int main ok");
        typeck::check_str("fn main(console: Console) -> Float:\n    2.5\n").expect("Float main ok");
        typeck::check_str("fn main(console: Console):\n    console.print(\"hi\")\n").expect("Nil main ok");
    }

    /// The `std/regex` toolkit — greedy quantifiers, escapes (`\d`/`\w`/`\s` and
    /// literal metacharacters), character classes with ranges and negation, and
    /// the span-based API (`find`/`find_all`/`extract`/`replace_all`/`split`) —
    /// agrees on both backends, including the `Option((Int, Int))` span payload.
    #[test]
    fn regex_module_toolkit_agrees_on_both_backends() {
        let src = "import regex\n\nfn main(console: Console):\n    console.print(\"${regex.matches(\"h.llo\", \"say hello\")}\")\n    console.print(\"${regex.matches(\"^\\\\d+$\", \"12345\")}\")\n    console.print(\"${regex.matches(\"^\\\\d+$\", \"12a45\")}\")\n    console.print(\"${regex.extract(\"\\\\d+\", \"a1b22c333\")}\")\n    console.print(regex.replace_all(\"\\\\s+\", \"too   many    spaces\", \" \"))\n    console.print(\"${regex.split(\",\\\\s*\", \"a, b,c\")}\")\n    console.print(\"${regex.matches(\"[a-f]+\", \"deadbeef\")}\")\n    console.print(\"${regex.matches(\"^[^0-9]+$\", \"abc\")}\")\n    console.print(\"${regex.find(\"a+\", \"caat\")}\")\n    console.print(\"${regex.matches(\"\\\\w+@\\\\w+\\\\.\\\\w+\", \"mail me: a_b@example.com\")}\")\n    console.print(regex.replace_all(\"[0-9]+\", \"r2d2\", \"#\"))\n";
        let want: Vec<String> = [
            "true",
            "true",
            "false",
            "[1, 22, 333]",
            "too many spaces",
            "[a, b, c]",
            "true",
            "true",
            "Some((1, 3))",
            "true",
            "r#d#",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// (BUG-186/RFC-0044) An invalid regex pattern is a loud error, not the same
    /// result as a valid regex with no matches. That keeps the module docs, native
    /// helper, and compiled host import on one contract.
    #[test]
    fn regex_invalid_pattern_is_loud_on_both_backends() {
        let src = "import regex\n\nfn main(console: Console):\n    console.print(\"${regex.matches(\"[\", \"x\")}\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp_err = interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect_err("interpreter must reject invalid regex syntax")
            .to_string();
        assert!(interp_err.contains("invalid regex pattern `[`"), "{interp_err}");

        let bytes = codegen::compile_module_binary(&linked)

            .expect_lowered("the binary path lowers regex");
        let wasm_err = crate::run_wasm_bytes(&bytes)
            .expect_err("WASM must reject invalid regex syntax")
            .to_string();
        assert!(wasm_err.contains("invalid regex pattern `[`"), "{wasm_err}");
    }

    /// Alternation `a|b` and grouping `(...)` — which the old hand-rolled engine
    /// silently failed to match — now work (the `regex` crate), identically on
    /// both backends, including grouped extract.
    #[test]
    fn regex_alternation_and_groups_agree_on_both_backends() {
        let src = "import regex\n\nfn main(console: Console):\n    console.print(\"${regex.matches(\"cat|dog\", \"I have a dog\")}\")\n    console.print(\"${regex.matches(\"(cat|dog)s?\", \"cats\")}\")\n    console.print(\"${regex.extract(\"(foo|bar)\", \"foo bar baz\")}\")\n    console.print(regex.replace_all(\"(a|b)+\", \"abab x\", \"Z\"))\n    console.print(\"${regex.find(\"(cat|dog)\", \"a dog\")}\")\n";
        let want: Vec<String> = ["true", "true", "[foo, bar]", "Z x", "Some((2, 5))"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `examples/brainfuck/src/brainfuck.witchy` — a full brainfuck interpreter — runs the
    /// canonical "Hello World!" program and a second that prints 'A', building
    /// output by indexing a printable-ASCII literal (no chr/ord builtin). The
    /// instruction dispatch compares `list.at(code, pc)` against operator literals,
    /// so it's another both-backends guard for content comparison.
    #[test]
    fn brainfuck_example_runs_hello_world() {
        assert_eq!(
            crate::execute_file("examples/brainfuck/src/brainfuck.witchy", Vec::new()).unwrap(),
            vec!["Hello World!", "A"]
        );
    }

    /// `examples/diff/src/diff.witchy` — an LCS line diff — fills the longest-common-
    /// subsequence table and backtracks into unchanged/removed/added lines. The
    /// backtrack compares `list.at(old, i) == list.at(new, j)` (two `List(String)` element
    /// reads), so it also guards content comparison on both backends.
    #[test]
    fn diff_example_emits_lcs_line_diff() {
        assert_eq!(
            crate::execute_file("examples/diff/src/diff.witchy", Vec::new()).unwrap(),
            vec![
                "  apple",
                "- banana",
                "  cherry",
                "  date",
                "+ elderberry",
                "  fig",
            ]
        );
    }

    /// `examples/rpn/src/rpn.witchy` — a stack-machine reverse-Polish calculator — folds
    /// tokens through an operand stack and reports underflow / division-by-zero
    /// through `Result`. Pure (Console), both backends.
    #[test]
    fn rpn_example_evaluates_postfix_with_a_stack() {
        assert_eq!(
            crate::execute_file("examples/rpn/src/rpn.witchy", Vec::new()).unwrap(),
            vec![
                "3 4 +               => 7",
                "5 1 2 + 4 * + 3 -   => 14",
                "10 2 /              => 5",
                "1 0 /               => error: division by zero",
                "1 +                 => error: stack underflow at `+`",
            ]
        );
    }

    /// `examples/maze/src/maze.witchy` — BFS shortest path through a grid maze, with a
    /// `prev` Dict for path reconstruction. Pure (Console); interpreter-hosted.
    #[test]
    fn maze_example_finds_shortest_path_by_bfs() {
        let out = crate::execute_file("examples/maze/src/maze.witchy", Vec::new())
            .unwrap()
            .join("\n");
        assert!(out.contains("shortest path: 14 steps"), "distance: {out}");
        assert!(
            out.contains("#S#***# #") && out.contains("### ###*#"),
            "route marked: {out}"
        );
    }

    /// `examples/traits/src/traits.witchy` — defines a custom `Shape` trait, implements it for
    /// three types, and dispatches generically (`where s: Shape`). Monomorphized,
    /// so it runs identically on both backends.
    #[test]
    fn traits_example_dispatches_a_custom_trait() {
        assert_eq!(
            crate::execute_file("examples/traits/src/traits.witchy", Vec::new()).unwrap(),
            vec![
                "square with area 25",
                "rectangle with area 12",
                "right triangle with area 12",
                "total of three squares: 29",
            ]
        );
    }

    /// `examples/sudoku/src/sudoku.witchy` — a backtracking solver over immutable boards —
    /// solves the canonical puzzle to its unique solution. Pure (Console),
    /// recursion + Option-backtracking heavy.
    #[test]
    fn sudoku_example_solves_by_backtracking() {
        let out = crate::execute_file("examples/sudoku/src/sudoku.witchy", Vec::new())
            .unwrap()
            .join("\n");
        assert!(
            out.contains("solved:\n534678912\n672195348\n198342567\n859761423"),
            "unique solution: {out}"
        );
        let src = std::fs::read_to_string("examples/sudoku/src/sudoku.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// `examples/life/src/life.witchy` — Conway's Game of Life over a `List(List(Bool))` —
    /// evolves a glider through its phases by the B3/S23 rule. Pure (Console),
    /// nested-list heavy, and identical on both backends.
    #[test]
    fn life_example_evolves_a_glider() {
        let out = crate::execute_file("examples/life/src/life.witchy", Vec::new())
            .unwrap()
            .join("\n");
        // Generation 0 is the seeded glider.
        assert!(
            out.contains("generation 0:\n.#......\n..#.....\n###....."),
            "seed glider: {out}"
        );
        // After 3 steps it has drifted down-and-right into its next phase.
        assert!(
            out.contains("generation 3:\n........\n.#......\n..##....\n.##....."),
            "evolved glider: {out}"
        );
        let src = std::fs::read_to_string("examples/life/src/life.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// Regression (found by `examples/calc/src/calc.witchy` via the both-backends invariant):
    /// comparing a String whose type isn't locally tracked — a List(String)
    /// element via `at` — to a literal must be a *structural* `$str_eq` on the
    /// WASM backend, not a pointer compare, with the literal on either side.
    #[test]
    fn wasm_string_eq_uses_str_eq_when_literal_on_either_side() {
        let src = "fn main(console: Console):\n    let cs = [\"a\", \" \", \"z\"]\n    console.print(if list.at(cs, 1) == \" \": \"eq\" else: \"ne\")\n    console.print(if \"a\" == list.at(cs, 0): \"eq\" else: \"ne\")\n    console.print(if list.at(cs, 0) == \"z\": \"eq\" else: \"ne\")\n";
        let want = vec!["eq".to_string(), "eq".to_string(), "ne".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// Comparing two `list.at(list, i)` results — where neither operand is a literal —
    /// must compare String *content* on WASM, not pointers. The list holds two
    /// runtime-built (concatenated) strings with equal content but distinct heap
    /// addresses, so a pointer comparison would wrongly report "ne". Codegen now
    /// carries a `List(String)`'s element value type to `list.at(...)`, so `==` lowers
    /// to `$str_eq`. (Regression for the run-length-encoding parity divergence.)
    #[test]
    fn wasm_string_eq_on_two_at_results_compares_content() {
        let src = "fn main(console: Console):\n    let a = \"x\" + \"y\"\n    let b = \"x\" + \"y\"\n    let xs = [a, b, \"zz\"]\n    console.print(if list.at(xs, 0) == list.at(xs, 1): \"eq\" else: \"ne\")\n    console.print(if list.at(xs, 0) == list.at(xs, 2): \"eq\" else: \"ne\")\n";
        let want = vec!["eq".to_string(), "ne".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// An *unbounded* generic function that compares its type-variable values
    /// (`x == target`) must compare String CONTENT on WASM, not pointers. The
    /// WASM backend monomorphizes the call on the concrete element type
    /// (`count_eq__String`), so `==` lowers to `$str_eq`. The strings are built at
    /// runtime (distinct pointers, equal content) so a pointer compare would give
    /// the wrong count. (Regression for the generic-`==`-on-non-primitives gap.)
    #[test]
    fn wasm_monomorphizes_generic_equality_on_strings() {
        let src = "fn count_eq(xs: List(a), target: a) -> Int:\n    var n = 0\n    for x in xs:\n        if x == target:\n            n = n + 1\n    n\n\nfn b(s: String) -> String:\n    s + \"\"\n\nfn main(console: Console):\n    console.print(\"${count_eq([b(\"aa\"), b(\"bb\"), b(\"aa\")], b(\"aa\"))}\")\n";
        let want = vec!["2".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// Every ```witchy code block in the documentation must be a real program:
    /// it parses, links, and type-checks; and when it defines a `main` whose
    /// footprint needs nothing beyond Console, it RUNS on both backends and the
    /// outputs must agree. Docs that drift from the language break the build.
    #[test]
    fn documentation_examples_are_valid() {
        let files = doc_markdown_files();

        let results: Vec<(usize, usize)> = std::thread::scope(|s| {
            let handles: Vec<_> = files.iter().map(|file| {
                s.spawn(move || {
                    let mut checked = 0usize;
                    let mut ran = 0usize;
                    let Ok(text) = std::fs::read_to_string(file) else { return (0, 0) };
                    for (idx, snippet) in extract_witchy_blocks(&text).into_iter().enumerate() {
                        let context = format!("{}: ```witchy block #{}", file.display(), idx + 1);
                        let module = parser::parse_module(&snippet)
                            .unwrap_or_else(|e| panic!("{context} fails to parse: {e:?}\n---\n{snippet}"));
                        let linked = crate::pipeline::link(vec![("main".into(), module)], "main")
                            .unwrap_or_else(|e| panic!("{context} fails to link: {e}\n---\n{snippet}"));
                        typeck::check(&linked)
                            .unwrap_or_else(|e| panic!("{context} fails to type-check: {e}\n---\n{snippet}"));
                        checked += 1;

                        let has_main = linked
                            .items
                            .iter()
                            .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
                        let has_actor = false;
                        let reads_argv = linked.items.iter().any(|it| {
                            matches!(it, ast::Item::Function(f) if f.name == "main"
                                && f.params.iter().any(|p| matches!(&p.ty,
                                    Some(ast::Type::Named(n, args)) if n == "List"
                                        && matches!(args.first(),
                                            Some(ast::Type::Named(s, _)) if s == "String"))))
                        });
                        let footprint = crate::capabilities::analyze(&linked);
                        let console_only = footprint.total.keys().all(|k| *k == "Console");
                        if has_main && console_only && !has_actor && !reads_argv {
                            let bytes = codegen::compile_module_binary(&linked)
                                .expect_lowered(&format!("{context} compiles to WASM"));
                            let interp =
                                interpreter::run_module(linked, std::path::Path::new("."), Vec::new())
                                    .unwrap_or_else(|e| panic!("{context} fails on the interpreter: {e}"));
                            let compiled = crate::run_wasm_bytes(&bytes)
                                .unwrap_or_else(|e| panic!("{context} fails on WASM: {e}"));
                            assert_eq!(interp, compiled, "{context}: the backends DIVERGE");
                            ran += 1;
                        }
                    }
                    (checked, ran)
                })
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let checked: usize = results.iter().map(|(c, _)| c).sum();
        let ran: usize = results.iter().map(|(_, r)| r).sum();
        assert!(checked >= 20, "expected the docs to carry many checked examples, found {checked}");
        assert!(ran >= 5, "expected several runnable doc examples, found {ran}");
    }

    /// Pull the contents of every fenced block tagged exactly `witchy`.
    fn extract_witchy_blocks(markdown: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut current: Option<String> = None;
        for line in markdown.lines() {
            match &mut current {
                None if line.trim_end() == "```witchy" => current = Some(String::new()),
                Some(body) => {
                    if line.trim_end() == "```" {
                        blocks.push(current.take().unwrap());
                    } else {
                        body.push_str(line);
                        body.push('\n');
                    }
                }
                None => {}
            }
        }
        blocks
    }

    /// The Markdown docs whose ```` ```witchy ```` blocks are validated + classified: the
    /// root docs, `spec/`, and `book/src/` (sorted for a stable manifest). Shared by
    /// `documentation_examples_are_valid` and the manifest generator so both walk one list.
    fn doc_markdown_files() -> Vec<std::path::PathBuf> {
        let mut files: Vec<std::path::PathBuf> = vec![
            "README.md".into(),
            "CONTRIBUTING.md".into(),
            "examples/README.md".into(),
        ];
        for dir in ["spec", "book/src"] {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut md: Vec<_> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
                .collect();
            md.sort();
            files.extend(md);
        }
        files
    }

    /// (RFC-0041 Phase 3) Generate the runnable-example classification manifest as pretty
    /// JSON. Reuses the SAME classifier as `documentation_examples_are_valid`: per doc
    /// `witchy` block, whether it is `runnable` (a `Console`-only `main`, no actor/argv) and
    /// `console_only`, its capability `footprint`, and — for runnable blocks — the interpreter
    /// `output`. This is the single source of truth the runnable book reads, so the browser
    /// never re-derives classification (which could disagree with the authoritative Rust one).
    fn generate_examples_manifest() -> String {
        let files = doc_markdown_files();
        let per_file: Vec<Vec<serde_json::Value>> = std::thread::scope(|s| {
            let handles: Vec<_> = files.iter().map(|file| {
                s.spawn(move || {
                    let mut file_entries = Vec::new();
                    let Ok(text) = std::fs::read_to_string(file) else { return file_entries };
                    for (idx, snippet) in extract_witchy_blocks(&text).into_iter().enumerate() {
                        let context = format!("{}: ```witchy block #{}", file.display(), idx + 1);
                        let module = parser::parse_module(&snippet)
                            .unwrap_or_else(|e| panic!("{context} fails to parse: {e:?}"));
                        let mut footprint_module = module.clone();
                        crate::comptime::expand("main", &mut footprint_module)
                            .unwrap_or_else(|e| panic!("{context} fails compile-time expansion: {e}"));
                        let linked = crate::pipeline::link(vec![("main".into(), module)], "main")
                            .unwrap_or_else(|e| panic!("{context} fails to link: {e}"));
                        typeck::check(&linked).unwrap_or_else(|e| panic!("{context} fails to type-check: {e}"));

                        let has_main = linked
                            .items
                            .iter()
                            .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
                        let reads_argv = linked.items.iter().any(|it| {
                            matches!(it, ast::Item::Function(f) if f.name == "main"
                                && f.params.iter().any(|p| matches!(&p.ty,
                                    Some(ast::Type::Named(n, args)) if n == "List"
                                        && matches!(args.first(),
                                            Some(ast::Type::Named(s, _)) if s == "String"))))
                        });
                        let fp = crate::capabilities::analyze(&footprint_module);
                        let console_only = fp.total.keys().all(|k| *k == "Console");
                        let uses_workers = footprint_module.imports.iter().any(|m| m == "vm")
                            || linked.imports.iter().any(|m| m == "vm");
                        let runnable = has_main && console_only && !reads_argv && !uses_workers;
                        // (RFC-0091) `browser_runnable`: whether the OPT-IN playground host
                        // (`instantiate(..., { capabilities })` in web/witchy-runtime) can run
                        // this block. That host backs exactly the browser-honest capability
                        // families — `Console`, `Clock` (real wall/monotonic time), `Env` (an
                        // empty/page-supplied map), and `Dir` (a per-run in-memory tree) — and
                        // leaves `Net`/`Exec`/`Secret`/argv/workers denied by omission. This is a
                        // SUPERSET of `runnable` (which stays Console-only + output-pinned): a
                        // `browser_runnable`-but-not-`runnable` block has NO pinned `output`,
                        // because its result depends on real time or on host-supplied Dir/Env
                        // fixtures rather than a deterministic oracle run. The docs cell uses
                        // this to offer a Run button (empty fixtures) without claiming a golden.
                        const BROWSER_CAP_FAMILIES: &[&str] = &["Console", "Clock", "Env", "Dir"];
                        let browser_caps_ok = fp
                            .total
                            .keys()
                            .all(|k| BROWSER_CAP_FAMILIES.iter().any(|f| f == k));
                        let browser_runnable =
                            has_main && browser_caps_ok && !reads_argv && !uses_workers;
                        let footprint: Vec<String> = fp
                            .total
                            .iter()
                            .map(|(cap, rights)| {
                                if rights.is_empty() {
                                    (*cap).to_string()
                                } else {
                                    format!("{}[{}]", cap, rights.iter().copied().collect::<Vec<_>>().join(","))
                                }
                            })
                            .collect();
                        let output: Vec<String> = if runnable {
                            interpreter::run_module(linked, std::path::Path::new("."), Vec::new())
                                .unwrap_or_else(|e| panic!("{context} fails on the interpreter: {e}"))
                        } else {
                            Vec::new()
                        };
                        file_entries.push(serde_json::json!({
                            "file": file.display().to_string(),
                            "block": idx + 1,
                            "runnable": runnable,
                            "browser_runnable": browser_runnable,
                            "console_only": console_only,
                            "expect_error": false,
                            "footprint": footprint,
                            "output": output,
                        }));
                    }
                    file_entries
                })
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let entries: Vec<serde_json::Value> = per_file.into_iter().flatten().collect();
        serde_json::to_string_pretty(&serde_json::Value::Array(entries)).unwrap() + "\n"
    }

    /// (RFC-0041 Phase 3) `book/examples.json` — the committed classification manifest — must
    /// match what the classifier produces, so the runnable book can never show a reader an
    /// output the toolchain would not. Freshness-gated exactly like `stdlib_docs_are_current`.
    /// Regenerate with: `BLESS_EXAMPLES=1 cargo test -p witchy book_examples_manifest_is_current`.
    #[test]
    fn book_examples_manifest_is_current() {
        let fresh = generate_examples_manifest();
        let path = std::path::Path::new("book/examples.json");
        if std::env::var("BLESS_EXAMPLES").is_ok() {
            std::fs::write(path, &fresh).expect("write book/examples.json");
            return;
        }
        let committed = std::fs::read_to_string(path).unwrap_or_else(|_| {
            panic!("book/examples.json missing — regenerate: BLESS_EXAMPLES=1 cargo test book_examples_manifest_is_current")
        });
        assert_eq!(
            committed, fresh,
            "book/examples.json is stale — regenerate: BLESS_EXAMPLES=1 cargo test book_examples_manifest_is_current"
        );
    }

    /// The in-language test framework: `witchy test` discovers zero-parameter
    /// `test_*` functions, a test passes by returning and fails by aborting
    /// (std/testing's assertions report a message). Capability-free by
    /// construction.
    #[test]
    fn in_language_test_framework_runs_and_reports() {
        let path = std::env::temp_dir().join(format!("witchy_testfw_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "import testing\n\nfn double(n: Int) -> Int:\n    n * 2\n\nfn test_double():\n    testing.assert_int_eq(double(21), 42)\n\nfn test_strings():\n    testing.assert_eq(\"a\" + \"b\", \"ab\")\n    testing.assert_ne(\"a\", \"b\")\n\nfn test_broken():\n    testing.assert(1 > 2, \"deliberately wrong\")\n",
        )
        .unwrap();
        let (passed, failed) = crate::run_tests_in_file(path.to_str().unwrap()).expect("run");
        assert_eq!(passed.len(), 2, "two passing tests: {passed:?}");
        assert_eq!(failed.len(), 1, "one failing test: {failed:?}");
        assert!(failed[0].0.ends_with("test_broken"));
        assert!(
            failed[0].1.contains("deliberately wrong"),
            "failure carries the assertion message: {}",
            failed[0].1
        );
        let _ = std::fs::remove_file(&path);
    }

    /// `fail` is the loud abort on BOTH backends: a runtime error in the
    /// interpreter, a trap in compiled code.
    #[test]
    fn fail_aborts_on_both_backends() {
        let src = "fn main(console: Console):\n    console.print(\"before\")\n    fail(\"boom\")\n    console.print(\"after\")\n";
        let err = interpreter::run(src).expect_err("interpreter must abort");
        assert!(err.message.contains("boom"));
        let module = parser::parse_module(src).expect("parse");
        // `fail()` lowers on the binary path: route the message through
        // `__witchy_abort`, then `unreachable`.
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("fail() lowers on the binary path");
        assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on fail()");
    }

    /// (RFC-0045) The message parity property: when the interpreter aborts, the
    /// compiled backend must abort with the SAME message CORE — not merely "both
    /// error". Covers each routed abort class: `fail(msg)` (dynamic), list-index
    /// OOB and `string.to_int` junk (static + dynamic data), and NaN ordering
    /// (static). This is the differential gate's semantics made a unit test: a
    /// compiled trap at the wrong site or for the wrong reason would diverge here.
    #[test]
    fn abort_messages_match_across_backends() {
        // Each case: (program, expected message core the interpreter produces).
        let cases: &[(&str, &str)] = &[
            (
                "fn main(console: Console):\n    fail(\"the reason\")\n",
                "the reason",
            ),
            (
                "import list\nfn main(console: Console):\n    let xs = [1, 2]\n    console.print(\"${list.at(xs, 5)}\")\n",
                "list index 5 out of bounds (length 2)",
            ),
            (
                "fn main(console: Console):\n    console.print(\"${\"junk\".to_int()}\")\n",
                "cannot parse `junk` as an Int",
            ),
            (
                "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(\"${nan < 1.0}\")\n",
                "cannot compare NaN",
            ),
        ];
        // (RFC-0045 / latent i32-wrap hole) A list index beyond i32 range must
        // still abort with its TRUE value on both backends — `$list_at` now checks
        // in i64, so a huge index can't wrap to an in-range i32 and read a bogus
        // slot. `4294967297` = 2^32 + 1 (wraps to 1 as i32) is the regression seed.
        let wrap_src = "import list\nfn main(console: Console):\n    let xs = [10, 20]\n    console.print(\"${list.at(xs, 4294967297)}\")\n";
        {
            let ierr = interpreter::run(wrap_src).expect_err("interpreter must abort on the huge index");
            assert!(
                ierr.message.ends_with("list index 4294967297 out of bounds (length 2)"),
                "interpreter: {}",
                ierr.message
            );
            let linked = resolve_std_src(wrap_src);
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("binary");
            let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort on the huge index");
            assert_eq!(
                cerr,
                format!("runtime error: {}", ierr.message),
                "compiled must report the TRUE index, not a wrapped one"
            );
        }

        for (src, want_core) in cases {
            // Interpreter (the oracle): its full message ends with the core.
            let ierr = interpreter::run(src).expect_err("interpreter must abort");
            assert!(
                ierr.message.ends_with(want_core),
                "interpreter core mismatch: got `{}`, want suffix `{want_core}`",
                ierr.message
            );
            // Compiled: the routed abort surfaces `runtime error: <core>` via the
            // host `bail!` (root cause). It must equal the interpreter's core.
            let linked = resolve_std_src(src);
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
            assert_eq!(
                cerr,
                format!("runtime error: {}", ierr.message),
                "compiled abort mismatch for src:\n{src}"
            );
        }
    }

    /// (RFC-0044 rule 3) The pure-witchy std contract-violation aborts: a bad
    /// argument that used to silently default now aborts, with the SAME message
    /// on both backends (they run the identical std source; RFC-0045 routes the
    /// message). Each case pairs a program with its message core.
    #[test]
    fn std_contract_violations_abort_on_both_backends() {
        let cases: &[(&str, &str)] = &[
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.factorial(-5)}\")\n",
                "math.factorial: `-5` is negative (expected n >= 0)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.pow(2, -1)}\")\n",
                "math.pow: exponent `-1` is negative (expected exp >= 0)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.isqrt(-5)}\")\n",
                "math.isqrt: `-5` is negative (expected n >= 0)",
            ),
            (
                "import time\nfn main(console: Console):\n    console.print(\"${time.days_in_month(2026, 13)}\")\n",
                "time.days_in_month: month `13` is out of range (expected 1..12)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(math.to_base(10, 17))\n",
                "math.to_base: base `17` is outside 2..16",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(math.to_base(10, 1))\n",
                "math.to_base: base `1` is outside 2..16",
            ),
            (
                "fn main(console: Console):\n    console.print(\"x\".pad_left(3, \"\"))\n",
                "string.pad_left: empty `fill` cannot pad to width 3",
            ),
            (
                "fn main(console: Console):\n    console.print(\"x\".pad_right(3, \"\"))\n",
                "string.pad_right: empty `fill` cannot pad to width 3",
            ),
            (
                "fn main(console: Console):\n    console.print(\"x\".center(3, \"\"))\n",
                "string.center: empty `fill` cannot pad to width 3",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.clamp(5, 10, 0)}\")\n",
                "math.clamp: lo `10` exceeds hi `0`",
            ),
            (
                "import cmp\nfn main(console: Console):\n    console.print(\"${cmp.clamp(5, 10, 0)}\")\n",
                "cmp.clamp: lo exceeds hi (an empty range)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.ceil_div(7, 0)}\")\n",
                "math.ceil_div: divisor `0` must be positive",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.round_div(7, -2)}\")\n",
                "math.round_div: divisor `-2` must be positive",
            ),
            (
                "import semver\nfn main(console: Console):\n    console.print(semver.format(semver.version(-1, 2, 3)))\n",
                "semver.version: components `-1.2.3` must be non-negative",
            ),
        ];
        for (src, want_core) in cases {
            // The interpreter resolves the std bodies at run time only when they are
            // linked in (these are real fn bodies, not builtins), so link first.
            let linked = resolve_std_src(src);
            let ierr = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter must abort");
            assert!(
                ierr.message.ends_with(want_core),
                "interpreter core mismatch: got `{}`, want suffix `{want_core}`",
                ierr.message
            );
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
            assert_eq!(
                cerr,
                format!("runtime error: {}", ierr.message),
                "compiled abort mismatch for src:\n{src}"
            );
        }
        // The valid-boundary values still work (no over-eager abort): factorial(0),
        // pow(x, 0), isqrt(0), days_in_month for every month 1..12, to_base at both
        // ends of 2..16, and an empty fill when the string is already wide enough
        // (no padding is needed, so nothing is violated).
        let ok = "import math\nimport time\nfn main(console: Console):\n    console.print(\"${math.factorial(0)}\")\n    console.print(\"${math.pow(2, 0)}\")\n    console.print(\"${math.isqrt(0)}\")\n    console.print(\"${time.days_in_month(2024, 2)}\")\n    console.print(\"${time.days_in_month(2026, 2)}\")\n    console.print(\"${time.days_in_month(2026, 12)}\")\n    console.print(math.to_base(10, 2))\n    console.print(math.to_base(255, 16))\n    console.print(\"abc\".pad_left(3, \"\"))\n    console.print(\"abcd\".center(3, \"\"))\n";
        let want = vec!["1", "1", "0", "29", "28", "31", "1010", "ff", "abc", "abcd"];
        assert_eq!(link_run(ok), want, "interpreter boundary");
        assert_eq!(wasm_run(ok), want, "compiled boundary");
    }

    /// Small stdlib edge contracts, pinned on both backends: `path.base("/")`
    /// honors its documented root case; `list.chunks` yields `[]` for a
    /// non-positive size (there are no chunks of length 0) like `windows`;
    /// `time.format` preserves a trailing bare `%` like any other unknown
    /// directive; `duration.human`/`clock` render a negative span as a signed
    /// magnitude, never truncated-division fields; `ascii` predicates reject
    /// multi-character strings instead of classifying by lexicographic prefix.
    #[test]
    fn stdlib_edge_contracts_backends_agree() {
        let src = r#"import path
import list
import time
import duration
import ascii
import option

fn show_chunks(xs: List(List(Int))) -> String:
    "[" + list.join(list.map(xs, fn(c: List(Int)): "[" + list.join(list.map(c, fn(x: Int): "${x}"), ",") + "]"), ";") + "]"

fn main(console: Console):
    console.print(path.base("/") + "|" + path.stem("/") + "|" + path.base("a/b/"))
    console.print(path.base("") + "|" + (path.dir("/") ?? "<none>") + "|" + (path.ext("..") ?? "<none>") + "|" + path.stem("..") + "|[" + (path.ext("foo.") ?? "<none>") + "]|" + path.stem("foo."))
    console.print(show_chunks(list.chunks([1, 2, 3], 2)) + "|" + show_chunks(list.chunks([1, 2, 3], 0)) + "|" + show_chunks(list.chunks([1, 2, 3], -1)))
    match time.civil(2026, 7, 5, 12, 34, 56):
        Ok(d) -> console.print(time.format(d, "done %") + "|" + time.format(d, "done %%") + "|" + time.format(d, "done %Q"))
        Err(e) -> console.print(time.time_error_message(e))
    console.print(duration.human(duration.seconds(0 - 1)) + "|" + duration.human(duration.minutes(0 - 1)) + "|" + duration.human(duration.milliseconds(0 - 1)) + "|" + duration.human(duration.seconds(90)))
    console.print(duration.clock(duration.seconds(0 - 1)) + "|" + duration.clock(duration.seconds(3661)))
    console.print("${ascii.is_digit("55")}|${ascii.is_digit("5")}|${ascii.is_upper("ABC")}|${ascii.is_upper("A")}|${ascii.is_lower("az")}|${ascii.is_lower("z")}")
    console.print("${ascii.to_digit("55")}|${ascii.to_digit("7")}|${ascii.is_digit("")}")
"#;
        let interpreted = link_run(src);
        let compiled = wasm_run(src);
        assert_eq!(interpreted, compiled, "stdlib edge contracts diverged");
        assert_eq!(
            compiled,
            vec![
                "/|/|b",
                ".|<none>|<none>|..|[]|foo",
                "[[1,2];[3]]|[]|[]",
                "done %|done %|done %Q",
                "-1s|-1m0s|-1ms|1m30s",
                "-0:00:01|1:01:01",
                "false|true|false|true|false|true",
                "None|Some(7)|false",
            ]
        );
    }

    /// Batch-2 stdlib edge contracts, pinned on both backends: the string
    /// module's empty-pattern rule is uniform (an empty pattern matches
    /// NOTHING — `index_of`/`split_once`/`replace_first` now agree with
    /// `count`/`last_index_of`/`rsplit_once`); semver rejects plus-signed
    /// components and still parses/orders normally; base64url (the no-padding
    /// JWT/WebAuthn form) rejects padded input that plain base64 accepts;
    /// oauth.authorize_url extends a query-bearing endpoint with `&`.
    #[test]
    fn stdlib_edge_contracts_batch2_backends_agree() {
        let src = r#"import semver
import encoding
import oauth
import option

fn ok_err(r: Result(String, encoding.EncodingError)) -> String:
    match r:
        Ok(_) -> "ok"
        Err(_) -> "err"

fn ver(s: String) -> String:
    match semver.parse(s):
        Ok(v) -> semver.format(v)
        Err(_) -> "err"

fn main(console: Console):
    let (a1, a2) = "abc".split_once("")
    console.print("abc".replace_first("", "X") + "|" + a1 + "," + a2 + "|" + "${"abc".index_of("")}" + "|" + "${"abc".count("")}")
    let (b1, b2) = "k=v".split_once("=")
    console.print("aXc".replace_first("X", "b") + "|" + b1 + "," + b2)
    console.print(ver("1.2.3") + "|" + ver("+1.2.3") + "|" + ver("1.+2.3") + "|" + ver("-1.2.3"))
    console.print(ok_err(encoding.base64url_decode("SGk")) + "|" + ok_err(encoding.base64url_decode("SGk=")) + "|" + ok_err(encoding.base64_decode("SGk=")))
    console.print(oauth.authorize_url("https://idp/auth?prompt=consent", "c", "https://app/cb", "openid", "s"))
    console.print(oauth.authorize_url("https://idp/auth", "c", "https://app/cb", "openid", "s"))
"#;
        let interpreted = link_run(src);
        let compiled = wasm_run(src);
        assert_eq!(interpreted, compiled, "batch-2 edge contracts diverged");
        assert_eq!(
            compiled,
            vec![
                "abc|abc,|None|0",
                "abc|k,v",
                "1.2.3|err|err|err",
                "ok|err|ok",
                "https://idp/auth?prompt=consent&response_type=code&client_id=c&redirect_uri=https%3A%2F%2Fapp%2Fcb&scope=openid&state=s",
                "https://idp/auth?response_type=code&client_id=c&redirect_uri=https%3A%2F%2Fapp%2Fcb&scope=openid&state=s",
            ]
        );
    }

    /// Batch-3 stdlib edge contracts, pinned on both backends: `list.min`/`max`
    /// are generic over `Ord` like `sort` (Strings, Durations — not just Int);
    /// `url.parse` normalizes the case-insensitive scheme so `HTTPS://` gets
    /// port 443 and formats canonically; `server.with_header` stores lowercase
    /// names so `http.header` lookup works for any spelling; the HTTP client
    /// drops a caller-supplied Host (the renderer owns it, like the framing
    /// headers); `server.render_for` suppresses a HEAD response's body while
    /// keeping its Content-Length; a large `iter.drop` skips iteratively.
    #[test]
    fn stdlib_edge_contracts_batch3_backends_agree() {
        let src = r#"import list
import url
import http
import server
import iter
import option
import duration

fn show_url(raw: String) -> String:
    match url.parse(raw):
        Err(_e) -> "err"
        Ok(u) -> url.scheme(u) + " " + "${url.port(u)}" + " " + url.format(u)

fn main(console: Console):
    console.print((list.min(["pear", "apple", "plum"]) ?? "none") + "|" + (list.max(["pear", "apple", "plum"]) ?? "none"))
    console.print("${list.min([3, 1, 4]) ?? 0}|${list.max([3, 1, 4]) ?? 0}")
    console.print(duration.human(list.max([duration.seconds(5), duration.minutes(1)]) ?? duration.seconds(0)))
    console.print(show_url("HTTPS://example.test/p") + "|" + show_url("https://example.test/p"))
    let resp = server.with_header(server.ok("body"), "X-Trace-Id", "abc")
    console.print((http.header(resp, "x-trace-id") ?? "none") + "|" + (http.header(resp, "X-Trace-Id") ?? "none"))
    let head_wire = server.render_for(server.text(200, "body"), "HEAD")
    let get_wire = server.render_for(server.text(200, "body"), "GET")
    console.print("${head_wire.contains("Content-Length: 4")}|${head_wire.ends_with("\r\n\r\n")}|${get_wire.ends_with("body")}")
    let tail: List(Int) = iter.collect(iter.range(0, 100000).drop(99997))
    console.print("${tail}")
"#;
        let interpreted = link_run(src);
        let compiled = wasm_run(src);
        assert_eq!(interpreted, compiled, "batch-3 edge contracts diverged");
        assert_eq!(
            compiled,
            vec![
                "apple|plum",
                "1|4",
                "1m0s",
                "https 443 https://example.test/p|https 443 https://example.test/p",
                "abc|abc",
                "true|true|true",
                "[99997, 99998, 99999]",
            ]
        );
    }

    /// `fs.collect_files(root, "", "", ext)` walks from the Dir root itself:
    /// root-level files are collected with bare relative paths (never "/name"),
    /// and root-level directories recurse instead of being silently skipped by
    /// the confinement resolver rejecting absolute-looking paths.
    #[test]
    fn fs_collect_files_walks_from_dir_root() {
        let dir = std::env::temp_dir().join(format!("witchy-collect-root-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("top.witchy"), "// top").unwrap();
        std::fs::write(dir.join("sub/nested.witchy"), "// nested").unwrap();
        std::fs::write(dir.join("skip.txt"), "no").unwrap();
        let src = r#"import fs
import list

fn main(console: Console, root: Dir):
    var names = []
    for f in fs.collect_files(root, "", "", ".witchy"):
        let (rel, _contents) = f
        list.push(names, rel)
    list.sort(names)
    console.print(list.join(names, ","))
"#;
        let linked = resolve_std_src(src);
        let interpreted =
            interpreter::run_module(linked, dir.to_str().unwrap(), Vec::new()).expect("interp run");
        assert_eq!(interpreted, vec!["sub/nested.witchy,top.witchy"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `rand.below` fails loudly for an impossible range (RFC-0044 rule 3,
    /// matching `prng.next_below`) — and still draws for a valid bound.
    #[test]
    fn rand_below_rejects_nonpositive_bound_on_both_backends() {
        let bad = "import rand\nfn main(console: Console, r: Rand):\n    console.print(\"${rand.below(r, 0)}\")\n";
        let want_core = "rand.below: bound `0` must be positive";
        let linked = resolve_std_src(bad);
        let ierr = interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect_err("interpreter must abort");
        assert!(
            ierr.message.ends_with(want_core),
            "interpreter core mismatch: got `{}`, want suffix `{want_core}`",
            ierr.message
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
        assert_eq!(cerr, format!("runtime error: {}", ierr.message), "compiled abort mismatch");

        // A valid bound still draws a value in range on both backends.
        let ok = "import rand\nfn main(console: Console, r: Rand):\n    let n = rand.below(r, 10)\n    console.print(\"${n >= 0 && n < 10}\")\n";
        assert_eq!(link_run(ok), vec!["true"], "interpreter valid bound");
        assert_eq!(wasm_run(ok), vec!["true"], "compiled valid bound");
    }

    /// `witchy compile <entry> --dep name=path` (RFC-0004 §4) links the entry
    /// with an explicitly-provided dependency source — one that is NOT a sibling
    /// or std module — type-checks, and compiles to wasm. This is the surface the
    /// witchy CLI front-end drives to build a multi-rune project.
    #[test]
    fn compile_resolves_explicit_deps() {
        let base = std::env::temp_dir().join(format!("witchy_compile_dep_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let depdir = base.join("dep");
        let appdir = base.join("app");
        std::fs::create_dir_all(&depdir).unwrap();
        std::fs::create_dir_all(&appdir).unwrap();
        std::fs::write(depdir.join("mylib.witchy"), "pub fn greet() -> String:\n    \"hi\"\n").unwrap();
        let app = appdir.join("app.witchy");
        std::fs::write(
            &app,
            "import mylib\n\nfn main(console: Console):\n    console.print(mylib.greet())\n",
        )
        .unwrap();

        // Without the dep mapping, the import resolves to neither a sibling nor std.
        assert!(crate::link_file(app.to_str().unwrap()).is_err(), "no sibling/std mylib");

        // With the dep mapping, it links, type-checks, and compiles to wasm.
        let mut deps = std::collections::HashMap::new();
        deps.insert("mylib".to_string(), depdir.join("mylib.witchy"));
        let (linked, _) =
            crate::link_file_with_deps(app.to_str().unwrap(), &deps).expect("link with dep");
        crate::typeck::check(&linked).expect("typecheck");
        let bytes = crate::codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        assert!(!bytes.is_empty(), "produced a wasm binary");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// (RFC-0047) A realistic custom equality — case-insensitive strings — honored
    /// through containers on both backends. `CI("Hi") == CI("hi")` and the same
    /// inside a `List`/`Option` are `true`; genuinely different values are `false`.
    #[test]
    fn case_insensitive_custom_eq_through_containers() {
        let src = "\ntype CI:\n    CI(String)\n\nimpl PartialEq for CI:\n    fn eq(self, other: CI) -> Bool:\n        match self:\n            CI(a) -> match other:\n                CI(b) -> a.to_lower() == b.to_lower()\n\nfn main(console: Console):\n    console.print(\"${CI(\"Hello\") == CI(\"hello\")}\")\n    console.print(\"${[CI(\"Hi\"), CI(\"YO\")] == [CI(\"hi\"), CI(\"yo\")]}\")\n    console.print(\"${Some(CI(\"Ab\")) == Some(CI(\"ab\"))}\")\n    console.print(\"${CI(\"x\") == CI(\"y\")}\")\n";
        let want = vec![
            "true".to_string(),
            "true".to_string(),
            "true".to_string(),
            "false".to_string(),
        ];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), want, "compiled WASM must agree");
    }

    /// RFC-0006 regression: an IMPORTED tag used inside a NON-`main` function
    /// expands and runs identically on both backends. This locks in the
    /// infinite-recursion fix in `tagged::expand`: to RUN a tag the compiler links
    /// a synthetic comptime program, and `linker::link` re-runs `tagged::expand`
    /// per module — so if the comptime program still carried the CONSUMER's
    /// tag-bearing function (`render`, with its unexpanded `box"…"`), expansion
    /// would loop forever (rebuild the program → expand the tag again → …) and
    /// overflow the stack. The fix prunes the comptime program to only the items
    /// REACHABLE FROM THE TAG (its callees + the types they name), which excludes
    /// `render`/`main`, so the program holds no tagged literals and terminates.
    /// Shape mirrors the glamour `html"…"`-in-`view` case that triggered the bug.
    #[test]
    fn imported_tag_in_non_main_fn_agrees_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        // The tag-defining module: a tiny `box"…"` tag that emits source wrapping
        // each hole in `widget.unwrap(widget.wrap(…))`. The `Wrapped` type + `wrap`/`unwrap`
        // helpers exercise the reachable-TYPES half of the prune (the tag's
        // signature/body reach `Wrapped`, so it must be kept for the comptime
        // program to type-check), and prove a tag works when defined in an
        // IMPORTED rune, not just locally.
        let widget = "type Wrapped:\n    Wrap(String)\n\npub fn unwrap(w: Wrapped) -> String:\n    match w:\n        Wrap(s) -> s\n\npub fn wrap(s: String) -> Wrapped:\n    Wrap(s)\n\npub fn box(parts: List(String), holes: List(String)) -> String:\n    var out = \"widget.unwrap(widget.wrap(\\\"\"\n    var i = 0\n    let n = list.length(parts)\n    for p in parts:\n        out = out + p\n        if i < n - 1:\n            out = out + \"\\\" + \" + list.at(holes, i) + \" + \\\"\"\n        i = i + 1\n    out + \"\\\"))\"\n";
        // The CONSUMER: the tag appears in `render`, a NON-`main` function. This is
        // the exact shape that recursed before the fix (cf. glamour's `view`).
        let app = "import widget\n\nfn render(x: String) -> String:\n    box\"[${x}]\"\n\nfn main(console: Console):\n    console.print(render(\"hi\"))\n";

        let want = vec!["[hi]".to_string()];
        let link = || {
            let app_m = parser::parse_module(app).expect("parse app");
            let widget_m = parser::parse_module(widget).expect("parse widget");
            crate::pipeline::link(
                vec![("main".into(), app_m), ("widget".into(), widget_m)],
                "main",
            )
            .expect("link (must not overflow the stack)")
        };

        let linked = link();
        typeck::check(&linked).expect("typecheck");
        let interp_out = interpreter::run_module(linked, ".", Vec::new()).expect("interp run");
        assert_eq!(interp_out, want, "interpreter");

        let linked = link();
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(&bytes, Capabilities { print: true, ..Default::default() }, 4)
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
    }

    /// A MULTI-parameter generic ADT (`Result`, whose Ok and Err payloads are
    /// different type variables) is structural on both backends: payloads pin
    /// from constructor literals (the variant's own variables must unify with
    /// its arguments; the other variant's take a safe placeholder), from
    /// declared parameter types, and from declared function returns. (Closes
    /// the last loud equality gap.)
    #[test]
    fn result_equality_agrees_on_both_backends() {
        let src = "import result\n\nfn classify(n: Int) -> Result(Int, String):\n    if n >= 0: Ok(n) else: Err(\"negative\")\n\nfn same(a: Result(Int, String), b: Result(Int, String)) -> Bool:\n    a == b\n\nfn main(console: Console):\n    let xs: Result(List(Int), String) = Ok([1, 2])\n    let xs_same: Result(List(Int), String) = Ok([1, 2])\n    let xs_diff: Result(List(Int), String) = Ok([1, 3])\n    console.print(\"${classify(5) == Ok(5)}\")\n    console.print(\"${classify(5) == Ok(6)}\")\n    console.print(\"${classify(0 - 1) == Err(\"negative\")}\")\n    console.print(\"${classify(0 - 1) == Err(\"positive\")}\")\n    console.print(\"${classify(5) == Err(\"negative\")}\")\n    console.print(\"${same(Ok(1), Ok(1))}\")\n    console.print(\"${same(Err(\"a\"), Err(\"a\"))}\")\n    console.print(\"${same(Ok(1), Err(\"a\"))}\")\n    console.print(\"${xs == xs_same}\")\n    console.print(\"${xs == xs_diff}\")\n";
        let want: Vec<String> =
            ["true", "false", "true", "false", "false", "true", "true", "false", "true", "false"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// A RECURSIVE generic ADT (`Stack(a)`, whose `Push` carries a `Stack(a)`)
    /// compares structurally on both backends: the shape is identified by its
    /// type arguments and the generated helper calls itself for the
    /// self-referential field — deep spines compare by content, through
    /// literals, declared parameter types, and nullary constructors.
    #[test]
    fn recursive_generic_adt_equality_agrees_on_both_backends() {
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\nfn same(s: Stack(Int), t: Stack(Int)) -> Bool:\n    s == t\n\nfn main(console: Console):\n    console.print(\"${Push(2, Push(1, Empty)) == Push(2, Push(1, Empty))}\")\n    console.print(\"${Push(2, Push(1, Empty)) == Push(2, Push(9, Empty))}\")\n    console.print(\"${Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"a\", Empty))}\")\n    console.print(\"${Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"z\", Empty))}\")\n    console.print(\"${same(Push(1, Empty), Push(1, Empty))}\")\n    console.print(\"${same(Push(1, Empty), Empty)}\")\n";
        let want: Vec<String> = ["true", "false", "true", "false", "true", "false"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// The boundary of structural equality stays LOUD where the payload is
    /// genuinely unresolvable. Return-position inference resolves non-empty list
    /// payloads and compares them by element. An empty-list payload still has no
    /// element evidence, so the checked pipeline rejects it before codegen —
    /// never silently pointer-comparing.
    #[test]
    fn unsupported_compound_equality_is_a_loud_error_not_silent() {
        let resolved = "import result\n\nfn wrap(x: a) -> Result(a, String):\n    Ok(x)\n\nfn main(console: Console):\n    console.print(\"${wrap([1]) == wrap([2])}\")\n";
        assert_eq!(interp(resolved), vec!["false"]);
        assert_eq!(wasm_run(resolved), vec!["false"], "backends agree");
        let empty = "import result\n\nfn wrap(x: a) -> Result(a, String):\n    Ok(x)\n\nfn main(console: Console):\n    console.print(\"${wrap([]) == wrap([])}\")\n";
        let rm = parser::parse_module(empty).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), rm)], "main").expect("link");
        assert!(
            typeck::check(&linked).is_err(),
            "an empty generic payload must stay a loud checked-pipeline error"
        );
    }

    /// Ordering a NaN must FAIL on both backends, not silently return IEEE false
    /// on WASM. The interpreter errors ("cannot compare NaN"); the compiled
    /// `<`/`<=`/`>`/`>=` on floats route through a NaN-trapping helper. Equality
    /// (`==`) is IEEE on both (NaN == NaN is false) and still agrees. Ordinary
    /// float ordering is unchanged. (Regression for a silent NaN-ordering
    /// divergence.)
    #[test]
    fn nan_ordering_errors_on_both_backends() {
        for cmp in ["nan < 1.0", "nan > 1.0", "nan <= nan", "nan >= 0.0"] {
            let src = format!(
                "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(\"${{{cmp}}}\")\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let bytes = codegen::compile_module_binary(&module)
                .expect_lowered("the binary path lowers this program");
            assert!(interpreter::run(&src).is_err(), "interpreter must error on `{cmp}`");
            assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on `{cmp}`");
        }
        // Ordinary float ordering and NaN equality still agree.
        let ok = "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(\"${1.5 < 2.5}\")\n    console.print(\"${2.5 <= 2.5}\")\n    console.print(\"${nan == nan}\")\n";
        let want = vec!["true".to_string(), "true".to_string(), "false".to_string()];
        assert_eq!(interp(ok), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(ok), want, "compiled WASM must agree");
    }

    /// `string_to_int` of a value that overflows i64 must FAIL on both backends,
    /// not silently wrap on WASM. The compiled `$str_to_int` now traps once the
    /// running magnitude would exceed the sign-appropriate i64 bound (2^63-1, or
    /// 2^63 for a negative), matching Rust's checked parse. The exact boundaries
    /// (i64::MAX / i64::MIN) still parse. (Regression for a silent overflow-wrap
    /// divergence.)
    #[test]
    fn string_to_int_overflow_errors_on_both_backends() {
        let err_cases = [
            "99999999999999999999999",
            "9223372036854775808",  // i64::MAX + 1
            "-9223372036854775809", // i64::MIN - 1
        ];
        for v in err_cases {
            let src = format!(
                "fn main(console: Console):\n    console.print(\"${{\"{v}\".to_int()}}\")\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let bytes = codegen::compile_module_binary(&module)
                .expect_lowered("the binary path lowers this program");
            assert!(interpreter::run(&src).is_err(), "interpreter must error on `{v}`");
            assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on `{v}`");
        }
        // The exact i64 boundaries parse identically on both backends.
        let ok = "fn main(console: Console):\n    console.print(\"${\"9223372036854775807\".to_int()}\")\n    console.print(\"${\"-9223372036854775808\".to_int()}\")\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
        ];
        assert_eq!(interp(ok), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(ok), want, "compiled WASM must agree");
    }

    /// `to_string` of a builtin call result (`has` -> Bool, `size` -> Int) must
    /// compile and render the same on both backends — codegen knows these
    /// builtins' value types, so it picks the right formatter instead of erroring
    /// with "could not determine the value's type". (Regression for the
    /// call-result val-type gap that previously forced `int_to_string`/explicit
    /// conversion.)
    #[test]
    fn to_string_of_builtin_call_results_agrees() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n    console.print(\"${dict.contains_key(d, \"a\")}\")\n    console.print(\"${dict.contains_key(d, \"z\")}\")\n    console.print(\"${dict.length(d)}\")\n    console.print(\"${\"hello\".contains(\"ell\")}\")\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "2".to_string(),
            "true".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// An early `return` inside an `var` function must agree on both backends.
    /// An var function yields multiple results (the declared return plus one per
    /// var param), so an early return reproduces that epilogue: it pushes each
    /// var param's current value before returning. (Regression for the
    /// interpreter-only return-in-var gap.)
    #[test]
    fn return_in_var_fn_agrees_on_both_backends() {
        let src = "fn clamp(var n: Int):\n    if (n > 10):\n        n = 10\n        return\n    n = n + 1\n\nfn main(console: Console):\n    var a = 5\n    clamp(a)\n    console.print(\"${a}\")\n    var b = 50\n    clamp(b)\n    console.print(\"${b}\")\n";
        let want = vec!["6".to_string(), "10".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// The `encoding` module (hex/base64) must agree on both backends. WASM
    /// bridges each `String -> String` transform to the same native registry the
    /// interpreter uses (a host import), so output is byte-for-byte identical.
    /// (Regression for the interpreter-only encoding-module gap.)
    #[test]
    fn encoding_module_agrees_on_both_backends() {
        let src = "import encoding\n\nfn main(console: Console):\n    let p = \"Hello, witchy!\"\n    let b = encoding.base64_encode(p)\n    console.print(b)\n    console.print(encoding.base64_decode(b).unwrap_or(\"?\"))\n    let h = encoding.hex_encode(p)\n    console.print(h)\n    console.print(encoding.hex_decode(h).unwrap_or(\"?\"))\n    console.print(encoding.base64_encode(\"foo\"))\n";
        let want = vec![
            "SGVsbG8sIHdpdGNoeSE=".to_string(),
            "Hello, witchy!".to_string(),
            "48656c6c6f2c2077697463687921".to_string(),
            "Hello, witchy!".to_string(),
            "Zm9v".to_string(),
        ];
        // `import encoding` is a native module: link to register its signatures,
        // then run each backend on the linked module.
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        assert_eq!(link_run(src), want.clone(), "interpreter (linked)");
        assert_eq!(crate::run_wasm_bytes(&bytes).expect("wasm run"), want, "compiled WASM must agree");
    }

    /// `to_string` on a `Float` must produce the same text on both backends.
    /// WASM has no float formatter in hand-written WAT, so codegen calls a
    /// `float_to_str` host import that formats with Rust `Display` — byte-for-byte
    /// the interpreter's format. (Regression for the interpreter-only float
    /// `to_string` gap.)
    #[test]
    fn float_to_string_agrees_on_both_backends() {
        // Ordinary floats plus the IEEE special values whose rendering is most
        // likely to diverge between a Rust f64 and the compiled backend: the
        // infinities, NaN, and negative zero must format identically on both.
        let src = "fn main(console: Console):\n    console.print(\"${3.5}\")\n    console.print(\"${2.0}\")\n    console.print(\"${0.0 - 1.0 / 3.0}\")\n    console.print(\"${0.1 + 0.2}\")\n    console.print(\"${1000000.0}\")\n    console.print(\"${0.0}\")\n    console.print(\"${10.0 / 0.0}\")\n    console.print(\"${(0.0 - 10.0) / 0.0}\")\n    console.print(\"${0.0 / 0.0}\")\n    console.print(\"${(0.0 - 1.0) * 0.0}\")\n";
        let want = vec![
            "3.5".to_string(),
            "2.0".to_string(),
            "-0.3333333333333333".to_string(),
            "0.30000000000000004".to_string(),
            "1000000.0".to_string(),
            "0.0".to_string(),
            "inf".to_string(),
            "-inf".to_string(),
            "NaN".to_string(),
            "-0.0".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `to_upper`/`to_lower` now compile to WASM (ASCII case mapping), matching
    /// the interpreter's ASCII fold byte-for-byte — no longer interpreter-only.
    #[test]
    fn wasm_ascii_case_mapping() {
        let src = "fn main(console: Console):\n    console.print(\"Hi, World! 9z\".to_upper())\n    console.print(\"Hi, World! 9A\".to_lower())\n";
        let want = vec!["HI, WORLD! 9Z".to_string(), "hi, world! 9a".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `float_to_int` on infinities or out-of-range finite values must saturate
    /// the same way on both backends. NaN is deliberately excluded here: BUG-466
    /// makes NaN a loud contract error, covered by `math_to_int_nan_aborts_on_both_backends`.
    #[test]
    fn wasm_float_to_int_saturates_like_the_interpreter() {
        let src = "fn main(console: Console):\n    console.print(\"${math.to_int(1.0 / 0.0)}\")\n    console.print(\"${math.to_int(0.0 - 1.0 / 0.0)}\")\n    console.print(\"${math.to_int(0.0 - 3.9)}\")\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
            "-3".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `string_to_int` must accumulate in i64 (matching the interpreter's
    /// `parse::<i64>()`) and trim surrounding whitespace. WASM used to parse into
    /// i32, so a value past 2^31 (e.g. 5000000000) silently truncated to a wrong
    /// number; it now agrees on both backends.
    #[test]
    fn wasm_string_to_int_uses_i64_and_trims() {
        let src = "fn main(console: Console):\n    console.print(\"${\"5000000000\".to_int()}\")\n    console.print(\"${\"-7000000000\".to_int()}\")\n    console.print(\"${\"  42  \".to_int()}\")\n";
        let want = vec![
            "5000000000".to_string(),
            "-7000000000".to_string(),
            "42".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// (BUG-011) `string.substring` must clamp BOTH indices to `[0, char_count]`
    /// in full i64 width on BOTH backends. The compiled path used to narrow the
    /// i64 char index to i32 *before* clamping, so an index near the i64 extremes
    /// wrapped (a huge `end` became `< start`) and the slice came back `""` while
    /// the interpreter clamped in i64 and returned the whole string. Covers a
    /// negative `i`, `i > len`, `j > len`, `i > j`, and both i64 extremes.
    #[test]
    fn wasm_substring_clamps_out_of_range_indices_in_i64() {
        let src = r#"fn main(console: Console):
    let s = "abcdef"
    console.print(s.substring((-2), 3))
    console.print(s.substring(2, 100))
    console.print(s.substring(4, 2))
    console.print(s.substring(0, 6))
    console.print(s.substring((-9000000000), 9000000000))
    console.print(s.substring((-9223372036854775807), 9223372036854775807))
    console.print("X-5166417078869286437Y".substring((-3261219961577993898), 5500724189412945291))
"#;
        let want = vec![
            "abc".to_string(),
            "cdef".to_string(),
            String::new(),
            "abcdef".to_string(),
            "abcdef".to_string(),
            "abcdef".to_string(),
            "X-5166417078869286437Y".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `examples/rle/src/rle.witchy` — run-length encoding and its inverse — collapses
    /// runs to "<count><char>" and expands them back, verifying decode∘encode is
    /// the identity. Pure string processing; identical on both backends. (Its
    /// run-counting loop is what exposed the two-`at`-results comparison gap.)
    #[test]
    fn rle_example_round_trips_runs() {
        assert_eq!(
            crate::execute_file("examples/rle/src/rle.witchy", Vec::new()).unwrap(),
            vec![
                "\"aaabbbbc\" -> \"3a4b1c\"  roundtrip ok",
                "\"wwwwww\" -> \"6w\"  roundtrip ok",
                "\"abcdef\" -> \"1a1b1c1d1e1f\"  roundtrip ok",
                "\"mississippi\" -> \"1m1i2s1i2s1i2p1i\"  roundtrip ok",
                "\"\" -> \"\"  roundtrip ok",
            ]
        );
    }

    /// `std/time` computes the civil UTC date from a unix timestamp (Hinnant's
    /// days<->civil algorithm), cross-checked against Python's datetime: leap
    /// years, weekday, an exact round-trip, and a pre-1970 timestamp (floor
    /// division, so the day is right).
    #[test]
    fn time_module_civil_date_from_unix() {
        let src = r#"import time

fn main(console: Console):
    console.print(time.iso8601(time.from_unix(1780000000)))
    console.print(time.weekday_name(time.from_unix(0)) + " " + time.iso8601(time.from_unix(0)))
    console.print(time.iso8601(time.from_unix(-86401)))
    console.print(yn(time.is_leap(2000)) + yn(time.is_leap(1900)) + yn(time.is_leap(2024)))
    console.print(yn(time.to_unix(time.from_unix(1780000000)) == 1780000000))

fn yn(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "2026-05-28T20:26:40Z",
                "Thursday 1970-01-01T00:00:00Z",
                "1969-12-30T23:59:59Z",
                "yny",
                "y",
            ]
        );
    }

    /// `std/fs` parent_dir + (with a real Dir) the recursive collect — exercised
    /// here for the pure part to confirm the module's functions resolve on import.
    #[test]
    fn fs_module_parent_dir_resolves() {
        let src = "import fs\nimport option\nfn main(console: Console):\n    console.print(fs.parent_dir(\"a/b/c\") ?? \"<none>\")\n    console.print(fs.parent_dir(\"top\") ?? \"<none>\")\n";
        assert_eq!(link_run(src), vec!["a/b", "<none>"]);
    }

    /// `clock.now_monotonic()` yields monotonic elapsed nanoseconds — a steady
    /// clock for measuring durations (used by the benchmark harness to time the
    /// compute kernel, excluding process startup). The absolute value is
    /// nondeterministic, so parity is asserted on a *derived* property (elapsed is
    /// non-negative and the kernel result is identical) that both backends agree on.
    #[test]
    fn now_monotonic_measures_elapsed_on_both_backends() {
        let src = "fn spin(n: Int) -> Int:\n    var a = 0\n    var i = 0\n    while i < n:\n        a = a + i\n        i = i + 1\n    a\n\nfn main(console: Console, clock: Clock):\n    let t0 = clock.now_monotonic()\n    let r = spin(1000)\n    let t1 = clock.now_monotonic()\n    console.print(\"${r}\")\n    console.print(\"${t1 - t0 >= 0}\")\n";
        let expected = vec!["499500".to_string(), "true".to_string()];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
        // Like `now`, it needs a Clock — another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let t = now_monotonic(c)\n").is_err());
        // The Clock requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module(
                "fn main(console: Console, clock: Clock):\n    let t = clock.now_monotonic()\n",
            )
            .expect("parse"),
        );
        assert!(fp.total.contains_key("Clock"), "Clock should appear in the footprint");
    }

    /// `main` may declare a `List(String)` parameter to receive command-line
    /// arguments — argv is input data, not authority, so it's an ordinary value
    /// parameter passed by the host (here `run_module_args`), not a capability.
    #[test]
    fn main_receives_command_line_args() {
        let run = |args: Vec<String>| -> Vec<String> {
            let src = "fn main(console: Console, args: List(String)):\n    console.print(list.join(args, \",\"))\n";
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("typecheck");
            interpreter::run_module_args(linked, ".", Vec::new(), args).expect("run")
        };
        assert_eq!(run(vec!["a".into(), "b".into(), "c".into()]), vec!["a,b,c"]);
        assert_eq!(run(Vec::new()), vec![""]); // empty argv -> empty list -> ""
    }

    /// The reference interpreter and the compiled WASM backend must produce the
    /// same output for the same program — the core promise of witchy's two-tier
    /// design. This differential test exercises a spread of features and asserts
    /// agreement directly (no hardcoded expectations), so a future codegen change
    /// that silently diverges from the interpreter is caught. Programs stay
    /// within the compiled backend's supported semantics (notably 32-bit Int).
    #[test]
    fn interpreter_and_compiled_backends_agree() {
        let programs: &[(&str, &str)] = &[
            (
                "arithmetic + control flow",
                r#"
fn main(console: Console):
    var acc = 0
    var i = 0
    while (i < 12):
        if ((i % 2) == 0):
            acc = (acc + i)
        else:
            acc = (acc - i)
        i = (i + 1)
    console.print("${acc}")
"#,
            ),
            (
                "records + update + field access",
                r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(3, 4)
    let q = Point(x: ((p).x + 10), ..p)
    console.print("${((q).x * (q).y)}")
"#,
            ),
            (
                "lists + recursion + head/tail match",
                r#"
fn sum(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [h, ..t] -> (h + sum(t))

fn main(console: Console):
    console.print("${sum([1, 2, 3, 4, 5])}")
"#,
            ),
            (
                "ADTs + match",
                r#"
type Shape:
    Circle(Int)
    Rect(Int, Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> ((3 * r) * r)
        Rect(w, h) -> (w * h)

fn main(console: Console):
    console.print("${(area(Circle(5)) + area(Rect(3, 4)))}")
"#,
            ),
            (
                "capturing closures + higher-order",
                r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let k = 100
    console.print("${apply(fn(n: Int): (n + k), 5)}")
"#,
            ),
            (
                "dicts",
                r#"
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, "a", 1)
    dict.insert(d, "b", 2)
    dict.insert(d, "a", 9)
    console.print("${(dict.get_or(d, "a", 0) + dict.length(d))}")
"#,
            ),
            (
                "strings",
                r#"
fn main(console: Console):
    console.print("a,b,c".replace(",", "-"))
    console.print("${"hello".contains("l")}")
    console.print("hello".substring(1, 4))
    for w in "the cat sat".split(" "):
        console.print(w)
"#,
            ),
            (
                "string equality across a List(String) parameter",
                r#"
fn count_matches(words: List(String), target: String) -> Int:
    var n = 0
    for w in words:
        if (w == target):
            n = (n + 1)
    n

fn main(console: Console):
    let words = "apple banana apple cherry apple".split(" ")
    console.print("${count_matches(words, "apple")}")
"#,
            ),
            (
                "string equality + ordering",
                r#"
fn main(console: Console):
    let a = "xapple".substring(1, 6)
    console.print("${(a == "apple")}")
    console.print("${(a == "apricot")}")
    console.print("${(a != "apricot")}")
    console.print("${("apple" < "banana")}")
    console.print("${("banana" < "apple")}")
    console.print("${("app" < "apple")}")
    console.print("${("apple" <= "apple")}")
"#,
            ),
            (
                "tuples + polymorphic to_string",
                r#"
fn main(console: Console):
    let (a, b) = (7, 8)
    console.print("${(a + b)}")
    console.print("${(a < b)}")
    console.print("${"done"}")
"#,
            ),
            (
                // Regression (M7): an inline `else:` ending in a bare identifier,
                // immediately followed by a `"${...}"` interpolation, must parse as
                // two statements (not `count(...)`). (Builtins/prelude only — this
                // harness doesn't link std modules.)
                "inline else bare-ident before an interpolation",
                r#"
fn describe(n: Int) -> String:
    let label = if n < 0: "neg" else: "pos"
    let mag = if n < 0: 0 - n else: n
    "${label}:${mag}"

fn main(console: Console):
    console.print(describe(0 - 4250))
    console.print(describe(150000))
"#,
            ),
        ];
        for (name, src) in programs {
            let interpreted = interp(src);
            let compiled = run_on_wasm(src);
            assert_eq!(
                interpreted, compiled,
                "interpreter and compiled backends diverged for `{name}`"
            );
        }
    }

    #[test]
    fn try_operator_result_backends_agree() {
        // `?` propagation on Result: the success path unwraps and continues, the
        // failure path short-circuits with the Err. Both backends must agree.
        let client = r#"
import result

fn parse_pos(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn add_two(a: Int, b: Int) -> Result(Int, String):
    let x = (parse_pos(a))?
    let y = (parse_pos(b))?
    Ok((x + y))

fn main(console: Console):
    console.print("${result.unwrap_or(add_two(3, 4), 0)}")
    console.print("${result.unwrap_or(add_two(3, (0 - 1)), 0)}")
    console.print("${result.is_err(add_two((0 - 5), 2))}")
    console.print("${result.is_ok(add_two(10, 20))}")
"#;
        let sources = [("result", crate::bundled_module("result").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "`?` on Result diverged between backends");
    }

    #[test]
    fn try_operator_with_message_backends_agree() {
        // `e ? "msg"` adds context and is generic over the operand: an `Option`'s
        // `None` becomes `Err(msg)`; a `Result`'s `Err(e)` becomes `Err("msg: e")`.
        // Both backends must agree (the message form works wherever bare `?` does).
        let client = r#"
import option
import result

fn need(o: Option(Int)) -> Result(Int, String):
    let x = o ? "missing value"
    Ok(x)

fn rewrap(r: Result(Int, String)) -> Result(Int, String):
    let x = r ? "while computing"
    Ok(x)

fn main(console: Console):
    console.print("${need(Some(5))}")
    console.print("${need(None)}")
    console.print("${rewrap(Ok(9))}")
    console.print("${rewrap(Err("boom"))}")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("result", crate::bundled_module("result").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "`? \"msg\"` diverged between backends");
        assert!(
            interpreted.iter().any(|l| l.contains("missing value")),
            "Option `None` must become `Err(msg)`: {interpreted:?}"
        );
        assert!(
            interpreted.iter().any(|l| l.contains("while computing: boom")),
            "Result `Err(e)` must become `Err(\"msg: e\")`: {interpreted:?}"
        );
    }

    #[test]
    fn try_operator_option_backends_agree() {
        // `?` propagation on Option: short-circuit on None, unwrap on Some.
        let client = r#"
import option

fn first_even(a: Int, b: Int) -> Option(Int):
    let x = (pick_even(a))?
    let y = (pick_even(b))?
    Some((x + y))

fn pick_even(n: Int) -> Option(Int):
    if ((n % 2) == 0):
        Some(n)
    else:
        None

fn main(console: Console):
    console.print("${option.unwrap_or(first_even(4, 6), 0)}")
    console.print("${option.unwrap_or(first_even(4, 7), 0)}")
    console.print("${option.is_none(first_even(3, 8))}")
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "`?` on Option diverged between backends");
    }

    #[test]
    fn sort_strings_backends_agree() {
        // Sorting strings lexicographically with `sort_by` and a String
        // comparator — exercising string `<` through call_indirect inside
        // insert_sorted — agrees across backends.
        let client = r#"
import list

fn main(console: Console):
    var words = ["cherry", "apple", "banana", "date", "apple"]
    list.sort_by(words, fn(a: String, b: String): (a < b))
    for w in words:
        console.print(w)
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string sort diverged between backends");
        assert_eq!(
            compiled,
            vec!["apple", "apple", "banana", "cherry", "date"]
        );
    }

    #[test]
    fn unwrap_or_else_backends_agree() {
        // Lazy defaults via a zero-arg closure, for both Option and Result.
        let opt = r#"
import option

fn main(console: Console):
    console.print("${option.unwrap_or_else(Some(5), fn(): 0)}")
    let fallback = 99
    console.print("${option.unwrap_or_else(option.filter(Some(3), fn(n: Int): (n > 10)), fn(): fallback)}")
"#;
        let osrc = [("option", crate::bundled_module("option").unwrap()), ("main", opt)];
        assert_eq!(
            interpreter::run_program(&osrc, "main").expect("interp"),
            run_linked_on_wasm(&osrc, "main")
        );
        assert_eq!(run_linked_on_wasm(&osrc, "main"), vec!["5", "99"]);

        let res = r#"
import result

fn checked(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    console.print("${result.unwrap_or_else(checked(7), fn(): 0)}")
    console.print("${result.unwrap_or_else(checked((0 - 1)), fn(): 42)}")
"#;
        let rsrc = [("result", crate::bundled_module("result").unwrap()), ("main", res)];
        assert_eq!(
            interpreter::run_program(&rsrc, "main").expect("interp"),
            run_linked_on_wasm(&rsrc, "main")
        );
        assert_eq!(run_linked_on_wasm(&rsrc, "main"), vec!["7", "42"]);
    }

    #[test]
    fn std_eq_member_backends_agree() {
        // The Eq trait + the bounded list `contains` / `index_of` give content-correct
        // equality on BOTH backends — even for runtime-BUILT strings, where a
        // generic `==` search does pointer comparison in compiled code and would
        // wrongly miss. A user `impl Eq` (Box) works, as does the default `ne`.
        let client = r#"
import list

type Box:
    Box(Int)

impl PartialEq for Box:
    fn eq(self, other: Self) -> Bool:
        match self:
            Box(a) -> match other:
                Box(b) -> (a == b)

impl Eq for Box

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < s.char_count()):
        acc = (acc + s.substring(i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("apple"), build("banana")]
    console.print("${list.contains(words, build("banana"))}")
    console.print("${list.contains(words, build("cherry"))}")
    console.print("${list.index_of([10, 20, 30], 20)}")
    console.print("${list.index_of([10, 20, 30], 99)}")
    console.print("${list.contains([Box(1), Box(2)], Box(2))}")
    console.print("${ne(Box(1), Box(2))}")
    console.print("${ne(Box(2), Box(2))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std eq member/index_of diverged");
        assert_eq!(
            compiled,
            vec!["true", "false", "Some(1)", "None", "true", "true", "false"]
        );
    }

    #[test]
    fn std_eq_count_unique_backends_agree() {
        // `list.count` / `list.unique` dispatch through the element type's Eq impl, so
        // they are content-correct on BOTH backends — including runtime-built
        // strings and user `impl Eq` types (Tag).
        let client = r#"
import list

type Tag:
    Tag(Int)

impl PartialEq for Tag:
    fn eq(self, other: Self) -> Bool:
        match self:
            Tag(a) -> match other:
                Tag(b) -> (a == b)

impl Eq for Tag

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < s.char_count()):
        acc = (acc + s.substring(i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("a"), build("b"), build("a"), build("c"), build("b"), build("a")]
    console.print("${list.count(words, build("a"))}")
    console.print("${list.count(words, build("z"))}")
    console.print(list.join(list.unique(words), ","))
    console.print("${list.length(list.unique([Tag(1), Tag(2), Tag(1), Tag(2), Tag(3)]))}")
    console.print("${list.count([Tag(1), Tag(2), Tag(1)], Tag(1))}")
"#;
        let sources = [
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std eq count/unique diverged");
        assert_eq!(compiled, vec!["3", "0", "a,b,c", "3", "2"]);
    }

    #[test]
    fn std_ascii_classification_backends_agree() {
        // ASCII predicates are implemented purely via string comparison, so they
        // must agree across the interpreter and the compiled backend. Also drives
        // a tiny tokenizer-style use: sum the digit values in a string.
        let client = r#"
import ascii

fn digit_sum(s: String) -> Int:
    var total = 0
    var i = 0
    while (i < s.char_count()):
        let c = s.char_at(i) ?? ""
        if ascii.is_digit(c):
            total = (total + (ascii.to_digit(c) ?? 0))
        i = (i + 1)
    total

fn main(console: Console):
    console.print("${ascii.is_digit("7")}")
    console.print("${ascii.is_digit("x")}")
    console.print("${ascii.is_alpha("Q")}")
    console.print("${ascii.is_alnum("_")}")
    console.print("${ascii.is_space("\t")}")
    console.print("${ascii.to_digit("4") ?? -1}")
    console.print("${ascii.to_digit("z") ?? -1}")
    console.print("${digit_sum("a1b2c3")}")
    console.print("${ascii.all_digits("12345")}")
    console.print("${ascii.all_digits("12a45")}")
    console.print("${ascii.all_digits("")}")
    console.print("${ascii.all_digits("0")}")
"#;
        let sources = [
            ("ascii", crate::bundled_module("ascii").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std ascii diverged");
        assert_eq!(
            compiled,
            vec![
                "true", "false", "true", "false", "true", "4", "-1", "6", // all_digits:
                "true", "false", "false", "true",
            ]
        );
    }

    #[test]
    fn std_show_list_backends_agree() {
        // The blanket `impl Show for List(a) where a: Show` renders via the
        // works for a user type (Coord) that the built-in to_string cannot print.
        // Monomorphized dispatch keeps it content-correct on both backends.
        let client = r#"
import show

type Coord:
    Coord(Int, Int)

impl Show for Coord:
    fn show(self) -> String:
        match self:
            Coord(x, y) -> (((("(" + "${x}") + ",") + "${y}") + ")")

fn main(console: Console):
    console.print(show([1, 2, 3]))
    console.print(show(["a", "b"]))
    console.print(show([Coord(0, 0), Coord(1, 2)]))
    console.print(show([true, false]))
"#;
        let sources = [
            ("show", crate::bundled_module("show").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std show_list diverged");
        assert_eq!(
            compiled,
            vec!["[1, 2, 3]", "[a, b]", "[(0,0), (1,2)]", "[true, false]"]
        );
    }

    #[test]
    fn inline_if_else_expression_form() {
        // Brace-free inline `if c: a else: b` (chained), here inside a brace-free
        // lambda inside call parens. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    let xs = [3, (0 - 2), 0, 5]
    let signs = list.map(xs, fn(n: Int): if (n > 0): 1 else: if (n < 0): (0 - 1) else: 0)
    console.print("${list.fold(signs, 0, fn(a: Int, b: Int): (a + b))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "inline if-else diverged");
        assert_eq!(compiled, vec!["1"]);
    }

    #[test]
    fn inherent_impl_in_indentation_syntax() {
        // The inherent impl works under the off-side rule too: `impl Point:`.
        let client = "type Point:\n    Point(Int, Int)\n\nimpl Point:\n    fn sum(self) -> Int:\n        match self:\n            Point(x, y) -> x + y\n\nfn main(console: Console):\n    console.print(\"${sum(Point(4, 5))}\")\n";
        assert_eq!(interp(client), vec!["9"]);
        assert_eq!(run_on_wasm(client), vec!["9"]);
    }

    #[test]
    fn inherent_impl_methods_dispatch_by_type() {
        // `impl Type { fn m(self) ... }` (no trait) defines methods dispatched by
        // receiver type, reusing the trait machinery. Two types share the method
        // name `mag`; each call resolves to the right one. Both backends agree.
        let client = r#"
type Point:
    Point(Int, Int)

type Circle:
    Circle(Int)

impl Point:
    fn mag(self) -> Int:
        match self:
            Point(x, y) -> ((x * x) + (y * y))

impl Circle:
    fn mag(self) -> Int:
        match self:
            Circle(r) -> (r * r)

fn main(console: Console):
    console.print("${mag(Point(3, 4))}")
    console.print("${mag(Circle(6))}")
"#;
        assert_eq!(interp(client), vec!["25", "36"]);
        assert_eq!(run_on_wasm(client), vec!["25", "36"]);
    }

    #[test]
    fn inherent_impl_on_generic_type() {
        // An inherent `impl Stack(a):` carries the type's OWN parameter, so each
        // method's `self` is `Stack(a)` (not a bare `Stack`) and the methods
        // monomorphize per element type. Covers a static constructor (`empty`), an
        // instance method returning Self (`push`, chained off the static), and an
        // instance method on a let-bound chain receiver (`howbig`). Two distinct
        // element types exercise monomorphization; both backends agree.
        let client = r#"
type Stack(a):
    items: List(a)

impl Stack(a):
    fn empty() -> Stack(a):
        Stack([])
    fn push(var self, x: a) -> Nil:
        list.push(self.items, x)
    fn howbig(self) -> Int:
        list.length(self.items)

fn main(console: Console):
    var s = Stack.empty()
    s.push(1)
    s.push(2)
    s.push(3)
    console.print("${s.howbig()}")
    var w = Stack.empty()
    w.push("a")
    w.push("b")
    console.print("${w.howbig()}")
"#;
        assert_eq!(interp(client), vec!["3", "2"]);
        assert_eq!(run_on_wasm(client), vec!["3", "2"]);
    }

    #[test]
    fn recursive_trait_dispatch_on_match_bound_fields() {
        // A trait method can now dispatch on a variable bound by a constructor
        // pattern when the field type is concrete: `show(x)` / `show(c)` inside a
        // Show impl resolve through the match arm. Covers a nested struct (Named
        // holds a Coord) and stays content-correct on both backends.
        let client = r#"
import show

type Coord:
    Coord(Int, Int)

impl Show for Coord:
    fn show(self) -> String:
        match self:
            Coord(x, y) -> (((("(" + show(x)) + ", ") + show(y)) + ")")

type Named:
    Named(String, Coord)

impl Show for Named:
    fn show(self) -> String:
        match self:
            Named(label, c) -> ((label + "=") + show(c))

fn main(console: Console):
    console.print(show(Coord(3, 4)))
    console.print(show(Named("p", Coord(1, 2))))
    console.print(show([Coord(0, 0), Coord(5, 6)]))
"#;
        let sources = [
            ("show", crate::bundled_module("show").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "recursive show dispatch diverged");
        assert_eq!(compiled, vec!["(3, 4)", "p=(1, 2)", "[(0, 0), (5, 6)]"]);
    }

    /// `string_chars` (the O(n) string -> List(String) primitive behind a fast
    /// `to_chars`) agrees across the interpreter and WASM —
    /// including a multi-byte (UTF-8) character. Counted by Unicode scalar.
    #[test]
    fn string_chars_backends_agree() {
        let src = "fn main(console: Console):\n    let cs = \"café\".chars()\n    console.print(\"${list.length(cs)}\")\n    console.print(list.at(cs, 0))\n    console.print(list.at(cs, 3))\n";
        let expected = vec!["4".to_string(), "c".to_string(), "é".to_string()];
        // Interpreter (source of truth).
        assert_eq!(interpreter::run(src).expect("interp"), expected);
        // WASM.
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm diverged");
    }

    #[test]
    fn math_isqrt_and_perfect_square_backends_agree() {
        // isqrt floors the square root (overflow-safe); is_perfect_square is
        // true exactly on 0,1,4,9,... and false for negatives. A negative isqrt
        // argument is a rule-3 abort (RFC-0044), covered by
        // std_contract_violations_abort_on_both_backends; is_perfect_square
        // short-circuits negatives to false without calling isqrt.
        let client = r#"
import math
import list
fn main(console: Console):
    let roots = list.map([0, 1, 2, 3, 4, 8, 9, 15, 16, 100, 99], fn(n: Int): math.isqrt(n))
    console.print(list.join(list.map(roots, fn(n: Int): "${n}"), ","))
    let flags = list.map([0, 1, 2, 4, 9, 10, 16, 17], fn(n: Int): if math.is_perfect_square(n): "T" else: "F")
    console.print(list.join(flags, ""))
    console.print(if math.is_perfect_square(-4): "T" else: "F")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("math", crate::bundled_module("math").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "isqrt/is_perfect_square diverged");
        assert_eq!(compiled, vec!["0,1,1,1,2,2,3,3,4,10,9", "TTFTTFTF", "F"]);
    }

    #[test]
    fn string_parse_int_backends_agree() {
        // parse_int validates an optional sign + digits before calling the raw
        // string_to_int builtin, so bad input is None (not a trap) consistently.
        let client = r#"
import option
fn show(o: Option(Int)) -> String:
    match o:
        Some(n) -> "${n}"
        None -> "none"
fn main(console: Console):
    console.print(show("42".parse_int()))
    console.print(show("-7".parse_int()))
    console.print(show("0".parse_int()))
    console.print(show("".parse_int()))
    console.print(show("-".parse_int()))
    console.print(show("12a".parse_int()))
    console.print(show("3.5".parse_int()))
    console.print(show(" 5".parse_int()))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "parse_int diverged");
        assert_eq!(
            compiled,
            vec!["42", "-7", "0", "none", "none", "none", "none", "none"]
        );
    }

    #[test]
    fn string_center_backends_agree() {
        // center pads both sides; an odd remainder goes on the right, and a
        // string already at/over width is returned unchanged.
        let client = r#"
fn main(console: Console):
    console.print("[" + "hi".center(6, " ") + "]")
    console.print("[" + "hi".center(7, " ") + "]")
    console.print("[" + "odd".center(8, "*") + "]")
    console.print("[" + "toolong".center(4, " ") + "]")
    console.print("[" + "x".center(1, " ") + "]")
"#;
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "center diverged");
        assert_eq!(
            compiled,
            vec!["[  hi  ]", "[  hi   ]", "[**odd***]", "[toolong]", "[x]"]
        );
    }

    #[test]
    fn url_format_roundtrip_backends_agree() {
        // format is parse's inverse; the default port is omitted, a non-default
        // port is kept, and an absent path renders as "/".
        let client = r#"
import url
import result
fn render(s: String) -> String:
    match url.parse(s):
        Ok(u) -> url.format(u)
        Err(_e) -> "no parse"
fn main(console: Console):
    console.print(render("https://example.com/path"))
    console.print(render("http://example.com:8080/x"))
    console.print(render("ftp://host:21/file"))
    console.print(render("http://example.com"))
    console.print(render("not a url"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url.format diverged");
        assert_eq!(
            compiled,
            vec![
                "https://example.com/path",
                "http://example.com:8080/x",
                "ftp://host:21/file",
                "http://example.com/",
                "no parse",
            ]
        );
    }

    #[test]
    fn url_parse_rejects_bad_port_without_trapping_backends_agree() {
        // A non-decimal or empty `:port` makes parse return None — it used to trap
        // in string_to_int. Signs accepted by the general integer parser are not
        // URL port syntax. A valid or defaulted port still parses, both backends.
        let client = r#"
import url
import result
fn p(s: String) -> String:
    match url.parse(s):
        Ok(u) -> "ok:" + "${url.port(u)}"
        Err(_e) -> "none"
fn main(console: Console):
    console.print(p("https://h:8443/x"))
    console.print(p("https://h:abc/x"))
    console.print(p("https://h:/x"))
    console.print(p("https://h:80x/x"))
    console.print(p("https://h:+80/x"))
    console.print(p("https://h:-0/x"))
    console.print(p("https://h/x"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url bad-port diverged");
        assert_eq!(
            compiled,
            vec!["ok:8443", "none", "none", "none", "none", "none", "ok:443"]
        );
    }

    /// (BUG-470) `url.decode` percent-decodes path components (+ stays literal),
    /// `url.decode_form` also maps + to space (query/form convention). Both handle
    /// multi-byte UTF-8 escapes and stray `%` passthrough. Parity on both backends.
    #[test]
    fn url_decode_and_decode_form_backends_agree() {
        let client = r#"
import url
fn main(console: Console):
    // Basic ASCII escapes
    console.print(url.decode("hello%20world"))
    // Multi-byte UTF-8 (€ = E2 82 AC)
    console.print(url.decode("%E2%82%AC"))
    // + stays literal in path mode
    console.print(url.decode("a+b"))
    // + becomes space in form mode
    console.print(url.decode_form("a+b"))
    // Mixed: encoded and plain
    console.print(url.decode_form("key%3D%26val+ue"))
    // Stray % passes through
    console.print(url.decode("100%"))
    // encode/decode round-trip
    console.print(url.decode(url.encode("hello world/€")))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url.decode diverged");
        assert_eq!(
            compiled,
            vec![
                "hello world",
                "€",
                "a+b",
                "a b",
                "key=&val ue",
                "100%",
                "hello world/€",
            ]
        );
    }

    #[test]
    fn url_parse_ipv6_and_userinfo_backends_agree() {
        // A bracketed IPv6 authority keeps its inner colons in the host and splits
        // the port at the colon after `]` — matching the Net layer's last-colon /
        // bracket-aware split (BUG-351). Userinfo (`user@`, `user:pass@`) is outside
        // this minimal grammar and is rejected loudly rather than reinterpreted as
        // host/port text (BUG-380), and an empty bracketed literal is malformed.
        // Both backends agree, and format round-trips.
        let client = r#"
import url
import result
fn p(s: String) -> String:
    match url.parse(s):
        Ok(u) -> url.host(u) + " " + "${url.port(u)}" + " " + url.format(u)
        Err(_e) -> "err"
fn main(console: Console):
    console.print(p("http://[::1]:8080/x"))
    console.print(p("http://[::1]/x"))
    console.print(p("https://[2001:db8::1]:443/y"))
    console.print(p("http://[]/x"))
    console.print(p("https://user@example.com/x"))
    console.print(p("https://user:pass@example.com/x"))
    console.print(p("https://example.com:8443/z"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url ipv6/userinfo diverged");
        assert_eq!(
            compiled,
            vec![
                "[::1] 8080 http://[::1]:8080/x",
                "[::1] 80 http://[::1]/x",
                "[2001:db8::1] 443 https://[2001:db8::1]/y",
                "err",
                "err",
                "err",
                "example.com 8443 https://example.com:8443/z",
            ]
        );
    }

    #[test]
    fn prng_next_below_rejects_uncoverable_bound_backends_agree() {
        // The Park-Miller reducer is `n % bound`; a bound at or above the generator
        // range (2^31-1) cannot cover its own range, so it fails loudly (BUG-482)
        // — like the non-positive guard. An ordinary small bound still draws.
        let bad = r#"
import prng
fn main(console: Console):
    var r = prng.seed(1)
    let _i = prng.next_below(r, 2147483647)
    console.print("unreachable")
"#;
        let linked = resolve_std_src(bad);
        let ierr =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect_err("interpreter must abort");
        assert!(
            ierr.message.contains("cannot be covered"),
            "interpreter core mismatch: {}",
            ierr.message
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
        assert!(cerr.contains("cannot be covered"), "compiled core mismatch: {cerr}");

        let ok = "import prng\nfn main(console: Console):\n    var r = prng.seed(1)\n    let i = prng.next_below(r, 6)\n    console.print(\"${i >= 0 && i < 6}\")\n";
        assert_eq!(link_run(ok), vec!["true"], "interpreter small bound");
        assert_eq!(wasm_run(ok), vec!["true"], "compiled small bound");
    }

    #[test]
    fn config_merge_example_runs_on_wasm() {
        // The layered-config example (json.merge shallow override + encode_pretty)
        // prints identically on both backends: base.debug survives, production
        // overrides host/port and adds workers.
        let sources = [
            ("json", crate::bundled_module("json").unwrap()),
            ("main", include_str!("../examples/config_merge/src/config_merge.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "config_merge diverged");
        assert_eq!(
            compiled,
            vec![
                "{\n  \"debug\": true,\n  \"host\": \"example.com\",\n  \"port\": 443,\n  \"workers\": 8\n}",
                "has workers",
            ]
        );
    }

    #[test]
    fn string_rsplit_once_backends_agree() {
        // rsplit_once splits on the LAST separator (vs split_once's first); when
        // the separator is absent the whole string is the right part.
        let client = r#"
fn show2(p: (String, String)) -> String:
    let (a, b) = p
    a + "|" + b
fn main(console: Console):
    console.print(show2("a.b.c".rsplit_once(".")))
    console.print(show2("a.b.c".split_once(".")))
    console.print(show2("nodot".rsplit_once(".")))
    console.print(show2("file.tar.gz".rsplit_once(".")))
    console.print("${"a.b.c".last_index_of(".") ?? -1}")
    console.print("${"nodot".last_index_of(".") ?? -1}")
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "rsplit_once diverged");
        assert_eq!(
            compiled,
            vec!["a.b|c", "a|b.c", "|nodot", "file.tar|gz", "3", "-1"]
        );
    }

    #[test]
    fn duration_literals_backends_agree() {
        // Native duration literals (1s/1ms/1m/1h/1d/1w, and the `hr` alias) are a
        // distinct Duration type carried as milliseconds: they add/subtract,
        // scale by an Int, divide to an Int ratio, and compare — identically on
        // both backends.
        let client = r#"
fn main(console: Console):
    console.print("${30s > 500ms}")
    console.print("${30s + 500ms == 30500ms}")
    console.print("${1m == 60s}")
    console.print("${2hr == 7200s}")
    console.print("${1d == 24h}")
    console.print("${1w > 6d}")
    console.print("${2 * 1h == 7200s}")
    console.print("${1h / 1m == 60}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "duration literals diverged");
        assert_eq!(
            compiled,
            vec!["true", "true", "true", "true", "true", "true", "true", "true"]
        );
    }

    #[test]
    fn durations_example_runs_on_wasm() {
        // The durations example (literals + Duration*Int + comparison + the
        // duration module) prints identically on both backends.
        let sources = [
            ("duration", crate::bundled_module("duration").unwrap()),
            ("main", include_str!("../examples/durations/src/durations.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "durations example diverged");
        assert_eq!(
            compiled,
            vec!["1s", "2s", "4s", "5s", "5s", "1:30:00", "true", "2m30s"]
        );
    }

    #[test]
    fn prng_module_backends_agree() {
        // The Park-Miller LCG replays a deterministic sequence (the canonical
        // seed-1 values) identically on both backends; next_below bounds it.
        let client = r#"
import prng
import list
fn main(console: Console):
    var r = prng.seed(1)
    var out = []
    var i = 0
    while i < 4:
        let n = prng.next(r)
        list.push(out, n)
        i = i + 1
    console.print(list.join(list.map(out, fn(n: Int): "${n}"), ","))
    var r3 = prng.seed(42)
    let d = prng.next_below(r3, 6)
    console.print("${d}")
    var r4 = prng.seed(2)
    let b = prng.next_bool(r4)
    console.print(if b: "even" else: "odd")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("prng", crate::bundled_module("prng").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "prng diverged");
        assert_eq!(
            compiled,
            vec!["16807,282475249,1622650073,984943658", "0", "even"]
        );
    }

    #[test]
    fn dice_example_runs_on_wasm() {
        // The dice example (seeded prng.next_below, threaded Rng) prints the
        // same deterministic rolls on both backends.
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("prng", crate::bundled_module("prng").unwrap()),
            ("main", include_str!("../examples/dice/src/dice.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "dice example diverged");
        assert_eq!(compiled, vec!["2 2 1 6 2 2 1 5 2 2", "total: 25"]);
    }

    #[test]
    fn result_and_option_all_backends_agree() {
        // `all` sequences a list of Results/Options: Ok/Some of the collected
        // values, or the first failure (Err / None).
        let client = r#"
import result
import option
import list
fn nums(r: Result(List(Int), String)) -> String:
    match r:
        Ok(xs) -> list.join(list.map(xs, fn(n: Int): "${n}"), ",")
        Err(e) -> "err:" + e
fn onums(o: Option(List(Int))) -> String:
    match o:
        Some(xs) -> list.join(list.map(xs, fn(n: Int): "${n}"), ",")
        None -> "none"
fn main(console: Console):
    console.print(nums(result.all([Ok(1), Ok(2), Ok(3)])))
    console.print(nums(result.all([Ok(1), Err("bad"), Ok(3)])))
    console.print(nums(result.all([])))
    console.print(onums(option.all([Some(1), Some(2)])))
    console.print(onums(option.all([Some(1), None, Some(3)])))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("result", crate::bundled_module("result").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result/option.all diverged");
        assert_eq!(compiled, vec!["1,2,3", "err:bad", "", "1,2", "none"]);
    }

    #[test]
    fn prng_choice_backends_agree() {
        // choice picks a pseudo-random element (None for an empty list),
        // deterministically for a given seed, identically on both backends.
        let client = r#"
import prng
import option
fn main(console: Console):
    var r = prng.seed(1)
    let c = prng.choice(["a", "b", "c", "d"], r)
    console.print(option.unwrap_or(c, "?"))
    var r2 = prng.seed(1)
    let e = prng.choice([], r2)
    console.print(option.unwrap_or(e, "empty"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("prng", crate::bundled_module("prng").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "prng.choice diverged");
        assert_eq!(compiled, vec!["d", "empty"]);
    }

    #[test]
    fn duration_module_backends_agree() {
        // The duration module over the built-in Duration type: human/clock format
        // a Duration (combined from literals), to_milliseconds bridges back to Int,
        // and the whole-unit total conversions (to_seconds..to_weeks) truncate.
        let client = r#"
import duration
fn main(console: Console):
    console.print("${duration.to_milliseconds(duration.from_clock(1, 2, 3))}")
    console.print(duration.clock(1h + 2m + 3s))
    console.print(duration.clock(90s))
    console.print(duration.human(1h + 1m + 1s))
    console.print(duration.human(90s))
    console.print(duration.human(5s))
    console.print(duration.human(500ms))
    console.print("${duration.to_milliseconds(duration.hours(2))}")
    console.print("${duration.part_minutes(1h + 2m + 3s)}")
    console.print("${duration.to_seconds(duration.days(10))}")
    console.print("${duration.to_minutes(duration.days(10))}")
    console.print("${duration.to_hours(duration.days(10))}")
    console.print("${duration.to_days(duration.days(10))}")
    console.print("${duration.to_weeks(duration.days(10))}")
"#;
        let sources = [
            ("duration", crate::bundled_module("duration").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "duration diverged");
        assert_eq!(
            compiled,
            vec![
                "3723000", "1:02:03", "0:01:30", "1h1m1s", "1m30s", "5s", "500ms", "7200000", "2",
                "864000", "14400", "240", "10", "1",
            ]
        );
    }

    #[test]
    fn convert_from_into_backends_agree() {
        // std/convert's From/Into: implementing `From` gives `.from(x)` on the
        // target type, and the blanket `impl Into for a where b: From(a)` derives
        // `.into()`. Both resolve and run identically on both backends — the
        // blanket trait impl + From->Into derivation was otherwise untested.
        let client = r#"
import convert

type Celsius:
    Celsius(Int)

type Fahrenheit:
    Fahrenheit(Int)

impl From(Celsius) for Fahrenheit:
    fn from(value: Celsius) -> Fahrenheit:
        match value:
            Celsius(deg) -> Fahrenheit(deg * 9 / 5 + 32)

fn degf(f: Fahrenheit) -> Int:
    match f:
        Fahrenheit(d) -> d

fn main(console: Console):
    console.print("${degf(Fahrenheit.from(Celsius(100)))}")
    let f: Fahrenheit = Celsius(0).into()
    console.print("${degf(f)}")
    let body: Fahrenheit = Celsius(37).into()
    console.print("${degf(body)}")
"#;
        let sources = [
            ("convert", crate::bundled_module("convert").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "convert From/Into diverged");
        assert_eq!(compiled, vec!["212", "32", "98"]);
    }

    #[test]
    fn duration_parse_backends_agree() {
        // parse is the inverse of human, returning a Duration (ms): unit-tagged
        // (incl. ms/hr) or bare-ms input, Err on junk/dangling (RFC-0044 rule 2),
        // and parse(human(d)) round-trips.
        let client = r#"
import duration
fn show(o: Result(Duration, duration.DurationParseError)) -> String:
    match o:
        Ok(d) -> "${duration.to_milliseconds(d)}"
        Err(_) -> "none"
fn roundtrip(d: Duration) -> String:
    match duration.parse(duration.human(d)):
        Ok(p) -> if p == d: "ok" else: "bad"
        Err(_) -> "none"
fn main(console: Console):
    console.print(show(duration.parse("1h2m3s")))
    console.print(show(duration.parse("500ms")))
    console.print(show(duration.parse("2hr")))
    console.print(show(duration.parse("90")))
    console.print(show(duration.parse("1h30")))
    console.print(show(duration.parse("")))
    console.print(show(duration.parse("abc")))
    console.print(roundtrip(1h + 1m + 1s))
    console.print(roundtrip(90s))
    console.print(roundtrip(250ms))
"#;
        let sources = [
            ("duration", crate::bundled_module("duration").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "duration.parse diverged");
        assert_eq!(
            compiled,
            vec![
                "3723000", "500", "7200000", "90", "none", "none", "none", "ok", "ok", "ok",
            ]
        );
    }

    #[test]
    fn math_ceil_and_round_div_backends_agree() {
        // ceil_div rounds toward +inf for the quotient; round_div rounds to the
        // nearest integer (ties away from zero). Both for a positive divisor.
        let client = r#"
import math
import list
fn show(xs: List(Int)) -> String:
    list.join(list.map(xs, fn(n: Int): "${n}"), ",")
fn main(console: Console):
    console.print(show([math.ceil_div(7, 3), math.ceil_div(6, 3), math.ceil_div(1, 3), math.ceil_div(0, 3)]))
    console.print(show([math.ceil_div(0 - 7, 3), math.ceil_div(0 - 6, 3)]))
    console.print(show([math.round_div(7, 2), math.round_div(5, 3), math.round_div(4, 3), math.round_div(0 - 7, 2)]))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("math", crate::bundled_module("math").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "ceil_div/round_div diverged");
        assert_eq!(compiled, vec!["3,2,1,0", "-2,-2", "4,2,1,-4"]);
    }

    #[test]
    fn math_to_base_backends_agree() {
        // to_base renders a number in base 2..16 (recursively, MSB-first);
        // zero is "0", negatives get a "-". An out-of-range base fails loudly
        // (RFC-0044 rule 3) — covered in std_contract_violations_abort_on_both_backends.
        let client = r#"
import math
fn main(console: Console):
    console.print(math.to_hex(255))
    console.print(math.to_hex(0))
    console.print(math.to_hex(4096))
    console.print(math.to_binary(5))
    console.print(math.to_base(255, 16))
    console.print(math.to_base(0 - 255, 16))
    console.print(math.to_base(0, 2))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "to_base diverged");
        assert_eq!(
            compiled,
            vec!["ff", "0", "1000", "101", "ff", "-ff", "0"]
        );
    }

    #[test]
    fn math_format_float_backends_agree() {
        // format_float renders a Float at a fixed number of places (rounded
        // half-up) using only float arithmetic, so it works on the compiled
        // backend where the `to_string` builtin cannot format floats.
        let client = r#"
import math
fn main(console: Console):
    console.print(math.format_float(3.14159, 2))
    console.print(math.format_float(0.0 - 0.5, 1))
    console.print(math.format_float(2.0, 0))
    console.print(math.format_float(0.0, 2))
    console.print(math.format_float(1.999, 2))
    console.print(math.format_float(0.0 - 0.04, 1))
    console.print(math.format_float(98.6, 1))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "format_float diverged");
        assert_eq!(compiled, vec!["3.14", "-0.5", "2", "0.00", "2.00", "0.0", "98.6"]);
    }

    /// The Fahrenheit-to-Celsius table (K&R / Go tour), reproduced in witchy. It
    /// needs real float output — `math.format_float` makes it compile and agree
    /// on both backends, which the float-formatting-less `to_string` could not.
    #[test]
    fn temperature_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/temperature/src/temperature.witchy").unwrap();
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client.as_str())];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "temperature diverged");
        assert_eq!(compiled[0], "0F = -17.8C");
        assert_eq!(compiled[1], "60F = 15.6C");
    }

    #[test]
    fn floats_in_collections_backends_agree() {
        // 8-byte slots also hold f64, so floats now live in lists and tuples
        // (read back with float_to_int, since Float to_string is still WASM-gated).
        let client = r#"
fn main(console: Console):
    let fs = [1.5, 2.5, 3.5]
    console.print("${list.length(fs)}")
    console.print("${math.to_int(list.at(fs, 1))}")
    let pair = (1.5, 9.5)
    let (lo, hi) = pair
    console.print("${math.to_int(hi)}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "floats-in-collections diverged");
        assert_eq!(compiled, vec!["3", "2", "9"]);
    }

    #[test]
    fn plugin_host_example_runs_on_wasm() {
        // The capability-thesis demo: a list of function-value plugins applied as
        // a pipeline, plus a console-capturing logger closure — identical on both
        // backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../examples/plugin_host/src/plugin_host.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "plugin_host diverged");
        assert_eq!(
            compiled,
            vec!["1 -> 12", "5 -> 20", "10 -> 30", "[log] ran the pipeline"]
        );
    }

    #[test]
    fn bst_example_runs_on_wasm() {
        // The binary search tree (recursive ADT + pattern matching + tree sort)
        // produces identical output on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("main", include_str!("../examples/bst/src/bst.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "bst diverged");
        assert_eq!(
            compiled,
            vec!["1 2 3 4 5 6 7 8 9", "contains 7: true", "contains 10: false"]
        );
    }

    #[test]
    fn generic_stack_example_runs_on_wasm() {
        // A recursive generic ADT `Stack(a)` used at two instantiations (Int and
        // String) with a generic `Option(a)` peek produces identical output on
        // both backends — parametric polymorphism end to end.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../examples/generic_stack/src/generic_stack.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic_stack diverged");
        assert_eq!(
            compiled,
            vec![
                "nums size:  3",
                "words size: 2",
                "nums top:   1",
                "words top:  first",
                "rev nums top:  3",
                "rev words top: second",
            ]
        );
    }

    #[test]
    fn ranges_example_runs_on_wasm() {
        // Integer range patterns (`lo..hi`, `lo..=hi`) are real `Pattern::IntRange`
        // nodes (RFC-0052), so the HTTP-status and grade classifiers match
        // identically on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/ranges/src/ranges.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "ranges diverged");
        assert_eq!(
            compiled,
            vec![
                "200 -> success",
                "204 -> success",
                "301 -> redirect",
                "404 -> client error",
                "503 -> server error",
                "600 -> unknown",
                "95 -> A",
                "83 -> B",
                "71 -> C",
                "42 -> F",
            ]
        );
    }

    #[test]
    fn roman_example_runs_on_wasm() {
        // Greedy table walk by subscript (to_roman) and a char scan with the
        // subtractive rule (from_roman) round-trip identically on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/roman/src/roman.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "roman diverged");
        assert_eq!(
            compiled,
            vec![
                "4 = IV -> 4",
                "9 = IX -> 9",
                "49 = XLIX -> 49",
                "90 = XC -> 90",
                "1994 = MCMXCIV -> 1994",
                "2024 = MMXXIV -> 2024",
            ]
        );
    }

    #[test]
    fn constants_example_runs_on_wasm() {
        // Top-level constants (including ones built from earlier constants) are
        // inlined before both backends, producing identical output.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/constants/src/constants.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "constants diverged");
        assert_eq!(
            compiled,
            vec![
                "1 hour      = 3600s",
                "1 day       = 86400s",
                "1d 2h 3m 4s = 93784s",
            ]
        );
    }

    #[test]
    fn print_trailing_newline_agrees_on_both_backends() {
        // Regression: a printed string ending in `\n` (the line terminator) must
        // produce identical output on both backends. The WASM host strips a
        // trailing newline; the interpreter now does too.
        let src = "fn main(console: Console):\n    console.print(\"ab\" + \"\\n\")\n    console.print(\"cd\")\n";
        let sources = [("main", src)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "trailing-newline print diverged");
        assert_eq!(compiled, vec!["ab", "cd"]);
    }

    #[test]
    fn aliases_example_runs_on_wasm() {
        // Type aliases are expanded before both backends everywhere a type is
        // written — signatures/fields AND body-level positions: the `let`
        // ascription (`hottest: Celsius`), the lambda's alias-typed parameter and
        // return (`Converter`), the `as` narrow through a capability alias
        // (`console as Out`), and the impl head (`impl Describe for Celsius`). So the
        // conversions, averaging, and `.describe()` all agree (RFC H1).
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/aliases/src/aliases.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "aliases diverged");
        assert_eq!(compiled, vec!["avg C = 21", "25C = 77F", "0C  = 32F", "hottest = 25C = 77F"]);
    }

    #[test]
    fn regex_example_runs_on_wasm() {
        // The std/regex backtracking matcher (. * + ? ^ $) produces identical
        // results on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("regex", crate::bundled_module("regex").unwrap()),
            ("main", include_str!("../examples/patterns/src/patterns.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "regex diverged");
        assert_eq!(
            compiled,
            vec![
                "match  ^h.*o$  ~  hello",
                "no     ^h.*o$  ~  hi there",
                "match  colou?r  ~  color",
                "match  colou?r  ~  colour",
                "match  ab+a  ~  abbba",
                "no     ab+a  ~  aa",
                "match  cat  ~  the cat sat",
                "no     ^cat  ~  the cat sat",
            ]
        );
    }

    #[test]
    fn calculator_example_runs_on_wasm() {
        // The recursive-descent calculator (mutual recursion + tuple cursors +
        // string scanning) parses and evaluates expressions identically on both
        // backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../examples/calculator/src/calculator.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "calculator diverged");
        assert_eq!(
            compiled,
            vec![
                "2 + 3 * 4        = 14",
                "(2 + 3) * 4      = 20",
                "100 - 2 * (3 + 4) = 86",
                "7 + 6 / 2 - 1    = 9",
            ]
        );
    }

    #[test]
    fn pipeline_example_runs_on_wasm() {
        // The method-chained data pipeline (filter/map/sum over list.range)
        // prints identically on both backends.
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/pipeline/src/pipeline.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pipeline diverged");
        assert_eq!(compiled, vec!["120", "0,2,4,6,8"]);
    }

    #[test]
    fn method_calls_resolve_to_real_methods_only() {
        // Method-call syntax resolves to impl methods (instance + static) and
        // trait-bound dispatch — NOT to arbitrary free functions. A free
        // function called as a method is a loud error naming the spelling.
        let client = r#"
type Counter:
    n: Int

impl Counter:
    fn fresh() -> Counter:
        Counter(0)
    fn bumped(self) -> Counter:
        Counter(self.n + 1)

fn main(console: Console):
    let c = Counter.fresh().bumped().bumped()
    console.print("${c.n}")
"#;
        let want = vec!["2".to_string()];
        assert_eq!(link_run(client), want, "interpreter");
        assert_eq!(wasm_run(client), want, "wasm");
        // Free-function UFCS is gone — one cut, loud error.
        let ufcs = "fn inc(x: Int) -> Int:\n    x + 1\n\nfn main(console: Console):\n    console.print(\"${5.inc()}\")\n";
        let module = parser::parse_module(ufcs).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("free-fn UFCS must be rejected");
        assert!(
            err.to_string().contains("methods come from `impl` blocks"),
            "got: {err}"
        );
    }

    /// A build step runs in the **WASM sandbox**: it is compiled, instantiated
    /// with only the BuildOut/BuildRead host functions linked, reads a confined
    /// schema and writes generated source — and a `..` write traps via the same
    /// confinement as a runtime `Dir`.
    #[test]
    fn build_step_runs_in_the_wasm_sandbox() {
        let root = std::env::temp_dir().join(format!("witchy_wasm_build_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let schema = root.join("schema");
        std::fs::create_dir_all(&schema).unwrap();
        std::fs::write(schema.join("svc.txt"), "Greeter").unwrap();

        let module = parser::parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    let nl = \"\\n\"\n    out.write_out(\"api.witchy\", \"pub fn service() -> String:\" + nl + \"    \\\"\" + schema.read_build(\"svc.txt\") + \"\\\"\" + nl)\n",
        )
        .expect("parse");
        let generated = crate::run_build_step_sandboxed(
            module,
            root.join("out"),
            vec![schema.clone()],
        )
        .expect("sandboxed build step runs");
        assert_eq!(generated, vec!["api.witchy".to_string()]);
        let body = std::fs::read_to_string(root.join("out/api.witchy")).unwrap();
        assert!(body.contains("\"Greeter\""), "generated source embeds the schema value: {body}");

        // A `..` escape traps inside the sandbox, exactly like a runtime Dir.
        let escaper = parser::parse_module(
            "fn build(out: BuildOut):\n    out.write_out(\"../escape.txt\", \"nope\")\n",
        )
        .unwrap();
        let err = crate::run_build_step_sandboxed(escaper, root.join("out2"), Vec::new())
            .expect_err("a `..` write must trap in the sandbox");
        assert!(err.contains("escapes the Dir capability"), "got: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A confinement violation in the WASM sandbox surfaces the same clean
    /// message both backends print — the root cause, not
    /// wasmtime's "error while executing at wasm backtrace…" wrapper.
    #[test]
    fn sandbox_confinement_error_is_clean() {
        let root = std::env::temp_dir().join(format!("witchy_sandbox_esc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src_path = root.join("reader.witchy");
        std::fs::write(
            &src_path,
            "fn main(console: Console, dir: Dir[Read], args: List(String)) -> Int:\n    console.print(dir.read(list.at(args, 0)))\n    0\n",
        )
        .unwrap();
        let err = crate::run_file_sandboxed(
            src_path.to_str().unwrap(),
            vec![root.clone()],
            Vec::new(),
            Vec::new(),
            vec!["../secret.txt".to_string()],
            None,
            Vec::new(),
        )
        .expect_err("a `..` traversal must be denied");
        assert!(
            err.contains("escapes the Dir capability"),
            "the denial should cite the Dir capability, got: {err}"
        );
        assert!(
            !err.contains("wasm backtrace"),
            "the raw wasmtime backtrace must not leak to the user, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_file_agrees_on_a_simple_program() {
        // `witchy verify` runs a program on both backends and confirms identical
        // output; on a normal program that should succeed.
        let path = std::env::temp_dir().join(format!("witchy_verify_smoke_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "fn main(console: Console):\n    console.print(\"${(2 + 3) * 4}\")\n    console.print(\"hi\")\n",
        )
        .unwrap();
        let outcome = crate::parity_check(path.to_str().unwrap());
        assert!(
            matches!(outcome, crate::ParityOutcome::Agree { .. }),
            "backends should agree: {}",
            outcome.message()
        );
    }

    /// (RFC-0045) `witchy parity` on an aborting program passes only when both
    /// backends produce the same complete location-prefixed diagnostic.
    #[test]
    fn parity_file_agrees_on_matching_aborts() {
        let path = std::env::temp_dir().join(format!("witchy_verify_abort_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "import list\nfn main(console: Console):\n    let xs = [1, 2]\n    console.print(\"${list.at(xs, 9)}\")\n",
        )
        .unwrap();
        let outcome = crate::parity_check(path.to_str().unwrap());
        assert!(
            matches!(outcome, crate::ParityOutcome::BothErrorAgree { .. }),
            "both backends must abort with the same complete diagnostic: {}",
            outcome.message()
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn every_example_type_checks() {
        let entries = example_entries();
        let failures: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(move || {
                    let p = path.to_str().unwrap();
                    crate::check_file(p).err().map(|e| format!("{p}: {e}"))
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(failures.is_empty(), "examples fail to type-check:\n{}", failures.join("\n"));
    }

    #[test]
    fn every_compilable_example_agrees_on_both_backends() {
        // Differential guard: every example that compiles to WASM must produce
        // identical output on the interpreter and the compiled backend. Examples
        // that are interpreter-only (actors, networking, float/case formatting) or
        // are libraries with no `main` cannot compile and are skipped — only a
        // genuine divergence fails. (This would have caught the trailing-newline
        // print divergence.)
        let entries = example_entries();
        let diverged: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    if let crate::ParityOutcome::Diverge { message, .. } = crate::parity_check(p) {
                        Some(message)
                    } else {
                        None
                    }
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            diverged.is_empty(),
            "examples diverge across backends:\n{}",
            diverged.join("\n")
        );
    }

    #[test]
    fn examples_agree_under_rc_floor() {
        // Metamorphic, NO-ORACLE guard for the RC-floor lever: `WITCHY_OPT=rc-floor` adds the
        // dup/drop refcount discipline + free-at-overwrite/last-use reclamation, which must be
        // OUTPUT-TRANSPARENT — compiling with it on produces byte-identical output to the default.
        // A premature or wrong free (a use-after-free) shows up as a divergence here. This is the
        // check that would have caught the free-at-overwrite alias-init UAF (it corrupted
        // `toml.get_array`); before it, NO test ran the examples under this lever — exactly how a
        // gated-lever memory bug hides. Restricted to console-only, `main`-bearing programs so the
        // run needs no capability grants and the output is deterministic.
        use crate::opt::{self, Opt, OptSet};
        let entries = example_entries();
        let diverged: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    let Ok((linked, _)) = crate::link_file(p) else {
                        return None;
                    };
                    if typeck::check(&linked).is_err() {
                        return None;
                    }
                    let has_main = linked
                        .items
                        .iter()
                        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
                    let console_only = crate::capabilities::analyze(&linked)
                        .total
                        .keys()
                        .all(|k| *k == "Console");
                    if !has_main || !console_only {
                        return None;
                    }
                    let compile_with_rc = |on: bool| {
                        opt::set_for_tests(if on {
                            Some(OptSet::default_set().with(Opt::RcFloor))
                        } else {
                            None
                        });
                        let bytes = codegen::compile_module_binary(&linked);
                        opt::set_for_tests(None);
                        bytes
                    };
                    if let (
                        codegen::LoweringOutcome::Lowered(def),
                        codegen::LoweringOutcome::Lowered(rc),
                    ) = (compile_with_rc(false), compile_with_rc(true)) {
                        let a = crate::run_wasm_bytes(&def);
                        let b = crate::run_wasm_bytes(&rc);
                        if a != b {
                            return Some(format!("{p}: default {a:?} vs rc-floor {b:?}"));
                        }
                    }
                    None
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            diverged.is_empty(),
            "rc-floor diverges from the default codegen on examples (a reclamation use-after-free):\n{}",
            diverged.join("\n")
        );
    }

    /// The oracle-based sweep run under an OPT-IN codegen lever: EVERY example — including the
    /// capability-using ones (coven_check's `Dir[Read]`, networking, …) — must still agree
    /// interpreter-vs-compiled with the lever on. The default `every_compilable_example_*` sweep
    /// only exercises the DEFAULT-ON opts, so an opt-in lever's frees / layout changes go
    /// unexercised on real programs — exactly how the free-at-overwrite alias-init use-after-free
    /// hid for ~2 days (it only fired under `WITCHY_OPT=rc-floor`, which no sweep ran). Every
    /// opt-in lever gets a sweep here so that class cannot recur. `set_for_tests` is thread-local,
    /// so the lever is isolated from the parallel test threads.
    fn assert_examples_agree_under(set: crate::opt::OptSet, lever: &str) {
        let entries = example_entries();
        let diverged: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(|| {
                    crate::opt::set_for_tests(Some(set));
                    let p = path.to_str().unwrap();
                    let result = if let crate::ParityOutcome::Diverge { message, .. } = crate::parity_check(p) {
                        Some(message)
                    } else {
                        None
                    };
                    crate::opt::set_for_tests(None);
                    result
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            diverged.is_empty(),
            "examples diverge under WITCHY_OPT={lever} (a codegen bug the default sweep misses):\n{}",
            diverged.join("\n")
        );
    }

    #[test]
    fn every_example_agrees_under_rc_floor() {
        use crate::opt::{Opt, OptSet};
        assert_examples_agree_under(OptSet::default_set().with(Opt::RcFloor), "rc-floor");
    }

    #[test]
    fn every_example_agrees_under_unbox() {
        // Same guard for the other opt-in codegen lever: `unbox` (RFC-0027 packed-by-inference)
        // changes heap LAYOUT, so a missed read/write site would corrupt exactly like a reclamation
        // UAF. It is currently clean, but — like rc-floor — the default sweep never exercised it.
        use crate::opt::{Opt, OptSet};
        assert_examples_agree_under(OptSet::default_set().with(Opt::Unbox), "unbox");
    }

    #[test]
    fn precompiled_wasm_runs_like_the_source() {
        // C Tier 1 (distribution): a program emitted to a `.wasm` and run as a
        // precompiled module — with authority derived from its imports — produces
        // the same output as running the source. This is "ship the .wasm, run it
        // with witchy".
        let tmp = std::env::temp_dir().join(format!("witchy_tier1_precompiled_{}.wasm", std::process::id()));
        let out = tmp.to_str().unwrap();
        crate::emit_wasm_file("examples/calc/src/calc.witchy", out).expect("emit-wasm");
        let (from_wasm, _) =
            crate::run_wasm_file(out, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, Vec::new(), false).expect("run .wasm");
        let from_source = crate::execute_file("examples/calc/src/calc.witchy", Vec::new()).expect("run source");
        assert_eq!(from_wasm, from_source, "precompiled .wasm diverges from the source run");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn std_ord_string_and_sort_backends_agree() {
        // `impl Ord for String` makes strings comparable, and the bounded generic
        // `list.sort` dispatches through the element's Ord impl — so it sorts
        // runtime-BUILT strings content-correctly on both backends (a pointer
        // comparison sort would scramble them in compiled code). Also covers
        // Ord-over-String for max_of/maximum and Ints via the same `sort`.
        let client = r#"
import cmp

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < s.char_count()):
        acc = (acc + s.substring(i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    var words = [build("pear"), build("apple"), build("fig"), build("apple")]
    list.sort(words)
    console.print(list.join(words, ","))
    var letters = ["c", "a", "b"]
    list.sort(letters)
    console.print(list.join(letters, ""))
    console.print(cmp.max_of(build("alpha"), build("omega")))
    console.print(cmp.maximum([build("x"), build("a"), build("m")], ""))
    var nums = [3, 1, 2, 1]
    list.sort(nums)
    console.print("${(list.at(nums, 0) + (list.at(nums, 3) * 10))}")
"#;
        let sources = [
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std ord string/sort diverged");
        assert_eq!(
            compiled,
            vec!["apple,apple,fig,pear", "abc", "omega", "x", "31"]
        );
    }

    fn assert_fn_compiles(src: &str) {
        assert!(typeck::check_str(src).is_ok(), "{:?}", typeck::check_str(src));
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        Module::new(&wasm_gc_engine(), &bytes).expect("valid wasm");
    }

    /// WIR migration progress meter (not an assertion): reports how many example
    /// programs take the AST→WIR→wasm-binary path vs. still fall back to WAT.
    /// Run with `cargo test --features native binary_path_coverage_report --
    /// --ignored --nocapture`; add `WIRDIAG=1` to also print, per bailing program,
    /// which function(s) didn't lower (the `assemble_wir_module` diagnostic).
    /// Library files (no `main`) can't be a standalone binary, so they're skipped
    /// rather than counted as fallbacks.
    #[test]
    #[ignore]
    fn binary_path_coverage_report() {
        let mut dirs = vec![std::path::PathBuf::from("examples")];
        let mut srcs: Vec<std::path::PathBuf> = vec![];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("witchy") {
                    srcs.push(p);
                }
            }
        }
        srcs.sort();
        let (mut ok, mut total) = (0, 0);
        let mut bailed: Vec<String> = vec![];
        for p in srcs {
            let ps = p.to_str().unwrap().to_string();
            if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("serve_")) {
                continue;
            }
            let linked = match crate::link_file(&ps) {
                Ok((m, _)) => m,
                Err(_) => continue,
            };
            if typeck::check(&linked).is_err() {
                continue;
            }
            // Skip library modules (no `main`): they're linked INTO a main program,
            // never compiled standalone, so they aren't real binary-path fallbacks.
            let has_main = linked
                .items
                .iter()
                .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
            if !has_main {
                continue;
            }
            total += 1;
            if matches!(
                codegen::compile_module_binary(&linked),
                codegen::LoweringOutcome::Lowered(_)
            ) {
                ok += 1;
            } else {
                bailed.push(ps);
            }
        }
        eprintln!("\n=== WIR binary-path coverage: {ok}/{total} ===");
        for b in &bailed {
            eprintln!("  fallback: {b}");
        }
    }

    /// End-to-end through the *compiled* path: type-check, compile to WASM, run
    /// on the wasmtime runtime with the output capabilities granted, and return
    /// what the program printed.
    fn run_on_wasm(src: &str) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                // Mirror the interpreter's automatic grants (output + the
                // read-only ambient Clock/Env), like `run_wasm_bytes`.
                Capabilities {
                    print: true,
                    print_int: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }

    /// Run a WIR-assembled binary with ONLY `print` granted — nothing else. If
    /// the module imported any other authority, instantiate would fail. Proves
    /// the pruned WIR-helper path emits capability-minimal modules.
    fn run_bytes_print_only(bytes: &[u8]) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                bytes,
                Capabilities { print: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn under a print-only grant");
        actor.run().expect("run");
        actor.output()
    }

    /// Run a WIR-assembled binary with EVERY capability granted. The static
    /// prelude is "all features on", so a raw-body-path module imports the full
    /// host surface; granting everything lets it instantiate. (The pruned
    /// WIR-helper path emits capability-minimal modules — see `run_bytes_print_only`.)
    fn run_bytes_all_caps(bytes: &[u8]) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    quiet: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
                    signing_key: Some([0u8; 32]),
                    build_out: Some(std::env::temp_dir()),
                    build_read_roots: vec![std::path::PathBuf::from(".")],
                    ..Default::default()
                },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }


    /// Run a WIR-binary under a print-only grant and read the exported
    /// `__witchy_reowns` counter — the timing-free proof of in-place (O(1)) vs
    /// copying (O(n)) accumulation.
    fn binary_run_reowns(bytes: &[u8]) -> (Vec<String>, i64) {
        use crate::runtime::{Capabilities, Runtime};
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                bytes,
                Capabilities { print: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        let reowns = actor.reowns().unwrap_or(0);
        (actor.output(), reowns)
    }

    /// An `var` fn with an EARLY `return` on the binary path: the return must
    /// yield the full multi-result tuple (the declared value, then each var
    /// param's final value) so the arity matches the move-out ABI — a single
    /// `N::Return` would mismatch and the whole module bailed to WAT. `clamp`
    /// returns early when `n > 10`; both the early and fall-through exits write
    /// `n` back into the caller's variable.
    #[test]
    fn wir_var_early_return_binary_path() {
        let src = "fn clamp(var n: Int):\n    if (n > 10):\n        n = 10\n        return\n    n = n + 1\n\nfn main(console: Console):\n    var a = 5\n    clamp(a)\n    console.print(\"${a}\")\n    var b = 50\n    clamp(b)\n    console.print(\"${b}\")\n";
        let want = vec!["6".to_string(), "10".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower an var fn with an early return");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// Criterion-2: the slot-elimination pass shows a MEASURABLE improvement on a
    /// real lowered program. `[list.at(xs, 0)]` (with `xs: List(Bool)`) reads an
    /// i64 slot, narrows it to the bool's i32, then re-widens it to store in the
    /// new list — a redundant `ToSlot(FromSlot(..))` the pass removes. The
    /// optimized binary still runs identically to the interpreter oracle.
    #[test]
    fn wir_slot_elimination_shows_measurable_improvement() {
        let src = "fn main(console: Console):\n    let xs = [true, false]\n    let ys = [list.at(xs, 0)]\n    if list.at(ys, 0):\n        console.print(\"t\")\n    else:\n        console.print(\"f\")\n";
        let want = vec!["t".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let m = codegen::assemble_wir_module(&linked)
            .expect_lowered("program takes the WIR binary path");
        // Measurable: the pass removes redundant slot conversions.
        let mut opt_m = m.clone();
        let stats = crate::wir_opt::optimize(&mut opt_m);
        assert!(
            stats.eliminated > 0,
            "expected the slot-elimination pass to remove nodes, eliminated={}",
            stats.eliminated
        );
        // Oracle-validated: both the unoptimized and optimized binaries match the
        // interpreter (a behavior-preserving win, not a behavior change).
        assert_eq!(run_bytes_print_only(&crate::wir_encode::encode(&m, &[])), want, "unoptimized");
        assert_eq!(run_bytes_print_only(&crate::wir_encode::encode(&opt_m, &[])), want, "optimized");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// The `wir_opt` slot-elimination pass is a SOUND, behavior-preserving
    /// rewrite: for every lowering-subset program, the unoptimized and optimized
    /// binaries both run identically to the interpreter oracle. (Node-count
    /// reduction is unit-tested in `wir_opt` on synthetic `FromSlot(ToSlot)`
    /// redundancy; the current lowering emits no such round-trips — those arise
    /// at generic/monomorphization boundaries that do not lower yet — so
    /// `eliminated` is 0 on these real programs. The measurable payoff lands when
    /// that lowering does, producing the redundancy the pass removes.)
    #[test]
    fn wir_slot_elimination_is_behavior_preserving() {
        let progs = [
            "fn main(console: Console):\n    console.print(\"hi\")\n",
            "fn inc(n: Int) -> Int:\n    n + 1\n\nfn main(console: Console):\n    if inc(inc(0)) > 1:\n        console.print(\"ok\")\n    else:\n        console.print(\"no\")\n",
            "fn classify(n: Int) -> Bool:\n    match n:\n        0 -> true\n        _ -> false\n\nfn main(console: Console):\n    if classify(0):\n        console.print(\"zero\")\n    else:\n        console.print(\"nonzero\")\n",
        ];
        for src in progs {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("typecheck");
            let m = codegen::assemble_wir_module(&linked)
                .expect_lowered(&format!("expected the WIR binary path to handle:\n{src}"));
            let oracle = link_run(src);
            // Unoptimized encoding runs like the oracle...
            let unopt = crate::wir_encode::encode(&m, &[]);
            assert_eq!(run_bytes_all_caps(&unopt), oracle, "unoptimized:\n{src}");
            // ...and the optimized encoding runs identically (sound rewrite).
            let mut opt_m = m.clone();
            let stats = crate::wir_opt::optimize(&mut opt_m);
            assert!(stats.nodes_after <= stats.nodes_before, "the pass never grows the tree");
            let opt = crate::wir_encode::encode(&opt_m, &[]);
            assert_eq!(run_bytes_all_caps(&opt), oracle, "optimized:\n{src}");
        }
    }

    /// M3 sink-flip: the WIR→binary path (`compile_module_binary`, NO
    /// `wat::parse_str`) must, for every program whose whole module lowers,
    /// assemble a VALID wasm module that runs identically to the interpreter
    /// oracle and to the legacy WAT path. Programs are chosen from the lowering
    /// subset (string literals + control flow + scalar helpers; no list-building,
    /// string concat, generated render, or Int/Float `main` yet).
    #[test]
    fn wir_binary_path_runs_and_agrees_with_oracle() {
        let cases: &[(&str, Vec<String>)] = &[
            (
                "fn main(console: Console):\n    console.print(\"hello from WIR\")\n",
                vec!["hello from WIR".to_string()],
            ),
            (
                "fn main(console: Console):\n    console.print(\"one\")\n    console.print(\"two\")\n",
                vec!["one".to_string(), "two".to_string()],
            ),
            (
                "fn main(console: Console):\n    if true:\n        console.print(\"yes\")\n    else:\n        console.print(\"no\")\n",
                vec!["yes".to_string()],
            ),
            (
                "fn pick(b: Bool) -> Bool:\n    b\n\nfn main(console: Console):\n    if pick(true):\n        console.print(\"picked\")\n    else:\n        console.print(\"nope\")\n",
                vec!["picked".to_string()],
            ),
            // An aggregate: builds a tuple ($mk2 → $ensure) and destructures it —
            // exercises the migrated allocator helpers on the pruned binary path.
            (
                "fn main(console: Console):\n    let t = (1, 2)\n    let (a, b) = t\n    if a < b:\n        console.print(\"ordered\")\n    else:\n        console.print(\"no\")\n",
                vec!["ordered".to_string()],
            ),
            // A list with indexing ($mk3 → $ensure, $list_at) on the binary path.
            (
                "fn main(console: Console):\n    let xs = [10, 20, 30]\n    if list.at(xs, 1) == 20:\n        console.print(\"twenty\")\n    else:\n        console.print(\"no\")\n",
                vec!["twenty".to_string()],
            ),
            // Integer rendering ($int_to_string → $ensure) on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${42}\")\n    console.print(\"${-7}\")\n",
                vec!["42".to_string(), "-7".to_string()],
            ),
            // String content equality ($str_eq) on the binary path.
            (
                "fn main(console: Console):\n    if \"abc\" == \"abc\":\n        console.print(\"eq\")\n    else:\n        console.print(\"ne\")\n    if \"abc\" == \"xyz\":\n        console.print(\"eq2\")\n    else:\n        console.print(\"ne2\")\n",
                vec!["eq".to_string(), "ne2".to_string()],
            ),
            // String concatenation ($concat → $ensure) on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"hello, \" + \"world\")\n    console.print(\"x\" + \"y\" + \"z\")\n",
                vec!["hello, world".to_string(), "xyz".to_string()],
            ),
            // list.length on the binary path.
            (
                "fn main(console: Console):\n    let xs = [10, 20, 30]\n    console.print(\"${list.length(xs)}\")\n",
                vec!["3".to_string()],
            ),
            // string.length on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${\"hello\".length()}\")\n",
                vec!["5".to_string()],
            ),
            // string.contains ($find_byte — a conditional br inside a loop) on
            // the binary path.
            (
                "fn main(console: Console):\n    if \"hello\".contains(\"ell\"):\n        console.print(\"yes\")\n    else:\n        console.print(\"no\")\n    if \"hello\".contains(\"xyz\"):\n        console.print(\"yes2\")\n    else:\n        console.print(\"no2\")\n",
                vec!["yes".to_string(), "no2".to_string()],
            ),
            // string.starts_with ($starts_with — prefix byte-compare loop) on
            // the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${\"hello\".starts_with(\"hel\")}\")\n    console.print(\"${\"hello\".starts_with(\"lo\")}\")\n    console.print(\"${\"hello\".starts_with(\"\")}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string()],
            ),
            // string.ends_with ($ends_with — suffix byte-compare loop) on the
            // binary path.
            (
                "fn main(console: Console):\n    console.print(\"${\"hello\".ends_with(\"llo\")}\")\n    console.print(\"${\"hello\".ends_with(\"hel\")}\")\n    console.print(\"${\"hello\".ends_with(\"\")}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string()],
            ),
            // string.substring ($str_substring → $char_to_byte + $substr, a
            // heap-allocating slice) on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"hello world\".substring(0, 5))\n    console.print(\"hello world\".substring(6, 11))\n",
                vec!["hello".to_string(), "world".to_string()],
            ),
            // string.trim ($trim → $is_ws + $substr, two whitespace scan loops)
            // on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"  hi  \".trim())\n    console.print(\"abc\".trim())\n",
                vec!["hi".to_string(), "abc".to_string()],
            ),
            // string.split ($split → $substr + $list_push, nested scan/compare
            // loops building a List(String)) on the binary path; indexed with
            // the already-migrated $list_at.
            (
                "fn main(console: Console):\n    let parts = \"a,b,c\".split(\",\")\n    console.print(list.at(parts, 0))\n    console.print(list.at(parts, 1))\n    console.print(list.at(parts, 2))\n",
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ),
            // for-loop over a list with an arena-resettable body (the watermark
            // optimization, ported to WIR): per-iteration `$heap` save/restore.
            (
                "fn main(console: Console):\n    for piece in \"a,b,c\".split(\",\"):\n        console.print(piece)\n",
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ),
            // range for-loop whose body allocates per iteration (nothing escapes,
            // so it's watermarked) — exercises the range-for arena reset on WIR.
            (
                "fn main(console: Console):\n    for i in 0..3:\n        console.print(\"abcdef\".substring(i, i + 2))\n",
                vec!["ab".to_string(), "bc".to_string(), "cd".to_string()],
            ),
            // while-loop with an arena-resettable allocating body (the watermark
            // now ported to WIR for `while` too).
            (
                "fn main(console: Console):\n    var i: Int = 0\n    while i < 3:\n        console.print(\"abcdef\".substring(i, i + 2))\n        i = i + 1\n",
                vec!["ab".to_string(), "bc".to_string(), "cd".to_string()],
            ),
            // match on an ADT constructor with a payload bind (Some(n)) / a
            // nullary variant (None) — the new lower_pattern Ctor arm.
            (
                "fn pick(b: Bool) -> Option(Int):\n    if b:\n        Some(7)\n    else:\n        None\n\nfn main(console: Console):\n    console.print(\"${match pick(true):\n        Some(n) -> n\n        None -> 99}\")\n    console.print(\"${match pick(false):\n        Some(n) -> n\n        None -> 99}\")\n",
                vec!["7".to_string(), "99".to_string()],
            ),
            // match on string-literal patterns (str_eq) with a wildcard fallback.
            (
                "fn classify(s: String) -> Int:\n    match s:\n        \"yes\" -> 1\n        \"no\" -> 0\n        _ -> 9\n\nfn main(console: Console):\n    console.print(\"${classify(\"yes\")}\")\n    console.print(\"${classify(\"no\")}\")\n    console.print(\"${classify(\"maybe\")}\")\n",
                vec!["1".to_string(), "0".to_string(), "9".to_string()],
            ),
            // match with a LITERAL constructor field (Some(0)) — the short-circuit
            // `if tag == Some: field == 0` path of the Ctor pattern arm.
            (
                "fn check(o: Option(Int)) -> Int:\n    match o:\n        Some(0) -> 100\n        Some(n) -> n\n        None -> 99\n\nfn main(console: Console):\n    console.print(\"${check(Some(0))}\")\n    console.print(\"${check(Some(5))}\")\n    console.print(\"${check(None)}\")\n",
                vec!["100".to_string(), "5".to_string(), "99".to_string()],
            ),
            // list patterns: empty, exact-length head bind, and a `[h, ..t]` tail
            // bind (via $list_drop).
            (
                "fn sum_head(xs: List(Int)) -> Int:\n    match xs:\n        [] -> 0\n        [a, b] -> a + b\n        [h, ..t] -> h + list.length(t)\n        _ -> 99\n\nfn main(console: Console):\n    console.print(\"${sum_head([])}\")\n    console.print(\"${sum_head([10, 20])}\")\n    console.print(\"${sum_head([5, 1, 2, 3])}\")\n",
                vec!["0".to_string(), "30".to_string(), "8".to_string()],
            ),
            // structural `==` on scalar-field compounds: a tuple, a list, and a
            // tuple with a String field ($str_eq). Distinct literals so a stray
            // pointer-compare would diverge from the structural result.
            (
                "fn main(console: Console):\n    console.print(\"${(1, 2) == (1, 2)}\")\n    console.print(\"${(1, 2) == (1, 3)}\")\n    console.print(\"${[1, 2, 3] == [1, 2, 3]}\")\n    console.print(\"${[1, 2] == [1, 9]}\")\n    console.print(\"${(\"a\", 1) == (\"a\", 1)}\")\n    console.print(\"${(\"a\", 1) == (\"b\", 1)}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string(), "false".to_string(), "true".to_string(), "false".to_string()],
            ),
            // NESTED structural `==`: a list of tuples and a tuple of (tuple, int)
            // — slot_cmp_wir recurses into the field shapes' eq helpers.
            (
                "fn main(console: Console):\n    console.print(\"${[(1, 2), (3, 4)] == [(1, 2), (3, 4)]}\")\n    console.print(\"${[(1, 2)] == [(1, 9)]}\")\n    console.print(\"${((1, 2), 3) == ((1, 2), 3)}\")\n    console.print(\"${((1, 2), 3) == ((1, 9), 3)}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string(), "false".to_string()],
            ),
            // Structural render of compounds (the $ts renderer): a tuple, a tuple
            // with a String + Bool field, and a list — built with $concat/
            // $int_to_string.
            (
                "fn main(console: Console):\n    console.print(\"${(1, 2)}\")\n    console.print(\"${(\"hi\", true)}\")\n    console.print(\"${[1, 2, 3]}\")\n    console.print(\"${[true, false]}\")\n",
                vec!["(1, 2)".to_string(), "(hi, true)".to_string(), "[1, 2, 3]".to_string(), "[true, false]".to_string()],
            ),
            // a record: structural `==` (eq helper) and render (ts helper,
            // `Name(f0, f1)`) on the binary path.
            (
                "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    console.print(\"${Point(1, 2)}\")\n    console.print(\"${Point(1, 2) == Point(1, 2)}\")\n    console.print(\"${Point(1, 2) == Point(1, 9)}\")\n",
                vec!["Point(1, 2)".to_string(), "true".to_string(), "false".to_string()],
            ),
            // a tuple with a Float field renders via $float_to_str (host import).
            (
                "fn main(console: Console):\n    console.print(\"${(1.5, 2)}\")\n",
                vec!["(1.5, 2)".to_string()],
            ),
            // a closure: a lambda bound to a local, then called (the lifted body +
            // closure object + call_indirect on the binary path).
            (
                "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    console.print(\"${f(5)}\")\n    console.print(\"${f(10)}\")\n",
                vec!["6".to_string(), "11".to_string()],
            ),
            // string.chars ($str_chars → $byte_to_char + $str_substring +
            // $list_push) splitting a multibyte string into a List(String).
            (
                "fn main(console: Console):\n    let cs = \"héllo\".chars()\n    console.print(list.at(cs, 0))\n    console.print(list.at(cs, 1))\n    console.print(list.at(cs, 4))\n",
                vec!["h".to_string(), "é".to_string(), "o".to_string()],
            ),
            // list.concat ($list_concat — two memory.copy's into a fresh slot
            // array) on the binary path.
            (
                "fn main(console: Console):\n    let xs = list.concat([10, 20], [30, 40])\n    console.print(\"${list.at(xs, 0)}\")\n    console.print(\"${list.at(xs, 2)}\")\n    console.print(\"${list.at(xs, 3)}\")\n",
                vec!["10".to_string(), "30".to_string(), "40".to_string()],
            ),
            // string.to_upper / to_lower ($ascii_case byte transform) on the
            // binary path.
            (
                "fn main(console: Console):\n    console.print(\"Hello, World!\".to_upper())\n    console.print(\"Hello, World!\".to_lower())\n",
                vec!["HELLO, WORLD!".to_string(), "hello, world!".to_string()],
            ),
            // string.to_int ($str_to_int — whitespace/sign/overflow-checked parse)
            // on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${\"123\".to_int() + \"-23\".to_int()}\")\n",
                vec!["100".to_string()],
            ),
            // string.replace ($replace + $match_at — count-then-fill) on the
            // binary path, including a growing replacement.
            (
                "fn main(console: Console):\n    console.print(\"hello world\".replace(\"o\", \"0\"))\n    console.print(\"a.b.c\".replace(\".\", \"::\"))\n",
                vec!["hell0 w0rld".to_string(), "a::b::c".to_string()],
            ),
            // dict with String keys ($dict_new/insert/get_or/has/size →
            // $dict_find + $key_eq's $str_eq path) on the binary path.
            (
                "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n    console.print(\"${dict.get_or(d, \"a\", 0)}\")\n    console.print(\"${dict.get_or(d, \"z\", 99)}\")\n    console.print(\"${dict.contains_key(d, \"b\")}\")\n    console.print(\"${dict.contains_key(d, \"z\")}\")\n    console.print(\"${dict.length(d)}\")\n",
                vec!["1".to_string(), "99".to_string(), "true".to_string(), "false".to_string(), "2".to_string()],
            ),
            // dict iteration + remove ($dict_keys/values/pairs/remove). Asserts
            // order-independent facts (lengths, post-remove membership) so it's
            // robust to entry ordering.
            (
                "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n    console.print(\"${list.length(dict.keys(d))}\")\n    console.print(\"${list.length(dict.values(d))}\")\n    console.print(\"${list.length(dict.pairs(d))}\")\n    var d2 = d\n    dict.remove(d2, \"a\")\n    console.print(\"${dict.length(d2)}\")\n    console.print(\"${dict.contains_key(d2, \"a\")}\")\n    console.print(\"${dict.contains_key(d2, \"b\")}\")\n",
                vec!["2".to_string(), "2".to_string(), "2".to_string(), "1".to_string(), "false".to_string(), "true".to_string()],
            ),
            // a capturing closure: the lambda closes over `k` (an Int local),
            // recovered from the env at offset 4 on the binary path.
            (
                "fn main(console: Console):\n    let k = 10\n    let g = fn(n: Int): n + k\n    console.print(\"${g(5)}\")\n    console.print(\"${g(0)}\")\n",
                vec!["15".to_string(), "10".to_string()],
            ),
            // a closure passed to a user function and called through its
            // fn-typed param (`f(f(x))` — the closure-typed-local call_indirect).
            (
                "fn apply_twice(f: fn(Int) -> Int, x: Int) -> Int:\n    f(f(x))\nfn main(console: Console):\n    let k = 10\n    let g = fn(n: Int): n + k\n    console.print(\"${apply_twice(g, 1)}\")\n",
                vec!["21".to_string()],
            ),
            // short-circuit `&&`/`||` lower to a value-`If` on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${true && false}\")\n    console.print(\"${true || false}\")\n    console.print(\"${1 < 2 && 3 < 4}\")\n    console.print(\"${1 > 2 || 3 < 4}\")\n",
                vec!["false".to_string(), "true".to_string(), "true".to_string(), "true".to_string()],
            ),
            // `&&` must short-circuit: the RHS index would be out of bounds when the
            // LHS guard (`i < n`) is false, so it must NOT be evaluated.
            (
                "fn main(console: Console):\n    let xs = [10, 20]\n    let n = list.length(xs)\n    var i = 0\n    var sum = 0\n    while i < n && list.at(xs, i) > 0:\n        sum = sum + list.at(xs, i)\n        i = i + 1\n    console.print(\"${sum}\")\n",
                vec!["30".to_string()],
            ),
            // float ordering (`<`/`<=`/`>`/`>=`) lowers to the NaN-trapping
            // `$f_lt`/`$f_le`/`$f_gt`/`$f_ge` helpers on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${1.5 < 2.5}\")\n    console.print(\"${2.5 <= 2.5}\")\n    console.print(\"${3.5 > 2.5}\")\n    console.print(\"${1.5 >= 2.5}\")\n",
                vec!["true".to_string(), "true".to_string(), "true".to_string(), "false".to_string()],
            ),
            // string ordering (`<`/`<=`/`>`/`>=`) lowers to `$str_cmp` sign
            // compares — lexicographic, including the prefix tie-break by length.
            (
                "fn main(console: Console):\n    console.print(\"${\"abc\" < \"abd\"}\")\n    console.print(\"${\"abc\" < \"ab\"}\")\n    console.print(\"${\"abc\" <= \"abc\"}\")\n    console.print(\"${\"b\" > \"abc\"}\")\n    console.print(\"${\"abc\" >= \"abd\"}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string(), "true".to_string(), "false".to_string()],
            ),
            // a string accumulator (`s = s + ..`) — a self-assign whose in-place fast
            // path is list-only — lowers as a plain value-rebind (the `list.join`
            // shape that blocked ~20 programs). The if/else picks first vs separator.
            (
                "fn main(console: Console):\n    var s = \"\"\n    var first = true\n    for w in [\"a\", \"b\", \"c\"]:\n        if first:\n            s = w\n            first = false\n        else:\n            s = s + \"-\" + w\n    console.print(s)\n",
                vec!["a-b-c".to_string()],
            ),
            // `string.char_count` (Unicode scalars, not bytes) via the `$char_count`
            // → `$byte_to_char` helper — the blocker for parse_int/pad_*.
            (
                "fn main(console: Console):\n    console.print(\"${\"abc\".char_count()}\")\n    console.print(\"${\"héllo\".char_count()}\")\n",
                vec!["3".to_string(), "5".to_string()],
            ),
            // Int<->Float numeric conversions + sqrt (the new `ToFloat`/`ToInt`/`Sqrt`
            // UnOps) and scalar Float render (via `$float_to_str`).
            (
                "fn main(console: Console):\n    console.print(\"${math.to_int(math.sqrt(16.0))}\")\n    console.print(\"${math.to_int(math.to_float(7) + 0.5)}\")\n    console.print(\"${3.5}\")\n",
                vec!["4".to_string(), "7".to_string(), "3.5".to_string()],
            ),
            // `string.from_code` (Unicode scalar -> single-char string) via the
            // `$string_from_code` host-import wrapper.
            (
                "fn main(console: Console):\n    console.print(string.from_code(65))\n    console.print(string.from_code(233))\n",
                vec!["A".to_string(), "é".to_string()],
            ),
            // a closure bound from a MATCH pattern then called (`Box(f) -> f(x)`) —
            // the `iter.next` shape (`Iter(thunk) -> thunk()`). Now lowers since a
            // local in call position is always a closure (the guard is just `locals`).
            (
                "type Box:\n    Box(fn(Int) -> Int)\nfn apply(b: Box, x: Int) -> Int:\n    match b:\n        Box(f) -> f(x)\nfn main(console: Console):\n    let b = Box(fn(n: Int): n + 1)\n    console.print(\"${apply(b, 5)}\")\n",
                vec!["6".to_string()],
            ),
            // nested lambdas: an outer lambda built inside another function's body,
            // with two instances in a list — exercises the lifted-lambda index/name
            // fix (a nested lambda lowered during the outer's build must not collide
            // on the outer's table slot).
            (
                "type Adder:\n    Adder(fn(Int) -> Int)\nfn make(base: Int) -> Adder:\n    Adder(fn(x: Int): x + base)\nfn run(a: Adder, v: Int) -> Int:\n    match a:\n        Adder(f) -> f(v)\nfn main(console: Console):\n    let pair = [make(10), make(100)]\n    console.print(\"${run(list.at(pair, 0), 5)}\")\n    console.print(\"${run(list.at(pair, 1), 5)}\")\n",
                vec!["15".to_string(), "105".to_string()],
            ),
            // a bare top-level function name passed as a VALUE to a higher-order fn —
            // materialized as a forwarding closure `fn(p): is_odd(p)`.
            (
                "fn is_odd(n: Int) -> Bool:\n    n % 2 == 1\nfn count_if(xs: List(Int), pred: fn(Int) -> Bool) -> Int:\n    var c = 0\n    for x in xs:\n        if pred(x):\n            c = c + 1\n    c\nfn main(console: Console):\n    console.print(\"${count_if([1, 2, 3, 4, 5], is_odd)}\")\n",
                vec!["3".to_string()],
            ),
            // a `region:` block — a scalar result (reclaimed by stashing the value in
            // a register and resetting `$heap`) and a `List(Int)` result (reclaimed via
            // the generated `$rcopy_list_int`: scalar payload, one `memory.copy`).
            (
                "fn main(console: Console):\n    let s = region -> Int:\n        var sum = 0\n        for i in 0..10:\n            sum = sum + i\n        sum\n    console.print(\"${s}\")\n    let xs = region -> List(Int):\n        var ys = []\n        for i in 0..5:\n            list.push(ys, i * i)\n        ys\n    console.print(\"${list.at(xs, 3)}\")\n",
                vec!["45".to_string(), "9".to_string()],
            ),
            // a `region -> (Int, String):` tuple — the generated `$rcopy_tuple_*`
            // copies the tag, the scalar slot verbatim, and recurses through
            // `$rcopy_str` for the string slot. The biased copy-out keeps `t.1`
            // pointing at the reclaimed string; `after` reuses the freed space.
            (
                "fn main(console: Console):\n    let t = region -> (Int, String):\n        var acc = \"\"\n        for i in 0..3:\n            acc = acc + \"z\"\n        (7 * 6, acc)\n    let after = \"OK\"\n    console.print(\"${t}\")\n    console.print(t.1)\n    console.print(after)\n",
                vec!["(42, zzz)".to_string(), "zzz".to_string(), "OK".to_string()],
            ),
            // a `region -> List(String):` — a list with a COMPOUND payload: the
            // generated `$rcopy_list_str` writes the length header then deep-copies
            // each element string through `$rcopy_str`, so every slot holds a biased
            // pointer into the reclaimed block.
            (
                "fn main(console: Console):\n    let xs = region -> List(String):\n        var ys = []\n        for i in 0..3:\n            list.push(ys, \"n\" + \"${i}\")\n        ys\n    let after = \"OK\"\n    console.print(list.at(xs, 0))\n    console.print(list.at(xs, 2))\n    console.print(after)\n",
                vec!["n0".to_string(), "n2".to_string(), "OK".to_string()],
            ),
            // enum/record structural render: the generated `$ts_*` tag-dispatch
            // helper emits `Name` (nullary), `Name(f0, f1, ...)` (fields), and a
            // record positionally (`Point(5, 6)`), matching the interpreter's
            // `Value::Ctor` Display. Unlike enum `==`, the WAT path renders enums
            // structurally too, so all three agree.
            (
                "type Color:\n    Red\n    Green\n    RGB(Int, Int, Int)\n\ntype Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let c = RGB(1, 2, 3)\n    let p = Point(x: 5, y: 6)\n    let g = Green\n    console.print(\"${c}\")\n    console.print(\"${p}\")\n    console.print(\"${g}\")\n",
                vec!["RGB(1, 2, 3)".to_string(), "Point(5, 6)".to_string(), "Green".to_string()],
            ),
            // Render of an INLINE call result (`"${mklist()}"`) — the shape comes
            // from typeck's type table (eq_operand_shape), not just tracked locals,
            // so a compound expression renders without being bound to a `let` first.
            (
                "fn mklist() -> List(Int):\n    [1, 2, 3]\n\nfn pair() -> (Int, String):\n    (7, \"x\")\n\nfn main(console: Console):\n    console.print(\"${mklist()}\")\n    console.print(\"${pair()}\")\n",
                vec!["[1, 2, 3]".to_string(), "(7, x)".to_string()],
            ),
            // Render of a self-RECURSIVE ADT (`Node(Tree, Tree)`): the `$ts`
            // helper's name is reserved before its body is built, so the nested
            // `Tree` fields render via a recursive `call` to the same helper
            // (tying the knot) rather than bailing the cycle guard. The WAT path
            // renders enums structurally too, so all three backends agree.
            (
                "type Tree:\n    Leaf(Int)\n    Node(Tree, Tree)\n\nfn main(console: Console):\n    let t = Node(Node(Leaf(1), Leaf(2)), Leaf(3))\n    console.print(\"${t}\")\n",
                vec!["Node(Node(Leaf(1), Leaf(2)), Leaf(3))".to_string()],
            ),
            // `var` parameters (the multi-value move-out ABI): the callee returns
            // its declared value plus each var param's final value, and the call
            // site (`CallStoreMulti`) writes them back into the caller's vars. Covers
            // a bare var, repeated calls, and an var alongside a non-var arg.
            (
                "fn bump(var n: Int):\n    n = n + 1\nfn add(var n: Int, by: Int):\n    n = n + by\nfn main(console: Console):\n    var a = 0\n    bump(a)\n    bump(a)\n    bump(a)\n    add(a, 10)\n    console.print(\"${a}\")\n",
                vec!["13".to_string()],
            ),
            // a `region -> String:` — a POINTER result reclaimed via `$rcopy_str`
            // (deep-copy the region-born string down to the watermark, return the
            // biased ptr). The following `let after` allocates right where the region
            // was reclaimed, so a bad copy/slide would corrupt it.
            (
                "fn main(console: Console):\n    let s = region -> String:\n        var acc = \"\"\n        for i in 0..5:\n            acc = acc + \"x\"\n        acc\n    let after = \"ok\"\n    console.print(s)\n    console.print(after)\n",
                vec!["xxxxx".to_string(), "ok".to_string()],
            ),
        ];
        assert!(!cases.is_empty(), "the WIR binary path lowered nothing — convergence regressed");
        std::thread::scope(|s| {
            let handles: Vec<_> = cases.iter().map(|(src, want)| {
                s.spawn(move || {
                    let module = parser::parse_module(src).expect("parse");
                    let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
                    typeck::check(&linked).expect("typecheck");
                    let bytes = codegen::compile_module_binary(&linked)
                        .expect_lowered(&format!(
                            "expected the WIR binary path to handle this program:\n{src}"
                        ));
                    assert_eq!(&run_bytes_print_only(&bytes), want, "binary path (print-only):\n{src}");
                    assert_eq!(&link_run(src), want, "interpreter oracle:\n{src}");
                    assert_eq!(&run_on_wasm(src), want, "legacy WAT path:\n{src}");
                })
            }).collect();
            for h in handles { h.join().unwrap(); }
        });
    }

    /// The first host-import helper ($encoding) on the binary path. Kept out of
    /// the corpus above because `encoding.*` requires `import encoding`, which the
    /// corpus's `run_on_wasm`/`typeck::check_str` leg can't resolve (it doesn't
    /// pull in std modules); the linked interpreter oracle (`link_run`) can. So we
    /// compare the pruned binary against the interpreter directly. The pruned
    /// module must import "encoding" alongside "print".
    #[test]
    fn wir_encoding_host_import_binary_path() {
        let src = "import encoding\nfn main(console: Console):\n    console.print(encoding.hex_encode(\"Hi\"))\n    console.print(encoding.base64_encode(\"Hi\"))\n";
        let want = vec!["4869".to_string(), "SGk=".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle encoding via the host import");
        // AST → WIR → binary runs identically to the interpreter oracle, under a
        // print-only grant (proving the pruned module imports only print+encoding,
        // and that `encoding` is host-provided regardless of the grant).
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// Enum (Adt) structural `==` on the binary path — the generated `$eq_*`
    /// tag-dispatch helper. Kept OUT of the 3-way corpus deliberately: the legacy
    /// WAT path pointer-compares enums (a pre-existing compiled-vs-interpreter
    /// divergence — it returns `false` even for `None == None`), so it can't be the
    /// oracle here. The binary path is structurally CORRECT, so we assert it against
    /// the INTERPRETER directly. Covers None==None, tag mismatch, nullary-variant
    /// equality, and equal/unequal nested-String payloads.
    #[test]
    fn wir_enum_eq_binary_path() {
        let src = "type CalcError derive(PartialEq):\n    StackUnderflow\n    UnknownToken(String)\n    DivByZero\n\nfn main(console: Console):\n    let a: Option(CalcError) = None\n    let b: Option(CalcError) = Some(StackUnderflow)\n    let c: Option(CalcError) = Some(UnknownToken(\"x\"))\n    let d: Option(CalcError) = Some(UnknownToken(\"y\"))\n    let cx: Option(CalcError) = Some(UnknownToken(\"x\"))\n    console.print(\"${a == None}\")\n    console.print(\"${b == None}\")\n    console.print(\"${b == Some(StackUnderflow)}\")\n    console.print(\"${c == cx}\")\n    console.print(\"${c == d}\")\n    console.print(\"${b == c}\")\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "true".to_string(),
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
        ];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should structurally lower enum `==`");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// Structural `==` on a self-RECURSIVE ADT (`Node(Tree, Tree)`) on the binary
    /// path. The `$eq_*` helper reserves its name before building, so a nested
    /// `Tree` field compares via a recursive `call` to the same helper. (2-way:
    /// the WAT path pointer-compares enums — a known WAT/interpreter divergence —
    /// so compare binary vs the interpreter oracle, which compares structurally.)
    #[test]
    fn wir_recursive_adt_eq_binary_path() {
        let src = "type Tree:\n    Leaf(Int)\n    Node(Tree, Tree)\n\nfn main(console: Console):\n    let a = Node(Node(Leaf(1), Leaf(2)), Leaf(3))\n    let b = Node(Node(Leaf(1), Leaf(2)), Leaf(3))\n    let c = Node(Node(Leaf(1), Leaf(9)), Leaf(3))\n    console.print(\"${a == b}\")\n    console.print(\"${a == c}\")\n";
        let want = vec!["true".to_string(), "false".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should compare a recursive ADT");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// The native regex engine on the binary path: `regex.match_spans` is a host
    /// import (the Rust `regex` crate, the same native the interpreter uses)
    /// wrapped by `$regex_match_spans` (length-prefixed `fill_pending` read, like
    /// `dir_read`). Ungated (matching needs no capability), so the print-only
    /// harness instantiates it. Compared against the linked interpreter oracle.
    #[test]
    fn wir_regex_match_spans_binary_path() {
        let src = "import regex\nfn main(console: Console):\n    console.print(\"${regex.matches(\"[0-9]+\", \"order 1234\")}\")\n    console.print(\"${regex.find_all(\"[0-9]+\", \"a1 b22 c333\")}\")\n    console.print(regex.replace_all(\"[0-9]+\", \"a1b22\", \"N\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower regex via the host engine");
        let want = link_run(src);
        assert_eq!(want[0], "true", "regex.matches sanity");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path vs oracle");
    }

    /// `$dir_read` (read a file) on the binary path — the two-phase
    /// `dir_read_len` + `fill_pending` host protocol, gated behind Dir(Read).
    /// Sets up a sandbox dir with a file and reads it back.
    #[test]
    fn wir_dir_read_host_import_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wir_dirread_{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("greeting.txt"), "hello from disk").expect("write file");
        let src = "fn main(console: Console, dir: Dir[Read]):\n    console.print(dir.read(\"greeting.txt\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle dir read via the host imports");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities { print: true, dir_root: Some(root.clone()), dir_read: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn with Dir(Read)");
        actor.run().expect("run");
        let got = actor.output();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(got, vec!["hello from disk".to_string()], "binary path");
    }

    /// `$get_env` on the binary path — a host-import helper returning an
    /// `Option(String)`, consumed via `match` (now lowering via the
    /// constructor-pattern arm). The absent branch is deterministic ("unset");
    /// the present branch (PATH) takes the `Some` arm. Env grant.
    #[test]
    fn wir_get_env_option_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "fn main(console: Console, env: Env):\n    match env.get_env(\"WITCHY_UNSET_XYZZY_VAR\"):\n        Some(v) -> console.print(v)\n        None -> console.print(\"unset\")\n    match env.get_env(\"PATH\"):\n        Some(v) -> console.print(\"has\")\n        None -> console.print(\"no-path\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle get_env + match");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(&bytes, Capabilities { print: true, env: true, quiet: true, ..Default::default() }, crate::RUN_MEMORY_PAGES)
            .expect("spawn with Env");
        actor.run().expect("run");
        let got = actor.output();
        assert_eq!(got[0], "unset", "absent var → None → unset");
        assert_eq!(got.len(), 2, "both matches print one line each");
        assert!(matches!(got[1].as_str(), "has" | "no-path"), "present-var branch: {got:?}");
    }

    /// An Int-returning `main` on the binary path: the `run` wrapper prints the
    /// result via `print_int` (the exit-code convention), matching the WAT sink.
    /// Validated against the WAT path (both compiled paths use i32 `Int`, so they
    /// agree exactly — unlike the i64 interpreter). Needs the `print_int` grant.
    #[test]
    fn wir_int_returning_main_prints_result() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "fn main(console: Console) -> Int:\n    console.print(\"hi\")\n    42\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle an Int-returning main");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(&bytes, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, crate::RUN_MEMORY_PAGES)
            .expect("spawn with print_int");
        actor.run().expect("run");
        let got = actor.output();
        assert_eq!(got, vec!["hi".to_string(), "42".to_string()], "binary path");
        assert_eq!(got, run_on_wasm(src), "binary path matches WAT path");
    }

    /// Link a multi-module program, compile the flat module to WASM, run it on
    /// the runtime with output capabilities, and return what it printed.
    fn run_linked_on_wasm(sources: &[(&str, &str)], entry: &str) -> Vec<String> {
        witchy_interp::compiler_natives::install();
        use crate::runtime::{Capabilities, Runtime};
        let mods: Vec<(String, ast::Module)> = sources
            .iter()
            .map(|(n, s)| ((*n).to_string(), parser::parse_module(s).expect("parse")))
            .collect();
        let linked = crate::pipeline::link(mods, entry).expect("link");
        assert!(typeck::check(&linked).is_ok(), "{:?}", typeck::check(&linked));
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                // Mirror the interpreter's automatic grants (output + the
                // read-only ambient Clock/Env), like `run_wasm_bytes`.
                Capabilities {
                    print: true,
                    print_int: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }


    /// Link + run a single-`main` source on the interpreter with a `Net` allowlist grant.
    fn link_run_net(src: &str, net_allow: &[&str]) -> Vec<String> {
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        interpreter::run_module(linked, ".", net_allow.iter().map(|s| s.to_string()).collect())
            .expect("run")
    }


    /// Like `run_linked_on_wasm` but with an explicit `Net` allowlist grant, for
    /// programs that `restrict`/`connect` to specific addresses.
    fn run_linked_on_wasm_net(sources: &[(&str, &str)], entry: &str, net_allow: &[&str]) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        let mods: Vec<(String, ast::Module)> = sources
            .iter()
            .map(|(n, s)| ((*n).to_string(), parser::parse_module(s).expect("parse")))
            .collect();
        let linked = crate::pipeline::link(mods, entry).expect("link");
        assert!(typeck::check(&linked).is_ok(), "{:?}", typeck::check(&linked));
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(net_allow.iter().map(|s| (*s).to_string()).collect()),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }

    #[test]
    fn std_math_compiles_and_runs_on_wasm() {
        // Importing `math` forces every function in it to compile (Int helpers
        // *and* the Float ones: float_min/float_max/float_abs/float_clamp, which use f64 compares and
        // unary negation). gcd(48,36)=12, pow(2,10)=1024, clamp(15,0,10)=10,
        // float_clamp(15,0,10)=10.0, float_abs(-3.5)=3.5 -> 12+1024+10+10+3 = 1059.
        let client = r#"
import math

fn main() -> Int:
    let a = math.gcd(48, 36)
    let b = math.pow(2, 10)
    let c = math.clamp(15, 0, 10)
    let f = math.float_clamp(15.0, 0.0, 10.0)
    let g = math.float_abs((0.0 - 3.5))
    ((((a + b) + c) + math.to_int(f)) + math.to_int(g))
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("math", crate::bundled_module("math").unwrap()), ("main", client)],
                "main",
            ),
            vec!["1059"]
        );
    }

    // factorial (1 for n<=1) and is_prime (trial division; n<2 not prime).
    // A Float-returning main now runs compiled: the auto-print wrapper calls
    // the newly-wired print_float host, which formats f64 exactly like the
    // interpreter's Value::Float Display. Previously the compiled module failed
    // to instantiate (no print_float import provider).
    // A broader compiled-float workout: division, float_abs (negation + compare),
    // float_max, a float comparison driving a float-valued `if`, multiply, subtract,
    // and sqrt — all feeding one Float result. Both backends agree.
    #[test]
    fn float_arithmetic_compiled_backends_agree() {
        let client = r#"
import math

fn main() -> Float:
    let a = (10.0 / 4.0)
    let b = math.float_abs((0.0 - 1.5))
    let c = math.float_max(a, b)
    let d = if (c > 2.0): (c * 2.0) else: 0.0
    (d - math.sqrt(4.0))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "compiled float arithmetic diverged");
        assert_eq!(compiled, vec!["3.0"]);
    }

    #[test]
    fn float_returning_main_backends_agree() {
        let client = r#"
import math

fn main() -> Float:
    (math.float_min(2.5, math.sqrt(2.25)) + math.float_clamp(5.0, 0.0, 1.0))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "float main diverged");
        assert_eq!(compiled, vec!["2.5"]);
    }

    #[test]
    fn std_math_factorial_is_prime_backends_agree() {
        let client = r#"
import math

fn main(console: Console):
    console.print("${math.factorial(5)}")
    console.print("${math.factorial(0)}")
    console.print("${math.factorial(1)}")
    console.print("${math.is_prime(7)}")
    console.print("${math.is_prime(12)}")
    console.print("${math.is_prime(1)}")
    console.print("${math.is_prime(2)}")
    console.print("${math.is_prime(97)}")
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "factorial/is_prime diverged");
        assert_eq!(compiled, vec!["120", "1", "1", "true", "false", "false", "true", "true"]);
    }

    #[test]
    fn std_math_lcm_parity_backends_agree() {
        // lcm (built on gcd) and the is_even/is_odd predicates agree across
        // backends, including negative operands.
        let client = r#"
import math

fn main(console: Console):
    console.print("${math.lcm(4, 6)}")
    console.print("${math.lcm(21, 6)}")
    console.print("${math.lcm(0, 5)}")
    console.print("${math.lcm((0 - 4), 6)}")
    console.print("${math.is_even(10)}")
    console.print("${math.is_odd(7)}")
    console.print("${math.is_odd((0 - 3))}")
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "math lcm/parity diverged");
        assert_eq!(compiled, vec!["12", "42", "0", "12", "true", "true", "true"]);
    }

    #[test]
    fn split_runs_on_wasm() {
        // `split` compiled to WASM, matching Rust's str::split: pieces between
        // separators, empty pieces kept, multi-char separators, and an empty
        // separator yielding the whole string.
        let src = r#"
fn main(console: Console):
    let p = "a,bb,ccc".split(",")
    console.print("${list.length(p)}")
    console.print(list.at(p, 0))
    console.print(list.at(p, 2))
    console.print("${list.length("a,,b".split(","))}")
    console.print(list.at("a,,b".split(","), 1))
    console.print("${list.length("".split(","))}")
    console.print("${list.length("abc".split(""))}")
    console.print(list.at("xXXyXXz".split("XX"), 2))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["3", "a", "ccc", "3", "", "1", "1", "z"]
        );
    }

    #[test]
    fn to_string_polymorphic_on_wasm() {
        // `to_string` renders by the argument's compile-time value type: Int
        // literals/arithmetic, Bool literals/comparisons/user-fn results, and
        // String pass-through — all in compiled code.
        let src = r#"
fn classify(n: Int) -> Bool:
    (n > 0)

fn main(console: Console):
    console.print("${42}")
    console.print("${(0 - 5)}")
    console.print("${true}")
    console.print("${(3 > 7)}")
    console.print("${"hi"}")
    console.print("${classify(9)}")
    let flag = (2 == 2)
    console.print("${flag}")
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["42", "-5", "true", "false", "hi", "true", "true"]
        );
    }

    #[test]
    fn to_string_on_compound_renders_on_wasm() {
        // A compound (list/tuple/record/ADT/dict, any nesting) renders byte-
        // identically to the interpreter via a generated per-shape helper — so
        // `to_string`/`${...}` work on WASM, not just the interpreter.
        let src = r#"
type Shape:
    Circle(Int)
    Dot

fn main(console: Console):
    console.print("${[1, 2, 3]}")
    console.print("${[[1, 2], [3]]}")
    console.print("${(1, "two", true)}")
    console.print("${[Circle(2), Dot]}")
    var d = dict.new()
    dict.insert(d, "a", 1)
    dict.insert(d, "b", 2)
    console.print("${d}")
    let tc = ([1, 2], (3, 4))          // a let-bound tuple whose slots are compound
    console.print("${tc}")
"#;
        assert_eq!(
            run_on_wasm(src),
            vec![
                "[1, 2, 3]",
                "[[1, 2], [3]]",
                "(1, two, true)",
                "[Circle(2), Dot]",
                "{a: 1, b: 2}",
                "([1, 2], (3, 4))",
            ]
        );
    }

    /// `==` on a compound whose *slots are themselves compound* must agree on
    /// both backends, whether the operands are `let`-bound or parameters. WASM
    /// previously returned `None` for the shape of such a binding and fell back to
    /// a pointer compare — a SILENT divergence (interpreter `true`, compiled
    /// `false`). The shape is now captured from the binding/declared type.
    #[test]
    fn nested_compound_equality_agrees_on_both_backends() {
        let src = "fn same(a: (List(Int), List(Int)), b: (List(Int), List(Int))) -> Bool:\n    a == b\nfn main(console: Console):\n    let v = ([1, 2], (3, 4))\n    let w = ([1, 2], (3, 4))\n    console.print(\"${v == w}\")\n    console.print(\"${same(([1], [2]), ([1], [2]))}\")\n    console.print(\"${same(([1], [2]), ([1], [9]))}\")\n";
        let want = vec!["true".to_string(), "true".to_string(), "false".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    #[test]
    fn to_string_through_generics_renders() {
        // Typed lowering (Phase 0) resolves what used to be undetermined: a
        // generic tuple rendered through a monomorphizable call works
        // identically on both backends. (The loud could-not-determine error
        // remains for shapes with NO resolvable call site.)
        let src = r#"
fn render(t: (a, a)) -> String:
    "${t}"

fn main(console: Console):
    console.print(render((1, 2)))
"#;
        assert_eq!(link_run(src), vec!["(1, 2)"], "interpreter");
        assert_eq!(wasm_run(src), vec!["(1, 2)"], "wasm");
    }

    #[test]
    fn negative_int_to_string_on_wasm() {
        // `int_to_string` renders negatives with a leading '-' (previously it
        // emitted garbage, e.g. "/" for -1).
        let src = r#"
fn main(console: Console):
    console.print("${(0 - 1)}")
    console.print("${(0 - 128)}")
    console.print("${255}")
    console.print("${0}")
"#;
        assert_eq!(run_on_wasm(src), vec!["-1", "-128", "255", "0"]);
    }

    #[test]
    fn replace_on_wasm() {
        // `replace` compiled to WASM, matching Rust's str::replace: simple and
        // multi-char patterns, greedy non-overlapping, deletion (empty `to`),
        // growth (`to` longer than `from`), no match, an empty `from` (inserted
        // at every char boundary), and UTF-8 (`é` is a 2-byte match).
        let src = r#"
fn main(console: Console):
    console.print("a,b,c".replace(",", ";"))
    console.print("aXXbXXc".replace("XX", "-"))
    console.print("aaa".replace("aa", "x"))
    console.print("a,b,c".replace(",", ""))
    console.print("abc".replace("b", "XYZ"))
    console.print("abc".replace("z", "Q"))
    console.print("ab".replace("", "-"))
    console.print("café".replace("é", "e"))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["a;b;c", "a-b-c", "xa", "abc", "aXYZc", "abc", "-a-b-", "cafe"]
        );
    }

    #[test]
    fn string_search_slice_on_wasm() {
        // contains / index_of / substring compiled to WASM, matching the
        // interpreter — including Unicode: "café!" has the `!` at character index
        // 4 (byte 5), and string.substring(3,5) is the two characters "é!".
        let src = r#"
fn main(console: Console):
    console.print("${if "hello world".contains("world"): 1 else: 0}")
    console.print("${if "abc".contains("xyz"): 1 else: 0}")
    console.print("${if "abc".contains(""): 1 else: 0}")
    console.print("${"hello".contains("l")}")
    console.print("${"hello".contains("z")}")
    console.print("hello".substring(1, 4))
    console.print("hi".substring(0, 100))
    console.print("hi".substring(5, 10))
    console.print("${"café!".contains("!")}")
    console.print("café!".substring(3, 5))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["1", "0", "1", "true", "false", "ell", "hi", "", "true", "é!"]
        );
    }

    #[test]
    fn parse_kv_example_runs_on_wasm() {
        // The `key=value` parser example compiles end-to-end: index_of +
        // substring + string_length + ends_with + Bool interpolation, matching the
        // interpreter. `.index_of` resolves to the std `string.index_of`
        // (Option-returning, RFC-0044), so it needs the std-linking `wasm_run`.
        assert_eq!(
            wasm_run(include_str!("../examples/parse_kv/src/parse_kv.witchy")),
            vec!["timeout", "30", "true"]
        );
    }

    #[test]
    fn dict_string_keys_on_wasm() {
        // String-keyed Dict compiled to WASM: insert (append + replace), get_or
        // (present/absent), has, and size — keys compared with $str_eq.
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, "a", 1)
    dict.insert(d, "b", 2)
    dict.insert(d, "a", 10)
    console.print("${dict.get_or(d, "a", 0)}")
    console.print("${dict.get_or(d, "b", 0)}")
    console.print("${dict.get_or(d, "z", (0 - 1))}")
    console.print("${dict.length(d)}")
    console.print("${if dict.contains_key(d, "b"): 1 else: 0}")
    console.print("${if dict.contains_key(d, "q"): 1 else: 0}")
"#;
        assert_eq!(run_on_wasm(src), vec!["10", "2", "-1", "2", "1", "0"]);
    }

    #[test]
    fn place_assignment_sugar_backends_agree() {
        // RFC-0022: `xs[i] = v`, `d[k] = v`, and `u.field = v` (plus their compound
        // forms) desugar to value-reassignment — list/dict `set_at` and a record
        // update — and run identically on both backends.
        let src = r#"
type P:
    x: Int
    y: Int

fn main(console: Console):
    var xs = [10, 20, 30]
    xs[0] = 9
    xs[2] += 5
    console.print("${xs}")
    var d = dict.new()
    d["a"] = 1
    d["b"] = 2
    console.print("${dict.get_or(d, "a", 0)} ${dict.get_or(d, "b", 0)}")
    var p = P(1, 2)
    p.x = 10
    p.y += 5
    console.print("${p.x} ${p.y}")
"#;
        assert_eq!(
            link_run(src),
            run_linked_on_wasm(&[("main", src)], "main"),
            "place-assignment must agree across backends"
        );
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            vec!["[9, 20, 35]", "1 2", "10 7"]
        );
    }

    #[test]
    fn std_string_compiles_and_runs_on_wasm() {
        // With `split` compiled, the whole `string` module compiles: `lines`
        // (split on "\n"), `join`, and `repeat`. lines -> ["a","bb","ccc"] (3);
        // join -> "a-bb-ccc" (8); repeat -> "zzzzz" (5): 3*100 + 8 + 5 = 313.
        let client = r#"

fn main() -> Int:
    let parts = "a\nbb\nccc".lines()
    let joined = list.join(parts, "-")
    let r = "z".repeat(5)
    (((list.length(parts) * 100) + joined.length()) + r.length())
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("string", crate::bundled_module("string").unwrap()), ("main", client)],
                "main",
            ),
            vec!["313"]
        );
    }

    #[test]
    fn std_string_pad_backends_agree() {
        // pad_left/pad_right reach an exact target width, trimming the padding
        // even when `fill` is multi-character; an already-wide string is left
        // untouched. Multi-char fill "-=" padding "ab" to 7 -> "-=-=-ab".
        let client = r#"

fn main(console: Console):
    console.print("42".pad_left(5, "0"))
    console.print("42".pad_right(5, "."))
    console.print("hello".pad_left(3, "x"))
    console.print("ab".pad_left(7, "-="))
    console.print("café".pad_left(6, "*"))
    console.print("café".pad_right(6, "*"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pad diverged between backends");
        // Widths are by character: "café" is 4 chars, so pad to 6 adds two stars
        // (a byte-based width would have added only one).
        assert_eq!(
            compiled,
            vec!["00042", "42...", "hello", "-=-=-ab", "**café", "café**"]
        );
    }

    #[test]
    fn std_string_strip_backends_agree() {
        // strip_prefix/strip_suffix remove an affix only when it matches,
        // leaving the string untouched otherwise; stripping the whole string
        // yields "". Complements starts_with/ends_with.
        let client = r#"

fn main(console: Console):
    console.print("witchy.lang".strip_prefix("witchy."))
    console.print("witchy.lang".strip_prefix("scala."))
    console.print("main.witchy".strip_suffix(".witchy"))
    console.print("main.rs".strip_suffix(".witchy"))
    console.print("abc".strip_prefix("abc"))
    console.print("émile".strip_prefix("é"))
    console.print("héllo!".strip_suffix("!"))
    console.print("naïveté".strip_suffix("té"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "strip diverged between backends");
        // The multibyte rows pin the char-count fix: the old bodies mixed
        // string.length (bytes) into substring's character offsets, so any
        // multibyte affix ate extra chars (prefix) or disabled the strip
        // entirely (suffix).
        assert_eq!(
            compiled,
            vec!["lang", "witchy.lang", "main", "main.rs", "", "mile", "héllo", "naïve"]
        );
    }

    #[test]
    fn large_string_concat_grows_memory() {
        // Concatenating a 400-char string one char at a time allocates ~80KB of
        // intermediate strings — past the initial page — and must grow.
        let src = r#"
fn main() -> Int:
    var s = ""
    var i = 0
    while (i < 400):
        s = (s + "x")
        i = (i + 1)
    s.length()
"#;
        assert_eq!(run_on_wasm(src), vec!["400"]);
    }

    #[test]
    fn compute_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../examples/compute/src/compute.witchy")),
            vec!["217"]
        );
    }

    #[test]
    fn string_prefix_suffix_on_wasm() {
        // starts_with / ends_with compile to byte-loop helpers.
        // check("html")=2, check("http")=1, check("xml")=0 -> 210.
        let src = r#"
fn check(s: String) -> Int:
    if s.starts_with("ht"):
        if s.ends_with("ml"):
            2
        else:
            1
    else:
        0

fn main() -> Int:
    (((check("html") * 100) + (check("http") * 10)) + check("xml"))
"#;
        assert_eq!(run_on_wasm(src), vec!["210"]);
    }

    #[test]
    fn try_operator_runs_on_wasm() {
        // `?` compiles: success unwraps, error early-returns. compute(3,4)=Ok(7),
        // compute(0,9)=Err(99); 7*100 + 99 = 799.
        let src = r#"
fn checked(n: Int) -> Result(Int, Int):
    match n:
        0 -> Err(99)
        _ -> Ok(n)

fn compute(a: Int, b: Int) -> Result(Int, Int):
    let x = (checked(a))?
    let y = (checked(b))?
    Ok((x + y))

fn main() -> Int:
    let ok = match compute(3, 4):
        Ok(v) -> v
        Err(e) -> e
    let bad = match compute(0, 9):
        Ok(v) -> v
        Err(e) -> e
    ((ok * 100) + bad)
"#;
        assert_eq!(run_on_wasm(src), vec!["799"]);
    }

    #[test]
    fn early_return_runs_on_wasm() {
        // Guard-clause early returns compile to valid WASM and run.
        // classify(-5) = -1, classify(0) = 0, classify(9) = 1; sum = 0.
        let src = r#"
fn classify(n: Int) -> Int:
    if (n < 0):
        return (0 - 1)
    if (n == 0):
        return 0
    1

fn main() -> Int:
    ((classify((0 - 5)) + classify(0)) + classify(9))
"#;
        assert_eq!(run_on_wasm(src), vec!["0"]);
    }

    #[test]
    fn shapes_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../examples/shapes/src/shapes.witchy")),
            vec!["325"]
        );
    }

    /// Real examples (not toy snippets) compile and run on the WASM backend,
    /// matching the interpreter — a concrete check of codegen breadth.
    #[test]
    fn eval_example_runs_on_wasm() {
        assert_eq!(run_on_wasm(include_str!("../examples/eval/src/eval.witchy")), vec!["20"]);
    }

    #[test]
    fn bank_example_runs_on_wasm() {
        // Records + lists + for-in + Result + `?` together, compiled to WASM.
        assert_eq!(
            run_on_wasm(include_str!("../examples/bank/src/bank.witchy")),
            vec!["total = 150", "remaining: 90", "error: insufficient funds for bob"]
        );
    }

    // A Net capability is an allow-list, and attenuation only ever narrows it.
    // These rejections fire on the allow-list check, before any socket is
    // opened, so the test needs no network. (`run_with` grants the root Net.)
    /// A library imported into a program brings its functions into scope but no
    /// authority: `lib` has no capability parameters, so it can only compute.
    #[test]
    fn imported_library_is_pure_and_confined() {
        let lib = r#"
pub fn label(n: Int) -> String:
    if (n < 0):
        "neg"
    else:
        "nonneg"
"#;
        let main = r#"
import lib

fn main(console: Console):
    console.print(lib.label((-2)))
    console.print(lib.label(7))
"#;
        let out = interpreter::run_program(&[("lib", lib), ("main", main)], "main")
            .expect("multi-module program runs");
        assert_eq!(out, vec!["neg", "nonneg"]);
    }

    /// Every example must at least compile (parse + link + type-check) and run
    /// to completion through the CLI without an error — whether it prints, just
    /// returns a value, or is a library/actor file with no `main`. Server demos
    /// (`serve_*`) are excluded: they need a `--net` grant and run forever, so
    /// they're covered by the loopback tests instead, not run-to-completion here.
    #[test]
    fn all_examples_run_via_cli() {
        let mut files: Vec<std::path::PathBuf> = example_entries()
            .into_iter()
            .filter(|p| {
                !p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("serve_"))
            })
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no examples found");
        let failures: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = files.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    match crate::execute_file(p, Vec::new()) {
                        Ok(_) => None,
                        Err(e) => Some(format!("{p}: {e:?}")),
                    }
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            failures.is_empty(),
            "examples failed:\n{}",
            failures.join("\n")
        );
    }

    /// EVERY example — including the server demos that run forever (and so are
    /// excluded from the run-to-completion test above) — must parse, link, and
    /// type-check. Catches type errors the run test can't reach.
    #[test]
    fn all_examples_type_check() {
        let entries = example_entries();
        assert!(!entries.is_empty(), "no examples found");
        let failures: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    match crate::check_file(p) {
                        Ok(_) => None,
                        Err(e) => Some(format!("{p}: {e:?}")),
                    }
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            failures.is_empty(),
            "type-check failed:\n{}",
            failures.join("\n")
        );
    }

    /// Every example rune's in-language tests (`src/*_test.witchy`) pass. This
    /// keeps the per-example `witchy test` suites green in CI — so an example
    /// whose behavior drifts from its documented tests fails the build, not just
    /// a manual run. (Multi-rune `projects/` are skipped here: their cross-rune
    /// path dependencies are exercised by the package-manager tests instead.)
    #[test]
    fn all_example_rune_tests_pass() {
        let mut test_files: Vec<std::path::PathBuf> = Vec::new();
        for dir in std::fs::read_dir("examples").expect("examples directory") {
            let dir = dir.unwrap().path();
            if !dir.is_dir() {
                continue;
            }
            if dir.file_name().and_then(|s| s.to_str()) == Some("projects") {
                continue;
            }
            let Ok(rd) = std::fs::read_dir(dir.join("src")) else {
                continue;
            };
            let mut files: Vec<std::path::PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with("_test.witchy"))
                })
                .collect();
            files.sort();
            test_files.extend(files);
        }
        assert!(!test_files.is_empty(), "no example rune test files found");
        let ran: usize = std::thread::scope(|s| {
            let handles: Vec<_> = test_files.iter().map(|tf| {
                s.spawn(move || {
                    let p = tf.to_str().unwrap();
                    let (passed, failed) =
                        crate::run_tests_in_file(p).unwrap_or_else(|e| panic!("{p}: {e}"));
                    assert!(failed.is_empty(), "{p}: test failures: {failed:?}");
                    assert!(!passed.is_empty(), "{p}: a `*_test.witchy` with no `test_*` functions");
                    passed.len()
                })
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>().into_iter().sum()
        });
        assert!(ran > 0, "no example rune tests ran");
    }

    /// Every bundled std module must type-check on its own (linked with its
    /// imports). The interpreter doesn't type-check, so without this a latent
    /// type error in a module no example imports would go unnoticed.
    #[test]
    fn all_std_modules_type_check() {
        let mut any = false;
        for entry in std::fs::read_dir("std").expect("std directory") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("witchy") {
                continue;
            }
            any = true;
            let p = path.to_str().unwrap();
            assert!(
                crate::check_file(p).is_ok(),
                "type-check failed for `{p}`: {:?}",
                crate::check_file(p)
            );
        }
        assert!(any, "no std modules found");
    }

    #[test]
    fn compute_example_returns_value() {
        assert_eq!(
            crate::execute_file("examples/compute/src/compute.witchy", Vec::new()).unwrap(),
            vec!["217"]
        );
    }

    #[test]
    fn shapes_example_returns_value() {
        assert_eq!(
            crate::execute_file("examples/shapes/src/shapes.witchy", Vec::new()).unwrap(),
            vec!["325"]
        );
    }

    /// `largest` reproduces the generic function from The Rust Programming
    /// Language ch. 10: a `where a: Ord` bound finds the biggest element of a
    /// list, for `Int` and for a user `Version` type with an `Ord` impl (the
    /// trait's derived `greater` dispatches correctly through monomorphization).
    #[test]
    fn largest_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/largest/src/largest.witchy").unwrap();
        let sources = [("cmp", crate::bundled_module("cmp").unwrap()), ("main", client.as_str())];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "largest diverged");
        assert_eq!(
            compiled,
            vec!["largest number: 100".to_string(), "latest version: 2.0".to_string()]
        );
    }

    /// `minigrep` is the CLI search tool from The Rust Programming Language ch. 12,
    /// reproduced in witchy: it takes a query and a file path as args, reads the
    /// file with a `Dir[Read]` capability, and prints the matching lines. Missing
    /// args print usage and exit 1 (the conventional process exit code).
    #[test]
    fn minigrep_example_searches_a_file_like_the_rust_book() {
        let (out, code) = crate::execute_file_exit(
            "examples/minigrep/src/minigrep.witchy",
            Vec::new(),
            vec!["nobody".into(), "examples/data/poem.txt".into()],
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            out,
            vec!["I'm nobody! Who are you?".to_string(), "Are you nobody, too?".to_string()]
        );
        // No args: usage message and a non-zero exit code.
        let (out, code) =
            crate::execute_file_exit("examples/minigrep/src/minigrep.witchy", Vec::new(), Vec::new(), None, Vec::new())
                .unwrap();
        assert_eq!(code, 1);
        assert_eq!(out, vec!["usage: minigrep <query> <file>".to_string()]);
    }


    #[test]
    fn hello_example() {
        assert_eq!(
            interp(include_str!("../examples/hello/src/hello.witchy")),
            vec!["hello, witchy", "8 doubled is 16", "negative"]
        );
    }

    #[test]
    fn mutate_example() {
        assert_eq!(
            interp(include_str!("../examples/mutate/src/mutate.witchy")),
            vec!["bumped to 3"]
        );
    }

    #[test]
    fn ownership_example() {
        assert_eq!(
            interp(include_str!("../examples/ownership/src/ownership.witchy")),
            vec!["[witchy]"]
        );
    }

    #[test]
    fn string_interpolation_backends_agree() {
        // `${expr}` desugars through generated render + concat, so interpolation works
        // in both backends: String pass-through, Int/Bool via to_string, embedded
        // calls/arithmetic, `\$` for a literal `$`, and adjacent interpolations.
        let src = r#"
fn main(console: Console):
    let name = "witchy"
    let age = 3
    console.print("hi ${name}, age ${age}")
    console.print("sum: ${"${age + 10}"}")
    console.print("flag ${age > 1}")
    console.print("literal \${x} stays")
    console.print("${name}${name}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec![
                "hi witchy, age 3",
                "sum: 13",
                "flag true",
                "literal ${x} stays",
                "witchywitchy",
            ]
        );
    }

    #[test]
    fn guard_example_runs_on_wasm() {
        // Early `return` from a function and from inside a `for` loop.
        let src = include_str!("../examples/guard/src/guard.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["negative", "zero", "positive", "8", "-1"]);
    }

    #[test]
    fn compound_assignment_backends_agree() {
        // `x op= e` desugars to `x = x op e`; verify all five ops in both
        // backends, in a loop and a sequence.
        let src = r#"
fn main(console: Console):
    var sum = 0
    var i = 0
    while (i < 5):
        sum = (sum + i)
        i = (i + 1)
    console.print("${sum}")
    var x = 100
    x = (x - 30)
    x = (x * 2)
    x = (x / 7)
    x = (x % 5)
    console.print("${x}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["10", "0"]);
    }

    // `replace` with an empty `from` is a notorious edge (the interpreter's
    // Rust `str::replace` inserts the replacement around every character);
    // Int-keyed dicts exercise the by-value key-comparison path. Both must
    // match the compiled backend exactly. Agreement-only, so a future divergence
    // is caught without baking in a hand-computed expectation.
    #[test]
    fn replace_and_int_keyed_dict_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print((("[" + "abc".replace("", "-")) + "]"))
    console.print("abc".replace("x", "y"))
    console.print("aaa".replace("a", "bb"))
    console.print("hello world".replace("o", "0"))
    var d = dict.new()
    dict.insert(d, 1, 100)
    dict.insert(d, 2, 200)
    dict.insert(d, 1, 111)
    console.print("${dict.get_or(d, 1, 0)}")
    console.print("${dict.get_or(d, 2, 0)}")
    console.print("${dict.length(d)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "replace/int-key dict diverged");
    }

    // A guard on a constructor pattern must bind the field first, then test it
    // (`Yep(n) if n > 10`), and fall through to the next arm when the guard
    // fails. Mutual recursion exercises forward references between compiled
    // functions. Both must agree across backends.
    #[test]
    fn adt_guards_and_mutual_recursion_backends_agree() {
        let src = r#"
type Opt:
    Nope
    Yep(Int)

fn describe(o: Opt) -> String:
    match o:
        Yep(n) if (n > 10) -> "big"
        Yep(n) -> "small"
        Nope -> "none"

fn is_even(n: Int) -> Bool:
    if (n == 0):
        true
    else:
        is_odd((n - 1))

fn is_odd(n: Int) -> Bool:
    if (n == 0):
        false
    else:
        is_even((n - 1))

fn main(console: Console):
    console.print(describe(Yep(50)))
    console.print(describe(Yep(3)))
    console.print(describe(Nope))
    console.print("${if is_even(10): 1 else: 0}")
    console.print("${if is_odd(7): 1 else: 0}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "adt guards / mutual recursion diverged");
        assert_eq!(run_on_wasm(src), vec!["big", "small", "none", "1", "1"]);
    }

    // A recursive ADT (binary tree) with nested constructor patterns, exercised
    // by two recursive traversals (sum and depth). Recursion through a
    // heap-allocated ADT and destructuring `Node(l, v, r)` must agree across
    // backends.
    #[test]
    fn recursive_tree_adt_backends_agree() {
        let src = r#"
type Tree:
    Leaf
    Node(Tree, Int, Tree)

fn sum_tree(t: Tree) -> Int:
    match t:
        Leaf -> 0
        Node(l, v, r) -> ((sum_tree(l) + v) + sum_tree(r))

fn depth(t: Tree) -> Int:
    match t:
        Leaf -> 0
        Node(l, v, r) ->
            let dl = depth(l)
            let dr = depth(r)
            (1 + if (dl > dr): dl else: dr)

fn main(console: Console):
    let t = Node(Node(Leaf, 1, Node(Leaf, 5, Leaf)), 2, Node(Leaf, 3, Leaf))
    console.print("${sum_tree(t)}")
    console.print("${depth(t)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "recursive tree ADT diverged");
        assert_eq!(run_on_wasm(src), vec!["11", "3"]);
    }

    // Generic functions instantiated at several distinct types within one
    // program: pair_up / first_of / second_of are used at (Int,Int),
    // (String,String), and (Int,String). Per-call generalization must give each
    // call site its own instantiation, and both backends must agree.
    #[test]
    fn generics_at_multiple_types_backends_agree() {
        let src = r#"
fn pair_up(x: a, y: b) -> (a, b):
    (x, y)

fn first_of(p: (a, b)) -> a:
    let (f, s) = p
    f

fn second_of(p: (a, b)) -> b:
    let (f, s) = p
    s

fn main(console: Console):
    let pi = pair_up(1, 2)
    let ps = pair_up("a", "b")
    let pm = pair_up(7, "mixed")
    console.print("${first_of(pi)}")
    console.print(first_of(ps))
    console.print(second_of(ps))
    console.print("${first_of(pm)}")
    console.print(second_of(pm))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "multi-type generics diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "a", "b", "7", "mixed"]);
    }

    // char_count returns Unicode scalars; string_length returns bytes. They
    // agree for ASCII and diverge for multi-byte UTF-8 ("café" is 4 chars, 5
    // bytes) — and both backends must compute each identically.
    #[test]
    fn char_count_vs_string_length_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print("${"hello".char_count()}")
    console.print("${"hello".length()}")
    console.print("${"café".char_count()}")
    console.print("${"café".length()}")
    console.print("${"".char_count()}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "char_count diverged");
        assert_eq!(run_on_wasm(src), vec!["5", "5", "4", "5", "0"]);
    }

    #[test]
    fn substring_is_char_indexed_across_multibyte_on_both_backends() {
        // substring indexes by CHARACTER, not byte: slicing across a 2-byte (é)
        // or 4-byte (emoji) boundary must compute the same char->byte offsets on
        // both backends, while length (bytes) vs char_count tracks UTF-8 widths.
        let src = r#"
fn main(console: Console):
    console.print("café".substring(0, 3))
    console.print("café".substring(3, 4))
    console.print("${"a😀b".length()}")
    console.print("${"a😀b".char_count()}")
    console.print("a😀b".substring(1, 2))
    console.print("a😀b".substring(0, 2))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "multibyte substring diverged");
        assert_eq!(run_on_wasm(src), vec!["caf", "é", "6", "3", "😀", "a😀"]);
    }

    // reverse flips character order using char_count + char-based substring, so
    // it's correct for multi-byte UTF-8 ("café" -> "éfac"), not just ASCII.
    // Char-based take/drop: clamp at the ends and count by Unicode scalar, so
    // they slice "café" correctly (take 2 -> "ca", drop 3 -> "é").
    #[test]
    fn std_string_take_drop_backends_agree() {
        let client = r#"

fn main(console: Console):
    console.print("hello".take(3))
    console.print((("[" + "hi".take(10)) + "]"))
    console.print((("[" + "hi".take(0)) + "]"))
    console.print("hello".drop(2))
    console.print((("[" + "hi".drop(5)) + "]"))
    console.print("café".take(2))
    console.print("café".drop(3))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string take/drop diverged");
        assert_eq!(compiled, vec!["hel", "[hi]", "[]", "llo", "[]", "ca", "é"]);
    }

    #[test]
    fn std_string_reverse_backends_agree() {
        let client = r#"

fn main(console: Console):
    console.print("hello".reverse())
    console.print((("[" + "".reverse()) + "]"))
    console.print("a".reverse())
    console.print("café".reverse())
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string reverse diverged");
        assert_eq!(compiled, vec!["olleh", "[]", "a", "éfac"]);
    }

    // to_chars splits a string into single-character strings by Unicode scalar
    // (so "café" yields 4 chars including the multi-byte é). Both backends agree.
    // words splits on any whitespace (tabs/newlines/CRs treated as spaces) and
    // drops empty pieces from runs of whitespace or trailing space.
    // split_once splits at the first separator into (before, after); the
    // separator is dropped, later occurrences stay in `after`, and an absent
    // separator gives (s, ""). Both backends agree.
    // replace_first swaps only the first occurrence (unlike the all-replacing
    // `replace` builtin); an absent needle leaves the string unchanged.
    #[test]
    fn std_string_replace_first_backends_agree() {
        let client = r#"

fn main(console: Console):
    console.print("a.b.c".replace_first(".", "/"))
    console.print("hello".replace_first("l", "L"))
    console.print("xyz".replace_first("q", "Q"))
    console.print("aa".replace_first("a", "bb"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "replace_first diverged");
        assert_eq!(compiled, vec!["a/b.c", "heLlo", "xyz", "bba"]);
    }

    #[test]
    fn std_string_split_once_backends_agree() {
        let client = r#"

fn main(console: Console):
    let (k, v) = "name=witchy".split_once("=")
    console.print(k)
    console.print(v)
    let (a, b) = "no-sep-here".split_once("=")
    console.print(a)
    console.print((("[" + b) + "]"))
    let (h, rest) = "a=b=c".split_once("=")
    console.print(h)
    console.print(rest)
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "split_once diverged");
        assert_eq!(compiled, vec!["name", "witchy", "no-sep-here", "[]", "a", "b=c"]);
    }

    #[test]
    fn std_string_words_backends_agree() {
        let client = r#"

fn main(console: Console):
    let ws = "the  quick\tbrown\nfox ".words()
    console.print("${list.length(ws)}")
    for w in ws:
        console.print(w)
    console.print("${list.length("   ".words())}")
    console.print("${list.length("".words())}")
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "words diverged");
        assert_eq!(compiled, vec!["4", "the", "quick", "brown", "fox", "0", "0"]);
    }

    #[test]
    fn std_string_to_chars_backends_agree() {
        let client = r#"

fn main(console: Console):
    let cs = "café".chars()
    console.print("${list.length(cs)}")
    for c in cs:
        console.print(c)
    console.print("${list.length("".chars())}")
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "to_chars diverged");
        assert_eq!(compiled, vec!["4", "c", "a", "f", "é", "0"]);
    }

    #[test]
    fn std_string_is_empty_count_backends_agree() {
        // is_empty checks for zero characters; count returns non-overlapping
        // occurrences (0 for an empty needle, and overlapping matches don't
        // double-count: "aaaa"/"aa" is 2). Both backends agree.
        let client = r#"

fn main(console: Console):
    console.print("${"".is_empty()}")
    console.print("${"x".is_empty()}")
    console.print("${"banana".count("a")}")
    console.print("${"banana".count("an")}")
    console.print("${"aaaa".count("aa")}")
    console.print("${"abc".count("x")}")
    console.print("${"abc".count("")}")
    console.print("${"aéaéa".count("éa")}")
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string is_empty/count diverged");
        // The last counts a multi-byte needle: "éa" occurs twice in "aéaéa" —
        // a byte-based advance would miscount it (and matters only off ASCII).
        assert_eq!(compiled, vec!["true", "false", "3", "2", "2", "0", "0", "2"]);
    }

    #[test]
    fn std_string_char_at_backends_agree() {
        // RFC-0044 rule 1: char_at returns `Some(c)` in range, `None` out of range
        // (no more "" sentinel). `?? "?"` recovers a display char for the miss.
        let client = r#"

fn main(console: Console):
    console.print("witchy".char_at(0) ?? "?")
    console.print("witchy".char_at(5) ?? "?")
    console.print((("[" + ("witchy".char_at(10) ?? "")) + "]"))
    console.print((("[" + ("".char_at(0) ?? "")) + "]"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "char_at diverged");
        assert_eq!(compiled, vec!["w", "y", "[]", "[]"]);
    }

    #[test]
    fn dict_string_key_through_helpers_backends_agree() {
        let src = r#"
fn put(var d: Dict(String, Int), k: String, v: Int) -> Nil:
    dict.insert(d, k, v)
    return

fn lookup(d: Dict(String, Int), k: String) -> Int:
    dict.get_or(d, k, (0 - 1))

fn main(console: Console):
    var d = dict.new()
    put(d, "apple", 1)
    put(d, "banana", 2)
    console.print("${lookup(d, ("ap" + "ple"))}")
    console.print("${lookup(d, "banana")}")
    console.print("${lookup(d, "cherry")}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "dict string-key via helpers diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "2", "-1"]);
    }

    // Regression: a local variable that shares its name with a same-module
    // function must stay a local, not be rewritten into a first-class reference
    // to that function by the linker. (The function-as-value feature qualifies
    // bare function-name Vars; it must skip names shadowed by a local.)
    #[test]
    fn local_shadowing_function_name_backends_agree() {
        let client = r#"
fn size(n: Int) -> Int:
    (n * 100)

fn main(console: Console):
    var size = 3
    size = (size + 4)
    console.print("${size}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "local shadowing a function name diverged");
        assert_eq!(compiled, vec!["7"]);
    }

    // The linker treats the bundled std as a built-in search path: a program can
    // `import list` without the caller listing the module's source. This unblocks
    // composable std modules (one std module importing another). Verified on both
    // backends with only `main` provided.
    #[test]
    fn linker_auto_resolves_std_imports() {
        let client = r#"
import list

fn main(console: Console):
    console.print("${list.sum(list.range(5))}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "auto-resolved std import diverged");
        assert_eq!(compiled, vec!["10"]);
    }


    #[test]
    fn string_edge_cases_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print("${list.length("abc".split(""))}")
    console.print("${list.length("abc".split("x"))}")
    console.print("${list.length("a,b,c".split(","))}")
    console.print((("[" + "".substring(0, 5)) + "]"))
    console.print((("[" + "hello".substring(3, 1)) + "]"))
    console.print("hello".substring(2, 100))
    console.print("${"hello".contains("")}")
    console.print("${"hello".contains("z")}")
    console.print((("[" + (("" + "x") + "")) + "]"))
    console.print("${"".length()}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "string edge cases diverged");
    }

    #[test]
    fn trim_backends_agree() {
        // trim now compiles: leading/trailing ASCII whitespace (spaces, tabs,
        // newlines, CRs) is stripped; an all-whitespace string trims to "".
        let src = r#"
fn main(console: Console):
    console.print("  hello  ".trim())
    console.print("\t\nfoo\r\n".trim())
    console.print("nospaces".trim())
    console.print("   ".trim())
    console.print("${"  a b  ".trim().length()}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["hello", "foo", "nospaces", "", "3"]);
    }

    // std/json get_path: follow a dotted key path into a decoded value (and a
    // missing path -> None). Pure, so both backends agree.
    // Dead-code elimination: a program importing `list` but using only `map`
    // and `sum` must not compile the rest of the list API (or `option`, which
    // `list` imports) into the WASM — only functions reachable from `main`.
    #[test]
    fn dce_drops_unused_stdlib_functions() {
        let client = r#"
import list

fn main(console: Console):
    let xs = list.map([1, 2, 3], fn(x: Int): (x * 2))
    console.print("${list.sum(xs)}")
"#;
        let mods = vec![("main".to_string(), parser::parse_module(client).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        // Reachable functions are present and the unused ones are dropped: the
        // binary path's `assemble_wir_module` runs the same `reachable_functions`
        // DCE, so inspect the assembled WIR func names directly.
        let wir = codegen::assemble_wir_module(&linked)
            .expect_lowered("the binary path lowers this program");
        // The binary path monomorphizes generics, so the method implementation
        // appears as `List__map__Int__Int`; match on the generated-method prefix.
        let names: Vec<&str> = wir.funcs.iter().map(|f| f.name.as_str()).collect();
        let has = |fn_name: &str| names.iter().any(|n| *n == fn_name || n.starts_with(&format!("{fn_name}__")));
        assert!(has("List__map"), "map should be compiled: {names:?}");
        assert!(has("List__sum"), "sum should be compiled: {names:?}");
        assert!(!has("List__partition"), "partition should be eliminated: {names:?}");
        assert!(!has("List__windows"), "windows should be eliminated: {names:?}");
        assert!(!has("List__sort_by"), "sort_by should be eliminated: {names:?}");
        assert!(!names.iter().any(|n| n.starts_with("option.")), "unused option fns should be eliminated: {names:?}");
        // And it still runs correctly.
        assert_eq!(run_linked_on_wasm(&[("main", client)], "main"), vec!["12"]);
    }

    // std/url: parse assorted URL strings (default ports, explicit port, path,
    // and a malformed one). Pure, so both backends agree.
    #[test]
    fn std_url_parse_backends_agree() {
        let client = r#"
import url
fn describe(s: String) -> String:
    match url.parse(s):
        Ok(u) -> url.scheme(u) + " " + url.host(u) + " " + "${url.port(u)}" + " " + url.path(u)
        Err(e) -> "invalid: " + url.url_error_message(e)
fn main(console: Console):
    console.print(describe("http://example.com"))
    console.print(describe("http://example.com:8080/foo"))
    console.print(describe("https://x.com/a/b"))
    console.print(describe("notaurl"))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std url parse diverged");
        assert_eq!(
            compiled,
            vec![
                "http example.com 80 /",
                "http example.com 8080 /foo",
                "https x.com 443 /a/b",
                "invalid: missing `scheme://` in: notaurl"
            ]
        );
    }

    // std/http get_url: parse a URL string and GET it (loopback). Interpreter-only.
    // std/string trimming: trim/trim_start/trim_end over assorted whitespace.
    // Pure, so both backends agree.
    #[test]
    fn std_string_trim_backends_agree() {
        let client = r#"
fn main(console: Console):
    console.print("[" + "  hello  ".trim() + "]")
    console.print("[" + "  hi".trim_start() + "]")
    console.print("[" + "bye  ".trim_end() + "]")
    console.print("[" + "\t\n x \r\n".trim() + "]")
    console.print("[" + "nospace".trim() + "]")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std string trim diverged");
        assert_eq!(compiled, vec!["[hello]", "[hi]", "[bye]", "[x]", "[nospace]"]);
    }


    // Indentation-based (off-side rule) syntax: blocks are delimited by `:` +
    // indentation rather than braces. A layout pass turns it into the brace form
    // the rest of the pipeline expects, so both backends agree — here over a
    // type, match, for-loop, let/var, and calls.
    #[test]
    fn indentation_syntax_backends_agree() {
        let src = r#"
type Shape:
    Circle(Int)
    Rect(Int, Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> 3 * r * r
        Rect(w, h) -> w * h

fn main(console: Console):
    let xs = [area(Circle(2)), area(Rect(3, 4))]
    var total = 0
    for x in xs:
        total = total + x
    console.print("${total}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "indentation backends diverged");
        assert_eq!(run_on_wasm(src), vec!["24"]);
    }

    // Indentation syntax with traits/impls and a nested if/else expression.
    #[test]
    fn indentation_traits_backends_agree() {
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
"#;
        assert_eq!(interp(src), run_on_wasm(src), "indentation traits diverged");
        assert_eq!(run_on_wasm(src), vec!["42", "yes"]);
    }

    // Regression: a `(...)` expression on the line after a block must be its own
    // statement, not an application of the block's value — the virtual closing
    // brace sits on the previous line so `} (a, n)` stays two things. (This is
    // what `list.partition`'s trailing `(yes, no)` exercises.)
    #[test]
    fn indentation_block_then_paren_expr_backends_agree() {
        let src = r#"
fn pair(n: Int) -> (Int, Int):
    var a = 0
    for i in [1, 2, 3]:
        a = a + i
    (a, n)

fn main(console: Console):
    let (x, y) = pair(10)
    console.print("${x}")
    console.print("${y}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "block-then-paren diverged");
        assert_eq!(run_on_wasm(src), vec!["6", "10"]);
    }

    // std/http: a real HTTP/1.1 GET over the Net capability against a loopback
    // server. Networking is interpreter-only (not compiled), so this isn't a
    // differential test; it proves the capability-gated socket primitives plus
    // the http library parse a live response into status + body.
    // A server replying with a non-numeric status code must not crash the client:
    // `status_code` guards `string_to_int` and reports 0 for a malformed status
    // line, so the body is still readable. Interpreter-only.
    // std/http POST: send a request body and read it back from a loopback echo
    // server. Interpreter-only (networking isn't compiled).
    // std/http response headers: case-insensitive lookup + a missing header.
    // Interpreter-only (networking).
    // std/json: build a nested Json value and serialize it. Pure (no
    // capabilities), so it compiles to WASM and both backends must agree.
    // std/json decode: parse JSON text then re-encode it. The round trip
    // exercises the recursive-descent parser (objects, arrays, strings, bools,
    // null, negative ints, nesting) and must agree on both backends.
    // std/json accessors: decode then pull out a string field (object key
    // lookup), an int field, and an array element. Object lookup compares the
    // decoded, heap-built key with `==`; both backends agree now that codegen
    // tracks the type of a tuple-destructured loop variable (so the comparison
    // is by content, not pointer).
    // Hex (0x..) and binary (0b..) integer literals, including underscore
    // separators, feeding the bitwise operators. Both backends agree.
    #[test]
    fn hex_binary_literals_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print("${255}")
    console.print("${10}")
    console.print("${(255 & 15)}")
    console.print("${(12 | 3)}")
    console.print("${65535}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "hex/binary literals diverged");
        assert_eq!(run_on_wasm(src), vec!["255", "10", "15", "15", "65535"]);
    }

    #[test]
    fn string_to_int_backends_agree() {
        // string_to_int now compiles: leading whitespace and an optional sign
        // are honored, and the parsed value feeds straight into arithmetic.
        let src = r#"
fn main(console: Console):
    console.print("${"42".to_int()}")
    console.print("${"-17".to_int()}")
    console.print("${"  123  ".to_int()}")
    console.print("${"+8".to_int()}")
    console.print("${"0".to_int()}")
    console.print("${("1000000".to_int() + 1)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "-17", "123", "8", "0", "1000001"]);
    }

    #[test]
    fn bitwise_not_backends_agree() {
        // ~x = -x-1 (width-independent), so it agrees across backends.
        let src = r#"
fn main(console: Console):
    console.print("${(~0)}")
    console.print("${(~5)}")
    console.print("${(~(0 - 1))}")
    console.print("${(255 & (~15))}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["-1", "-6", "0", "240"]);
    }

    #[test]
    fn bitwise_operators_backends_agree() {
        // & | ^ << >> on Int, with precedence (& tighter than |, both tighter
        // than ==), and or-patterns still parsing (| in pattern position). Both
        // backends agree.
        let src = r#"
fn classify(n: Int) -> String:
    match n:
        1 -> "pow2"
        2 -> "pow2"
        4 -> "pow2"
        _ -> "other"

fn main(console: Console):
    console.print("${(12 & 10)}")
    console.print("${(12 | 10)}")
    console.print("${(12 ^ 10)}")
    console.print("${(1 << 4)}")
    console.print("${(256 >> 2)}")
    console.print("${((5 & 3) | 8)}")
    console.print("${((5 & 4) == 4)}")
    console.print(classify(2))
    console.print(classify(3))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["8", "14", "6", "16", "64", "9", "true", "pow2", "other"]
        );
    }

    #[test]
    fn generics_example_runs_on_wasm() {
        // A generic `swap((a, b)) -> (b, a)` on a mixed (Int, String) tuple:
        // tuple pattern match + construction through a generic function.
        let src = include_str!("../examples/generics/src/generics.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["answer", "42"]);
    }

    #[test]
    fn signs_example_runs_on_wasm() {
        // Negative-literal match patterns (`-1 -> ...`).
        let src = include_str!("../examples/signs/src/signs.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["left", "right", "stay", "?"]);
    }

    #[test]
    fn mutate_example_runs_on_wasm() {
        // `var` (move-in / move-out) compiles: the example agrees with the
        // interpreter through the WASM backend.
        let src = include_str!("../examples/mutate/src/mutate.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
    }

    #[test]
    fn ownership_example_runs_on_wasm() {
        // `own` (consume / move ownership) compiles and agrees across backends.
        let src = include_str!("../examples/ownership/src/ownership.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
    }

    #[test]
    fn commands_example_runs_and_compiles() {
        let src = include_str!("../examples/commands/src/commands.witchy");
        assert_eq!(interp(src), vec!["total is 1"]);
        assert_fn_compiles(src);
    }

    #[test]
    fn runs_a_file_with_file_based_imports() {
        let dir = std::env::temp_dir().join(format!("witchy_cli_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("strutil.witchy"),
            r#"
pub fn shout(s: String) -> String:
    ("HI " + s)
"#,
        )
        .unwrap();
        let app = dir.join("app.witchy");
        std::fs::write(
            &app,
            "import strutil\nfn main(console: Console):\n    console.print(strutil.shout(\"x\"))\n",
        )
        .unwrap();

        let out = crate::execute_file(app.to_str().unwrap(), Vec::new()).unwrap();
        assert_eq!(out, vec!["HI x"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn caps_diff_gate_flags_a_widening_across_versions() {
        // The supply-chain gate end-to-end: a dependency update whose public API
        // newly demands `Net` is reported as a widening (so CI/install can block),
        // while an unchanged footprint is not.
        let dir = std::env::temp_dir().join(format!("witchy_capsdiff_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v1 = dir.join("v1.witchy");
        let v2 = dir.join("v2.witchy");
        std::fs::write(&v1, r#"
pub fn serve(console: Console) -> Int:
    0
"#).unwrap();
        std::fs::write(
            &v2,
            r#"
pub fn serve(console: Console, net: Net) -> Int:
    0
"#,
        )
        .unwrap();
        assert!(
            crate::report_capability_diff(v1.to_str().unwrap(), v2.to_str().unwrap()).unwrap(),
            "newly demanding Net must be flagged as a widening"
        );
        assert!(
            !crate::report_capability_diff(v1.to_str().unwrap(), v1.to_str().unwrap()).unwrap(),
            "an unchanged footprint must not be a widening"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generics_example() {
        assert_eq!(
            interp(include_str!("../examples/generics/src/generics.witchy")),
            vec!["answer", "42"]
        );
    }

    #[test]
    fn result_example() {
        assert_eq!(
            interp(include_str!("../examples/result/src/result_demo.witchy")),
            vec!["ok 5", "err divide by zero"]
        );
    }

    #[test]
    fn try_example() {
        assert_eq!(
            interp(include_str!("../examples/try/src/try.witchy")),
            vec!["= 11", "error: divide by zero", "error: divide by zero"]
        );
    }

    #[test]
    fn eval_example() {
        assert_eq!(interp(include_str!("../examples/eval/src/eval.witchy")), vec!["20"]);
    }

    #[test]
    fn bank_example() {
        assert_eq!(
            interp(include_str!("../examples/bank/src/bank.witchy")),
            vec![
                "total = 150",
                "remaining: 90",
                "error: insufficient funds for bob"
            ]
        );
    }

    #[test]
    fn guard_example() {
        assert_eq!(
            interp(include_str!("../examples/guard/src/guard.witchy")),
            vec!["negative", "zero", "positive", "8", "-1"]
        );
    }

    #[test]
    fn signs_example() {
        assert_eq!(
            interp(include_str!("../examples/signs/src/signs.witchy")),
            vec!["left", "right", "stay", "?"]
        );
    }

    #[test]
    fn parse_kv_example() {
        // Uses `setting.index_of("=")` → std `string.index_of` (now Option-returning,
        // RFC-0044), so it must link std — `link_run` pulls in the `string` prelude,
        // where the plain `interp` (builtins only) cannot resolve the std function.
        assert_eq!(
            link_run(include_str!("../examples/parse_kv/src/parse_kv.witchy")),
            vec!["timeout", "30", "true"]
        );
    }

    #[test]
    fn fizzbuzz_example() {
        assert_eq!(
            interp(include_str!("../examples/fizzbuzz/src/fizzbuzz.witchy")),
            vec![
                "1", "2", "Fizz", "4", "Buzz", "Fizz", "7", "8", "Fizz", "Buzz", "11", "Fizz",
                "13", "14", "FizzBuzz"
            ]
        );
    }

    #[test]
    fn compute_example_compiles() {
        assert_fn_compiles(include_str!("../examples/compute/src/compute.witchy"));
    }

    #[test]
    fn strings_example_compiles() {
        assert_fn_compiles(include_str!("../examples/strings/src/strings.witchy"));
    }

    #[test]
    fn shapes_example_compiles() {
        assert_fn_compiles(include_str!("../examples/shapes/src/shapes.witchy"));
    }


    /// The glamour rune's source, embedded so tests can `import glamour` without a
    /// sibling file on disk — the same trick `coven`'s server modules use.
    const GLAMOUR_SRC: &str = include_str!("../projects/glamour/src/glamour.witchy");


    /// `std/markdown`'s source, embedded so a test can `import markdown` (it `import glamour`
    /// transitively) without sibling files on disk.
    const MARKDOWN_SRC: &str = include_str!("../projects/glamour/src/markdown.witchy");


    // ---- RFC-0043: write-back by declaration (not the name census) ----

    // ---- RFC-0087: uniform var write-back ----

    /// std/url: malformed URLs return `Err` identically on both backends rather
    /// than accepting a blank scheme/host (BUG-187), swallowing a query into the
    /// host (BUG-249), or trapping on an oversized port (BUG-197).
    #[test]
    fn url_parse_rejects_malformed_on_both_backends() {
        let src = "import url\n\
                   fn show(label: String, s: String, console: Console):\n\
                   \x20   match url.parse(s):\n\
                   \x20       Ok(u) -> console.print(label + \": \" + url.scheme(u) + \"|\" + url.host(u) + \"|${url.port(u)}|\" + url.path(u))\n\
                   \x20       Err(e) -> console.print(label + \": ERR\")\n\
                   fn main(console: Console):\n\
                   \x20   show(\"empty_scheme\", \"://host\", console)\n\
                   \x20   show(\"empty_host\", \"https:///path\", console)\n\
                   \x20   show(\"query\", \"https://example.com?x=1\", console)\n\
                   \x20   show(\"big_port\", \"https://host:99999999999999999999999/p\", console)\n\
                   \x20   show(\"bad_port\", \"https://host:abc/x\", console)\n\
                   \x20   show(\"ok\", \"https://example.com/a/b\", console)\n";
        let expected = [
            "empty_scheme: ERR",
            "empty_host: ERR",
            "query: https|example.com|443|?x=1",
            "big_port: ERR",
            "bad_port: ERR",
            "ok: https|example.com|443|/a/b",
        ];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// std/encoding: base64/base64url reject malformed `=` padding (a middle `=`,
    /// three `=`, an incomplete final group) rather than silently accepting it
    /// (BUG-198), and `hex_to_base64url` is fallible on non-hex input instead of
    /// silently dropping bytes (BUG-201). Both backends agree.
    #[test]
    fn encoding_rejects_malformed_padding_and_hex_on_both_backends() {
        let src = "import encoding\n\
                   fn show(label: String, r: Result(String, encoding.EncodingError), console: Console):\n\
                   \x20   match r:\n\
                   \x20       Ok(v) -> console.print(label + \": OK\")\n\
                   \x20       Err(e) -> console.print(label + \": ERR\")\n\
                   fn main(console: Console):\n\
                   \x20   show(\"mid_pad\", encoding.base64_decode(\"S=Gk\"), console)\n\
                   \x20   show(\"tail_after_pad\", encoding.base64_decode(\"ab=c\"), console)\n\
                   \x20   show(\"triple_pad\", encoding.base64_decode(\"ab===\"), console)\n\
                   \x20   show(\"pad_ok\", encoding.base64_decode(\"SGk=\"), console)\n\
                   \x20   show(\"nopad_ok\", encoding.base64_decode(\"SGk\"), console)\n\
                   \x20   show(\"url_mid_pad\", encoding.base64url_decode(\"J=Gk\"), console)\n\
                   \x20   show(\"url_ok\", encoding.base64url_decode(\"SGk\"), console)\n\
                   \x20   show(\"bad_hex\", encoding.hex_to_base64url(\"zz\"), console)\n\
                   \x20   show(\"good_hex\", encoding.hex_to_base64url(\"4869\"), console)\n";
        let expected = [
            "mid_pad: ERR",
            "tail_after_pad: ERR",
            "triple_pad: ERR",
            "pad_ok: OK",
            "nopad_ok: OK",
            "url_mid_pad: ERR",
            "url_ok: OK",
            "bad_hex: ERR",
            "good_hex: OK",
        ];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    #[test]
    fn bug307_real_body_error_surfaces_over_collect_inference_fallback() {
        // (BUG-307) A genuine body type error must surface even when the module has a
        // result-position bounded call (`iter.collect`) whose annotate fell back —
        // the false "cannot infer the result type" diagnostic must not mask it.
        let src = "import iter\n\
                   import list\n\
                   fn broken() -> Int:\n\
                   \x20   \"oops\"\n\
                   fn main(console: Console):\n\
                   \x20   let a: List(Int) = iter.collect(iter.range(0, 3))\n\
                   \x20   console.print(\"${list.length(a)}\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("broken body must fail").to_string();
        assert!(err.contains("broken") && err.contains("expected `Int`"), "{err}");
    }

    #[test]
    fn bug181_tagged_literals_in_impls_and_consts_expand() {
        // (BUG-181) a `tag"…"` in an impl method OR a top-level `let` constant must
        // be expanded before type-checking — it must not survive as an
        // `Expr::TaggedLit` (which the type checker `unreachable!`s on). The `lit`
        // tag here emits the source `"ok"`, so both sites render `ok`.
        let src = "fn lit(parts: List(String), holes: List(String)) -> String:\n\
                   \x20   \"\\\"ok\\\"\"\n\
                   type Box:\n\
                   \x20   value: Int\n\
                   impl Box:\n\
                   \x20   pub fn label(self) -> String:\n\
                   \x20       lit\"ignored\"\n\
                   let LABEL = lit\"ignored\"\n\
                   fn main(console: Console):\n\
                   \x20   console.print(Box(1).label())\n\
                   \x20   console.print(LABEL)\n";
        let expected = ["ok", "ok"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// REGRESSION (BUG-189/BUG-413): `duration.parse` returns a reachable `Err` for
    /// a unit with no preceding count (`"ms"`) and for an overflowing value (rather
    /// than `Ok(0)` or a silently-wrapped, backend-divergent number), and
    /// `duration.abs` saturates the most-negative value instead of staying negative.
    #[test]
    fn duration_parse_and_abs_edge_cases_backends_agree() {
        let src = "import duration\nfn tag(r: Result(Duration, duration.DurationParseError)) -> String:\n    match r:\n        Ok(d) -> \"ok:\" + \"${duration.to_milliseconds(d)}\"\n        Err(_e) -> \"err\"\nfn main(console: Console):\n    console.print(tag(duration.parse(\"ms\")))\n    console.print(tag(duration.parse(\"1h2m3s\")))\n    console.print(tag(duration.parse(\"99999999999999999999w\")))\n    console.print(\"${duration.to_milliseconds(duration.abs(duration.milliseconds(0 - 9223372036854775807 - 1)))}\")\n";
        let expected = ["err", "ok:3723000", "err", "9223372036854775807"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// REGRESSION (BUG-408/BUG-250): `jwt.rsa_key_from_jwk` rejects an empty JWK (it
    /// used to return an `Ok` bogus key), and `verify_oidc` pins the header `alg` to
    /// RS256 — an `alg: none` token is refused before the signature is even checked
    /// (algorithm-confusion defense, fail closed). Identical on both backends.
    #[test]
    fn jwt_rejects_empty_jwk_and_non_rs256_alg_backends_agree() {
        let src = "import jwt\nfn tag(r: Result(String, jwt.JwtError)) -> String:\n    match r:\n        Ok(_k) -> \"ok\"\n        Err(_e) -> \"err\"\nfn main(console: Console):\n    console.print(tag(jwt.rsa_key_from_jwk(\"\", \"\")))\n    let token = \"eyJhbGciOiJub25lIn0.eyJpc3MiOiJpIiwiYXVkIjoiYSIsImV4cCI6OTk5OTk5OTk5OX0.AAAA\"\n    match jwt.verify_oidc(token, \"00\", \"i\", \"a\", 0):\n        Ok(_c) -> console.print(\"accepted\")\n        Err(e) -> console.print(jwt.jwt_error_message(e))\n";
        let expected = ["err", "JWT `alg` is `none`, not the required `RS256`"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// REGRESSION (BUG-280): a negative channel capacity is unbounded (sends never
    /// block), not the permanently-full channel it used to build. Identical on both
    /// backends.
    #[test]
    fn chan_negative_capacity_is_unbounded_backends_agree() {
        let src = "from chan import Sender\nasync fn prod(tx: Sender(Int)) -> Nil:\n    chan.send(tx, 1).await\n    chan.send(tx, 2).await\n    chan.send(tx, 3).await\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(0 - 1).await\n    chan.scope([prod(tx)]).await\n    let a = chan.recv(rx).await\n    let b = chan.recv(rx).await\n    let c = chan.recv(rx).await\n    console.print(\"${a}\" + \"${b}\" + \"${c}\")\n";
        let expected = ["Some(1)Some(2)Some(3)"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// REGRESSION (BUG-396): `chan.par_map` returns results in INPUT order.
    #[test]
    fn chan_par_map_preserves_input_order_backends_agree() {
        let src = "import list\nasync fn sq(n: Int) -> Int:\n    n * n\nasync fn main(console: Console):\n    let m = chan.par_map([5, 3, 8, 1], fn(x): sq(x)).await\n    console.print(\"${m}\")\n";
        let expected = ["[25, 9, 64, 1]"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// REGRESSION (BUG-396): `chan.par_map`'s structured fan-out is iterative — the
    /// tail-recursive par_build/recv_each/spawn_all no longer build the O(n)-deep
    /// continuation that overflowed the compiled backend's stack (wasm OOB) at
    /// N≈2000. Compiled-only: the interpreter's O(n^2) clone-per-push is too slow
    /// at this scale.
    #[test]
    fn chan_par_map_is_iterative_at_scale_on_wasm() {
        let src = "import list\nasync fn ident(n: Int) -> Int:\n    n\nasync fn main(console: Console):\n    let m = chan.par_map(list.range(2000), fn(x): ident(x)).await\n    console.print(\"${list.length(m)}\")\n";
        assert_eq!(wasm_run(src), vec!["2000"]);
    }

    /// PARITY (BUG-246): a TAB in leading indentation is rejected at the SHARED
    /// parse stage, so neither backend can silently mis-nest a tab-indented body.
    /// The old bug let a tab count as one column, so the body of `if false:` below
    /// lexed shallower than it looked and *executed*. Rejection before codegen means
    /// `witchy run` and the compiled path fail identically (parity by construction).
    #[test]
    fn tab_indentation_rejected_identically_on_both_backends() {
        let src = "fn main(console: Console):\n    if false:\n\tconsole.print(\"tab body executed\")\n    console.print(\"done\")\n";
        let err = typeck::check_str(src).expect_err("a tab-indented body must be rejected");
        assert!(
            err.to_string().contains("tab in leading indentation"),
            "unexpected error: {err}"
        );
    }

    /// PARITY (BUG-339): a multiline tagged literal keeps the raw newline in its
    /// content byte-for-byte. `tagged::parse_splice_expr` used to reindent EVERY
    /// newline when nesting the emitted source under its throwaway `fn __tagsplice()`
    /// wrapper, injecting four spaces after a newline that fell inside a string
    /// literal — so `line1\nline2` rendered as `line1\n    line2`. The fix reindents
    /// only STRUCTURAL newlines (outside string literals), so the tagged literal now
    /// matches a plain multiline string and both backends produce identical bytes.
    #[test]
    fn multiline_tag_literal_preserves_raw_newlines_on_both_backends() {
        let src = "import list\n\nfn raw(parts: List(String), holes: List(String)) -> String:\n    \"\\\"\" + parts.at(0) + \"\\\"\"\n\nfn main(console: Console):\n    console.print(raw\"line1\\nline2\")\n    console.print(\"line1\\nline2\")\n";
        // The plain multiline string (line 2 of output) is the oracle; the tagged
        // literal (line 1) must match it exactly on BOTH backends.
        let expected = vec!["line1\nline2".to_string(), "line1\nline2".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }
