    use crate::{ast, codegen, interpreter, parser, typeck};
    use wasmtime::{Engine, Module};

    fn interp(src: &str) -> Vec<String> {
        assert!(
            typeck::check_str(src).is_ok(),
            "type error: {:?}",
            typeck::check_str(src)
        );
        interpreter::run(src).expect("should run")
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

    /// Link a single source as the entry module `t`, for performance-mode tests.
    fn link_mode(src: &str) -> ast::Module {
        let module = parser::parse_module(src).expect("parse");
        crate::pipeline::link(vec![("t".into(), module)], "t").expect("link")
    }

    /// Every shipped example's entry module: `examples/<name>/src/<name>.witchy`
    /// (the file whose stem matches its rune directory — the one bearing `main`).
    /// Skips `examples/projects/` (multi-rune workspaces, covered by the pm tests)
    /// and each rune's `*_test.witchy` modules and helper modules.
    fn example_entries() -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir("examples").expect("examples directory") {
            let dir = entry.expect("dir entry").path();
            if !dir.is_dir() {
                continue;
            }
            let Some(name) = dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name == "projects" {
                continue;
            }
            let entry_file = dir.join("src").join(format!("{name}.witchy"));
            if entry_file.exists() {
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
        let m = parser::parse_module("mode opt\n\nfn main(console: Console):\n    print(console, \"hi\")\n")
            .expect("parse");
        assert_eq!(m.modes, vec!["opt".to_string()]);
        assert!(parser::parse_module("mode strict\n\nfn main():\n    nil\n").is_err());
        assert!(parser::parse_module("mode turbo\n\nfn main():\n    nil\n").is_err());
        // `mode` stays usable as an ordinary identifier (contextual keyword).
        assert!(parser::parse_module("fn main(console: Console):\n    let mode = 3\n    print(console, __render(mode))\n").is_ok());
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
            "mode opt\nimport helper\n\nfn main(console: Console):\n    print(console, __render(helper.double(21)))\n",
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
            "mode opt\nimport list\n\nfn main(console: Console):\n    print(console, __render(list.length([1, 2, 3])))\n",
        ).expect("parse opt+std");
        crate::pipeline::link(vec![("main".into(), opt_std)], "main").expect("opt importing std is exempt");
    }

    /// In a `mode opt` file, an ownership-relevant parameter (a heap buffer) must
    /// carry an explicit `let`/`var`/`own` convention; scalars and capabilities are
    /// exempt; an ordinary file is never enforced.
    #[test]
    fn mode_requires_ownership_conventions() {
        let unannotated = "mode opt\n\nfn tag(xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    print(console, __render(tag([1, 2, 3])))\n";
        let err = crate::enforce_performance_modes(&link_mode(unannotated), "t")
            .expect_err("unannotated List param must be rejected in a mode file");
        assert!(err.contains("ownership convention"), "{err}");

        // The same code with `let` is accepted.
        let annotated = unannotated.replace("fn tag(xs:", "fn tag(let xs:");
        crate::enforce_performance_modes(&link_mode(&annotated), "t").expect("annotated param passes");

        // A scalar param needs no annotation even in a mode file.
        let scalar = "mode opt\n\nfn twice(n: Int) -> Int:\n    n + n\n\nfn main(console: Console):\n    print(console, __render(twice(3)))\n";
        crate::enforce_performance_modes(&link_mode(scalar), "t").expect("scalar param is exempt");

        // Without a mode directive, the unannotated param is fine.
        let plain = unannotated.replacen("mode opt\n\n", "", 1);
        crate::enforce_performance_modes(&link_mode(&plain), "t").expect("non-mode file is not enforced");
    }

    /// In a mode file, an accumulator that reverts to the copying path inside a
    /// loop (a `Cliff`) is a hard error; in an ordinary file the same shape only
    /// warns (no error).
    #[test]
    fn mode_rejects_accumulator_cliff() {
        let cliff = "mode opt\n\nfn main(console: Console):\n    var xs = []\n    var snaps = []\n    for i in [1, 2, 3]:\n        snaps = list.push(snaps, xs)\n        xs = list.push(xs, i)\n    print(console, __render(list.length(xs)))\n";
        let err = crate::enforce_performance_modes(&link_mode(cliff), "t")
            .expect_err("a repeated copy-revert in a mode file must be rejected");
        assert!(err.contains("rebuilt by copy"), "{err}");

        // The same body without the mode directive is accepted (a note, not an error).
        let plain = cliff.replacen("mode opt\n\n", "", 1);
        crate::enforce_performance_modes(&link_mode(&plain), "t").expect("non-mode file only warns");
    }

    /// A clean `mode opt` program — properly annotated, accumulator stays
    /// in-place — passes enforcement and runs.
    #[test]
    fn clean_mode_program_passes_and_runs() {
        let src = "mode opt\n\nfn main(console: Console):\n    var xs = []\n    for i in [1, 2, 3]:\n        xs = list.push(xs, i)\n    print(console, __render(list.length(xs)))\n";
        crate::enforce_performance_modes(&link_mode(src), "t").expect("clean mode program passes");
        assert_eq!(interpreter::run(src).expect("interp"), vec!["3"]);
    }

    /// `crypto.sha256` — a native intrinsic of the `crypto` module, *not* a global
    /// builtin — matches the canonical SHA-256 vectors, requires `import crypto`,
    /// and computes the same digest on the interpreter and the compiled WASM
    /// backend (the host fills the guest-allocated result string).
    #[test]
    fn crypto_sha256_matches_known_vectors() {
        let out = link_run(
            "import crypto\nfn main(console: Console):\n    print(console, crypto.sha256(\"\"))\n    print(console, crypto.sha256(\"abc\"))\n",
        );
        assert_eq!(
            out,
            vec![
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ]
        );
        // No global builtin: bare `sha256` (without `import crypto`) is unknown.
        assert!(typeck::check_str("fn main(c: Console):\n    print(c, sha256(\"x\"))\n").is_err());
        // The compiled WASM backend computes the same digest (the host fills the
        // 64-byte result the guest pre-allocated) — interpreter↔WASM parity.
        let module = parser::parse_module(
            "import crypto\nfn main(console: Console):\n    print(console, crypto.sha256(\"abc\"))\n",
        )
        .expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        assert_eq!(
            crate::run_wasm_bytes(&bytes).expect("wasm run"),
            vec!["ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"]
        );
    }

    /// The parameter conventions (`var`/`let`/`own` + `move`) behave identically
    /// on both the interpreter and WASM backends — value semantics are
    /// preserved regardless of which knob the author reaches for. `var` writes
    /// back, `let` borrows (read-only), `own` consumes, a bare param is owned, and
    /// `move x` transfers ownership.
    #[test]
    fn conventions_backends_agree() {
        let src = "fn bump(var n: Int):\n    n = n + 1\n\nfn total(let xs: List(Int)) -> Int:\n    var s = 0\n    for x in xs:\n        s = s + x\n    s\n\nfn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn doubled(xs: List(Int)) -> Int:\n    list.at(xs, 0) * 2\n\nfn main(console: Console):\n    var c = 0\n    bump(c)\n    bump(c)\n    print(console, __render(c))\n    let nums = [10, 20, 30]\n    print(console, __render(total(nums)))\n    print(console, __render(doubled(nums)))\n    print(console, __render(list.length(nums)))\n    let g = [1, 2, 3, 4]\n    print(console, __render(drain(move g)))\n";
        let expected = ["2", "60", "20", "3", "4"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0032) `vm.par_map(xs, f)` maps a capture-free function over a list. On the
    /// interpreter it is the sequential oracle; on the compiled backend it runs across
    /// OS-thread VMs. Because results are collected by input index and `f` is pure, the
    /// two backends produce identical output (parity by determinism).
    #[test]
    fn vm_par_map_backends_agree() {
        let src = "import vm\n\nfn dbl(n: Int) -> Int:\n    n * 2\n\nfn main(console: Console):\n    let ys = vm.par_map([1, 2, 3, 4, 5], dbl)\n    print(console, __render(ys))\n    print(console, __render(list.length(ys)))\n";
        let expected = ["[2, 4, 6, 8, 10]", "5"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (Bytes) The first-class `Bytes` type: a UTF-8-free flat byte buffer. Exercises
    /// the round-trip with `String`, length/at/concat/slice/to_list, on both backends
    /// (linked interp + compiled WASM), which must agree — `Bytes` shares `String`'s
    /// `[len][bytes]` layout, so the compiled ops are identity/String-reuse.
    #[test]
    fn bytes_type_backends_agree() {
        let src = "import bytes\nimport list\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    print(console, __render(bytes.length(b)))\n    print(console, __render(bytes.at(b, 0)))\n    print(console, bytes.to_string(b))\n    let c = bytes.concat(b, bytes.from_string(\"?\"))\n    print(console, bytes.to_string(c))\n    print(console, bytes.to_string(bytes.slice(c, 1, 3)))\n    print(console, __render(bytes.to_list(b)))\n    print(console, __render(bytes.is_empty(b)))\n";
        let expected = ["3", "104", "hi!", "hi!?", "i!", "[104, 105, 33]", "false"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (SEC-038) `bytes.at` out of bounds must FAIL on both backends, not silently
    /// read adjacent heap on WASM. The compiled `$bytes_at` bounds-checks and traps
    /// (like `$list_at`), matching the interpreter's "bytes index out of bounds"
    /// error. In-bounds indexing still agrees. (Regression for a silent OOB-read
    /// parity divergence: the old lowering was an unchecked `load8_u`.)
    #[test]
    fn bytes_index_out_of_bounds_errors_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect("compile")
                .expect("the binary path lowers this program");
            (linked, bytes)
        };
        let oob = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    print(console, __render(bytes.at(b, 5)))\n";
        let (lmod, wasm) = compile(oob);
        assert!(
            interpreter::run_module(lmod, ".", Vec::new()).is_err(),
            "interpreter must error on OOB bytes index"
        );
        assert!(crate::run_wasm_bytes(&wasm).is_err(), "WASM must trap on OOB bytes index");
        // A negative index likewise traps (it used to read backwards into the heap).
        let neg = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    print(console, __render(bytes.at(b, 0 - 1)))\n";
        let (nmod, nwasm) = compile(neg);
        assert!(
            interpreter::run_module(nmod, ".", Vec::new()).is_err(),
            "interpreter must error on negative bytes index"
        );
        assert!(crate::run_wasm_bytes(&nwasm).is_err(), "WASM must trap on negative bytes index");
        // In-bounds indexing still agrees.
        let ok = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    print(console, __render(bytes.at(b, 2)))\n";
        let expected = ["33"];
        assert_eq!(link_run(ok), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", ok)], "main"), expected, "wasm");
    }

    /// (RFC-0047) `==` on a function type is a compile-time error — there is no
    /// stable equality for functions (identity is a monomorphization/inlining
    /// accident), and comparing them was a confirmed backend parity divergence
    /// (interpreter name-compares `true`, compiled pointer-compares `false`).
    /// Rejecting deletes the divergence by construction. Both the direct case and
    /// the container/tuple case must error with a teaching message.
    #[test]
    fn function_equality_is_a_compile_error() {
        let direct = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    print(console, __render(f == f))\n";
        let e = typeck::check_str(direct).expect_err("`f == f` must be rejected");
        assert!(e.contains("not defined on function types"), "teaching error, got: {e}");
        // Nested inside a container is caught the same way (depth-uniform).
        let in_list = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    print(console, __render([f] == [f]))\n";
        let el = typeck::check_str(in_list).expect_err("`[f] == [f]` must be rejected");
        assert!(el.contains("not defined on function types"), "teaching error, got: {el}");
        let in_tuple = "fn f(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    print(console, __render((f, 1) == (f, 1)))\n";
        assert!(
            typeck::check_str(in_tuple).expect_err("`(f, 1) == (f, 1)` must be rejected")
                .contains("not defined on function types"),
            "a function nested in a tuple must be rejected too"
        );
    }

    /// (RFC-0047) `==` on a capability type is a compile-time error — capabilities
    /// are authority, not data. Direct and nested-in-a-container both error.
    #[test]
    fn capability_equality_is_a_compile_error() {
        let direct = "fn main(console: Console):\n    print(console, __render(console == console))\n";
        let e = typeck::check_str(direct).expect_err("`console == console` must be rejected");
        assert!(e.contains("not defined on capability types"), "teaching error, got: {e}");
        let in_tuple = "fn main(console: Console):\n    print(console, __render((console, 1) == (console, 1)))\n";
        assert!(
            typeck::check_str(in_tuple).expect_err("cap in a tuple must be rejected")
                .contains("not defined on capability types"),
            "a capability nested in a tuple must be rejected too"
        );
    }

    /// (RFC-0032) `vm.par_map` over `String` elements: each string is a flat
    /// `[len][bytes]` value, so it crosses to a worker VM by a plain byte copy (in via
    /// the worker's `__galloc`, result back out) — no marshaling. A witchy `String` is
    /// always valid UTF-8, so the round-trip is lossless. Both backends must agree.
    #[test]
    fn vm_par_map_string_backends_agree() {
        let src = "import vm\n\nfn shout(s: String) -> String:\n    s + \"!\"\n\nfn main(console: Console):\n    let ys = vm.par_map([\"a\", \"bb\", \"ccc\"], shout)\n    print(console, __render(ys))\n";
        let expected = ["[a!, bb!, ccc!]"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0032) Cross-VM channels: `vm.serve(init, requests, handler)` runs a stateful
    /// service on a long-lived isolated worker VM — it threads `state` through the request
    /// stream (here a running byte concatenation) and emits each new state. Lock-step and
    /// deterministic, so the interpreter (sequential scan) and the compiled backend
    /// (persistent worker VM) produce identical responses.
    #[test]
    fn vm_serve_stateful_service_agrees() {
        let src = "import vm\nimport bytes\n\nfn step(state: Bytes, req: Bytes) -> Bytes:\n    bytes.concat(state, req)\n\nfn main(console: Console):\n    let reqs = [bytes.from_string(\"a\"), bytes.from_string(\"b\"), bytes.from_string(\"c\")]\n    let outs = vm.serve(bytes.from_string(\"\"), reqs, step)\n    for o in outs:\n        print(console, bytes.to_string(o))\n";
        let expected = ["a", "ab", "abc"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0032) Capability-passing: `vm.with_dir(dir, f, input)` runs `f` in an isolated
    /// worker VM granted EXACTLY `dir`. The worker reads a file through the passed `Dir`
    /// (and could reach nothing else). Output is a deterministic function of the file +
    /// input, so the interpreter (runs `f` directly) and the compiled backend (isolated
    /// worker) agree — the isolation is a security property invisible to the result.
    #[test]
    fn vm_with_dir_capability_passing_agrees() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_withdir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("ok.txt"), "hello-from-dir").expect("seed");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = "import vm\nimport bytes\n\nfn reader(d: Dir, name: Bytes) -> Bytes:\n    bytes.from_string(read(d, bytes.to_string(name)))\n\nfn main(console: Console, dir: Dir):\n    let out = vm.with_dir(dir, reader, bytes.from_string(\"ok.txt\"))\n    print(console, bytes.to_string(out))\n";
        let want = vec!["hello-from-dir".to_string()];
        assert_eq!(
            interpreter::run_module(resolve_std_src(src), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&resolve_std_src(src))
            .expect("compile")
            .expect("the binary path lowers this program");
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
    /// both backends agree (the interp oracle runs the sequential `list.map` body).
    #[test]
    fn vm_par_map_bytes_backends_agree() {
        let src = "import vm\nimport bytes\n\nfn tag(b: Bytes) -> Bytes:\n    bytes.concat(b, bytes.from_string(\"!\"))\n\nfn main(console: Console):\n    let xs = [bytes.from_string(\"a\"), bytes.from_string(\"bb\"), bytes.from_string(\"ccc\")]\n    let ys = vm.par_map(xs, tag)\n    print(console, bytes.to_string(list.at(ys, 0)))\n    print(console, bytes.to_string(list.at(ys, 2)))\n    print(console, __render(bytes.length(list.at(ys, 1))))\n";
        let expected = ["a!", "ccc!", "3"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0032) `vm.par_map` stays correct when the native worker-VM fast path does
    /// NOT apply — a CAPTURING closure (here `fn(n): n + base`) would be unsound to run
    /// with a null environment in a separate worker VM, so the compiled backend must
    /// fall through to the sequential `list.map` body. Both backends must still agree.
    #[test]
    fn vm_par_map_capturing_closure_agrees() {
        let src = "import vm\n\nfn main(console: Console):\n    let base = 100\n    let ys = vm.par_map([1, 2, 3], fn(n): n + base)\n    print(console, __render(ys))\n";
        let expected = ["[101, 102, 103]"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// Host-capability operations are reachable via UFCS method syntax: `console.print(x)`
    /// lowers to the bare intrinsic `print(console, x)` — the same surface a library
    /// capability's own `impl` methods already get. The foundation for RFC-0011's
    /// "refinement is a method" model (`net.only(...)`, `dir.subtree(...)`). The method
    /// and free-function forms must agree on both backends.
    #[test]
    fn host_capability_ufcs_method_calls() {
        let src = "fn main(console: Console):\n    console.print(\"a\")\n    print(console, \"b\")\n";
        let expected = ["a", "b"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
        // The refinement verb `net.only(...)` (method) / `only(...)` (free) is exercised on
        // both backends by `net_only_refinement_verb_backends_agree` below.
    }

    /// RFC-0011: `std/confine` builds a typed `NetPolicy` (`confine.tcp(host, port)`)
    /// instead of a hand-written string, and `net.only(policy)` narrows the `Net` to it.
    /// The typed policy carries the same `host:port` pattern the host enforces, so both
    /// backends agree. The grant must admit the pattern.
    #[test]
    fn confine_typed_net_policies_backends_agree() {
        let src = "import confine\nfn main(net: Net, console: Console):\n    let db = net.only(confine.tcp(\"10.0.0.5\", 6379))\n    print(console, \"confined\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let expected = vec!["confined".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec!["10.0.0.5:6379".into()]).expect("interp"),
            expected,
            "interpreter",
        );
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["10.0.0.5:6379"]),
            expected,
            "wasm",
        );
    }

    #[test]
    fn confine_private_denies_internal_addresses_backends_agree() {
        // RFC-0020: `net.deny(confine.private())` is the one-line SSRF/rebinding
        // defense — a connect to a private IP (here loopback) is refused at the
        // capability layer, identically on both backends. `connect` aborts on a
        // denied address, so a successful run means the deny held.
        let src = "import confine\nfn main(net: Net, console: Console):\n    let safe = net.deny(confine.private())\n    print(console, \"denied private ranges\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let expected = vec!["denied private ranges".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec!["8.8.8.8:443".into()]).expect("interp"),
            expected,
            "interpreter",
        );
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["8.8.8.8:443"]),
            expected,
            "wasm",
        );
    }

    /// RFC-0011: `net.only(policy)` is the typed refinement verb — it narrows a `Net`'s
    /// address set to a `NetPolicy` built by `confine`. It narrows identically on both
    /// backends. (The raw-string form survives only as a `--net`/config grant, not a
    /// language builtin — see `retired_restrict_builtin_is_rejected`.)
    #[test]
    fn net_only_refinement_verb_backends_agree() {
        let src = "import confine\nfn main(net: Net, console: Console):\n    let m = net.only(confine.tcp(\"10.0.0.5\", 6379))\n    print(console, \"only\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let expected = vec!["only".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec!["10.0.0.5:6379".into()]).expect("interp"),
            expected,
            "interpreter",
        );
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["10.0.0.5:6379"]),
            expected,
            "wasm",
        );
    }

    /// RFC-0011: the raw-string `restrict` builtin is RETIRED. Address narrowing now goes
    /// only through the typed `net.only(confine...)` verb; a raw `host:port` string survives
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

    /// RFC-0011: `confine.union(a, b)` builds a multi-endpoint `NetPolicy`, and
    /// `net.only(union(...))` narrows to the WHOLE set — so a further refinement to EITHER
    /// endpoint still succeeds (both are admitted). On both backends.
    #[test]
    fn net_only_union_admits_each_endpoint_backends_agree() {
        let src = "import confine\nfn main(net: Net, console: Console):\n    let pair = net.only(confine.union(confine.tcp(\"10.0.0.5\", 6379), confine.tcp(\"10.0.0.6\", 6379)))\n    let a = pair.only(confine.tcp(\"10.0.0.5\", 6379))\n    let b = pair.only(confine.tcp(\"10.0.0.6\", 6379))\n    print(console, \"both\")\n";
        let expected = vec!["both".to_string()];
        assert_eq!(link_run_net(src, &["10.0.0.5:6379", "10.0.0.6:6379"]), expected, "interp");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["10.0.0.5:6379", "10.0.0.6:6379"]),
            expected,
            "wasm",
        );
    }

    /// A comparison operator (`==`/`<`/…) desugars to its trait impl by recovering
    /// the operands' concrete type. The receiver may be introduced by a PATTERN
    /// binding — a `match` arm, an `if let`, or a tuple destructure — whose type
    /// the binding scope alone can't surface (it comes from the scrutinee). Since
    /// both operands share a type, the head is recovered from EITHER side and the
    /// impl mangled directly, so `Ok(p) -> p == base` resolves the same on both
    /// backends instead of failing with "unknown function `eq`".
    #[test]
    fn comparison_on_pattern_bound_operand_backends_agree() {
        let src = "import cmp\n\ntype T derive(Show, PartialEq, Eq, PartialOrd, Ord):\n    x: Int\n    y: Int\n\nfn mk() -> Result(T, String):\n    Ok(T(1, 2))\n\nfn pair() -> (T, T):\n    (T(1, 2), T(3, 4))\n\nfn main(console: Console):\n    let base = T(1, 2)\n    match mk():\n        Ok(p) -> print(console, __render(p == base))\n        Err(_e) -> print(console, \"err\")\n    if let Ok(p) = mk():\n        print(console, __render(p < T(9, 9)))\n    let (a, b) = pair()\n    print(console, __render(a == b))\n    print(console, __render(a < b))\n";
        let expected = ["true", "true", "false", "true"];
        // The linked path (what the CLI and `witchy parity` use) — it resolves
        // `import cmp` and expands the `derive(Ord)` impls the comparisons need.
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// Misusing the ownership conventions is rejected up front by the type checker
    /// (so the same program fails on every backend, never just native): using a
    /// value after it was consumed by `own`, or after `move`. A bare `let` borrow
    /// imposes no such restriction.
    #[test]
    fn conventions_reuse_after_move_rejected() {
        // Reuse after an `own` parameter consumes it.
        let after_own = "fn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    print(c, __render(drain(d)))\n    print(c, __render(list.length(d)))\n";
        let e1 = typeck::check_str(after_own).expect_err("reuse after own should fail");
        assert!(e1.to_string().contains("after it was moved"), "got: {e1:?}");
        // Reuse after an explicit `move`.
        let after_move = "fn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    print(c, __render(drain(move d)))\n    print(c, __render(list.length(d)))\n";
        assert!(
            typeck::check_str(after_move).is_err(),
            "reuse after move should fail"
        );
        // A `let` borrow does NOT consume — reuse is fine.
        let after_borrow = "fn peek(let xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    print(c, __render(peek(d)))\n    print(c, __render(list.length(d)))\n";
        assert!(typeck::check_str(after_borrow).is_ok(), "borrow reuse should be fine");
    }

    /// The full conventions showcase (examples/conventions/src/conventions.witchy) — `var`/`let`/
    /// `own`/`move` across a function, a method (`let self`), an actor (`var`
    /// state, `own` payload), and local bindings — runs identically on the
    /// interpreter and WASM backends.
    #[test]
    fn conventions_showcase_runs() {
        let expected = "count: 2\nsum: 10\ndoubled first: 2\nnums still here, length: 4\nbag total: 60\ndrained length: 3\nrunning sum: 300\nrunning sum: 306\n";
        let (linked, _) = crate::link_file("examples/conventions/src/conventions.witchy").expect("link");
        let interp =
            interpreter::run_module(linked, ".", Vec::new()).expect("interp run").join("\n");
        assert_eq!(format!("{interp}\n"), expected, "interp showcase");
    }

    /// `let` borrows extend past `List` to the other heap types: a `String`
    /// parameter (the recursive-parser shape that motivated this — char ops on a
    /// borrowed string, no clone) and a `Dict`. Native emits `&String` / `&WMap`
    /// and the output matches every backend.
    #[test]
    fn convention_string_and_dict_borrow() {
        let strs = "fn first_char(let s: String) -> String:\n    if string.char_count(s) > 0:\n        string.substring(s, 0, 1)\n    else:\n        \"\"\nfn main(c: Console):\n    let txt = \"héllo\"\n    print(c, first_char(txt))\n    print(c, __render(string.char_count(txt)))\n";
        assert_eq!(interpreter::run(strs).expect("interp str"), ["h", "5"]);
        assert_eq!(run_linked_on_wasm(&[("main", strs)], "main"), ["h", "5"], "wasm str");

        let dict = "fn lookup(let d: Dict(String, Int)) -> Int:\n    dict.get_or(d, \"a\", -1)\nfn main(c: Console):\n    var m = dict.new()\n    m = dict.insert(m, \"a\", 42)\n    print(c, __render(lookup(m)))\n    print(c, __render(dict.length(m)))\n";
        assert_eq!(interpreter::run(dict).expect("interp dict"), ["42", "1"]);
        assert_eq!(run_linked_on_wasm(&[("main", dict)], "main"), ["42", "1"], "wasm dict");
    }

    /// `move` works in every value position (let value, list element, call
    /// argument), forcing a move; the moved binding can't be reused (rejected by
    /// the type checker, uniformly).
    #[test]
    fn convention_move_value_positions() {
        let prog = "fn main(console: Console):\n    let a = [1, 2, 3]\n    let b = move a\n    print(console, __render(list.length(b)))\n";
        assert_eq!(interpreter::run(prog).expect("interp"), ["3"]);
        assert_eq!(run_linked_on_wasm(&[("main", prog)], "main"), ["3"], "wasm");
        // Reuse after move is rejected everywhere.
        let reuse = "fn main(console: Console):\n    let a = [1, 2, 3]\n    let b = move a\n    print(console, __render(list.length(b) + list.length(a)))\n";
        assert!(typeck::check_str(reuse).is_err(), "reuse after move must fail");
    }

    /// A borrow can't escape: returning a `let` parameter transpiles, but Rust's
    /// borrow checker rejects it at compile time (the opt-in contract — drop `let`
    /// or use `own`). A non-escaping borrow compiles fine.
    #[test]
    fn convention_borrow_cannot_escape() {
        // Returning a `let` parameter escapes the borrow — a TYPE error on
        // every backend (the rule moved from the removed native backend's
        // borrow checker into typeck).
        let escapes = "fn id(let xs: List(Int)) -> List(Int):\n    xs\nfn main(c: Console):\n    print(c, __render(list.length(id([1, 2, 3]))))\n";
        let err = typeck::check_str(escapes).expect_err("escaping borrow must be rejected");
        assert!(err.to_string().contains("cannot be returned"), "{err}");
        // Reading it (no escape) is fine.
        let reads = "fn count(let xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    print(c, __render(count([1, 2, 3])))\n";
        assert!(typeck::check_str(reads).is_ok(), "a read-only borrow should check");
    }

    #[test]
    fn match_soundness_exhaustiveness_and_linearity() {
        // C3: an infinite scalar domain needs a catch-all — a guard-only match is
        // non-exhaustive and would trap at runtime, so it's rejected at check time.
        let guard_only = "fn f(n: Int) -> String:\n    match n:\n        m if m > 0 -> \"p\"\n        z if z < 0 -> \"n\"\nfn main(c: Console):\n    print(c, f(1))\n";
        let e = typeck::check_str(guard_only).expect_err("guard-only Int match must be rejected");
        assert!(e.to_string().contains("non-exhaustive match on `Int`"), "{e}");

        // C2: a single-field variant matched only with a narrower sub-pattern
        // (`Circle(Red)`) is rejected when an inner case (`Circle(Blue)`) is
        // missing — the recursive coverage check catches the nested hole.
        let nested = "type Color:\n    Red\n    Blue\ntype Shape:\n    Circle(Color)\n    Square\nfn f(s: Shape) -> Int:\n    match s:\n        Circle(Red) -> 1\n        Square -> 2\nfn main(c: Console):\n    print(c, __render(f(Square)))\n";
        let e = typeck::check_str(nested).expect_err("nested non-exhaustive match must be rejected");
        assert!(e.to_string().contains("non-exhaustive"), "{e}");

        // ...but the idiomatic `Some(V) / None` form — `Some` covered by
        // ENUMERATING the inner variants, no wholesale `Some(_)` — must still check
        // (the conservative earlier rule wrongly rejected this; the recursion does not).
        let some_enum = "type Msg:\n    A\n    B\nfn f(o: Option(Msg)) -> Int:\n    match o:\n        Some(A) -> 1\n        Some(B) -> 2\n        None -> 0\nfn main(c: Console):\n    print(c, __render(f(Some(A))))\n";
        assert!(typeck::check_str(some_enum).is_ok(), "idiomatic Some(V)/None must check");

        // C5: a pattern may not bind the same name twice (no equality patterns).
        let dup = "type P:\n    P(Int, Int)\nfn f(p: P) -> Int:\n    match p:\n        P(x, x) -> x\nfn main(c: Console):\n    print(c, __render(f(P(3, 4))))\n";
        let e = typeck::check_str(dup).expect_err("duplicate pattern binding must be rejected");
        assert!(e.to_string().contains("more than once"), "{e}");

        // Valid exhaustive / linear matches still check (no over-rejection).
        let ok = "type Shape:\n    Circle(Int)\n    Square\nfn f(s: Shape) -> Int:\n    match s:\n        Circle(r) -> r\n        Square -> 0\nfn g(n: Int) -> Int:\n    match n:\n        0 -> 0\n        _ -> 1\nfn main(c: Console):\n    print(c, __render(f(Circle(3)) + g(5)))\n";
        assert!(typeck::check_str(ok).is_ok(), "valid exhaustive matches must check");
    }

    #[test]
    fn capability_is_sealed_across_modules() {
        // RFC-0002: `capability Conn from Net` is a SEALED brand — it may be
        // constructed or destructured only in its declaring module (`redis`).
        use crate::pipeline::link;
        use crate::parser::parse_module;
        let lib = "capability Conn from Net[Connect, Tcp]\npub fn open(net: Net[Connect, Tcp]) -> Conn:\n    Conn(net)\npub fn ping(c: Conn) -> Int:\n    match c:\n        Conn(net) -> 1\n";
        let mods = |app: &str| {
            vec![
                ("redis".to_string(), parse_module(lib).expect("lib parse")),
                ("app".to_string(), parse_module(app).expect("app parse")),
            ]
        };
        // Forging the sealed cap in another module is rejected.
        let forge = "import redis\nfn main(console: Console, net: Net):\n    let c = Conn(net)\n    print(console, \"${redis.ping(c)}\")\n";
        let e = format!("{:?}", link(mods(forge), "app").expect_err("forge must be rejected"));
        assert!(e.contains("sealed capability") && e.contains("construct"), "{e}");
        // Unwrapping (destructuring) it in another module is rejected too.
        let unwrap = "import redis\nfn main(console: Console, net: Net):\n    let c = redis.open(net)\n    match c:\n        Conn(n) -> print(console, \"x\")\n";
        let e2 = format!("{:?}", link(mods(unwrap), "app").expect_err("unwrap must be rejected"));
        assert!(e2.contains("destructure"), "{e2}");
        // The legitimate path — mint via the library, then use it — links fine.
        let ok = "import redis\nfn main(console: Console, net: Net):\n    let c = redis.open(net)\n    print(console, \"${redis.ping(c)}\")\n";
        assert!(link(mods(ok), "app").is_ok(), "legit mint-then-use must link");
        // A module can construct/destructure its OWN sealed capability.
        assert!(parse_module(lib).is_ok());
    }

    /// Conventions apply to a method's receiver too: `let self` borrows it
    /// (read-only), and `own self` consumes it (the value can't be used after the
    /// call). Both run identically on interpreter and native.
    #[test]
    fn convention_method_receivers() {
        // `let self` — borrow the receiver, return a fresh value (functional style).
        let borrow_self = "type Counter:\n    Counter(Int)\nimpl Counter:\n    fn incremented(let self) -> Counter:\n        match self:\n            Counter(n) -> Counter(n + 1)\nfn main(c: Console):\n    let a = Counter(5)\n    match a.incremented():\n        Counter(n) -> print(c, __render(n))\n";
        // `own self` — consume the receiver.
        let own_self = "import list\ntype Buffer:\n    Buffer(List(Int))\nimpl Buffer:\n    fn drain(own self) -> Int:\n        match self:\n            Buffer(xs) -> list.sum(xs)\nfn main(c: Console):\n    let buf = Buffer([1, 2, 3])\n    print(c, __render(buf.drain()))\n";
        for (tag, src) in [("let_self", borrow_self), ("own_self", own_self)] {
            assert_eq!(link_run(src), vec!["6"], "{tag} interp");
            assert_eq!(wasm_run(src), vec!["6"], "{tag} wasm");
        }
    }

    /// A borrow can be forwarded BOTH ways: to another borrow parameter it passes
    /// straight through (`&T` -> `&T`, no copy), and to an owned parameter it is
    /// deref-cloned (you can't move out of a borrow). Same result on every backend.
    #[test]
    fn convention_borrow_forwarding() {
        let src = "fn owned_first(xs: List(Int)) -> Int:\n    list.at(xs, 0) * 2\n\nfn borrowed_len(let ys: List(Int)) -> Int:\n    list.length(ys)\n\nfn report(let xs: List(Int)) -> Int:\n    borrowed_len(xs) + owned_first(xs)\n\nfn main(c: Console):\n    let data = [5, 6, 7]\n    print(c, __render(report(data)))\n    print(c, __render(list.length(data)))\n";
        assert_eq!(interpreter::run(src).expect("interp"), ["13", "3"]);
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), ["13", "3"], "wasm");
    }

    /// Python-style f-strings: `f"...{expr}..."` interpolates (with `{{`/`}}` for
    /// literal braces), desugaring to `__render` + concat — same result on both
    /// backends.
    #[test]
    fn f_strings_interpolate() {
        let src = "fn main(console: Console):\n    let name = \"world\"\n    let n = 6\n    print(console, f\"hi {name} #{n * 7}\")\n    print(console, f\"{{braces}}\")\n";
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
            "fn fib(n: Int) -> Int:\n    if n < 2:\n        n\n    else:\n        fib(n - 1) + fib(n - 2)\nfn main(console: Console):\n    print(console, __render(fib(10)))\n",
        )
        .expect("write temp source");
        let wat = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat");
        let _ = std::fs::remove_file(&path);
        assert!(wat.starts_with("(module"), "expected a wasm module, got: {}", &wat[..wat.len().min(40)]);
        // The fib function is emitted, module-qualified by the file stem.
        assert!(wat.contains(".fib (param $n i64)"), "expected the fib function in the WAT");
    }

    /// (RFC-0034 L3) Closure devirtualization fires and is gated. A single-bound,
    /// never-reassigned closure local `f` is called with a direct `call $__lamw{i}`
    /// under the default (`direct-call` on) and falls back to `call_indirect` under
    /// `-direct-call`. This is the call-SHAPE proof the lever fired — devirt moves no
    /// heap, so there is no `witchy stats` counter; OUTPUT invariance (and the
    /// captures-through-a-direct-call case) is the differential sweep's job.
    #[test]
    fn devirtualizes_single_bound_closure_call() {
        use crate::opt::{self, Opt, OptSet};
        let path =
            std::env::temp_dir().join(format!("witchy_devirt_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "fn main(console: Console):\n    let f = fn(x: Int): x % 7\n    var i = 0\n    var acc = 0\n    while i < 20:\n        acc = acc + f(i)\n        i = i + 1\n    print(console, __render(acc))\n",
        )
        .expect("write temp source");

        opt::set_for_tests(Some(OptSet::default_set()));
        let on = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::DirectCall)));
        let off = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat off");
        opt::set_for_tests(None);
        let _ = std::fs::remove_file(&path);

        // `f`'s single call site is the only indirect-call candidate in this program.
        assert!(on.contains("call $__lamw"), "direct-call on: expected a direct closure call");
        assert!(!on.contains("call_indirect"), "direct-call on: the closure call should be devirtualized");
        assert!(off.contains("call_indirect"), "direct-call off: expected the indirect closure call");
        assert!(!off.contains("call $__lamw"), "direct-call off: no direct closure call");
    }

    /// (RFC-0034 L2) Bounds-check elision fires and is gated. `xs[i]` inside a
    /// `for i in 0..list.length(xs)` loop (xs unmutated) lowers to an unchecked element
    /// load under the default (`bounds-elide` on) — no `call $list_at` — and keeps the
    /// checked `$list_at` trap guard under `-bounds-elide`. The call-SHAPE firing proof;
    /// OUTPUT invariance (and the reassigned/inclusive/aliased-list cases that must
    /// stay checked) is the differential sweep's job.
    #[test]
    fn elides_bounds_check_in_counted_loop() {
        use crate::opt::{self, Opt, OptSet};
        let path =
            std::env::temp_dir().join(format!("witchy_bounds_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "fn main(console: Console):\n    let xs = [10, 20, 30, 40, 50]\n    var total = 0\n    for i in 0..list.length(xs):\n        total = total + xs[i]\n    print(console, __render(total))\n",
        )
        .expect("write temp source");

        opt::set_for_tests(Some(OptSet::default_set()));
        let on = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::BoundsElide)));
        let off = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat off");
        opt::set_for_tests(None);
        let _ = std::fs::remove_file(&path);

        // `xs[i]` is the only list access in this program.
        assert!(!on.contains("call $list_at"), "bounds-elide on: the access should be unchecked");
        assert!(off.contains("call $list_at"), "bounds-elide off: expected the checked $list_at call");
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
    print(console, __render(a))
    var b = 0
    for i in 1..=5:
        b = b + i
    print(console, __render(b))
    var c = 0
    for i in 0..100:
        if i == 10:
            break
        c = c + i
    print(console, __render(c))
    var d = 0
    for i in 0..10:
        if i % 2 == 0:
            continue
        d = d + i
    print(console, __render(d))
    var e = 0
    for i in 5..5:
        e = e + 1
    for i in 5..2:
        e = e + 1
    print(console, __render(e))
    var f = 0
    for i in 0..3:
        for j in 0..3:
            f = f + i * j
    print(console, __render(f))
    var g = 0
    for i in 0..100000:
        g = g + 1
    print(console, __render(g))
"#;
        let expected = vec!["10", "15", "45", "25", "0", "9", "100000"];
        assert_eq!(interp(src), expected);
        assert_eq!(run_on_wasm(src), expected);
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
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}{op}{hi}:\n        s = s + i\n    print(console, __render(s))\n"
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
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}..{hi}:\n        if i % 2 != 0:\n            continue\n        s = s + i\n    print(console, __render(s))\n"
                );
                let reference: i64 = (lo..hi).filter(|x| x % 2 == 0).sum();
                let want = vec![reference.to_string()];
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm(&src), want);
            }
        }
    }

    /// Property tests over the standard library: invariants that must hold for
    /// *any* input — encode/decode round-trips, calendar inverses, semver
    /// rendering — checked by generating the input, running it through the witchy
    /// stdlib, and comparing to a Rust reference. These catch edge cases (empty
    /// strings, embedded quotes/newlines, negative timestamps) unit tests miss.
    mod stdlib_properties {
        use super::link_run;
        use proptest::prelude::*;

        /// Escape a Rust string into the body of a witchy `"..."` literal.
        fn esc(s: &str) -> String {
            let mut out = String::new();
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '"' => out.push_str("\\\""),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    _ => out.push(c),
                }
            }
            out
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            /// `encoding.hex_encode` equals the byte-wise lowercase hex reference.
            #[test]
            fn hex_encode_matches_reference(s in "[ -#%-z|~]{0,40}") {
                let src = format!(
                    "import encoding\nfn main(console: Console):\n    print(console, encoding.hex_encode(\"{}\"))\n",
                    esc(&s)
                );
                let reference: String = s.bytes().map(|b| format!("{b:02x}")).collect();
                prop_assert_eq!(link_run(&src), vec![reference]);
            }

            /// base64 decode is the inverse of encode, for any printable ASCII.
            #[test]
            fn base64_roundtrips(s in "[ -#%-z|~]{0,48}") {
                let src = format!(
                    "import encoding\nfn main(console: Console):\n    let s = \"{}\"\n    print(console, yn(encoding.base64_decode(encoding.base64_encode(s)) == s))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                    esc(&s)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// hex decode is the inverse of encode.
            #[test]
            fn hex_roundtrips(s in "[ -#%-z|~]{0,48}") {
                let src = format!(
                    "import encoding\nfn main(console: Console):\n    let s = \"{}\"\n    print(console, yn(encoding.hex_decode(encoding.hex_encode(s)) == s))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                    esc(&s)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// `time.to_unix` is the exact inverse of `time.from_unix`, across the
            /// CE range and negative (pre-1970) timestamps.
            #[test]
            fn time_unix_roundtrips(n in -62135596800i64..=253402300799i64) {
                let src = format!(
                    "import time\nfn main(console: Console):\n    print(console, yn(time.to_unix(time.from_unix({n})) == {n}))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n"
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// A single CSV field round-trips through encode/parse — including
            /// embedded commas, quotes, and newlines (the cases that need quoting).
            #[test]
            fn csv_field_roundtrips(s in "[a-zA-Z0-9 ,\"\n]{0,24}") {
                let src = format!(
                    "import csv\nfn main(console: Console):\n    let s = \"{}\"\n    let rows = csv.parse(csv.encode([[s]]))\n    print(console, yn(list.length(rows) == 1 && list.length(list.at(rows, 0)) == 1 && list.at(list.at(rows, 0), 0) == s))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                    esc(&s)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// `semver.format` after `parse` reproduces the canonical version.
            #[test]
            fn semver_roundtrips(a in 0i64..2000, b in 0i64..2000, c in 0i64..2000) {
                let v = format!("{a}.{b}.{c}");
                let src = format!(
                    "import semver\nfn main(console: Console):\n    match semver.parse(\"{v}\"):\n        Ok(x) -> print(console, semver.format(x))\n        Err(e) -> print(console, \"err\")\n"
                );
                prop_assert_eq!(link_run(&src), vec![v]);
            }

            /// `path.normalize` is idempotent — normalizing an already-normal path
            /// changes nothing — over arbitrary `.`/`..`/segment soup.
            #[test]
            fn path_normalize_is_idempotent(p in "[a-c./]{0,24}") {
                let src = format!(
                    "import path\nfn main(console: Console):\n    let once = path.normalize(\"{}\")\n    print(console, yn(path.normalize(once) == once))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                    esc(&p)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// Run-length decode is the inverse of encode (the `examples/rle`
            /// algorithm, exercising string.to_chars/repeat + ascii.is_digit/
            /// to_digit). Restricted to digit-free input: the count-prefix format
            /// is only unambiguous when the data carries no digits, so this both
            /// asserts the round-trip and documents that boundary.
            #[test]
            fn rle_round_trips_over_digit_free_text(s in "[a-zA-Z ]{0,40}") {
                let src = format!(
                    "import string\nimport ascii\n\nfn encode(t: String) -> String:\n    let cs = string.chars(t)\n    let n = list.length(cs)\n    var out = \"\"\n    var i = 0\n    while i < n:\n        let c = list.at(cs, i)\n        var k = 0\n        while i < n && list.at(cs, i) == c:\n            k = k + 1\n            i = i + 1\n        out = out + __render(k) + c\n    out\n\nfn decode(e: String) -> String:\n    let cs = string.chars(e)\n    let n = list.length(cs)\n    var out = \"\"\n    var i = 0\n    while i < n:\n        var k = 0\n        while i < n && ascii.is_digit(list.at(cs, i)):\n            k = k * 10 + ascii.to_digit(list.at(cs, i))\n            i = i + 1\n        if i < n:\n            out = out + string.repeat(list.at(cs, i), k)\n            i = i + 1\n    out\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n\nfn main(console: Console):\n    let s = \"{}\"\n    print(console, yn(decode(encode(s)) == s))\n",
                    esc(&s)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }
        }
    }

    /// `crypto.ed25519_verify` — a native intrinsic of the `crypto` module — is a
    /// total signature check: it accepts a genuine signature and rejects a
    /// tampered message and malformed input.
    #[test]
    fn crypto_ed25519_verify_checks_signatures() {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let hex = |bs: &[u8]| -> String { bs.iter().map(|b| format!("{b:02x}")).collect() };
        let pk = hex(sk.verifying_key().as_bytes());
        let msg = "release: acme/widget@1.0.0";
        let sig = hex(&sk.sign(msg.as_bytes()).to_bytes());

        let prog = |pubk: &str, m: &str, s: &str| {
            format!(
                "import crypto\nfn main(console: Console):\n    print(console, if crypto.ed25519_verify(\"{pubk}\", \"{m}\", \"{s}\"): \"ok\" else: \"bad\")\n"
            )
        };
        assert_eq!(link_run(&prog(&pk, msg, &sig)), vec!["ok"], "valid signature must verify");
        assert_eq!(
            link_run(&prog(&pk, "release: acme/widget@1.0.1", &sig)),
            vec!["bad"],
            "tampered message must fail"
        );
        assert_eq!(link_run(&prog(&pk, msg, "00")), vec!["bad"], "malformed sig must fail, not panic");
    }

    /// `crypto.ecdsa_p256_verify` (WebAuthn "ES256") verifies a real P-256/SHA-256
    /// signature, rejects a tampered message, and is total on a malformed signature.
    /// KAT: SEC1-uncompressed pubkey + ASN.1-DER sig (generated with the `cryptography` lib).
    #[test]
    fn crypto_ecdsa_p256_verify_checks_signatures() {
        let pk = "048f81cd9fca785a42a6f5dd58972cc0f702e83b1c960b5912354471496597e227fec81ff1d52530b06d7091649e6beb49dba70968b4b727bb24e3ceb7dd01a039";
        let msg = "webauthn-es256-test-message";
        let sig = "304402203260029f4c6beb2e78afdd906c057c63f8828e2b03820de7053d97254577fb8c02204478b9b75f8fd7a1ce4298f0d119e12926dafda116ae4c197b0048dc117bc9de";
        let prog = |pubk: &str, m: &str, s: &str| {
            format!(
                "import crypto\nfn main(console: Console):\n    print(console, if crypto.ecdsa_p256_verify(\"{pubk}\", \"{m}\", \"{s}\"): \"ok\" else: \"bad\")\n"
            )
        };
        assert_eq!(link_run(&prog(pk, msg, sig)), vec!["ok"], "valid ES256 signature must verify");
        assert_eq!(link_run(&prog(pk, "wrong-message", sig)), vec!["bad"], "tampered message must fail");
        assert_eq!(link_run(&prog(pk, msg, "30060201010201ff")), vec!["bad"], "malformed sig must fail, not panic");
    }

    /// `crypto.sha512` and `crypto.hmac_sha256` against standard known-answer vectors
    /// (SHA-512("abc"); HMAC-SHA256 RFC 4231 test case 1).
    #[test]
    fn crypto_sha512_and_hmac_match_known_vectors() {
        let p1 = "import crypto\nfn main(console: Console):\n    print(console, crypto.sha512(\"abc\"))\n";
        assert_eq!(
            link_run(p1),
            vec!["ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"]
        );
        let key = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
        let p2 = format!(
            "import crypto\nfn main(console: Console):\n    print(console, crypto.hmac_sha256(\"{key}\", \"Hi There\"))\n"
        );
        assert_eq!(link_run(&p2), vec!["b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"]);
    }

    /// The aws-lc-rs crypto extensions (`sha512`, `sha3_256`, `hmac_sha256`,
    /// `ecdsa_p256_verify`) produce byte-identical results on the interpreter and
    /// the compiled WASM backend: the host imports bridge to the SAME native
    /// registry the interpreter calls, so the backends agree by construction.
    /// This guards the bridge that lets coven-web run fully sandboxed.
    #[test]
    fn crypto_extensions_backends_agree() {
        let pk = "048f81cd9fca785a42a6f5dd58972cc0f702e83b1c960b5912354471496597e227fec81ff1d52530b06d7091649e6beb49dba70968b4b727bb24e3ceb7dd01a039";
        let sig = "304402203260029f4c6beb2e78afdd906c057c63f8828e2b03820de7053d97254577fb8c02204478b9b75f8fd7a1ce4298f0d119e12926dafda116ae4c197b0048dc117bc9de";
        let src = format!(
"import crypto
fn main(console: Console):
    print(console, crypto.sha512(\"abc\"))
    print(console, crypto.sha3_256(\"abc\"))
    print(console, crypto.hmac_sha256(\"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b\", \"Hi There\"))
    print(console, if crypto.ecdsa_p256_verify(\"{pk}\", \"webauthn-es256-test-message\", \"{sig}\"): \"ok\" else: \"bad\")
    print(console, if crypto.ecdsa_p256_verify(\"{pk}\", \"tampered\", \"{sig}\"): \"ok\" else: \"bad\")
"
        );
        let expected = vec![
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532",
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            "ok",
            "bad",
        ];
        assert_eq!(link_run(&src), expected, "interpreter");
        assert_eq!(wasm_run(&src), expected, "wasm");
    }

    /// `string.from_code` (a code point -> its UTF-8 character) agrees across the
    /// interpreter and the compiled WASM backend, for a 1-byte (ASCII), 2-byte
    /// (é), 3-byte (中) and 4-byte (😀) encoding, and yields U+FFFD for a lone
    /// surrogate (an invalid scalar value) rather than trapping.
    #[test]
    fn string_from_code_backends_agree() {
        let src = "import string\nfn main(console: Console):\n    print(console, string.from_code(65) + string.from_code(233) + string.from_code(20013) + string.from_code(128512) + string.from_code(55296))\n";
        let expected = vec!["A\u{e9}\u{4e2d}\u{1f600}\u{fffd}"];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// The JSON decoder unescapes `\uXXXX` — including astral characters spelled
    /// as a UTF-16 surrogate pair (`😀` -> 😀) — identically on both
    /// backends. Guards the `string.from_code`-powered `\u` path in `std/json`.
    #[test]
    fn json_unicode_escapes_backends_agree() {
        let src = r#"import json
import option

fn show(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "none"

fn main(console: Console):
    match json.decode("{\"k\":\"caf\\u00e9 \\ud83d\\ude00 \\u4e2d\"}"):
        Ok(j) ->
            match json.get(j, "k"):
                Some(v) -> print(console, show(json.as_string(v)))
                None -> print(console, "nokey")
        Err(e) -> print(console, "err")
"#;
        let expected = vec!["caf\u{e9} \u{1f600} \u{4e2d}"];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// `std/webauthn.verify_assertion` accepts a real ES256 WebAuthn assertion and
    /// rejects a tampered signature and a missing user-verification flag. Vectors
    /// generated with the `cryptography` lib (P-256, real authenticatorData).
    #[test]
    fn webauthn_verify_assertion_checks_an_es256_assertion() {
        let pubkey = "045336195e14d40d2d2d3084160b8d776b7d6cdc2e0d162b8da57d8c87dcb6360b67c39ee3d657d7387cec773723df914e5547359511f051fbb6e327368723dba1";
        let client = "{\\\"type\\\":\\\"webauthn.get\\\",\\\"challenge\\\":\\\"dGVzdC1jaGFsbGVuZ2U\\\",\\\"origin\\\":\\\"https://coven.example\\\"}";
        let ad_uv = "fb829c116ec8fed5624aba5b473a0b3a93ca17f477ea91ab2c6ebc49166f860d0500000001";
        let sig_uv = "304602210088b792258e9149557b201f677ffadeda762a2bbd819fb43a6aaff3940681f16e022100b0b770fd5d498d536a6a7d4e641becad007790eb01a85fb8fd9c6e8304ead0ec";
        let ad_up = "fb829c116ec8fed5624aba5b473a0b3a93ca17f477ea91ab2c6ebc49166f860d0100000001";
        let sig_up = "304402207cdb90e725b9051a0918c3a12b2d18e4c952e8e90acde4f49bd0cc7d0c8a18bd02200b9b3f40d586103527e3aa27677746366d62a200209c9a19a6547d515a49a1f8";
        let prog = |ad: &str, sig: &str, uv: &str| {
            format!(
"import webauthn
fn show(r: Result(Bool, String)) -> String:
    match r:
        Ok(_) -> \"ok\"
        Err(e) -> e
fn main(console: Console):
    print(console, show(webauthn.verify_assertion(\"{pubkey}\", \"{ad}\", \"{client}\", \"{sig}\", \"dGVzdC1jaGFsbGVuZ2U\", \"https://coven.example\", \"coven.example\", {uv})))
"
            )
        };
        assert_eq!(link_run(&prog(ad_uv, sig_uv, "true")), vec!["ok"], "valid assertion must verify");
        assert!(
            link_run(&prog(ad_uv, &format!("00{sig_uv}"), "true")).join("").contains("signature"),
            "tampered signature must be rejected"
        );
        assert!(
            link_run(&prog(ad_up, sig_up, "true")).join("").contains("verification"),
            "missing user-verification flag must be rejected when required"
        );
    }

    /// `encoding.base64url_of_hex` — base64url (no padding) of bytes given as hex.
    #[test]
    fn encoding_base64url_of_hex_matches() {
        // hex("test-challenge") -> base64url "dGVzdC1jaGFsbGVuZ2U" (WebAuthn challenge form).
        let p = "import encoding\nfn main(console: Console):\n    print(console, encoding.base64url_of_hex(\"746573742d6368616c6c656e6765\"))\n";
        assert_eq!(link_run(p), vec!["dGVzdC1jaGFsbGVuZ2U"]);
    }

    /// `crypto.ed25519_verify` runs in the *compiled WASM backend* too — bridged
    /// into the sandbox as a host import that calls the same `native` registry
    /// the interpreter uses, so the two tiers agree. (The native module runs at
    /// full Rust speed on the host; the sandbox only sees this one pure import.)
    #[test]
    fn crypto_ed25519_verify_runs_in_the_wasm_backend() {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let hex = |bs: &[u8]| -> String { bs.iter().map(|b| format!("{b:02x}")).collect() };
        let pk = hex(sk.verifying_key().as_bytes());
        let msg = "wasm-signed";
        let sig = hex(&sk.sign(msg.as_bytes()).to_bytes());
        let prog = |m: &str| {
            format!(
                "import crypto\nfn main(console: Console):\n    print(console, if crypto.ed25519_verify(\"{pk}\", \"{m}\", \"{sig}\"): \"ok\" else: \"bad\")\n"
            )
        };
        let wasm = |src: &str| -> Vec<String> {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect("compile")
                .expect("the binary path lowers this program");
            crate::run_wasm_bytes(&bytes).expect("wasm run")
        };
        // Genuine signature verifies in both backends; a tampered message fails
        // in both — the WASM host import and the interpreter agree.
        assert_eq!(wasm(&prog(msg)), vec!["ok"]);
        assert_eq!(link_run(&prog(msg)), vec!["ok"]);
        assert_eq!(wasm(&prog("tampered")), vec!["bad"]);
        assert_eq!(link_run(&prog("tampered")), vec!["bad"]);
    }

    fn wasm_run(src: &str) -> Vec<String> {
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        crate::run_wasm_bytes(&bytes).expect("wasm run")
    }

    /// `wasm_run` that also reads the exported `__witchy_reowns` counter —
    /// the timing-free proof of whether accumulation ran in place (O(1)
    /// re-owns) or fell to the copying path (O(n) re-owns).
    fn wasm_run_reowns(src: &str) -> (Vec<String>, i64) {
        use crate::runtime::{Capabilities, Runtime};
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
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
            .expect("compile")
            .expect("the binary path lowers this program");
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

    /// THE UNIQUENESS ANALYSIS, observable: an alias taken BEFORE the loop
    /// zeroes the ownership token once — the first push re-owns (one copy)
    /// and everything after runs in place. The old syntactic whitelist
    /// disqualified the variable outright (O(n²), memory-cap trap at this
    /// size). The alias still sees its snapshot.
    #[test]
    fn analysis_alias_before_loop_stays_linear() {
        let src = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    let snapshot = xs\n    var i = 0\n    while i < 50000:\n        xs = list.push(xs, i)\n        i = i + 1\n    print(console, __render(snapshot))\n    print(console, __render(list.length(xs)))\n";
        let want = vec!["[1, 2, 3]".to_string(), "50003".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns <= 2, "expected O(1) re-owns, got {reowns}");
    }

    /// An alias RE-TAKEN inside the loop forces the copying path each
    /// iteration (the kill re-zeroes the token) — correct, O(n) re-owns, and
    /// exactly what the cliff diagnostic exists to flag.
    #[test]
    fn analysis_alias_inside_loop_reowns_per_iteration() {
        let src = "fn main(console: Console):\n    var ys = []\n    var last = [9]\n    var j = 0\n    while j < 200:\n        ys = list.push(ys, j)\n        last = ys\n        j = j + 1\n    print(console, __render(list.length(last)))\n";
        let want = vec!["200".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns >= 150, "every iteration must re-own, got {reowns}");
    }

    /// FUNCTION SUMMARIES: a read-only helper called in the hot loop no
    /// longer kills the token (the bottom-up pass proves its parameter never
    /// aliases out). Under the whitelist this was an instant disqualification.
    #[test]
    fn analysis_readonly_call_keeps_loop_linear() {
        let src = "fn peek(xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    var ws = []\n    var m = 0\n    var probe = 0\n    while m < 3000:\n        ws = list.push(ws, m)\n        probe = peek(ws)\n        m = m + 1\n    print(console, __render(probe))\n";
        let want = vec!["3000".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns <= 2, "the summary must keep the loop in place, got {reowns}");
    }

    /// A function that RETURNS its parameter (may_alias_out) still kills:
    /// the bound result whole-aliases the buffer, so the next push copies —
    /// and the alias keeps its snapshot.
    #[test]
    fn analysis_alias_returning_call_still_kills() {
        let src = "fn same(xs: List(Int)) -> List(Int):\n    xs\n\nfn main(console: Console):\n    var xs = [1]\n    var i = 0\n    while i < 100:\n        xs = list.push(xs, i)\n        i = i + 1\n    let held = same(xs)\n    xs = list.push(xs, 999)\n    print(console, __render(list.length(held)))\n    print(console, __render(list.length(xs)))\n";
        let want = vec!["101".to_string(), "102".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, _) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
    }

    /// DIRTY SITES: a self-assign whose RHS embeds the variable (`s = s + s`,
    /// a pushed snapshot stored into a dict) runs through the copying path
    /// and stays value-semantic on both backends.
    #[test]
    fn analysis_dirty_shapes_stay_value_semantic() {
        let src = "fn main(console: Console):\n    var s = \"ab\"\n    var k = 0\n    while k < 5:\n        s = s + s\n        k = k + 1\n    print(console, __render(string.length(s)))\n    var d = dict.new()\n    var zs = [1]\n    d = dict.insert(d, \"snap\", zs)\n    zs = list.push(zs, 2)\n    print(console, __render(list.length(dict.get_or(d, \"snap\", []))))\n    print(console, __render(list.length(zs)))\n";
        let want: Vec<String> = ["64", "1", "2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// A lambda body is its own analysis unit: an accumulator inside one gets
    /// its own ownership token (this used to emit an undeclared `__cap`
    /// local — a loud compile failure).
    #[test]
    fn analysis_lambda_accumulator_compiles() {
        let src = "fn main(console: Console):\n    let build = fn(n: Int):\n        var acc = [0]\n        var t = 0\n        while t < n:\n            acc = list.push(acc, t)\n            t = t + 1\n        list.length(acc)\n    print(console, __render(build(1000)))\n";
        let want = vec!["1001".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `derive(Show, Eq, Ord)`: compiler-generated impls, byte-identical in
    /// behavior to handwritten ones on both backends, additive-only (the
    /// expansion appends impls before checking; footprint analysis covers
    /// the expanded program).
    #[test]
    fn derive_show_eq_ord_generates_working_impls() {
        let src = "import show\nimport cmp\nimport list\n\ntype Point derive(Show, PartialEq, Eq, PartialOrd, Ord):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let a = Point(1, 2)\n    let b = Point(1, 3)\n    say(console, a)\n    print(console, \"${eq(a, Point(1, 2))} ${eq(a, b)}\")\n    print(console, \"${less(a, b)} ${less(b, a)}\")\n    print(console, \"${list.contains([a, b], Point(1, 3))}\")\n";
        let want: Vec<String> = ["Point(1, 2)", "true false", "true false", "true"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // A derive now routes to a user generator `derive_<name>`; with none in
        // scope it's a loud error at comptime (the generated call can't resolve).
        let bad = "type T derive(Serialize):\n    n: Int\n\nfn main(console: Console):\n    print(console, \"x\")\n";
        let res = crate::pipeline::link(
            vec![("main".to_string(), parser::parse_module(bad).expect("parse"))],
            "main",
        );
        let err = format!("{:?}", res.expect_err("missing derive generator must be rejected"));
        assert!(err.to_lowercase().contains("serialize"), "got: {err}");
    }

    #[test]
    fn derive_ord_on_generic_record() {
        // derive(Ord) on a GENERIC record: the generated impl and the Ord trait's
        // default methods (`greater`/`less`, used by `cmp.max_of`) must be typed
        // against the applied `Pair(a, b)`, not the bare head `Pair` — otherwise a
        // real `Pair(Int, Int)` clashes with the method's `other: Self`. Both
        // backends agree. (Regression for the bare-head `Self` substitution.)
        let src = "import cmp\n\ntype Pair(a, b) derive(PartialEq, Eq, PartialOrd, Ord):\n    first: a\n    second: b\n\nfn main(console: Console):\n    let m = cmp.max_of(Pair(1, 9), Pair(1, 4))\n    print(console, \"${m.first} ${m.second}\")\n    print(console, \"${less(Pair(1, 2), Pair(1, 3))} ${less(Pair(2, 0), Pair(1, 9))}\")\n";
        let want: Vec<String> = ["1 9", "true false"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `comptime:` — compile-time item generation: zero capabilities
    /// reachable (deterministic by construction), `emit(line)` as the
    /// channel, output parsed as ADDITIVE items before checking — so the
    /// generated functions exist on both backends and in the footprint.
    #[test]
    fn comptime_blocks_generate_items_additively() {
        let src = "comptime:\n    var i = 0\n    while i < 3:\n        emit(\"pub fn lucky_${i}() -> Int:\")\n        emit(\"    ${i * 7}\")\n        emit(\"\")\n        i = i + 1\n\nfn main(console: Console):\n    print(console, \"${lucky_0()} ${lucky_1()} ${lucky_2()}\")\n";
        let want = vec!["0 7 14".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // Emitted garbage is a loud error carrying the emitted source.
        let bad = "comptime:\n    emit(\"fn (((\")\n\nfn main(console: Console):\n    print(console, \"x\")\n";
        let module = parser::parse_module(bad).expect("parse");
        let err = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect_err("bad emission must be loud");
        assert!(err.to_string().contains("does not parse"), "got: {err}");
    }

    /// A `derive` (comptime) in a module that ALSO imports a project-local sibling
    /// must still link. The comptime program runs in the isolated, std-only
    /// `comptime` link, so project-local imports are filtered out of it (a comptime
    /// is a capability-free, link-time eval that cannot use sibling runtime code
    /// anyway). Regression for `comptime block: imports unknown module <sibling>`,
    /// which made `derive` unusable in any multi-module rune (e.g. its test module).
    #[test]
    fn derive_links_alongside_a_project_local_import() {
        let sibling = parser::parse_module("pub fn helper() -> Int:\n    7\n").expect("parse sibling");
        let main = parser::parse_module(
            "import sibling\nimport json\nimport result\n\ntype Foo derive(Deserialize):\n    x: Int\n\nfn main(console: Console):\n    print(console, \"${helper()}\")\n",
        )
        .expect("parse main");
        let linked = crate::pipeline::link(
            vec![("sibling".into(), sibling), ("main".into(), main)],
            "main",
        )
        .expect("a derive must link in a module that also imports a project-local sibling");
        crate::typeck::check(&linked).expect("typecheck");
        let out = interpreter::run_module(linked, ".", Vec::new()).expect("run");
        assert_eq!(out, vec!["7".to_string()]);
    }

    /// Tuple patterns in `for` (the learning log's F4): `for (k, v) in
    /// dict.pairs(d):` destructures per element, round-trips through fmt,
    /// and agrees on both backends.
    #[test]
    fn for_tuple_patterns_destructure() {
        // Both the parenthesized and the unparenthesized (canonical, Python-style)
        // tuple patterns parse and run identically on both backends.
        let head = "fn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, \"a\", 1)\n    d = dict.insert(d, \"b\", 2)\n";
        let paren = format!("{head}    for (k, v) in dict.pairs(d):\n        print(console, \"${{k}}=${{v}}\")\n");
        let unparen = format!("{head}    for k, v in dict.pairs(d):\n        print(console, \"${{k}}=${{v}}\")\n");
        let want: Vec<String> = ["a=1", "b=2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(&paren), want, "interpreter (paren)");
        assert_eq!(wasm_run(&paren), want, "wasm (paren)");
        assert_eq!(link_run(&unparen), want, "interpreter (unparen)");
        assert_eq!(wasm_run(&unparen), want, "wasm (unparen)");
        // fmt canonicalizes to the unparenthesized form, which round-trips.
        assert_eq!(
            crate::format::reformat(&paren).as_deref(),
            Some(unparen.as_str()),
            "paren form canonicalizes to unparenthesized"
        );
        assert_eq!(
            crate::format::reformat(&unparen).as_deref(),
            Some(unparen.as_str()),
            "unparenthesized form round-trips through fmt"
        );
    }

    /// `return X if cond` — a postfix-guard return, sugar for `if cond: return X`.
    /// It round-trips through fmt (the parser tags the desugared block with the
    /// synthetic-line marker so the formatter re-collapses exactly this shape),
    /// while an explicitly written multi-line `if cond: return X` is left untouched.
    /// Runs identically on both backends.
    #[test]
    fn postfix_guard_return() {
        let src = "fn classify(n: Int) -> String:\n    return \"neg\" if n < 0\n    return \"zero\" if n == 0\n    \"pos\"\n\nfn main(console: Console):\n    print(console, classify(-5))\n    print(console, classify(0))\n    print(console, classify(7))\n";
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

    /// GENERIC IMPLS COMPOSE: reflection now reaches `List`, `Option`, tuples, and
    /// generic records through ordinary `impl Reflect for List(a)` etc. — a generic
    /// consumer (`json.stringify`, `where a: Reflect`) calling a generic impl method
    /// monomorphizes per element. No builtins; identical on both backends.
    #[test]
    fn reflection_covers_lists_options_tuples_and_generic_records() {
        let src = "import json\nimport reflect\n\ntype Box(a) derive(Reflect):\n    item: a\n\nfn main(console: Console):\n    print(console, json.stringify([1, 2, 3]))\n    print(console, json.stringify(Some(\"x\")))\n    print(console, json.stringify((\"p\", 5)))\n    print(console, json.stringify([(\"a\", \"b\")]))\n    print(console, json.stringify(Box([1, 2])))\n";
        let want: Vec<String> = ["[1,2,3]", "\"x\"", "[\"p\",5]", "[[\"a\",\"b\"]]", "{\"item\":[1,2]}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// FROM / INTO (std/convert): a user implements `From` and gets `Into` free via
    /// the blanket `impl Into(b) for a where b: From(a)`. The blanket body calls the
    /// STATIC `b.from(self)` on the bound target type (no receiver), resolved through
    /// the bound at monomorphization. Both backends.
    #[test]
    fn from_into_conversion_traits() {
        let src = "import convert\n\ntype Celsius:\n    deg: Int\n\nimpl From(Int) for Celsius:\n    fn from(value: Int) -> Celsius:\n        Celsius(value)\n\nfn main(console: Console):\n    let c: Celsius = (5).into()\n    let d = Celsius.from(9)\n    print(console, \"${c.deg} ${d.deg}\")\n";
        let want = vec!["5 9".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `From`/`Into` reach `Json`: `impl From(a) for Json where a: Reflect` means any
    /// reflectable value converts — `x.into()` / `Json.from(x)` — and `server.send`
    /// serializes any reflectable response. Both backends.
    #[test]
    fn into_json_via_from() {
        let src = "import json\n\nfn main(console: Console):\n    let j: Json = [1, 2, 3].into()\n    print(console, json.encode(j))\n    print(console, json.encode(Json.from((\"x\", 5))))\n";
        let want = vec!["[1,2,3]".to_string(), "[\"x\",5]".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// ANONYMOUS STRUCTS: `.{ field: expr, … }` is an ad-hoc reflectable record (a
    /// generic synthetic type carrying `derive(Reflect)`), so `json.stringify(.{…})`
    /// works on any field types — including a `List` of tuples — with no per-type
    /// boilerplate. Fields render in sorted order; `.{…}` round-trips through fmt.
    #[test]
    fn anonymous_structs_reflect_to_json() {
        let src = "import json\n\nfn main(console: Console):\n    let files = [(\"a\", \"x\"), (\"b\", \"y\")]\n    print(console, json.stringify(.{files: files}))\n    print(console, json.stringify(.{name: \"acme\", count: 5}))\n";
        let want: Vec<String> = [
            "{\"files\":[[\"a\",\"x\"],[\"b\",\"y\"]]}",
            "{\"count\":5,\"name\":\"acme\"}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        assert!(
            crate::format::reformat(src).unwrap().contains(".{files: files}"),
            "`.{{…}}` round-trips through fmt"
        );
    }

    /// REFLECTION: `json.stringify(x)` encodes ANY value with no `derive(Json)` —
    /// only `derive(Reflect)`, the one generated impl every reflective library
    /// consumes. Covers scalars, nested records, `List`, and `Option` (Some/None),
    /// identical on both backends (the generated `reflect` is ordinary witchy code).
    #[test]
    fn reflective_json_encode_without_derive() {
        let src = "import json\nimport reflect\n\ntype Point derive(Reflect):\n    x: Int\n    y: Int\n\ntype Line derive(Reflect):\n    head: Point\n    tail: Point\n    tags: List(String)\n    note: Option(String)\n\nfn main(console: Console):\n    print(console, json.stringify(Point(1, 2)))\n    print(console, json.stringify(Line(Point(0, 0), Point(3, 4), [\"a\", \"b\"], Some(\"hi\"))))\n    print(console, json.stringify(Line(Point(5, 6), Point(7, 8), [], None)))\n";
        let want: Vec<String> = [
            "{\"x\":1,\"y\":2}",
            "{\"head\":{\"x\":0,\"y\":0},\"tail\":{\"x\":3,\"y\":4},\"tags\":[\"a\",\"b\"],\"note\":\"hi\"}",
            "{\"head\":{\"x\":5,\"y\":6},\"tail\":{\"x\":7,\"y\":8},\"tags\":[],\"note\":null}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // The generated `reflect` makes BARE trait calls, so the interpreter path
        // also needs std/reflect linked (link_run's single-module typeck can't see
        // it) — resolve std for both backends, like the real run path does.
        // The generated `reflect` makes trait calls that need std/reflect linked,
        // so resolve std for the interpreter path too (link_run's single-module
        // typeck can't see it); the real `witchy run` path resolves std the same way.
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// RECURSIVE-TYPE REFLECTION: a self-referential type (`Tree` with a `List(Tree)`
    /// arm) reflects + serializes without the monomorphizer overflowing, and an
    /// already-built `Json` reflects to its own value so it embeds verbatim inside an
    /// anonymous struct. Both backends. (The compiler's scope-name encoder is depth-
    /// guarded so recursive-type reflection can never stack-overflow the compiler.)
    #[test]
    fn recursive_types_and_json_reflect() {
        let tree = "import json\nimport reflect\n\ntype Tree derive(Reflect):\n    Leaf(Int)\n    Node(List(Tree))\n\nfn main(console: Console):\n    print(console, json.stringify(Node([Leaf(1), Node([Leaf(2)])])))\n";
        let tw = vec!["{\"$variant\":\"Node\",\"$values\":[[{\"$variant\":\"Leaf\",\"$values\":[1]},{\"$variant\":\"Node\",\"$values\":[[{\"$variant\":\"Leaf\",\"$values\":[2]}]]}]]}".to_string()];
        assert_eq!(link_run(tree), tw, "interpreter (tree)");
        assert_eq!(wasm_run(tree), tw, "wasm (tree)");
        // An already-built Json embeds verbatim in an anonymous struct.
        let embed = "import json\n\nfn main(console: Console):\n    let rec: Json = json.decode(\"{\\\"a\\\":1}\").unwrap_or(JsonNull)\n    print(console, json.stringify(.{record: rec, ok: true}))\n";
        let ew = vec!["{\"ok\":true,\"record\":{\"a\":1}}".to_string()];
        assert_eq!(link_run(embed), ew, "interpreter (embed)");
        assert_eq!(wasm_run(embed), ew, "wasm (embed)");
    }

    /// `||` is the truthy fallback `a || b` ≡ `if truthy(a): a else: b` over the
    /// emptyable built-ins: "" / None / [] are falsy, Bool stays logical-or, and the
    /// operator chains. Both backends must agree — the wasm path reads a single
    /// header word (length for String/List, variant tag for Option) where the
    /// interpreter checks the runtime value, so this guards that they stay in sync.
    #[test]
    fn or_truthy_fallback_both_backends() {
        let src = "fn main(console: Console):\n    print(console, \"\" || \"fallback\")\n    print(console, \"set\" || \"keep\")\n    match None || Some(\"x\"):\n        Some(v) -> print(console, v)\n        None -> print(console, \"none\")\n    match Some(\"y\") || Some(\"z\"):\n        Some(v) -> print(console, v)\n        None -> print(console, \"none\")\n    print(console, list.at([] || [\"A\"], 0))\n    print(console, list.at([\"B\"] || [\"C\"], 0))\n    print(console, \"${false || true}\")\n    let chain = \"\" || \"\" || \"third\"\n    print(console, chain)\n";
        let want: Vec<String> = ["fallback", "set", "x", "y", "A", "B", "true", "third"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// REFLECTION, SECOND USE CASE: `reflect.debug(x)` renders any value from the
    /// SAME `reflect` that powers `json` — proving the engine is general, not a
    /// json-specific hack. Records, lists-in-fields, and scalars, both backends.
    #[test]
    fn reflective_debug_render_other_use_case() {
        let src = "import reflect\n\ntype Point derive(Reflect):\n    x: Int\n    y: Int\n\ntype Bag derive(Reflect):\n    items: List(Int)\n    label: String\n\nfn main(console: Console):\n    print(console, reflect.debug(Point(1, 2)))\n    print(console, reflect.debug(Bag([1, 2, 3], \"nums\")))\n    print(console, reflect.debug(42))\n";
        let want: Vec<String> = [
            "Point { x: 1, y: 2 }",
            "Bag { items: [1, 2, 3], label: \"nums\" }",
            "42",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// COMPTIME REFLECTION (typeInfo, Phase 1 / Path 2a): a `comptime:` block reads
    /// its module's type structure via `module_types` and GENERATES a specialized
    /// `to_json` per record — direct field access, no runtime `Mirror`, written in
    /// pure witchy. This is Zig-style comptime-over-types proven end-to-end, both
    /// backends (comptime runs at link time, so the generated code is identical).
    #[test]
    fn comptime_typeinfo_generates_specialized_to_json() {
        let src = "import meta\nimport json\nimport string\n\ntype Point:\n    x: Int\n    y: Int\n\ntype User:\n    name: String\n    age: Int\n    active: Bool\n\ncomptime:\n    let ctor = fn(ty: String) -> String:\n        if ty == \"Int\": \"JsonInt\"\n        else if ty == \"String\": \"JsonString\"\n        else if ty == \"Bool\": \"JsonBool\"\n        else: \"JsonNull\"\n    for t in module_types:\n        if t.kind == \"record\":\n            emit(\"fn to_json_${t.name}(v: ${t.name}) -> Json:\")\n            var pairs = []\n            for f in t.fields:\n                pairs = list.push(pairs, \"(\\\"\" + f.name + \"\\\", \" + ctor(f.type_name) + \"(v.\" + f.name + \"))\")\n            emit(\"    JsonObject([\" + list.join(pairs, \", \") + \"])\")\n            emit(\"\")\n\nfn main(console: Console):\n    print(console, json.encode(to_json_Point(Point(1, 2))))\n    print(console, json.encode(to_json_User(User(\"ann\", 30, true))))\n";
        let want: Vec<String> = [
            "{\"x\":1,\"y\":2}",
            "{\"name\":\"ann\",\"age\":30,\"active\":true}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// VALUE EQUALITY, ALWAYS (the learning log's F15): dict lookups with
    /// RUNTIME-BUILT keys (trim/split/concat-sourced) — the case literal-key
    /// tests pass vacuously through interning. dict.get/has must find them
    /// by CONTENT on both backends; the compiled tier used to silently
    /// pointer-compare and return None.
    #[test]
    fn runtime_built_dict_keys_compare_by_content() {
        let src = "import dict\nimport string\n\nfn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, string.trim(\"  host  \"), \"localhost\")\n    let parts = string.split(\"port=8080\", \"=\")\n    d = dict.insert(d, list.at(parts, 0), list.at(parts, 1))\n    d = dict.insert(d, \"lit\" + \"eral\", \"joined\")\n    match dict.get(d, \"host\"):\n        Some(v) -> print(console, \"host=\" + v)\n        None -> print(console, \"host MISSING\")\n    match dict.get(d, \"port\"):\n        Some(v) -> print(console, \"port=\" + v)\n        None -> print(console, \"port MISSING\")\n    print(console, \"${dict.contains_key(d, \"literal\")}\")\n    print(console, \"${dict.length(d)}\")\n";
        let want: Vec<String> = ["host=localhost", "port=8080", "true", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Generic stdlib functions over USER RECORD types compare by content:
    /// typed lowering resolves the type argument (confirmed via the table),
    /// the specialization's `==` becomes structural. Previously the generic
    /// fallback pointer-compared (or, post-hotfix, refused to compile).
    #[test]
    fn generic_equality_on_records_is_structural() {
        let src = "import list\n\ntype Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let pts = [Point(1, 2), Point(3, 4)]\n    let probe = Point(1 + 2, 4)\n    print(console, \"${list.contains(pts, probe)}\")\n    print(console, \"${list.index_of(pts, Point(1, 2))}\")\n";
        let want: Vec<String> = ["true", "0"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// THE F11 FAMILY (learning log): interpolating values whose type only
    /// typed lowering knows — an ADT String payload and a generic-combinator
    /// return — renders identically on both backends.
    #[test]
    fn interpolation_of_mono_typed_values_agrees() {
        let src = "import iter\n\ntype Msg:\n    Text(String)\n    Silence\n\nfn main(console: Console):\n    match Text(\"hi\"):\n        Text(s) -> print(console, \"got: ${s}\")\n        Silence -> print(console, \"none\")\n    let collected: List(Int) = iter.collect(iter.take(iter.range(1, 100), 3))\n    print(console, \"collected: ${collected}\")\n";
        let want: Vec<String> = ["got: hi", "collected: [1, 2, 3]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `say` covers every scalar out of the box (Duration in its HUMAN form
    /// — the custom rendering `Show` exists for), and a missing impl is a
    /// clean check-time error naming the trait and type, not a post-lowering
    /// "unknown function" artifact.
    #[test]
    fn show_scalars_and_missing_impl_diagnostic() {
        let src = "import show\n\nfn main(console: Console):\n    say(console, 42)\n    say(console, 3.5)\n    say(console, 90s)\n    say(console, true)\n";
        let want: Vec<String> =
            ["42", "3.5", "1m30s", "true"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        let missing = "import show\n\ntype Blob:\n    n: Int\n\nfn main(console: Console):\n    say(console, Blob(1))\n";
        let module = parser::parse_module(missing).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("missing impl must be rejected");
        assert!(
            err.to_string().contains("`Blob` does not implement `Show`"),
            "want a clean trait error, got: {err}"
        );
    }

    /// The formatter ROUND-TRIPS string interpolation (the lexer desugars it
    /// to a `<>` chain; `interpolation_sugar` prints it back), and
    /// canonicalizes a hand-written chain of the exact desugared shape into
    /// the idiom.
    #[test]
    fn fmt_round_trips_interpolation() {
        let src = "fn main(console: Console):\n    let n = 3\n    print(console, \"n is ${n}, doubled ${n * 2}\")\n    print(console, \"cost: \\$${n}\")\n";
        assert_eq!(crate::format::reformat(src).as_deref(), Some(src), "interpolation must round-trip");
        let chain = "fn main(console: Console):\n    let n = 3\n    print(console, \"n is \" + __render(n) + \"\")\n";
        let want = "fn main(console: Console):\n    let n = 3\n    print(console, \"n is ${n}\")\n";
        assert_eq!(
            crate::format::reformat(chain).as_deref(),
            Some(want),
            "the canonical chain shape prints as interpolation"
        );
    }

    /// THE OWN-ABI: `xs = grow(move xs, i)` is a linear pipeline — the
    /// ownership token crosses the call in both directions (an extra cap
    /// param and result), so a cross-function builder stays O(n). Without
    /// the transfer each call re-owned by copy: O(n²) — the reowns counter
    /// (not timing) is the proof. (The interpreter leg stays small: it
    /// clones at every call by design.)
    #[test]
    fn analysis_own_abi_pipelines_in_place() {
        let src = "fn grow(own xs: List(Int), n: Int) -> List(Int):\n    xs = list.push(xs, n)\n    xs\n\nfn main(console: Console):\n    var xs = [0]\n    var i = 0\n    while i < 3000:\n        xs = grow(move xs, i)\n        i = i + 1\n    print(console, __render(list.length(xs)))\n    print(console, __render(list.at(xs, 3000)))\n";
        let want = vec!["3001".to_string(), "2999".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns <= 2, "the token must survive the calls, got {reowns} re-owns");
    }

    /// An own-ABI callee that returns its parameter only on SOME paths: the
    /// other paths return a zero token (the caller re-owns later) — always
    /// correct, never corrupting.
    #[test]
    fn analysis_own_abi_partial_return_paths_are_sound() {
        let src = "fn cap_at(own xs: List(Int), n: Int) -> List(Int):\n    if list.length(xs) >= n:\n        []\n    else:\n        xs = list.push(xs, n)\n        xs\n\nfn main(console: Console):\n    var xs = [0]\n    var i = 0\n    while i < 50:\n        xs = cap_at(move xs, i)\n        i = i + 1\n    print(console, __render(xs))\n";
        let interp = link_run(src);
        assert_eq!(wasm_run(src), interp, "wasm must agree on the mixed paths");
    }

    /// THE FORCED-COPY DIFFERENTIAL: `WITCHY_OPT=-inplace` compiles with the
    /// in-place machinery off (the copying paths ARE the semantics). Outputs
    /// must be identical — any divergence is an analysis soundness bug.
    #[test]
    fn forced_copy_mode_is_differential() {
        let src = "fn tag(let prefix: String, n: Int) -> String:\n    prefix + __render(n)\n\nfn main(console: Console):\n    var xs = []\n    let alias = xs\n    var s = \"\"\n    var d = dict.new()\n    var i = 0\n    while i < 800:\n        xs = list.push(xs, i)\n        s = s + tag(\"x\", i)\n        d = dict.update(d, i % 7, 0, fn(n: Int): n + 1)\n        i = i + 1\n    print(console, __render(list.length(xs)))\n    print(console, __render(list.length(alias)))\n    print(console, __render(string.length(s)))\n    print(console, __render(dict.get_or(d, 3, 0)))\n";
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
        let src = "fn tag(let prefix: String, n: Int) -> String:\n    prefix + __render(n)\n\nfn main(console: Console):\n    var xs = []\n    let alias = xs\n    var s = \"\"\n    var d = dict.new()\n    var i = 0\n    while i < 600:\n        xs = list.push(xs, i)\n        s = s + tag(\"x\", i)\n        d = dict.update(d, i % 7, 0, fn(n: Int): n + 1)\n        i = i + 1\n    print(console, __render(list.length(xs)))\n    print(console, __render(list.length(alias)))\n    print(console, __render(string.length(s)))\n    print(console, __render(dict.get_or(d, 3, 0)))\n";
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

    /// (RFC-0035) LAST-USE DROP — observable + differential. A dead per-iteration
    /// scratch buffer (`list.concat` read exactly once, then dead) sits in a loop that
    /// is NOT arena-resettable: the dict `acc` escapes each iteration, so the RFC-0030
    /// watermark is OFF and the scratch would otherwise leak O(iterations). Under
    /// `rc-floor` the `last_use` analysis frees it right after its last use. Three
    /// obligations, all asserted: (1) output IDENTICAL to the interpreter oracle and to
    /// the default build — the free is sound, never observable; (2) the free-list is
    /// actually recycled (`rc_reused_bytes > 0`) — the drop fired, it is not a no-op;
    /// (3) the heap frontier stays flat instead of growing with the leak — an order of
    /// magnitude below the default. This is exactly the niche the heap-reset-boundary
    /// guard (`wm_level == 0`) preserves: rc-floor reclaims where the watermark cannot,
    /// and cedes (never double-frees) where it can.
    #[test]
    fn rc_floor_last_use_drop_is_differential_and_bounds_the_leak() {
        use crate::opt::{self, Opt, OptSet};
        let src = "import list\nimport dict\nfn main(console: Console):\n    var acc = dict.new()\n    var i = 0\n    let base = [1, 2, 3, 4, 5]\n    while i < 2000:\n        let scratch = list.concat(base, base)\n        let n = list.length(scratch)\n        acc = dict.insert(acc, i % 8, n)\n        i = i + 1\n    print(console, __render(dict.length(acc)))\n";
        let oracle = link_run(src);

        // rc-floor OFF (explicit — it is default-on now): correct, but the scratch leaks each iteration.
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::RcFloor)));
        let (default_out, default_heap, _default_reused) = wasm_run_heap(src);
        opt::set_for_tests(None);
        assert_eq!(default_out, oracle, "default build diverged from the interpreter oracle");

        // rc-floor ON: identical output, heap bounded, free-list recycled.
        opt::set_for_tests(Some(OptSet::default_set().with(Opt::RcFloor)));
        let (rc_out, rc_heap, rc_reused) = wasm_run_heap(src);
        opt::set_for_tests(None);
        assert_eq!(rc_out, oracle, "rc-floor diverged from the interpreter oracle");
        assert_eq!(rc_out, default_out, "rc-floor changed observable output — unsound");
        assert!(rc_reused > 0, "rc-floor never recycled: the last_use drop did not fire (reused={rc_reused})");
        assert!(
            rc_heap.saturating_mul(10) < default_heap,
            "rc-floor did not bound the leak: rc_heap={rc_heap} default_heap={default_heap}"
        );
    }

    /// (RFC-0035) Assert an adversarial-aliasing program computes `expected` IDENTICALLY on
    /// the interpreter oracle, the compiled default build, AND the compiled build with
    /// `rc-floor` on. This is the **use-after-free corpus gate** the per-object refcount
    /// (the remaining floor: `$drop` at a `set_at` overwrite) must keep green: an element
    /// still aliased — read into a live binding, duplicated, or stored elsewhere — when its
    /// container slot is overwritten must NOT be reclaimed. The programs pass today (nothing
    /// frees the displaced element); when the refcount lands, its `$drop` must decrement to a
    /// still-positive count (a live alias holds it) and free NOTHING here. A regression flips
    /// these red — the corpus is authored FIRST, as the gate, per the goal + RFC-0035 step 3.
    fn assert_rc_corpus_stable(src: &str, expected: &[&str]) {
        use crate::opt::{self, Opt, OptSet};
        let want: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter oracle diverged");
        assert_eq!(wasm_run(src), want, "compiled default diverged from oracle");
        opt::set_for_tests(Some(OptSet::default_set().with(Opt::RcFloor)));
        let rc = wasm_run(src);
        opt::set_for_tests(None);
        assert_eq!(rc, want, "compiled under rc-floor diverged — a premature free");
    }

    /// Corpus 1: an element read into a binding that stays live PAST the `set_at` that
    /// overwrites its slot. `held` must still observe the original element.
    #[test]
    fn rc_corpus_element_read_lives_past_set_at() {
        let src = "import list\ntype Box:\n    Box(String)\nfn unwrap(b: Box) -> String:\n    match b:\n        Box(s) -> s\nfn main(console: Console):\n    var xs = [Box(\"a\"), Box(\"b\"), Box(\"c\")]\n    let held = list.at(xs, 1)\n    xs = list.set_at(xs, 1, Box(\"z\"))\n    print(console, unwrap(held))\n    print(console, unwrap(list.at(xs, 1)))\n";
        assert_rc_corpus_stable(src, &["b", "z"]);
    }

    /// Corpus 2: the SAME element aliased into two live bindings, then the container slot
    /// overwritten. Both aliases must survive (count ≥ 2 at the overwrite).
    #[test]
    fn rc_corpus_aliased_element_survives_container_mutation() {
        let src = "import list\ntype Box:\n    Box(String)\nfn unwrap(b: Box) -> String:\n    match b:\n        Box(s) -> s\nfn main(console: Console):\n    var xs = [Box(\"a\"), Box(\"b\")]\n    let a1 = list.at(xs, 0)\n    let a2 = list.at(xs, 0)\n    xs = list.set_at(xs, 0, Box(\"z\"))\n    print(console, unwrap(a1))\n    print(console, unwrap(a2))\n    print(console, unwrap(list.at(xs, 0)))\n";
        assert_rc_corpus_stable(src, &["a", "a", "z"]);
    }

    /// Corpus 3: an element STORED into another container (the same shape as returning it or
    /// sending it down a channel — it escapes to a place that outlives the overwrite), then
    /// the original container slot overwritten. The stored copy must survive.
    #[test]
    fn rc_corpus_element_stored_elsewhere_survives_container_mutation() {
        let src = "import list\ntype Box:\n    Box(String)\nfn unwrap(b: Box) -> String:\n    match b:\n        Box(s) -> s\nfn main(console: Console):\n    var xs = [Box(\"a\"), Box(\"b\")]\n    var ys = []\n    ys = list.push(ys, list.at(xs, 0))\n    xs = list.set_at(xs, 0, Box(\"z\"))\n    print(console, unwrap(list.at(ys, 0)))\n    print(console, unwrap(list.at(xs, 0)))\n";
        assert_rc_corpus_stable(src, &["a", "z"]);
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

    /// Matrix: a HEAP String element (built via `${…}` interpolation, so it is a real
    /// $rc_alloc'd cell, not a static literal) read into a binding that outlives the set_at.
    #[test]
    fn rc_corpus_heap_string_element_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [\"v${i}\", \"v${i + 1}\", \"v${i + 2}\"]\n    let held = list.at(xs, 1)\n    xs = list.set_at(xs, 1, \"v${i + 9}\")\n    print(console, held)\n    print(console, list.at(xs, 1))\n";
        assert_rc_corpus_stable(src, &["v2", "v10"]);
    }

    /// Matrix: a LIST element (`List(List(Int))`, runtime-built so each inner list is a heap
    /// cell) read into a binding that outlives the set_at.
    #[test]
    fn rc_corpus_list_element_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [[i, i + 1], [i + 2, i + 3], [i + 4, i + 5]]\n    let held = list.at(xs, 1)\n    xs = list.set_at(xs, 1, [9, 9])\n    print(console, __render(held))\n    print(console, __render(list.at(xs, 1)))\n";
        assert_rc_corpus_stable(src, &["[3, 4]", "[9, 9]"]);
    }

    /// Matrix: a TUPLE element (`List((Int, Int))`) read into a binding that outlives the set_at.
    #[test]
    fn rc_corpus_tuple_element_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [(i, i + 1), (i + 2, i + 3), (i + 4, i + 5)]\n    let held = list.at(xs, 1)\n    xs = list.set_at(xs, 1, (9, 9))\n    print(console, __render(held))\n    print(console, __render(list.at(xs, 1)))\n";
        assert_rc_corpus_stable(src, &["(3, 4)", "(9, 9)"]);
    }

    /// Matrix: a DICT element (`List(Dict)`) — the specific case the revert trapped on, because
    /// a dict pointer is `rc_res + 4` (the hidden index word), so its rc header sits at a
    /// DIFFERENT negative offset than a plain record. Read into a binding that outlives the set_at.
    #[test]
    fn rc_corpus_dict_element_survives_set_at() {
        let src = "import list\nimport dict\nfn mkd(v: Int) -> Dict(String, Int):\n    var d = dict.new()\n    d = dict.insert(d, \"k\", v)\n    d\nfn main(console: Console):\n    var xs = [mkd(1), mkd(2), mkd(3)]\n    let held = list.at(xs, 1)\n    xs = list.set_at(xs, 1, mkd(9))\n    print(console, __render(dict.get(held, \"k\")))\n    print(console, __render(dict.get(list.at(xs, 1), \"k\")))\n";
        assert_rc_corpus_stable(src, &["Some(2)", "Some(9)"]);
    }

    /// Matrix: the SAME heap-String element aliased into two live bindings, then the slot
    /// overwritten. Both aliases must survive (refcount ≥ 2 at the displaced drop).
    #[test]
    fn rc_corpus_aliased_heap_string_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [\"v${i}\", \"v${i + 1}\"]\n    let a1 = list.at(xs, 0)\n    let a2 = list.at(xs, 0)\n    xs = list.set_at(xs, 0, \"v${i + 9}\")\n    print(console, a1)\n    print(console, a2)\n    print(console, list.at(xs, 0))\n";
        assert_rc_corpus_stable(src, &["v1", "v1", "v10"]);
    }

    /// Matrix: MATCH-ON-READ of an ADT with a heap payload — the executor's actual shape
    /// (`match list.at(slots, i): Active(task) -> …`). The scrutinee is a dup'd read temp
    /// (not a let-binding); its heap payload is extracted into `r`, which must survive the set_at.
    #[test]
    fn rc_corpus_match_on_read_adt_payload_survives_set_at() {
        let src = "import list\ntype W:\n    W(String)\nfn unwrap(w: W) -> String:\n    match w:\n        W(s) -> s\nfn main(console: Console):\n    var i = 1\n    var ws = [W(\"v${i}\"), W(\"v${i + 1}\"), W(\"v${i + 2}\")]\n    let r = match list.at(ws, 1):\n        W(s) -> s\n    ws = list.set_at(ws, 1, W(\"v${i + 9}\"))\n    print(console, r)\n    print(console, unwrap(list.at(ws, 1)))\n";
        assert_rc_corpus_stable(src, &["v2", "v10"]);
    }

    /// Matrix: a heap-String element STORED into another container (escapes past the set_at,
    /// same shape as returning it or sending it down a channel). The stored copy must survive.
    #[test]
    fn rc_corpus_heap_string_element_stored_elsewhere_survives() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [\"v${i}\", \"v${i + 1}\"]\n    var ys = []\n    ys = list.push(ys, list.at(xs, 0))\n    xs = list.set_at(xs, 0, \"v${i + 9}\")\n    print(console, list.at(ys, 0))\n    print(console, list.at(xs, 0))\n";
        assert_rc_corpus_stable(src, &["v1", "v10"]);
    }

    /// Matrix (executor): the async channel path — a spawned producer sends N ints over a bounded
    /// channel and the consumer drains them (chan_throughput's shape, N=100 for a fast test). This
    /// is THE residual the RC floor must bound: the cooperative executor does not reset its arena
    /// per scheduling step, so the per-message garbage (the displaced Slot / continuation closure)
    /// leaks. With emission off this proves the executor path stays byte-identical under rc-floor;
    /// when the dup/drop lands, a wrong dec here traps or diverges — it did, at ~8k, in 5e9e167,
    /// which the record-only corpus + fuzzer MISSED. This is why the executor is in the gate.
    #[test]
    fn rc_corpus_channel_executor_is_stable() {
        let src = "import chan\nasync fn producer(tx: Sender(Int), n: Int) -> Nil:\n    for i in 0..n:\n        chan.send(tx, i).await\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(8).await\n    chan.spawn(producer(tx, 100)).await\n    for await v in rx:\n        chan.done(v)\n    print(console, \"100\")\n";
        assert_rc_corpus_stable(src, &["100"]);
    }

    /// Matrix: NESTED match-on-read — `match list.at(xs,0): W(s1) -> match list.at(ys,0): Box(s2)
    /// -> …`. Both scrutinees are dup'd reads that must drop after their arms; the shared MATCH_TMP
    /// is clobbered by the inner match, so this exercises the per-depth `__witchy_scrut_save` pool.
    /// Both displaced elements must survive through their bindings.
    #[test]
    fn rc_corpus_nested_match_on_read_uses_the_scrut_pool() {
        let src = "import list\ntype W:\n    W(String)\ntype Box:\n    Box(String)\nfn main(console: Console):\n    var i = 1\n    var xs = [W(\"a${i}\"), W(\"b${i}\")]\n    var ys = [Box(\"c${i}\"), Box(\"d${i}\")]\n    let r = match list.at(xs, 0):\n        W(s1) ->\n            match list.at(ys, 0):\n                Box(s2) -> s1 + s2\n    xs = list.set_at(xs, 0, W(\"z${i}\"))\n    ys = list.set_at(ys, 0, Box(\"q${i}\"))\n    print(console, r)\n";
        assert_rc_corpus_stable(src, &["a1c1"]);
    }

    /// Matrix: a NESTED `List(List(String))` element read into a binding, then the outer slot
    /// overwritten. The displaced inner list (and its heap strings) must survive via the binding —
    /// dup/drop on a List element whose own children are heap.
    #[test]
    fn rc_corpus_nested_list_element_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var ls = [[\"p${i}\", \"q${i}\"], [\"r${i}\"]]\n    let inner = list.at(ls, 0)\n    ls = list.set_at(ls, 0, [\"z${i}\"])\n    print(console, list.at(inner, 1))\n    print(console, list.at(list.at(ls, 0), 0))\n";
        assert_rc_corpus_stable(src, &["q1", "z1"]);
    }

    /// Regression: `toml.get_array` on a real manifest is sound under `WITCHY_OPT=rc-floor`.
    /// This once returned [] — a free-at-overwrite use-after-free in `std/string.last_index_of`
    /// (`var rest = s; rest = string.substring(rest, …)`): the first reassignment freed `rest`'s
    /// initial buffer, which ALIASED the borrowed param `s`, so the caller's string dangled and a
    /// later allocation (routed through the Phase-A free-list) overwrote it. Fixed by excluding
    /// alias-initialized vars from `escape::confined_reassigned_vars` — a var whose first buffer it
    /// does not own is never free-at-overwrite-reclaimed. (Not the dup/drop emission: bisected to the
    /// free-at-overwrite pass with every step-1..4 dup/drop disabled.)
    #[test]
    fn rc_floor_toml_get_array_is_sound() {
        let src = "import toml\nimport list\nfn main(console: Console):\n    let m = \"[capabilities]\\nruntime = [\\\"Console\\\", \\\"Dir[Read]\\\", \\\"Net[Connect]\\\"]\\n\"\n    let declared = toml.get_array(m, \"capabilities.runtime\")\n    print(console, list.join(declared, \",\"))\n";
        assert_rc_corpus_stable(src, &["Console,Dir[Read],Net[Connect]"]);
    }

    /// `crypto.rune_hash` produces the same store hash (`src/pm/store.rs`
    /// format) on both backends — the host walks the guest's string lists.
    #[test]
    fn crypto_rune_hash_runs_in_the_wasm_backend() {
        let prog = "import crypto\nfn main(console: Console):\n    print(console, crypto.rune_hash([\"a.witchy\", \"b.witchy\"], [\"fn one\", \"fn two\"]))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].starts_with("sha256:") && out[0].len() == 71, "{out:?}");
    }

    /// `compiler.footprint` runs in the WASM backend (staged-JSON host bridge)
    /// and agrees byte-for-byte with the interpreter — a self-hosted package
    /// manager can compute footprints from inside the sandbox.
    #[test]
    fn compiler_footprint_runs_in_the_wasm_backend() {
        let prog = "import compiler\nfn main(console: Console):\n    print(console, compiler.footprint(\"pub fn read_all(d: Dir[Read]) -> String:\\n    read(d, \\\"x\\\")\\n\"))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].contains("Dir[Read]"), "{out:?}");
    }

    /// `compiler.diff` runs in the WASM backend and flags widening exactly as
    /// the interpreter does.
    #[test]
    fn compiler_diff_runs_in_the_wasm_backend() {
        let prog = "import compiler\nfn main(console: Console):\n    let old = \"pub fn pure(x: Int) -> Int:\\n    x\\n\"\n    let new = \"pub fn pure(x: Int, d: Dir) -> Int:\\n    x\\n\"\n    print(console, compiler.diff(old, new))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].contains("\"widened\":true"), "{out:?}");
    }

    /// The full `std/http` client runs in the WASM backend: a real GET against
    /// a local server returns the same status and body on both backends.
    #[test]
    fn std_http_client_runs_in_the_wasm_backend() {
        use crate::runtime::{Capabilities, Runtime};
        use std::io::{BufRead, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        // One request per backend run: consume the request head, reply 200.
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = server.accept().expect("accept");
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).expect("read");
                    if line == "\r\n" || line == "\n" || line.is_empty() {
                        break;
                    }
                }
                reader
                    .get_mut()
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello")
                    .expect("write");
            }
        });

        let src = format!(
            "import http\nfn main(console: Console, net: Net):\n    let res = http.get(net, \"127.0.0.1\", {port}, \"/greet\")\n    print(console, f\"{{http.status(res)}} {{http.body(res)}}\")\n"
        );
        let want = vec!["200 hello".to_string()];
        let module = parser::parse_module(&src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec![addr.clone()]).expect("interp"),
            want,
            "interpreter"
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        handle.join().expect("server thread");
    }

    /// `http.try_get` is fallible: a dial to an ALLOWLISTED-but-closed port
    /// yields `Err(...)` rather than trapping — on BOTH backends. This is the
    /// primitive that lets a proxy answer 502 for a down upstream instead of
    /// aborting the VM. (A capability violation still traps; here the address is
    /// permitted, so only the transient dial failure path is exercised.) The
    /// closed port comes from binding then dropping a loopback listener, so the
    /// address is well-formed and reachable-to-refuse, not merely unroutable.
    #[test]
    fn http_try_get_returns_err_on_closed_port() {
        use crate::runtime::{Capabilities, Runtime};
        // Bind to grab a free loopback port, then drop the listener so a connect
        // is refused fast (RST) rather than hanging.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        drop(listener);

        let src = format!(
            "import http\nfn main(console: Console, net: Net):\n    match http.try_get(net, \"127.0.0.1\", {port}, \"/\"):\n        Ok(_) -> print(console, \"ok\")\n        Err(_) -> print(console, \"err\")\n"
        );
        let want = vec!["err".to_string()];
        let module = parser::parse_module(&src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec![addr.clone()]).expect("interp"),
            want,
            "interpreter must report Err for a closed-port dial"
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree: Err, not a trap");
    }

    /// Signing round-trips entirely in witchy: a host-granted `Secret`
    /// capability signs a message (`crypto.sign`), and `crypto.ed25519_verify`
    /// against the key's public half (`crypto.public_key`) accepts it. Without a
    /// granted key, a `Secret` parameter is refused, and the capability
    /// surfaces in the footprint.
    #[test]
    fn crypto_signing_round_trips_in_witchy() {
        let src = "import crypto\nfn main(console: Console, signer: Secret):\n    let msg = \"sign me\"\n    let sig = crypto.sign(signer, msg)\n    print(console, if crypto.ed25519_verify(crypto.public_key(signer), msg, sig): \"verified\" else: \"FAILED\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let out = interpreter::run_module_signed(linked, ".", Vec::new(), Vec::new(), Some([7u8; 32]))
            .expect("run");
        assert_eq!(out, vec!["verified"]);

        // A `Secret` parameter without a host-granted key is refused.
        let m2 = parser::parse_module("fn main(console: Console, s: Secret):\n    print(console, \"x\")\n").expect("parse");
        let l2 = crate::pipeline::link(vec![("main".into(), m2)], "main").expect("link");
        assert!(interpreter::run_module_signed(l2, ".", Vec::new(), Vec::new(), None).is_err());

        // The signing authority surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, s: Secret):\n    print(console, \"x\")\n").expect("parse"),
        );
        assert!(fp.total.contains_key("Secret"), "Secret should appear in the footprint");
    }

    /// `compiler.footprint` exposes witchy's own capability analyzer to witchy
    /// programs (the heart of a self-hosted package manager): it returns the
    /// rights-precise footprint as JSON, which composes with `std/json`.
    #[test]
    fn compiler_footprint_exposes_the_analyzer() {
        // The rights-precise footprint comes back as JSON.
        let out = link_run(
            "import compiler\nfn main(console: Console):\n    print(console, compiler.footprint(\"pub fn load(d: Dir[Read]) -> String:\\n    read(d, \\\"x\\\")\\n\"))\n",
        );
        assert!(out[0].contains("\"total\":[\"Dir[Read]\"]"), "total wrong: {}", out[0]);
        assert!(out[0].contains("\"name\":\"load\""), "entry missing: {}", out[0]);
        // The output is valid JSON — it round-trips through `std/json`.
        let composed = link_run(
            "import compiler\nimport json\nfn main(console: Console):\n    match json.decode(compiler.footprint(\"pub fn serve(n: Net) -> Int:\\n    0\\n\")):\n        Ok(doc) -> print(console, \"valid\")\n        Err(e) -> print(console, \"invalid: \" + e)\n",
        );
        assert_eq!(composed, vec!["valid"]);
        // Malformed source degrades to an error object, not a crash.
        let bad = link_run(
            "import compiler\nfn main(console: Console):\n    print(console, compiler.footprint(\"fn oops(\"))\n",
        );
        assert!(bad[0].contains("\"error\""), "expected an error object: {}", bad[0]);
    }

    /// `compiler.diff` is the rights-precise block-on-widening gate (the package
    /// manager's core safety check), exposed to witchy as JSON.
    #[test]
    fn compiler_diff_is_the_widening_gate() {
        let diff = |old: &str, new: &str| -> String {
            link_run(&format!(
                "import compiler\nfn main(console: Console):\n    print(console, compiler.diff(\"{old}\", \"{new}\"))\n"
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

    /// `std/toml` (pure witchy) reads `witchy.toml` manifests: `toml.get` for
    /// string values by `section.key`, `toml.get_array` for string arrays — what
    /// a self-hosted package manager needs to read a manifest.
    #[test]
    fn toml_module_reads_manifest_values() {
        let src = r#"import toml
import string

fn main(console: Console):
    let m = "[rune]\nname = \"acme/widget\"\nversion = \"1.2.0\"\n\n[capabilities]\nruntime = [\"Net\", \"Console\"]\n"
    print(console, opt(toml.get(m, "rune.name")))
    print(console, opt(toml.get(m, "rune.version")))
    print(console, list.join(toml.get_array(m, "capabilities.runtime"), "|"))
    print(console, opt(toml.get(m, "rune.absent")))

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        assert_eq!(
            link_run(src),
            vec!["acme/widget", "1.2.0", "Net|Console", "(none)"]
        );
    }

    /// `toml.decode` builds a structured `Toml` tree — top-level keys, `[section]`
    /// and dotted `[a.b]` tables, and typed string/int/bool/array values —
    /// identically on both backends.
    #[test]
    fn toml_decode_builds_typed_tree_on_both_backends() {
        let src = r#"import toml

fn main(console: Console):
    let doc = "title = \"demo\"\nport = 8080\nenabled = true\ntags = [\"a\", \"b\"]\n\n[server]\nhost = \"localhost\"\nworkers = 4\n\n[server.tls]\nenabled = false\n"
    match toml.decode(doc):
        Ok(t) -> print(console, "${t}")
        Err(e) -> print(console, e)
"#;
        let want = vec!["TomlTable([(title, TomlString(demo)), (port, TomlInt(8080)), (enabled, TomlBool(true)), (tags, TomlArray([TomlString(a), TomlString(b)])), (server, TomlTable([(host, TomlString(localhost)), (workers, TomlInt(4)), (tls, TomlTable([(enabled, TomlBool(false))]))]))])".to_string()];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Trailing `# comments` on values and arrays are stripped, but a `#` inside a
    /// quoted string and a `]` inside an array element (e.g. "Dir[Read]") are
    /// preserved — real manifests carry comments, so the reader must tolerate them.
    #[test]
    fn toml_module_ignores_trailing_comments() {
        let src = r#"import toml
import string

fn main(console: Console):
    let m = "[rune]\nname = \"acme/widget\"  # the canonical name\ntag = \"v#1\"  # has a hash inside\n\n[capabilities]\nruntime = [\"Console\", \"Dir[Read]\"]  # what it needs\n"
    print(console, opt(toml.get(m, "rune.name")))
    print(console, opt(toml.get(m, "rune.tag")))
    print(console, list.join(toml.get_array(m, "capabilities.runtime"), "|"))

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        assert_eq!(
            link_run(src),
            vec!["acme/widget", "v#1", "Console|Dir[Read]"]
        );
    }

    /// `toml.table`/`keys`/`inline_get` enumerate a table whose keys aren't known
    /// ahead of time (`[dependencies]`, whose values are inline tables), and
    /// `array_tables` walks a `[[rune]]` array-of-tables (a `witchy.lock`) — the
    /// manifest+lock shapes a self-hosted package manager reads but `get` cannot.
    #[test]
    fn toml_module_enumerates_tables_and_arrays() {
        let src = r#"import toml
import string

fn main(console: Console):
    let m = "[rune]\nname = \"ledger\"\n\n[dependencies]\n\"money\" = { path = \"../money\" }\n\"acme/util\" = { path = \"../util\", version = \"1.2\" }\n"
    print(console, list.join(toml.keys(m, "dependencies"), "|"))
    print(console, opt(toml.inline_get("{ path = \"../money\" }", "path")))
    print(console, opt(toml.inline_get("{ path = \"../util\", version = \"1.2\" }", "version")))
    let lock = "[[rune]]\nname = \"money\"\nhash = \"sha256:aa\"\n\n[[rune]]\nname = \"util\"\nhash = \"sha256:bb\"\n"
    var names = []
    for block in toml.array_tables(lock, "rune"):
        names = list.push(names, opt(toml.get(block, "name")) + "=" + opt(toml.get(block, "hash")))
    print(console, list.join(names, "|"))

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "money|acme/util",
                "../money",
                "1.2",
                "money=sha256:aa|util=sha256:bb"
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
    print(console, yes(req_matches("^1.2.0", "1.9.9")))
    print(console, yes(req_matches("^1.2.0", "2.0.0")))
    print(console, yes(req_matches("^0.4.0", "0.5.0")))
    print(console, yes(req_matches("~1.2.0", "1.2.9")))
    print(console, yes(req_matches("~1.2.0", "1.3.0")))
    print(console, yes(req_matches(">=1.0.0", "3.0.0")))
    print(console, best_of("^1.2.0"))

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

fn yes(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec!["y", "n", "n", "y", "n", "y", "1.9.9"]
        );
    }

    /// `std/path` does pure '/'-path surgery: base/dir/ext/stem, join (an absolute
    /// right-hand side replaces), and normalize (collapsing `.`/`..`, never
    /// escaping an absolute root, keeping leading `..` when relative).
    #[test]
    fn path_module_components_and_normalize() {
        let src = r#"import path

fn main(console: Console):
    print(console, path.base("a/b/c.txt") + "|" + path.dir("a/b/c.txt"))
    print(console, path.ext("a/b.tar.gz") + "|" + path.stem("a/b.tar.gz"))
    print(console, "[" + path.ext(".bashrc") + "]|" + path.base("a/b/"))
    print(console, path.join("a/b", "c") + "|" + path.join("a", "/x"))
    print(console, path.normalize("a/./b/../c/") + "|" + path.normalize("/a/b/../../../x"))
    print(console, path.normalize("../a/../../b"))
"#;
        assert_eq!(
            link_run(src),
            vec![
                "c.txt|a/b",
                "gz|b.tar",
                "[]|b",
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
    print(console, encoding.hex_encode("hello"))
    print(console, encoding.hex_decode("68656c6c6f"))
    print(console, encoding.base64_encode("Man"))
    print(console, encoding.base64_encode("Ma"))
    print(console, encoding.base64_decode("aGVsbG8="))
    print(console, yn(encoding.base64_decode(encoding.base64_encode("witchy! 🧙")) == "witchy! 🧙"))

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

    /// `examples/anagram/src/anagram.witchy` — groups words that are letter-rearrangements
    /// of each other by a sorted-character signature, bucketing with a parallel
    /// signatures/groups list (no Dict). Exercises sorting characters (string
    /// `<`) and signature equality (string `==`) on both backends.
    #[test]
    fn anagram_example_groups_by_sorted_signature() {
        assert_eq!(
            crate::execute_file("examples/anagram/src/anagram.witchy", Vec::new()).unwrap(),
            vec!["listen, silent, enlist", "cat, act, tac", "dog, god"]
        );
    }

    /// `examples/stats/src/stats.witchy` — summary statistics over a `List(Float)` —
    /// computes count/mean/variance/stddev/min/max, rendering with
    /// math.format_float. Floats live in the list and flow through arithmetic and
    /// sqrt; a guard that floats-in-collections + fixed-decimal formatting agree
    /// on both backends.
    #[test]
    fn stats_example_summarizes_a_float_list() {
        assert_eq!(
            crate::execute_file("examples/stats/src/stats.witchy", Vec::new()).unwrap(),
            vec![
                "count    8",
                "mean     5.00",
                "variance 4.00",
                "stddev   2.00",
                "min      2.00",
                "max      9.00",
            ]
        );
    }

    /// `examples/regex/src/regex.witchy` — a tiny K&P-style regex matcher (literals, `.`,
    /// `*`, `^`, `$`) — matches a battery of pattern/text pairs. Every step is a
    /// two-`list.at(..)` character comparison, so it stresses content comparison on
    /// both backends.
    #[test]
    fn regex_example_matches_literals_dot_star_anchors() {
        assert_eq!(
            crate::execute_file("examples/regex/src/regex.witchy", Vec::new()).unwrap(),
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

    /// `region:` Phase 1 (rfcs/regions.md): the syntax parses (with optional
    /// `-> T` ascription), the block's value escapes, scalar outer
    /// assignments are allowed, and both backends agree — a region NEVER
    /// changes observable behavior, only when memory is reclaimed.
    #[test]
    fn region_blocks_value_escape_and_parity() {
        let src = "import string\n\nfn main(console: Console):\n    let summary = region:\n        var parts = []\n        for i in 0..50:\n            parts = list.push(parts, __render(i))\n        list.join(parts, \",\")\n    print(console, __render(string.length(summary)))\n    var n = 0\n    let direct = region -> Int:\n        n = n + 42\n        n\n    print(console, __render(direct))\n";
        let want: Vec<String> = ["139", "42"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `region:` Phase 2: the copy-out handles every shape — string, record
    /// with a nested list, recursive generic ADT, dict (whose hidden index is
    /// dropped on the way out), nested regions — and parent-side values pass
    /// through shared, all agreeing with the interpreter.
    #[test]
    fn region_copy_out_shapes_agree_on_both_backends() {
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\ntype Reading:\n    sensor: String\n    values: List(Int)\n\nfn main(console: Console):\n    let st = region -> Stack(Int):\n        Push(1, Push(2, Empty))\n    print(console, __render(st == Push(1, Push(2, Empty))))\n    let r = region -> Reading:\n        var vs = []\n        for i in 0..50:\n            vs = list.push(vs, i * i)\n        Reading(sensor: \"t\" + \"0\", values: vs)\n    print(console, r.sensor)\n    print(console, __render(list.at(r.values, 49)))\n    let d = region -> Dict(String, Int):\n        var m = dict.new()\n        for i in 0..100:\n            m = dict.insert(m, \"k\" + __render(i), i)\n        m\n    print(console, __render(dict.get_or(d, \"k42\", 0 - 1)))\n    let shared = \"parent-side\"\n    let s = region -> String:\n        shared\n    print(console, s)\n    let nested = region -> Int:\n        let inner = region -> String:\n            \"abc\" + \"def\"\n        string.length(inner)\n    print(console, __render(nested))\n";
        let want: Vec<String> = ["true", "t0", "2401", "42", "parent-side", "6"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `region:` reclamation: 100k regions each churning a 1000-element
    /// throwaway list (~1.6 GB cumulative, past the 1 GB cap) run in constant
    /// memory — inside a loop the automatic reset cannot help (an outer list
    /// grows), so only the region machinery reclaims. WASM-only for speed.
    #[test]
    fn region_reclaims_inside_nonresettable_loops() {
        let src = "fn main(console: Console):\n    var total = 0\n    var keep = []\n    for i in 0..100000:\n        let last = region -> Int:\n            var row = []\n            var j = 0\n            for j in 0..1000:\n                row = list.push(row, j)\n            list.at(row, 999)\n        total = total + last\n        keep = list.push(keep, i)\n    print(console, __render(total))\n    print(console, __render(list.length(keep)))\n";
        assert_eq!(wasm_run(src), vec!["99900000", "100000"]);
    }

    /// `region:` Phase 3: the `__region_copy_bytes` counter proves the
    /// watermark short-circuit — a parent-side passthrough copies ZERO bytes,
    /// and a region-born string copies exactly its own block.
    #[test]
    fn region_copy_counter_proves_passthrough_is_free() {
        use crate::runtime::{Capabilities, Runtime};
        let run_and_count = |src: &str| -> (Vec<String>, i64) {
            let module = parser::parse_module(src).expect("parse");
            let bytes = codegen::compile_module_binary(&module)
                .expect("compile")
                .expect("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("rt");
            let mut actor = rt
                .spawn(
                    &bytes,
                    Capabilities { print: true, quiet: true, ..Default::default() },
                    64,
                )
                .expect("spawn");
            actor.run().expect("run");
            (actor.output(), actor.region_copy_bytes().expect("counter"))
        };
        // Parent-side value: shared, not copied.
        let (out, copied) = run_and_count(
            "fn main(console: Console):\n    let shared = \"twelve chars\"\n    let s = region -> String:\n        shared\n    print(console, s)\n",
        );
        assert_eq!(out, vec!["twelve chars"]);
        assert_eq!(copied, 0, "parent passthrough must copy nothing");
        // Region-born value: exactly its own block (4-byte header + 6 bytes).
        let (out, copied) = run_and_count(
            "fn main(console: Console):\n    let s = region -> String:\n        \"abc\" + \"def\"\n    print(console, s)\n",
        );
        assert_eq!(out, vec!["abcdef"]);
        assert_eq!(copied, 10, "a region-born string copies header + bytes");
    }

    /// `region:` rejections: an outer pointer-typed assignment and a `yield`
    /// are type errors — the region's only pointer escape is its value.
    #[test]
    fn region_rejects_outer_pointer_assign_and_yield() {
        let leak = "fn main(console: Console):\n    var leak = [1]\n    let x = region:\n        leak = list.push(leak, 2)\n        7\n    print(console, __render(x))\n";
        let module = parser::parse_module(leak).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("outer pointer assign must be rejected");
        assert!(err.to_string().contains("inside `region:`"), "{err}");
    }
    /// open-addressing table over the (insertion-ordered) entry array, so
    /// get_or/has/insert lookups probe instead of scanning. String and Int
    /// keys, growth rebuilds, removal (index dropped, rebuilt on next
    /// growth), and a missing-key probe all agree with the interpreter.
    #[test]
    fn dict_hash_index_agrees_on_both_backends() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..3000:\n        d = dict.insert(d, \"k\" + __render(i), i * 2)\n    print(console, __render(dict.length(d)))\n    print(console, __render(dict.get_or(d, \"k2999\", 0 - 1)))\n    print(console, __render(dict.get_or(d, \"absent\", 0 - 1)))\n    print(console, __render(dict.contains_key(d, \"k1500\")))\n    d = dict.remove(d, \"k0\")\n    print(console, __render(dict.length(d)))\n    d = dict.insert(d, \"again\", 7)\n    print(console, __render(dict.get_or(d, \"again\", 0 - 1)))\n";
        let want: Vec<String> = ["3000", "5998", "-1", "true", "2999", "7"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// REGRESSION GUARD: `list.reverse`/`flatten`/`flat_map` are O(n), not O(n^2).
    /// They used to accumulate with `list.concat`, which copies the whole growing
    /// result each iteration — O(n^2) time AND allocation, which traps the WASM
    /// bump allocator (out-of-bounds) at ~20k elements. At 50k the linear
    /// push-loop forms stay far under the heap ceiling; an O(n^2) regression would
    /// trap on the compiled backend and fail here.
    #[test]
    fn list_reverse_flatten_flat_map_are_linear_at_scale() {
        let src = "fn main(console: Console):\n    var xs = []\n    for i in 0..50000:\n        xs = list.push(xs, i)\n    let r = list.reverse(xs)\n    print(console, __render(list.at(r, 0)))\n    print(console, __render(list.at(r, 49999)))\n    print(console, __render(list.flatten([[1, 2], [], [3]])))\n    print(console, __render(list.flat_map([1, 2, 3], fn(x: Int): [x, x * 10])))\n";
        let want: Vec<String> = ["49999", "0", "[1, 2, 3]", "[1, 10, 2, 20, 3, 30]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE SET_AT: `xs = list.set_at(xs, i, v)` mutates the owned buffer's
    /// slot in place (O(1)) via `$list_set_cap`, instead of rebuilding the whole
    /// list each set — which is O(n^2) memory that traps the WASM bump allocator
    /// at ~10k. An aliased list keeps the copying set_at (the alias still sees the
    /// original), and an out-of-range index leaves the list unchanged.
    #[test]
    fn inplace_set_at_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var xs = []\n    for i in 0..5000:\n        xs = list.push(xs, 0)\n    var k = 0\n    while k < 5000:\n        xs = list.set_at(xs, k, k * 2)\n        k = k + 1\n    print(console, __render(list.at(xs, 4999)))\n    xs = list.set_at(xs, 99999, 7)\n    print(console, __render(list.length(xs)))\n    var ys = [1, 2, 3]\n    let alias = ys\n    ys = list.set_at(ys, 1, 99)\n    print(console, __render(list.at(ys, 1)))\n    print(console, __render(list.at(alias, 1)))\n";
        let want: Vec<String> =
            ["9998", "5000", "99", "2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE UPDATE_AT: `xs = list.update_at(xs, i, f)` applies the closure to
    /// the owned buffer's slot in place (O(1)) via `$list_update_cap`, instead of
    /// rebuilding the whole list each update (O(n^2), OOM-prone). Alias-safe (a
    /// shared list keeps the copy), and an out-of-range index is a no-op.
    #[test]
    fn inplace_update_at_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var xs = []\n    for i in 0..5000:\n        xs = list.push(xs, 1)\n    var k = 0\n    while k < 5000:\n        xs = list.update_at(xs, k, fn(v: Int): v + 1)\n        k = k + 1\n    print(console, __render(list.at(xs, 4999)))\n    xs = list.update_at(xs, 99999, fn(v: Int): v + 1)\n    print(console, __render(list.length(xs)))\n    var ys = [1, 2, 3]\n    let alias = ys\n    ys = list.update_at(ys, 1, fn(v: Int): v + 100)\n    print(console, __render(list.at(ys, 1)))\n    print(console, __render(list.at(alias, 1)))\n";
        let want: Vec<String> =
            ["2", "5000", "102", "2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE DICT INSERT: `d = dict.insert(d, k, v)` updates/appends into owned
    /// entry slack (no per-insert table copy); an aliased dict keeps the
    /// copying insert, so the alias still sees the original.
    #[test]
    fn inplace_dict_insert_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..2000:\n        d = dict.insert(d, i, i * 2)\n    print(console, __render(dict.length(d)))\n    print(console, __render(dict.get_or(d, 1999, 0 - 1)))\n    var e = dict.new()\n    let alias = e\n    e = dict.insert(e, 1, 10)\n    print(console, __render(dict.length(alias)))\n    print(console, __render(dict.length(e)))\n";
        let want: Vec<String> =
            ["2000", "3998", "0", "1"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// ARENA WATERMARK RESETS: a loop whose body lets nothing escape an
    /// iteration (only scalar outer assignments) reclaims each iteration's
    /// allocations — 200k iterations that would otherwise demand ~6 GB run in
    /// constant memory. WASM-only: the interpreter's clone-per-push is
    /// quadratic and would take far too long at this scale.
    #[test]
    fn arena_resets_keep_escape_free_loops_constant_memory() {
        let src = "fn main(console: Console):\n    var total = 0\n    for i in 0..200000:\n        var row = []\n        var j = 0\n        for j in 0..1000:\n            row = list.push(row, j)\n        total = total + list.at(row, 999)\n    print(console, __render(total))\n";
        assert_eq!(wasm_run(src), vec!["199800000"]);
    }

    /// IN-PLACE STRING APPEND: the builder pattern `s = s + piece` appends
    /// into owned byte slack (amortized O(1)); a literal-seeded alias keeps
    /// the copying path, so the interned literal is never mutated.
    #[test]
    fn inplace_string_append_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var s = \"\"\n    for i in 0..20000:\n        s = s + \"ab\"\n    print(console, __render(string.length(s)))\n    var t = \"seed\"\n    let alias = t\n    t = t + \"!\"\n    print(console, alias)\n    print(console, t)\n";
        let want: Vec<String> =
            ["40000", "seed", "seed!"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE PUSH (the linear-update optimization): an unaliased
    /// accumulate-in-loop appends into owned slack — 50k pushes complete
    /// instantly instead of O(n²) copying — while an ALIASED list keeps the
    /// copying push, so value semantics hold: `ys` still sees the original.
    #[test]
    fn inplace_push_is_fast_and_alias_safe() {
        // 50k would take minutes under clone-per-push on either backend; both
        // have an in-place fast path for the unaliased self-assign shape.
        let src = "fn main(console: Console):\n    var xs = []\n    for i in 0..50000:\n        xs = list.push(xs, i)\n    print(console, __render(list.length(xs)))\n    print(console, __render(list.at(xs, 49999)))\n    var small = [1]\n    let alias = small\n    small = list.push(small, 2)\n    print(console, __render(alias))\n    print(console, __render(small))\n";
        let want: Vec<String> = ["50000", "49999", "[1]", "[1, 2]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE DICT ACCUMULATION: the `d = dict.insert(d, k, v)` and
    /// `d = dict.update(d, k, dflt, f)` self-assign shapes mutate the slot in place
    /// on both backends; an aliased dict keeps the copying path so value
    /// semantics hold.
    #[test]
    fn inplace_dict_upsert_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..10000:\n        d = dict.insert(d, i, i)\n    print(console, __render(dict.length(d)))\n    var counts = dict.new()\n    for i in 0..30000:\n        counts = dict.update(counts, i % 3, 0, fn(n: Int): n + 1)\n    print(console, __render(dict.get_or(counts, 0, 0)))\n    var small = dict.new()\n    small = dict.insert(small, 1, 10)\n    let alias = small\n    small = dict.insert(small, 2, 20)\n    print(console, __render(dict.length(alias)))\n    print(console, __render(dict.length(small)))\n";
        let want: Vec<String> = ["10000", "10000", "1", "2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// Closure capture is pruned to the names the body mentions (the
    /// interpreter used to clone the entire environment per closure — itself a
    /// quadratic cost in accumulation loops). Calling through a captured
    /// closure variable still works, and capture remains a snapshot: a later
    /// reassignment of the source variable is invisible to the closure.
    #[test]
    fn closure_capture_pruned_and_snapshot() {
        let src = "fn main(console: Console):\n    let add = fn(x: Int): x + 1\n    let twice = fn(y: Int): add(add(y))\n    print(console, __render(twice(3)))\n    var n = 10\n    let snap = fn(): n\n    n = 99\n    print(console, __render(snap()))\n";
        let want: Vec<String> = ["5", "10"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// The `std/regex` toolkit — greedy quantifiers, escapes (`\d`/`\w`/`\s` and
    /// literal metacharacters), character classes with ranges and negation, and
    /// the span-based API (`find`/`find_all`/`extract`/`replace_all`/`split`) —
    /// agrees on both backends, including the `Option((Int, Int))` span payload.
    #[test]
    fn regex_module_toolkit_agrees_on_both_backends() {
        let src = "import regex\nimport string\n\nfn main(console: Console):\n    print(console, __render(regex.matches(\"h.llo\", \"say hello\")))\n    print(console, __render(regex.matches(\"^\\\\d+$\", \"12345\")))\n    print(console, __render(regex.matches(\"^\\\\d+$\", \"12a45\")))\n    print(console, __render(regex.extract(\"\\\\d+\", \"a1b22c333\")))\n    print(console, regex.replace_all(\"\\\\s+\", \"too   many    spaces\", \" \"))\n    print(console, __render(regex.split(\",\\\\s*\", \"a, b,c\")))\n    print(console, __render(regex.matches(\"[a-f]+\", \"deadbeef\")))\n    print(console, __render(regex.matches(\"^[^0-9]+$\", \"abc\")))\n    print(console, __render(regex.find(\"a+\", \"caat\")))\n    print(console, __render(regex.matches(\"\\\\w+@\\\\w+\\\\.\\\\w+\", \"mail me: a_b@example.com\")))\n    print(console, regex.replace_all(\"[0-9]+\", \"r2d2\", \"#\"))\n";
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

    /// An invalid regex pattern must NOT trap the VM: `matches`/`find`/`find_all`/`split`
    /// return Bool/Option/List (a total contract), so a bad pattern yields the empty result,
    /// identically on both backends. Previously the compiled backend trapped while the
    /// interpreter raised a RuntimeError — a parity gap and a latent DoS if a pattern were
    /// ever attacker-supplied.
    #[test]
    fn regex_invalid_pattern_is_total_on_both_backends() {
        let src = "import regex\nimport string\n\nfn main(console: Console):\n    print(console, __render(regex.matches(\"[\", \"x\")))\n    print(console, __render(regex.find(\"(unclosed\", \"x\")))\n    print(console, __render(regex.find_all(\"*\", \"x\")))\n    print(console, __render(regex.split(\"((((\", \"a,b\")))\n";
        let want: Vec<String> =
            ["false", "None", "[]", "[a,b]"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// Alternation `a|b` and grouping `(...)` — which the old hand-rolled engine
    /// silently failed to match — now work (the `regex` crate), identically on
    /// both backends, including grouped extract.
    #[test]
    fn regex_alternation_and_groups_agree_on_both_backends() {
        let src = "import regex\n\nfn main(console: Console):\n    print(console, __render(regex.matches(\"cat|dog\", \"I have a dog\")))\n    print(console, __render(regex.matches(\"(cat|dog)s?\", \"cats\")))\n    print(console, __render(regex.extract(\"(foo|bar)\", \"foo bar baz\")))\n    print(console, regex.replace_all(\"(a|b)+\", \"abab x\", \"Z\"))\n    print(console, __render(regex.find(\"(cat|dog)\", \"a dog\")))\n";
        let want: Vec<String> = ["true", "true", "[foo, bar]", "Z x", "Some((2, 5))"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `examples/matrix/src/matrix.witchy` — integer matrices — multiplies a 2x3 by a 3x2,
    /// transposes, and prints an identity, all with right-aligned columns. A
    /// `List(List(Int))` workout (nested `at`) that agrees on both backends.
    #[test]
    fn matrix_example_multiplies_and_transposes() {
        assert_eq!(
            crate::execute_file("examples/matrix/src/matrix.witchy", Vec::new()).unwrap(),
            vec![
                "A x B =",
                "[  58  64 ]",
                "[ 139 154 ]",
                "transpose(A) =",
                "[ 1 4 ]",
                "[ 2 5 ]",
                "[ 3 6 ]",
                "identity(3) =",
                "[ 1 0 0 ]",
                "[ 0 1 0 ]",
                "[ 0 0 1 ]",
            ]
        );
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

    /// `examples/toposort/src/toposort.witchy` — Kahn's topological sort over a dependency
    /// graph — produces a valid build order and reports a cycle. Pure (Console),
    /// list-based (no Dict), both backends.
    #[test]
    fn toposort_example_orders_and_detects_cycles() {
        assert_eq!(
            crate::execute_file("examples/toposort/src/toposort.witchy", Vec::new()).unwrap(),
            vec![
                "build order: boot -> config -> db -> cache -> api -> web",
                "cyclic:      error: cycle among egg, chicken",
            ]
        );
    }

    /// `examples/jq/src/jq.witchy` — a JSON query tool — walks a dotted path (object keys
    /// and numeric array indices) into a decoded document and renders the value.
    /// Pure (Console), both backends.
    #[test]
    fn jq_example_queries_json_by_path() {
        assert_eq!(
            crate::execute_file("examples/jq/src/jq.witchy", Vec::new()).unwrap(),
            vec![
                "user.name       => \"Ada\"",
                "user.roles      => [\"admin\",\"dev\"]",
                "user.roles.0    => \"admin\"",
                "user.roles.1    => \"dev\"",
                "count           => 42",
                "active          => true",
                "user.missing    => (no such path)",
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
        let src = "fn main(console: Console):\n    let cs = [\"a\", \" \", \"z\"]\n    print(console, if list.at(cs, 1) == \" \": \"eq\" else: \"ne\")\n    print(console, if \"a\" == list.at(cs, 0): \"eq\" else: \"ne\")\n    print(console, if list.at(cs, 0) == \"z\": \"eq\" else: \"ne\")\n";
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
        let src = "fn main(console: Console):\n    let a = \"x\" + \"y\"\n    let b = \"x\" + \"y\"\n    let xs = [a, b, \"zz\"]\n    print(console, if list.at(xs, 0) == list.at(xs, 1): \"eq\" else: \"ne\")\n    print(console, if list.at(xs, 0) == list.at(xs, 2): \"eq\" else: \"ne\")\n";
        let want = vec!["eq".to_string(), "ne".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A negative `Int` that enters a list through a *generic* function (the
    /// element type is a type variable, so it crosses the i32 generic ABI) and is
    /// then read back through *concrete* `List(Int)` code must keep its sign on
    /// WASM. `to_slot` used to zero-extend, turning -1 into 4294967295 when a
    /// concrete reader loaded the i64 slot; it now sign-extends (pointers/Bools
    /// are < 2^31, so they're unaffected). Regression for the generic-list bug
    /// found via `list.repeat(-1, n)`.
    #[test]
    fn wasm_negative_int_survives_the_generic_list_abi() {
        let src = "fn fill(x: a, n: Int) -> List(a):\n    var out = []\n    var i = 0\n    while i < n:\n        out = list.push(out, x)\n        i = i + 1\n    out\n\nfn show(xs: List(Int)) -> String:\n    var out = \"\"\n    for v in xs:\n        out = out + __render(v) + \" \"\n    out\n\nfn main(console: Console):\n    print(console, show(fill(-1, 3)))\n";
        let want = vec!["-1 -1 -1 ".to_string()];
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
        let src = "fn count_eq(xs: List(a), target: a) -> Int:\n    var n = 0\n    for x in xs:\n        if x == target:\n            n = n + 1\n    n\n\nfn b(s: String) -> String:\n    s + \"\"\n\nfn main(console: Console):\n    print(console, __render(count_eq([b(\"aa\"), b(\"bb\"), b(\"aa\")], b(\"aa\"))))\n";
        let want = vec!["2".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A large `Int` carried through an *unbounded* generic function must keep its
    /// 64 bits on WASM. The generic i32 ABI truncated it; the WASM backend now
    /// monomorphizes the call on `Int` (`fill__Int`), so the i64 survives.
    /// (Regression for the big-int-through-generic gap.)
    #[test]
    fn wasm_monomorphizes_big_int_through_generic() {
        let src = "fn fill(x: a, n: Int) -> List(a):\n    var out = []\n    var i = 0\n    while i < n:\n        out = list.push(out, x)\n        i = i + 1\n    out\n\nfn main(console: Console):\n    let xs = fill(5000000000, 2)\n    print(console, __render(list.at(xs, 0)))\n";
        let want = vec!["5000000000".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A large `Int` RETURNED from a closure must keep its 64 bits on WASM.
    /// Closures use the i64 universal-slot result ABI, and a higher-order call
    /// recovers the result at the closure's return kind (here `fn(Int) -> Int`).
    /// (Regression for the big-Int-through-closure-return gap.)
    #[test]
    fn wasm_big_int_returned_from_closure() {
        let src = "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main(console: Console):\n    print(console, __render(apply(fn(k: Int): k * 5000000000, 2)))\n";
        let want = vec!["10000000000".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A large `Int` passed AS a closure argument, and one CAPTURED by a closure,
    /// must keep their 64 bits on WASM. Closure params and captures use the i64
    /// universal slot (recovered at their kind in the lambda prologue), matching
    /// the result ABI. (Regression for big-Int-through-closure arg/capture.)
    #[test]
    fn wasm_big_int_closure_arg_and_capture() {
        // Argument: 5000000000 passed to the closure, + 1.
        let arg = "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main(console: Console):\n    print(console, __render(apply(fn(k: Int): k + 1, 5000000000)))\n";
        assert_eq!(interp(arg), vec!["5000000001"], "interpreter (arg)");
        assert_eq!(run_on_wasm(arg), vec!["5000000001"], "WASM (arg)");
        // Capture: a big Int captured by the closure, recovered from the env.
        let cap = "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main(console: Console):\n    let big = 5000000000\n    print(console, __render(apply(fn(x: Int): x + big, 1)))\n";
        assert_eq!(interp(cap), vec!["5000000001"], "interpreter (capture)");
        assert_eq!(run_on_wasm(cap), vec!["5000000001"], "WASM (capture)");
    }

    /// A Dict keyed by `Float` must look up the same on both backends. Float keys
    /// go into the universal i64 slot as their bit pattern; `$key_eq` mode 2
    /// reinterprets and compares with `f64.eq`, matching the interpreter's `==`
    /// (insertion-order, value equality). (Regression for the interpreter-only
    /// Float-key gap.)
    #[test]
    fn dict_float_keys_agree_on_both_backends() {
        let src = "fn main(console: Console):\n    let d = dict.insert(dict.insert(dict.insert(dict.new(), 1.5, \"a\"), 2.5, \"b\"), 1.5, \"c\")\n    print(console, dict.get_or(d, 1.5, \"?\"))\n    print(console, dict.get_or(d, 2.5, \"?\"))\n    print(console, dict.get_or(d, 9.9, \"?\"))\n    print(console, __render(dict.length(d)))\n    let e = dict.remove(d, 1.5)\n    print(console, dict.get_or(e, 1.5, \"gone\"))\n    print(console, __render(dict.length(e)))\n";
        let want = vec![
            "c".to_string(),
            "b".to_string(),
            "?".to_string(),
            "2".to_string(),
            "gone".to_string(),
            "1".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// The Secret capability is enforced in the WASM sandbox: with the same
    /// seed granted, sign/public_key/verify produce byte-identical results on
    /// both backends (Ed25519 is deterministic), and a module importing the
    /// signing ops cannot instantiate without the grant — the seed never enters
    /// guest memory.
    #[test]
    fn signing_key_compiles_to_wasm_and_is_gated() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import crypto\nfn main(console: Console, signer: Secret):\n    let msg = \"sign me\"\n    let sig = crypto.sign(signer, msg)\n    print(console, crypto.public_key(signer))\n    print(console, sig)\n    print(console, if crypto.ed25519_verify(crypto.public_key(signer), msg, sig): \"verified\" else: \"FAILED\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let seed = [7u8; 32];
        let interp_out =
            interpreter::run_module_signed(linked.clone(), ".", Vec::new(), Vec::new(), Some(seed))
                .expect("interp");
        assert_eq!(interp_out[2], "verified");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    signing_key: Some(seed),
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), interp_out, "signature + pubkey must be byte-identical");

        // Ungranted: the imports are absent, so instantiation fails.
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &bytes,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.is_err(), "signing imports must not instantiate without the grant");
    }

    /// `SecretStore.get`/`.require` and `crypto.sign`/`public_key` must behave
    /// identically on both backends. The `signing` secret (granted by the seed) is
    /// fetched via `require`, signed over, and its public key derived; an absent
    /// secret yields `None`. The interpreter (oracle) and the compiled WASM must
    /// produce byte-identical output — the same parity discipline as raw
    /// `crypto.sign`. (Revealing the signing key is SEC-004-gated and covered by
    /// `signing_key_is_not_revealable_on_both_backends`.)
    #[test]
    fn secretstore_and_reveal_agree_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import secretstore\nimport crypto\nfn main(console: Console, secrets: SecretStore):\n    let key = secrets.require(\"signing\")\n    print(console, crypto.public_key(key))\n    print(console, crypto.sign(key, \"msg\"))\n    match secrets.get(\"signing\"):\n        Some(k) -> print(console, \"got signing\")\n        None -> print(console, \"no signing\")\n    match secrets.get(\"absent\"):\n        Some(k) -> print(console, \"unexpected\")\n        None -> print(console, \"absent none\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let seed = [7u8; 32];
        let interp_out =
            interpreter::run_module_signed(linked.clone(), ".", Vec::new(), Vec::new(), Some(seed))
                .expect("interp");
        assert_eq!(interp_out[2], "got signing");
        assert_eq!(interp_out[3], "absent none");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    signing_key: Some(seed),
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(
            actor.output(),
            interp_out,
            "SecretStore.get/require + crypto.sign/public_key must be byte-identical on both backends"
        );
    }

    /// SEC-004: the signing key — the bare `Secret`, or `require("signing")` — is
    /// SIGN-ONLY. `crypto.reveal` on it must error (so handing code a key to sign
    /// with cannot also exfiltrate it), and it must error IDENTICALLY on both
    /// backends (the gate is one shared identity rule). Named value-secrets stay
    /// revealable — covered end-to-end by the `sandbox` CLI e2e test.
    #[test]
    fn signing_key_is_not_revealable_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import secretstore\nimport crypto\nfn main(console: Console, secrets: SecretStore):\n    print(console, crypto.reveal(secrets.require(\"signing\")))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let seed = [7u8; 32];

        // Interpreter (oracle): refuses.
        let interp =
            interpreter::run_module_signed(linked.clone(), ".", Vec::new(), Vec::new(), Some(seed));
        let msg = interp.expect_err("interp must refuse to reveal the signing key").message;
        assert!(msg.contains("not revealable"), "unexpected interp error: {msg}");

        // Compiled WASM (the security boundary): also refuses.
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    signing_key: Some(seed),
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(
            actor.run().is_err(),
            "compiled backend must refuse to reveal the signing key"
        );
    }

    /// Every ```witchy code block in the documentation must be a real program:
    /// it parses, links, and type-checks; and when it defines a `main` whose
    /// footprint needs nothing beyond Console, it RUNS on both backends and the
    /// outputs must agree. Docs that drift from the language break the build.
    #[test]
    fn documentation_examples_are_valid() {
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

        let mut checked = 0usize;
        let mut ran = 0usize;
        for file in &files {
            let Ok(text) = std::fs::read_to_string(file) else { continue };
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
                // Actors compile through a separate module path (and run on the
                // demo scheduler), so the single-module run below doesn't apply —
                // such examples are still fully parse + type-checked above.
                let has_actor = false;
                // A `main` that declares an argv parameter (`args: List(String)`)
                // is type-checked but not run here: argv isn't a capability (so the
                // footprint still looks "Console-only"), yet the interpreter and
                // WASM run paths don't share an argv source, so comparing their
                // output is meaningless. Same rationale as the actor skip above.
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
                    // Every console-only doc example compiles on the binary path
                    // (AST → WIR → wasm-binary) and runs identically to the
                    // interpreter.
                    let bytes = codegen::compile_module_binary(&linked)
                        .unwrap_or_else(|e| panic!("{context} fails to compile to WASM: {e}"))
                        .unwrap_or_else(|| panic!("{context}: the binary backend does not support a construct it uses"));
                    let interp =
                        interpreter::run_module(linked, std::path::Path::new("."), Vec::new())
                            .unwrap_or_else(|e| panic!("{context} fails on the interpreter: {e}"));
                    let compiled = crate::run_wasm_bytes(&bytes)
                        .unwrap_or_else(|e| panic!("{context} fails on WASM: {e}"));
                    assert_eq!(interp, compiled, "{context}: the backends DIVERGE");
                    ran += 1;
                }
            }
        }
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
        let src = "fn main(console: Console):\n    print(console, \"before\")\n    fail(\"boom\")\n    print(console, \"after\")\n";
        let err = interpreter::run(src).expect_err("interpreter must abort");
        assert!(err.message.contains("boom"));
        let module = parser::parse_module(src).expect("parse");
        // `fail()` lowers on the binary path: drop the message, then `unreachable`.
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("fail() lowers on the binary path");
        assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on fail()");
    }

    /// `now` (Clock) and `get_env` (Env) compile to capability-gated host
    /// imports. `get_env` is deterministic given the process env, so both
    /// backends must agree exactly; `now` is wall-clock, so each backend is
    /// checked for plausibility instead. Also exercises a multi-capability
    /// `main` (Console + Env / Console + Clock), which codegen now accepts.
    #[test]
    fn clock_and_env_compile_to_wasm_and_agree() {
        // SAFETY-free env set: std::env::set_var is fine in a single-threaded
        // test context; the var is namespaced to this test.
        unsafe { std::env::set_var("WITCHY_E2E_ENV_VAR", "from the host") };
        let env_src = "import option\n\nfn main(console: Console, env: Env):\n    match get_env(env, \"WITCHY_E2E_ENV_VAR\"):\n        Some(v) -> print(console, \"got: \" + v)\n        None -> print(console, \"unset\")\n    match get_env(env, \"WITCHY_E2E_DEFINITELY_UNSET\"):\n        Some(v) -> print(console, \"got: \" + v)\n        None -> print(console, \"unset\")\n";
        let want = vec!["got: from the host".to_string(), "unset".to_string()];
        let module = parser::parse_module(env_src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        assert_eq!(link_run(env_src), want.clone(), "interpreter");
        assert_eq!(crate::run_wasm_bytes(&bytes).expect("wasm"), want, "compiled WASM must agree");

        // The clock: both backends must yield a plausible epoch-milliseconds.
        let clock_src = "fn main(console: Console, clock: Clock):\n    print(console, if now(clock) > 1500000000000: \"plausible\" else: \"implausible\")\n";
        assert_eq!(interp(clock_src), vec!["plausible"], "interpreter");
        assert_eq!(run_on_wasm(clock_src), vec!["plausible"], "compiled WASM");
    }

    /// The full Dir family compiles to capability-gated host imports and agrees
    /// with the interpreter: read/exists/is_dir/subdir/write/make_dir/list all
    /// round-trip in a confined temp directory, and escape attempts (`..`,
    /// absolute paths) FAIL on both backends.
    #[test]
    fn dir_capability_compiles_to_wasm_and_confines() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("a.txt"), "alpha").expect("seed a");
        std::fs::write(root.join("sub/b.txt"), "beta").expect("seed b");

        let src = "fn main(console: Console, dir: Dir):\n    print(console, read(dir, \"a.txt\"))\n    print(console, __render(exists(dir, \"a.txt\")))\n    print(console, __render(exists(dir, \"missing.txt\")))\n    let sub = subtree(dir, \"sub\")\n    print(console, read(sub, \"b.txt\"))\n    write(dir, \"out.txt\", \"written\")\n    print(console, read(dir, \"out.txt\"))\n    make_dir(dir, \"made\")\n    print(console, __render(is_dir(dir, \"made\")))\n    for name in list(dir):\n        print(console, \"entry: \" + name)\n";
        let want = vec![
            "alpha".to_string(),
            "true".to_string(),
            "false".to_string(),
            "beta".to_string(),
            "written".to_string(),
            "true".to_string(),
            "entry: a.txt".to_string(),
            "entry: made".to_string(),
            "entry: out.txt".to_string(),
            "entry: sub".to_string(),
        ];
        let interp_out = interpreter::run_in(src, &root).expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    dir_write: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        for bad in ["../outside.txt", "/etc/hosts"] {
            let esc = format!(
                "fn main(console: Console, dir: Dir):\n    print(console, read(dir, \"{bad}\"))\n"
            );
            assert!(interpreter::run_in(&esc, &root).is_err(), "interp must reject `{bad}`");
            let m = parser::parse_module(&esc).expect("parse");
            let wbytes = codegen::compile_module_binary(&m)
                .expect("compile")
                .expect("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("runtime");
            let mut a = rt
                .spawn(
                    &wbytes,
                    Capabilities {
                        print: true,
                        quiet: true,
                        dir_root: Some(root.clone()),
                        dir_read: true,
                        dir_write: true,
                        ..Default::default()
                    },
                    64,
                )
                .expect("spawn");
            assert!(a.run().is_err(), "WASM must trap on `{bad}`");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0011: `dir.only(confine.ext(...))` confines a `Dir` to an ENTRY policy —
    /// reading a matching extension is allowed, a non-matching one is refused at the
    /// policy check — identically on both backends.
    #[test]
    fn dir_only_ext_policy_confines_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_dirpol_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("ok.txt"), "hello").expect("seed txt");
        std::fs::write(root.join("secret.key"), "TOPSECRET").expect("seed key");
        let root_str = root.to_str().expect("utf8 root").to_string();

        let caps = || Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            dir_write: true,
            ..Default::default()
        };

        // Allowed: read a `.txt` through a Dir narrowed to `ext(".txt")`.
        let ok_src = "import confine\nfn main(console: Console, dir: Dir):\n    let txt = dir.only(confine.ext(\".txt\"))\n    print(console, read(txt, \"ok.txt\"))\n";
        let want = vec!["hello".to_string()];
        assert_eq!(
            interpreter::run_module(resolve_std_src(ok_src), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bytes = codegen::compile_module_binary(&resolve_std_src(ok_src))
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt.spawn(&bytes, caps(), 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        // Denied: a `.key` through the same narrowed Dir is refused on both backends.
        let bad_src = "import confine\nfn main(console: Console, dir: Dir):\n    let txt = dir.only(confine.ext(\".txt\"))\n    print(console, read(txt, \"secret.key\"))\n";
        assert!(
            interpreter::run_module(resolve_std_src(bad_src), &root_str, Vec::new()).is_err(),
            "interp must refuse a .key",
        );
        let bbytes = codegen::compile_module_binary(&resolve_std_src(bad_src))
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt2 = Runtime::batch().expect("runtime");
        let mut a = rt2.spawn(&bbytes, caps(), 64).expect("spawn");
        assert!(a.run().is_err(), "WASM must refuse a .key");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0011: the `kind:` Dir entry policy. `dir.only(confine.files())` admits a file
    /// read but DENIES opening a sub-directory; `dir.only(confine.dirs())` is the mirror.
    /// An `ext`-only policy still traverses (kind gates directories, ext gates file names),
    /// so `kind` is additive and backward-compatible — all identical on both backends.
    #[test]
    fn dir_kind_policy_confines_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_dirkind_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir sub");
        std::fs::write(root.join("ok.txt"), "hello").expect("seed txt");
        let root_str = root.to_str().expect("utf8 root").to_string();

        let caps = || Capabilities {
            print: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            dir_write: true,
            ..Default::default()
        };
        // Assert BOTH backends produce `want`.
        let ok_both = |src: &str, want: Vec<String>| {
            assert_eq!(
                interpreter::run_module(resolve_std_src(src), &root_str, Vec::new()).expect("interp"),
                want,
                "interp: {src}",
            );
            let bytes = codegen::compile_module_binary(&resolve_std_src(src))
                .expect("compile")
                .expect("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("runtime");
            let mut actor = rt.spawn(&bytes, caps(), 64).expect("spawn");
            actor.run().expect("run");
            assert_eq!(actor.output(), want, "wasm: {src}");
        };
        // Assert BOTH backends REFUSE (the policy check trips identically).
        let err_both = |src: &str| {
            assert!(
                interpreter::run_module(resolve_std_src(src), &root_str, Vec::new()).is_err(),
                "interp should refuse: {src}",
            );
            let bytes = codegen::compile_module_binary(&resolve_std_src(src))
                .expect("compile")
                .expect("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("runtime");
            let mut actor = rt.spawn(&bytes, caps(), 64).expect("spawn");
            assert!(actor.run().is_err(), "wasm should refuse: {src}");
        };

        // `files()`: read a file OK; opening a sub-directory DENIED (the DoD headline).
        ok_both(
            "import confine\nfn main(console: Console, dir: Dir):\n    let d = dir.only(confine.files())\n    print(console, read(d, \"ok.txt\"))\n",
            vec!["hello".to_string()],
        );
        err_both("import confine\nfn main(console: Console, dir: Dir):\n    let d = dir.only(confine.files())\n    let s = d.subtree(\"sub\")\n    print(console, \"unreached\")\n");

        // `dirs()`: open a sub-directory OK; reading a file DENIED (the mirror).
        ok_both(
            "import confine\nfn main(console: Console, dir: Dir):\n    let d = dir.only(confine.dirs())\n    let s = d.subtree(\"sub\")\n    print(console, \"traversed\")\n",
            vec!["traversed".to_string()],
        );
        err_both("import confine\nfn main(console: Console, dir: Dir):\n    let d = dir.only(confine.dirs())\n    print(console, read(d, \"ok.txt\"))\n");

        // An `ext`-only policy still traverses — kind gates directories, ext gates files.
        ok_both(
            "import confine\nfn main(console: Console, dir: Dir):\n    let d = dir.only(confine.ext(\".txt\"))\n    let s = d.subtree(\"sub\")\n    print(console, \"traversed\")\n",
            vec!["traversed".to_string()],
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0011: `dir.subtree(path)` is the method form of `subdir` — it narrows a
    /// `Dir` to a subtree identically on both backends, and the same `..`/absolute
    /// confinement applies. Mirrors `net.only(...)` as the host-primitive method form.
    #[test]
    fn dir_subtree_method_narrows_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_subtree_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("sub/b.txt"), "beta").expect("seed b");

        // Method form `dir.subtree("sub")` and chained `.subtree(...).subtree(...)`.
        std::fs::create_dir_all(root.join("sub/deep")).expect("mkdir deep");
        std::fs::write(root.join("sub/deep/c.txt"), "gamma").expect("seed c");
        let src = "fn main(console: Console, dir: Dir):\n    let s = dir.subtree(\"sub\")\n    print(console, read(s, \"b.txt\"))\n    print(console, read(s.subtree(\"deep\"), \"c.txt\"))\n";
        let want = vec!["beta".to_string(), "gamma".to_string()];

        let interp_out = interpreter::run_in(src, &root).expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        // The subtree is still confined: `..` from inside it escapes and FAILS.
        let esc = "fn main(console: Console, dir: Dir):\n    print(console, read(dir.subtree(\"sub\"), \"../a.txt\"))\n";
        assert!(interpreter::run_in(esc, &root).is_err(), "interp rejects `..` from a subtree");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0012: the `File` capability round-trips on BOTH backends. `dir.read_file`/
    /// `dir.write_file` navigate a `Dir` to a confined `File` leaf; `read(File)` /
    /// `write(File, data)` operate on it (no path arg), with the same `..`/absolute
    /// confinement as `Dir`. The compiled path uses the `file_read` WIR helper plus
    /// `dir_open`/`dir_create`/`file_write` host ops.
    #[test]
    fn file_capability_compiles_to_wasm_and_confines() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_file_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("note.txt"), "alpha").expect("seed");

        let src = "fn main(console: Console, dir: Dir):\n    print(console, read(dir.read_file(\"note.txt\")))\n    let out = dir.write_file(\"out.txt\")\n    write(out, \"beta\")\n    print(console, read(dir.read_file(\"out.txt\")))\n";
        let want = vec!["alpha".to_string(), "beta".to_string()];
        let interp_out = interpreter::run_in(src, &root).expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    dir_write: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        // A `File` opened via navigation is still confined: `..` escapes and FAILS
        // on both backends.
        let esc = "fn main(console: Console, dir: Dir):\n    print(console, read(dir.read_file(\"../escape.txt\")))\n";
        assert!(interpreter::run_in(esc, &root).is_err(), "interp rejects `..` via open");
        let m = parser::parse_module(esc).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut a = rt
            .spawn(
                &wbytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(a.run().is_err(), "WASM must trap on `..` via open");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0012: `main` may receive a `File` DIRECTLY (the `--file` grant) — the
    /// least-authority single-file case, with NO `Dir`. The i-th `File` param maps
    /// to the i-th grant on both backends (interpreter `file_grants` /
    /// `Capabilities::file_grants` + the pre-populated files table).
    #[test]
    fn file_main_grant_runs_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_fmg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let a_txt = root.join("a.txt");
        let b_txt = root.join("b.txt");
        std::fs::write(&a_txt, "alpha").expect("seed a");
        std::fs::write(&b_txt, "beta").expect("seed b");

        // Two File params, mapped positionally to two grants; no Dir granted.
        let src = "fn main(console: Console, first: File[Read], second: File[Read]):\n    print(console, read(first))\n    print(console, read(second))\n";
        let want = vec!["alpha".to_string(), "beta".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let interp_out = interpreter::run_module_files(module, &root, vec![a_txt.clone(), b_txt.clone()])
            .expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    file_grants: vec![a_txt.clone(), b_txt.clone()],
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The `Exec` capability compiles to a capability-gated host import and
    /// agrees with the interpreter: a confined subprocess runs identically on
    /// both backends, returning the `"<code>\n<output>"` payload, and an
    /// executable outside the granted `Dir` subtree FAILS on both. (Unix-only —
    /// it spawns a shell script.)
    #[cfg(unix)]
    #[test]
    fn exec_capability_compiles_to_wasm_and_agrees() {
        use crate::runtime::{Capabilities, Runtime};
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("witchy_wasm_exec_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        // A tiny deterministic program: echo its two args, then echo stdin.
        let script = root.join("greet");
        std::fs::write(&script, "#!/bin/sh\necho \"args=$1,$2\"\ncat\n").expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        // args "a\0b" -> argv [a, b]; stdin "hi". Payload: "0\nargs=a,b\nhi".
        let src = "fn main(console: Console, runner: Exec, dir: Dir):\n    print(console, exec(runner, dir, \"greet\", \"a\\0b\", \"hi\"))\n";

        let interp_out = interpreter::run_in(src, &root).expect("interp");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    exec: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        // The parity invariant: byte-identical output on both backends.
        assert_eq!(interp_out, actor.output(), "exec must agree across backends");
        // And it actually ran the process.
        assert!(
            interp_out.join("\n").contains("args=a,b") && interp_out.join("\n").contains("hi"),
            "exec output should contain the subprocess result, got {interp_out:?}"
        );

        // An executable outside the granted subtree is rejected on both backends.
        let esc = "fn main(console: Console, runner: Exec, dir: Dir):\n    print(console, exec(runner, dir, \"../escape\", \"\", \"\"))\n";
        assert!(interpreter::run_in(esc, &root).is_err(), "interp must reject escape");
        let m = parser::parse_module(esc).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt2 = Runtime::batch().expect("runtime");
        let mut a = rt2
            .spawn(
                &wbytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    exec: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(a.run().is_err(), "WASM must trap on an escaping exec path");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `main` taking several `Dir` params gets several *distinct* grants —
    /// positional handles (the first `--dir` backs handle 0, the next handle 1)
    /// — identically on both backends. Reading from each confined subtree yields
    /// that subtree's file, and the two never cross. (RFC-0004 multi-Dir.)
    #[test]
    fn multi_dir_grants_are_positional_and_agree() {
        use crate::runtime::{Capabilities, Runtime};
        let base = std::env::temp_dir().join(format!("witchy_multidir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir_a = base.join("a");
        let dir_b = base.join("b");
        std::fs::create_dir_all(&dir_a).expect("mkdir a");
        std::fs::create_dir_all(&dir_b).expect("mkdir b");
        std::fs::write(dir_a.join("f.txt"), "from-A").expect("seed a");
        std::fs::write(dir_b.join("f.txt"), "from-B").expect("seed b");

        // Both Dirs name `f.txt`, but each resolves within its own subtree.
        let src = "fn main(console: Console, da: Dir, db: Dir):\n    print(console, read(da, \"f.txt\"))\n    print(console, read(db, \"f.txt\"))\n";
        let want = vec!["from-A".to_string(), "from-B".to_string()];

        let interp_out =
            interpreter::run_in_dirs(src, &[dir_a.clone(), dir_b.clone()]).expect("interp");
        assert_eq!(interp_out, want, "interpreter multi-dir");

        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(dir_a.clone()),
                    dir_roots: vec![dir_b.clone()],
                    dir_read: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM multi-dir must agree");

        let _ = std::fs::remove_dir_all(&base);
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
            "import mylib\n\nfn main(console: Console):\n    print(console, mylib.greet())\n",
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
            .expect("compile")
            .expect("the binary path lowers this program");
        assert!(!bytes.is_empty(), "produced a wasm binary");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The Net family compiles to capability-gated host imports and agrees with
    /// the interpreter: a client connects to an allowlisted loopback server,
    /// exchanges a line on both backends, and a non-allowlisted address FAILS
    /// on both.
    #[test]
    fn net_capability_compiles_to_wasm_and_confines() {
        use crate::runtime::{Capabilities, Runtime};
        use std::io::{BufRead, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("127.0.0.1:{}", server.local_addr().unwrap().port());
        // One echo exchange per backend run.
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = server.accept().expect("accept");
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read");
                let reply = format!("echo: {}\n", line.trim_end());
                reader.get_mut().write_all(reply.as_bytes()).expect("write");
            }
        });

        let src = format!(
            "fn main(console: Console, net: Net):\n    let sock = connect(net, \"{addr}\")\n    send_line(sock, \"hello\")\n    print(console, recv_line(sock))\n    close(sock)\n"
        );
        let want = vec!["echo: hello".to_string()];
        assert_eq!(
            interpreter::run_with(&src, ".", vec![addr.clone()]).expect("interp"),
            want,
            "interpreter"
        );
        let module = parser::parse_module(&src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr.clone()]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        handle.join().expect("server thread");

        // A non-allowlisted address fails on BOTH backends.
        let bad = "fn main(console: Console, net: Net):\n    let sock = connect(net, \"127.0.0.1:1\")\n    print(console, \"connected\")\n";
        assert!(
            interpreter::run_with(bad, ".", vec![addr.clone()]).is_err(),
            "interp must reject a non-allowlisted address"
        );
        let m = parser::parse_module(bad).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut a = rt
            .spawn(
                &wbytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(a.run().is_err(), "WASM must trap on a non-allowlisted address");
    }

    /// Net verbs are enforced at the GRANT: a listening module cannot
    /// instantiate under a connect-only grant, and any net import fails with no
    /// grant at all.
    #[test]
    fn net_rights_enforced_at_instantiation() {
        use crate::runtime::{Capabilities, Runtime};
        let listener_src = "fn main(console: Console, net: Net):\n    let l = listen(net, \"127.0.0.1:39999\")\n    print(console, \"listening\")\n";
        let m = parser::parse_module(listener_src).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &wbytes,
            Capabilities {
                print: true,
                quiet: true,
                net_allow: Some(vec!["127.0.0.1:39999".into()]),
                net_connect: true,
                net_listen: false,
                ..Default::default()
            },
            64,
        );
        assert!(denied.is_err(), "listen import must not instantiate under connect-only");
        let client = "fn main(console: Console, net: Net):\n    let s = connect(net, \"127.0.0.1:1\")\n    print(console, \"x\")\n";
        let m = parser::parse_module(client).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &wbytes,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.is_err(), "net import must not instantiate without a Net grant");
    }

    /// Rights are enforced at the GRANT: a module that imports a write operation
    /// cannot even instantiate under a read-only Dir grant, and any Dir import
    /// fails with no grant at all.
    #[test]
    fn dir_rights_enforced_at_instantiation() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_dir_rights_{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("mkdir");
        let writer = "fn main(console: Console, dir: Dir):\n    write(dir, \"x.txt\", \"data\")\n    print(console, \"wrote\")\n";
        let module = parser::parse_module(writer).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &bytes,
            Capabilities {
                print: true,
                quiet: true,
                dir_root: Some(root.clone()),
                dir_read: true,
                dir_write: false,
                ..Default::default()
            },
            64,
        );
        assert!(denied.is_err(), "write import must not instantiate under a read-only grant");
        let reader = "fn main(console: Console, dir: Dir):\n    print(console, read(dir, \"x.txt\"))\n";
        let m = parser::parse_module(reader).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            &wbytes,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.is_err(), "Dir import must not instantiate without a Dir grant");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The enforcement half: a module that imports `now`/`env_*` but was NOT
    /// granted Clock/Env must fail at instantiation — the host function simply
    /// is not linked, so the authority is structurally absent.
    #[test]
    fn ungranted_clock_and_env_fail_to_instantiate() {
        use crate::runtime::{Capabilities, Runtime};
        let srcs = [
            "fn main(console: Console, clock: Clock):\n    print(console, __render(now(clock)))\n",
            "import option\n\nfn main(console: Console, env: Env):\n    match get_env(env, \"X\"):\n        Some(v) -> print(console, v)\n        None -> print(console, \"unset\")\n",
        ];
        for src in srcs {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            let bytes = codegen::compile_module_binary(&linked)
                .expect("compile")
                .expect("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("runtime");
            let denied = rt.spawn(
                &bytes,
                Capabilities { print: true, ..Default::default() },
                4,
            );
            assert!(denied.is_err(), "ungranted Clock/Env import must fail to instantiate");
        }
    }

    /// Structural `==`/`!=` on compound values (lists, nested lists, tuples,
    /// records, lists of records) must agree on both backends. WASM previously
    /// compared heap POINTERS, so two equal-but-distinct values compared unequal;
    /// codegen now derives the operands' `EqShape` and routes through generated
    /// per-shape structural-equality helpers. (Regression for the silent
    /// compound-`==` pointer-compare divergence.)
    #[test]
    fn structural_equality_agrees_on_both_backends() {
        let src = "type Pt:\n    x: Int\n    y: Int\ntype Bag:\n    items: List(Int)\nfn main(console: Console):\n    print(console, __render([1, 2, 3] == [1, 2, 3]))\n    print(console, __render([1, 2, 3] == [1, 9, 3]))\n    print(console, __render([[1], [2]] == [[1], [2]]))\n    print(console, __render((1, \"a\") == (1, \"a\")))\n    print(console, __render((1, \"a\") != (1, \"b\")))\n    print(console, __render(Pt(1, 2) == Pt(1, 2)))\n    print(console, __render(Pt(1, 2) == Pt(3, 4)))\n    print(console, __render([Pt(1, 2)] == [Pt(1, 2)]))\n    print(console, __render(Bag([1, 2]) == Bag([1, 2])))\n    print(console, __render([\"a\", \"b\"] == [\"a\", \"b\"]))\n";
        let want = vec![
            "true".to_string(),  // [1,2,3] == [1,2,3]
            "false".to_string(), // [1,2,3] == [1,9,3]
            "true".to_string(),  // nested lists
            "true".to_string(),  // tuple ==
            "true".to_string(),  // tuple != (differs)
            "true".to_string(),  // record ==
            "false".to_string(), // record == (differs)
            "true".to_string(),  // list of records
            "true".to_string(),  // record with a List field
            "true".to_string(),  // list of strings
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// Structural `==` on sum types: nullary enums and concrete-field variants
    /// compare by tag (then by the matched variant's fields) on both backends.
    /// (Regression for the silent ADT pointer-compare divergence.)
    #[test]
    fn adt_structural_equality_agrees_on_both_backends() {
        let src = "type Color:\n    Red\n    Green\n    Blue\ntype Shape:\n    Circle(Int)\n    Square(Int)\nfn main(console: Console):\n    print(console, __render(Red == Red))\n    print(console, __render(Red == Blue))\n    print(console, __render(Circle(3) == Circle(3)))\n    print(console, __render(Circle(3) == Circle(4)))\n    print(console, __render(Circle(3) == Square(3)))\n    print(console, __render([Red, Green] == [Red, Green]))\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
            "true".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
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
        // each hole in `unwrap(wrap(…))`. The `Wrapped` type + `wrap`/`unwrap`
        // helpers exercise the reachable-TYPES half of the prune (the tag's
        // signature/body reach `Wrapped`, so it must be kept for the comptime
        // program to type-check), and prove a tag works when defined in an
        // IMPORTED rune, not just locally.
        let widget = "type Wrapped:\n    Wrap(String)\n\npub fn unwrap(w: Wrapped) -> String:\n    match w:\n        Wrap(s) -> s\n\npub fn wrap(s: String) -> Wrapped:\n    Wrap(s)\n\npub fn box(parts: List(String), holes: List(String)) -> String:\n    var out = \"unwrap(wrap(\\\"\"\n    var i = 0\n    let n = list.length(parts)\n    for p in parts:\n        out = out + p\n        if i < n - 1:\n            out = out + \"\\\" + \" + list.at(holes, i) + \" + \\\"\"\n        i = i + 1\n    out + \"\\\"))\"\n";
        // The CONSUMER: the tag appears in `render`, a NON-`main` function. This is
        // the exact shape that recursed before the fix (cf. glamour's `view`).
        let app = "import widget\n\nfn render(x: String) -> String:\n    box\"[${x}]\"\n\nfn main(console: Console):\n    print(console, render(\"hi\"))\n";

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
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(&bytes, Capabilities { print: true, ..Default::default() }, 4)
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
    }

    /// Interpolating a record field — `"${p.x}"` (scalar) and `"${p.tags}"`
    /// (compound) — renders on WASM, including inside a custom `Show` impl. A
    /// field access previously resolved to no value type, so `to_string` of it
    /// errored on the compiled backend even though the field's type is known.
    #[test]
    fn record_field_interpolation_renders_on_wasm() {
        let src = "type Post:\n    title: String\n    views: Int\n    tags: List(Int)\nfn main(console: Console):\n    let p = Post(\"hi\", 9, [1, 2, 3])\n    print(console, \"${p.title} (${p.views}): ${p.tags}\")\n";
        assert_eq!(run_on_wasm(src), vec!["hi (9): [1, 2, 3]".to_string()]);
    }

    /// `Option` `==` is structural on both backends: a single-parameter generic
    /// ADT is instantiated at the comparison site from a constructor literal
    /// (sound for both operands — the type checker guarantees they share a
    /// type). Dict `==` compares entries pairwise in insertion order, exactly
    /// like the interpreter. (Closes the former loud-error gaps.)
    #[test]
    fn option_and_dict_equality_agree_on_both_backends() {
        let src = "import option\n\nfn main(console: Console):\n    print(console, __render(Some(5) == Some(5)))\n    print(console, __render(Some(5) == Some(6)))\n    print(console, __render(Some(5) == None))\n    print(console, __render(None == None))\n    print(console, __render(Some(\"a\") == Some(\"a\")))\n    print(console, __render(Some(\"a\") == Some(\"b\")))\n    let a = dict.insert(dict.insert(dict.new(), \"k\", 1), \"j\", 2)\n    let b = dict.insert(dict.insert(dict.new(), \"k\", 1), \"j\", 2)\n    let c = dict.insert(dict.insert(dict.new(), \"k\", 1), \"j\", 9)\n    let rev = dict.insert(dict.insert(dict.new(), \"j\", 2), \"k\", 1)\n    print(console, __render(a == b))\n    print(console, __render(a == c))\n    print(console, __render(a == rev))\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
            "true".to_string(),
            "true".to_string(),
            "false".to_string(),
            "true".to_string(),  // identical insert order + contents
            "false".to_string(), // differing value
            "false".to_string(), // same pairs, different insertion order
        ];
        // Dict `==` now lowers on the binary path — an insertion-order pairwise
        // compare of the entries, matching the interpreter.
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A MULTI-parameter generic ADT (`Result`, whose Ok and Err payloads are
    /// different type variables) is structural on both backends: payloads pin
    /// from constructor literals (the variant's own variables must unify with
    /// its arguments; the other variant's take a safe placeholder), from
    /// declared parameter types, and from declared function returns. (Closes
    /// the last loud equality gap.)
    #[test]
    fn result_equality_agrees_on_both_backends() {
        let src = "import result\n\nfn classify(n: Int) -> Result(Int, String):\n    if n >= 0: Ok(n) else: Err(\"negative\")\n\nfn same(a: Result(Int, String), b: Result(Int, String)) -> Bool:\n    a == b\n\nfn main(console: Console):\n    print(console, __render(classify(5) == Ok(5)))\n    print(console, __render(classify(5) == Ok(6)))\n    print(console, __render(classify(0 - 1) == Err(\"negative\")))\n    print(console, __render(classify(0 - 1) == Err(\"positive\")))\n    print(console, __render(classify(5) == Err(\"negative\")))\n    print(console, __render(same(Ok(1), Ok(1))))\n    print(console, __render(same(Err(\"a\"), Err(\"a\"))))\n    print(console, __render(same(Ok(1), Err(\"a\"))))\n    print(console, __render(Ok([1, 2]) == Ok([1, 2])))\n    print(console, __render(Ok([1, 2]) == Ok([1, 3])))\n";
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
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\nfn same(s: Stack(Int), t: Stack(Int)) -> Bool:\n    s == t\n\nfn main(console: Console):\n    print(console, __render(Push(2, Push(1, Empty)) == Push(2, Push(1, Empty))))\n    print(console, __render(Push(2, Push(1, Empty)) == Push(2, Push(9, Empty))))\n    print(console, __render(Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"a\", Empty))))\n    print(console, __render(Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"z\", Empty))))\n    print(console, __render(same(Push(1, Empty), Push(1, Empty))))\n    print(console, __render(same(Push(1, Empty), Empty)))\n";
        let want: Vec<String> = ["true", "false", "true", "false", "true", "false"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// The boundary of structural equality stays LOUD where the payload is
    /// genuinely unresolvable — and return-position inference has moved that
    /// boundary: a list-literal payload now RESOLVES (and compares content-
    /// correctly on both backends), while an empty-list payload, with nothing
    /// to pin its element type, stays a codegen error — never a silent
    /// pointer compare.
    #[test]
    fn unsupported_compound_equality_is_a_loud_error_not_silent() {
        let resolved = "import result\n\nfn wrap(x: a) -> Result(a, String):\n    Ok(x)\n\nfn main(console: Console):\n    print(console, __render(wrap([1]) == wrap([2])))\n";
        assert_eq!(interp(resolved), vec!["false"]);
        assert_eq!(wasm_run(resolved), vec!["false"], "backends agree");
        let unresolvable = "import result\n\nfn wrap(x: a) -> Result(a, String):\n    Ok(x)\n\nfn main(console: Console):\n    print(console, __render(wrap([]) == wrap([])))\n";
        let rm = parser::parse_module(unresolvable).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), rm)], "main").expect("link");
        assert!(
            codegen::compile_module_binary(&linked).is_err(),
            "an unresolvable generic payload must stay a loud codegen error"
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
                "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    print(console, __render({cmp}))\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let bytes = codegen::compile_module_binary(&module)
                .expect("compile")
                .expect("the binary path lowers this program");
            assert!(interpreter::run(&src).is_err(), "interpreter must error on `{cmp}`");
            assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on `{cmp}`");
        }
        // Ordinary float ordering and NaN equality still agree.
        let ok = "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    print(console, __render(1.5 < 2.5))\n    print(console, __render(2.5 <= 2.5))\n    print(console, __render(nan == nan))\n";
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
                "fn main(console: Console):\n    print(console, __render(string.to_int(\"{v}\")))\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let bytes = codegen::compile_module_binary(&module)
                .expect("compile")
                .expect("the binary path lowers this program");
            assert!(interpreter::run(&src).is_err(), "interpreter must error on `{v}`");
            assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on `{v}`");
        }
        // The exact i64 boundaries parse identically on both backends.
        let ok = "fn main(console: Console):\n    print(console, __render(string.to_int(\"9223372036854775807\")))\n    print(console, __render(string.to_int(\"-9223372036854775808\")))\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
        ];
        assert_eq!(interp(ok), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(ok), want, "compiled WASM must agree");
    }

    /// Indexing a list out of bounds must FAIL on both backends, not silently
    /// read adjacent heap on WASM. The compiled `$list_at` bounds-checks and traps
    /// (like division-by-zero), matching the interpreter's "index out of bounds"
    /// error. In-bounds indexing still agrees. (Regression for a silent OOB-read
    /// divergence.)
    #[test]
    fn list_index_out_of_bounds_errors_on_both_backends() {
        let oob = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    print(console, __render(list.at(xs, 5)))\n";
        let module = parser::parse_module(oob).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        assert!(interpreter::run(oob).is_err(), "interpreter must error on OOB index");
        assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on OOB index");
        // A negative index likewise traps (it used to read backwards into the heap).
        let neg = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    print(console, __render(list.at(xs, 0 - 1)))\n";
        let nmod = parser::parse_module(neg).expect("parse");
        let nbytes = codegen::compile_module_binary(&nmod)
            .expect("compile")
            .expect("the binary path lowers this program");
        assert!(interpreter::run(neg).is_err(), "interpreter must error on negative index");
        assert!(crate::run_wasm_bytes(&nbytes).is_err(), "WASM must trap on negative index");
        // In-bounds indexing still agrees.
        let ok = "fn main(console: Console):\n    let xs = [10, 20, 30]\n    print(console, __render(list.at(xs, 2)))\n";
        assert_eq!(interp(ok), vec!["30".to_string()], "interpreter");
        assert_eq!(run_on_wasm(ok), vec!["30".to_string()], "compiled WASM must agree");
    }

    /// `trim` must strip exactly the same whitespace on both backends. The WASM
    /// `$is_ws` helper handles the 6 ASCII whitespace bytes (incl. VT/FF); Rust's
    /// `str::trim` would also strip Unicode whitespace (e.g. NBSP), which WASM does
    /// not — so the interpreter is pinned to the ASCII set. Here a NBSP (U+00A0)
    /// must survive on BOTH backends, while VT/FF are stripped by both. (Regression
    /// for a silent Unicode-whitespace trim divergence.)
    #[test]
    fn trim_whitespace_set_agrees_on_both_backends() {
        // "  \t\n hi \r\x0b" -> "hi"; "\x0c x \x0c" -> "x"; NBSP stays around 'y'.
        let src = "fn main(console: Console):\n    print(console, \"[\" + string.trim(\"  \\t\\n hi \\r\u{0b}\") + \"]\")\n    print(console, \"[\" + string.trim(\"\u{0c} x \u{0c}\") + \"]\")\n    print(console, \"[\" + string.trim(\"\u{a0}y\u{a0}\") + \"]\")\n";
        let want = vec![
            "[hi]".to_string(),
            "[x]".to_string(),
            "[\u{a0}y\u{a0}]".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `to_string` of a builtin call result (`has` -> Bool, `size` -> Int) must
    /// compile and render the same on both backends — codegen knows these
    /// builtins' value types, so it picks the right formatter instead of erroring
    /// with "could not determine the value's type". (Regression for the
    /// call-result val-type gap that previously forced `int_to_string`/explicit
    /// conversion.)
    #[test]
    fn to_string_of_builtin_call_results_agrees() {
        let src = "fn main(console: Console):\n    let d = dict.insert(dict.insert(dict.new(), \"a\", 1), \"b\", 2)\n    print(console, __render(dict.contains_key(d, \"a\")))\n    print(console, __render(dict.contains_key(d, \"z\")))\n    print(console, __render(dict.length(d)))\n    print(console, __render(string.contains(\"hello\", \"ell\")))\n";
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
        let src = "fn clamp(var n: Int):\n    if (n > 10):\n        n = 10\n        return\n    n = n + 1\n\nfn main(console: Console):\n    var a = 5\n    clamp(a)\n    print(console, __render(a))\n    var b = 50\n    clamp(b)\n    print(console, __render(b))\n";
        let want = vec!["6".to_string(), "10".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// The `?` operator inside an `var` function must agree on both backends.
    /// `?` early-returns the Err, and (like the interpreter's `Flow::Return`) the
    /// var param is still written back at its value on the error path — so WASM
    /// pushes the var params before the `?`-return too. (Regression for the
    /// interpreter-only `?`-in-var gap.)
    #[test]
    fn try_in_var_fn_agrees_on_both_backends() {
        let src = "import result\n\nfn step(var n: Int, r: Result(Int, String)) -> Result(Int, String):\n    n = n + 100\n    let got = r?\n    n = n + got\n    Ok(n)\n\nfn describe(r: Result(Int, String)) -> String:\n    match r:\n        Ok(v) -> \"ok:\" + __render(v)\n        Err(e) -> \"err:\" + e\n\nfn main(console: Console):\n    var a = 1\n    let ok = step(a, Ok(5))\n    print(console, __render(a))\n    print(console, describe(ok))\n    var b = 1\n    let bad = step(b, Err(\"nope\"))\n    print(console, __render(b))\n    print(console, describe(bad))\n";
        let want = vec![
            "106".to_string(),
            "ok:106".to_string(),
            "101".to_string(),
            "err:nope".to_string(),
        ];
        // `?` inside an `var` fn now lowers on the binary path: the Err
        // early-return carries the multi-result tuple (the Err value + each var
        // param), so the var writeback still happens on the error path.
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// The `encoding` module (hex/base64) must agree on both backends. WASM
    /// bridges each `String -> String` transform to the same native registry the
    /// interpreter uses (a host import), so output is byte-for-byte identical.
    /// (Regression for the interpreter-only encoding-module gap.)
    #[test]
    fn encoding_module_agrees_on_both_backends() {
        let src = "import encoding\n\nfn main(console: Console):\n    let p = \"Hello, witchy!\"\n    let b = encoding.base64_encode(p)\n    print(console, b)\n    print(console, encoding.base64_decode(b))\n    let h = encoding.hex_encode(p)\n    print(console, h)\n    print(console, encoding.hex_decode(h))\n    print(console, encoding.base64_encode(\"foo\"))\n";
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
            .expect("compile")
            .expect("the binary path lowers this program");
        assert_eq!(link_run(src), want.clone(), "interpreter (linked)");
        assert_eq!(crate::run_wasm_bytes(&bytes).expect("wasm run"), want, "compiled WASM must agree");
    }

    /// Dict `update` (single-lookup upsert) must agree on both backends, including
    /// nested updates and a big-`Int` value. WASM lowers it to a `$dict_update`
    /// helper that reads the current value (or default), applies the closure via
    /// `call_indirect`, and reinserts — equivalent to the interpreter's
    /// `dict.insert(d, k, f(dict.get_or(d, k, default)))`. (Regression for the
    /// interpreter-only dict-upsert gap.)
    #[test]
    fn dict_update_upsert_agrees_on_both_backends() {
        let src = "fn main(console: Console):\n    let d = dict.insert(dict.insert(dict.new(), \"a\", 1), \"b\", 2)\n    let d2 = dict.update(d, \"a\", 0, fn(x: Int): x + 10)\n    let d3 = dict.update(d2, \"c\", 100, fn(x: Int): x + 1)\n    print(console, __render(dict.get_or(d3, \"a\", -1)))\n    print(console, __render(dict.get_or(d3, \"b\", -1)))\n    print(console, __render(dict.get_or(d3, \"c\", -1)))\n    print(console, __render(dict.length(d3)))\n    let counts = dict.update(dict.update(dict.new(), \"hit\", 0, fn(n: Int): n + 1), \"hit\", 0, fn(n: Int): n + 1)\n    print(console, __render(dict.get_or(counts, \"hit\", -1)))\n";
        let want = vec![
            "11".to_string(),
            "2".to_string(),
            "101".to_string(),
            "3".to_string(),
            "2".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
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
        let src = "fn main(console: Console):\n    print(console, __render(3.5))\n    print(console, __render(2.0))\n    print(console, __render(0.0 - 1.0 / 3.0))\n    print(console, __render(0.1 + 0.2))\n    print(console, __render(1000000.0))\n    print(console, __render(0.0))\n    print(console, __render(10.0 / 0.0))\n    print(console, __render((0.0 - 10.0) / 0.0))\n    print(console, __render(0.0 / 0.0))\n    print(console, __render((0.0 - 1.0) * 0.0))\n";
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

    /// A closure RETURNED from a function and bound to a `let` (currying) must
    /// keep a big `Int` result on WASM: the binding records the closure's
    /// call-return kind (from the `-> fn(...) -> RET` declaration), so the later
    /// `f(x)` recovers at i64. (Regression for the let-bound-closure-return gap.)
    #[test]
    fn wasm_big_int_through_curried_closure() {
        let src = "fn make(big: Int) -> fn(Int) -> Int:\n    fn(x: Int): x + big\n\nfn main(console: Console):\n    let f = make(5000000000)\n    print(console, __render(f(1)))\n";
        let want = vec!["5000000001".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A big `Int` destructured from a tuple RETURNED by a (monomorphized)
    /// generic function must keep its 64 bits. The tuple slots carry i64; codegen
    /// now tracks a tuple-returning function's slot types so `let (a, b) = f(...)`
    /// (direct or via a `let`) reads each at the right width.
    #[test]
    fn wasm_big_int_from_returned_tuple() {
        let src = "fn pair(x: a, y: a) -> (a, a):\n    (x, y)\n\nfn main(console: Console):\n    let (p, q) = pair(9000000000, 1)\n    print(console, __render(p))\n    print(console, __render(q))\n";
        let want = vec!["9000000000".to_string(), "1".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A `Dict` value (and key) keeps its 64 bits on WASM: the Dict now stores
    /// 16-byte entries with i64 key and i64 value slots, and `get_or` recovers the
    /// value at the default's kind. A big-Int value round-trips; a String value
    /// (a pointer in the low bits) still works. (Regression for big-Int-Dict.)
    #[test]
    fn wasm_dict_keeps_big_int_values() {
        let big = "fn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, \"k\", 9000000000)\n    print(console, __render(dict.get_or(d, \"k\", 0)))\n";
        assert_eq!(interp(big), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(big), vec!["9000000000"], "WASM");
        let s = "fn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, \"a\", \"hello\")\n    print(console, dict.get_or(d, \"a\", \"none\"))\n";
        assert_eq!(interp(s), vec!["hello"], "interpreter (string value)");
        assert_eq!(run_on_wasm(s), vec!["hello"], "WASM (string value)");
    }

    /// Iterating a `Dict`'s `dict.values()` (or binding the list) must keep big-Int
    /// values 64-bit: codegen tracks the Dict's value type from `insert` and
    /// carries it to `dict.values(d)`, so the loop variable is i64.
    #[test]
    fn wasm_dict_values_iteration_keeps_big_ints() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, \"k\", 9000000000)\n    var s = 0\n    for v in dict.values(d):\n        s = s + v\n    print(console, __render(s))\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// A big `Int` in a tuple ELEMENT of a list must survive being read back —
    /// `list.at(list_of_tuples, i)` then destructured, and `for t in list_of_tuples`.
    /// Codegen tracks a list's element-tuple slot types (literal or variable) and
    /// applies them to the `at`/loop tuple destructure. (Two-level nesting.)
    #[test]
    fn wasm_big_int_in_list_of_tuples() {
        let direct = "fn main(console: Console):\n    let (a, b) = list.at([(9000000000, 1)], 0)\n    print(console, __render(a))\n    print(console, __render(b))\n";
        assert_eq!(interp(direct), vec!["9000000000", "1"], "interpreter (direct)");
        assert_eq!(run_on_wasm(direct), vec!["9000000000", "1"], "WASM (direct)");
        let loop_src = "fn main(console: Console):\n    for t in [(9000000000, 1)]:\n        let (a, b) = t\n        print(console, __render(a))\n";
        assert_eq!(interp(loop_src), vec!["9000000000"], "interpreter (loop)");
        assert_eq!(run_on_wasm(loop_src), vec!["9000000000"], "WASM (loop)");
    }

    /// A big `Int` in a nested list (`list.at(list.at(xs, i), j)`) must survive. Codegen
    /// tracks a list-of-lists' inner element type so the inner `at` recovers it
    /// as i64. (Two levels of list nesting — e.g. a matrix row/column.)
    #[test]
    fn wasm_big_int_in_nested_list() {
        let src = "fn main(console: Console):\n    let m = [[1, 9000000000], [3, 4]]\n    print(console, __render(list.at(list.at(m, 0), 1)))\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// A generic function over `List((a, b))` (the `zip`/`unzip` shape) must keep
    /// big Ints. Monomorphization resolves `a`/`b` from the argument list's
    /// element tuple, the inner `let (x, y) = p` destructures at i64, and the
    /// `List(a)` return carries the element type. (The deepest nesting case.)
    #[test]
    fn wasm_big_int_through_list_of_tuples_generic() {
        let src = "fn firsts(ps: List((a, b))) -> List(a):\n    var out = []\n    for p in ps:\n        let (x, y) = p\n        out = list.push(out, x)\n    out\n\nfn main(console: Console):\n    print(console, __render(list.at(firsts([(9000000000, 1)]), 0)))\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// A big `Int` at ARBITRARY list-nesting depth must survive — via a chain of
    /// `at`, nested `for` loops, and a nested-list parameter. Codegen tracks a
    /// list's `(depth, scalar)` nesting (literal, variable, or declared type) and
    /// peels one level per `at`/loop, so the scalar is recovered as i64 at any
    /// depth. (Closes the recursive nested-collection class.)
    #[test]
    fn wasm_big_int_at_arbitrary_list_depth() {
        // Depth-4 `at` chain (literal).
        let chain = "fn main(console: Console):\n    let xs = [[[[9000000000]]]]\n    print(console, __render(list.at(list.at(list.at(list.at(xs, 0), 0), 0), 0)))\n";
        assert_eq!(interp(chain), vec!["9000000000"], "interpreter (at-chain)");
        assert_eq!(run_on_wasm(chain), vec!["9000000000"], "WASM (at-chain)");
        // Depth-3 nested loops through a nested-list parameter.
        let loops = "fn total(c: List(List(List(Int)))) -> Int:\n    var s = 0\n    for plane in c:\n        for row in plane:\n            for x in row:\n                s = s + x\n    s\n\nfn main(console: Console):\n    print(console, __render(total([[[9000000000]]])))\n";
        assert_eq!(interp(loops), vec!["9000000000"], "interpreter (loops/param)");
        assert_eq!(run_on_wasm(loops), vec!["9000000000"], "WASM (loops/param)");
    }

    /// A big `Int` in a tuple at the bottom of NESTED lists (`[[(big, 1)]]`)
    /// survives: the `(depth, bottom)` nesting allows a tuple bottom, so peeling
    /// to the inner list then destructuring the tuple recovers the Int as i64.
    #[test]
    fn wasm_big_int_in_nested_list_of_tuples() {
        let src = "fn main(console: Console):\n    for inner in [[(9000000000, 1)]]:\n        for t in inner:\n            let (a, b) = t\n            print(console, __render(a))\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// `to_upper`/`to_lower` now compile to WASM (ASCII case mapping), matching
    /// the interpreter's ASCII fold byte-for-byte — no longer interpreter-only.
    #[test]
    fn wasm_ascii_case_mapping() {
        let src = "fn main(console: Console):\n    print(console, string.to_upper(\"Hi, World! 9z\"))\n    print(console, string.to_lower(\"Hi, World! 9A\"))\n";
        let want = vec!["HI, WORLD! 9Z".to_string(), "hi, world! 9a".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A large `Int` carried as an `Option`/`Result` success payload must keep its
    /// 64 bits on WASM, through both `?` and a `match`. The payload field is a type
    /// variable (generic i32 ABI), so codegen would truncate; it now tracks the
    /// declared scalar payload type and recovers `Some`/`Ok` values (and `?`
    /// results) at i64. (Regression for the big-Int-through-Option/Result gap.)
    #[test]
    fn wasm_big_int_through_result_payload_and_try() {
        let src = "type Result:\n    Ok(a)\n    Err(e)\n\nfn fetch() -> Result(Int, String):\n    Ok(5000000000)\n\nfn chain() -> Result(Int, String):\n    let x = (fetch())?\n    Ok((x + 1))\n\nfn main(console: Console):\n    match chain():\n        Ok(v) -> print(console, __render(v))\n        Err(e) -> print(console, e)\n";
        let want = vec!["5000000001".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `float_to_int` on a non-finite or out-of-range Float must saturate the same
    /// way on both backends. The interpreter uses Rust's `as i64` (NaN -> 0,
    /// +inf -> i64::MAX, -inf -> i64::MIN, out-of-range clamps); WASM used the
    /// trapping `i64.trunc_f64_s` and would crash on those, so it now uses the
    /// saturating `i64.trunc_sat_f64_s`.
    #[test]
    fn wasm_float_to_int_saturates_like_the_interpreter() {
        let src = "fn main(console: Console):\n    print(console, __render(math.to_int(1.0 / 0.0)))\n    print(console, __render(math.to_int(0.0 - 1.0 / 0.0)))\n    print(console, __render(math.to_int(0.0 / 0.0)))\n    print(console, __render(math.to_int(0.0 - 3.9)))\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
            "0".to_string(),
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
        let src = "fn main(console: Console):\n    print(console, __render(string.to_int(\"5000000000\")))\n    print(console, __render(string.to_int(\"-7000000000\")))\n    print(console, __render(string.to_int(\"  42  \")))\n";
        let want = vec![
            "5000000000".to_string(),
            "-7000000000".to_string(),
            "42".to_string(),
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
    print(console, time.iso8601(time.from_unix(1780000000)))
    print(console, time.weekday_name(time.from_unix(0)) + " " + time.iso8601(time.from_unix(0)))
    print(console, time.iso8601(time.from_unix(-86401)))
    print(console, yn(time.is_leap(2000)) + yn(time.is_leap(1900)) + yn(time.is_leap(2024)))
    print(console, yn(time.to_unix(time.from_unix(1780000000)) == 1780000000))

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

    /// `std/csv` round-trips RFC-4180-ish CSV: quoted fields with embedded commas,
    /// doubled quotes (`""`), proper re-quoting on encode, and header records.
    #[test]
    fn csv_module_parses_quotes_and_encodes() {
        let src = r#"import csv
import string

fn main(console: Console):
    let text = "name,city\nAda,\"London, UK\"\nGrace,\"NY\"\"C\"\"\"\n"
    let rows = csv.parse(text)
    print(console, __render(list.length(rows)))
    print(console, list.at(list.at(rows, 1), 1))
    print(console, list.at(list.at(rows, 2), 1))
    let enc = csv.encode([["a", "b,c"], ["d\"e", "f"]])
    print(console, bs(enc == "a,\"b,c\"\n\"d\"\"e\",f\n"))
    print(console, bs(csv.encode(csv.parse(enc)) == enc))
    let recs = csv.parse_records(text)
    print(console, __render(list.length(recs)) + ":" + dict.get_or(list.at(recs, 0), "city", "?"))

fn bs(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec!["3", "London, UK", "NY\"C\"", "y", "y", "2:London, UK"]
        );
    }

    /// `std/dict` adds the compositional layer over the builtin Dict: a `get`
    /// returning `Option`, `from_pairs`, and the `map_values`/`filter`/`merge`
    /// transforms — verified against the builtin `size`/`get_or`.
    #[test]
    fn dict_module_higher_level_operations() {
        let src = r#"import dict
import string

fn main(console: Console):
    let d = dict.from_pairs([("a", 1), ("b", 2), ("c", 3)])
    print(console, __render(dict.length(d)))
    print(console, oi(dict.get(d, "b")))
    print(console, oi(dict.get(d, "z")))
    let m = dict.merge(d, dict.from_pairs([("b", 20), ("d", 4)]))
    print(console, __render(dict.get_or(m, "b", 0)) + "," + __render(dict.get_or(m, "d", 0)))
    let tens = dict.map_values(d, fn(v: Int): v * 10)
    print(console, oi(dict.get(tens, "c")))
    let evens = dict.filter(d, fn(k: String, v: Int): v % 2 == 0)
    print(console, __render(dict.length(evens)))
    print(console, bs(dict.is_empty(dict.new())))

fn oi(o: Option(Int)) -> String:
    match o:
        Some(n) -> __render(n)
        None -> "none"

fn bs(b: Bool) -> String:
    if b: "yes" else: "no"
"#;
        assert_eq!(
            link_run(src),
            vec!["3", "2", "none", "20,4", "30", "1", "yes"]
        );
    }

    /// `std/json` typed field accessors: `get_string`/`get_int`/`get_strings`/
    /// `index_string` compose `get`/`index` with the `as_*` coercions — collapsing
    /// the common "read a typed field" pattern, and yielding `[]` for an absent
    /// string array.
    #[test]
    fn json_module_typed_field_accessors() {
        let src = r#"import json
import string

fn main(console: Console):
    match json.decode("{\"name\":\"acme\",\"n\":7,\"caps\":[\"Net\",\"Console\"],\"arr\":[\"a\",\"b\"]}"):
        Ok(d) ->
            print(console, opt(json.get_string(d, "name")))
            print(console, oi(json.get_int(d, "n")))
            print(console, list.join(json.get_strings(d, "caps"), ","))
            print(console, "[" + list.join(json.get_strings(d, "absent"), ",") + "]")
        Err(e) -> print(console, "err")

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"

fn oi(o: Option(Int)) -> String:
    match o:
        Some(n) -> __render(n)
        None -> "?"
"#;
        assert_eq!(link_run(src), vec!["acme", "7", "Net,Console", "[]"]);
    }

    /// `std/fs` parent_dir + (with a real Dir) the recursive collect — exercised
    /// here for the pure part to confirm the module's functions resolve on import.
    #[test]
    fn fs_module_parent_dir_resolves() {
        let src = "import fs\nfn main(console: Console):\n    print(console, fs.parent_dir(\"a/b/c\"))\n    print(console, fs.parent_dir(\"top\"))\n";
        assert_eq!(link_run(src), vec!["a/b", ""]);
    }

    /// `std/rights` matches capability strings rights-precisely (the logic the pm
    /// check/gate and coven's publish enforcement share): a bare kind covers any
    /// rights of that kind, a bracketed one only a subset — so `Net[Connect]` does
    /// NOT cover full `Net`.
    #[test]
    fn rights_module_covers_capabilities_rights_precisely() {
        let src = r#"import rights
import string

fn main(console: Console):
    print(console, yes(rights.covers("Net", "Net[Listen]")))
    print(console, yes(rights.covers("Net[Connect]", "Net")))
    print(console, yes(rights.covers("Net[Connect, Tcp]", "Net[Connect]")))
    print(console, yes(rights.covers("Dir", "Console")))
    print(console, yes(rights.covered(["Console", "Dir[Read]"], "Dir[Read]")))
    print(console, list.join(rights.uncovered(["Net[Connect]"], ["Net", "Console"]), "|"))

fn yes(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec!["y", "n", "y", "n", "y", "Net|Console"]
        );
    }

    /// The `Clock` capability yields wall-clock time (ms since epoch) via `now`.
    /// Reading the clock is ambient nondeterminism, so it's capability-gated and
    /// surfaces in the footprint — not a pure builtin.
    #[test]
    fn clock_capability_yields_wall_clock_time() {
        let out = interp(
            "fn main(console: Console, clock: Clock):\n    print(console, __render(now(clock)))\n",
        );
        let ms: i64 = out[0].parse().expect("now should print an integer");
        assert!(ms > 1_600_000_000_000, "now should be ms since the Unix epoch (got {ms})");
        // `now` needs a Clock — calling it with another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let t = now(c)\n").is_err());
        // The Clock requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, clock: Clock):\n    let t = now(clock)\n")
                .expect("parse"),
        );
        assert!(fp.total.contains_key("Clock"), "Clock should appear in the footprint");
    }

    /// `now_monotonic(clock)` yields monotonic elapsed nanoseconds — a steady
    /// clock for measuring durations (used by the benchmark harness to time the
    /// compute kernel, excluding process startup). The absolute value is
    /// nondeterministic, so parity is asserted on a *derived* property (elapsed is
    /// non-negative and the kernel result is identical) that both backends agree on.
    #[test]
    fn now_monotonic_measures_elapsed_on_both_backends() {
        let src = "fn spin(n: Int) -> Int:\n    var a = 0\n    var i = 0\n    while i < n:\n        a = a + i\n        i = i + 1\n    a\n\nfn main(console: Console, clock: Clock):\n    let t0 = now_monotonic(clock)\n    let r = spin(1000)\n    let t1 = now_monotonic(clock)\n    print(console, \"${r}\")\n    print(console, \"${t1 - t0 >= 0}\")\n";
        let expected = vec!["499500".to_string(), "true".to_string()];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
        // Like `now`, it needs a Clock — another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let t = now_monotonic(c)\n").is_err());
        // The Clock requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module(
                "fn main(console: Console, clock: Clock):\n    let t = now_monotonic(clock)\n",
            )
            .expect("parse"),
        );
        assert!(fp.total.contains_key("Clock"), "Clock should appear in the footprint");
    }

    /// (RFC-0038) A bare grantable capability granted to `main` mints an identical
    /// sealed record on BOTH backends: the interpreter builds a `Value::Ctor` from
    /// the grant fields; the compiled backend stages each field host-side and
    /// wraps them in a record via `mk{N}`. The two must agree bit-for-bit.
    #[test]
    fn grantable_user_cap_mints_identically_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "grantable capability UiRoot:\n    policy: String\n    app_id: String\n\nfn descr(u: UiRoot) -> String:\n    match u:\n        UiRoot(p, a) -> p + \"@\" + a\n\nfn main(console: Console, ui: UiRoot):\n    print(console, descr(ui))\n";
        let expected = vec!["coven-web@web".to_string()];

        // Interpreter: grant keyed by param name -> field values.
        let module = parser::parse_module(src).expect("parse");
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("policy".to_string(), "coven-web".to_string());
        fields.insert("app_id".to_string(), "web".to_string());
        let mut grants = std::collections::BTreeMap::new();
        grants.insert("ui".to_string(), fields);
        assert_eq!(
            interpreter::run_module_user_caps(module, ".", vec![], vec![], vec![], grants).expect("interp"),
            expected,
            "interp"
        );

        // Compiled: field values staged host-side in declaration order.
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    quiet: true,
                    user_cap_fields: vec![vec!["coven-web".to_string(), "web".to_string()]],
                    ..Default::default()
                },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), expected, "compiled WASM must agree");
    }

    /// The `Env` capability reads process environment variables via `get_env`,
    /// returning `Option(String)` (None when unset). Reading the environment is
    /// ambient authority, so it's capability-gated and surfaces in the footprint.
    #[test]
    fn env_capability_reads_environment_variables() {
        // A definitely-unset variable yields None.
        let out = interp(
            "fn main(console: Console, env: Env):\n    match get_env(env, \"WITCHY_NOPE_UNSET_VAR\"):\n        Some(v) -> print(console, v)\n        None -> print(console, \"unset\")\n",
        );
        assert_eq!(out, vec!["unset"]);
        // `get_env` needs an Env capability — another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let x = get_env(c, \"X\")\n").is_err());
        // The Env requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, env: Env):\n    let x = get_env(env, \"X\")\n")
                .expect("parse"),
        );
        assert!(fp.total.contains_key("Env"), "Env should appear in the footprint");
    }

    /// `main` may declare a `List(String)` parameter to receive command-line
    /// arguments — argv is input data, not authority, so it's an ordinary value
    /// parameter passed by the host (here `run_module_args`), not a capability.
    #[test]
    fn main_receives_command_line_args() {
        let run = |args: Vec<String>| -> Vec<String> {
            let src = "import string\nfn main(console: Console, args: List(String)):\n    print(console, list.join(args, \",\"))\n";
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
    print(console, __render(acc))
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
    print(console, __render(((q).x * (q).y)))
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
    print(console, __render(sum([1, 2, 3, 4, 5])))
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
    print(console, __render((area(Circle(5)) + area(Rect(3, 4)))))
"#,
            ),
            (
                "capturing closures + higher-order",
                r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let k = 100
    print(console, __render(apply(fn(n: Int): (n + k), 5)))
"#,
            ),
            (
                "dicts",
                r#"
fn main(console: Console):
    var d = dict.new()
    d = dict.insert(d, "a", 1)
    d = dict.insert(d, "b", 2)
    d = dict.insert(d, "a", 9)
    print(console, __render((dict.get_or(d, "a", 0) + dict.length(d))))
"#,
            ),
            (
                "strings",
                r#"
fn main(console: Console):
    print(console, string.replace("a,b,c", ",", "-"))
    print(console, __render(string.index_of("hello", "l")))
    print(console, string.substring("hello", 1, 4))
    for w in string.split("the cat sat", " "):
        print(console, w)
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
    let words = string.split("apple banana apple cherry apple", " ")
    print(console, __render(count_matches(words, "apple")))
"#,
            ),
            (
                "string equality + ordering",
                r#"
fn main(console: Console):
    let a = string.substring("xapple", 1, 6)
    print(console, __render((a == "apple")))
    print(console, __render((a == "apricot")))
    print(console, __render((a != "apricot")))
    print(console, __render(("apple" < "banana")))
    print(console, __render(("banana" < "apple")))
    print(console, __render(("app" < "apple")))
    print(console, __render(("apple" <= "apple")))
"#,
            ),
            (
                "tuples + polymorphic to_string",
                r#"
fn main(console: Console):
    let (a, b) = (7, 8)
    print(console, __render((a + b)))
    print(console, __render((a < b)))
    print(console, __render("done"))
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
    print(console, describe(0 - 4250))
    print(console, describe(150000))
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

    /// The std `list` library is the most-exercised witchy code; verify a broad
    /// slice of it (reverse/take/drop/sort_by/zip/enumerate/map/filter/fold/
    /// index_of/contains/any/all) produces identical results in the interpreter
    /// and compiled to WASM. Int element lists keep this clear of the known
    /// generic-`==`-on-strings limitation (compiled compares those by pointer).
    #[test]
    fn std_list_library_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let xs = [5, 3, 8, 1, 9, 2]
    let rev = list.reverse(xs)
    print(console, ((__render(list.at(rev, 0)) + ",") + __render(list.at(rev, 5))))
    print(console, ((__render(list.length(list.take(xs, 3))) + ":") + __render(list.at(list.take(xs, 3), 2))))
    print(console, __render(list.at(list.drop(xs, 4), 0)))
    let sorted = list.sort_by(xs, fn(a: Int, b: Int): (a < b))
    print(console, ((__render(list.at(sorted, 0)) + "..") + __render(list.at(sorted, 5))))
    let pairs = list.zip([1, 2, 3], [10, 20, 30])
    let (pa, pb) = list.at(pairs, 1)
    print(console, __render((pa + pb)))
    let en = list.enumerate([100, 200])
    let (ei, ev) = list.at(en, 1)
    print(console, __render(((ei * 1000) + ev)))
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    let evens = list.filter(xs, fn(n: Int): ((n % 2) == 0))
    print(console, __render(list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))))
    print(console, __render(list.length(evens)))
    print(console, __render(list.index_of(xs, 8)))
    print(console, __render(list.contains(xs, 9)))
    print(console, __render(list.any(xs, fn(n: Int): (n > 8))))
    print(console, __render(list.all(xs, fn(n: Int): (n > 0))))
    print(console, __render(list.sum(xs)))
    print(console, __render(list.is_empty(xs)))
    print(console, __render(list.is_empty(list.filter(xs, fn(n: Int): (n > 100)))))
    print(console, __render(list.count(xs, fn(n: Int): ((n % 2) == 0))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(
            interpreted, compiled,
            "std list library diverged between interpreter and compiled"
        );
    }

    #[test]
    fn generic_function_with_match_body_runs_at_multiple_types() {
        // A *single* generic function whose body binds its type param (a match)
        // may be called at different type instantiations in one program. `unwrap`
        // is used at Box(Int) and Box(String); both backends agree.
        let src = r#"
type Box:
    Wrap(a)

fn unwrap(b: Box(a), default: a) -> a:
    match b:
        Wrap(v) -> v

fn main(console: Console):
    print(console, __render(unwrap(Wrap(42), 0)))
    print(console, unwrap(Wrap("hello"), "none"))
"#;
        assert_eq!(interp(src), vec!["42", "hello"]);
        assert_eq!(run_on_wasm(src), vec!["42", "hello"]);
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
    print(console, __render(result.unwrap_or(add_two(3, 4), 0)))
    print(console, __render(result.unwrap_or(add_two(3, (0 - 1)), 0)))
    print(console, __render(result.is_err(add_two((0 - 5), 2))))
    print(console, __render(result.is_ok(add_two(10, 20))))
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
    print(console, __render(need(Some(5))))
    print(console, __render(need(None)))
    print(console, __render(rewrap(Ok(9))))
    print(console, __render(rewrap(Err("boom"))))
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
    print(console, __render(option.unwrap_or(first_even(4, 6), 0)))
    print(console, __render(option.unwrap_or(first_even(4, 7), 0)))
    print(console, __render(option.is_none(first_even(3, 8))))
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
    let words = ["cherry", "apple", "banana", "date", "apple"]
    let sorted = list.sort_by(words, fn(a: String, b: String): (a < b))
    for w in sorted:
        print(console, w)
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
    fn std_list_find_index_backends_agree() {
        // find_index returns the position of the first predicate match, or -1.
        let client = r#"
import list

fn main(console: Console):
    let xs = [3, 8, 1, 9, 4]
    print(console, __render(list.find_index(xs, fn(n: Int): (n > 5))))
    print(console, __render(list.find_index(xs, fn(n: Int): (n > 100))))
    print(console, __render(list.find_index(xs, fn(n: Int): (n == 1))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "find_index diverged");
        assert_eq!(compiled, vec!["1", "-1", "2"]);
    }

    #[test]
    fn std_list_zip_with_intersperse_backends_agree() {
        // zip_with combines element-wise (stopping at the shorter list);
        // intersperse inserts a separator between elements. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    let sums = list.zip_with([1, 2, 3], [10, 20], fn(a: Int, b: Int): (a + b))
    print(console, __render(list.length(sums)))
    print(console, __render(list.sum(sums)))
    let spaced = list.intersperse([5, 6, 7], 0)
    print(console, __render(list.length(spaced)))
    print(console, __render(list.sum(spaced)))
    print(console, __render(list.length(list.intersperse([9], 0))))
    print(console, __render(list.length(list.intersperse([], 0))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "zip_with/intersperse diverged");
        assert_eq!(compiled, vec!["2", "33", "5", "18", "1", "0"]);
    }

    #[test]
    fn std_list_take_drop_while_repeat_backends_agree() {
        // take_while/drop_while split at the first failing element; repeat makes
        // n copies. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 10, 4, 5]
    print(console, __render(list.sum(list.take_while(xs, fn(n: Int): (n < 5)))))
    print(console, __render(list.sum(list.drop_while(xs, fn(n: Int): (n < 5)))))
    let threes = list.repeat(7, 3)
    print(console, __render(list.sum(threes)))
    print(console, __render(list.length(threes)))
    print(console, __render(list.length(list.repeat(9, 0))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "take_while/drop_while/repeat diverged");
        assert_eq!(compiled, vec!["6", "19", "21", "3", "0"]);
    }

    #[test]
    fn std_list_flatten_flatmap_backends_agree() {
        // flatten / flat_map (concat-based, with a list-returning closure for
        // flat_map) behave identically in both backends.
        let client = r#"
import list

fn main(console: Console):
    let nested = [[1, 2], [3], [4, 5, 6]]
    let flat = list.flatten(nested)
    print(console, __render(list.length(flat)))
    print(console, __render(list.sum(flat)))
    let fm = list.flat_map([1, 2, 3], fn(n: Int): [n, (n * 10)])
    print(console, __render(list.length(fm)))
    print(console, __render(list.sum(fm)))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "flatten/flat_map diverged");
        assert_eq!(compiled, vec!["6", "21", "6", "66"]);
    }

    #[test]
    fn unwrap_or_else_backends_agree() {
        // Lazy defaults via a zero-arg closure, for both Option and Result.
        let opt = r#"
import option

fn main(console: Console):
    print(console, __render(option.unwrap_or_else(Some(5), fn(): 0)))
    let fallback = 99
    print(console, __render(option.unwrap_or_else(option.filter(Some(3), fn(n: Int): (n > 10)), fn(): fallback)))
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
    print(console, __render(result.unwrap_or_else(checked(7), fn(): 0)))
    print(console, __render(result.unwrap_or_else(checked((0 - 1)), fn(): 42)))
"#;
        let rsrc = [("result", crate::bundled_module("result").unwrap()), ("main", res)];
        assert_eq!(
            interpreter::run_program(&rsrc, "main").expect("interp"),
            run_linked_on_wasm(&rsrc, "main")
        );
        assert_eq!(run_linked_on_wasm(&rsrc, "main"), vec!["7", "42"]);
    }

    #[test]
    fn std_option_combinators_backends_agree() {
        // is_none / and_then / filter behave identically in both backends.
        let client = r#"
import option

fn main(console: Console):
    let s = Some(5)
    print(console, __render(option.is_none(s)))
    print(console, __render(option.is_none(option.filter(s, fn(n: Int): (n > 10)))))
    let chained = option.and_then(s, fn(n: Int): Some((n * 2)))
    print(console, __render(option.unwrap_or(chained, 0)))
    let kept = option.filter(s, fn(n: Int): (n > 0))
    print(console, __render(option.unwrap_or(kept, 0)))
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option combinators diverged");
    }

    // flatten collapses Option(Option(a)) one level; zip pairs two options into
    // Option((a, b)) only when both are Some. Both backends agree.
    #[test]
    fn std_option_flatten_zip_backends_agree() {
        let client = r#"
import option

fn nested(n: Int) -> Option(Option(Int)):
    if (n > 0):
        Some(Some(n))
    else:
        Some(None)

fn main(console: Console):
    print(console, __render(option.unwrap_or(option.flatten(nested(7)), (0 - 1))))
    print(console, __render(option.unwrap_or(option.flatten(nested(0)), (0 - 1))))
    match option.zip(Some(3), Some(4)):
        Some(pair) ->
            let (x, y) = pair
            print(console, __render((x + y)))
        None -> print(console, "none")
    print(console, __render(option.is_none(option.zip(Some(1), None))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option flatten/zip diverged");
        assert_eq!(compiled, vec!["7", "-1", "7", "true"]);
    }

    #[test]
    fn std_option_or_mapor_backends_agree() {
        // The fallback combinators: `or` / `or_else` keep a Some or supply an
        // alternative (eagerly / lazily), and `map_or` transforms a Some or
        // returns the default for None. Both backends agree.
        let client = r#"
import option

fn main(console: Console):
    print(console, __render(option.unwrap_or(option.or(Some(5), Some(9)), 0)))
    print(console, __render(option.unwrap_or(option.or(None, Some(9)), 0)))
    print(console, __render(option.unwrap_or(option.or_else(None, fn(): Some(7)), 0)))
    print(console, __render(option.unwrap_or(option.or_else(Some(3), fn(): Some(7)), 0)))
    print(console, __render(option.map_or(Some(10), 0, fn(x: Int): (x * 2))))
    print(console, __render(option.map_or(None, 99, fn(x: Int): (x * 2))))
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option or/map_or diverged");
        assert_eq!(compiled, vec!["5", "9", "7", "3", "20", "99"]);
    }

    #[test]
    fn std_eq_member_backends_agree() {
        // The Eq trait + the bounded `member` / `index_of` give content-correct
        // equality on BOTH backends — even for runtime-BUILT strings, where a
        // generic `==` search does pointer comparison in compiled code and would
        // wrongly miss. A user `impl Eq` (Box) works, as does the default `ne`.
        let client = r#"
import cmp

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
    while (i < string.char_count(s)):
        acc = (acc + string.substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("apple"), build("banana")]
    print(console, __render(cmp.member(words, build("banana"))))
    print(console, __render(cmp.member(words, build("cherry"))))
    print(console, __render(cmp.index_of([10, 20, 30], 20)))
    print(console, __render(cmp.index_of([10, 20, 30], 99)))
    print(console, __render(cmp.member([Box(1), Box(2)], Box(2))))
    print(console, __render(ne(Box(1), Box(2))))
    print(console, __render(ne(Box(2), Box(2))))
"#;
        let sources = [("cmp", crate::bundled_module("cmp").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std eq member/index_of diverged");
        assert_eq!(compiled, vec!["true", "false", "1", "-1", "true", "true", "false"]);
    }

    #[test]
    fn std_eq_count_unique_backends_agree() {
        // `eq.count` / `eq.unique` dispatch through the element type's Eq impl, so
        // they are content-correct on BOTH backends — including runtime-built
        // strings, where `list.unique`'s generic `==` compares pointers and fails
        // to dedupe in compiled code. A user `impl Eq` works too (Tag).
        let client = r#"
import cmp
import string

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
    while (i < string.char_count(s)):
        acc = (acc + string.substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("a"), build("b"), build("a"), build("c"), build("b"), build("a")]
    print(console, __render(cmp.count(words, build("a"))))
    print(console, __render(cmp.count(words, build("z"))))
    print(console, list.join(cmp.unique(words), ","))
    print(console, __render(list.length(cmp.unique([Tag(1), Tag(2), Tag(1), Tag(2), Tag(3)]))))
    print(console, __render(cmp.count([Tag(1), Tag(2), Tag(1)], Tag(1))))
"#;
        let sources = [
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std eq count/unique diverged");
        assert_eq!(compiled, vec!["3", "0", "a,b,c", "3", "2"]);
    }

    #[test]
    fn std_set_operations_backends_agree() {
        // Set ops dispatch through Eq (cross-module: set -> eq.member, both
        // bounded generics), so they are content-correct on both backends for
        // runtime-built strings and a user Eq type (Id), and dedupe along the way.
        let client = r#"
import set
import string

type Id:
    Id(Int)

impl PartialEq for Id:
    fn eq(self, other: Self) -> Bool:
        match self:
            Id(a) -> match other:
                Id(b) -> (a == b)

impl Eq for Id

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < string.char_count(s)):
        acc = (acc + string.substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let a = set.from_list([build("x"), build("y"), build("x")])
    let b = set.from_list([build("y"), build("z")])
    let u = set.union(a, b)
    let i = set.intersection(a, b)
    let d = set.difference(a, b)
    print(console, list.join(set.to_list(u), ","))
    print(console, list.join(set.to_list(i), ","))
    print(console, list.join(set.to_list(d), ","))
    print(console, __render(set.is_subset(set.from_list([build("y")]), a)))
    print(console, __render(set.is_subset(set.from_list([build("z")]), a)))
    let ids = set.union(set.from_list([Id(1), Id(2), Id(1)]), set.from_list([Id(2), Id(3)]))
    print(console, __render(set.length(ids)))
"#;
        let sources = [
            ("set", crate::bundled_module("set").unwrap()),
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std set ops diverged");
        assert_eq!(
            compiled,
            vec!["x,y,z", "y", "x", "true", "false", "3"]
        );
    }

    /// The first-class `Set(a)` type: construction, membership, `for x in set`
    /// iteration (IntoIter-style), removal, and collecting an iterator into a set
    /// (`set.from_list(iter.collect(...))`) — identical on both backends.
    #[test]
    fn std_set_type_iteration_and_collect_agree() {
        let client = "import set\nimport iter\n\nfn main(console: Console):\n    let s = set.from_list([3, 1, 2, 3, 1])\n    print(console, __render(set.length(s)))\n    print(console, __render(set.contains(s, 2)))\n    var total = 0\n    for x in s:\n        total = (total + x)\n    print(console, __render(total))\n    let r = set.remove(s, 2)\n    print(console, set.show(r))\n    let cs: Set(Int) = iter.collect(iter.range(1, 4))\n    print(console, set.show(cs))\n";
        let sources = [
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("iter", crate::bundled_module("iter").unwrap()),
            ("set", crate::bundled_module("set").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "set type diverged");
        assert_eq!(compiled, vec!["3", "true", "6", "{3, 1}", "{1, 2, 3}"]);
    }

    #[test]
    fn std_ascii_classification_backends_agree() {
        // ASCII predicates are implemented purely via string comparison, so they
        // must agree across the interpreter and the compiled backend. Also drives
        // a tiny tokenizer-style use: sum the digit values in a string.
        let client = r#"
import ascii
import string

fn digit_sum(s: String) -> Int:
    var total = 0
    var i = 0
    while (i < string.char_count(s)):
        let c = string.char_at(s, i)
        if ascii.is_digit(c):
            total = (total + ascii.to_digit(c))
        i = (i + 1)
    total

fn main(console: Console):
    print(console, __render(ascii.is_digit("7")))
    print(console, __render(ascii.is_digit("x")))
    print(console, __render(ascii.is_alpha("Q")))
    print(console, __render(ascii.is_alnum("_")))
    print(console, __render(ascii.is_space("\t")))
    print(console, __render(ascii.to_digit("4")))
    print(console, __render(ascii.to_digit("z")))
    print(console, __render(digit_sum("a1b2c3")))
    print(console, __render(ascii.all_digits("12345")))
    print(console, __render(ascii.all_digits("12a45")))
    print(console, __render(ascii.all_digits("")))
    print(console, __render(ascii.all_digits("0")))
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
        // `show.show_list` renders a list via the element type's Show impl, so it
        // works for a user type (Coord) that the built-in to_string cannot print.
        // Monomorphized dispatch keeps it content-correct on both backends.
        let client = r#"
import show

type Coord:
    Coord(Int, Int)

impl Show for Coord:
    fn show(self) -> String:
        match self:
            Coord(x, y) -> (((("(" + __render(x)) + ",") + __render(y)) + ")")

fn main(console: Console):
    print(console, show.show_list([1, 2, 3]))
    print(console, show.show_list(["a", "b"]))
    print(console, show.show_list([Coord(0, 0), Coord(1, 2)]))
    print(console, show.show_list([true, false]))
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
    fn multi_statement_match_arm_body_indented() {
        // A match arm with a multi-statement body, brace-free: `Pat ->` opens an
        // indented block. Both backends agree.
        let client = "type Cmd:\n    Inc\n    Dec\n\nfn apply(n: Int, c: Cmd) -> Int:\n    match c:\n        Inc ->\n            let m = n + 1\n            m\n        Dec ->\n            n - 1\n\nfn main(console: Console):\n    print(console, __render(apply(10, Inc)))\n    print(console, __render(apply(10, Dec)))\n";
        assert_eq!(interp(client), vec!["11", "9"]);
        assert_eq!(run_on_wasm(client), vec!["11", "9"]);
    }

    #[test]
    fn brace_free_record_update_form() {
        // `update e: field = value ...` — brace-free record update (one or more
        // whitespace-separated `name = value` overrides). Both backends agree.
        let client = r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(1, 2)
    let q = Point(x: ((p).x + 10), ..p)
    print(console, __render(((q).x + (q).y)))
    let r = Point(x: 5, y: 6, ..p)
    print(console, __render(((r).x + (r).y)))
"#;
        assert_eq!(interp(client), vec!["13", "11"]);
        assert_eq!(run_on_wasm(client), vec!["13", "11"]);
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
    print(console, __render(list.fold(signs, 0, fn(a: Int, b: Int): (a + b))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "inline if-else diverged");
        assert_eq!(compiled, vec!["1"]);
    }

    #[test]
    fn brace_free_lambda_form() {
        // `fn(params): expr` — the brace-free single-expression lambda, used
        // inline inside call parens where layout is suppressed. Both backends.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 4]
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    print(console, __render(list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))))
    print(console, __render(list.length(list.filter(xs, fn(n: Int): ((n % 2) == 0)))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "brace-free lambda diverged");
        assert_eq!(compiled, vec!["20", "2"]);
    }

    #[test]
    fn inherent_impl_in_indentation_syntax() {
        // The inherent impl works under the off-side rule too: `impl Point:`.
        let client = "type Point:\n    Point(Int, Int)\n\nimpl Point:\n    fn sum(self) -> Int:\n        match self:\n            Point(x, y) -> x + y\n\nfn main(console: Console):\n    print(console, __render(sum(Point(4, 5))))\n";
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
    print(console, __render(mag(Point(3, 4))))
    print(console, __render(mag(Circle(6))))
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
    fn push(self, x: a) -> Stack(a):
        Stack(list.push(self.items, x))
    fn howbig(self) -> Int:
        list.length(self.items)

fn main(console: Console):
    let s = Stack.empty().push(1).push(2).push(3)
    print(console, __render(s.howbig()))
    let w = Stack.empty().push("a").push("b")
    print(console, __render(w.howbig()))
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
    print(console, show(Coord(3, 4)))
    print(console, show(Named("p", Coord(1, 2))))
    print(console, show.show_list([Coord(0, 0), Coord(5, 6)]))
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

    #[test]
    fn json_encode_pretty_backends_agree() {
        let client = r#"
import json
fn main(console: Console):
    let doc = JsonObject([("name", JsonString("witchy")), ("tags", JsonArray([JsonInt(1), JsonInt(2)])), ("empty", JsonArray([]))])
    print(console, json.encode_pretty(doc))
"#;
        let sources = [("json", crate::bundled_module("json").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "encode_pretty diverged");
        assert_eq!(
            compiled,
            vec!["{\n  \"name\": \"witchy\",\n  \"tags\": [\n    1,\n    2\n  ],\n  \"empty\": []\n}"]
        );
    }

    /// `string_chars` (the O(n) string -> List(String) primitive behind a fast
    /// `to_chars`) agrees across the interpreter and WASM —
    /// including a multi-byte (UTF-8) character. Counted by Unicode scalar.
    #[test]
    fn string_chars_backends_agree() {
        let src = "fn main(console: Console):\n    let cs = string.chars(\"café\")\n    print(console, __render(list.length(cs)))\n    print(console, list.at(cs, 0))\n    print(console, list.at(cs, 3))\n";
        let expected = vec!["4".to_string(), "c".to_string(), "é".to_string()];
        // Interpreter (source of truth).
        assert_eq!(interpreter::run(src).expect("interp"), expected);
        // WASM.
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm diverged");
    }

    #[test]
    fn json_as_object_backends_agree() {
        // as_object exposes an object's key/value pairs for iteration when the
        // keys aren't known ahead of time; a non-object yields None.
        let client = r#"
import json
import option
fn main(console: Console):
    match json.decode("{\"a\": 1, \"b\": 2}"):
        Ok(doc) ->
            match json.as_object(doc):
                Some(pairs) ->
                    for p in pairs:
                        let (k, _v) = p
                        print(console, k)
                None -> print(console, "not object")
        Err(_e) -> print(console, "err")
    print(console, if option.is_none(json.as_object(JsonInt(5))): "none" else: "some")
"#;
        let sources = [
            ("json", crate::bundled_module("json").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "as_object diverged");
        assert_eq!(compiled, vec!["a", "b", "none"]);
    }

    #[test]
    fn list_range_between_and_step_backends_agree() {
        // range_between is the half-open lo..hi; range_step counts by `step`,
        // ascending or descending, and yields [] when step is 0.
        let client = r#"
import list
import string
fn show_ints(xs: List(Int)) -> String:
    list.join(list.map(xs, fn(n: Int): __render(n)), ",")
fn main(console: Console):
    print(console, show_ints(list.range_between(2, 6)))
    print(console, show_ints(list.range_between(5, 5)))
    print(console, show_ints(list.range_step(0, 10, 3)))
    print(console, show_ints(list.range_step(5, 0, -2)))
    print(console, show_ints(list.range_step(0, 5, 0)))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "range_between/range_step diverged");
        assert_eq!(compiled, vec!["2,3,4,5", "", "0,3,6,9", "5,3,1", ""]);
    }

    #[test]
    fn set_symmetric_difference_and_disjoint_backends_agree() {
        // symmetric_difference composes difference+union (so it de-dups);
        // is_disjoint is true exactly when the intersection is empty.
        let client = r#"
import set
import list
import string
fn show_ints(xs: List(Int)) -> String:
    list.join(list.map(xs, fn(n: Int): __render(n)), ",")
fn main(console: Console):
    let sd1 = set.symmetric_difference(set.from_list([1, 2, 3]), set.from_list([2, 3, 4]))
    let sd2 = set.symmetric_difference(set.from_list([1, 1, 2]), set.from_list([2, 2, 3]))
    print(console, show_ints(set.to_list(sd1)))
    print(console, show_ints(set.to_list(sd2)))
    let d1a = set.from_list([1, 2])
    print(console, if set.is_disjoint(d1a, set.from_list([3, 4])): "yes" else: "no")
    print(console, if set.is_disjoint(d1a, set.from_list([2, 3])): "yes" else: "no")
"#;
        let sources = [
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("set", crate::bundled_module("set").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "set ops diverged");
        assert_eq!(compiled, vec!["1,4", "1,3", "yes", "no"]);
    }

    #[test]
    fn math_isqrt_and_perfect_square_backends_agree() {
        // isqrt floors the square root (overflow-safe); is_perfect_square is
        // true exactly on 0,1,4,9,... and false for negatives.
        let client = r#"
import math
import list
import string
fn main(console: Console):
    let roots = list.map([0, 1, 2, 3, 4, 8, 9, 15, 16, 100, 99], fn(n: Int): math.isqrt(n))
    print(console, list.join(list.map(roots, fn(n: Int): __render(n)), ","))
    let flags = list.map([0, 1, 2, 4, 9, 10, 16, 17], fn(n: Int): if math.is_perfect_square(n): "T" else: "F")
    print(console, list.join(flags, ""))
    print(console, __render(math.isqrt(-5)))
    print(console, if math.is_perfect_square(-4): "T" else: "F")
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
        assert_eq!(compiled, vec!["0,1,1,1,2,2,3,3,4,10,9", "TTFTTFTF", "0", "F"]);
    }

    #[test]
    fn string_parse_int_backends_agree() {
        // parse_int validates an optional sign + digits before calling the raw
        // string_to_int builtin, so bad input is None (not a trap) consistently.
        let client = r#"
import string
import option
fn show(o: Option(Int)) -> String:
    match o:
        Some(n) -> __render(n)
        None -> "none"
fn main(console: Console):
    print(console, show(string.parse_int("42")))
    print(console, show(string.parse_int("-7")))
    print(console, show(string.parse_int("0")))
    print(console, show(string.parse_int("")))
    print(console, show(string.parse_int("-")))
    print(console, show(string.parse_int("12a")))
    print(console, show(string.parse_int("3.5")))
    print(console, show(string.parse_int(" 5")))
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
import string
fn main(console: Console):
    print(console, "[" + string.center("hi", 6, " ") + "]")
    print(console, "[" + string.center("hi", 7, " ") + "]")
    print(console, "[" + string.center("odd", 8, "*") + "]")
    print(console, "[" + string.center("toolong", 4, " ") + "]")
    print(console, "[" + string.center("x", 1, " ") + "]")
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
    print(console, render("https://example.com/path"))
    print(console, render("http://example.com:8080/x"))
    print(console, render("ftp://host:21/file"))
    print(console, render("http://example.com"))
    print(console, render("not a url"))
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
        // A non-numeric or empty `:port` makes parse return None — it used to trap
        // in string_to_int. A valid or defaulted port still parses, both backends.
        let client = r#"
import url
import result
fn p(s: String) -> String:
    match url.parse(s):
        Ok(u) -> "ok:" + __render(url.port(u))
        Err(_e) -> "none"
fn main(console: Console):
    print(console, p("https://h:8443/x"))
    print(console, p("https://h:abc/x"))
    print(console, p("https://h:/x"))
    print(console, p("https://h:80x/x"))
    print(console, p("https://h/x"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url bad-port diverged");
        assert_eq!(compiled, vec!["ok:8443", "none", "none", "none", "ok:443"]);
    }

    #[test]
    fn func_on_backends_agree() {
        // on(op, f) lifts op to act on projections — here sorting (name, age)
        // pairs by age via func.on_key(lt, snd).
        let client = r#"
import func
import list
import string
fn fst(p: (String, Int)) -> String:
    let (a, _b) = p
    a
fn snd(p: (String, Int)) -> Int:
    let (_a, b) = p
    b
fn lt(a: Int, b: Int) -> Bool:
    a < b
fn main(console: Console):
    let people = [("alice", 30), ("bob", 25), ("carol", 35)]
    let sorted = list.sort_by(people, func.on_key(lt, snd))
    print(console, list.join(list.map(sorted, fst), ","))
    let by_age = func.on_key(lt, snd)
    print(console, if by_age(("x", 1), ("y", 2)): "lt" else: "ge")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("func", crate::bundled_module("func").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "func.on diverged");
        assert_eq!(compiled, vec!["bob,alice,carol", "lt"]);
    }

    #[test]
    fn json_merge_and_has_key_backends_agree() {
        // merge is a shallow override (b wins per-key; a's other keys kept; a
        // non-object b replaces wholesale); has_key checks top-level presence.
        let client = r#"
import json
fn main(console: Console):
    let a = JsonObject([("name", JsonString("a")), ("x", JsonInt(1))])
    let b = JsonObject([("x", JsonInt(2)), ("y", JsonInt(3))])
    print(console, json.encode(json.merge(a, b)))
    print(console, json.encode(json.merge(a, JsonInt(9))))
    print(console, if json.contains_key(a, "x"): "T" else: "F")
    print(console, if json.contains_key(a, "z"): "T" else: "F")
    print(console, if json.contains_key(JsonInt(5), "x"): "T" else: "F")
"#;
        let sources = [("json", crate::bundled_module("json").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "json.merge/has_key diverged");
        assert_eq!(
            compiled,
            vec!["{\"name\":\"a\",\"x\":2,\"y\":3}", "9", "T", "F", "F"]
        );
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
    fn json_decode_rejects_trailing_content_backends_agree() {
        // decode must consume the whole input: trailing whitespace is fine, but
        // any trailing non-whitespace is an error (not a silently-ignored tail).
        let client = r#"
import json
fn classify(s: String) -> String:
    match json.decode(s):
        Ok(j) ->
            match json.as_int(j):
                Some(n) -> "int:" + __render(n)
                None -> "ok"
        Err(_e) -> "err"
fn main(console: Console):
    print(console, classify("[1, 2]"))
    print(console, classify("42  "))
    print(console, classify("1 2"))
    print(console, classify("true xyz"))
    print(console, classify("{}extra"))
    print(console, classify("  7"))
"#;
        let sources = [("json", crate::bundled_module("json").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "decode trailing-content diverged");
        assert_eq!(compiled, vec!["ok", "int:42", "err", "err", "err", "int:7"]);
    }

    #[test]
    fn json_floats_decode_and_round_trip_on_both_backends() {
        // JSON numbers with fractions/exponents decode to JsonFloat and
        // re-encode through the shared float formatter — identical on both
        // backends (the learning log's F12).
        let client = r#"
import json
fn round_trip(s: String) -> String:
    match json.decode(s):
        Ok(j) -> json.encode(j)
        Err(e) -> "err:" + e
fn main(console: Console):
    print(console, round_trip("10"))
    print(console, round_trip("-3"))
    print(console, round_trip("3.25"))
    print(console, round_trip("-0.5"))
    print(console, round_trip("1.5e3"))
    print(console, round_trip("{\"pi\": 3.25}"))
"#;
        let want: Vec<String> = ["10", "-3", "3.25", "-0.5", "1500.0", "{\"pi\":3.25}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(client), want, "interpreter");
        assert_eq!(wasm_run(client), want, "wasm");
    }

    #[test]
    fn string_rsplit_once_backends_agree() {
        // rsplit_once splits on the LAST separator (vs split_once's first); when
        // the separator is absent the whole string is the right part.
        let client = r#"
import string
fn show2(p: (String, String)) -> String:
    let (a, b) = p
    a + "|" + b
fn main(console: Console):
    print(console, show2(string.rsplit_once("a.b.c", ".")))
    print(console, show2(string.split_once("a.b.c", ".")))
    print(console, show2(string.rsplit_once("nodot", ".")))
    print(console, show2(string.rsplit_once("file.tar.gz", ".")))
    print(console, __render(string.last_index_of("a.b.c", ".")))
    print(console, __render(string.last_index_of("nodot", ".")))
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
    fn list_transpose_backends_agree() {
        // transpose swaps rows and columns; a ragged input is truncated to the
        // shortest row, and an empty input gives an empty result.
        let client = r#"
import list
import string
fn show_row(r: List(Int)) -> String:
    list.join(list.map(r, fn(n: Int): __render(n)), ",")
fn show_grid(g: List(List(Int))) -> String:
    list.join(list.map(g, show_row), ";")
fn main(console: Console):
    print(console, show_grid(list.transpose([[1, 2, 3], [4, 5, 6]])))
    print(console, show_grid(list.transpose([[1, 2], [3, 4, 5]])))
    print(console, show_grid(list.transpose([])))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "transpose diverged");
        assert_eq!(compiled, vec!["1,4;2,5;3,6", "1,3;2,4", ""]);
    }

    #[test]
    fn duration_literals_backends_agree() {
        // Native duration literals (1s/1ms/1m/1h/1d/1w, and the `hr` alias) are a
        // distinct Duration type carried as milliseconds: they add/subtract,
        // scale by an Int, divide to an Int ratio, and compare — identically on
        // both backends.
        let client = r#"
fn main(console: Console):
    print(console, __render(30s > 500ms))
    print(console, __render(30s + 500ms == 30500ms))
    print(console, __render(1m == 60s))
    print(console, __render(2hr == 7200s))
    print(console, __render(1d == 24h))
    print(console, __render(1w > 6d))
    print(console, __render(2 * 1h == 7200s))
    print(console, __render(1h / 1m == 60))
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
    fn random_module_backends_agree() {
        // The Park-Miller LCG replays a deterministic sequence (the canonical
        // seed-1 values) identically on both backends; next_below bounds it.
        let client = r#"
import random
import list
import string
fn main(console: Console):
    var r = random.seed(1)
    var out = []
    var i = 0
    while i < 4:
        let (n, r2) = random.next(r)
        out = list.push(out, n)
        r = r2
        i = i + 1
    print(console, list.join(list.map(out, fn(n: Int): __render(n)), ","))
    let (d, _r3) = random.next_below(random.seed(42), 6)
    print(console, __render(d))
    let (b, _r4) = random.next_bool(random.seed(2))
    print(console, if b: "even" else: "odd")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("random", crate::bundled_module("random").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "random diverged");
        assert_eq!(
            compiled,
            vec!["16807,282475249,1622650073,984943658", "0", "even"]
        );
    }

    #[test]
    fn dice_example_runs_on_wasm() {
        // The dice example (seeded random.next_below, threaded Rng) prints the
        // same deterministic rolls on both backends.
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("random", crate::bundled_module("random").unwrap()),
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
import string
fn nums(r: Result(List(Int), String)) -> String:
    match r:
        Ok(xs) -> list.join(list.map(xs, fn(n: Int): __render(n)), ",")
        Err(e) -> "err:" + e
fn onums(o: Option(List(Int))) -> String:
    match o:
        Some(xs) -> list.join(list.map(xs, fn(n: Int): __render(n)), ",")
        None -> "none"
fn main(console: Console):
    print(console, nums(result.all([Ok(1), Ok(2), Ok(3)])))
    print(console, nums(result.all([Ok(1), Err("bad"), Ok(3)])))
    print(console, nums(result.all([])))
    print(console, onums(option.all([Some(1), Some(2)])))
    print(console, onums(option.all([Some(1), None, Some(3)])))
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
    fn result_partition_backends_agree() {
        // partition splits a list of Results into the Ok values and the Err
        // values, each in order.
        let client = r#"
import result
import list
import string
fn main(console: Console):
    let (oks, errs) = result.partition([Ok(1), Err("a"), Ok(2), Err("b"), Ok(3)])
    print(console, list.join(list.map(oks, fn(n: Int): __render(n)), ","))
    print(console, list.join(errs, ","))
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
        assert_eq!(interpreted, compiled, "result.partition diverged");
        assert_eq!(compiled, vec!["1,2,3", "a,b"]);
    }

    #[test]
    fn random_choice_backends_agree() {
        // choice picks a uniformly-random element (None for an empty list),
        // deterministically for a given seed, identically on both backends.
        let client = r#"
import random
import option
fn main(console: Console):
    let (c, _r) = random.choice(["a", "b", "c", "d"], random.seed(1))
    print(console, option.unwrap_or(c, "?"))
    let (e, _r2) = random.choice([], random.seed(1))
    print(console, option.unwrap_or(e, "empty"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("random", crate::bundled_module("random").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "random.choice diverged");
        assert_eq!(compiled, vec!["d", "empty"]);
    }

    #[test]
    fn duration_combinators_backends_agree() {
        // max/min/is_zero/abs over the Duration type (it has no Ord impl, so the
        // generic ord helpers don't apply).
        let client = r#"
import duration
fn main(console: Console):
    print(console, duration.human(duration.max(30s, 1m)))
    print(console, duration.human(duration.min(30s, 1m)))
    print(console, __render(duration.is_zero(0ms)))
    print(console, __render(duration.is_zero(1s)))
    print(console, duration.human(duration.abs(0s - 5s)))
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
    fn duration_module_backends_agree() {
        // The duration module over the built-in Duration type: human/clock format
        // a Duration (combined from literals), to_milliseconds bridges back to Int,
        // and the whole-unit total conversions (to_seconds..to_weeks) truncate.
        let client = r#"
import duration
fn main(console: Console):
    print(console, __render(duration.to_milliseconds(duration.from_clock(1, 2, 3))))
    print(console, duration.clock(1h + 2m + 3s))
    print(console, duration.clock(90s))
    print(console, duration.human(1h + 1m + 1s))
    print(console, duration.human(90s))
    print(console, duration.human(5s))
    print(console, duration.human(500ms))
    print(console, __render(duration.to_milliseconds(duration.hours(2))))
    print(console, __render(duration.part_minutes(1h + 2m + 3s)))
    print(console, __render(duration.to_seconds(duration.days(10))))
    print(console, __render(duration.to_minutes(duration.days(10))))
    print(console, __render(duration.to_hours(duration.days(10))))
    print(console, __render(duration.to_days(duration.days(10))))
    print(console, __render(duration.to_weeks(duration.days(10))))
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
    print(console, __render(degf(Fahrenheit.from(Celsius(100)))))
    let f: Fahrenheit = Celsius(0).into()
    print(console, __render(degf(f)))
    let body: Fahrenheit = Celsius(37).into()
    print(console, __render(degf(body)))
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
        // (incl. ms/hr) or bare-ms input, None on junk/dangling, and
        // parse(human(d)) round-trips.
        let client = r#"
import duration
import option
fn show(o: Option(Duration)) -> String:
    match o:
        Some(d) -> __render(duration.to_milliseconds(d))
        None -> "none"
fn roundtrip(d: Duration) -> String:
    match duration.parse(duration.human(d)):
        Some(p) -> if p == d: "ok" else: "bad"
        None -> "none"
fn main(console: Console):
    print(console, show(duration.parse("1h2m3s")))
    print(console, show(duration.parse("500ms")))
    print(console, show(duration.parse("2hr")))
    print(console, show(duration.parse("90")))
    print(console, show(duration.parse("1h30")))
    print(console, show(duration.parse("")))
    print(console, show(duration.parse("abc")))
    print(console, roundtrip(1h + 1m + 1s))
    print(console, roundtrip(90s))
    print(console, roundtrip(250ms))
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
import string
fn show(xs: List(Int)) -> String:
    list.join(list.map(xs, fn(n: Int): __render(n)), ",")
fn main(console: Console):
    print(console, show([math.ceil_div(7, 3), math.ceil_div(6, 3), math.ceil_div(1, 3), math.ceil_div(0, 3)]))
    print(console, show([math.ceil_div(0 - 7, 3), math.ceil_div(0 - 6, 3)]))
    print(console, show([math.round_div(7, 2), math.round_div(5, 3), math.round_div(4, 3), math.round_div(0 - 7, 2)]))
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
        // zero is "0", negatives get a "-", an out-of-range base is "".
        let client = r#"
import math
fn main(console: Console):
    print(console, math.to_hex(255))
    print(console, math.to_hex(0))
    print(console, math.to_hex(4096))
    print(console, math.to_binary(5))
    print(console, math.to_base(255, 16))
    print(console, math.to_base(0 - 255, 16))
    print(console, math.to_base(100, 1))
    print(console, math.to_base(0, 2))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "to_base diverged");
        assert_eq!(
            compiled,
            vec!["ff", "0", "1000", "101", "ff", "-ff", "", "0"]
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
    print(console, math.format_float(3.14159, 2))
    print(console, math.format_float(0.0 - 0.5, 1))
    print(console, math.format_float(2.0, 0))
    print(console, math.format_float(0.0, 2))
    print(console, math.format_float(1.999, 2))
    print(console, math.format_float(0.0 - 0.04, 1))
    print(console, math.format_float(98.6, 1))
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
    fn big_int_arithmetic_backends_agree() {
        // Compiled Int is now i64, so arithmetic beyond the old 32-bit range
        // agrees with the interpreter instead of wrapping.
        let client = r#"
fn main(console: Console):
    let a = 3000000000
    let b = 4000000000
    print(console, __render(a + b))
    print(console, __render(a * 3))
    let big = 9000000000000
    print(console, __render(big))
    print(console, __render(big / 1000))
    print(console, __render(0 - big))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "big-int arithmetic diverged");
        assert_eq!(
            compiled,
            vec![
                "7000000000",
                "9000000000",
                "9000000000000",
                "9000000000",
                "-9000000000000",
            ]
        );
    }

    #[test]
    fn big_ints_in_list_backends_agree() {
        // 8-byte heap slots carry a full i64 Int inside a (concretely-typed) list.
        let client = r#"
fn main(console: Console):
    let xs = [3000000000, 5000000000]
    print(console, __render(list.at(xs, 0)))
    print(console, __render(list.at(xs, 1)))
    print(console, __render(list.at(xs, 0) + list.at(xs, 1)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "big-ints-in-list diverged");
        assert_eq!(compiled, vec!["3000000000", "5000000000", "8000000000"]);
    }

    #[test]
    fn floats_in_collections_backends_agree() {
        // 8-byte slots also hold f64, so floats now live in lists and tuples
        // (read back with float_to_int, since Float to_string is still WASM-gated).
        let client = r#"
fn main(console: Console):
    let fs = [1.5, 2.5, 3.5]
    print(console, __render(list.length(fs)))
    print(console, __render(math.to_int(list.at(fs, 1))))
    let pair = (1.5, 9.5)
    let (lo, hi) = pair
    print(console, __render(math.to_int(hi)))
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
    fn let_patterns_example_runs_on_wasm() {
        // `if let` / `while let` desugar to `match`, so the pattern-binding control
        // flow (including draining a list via head/tail in a `while let`) produces
        // identical output on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../examples/let_patterns/src/let_patterns.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "let_patterns diverged");
        assert_eq!(
            compiled,
            vec![
                "found: 42",
                "head is 7",
                "pop 1",
                "pop 2",
                "pop 3",
                "pop 4",
                "drained",
            ]
        );
    }

    #[test]
    fn ranges_example_runs_on_wasm() {
        // Integer range patterns (`lo..hi`, `lo..=hi`) desugar to a guarded
        // binding, so the HTTP-status and grade classifiers match identically on
        // both backends.
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
    fn subscript_example_runs_on_wasm() {
        // `xs[i]` desugars to `list.at(xs, i)`; chained subscripts index nested lists.
        // The dot product and 2D-grid diagonal match on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/subscript/src/subscript.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "subscript diverged");
        assert_eq!(
            compiled,
            vec!["dot = 32", "grid[1][2] = 6", "diagonal sum = 15"]
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
        let src = "fn main(console: Console):\n    print(console, \"ab\" + \"\\n\")\n    print(console, \"cd\")\n";
        let sources = [("main", src)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "trailing-newline print diverged");
        assert_eq!(compiled, vec!["ab", "cd"]);
    }

    #[test]
    fn aliases_example_runs_on_wasm() {
        // Type aliases (scalar and compound) are expanded before both backends,
        // so the temperature conversions and averaging agree.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/aliases/src/aliases.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "aliases diverged");
        assert_eq!(compiled, vec!["avg C = 21", "25C = 77F", "0C  = 32F"]);
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
    print(console, "${c.n}")
"#;
        let want = vec!["2".to_string()];
        assert_eq!(link_run(client), want, "interpreter");
        assert_eq!(wasm_run(client), want, "wasm");
        // Free-function UFCS is gone — one cut, loud error.
        let ufcs = "fn inc(x: Int) -> Int:\n    x + 1\n\nfn main(console: Console):\n    print(console, \"${5.inc()}\")\n";
        let module = parser::parse_module(ufcs).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("free-fn UFCS must be rejected");
        assert!(
            err.to_string().contains("methods come from `impl` blocks"),
            "got: {err}"
        );
    }

    #[test]
    fn sandbox_runs_compiled_and_captures_output() {
        // `witchy sandbox` compiles to WASM and runs in the capability sandbox,
        // returning the program's output.
        let path = std::env::temp_dir().join("witchy_sandbox_smoke.witchy");
        std::fs::write(
            &path,
            "fn main(console: Console):\n    print(console, __render(6 * 7))\n",
        )
        .unwrap();
        let (out, exit) =
            crate::run_file_sandboxed(path.to_str().unwrap(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, Vec::new())
                .expect("sandbox run");
        assert_eq!(out, vec!["42"]);
        assert_eq!(exit, None, "a Nil-returning main has no exit code");
    }

    /// The sandbox grants exactly the computed footprint: a program combining
    /// argv, Env, and a read-only Dir (minigrep's shape) runs confined, and its
    /// Int-returning `main` becomes the exit code rather than an output line.
    #[test]
    fn sandbox_grants_full_footprint() {
        let root = std::env::temp_dir().join(format!("witchy_sandbox_fp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("data.txt"), "needle in here\nnothing\n").unwrap();
        let src_path = root.join("prog.witchy");
        std::fs::write(
            &src_path,
            "import option\nimport string\n\nfn main(console: Console, env: Env, dir: Dir[Read], args: List(String)) -> Int:\n    let path = list.at(args, 0)\n    let label = match get_env(env, \"WITCHY_SANDBOX_LABEL\"):\n        Some(v) -> v\n        None -> \"unlabeled\"\n    for line in string.lines(read(dir, path)):\n        if string.contains(line, \"needle\"):\n            print(console, label + \": \" + line)\n    0\n",
        )
        .unwrap();
        unsafe { std::env::set_var("WITCHY_SANDBOX_LABEL", "found") };
        let (out, exit) = crate::run_file_sandboxed(
            src_path.to_str().unwrap(),
            vec![root.clone()],
            Vec::new(),
            Vec::new(),
            vec!["data.txt".to_string()],
            None,
            Vec::new(),
        )
        .expect("sandbox run");
        assert_eq!(out, vec!["found: needle in here"]);
        assert_eq!(exit, Some(0), "Int-returning main becomes the exit code");
        let _ = std::fs::remove_dir_all(&root);
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
            "fn build(out: BuildOut, schema: BuildRead):\n    let nl = \"\\n\"\n    write_out(out, \"api.witchy\", \"pub fn service() -> String:\" + nl + \"    \\\"\" + read_build(schema, \"svc.txt\") + \"\\\"\" + nl)\n",
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
            "fn build(out: BuildOut):\n    write_out(out, \"../escape.txt\", \"nope\")\n",
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
            "fn main(console: Console, dir: Dir[Read], args: List(String)) -> Int:\n    print(console, read(dir, list.at(args, 0)))\n    0\n",
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
        let path = std::env::temp_dir().join("witchy_verify_smoke.witchy");
        std::fs::write(
            &path,
            "fn main(console: Console):\n    print(console, __render((2 + 3) * 4))\n    print(console, \"hi\")\n",
        )
        .unwrap();
        crate::verify_file(path.to_str().unwrap()).expect("backends should agree");
    }

    #[test]
    fn every_example_type_checks() {
        // Every shipped example must link and type-check (this also exercises
        // import resolution and the constant/alias cycle checks). The parity test
        // skips non-divergence errors, so without this a type error in an example
        // could slip through CI.
        let mut failures = Vec::new();
        for path in example_entries() {
            let p = path.to_str().unwrap();
            if let Err(e) = crate::check_file(p) {
                failures.push(format!("{p}: {e}"));
            }
        }
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
        let mut diverged = Vec::new();
        for path in example_entries() {
            let p = path.to_str().unwrap();
            match crate::verify_file(p) {
                Ok(()) => {}
                Err(e) if e.contains("DIVERGE") => diverged.push(e),
                // Interpreter-only feature or no `main`: not comparable, skip.
                Err(_) => {}
            }
        }
        assert!(
            diverged.is_empty(),
            "examples diverge across backends:\n{}",
            diverged.join("\n")
        );
    }

    #[test]
    fn examples_agree_under_inplace_and_forced_copy() {
        // Metamorphic, NO-ORACLE codegen check: the in-place update machinery and
        // the forced-copy fallback are two lowerings of the same program and must
        // produce identical output. This catches an in-place aliasing bug on the
        // compiled backend WITHOUT consulting the interpreter — the kind of
        // self-consistency guard that lets the differential oracle be retired.
        // Restricted to console-only, `main`-bearing programs so output is
        // self-contained and deterministic.
        let mut diverged = Vec::new();
        for path in example_entries() {
            let p = path.to_str().unwrap();
            let Ok((linked, _)) = crate::link_file(p) else {
                continue;
            };
            if typeck::check(&linked).is_err() {
                continue;
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
                continue;
            }
            let compile_with = |force_copy: bool| {
                codegen::set_force_copy_for_tests(Some(force_copy));
                let bytes = codegen::compile_module_binary(&linked);
                codegen::set_force_copy_for_tests(None);
                bytes
            };
            if let (Ok(Some(inplace)), Ok(Some(copy))) = (compile_with(false), compile_with(true)) {
                let a = crate::run_wasm_bytes(&inplace);
                let b = crate::run_wasm_bytes(&copy);
                if a != b {
                    diverged.push(format!("{p}: in-place {a:?} vs forced-copy {b:?}"));
                }
            }
        }
        assert!(
            diverged.is_empty(),
            "in-place and forced-copy codegen diverge:\n{}",
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
        let mut diverged = Vec::new();
        for path in example_entries() {
            let p = path.to_str().unwrap();
            let Ok((linked, _)) = crate::link_file(p) else {
                continue;
            };
            if typeck::check(&linked).is_err() {
                continue;
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
                continue;
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
            if let (Ok(Some(def)), Ok(Some(rc))) = (compile_with_rc(false), compile_with_rc(true)) {
                let a = crate::run_wasm_bytes(&def);
                let b = crate::run_wasm_bytes(&rc);
                if a != b {
                    diverged.push(format!("{p}: default {a:?} vs rc-floor {b:?}"));
                }
            }
        }
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
        crate::opt::set_for_tests(Some(set));
        let mut diverged = Vec::new();
        for path in example_entries() {
            let p = path.to_str().unwrap();
            match crate::verify_file(p) {
                Ok(()) => {}
                Err(e) if e.contains("DIVERGE") => diverged.push(e),
                // Interpreter-only feature or no `main`: not comparable, skip.
                Err(_) => {}
            }
        }
        crate::opt::set_for_tests(None);
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
        let tmp = std::env::temp_dir().join("witchy_tier1_precompiled.wasm");
        let out = tmp.to_str().unwrap();
        crate::emit_wasm_file("examples/calc/src/calc.witchy", out).expect("emit-wasm");
        let (from_wasm, _) =
            crate::run_wasm_file(out, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, Vec::new()).expect("run .wasm");
        let from_source = crate::execute_file("examples/calc/src/calc.witchy", Vec::new()).expect("run source");
        assert_eq!(from_wasm, from_source, "precompiled .wasm diverges from the source run");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn merge_sort_is_stable_on_both_backends() {
        // list.sort_by is a stable merge sort: equal keys keep their original
        // order. Sort (key, tag) items by key only; ties must preserve insertion
        // order. Both backends agree.
        let client = r#"
import list
type Item:
    Item(Int, String)
fn key(it: Item) -> Int:
    match it:
        Item(k, _t) -> k
fn tag(it: Item) -> String:
    match it:
        Item(_k, t) -> t
fn main(console: Console):
    let xs = [Item(2, "a"), Item(1, "b"), Item(2, "c"), Item(1, "d"), Item(2, "e")]
    let sorted = list.sort_by(xs, fn(p: Item, q: Item): key(p) < key(q))
    for it in sorted:
        print(console, __render(key(it)) + tag(it))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "merge sort diverged");
        assert_eq!(compiled, vec!["1b", "1d", "2a", "2c", "2e"]);
    }

    #[test]
    fn std_ord_string_and_sort_backends_agree() {
        // `impl Ord for String` makes strings comparable, and the bounded generic
        // `cmp.sort` dispatches through the element's Ord impl — so it sorts
        // runtime-BUILT strings content-correctly on both backends (a pointer
        // comparison sort would scramble them in compiled code). Also covers
        // Ord-over-String for max_of/maximum and Ints via the same `sort`.
        let client = r#"
import cmp
import string

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < string.char_count(s)):
        acc = (acc + string.substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("pear"), build("apple"), build("fig"), build("apple")]
    print(console, list.join(cmp.sort(words), ","))
    print(console, list.join(cmp.sort(["c", "a", "b"]), ""))
    print(console, cmp.max_of(build("alpha"), build("omega")))
    print(console, cmp.maximum([build("x"), build("a"), build("m")], ""))
    let nums = cmp.sort([3, 1, 2, 1])
    print(console, __render((list.at(nums, 0) + (list.at(nums, 3) * 10))))
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

    #[test]
    fn std_result_or_mapor_backends_agree() {
        // The fallback combinators mirror Option's: `or` / `or_else` keep an Ok
        // or supply an alternative (eagerly / error-aware lazily), and `map_or`
        // transforms an Ok or returns the default for an Err. Both backends agree.
        let client = r#"
import result

fn checked(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    print(console, __render(result.unwrap_or(result.or(checked(5), Ok(9)), 0)))
    print(console, __render(result.unwrap_or(result.or(checked((0 - 1)), Ok(9)), 0)))
    print(console, __render(result.unwrap_or(result.or_else(checked((0 - 1)), fn(e: String): Ok(string.length(e))), 0)))
    print(console, __render(result.map_or(checked(5), 0, fn(x: Int): (x * 2))))
    print(console, __render(result.map_or(checked((0 - 1)), 99, fn(x: Int): (x * 2))))
"#;
        let sources = [("result", crate::bundled_module("result").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result or/map_or diverged");
        assert_eq!(compiled, vec!["5", "9", "3", "10", "99"]);
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
    print(console, __render(result.is_err(checked(5))))
    print(console, __render(result.is_err(checked((0 - 1)))))
    let chained = result.and_then(checked(5), fn(n: Int): Ok((n * 10)))
    print(console, __render(result.unwrap_or(chained, 0)))
    let mapped = result.map_err(checked((0 - 1)), fn(s: String): string.length(s))
    print(console, __render(result.is_err(mapped)))
"#;
        let sources = [("result", crate::bundled_module("result").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result combinators diverged");
    }

    fn assert_fn_compiles(src: &str) {
        assert!(typeck::check_str(src).is_ok(), "{:?}", typeck::check_str(src));
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        Module::new(&Engine::default(), &bytes).expect("valid wasm");
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
                Ok(Some(_))
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
        assert!(typeck::check_str(src).is_ok(), "{:?}", typeck::check_str(src));
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
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

    /// #35 keystone: a WIR-native prelude helper (vs the raw-body "all features
    /// on" prelude) yields a CAPABILITY-MINIMAL module — it imports only the
    /// authority the reached helpers need. A module whose only helper is
    /// `print_str` imports only `print`, so it instantiates and runs under a
    /// print-ONLY grant. (Were it the raw-body prelude, it would import
    /// crypto.sign/dir/net/… and fail to instantiate here.) This proves the
    /// incremental WIR-helper path that unblocks the M3 flip.
    #[test]
    fn wir_native_helper_yields_capability_minimal_module() {
        use crate::wir::{
            DataSegment, Kind, WirExpr, WirFunc, WirImport, WirModule, WirNode,
        };
        use crate::wir_helpers::print_str_helper;
        // Intern "hello" at offset 1024: [i32 len=5]["hello"].
        let off = 1024u32;
        let text = "hello";
        let mut bytes = (text.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(text.as_bytes());

        let main = WirFunc {
            name: "main".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(WirExpr::Call {
                func: "print_str".into(),
                args: vec![WirExpr::StrPtr(off)],
            })],
            raw_body: None,
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(WirExpr::Call { func: "main".into(), args: vec![] })],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print".into(),
                params: vec![Kind::I32, Kind::I32],
                results: vec![],
            }],
            funcs: vec![print_str_helper(), main, run],
            memory_pages: 1,
            data: vec![DataSegment { offset: off, bytes }],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        let wasm = crate::wir_encode::encode(&module);
        assert!(wasmparser::validate(&wasm).is_ok(), "encoded module must validate");

        // Run with ONLY `print` granted — nothing else. Success proves the module
        // imports no other authority (else instantiate would fail).
        use crate::runtime::{Capabilities, Runtime};
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &wasm,
                Capabilities { print: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn under a print-only grant");
        actor.run().expect("run");
        assert_eq!(actor.output(), vec!["hello".to_string()]);
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

    /// Regression: a for-loop in a function body followed by a closure both lower
    /// on the binary path; the loop watermark must be captured for the WHOLE
    /// function, NOT mistaking the inner loop body for the function body — a bug
    /// that silently compiled a loop to a single iteration. Closures now lower
    /// (the lifted body + closure object + `call_indirect`), so this program takes
    /// the binary path end-to-end; the loop emitting all three iterations under the
    /// binary sink is the live proof the capture is not mis-scoped.
    #[test]
    fn wir_loop_then_closure_lowers_and_keeps_loop_scope() {
        let src = "fn main(console: Console):\n    for x in [10, 20, 30]:\n        print(console, __render(x))\n    let f = fn(n: Int): n + 1\n    print(console, __render(f(5)))\n";
        let want = vec!["10".to_string(), "20".to_string(), "30".to_string(), "6".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        // Takes the binary path (closures lower) AND emits all loop iterations —
        // a mis-scoped capture would drop the loop to a single pass.
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("loop + closure must lower on the binary path");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
        assert_eq!(run_on_wasm(src), want, "WAT path");
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

    /// Criterion-3 MEASURABLE: a clean in-place accumulator builds in place on the
    /// binary path — amortized O(1) re-owns (the exported `__witchy_reowns`
    /// counter ≤ 2), not O(n) copies — and prints the right element.
    #[test]
    fn wir_inplace_accumulator_is_o1_reowns() {
        let src = "fn build(n: Int) -> List(Int):\n    var xs: List(Int) = []\n    for i in 0..n:\n        xs = list.push(xs, i)\n    xs\n\nfn main(console: Console):\n    let ys = build(500)\n    print(console, __render(list.at(ys, 499)))\n";
        let want = vec!["499".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the accumulator program takes the WIR binary path");
        let (out, reowns) = binary_run_reowns(&bytes);
        assert_eq!(out, want, "binary output");
        assert!(reowns <= 2, "expected O(1) re-owns on the binary path, got {reowns}");
    }

    /// Criterion-3: an in-place accumulator (`xs = list.push(xs, i)` in a loop)
    /// lowers to the cap ABI (`$list_push_cap` via CallStoreMulti) on the binary
    /// path. Consumed via `list.at` so the whole program stays on the pruned
    /// binary path; runs identically to the interpreter oracle AND the WAT path.
    #[test]
    fn wir_inplace_accumulator_runs_and_agrees() {
        let src = "fn build(n: Int) -> List(Int):\n    var xs: List(Int) = []\n    for i in 0..n:\n        xs = list.push(xs, i)\n    xs\n\nfn main(console: Console):\n    let ys = build(3)\n    print(console, __render(list.at(ys, 0)))\n    print(console, __render(list.at(ys, 1)))\n    print(console, __render(list.at(ys, 2)))\n";
        let want = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        assert_eq!(run_on_wasm(src), want, "legacy WAT path");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the accumulator program takes the WIR binary path");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path (in-place accumulator)");
    }

    /// The in-place DICT accumulator on the binary path: `d = dict.insert(d, k, v)`
    /// in a loop lowers to `$dict_insert_cap` (O(1) amortized into owned entry
    /// slack) instead of copying the whole dict each insert. Proven the same two
    /// ways as the list accumulator: the values agree with the interpreter, AND
    /// the observable `$__witchy_reowns` counter stays O(1) (one re-own, not one
    /// per insert) — the timing-free proof the copy-per-insert path was avoided.
    #[test]
    fn wir_inplace_dict_insert_is_o1_reowns() {
        let src = "fn build(n: Int) -> Dict(String, Int):\n    var d = dict.new()\n    for i in 0..n:\n        d = dict.insert(d, \"k\" + __render(i), i)\n    d\n\nfn main(console: Console):\n    let m = build(500)\n    print(console, __render(dict.get_or(m, \"k499\", 0 - 1)))\n    print(console, __render(dict.length(m)))\n";
        let want = vec!["499".to_string(), "500".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the dict accumulator program takes the WIR binary path");
        let (out, reowns) = binary_run_reowns(&bytes);
        assert_eq!(out, want, "binary output");
        assert!(reowns <= 2, "expected O(1) re-owns for the in-place dict insert, got {reowns}");
    }

    /// The in-place STRING builder on the binary path: `s = s + piece` in a loop
    /// lowers to `$str_append_cap` (append bytes into owned slack) instead of
    /// re-concatenating the whole string each statement. Proven both ways: values
    /// agree with the interpreter, AND `$__witchy_reowns` stays O(1).
    #[test]
    fn wir_inplace_str_append_is_o1_reowns() {
        let src = "fn build(n: Int) -> String:\n    var s = \"\"\n    var i = 0\n    while i < n:\n        s = s + \"x\"\n        i = i + 1\n    s\n\nfn main(console: Console):\n    let r = build(500)\n    print(console, \"${string.length(r)}\")\n";
        let want = vec!["500".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the string builder takes the WIR binary path");
        let (out, reowns) = binary_run_reowns(&bytes);
        assert_eq!(out, want, "binary output");
        assert!(reowns <= 2, "expected O(1) re-owns for the in-place string builder, got {reowns}");
    }

    /// The in-place dict.update accumulator (the word-count shape) on the binary
    /// path: `d = dict.update(d, k, dflt, f)` in a loop lowers to
    /// `$dict_update_cap` (apply the closure, reinsert into owned slack) instead
    /// of copying the dict each update. Values agree with the interpreter AND
    /// `$__witchy_reowns` stays O(1).
    #[test]
    fn wir_inplace_dict_update_is_o1_reowns() {
        let src = "fn build(n: Int) -> Dict(String, Int):\n    var d = dict.new()\n    var i = 0\n    while i < n:\n        d = dict.update(d, \"k\" + __render(i % 10), 0, fn(c: Int): c + 1)\n        i = i + 1\n    d\n\nfn main(console: Console):\n    let d = build(500)\n    print(console, \"${dict.get_or(d, \"k0\", 0 - 1)}\")\n    print(console, \"${dict.length(d)}\")\n";
        let want = vec!["50".to_string(), "10".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the dict.update accumulator takes the WIR binary path");
        let (out, reowns) = binary_run_reowns(&bytes);
        assert_eq!(out, want, "binary output");
        assert!(reowns <= 2, "expected O(1) re-owns for the in-place dict update, got {reowns}");
    }

    /// A RecordUpdate whose base is a non-Var EXPRESSION on the binary path:
    /// `Point(x: 100, ..(l).from)` — the base `(l).from` (a field access) is
    /// evaluated ONCE into the `$TUPLE_TMP` scratch, base-first, so each
    /// un-updated field (`y`) reads it (was: the lowering required a Var base and
    /// bailed to WAT). Compared against the interpreter oracle.
    #[test]
    fn wir_record_update_expr_base_binary_path() {
        let src = "type Point:\n    x: Int\n    y: Int\n\ntype Line:\n    from: Point\n    to: Point\n\nfn main(console: Console):\n    let l = Line(Point(1, 2), Point(3, 4))\n    let p2 = Point(x: 100, ..(l).from)\n    print(console, \"${(p2).x}\")\n    print(console, \"${(p2).y}\")\n";
        let want = vec!["100".to_string(), "2".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should lower a RecordUpdate with an expression base");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// An `var` fn with an EARLY `return` on the binary path: the return must
    /// yield the full multi-result tuple (the declared value, then each var
    /// param's final value) so the arity matches the move-out ABI — a single
    /// `N::Return` would mismatch and the whole module bailed to WAT. `clamp`
    /// returns early when `n > 10`; both the early and fall-through exits write
    /// `n` back into the caller's variable.
    #[test]
    fn wir_var_early_return_binary_path() {
        let src = "fn clamp(var n: Int):\n    if (n > 10):\n        n = 10\n        return\n    n = n + 1\n\nfn main(console: Console):\n    var a = 5\n    clamp(a)\n    print(console, \"${a}\")\n    var b = 50\n    clamp(b)\n    print(console, \"${b}\")\n";
        let want = vec!["6".to_string(), "10".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should lower an var fn with an early return");
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
        let src = "fn main(console: Console):\n    let xs = [true, false]\n    let ys = [list.at(xs, 0)]\n    if list.at(ys, 0):\n        print(console, \"t\")\n    else:\n        print(console, \"f\")\n";
        let want = vec!["t".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let m = codegen::assemble_wir_module(&linked)
            .expect("assemble")
            .expect("program takes the WIR binary path");
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
        assert_eq!(run_bytes_print_only(&crate::wir_encode::encode(&m)), want, "unoptimized");
        assert_eq!(run_bytes_print_only(&crate::wir_encode::encode(&opt_m)), want, "optimized");
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
            "fn main(console: Console):\n    print(console, \"hi\")\n",
            "fn inc(n: Int) -> Int:\n    n + 1\n\nfn main(console: Console):\n    if inc(inc(0)) > 1:\n        print(console, \"ok\")\n    else:\n        print(console, \"no\")\n",
            "fn classify(n: Int) -> Bool:\n    match n:\n        0 -> true\n        _ -> false\n\nfn main(console: Console):\n    if classify(0):\n        print(console, \"zero\")\n    else:\n        print(console, \"nonzero\")\n",
        ];
        for src in progs {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("typecheck");
            let m = codegen::assemble_wir_module(&linked)
                .expect("assemble")
                .unwrap_or_else(|| panic!("expected the WIR binary path to handle:\n{src}"));
            let oracle = link_run(src);
            // Unoptimized encoding runs like the oracle...
            let unopt = crate::wir_encode::encode(&m);
            assert_eq!(run_bytes_all_caps(&unopt), oracle, "unoptimized:\n{src}");
            // ...and the optimized encoding runs identically (sound rewrite).
            let mut opt_m = m.clone();
            let stats = crate::wir_opt::optimize(&mut opt_m);
            assert!(stats.nodes_after <= stats.nodes_before, "the pass never grows the tree");
            let opt = crate::wir_encode::encode(&opt_m);
            assert_eq!(run_bytes_all_caps(&opt), oracle, "optimized:\n{src}");
        }
    }

    /// M3 sink-flip: the WIR→binary path (`compile_module_binary`, NO
    /// `wat::parse_str`) must, for every program whose whole module lowers,
    /// assemble a VALID wasm module that runs identically to the interpreter
    /// oracle and to the legacy WAT path. Programs are chosen from the lowering
    /// subset (string literals + control flow + scalar helpers; no list-building,
    /// string concat, `__render`, or Int/Float `main` yet).
    #[test]
    fn wir_binary_path_runs_and_agrees_with_oracle() {
        let cases: &[(&str, Vec<String>)] = &[
            (
                "fn main(console: Console):\n    print(console, \"hello from WIR\")\n",
                vec!["hello from WIR".to_string()],
            ),
            (
                "fn main(console: Console):\n    print(console, \"one\")\n    print(console, \"two\")\n",
                vec!["one".to_string(), "two".to_string()],
            ),
            (
                "fn main(console: Console):\n    if true:\n        print(console, \"yes\")\n    else:\n        print(console, \"no\")\n",
                vec!["yes".to_string()],
            ),
            (
                "fn pick(b: Bool) -> Bool:\n    b\n\nfn main(console: Console):\n    if pick(true):\n        print(console, \"picked\")\n    else:\n        print(console, \"nope\")\n",
                vec!["picked".to_string()],
            ),
            // An aggregate: builds a tuple ($mk2 → $ensure) and destructures it —
            // exercises the migrated allocator helpers on the pruned binary path.
            (
                "fn main(console: Console):\n    let t = (1, 2)\n    let (a, b) = t\n    if a < b:\n        print(console, \"ordered\")\n    else:\n        print(console, \"no\")\n",
                vec!["ordered".to_string()],
            ),
            // A list with indexing ($mk3 → $ensure, $list_at) on the binary path.
            (
                "fn main(console: Console):\n    let xs = [10, 20, 30]\n    if list.at(xs, 1) == 20:\n        print(console, \"twenty\")\n    else:\n        print(console, \"no\")\n",
                vec!["twenty".to_string()],
            ),
            // Integer rendering ($int_to_string → $ensure) on the binary path.
            (
                "fn main(console: Console):\n    print(console, __render(42))\n    print(console, __render(-7))\n",
                vec!["42".to_string(), "-7".to_string()],
            ),
            // String content equality ($str_eq) on the binary path.
            (
                "fn main(console: Console):\n    if \"abc\" == \"abc\":\n        print(console, \"eq\")\n    else:\n        print(console, \"ne\")\n    if \"abc\" == \"xyz\":\n        print(console, \"eq2\")\n    else:\n        print(console, \"ne2\")\n",
                vec!["eq".to_string(), "ne2".to_string()],
            ),
            // String concatenation ($concat → $ensure) on the binary path.
            (
                "fn main(console: Console):\n    print(console, \"hello, \" + \"world\")\n    print(console, \"x\" + \"y\" + \"z\")\n",
                vec!["hello, world".to_string(), "xyz".to_string()],
            ),
            // list.length on the binary path.
            (
                "fn main(console: Console):\n    let xs = [10, 20, 30]\n    print(console, __render(list.length(xs)))\n",
                vec!["3".to_string()],
            ),
            // string.length on the binary path.
            (
                "fn main(console: Console):\n    print(console, __render(string.length(\"hello\")))\n",
                vec!["5".to_string()],
            ),
            // string.contains ($find_byte — a conditional br inside a loop) on
            // the binary path.
            (
                "fn main(console: Console):\n    if string.contains(\"hello\", \"ell\"):\n        print(console, \"yes\")\n    else:\n        print(console, \"no\")\n    if string.contains(\"hello\", \"xyz\"):\n        print(console, \"yes2\")\n    else:\n        print(console, \"no2\")\n",
                vec!["yes".to_string(), "no2".to_string()],
            ),
            // string.starts_with ($starts_with — prefix byte-compare loop) on
            // the binary path.
            (
                "fn main(console: Console):\n    print(console, __render(string.starts_with(\"hello\", \"hel\")))\n    print(console, __render(string.starts_with(\"hello\", \"lo\")))\n    print(console, __render(string.starts_with(\"hello\", \"\")))\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string()],
            ),
            // string.ends_with ($ends_with — suffix byte-compare loop) on the
            // binary path.
            (
                "fn main(console: Console):\n    print(console, __render(string.ends_with(\"hello\", \"llo\")))\n    print(console, __render(string.ends_with(\"hello\", \"hel\")))\n    print(console, __render(string.ends_with(\"hello\", \"\")))\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string()],
            ),
            // string.index_of ($str_index_of → $find_byte + $byte_to_char, the
            // byte-offset → char-index conversion) on the binary path.
            (
                "fn main(console: Console):\n    print(console, __render(string.index_of(\"hello\", \"ll\")))\n    print(console, __render(string.index_of(\"hello\", \"xyz\")))\n",
                vec!["2".to_string(), "-1".to_string()],
            ),
            // string.substring ($str_substring → $char_to_byte + $substr, a
            // heap-allocating slice) on the binary path.
            (
                "fn main(console: Console):\n    print(console, string.substring(\"hello world\", 0, 5))\n    print(console, string.substring(\"hello world\", 6, 11))\n",
                vec!["hello".to_string(), "world".to_string()],
            ),
            // string.trim ($trim → $is_ws + $substr, two whitespace scan loops)
            // on the binary path.
            (
                "fn main(console: Console):\n    print(console, string.trim(\"  hi  \"))\n    print(console, string.trim(\"abc\"))\n",
                vec!["hi".to_string(), "abc".to_string()],
            ),
            // string.split ($split → $substr + $list_push, nested scan/compare
            // loops building a List(String)) on the binary path; indexed with
            // the already-migrated $list_at.
            (
                "fn main(console: Console):\n    let parts = string.split(\"a,b,c\", \",\")\n    print(console, list.at(parts, 0))\n    print(console, list.at(parts, 1))\n    print(console, list.at(parts, 2))\n",
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ),
            // for-loop over a list with an arena-resettable body (the watermark
            // optimization, ported to WIR): per-iteration `$heap` save/restore.
            (
                "fn main(console: Console):\n    for piece in string.split(\"a,b,c\", \",\"):\n        print(console, piece)\n",
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ),
            // range for-loop whose body allocates per iteration (nothing escapes,
            // so it's watermarked) — exercises the range-for arena reset on WIR.
            (
                "fn main(console: Console):\n    for i in 0..3:\n        print(console, string.substring(\"abcdef\", i, i + 2))\n",
                vec!["ab".to_string(), "bc".to_string(), "cd".to_string()],
            ),
            // while-loop with an arena-resettable allocating body (the watermark
            // now ported to WIR for `while` too).
            (
                "fn main(console: Console):\n    var i: Int = 0\n    while i < 3:\n        print(console, string.substring(\"abcdef\", i, i + 2))\n        i = i + 1\n",
                vec!["ab".to_string(), "bc".to_string(), "cd".to_string()],
            ),
            // match on an ADT constructor with a payload bind (Some(n)) / a
            // nullary variant (None) — the new lower_pattern Ctor arm.
            (
                "fn pick(b: Bool) -> Option(Int):\n    if b:\n        Some(7)\n    else:\n        None\n\nfn main(console: Console):\n    print(console, __render(match pick(true):\n        Some(n) -> n\n        None -> 99))\n    print(console, __render(match pick(false):\n        Some(n) -> n\n        None -> 99))\n",
                vec!["7".to_string(), "99".to_string()],
            ),
            // match on string-literal patterns (str_eq) with a wildcard fallback.
            (
                "fn classify(s: String) -> Int:\n    match s:\n        \"yes\" -> 1\n        \"no\" -> 0\n        _ -> 9\n\nfn main(console: Console):\n    print(console, __render(classify(\"yes\")))\n    print(console, __render(classify(\"no\")))\n    print(console, __render(classify(\"maybe\")))\n",
                vec!["1".to_string(), "0".to_string(), "9".to_string()],
            ),
            // match with a LITERAL constructor field (Some(0)) — the short-circuit
            // `if tag == Some: field == 0` path of the Ctor pattern arm.
            (
                "fn check(o: Option(Int)) -> Int:\n    match o:\n        Some(0) -> 100\n        Some(n) -> n\n        None -> 99\n\nfn main(console: Console):\n    print(console, __render(check(Some(0))))\n    print(console, __render(check(Some(5))))\n    print(console, __render(check(None)))\n",
                vec!["100".to_string(), "5".to_string(), "99".to_string()],
            ),
            // list patterns: empty, exact-length head bind, and a `[h, ..t]` tail
            // bind (via $list_drop).
            (
                "fn sum_head(xs: List(Int)) -> Int:\n    match xs:\n        [] -> 0\n        [a, b] -> a + b\n        [h, ..t] -> h + list.length(t)\n        _ -> 99\n\nfn main(console: Console):\n    print(console, __render(sum_head([])))\n    print(console, __render(sum_head([10, 20])))\n    print(console, __render(sum_head([5, 1, 2, 3])))\n",
                vec!["0".to_string(), "30".to_string(), "8".to_string()],
            ),
            // structural `==` on scalar-field compounds: a tuple, a list, and a
            // tuple with a String field ($str_eq). Distinct literals so a stray
            // pointer-compare would diverge from the structural result.
            (
                "fn main(console: Console):\n    print(console, __render((1, 2) == (1, 2)))\n    print(console, __render((1, 2) == (1, 3)))\n    print(console, __render([1, 2, 3] == [1, 2, 3]))\n    print(console, __render([1, 2] == [1, 9]))\n    print(console, __render((\"a\", 1) == (\"a\", 1)))\n    print(console, __render((\"a\", 1) == (\"b\", 1)))\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string(), "false".to_string(), "true".to_string(), "false".to_string()],
            ),
            // NESTED structural `==`: a list of tuples and a tuple of (tuple, int)
            // — slot_cmp_wir recurses into the field shapes' eq helpers.
            (
                "fn main(console: Console):\n    print(console, __render([(1, 2), (3, 4)] == [(1, 2), (3, 4)]))\n    print(console, __render([(1, 2)] == [(1, 9)]))\n    print(console, __render(((1, 2), 3) == ((1, 2), 3)))\n    print(console, __render(((1, 2), 3) == ((1, 9), 3)))\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string(), "false".to_string()],
            ),
            // __render of compounds (the $ts renderer): a tuple, a tuple with a
            // String + Bool field, and a list — built with $concat/$int_to_string.
            (
                "fn main(console: Console):\n    print(console, __render((1, 2)))\n    print(console, __render((\"hi\", true)))\n    print(console, __render([1, 2, 3]))\n    print(console, __render([true, false]))\n",
                vec!["(1, 2)".to_string(), "(hi, true)".to_string(), "[1, 2, 3]".to_string(), "[true, false]".to_string()],
            ),
            // a record: structural `==` (eq helper) and `__render` (ts helper,
            // `Name(f0, f1)`) on the binary path.
            (
                "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    print(console, __render(Point(1, 2)))\n    print(console, __render(Point(1, 2) == Point(1, 2)))\n    print(console, __render(Point(1, 2) == Point(1, 9)))\n",
                vec!["Point(1, 2)".to_string(), "true".to_string(), "false".to_string()],
            ),
            // a tuple with a Float field renders via $float_to_str (host import).
            (
                "fn main(console: Console):\n    print(console, __render((1.5, 2)))\n",
                vec!["(1.5, 2)".to_string()],
            ),
            // a closure: a lambda bound to a local, then called (the lifted body +
            // closure object + call_indirect on the binary path).
            (
                "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    print(console, __render(f(5)))\n    print(console, __render(f(10)))\n",
                vec!["6".to_string(), "11".to_string()],
            ),
            // string.chars ($str_chars → $byte_to_char + $str_substring +
            // $list_push) splitting a multibyte string into a List(String).
            (
                "fn main(console: Console):\n    let cs = string.chars(\"héllo\")\n    print(console, list.at(cs, 0))\n    print(console, list.at(cs, 1))\n    print(console, list.at(cs, 4))\n",
                vec!["h".to_string(), "é".to_string(), "o".to_string()],
            ),
            // list.concat ($list_concat — two memory.copy's into a fresh slot
            // array) on the binary path.
            (
                "fn main(console: Console):\n    let xs = list.concat([10, 20], [30, 40])\n    print(console, __render(list.at(xs, 0)))\n    print(console, __render(list.at(xs, 2)))\n    print(console, __render(list.at(xs, 3)))\n",
                vec!["10".to_string(), "30".to_string(), "40".to_string()],
            ),
            // string.to_upper / to_lower ($ascii_case byte transform) on the
            // binary path.
            (
                "fn main(console: Console):\n    print(console, string.to_upper(\"Hello, World!\"))\n    print(console, string.to_lower(\"Hello, World!\"))\n",
                vec!["HELLO, WORLD!".to_string(), "hello, world!".to_string()],
            ),
            // string.to_int ($str_to_int — whitespace/sign/overflow-checked parse)
            // on the binary path.
            (
                "fn main(console: Console):\n    print(console, __render(string.to_int(\"123\") + string.to_int(\"-23\")))\n",
                vec!["100".to_string()],
            ),
            // string.replace ($replace + $match_at — count-then-fill) on the
            // binary path, including a growing replacement.
            (
                "fn main(console: Console):\n    print(console, string.replace(\"hello world\", \"o\", \"0\"))\n    print(console, string.replace(\"a.b.c\", \".\", \"::\"))\n",
                vec!["hell0 w0rld".to_string(), "a::b::c".to_string()],
            ),
            // dict with String keys ($dict_new/insert/get_or/has/size →
            // $dict_find + $key_eq's $str_eq path) on the binary path.
            (
                "fn main(console: Console):\n    let d = dict.insert(dict.insert(dict.new(), \"a\", 1), \"b\", 2)\n    print(console, __render(dict.get_or(d, \"a\", 0)))\n    print(console, __render(dict.get_or(d, \"z\", 99)))\n    print(console, __render(dict.contains_key(d, \"b\")))\n    print(console, __render(dict.contains_key(d, \"z\")))\n    print(console, __render(dict.length(d)))\n",
                vec!["1".to_string(), "99".to_string(), "true".to_string(), "false".to_string(), "2".to_string()],
            ),
            // dict iteration + remove ($dict_keys/values/pairs/remove). Asserts
            // order-independent facts (lengths, post-remove membership) so it's
            // robust to entry ordering.
            (
                "fn main(console: Console):\n    let d = dict.insert(dict.insert(dict.new(), \"a\", 1), \"b\", 2)\n    print(console, __render(list.length(dict.keys(d))))\n    print(console, __render(list.length(dict.values(d))))\n    print(console, __render(list.length(dict.pairs(d))))\n    let d2 = dict.remove(d, \"a\")\n    print(console, __render(dict.length(d2)))\n    print(console, __render(dict.contains_key(d2, \"a\")))\n    print(console, __render(dict.contains_key(d2, \"b\")))\n",
                vec!["2".to_string(), "2".to_string(), "2".to_string(), "1".to_string(), "false".to_string(), "true".to_string()],
            ),
            // a capturing closure: the lambda closes over `k` (an Int local),
            // recovered from the env at offset 4 on the binary path.
            (
                "fn main(console: Console):\n    let k = 10\n    let g = fn(n: Int): n + k\n    print(console, __render(g(5)))\n    print(console, __render(g(0)))\n",
                vec!["15".to_string(), "10".to_string()],
            ),
            // a closure passed to a user function and called through its
            // fn-typed param (`f(f(x))` — the closure-typed-local call_indirect).
            (
                "fn apply_twice(f: fn(Int) -> Int, x: Int) -> Int:\n    f(f(x))\nfn main(console: Console):\n    let k = 10\n    let g = fn(n: Int): n + k\n    print(console, __render(apply_twice(g, 1)))\n",
                vec!["21".to_string()],
            ),
            // short-circuit `&&`/`||` lower to a value-`If` on the binary path.
            (
                "fn main(console: Console):\n    print(console, __render(true && false))\n    print(console, __render(true || false))\n    print(console, __render(1 < 2 && 3 < 4))\n    print(console, __render(1 > 2 || 3 < 4))\n",
                vec!["false".to_string(), "true".to_string(), "true".to_string(), "true".to_string()],
            ),
            // `&&` must short-circuit: the RHS index would be out of bounds when the
            // LHS guard (`i < n`) is false, so it must NOT be evaluated.
            (
                "fn main(console: Console):\n    let xs = [10, 20]\n    let n = list.length(xs)\n    var i = 0\n    var sum = 0\n    while i < n && list.at(xs, i) > 0:\n        sum = sum + list.at(xs, i)\n        i = i + 1\n    print(console, __render(sum))\n",
                vec!["30".to_string()],
            ),
            // float ordering (`<`/`<=`/`>`/`>=`) lowers to the NaN-trapping
            // `$f_lt`/`$f_le`/`$f_gt`/`$f_ge` helpers on the binary path.
            (
                "fn main(console: Console):\n    print(console, __render(1.5 < 2.5))\n    print(console, __render(2.5 <= 2.5))\n    print(console, __render(3.5 > 2.5))\n    print(console, __render(1.5 >= 2.5))\n",
                vec!["true".to_string(), "true".to_string(), "true".to_string(), "false".to_string()],
            ),
            // string ordering (`<`/`<=`/`>`/`>=`) lowers to `$str_cmp` sign
            // compares — lexicographic, including the prefix tie-break by length.
            (
                "fn main(console: Console):\n    print(console, __render(\"abc\" < \"abd\"))\n    print(console, __render(\"abc\" < \"ab\"))\n    print(console, __render(\"abc\" <= \"abc\"))\n    print(console, __render(\"b\" > \"abc\"))\n    print(console, __render(\"abc\" >= \"abd\"))\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string(), "true".to_string(), "false".to_string()],
            ),
            // a string accumulator (`s = s + ..`) — a self-assign whose in-place fast
            // path is list-only — lowers as a plain value-rebind (the `list.join`
            // shape that blocked ~20 programs). The if/else picks first vs separator.
            (
                "fn main(console: Console):\n    var s = \"\"\n    var first = true\n    for w in [\"a\", \"b\", \"c\"]:\n        if first:\n            s = w\n            first = false\n        else:\n            s = s + \"-\" + w\n    print(console, s)\n",
                vec!["a-b-c".to_string()],
            ),
            // `string.char_count` (Unicode scalars, not bytes) via the `$char_count`
            // → `$byte_to_char` helper — the blocker for parse_int/pad_*.
            (
                "fn main(console: Console):\n    print(console, __render(string.char_count(\"abc\")))\n    print(console, __render(string.char_count(\"héllo\")))\n",
                vec!["3".to_string(), "5".to_string()],
            ),
            // Int<->Float numeric conversions + sqrt (the new `ToFloat`/`ToInt`/`Sqrt`
            // UnOps) and a scalar Float `__render` (via `$float_to_str`).
            (
                "fn main(console: Console):\n    print(console, __render(math.to_int(math.sqrt(16.0))))\n    print(console, __render(math.to_int(math.to_float(7) + 0.5)))\n    print(console, __render(3.5))\n",
                vec!["4".to_string(), "7".to_string(), "3.5".to_string()],
            ),
            // `string.from_code` (Unicode scalar -> single-char string) via the
            // `$string_from_code` host-import wrapper.
            (
                "fn main(console: Console):\n    print(console, string.from_code(65))\n    print(console, string.from_code(233))\n",
                vec!["A".to_string(), "é".to_string()],
            ),
            // a closure bound from a MATCH pattern then called (`Box(f) -> f(x)`) —
            // the `iter.next` shape (`Iter(thunk) -> thunk()`). Now lowers since a
            // local in call position is always a closure (the guard is just `locals`).
            (
                "type Box:\n    Box(fn(Int) -> Int)\nfn apply(b: Box, x: Int) -> Int:\n    match b:\n        Box(f) -> f(x)\nfn main(console: Console):\n    let b = Box(fn(n: Int): n + 1)\n    print(console, __render(apply(b, 5)))\n",
                vec!["6".to_string()],
            ),
            // nested lambdas: an outer lambda built inside another function's body,
            // with two instances in a list — exercises the lifted-lambda index/name
            // fix (a nested lambda lowered during the outer's build must not collide
            // on the outer's table slot).
            (
                "type Adder:\n    Adder(fn(Int) -> Int)\nfn make(base: Int) -> Adder:\n    Adder(fn(x: Int): x + base)\nfn run(a: Adder, v: Int) -> Int:\n    match a:\n        Adder(f) -> f(v)\nfn main(console: Console):\n    let pair = [make(10), make(100)]\n    print(console, __render(run(list.at(pair, 0), 5)))\n    print(console, __render(run(list.at(pair, 1), 5)))\n",
                vec!["15".to_string(), "105".to_string()],
            ),
            // a bare top-level function name passed as a VALUE to a higher-order fn —
            // materialized as a forwarding closure `fn(p): is_odd(p)`.
            (
                "fn is_odd(n: Int) -> Bool:\n    n % 2 == 1\nfn count_if(xs: List(Int), pred: fn(Int) -> Bool) -> Int:\n    var c = 0\n    for x in xs:\n        if pred(x):\n            c = c + 1\n    c\nfn main(console: Console):\n    print(console, __render(count_if([1, 2, 3, 4, 5], is_odd)))\n",
                vec!["3".to_string()],
            ),
            // a `region:` block — a scalar result (reclaimed by stashing the value in
            // a register and resetting `$heap`) and a `List(Int)` result (reclaimed via
            // the generated `$rcopy_list_int`: scalar payload, one `memory.copy`).
            (
                "fn main(console: Console):\n    let s = region -> Int:\n        var sum = 0\n        for i in 0..10:\n            sum = sum + i\n        sum\n    print(console, __render(s))\n    let xs = region -> List(Int):\n        var ys = []\n        for i in 0..5:\n            ys = list.push(ys, i * i)\n        ys\n    print(console, __render(list.at(xs, 3)))\n",
                vec!["45".to_string(), "9".to_string()],
            ),
            // a `region -> (Int, String):` tuple — the generated `$rcopy_tuple_*`
            // copies the tag, the scalar slot verbatim, and recurses through
            // `$rcopy_str` for the string slot. The biased copy-out keeps `t.1`
            // pointing at the reclaimed string; `after` reuses the freed space.
            (
                "fn main(console: Console):\n    let t = region -> (Int, String):\n        var acc = \"\"\n        for i in 0..3:\n            acc = acc + \"z\"\n        (7 * 6, acc)\n    let after = \"OK\"\n    print(console, __render(t))\n    print(console, t.1)\n    print(console, after)\n",
                vec!["(42, zzz)".to_string(), "zzz".to_string(), "OK".to_string()],
            ),
            // a `region -> List(String):` — a list with a COMPOUND payload: the
            // generated `$rcopy_list_str` writes the length header then deep-copies
            // each element string through `$rcopy_str`, so every slot holds a biased
            // pointer into the reclaimed block.
            (
                "fn main(console: Console):\n    let xs = region -> List(String):\n        var ys = []\n        for i in 0..3:\n            ys = list.push(ys, \"n\" + __render(i))\n        ys\n    let after = \"OK\"\n    print(console, list.at(xs, 0))\n    print(console, list.at(xs, 2))\n    print(console, after)\n",
                vec!["n0".to_string(), "n2".to_string(), "OK".to_string()],
            ),
            // enum/record `__render`: the generated `$ts_*` tag-dispatch helper emits
            // `Name` (nullary), `Name(f0, f1, ...)` (fields), and a record positionally
            // (`Point(5, 6)`), matching the interpreter's `Value::Ctor` Display. Unlike
            // enum `==`, the WAT path renders enums structurally too, so all three agree.
            (
                "type Color:\n    Red\n    Green\n    RGB(Int, Int, Int)\n\ntype Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let c = RGB(1, 2, 3)\n    let p = Point(x: 5, y: 6)\n    let g = Green\n    print(console, \"${c}\")\n    print(console, \"${p}\")\n    print(console, \"${g}\")\n",
                vec!["RGB(1, 2, 3)".to_string(), "Point(5, 6)".to_string(), "Green".to_string()],
            ),
            // `__render` of an INLINE call result (`"${mklist()}"`) — the shape comes
            // from typeck's type table (eq_operand_shape), not just tracked locals, so
            // a compound expression renders without being bound to a `let` first.
            (
                "fn mklist() -> List(Int):\n    [1, 2, 3]\n\nfn pair() -> (Int, String):\n    (7, \"x\")\n\nfn main(console: Console):\n    print(console, \"${mklist()}\")\n    print(console, \"${pair()}\")\n",
                vec!["[1, 2, 3]".to_string(), "(7, x)".to_string()],
            ),
            // Render of a self-RECURSIVE ADT (`Node(Tree, Tree)`): the `$ts`
            // helper's name is reserved before its body is built, so the nested
            // `Tree` fields render via a recursive `call` to the same helper
            // (tying the knot) rather than bailing the cycle guard. The WAT path
            // renders enums structurally too, so all three backends agree.
            (
                "type Tree:\n    Leaf(Int)\n    Node(Tree, Tree)\n\nfn main(console: Console):\n    let t = Node(Node(Leaf(1), Leaf(2)), Leaf(3))\n    print(console, \"${t}\")\n",
                vec!["Node(Node(Leaf(1), Leaf(2)), Leaf(3))".to_string()],
            ),
            // `var` parameters (the multi-value move-out ABI): the callee returns
            // its declared value plus each var param's final value, and the call
            // site (`CallStoreMulti`) writes them back into the caller's vars. Covers
            // a bare var, repeated calls, and an var alongside a non-var arg.
            (
                "fn bump(var n: Int):\n    n = n + 1\nfn add(var n: Int, by: Int):\n    n = n + by\nfn main(console: Console):\n    var a = 0\n    bump(a)\n    bump(a)\n    bump(a)\n    add(a, 10)\n    print(console, __render(a))\n",
                vec!["13".to_string()],
            ),
            // a `region -> String:` — a POINTER result reclaimed via `$rcopy_str`
            // (deep-copy the region-born string down to the watermark, return the
            // biased ptr). The following `let after` allocates right where the region
            // was reclaimed, so a bad copy/slide would corrupt it.
            (
                "fn main(console: Console):\n    let s = region -> String:\n        var acc = \"\"\n        for i in 0..5:\n            acc = acc + \"x\"\n        acc\n    let after = \"ok\"\n    print(console, s)\n    print(console, after)\n",
                vec!["xxxxx".to_string(), "ok".to_string()],
            ),
        ];
        let mut lowered_any = false;
        for (src, want) in cases {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect("compile_module_binary")
                .unwrap_or_else(|| {
                    panic!("expected the WIR binary path to handle this program:\n{src}")
                });
            lowered_any = true;
            // AST → WIR → binary (no wat::parse_str) runs identically to the
            // interpreter oracle and to the legacy WAT sink — AND under a
            // print-ONLY grant, proving the pruned module imports only `print`.
            assert_eq!(&run_bytes_print_only(&bytes), want, "binary path (print-only):\n{src}");
            assert_eq!(&link_run(src), want, "interpreter oracle:\n{src}");
            assert_eq!(&run_on_wasm(src), want, "legacy WAT path:\n{src}");
        }
        assert!(lowered_any, "the WIR binary path lowered nothing — convergence regressed");
    }

    /// The first host-import helper ($encoding) on the binary path. Kept out of
    /// the corpus above because `encoding.*` requires `import encoding`, which the
    /// corpus's `run_on_wasm`/`typeck::check_str` leg can't resolve (it doesn't
    /// pull in std modules); the linked interpreter oracle (`link_run`) can. So we
    /// compare the pruned binary against the interpreter directly. The pruned
    /// module must import "encoding" alongside "print".
    #[test]
    fn wir_encoding_host_import_binary_path() {
        let src = "import encoding\nfn main(console: Console):\n    print(console, encoding.hex_encode(\"Hi\"))\n    print(console, encoding.base64_encode(\"Hi\"))\n";
        let want = vec!["4869".to_string(), "SGk=".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should handle encoding via the host import");
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
        let src = "type CalcError:\n    StackUnderflow\n    UnknownToken(String)\n    DivByZero\n\nfn main(console: Console):\n    let a: Option(CalcError) = None\n    let b: Option(CalcError) = Some(StackUnderflow)\n    let c: Option(CalcError) = Some(UnknownToken(\"x\"))\n    let d: Option(CalcError) = Some(UnknownToken(\"y\"))\n    let cx: Option(CalcError) = Some(UnknownToken(\"x\"))\n    print(console, \"${a == None}\")\n    print(console, \"${b == None}\")\n    print(console, \"${b == Some(StackUnderflow)}\")\n    print(console, \"${c == cx}\")\n    print(console, \"${c == d}\")\n    print(console, \"${b == c}\")\n";
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
            .expect("compile_module_binary")
            .expect("the WIR binary path should structurally lower enum `==`");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// Dict `__render` on the binary path — the generated `$ts_dict_*` helper walks
    /// the `[count][key, value]…` entries (16-byte stride) emitting `{k: v, ...}` in
    /// insertion order. Kept OUT of the 3-way corpus because `dict.from_pairs` is a
    /// std fn the corpus's `check_str`/`run_on_wasm` leg can't resolve (like the
    /// encoding case); compared against the linked interpreter oracle directly.
    #[test]
    fn wir_dict_render_binary_path() {
        let src = "fn main(console: Console):\n    let d = dict.from_pairs([(\"a\", 1), (\"b\", 2), (\"c\", 3)])\n    print(console, \"${d}\")\n";
        let want = vec!["{a: 1, b: 2, c: 3}".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should render a dict");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// `dict.update` on the binary path — the closure-driven upsert. The updater
    /// `fn(c: Int): c + 1` lowers to a lambda the table calls indirectly; the
    /// `$dict_update` helper reads the current value (or `default`), applies the
    /// closure via `call_indirect (type $clos1)`, and reinserts. (2-way: the
    /// WAT-leg `check_str` can't resolve std `dict.*`, so compare binary vs
    /// interpreter directly.)
    #[test]
    fn wir_dict_update_binary_path() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    d = dict.update(d, \"a\", 0, fn(c: Int): c + 1)\n    d = dict.update(d, \"a\", 0, fn(c: Int): c + 1)\n    d = dict.update(d, \"a\", 0, fn(c: Int): c + 1)\n    d = dict.update(d, \"b\", 0, fn(c: Int): c + 1)\n    print(console, \"${d}\")\n    print(console, \"${dict.get_or(d, \"a\", -1)}\")\n";
        let want = vec!["{a: 3, b: 1}".to_string(), "3".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should lower dict.update");
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
        let src = "type Tree:\n    Leaf(Int)\n    Node(Tree, Tree)\n\nfn main(console: Console):\n    let a = Node(Node(Leaf(1), Leaf(2)), Leaf(3))\n    let b = Node(Node(Leaf(1), Leaf(2)), Leaf(3))\n    let c = Node(Node(Leaf(1), Leaf(9)), Leaf(3))\n    print(console, \"${a == b}\")\n    print(console, \"${a == c}\")\n";
        let want = vec!["true".to_string(), "false".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should compare a recursive ADT");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// The own-ABI (`own` buffer param threaded through `move`) on the binary
    /// path: `grow` takes `own xs`, appends in place, and returns it. The callee
    /// gains a trailing `$xs__cap` i32 param and a second i32 result (the
    /// ownership token); the self-call `xs = grow(move xs, i)` lowers to a
    /// CallStoreMulti capturing (value → xs, cap → xs__cap). (2-way: `list.push`
    /// isn't resolvable by the WAT-leg `check_str`, so compare binary vs oracle.)
    #[test]
    fn wir_own_abi_move_pipeline_binary_path() {
        let src = "fn grow(own xs: List(Int), n: Int) -> List(Int):\n    xs = list.push(xs, n)\n    xs\n\nfn main(console: Console):\n    var xs = []\n    for i in 1..6:\n        xs = grow(move xs, i)\n    print(console, \"${xs}\")\n";
        let want = vec!["[1, 2, 3, 4, 5]".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should lower the own-ABI move pipeline");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// An in-place accumulator INSIDE a lifted lambda on the binary path: the
    /// lambda's own `var acc = [...]` + self-push loop needs its `$acc__cap`
    /// ownership-token shadow declared as a local in the lifted `$__lamw{i}`. The
    /// builder snapshots the lambda's `inplace_push` set before restoring the
    /// enclosing function's, so the cap local isn't dropped (was: encode panic
    /// "unknown local $acc__cap"). (2-way: list.push isn't WAT-leg resolvable.)
    #[test]
    fn wir_lambda_inplace_accumulator_binary_path() {
        let src = "fn main(console: Console):\n    let build = fn(n: Int):\n        var acc = [0]\n        var t = 0\n        while t < n:\n            acc = list.push(acc, t)\n            t = t + 1\n        list.length(acc)\n    print(console, \"${build(5)}\")\n";
        let want = vec!["6".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should lower a lambda-local accumulator");
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
        let src = "import regex\nfn main(console: Console):\n    print(console, \"${regex.matches(\"[0-9]+\", \"order 1234\")}\")\n    print(console, \"${regex.find_all(\"[0-9]+\", \"a1 b22 c333\")}\")\n    print(console, regex.replace_all(\"[0-9]+\", \"a1b22\", \"N\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should lower regex via the host engine");
        let want = link_run(src);
        assert_eq!(want[0], "true", "regex.matches sanity");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path vs oracle");
    }

    /// The crypto digest helpers ($crypto_sha256/sha512/sha3_256/hmac_sha256) on
    /// the binary path — host-import wrappers returning a String. The crypto
    /// imports are host-provided regardless of grant (hashing needs no
    /// capability), so the print-only harness instantiates them. Compared against
    /// the linked interpreter oracle (which computes the real digests).
    #[test]
    fn wir_crypto_digests_host_import_binary_path() {
        let src = "import crypto\nfn main(console: Console):\n    print(console, crypto.sha256(\"abc\"))\n    print(console, crypto.sha512(\"abc\"))\n    print(console, crypto.sha3_256(\"abc\"))\n    print(console, crypto.hmac_sha256(\"abcdef\", \"msg\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should handle the crypto digests via host imports");
        let want = link_run(src);
        // SHA-256("abc") — a well-known vector — confirms the host actually wrote it.
        assert_eq!(want[0], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(want[1].len(), 128, "sha512 hex is 128 chars");
        assert_eq!(want[2].len(), 64, "sha3_256 hex is 64 chars");
        assert_eq!(want[3].len(), 64, "hmac-sha256 hex is 64 chars");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path vs oracle");
    }

    /// `$crypto_rune_hash` on the binary path — a host-import wrapper taking two
    /// List(String) args (list literals lower fine) and returning a 71-char
    /// digest String. Ungated, so the print-only harness instantiates it.
    #[test]
    fn wir_crypto_rune_hash_host_import_binary_path() {
        let src = "import crypto\nfn main(console: Console):\n    print(console, crypto.rune_hash([\"a.witchy\", \"b.witchy\"], [\"fn one\", \"fn two\"]))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should handle crypto.rune_hash via the host import");
        let want = link_run(src);
        assert_eq!(want.len(), 1);
        assert_eq!(want[0].len(), 71, "rune_hash digest is 71 chars");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path vs oracle");
    }

    /// `crypto.ecdsa_p256_verify` on the binary path — a P-256 ECDSA verify host
    /// import (`$crypto_ecdsa_p256_verify`, three string headers → i32 bool,
    /// no capability). A valid (pubkey, message, signature) triple verifies; a
    /// tampered message does not. Compared against the linked interpreter oracle.
    #[test]
    fn wir_crypto_ecdsa_verify_binary_path() {
        let pk = "048f81cd9fca785a42a6f5dd58972cc0f702e83b1c960b5912354471496597e227fec81ff1d52530b06d7091649e6beb49dba70968b4b727bb24e3ceb7dd01a039";
        let sig = "304402203260029f4c6beb2e78afdd906c057c63f8828e2b03820de7053d97254577fb8c02204478b9b75f8fd7a1ce4298f0d119e12926dafda116ae4c197b0048dc117bc9de";
        let src = format!("import crypto\nfn main(console: Console):\n    print(console, if crypto.ecdsa_p256_verify(\"{pk}\", \"webauthn-es256-test-message\", \"{sig}\"): \"ok\" else: \"bad\")\n    print(console, if crypto.ecdsa_p256_verify(\"{pk}\", \"tampered\", \"{sig}\"): \"ok\" else: \"bad\")\n");
        let module = parser::parse_module(&src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should lower crypto.ecdsa_p256_verify");
        let want = vec!["ok".to_string(), "bad".to_string()];
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(&src), want, "interpreter oracle");
    }

    /// `$crypto_sign` + `$crypto_public_key` on the binary path — the Secret
    /// capability host imports (the seed never enters guest memory). Both need a
    /// signing key granted; the run yields a 64-char public-key hex and a 128-char
    /// signature hex.
    #[test]
    fn wir_crypto_signing_host_imports_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import crypto\nfn main(console: Console, signer: Secret):\n    print(console, crypto.public_key(signer))\n    print(console, crypto.sign(signer, \"hello\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should handle the signing host imports");
        let caps = || Capabilities { print: true, signing_key: Some([7u8; 32]), quiet: true, ..Default::default() };
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt.spawn(&bytes, caps(), crate::RUN_MEMORY_PAGES).expect("spawn with signing key");
        actor.run().expect("run");
        let got = actor.output();
        assert_eq!(got[0].len(), 64, "public key hex is 64 chars");
        assert_eq!(got[1].len(), 128, "signature hex is 128 chars");
    }

    /// `crypto.ed25519_verify` on the binary path — the signature verify the
    /// self-hosted package manager (coven/pm) uses, and the construct whose
    /// missing wir_helper made `pm.witchy` fall back to WAT. Sign a message with
    /// the granted Secret, then verify: the valid (pubkey, message, signature)
    /// triple verifies true, a tampered message false.
    #[test]
    fn wir_crypto_ed25519_verify_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import crypto\nfn main(console: Console, signer: Secret):\n    let pk = crypto.public_key(signer)\n    let sig = crypto.sign(signer, \"hello\")\n    print(console, \"${crypto.ed25519_verify(pk, \"hello\", sig)}\")\n    print(console, \"${crypto.ed25519_verify(pk, \"tampered\", sig)}\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should lower crypto.ed25519_verify");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities { print: true, signing_key: Some([7u8; 32]), quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn with signing key");
        actor.run().expect("run");
        assert_eq!(actor.output(), vec!["true".to_string(), "false".to_string()]);
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
        let src = "fn main(console: Console, dir: Dir[Read]):\n    print(console, read(dir, \"greeting.txt\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should handle dir read via the host imports");
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

    /// `$dir_list` (list a directory) on the binary path — the host reports the
    /// marshaled-list size (`dir_list_size`) then writes it (`write_pending_list`),
    /// gated behind Dir(Read). Counts the directory's entries.
    #[test]
    fn wir_dir_list_host_import_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wir_dirlist_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("one.txt"), "1").expect("write");
        std::fs::write(root.join("two.txt"), "2").expect("write");
        let src = "fn main(console: Console, dir: Dir[Read]):\n    print(console, __render(list.length(list(dir))))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should handle dir list via the host imports");
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
        assert_eq!(got, vec!["2".to_string()], "binary path: 2 entries");
    }

    /// `$get_env` on the binary path — a host-import helper returning an
    /// `Option(String)`, consumed via `match` (now lowering via the
    /// constructor-pattern arm). The absent branch is deterministic ("unset");
    /// the present branch (PATH) takes the `Some` arm. Env grant.
    #[test]
    fn wir_get_env_option_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "fn main(console: Console, env: Env):\n    match get_env(env, \"WITCHY_UNSET_XYZZY_VAR\"):\n        Some(v) -> print(console, v)\n        None -> print(console, \"unset\")\n    match get_env(env, \"PATH\"):\n        Some(v) -> print(console, \"has\")\n        None -> print(console, \"no-path\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should handle get_env + match");
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
        let src = "fn main(console: Console) -> Int:\n    print(console, \"hi\")\n    42\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile_module_binary")
            .expect("the WIR binary path should handle an Int-returning main");
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
        use crate::runtime::{Capabilities, Runtime};
        let mods: Vec<(String, ast::Module)> = sources
            .iter()
            .map(|(n, s)| ((*n).to_string(), parser::parse_module(s).expect("parse")))
            .collect();
        let linked = crate::pipeline::link(mods, entry).expect("link");
        assert!(typeck::check(&linked).is_ok(), "{:?}", typeck::check(&linked));
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
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

    /// RFC-0011: `net.deny(policy)` subtracts an address pattern from a `Net` — the
    /// monotone allow/deny algebra `effective = allows \ denies`, recorded as a
    /// `!`-prefixed allowlist entry honoured by the shared `net_allows`. A non-denied
    /// address still narrows on both backends; a denied one is refused.
    #[test]
    fn net_deny_subtracts_addresses_backends_agree() {
        let src = "import confine\nfn main(net: Net, console: Console):\n    let d = net.deny(confine.cidr_any(\"10.0.0.0/8\"))\n    let ok = only(d, confine.tcp(\"192.168.1.1\", 80))\n    print(console, \"denied\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let grant = vec!["10.0.0.0/8:*".to_string(), "192.168.1.1:80".to_string()];
        let expected = vec!["denied".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", grant.clone()).expect("interp"),
            expected,
            "interpreter: a non-denied address still narrows",
        );
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["10.0.0.0/8:*", "192.168.1.1:80"]),
            expected,
            "wasm",
        );
        // A denied address (inside the denied block) is refused — the exclusion bites.
        let bad = "import confine\nfn main(net: Net, console: Console):\n    let d = net.deny(confine.cidr_any(\"10.0.0.0/8\"))\n    let x = only(d, confine.tcp(\"10.0.0.5\", 6379))\n    print(console, \"unreached\")\n";
        let bad_linked = resolve_std_src(bad);
        typeck::check(&bad_linked).expect("typecheck");
        assert!(
            interpreter::run_module(bad_linked, ".", vec!["10.0.0.0/8:*".into()]).is_err(),
            "a denied address must be refused",
        );
    }

    /// A method call on a let-bound capability-op RESULT (`net.deny(...)` then
    /// `d.only(...)`) resolves — cap-op intrinsics now carry a return type so the
    /// trait-lowering types the binding, not just function parameters. Both backends
    /// agree, so chained refinement (`net.deny(...).only(...)`) is usable.
    #[test]
    fn cap_method_chaining_on_let_bindings_backends_agree() {
        let src = "import confine\nfn main(net: Net, console: Console):\n    let d = net.deny(confine.cidr_any(\"10.0.0.0/8\"))\n    let ok = d.only(confine.tcp(\"192.168.1.1\", 80))\n    print(console, \"chained\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let expected = vec!["chained".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec!["10.0.0.0/8:*".into(), "192.168.1.1:80".into()]).expect("interp"),
            expected,
            "interpreter",
        );
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src)], "main", &["10.0.0.0/8:*", "192.168.1.1:80"]),
            expected,
            "wasm",
        );
    }

    /// RS256 (`crypto.rsa_pkcs1_sha256_verify`, the OIDC/JWT signature algorithm) is
    /// reachable and TOTAL on both backends — a malformed key/signature yields `false`,
    /// never a trap. (The verify LOGIC is proven by `rs256_native_roundtrip_verifies`.)
    #[test]
    fn rsa_pkcs1_sha256_verify_total_backends_agree() {
        let src = "import crypto\nfn main(console: Console):\n    if crypto.rsa_pkcs1_sha256_verify(\"00\", \"msg\", \"00\"):\n        print(console, \"valid\")\n    else:\n        print(console, \"invalid\")\n";
        let expected = vec!["invalid".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// RS256 verify LOGIC is correct: a real RSA-2048 PKCS#1 signature over a message
    /// verifies, a wrong message is rejected, and a malformed key is total — exercising
    /// the native aws-lc path both backends route through.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn rs256_native_roundtrip_verifies() {
        use crate::value::NativeValue as NV;
        use aws_lc_rs::signature::KeyPair; // brings `public_key()` into scope
        let hexs = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let pk_hex = hexs(kp.public_key().as_ref());
        let msg = "hello rs256";
        let mut sig = vec![0u8; kp.public_modulus_len()];
        kp.sign(
            &aws_lc_rs::signature::RSA_PKCS1_SHA256,
            &aws_lc_rs::rand::SystemRandom::new(),
            msg.as_bytes(),
            &mut sig,
        )
        .expect("sign");
        let sig_hex = hexs(&sig);
        // Reach the intrinsic through the PUBLIC native registry (the path both backends use).
        let f = crate::native::lookup("crypto.rsa_pkcs1_sha256_verify").expect("registered");
        let verify = |pk: &str, m: &str, s: &str| {
            f(&[NV::Str(pk.into()), NV::Str(m.into()), NV::Str(s.into())]).unwrap()
        };
        assert_eq!(verify(&pk_hex, msg, &sig_hex), NV::Bool(true), "valid RS256 signature verifies");
        assert_eq!(verify(&pk_hex, "tampered", &sig_hex), NV::Bool(false), "wrong message rejected");
        assert_eq!(verify("00", msg, &sig_hex), NV::Bool(false), "malformed key is total (false)");
    }

    /// End-to-end `std/jwt`: a REAL aws-lc-signed compact RS256 JWT, embedded as a
    /// witchy string literal, verifies identically on both backends — `Ok` yields the
    /// claims (we read `sub`); a tampered signature, an expired token, and a wrong
    /// audience each reject with the module's reason. Proves `jwt.verify_rs256`
    /// composes RS256 + base64url + json end to end, with no host capability.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_verify_rs256_backends_agree() {
        use aws_lc_rs::signature::KeyPair;
        // base64url, no padding — the JWT segment encoding.
        fn b64url(bytes: &[u8]) -> String {
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for c in bytes.chunks(3) {
                let n = ((c[0] as u32) << 16)
                    | ((*c.get(1).unwrap_or(&0) as u32) << 8)
                    | (*c.get(2).unwrap_or(&0) as u32);
                out.push(A[(n >> 18 & 63) as usize] as char);
                out.push(A[(n >> 12 & 63) as usize] as char);
                if c.len() > 1 {
                    out.push(A[(n >> 6 & 63) as usize] as char);
                }
                if c.len() > 2 {
                    out.push(A[(n & 63) as usize] as char);
                }
            }
            out
        }
        let hexs = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let pk_hex = hexs(kp.public_key().as_ref());
        let sign_jwt = |payload: &str| -> String {
            let signed = format!("{}.{}", b64url(br#"{"alg":"RS256","typ":"JWT"}"#), b64url(payload.as_bytes()));
            let mut sig = vec![0u8; kp.public_modulus_len()];
            kp.sign(
                &aws_lc_rs::signature::RSA_PKCS1_SHA256,
                &aws_lc_rs::rand::SystemRandom::new(),
                signed.as_bytes(),
                &mut sig,
            )
            .expect("sign");
            format!("{signed}.{}", b64url(&sig))
        };
        let good = sign_jwt(r#"{"aud":"coven","exp":9999,"sub":"octocat"}"#);
        let expired = sign_jwt(r#"{"aud":"coven","exp":5,"sub":"octocat"}"#);
        let wrong_aud = sign_jwt(r#"{"aud":"evil","exp":9999,"sub":"octocat"}"#);
        let tampered = {
            // Flip the FIRST char of the signature segment. base64url's last char of a
            // 256-byte RSA signature carries only 2 significant bits (4 are padding), so
            // a last-char flip can decode to the same bytes — a no-op; the first char is
            // always fully significant, so this reliably corrupts the signature.
            let sig_start = good.rfind('.').unwrap() + 1;
            let mut chars: Vec<char> = good.chars().collect();
            chars[sig_start] = if chars[sig_start] == 'A' { 'B' } else { 'A' };
            chars.into_iter().collect::<String>()
        };
        // `now` = 1000, audience "coven". Print `sub` on success, else the error.
        let run = |token: &str| -> Vec<String> {
            let src = format!(
                "import jwt\nimport json\nfn main(console: Console):\n    match jwt.verify_rs256(\"{token}\", \"{pk_hex}\", \"coven\", 1000):\n        Ok(claims) -> print(console, json.get_string(claims, \"sub\").unwrap_or(\"?\"))\n        Err(e) -> print(console, e)\n"
            );
            let interp = link_run(&src);
            let wasm = run_linked_on_wasm(&[("main", src.as_str())], "main");
            assert_eq!(interp, wasm, "interp vs wasm must agree");
            interp
        };
        assert_eq!(run(&good), vec!["octocat".to_string()], "valid JWT yields its claims");
        assert_eq!(run(&expired), vec!["JWT has expired".to_string()]);
        assert_eq!(
            run(&wrong_aud),
            vec!["JWT audience mismatch (wrong relying party / replay)".to_string()]
        );
        assert_eq!(
            run(&tampered),
            vec!["JWT signature is invalid (untrusted or forged)".to_string()]
        );
    }

    /// `jwt.rsa_key_from_jwk` reconstructs a DER PKCS#1 RSA public key from a JWK's
    /// base64url `n`/`e` BYTE-FOR-BYTE identically to aws-lc's own encoding — the pure-
    /// witchy ASN.1 DER (length long-form, the signed-integer `00` pad) is exact, and
    /// matches on both backends. This is the bridge from a JWKS entry to `verify_rs256`.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_rsa_key_from_jwk_matches_aws_lc_der() {
        use aws_lc_rs::signature::KeyPair;
        fn b64url(bytes: &[u8]) -> String {
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for c in bytes.chunks(3) {
                let n = ((c[0] as u32) << 16)
                    | ((*c.get(1).unwrap_or(&0) as u32) << 8)
                    | (*c.get(2).unwrap_or(&0) as u32);
                out.push(A[(n >> 18 & 63) as usize] as char);
                out.push(A[(n >> 12 & 63) as usize] as char);
                if c.len() > 1 {
                    out.push(A[(n >> 6 & 63) as usize] as char);
                }
                if c.len() > 2 {
                    out.push(A[(n & 63) as usize] as char);
                }
            }
            out
        }
        // Read the two INTEGER contents of a DER `SEQUENCE { INTEGER, INTEGER }`.
        fn two_ints(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
            fn len_at(b: &[u8], i: &mut usize) -> usize {
                let mut len = b[*i] as usize;
                *i += 1;
                if len & 0x80 != 0 {
                    let nbytes = len & 0x7f;
                    len = 0;
                    for _ in 0..nbytes {
                        len = (len << 8) | b[*i] as usize;
                        *i += 1;
                    }
                }
                len
            }
            fn tlv(b: &[u8], i: &mut usize) -> Vec<u8> {
                *i += 1; // tag
                let len = len_at(b, i);
                let v = b[*i..*i + len].to_vec();
                *i += len;
                v
            }
            let mut i = 0;
            i += 1; // SEQUENCE tag
            let _ = len_at(der, &mut i); // SEQUENCE length (then parse contents)
            let n = tlv(der, &mut i);
            let e = tlv(der, &mut i);
            (n, e)
        }
        let hexs = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let der = kp.public_key().as_ref();
        let (n_int, e_int) = two_ints(der);
        // The JWK carries the unsigned magnitude — drop the DER sign byte if present.
        let strip = |v: &[u8]| if v.first() == Some(&0) { v[1..].to_vec() } else { v.to_vec() };
        let n_b64 = b64url(&strip(&n_int));
        let e_b64 = b64url(&strip(&e_int));
        let src = format!(
            "import jwt\nfn main(console: Console):\n    print(console, jwt.rsa_key_from_jwk(\"{n_b64}\", \"{e_b64}\"))\n"
        );
        let expected = vec![hexs(der)];
        assert_eq!(link_run(&src), expected, "interp: JWK->DER byte-exact vs aws-lc");
        assert_eq!(run_linked_on_wasm(&[("main", src.as_str())], "main"), expected, "wasm");
    }

    /// `jwt.verify_oidc` is the full relying-party check: a real RS256 GitHub-Actions-
    /// shaped OIDC token verifies only against its TRUE issuer (the bind to a trusted
    /// provider), and rejects a not-yet-active (`nbf`) token. On success the caller reads
    /// identity claims — here the `repository` a trusted-publishing flow would authorize.
    /// Both backends agree. This is the verification half of OIDC login / publishing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_verify_oidc_binds_issuer_backends_agree() {
        use aws_lc_rs::signature::KeyPair;
        fn b64url(bytes: &[u8]) -> String {
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for c in bytes.chunks(3) {
                let n = ((c[0] as u32) << 16)
                    | ((*c.get(1).unwrap_or(&0) as u32) << 8)
                    | (*c.get(2).unwrap_or(&0) as u32);
                out.push(A[(n >> 18 & 63) as usize] as char);
                out.push(A[(n >> 12 & 63) as usize] as char);
                if c.len() > 1 {
                    out.push(A[(n >> 6 & 63) as usize] as char);
                }
                if c.len() > 2 {
                    out.push(A[(n & 63) as usize] as char);
                }
            }
            out
        }
        let hexs = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let pk_hex = hexs(kp.public_key().as_ref());
        let sign_jwt = |payload: &str| -> String {
            let signed = format!("{}.{}", b64url(br#"{"alg":"RS256","typ":"JWT"}"#), b64url(payload.as_bytes()));
            let mut sig = vec![0u8; kp.public_modulus_len()];
            kp.sign(
                &aws_lc_rs::signature::RSA_PKCS1_SHA256,
                &aws_lc_rs::rand::SystemRandom::new(),
                signed.as_bytes(),
                &mut sig,
            )
            .expect("sign");
            format!("{signed}.{}", b64url(&sig))
        };
        let gh = "https://token.actions.githubusercontent.com";
        let token = sign_jwt(
            r#"{"iss":"https://token.actions.githubusercontent.com","aud":"coven","sub":"repo:octo/witchy:ref:refs/heads/main","repository":"octo/witchy","nbf":0,"exp":9999}"#,
        );
        let future = sign_jwt(
            r#"{"iss":"https://token.actions.githubusercontent.com","aud":"coven","repository":"octo/witchy","nbf":5000,"exp":9999}"#,
        );
        // (token, issuer-to-trust) -> printed line. now = 1000, audience "coven".
        let run = |tok: &str, issuer: &str| -> Vec<String> {
            let src = format!(
                "import jwt\nimport json\nfn main(console: Console):\n    match jwt.verify_oidc(\"{tok}\", \"{pk_hex}\", \"{issuer}\", \"coven\", 1000):\n        Ok(claims) -> print(console, json.get_string(claims, \"repository\").unwrap_or(\"?\"))\n        Err(e) -> print(console, e)\n"
            );
            let interp = link_run(&src);
            assert_eq!(interp, run_linked_on_wasm(&[("main", src.as_str())], "main"), "backends agree");
            interp
        };
        assert_eq!(run(&token, gh), vec!["octo/witchy".to_string()], "trusted issuer admits, claims readable");
        assert_eq!(
            run(&token, "https://evil.example"),
            vec!["JWT issuer mismatch (untrusted identity provider)".to_string()],
            "a token from the wrong issuer is rejected even with a valid signature"
        );
        assert_eq!(
            run(&future, gh),
            vec!["JWT is not yet valid (nbf is in the future)".to_string()]
        );
    }

    /// The full OIDC-via-JWKS verification (how "Log in with Google" / GitHub-Actions
    /// publishing checks an id_token): read the token's `kid`, pick the matching RSA key
    /// from the provider's published JWKS, and `verify_oidc`. Exercised against a REAL
    /// aws-lc-signed id_token + a JWKS built from the same key — identical on both backends.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn jwt_verify_oidc_via_jwks_backends_agree() {
        use aws_lc_rs::signature::KeyPair;
        fn b64url(bytes: &[u8]) -> String {
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for c in bytes.chunks(3) {
                let n = ((c[0] as u32) << 16)
                    | ((*c.get(1).unwrap_or(&0) as u32) << 8)
                    | (*c.get(2).unwrap_or(&0) as u32);
                out.push(A[(n >> 18 & 63) as usize] as char);
                out.push(A[(n >> 12 & 63) as usize] as char);
                if c.len() > 1 {
                    out.push(A[(n >> 6 & 63) as usize] as char);
                }
                if c.len() > 2 {
                    out.push(A[(n & 63) as usize] as char);
                }
            }
            out
        }
        fn two_ints(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
            fn len_at(b: &[u8], i: &mut usize) -> usize {
                let mut len = b[*i] as usize;
                *i += 1;
                if len & 0x80 != 0 {
                    let nbytes = len & 0x7f;
                    len = 0;
                    for _ in 0..nbytes {
                        len = (len << 8) | b[*i] as usize;
                        *i += 1;
                    }
                }
                len
            }
            fn tlv(b: &[u8], i: &mut usize) -> Vec<u8> {
                *i += 1;
                let len = len_at(b, i);
                let v = b[*i..*i + len].to_vec();
                *i += len;
                v
            }
            let mut i = 0;
            i += 1;
            let _ = len_at(der, &mut i);
            (tlv(der, &mut i), tlv(der, &mut i))
        }
        let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("keygen");
        let (n_int, e_int) = two_ints(kp.public_key().as_ref());
        let strip = |v: &[u8]| if v.first() == Some(&0) { v[1..].to_vec() } else { v.to_vec() };
        let jwks = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"google-key-1","n":"{}","e":"{}"}}]}}"#,
            b64url(&strip(&n_int)),
            b64url(&strip(&e_int))
        );
        let signed = format!(
            "{}.{}",
            b64url(br#"{"alg":"RS256","kid":"google-key-1","typ":"JWT"}"#),
            b64url(br#"{"iss":"https://accounts.google.com","aud":"myclient","email":"a@b.com","sub":"42","exp":9999,"nbf":0}"#)
        );
        let mut sig = vec![0u8; kp.public_modulus_len()];
        kp.sign(
            &aws_lc_rs::signature::RSA_PKCS1_SHA256,
            &aws_lc_rs::rand::SystemRandom::new(),
            signed.as_bytes(),
            &mut sig,
        )
        .expect("sign");
        let token = format!("{signed}.{}", b64url(&sig));
        let jwks_lit = jwks.replace('"', "\\\"");
        let src = format!(
            "import jwt\nimport json\nfn main(console: Console):\n    match json.decode(\"{jwks_lit}\"):\n        Err(e) -> print(console, \"bad jwks\")\n        Ok(doc) ->\n            match jwt.kid(\"{token}\"):\n                None -> print(console, \"no kid\")\n                Some(k) ->\n                    match jwt.rsa_key_for_kid(doc, k):\n                        Err(e) -> print(console, \"key: \" + e)\n                        Ok(der) ->\n                            match jwt.verify_oidc(\"{token}\", der, \"https://accounts.google.com\", \"myclient\", 1000):\n                                Ok(claims) -> print(console, json.get_string(claims, \"email\").unwrap_or(\"?\"))\n                                Err(e) -> print(console, e)\n"
        );
        let expected = vec!["a@b.com".to_string()];
        assert_eq!(link_run(&src), expected, "interp OIDC-via-JWKS");
        assert_eq!(run_linked_on_wasm(&[("main", src.as_str())], "main"), expected, "wasm OIDC-via-JWKS");
    }

    /// `jwt.claims_unverified` decodes a token's payload WITHOUT checking the signature —
    /// for reading `iss` to select the verification key before `verify_oidc`. Both backends.
    #[test]
    fn jwt_claims_unverified_reads_routing_fields() {
        let src = "import jwt\nimport json\nimport encoding\nfn main(console: Console):\n    let payload = encoding.base64url_of_hex(encoding.hex_encode(\"{\\\"iss\\\":\\\"acme\\\",\\\"sub\\\":\\\"x\\\"}\"))\n    match jwt.claims_unverified(\"aaa.\" + payload + \".bbb\"):\n        Err(e) -> print(console, e)\n        Ok(claims) -> print(console, json.get_string(claims, \"iss\").unwrap_or(\"?\"))\n";
        let expected = vec!["acme".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// The `tls:` scheme is split off an address before the allowlist match: the
    /// capability governs the bare `host:port`, the scheme is a connect-time choice.
    #[test]
    fn tls_scheme_is_stripped_for_the_allowlist() {
        assert_eq!(crate::net::parse_scheme("tls:github.com:443"), (true, "github.com:443"));
        assert_eq!(crate::net::parse_scheme("github.com:443"), (false, "github.com:443"));
    }

    /// Link + run a single-`main` source on the interpreter with a `Net` allowlist grant.
    fn link_run_net(src: &str, net_allow: &[&str]) -> Vec<String> {
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        interpreter::run_module(linked, ".", net_allow.iter().map(|s| s.to_string()).collect())
            .expect("run")
    }

    /// RFC-0011 carried-state: a SEALED record capability (`capability X:` with named
    /// fields) wraps a host capability AND carries policy data. It is footprint-
    /// transparent (audits as its cap fields), refines monotonically, and enforces its
    /// carried policy in the library's own operations — identically on both backends.
    #[test]
    fn carried_state_capability_runs_and_audits_through_record() {
        let src = "capability Postgres:\n    net: Net[Connect, Tcp]\n    table: String\npub fn connect(net: Net[Connect, Tcp]) -> Postgres:\n    Postgres(net, \"public\")\npub fn use_table(pg: Postgres, name: String) -> Postgres:\n    match pg:\n        Postgres(net, _) -> Postgres(net, name)\npub fn count_rows(pg: Postgres, requested: String) -> String:\n    match pg:\n        Postgres(_, table) ->\n            if requested == table:\n                \"ok: \" + requested\n            else:\n                \"denied: \" + requested\nfn main(console: Console, net: Net):\n    let users = use_table(connect(net), \"users\")\n    print(console, count_rows(users, \"users\"))\n    print(console, count_rows(users, \"secrets\"))\n";
        let want = vec!["ok: users".to_string(), "denied: secrets".to_string()];
        assert_eq!(link_run_net(src, &[]), want, "interpreter");
        assert_eq!(run_linked_on_wasm_net(&[("main", src)], "main", &[]), want, "compiled WASM must agree");

        // Footprint sees through the record: the sealed `Postgres` (a `Net` + a
        // `String`) audits as exactly `Net` — the carried `String` adds no authority.
        let module = parser::parse_module(src).expect("parse");
        let fp = crate::capabilities::analyze(&module);
        let connect_fn = fp.per_function.iter().find(|e| e.name == "connect").expect("connect entry");
        let keys: Vec<&str> = connect_fn.capabilities.keys().copied().collect();
        assert_eq!(keys, vec!["Net"], "carried String adds no authority — Postgres audits as Net only");
    }

    /// A sealed record capability is OPAQUE: its fields cannot be read with `.field`
    /// (only `match`, which the linker confines to the home module) and it cannot be
    /// `update`d — otherwise an alias would leak the underlying authority past the
    /// carried policy.
    #[test]
    fn sealed_capability_fields_are_opaque() {
        let leak = "capability Vault:\n    net: Net[Connect, Tcp]\n    label: String\npub fn open(net: Net[Connect, Tcp]) -> Vault:\n    Vault(net, \"x\")\nfn main(console: Console, net: Net):\n    let v = open(net)\n    let raw = v.net\n    print(console, \"leaked\")\n";
        let module = parser::parse_module(leak).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("`.field` on a sealed cap must be rejected");
        assert!(err.message.contains("sealed capability"), "got: {}", err.message);
    }

    /// TLS works end to end through the `tls:` address scheme (RFC-0009), HERMETICALLY:
    /// a local rustls server with a self-signed `localhost` cert (trusted via the
    /// `WITCHY_TLS_EXTRA_ROOTS` hook), and a witchy program that `connect`s to
    /// `tls:localhost:PORT`, sends a line, and reads the echo — identical on BOTH
    /// backends. Proves rustls+aws-lc terminates TLS host-side (the guest sees
    /// plaintext) with real certificate validation, no network access.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn tls_scheme_connects_through_a_local_server_backends_agree() {
        use std::io::{Read, Write};
        use std::sync::Arc;
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
        let cert_der = ck.cert.der().clone();
        let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());
        let server_config = Arc::new(
            rustls::ServerConfig::builder_with_provider(
                rustls::crypto::aws_lc_rs::default_provider().into(),
            )
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], rustls::pki_types::PrivateKeyDer::Pkcs8(key_der))
            .unwrap(),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cert_path = std::env::temp_dir().join(format!("witchy-tls-test-{port}.pem"));
        std::fs::write(&cert_path, ck.cert.pem()).unwrap();
        // SAFETY: nextest runs each test in its own process, so this env var is not
        // observed by another thread/test racing the set.
        unsafe { std::env::set_var("WITCHY_TLS_EXTRA_ROOTS", &cert_path) };

        // Echo server: two connections (one per backend run), each echoing one line.
        let sc = server_config.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (tcp, _) = listener.accept().unwrap();
                let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                let mut line = Vec::new();
                let mut b = [0u8; 1];
                while tls.read_exact(&mut b).is_ok() {
                    if b[0] == b'\n' {
                        break;
                    }
                    line.push(b[0]);
                }
                let _ = tls.write_all(&line).and_then(|_| tls.write_all(b"\n")).and_then(|_| tls.flush());
            }
        });

        let src = format!(
            "fn main(console: Console, net: Net):\n    match try_connect(net, \"tls:localhost:{port}\"):\n        None -> print(console, \"connect failed\")\n        Some(sock) ->\n            send_line(sock, \"ping\")\n            print(console, recv_line(sock))\n            close(sock)\n"
        );
        let allow = format!("localhost:{port}");
        assert_eq!(link_run_net(&src, &[allow.as_str()]), vec!["ping".to_string()], "interp TLS echo");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src.as_str())], "main", &[allow.as_str()]),
            vec!["ping".to_string()],
            "wasm TLS echo"
        );
        server.join().unwrap();
        let _ = std::fs::remove_file(&cert_path);
    }

    /// HTTPS end to end: `http.get_url("https://localhost:PORT/")` routes through the
    /// `tls:` scheme to a local rustls server speaking minimal HTTP/1.1, and parses the
    /// response — status and body identical on BOTH backends. Closes the loop from the
    /// TLS transport up to the `std/http` client (the shape an OAuth/OIDC call makes).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn https_get_url_through_a_local_server_backends_agree() {
        use std::io::{Read, Write};
        use std::sync::Arc;
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
        let cert_der = ck.cert.der().clone();
        let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());
        let server_config = Arc::new(
            rustls::ServerConfig::builder_with_provider(
                rustls::crypto::aws_lc_rs::default_provider().into(),
            )
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], rustls::pki_types::PrivateKeyDer::Pkcs8(key_der))
            .unwrap(),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cert_path = std::env::temp_dir().join(format!("witchy-https-test-{port}.pem"));
        std::fs::write(&cert_path, ck.cert.pem()).unwrap();
        // SAFETY: nextest runs each test in its own process — no other thread races this.
        unsafe { std::env::set_var("WITCHY_TLS_EXTRA_ROOTS", &cert_path) };

        let sc = server_config.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (tcp, _) = listener.accept().unwrap();
                let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                let mut req = Vec::new();
                let mut b = [0u8; 1];
                while tls.read_exact(&mut b).is_ok() {
                    req.push(b[0]);
                    if req.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
                let _ = tls.flush();
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        });

        let src = format!(
            "import http\nfn main(console: Console, net: Net):\n    match http.get_url(net, \"https://localhost:{port}/\"):\n        Ok(resp) -> print(console, \"${{http.status(resp)}} ${{http.body(resp)}}\")\n        Err(e) -> print(console, \"error: \" + e)\n"
        );
        let allow = format!("localhost:{port}");
        assert_eq!(link_run_net(&src, &[allow.as_str()]), vec!["200 hi".to_string()], "interp https");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src.as_str())], "main", &[allow.as_str()]),
            vec!["200 hi".to_string()],
            "wasm https"
        );
        server.join().unwrap();
        let _ = std::fs::remove_file(&cert_path);
    }

    /// `url.encode` percent-encodes query values (RFC 3986): the unreserved set passes,
    /// reserved/space bytes become `%XX`. Both backends agree.
    #[test]
    fn url_encode_percent_encodes_query_values() {
        let src = "import url\nfn main(console: Console):\n    print(console, url.encode(\"a b/c:?=&-_.~Z9\"))\n";
        let expected = vec!["a%20b%2Fc%3A%3F%3D%26-_.~Z9".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// `oauth.authorize_url` builds the OAuth2 authorization-code redirect, percent-
    /// encoding each parameter. Both backends agree.
    #[test]
    fn oauth_authorize_url_builds_the_redirect() {
        let src = "import oauth\nfn main(console: Console):\n    print(console, oauth.authorize_url(\"https://idp/auth\", \"cid\", \"http://app/cb\", \"openid email\", \"st8\"))\n";
        let expected = vec!["https://idp/auth?response_type=code&client_id=cid&redirect_uri=http%3A%2F%2Fapp%2Fcb&scope=openid%20email&state=st8".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// `oauth.exchange_code` POSTs to a token endpoint over HTTPS and reads the
    /// `access_token` — exercised HERMETICALLY against a local rustls server that
    /// returns the GitHub/Google JSON token shape, identical on BOTH backends. This is
    /// the network step of "Log in with GitHub" (code → access token).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn oauth_exchange_code_against_a_local_token_server_backends_agree() {
        use std::io::{Read, Write};
        use std::sync::Arc;
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
        let cert_der = ck.cert.der().clone();
        let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());
        let server_config = Arc::new(
            rustls::ServerConfig::builder_with_provider(
                rustls::crypto::aws_lc_rs::default_provider().into(),
            )
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], rustls::pki_types::PrivateKeyDer::Pkcs8(key_der))
            .unwrap(),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cert_path = std::env::temp_dir().join(format!("witchy-oauth-test-{port}.pem"));
        std::fs::write(&cert_path, ck.cert.pem()).unwrap();
        // SAFETY: nextest runs each test in its own process — no other thread races this.
        unsafe { std::env::set_var("WITCHY_TLS_EXTRA_ROOTS", &cert_path) };

        let sc = server_config.clone();
        let server = std::thread::spawn(move || {
            let body = b"{\"access_token\":\"gho_test_token\",\"token_type\":\"bearer\"}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            );
            for _ in 0..2 {
                let (tcp, _) = listener.accept().unwrap();
                let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                let mut req = Vec::new();
                let mut b = [0u8; 1];
                while tls.read_exact(&mut b).is_ok() {
                    req.push(b[0]);
                    if req.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = tls.write_all(response.as_bytes());
                let _ = tls.flush();
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        });

        let src = format!(
            "import oauth\nfn main(console: Console, net: Net):\n    match oauth.exchange_code(net, \"https://localhost:{port}/token\", \"cid\", \"sekret\", \"thecode\", \"http://app/cb\"):\n        Ok(tok) -> print(console, tok)\n        Err(e) -> print(console, \"error: \" + e)\n"
        );
        let allow = format!("localhost:{port}");
        assert_eq!(link_run_net(&src, &[allow.as_str()]), vec!["gho_test_token".to_string()], "interp exchange");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src.as_str())], "main", &[allow.as_str()]),
            vec!["gho_test_token".to_string()],
            "wasm exchange"
        );
        server.join().unwrap();
        let _ = std::fs::remove_file(&cert_path);
    }

    /// `oauth.bearer_get_json` GETs an API with a `Bearer` token and parses the JSON —
    /// the "fetch the signed-in user" step. HERMETIC: a local rustls server checks the
    /// `Authorization` header and returns a GitHub-`/user`-shaped body; the witchy
    /// program reads `login`. Identical on BOTH backends.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn oauth_bearer_get_json_against_a_local_api_backends_agree() {
        use std::io::{Read, Write};
        use std::sync::Arc;
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
        let cert_der = ck.cert.der().clone();
        let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());
        let server_config = Arc::new(
            rustls::ServerConfig::builder_with_provider(
                rustls::crypto::aws_lc_rs::default_provider().into(),
            )
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], rustls::pki_types::PrivateKeyDer::Pkcs8(key_der))
            .unwrap(),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cert_path = std::env::temp_dir().join(format!("witchy-bearer-test-{port}.pem"));
        std::fs::write(&cert_path, ck.cert.pem()).unwrap();
        // SAFETY: nextest runs each test in its own process — no other thread races this.
        unsafe { std::env::set_var("WITCHY_TLS_EXTRA_ROOTS", &cert_path) };

        let sc = server_config.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (tcp, _) = listener.accept().unwrap();
                let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                let mut req = Vec::new();
                let mut b = [0u8; 1];
                while tls.read_exact(&mut b).is_ok() {
                    req.push(b[0]);
                    if req.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                // Honour the bearer token: 401 without it, the user JSON with it.
                let authed = String::from_utf8_lossy(&req).to_lowercase().contains("authorization: bearer gho_test_token");
                let body: &[u8] = if authed {
                    b"{\"login\":\"octocat\",\"id\":583231}"
                } else {
                    b"{\"message\":\"Requires authentication\"}"
                };
                let code = if authed { "200 OK" } else { "401 Unauthorized" };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    code,
                    body.len(),
                    std::str::from_utf8(body).unwrap()
                );
                let _ = tls.write_all(response.as_bytes());
                let _ = tls.flush();
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        });

        let src = format!(
            "import oauth\nimport json\nfn main(console: Console, net: Net):\n    match oauth.bearer_get_json(net, \"https://localhost:{port}/user\", \"gho_test_token\"):\n        Ok(doc) -> print(console, json.get_string(doc, \"login\").unwrap_or(\"?\"))\n        Err(e) -> print(console, \"error: \" + e)\n"
        );
        let allow = format!("localhost:{port}");
        assert_eq!(link_run_net(&src, &[allow.as_str()]), vec!["octocat".to_string()], "interp bearer get");
        assert_eq!(
            run_linked_on_wasm_net(&[("main", src.as_str())], "main", &[allow.as_str()]),
            vec!["octocat".to_string()],
            "wasm bearer get"
        );
        server.join().unwrap();
        let _ = std::fs::remove_file(&cert_path);
    }

    /// base64url decode (URL-safe `-`/`_`, no padding) — the JWT/OIDC segment codec.
    /// `base64url_to_hex` round-trips the bytes of `base64url_of_hex`, and
    /// `base64url_decode` yields the text; identical on both backends.
    #[test]
    fn base64url_decode_backends_agree() {
        let src = "import encoding\nfn main(console: Console):\n    let e = encoding.base64url_of_hex(\"7b2274223a317d\")\n    print(console, encoding.base64url_to_hex(e))\n    print(console, encoding.base64url_decode(e))\n";
        let expected = vec!["7b2274223a317d".to_string(), "{\"t\":1}".to_string()];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
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
            .expect("compile")
            .expect("the binary path lowers this program");
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
    fn std_list_compiles_and_runs_on_wasm() {
        // The whole bundled `list` library links + compiles to WASM (every
        // function in it must compile), and map/filter/fold driven by closures
        // run end-to-end: doubled = [2,4,6,8,10] (sum 30); evens = [2,4] (len 2).
        let client = r#"
import list

fn main() -> Int:
    let xs = [1, 2, 3, 4, 5]
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    let evens = list.filter(xs, fn(n: Int): ((n % 2) == 0))
    let sum = list.fold(doubled, 0, fn(acc: Int, n: Int): (acc + n))
    (sum + list.length(evens))
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
                "main",
            ),
            vec!["32"]
        );
    }

    #[test]
    fn std_list_sort_by_runs_on_wasm() {
        // A comparator closure threaded through `sort_by` into its `insert_sorted`
        // helper (which calls it via call_indirect) compiles and sorts ascending:
        // [1,1,2,3,4,5,6,9]; first*100 + last = 109.
        let client = r#"
import list

fn main() -> Int:
    let xs = [3, 1, 4, 1, 5, 9, 2, 6]
    let sorted = list.sort_by(xs, fn(a: Int, b: Int): (a < b))
    ((list.at(sorted, 0) * 100) + list.at(sorted, 7))
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
                "main",
            ),
            vec!["109"]
        );
    }

    #[test]
    fn list_pipeline_example_runs_on_wasm() {
        // The example program (import list; map/filter/fold/sort_by + a capturing
        // closure) compiles to WASM and prints identically to the interpreter.
        assert_eq!(
            run_linked_on_wasm(
                &[
                    ("list", crate::bundled_module("list").unwrap()),
                    ("main", include_str!("../examples/list_pipeline/src/list_pipeline.witchy")),
                ],
                "main",
            ),
            vec!["233", "2 8", "735"]
        );
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
    print(console, __render(math.factorial(5)))
    print(console, __render(math.factorial(0)))
    print(console, __render(math.factorial(1)))
    print(console, __render(math.is_prime(7)))
    print(console, __render(math.is_prime(12)))
    print(console, __render(math.is_prime(1)))
    print(console, __render(math.is_prime(2)))
    print(console, __render(math.is_prime(97)))
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
    print(console, __render(math.lcm(4, 6)))
    print(console, __render(math.lcm(21, 6)))
    print(console, __render(math.lcm(0, 5)))
    print(console, __render(math.lcm((0 - 4), 6)))
    print(console, __render(math.is_even(10)))
    print(console, __render(math.is_odd(7)))
    print(console, __render(math.is_odd((0 - 3))))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "math lcm/parity diverged");
        assert_eq!(compiled, vec!["12", "42", "0", "12", "true", "true", "true"]);
    }

    #[test]
    fn std_result_compiles_and_runs_on_wasm() {
        // The Result type + combinators compile; map_ok runs a closure over Ok.
        let client = r#"
import result

fn main() -> Int:
    let r = result.map_ok(Ok(20), fn(n: Int): (n + 1))
    result.unwrap_or(r, 0)
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("result", crate::bundled_module("result").unwrap()), ("main", client)],
                "main",
            ),
            vec!["21"]
        );
    }

    #[test]
    fn std_option_compiles_and_runs_on_wasm() {
        // The Option type + combinators compile; map runs a closure over Some.
        let client = r#"
import option

fn main() -> Int:
    let o = option.map(Some(20), fn(n: Int): (n * 2))
    option.unwrap_or(o, 0)
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("option", crate::bundled_module("option").unwrap()), ("main", client)],
                "main",
            ),
            vec!["40"]
        );
    }

    #[test]
    fn split_runs_on_wasm() {
        // `split` compiled to WASM, matching Rust's str::split: pieces between
        // separators, empty pieces kept, multi-char separators, and an empty
        // separator yielding the whole string.
        let src = r#"
fn main(console: Console):
    let p = string.split("a,bb,ccc", ",")
    print(console, __render(list.length(p)))
    print(console, list.at(p, 0))
    print(console, list.at(p, 2))
    print(console, __render(list.length(string.split("a,,b", ","))))
    print(console, list.at(string.split("a,,b", ","), 1))
    print(console, __render(list.length(string.split("", ","))))
    print(console, __render(list.length(string.split("abc", ""))))
    print(console, list.at(string.split("xXXyXXz", "XX"), 2))
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
    print(console, __render(42))
    print(console, __render((0 - 5)))
    print(console, __render(true))
    print(console, __render((3 > 7)))
    print(console, __render("hi"))
    print(console, __render(classify(9)))
    let flag = (2 == 2)
    print(console, __render(flag))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["42", "-5", "true", "false", "hi", "true", "true"]
        );
    }

    #[test]
    fn to_string_respects_lambda_param_shadowing_on_wasm() {
        // The outer `x` is an Int; the lambda's `x` is a String param. `to_string`
        // inside the lambda must pass the String through, not run int_to_string on
        // the pointer — i.e. value-type tracking is scoped per lambda.
        let src = r#"
fn apply(f: fn(String) -> String, s: String) -> String:
    f(s)

fn main(console: Console):
    let x = 5
    print(console, __render(x))
    print(console, apply(fn(x: String): __render(x), "hey"))
"#;
        assert_eq!(run_on_wasm(src), vec!["5", "hey"]);
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
    print(console, __render([1, 2, 3]))
    print(console, "${[[1, 2], [3]]}")
    print(console, "${(1, "two", true)}")
    print(console, "${[Circle(2), Dot]}")
    let d = dict.insert(dict.insert(dict.new(), "a", 1), "b", 2)
    print(console, "${d}")
    let tc = ([1, 2], (3, 4))          // a let-bound tuple whose slots are compound
    print(console, "${tc}")
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
        let src = "fn same(a: (List(Int), List(Int)), b: (List(Int), List(Int))) -> Bool:\n    a == b\nfn main(console: Console):\n    let v = ([1, 2], (3, 4))\n    let w = ([1, 2], (3, 4))\n    print(console, __render(v == w))\n    print(console, __render(same(([1], [2]), ([1], [2]))))\n    print(console, __render(same(([1], [2]), ([1], [9]))))\n";
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
    __render(t)

fn main(console: Console):
    print(console, render((1, 2)))
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
    print(console, __render((0 - 1)))
    print(console, __render((0 - 128)))
    print(console, __render(255))
    print(console, __render(0))
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
    print(console, string.replace("a,b,c", ",", ";"))
    print(console, string.replace("aXXbXXc", "XX", "-"))
    print(console, string.replace("aaa", "aa", "x"))
    print(console, string.replace("a,b,c", ",", ""))
    print(console, string.replace("abc", "b", "XYZ"))
    print(console, string.replace("abc", "z", "Q"))
    print(console, string.replace("ab", "", "-"))
    print(console, string.replace("café", "é", "e"))
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
    print(console, __render(if string.contains("hello world", "world"): 1 else: 0))
    print(console, __render(if string.contains("abc", "xyz"): 1 else: 0))
    print(console, __render(if string.contains("abc", ""): 1 else: 0))
    print(console, __render(string.index_of("hello", "l")))
    print(console, __render(string.index_of("hello", "z")))
    print(console, string.substring("hello", 1, 4))
    print(console, string.substring("hi", 0, 100))
    print(console, string.substring("hi", 5, 10))
    print(console, __render(string.index_of("café!", "!")))
    print(console, string.substring("café!", 3, 5))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["1", "0", "1", "2", "-1", "ell", "hi", "", "4", "é!"]
        );
    }

    #[test]
    fn parse_kv_example_runs_on_wasm() {
        // The `key=value` parser example now compiles end-to-end: index_of +
        // substring + string_length + ends_with + __render(Bool), matching the
        // interpreter.
        assert_eq!(
            run_on_wasm(include_str!("../examples/parse_kv/src/parse_kv.witchy")),
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
    d = dict.insert(d, "a", 1)
    d = dict.insert(d, "b", 2)
    d = dict.insert(d, "a", 10)
    print(console, __render(dict.get_or(d, "a", 0)))
    print(console, __render(dict.get_or(d, "b", 0)))
    print(console, __render(dict.get_or(d, "z", (0 - 1))))
    print(console, __render(dict.length(d)))
    print(console, __render(if dict.contains_key(d, "b"): 1 else: 0))
    print(console, __render(if dict.contains_key(d, "q"): 1 else: 0))
"#;
        assert_eq!(run_on_wasm(src), vec!["10", "2", "-1", "2", "1", "0"]);
    }

    #[test]
    fn dict_int_keys_on_wasm() {
        // Int-keyed Dict: keys compared with i32 equality (mode 0).
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    d = dict.insert(d, 1, 100)
    d = dict.insert(d, 2, 200)
    print(console, __render(dict.get_or(d, 1, 0)))
    print(console, __render(dict.get_or(d, 2, 0)))
    print(console, __render(dict.get_or(d, 3, (0 - 1))))
"#;
        assert_eq!(run_on_wasm(src), vec!["100", "200", "-1"]);
    }

    #[test]
    fn wordcount_example_runs_on_wasm() {
        // The word-frequency example compiles to WASM: a String-keyed Dict built
        // in a `for w in string.split(...)` loop (so `w`'s type resolves to String).
        // the=3, cat=1, missing=0, size=4.
        assert_eq!(
            run_on_wasm(include_str!("../examples/wordcount/src/wordcount.witchy")),
            vec!["3", "1", "0", "4"]
        );
    }

    #[test]
    fn dict_undetermined_key_is_rejected() {
        // A key whose type codegen can't pin down (here a list) errors clearly
        // rather than picking a wrong comparison.
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    d = dict.insert(d, [1, 2], 5)
    print(console, __render(dict.length(d)))
"#;
        let module = parser::parse_module(src).expect("parse");
        let err = codegen::compile_module_binary(&module)
            .expect_err("should reject");
        assert!(
            err.to_string().contains("could not determine the Dict key type"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn direct_field_access_on_expressions_backends_agree() {
        // Field access directly on a record-producing expression (no `let`): a
        // constructor literal, a record-returning call, and an `at` result.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn lookup(b: Bool) -> Item:
    if b:
        Item(3, 10)
    else:
        Item(5, 2)

fn main(console: Console):
    print(console, __render((Item(7, 6)).price))
    print(console, __render((lookup(true)).qty))
    let items = [Item(1, 2), Item(3, 4)]
    print(console, __render((list.at(items, 1)).qty))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["7", "10", "4"]);
    }

    #[test]
    fn conditional_record_field_access_backends_agree() {
        // `let x = if c { A } else { B }; x.field` (and a match-bound record):
        // the binding's record type is recovered from the branch/arm.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn pick(b: Bool) -> Int:
    let x = if b: Item(3, 10) else: Item(5, 2)
    ((x).price * (x).qty)

fn from_tag(t: Int) -> Int:
    let y = match t:
        0 -> Item(1, 1)
        _ -> Item(2, 3)
    ((y).price + (y).qty)

fn main(console: Console):
    print(console, __render(pick(true)))
    print(console, __render(pick(false)))
    print(console, __render(from_tag(0)))
    print(console, __render(from_tag(9)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "10", "2", "5"]);
    }

    #[test]
    fn or_unwraps_option_backends_agree() {
        // RFC-0021: `Option(T) || T` unwraps to `T` (None -> the default, evaluated
        // lazily; Some(x) -> x, present even when empty). Every other `||` is the
        // unchanged same-type truthy fallback.
        let src = r#"
import option

fn pick(b: Bool) -> Option(Int):
    if b: Some(36) else: None

fn main(console: Console):
    print(console, "${pick(true) || 0}")
    print(console, "${pick(false) || 0}")
    print(console, "" || "default")
    print(console, "${pick(true) || pick(false)}")
    print(console, "${Some("") || "x"}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["36", "0", "default", "Some(36)", ""]
        );
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
    print(console, "${xs}")
    var d = dict.new()
    d["a"] = 1
    d["b"] = 2
    print(console, "${dict.get_or(d, "a", 0)} ${dict.get_or(d, "b", 0)}")
    var p = P(1, 2)
    p.x = 10
    p.y += 5
    print(console, "${p.x} ${p.y}")
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
    fn list_of_records_index_access_backends_agree() {
        // `list.at(items, i).field` via a let, for both a List(Record) parameter and a
        // let-bound list literal of records; and a for-loop over the let-bound
        // list. Both backends agree.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn first_value(items: List(Item)) -> Int:
    let first = list.at(items, 0)
    ((first).price * (first).qty)

fn main(console: Console):
    print(console, __render(first_value([Item(3, 10), Item(5, 2)])))
    let items = [Item(2, 4), Item(7, 1)]
    let second = list.at(items, 1)
    print(console, __render(((second).price + (second).qty)))
    var total = 0
    for it in items:
        total = (total + (it).price)
    print(console, __render(total))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "8", "9"]);
    }

    #[test]
    fn dict_of_records_field_access_backends_agree() {
        // Looking a record up in a Dict and accessing its field: the result of
        // get_or carries the default's record type, so `it.price` resolves.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn main(console: Console):
    var d = dict.new()
    d = dict.insert(d, "apple", Item(3, 10))
    d = dict.insert(d, "bread", Item(2, 5))
    let it = dict.get_or(d, "apple", Item(0, 0))
    print(console, __render(((it).price * (it).qty)))
    let missing = dict.get_or(d, "milk", Item(0, 0))
    print(console, __render((missing).price))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "0"]);
    }

    #[test]
    fn dict_remove_backends_agree() {
        // `remove` (string and int keys) — present, absent, and the surviving
        // entries — agrees across the interpreter and compiled backends.
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    d = dict.insert(d, "a", 1)
    d = dict.insert(d, "b", 2)
    d = dict.insert(d, "c", 3)
    let d2 = dict.remove(d, "b")
    print(console, __render(dict.length(d2)))
    print(console, __render(if dict.contains_key(d2, "b"): 1 else: 0))
    print(console, __render(dict.get_or(d2, "a", 0)))
    print(console, __render(dict.get_or(d2, "c", 0)))
    let d3 = dict.remove(d, "missing")
    print(console, __render(dict.length(d3)))
    print(console, __render(dict.length(d)))
    var nums = dict.new()
    nums = dict.insert(nums, 10, 100)
    nums = dict.insert(nums, 20, 200)
    let nums2 = dict.remove(nums, 10)
    print(console, __render(dict.length(nums2)))
    print(console, __render(dict.get_or(nums2, 20, 0)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2", "0", "1", "3", "3", "3", "1", "200"]);
    }

    /// Regression for a compiled-dict parity violation (FIXED in `dict_remove`):
    /// removing a key then re-inserting it, followed by any `dict.keys`/`values`/
    /// `pairs` iteration, used to corrupt the re-inserted entry on the COMPILED
    /// backend so `get_or` returned the default (the interpreter oracle was
    /// always correct). Root cause: `dict_remove` allocated `count` entry slots
    /// but advanced `heap` only past the `n` surviving entries, leaving the
    /// `count-n` slack the own-ABI tracks as capacity UNRESERVED — so the next
    /// in-place insert appended into it and the following allocation stomped the
    /// entry. Fixed by reserving the full allocated capacity. Both backends now
    /// agree on "5","1","5".
    #[test]
    fn dict_remove_reinsert_then_iterate_keeps_entry() {
        let src = r#"
import dict
fn main(console: Console):
    var b = dict.new()
    b = dict.insert(b, "x", 1)
    b = dict.remove(b, "x")
    b = dict.insert(b, "x", 5)
    print(console, __render(dict.get_or(b, "x", -1)))
    print(console, __render(list.length(dict.keys(b))))
    print(console, __render(dict.get_or(b, "x", -1)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "compiled backend diverges from the interpreter oracle");
        assert_eq!(run_on_wasm(src), vec!["5", "1", "5"]);
    }

    #[test]
    fn dict_keys_values_pairs_on_wasm() {
        // keys/values/pairs compiled to WASM: keys -> list of keys, values ->
        // list of values, pairs -> list of (k, v) tuples destructured in a loop.
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    d = dict.insert(d, "a", 10)
    d = dict.insert(d, "b", 20)
    d = dict.insert(d, "c", 30)
    var ksum = 0
    for k in dict.keys(d):
        ksum = (ksum + string.length(k))
    print(console, __render(ksum))
    var vsum = 0
    for v in dict.values(d):
        vsum = (vsum + v)
    print(console, __render(vsum))
    var psum = 0
    for entry in dict.pairs(d):
        let (k, v) = entry
        psum = ((psum + string.length(k)) + v)
    print(console, __render(psum))
"#;
        // keys "a","b","c" (len 1 each) -> 3; values 10+20+30 -> 60; pairs
        // (1+10)+(1+20)+(1+30) -> 63.
        assert_eq!(run_on_wasm(src), vec!["3", "60", "63"]);
    }

    #[test]
    fn inventory_example_runs_on_wasm() {
        // Dict iteration example compiles: `values` summed in a loop, and `pairs`
        // destructured. total = 3+2+4 = 9; prices over 2 = {apple, milk} = 2.
        assert_eq!(
            run_on_wasm(include_str!("../examples/inventory/src/inventory.witchy")),
            vec!["total = 9", "over 2: 2"]
        );
    }

    #[test]
    fn std_string_compiles_and_runs_on_wasm() {
        // With `split` compiled, the whole `string` module compiles: `lines`
        // (split on "\n"), `join`, and `repeat`. lines -> ["a","bb","ccc"] (3);
        // join -> "a-bb-ccc" (8); repeat -> "zzzzz" (5): 3*100 + 8 + 5 = 313.
        let client = r#"
import string

fn main() -> Int:
    let parts = string.lines("a\nbb\nccc")
    let joined = list.join(parts, "-")
    let r = string.repeat("z", 5)
    (((list.length(parts) * 100) + string.length(joined)) + string.length(r))
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
import string

fn main(console: Console):
    print(console, string.pad_left("42", 5, "0"))
    print(console, string.pad_right("42", 5, "."))
    print(console, string.pad_left("hello", 3, "x"))
    print(console, string.pad_left("ab", 7, "-="))
    print(console, string.pad_left("café", 6, "*"))
    print(console, string.pad_right("café", 6, "*"))
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
    fn std_list_partition_unzip_backends_agree() {
        // partition splits by a predicate in one pass; unzip is the inverse of
        // zip. Both return tuples of lists, so this also exercises tuple-valued
        // returns from generic std functions across backends.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 4, 5, 6]
    let (evens, odds) = list.partition(xs, fn(n: Int): ((n % 2) == 0))
    print(console, __render(list.sum(evens)))
    print(console, __render(list.sum(odds)))
    let pairs = list.zip([10, 20, 30], [1, 2, 3])
    let (a, b) = list.unzip(pairs)
    print(console, __render(list.sum(a)))
    print(console, __render(list.sum(b)))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "partition/unzip diverged between backends");
        assert_eq!(compiled, vec!["12", "9", "60", "6"]);
    }

    #[test]
    fn std_string_strip_backends_agree() {
        // strip_prefix/strip_suffix remove an affix only when it matches,
        // leaving the string untouched otherwise; stripping the whole string
        // yields "". Complements starts_with/ends_with.
        let client = r#"
import string

fn main(console: Console):
    print(console, string.strip_prefix("witchy.lang", "witchy."))
    print(console, string.strip_prefix("witchy.lang", "scala."))
    print(console, string.strip_suffix("main.witchy", ".witchy"))
    print(console, string.strip_suffix("main.rs", ".witchy"))
    print(console, string.strip_prefix("abc", "abc"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "strip diverged between backends");
        assert_eq!(compiled, vec!["lang", "witchy.lang", "main", "main.rs", ""]);
    }

    #[test]
    fn large_list_allocation_grows_memory() {
        // Building a 200-element list via `push` allocates ~80KB total (each push
        // copies the whole list, and the bump allocator never frees) — past the
        // initial 64KB page, so the memory must grow. Summing 0..199 verifies no
        // element was corrupted by the growth.
        let src = r#"
fn main() -> Int:
    var out = []
    var i = 0
    while (i < 200):
        out = list.push(out, i)
        i = (i + 1)
    var total = 0
    for x in out:
        total = (total + x)
    total
"#;
        assert_eq!(run_on_wasm(src), vec!["19900"]); // 199*200/2
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
    string.length(s)
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
    fn list_patterns_on_wasm() {
        // Recursive head/tail list processing compiles: `[]` and `[h, ..t]`
        // (the tail is a freshly allocated sublist). sum([10,20,30,40]) = 100.
        let src = r#"
fn sum(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [h, ..t] -> (h + sum(t))

fn main() -> Int:
    sum([10, 20, 30, 40])
"#;
        assert_eq!(run_on_wasm(src), vec!["100"]);
    }

    #[test]
    fn list_push_and_concat_on_wasm() {
        // Build a list with `push` in a loop, then `concat` — both allocate new
        // lists at runtime. double_all([1,2,3]) = [2,4,6], ++ [100], summed = 112.
        let src = r#"
fn double_all(xs: List(Int)) -> List(Int):
    var out = []
    for x in xs:
        out = list.push(out, (x * 2))
    out

fn main() -> Int:
    let ys = list.concat(double_all([1, 2, 3]), [100])
    (((list.at(ys, 0) + list.at(ys, 1)) + list.at(ys, 2)) + list.at(ys, 3))
"#;
        assert_eq!(run_on_wasm(src), vec!["112"]);
    }

    #[test]
    fn for_in_over_list_on_wasm() {
        // `for x in list` compiles to a WASM loop; sum a list = 100.
        let src = r#"
fn total(xs: List(Int)) -> Int:
    var sum = 0
    for x in xs:
        sum = (sum + x)
    sum

fn main() -> Int:
    total([10, 20, 30, 40])
"#;
        assert_eq!(run_on_wasm(src), vec!["100"]);
    }

    #[test]
    fn tuple_match_patterns_on_wasm() {
        // Tuple patterns in `match` compile to WASM (no tag; element-wise).
        // classify((3,0))=3, classify((0,5))=5, classify((2,4))=6; sum = 14.
        let src = r#"
fn classify(p: (Int, Int)) -> Int:
    match p:
        (0, 0) -> 0
        (x, 0) -> x
        (0, y) -> y
        (x, y) -> (x + y)

fn main() -> Int:
    ((classify((3, 0)) + classify((0, 5))) + classify((2, 4)))
"#;
        assert_eq!(run_on_wasm(src), vec!["14"]);
    }

    #[test]
    fn tuple_construct_and_destructure_on_wasm() {
        // Multiple-return-value tuples compile to WASM: divmod(17,5) = (3,2),
        // then 3*100 + 2 = 302.
        let src = r#"
fn divmod(a: Int, b: Int) -> (Int, Int):
    ((a / b), (a % b))

fn main() -> Int:
    let (q, r) = divmod(17, 5)
    ((q * 100) + r)
"#;
        assert_eq!(run_on_wasm(src), vec!["302"]);
    }

    #[test]
    fn string_prefix_suffix_on_wasm() {
        // starts_with / ends_with compile to byte-loop helpers.
        // check("html")=2, check("http")=1, check("xml")=0 -> 210.
        let src = r#"
fn check(s: String) -> Int:
    if string.starts_with(s, "ht"):
        if string.ends_with(s, "ml"):
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
type Result:
    Ok(a)
    Err(e)

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

    #[test]
    fn nested_record_chained_field_access_on_wasm() {
        // `o.inner.v` — chained access through a nested record — compiles to WASM.
        let src = r#"
type Inner:
    v: Int

type Outer:
    inner: Inner
    tag: Int

fn deep(o: Outer) -> Int:
    (((o).inner).v + (o).tag)

fn main() -> Int:
    let o = Outer(Inner(42), 8)
    deep(o)
"#;
        assert_eq!(run_on_wasm(src), vec!["50"]);
    }

    #[test]
    fn record_call_and_update_results_field_access_on_wasm() {
        // Field access on a `let` bound to a record-returning call / update —
        // exercises return-record and update-result type tracking in codegen.
        let src = r#"
type Point:
    x: Int
    y: Int

fn make(a: Int, b: Int) -> Point:
    Point(a, b)

fn shift(p: Point, dx: Int) -> Point:
    Point(x: ((p).x + dx), ..p)

fn main() -> Int:
    let p = make(3, 4)
    let q = shift(p, 7)
    ((q).x + (q).y)
"#;
        assert_eq!(run_on_wasm(src), vec!["14"]);
    }

    /// Real examples (not toy snippets) compile and run on the WASM backend,
    /// matching the interpreter — a concrete check of codegen breadth.
    #[test]
    fn eval_example_runs_on_wasm() {
        assert_eq!(run_on_wasm(include_str!("../examples/eval/src/eval.witchy")), vec!["20"]);
    }

    #[test]
    fn records_example_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../examples/records/src/records.witchy")),
            vec!["origin.x = 2", "moved = (12, 3)", "manhattan(moved) = 15"]
        );
    }

    #[test]
    fn bank_example_runs_on_wasm() {
        // Records + lists + for-in + Result + `?` together, compiled to WASM.
        assert_eq!(
            run_on_wasm(include_str!("../examples/bank/src/bank.witchy")),
            vec!["total = 150", "remaining: 90", "error: insufficient funds for bob"]
        );
    }

    #[test]
    fn closures_example_runs_on_wasm() {
        // Higher-order functions + closures, compiled to WASM: apply(square, 9) =
        // 81; twice(+3, 10) = ((10+3)+3) = 16; apply(adder(100), 5) = 105 (the
        // returned closure captures `by = 100`).
        assert_eq!(
            run_on_wasm(include_str!("../examples/closures/src/closures.witchy")),
            vec!["81", "16", "105"]
        );
    }

    #[test]
    fn record_typed_list_iteration_on_wasm() {
        // `for it in items` where items: List(Record) — the loop var's fields
        // resolve. total([Item(3,2), Item(5,1)]) = 3*2 + 5*1 = 11.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn total(items: List(Item)) -> Int:
    var sum = 0
    for it in items:
        sum = (sum + ((it).price * (it).qty))
    sum

fn main() -> Int:
    total([Item(3, 2), Item(5, 1)])
"#;
        assert_eq!(run_on_wasm(src), vec!["11"]);
    }

    #[test]
    fn record_field_access_and_update_run_on_wasm() {
        // Records — field access *and* update — compile and run on the WASM
        // runtime. shift_x(Point(3,4), 1) = Point(4,4); 4*4 + 4*4 = 32.
        assert_eq!(
            run_on_wasm(include_str!("../examples/record_compiled/src/record_compiled.witchy")),
            vec!["32"]
        );
    }

    /// The capability thesis at the WASM boundary: without the `print_int` host
    /// function granted, the compiled module imports something that isn't there
    /// and cannot even instantiate.
    #[test]
    fn compiled_program_without_capability_cannot_instantiate() {
        use crate::runtime::{Capabilities, Runtime};
        let module = parser::parse_module(include_str!("../examples/compute/src/compute.witchy")).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let result = rt.spawn(&bytes, Capabilities::none(), 4);
        assert!(result.is_err(), "ungranted module must fail to instantiate");
    }

    // A Net capability is an allow-list, and attenuation only ever narrows it.
    // These rejections fire on the allow-list check, before any socket is
    // opened, so the test needs no network. (`run_with` grants the root Net.)
    #[test]
    fn net_capability_cannot_escalate() {
        // connect outside the granted allow-list is denied.
        let connect_denied = r#"
fn main(console: Console, net: Net):
    send_line(connect(net, "evil.test:80"), "x")
"#;
        let e = interpreter::run_with(connect_denied, ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not permitted"), "expected a connect denial, got: {e}");

        // narrowing to an address not already held is denied (can't widen).
        let restrict_denied = r#"
import confine
fn main(console: Console, net: Net):
    send_line(connect(net.only(confine.tcp("evil.test", 80)), "evil.test:80"), "x")
"#;
        // `resolve_std_src` links `confine`; `run_module` grants the Net allow-list.
        let e = interpreter::run_module(resolve_std_src(restrict_denied), ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not in this Net"), "expected a restrict denial, got: {e}");

        // Attenuation is real: after narrowing to one address, a sibling that
        // was in the original grant is no longer reachable.
        let attenuated = r#"
import confine
fn main(console: Console, net: Net):
    let narrow = net.only(confine.tcp("a.test", 80))
    send_line(connect(narrow, "b.test:80"), "x")
"#;
        let e = interpreter::run_module(resolve_std_src(attenuated), ".", vec!["a.test:80".into(), "b.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not permitted"), "expected the sibling to be unreachable, got: {e}");
    }

    /// A library imported into a program brings its functions into scope but no
    /// authority: `lib` has no capability parameters, so it can only compute.
    #[test]
    fn imported_library_is_pure_and_confined() {
        let lib = r#"
fn label(n: Int) -> String:
    if (n < 0):
        "neg"
    else:
        "nonneg"
"#;
        let main = r#"
import lib

fn main(console: Console):
    print(console, lib.label((-2)))
    print(console, lib.label(7))
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
        for path in files {
            let p = path.to_str().unwrap();
            let result = crate::execute_file(p, Vec::new());
            assert!(result.is_ok(), "example `{p}` failed: {result:?}");
        }
    }

    /// EVERY example — including the server demos that run forever (and so are
    /// excluded from the run-to-completion test above) — must parse, link, and
    /// type-check. Catches type errors the run test can't reach.
    #[test]
    fn all_examples_type_check() {
        let mut any = false;
        for path in example_entries() {
            any = true;
            let p = path.to_str().unwrap();
            assert!(
                crate::check_file(p).is_ok(),
                "type-check failed for `{p}`: {:?}",
                crate::check_file(p)
            );
        }
        assert!(any, "no examples found");
    }

    /// Every example rune's in-language tests (`src/*_test.witchy`) pass. This
    /// keeps the per-example `witchy test` suites green in CI — so an example
    /// whose behavior drifts from its documented tests fails the build, not just
    /// a manual run. (Multi-rune `projects/` are skipped here: their cross-rune
    /// path dependencies are exercised by the package-manager tests instead.)
    #[test]
    fn all_example_rune_tests_pass() {
        let mut ran = 0usize;
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
            let mut test_files: Vec<std::path::PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with("_test.witchy"))
                })
                .collect();
            test_files.sort();
            for tf in test_files {
                let p = tf.to_str().unwrap();
                let (passed, failed) =
                    crate::run_tests_in_file(p).unwrap_or_else(|e| panic!("{p}: {e}"));
                assert!(failed.is_empty(), "{p}: test failures: {failed:?}");
                assert!(!passed.is_empty(), "{p}: a `*_test.witchy` with no `test_*` functions");
                ran += passed.len();
            }
        }
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

    #[test]
    fn files_example_reads_sandboxed_file() {
        assert_eq!(
            crate::execute_file("examples/files/src/files.witchy", Vec::new()).unwrap(),
            vec!["hello from a sandboxed Dir capability"]
        );
    }

    /// The capability-rights showcase: it runs (exercising implicit + explicit
    /// `as` narrowing of a `Dir` to `Dir[Read]`) and its footprint is
    /// verb/transport-precise — the end-to-end demonstration of the feature.
    #[test]
    fn capability_rights_example_runs_and_audits() {
        assert_eq!(
            crate::execute_file("examples/capability_rights/src/capability_rights.witchy", Vec::new()).unwrap(),
            vec![
                "implicit: hello from a sandboxed Dir capability",
                "explicit: hello from a sandboxed Dir capability",
            ]
        );
        let src = std::fs::read_to_string("examples/capability_rights/src/capability_rights.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        let shown = |name: &str| {
            let e = fp.entries.iter().find(|e| e.name == name).expect("entry");
            crate::capabilities::show_caps(&e.capabilities)
        };
        assert_eq!(shown("load"), "Dir[Read]");
        assert_eq!(shown("fetch"), "Net[Connect, Tcp]");
        assert_eq!(shown("serve"), "Net[Listen]");
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

    /// The `std/iter` adapters `enumerate`/`zip`/`chain`/`flat_map`/`for_each`
    /// (plus `func.first`/`second` for the pairs they produce) must agree on both
    /// backends — they compose lazily over finite and infinite iterators.
    #[test]
    fn std_iter_more_adapters_backends_agree() {
        let client = r#"
import iter
import func
import string
fn main(console: Console):
    var es = []
    let ps: List((Int, String)) = iter.collect(iter.enumerate(iter.from_list(["a", "b", "c"])))
    for p in ps:
        es = list.push(es, __render(func.first(p)) + func.second(p))
    print(console, list.join(es, " "))
    print(console, __render(iter.count(iter.zip(iter.count_from(1), iter.from_list([0, 0, 0])))))
    print(console, __render(iter.sum(iter.chain(iter.range(0, 4), iter.range(10, 13)))))
    print(console, __render(iter.sum(iter.flat_map(iter.range(1, 4), fn(n: Int): iter.from_list([n, n])))))
    iter.for_each(iter.take(iter.count_from(100), 3), fn(n: Int): print(console, __render(n)))
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

    /// A `gen fn` lowers to a `__gen_*` helper (yield -> counter + early return)
    /// plus a wrapper calling `iter.from_gen`, and `import iter` is injected.
    #[test]
    fn gen_fn_lowers_to_helper_and_wrapper() {
        let m = parser::parse_module("gen fn nums() -> Iter(Int):\n    yield 1\n    yield 2\n")
            .expect("parse");
        let lowered = crate::generators::lower(m);
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
    let sq = iter.map(iter.count_from(1), fn(n: Int): n * n)
    let small = iter.take_while(sq, fn(s: Int): s < 100)
    print(console, __render(iter.sum(iter.filter(small, fn(s: Int): s % 2 == 1))))
    // first multiple of 7 above 50, from an infinite iterator
    match iter.find(iter.count_from(51), fn(n: Int): n % 7 == 0):
        Some(n) -> print(console, __render(n))
        None -> print(console, "none")
    // a finite range, doubled and collected
    print(console, __render(iter.count(iter.range(0, 5))))
    let vs: List(Int) = iter.collect(iter.map(iter.range(0, 3), fn(n: Int): n * 10))
    for v in vs:
        print(console, __render(v))
"#;
        let sources = [("iter", crate::bundled_module("iter").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std/iter diverged");
        assert_eq!(compiled, vec!["165", "56", "5", "0", "10", "20"]);
    }

    /// `lazy_fib` builds an *infinite* Fibonacci iterator with `iter.unfold` and
    /// bounds it with take / take_while / find — the canonical lazy-generator
    /// demo, agreeing on both backends.
    #[test]
    fn lazy_fib_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/lazy_fib/src/lazy_fib.witchy").unwrap();
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client.as_str()),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "lazy_fib diverged");
        assert_eq!(
            compiled,
            vec![
                "first 10: 0, 1, 1, 2, 3, 5, 8, 13, 21, 34".to_string(),
                "even fib sum < 1000: 798".to_string(),
                "first fib > 1000: 1597".to_string(),
            ]
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

    /// `higher_order_sum` reproduces Rust by Example's "sum of squared odd numbers
    /// under 1000" — an imperative range loop and a functional `std/list` pipeline
    /// (map / take_while / filter / sum) that must agree, on both backends.
    #[test]
    fn higher_order_sum_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/higher_order_sum/src/higher_order_sum.witchy").unwrap();
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client.as_str())];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "higher_order_sum diverged");
        assert_eq!(compiled, vec!["imperative: 5456".to_string(), "functional: 5456".to_string()]);
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

    /// `caps_audit` is a capability auditor written *in witchy*: it reads a source
    /// file (`Dir[Read]`), computes its footprint via `compiler.footprint`, parses
    /// the JSON with `std/json`, and prints the total — a self-hosted slice of
    /// `witchy caps`, proving the toolchain is usable from within the language.
    #[test]
    fn caps_audit_example_audits_a_rune_in_witchy() {
        assert_eq!(
            crate::execute_file("examples/caps_audit/src/caps_audit.witchy", Vec::new()).unwrap(),
            vec!["examples/data/sample_rune.witchy demands: Dir[Read], Net[Connect]"]
        );
        // The auditor itself only reads files and prints — provably no writes/net.
        let src = std::fs::read_to_string("examples/caps_audit/src/caps_audit.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir[Read]");
    }

    /// `caps_guard` is the supply-chain gate written *in witchy*: it reads two
    /// versions of a rune, asks `compiler.diff` whether the new one widens the
    /// footprint, prints a BLOCK/OK verdict, AND exits non-zero on a widening
    /// (the sample upgrade adds `Listen`, so it BLOCKs and exits 2 — wireable into
    /// CI). The whole gate is self-hosted.
    #[test]
    fn caps_guard_example_blocks_a_widening_in_witchy() {
        let (output, code) =
            crate::execute_file_exit("examples/caps_guard/src/caps_guard.witchy", Vec::new(), Vec::new(), None, Vec::new())
                .unwrap();
        assert_eq!(output, vec!["BLOCK: upgrade widens authority by Net[Listen]"]);
        assert_eq!(code, 2, "a widening must exit 2");
        let src = std::fs::read_to_string("examples/caps_guard/src/caps_guard.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir[Read]");
    }

    /// `coven_check` is the package manager's `check_declared`, self-hosted: it
    /// reads a rune's `witchy.toml` (`std/toml`) and its source, asks the compiler
    /// what the code demands (`compiler.footprint`), and verifies the manifest's
    /// `[capabilities]` admits every demanded cap *rights-precisely*. The sample
    /// manifest admits `Net[Connect]`, but the code demands full `Net` (it also
    /// listens), so it flags the under-declaration and exits 1 even though the
    /// `Net` *kind* is declared — the case a kind-level check would miss.
    #[test]
    fn coven_check_example_flags_under_declared_manifest_in_witchy() {
        let (output, code) =
            crate::execute_file_exit("examples/coven_check/src/coven_check.witchy", Vec::new(), Vec::new(), None, Vec::new())
                .unwrap();
        assert_eq!(
            output,
            vec!["UNDER-DECLARED: code demands Net not admitted by [capabilities]"]
        );
        assert_eq!(code, 1, "an under-declared manifest must exit 1");
        // The checker itself only reads files and prints — provably no writes/net.
        let src = std::fs::read_to_string("examples/coven_check/src/coven_check.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir[Read]");
    }

    /// `projects/pm` is the package manager itself, written in witchy. `pm audit`
    /// prints the capability footprint a source file demands — the self-hosted
    /// `witchy caps`, dispatched from a real CLI (`args: List(String)`).
    #[test]
    fn pm_audits_a_files_footprint() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["audit".into(), "examples/data/sample_rune.witchy".into()],
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            out,
            vec!["examples/data/sample_rune.witchy demands: Dir[Read], Net[Connect]"]
        );
        assert_eq!(code, 0);
        // pm reads/writes project files, prints, `add` fetches over the network,
        // `run` drives the compiler via Exec, `publish` reads COVEN_ID_TOKEN via
        // Env, and `add`'s staging-cooldown gate reads the wall clock (Clock) —
        // Clock, Console, Dir, Env, Exec, Net. `compiler.*` is a host introspection
        // intrinsic, not a runtime capability.
        let src = std::fs::read_to_string("projects/pm/src/pm.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Clock, Console, Dir, Env, Exec, Net");
    }

    /// `pm guard <old> <new>` is the supply-chain gate: it asks `compiler.diff`
    /// whether the upgrade widens authority and exits 2 on a widening (wireable
    /// into CI). The sample upgrade adds `Listen`, so it BLOCKs.
    #[test]
    fn pm_guard_blocks_a_widening() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec![
                "guard".into(),
                "examples/data/sample_rune.witchy".into(),
                "examples/data/sample_rune_v2.witchy".into(),
            ],
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(out, vec!["BLOCK: upgrade widens authority by Net[Listen]"]);
        assert_eq!(code, 2, "a widening must exit 2");
    }

    /// `pm check <dir>` recomputes a rune's footprint from source and fails if the
    /// manifest's `[capabilities]` does not admit it — rights-precisely. The
    /// `leaky` fixture declares only `Console` but its code reads files, so the
    /// undeclared `Dir[Read]` is caught and the gate exits 2.
    #[test]
    fn pm_check_blocks_an_under_declared_rune() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["check".into(), "projects/pm/tests/fixtures/leaky".into()],
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            out,
            vec!["BLOCK: code demands authority not admitted by [capabilities]: Dir[Read]"]
        );
        assert_eq!(code, 2, "an under-declared rune must exit 2");
    }

    /// pm passes its *own* `check`: its manifest declares exactly `Console, Dir`,
    /// which is what the code demands — the package manager is consistent with
    /// itself, proving the self-hosted gate is honest.
    #[test]
    fn pm_passes_its_own_check() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["check".into(), "projects/pm".into()],
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(out, vec!["OK: declared footprint admits the code, nothing unused"]);
        assert_eq!(code, 0);
    }

    /// `pm new <name>` scaffolds a runnable rune (manifest + src stub) using the
    /// *write* Dir capability, confined to the workspace root. The scaffold is
    /// real: the generated rune both passes its own `check` and runs.
    #[test]
    fn pm_new_scaffolds_a_runnable_rune() {
        let tmp = std::env::temp_dir().join("witchy_pm_new_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let (linked, _stem) = crate::link_file("projects/pm/src/pm.witchy").expect("link");
        typeck::check(&linked).expect("typeck");
        let (out, code) = interpreter::run_module_exit(
            linked,
            &tmp,
            Vec::new(),
            vec!["new".into(), "widget".into()],
            None,
        )
        .expect("run");
        assert_eq!(code, 0);
        assert!(out.iter().any(|l| l.contains("created rune `widget`")));

        let manifest = std::fs::read_to_string(tmp.join("widget/witchy.toml"))
            .expect("manifest was written");
        assert!(manifest.contains("name = \"widget\""));
        assert!(manifest.contains("runtime = [\"Console\"]"));
        let src = std::fs::read_to_string(tmp.join("widget/src/widget.witchy"))
            .expect("src stub was written");
        assert!(src.contains("hello from widget"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `pm deps <dir>` lists a rune's dependencies and their source — read
    /// straight from `[dependencies]`'s inline tables (`toml.table`/`inline_get`).
    #[test]
    fn pm_lists_dependencies() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["deps".into(), "examples/projects/ledger/ledger".into()],
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(out, vec!["money -> path:../money"]);
        assert_eq!(code, 0);
    }

    /// `pm info <dir>` summarizes a rune: name, version, declared vs. recomputed
    /// footprint. Run on the pm itself — its declared `[capabilities]` exactly
    /// match what the code demands (Console, Dir, Net), the self-consistency the
    /// `check` gate enforces.
    #[test]
    fn pm_info_summarizes_a_rune() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["info".into(), "projects/pm".into()],
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            out,
            vec![
                "name:     pm",
                "version:  0.1.0",
                "declared: Console, Dir, Net, Exec, Env, Clock",
                "actual:   Clock, Console, Dir, Env, Exec, Net",
            ]
        );
        assert_eq!(code, 0);
    }

    /// The interop milestone: `pm verify` recomputes each dependency's content
    /// hash and checks it against the *committed, coven-generated* `witchy.lock`.
    /// It passes — the self-hosted pm's hashing is byte-identical to coven's
    /// store, so a witchy-checked lock and a coven-written one agree.
    #[test]
    fn pm_verify_validates_a_coven_generated_lockfile() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["verify".into(), "examples/projects/ledger/ledger".into()],
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(out, vec!["OK: every locked hash matches the dependency sources"]);
        assert_eq!(code, 0);
    }

    /// `pm gate` is the supply-chain gate: after a dependency is locked, an edit
    /// to its source that *widens* its capability footprint is BLOCKed (exit 2),
    /// with the new authority attributed to the rune that introduced it.
    /// Explicitly accepting those caps (like `--allow-cap`) folds them into the
    /// baseline and clears the block.
    #[test]
    fn pm_gate_blocks_a_dependency_that_widens_authority() {
        let tmp = std::env::temp_dir().join(format!("witchy_pm_gate_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("app/src")).unwrap();
        std::fs::create_dir_all(tmp.join("lib/src")).unwrap();
        std::fs::write(
            tmp.join("app/witchy.toml"),
            "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"lib\" = { path = \"../lib\" }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("app/src/app.witchy"),
            "fn main(console: Console):\n    print(console, \"hi\")\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("lib/witchy.toml"),
            "[rune]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        // lib starts pure: no capabilities.
        std::fs::write(
            tmp.join("lib/src/lib.witchy"),
            "fn f(s: String) -> String:\n    s\n",
        )
        .unwrap();

        let run_pm = |args: Vec<String>| -> (Vec<String>, i32) {
            let (linked, _stem) = crate::link_file("projects/pm/src/pm.witchy").expect("link");
            typeck::check(&linked).expect("typeck");
            interpreter::run_module_exit(linked, &tmp, Vec::new(), args, None).expect("run")
        };

        run_pm(vec!["lock".into(), "app".into()]);
        let (out, code) = run_pm(vec!["gate".into(), "app".into()]);
        assert_eq!(out, vec!["OK: dependencies demand no authority beyond witchy.lock"]);
        assert_eq!(code, 0);

        // lib's source widens to demand Console + Net — gate must BLOCK and name lib.
        std::fs::write(
            tmp.join("lib/src/lib.witchy"),
            "fn main(console: Console, net: Net):\n    let s = connect(net, \"example.com:80\")\n    print(console, \"connected\")\n",
        )
        .unwrap();
        let (out, code) = run_pm(vec!["gate".into(), "app".into()]);
        assert_eq!(
            out,
            vec![
                "BLOCK: dependencies demand new authority: Console, Net",
                "  Console <- lib",
                "  Net <- lib",
            ]
        );
        assert_eq!(code, 2, "a widening dependency must exit 2");

        // Accepting both new caps clears the gate.
        let (out, code) = run_pm(vec![
            "gate".into(),
            "app".into(),
            "Console".into(),
            "Net".into(),
        ]);
        assert_eq!(out, vec!["OK: dependencies demand no authority beyond witchy.lock"]);
        assert_eq!(code, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `main -> Int` sets the process exit code (C/Go/Rust convention) and is
    /// *not* printed; `main` returning Nil exits 0 and shows its `print` output.
    #[test]
    fn main_int_return_is_the_process_exit_code() {
        let run = |src: &str| {
            let m = parser::parse_module(src).expect("parse");
            let l = crate::pipeline::link(vec![("main".into(), m)], "main").expect("link");
            interpreter::run_module_exit(l, ".", Vec::new(), Vec::new(), None).expect("run")
        };
        let (out, code) = run("fn main() -> Int:\n    7\n");
        assert!(out.is_empty(), "an Int return must not be printed, got {out:?}");
        assert_eq!(code, 7);
        let (out, code) = run("fn main(console: Console):\n    print(console, \"hi\")\n");
        assert_eq!(out, vec!["hi"]);
        assert_eq!(code, 0);
    }

    #[test]
    fn dir_write_is_confined_to_the_subtree() {
        let tmp = std::env::temp_dir().join("witchy_dir_write_test");
        std::fs::create_dir_all(&tmp).unwrap();
        let run = |src: &str| {
            let mods = vec![("main".to_string(), parser::parse_module(src).expect("parse"))];
            let linked = crate::pipeline::link(mods, "main").expect("link");
            interpreter::run_module(linked, &tmp, Vec::new())
        };
        // Write then read back, within the confined Dir.
        let out = run("fn main(console: Console, root: Dir):\n    write(root, \"out.txt\", \"hi\")\n    print(console, read(root, \"out.txt\"))\n")
            .expect("run");
        assert_eq!(out, vec!["hi"]);
        assert_eq!(std::fs::read_to_string(tmp.join("out.txt")).unwrap(), "hi");
        // A `..` write is refused — the capability can't escape its subtree.
        assert!(run("fn main(console: Console, root: Dir):\n    write(root, \"../escape.txt\", \"x\")\n").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `list` (enumerate, sorted) and `make_dir` (create a confined subdir) — the
    /// filesystem ops a package store/registry needs. `list` needs `Read`,
    /// `make_dir` needs `Write`, and both stay confined to the capability's subtree.
    #[test]
    fn dir_list_and_make_dir_work_and_are_rights_checked() {
        let tmp = std::env::temp_dir().join("witchy_dir_list_test");
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
        let out = run("import string\nfn main(console: Console, root: Dir):\n    print(console, list.join(list(subtree(root, \"store\")), \",\"))\n")
            .expect("run");
        assert_eq!(out, vec!["alpha,bravo"]);
        // `make_dir` creates a confined subdirectory.
        run("fn main(console: Console, root: Dir):\n    make_dir(root, \"fresh\")\n").expect("run");
        assert!(tmp.join("fresh").is_dir(), "make_dir should have created the directory");
        // Confinement: a `..` make_dir is refused.
        assert!(run("fn main(console: Console, root: Dir):\n    make_dir(root, \"../escaped\")\n").is_err());
        assert!(!tmp.parent().unwrap().join("escaped").exists(), "make_dir must not escape the subtree");

        // Rights: `list` needs Read, `make_dir` needs Write.
        assert!(typeck::check_str("fn main(c: Console, d: Dir[Write]):\n    let n = list(d)\n").is_err());
        assert!(typeck::check_str("fn main(c: Console, d: Dir[Read]):\n    make_dir(d, \"x\")\n").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn dir_write_refuses_a_symlink_leaf() {
        // A pre-existing symlink in the subtree must not let a write escape it.
        let base = std::env::temp_dir().join("witchy_dir_symlink_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sandbox")).unwrap();
        std::fs::write(base.join("secret.txt"), "ORIGINAL").unwrap();
        std::os::unix::fs::symlink("../secret.txt", base.join("sandbox/link.txt")).unwrap();

        let mods = vec![(
            "main".to_string(),
            parser::parse_module(
                "fn main(console: Console, root: Dir):\n    write(subtree(root, \"sandbox\"), \"link.txt\", \"PWNED\")\n",
            )
            .expect("parse"),
        )];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        assert!(interpreter::run_module(linked, &base, Vec::new()).is_err());
        // The symlink target outside the subtree is untouched.
        assert_eq!(std::fs::read_to_string(base.join("secret.txt")).unwrap(), "ORIGINAL");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Rights-parameterized `Dir`: the right-set in the type statically gates the
    /// ops. A `Dir[Read]` structurally cannot `write`; bare `Dir` is the full set
    /// (back-compat); `read_only`/`write_only` are monotone attenuations that the
    /// checker enforces (you can only keep a right you already hold).
    #[test]
    fn dir_rights_are_statically_enforced() {
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

        // Bare `Dir` carries the full right-set: reads and writes both type-check.
        ok("fn use_both(d: Dir):\n    write(d, \"o\", read(d, \"i\"))\nfn main(c: Console, root: Dir):\n    use_both(root)\n");
        // `Dir[Read]` cannot write — a compile-time error.
        err(
            "fn save(d: Dir[Read]):\n    write(d, \"o\", \"x\")\nfn main(c: Console, root: Dir):\n    save(root)\n",
            "`write` needs `Write`",
        );
        // `Dir[Write]` cannot read.
        err(
            "fn load(d: Dir[Write]):\n    let s = read(d, \"i\")\nfn main(c: Console, root: Dir):\n    load(root)\n",
            "`read` needs `Read`",
        );
        // `as Dir[Read]` narrows; a later write through it is rejected.
        err(
            "fn f(d: Dir):\n    let r = d as Dir[Read]\n    write(r, \"o\", \"x\")\nfn main(c: Console, root: Dir):\n    f(root)\n",
            "`write` needs `Write`",
        );
        // `as` cannot resurrect a `Write` the capability never had (not a subset).
        err(
            "fn f(d: Dir[Read]):\n    let w = d as Dir[Write]\nfn main(c: Console, root: Dir):\n    f(root)\n",
            "`as` can only drop rights",
        );
        // `Dir[Read, Write]` is equivalent to bare `Dir` — both verbs allowed.
        ok("fn f(d: Dir[Read, Write]):\n    write(d, \"o\", read(d, \"i\"))\nfn main(c: Console, root: Dir):\n    f(root)\n");
    }

    /// `as` narrowing is the identity at runtime (rights live only in the type),
    /// so a narrowed handle still reads the same confined subtree.
    #[test]
    fn as_narrowing_is_identity_at_runtime() {
        let tmp = std::env::temp_dir().join("witchy_dir_as_narrow_test");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("in.txt"), "narrowed").unwrap();
        let src = "fn main(console: Console, root: Dir):\n    let r = root as Dir[Read]\n    print(console, read(r, \"in.txt\"))\n";
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
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

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
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

        // A full `Net`/`Dir` coerces into a narrowed parameter — no `as` needed.
        ok("fn fetch(n: Net[Connect]) -> Socket:\n    connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    let s = fetch(net)\n");
        ok("fn dial(n: Net[Connect, Tcp]) -> Socket:\n    connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    let s = dial(net)\n");
        ok("fn load(d: Dir[Read]) -> String:\n    read(d, \"f\")\nfn main(c: Console, root: Dir):\n    let x = load(root)\n");
        // The type ceiling holds: a `Net[Connect]` cannot be re-widened to satisfy
        // a full-`Net` parameter (soundness — no laundering authority back up).
        err(
            "fn g(m: Net):\n    let l = listen(m, \"b:2\")\nfn f(n: Net[Connect]):\n    g(n)\nfn main(c: Console, net: Net):\n    f(net)\n",
            "expected `Net`, found `Net[Connect]`",
        );
        // A too-narrow argument is still rejected (Connect cannot satisfy Listen).
        err(
            "fn serve(n: Net[Listen]):\n    let l = listen(n, \"b:2\")\nfn main(c: Console, net: Net):\n    serve(net as Net[Connect])\n",
            "expected `Net[Listen]`, found `Net[Connect]`",
        );

        // The same directional narrowing holds wherever a value flows into a
        // capability-typed slot, not just call arguments:
        // (a) a return type — return a full `Net` where `Net[Connect]` is declared,
        ok("fn client(net: Net) -> Net[Connect]:\n    net\nfn main(c: Console, net: Net):\n    let s = connect(client(net), \"a:1\")\n");
        // (b) a constructor field that holds a narrowed capability.
        ok("type Client:\n    Client(Net[Connect])\nfn main(c: Console, net: Net):\n    let x = Client(net)\n");
        // Both still reject *widening* (the type ceiling holds at every position).
        err(
            "fn bad(n: Net[Connect]) -> Net:\n    n\nfn main(c: Console, net: Net):\n    bad(net as Net[Connect])\n",
            "expected `Net`, found `Net[Connect]`",
        );
        err(
            "type Server:\n    Server(Net)\nfn make(n: Net[Connect]) -> Server:\n    Server(n)\nfn main(c: Console, net: Net):\n    make(net as Net[Connect])\n",
            "expected `Net`, found `Net[Connect]`",
        );
    }

    /// Rights-parameterized `Net`: the verb-set in the type distinguishes a client
    /// from a server. `Net[Connect]` cannot `listen`; `Net[Listen]` cannot
    /// `connect`; bare `Net` is the full set (back-compat). Narrowing is done with
    /// the `as` ascription, which can only drop rights.
    #[test]
    fn net_verbs_are_statically_enforced() {
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

        // Bare `Net` grants both verbs.
        ok("fn f(n: Net):\n    let s = connect(n, \"a:1\")\n    let l = listen(n, \"b:2\")\nfn main(c: Console, net: Net):\n    f(net)\n");
        // `Net[Connect]` is a client — it cannot listen.
        err(
            "fn f(n: Net[Connect]):\n    let l = listen(n, \"b:2\")\nfn main(c: Console, net: Net):\n    f(net)\n",
            "`listen` needs `Listen`",
        );
        // `Net[Listen]` is a server — it cannot dial out.
        err(
            "fn f(n: Net[Listen]):\n    let s = connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    f(net)\n",
            "`connect` needs `Connect`",
        );
        // `as Net[Connect]` narrows; listening through it is rejected.
        err(
            "fn f(n: Net):\n    let c = n as Net[Connect]\n    let l = listen(c, \"b:2\")\nfn main(c: Console, net: Net):\n    f(net)\n",
            "`listen` needs `Listen`",
        );
        // `as` cannot resurrect a `Connect` the capability never had (not a subset).
        err(
            "fn f(n: Net[Listen]):\n    let c = n as Net[Connect]\nfn main(c: Console, net: Net):\n    f(net)\n",
            "`as` can only drop rights",
        );
        // The refinement verb `only` is verb-neutral (it preserves the rights set) — the
        // property this arm shares with the retired `restrict`; it is exercised end-to-end by
        // `net_only_refinement_verb_backends_agree`.
    }

    /// The `Net` transport axis: only `Tcp` is implemented, so `connect`/`listen`
    /// require it; `Udp`/`Uds` are type-level markers that keep the taxonomy
    /// expressible. Each axis defaults to full independently (`Net[Connect]` keeps
    /// all transports). Narrowing the transport axis is done with `as`.
    #[test]
    fn net_transport_is_statically_enforced() {
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

        // `Net[Connect]` keeps all transports (incl. Tcp), so connect works.
        ok("fn f(n: Net[Connect]):\n    let s = connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    f(net as Net[Connect])\n");
        // A transport narrowed away from Tcp cannot drive a (TCP-only) connect.
        err(
            "fn f(n: Net[Connect, Udp]):\n    let s = connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    f(net as Net[Connect, Udp])\n",
            "only implemented over `Tcp`",
        );
        err(
            "fn f(n: Net[Listen, Uds]):\n    let l = listen(n, \"a:1\")\nfn main(c: Console, net: Net):\n    f(net as Net[Listen, Uds])\n",
            "only implemented over `Tcp`",
        );
        // `as Net[Connect, Tcp]` narrows both axes; a TCP connect through the
        // result type-checks end to end.
        ok("fn dial(n: Net[Connect, Tcp]) -> Socket:\n    connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    let s = dial(net as Net[Connect, Tcp])\n");
        // You cannot keep a transport the capability does not hold (not a subset).
        err(
            "fn f(n: Net[Connect, Tcp]):\n    let u = n as Net[Connect, Udp]\nfn main(c: Console, net: Net):\n    f(net as Net[Connect, Tcp])\n",
            "`as` can only drop rights",
        );
    }

    /// `import list` resolves to the bundled standard library (no local file),
    /// links, type-checks, and runs end to end through the CLI.
    #[test]
    fn std_library_resolves_and_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/std_demo/src/std_demo.witchy", Vec::new()).unwrap(),
            vec!["30", "3"]
        );
    }

    /// Sorting with a comparator closure, end to end through the bundled std.
    #[test]
    fn sort_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/sort/src/sort.witchy", Vec::new()).unwrap(),
            vec!["1,1,3,4,5", "5,4,3,1,1"]
        );
    }

    /// The bundled `math` module resolves and computes via the CLI.
    #[test]
    fn math_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/math_demo/src/math_demo.witchy", Vec::new()).unwrap(),
            vec!["7", "5", "10", "1024", "12"]
        );
    }

    /// Float math: the `sqrt` builtin and the `math` module's Float helpers.
    #[test]
    fn floats_run_via_cli() {
        assert_eq!(
            crate::execute_file("examples/floats/src/floats.witchy", Vec::new()).unwrap(),
            vec!["4.0", "3.5", "5.0", "1.0"]
        );
    }

    /// The list module's search/slice helpers via the CLI.
    #[test]
    fn list_more_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/list_more/src/list_more.witchy", Vec::new()).unwrap(),
            vec!["true", "3", "-1", "20", "30"]
        );
    }

    /// The list-combinator pipeline example runs via the CLI (interpreter); a
    /// companion compiled test (`list_pipeline_example_runs_on_wasm`) asserts the
    /// same output through the WASM backend.
    #[test]
    fn list_pipeline_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/list_pipeline/src/list_pipeline.witchy", Vec::new()).unwrap(),
            vec!["233", "2 8", "735"]
        );
    }

    /// `zip`/`enumerate` and tuple destructuring in a loop, via the CLI.
    #[test]
    fn zip_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/zip/src/zip.witchy", Vec::new()).unwrap(),
            vec!["0:alice 1:bob 2:carol", "alice=30 bob=25 carol=40"]
        );
    }

    /// `any`/`all` predicate combinators via the CLI.
    #[test]
    fn predicates_run_via_cli() {
        assert_eq!(
            crate::execute_file("examples/predicates/src/predicates.witchy", Vec::new()).unwrap(),
            vec!["true", "true", "false", "false"]
        );
    }

    /// `all` is vacuously true on the empty list; `any` is false.
    #[test]
    fn any_all_empty_list_edge_cases() {
        let client = r#"
import list

fn main(console: Console):
    let empty = list.filter([1], fn(n: Int): (n > 100))
    print(console, __render(list.all(empty, fn(n: Int): (n > 0))))
    print(console, __render(list.any(empty, fn(n: Int): (n > 0))))
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("predicates program runs");
        assert_eq!(out, vec!["true", "false"]);
    }

    /// `zip` is generic and stops at the shorter list.
    #[test]
    fn zip_is_generic_and_truncates() {
        let client = r#"
import list

fn main(console: Console):
    let ps = list.zip([1, 2, 3], ["a", "b"])
    print(console, __render(list.length(ps)))
    let first = list.at(ps, 0)
    let (n, s) = first
    print(console, (__render(n) + s))
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("zip program runs");
        assert_eq!(out, vec!["2", "1a"]);
    }

    /// `contains`/`index_of` are generic — they work on Strings too (by value).
    #[test]
    fn list_contains_is_generic_over_element_type() {
        let client = r#"
import list

fn main(console: Console):
    let words = ["a", "bb", "ccc"]
    print(console, __render(list.contains(words, "bb")))
    print(console, __render(list.index_of(words, "ccc")))
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("list program runs");
        assert_eq!(out, vec!["true", "2"]);
    }

    /// The bundled `option` module (type + helpers) resolves via the CLI.
    #[test]
    fn option_module_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/option_std/src/option_std.witchy", Vec::new()).unwrap(),
            vec!["10", "-1"]
        );
    }

    /// The bundled `result` module supplies the type `?` recognizes, plus
    /// helpers, when linked against a client.
    #[test]
    fn result_module_links_with_try_and_helpers() {
        let client = r#"
import result

fn checked_div(a: Int, b: Int) -> Result(Int, String):
    match b:
        0 -> Err("zero")
        _ -> Ok((a / b))

fn compute(x: Int, y: Int) -> Result(Int, String):
    let q = (checked_div(x, y))?
    Ok((q + 1))

fn main(console: Console):
    print(console, __render(result.unwrap_or(compute(10, 2), (0 - 1))))
    print(console, __render(result.unwrap_or(compute(10, 0), (0 - 1))))
    print(console, __render(result.is_ok(compute(10, 0))))
"#;
        let out = interpreter::run_program(
            &[("result", crate::bundled_module("result").unwrap()), ("main", client)],
            "main",
        )
        .expect("result module program runs");
        assert_eq!(out, vec!["6", "-1", "false"]);
    }

    /// String builtins + the bundled `list`/`string` modules end to end.
    #[test]
    fn text_processing_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/text/src/text.witchy", Vec::new()).unwrap(),
            vec!["ALICE | BOB | CAROL", "===", "alice,***,carol"]
        );
    }

    /// The bundled `list` module type-checks and links against a client program.
    #[test]
    fn bundled_list_module_links() {
        let client = r#"
import list

fn main(console: Console):
    let xs = list.map(list.range(4), fn(n: Int): (n + 1))
    print(console, __render(list.fold(xs, 0, fn(a: Int, b: Int): (a + b))))
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("std list program runs");
        assert_eq!(out, vec!["10"]); // (1+2+3+4)
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
        // `${expr}` desugars to `<> __render(expr) <>`, so interpolation works
        // in both backends: String pass-through, Int/Bool via to_string, embedded
        // calls/arithmetic, `\$` for a literal `$`, and adjacent interpolations.
        let src = r#"
fn main(console: Console):
    let name = "witchy"
    let age = 3
    print(console, "hi ${name}, age ${age}")
    print(console, "sum: ${__render(age + 10)}")
    print(console, "flag ${age > 1}")
    print(console, "literal \${x} stays")
    print(console, "${name}${name}")
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
    fn higher_order_example_runs_on_wasm() {
        // Closure returned from a function (make_adder) + higher-order reduce.
        let src = include_str!("../examples/higher_order/src/higher_order.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["15", "81", "15", "120"]);
    }

    #[test]
    fn record_update_example_runs_on_wasm() {
        // `update` referencing the original record, plus a String-field update;
        // the original is unchanged.
        let src = include_str!("../examples/record_update/src/record_update.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["alice 100", "alice 150", "alice smith 150"]
        );
    }

    #[test]
    fn closures_capturing_loop_var_backends_agree() {
        // Closures created in a loop each capture that iteration's value of the
        // loop variable (by value), are stored in a list, and called back. Both
        // backends agree — no shared-loop-variable surprise.
        let src = r#"
fn main(console: Console):
    var fs = []
    for i in [1, 2, 3]:
        fs = list.push(fs, fn(x: Int): (x + i))
    let f0 = list.at(fs, 0)
    let f2 = list.at(fs, 2)
    print(console, __render(f0(10)))
    print(console, __render(f2(10)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["11", "13"]);
    }

    #[test]
    fn zero_arg_closures_backends_agree() {
        // Zero-argument closures (incl. capturing ones) compile and run.
        let src = r#"
fn call0(f: fn() -> Int) -> Int:
    f()

fn main(console: Console):
    print(console, __render(call0(fn(): 42)))
    let base = 100
    print(console, __render(call0(fn(): (base + 1))))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "101"]);
    }

    #[test]
    fn closure_capturing_closure_backends_agree() {
        // A closure that captures another closure and calls it through a
        // higher-order function. Both backends agree.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let g = fn(x: Int): (x + 1)
    let h = fn(y: Int): (apply(g, y) * 2)
    print(console, __render(apply(h, 5)))
    print(console, __render(apply(h, 20)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["12", "42"]); // (5+1)*2, (20+1)*2
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
    print(console, __render(sum))
    var x = 100
    x = (x - 30)
    x = (x * 2)
    x = (x / 7)
    x = (x % 5)
    print(console, __render(x))
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
    print(console, (("[" + string.replace("abc", "", "-")) + "]"))
    print(console, string.replace("abc", "x", "y"))
    print(console, string.replace("aaa", "a", "bb"))
    print(console, string.replace("hello world", "o", "0"))
    var d = dict.new()
    d = dict.insert(d, 1, 100)
    d = dict.insert(d, 2, 200)
    d = dict.insert(d, 1, 111)
    print(console, __render(dict.get_or(d, 1, 0)))
    print(console, __render(dict.get_or(d, 2, 0)))
    print(console, __render(dict.length(d)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "replace/int-key dict diverged");
    }

    // Integer division/modulo truncate toward zero, and their signs must agree
    // for negative operands across the i64 interpreter and i32 codegen (the
    // results here stay well within i32). Also locks in dict insert-overwrite,
    // removing an absent key, and `get_or`'s default path.
    #[test]
    fn negative_arithmetic_and_dict_mutation_backends_agree() {
        let src = r#"
fn main(console: Console):
    print(console, __render((0 - (7 / 2))))
    print(console, __render(((0 - 7) % 2)))
    print(console, __render((7 / (0 - 2))))
    print(console, __render((7 % (0 - 2))))
    print(console, __render(((0 - 7) / (0 - 2))))
    var d = dict.new()
    d = dict.insert(d, "k", 1)
    d = dict.insert(d, "k", 2)
    print(console, __render(dict.get_or(d, "k", 0)))
    print(console, __render(dict.length(d)))
    d = dict.remove(d, "missing")
    print(console, __render(dict.length(d)))
    print(console, __render(dict.get_or(d, "absent", 99)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "int/dict edges diverged");
    }

    // Boundary behavior of the string builtins: an empty separator yields the
    // whole string, substrings clamp (and start>end gives ""), an empty needle
    // for index_of returns 0, a missing one returns -1, and empty-string concat
    // is identity. These clamp/empty rules are easy to get subtly different in
    // codegen, so assert the backends agree.
    // Immediately applying a function-valued expression — `make(3)(4)`,
    // `(fn(x){..})(7)`, chains, and an application nested inside another's
    // argument — must behave identically on both backends. The nested-in-arg
    // case in particular exercises codegen's per-level scratch locals (the
    // callee pointer must survive argument evaluation).
    // Function values stored in data structures and applied immediately — the
    // composition unlocked by Expr::Apply. A closure pulled from a list with
    // `at`, one selected by an `if` expression, and one held in a record field
    // (reached via `(b.f)(b.n)`) must all apply identically on both backends.
    #[test]
    fn fn_values_in_data_backends_agree() {
        let src = r#"
type Box:
    f: fn(Int) -> Int
    n: Int

fn main(console: Console):
    let fns = [fn(x: Int): (x + 1), fn(x: Int): (x * 10)]
    print(console, __render((list.at(fns, 0))(5)))
    print(console, __render((list.at(fns, 1))(5)))
    let pick = true
    print(console, __render((if pick: fn(x: Int): (x + 100) else: fn(x: Int): x)(7)))
    let b = Box(fn(x: Int): (x * 3), 7)
    print(console, __render(((b).f)((b).n)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "fn-values-in-data diverged");
        assert_eq!(run_on_wasm(src), vec!["6", "50", "107", "21"]);
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
    print(console, describe(Yep(50)))
    print(console, describe(Yep(3)))
    print(console, describe(Nope))
    print(console, __render(if is_even(10): 1 else: 0))
    print(console, __render(if is_odd(7): 1 else: 0))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "adt guards / mutual recursion diverged");
        assert_eq!(run_on_wasm(src), vec!["big", "small", "none", "1", "1"]);
    }

    // `match` arm guards (`pattern if cond -> body`): a guard that fails must
    // fall through to later arms, and a wildcard catches the rest. The boundary
    // value 100 (not > 100) must fall through to the `_` arm on both backends.
    #[test]
    fn match_guards_backends_agree() {
        let src = r#"
fn classify(n: Int) -> String:
    match n:
        x if (x < 0) -> "negative"
        0 -> "zero"
        x if (x > 100) -> "big"
        _ -> "small"

fn main(console: Console):
    print(console, classify((0 - 5)))
    print(console, classify(0))
    print(console, classify(200))
    print(console, classify(50))
    print(console, classify(100))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "match guards diverged");
        assert_eq!(run_on_wasm(src), vec!["negative", "zero", "big", "small", "small"]);
    }

    // Dict operations factored into helper functions: codegen picks the
    // string-vs-i32 key comparison from the static key type, so a `k: String`
    // parameter must compile to by-value comparison just like an inline String
    // key. Looking up with a freshly built string (`"ap" + "ple"`) proves the
    // match is structural, not by pointer — and both backends must agree.
    // An integration stress test for first-class functions: a list of closures
    // folded with a higher-order lambda that applies each function-typed
    // element to the accumulator (`f(acc)`). Exercises closures stored in a
    // list, a function-typed fold element, and calling a function-valued lambda
    // parameter — all of which must agree across backends.
    // Nested records: `l.from.x` requires codegen to resolve the record type of
    // the intermediate field (`l.from` is a Point) to index the next one. Record
    // update rebuilds the outer record with one field replaced, leaving the rest
    // (and the original value) untouched. Both backends must agree.
    #[test]
    fn nested_records_and_update_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Line:
    from: Point
    to: Point

fn main(console: Console):
    let l = Line(Point(1, 2), Point(3, 4))
    print(console, __render(((l).from).x))
    print(console, __render(((l).to).y))
    let l2 = Line(from: Point(10, 20), ..l)
    print(console, __render(((l2).from).x))
    print(console, __render(((l2).to).y))
    print(console, __render(((l).from).x))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "nested records / update diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "4", "10", "4", "1"]);
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
    print(console, __render(sum_tree(t)))
    print(console, __render(depth(t)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "recursive tree ADT diverged");
        assert_eq!(run_on_wasm(src), vec!["11", "3"]);
    }

    // Tuple patterns in `match`: literals and wildcards in each position
    // (quadrant), plus binding tuple elements alongside a literal in another
    // position (describe). Destructuring a matched tuple must agree across
    // backends.
    // List comprehensions desugar to a block that builds the list with a for
    // loop and push: `[elem for x in xs (if cond)?]`. Mapping, filtering, and an
    // empty source all agree across backends.
    // Comprehensions compose with records: the element expression and the `if`
    // filter both access fields of the loop variable (resolved because the
    // source is a List(Record)). Both backends agree.
    #[test]
    fn list_comprehension_over_records_backends_agree() {
        let client = r#"
import list
type Item:
    name: String
    qty: Int
fn main(console: Console):
    let cart = [Item("apple", 3), Item("bread", 1), Item("milk", 2)]
    let multi = [it.name for it in cart if it.qty > 1]
    for n in multi:
        print(console, n)
    let qtys = [it.qty * 10 for it in cart]
    for q in qtys:
        print(console, __render(q))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "comprehension over records diverged");
        assert_eq!(compiled, vec!["apple", "milk", "30", "10", "20"]);
    }

    // Multi-generator comprehensions nest in source order: two `for` clauses
    // form a cartesian product, and an interleaved `if` filters using earlier
    // loop variables. Both backends agree.
    // Integration showcase: Pythagorean triples in one comprehension —
    // three nested generators over inclusive ranges with variable bounds
    // (`b in a..=20`), a filter, and tuple construction, then tuple
    // destructuring in a for-loop. Exercises ranges + multi-generator
    // comprehensions + tuples together; both backends agree.
    #[test]
    fn pythagorean_triples_comprehension_backends_agree() {
        let client = r#"
import list
fn main(console: Console):
    let triples = [(a, b, c) for a in 1..=20 for b in a..=20 for c in b..=20 if a * a + b * b == c * c]
    print(console, __render(list.length(triples)))
    var total = 0
    for t in triples:
        let (a, b, c) = t
        total = total + c
    print(console, __render(total))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pythagorean comprehension diverged");
        assert_eq!(compiled, vec!["6", "80"]);
    }

    #[test]
    fn multi_generator_comprehension_backends_agree() {
        let src = r#"
fn main(console: Console):
    let pairs = [x * 10 + y for x in [1, 2] for y in [3, 4]]
    for p in pairs:
        print(console, __render(p))
    let upper = [x * 10 + y for x in [1, 2, 3] for y in [1, 2, 3] if y > x]
    for p in upper:
        print(console, __render(p))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "multi-generator comprehension diverged");
        assert_eq!(
            run_on_wasm(src),
            vec!["13", "14", "23", "24", "12", "13", "23"]
        );
    }

    // break exits the innermost loop; continue skips to the next iteration —
    // in both for-loops (continue advances the index) and while-loops (continue
    // re-checks the condition). Both backends agree.
    // break/continue branching out of a result-typed `match` arm inside a loop
    // must still produce valid WASM (the branch unwinds the match's value).
    #[test]
    fn break_inside_match_in_loop_backends_agree() {
        let src = r#"
fn main(console: Console):
    var total = 0
    for x in [1, 2, 3, 4, 5]:
        match x:
            3 ->
                break
            _ ->
                total = (total + x)
    print(console, __render(total))
    var kept = 0
    for y in [1, 2, 3, 4]:
        match y:
            2 ->
                continue
            _ -> 0
        kept = (kept + y)
    print(console, __render(kept))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "break/continue in match diverged");
        assert_eq!(run_on_wasm(src), vec!["3", "8"]);
    }

    #[test]
    fn break_continue_backends_agree() {
        let src = r#"
fn main(console: Console):
    var sum = 0
    for x in [1, 2, 3, 4, 5, 6, 7, 8]:
        if (x > 5):
            break
        if ((x % 2) == 0):
            continue
        sum = (sum + x)
    print(console, __render(sum))
    var i = 0
    var found = 0
    while (i < 100):
        i = (i + 1)
        if (i < 10):
            continue
        found = i
        break
    print(console, __render(found))
    var count = 0
    for a in [1, 2, 3]:
        for b in [1, 2, 3]:
            if (b == 2):
                break
            count = (count + 1)
    print(console, __render(count))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "break/continue diverged");
        assert_eq!(run_on_wasm(src), vec!["9", "10", "3"]);
    }

    // The `a..b` range operator builds the half-open list [a, b): usable in a
    // for-loop, in a comprehension, and empty when a >= b. Both backends agree.
    // Inclusive range `a..=b` includes the upper bound: [a, b]. Empty when
    // a > b, single when a == b, and composes with comprehensions. Both backends agree.
    #[test]
    fn inclusive_range_backends_agree() {
        let src = r#"
fn main(console: Console):
    for i in 1..=5:
        print(console, __render(i))
    print(console, __render(list.length(0..=0)))
    print(console, __render(list.length(5..=2)))
    print(console, __render(list.length([n for n in 1..=4])))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "inclusive range diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "2", "3", "4", "5", "1", "0", "4"]);
    }

    #[test]
    fn range_operator_backends_agree() {
        let src = r#"
fn main(console: Console):
    for i in 0..5:
        print(console, __render(i))
    let squares = [x * x for x in 1..5]
    for s in squares:
        print(console, __render(s))
    print(console, __render(list.length(3..3)))
    print(console, __render(list.length(2..(1 + 4))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "range operator diverged");
        assert_eq!(
            run_on_wasm(src),
            vec!["0", "1", "2", "3", "4", "1", "4", "9", "16", "0", "3"]
        );
    }

    #[test]
    fn list_comprehension_backends_agree() {
        let src = r#"
fn main(console: Console):
    let squares = [n * n for n in [1, 2, 3, 4]]
    for s in squares:
        print(console, __render(s))
    let evens = [n for n in [1, 2, 3, 4, 5, 6] if n % 2 == 0]
    for e in evens:
        print(console, __render(e))
    print(console, __render(list.length([x for x in [] if x > 0])))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "list comprehension diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "4", "9", "16", "2", "4", "6", "0"]);
    }

    #[test]
    fn tuple_patterns_backends_agree() {
        let src = r#"
fn quadrant(x: Int, y: Int) -> String:
    match (x, y):
        (0, 0) -> "origin"
        (0, _) -> "y-axis"
        (_, 0) -> "x-axis"
        _ -> "other"

fn describe(pair: (Int, String)) -> String:
    match pair:
        (0, s) -> ("zero:" + s)
        (n, "stop") -> ("stop@" + __render(n))
        (n, s) -> ((s + "=") + __render(n))

fn main(console: Console):
    print(console, quadrant(0, 0))
    print(console, quadrant(0, 5))
    print(console, quadrant(5, 0))
    print(console, quadrant(2, 3))
    print(console, describe((0, "x")))
    print(console, describe((7, "stop")))
    print(console, describe((4, "k")))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "tuple patterns diverged");
        assert_eq!(
            run_on_wasm(src),
            vec!["origin", "y-axis", "x-axis", "other", "zero:x", "stop@7", "k=4"]
        );
    }

    // The classic loop-capture pitfall: each iteration creates a closure that
    // captures a fresh `let` binding. Capture is by value at creation, so the
    // three closures must remember 0, 1, 2 (giving 10, 11, 12) — not share the
    // final loop value. Both backends must agree.
    #[test]
    fn closure_captures_loop_value_backends_agree() {
        let src = r#"
fn main(console: Console):
    var fns = []
    var i = 0
    while (i < 3):
        let captured = i
        fns = list.push(fns, fn(x: Int): (x + captured))
        i = (i + 1)
    for f in fns:
        print(console, __render(f(10)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "loop-captured closures diverged");
        assert_eq!(run_on_wasm(src), vec!["10", "11", "12"]);
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
    print(console, __render(first_of(pi)))
    print(console, first_of(ps))
    print(console, second_of(ps))
    print(console, __render(first_of(pm)))
    print(console, second_of(pm))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "multi-type generics diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "a", "b", "7", "mixed"]);
    }

    // `update` on a base that is not a bare variable: a field access (`l.from`)
    // and an `if` expression. Codegen used to require a record-typed variable;
    // it now evaluates an arbitrary base once into a scratch slot, matching the
    // interpreter. Nested update in an override (`update p { x: update q ... }`)
    // exercises the level-scoped scratch reuse.
    #[test]
    fn record_update_on_expression_base_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Line:
    from: Point
    to: Point

fn main(console: Console):
    let l = Line(Point(1, 2), Point(3, 4))
    let p2 = Point(x: 100, ..(l).from)
    print(console, __render((p2).x))
    print(console, __render((p2).y))
    let cond = true
    let p3 = Point(y: 99, ..(if cond: (l).from else: (l).to))
    print(console, __render((p3).x))
    print(console, __render((p3).y))
    let l2 = Line(from: Point(x: 7, ..(l).to), ..l)
    print(console, __render(((l2).from).x))
    print(console, __render(((l2).from).y))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "record update on expression base diverged");
        assert_eq!(run_on_wasm(src), vec!["100", "2", "1", "99", "7", "4"]);
    }

    // Iterating records produced by a non-variable expression: a call returning
    // `List(Record)` and a list literal of records. Codegen now resolves the
    // loop variable's record type (so `p.x` works in the body) for any list
    // expression, not just a bare variable — matching the interpreter.
    #[test]
    fn for_over_nonvar_record_list_backends_agree() {
        let src = r#"
type P:
    x: Int
    y: Int

fn mk() -> List(P):
    [P(1, 2), P(3, 4), P(5, 6)]

fn main(console: Console):
    for p in mk():
        print(console, __render(((p).x + (p).y)))
    for q in [P(10, 1), P(20, 2)]:
        print(console, __render((q).x))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "for over non-var record list diverged");
        assert_eq!(run_on_wasm(src), vec!["3", "7", "11", "10", "20"]);
    }

    #[test]
    fn function_pipeline_fold_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let inc = fn(x: Int): (x + 1)
    let dbl = fn(x: Int): (x * 2)
    let neg = fn(x: Int): (0 - x)
    let pipeline = [inc, dbl, neg]
    let result = list.fold(pipeline, 5, fn(acc: Int, f: fn(Int) -> Int): f(acc))
    print(console, __render(result))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "function-pipeline fold diverged");
        assert_eq!(compiled, vec!["-12"]);
    }

    // char_count returns Unicode scalars; string_length returns bytes. They
    // agree for ASCII and diverge for multi-byte UTF-8 ("café" is 4 chars, 5
    // bytes) — and both backends must compute each identically.
    #[test]
    fn char_count_vs_string_length_backends_agree() {
        let src = r#"
fn main(console: Console):
    print(console, __render(string.char_count("hello")))
    print(console, __render(string.length("hello")))
    print(console, __render(string.char_count("café")))
    print(console, __render(string.length("café")))
    print(console, __render(string.char_count("")))
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
import string
fn main(console: Console):
    print(console, string.substring("café", 0, 3))
    print(console, string.substring("café", 3, 4))
    print(console, __render(string.length("a😀b")))
    print(console, __render(string.char_count("a😀b")))
    print(console, string.substring("a😀b", 1, 2))
    print(console, string.substring("a😀b", 0, 2))
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
import string

fn main(console: Console):
    print(console, string.take("hello", 3))
    print(console, (("[" + string.take("hi", 10)) + "]"))
    print(console, (("[" + string.take("hi", 0)) + "]"))
    print(console, string.drop("hello", 2))
    print(console, (("[" + string.drop("hi", 5)) + "]"))
    print(console, string.take("café", 2))
    print(console, string.drop("café", 3))
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
import string

fn main(console: Console):
    print(console, string.reverse("hello"))
    print(console, (("[" + string.reverse("")) + "]"))
    print(console, string.reverse("a"))
    print(console, string.reverse("café"))
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
import string

fn main(console: Console):
    print(console, string.replace_first("a.b.c", ".", "/"))
    print(console, string.replace_first("hello", "l", "L"))
    print(console, string.replace_first("xyz", "q", "Q"))
    print(console, string.replace_first("aa", "a", "bb"))
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
import string

fn main(console: Console):
    let (k, v) = string.split_once("name=witchy", "=")
    print(console, k)
    print(console, v)
    let (a, b) = string.split_once("no-sep-here", "=")
    print(console, a)
    print(console, (("[" + b) + "]"))
    let (h, rest) = string.split_once("a=b=c", "=")
    print(console, h)
    print(console, rest)
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
import string

fn main(console: Console):
    let ws = string.words("the  quick\tbrown\nfox ")
    print(console, __render(list.length(ws)))
    for w in ws:
        print(console, w)
    print(console, __render(list.length(string.words("   "))))
    print(console, __render(list.length(string.words(""))))
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
import string

fn main(console: Console):
    let cs = string.chars("café")
    print(console, __render(list.length(cs)))
    for c in cs:
        print(console, c)
    print(console, __render(list.length(string.chars(""))))
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
import string

fn main(console: Console):
    print(console, __render(string.is_empty("")))
    print(console, __render(string.is_empty("x")))
    print(console, __render(string.count("banana", "a")))
    print(console, __render(string.count("banana", "an")))
    print(console, __render(string.count("aaaa", "aa")))
    print(console, __render(string.count("abc", "x")))
    print(console, __render(string.count("abc", "")))
    print(console, __render(string.count("aéaéa", "éa")))
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
        // char_at returns the single character at an index, or "" out of range.
        let client = r#"
import string

fn main(console: Console):
    print(console, string.char_at("witchy", 0))
    print(console, string.char_at("witchy", 5))
    print(console, (("[" + string.char_at("witchy", 10)) + "]"))
    print(console, (("[" + string.char_at("", 0)) + "]"))
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
fn put(d: Dict(String, Int), k: String, v: Int) -> Dict(String, Int):
    dict.insert(d, k, v)

fn lookup(d: Dict(String, Int), k: String) -> Int:
    dict.get_or(d, k, (0 - 1))

fn main(console: Console):
    var d = dict.new()
    d = put(d, "apple", 1)
    d = put(d, "banana", 2)
    print(console, __render(lookup(d, ("ap" + "ple"))))
    print(console, __render(lookup(d, "banana")))
    print(console, __render(lookup(d, "cherry")))
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
    print(console, __render(size))
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
    print(console, __render(list.sum(list.range(5))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "auto-resolved std import diverged");
        assert_eq!(compiled, vec!["10"]);
    }

    // The composable, total lookups: list.head/last/get/find return Option
    // (None instead of an out-of-bounds trap). `list` imports `option`, and the
    // caller provides only `main` — the linker auto-resolves both std modules.
    // More total Option-returning list functions: min/max (None for the empty
    // list) and position (the Option counterpart to index_of's -1 sentinel).
    // result -> option conversions (result imports option): `ok` keeps the Ok
    // value as Some and drops an Err to None; `err` does the reverse. Caller
    // provides only `main`; the linker resolves result and option.
    // option -> result conversions (option imports result, completing the
    // Option<->Result pair; the linker flattens the cyclic import). ok_or maps
    // Some to Ok and None to Err(err); ok_or_else computes the error lazily.
    #[test]
    fn std_option_to_result_backends_agree() {
        let client = r#"
import option
import result

fn main(console: Console):
    print(console, __render(result.unwrap_or(option.ok_or(Some(5), "none"), 0)))
    print(console, __render(result.is_err(option.ok_or(None, "none"))))
    print(console, __render(result.unwrap_or(option.ok_or_else(Some(9), fn(): "none"), 0)))
    print(console, __render(result.is_err(option.ok_or_else(None, fn(): "none"))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option->result diverged");
        assert_eq!(compiled, vec!["5", "true", "9", "true"]);
    }

    // result.flatten collapses Result(Result(a, e), e) one level (Ok(Ok(v)) ->
    // Ok(v); Ok(Err) and Err -> Err), mirroring option.flatten. Both backends agree.
    #[test]
    fn std_result_flatten_backends_agree() {
        let client = r#"
import result

fn nested(n: Int) -> Result(Result(Int, String), String):
    if (n > 0):
        Ok(Ok(n))
    else:
        Ok(Err("inner"))

fn main(console: Console):
    print(console, __render(result.unwrap_or(result.flatten(nested(5)), 0)))
    print(console, __render(result.is_err(result.flatten(nested(0)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result flatten diverged");
        assert_eq!(compiled, vec!["5", "true"]);
    }

    #[test]
    fn std_result_to_option_backends_agree() {
        let client = r#"
import result
import option

fn check(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    print(console, __render(option.unwrap_or(result.ok(check(5)), 0)))
    print(console, __render(option.is_none(result.ok(check((0 - 1))))))
    print(console, __render(option.is_none(result.err(check(5)))))
    print(console, __render(string.length(option.unwrap_or(result.err(check((0 - 1))), ""))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result->option diverged");
        assert_eq!(compiled, vec!["5", "true", "true", "3"]);
    }

    // sort (ascending Int convenience over sort_by) and unique (drop duplicates,
    // keeping the first occurrence in order). Both backends agree.
    #[test]
    fn std_list_sort_unique_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let s = list.sort([3, 1, 4, 1, 5, 9, 2, 6])
    for x in s:
        print(console, __render(x))
    let u = list.unique([1, 2, 2, 3, 1, 4, 3])
    print(console, __render(list.length(u)))
    for x in u:
        print(console, __render(x))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "sort/unique diverged");
        assert_eq!(
            compiled,
            vec!["1", "1", "2", "3", "4", "5", "6", "9", "4", "1", "2", "3", "4"]
        );
    }

    // max_by/min_by generalize min/max to any type via a comparator, returning
    // Option. The second comparator (`(0-a) < (0-b)`, i.e. larger magnitude is
    // "less") shows the result tracks the supplied ordering, not the natural one.
    // A variable bound to a record-typed constructor field in a match pattern
    // (`Circle(c)`) now resolves field access in the arm body (`c.x`). Codegen
    // previously rejected this; it's fixed for concrete (non-generic) field
    // types. Both backends agree.
    // Matching the Some of a function-returned Option(Record) binds the payload
    // to its record type, so `a.balance` resolves. Codegen learns the payload
    // record from the function's declared `-> Option(Account)` return.
    // Let-bound intermediates inherit derived types: `let o = lookup()` carries
    // the Option(Account) payload (so a later `match o { Some(a) -> a.balance }`
    // resolves), and `let xs = mk()` carries the List(P) element type (so
    // `for p in xs { p.x }` resolves). Both backends agree.
    #[test]
    fn let_bound_derived_types_backends_agree() {
        let client = r#"
import option

type Account:
    id: Int
    balance: Int

type P:
    x: Int
    y: Int

fn lookup(n: Int) -> Option(Account):
    if (n > 0):
        Some(Account(n, (n * 100)))
    else:
        None

fn mk() -> List(P):
    [P(1, 2), P(3, 4)]

fn main(console: Console):
    let o = lookup(7)
    match o:
        Some(a) -> print(console, __render((a).balance))
        None -> print(console, "none")
    let xs = mk()
    for p in xs:
        print(console, __render((p).x))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "let-bound derived types diverged");
        assert_eq!(compiled, vec!["700", "1", "3"]);
    }

    // The generic stdlib case: `list.find` etc. have shape `fn(List(a),..) ->
    // Option(a)`, so matching their result binds the payload to the list's
    // element record type. `acc.field` now resolves through a generic lookup.
    // Generic `fn(List(a),..) -> List(a)` results (filter/reverse/...) carry the
    // argument's element record type, so iterating them resolves field access:
    // `for p in list.filter(records, pred) { p.field }`.
    // map's result element type is the mapper's return type, so iterating a
    // `list.map(records, fn(r){ OtherRecord(..) })` resolves field access on the
    // mapped records (a different record type than the input).
    // End-to-end: records flow through the whole stdlib pipeline with correct
    // field resolution — fold over records, max_by/find returning Option(record)
    // (match payload reads fields), filter then iterate (loop var reads fields),
    // a helper function over a record, and first-class lambdas throughout.
    // The `?` operator unwrapping a Result(Record): `let acc = lookup(n)?` binds
    // acc to the payload record so `acc.balance` resolves, and an Err short-
    // circuits the enclosing Result-returning function. Both backends agree.
    #[test]
    fn try_operator_record_payload_backends_agree() {
        let client = r#"
import result

type Account:
    id: Int
    balance: Int

fn lookup(n: Int) -> Result(Account, String):
    if (n > 0):
        Ok(Account(n, (n * 100)))
    else:
        Err("bad")

fn process(n: Int) -> Result(Int, String):
    let acc = (lookup(n))?
    Ok(((acc).balance + 1))

fn main(console: Console):
    match process(5):
        Ok(v) -> print(console, __render(v))
        Err(e) -> print(console, e)
    match process((0 - 1)):
        Ok(v) -> print(console, __render(v))
        Err(e) -> print(console, e)
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "? with Result(Record) diverged");
        assert_eq!(compiled, vec!["501", "bad"]);
    }

    // Integration showcase: a recursive JSON-value renderer. Exercises a
    // recursive ADT (JArr holds List(Json)), every match arm form, recursion,
    // list.map with a *named function* argument (function-as-value), and
    // list.join — all composing. Both backends agree.
    #[test]
    fn json_renderer_integration_backends_agree() {
        let client = r#"
import list
import string

type Json:
    JNull
    JBool(Bool)
    JNum(Int)
    JStr(String)
    JArr(List(Json))

fn render(j: Json) -> String:
    match j:
        JNull -> "null"
        JBool(b) -> if b: "true" else: "false"
        JNum(n) -> __render(n)
        JStr(s) -> (("\"" + s) + "\"")
        JArr(items) -> (("[" + list.join(list.map(items, render), ",")) + "]")

fn main(console: Console):
    let doc = JArr([JNum(1), JStr("hi"), JBool(true), JNull, JArr([JNum(2), JNum(3)])])
    print(console, render(doc))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "json renderer diverged");
        assert_eq!(compiled, vec!["[1,\"hi\",true,null,[2,3]]"]);
    }

    #[test]
    fn order_processing_integration_backends_agree() {
        let client = r#"
import list
import option

type Item:
    name: String
    price: Int
    qty: Int

fn line_total(it: Item) -> Int:
    ((it).price * (it).qty)

fn main(console: Console):
    let cart = [Item("apple", 50, 3), Item("bread", 200, 1), Item("milk", 150, 2)]
    let total = list.fold(cart, 0, fn(acc: Int, it: Item): (acc + line_total(it)))
    print(console, __render(total))
    match list.max_by(cart, fn(a: Item, b: Item): (line_total(a) < line_total(b))):
        Some(it) -> print(console, (it).name)
        None -> print(console, "none")
    let multi = list.filter(cart, fn(it: Item): ((it).qty > 1))
    for it in multi:
        print(console, (it).name)
    match list.find(cart, fn(it: Item): ((it).name == "bread")):
        Some(it) -> print(console, __render((it).price))
        None -> print(console, "0")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "order processing diverged");
        assert_eq!(compiled, vec!["650", "milk", "apple", "milk", "200"]);
    }

    #[test]
    fn iterate_map_result_records_backends_agree() {
        let client = r#"
import list

type Raw:
    a: Int
    b: Int

type Point:
    x: Int
    y: Int

fn main(console: Console):
    let raws = [Raw(1, 2), Raw(3, 4)]
    let pts = list.map(raws, fn(r: Raw): Point(((r).a + (r).b), ((r).a * (r).b)))
    for p in pts:
        print(console, __render((p).x))
    for p in list.map(raws, fn(r: Raw): Point((r).b, (r).a)):
        print(console, __render((p).y))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "iterate map result diverged");
        assert_eq!(compiled, vec!["3", "7", "1", "3"]);
    }

    #[test]
    fn iterate_generic_list_result_records_backends_agree() {
        let client = r#"
import list

type P:
    x: Int
    y: Int

fn main(console: Console):
    let ps = [P(1, 10), P(2, 20), P(3, 30)]
    let evens = list.filter(ps, fn(p: P): (((p).x % 2) == 0))
    for p in evens:
        print(console, __render((p).y))
    for p in list.reverse(ps):
        print(console, __render((p).x))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "iterate generic list result diverged");
        assert_eq!(compiled, vec!["20", "3", "2", "1"]);
    }

    #[test]
    fn match_generic_list_lookup_payload_backends_agree() {
        let client = r#"
import list
import option

type Account:
    id: Int
    balance: Int

fn main(console: Console):
    let accounts = [Account(1, 100), Account(2, 200), Account(3, 300)]
    match list.find(accounts, fn(a: Account): ((a).balance > 150)):
        Some(acc) -> print(console, __render((acc).balance))
        None -> print(console, "none")
    match list.head(accounts):
        Some(acc) -> print(console, __render((acc).id))
        None -> print(console, "none")
    match list.find(accounts, fn(a: Account): ((a).balance > 999)):
        Some(acc) -> print(console, __render((acc).id))
        None -> print(console, "none")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic list lookup payload diverged");
        assert_eq!(compiled, vec!["200", "1", "none"]);
    }

    #[test]
    fn match_option_record_payload_backends_agree() {
        let client = r#"
import option

type Account:
    id: Int
    balance: Int

fn lookup(n: Int) -> Option(Account):
    if (n > 0):
        Some(Account(n, (n * 100)))
    else:
        None

fn main(console: Console):
    match lookup(5):
        Some(a) -> print(console, __render((a).balance))
        None -> print(console, "none")
    match lookup((0 - 1)):
        Some(a) -> print(console, __render((a).balance))
        None -> print(console, "none")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "Option(Record) match diverged");
        assert_eq!(compiled, vec!["500", "none"]);
    }

    // Nested constructor patterns destructure through a record: `Circle(Point(x,
    // y))` binds x and y from the inner Point in one pattern. Both backends agree.
    #[test]
    fn nested_constructor_pattern_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Shape:
    Circle(Point)
    Origin

fn f(s: Shape) -> Int:
    match s:
        Circle(Point(x, y)) -> (x + y)
        Origin -> 0

fn main(console: Console):
    print(console, __render(f(Circle(Point(3, 4)))))
    print(console, __render(f(Circle(Point(10, 1)))))
    print(console, __render(f(Origin)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "nested constructor pattern diverged");
        assert_eq!(run_on_wasm(src), vec!["7", "11", "0"]);
    }

    #[test]
    fn match_binds_record_field_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Shape:
    Circle(Point)
    Rect(Int, Int)

fn describe(s: Shape) -> Int:
    match s:
        Circle(c) -> ((c).x + (c).y)
        Rect(w, h) -> (w * h)

fn main(console: Console):
    print(console, __render(describe(Circle(Point(3, 4)))))
    print(console, __render(describe(Rect(5, 6))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "match record-field bind diverged");
        assert_eq!(run_on_wasm(src), vec!["7", "30"]);
    }

    // find_map searches and transforms in one pass: the first non-None result
    // of f, or None. Here it returns half of the first even number.
    // reduce folds with the first element as the seed (Option-returning, None
    // for empty) — here used as max and sum without an explicit initial value.
    #[test]
    fn std_list_reduce_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    let mx = list.reduce([3, 1, 4, 1, 5], fn(a: Int, b: Int): if (a > b): a else: b)
    print(console, __render(option.unwrap_or(mx, 0)))
    print(console, __render(option.is_none(list.reduce([], fn(a: Int, b: Int): (a + b)))))
    let sum = list.reduce([10, 20, 30], fn(a: Int, b: Int): (a + b))
    print(console, __render(option.unwrap_or(sum, 0)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "reduce diverged");
        assert_eq!(compiled, vec!["5", "true", "60"]);
    }

    #[test]
    fn std_list_find_map_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    let r = list.find_map([3, 5, 8, 10], fn(x: Int): if ((x % 2) == 0): Some((x / 2)) else: None)
    print(console, __render(option.unwrap_or(r, (0 - 1))))
    let none = list.find_map([1, 3, 5], fn(x: Int): if (x > 100): Some(x) else: None)
    print(console, __render(option.is_none(none)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "find_map diverged");
        assert_eq!(compiled, vec!["4", "true"]);
    }

    #[test]
    fn std_list_min_max_by_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    let xs = [3, 1, 4, 1, 5, 9, 2]
    print(console, __render(option.unwrap_or(list.max_by(xs, fn(a: Int, b: Int): (a < b)), 0)))
    print(console, __render(option.unwrap_or(list.min_by(xs, fn(a: Int, b: Int): (a < b)), 0)))
    print(console, __render(option.unwrap_or(list.max_by(xs, fn(a: Int, b: Int): ((0 - a) < (0 - b))), 0)))
    print(console, __render(option.is_none(list.max_by([], fn(a: Int, b: Int): (a < b)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "min_by/max_by diverged");
        assert_eq!(compiled, vec!["9", "1", "1", "true"]);
    }

    #[test]
    fn std_list_min_max_position_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    print(console, __render(option.unwrap_or(list.min([3, 1, 4, 1, 5]), 0)))
    print(console, __render(option.unwrap_or(list.max([3, 1, 4, 1, 5]), 0)))
    print(console, __render(option.is_none(list.min([]))))
    print(console, __render(option.unwrap_or(list.position([10, 20, 30], 20), (0 - 1))))
    print(console, __render(option.is_none(list.position([10, 20], 99))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "list min/max/position diverged");
        assert_eq!(compiled, vec!["1", "5", "true", "1", "true"]);
    }

    #[test]
    fn std_list_option_lookups_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    print(console, __render(option.unwrap_or(list.head([10, 20]), 0)))
    print(console, __render(option.unwrap_or(list.head([]), (0 - 1))))
    print(console, __render(option.unwrap_or(list.last([10, 20]), 0)))
    print(console, __render(option.unwrap_or(list.get([10, 20, 30], 1), 0)))
    print(console, __render(option.unwrap_or(list.get([10], 5), (0 - 1))))
    print(console, __render(option.unwrap_or(list.find([1, 3, 4], fn(n: Int): ((n % 2) == 0)), (0 - 1))))
    print(console, __render(option.is_none(list.find([1, 3, 5], fn(n: Int): ((n % 2) == 0)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "list option lookups diverged");
        assert_eq!(compiled, vec!["10", "-1", "20", "20", "-1", "4", "true"]);
    }

    #[test]
    fn std_list_head_last_find_or_backends_agree() {
        // Total accessors: head_or/last_or return a default for the empty list
        // (never indexing out of bounds), and find_or returns the first match or
        // a default. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    print(console, __render(list.head_or([10, 20, 30], 0)))
    print(console, __render(list.head_or([], (0 - 1))))
    print(console, __render(list.last_or([10, 20, 30], 0)))
    print(console, __render(list.last_or([], (0 - 1))))
    print(console, __render(list.find_or([1, 3, 4, 7], fn(n: Int): ((n % 2) == 0), (0 - 1))))
    print(console, __render(list.find_or([1, 3, 5], fn(n: Int): ((n % 2) == 0), (0 - 1))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "head_or/last_or/find_or diverged");
        assert_eq!(compiled, vec!["10", "-1", "30", "-1", "4", "-1"]);
    }

    // windows: sliding sublists of length n (step 1), empty when n exceeds the
    // list or n < 1. Complements chunks. Iterating List(List(Int)) too.
    #[test]
    fn std_list_windows_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let ws = list.windows([1, 2, 3, 4], 2)
    print(console, __render(list.length(ws)))
    for w in ws:
        print(console, __render(list.sum(w)))
    print(console, __render(list.length(list.windows([1, 2], 5))))
    print(console, __render(list.length(list.windows([1, 2, 3], 0))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "windows diverged");
        assert_eq!(compiled, vec!["3", "3", "5", "7", "0", "0"]);
    }

    // split_at splits a list into (first n, the rest); n is clamped at both
    // ends. The list analogue of string.split_once. Both backends agree.
    #[test]
    fn std_list_split_at_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let (a, b) = list.split_at([1, 2, 3, 4, 5], 2)
    print(console, __render(list.sum(a)))
    print(console, __render(list.sum(b)))
    let (c, d) = list.split_at([1, 2], 5)
    print(console, __render(list.sum(c)))
    print(console, __render(list.length(d)))
    let (e, f) = list.split_at([1, 2, 3], 0)
    print(console, __render(list.length(e)))
    print(console, __render(list.sum(f)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "split_at diverged");
        assert_eq!(compiled, vec!["3", "12", "3", "0", "0", "6"]);
    }

    #[test]
    fn std_list_chunks_tail_init_backends_agree() {
        // chunks groups into fixed-size sublists (last may be short), tail drops
        // the first element, init drops the last — all total (empty stays empty).
        // Iterating List(List(Int)) also exercises nested lists across backends.
        let client = r#"
import list

fn main(console: Console):
    let cs = list.chunks([1, 2, 3, 4, 5], 2)
    print(console, __render(list.length(cs)))
    for c in cs:
        print(console, __render(list.sum(c)))
    print(console, __render(list.sum(list.tail([1, 2, 3]))))
    print(console, __render(list.sum(list.drop_last([1, 2, 3]))))
    print(console, __render(list.length(list.tail([]))))
    print(console, __render(list.length(list.drop_last([]))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "chunks/tail/init diverged");
        assert_eq!(compiled, vec!["3", "3", "7", "5", "5", "3", "0", "0"]);
    }

    // sum_by totals a projection of each element (0 for empty) — including a
    // record field via a record-typed lambda parameter.
    #[test]
    fn std_list_sum_by_backends_agree() {
        let client = r#"
import list

type Item:
    price: Int
    qty: Int

fn main(console: Console):
    let cart = [Item(50, 3), Item(200, 1), Item(150, 2)]
    print(console, __render(list.sum_by(cart, fn(it: Item): ((it).price * (it).qty))))
    print(console, __render(list.sum_by([1, 2, 3, 4], fn(n: Int): (n * n))))
    print(console, __render(list.sum_by([], fn(n: Int): n)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "sum_by diverged");
        assert_eq!(compiled, vec!["650", "30", "0"]);
    }

    #[test]
    fn std_list_product_slice_scan_backends_agree() {
        // product (1 for empty), slice (clamped half-open range), and scan
        // (running fold collecting intermediates) all agree across backends.
        let client = r#"
import list

fn main(console: Console):
    print(console, __render(list.product([1, 2, 3, 4])))
    print(console, __render(list.product([])))
    let s = list.slice([10, 20, 30, 40, 50], 1, 4)
    for x in s:
        print(console, __render(x))
    let running = list.scan([1, 2, 3], 0, fn(acc: Int, n: Int): (acc + n))
    for x in running:
        print(console, __render(x))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "product/slice/scan diverged");
        assert_eq!(compiled, vec!["24", "1", "20", "30", "40", "0", "1", "3", "6"]);
    }

    #[test]
    fn std_func_combinators_backends_agree() {
        // The whole `func` module links + compiles, and its combinators — built
        // on first-class functions — agree across backends: compose threads
        // named functions, flip swaps a subtraction's operands, constant
        // ignores its argument, identity is a no-op.
        let client = r#"
import func

fn double(x: Int) -> Int:
    (x * 2)

fn inc(x: Int) -> Int:
    (x + 1)

fn sub(a: Int, b: Int) -> Int:
    (a - b)

fn main(console: Console):
    let h = func.compose(double, inc)
    print(console, __render(h(10)))
    print(console, __render((func.flip(sub))(3, 10)))
    print(console, __render((func.constant(42))(999)))
    print(console, __render(func.identity(7)))
"#;
        let sources = [("func", crate::bundled_module("func").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "func combinators diverged");
        assert_eq!(compiled, vec!["22", "7", "42", "7"]);
    }

    // A closure that *calls* a captured function-valued variable (`f(g(x))`,
    // where f and g are captured) must thread f and g through the closure
    // environment and invoke them indirectly — not emit a direct `call $g`.
    // This is the classic `compose`; it must agree across backends.
    #[test]
    fn compose_captured_functions_backends_agree() {
        let src = r#"
fn compose(f: fn(Int) -> Int, g: fn(Int) -> Int) -> fn(Int) -> Int:
    fn(x: Int): f(g(x))

fn double(x: Int) -> Int:
    (x * 2)

fn inc(x: Int) -> Int:
    (x + 1)

fn main(console: Console):
    let h = compose(double, inc)
    print(console, __render(h(10)))
    print(console, __render((compose(inc, double))(10)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "compose diverged");
        assert_eq!(run_on_wasm(src), vec!["22", "21"]);
    }

    #[test]
    fn function_by_name_as_value_backends_agree() {
        // A bare top-level function name is a first-class value: bind it, call
        // it, and apply it repeatedly. Both backends materialize it as a
        // callable closure.
        let src = r#"
fn double(x: Int) -> Int:
    (x * 2)

fn inc(x: Int) -> Int:
    (x + 1)

fn main(console: Console):
    let f = double
    print(console, __render(f(5)))
    let g = inc
    print(console, __render(g(g(g(0)))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "function-as-value diverged");
        assert_eq!(run_on_wasm(src), vec!["10", "3"]);
    }

    #[test]
    fn named_function_passed_to_map_backends_agree() {
        // Point-free style: pass a named function (not a lambda) straight to a
        // higher-order std function. Exercises the linker qualifying a bare
        // function-name reference and codegen forwarding through a closure.
        let client = r#"
import list

fn triple(x: Int) -> Int:
    (x * 3)

fn main(console: Console):
    let ys = list.map([1, 2, 3], triple)
    for y in ys:
        print(console, __render(y))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "named-function-to-map diverged");
        assert_eq!(compiled, vec!["3", "6", "9"]);
    }

    #[test]
    fn immediate_application_backends_agree() {
        let src = r#"
fn twice(f: fn(Int) -> Int, x: Int) -> Int:
    f(f(x))

fn main(console: Console):
    let make_adder = fn(x: Int): fn(y: Int): (x + y)
    let make_mul = fn(a: Int): fn(b: Int): fn(c: Int): ((a * b) * c)
    print(console, __render((make_adder(10))(5)))
    print(console, __render(((make_mul(2))(3))(4)))
    print(console, __render((fn(n: Int): (n * n))(7)))
    print(console, __render(twice(make_adder(1), 10)))
    print(console, __render((make_adder(10))((make_adder(2))(3))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "immediate application diverged");
        assert_eq!(run_on_wasm(src), vec!["15", "24", "49", "12", "15"]);
    }

    #[test]
    fn closures_and_string_ordering_backends_agree() {
        let src = r#"
fn main(console: Console):
    let base = 10
    let add = fn(n: Int): (n + base)
    var total = 0
    var i = 0
    while (i < 5):
        total = (total + add(i))
        i = (i + 1)
    print(console, __render(total))
    let make_adder = fn(x: Int): fn(y: Int): (x + y)
    let add3 = make_adder(3)
    print(console, __render(add3(4)))
    print(console, __render((make_adder(100))(1)))
    if ("abc" < "abcd"):
        print(console, "lt1")
    else:
        print(console, "ge1")
    if ("Z" < "a"):
        print(console, "lt2")
    else:
        print(console, "ge2")
    if ("" < "a"):
        print(console, "lt3")
    else:
        print(console, "ge3")
    if ("apple" < "apply"):
        print(console, "lt4")
    else:
        print(console, "ge4")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "closures/ordering diverged");
    }

    #[test]
    fn string_edge_cases_backends_agree() {
        let src = r#"
fn main(console: Console):
    print(console, __render(list.length(string.split("abc", ""))))
    print(console, __render(list.length(string.split("abc", "x"))))
    print(console, __render(list.length(string.split("a,b,c", ","))))
    print(console, (("[" + string.substring("", 0, 5)) + "]"))
    print(console, (("[" + string.substring("hello", 3, 1)) + "]"))
    print(console, string.substring("hello", 2, 100))
    print(console, __render(string.index_of("hello", "")))
    print(console, __render(string.index_of("hello", "z")))
    print(console, (("[" + (("" + "x") + "")) + "]"))
    print(console, __render(string.length("")))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "string edge cases diverged");
    }

    #[test]
    fn trim_backends_agree() {
        // trim now compiles: leading/trailing ASCII whitespace (spaces, tabs,
        // newlines, CRs) is stripped; an all-whitespace string trims to "".
        let src = r#"
fn main(console: Console):
    print(console, string.trim("  hello  "))
    print(console, string.trim("\t\nfoo\r\n"))
    print(console, string.trim("nospaces"))
    print(console, string.trim("   "))
    print(console, __render(string.length(string.trim("  a b  "))))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["hello", "foo", "nospaces", "", "3"]);
    }

    // std/json get_path: follow a dotted key path into a decoded value (and a
    // missing path -> None). Pure, so both backends agree.
    #[test]
    fn std_json_get_path_backends_agree() {
        let client = r#"
import json
import option
fn str_at(j: Json, path: String) -> String:
    match json.get_path(j, path):
        Some(v) -> option.unwrap_or(json.as_string(v), "?")
        None -> "none"
fn int_at(j: Json, path: String) -> Int:
    match json.get_path(j, path):
        Some(v) -> option.unwrap_or(json.as_int(v), 0)
        None -> 0
fn main(console: Console):
    match json.decode("{\"user\":{\"name\":\"witchy\",\"age\":1},\"tags\":[\"a\"]}"):
        Ok(j) ->
            print(console, str_at(j, "user.name"))
            print(console, __render(int_at(j, "user.age")))
            print(console, str_at(j, "user.missing"))
        Err(e) -> print(console, e)
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std json get_path diverged");
        assert_eq!(compiled, vec!["witchy", "1", "none"]);
    }

    // Dead-code elimination: a program importing `list` but using only `map`
    // and `sum` must not compile the rest of the list API (or `option`, which
    // `list` imports) into the WASM — only functions reachable from `main`.
    #[test]
    fn dce_drops_unused_stdlib_functions() {
        let client = r#"
import list

fn main(console: Console):
    let xs = list.map([1, 2, 3], fn(x: Int): (x * 2))
    print(console, __render(list.sum(xs)))
"#;
        let mods = vec![("main".to_string(), parser::parse_module(client).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        // Reachable functions are present and the unused ones are dropped: the
        // binary path's `assemble_wir_module` runs the same `reachable_functions`
        // DCE, so inspect the assembled WIR func names directly.
        let wir = codegen::assemble_wir_module(&linked)
            .expect("assemble")
            .expect("the binary path lowers this program");
        // The binary path monomorphizes generics, so `list.map` appears as
        // `list.map__Int__Int`; match on the `list.<fn>` prefix.
        let names: Vec<&str> = wir.funcs.iter().map(|f| f.name.as_str()).collect();
        let has = |fn_name: &str| names.iter().any(|n| *n == fn_name || n.starts_with(&format!("{fn_name}__")));
        assert!(has("list.map"), "map should be compiled: {names:?}");
        assert!(has("list.sum"), "sum should be compiled: {names:?}");
        assert!(!has("list.partition"), "partition should be eliminated: {names:?}");
        assert!(!has("list.windows"), "windows should be eliminated: {names:?}");
        assert!(!has("list.sort_by"), "sort_by should be eliminated: {names:?}");
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
        Ok(u) -> url.scheme(u) + " " + url.host(u) + " " + __render(url.port(u)) + " " + url.path(u)
        Err(e) -> "invalid: " + e
fn main(console: Console):
    print(console, describe("http://example.com"))
    print(console, describe("http://example.com:8080/foo"))
    print(console, describe("https://x.com/a/b"))
    print(console, describe("notaurl"))
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
    #[test]
    fn std_http_get_url_against_loopback() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = Vec::new();
                let mut tmp = [0u8; 256];
                while let Ok(n) = stream.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&tmp[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nhello-url";
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    match http.get_url(net, "http://127.0.0.1:{port}/path"):
        Ok(r) -> print(console, http.body(r))
        Err(e) -> print(console, e)
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["hello-url"]);
    }

    // std/string trimming: trim/trim_start/trim_end over assorted whitespace.
    // Pure, so both backends agree.
    #[test]
    fn std_string_trim_backends_agree() {
        let client = r#"
import string
fn main(console: Console):
    print(console, "[" + string.trim("  hello  ") + "]")
    print(console, "[" + string.trim_start("  hi") + "]")
    print(console, "[" + string.trim_end("bye  ") + "]")
    print(console, "[" + string.trim("\t\n x \r\n") + "]")
    print(console, "[" + string.trim("nospace") + "]")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std string trim diverged");
        assert_eq!(compiled, vec!["[hello]", "[hi]", "[bye]", "[x]", "[nospace]"]);
    }

    // Traits: an `impl` provides a method per type, and a trait-method call
    // resolves to the impl for the receiver's concrete type — at a literal
    // receiver, a `let`-bound one, and across two implementing types. The trait
    // is lowered to ordinary functions, so both backends agree.
    #[test]
    fn traits_concrete_dispatch_backends_agree() {
        let src = r#"
trait Show:
    fn show(self) -> String

impl Show for Int:
    fn show(self) -> String:
        __render(self)

impl Show for Bool:
    fn show(self) -> String:
        if self:
            "yes"
        else:
            "no"

fn main(console: Console):
    print(console, show(42))
    print(console, show(true))
    let n = 7
    print(console, show(n))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "trait dispatch diverged");
        assert_eq!(run_on_wasm(src), vec!["42", "yes", "7"]);
    }

    // Phase 2 of the concurrency redesign: an `async fn` lowers (CPS over closures,
    // `crate::async_lower`) to a cooperative `chan` task, and `await` chains
    // continuations. An async `main` is the executor entry (lowers to `task.run`).
    // The lowering is ordinary closures + calls, so both backends agree.
    #[test]
    fn async_await_lowers_and_runs_backends_agree() {
        let src = r#"
async fn double(n: Int) -> Int:
    n + n

async fn pipeline(seed: Int) -> Int:
    let a = double(seed).await
    let b = double(a).await
    a + b

async fn main(console: Console):
    let r = pipeline(3).await
    print(console, "${r}")
    let d = double(10).await
    print(console, "${d}")
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "async lowering diverged across backends");
        // pipeline(3): a=6, b=12, a+b=18.  double(10)=20.
        assert_eq!(interp_out, vec!["18", "20"]);
    }

    // The headline of the unification: `async`/`await` and channels are ONE
    // substrate. A producer and a consumer, both written as straight-line
    // `async fn`s using `await chan.send`/`await chan.recv`, run concurrently under
    // `task.run`. The consumer loops on `recv` (recursively) — this is the actor
    // idiom, now ergonomic — and the schedule is byte-identical on both backends.
    #[test]
    fn async_with_channels_backends_agree() {
        let src = r#"
import chan

async fn producer(tx: Sender(Int)) -> Nil:
    chan.send(tx, 1).await
    chan.send(tx, 2).await

async fn consumer(console: Console, rx: Receiver(Int)) -> Nil:
    chan.consume(rx, fn(v): task.done(print(console, "got ${v}"))).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    task.spawn(producer(tx)).await
    consumer(console, rx).await
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "async+channel schedule diverged across backends");
        assert_eq!(interp_out, vec!["got 1", "got 2"]);
    }

    // `await` inside a `for` loop — over a list (producer) and a range (consumer)
    // — lowers to a sequential `task.for_each`, so iterating with `await` needs no
    // hand-written recursion. Both backends must agree, byte-for-byte.
    #[test]
    fn for_await_loop_backends_agree() {
        let src = r#"
import chan

async fn producer(tx: Sender(Int)) -> Nil:
    for x in [1, 2, 3]:
        chan.send(tx, x).await

async fn consumer(console: Console, rx: Receiver(Int)) -> Nil:
    for _i in 0..3:
        let o = chan.recv(rx).await
        match o:
            Some(v) -> print(console, "got ${v}")
            None -> print(console, "closed")

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    task.spawn(producer(tx)).await
    consumer(console, rx).await
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "for-await schedule diverged across backends");
        assert_eq!(interp_out, vec!["got 1", "got 2", "got 3"]);
    }

    // `for await x in rx:` — a receive loop over a channel whose body may itself
    // `await` (here it forwards a squared value). Lowers to chan.consume; both
    // backends agree byte-for-byte.
    #[test]
    fn for_await_over_receiver_backends_agree() {
        let src = r#"
import chan

async fn producer(tx: Sender(Int)) -> Nil:
    for n in [1, 2, 3]:
        chan.send(tx, n).await

async fn relay(rx: Receiver(Int), out: Sender(Int)) -> Nil:
    for await x in rx:
        chan.send(out, x * x).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let (otx, orx) = chan.channel(4).await
    task.spawn(producer(tx)).await
    task.spawn(relay(rx, otx)).await
    chan.consume(orx, fn(v): task.done(print(console, "got ${v}"))).await
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "for-await-over-receiver diverged across backends");
        assert_eq!(interp_out, vec!["got 1", "got 4", "got 9"]);
    }

    // The multi-actor case: each task has its OWN inbox, so several actors with
    // separate mailboxes run together (what a single shared channel cannot do).
    // A logger (#0), a forwarder (#1) that relays to the logger, and a driver (#2)
    // that messages both — `send(target, msg)` routes by actor index. This is the
    // shape `examples/actors/src/actors.witchy` (Logger + Forwarder) needs, now in async/chan,
    // byte-identical on both backends.
    #[test]
    fn chan_multi_actor_separate_inboxes_backends_agree() {
        let src = r#"
import chan

async fn logger(console: Console, rx: Receiver(Int)) -> Nil:
    chan.consume(rx, fn(a): task.done(print(console, "log ${a}"))).await

async fn forwarder(rx: Receiver(Int), log_tx: Sender(Int)) -> Nil:
    chan.consume(rx, fn(m): chan.send(log_tx, m)).await

async fn driver(log_tx: Sender(Int), fwd_tx: Sender(Int)) -> Nil:
    chan.send(log_tx, 100).await
    chan.send(fwd_tx, 200).await

async fn main(console: Console):
    let (log_tx, log_rx) = chan.channel(4).await
    let (fwd_tx, fwd_rx) = chan.channel(4).await
    let lh = task.spawn(logger(console, log_rx)).await
    let fh = task.spawn(forwarder(fwd_rx, log_tx)).await
    driver(log_tx, fwd_tx).await
    task.join(fh).await
    task.join(lh).await
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "multi-actor schedule diverged across backends");
        assert_eq!(interp_out, vec!["log 100", "log 200"]);
    }

    // Phase 4 of the concurrency redesign: channels. `std/chan` is a cooperative
    // message-passing executor written in pure witchy via an effect protocol
    // (a task yields `Emit`/`Recv` requests; the executor owns the one FIFO buffer
    // and threads it through the schedule — no shared mutable state, no runtime
    // primitive). A producer sends, a consumer loops on `recv` (the actor idiom),
    // and the run is byte-identical on both backends.
    #[test]
    fn chan_producer_consumer_backends_agree() {
        let src = r#"
import chan

async fn producer(tx: Sender(Int)) -> Nil:
    chan.send(tx, 1).await
    chan.send(tx, 2).await

async fn consumer(console: Console, rx: Receiver(Int)) -> Nil:
    chan.consume(rx, fn(v): task.done(print(console, "got ${v}"))).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    task.spawn(producer(tx)).await
    consumer(console, rx).await
    print(console, "drained")
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "channel schedule diverged across backends");
        assert_eq!(interp_out, vec!["got 1", "got 2", "drained"]);
    }

    // The channel message type is GENERIC (here `String`), proving the explicit
    // type-parameter fix to the monomorphizer: a multi-param ADT whose constructor
    // omits a param (`Done(a)` for `Step(m, a)`) now keeps that param generic
    // because `type Step(m, a)` fixes the order. Byte-identical on both backends.
    #[test]
    fn chan_generic_message_type_backends_agree() {
        let src = r#"
import chan

async fn producer(tx: Sender(String)) -> Nil:
    chan.send(tx, "alice").await
    chan.send(tx, "bob").await

async fn consumer(console: Console, rx: Receiver(String)) -> Nil:
    chan.consume(rx, fn(name): task.done(print(console, "hello ${name}"))).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    task.spawn(producer(tx)).await
    consumer(console, rx).await
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "generic-message channel diverged across backends");
        assert_eq!(interp_out, vec!["hello alice", "hello bob"]);
    }

    // `chan.select` races two receivers, taking from whichever is ready (a tie
    // favours the first) and yielding `Closed` once neither can deliver. Both
    // backends must agree on the merged order.
    #[test]
    fn chan_select_backends_agree() {
        let src = r#"
import chan

async fn pa(tx: Sender(Int)) -> Nil:
    chan.send(tx, 1).await
    chan.send(tx, 2).await

async fn pb(tx: Sender(Int)) -> Nil:
    chan.send(tx, 9).await

async fn collector(console: Console, a: Receiver(Int), b: Receiver(Int)) -> Nil:
    let s = chan.select(a, b).await
    match s:
        First(x) ->
            print(console, "a ${x}")
            collector(console, a, b).await
        Second(y) ->
            print(console, "b ${y}")
            collector(console, a, b).await
        Closed -> print(console, "done")

async fn main(console: Console):
    let (atx, arx) = chan.channel(4).await
    let (btx, brx) = chan.channel(4).await
    task.spawn(pa(atx)).await
    task.spawn(pb(btx)).await
    collector(console, arx, brx).await
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "select schedule diverged across backends");
        assert_eq!(interp_out, vec!["a 1", "a 2", "b 9", "done"]);
    }

    // Phase 5 (racing): `future.select` drives tasks concurrently and returns the
    // first to finish, dropping the losers. Among tasks of length 5/2/8, the
    // index-1 task (length 2) wins first — deterministically on both backends.
    #[test]
    fn future_select_first_wins_backends_agree() {
        let src = r#"
import future

fn counter(label: Int, steps: Int) -> Future(Int):
    if steps <= 0:
        future.ready(label)
    else:
        future.and_then(future.pending(0), fn(_a): counter(label, steps - 1))

fn main(console: Console):
    let (idx, val) = future.select([counter(10, 5), counter(20, 2), counter(30, 8)])
    print(console, "winner ${idx} ${val}")
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "select diverged across backends");
        assert_eq!(interp_out, vec!["winner 1 20"]);
    }

    // The coloring rule: `await` is a parse error outside an `async fn`.
    #[test]
    fn await_outside_async_is_a_parse_error() {
        // `.await` is postfix and legal only inside an `async fn`.
        let src = "fn f():\n    let _x = (5).await\n";
        let err = parser::parse_module(src).expect_err("`.await` in a sync fn must not parse");
        assert!(
            format!("{err:?}").contains("async fn"),
            "error should name the async-fn rule: {err:?}"
        );
        // A leading `await` (the old prefix form) is no longer accepted at all.
        assert!(parser::parse_module("async fn main():\n    await f()\n").is_err());
    }

    // Phase 3 of the concurrency redesign: the deterministic round-robin executor
    // `future.join_all`, written in pure witchy over the `std/future` substrate.
    // Two cooperative tasks (each yielding via `future.pending`) interleave at
    // their yield points in a fixed schedule, so the interleaved output is
    // byte-identical on both backends — concurrency with parity, no scheduler
    // state in the runtime and no WASM feature.
    #[test]
    fn future_executor_interleaves_backends_agree() {
        let src = r#"
import future

fn ticker(console: Console, name: String, n: Int) -> Future(Int):
    if n <= 0:
        future.ready(n)
    else:
        future.and_then(future.defer(fn(): print(console, name + " " + "${n}")), fn(_a):
            future.and_then(future.pending(0), fn(_b):
                ticker(console, name, n - 1)))

fn main(console: Console):
    let results = future.join_all([ticker(console, "A", 2), ticker(console, "B", 2)])
    print(console, "done ${results}")
"#;
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp_out =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm_out = crate::run_wasm_bytes(&bytes).expect("wasm");
        assert_eq!(interp_out, wasm_out, "executor schedule diverged across backends");
        assert_eq!(interp_out, vec!["A 2", "B 2", "A 1", "B 1", "done [0, 0]"]);
    }

    // Traits over a user ADT: the receiver type comes from the constructor, and
    // the impl body matches on `self`. Both backends agree.
    #[test]
    fn traits_dispatch_on_adt_backends_agree() {
        let src = r#"
type Shape:
    Circle(Int)
    Square(Int)

trait Area:
    fn area(self) -> Int

impl Area for Shape:
    fn area(self) -> Int:
        match self:
            Circle(r) -> ((r * r) * 3)
            Square(s) -> (s * s)

fn main(console: Console):
    print(console, __render(area(Circle(2))))
    print(console, __render(area(Square(3))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "trait ADT dispatch diverged");
        assert_eq!(run_on_wasm(src), vec!["12", "9"]);
    }

    // Default trait methods: a method with a body in the trait is inherited by
    // impls that don't define it (calling the impl's other methods on `self`),
    // and can be overridden. Both backends agree.
    #[test]
    fn traits_default_methods_backends_agree() {
        let src = r#"
trait Label:
    fn tag(self) -> String
    fn shout(self) -> String:
        (tag(self) + "!")

impl Label for Int:
    fn tag(self) -> String:
        "int"

impl Label for Bool:
    fn tag(self) -> String:
        "bool"

    fn shout(self) -> String:
        "BOOL!!"

fn main(console: Console):
    print(console, shout(5))
    print(console, shout(true))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "trait default-method diverged");
        assert_eq!(run_on_wasm(src), vec!["int!", "BOOL!!"]);
    }

    // Cross-module traits: a trait and its impls defined in one module are used
    // from another that imports it. Desugaring runs after linking, so the
    // generated methods and their call sites resolve across the flat merged
    // namespace. Both backends agree.
    #[test]
    fn traits_cross_module_backends_agree() {
        let show_mod = r#"
trait Show:
    fn show(self) -> String

impl Show for Int:
    fn show(self) -> String:
        __render(self)

impl Show for Bool:
    fn show(self) -> String:
        if self:
            "Y"
        else:
            "N"
"#;
        let app = r#"
import show_mod

fn main(console: Console):
    print(console, show(42))
    print(console, show(false))
"#;
        let sources = [("show_mod", show_mod), ("app", app)];
        let interpreted = interpreter::run_program(&sources, "app").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "app");
        assert_eq!(interpreted, compiled, "cross-module trait diverged");
        assert_eq!(compiled, vec!["42", "N"]);
    }

    // The standard comparison hierarchy: `import cmp` brings the `PartialEq` ->
    // `Eq` -> `PartialOrd` -> `Ord` traits into scope. The built-in Int impl, a
    // user type implementing the hierarchy, the `Ordering` result of `compare`,
    // the `PartialOrd` default methods (`less`/`greater`/`greater_equal`), and
    // `Float` being only `PartialOrd` (so `less` works, `compare` does not) all
    // hold, and both backends agree.
    #[test]
    fn std_ord_trait_backends_agree() {
        let client = r#"
import cmp

type Money:
    Money(Int)

impl PartialEq for Money:
    fn eq(self, other: Money) -> Bool:
        match self:
            Money(a) -> match other:
                Money(b) -> a == b

impl Eq for Money

impl PartialOrd for Money:
    fn partial_compare(self, other: Money) -> Option(Ordering):
        Some(compare(self, other))

impl Ord for Money:
    fn compare(self, other: Money) -> Ordering:
        match self:
            Money(a) -> match other:
                Money(b) -> if (a < b): Less else: if (a > b): Greater else: Equal

fn main(console: Console):
    print(console, __render(compare(3, 5)))
    print(console, __render(less(3, 5)))
    print(console, __render(greater_equal(5, 5)))
    print(console, __render(less(1.5, 2.5)))
    print(console, __render(compare(Money(10), Money(4))))
    print(console, __render(greater(Money(10), Money(4))))
    print(console, __render(eq(Money(7), Money(7))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std Ord diverged");
        assert_eq!(
            compiled,
            vec!["Less", "true", "true", "true", "Greater", "true", "true"]
        );
    }

    // The comparison OPERATORS (`== != < > <= >=`) desugar through the derived
    // PartialEq/PartialOrd impls of a user record — no named `eq`/`less` call —
    // and both backends agree. Also covers the `Ordering` result of `compare`,
    // `cmp.reverse`, and `cmp.sort` over the user type.
    #[test]
    fn comparison_operators_dispatch_on_user_types() {
        let src = "import cmp\n\ntype Coord derive(PartialEq, Eq, PartialOrd, Ord):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let a = Coord(1, 2)\n    let b = Coord(1, 5)\n    print(console, \"${a == a} ${a == b} ${a != b}\")\n    print(console, \"${a < b} ${b > a} ${a <= a} ${b >= b}\")\n    print(console, __render(compare(a, b)))\n    print(console, __render(cmp.reverse(compare(a, b))))\n    print(console, \"${cmp.sort([Coord(2, 0), Coord(1, 9), Coord(1, 1)])}\")\n";
        let want: Vec<String> = [
            "true false true",
            "true true true true",
            "Less",
            "Greater",
            "[Coord(1, 1), Coord(1, 9), Coord(2, 0)]",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    // `Float` implements `PartialEq` + `PartialOrd` only (NaN is unequal to itself
    // and unordered), so the operators work but an `Ord`-bounded helper rejects
    // `List(Float)` at check time — Float is not totally ordered.
    #[test]
    fn float_is_partial_ord_not_ord() {
        let ok = "import cmp\n\nfn main(console: Console):\n    print(console, \"${1.5 < 2.5} ${2.5 == 2.5} ${2.5 != 1.5}\")\n";
        assert_eq!(link_run(ok), vec!["true true true".to_string()], "Float PartialOrd works");

        let bad = "import cmp\n\nfn main(console: Console):\n    print(console, \"${cmp.sort([3.0, 1.0, 2.0])}\")\n";
        let module = parser::parse_module(bad).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("Float is not Ord — cmp.sort must reject it").message;
        assert!(err.contains("Ord"), "error should mention Ord: {err}");
    }

    // A user supertrait hierarchy (`trait Derived: Base`): a `where a: Derived`
    // bound discharges the SUPERTRAIT's methods too, so the body calls both
    // `base` (declared on `Base`) and `derived`. Both backends agree.
    #[test]
    fn supertrait_methods_resolve_through_bound() {
        let src = "trait Base:\n    fn base(self) -> Int\n\ntrait Derived: Base:\n    fn derived(self) -> Int\n\ntype W:\n    W(Int)\n\nimpl Base for W:\n    fn base(self) -> Int:\n        match self:\n            W(n) -> n\n\nimpl Derived for W:\n    fn derived(self) -> Int:\n        match self:\n            W(n) -> n * 2\n\nfn use_it(x: a) -> Int where a: Derived:\n    base(x) + derived(x)\n\nfn main(console: Console):\n    print(console, __render(use_it(W(5))))\n";
        let want = vec!["15".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");

        // Omitting the supertrait impl is a loud check error.
        let bad = "trait Base:\n    fn base(self) -> Int\n\ntrait Derived: Base:\n    fn derived(self) -> Int\n\ntype W:\n    W(Int)\n\nimpl Derived for W:\n    fn derived(self) -> Int:\n        match self:\n            W(n) -> n\n\nfn main(console: Console):\n    print(console, \"x\")\n";
        let module = parser::parse_module(bad).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("missing supertrait impl must be rejected").message;
        assert!(err.contains("Base"), "error should name the missing supertrait: {err}");
    }

    // The standard `Show` trait: `show` renders built-in types and any user type
    // that implements it — including the rendering of a value the built-in
    // `to_string` couldn't. Both backends agree.
    #[test]
    fn std_show_trait_backends_agree() {
        let client = r#"
import show

type Point:
    Point(Int, Int)

impl Show for Point:
    fn show(self) -> String:
        match self:
            Point(x, y) -> (((("(" + __render(x)) + ", ") + __render(y)) + ")")

fn main(console: Console):
    print(console, show(42))
    print(console, show(true))
    print(console, show("hi"))
    print(console, show(Point(2, 3)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std Show diverged");
        assert_eq!(compiled, vec!["42", "true", "hi", "(2, 3)"]);
    }

    // Generic bounds: `pick_max(x: a, y: a) -> a where a: Ord` is a template,
    // monomorphized per concrete instantiation; the `greater` trait call inside
    // each specialization resolves to that type's Ord impl. Exercised over Int
    // (built-in impl) and a user type. Both backends agree.
    #[test]
    fn generic_bounds_backends_agree() {
        let client = r#"
import cmp

type Box:
    Box(Int)

impl PartialEq for Box:
    fn eq(self, other: Box) -> Bool:
        match self:
            Box(a) -> match other:
                Box(b) -> a == b

impl Eq for Box

impl PartialOrd for Box:
    fn partial_compare(self, other: Box) -> Option(Ordering):
        Some(compare(self, other))

impl Ord for Box:
    fn compare(self, other: Box) -> Ordering:
        match self:
            Box(a) -> match other:
                Box(b) -> if (a < b): Less else: if (a > b): Greater else: Equal

fn pick_max(x: a, y: a) -> a where a: Ord:
    if greater(x, y):
        x
    else:
        y

fn unbox(b: Box) -> Int:
    match b:
        Box(n) -> n

fn main(console: Console):
    print(console, __render(pick_max(3, 7)))
    print(console, __render(pick_max(20, 5)))
    print(console, __render(unbox(pick_max(Box(4), Box(11)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic bounds diverged");
        assert_eq!(compiled, vec!["7", "20", "11"]);
    }

    // The stdlib's generic `Ord` helpers (max_of/min_of/clamp) are bounded
    // generics living in the `ord` module, monomorphized at the user's call
    // sites — over Int (incl. a negative literal) and a user Box type. Proves
    // cross-module bounded-generic monomorphization. Both backends agree.
    #[test]
    fn std_ord_generics_backends_agree() {
        let client = r#"
import cmp

type Box:
    Box(Int)

impl PartialEq for Box:
    fn eq(self, other: Box) -> Bool:
        match self:
            Box(a) -> match other:
                Box(b) -> a == b

impl Eq for Box

impl PartialOrd for Box:
    fn partial_compare(self, other: Box) -> Option(Ordering):
        Some(compare(self, other))

impl Ord for Box:
    fn compare(self, other: Box) -> Ordering:
        match self:
            Box(a) -> match other:
                Box(b) -> if (a < b): Less else: if (a > b): Greater else: Equal

fn unbox(b: Box) -> Int:
    match b:
        Box(n) -> n

fn main(console: Console):
    print(console, __render(cmp.max_of((-5), 3)))
    print(console, __render(cmp.min_of(8, 2)))
    print(console, __render(cmp.clamp(10, 0, 5)))
    print(console, __render(cmp.clamp(0, 3, 9)))
    print(console, __render(unbox(cmp.max_of(Box(4), Box(11)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std Ord generics diverged");
        assert_eq!(compiled, vec!["3", "2", "5", "3", "11"]);
    }

    // Bounds through `List(a)`: a generic over a collection. `cmp.maximum` /
    // `cmp.minimum` are bounded generics taking `List(a) where a: Ord`,
    // monomorphized by the list's element type; the trait call inside resolves
    // via the for-loop variable's element type. Exercised over Int (incl. an
    // empty list -> default) and a user Box type. Both backends agree.
    #[test]
    fn generic_over_list_backends_agree() {
        let client = r#"
import cmp

type Box:
    Box(Int)

impl PartialEq for Box:
    fn eq(self, other: Box) -> Bool:
        match self:
            Box(a) -> match other:
                Box(b) -> a == b

impl Eq for Box

impl PartialOrd for Box:
    fn partial_compare(self, other: Box) -> Option(Ordering):
        Some(compare(self, other))

impl Ord for Box:
    fn compare(self, other: Box) -> Ordering:
        match self:
            Box(a) -> match other:
                Box(b) -> if (a < b): Less else: if (a > b): Greater else: Equal

fn unbox(b: Box) -> Int:
    match b:
        Box(n) -> n

fn main(console: Console):
    print(console, __render(cmp.maximum([3, 7, 2, 9, 4], 0)))
    print(console, __render(cmp.minimum([3, 7, 2, 9, 4], 100)))
    print(console, __render(cmp.maximum([], 42)))
    print(console, __render(unbox(cmp.maximum([Box(2), Box(8), Box(5)], Box(0)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic-over-list diverged");
        assert_eq!(compiled, vec!["9", "2", "42", "8"]);
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
    print(console, __render(total))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "indentation backends diverged");
        assert_eq!(run_on_wasm(src), vec!["24"]);
    }

    // Indentation syntax with traits/impls and a nested if/else expression.
    #[test]
    fn indentation_traits_backends_agree() {
        let src = r#"
trait Show:
    fn show(self) -> String

impl Show for Int:
    fn show(self) -> String:
        __render(self)

impl Show for Bool:
    fn show(self) -> String:
        if self:
            "yes"
        else:
            "no"

fn main(console: Console):
    print(console, show(42))
    print(console, show(true))
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
    print(console, __render(x))
    print(console, __render(y))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "block-then-paren diverged");
        assert_eq!(run_on_wasm(src), vec!["6", "10"]);
    }

    // std/http: a real HTTP/1.1 GET over the Net capability against a loopback
    // server. Networking is interpreter-only (not compiled), so this isn't a
    // differential test; it proves the capability-gated socket primitives plus
    // the http library parse a live response into status + body.
    #[test]
    fn std_http_get_against_loopback() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the whole request (up to the blank header line) before
                // replying — closing with unread data would RST the client.
                let mut req = Vec::new();
                let mut tmp = [0u8; 256];
                while let Ok(n) = stream.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&tmp[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let r = http.get(net, "127.0.0.1", {port}, "/")
    print(console, __render(http.status(r)))
    print(console, http.body(r))
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["200".to_string(), "hello".to_string()]);
    }

    // A server replying with a non-numeric status code must not crash the client:
    // `status_code` guards `string_to_int` and reports 0 for a malformed status
    // line, so the body is still readable. Interpreter-only.
    #[test]
    fn std_http_tolerates_malformed_status_line() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut tmp = [0u8; 256];
                let mut req = Vec::new();
                while let Ok(n) = stream.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&tmp[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                // A non-numeric status code (`BAD`) — would trap string_to_int.
                let resp = "HTTP/1.1 BAD Weird\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi";
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let r = http.get(net, "127.0.0.1", {port}, "/")
    print(console, __render(http.status(r)))
    print(console, http.body(r))
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["0".to_string(), "hi".to_string()]);
    }

    // std/http POST: send a request body and read it back from a loopback echo
    // server. Interpreter-only (networking isn't compiled).
    #[test]
    fn std_http_post_against_loopback() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = match listener.accept() {
                Ok(x) => x,
                Err(_) => return,
            };
            // Read the full request: headers, then Content-Length body bytes.
            let mut data: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let text = String::from_utf8_lossy(&data).into_owned();
                if let Some(hdr_end) = text.find("\r\n\r\n") {
                    let clen: usize = text[..hdr_end]
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if data.len() >= hdr_end + 4 + clen {
                        break;
                    }
                }
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(k) => data.extend_from_slice(&tmp[..k]),
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&data);
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let r = http.post(net, "127.0.0.1", {port}, "/echo", "hello body")
    print(console, __render(http.status(r)))
    print(console, http.body(r))
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["200".to_string(), "hello body".to_string()]);
    }

    // std/http response headers: case-insensitive lookup + a missing header.
    // Interpreter-only (networking).
    #[test]
    fn std_http_headers_against_loopback() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = Vec::new();
                let mut tmp = [0u8; 256];
                while let Ok(n) = stream.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&tmp[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Custom: abc\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi";
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let program = format!(
            r#"
import http
import option
fn main(console: Console, net: Net):
    let r = http.get(net, "127.0.0.1", {port}, "/")
    print(console, option.unwrap_or(http.header(r, "Content-Type"), "none"))
    print(console, option.unwrap_or(http.header(r, "x-custom"), "none"))
    print(console, option.unwrap_or(http.header(r, "Missing"), "none"))
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["application/json", "abc", "none"]);
    }

    // std/json: build a nested Json value and serialize it. Pure (no
    // capabilities), so it compiles to WASM and both backends must agree.
    #[test]
    fn std_json_encode_backends_agree() {
        let client = r#"
import json
fn main(console: Console):
    let j = JsonObject([
        ("name", JsonString("witchy")),
        ("version", JsonInt(1)),
        ("tags", JsonArray([JsonString("safe"), JsonString("fast")])),
        ("stable", JsonBool(false)),
        ("extra", JsonNull)
    ])
    print(console, json.encode(j))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std json encode diverged");
        assert_eq!(
            compiled,
            vec![
                r#"{"name":"witchy","version":1,"tags":["safe","fast"],"stable":false,"extra":null}"#
            ]
        );
    }

    // std/json decode: parse JSON text then re-encode it. The round trip
    // exercises the recursive-descent parser (objects, arrays, strings, bools,
    // null, negative ints, nesting) and must agree on both backends.
    #[test]
    fn std_json_decode_roundtrip_backends_agree() {
        let client = r#"
import json
fn main(console: Console):
    let input = "{\"name\":\"witchy\",\"nums\":[1,2,3],\"ok\":true,\"nil\":null,\"neg\":-5,\"nested\":{\"a\":[true,false]}}"
    match json.decode(input):
        Ok(j) -> print(console, json.encode(j))
        Err(e) -> print(console, "error: " + e)
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std json decode diverged");
        assert_eq!(
            compiled,
            vec![
                r#"{"name":"witchy","nums":[1,2,3],"ok":true,"nil":null,"neg":-5,"nested":{"a":[true,false]}}"#
            ]
        );
    }

    // std/json accessors: decode then pull out a string field (object key
    // lookup), an int field, and an array element. Object lookup compares the
    // decoded, heap-built key with `==`; both backends agree now that codegen
    // tracks the type of a tuple-destructured loop variable (so the comparison
    // is by content, not pointer).
    #[test]
    fn std_json_accessors_backends_agree() {
        let client = r#"
import json
import option
fn field(j: Json, k: String) -> Json:
    match json.get(j, k):
        Some(v) -> v
        None -> JsonNull

fn elem_int(j: Json, k: String, i: Int) -> Int:
    match json.index(field(j, k), i):
        Some(e) -> option.unwrap_or(json.as_int(e), 0)
        None -> 0

fn main(console: Console):
    match json.decode("{\"name\":\"witchy\",\"version\":3,\"items\":[10,20,30]}"):
        Ok(j) ->
            print(console, option.unwrap_or(json.as_string(field(j, "name")), "?"))
            print(console, __render(option.unwrap_or(json.as_int(field(j, "version")), 0)))
            print(console, __render(elem_int(j, "items", 1)))
        Err(e) -> print(console, e)
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std json accessors diverged");
        assert_eq!(compiled, vec!["witchy", "3", "20"]);
    }

    // Hex (0x..) and binary (0b..) integer literals, including underscore
    // separators, feeding the bitwise operators. Both backends agree.
    #[test]
    fn hex_binary_literals_backends_agree() {
        let src = r#"
fn main(console: Console):
    print(console, __render(255))
    print(console, __render(10))
    print(console, __render((255 & 15)))
    print(console, __render((12 | 3)))
    print(console, __render(65535))
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
    print(console, __render(string.to_int("42")))
    print(console, __render(string.to_int("-17")))
    print(console, __render(string.to_int("  123  ")))
    print(console, __render(string.to_int("+8")))
    print(console, __render(string.to_int("0")))
    print(console, __render((string.to_int("1000000") + 1)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "-17", "123", "8", "0", "1000001"]);
    }

    #[test]
    fn bitwise_not_backends_agree() {
        // ~x = -x-1 (width-independent), so it agrees across backends.
        let src = r#"
fn main(console: Console):
    print(console, __render((~0)))
    print(console, __render((~5)))
    print(console, __render((~(0 - 1))))
    print(console, __render((255 & (~15))))
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
    print(console, __render((12 & 10)))
    print(console, __render((12 | 10)))
    print(console, __render((12 ^ 10)))
    print(console, __render((1 << 4)))
    print(console, __render((256 >> 2)))
    print(console, __render(((5 & 3) | 8)))
    print(console, __render(((5 & 4) == 4)))
    print(console, classify(2))
    print(console, classify(3))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["8", "14", "6", "16", "64", "9", "true", "pow2", "other"]
        );
    }

    #[test]
    fn or_patterns_backends_agree() {
        // `p1 | p2 -> body` desugars to one arm per alternative. Works for
        // literal alternatives and for constructor alternatives that bind the
        // same variable. Both backends agree.
        let src = r#"
type Shape:
    Circle(Int)
    Square(Int)
    Rect(Int, Int)

fn classify(n: Int) -> String:
    match n:
        1 -> "small"
        2 -> "small"
        3 -> "small"
        4 -> "medium"
        5 -> "medium"
        _ -> "big"

fn side(s: Shape) -> Int:
    match s:
        Circle(r) -> r
        Square(r) -> r
        Rect(w, h) -> w

fn main(console: Console):
    print(console, classify(2))
    print(console, classify(5))
    print(console, classify(10))
    print(console, __render(side(Circle(5))))
    print(console, __render(side(Square(7))))
    print(console, __render(side(Rect(3, 4))))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["small", "medium", "big", "5", "7", "3"]
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
    fn nested_scope_shadowing_backends_agree() {
        // An inner binding that shadows an outer one of the same name must not
        // clobber the outer: after the inner scope ends, the outer value is back.
        let src = r#"
fn main(console: Console):
    let x = 1
    if true:
        let x = 2
        print(console, __render(x))
    print(console, __render(x))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2", "1"]);
    }

    #[test]
    fn record_with_collection_field_backends_agree() {
        // A record holding a List(Int) and a String: field access, length on a
        // list field, and a `for` loop iterating a list *field* (the iterand is a
        // field expression, not a variable). Both backends agree.
        let src = r#"
type Bag:
    items: List(Int)
    label: String

fn main(console: Console):
    let b = Bag([10, 20, 30], "nums")
    print(console, (b).label)
    print(console, __render(list.length((b).items)))
    var total = 0
    for x in (b).items:
        total = (total + x)
    print(console, __render(total))
    print(console, __render(list.at((b).items, 1)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["nums", "3", "60", "20"]);
    }

    #[test]
    fn nested_records_backends_agree() {
        // A record containing a record: chained field access (o.inner.v), nested
        // construction, `update` on a nested field, and immutability of the
        // original. Both backends must agree.
        let src = r#"
type Inner:
    v: Int

type Outer:
    name: String
    inner: Inner

fn main(console: Console):
    let o = Outer("x", Inner(42))
    print(console, __render(((o).inner).v))
    let o2 = Outer(inner: Inner((((o).inner).v + 1)), ..o)
    print(console, __render(((o2).inner).v))
    print(console, (o).name)
    print(console, __render(((o).inner).v))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "43", "x", "42"]);
    }

    #[test]
    fn var_swap_and_loop_backends_agree() {
        // Harder `var`: two var parameters (swap) — exercising move-out of
        // multiple values — and an var mutation inside a loop. Both backends
        // must agree.
        let src = r#"
fn swap(var a: Int, var b: Int):
    let t = a
    a = b
    b = t

fn bump_by(var n: Int, d: Int):
    n = (n + d)

fn main(console: Console):
    var x = 3
    var y = 8
    swap(x, y)
    print(console, __render(x))
    print(console, __render(y))
    var acc = 0
    var i = 1
    while (i < 5):
        bump_by(acc, i)
        i = (i + 1)
    print(console, __render(acc))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        // And the concrete values, to be sure both compute the right thing.
        assert_eq!(run_on_wasm(src), vec!["8", "3", "10"]);
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
    fn files_example_reads_through_capability() {
        // Run from the crate root so examples/data/greeting.txt resolves.
        assert_eq!(
            interp(include_str!("../examples/files/src/files.witchy")),
            vec!["hello from a sandboxed Dir capability"]
        );
    }

    #[test]
    fn runs_a_file_with_file_based_imports() {
        let dir = std::env::temp_dir().join(format!("witchy_cli_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("strutil.witchy"),
            r#"
fn shout(s: String) -> String:
    ("HI " + s)
"#,
        )
        .unwrap();
        let app = dir.join("app.witchy");
        std::fs::write(
            &app,
            "import strutil\nfn main(console: Console):\n    print(console, strutil.shout(\"x\"))\n",
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
    fn tuples_example() {
        assert_eq!(
            interp(include_str!("../examples/tuples/src/tuples.witchy")),
            vec!["3 remainder 2", "7 spells seven", "just the remainder: 2", "2 3"]
        );
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
            interp(include_str!("../examples/result/src/result.witchy")),
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
    fn loops_example() {
        assert_eq!(
            interp(include_str!("../examples/loops/src/loops.witchy")),
            vec!["sum = 108", "witchy loops work"]
        );
    }

    #[test]
    fn listmatch_example() {
        assert_eq!(
            interp(include_str!("../examples/listmatch/src/listmatch.witchy")),
            vec!["sum = 21", "starts with 3", "one: 42", "empty"]
        );
    }

    #[test]
    fn records_example() {
        assert_eq!(
            interp(include_str!("../examples/records/src/records.witchy")),
            vec![
                "origin.x = 2",
                "moved = (12, 3)",
                "manhattan(moved) = 15"
            ]
        );
    }

    #[test]
    fn record_update_example() {
        assert_eq!(
            interp(include_str!("../examples/record_update/src/record_update.witchy")),
            vec!["alice 100", "alice 150", "alice smith 150"]
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
    fn higher_order_example() {
        assert_eq!(
            interp(include_str!("../examples/higher_order/src/higher_order.witchy")),
            vec!["15", "81", "15", "120"]
        );
    }

    #[test]
    fn list_ops_example() {
        assert_eq!(
            interp(include_str!("../examples/list_ops/src/list_ops.witchy")),
            vec!["55", "6", "0-2-4"]
        );
    }

    #[test]
    fn wordcount_example() {
        assert_eq!(
            interp(include_str!("../examples/wordcount/src/wordcount.witchy")),
            vec!["3", "1", "0", "4"]
        );
    }

    #[test]
    fn inventory_example() {
        assert_eq!(
            interp(include_str!("../examples/inventory/src/inventory.witchy")),
            vec!["total = 9", "over 2: 2"]
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
        assert_eq!(
            interp(include_str!("../examples/parse_kv/src/parse_kv.witchy")),
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

    /// RFC-0006: a `tag"…${e}…"` tagged literal expands at COMPILE TIME — the tag
    /// (`twice`, a self-contained `fn(parts, holes) -> String` returning witchy
    /// EXPRESSION SOURCE) runs once in the compiler and its result is parsed and
    /// SPLICED over the literal. The literal is gone before either backend sees
    /// the program, so the interpreter and the compiled-WASM backend must produce
    /// IDENTICAL output. `twice"a${v}b"` expands to source `"a" + v + v + "b"`,
    /// which at the call site (`v = "X"`) evaluates to `"aXXb"`.
    #[test]
    fn tagged_literal_expands_identically_on_both_backends() {
        let src = "import list\n\
                   \n\
                   fn twice(parts: List(String), holes: List(String)) -> String:\n\
                   \x20   let a = list.at(parts, 0)\n\
                   \x20   let b = list.at(parts, 1)\n\
                   \x20   let h = list.at(holes, 0)\n\
                   \x20   \"\\\"\" + a + \"\\\" + \" + h + \" + \" + h + \" + \\\"\" + b + \"\\\"\"\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let v = \"X\"\n\
                   \x20   print(console, twice\"a${v}b\")\n";
        // Interpreter and compiled WASM agree, and both yield the spliced value.
        let interp = link_run(src);
        let wasm = wasm_run(src);
        assert_eq!(interp, vec!["aXXb".to_string()], "interpreter output");
        assert_eq!(wasm, interp, "compiled WASM must match the interpreter");
    }

    /// The glamour rune's source, embedded so tests can `import glamour` without a
    /// sibling file on disk — the same trick `coven`'s server modules use.
    const GLAMOUR_SRC: &str = include_str!("../projects/glamour/src/glamour.witchy");

    /// Link a program that `import glamour` against the embedded glamour source
    /// (and, transitively, the bundled std), then run it on BOTH backends and
    /// assert they agree — the parity oracle for the framework rune. Returns the
    /// agreed output. The `html` tag is a COMPILE-TIME literal (RFC-0006): it is
    /// expanded by the linker before either backend sees the program, so this is a
    /// genuine differential test of the *expanded* `VNode`-constructing AST.
    fn glamour_run_both(src: &str) -> Vec<String> {
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = parser::parse_module(GLAMOUR_SRC).expect("parse glamour");
        let modules = vec![("main".to_string(), entry), ("glamour".to_string(), glamour)];
        let linked = crate::pipeline::link(modules, "main").expect("link glamour consumer");
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp run");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let wasm = crate::run_wasm_bytes(&bytes).expect("wasm run");
        assert_eq!(wasm, interp, "compiled WASM must match the interpreter");
        interp
    }

    /// (RFC-0039) The secret effect's WIRE FORMAT is pure data, so it serializes IDENTICALLY
    /// on both backends. A `SecretField` VNode and a `SubmitSecret` Cmd carry ONLY their
    /// host-slot coordinates and port name — read out of the sealed `SecretInput`/`SecretRef`/
    /// `CredentialPort` tokens (minted here from a granted `UiRoot`), never a value — and the
    /// interpreter and compiled WASM agree byte-for-byte. This is the parity half of the
    /// host-custody guarantee: the description the rune emits is inert, identical data.
    #[test]
    fn glamour_secret_wire_is_identical_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import glamour\n\
                   import json\n\
                   \n\
                   type Msg:\n\
                   \x20   Done(String)\n\
                   \n\
                   fn mj(m: Msg) -> Json:\n\
                   \x20   JsonString(\"\")\n\
                   \n\
                   fn main(console: Console, ui: UiRoot):\n\
                   \x20   let input = glamour.secret_field(ui, \"login\", \"password\")\n\
                   \x20   let cred = glamour.credential_port(ui, \"passkeyLogin\")\n\
                   \x20   let cmd = glamour.submit_secret(glamour.secret_ref(input), cred, \"Done\")\n\
                   \x20   let node: VNode(Msg) = glamour.secret_input(input, \"PwStatus\")\n\
                   \x20   print(console, glamour.to_json(node, mj))\n\
                   \x20   print(console, json.encode(glamour.cmd_to_json(cmd, mj)))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = parser::parse_module(GLAMOUR_SRC).expect("parse glamour");
        let modules = vec![("main".to_string(), entry), ("glamour".to_string(), glamour)];
        let linked = crate::pipeline::link(modules, "main").expect("link glamour consumer");
        typeck::check(&linked).expect("typecheck");

        // Interpreter: grant a single-field `UiRoot` keyed by the param name `ui`.
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("policy".to_string(), "login".to_string());
        let mut grants = std::collections::BTreeMap::new();
        grants.insert("ui".to_string(), fields);
        let interp = interpreter::run_module_user_caps(linked.clone(), ".", vec![], vec![], vec![], grants)
            .expect("interp");

        // Compiled: stage the one field host-side (declaration order).
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    quiet: true,
                    user_cap_fields: vec![vec!["login".to_string()]],
                    ..Default::default()
                },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), interp, "compiled WASM must match the interpreter");

        // And the shape is exactly the host-shell protocol: slot + port, no value.
        assert_eq!(
            interp,
            vec![
                "{\"secret\":{\"form\":\"login\",\"field\":\"password\"},\"on_ready\":\"PwStatus\"}".to_string(),
                "{\"cmd\":\"submit_secret\",\"slot\":\"login/password\",\"port\":\"passkeyLogin\",\"tag\":\"Done\"}".to_string(),
            ],
            "the secret wire carries only slot + port names (from tokens), never a value"
        );
    }

    /// RFC-0008: glamour's `html` tag (RFC-0006 compile-time literal) builds a
    /// `VNode(msg)` tree, and the serializer renders it IDENTICALLY on both
    /// backends. The headline property is structural XSS-immunity: a text-position
    /// hole carrying `<script>x</script>` becomes a `Text` NODE, never markup, so
    /// the serializer escapes it to `&lt;script&gt;…` — proven observable here.
    #[test]
    fn glamour_html_tag_renders_and_is_xss_immune_on_both_backends() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let cls = \"card\"\n\
                   \x20   let title = \"Witchy\"\n\
                   \x20   let body = \"<script>x</script>\"\n\
                   \x20   let view = html\"<div class=${cls}><h2>${title}</h2><span class=\\\"cap\\\">${body}</span></div>\"\n\
                   \x20   print(console, to_html(view))\n";
        let out = glamour_run_both(src);
        assert_eq!(
            out,
            vec![
                "<div class=\"card\"><h2>Witchy</h2><span class=\"cap\">\
                 &lt;script&gt;x&lt;/script&gt;</span></div>"
                    .to_string()
            ],
            "static class -> prop, text holes -> text nodes, and the <script> \
             payload renders ESCAPED — XSS-immune by construction"
        );
        // The escaped payload is present; the raw executable form is NOT.
        let rendered = &out[0];
        assert!(rendered.contains("&lt;script&gt;"), "the payload must be escaped");
        assert!(
            !rendered.contains("<script>"),
            "no raw <script> may reach the output — that would be an injection"
        );
    }

    /// RFC-0008: events are DATA. `on:click=${Inc}` in attribute position lowers
    /// to `on("click", Inc)` carrying a `msg` VALUE (not a closure), and the same
    /// expanded AST runs identically on both backends.
    #[test]
    fn glamour_event_binding_is_a_msg_value_on_both_backends() {
        let src = "import glamour\n\
                   \n\
                   type Msg:\n\
                   \x20   Inc\n\
                   \x20   Dec\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let view = html\"<button on:click=${Inc}>+</button>\"\n\
                   \x20   print(console, to_html(view))\n";
        let out = glamour_run_both(src);
        assert_eq!(out, vec!["<button data-on-click=\"[msg]\">+</button>".to_string()]);
    }

    /// RFC-0008: glamour's `to_json` serializes a VNode tree to the wire format the
    /// JS DOM host shell (`web/witchy-runtime/glamour-dom.mjs`) consumes —
    /// `{"el":tag,"attrs":[["prop",k,v]|["on",evt,<msg-json>]],"kids":[...]}` /
    /// `{"text":"..."}`. The `On` binding embeds the msg via a caller-supplied
    /// `msg_to_json` (here `json.value_of`), so an event handler round-trips as its
    /// message value. The serialized string must be IDENTICAL on both backends.
    #[test]
    fn glamour_to_json_serializes_the_wire_format_on_both_backends() {
        let src = "import glamour\n\
                   import json\n\
                   import reflect\n\
                   \n\
                   type Msg derive(Reflect):\n\
                   \x20   Inc\n\
                   \x20   Dec\n\
                   \n\
                   fn msg_to_json(m: Msg) -> Json:\n\
                   \x20   json.value_of(m)\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let view = element(\"div\", [prop(\"class\", \"c\")], [\n\
                   \x20       element(\"button\", [on(\"click\", Inc)], [text(\"+\")]),\n\
                   \x20       text(\"hi\"),\n\
                   \x20   ])\n\
                   \x20   print(console, to_json(view, msg_to_json))\n";
        let out = glamour_run_both(src);
        assert_eq!(
            out,
            vec![
                "{\"el\":\"div\",\"attrs\":[[\"prop\",\"class\",\"c\"]],\"kids\":\
                 [{\"el\":\"button\",\"attrs\":[[\"on\",\"click\",\
                 {\"$variant\":\"Inc\",\"$values\":[]}]],\"kids\":[{\"text\":\"+\"}]},\
                 {\"text\":\"hi\"}]}"
                    .to_string()
            ],
            "to_json must emit the documented wire shape: el/attrs/kids, prop/on \
             attrs, and the On msg embedded as its reflected JSON"
        );
    }

    /// RFC-0008 §1 / RFC-0007: a `pub fn export_*(String) -> String` compiles to a
    /// JS-callable export. The module must export the `__galloc` allocator and the
    /// `__export_<name>` wrapper (so the host can write the input String header and
    /// call the function), keep the existing `run`/`memory` exports intact, and add
    /// NO import (the call path grants no authority). This is the codegen contract
    /// the JS `callString` (and the spike's round-trip) depend on.
    #[test]
    fn string_export_emits_galloc_and_wrapper_with_no_extra_import() {
        let src = "pub fn export_echo(s: String) -> String:\n\
                   \x20   \"echo: \" + s\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   print(console, export_echo(\"hi\"))\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");

        let mut exports: Vec<String> = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::ExportSection(s) = payload.expect("parse") {
                for e in s {
                    exports.push(e.expect("export").name.to_string());
                }
            }
        }
        for want in ["memory", "run", "__galloc", "__export_export_echo"] {
            assert!(
                exports.contains(&want.to_string()),
                "module must export `{want}`; got {exports:?}"
            );
        }
        // The module validates (the synthesized wrappers are well-formed wasm). The
        // spike (`tests/browser_shim.rs`) proves it round-trips through the JS shim
        // and that the wrappers add NO host import (the rune stays instantiable
        // under the deny-all pure-compute host).
        assert!(
            wasmparser::validate(&bytes).is_ok(),
            "a module with string-export wrappers must validate"
        );
    }

    /// (RFC-0040) A `pub fn export_*(cap: <grantable>, String) -> String` is a
    /// browser app root: the leading bare grantable cap is host-minted per call, so
    /// the module compiles, validates, and exports its `__export_*` wrapper (which
    /// mints the cap via `mk{N}(build_user_cap_field…)`, mirroring the `run` wrapper).
    #[test]
    fn cap_gated_string_export_compiles_and_validates() {
        let src = "grantable capability UiRoot:\n    policy: String\n\npub fn export_step(ui: UiRoot, input: String) -> String:\n    match ui:\n        UiRoot(p) -> p + \":\" + input\n\nfn main(console: Console):\n    print(console, \"ok\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect("compile")
            .expect("the binary path lowers this program");
        let mut exports: Vec<String> = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::ExportSection(s) = payload.expect("parse") {
                for e in s {
                    exports.push(e.expect("export").name.to_string());
                }
            }
        }
        assert!(
            exports.contains(&"__export_export_step".to_string()),
            "the cap-gated export's wrapper must be exported; got {exports:?}"
        );
        assert!(
            wasmparser::validate(&bytes).is_ok(),
            "a cap-gated export module must validate (the minting wrapper is well-formed)"
        );
    }

    /// RFC-0008 acceptance criterion: the glamour rune has an EMPTY runtime
    /// footprint — no Net, no Dir, no Clock, nothing. coven's own analyzer
    /// (`capabilities::analyze`, the engine behind `witchy caps`) proves it from
    /// source. This is the headline: a UI framework whose authority is provably
    /// nil. The `witchy.toml` declares the same (`runtime = []`).
    #[test]
    fn glamour_rune_has_an_empty_capability_footprint() {
        let fp = crate::capabilities::analyze(
            &parser::parse_module(GLAMOUR_SRC).expect("parse glamour"),
        );
        // `show_caps` renders the empty set as the literal `(none)`.
        assert_eq!(
            crate::capabilities::show_caps(&fp.total),
            "(none)",
            "glamour must demand NO capability — an empty footprint is RFC-0008's headline"
        );
        assert!(fp.total.is_empty(), "the footprint map itself must be empty");
        // And the manifest agrees: `runtime = []`.
        let toml = include_str!("../projects/glamour/witchy.toml");
        assert!(
            toml.contains("runtime = []"),
            "witchy.toml must declare an empty runtime footprint"
        );
    }

    /// RFC-0008: a hole in a FORBIDDEN position is a COMPILE error, not a runtime
    /// surprise. A `${hole}` used as a tag NAME makes the `html` tag `fail` at
    /// comptime with a message naming the problem — so the program never links.
    #[test]
    fn glamour_html_rejects_a_hole_in_tag_name_position() {
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let t = \"div\"\n\
                   \x20   let view = html\"<${t}>hi</${t}>\"\n\
                   \x20   print(console, to_html(view))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = parser::parse_module(GLAMOUR_SRC).expect("parse glamour");
        let modules = vec![("main".to_string(), entry), ("glamour".to_string(), glamour)];
        let err = crate::pipeline::link(modules, "main")
            .expect_err("a tag-name hole must be a compile error");
        assert!(
            err.to_string().contains("a tag NAME may not be a"),
            "the compile error must name the forbidden position, got: {err}"
        );
    }

    /// RFC-0006 hole-precise diagnostics: a type-wrong hole (an `Int` in TEXT
    /// position, which the `html` tag wraps in `glamour.text(…)` expecting a
    /// `String`) must report a type error whose LINE points INTO the literal — the
    /// `${5}` lives on the literal's line, not on the tag-emitted constructor or
    /// the desugared call. The marker-substitution machinery stamps each spliced
    /// hole with its captured source position so the diagnostic lands here.
    #[test]
    fn glamour_html_wrong_typed_hole_points_into_the_literal() {
        // The `html"…"` literal (with the `${5}` text hole) is on line 4.
        let src = "import glamour\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   let view = html\"<span>${5}</span>\"\n\
                   \x20   print(console, to_html(view))\n";
        let entry = parser::parse_module(src).expect("parse entry");
        let glamour = parser::parse_module(GLAMOUR_SRC).expect("parse glamour");
        let modules = vec![("main".to_string(), entry), ("glamour".to_string(), glamour)];
        let linked = crate::pipeline::link(modules, "main").expect("link (expansion succeeds)");
        let err = typeck::check(&linked)
            .expect_err("an Int in text position must be a type error (text holes need String)");
        let msg = err.to_string();
        assert!(
            msg.contains("line 4"),
            "the type error must point INTO the literal (line 4, where the `${{5}}` \
             hole lives), got: {msg}"
        );
    }

