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

    /// The parameter conventions (`var`/`let`/`own` + `move`) behave identically
    /// on both the interpreter and WASM backends — value semantics are
    /// preserved regardless of which knob the author reaches for. `var` writes
    /// back, `let` borrows (read-only), `own` consumes, a bare param is owned, and
    /// `move x` transfers ownership.
    #[test]
    fn conventions_backends_agree() {
        let src = "fn bump(var n: Int):\n    n = n + 1\n\nfn total(let xs: List(Int)) -> Int:\n    var s = 0\n    for x in xs:\n        s = s + x\n    s\n\nfn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn doubled(xs: List(Int)) -> Int:\n    list.at(xs, 0) * 2\n\nfn main(console: Console):\n    var c = 0\n    bump(c)\n    bump(c)\n    console.print(\"${c}\")\n    let nums = [10, 20, 30]\n    console.print(\"${total(nums)}\")\n    console.print(\"${doubled(nums)}\")\n    console.print(\"${list.length(nums)}\")\n    let g = [1, 2, 3, 4]\n    console.print(\"${drain(move g)}\")\n";
        let expected = ["2", "60", "20", "3", "4"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0056) Keyword arguments at a direct call site reorder to the callee's
    /// declared parameter order — resolved at the link layer, so both backends see
    /// the same positional call and agree. `label(n: 7, name: "ada")` binds `name`
    /// and `n` correctly despite the reversed written order.
    #[test]
    fn keyword_args_reorder_backends_agree() {
        let src = "fn label(name: String, n: Int) -> String:\n    \"${name}#${n}\"\n\nfn main(console: Console):\n    console.print(label(n: 7, name: \"ada\"))\n    console.print(label(\"bob\", n: 3))\n";
        let expected = ["ada#7", "bob#3"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-208, parity) A REORDERED labeled call whose reorder crosses a `var`
    /// parameter must still write back. The desugar temp-bound every reordered
    /// argument to an immutable `let __kwN`, so a `var` argument became ill-typed
    /// ("must be a mutable `var`") and leaked the synthetic `__kwN` into the error —
    /// legality depended on the order the labels were written. A `var` argument is a
    /// bare mutable variable with no evaluation effect, so it is now passed directly.
    #[test]
    fn keyword_args_var_reorder_writes_back() {
        // Reordered (`by:` before `xs:`) and in-order both mutate the caller's `var`.
        let reordered = "fn bump(var xs: List(Int), by: Int):\n    xs.push(by)\n    let _ = 0\n\nfn main(console: Console):\n    var xs: List(Int) = []\n    bump(by: 5, xs: xs)\n    bump(by: 7, xs: xs)\n    console.print(\"${xs}\")\n";
        assert_eq!(link_run(reordered), ["[5, 7]"], "interp reordered var write-back");
        assert_eq!(
            run_linked_on_wasm(&[("main", reordered)], "main"),
            ["[5, 7]"],
            "compiled reordered var write-back must agree",
        );
        // A reordered `own`/`move` argument still moves correctly (temp path intact).
        let owned = "fn eat(own s: String, n: Int) -> String:\n    s.repeat(n)\n\nfn main(console: Console):\n    let s = \"ab\"\n    console.print(eat(n: 3, s: move s))\n";
        assert_eq!(link_run(owned), ["ababab"], "interp reordered own/move");
        assert_eq!(
            run_linked_on_wasm(&[("main", owned)], "main"),
            ["ababab"],
            "compiled reordered own/move must agree",
        );
        // A genuinely non-mutable argument to a `var` param is still rejected — but
        // the diagnostic names the USER's variable, never a synthetic `__kwN` temp.
        let bad = "fn bump(var xs: List(Int), by: Int):\n    xs.push(by)\n    let _ = 0\n\nfn main(console: Console):\n    let ys: List(Int) = []\n    bump(by: 5, xs: ys)\n    console.print(\"${ys}\")\n";
        let module = parser::parse_module(bad).expect("parse");
        // `keyword_args::resolve` runs inside `pipeline::link`; the reorder now passes
        // the `var` argument directly, so typeck (not the desugar) reports the error.
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked)
            .expect_err("a `let` bound to a `var` param must be rejected")
            .message;
        assert!(err.contains("ys"), "diagnostic must name the user's variable: {err}");
        assert!(!err.contains("__kw"), "diagnostic must not leak a `__kwN` temp: {err}");
    }

    /// (RFC-0056) A labeled call evaluates its arguments in SOURCE order, not
    /// declared order: the desugar binds each written argument to a temp in the
    /// order written, then passes the temps in declared order. Here `b:` is written
    /// before `a:` but binds to the later parameter — the two effectful `side`
    /// calls must still print "first" before "second", identically on both backends.
    #[test]
    fn keyword_args_source_order_backends_agree() {
        let src = "fn record(console: Console, a: String, b: String) -> Nil:\n    console.print(\"a=${a} b=${b}\")\n\nfn side(console: Console, tag: String, ret: String) -> String:\n    console.print(\"eval ${tag}\")\n    ret\n\nfn main(console: Console):\n    record(console, b: side(console, \"first\", \"B\"), a: side(console, \"second\", \"A\"))\n";
        let expected = ["eval first", "eval second", "a=A b=B"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0056) A closed-constant default parameter is spliced in for an omitted
    /// argument at a direct call site. `connect("h", tls: false)` keeps the default
    /// `port = 443`; `connect("h", 8080)` overrides it positionally. Both backends
    /// see the fully-applied positional call and agree.
    #[test]
    fn keyword_args_default_backends_agree() {
        let src = "fn connect(host: String, port: Int = 443, tls: Bool = true) -> String:\n    \"${host}:${port} tls=${tls}\"\n\nfn main(console: Console):\n    console.print(connect(\"example.com\"))\n    console.print(connect(\"h\", tls: false))\n    console.print(connect(\"h\", 8080))\n";
        let expected = ["example.com:443 tls=true", "h:443 tls=false", "h:8080 tls=true"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-211) Named-field record construction is the same closed-constant
    /// shape as positional construction when every field value is closed. It is
    /// valid as a default argument and lowers before either backend sees it.
    #[test]
    fn keyword_args_default_accepts_named_field_record_constructor() {
        let src = "type Pt:\n    x: Int\n    y: Int\n\nfn score(p: Pt = Pt(y: 2, x: 40)) -> Int:\n    p.x + p.y\n\nfn main(console: Console):\n    console.print(\"${score()}\")\n    console.print(\"${score(Pt(x: 1, y: 2))}\")\n";
        let expected = ["42", "3"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0056) A `var` parameter cannot carry a default — there is no caller
    /// variable to write back to. Rejected loudly at parse time, identically for
    /// every consumer (both backends parse the same source).
    #[test]
    fn keyword_args_var_default_is_error() {
        let src = "fn inc(var n: Int = 0) -> Nil:\n    n = n + 1\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let err = parser::parse_module(src).expect_err("var + default must be rejected");
        assert!(
            format!("{err:?}").contains("`var` parameter cannot have a default"),
            "{err:?}"
        );
    }

    #[test]
    fn rfc0087_async_and_generator_var_parameters_are_rejected_explicitly() {
        for (src, kind) in [
            ("async fn bad(var state: Int) -> Nil:\n    return\n", "async"),
            ("gen fn bad(var state: Int) -> Iter(Int):\n    yield state\n", "generator"),
        ] {
            let err = parser::parse_module(src).expect_err("suspending `var` parameter");
            let message = format!("{err:?}");
            assert!(message.contains(kind), "diagnostic must name {kind}: {message}");
            assert!(message.contains("`var` parameter `state`"), "diagnostic: {message}");
            assert!(message.contains("suspension"), "diagnostic: {message}");
        }
    }

    /// (RFC-0056 v1) Keyword labels are excluded on UFCS method calls — the method
    /// callee resolves later (by receiver type, in traits.rs), so labels have no
    /// declaration to bind against yet. Rejected at parse time.
    #[test]
    fn keyword_args_method_label_is_error() {
        let src = "fn main(console: Console):\n    let s = \"hello\"\n    console.print(s.substring(start: 1))\n";
        let err = parser::parse_module(src).expect_err("method-call label must be rejected");
        assert!(
            format!("{err:?}").contains("not supported on method calls"),
            "{err:?}"
        );
    }

    /// (RFC-0056) A missing argument with no default is a link error naming the
    /// unbound parameter (the same shape record construction already reports for a
    /// missing field).
    #[test]
    fn keyword_args_missing_argument_is_link_error() {
        let src = "fn f(a: Int, b: Int) -> Int:\n    a + b\n\nfn main(console: Console):\n    print_int(f(a: 1))\n";
        let module = parser::parse_module(src).expect("parse");
        let err = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect_err("missing argument must be a link error");
        assert!(format!("{err}").contains("missing argument `b`"), "{err}");
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

    /// (BUG-366) Lazy iterator adapters must not recurse once per skipped
    /// element inside a single pull. Long rejected prefixes should behave like
    /// ordinary loop work on both backends.
    #[test]
    fn iterator_skip_adapters_handle_long_prefixes_on_both_backends() {
        let cases = [
            (
                "filter",
                "import iter\n\nfn even_after(n: Int) -> Bool:\n    n >= 1000 && n % 2 == 0\n\nfn main(console: Console):\n    match iter.range(0, 1002).filter(even_after).split_first():\n        Some(pair) ->\n            let (x, _rest) = pair\n            console.print(\"${x}\")\n        None -> console.print(\"missing\")\n",
                ["1000"],
            ),
            (
                "filter_map",
                "import iter\nimport option\n\nfn only_after(n: Int) -> Option(Int):\n    if n >= 1000:\n        Some(n + 1)\n    else:\n        None\n\nfn main(console: Console):\n    match iter.range(0, 1001).filter_map(only_after).split_first():\n        Some(pair) ->\n            let (x, _rest) = pair\n            console.print(\"${x}\")\n        None -> console.print(\"missing\")\n",
                ["1001"],
            ),
            (
                "drop_while",
                "import iter\n\nfn main(console: Console):\n    match iter.range(0, 1002).drop_while(fn(n: Int): n < 1000).split_first():\n        Some(pair) ->\n            let (x, _rest) = pair\n            console.print(\"${x}\")\n        None -> console.print(\"missing\")\n",
                ["1000"],
            ),
            (
                "flat_map",
                "import iter\n\nfn empty_until_last(n: Int) -> Iter(Int):\n    if n < 1000:\n        iter.empty()\n    else:\n        iter.once(n)\n\nfn main(console: Console):\n    match iter.range(0, 1001).flat_map(empty_until_last).split_first():\n        Some(pair) ->\n            let (x, _rest) = pair\n            console.print(\"${x}\")\n        None -> console.print(\"missing\")\n",
                ["1000"],
            ),
        ];
        for (label, src, expected) in cases {
            assert_eq!(link_run(src), expected, "interp: {label}");
            assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: {label}");
        }
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

    /// (Bytes) The first-class `Bytes` type: a UTF-8-free flat byte buffer. Exercises
    /// the round-trip with `String`, checked `from_list`, length/at/get/concat/slice/to_list/search, on
    /// both backends (linked interp + compiled WASM), which must agree — `Bytes` shares `String`'s
    /// `[len][bytes]` layout, so the compiled ops are identity/String-reuse.
    #[test]
    fn bytes_type_backends_agree() {
        let src = "import bytes\nimport list\nimport option\nimport result\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${bytes.length(b)}\")\n    console.print(\"${bytes.at(b, 0)}\")\n    console.print(\"${bytes.get(b, 1).unwrap_or(0)}\")\n    console.print(\"${bytes.get(b, 99).unwrap_or(0 - 1)}\")\n    console.print(bytes.to_string(b))\n    let c = bytes.concat(b, bytes.from_string(\"?\"))\n    console.print(bytes.to_string_lossy(c))\n    console.print(bytes.to_string(bytes.slice(c, 1, 3)))\n    console.print(\"${bytes.to_list(b)}\")\n    let raw = result.unwrap_or(bytes.from_list([0, 255, 65]), bytes.from_string(\"\"))\n    console.print(\"${bytes.to_list(raw)}\")\n    match bytes.decode_utf8(raw):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(bytes.bytes_error_message(e))\n    match bytes.from_list([0 - 1]):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(bytes.bytes_error_message(e))\n    match bytes.from_list([256]):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(bytes.bytes_error_message(e))\n    console.print(\"${bytes.is_empty(b)}\")\n    console.print(\"${bytes.index_of(c, bytes.from_string(\"i!\"))}\")\n    console.print(\"${bytes.index_of(c, bytes.from_string(\"zz\"))}\")\n    console.print(\"${bytes.contains(c, bytes.from_string(\"!?\"))}\")\n    console.print(\"${bytes.starts_with(c, b)}\")\n    console.print(\"${bytes.ends_with(c, bytes.from_string(\"!?\"))}\")\n";
        let expected = [
            "3",
            "104",
            "105",
            "-1",
            "hi!",
            "hi!?",
            "i!",
            "[104, 105, 33]",
            "[0, 255, 65]",
            "bytes.decode_utf8: invalid UTF-8",
            "bytes.from_list: value -1 is outside 0..=255",
            "bytes.from_list: value 256 is outside 0..=255",
            "false",
            "Some(1)",
            "None",
            "true",
            "true",
            "true",
        ];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0050) Bytes has a real inherent-method surface like the other
    /// standard value types; module functions remain callable for explicit
    /// module use and as first-class values.
    #[test]
    fn bytes_methods_cover_primary_surface_on_both_backends() {
        let src = "import bytes\nimport result\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${b.length()}\")\n    console.print(\"${b.at(0)}\")\n    console.print(\"${b.get(1).unwrap_or(0)}\")\n    console.print(\"${b.get(99).unwrap_or(0 - 1)}\")\n    console.print(b.to_string())\n    let c = b.concat(bytes.from_string(\"?\"))\n    console.print(c.to_string_lossy())\n    console.print(c.slice(1, 3).to_string())\n    console.print(\"${b.to_list()}\")\n    let raw = result.unwrap_or(bytes.from_list([0, 255, 65]), bytes.from_string(\"\"))\n    console.print(\"${raw.to_list()}\")\n    match raw.decode_utf8():\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(bytes.bytes_error_message(e))\n    match raw.decode_utf8_string():\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(e)\n    console.print(\"${b.is_empty()}\")\n    console.print(\"${c.index_of(bytes.from_string(\"i!\"))}\")\n    console.print(\"${c.index_of(bytes.from_string(\"zz\"))}\")\n    console.print(\"${c.contains(bytes.from_string(\"!?\"))}\")\n    console.print(\"${c.starts_with(b)}\")\n    console.print(\"${c.ends_with(bytes.from_string(\"!?\"))}\")\n    console.print(\"${bytes.length(b)}\")\n";
        let expected = [
            "3",
            "104",
            "105",
            "-1",
            "hi!",
            "hi!?",
            "i!",
            "[104, 105, 33]",
            "[0, 255, 65]",
            "bytes.decode_utf8: invalid UTF-8",
            "bytes.decode_utf8: invalid UTF-8",
            "false",
            "Some(1)",
            "None",
            "true",
            "true",
            "true",
            "3",
        ];
        assert_eq!(link_run(src), expected, "interp: bytes methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: bytes methods",
        );
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

    /// (parity, SEC-040) `bytes.slice` is BYTE-indexed and `bytes.to_string` is
    /// LOSSY on BOTH backends, while `bytes.decode_utf8` is strict. The compiled
    /// `bytes.slice` used to route through the CHAR-indexed `$str_substring` (so
    /// slicing a multibyte payload returned the wrong byte count — a
    /// binary-corruption primitive) and `bytes.to_string` was a raw identity (so
    /// invalid UTF-8 came back verbatim instead of the U+FFFD the interpreter's
    /// `from_utf8_lossy` produces). Both now match the byte-exact interpreter
    /// oracle. Same family as SEC-038 (the `bytes.at` OOB read).
    #[test]
    fn bytes_slice_is_byte_indexed_and_to_string_is_lossy() {
        // `é` is 2 UTF-8 bytes (0xC3 0xA9). Byte-slicing [0,1) yields ONE byte
        // (the interpreter's answer); the old char-indexed slice returned 2.
        let slice_src = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"héllo\")\n    console.print(\"${bytes.length(bytes.slice(b, 0, 1))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 1, 3))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 0, 100))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 3, 1))}\")\n";
        // "héllo" = h(1) é(2) l(1) l(1) o(1) = 6 bytes. slice(0,1)=1, slice(1,3)=2
        // (the two bytes of é), slice(0,100) clamps to 6, slice(3,1) empty -> 0.
        let want_slice = ["1", "2", "6", "0"];
        assert_eq!(link_run(slice_src), want_slice, "interp bytes.slice is byte-indexed");
        assert_eq!(
            run_linked_on_wasm(&[("main", slice_src)], "main"),
            want_slice,
            "compiled bytes.slice must be byte-indexed too"
        );

        // Slicing `é` at [0,1) leaves a lone 0xC3 — invalid UTF-8. `to_string` must
        // lossily decode it to U+FFFD (3 bytes) on both backends, not return the
        // raw invalid byte.
        let lossy_src = "import bytes\n\nfn main(console: Console):\n    let half = bytes.slice(bytes.from_string(\"é\"), 0, 1)\n    let s = bytes.to_string_lossy(half)\n    console.print(\"${s.length()}\")\n    console.print(\"${bytes.length(bytes.from_string(s))}\")\n    match bytes.decode_utf8(half):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(bytes.bytes_error_message(e))\n    match bytes.decode_utf8(bytes.from_string(\"ok\")):\n        Ok(text) -> console.print(text)\n        Err(e) -> console.print(\"bad\")\n";
        // The lossy decode replaces the lone invalid byte with U+FFFD, which is 3
        // UTF-8 bytes (`string.length` is a BYTE count). The old buggy compiled
        // identity returned the single raw byte, so both readings would be "1".
        let want_lossy = ["3", "3", "bytes.decode_utf8: invalid UTF-8", "ok"];
        assert_eq!(link_run(lossy_src), want_lossy, "interp bytes.to_string is lossy");
        assert_eq!(
            run_linked_on_wasm(&[("main", lossy_src)], "main"),
            want_lossy,
            "compiled bytes.to_string must lossily decode to U+FFFD too"
        );
    }

    /// (BUG-392, parity) `bytes.slice` bounds are clamped in i64 on BOTH backends.
    /// The compiled `$bytes_slice` used to narrow `start`/`end` to i32 BEFORE
    /// clamping, so a large positive bound wrapped negative: `slice(b, 0, 2^31)`
    /// returned the FULL buffer on the interpreter (its `Int` clamp saw 2^31 > len)
    /// but an EMPTY slice compiled (2^31 truncated to a negative i32 clamped up to
    /// `lo`). Now both clamp the full `Int` first (like `$bytes_at`/`$list_at`).
    #[test]
    fn bytes_slice_clamps_bounds_in_i64_on_both_backends() {
        // "hello" = 5 bytes. Large positive `end` clamps to len (full buffer);
        // an out-of-i32-range `[start, end)` yields empty without wrapping into an
        // in-bounds slice; a large-magnitude negative `start` clamps up to 0.
        let src = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hello\")\n    console.print(\"${bytes.length(bytes.slice(b, 0, 2147483648))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 2147483648, 2147483649))}\")\n    console.print(\"${bytes.length(bytes.slice(b, 0 - 2147483648, 2))}\")\n    console.print(bytes.to_string(bytes.slice(b, 1, 3)))\n";
        let expected = ["5", "0", "2", "el"];
        assert_eq!(link_run(src), expected, "interp clamps bytes.slice bounds in i64");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled bytes.slice must clamp bounds in i64 like the interpreter",
        );
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

    /// (BUG-498, parity) `derive(PartialOrd)` must compose through each field's
    /// `partial_compare`, not through `<`/`>` tests that accidentally treat
    /// incomparable fields as equal. A Float NaN field should propagate `None`
    /// on both backends.
    #[test]
    fn derive_partial_ord_float_field_propagates_none_on_both_backends() {
        let src = "import cmp\n\ntype Reading derive(PartialEq, PartialOrd):\n    value: Float\n\nfn describe(o: Option(Ordering)) -> String:\n    match o:\n        None -> \"none\"\n        Some(Less) -> \"less\"\n        Some(Equal) -> \"equal\"\n        Some(Greater) -> \"greater\"\n\nfn main(console: Console):\n    console.print(describe(partial_compare(Reading(0.0 / 0.0), Reading(1.0))))\n    console.print(describe(partial_compare(Reading(1.0), Reading(2.0))))\n    console.print(describe(partial_compare(Reading(2.0), Reading(1.0))))\n    console.print(describe(partial_compare(Reading(2.0), Reading(2.0))))\n";
        let expected = ["none", "less", "greater", "equal"];
        assert_eq!(link_run(src), expected, "interp: derived PartialOrd propagates None");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: derived PartialOrd propagates None",
        );
    }

    /// (BUG-468) `Eq` refines `PartialEq`, so `derive(Eq)` must generate the
    /// structural `PartialEq` impl too. The explicit `derive(PartialEq, Eq)`
    /// spelling remains valid and must not generate duplicate impl heads.
    #[test]
    fn derive_eq_alone_implies_partial_eq_on_both_backends() {
        let src = "import cmp\n\ntype OnlyEq derive(Eq):\n    x: Int\n\ntype Both derive(PartialEq, Eq):\n    x: Int\n\nfn main(console: Console):\n    console.print(\"${OnlyEq(1) == OnlyEq(1)}\")\n    console.print(\"${OnlyEq(1) == OnlyEq(2)}\")\n    console.print(\"${Both(1) == Both(1)}\")\n";
        let expected = ["true", "false", "true"];
        assert_eq!(link_run(src), expected, "interp: derive(Eq) implies PartialEq");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: derive(Eq) implies PartialEq",
        );
    }

    /// (BUG-478) `derive(Eq)` is a marker when the type already has a custom
    /// `PartialEq`. It must not mark that `PartialEq` as structural, because
    /// nested equality then stops calling the hand-written semantics.
    #[test]
    fn derive_eq_marker_preserves_custom_partial_eq_at_depth_on_both_backends() {
        let src = "import cmp\n\ntype Key derive(Eq):\n    id: Int\n    cache: Int\n\nimpl PartialEq for Key:\n    fn eq(self, other: Key) -> Bool:\n        self.id == other.id\n\ntype Wrapper derive(PartialEq, Eq):\n    key: Key\n\nfn a() -> Key:\n    Key(1, 10)\n\nfn b() -> Key:\n    Key(1, 20)\n\nfn main(console: Console):\n    console.print(\"${a() == b()}\")\n    console.print(\"${[a()] == [b()]}\")\n    console.print(\"${Some(a()) == Some(b())}\")\n    console.print(\"${(a(), 1) == (b(), 1)}\")\n    console.print(\"${Wrapper(a()) == Wrapper(b())}\")\n";
        let expected = ["true", "true", "true", "true", "true"];
        assert_eq!(
            link_run(src),
            expected,
            "interp: derive(Eq) marker must preserve custom PartialEq",
        );
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: derive(Eq) marker must preserve custom PartialEq",
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

    /// (BUG-535) Lists are ordinary comparison-protocol values: if their elements
    /// satisfy `PartialEq`/`Eq`, the list itself satisfies the same bound instead
    /// of relying on one-off direct-operator magic.
    #[test]
    fn list_equality_satisfies_partial_eq_bounds_on_both_backends() {
        let src = "import cmp\nimport testing\n\ntype Key derive(Show, Eq):\n    id: Int\n    cache: Int\n\nimpl PartialEq for Key:\n    fn eq(self, other: Key) -> Bool:\n        self.id == other.id\n\nfn same(x: a, y: a) -> Bool where a: PartialEq:\n    x == y\n\nfn total_same(x: a, y: a) -> Bool where a: Eq:\n    x == y\n\nfn main(console: Console):\n    console.print(\"${same([1, 2, 3], [1, 2, 3])}\")\n    console.print(\"${same([Key(1, 10)], [Key(1, 20)])}\")\n    console.print(\"${total_same([Key(1, 10)], [Key(1, 20)])}\")\n    testing.assert_value_eq([Key(1, 10)], [Key(1, 20)])\n";
        let expected = ["true", "true", "true"];
        assert_eq!(link_run(src), expected, "interp: list PartialEq bounds");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: list PartialEq bounds",
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

    /// Dicts should be ordinary protocol values too: direct dict equality is not
    /// enough if a generic helper cannot ask for `PartialEq`/`Eq` over a concrete
    /// `Dict(k, v)`.
    #[test]
    fn dict_equality_satisfies_protocol_bounds_on_both_backends() {
        let src = "import cmp\nimport dict\nimport testing\n\ntype Key derive(Show, Eq):\n    id: Int\n    cache: Int\n\nimpl PartialEq for Key:\n    fn eq(self, other: Key) -> Bool:\n        self.id == other.id\n\ntype Val derive(Show, Eq):\n    label: String\n    noise: Int\n\nimpl PartialEq for Val:\n    fn eq(self, other: Val) -> Bool:\n        self.label == other.label\n\nfn same(x: a, y: a) -> Bool where a: PartialEq:\n    x == y\n\nfn total_same(x: a, y: a) -> Bool where a: Eq:\n    x == y\n\nfn make_left() -> Dict(Key, Val):\n    var d = dict.new()\n    dict.insert(d, Key(1, 10), Val(\"one\", 100))\n    dict.insert(d, Key(2, 20), Val(\"two\", 200))\n    d\n\nfn make_right() -> Dict(Key, Val):\n    var d = dict.new()\n    dict.insert(d, Key(2, 99), Val(\"two\", 999))\n    dict.insert(d, Key(1, 42), Val(\"one\", 111))\n    d\n\nfn main(console: Console):\n    let left = make_left()\n    let right = make_right()\n    console.print(\"${left == right}\")\n    console.print(\"${same(left, right)}\")\n    console.print(\"${total_same(left, right)}\")\n    testing.assert_value_eq(left, right)\n";
        let expected = ["true", "true", "true"];
        assert_eq!(link_run(src), expected, "interp: dict PartialEq bounds");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: dict PartialEq bounds",
        );
    }

    /// (BUG-557) A generic container equality specialization can need a second
    /// generated generic impl for the element type. List(tuple5) must therefore
    /// emit the tuple `PartialEq` specialization even when no source call compares
    /// the tuple directly.
    #[test]
    fn list_of_tuple_equality_satisfies_protocol_bounds_on_both_backends() {
        let src = "import cmp\n\nfn total_same(x: a, y: a) -> Bool where a: Eq:\n    x == y\n\nfn main(console: Console):\n    let xs = [(1, \"x\", true, 90s, Greater)]\n    let ys = [(1, \"x\", true, 90s, Greater)]\n    let zs = [(1, \"x\", false, 90s, Greater)]\n    console.print(\"${total_same(xs, ys)}\")\n    console.print(\"${total_same(xs, zs)}\")\n";
        let expected = ["true", "false"];
        assert_eq!(link_run(src), expected, "interp: List(tuple5) Eq");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: List(tuple5) Eq",
        );
    }

    /// (BUG-395 / RFC-0047) Public `std/dict` helpers expose the same key
    /// equality contract as direct native dict operations. Concrete key/value
    /// types without `Eq` cannot route through `dict.get`, `from_pairs`,
    /// `map_values`, `filter`, `merge`, or `invert` on the public `witchy check`
    /// path (type check + compiled-backend acceptance); supported `Eq` key
    /// shapes can.
    #[test]
    fn dict_wrapper_key_operations_require_visible_eq_bounds() {
        let resolve_fs_std = |src: &str| -> ast::Module {
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
                    let source = std::fs::read_to_string(format!("std/{name}.witchy"))
                        .expect("std module source");
                    let parsed = parser::parse_module(&source).expect("parse std module");
                    queue.push_back(parsed.clone());
                    modules.push((name, parsed));
                }
            }
            crate::pipeline::link(modules, "main").expect("link")
        };

        let rejected = [
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn main(console: Console):\n    let d: Dict(Key, Int) = dict.new()\n    let _x = d.get(Key(1))\n    console.print(\"bad\")\n",
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn main(console: Console):\n    let _x = dict.from_pairs([(Key(1), 1)])\n    console.print(\"bad\")\n",
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn id(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    let d: Dict(Key, Int) = dict.new()\n    let _x = d.map_values(id)\n    console.print(\"bad\")\n",
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn keep(_k: Key, _v: Int) -> Bool:\n    true\n\nfn main(console: Console):\n    let d: Dict(Key, Int) = dict.new()\n    let _x = d.filter(keep)\n    console.print(\"bad\")\n",
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn main(console: Console):\n    let d: Dict(Key, Int) = dict.new()\n    let _x = d.merge(d)\n    console.print(\"bad\")\n",
            "import dict\n\ntype Value:\n    Value(Int)\n\nfn main(console: Console):\n    let d: Dict(String, Value) = dict.new()\n    let _x = d.invert()\n    console.print(\"bad\")\n",
        ];
        for src in rejected {
            let linked = resolve_fs_std(src);
            match typeck::check(&linked) {
                Err(err) => assert!(
                    err.message.contains("Eq"),
                    "expected visible Eq-bound error, got: {}",
                    err.message
                ),
                Ok(()) => {
                    let result = codegen::compile_module_binary(&linked);
                    assert!(
                        matches!(result, codegen::LoweringOutcome::Rejected(_)),
                        "non-Eq dict wrapper must be a hard compiled rejection"
                    );
                }
            }
        }

        let erased_wrapper = "import dict\n\npub fn wrapped(d: Dict(k, v), key: k) -> Option(v):\n    d.get(key)\n";
        let linked = resolve_fs_std(erased_wrapper);
        let err = typeck::check(&linked).expect_err("generic wrapper must forward dict.get's Eq bound");
        assert!(err.message.contains("requires `k: Eq`"), "expected forwarded Eq-bound error, got: {}", err.message);

        let bounded_wrapper = "import dict\n\npub fn wrapped(d: Dict(k, v), key: k) -> Option(v) where k: Eq:\n    d.get(key)\n";
        let linked = resolve_fs_std(bounded_wrapper);
        typeck::check(&linked).expect("generic wrapper can forward dict.get's Eq bound");

        let accepted = "import dict\n\nfn id(x: Int) -> Int:\n    x\n\nfn keep(_k: String, _v: Int) -> Bool:\n    true\n\nfn main(console: Console):\n    let d: Dict(String, Int) = dict.new()\n    let values: Dict(String, Int) = dict.new()\n    let _a = d.get(\"one\")\n    let _b = dict.from_pairs([(\"one\", 1)])\n    let _c = d.map_values(id)\n    let _d = d.filter(keep)\n    let _e = d.merge(d)\n    let _f = values.invert()\n    console.print(\"ok\")\n";
        let linked = resolve_fs_std(accepted);
        typeck::check(&linked).expect("bounded dict wrappers type-check");
        codegen::compile_module_binary(&linked)
            .expect_lowered("bounded dict wrappers lower");
    }

    /// (BUG-544) `Ordering` is ordinary std data: it renders through `Show`,
    /// reflects as a nullary variant, and therefore serializes through JSON
    /// reflection, including when it appears in a derived-reflect record.
    #[test]
    fn ordering_is_showable_and_reflectable_on_both_backends() {
        let src = "import cmp\nimport show\nimport reflect\nimport json\n\ntype SortStep derive(Reflect):\n    ordering: Ordering\n\nfn main(console: Console):\n    let o: Ordering = cmp.reverse(Greater)\n    show.say(console, o)\n    console.print(reflect.debug(o))\n    console.print(json.stringify(o))\n    console.print(json.stringify(SortStep(o)))\n";
        let expected = [
            "Less",
            "Less",
            "{\"$variant\":\"Less\",\"$values\":[]}",
            "{\"ordering\":{\"$variant\":\"Less\",\"$values\":[]}}",
        ];
        assert_eq!(link_run(src), expected, "interp: Ordering protocols");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Ordering protocols");
    }

    /// (BUG-545) A decoded/built `JsonObject` reflects as the JSON object shape,
    /// not as the `JsonObject(...)` constructor. Its debug rendering should
    /// therefore look like an object, not like an accidental nameless record with
    /// a leading space.
    #[test]
    fn json_object_debug_renders_as_object_on_both_backends() {
        let src = "import json\nimport reflect\n\nfn main(console: Console):\n    let obj = json.JsonObject([(\"ok\", json.JsonBool(true)), (\"n\", json.JsonInt(2))])\n    let arr = json.JsonArray([json.JsonInt(1), json.JsonString(\"x\")])\n    console.print(reflect.debug(obj))\n    console.print(reflect.debug(arr))\n    console.print(json.stringify(obj))\n";
        let expected = [
            "{ ok: true, n: 2 }",
            "[1, \"x\"]",
            "{\"ok\":true,\"n\":2}",
        ];
        assert_eq!(link_run(src), expected, "interp: Json debug object shape");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Json debug object shape");
    }

    /// (BUG-483) JSON keys are arbitrary strings, so nested lookup needs an exact
    /// segment API in addition to the dotted-string convenience helper.
    #[test]
    fn json_get_in_reaches_literal_dot_keys_on_both_backends() {
        let src = "import json\n\nfn show(console: Console, v: Option(json.Json)):\n    match v:\n        Some(j) -> console.print(json.encode(j))\n        None -> console.print(\"missing\")\n\nfn main(console: Console):\n    let obj = json.JsonObject([(\"a.b\", json.JsonInt(1)), (\"a\", json.JsonObject([(\"b\", json.JsonInt(2))])), (\"\", json.JsonObject([(\"x.y\", json.JsonInt(3))]))])\n    show(console, json.get_in(obj, [\"a.b\"]))\n    show(console, json.get_path(obj, \"a.b\"))\n    show(console, json.get_in(obj, [\"\", \"x.y\"]))\n    show(console, json.get_in(obj, [\"missing.dot\"]))\n";
        let expected = ["1", "2", "3", "missing"];
        assert_eq!(link_run(src), expected, "interp: Json exact path segments");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Json exact path segments");
    }

    /// (BUG-262) JSON decode rejects duplicate object names, and the public
    /// helper/encoding boundaries must not let hand-built duplicate objects become
    /// signed or emitted wire JSON silently.
    #[test]
    fn json_duplicate_object_keys_fail_at_encoding_boundaries_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        let cases = [
            (
                "encode",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"aud\", json.JsonString(\"good\")), (\"aud\", json.JsonString(\"evil\"))])\n    console.print(json.encode(j))\n",
                "json.encode: duplicate object key `aud`",
            ),
            (
                "pretty",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"aud\", json.JsonString(\"good\")), (\"aud\", json.JsonString(\"evil\"))])\n    console.print(json.encode_pretty(j))\n",
                "json.encode_pretty: duplicate object key `aud`",
            ),
            (
                "object_sorted",
                "import json\n\nfn main(console: Console):\n    let j = json.object_sorted([(\"kid\", json.JsonString(\"a\")), (\"kid\", json.JsonString(\"b\"))])\n    console.print(json.encode(j))\n",
                "json.object_sorted: duplicate object key `kid`",
            ),
            (
                "merge left",
                "import json\n\nfn main(console: Console):\n    let left = json.JsonObject([(\"a\", json.JsonInt(1)), (\"a\", json.JsonInt(2))])\n    let right = json.JsonObject([(\"b\", json.JsonInt(3))])\n    console.print(json.encode(json.merge(left, right)))\n",
                "json.merge: duplicate object key `a`",
            ),
            (
                "merge right",
                "import json\n\nfn main(console: Console):\n    let left = json.JsonObject([(\"a\", json.JsonInt(1))])\n    let right = json.JsonObject([(\"b\", json.JsonInt(2)), (\"b\", json.JsonInt(3))])\n    console.print(json.encode(json.merge(left, right)))\n",
                "json.merge: duplicate object key `b`",
            ),
            (
                "get",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"aud\", json.JsonString(\"good\")), (\"aud\", json.JsonString(\"evil\"))])\n    let _ = json.get(j, \"aud\")\n    console.print(\"bad\")\n",
                "json.get: duplicate object key `aud`",
            ),
            (
                "typed accessor",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"aud\", json.JsonString(\"good\")), (\"aud\", json.JsonString(\"evil\"))])\n    let _ = json.get_string(j, \"aud\")\n    console.print(\"bad\")\n",
                "json.get: duplicate object key `aud`",
            ),
            (
                "contains_key",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"kid\", json.JsonString(\"a\")), (\"kid\", json.JsonString(\"b\"))])\n    console.print(\"${json.contains_key(j, \"kid\")}\")\n",
                "json.contains_key: duplicate object key `kid`",
            ),
            (
                "as_object",
                "import json\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"kid\", json.JsonString(\"a\")), (\"kid\", json.JsonString(\"b\"))])\n    let _ = json.as_object(j)\n    console.print(\"bad\")\n",
                "json.as_object: duplicate object key `kid`",
            ),
            (
                "reflect",
                "import json\nimport reflect\n\nfn main(console: Console):\n    let j = json.JsonObject([(\"kid\", json.JsonString(\"a\")), (\"kid\", json.JsonString(\"b\"))])\n    console.print(reflect.debug(j))\n",
                "json.reflect: duplicate object key `kid`",
            ),
        ];
        for (label, src, expected_msg) in cases {
            let (linked, wasm) = compile(src);
            let interp_err = interpreter::run_module(linked, ".", Vec::new())
                .expect_err("interpreter must abort on duplicate JSON object keys")
                .to_string();
            assert!(interp_err.contains(expected_msg), "{label}: {interp_err}");
            let wasm_err = crate::run_wasm_bytes(&wasm)
                .expect_err("WASM must abort on duplicate JSON object keys")
                .to_string();
            assert!(wasm_err.contains(expected_msg), "{label}: {wasm_err}");
        }

        let ok = "import json\n\nfn main(console: Console):\n    let left = json.JsonObject([(\"a\", json.JsonInt(1)), (\"b\", json.JsonInt(2))])\n    let right = json.JsonObject([(\"b\", json.JsonInt(3)), (\"c\", json.JsonInt(4))])\n    console.print(json.encode(json.merge(left, right)))\n";
        let expected = ["{\"a\":1,\"b\":3,\"c\":4}"];
        assert_eq!(link_run(ok), expected, "interp: unique JSON merge still works");
        assert_eq!(run_linked_on_wasm(&[("main", ok)], "main"), expected, "compiled: unique JSON merge still works");
    }

    /// (BUG-374) JSON has no NaN/Infinity tokens, and `null` already means an
    /// intentional JSON null / `Option.None`. Strict JSON boundaries must fail
    /// loudly instead of silently erasing non-finite Float values to null.
    #[test]
    fn json_nonfinite_float_encoding_aborts_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        let cases = [
            (
                "direct NaN",
                "import json\n\nfn main(console: Console):\n    console.print(json.encode(json.JsonFloat(0.0 / 0.0)))\n",
            ),
            (
                "direct infinity",
                "import json\n\nfn main(console: Console):\n    console.print(json.encode(json.JsonFloat(1.0 / 0.0)))\n",
            ),
            (
                "reflective field",
                "import json\nimport reflect\n\ntype Reading derive(Reflect):\n    ratio: Float\n\nfn main(console: Console):\n    console.print(json.stringify(Reading(0.0 / 0.0)))\n",
            ),
        ];
        for (label, src) in cases {
            let (linked, wasm) = compile(src);
            let interp_err = interpreter::run_module(linked, ".", Vec::new())
                .expect_err("interpreter must abort on non-finite JSON Float")
                .to_string();
            assert!(
                interp_err.contains("json.encode: non-finite Float cannot be encoded as JSON"),
                "{label}: {interp_err}"
            );
            let wasm_err = crate::run_wasm_bytes(&wasm)
                .expect_err("WASM must abort on non-finite JSON Float")
                .to_string();
            assert!(
                wasm_err.contains("json.encode: non-finite Float cannot be encoded as JSON"),
                "{label}: {wasm_err}"
            );
        }
    }

    /// (BUG-370) `reflect.debug` strings escape every C0 control, matching JSON's
    /// discipline instead of emitting raw terminal controls into structural text.
    #[test]
    fn reflect_debug_escapes_all_c0_controls_on_both_backends() {
        let src = "import reflect\nimport string\n\ntype Note derive(Reflect):\n    text: String\n\nfn main(console: Console):\n    console.print(reflect.debug(\"a\" + string.from_code(8) + \"b\"))\n    console.print(reflect.debug(\"a\" + string.from_code(12) + \"b\"))\n    console.print(reflect.debug(\"a\" + string.from_code(0) + \"b\"))\n    console.print(reflect.debug(Note(\"x\" + string.from_code(27) + \"y\")))\n";
        let expected = [
            "\"a\\bb\"",
            "\"a\\fb\"",
            "\"a\\u0000b\"",
            "Note { text: \"x\\u001by\" }",
        ];
        assert_eq!(link_run(src), expected, "interp: reflect.debug C0 escapes");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: reflect.debug C0 escapes");
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
    fn rfc0054_json_decode_uses_typed_error_and_converts_to_string() {
        let src = "import json\nfrom json import Json\nimport show\n\nfn via_string() -> Result(Json, String):\n    let doc = json.decode(\"1 2\")?\n    Ok(doc)\n\nfn main(console: Console):\n    match json.decode(\"1 2\"):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) ->\n            console.print(json.decode_error_message(e))\n            console.print(show.render(e))\n    match via_string():\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(e)\n";
        let expected = [
            "unexpected trailing content at 2",
            "unexpected trailing content at 2",
            "unexpected trailing content at 2",
        ];
        assert_eq!(link_run(src), expected, "interp: typed json.DecodeError");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: typed json.DecodeError",
        );
    }

    #[test]
    fn rfc0054_server_json_body_uses_typed_error_and_string_bridge() {
        let src = r#"import json
import server
from http import Request
from json import Json

fn typed(req: Request) -> Result(Json, json.DecodeError):
    server.json_body(req)

fn via_string(req: Request) -> Result(Json, String):
    let doc = server.json_body(req)?
    Ok(doc)

fn main(console: Console):
    let good = Request("POST", "/", [], [], [], "{\"ok\":true}")
    let bad = Request("POST", "/", [], [], [], "1 2")
    match typed(good):
        Ok(doc) -> console.print(json.encode(doc))
        Err(e) -> console.print(json.decode_error_message(e))
    match typed(bad):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(json.decode_error_message(e))
    match via_string(bad):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
    match server.json_body_string(bad):
        Ok(_) -> console.print("bad")
        Err(e) -> console.print(e)
"#;
        let expected = [
            "{\"ok\":true}",
            "unexpected trailing content at 2",
            "unexpected trailing content at 2",
            "unexpected trailing content at 2",
        ];
        assert_eq!(link_run(src), expected, "interp: server.json_body typed error");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: server.json_body typed error",
        );
    }

    #[test]
    fn rfc0054_server_parse_request_uses_typed_error_and_response_bridge() {
        let src = r#"import http
import server
import show

fn classify(e: server.RequestParseError) -> String:
    match e:
        server.UnsupportedTransferEncoding -> "transfer"
        server.ConflictingContentLength -> "length"
        server.BadRequestLine -> "badline"

fn via_string(raw: String) -> Result(http.Request, String):
    let req = server.parse_request(raw)?
    Ok(req)

fn main(console: Console):
    let conflict = "POST /x HTTP/1.1\r\nContent-Length: 3\r\nContent-Length: 5\r\n\r\nabc"
    let chunked = "POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ntest\r\n0\r\n\r\n"
    match server.parse_request(conflict):
        Ok(_r) -> console.print("bad")
        Err(e) ->
            console.print(classify(e))
            console.print(server.request_parse_error_message(e))
            console.print(show.render(e))
    match via_string(chunked):
        Ok(_r) -> console.print("bad")
        Err(e) -> console.print(e)
    match server.parse_request_response(chunked):
        Ok(_r) -> console.print("bad")
        Err(resp) -> console.print("response:${http.status(resp)}:" + http.body(resp))
"#;
        let expected = [
            "length",
            "conflicting Content-Length headers",
            "conflicting Content-Length headers",
            "unsupported Transfer-Encoding",
            "response:400:unsupported Transfer-Encoding",
        ];
        assert_eq!(link_run(src), expected, "interp: server.parse_request typed error");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: server.parse_request typed error",
        );
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

    /// (BUG-539) `Bytes` is ordinary core data, so it must participate in the
    /// public display and reflection protocols instead of being printable only by
    /// the interpreter's private `Value::Display` path. `Show` stays concise and
    /// non-lossy; reflection exposes raw byte values for debug/JSON consumers.
    #[test]
    fn bytes_are_showable_reflectable_and_renderable_on_both_backends() {
        let src = "import bytes\nimport show\nimport reflect\nimport json\n\ntype Packet derive(Reflect):\n    payload: Bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi\")\n    show.say(console, b)\n    console.print(show.render([b]))\n    console.print(\"${b}\")\n    console.print(reflect.debug(b))\n    console.print(json.stringify(Packet(b)))\n";
        let expected = [
            "Bytes(len=2)",
            "[Bytes(len=2)]",
            "Bytes(len=2)",
            "[104, 105]",
            "{\"payload\":[104,105]}",
        ];
        assert_eq!(link_run(src), expected, "interp: Bytes protocols");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Bytes protocols");

        let raw = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi\")\n    console.print(\"${b}\")\n    console.print(\"${bytes.concat(b, b)}\")\n";
        let raw_expected = ["Bytes(len=2)", "Bytes(len=4)"];
        assert_eq!(link_run(raw), raw_expected, "interp: raw Bytes rendering");
        assert_eq!(run_linked_on_wasm(&[("main", raw)], "main"), raw_expected, "compiled: raw Bytes rendering");
    }

    /// (BUG-530) Tuple values are legal beyond arity four, so the public protocol
    /// surface must not silently stop there. Witchy does not have variadic trait
    /// impls yet; the documented 0.1 contract is tuple `Show`/`Reflect` through
    /// arity 8, with wider heterogeneous values modeled as named records.
    #[test]
    fn tuple5_show_and_reflect_protocols_work_on_both_backends() {
        let src = "import show\nimport reflect\nimport json\n\ntype Box5 derive(Reflect):\n    value: (Int, Int, Int, Int, Int)\n\nfn main(console: Console):\n    let t = (1, 2, 3, 4, 5)\n    show.say(console, t)\n    console.print(\"${t}\")\n    console.print(reflect.debug(t))\n    console.print(json.stringify(t))\n    console.print(json.stringify(Box5(t)))\n    let t8 = (1, 2, 3, 4, 5, 6, 7, 8)\n    console.print(show.render(t8))\n    console.print(json.stringify(t8))\n";
        let expected = [
            "(1, 2, 3, 4, 5)",
            "(1, 2, 3, 4, 5)",
            "(1, 2, 3, 4, 5)",
            "[1,2,3,4,5]",
            "{\"value\":[1,2,3,4,5]}",
            "(1, 2, 3, 4, 5, 6, 7, 8)",
            "[1,2,3,4,5,6,7,8]",
        ];
        assert_eq!(link_run(src), expected, "interp: tuple protocol arity");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: tuple protocol arity");
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

    /// (BUG-486) `MNil` is the reflection shape for the language's unit value,
    /// not only for JSON null. Exercise it through a Nil-returning helper so this
    /// stays independent of the separate bare-`Nil` expression backend bug.
    #[test]
    fn nil_is_reflectable_on_both_backends() {
        let src = "import reflect\nimport json\n\nfn unit() -> Nil:\n    return\n\nfn main(console: Console):\n    console.print(reflect.debug(unit()))\n    console.print(json.stringify(unit()))\n";
        let expected = ["nil", "null"];
        assert_eq!(link_run(src), expected, "interp: Nil reflection");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "compiled: Nil reflection");
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

    /// BUG-381: `std/rights` must model `Net` as two independent axes:
    /// verbs (`Connect`/`Listen`) and transports (`Tcp`/`Udp`/`Uds`). Omitting
    /// an axis means "all values on that axis", matching the compiler's
    /// capability analyzer and the package/Coven authority gates.
    #[test]
    fn rights_net_axis_coverage_agrees_on_both_backends() {
        let src = r#"import rights

fn mark(v: Bool) -> String:
    if v:
        "T"
    else:
        "F"

fn main(console: Console):
    console.print(mark(rights.covers("Net[Connect]", "Net[Connect, Tcp]")))
    console.print(mark(rights.covers("Net[Tcp]", "Net[Connect, Tcp]")))
    console.print(mark(rights.covers("Net[Connect, Tcp]", "Net[Connect]")))
    console.print(mark(rights.covers("Net[Connect]", "Net[Listen]")))
    console.print(mark(rights.covers("Net[Tcp]", "Net[Udp]")))
    console.print(mark(rights.covers("Net", "Net[Connect]")))
    console.print(mark(rights.covers("Net[Connect]", "Net")))
    console.print(mark(rights.covers("Dir[Read]", "Dir[Read, Write]")))
    console.print(mark(rights.covers("Dir[Read, Write]", "Dir[Read]")))
"#;
        let expected = ["T", "T", "F", "F", "F", "T", "F", "F", "T"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// `Rand` follows the same receiver-method shape as other capabilities:
    /// `rand.hex(n)` lowers to `rand.hex(rand, n)`, without needing the ambiguous
    /// double-receiver spelling `rand.hex(rand, n)`.
    #[test]
    fn rand_capability_supports_std_method_syntax() {
        let src = "import rand\n\nfn main(console: Console, rand: Rand):\n    let token = rand.hex(4)\n    console.print(\"${token.length()}\")\n";
        let expected = ["8"];
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );

        use crate::runtime::{Capabilities, Runtime};
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    rand: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), expected, "wasm");
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

    /// (BUG-324) The lexer admits the Int.MIN magnitude as a wrapped token so
    /// expression literals can spell `-9223372036854775808`; pattern parsing must
    /// use the same wraparound negation instead of panicking in debug builds.
    #[test]
    fn int_min_literal_patterns_work_on_both_backends() {
        let src = "fn main(console: Console):\n    let n = -9223372036854775808\n    match n:\n        -9223372036854775808 -> console.print(\"min\")\n        _ -> console.print(\"other\")\n    let m = -9223372036854775807\n    match m:\n        -9223372036854775808..=-9223372036854775807 -> console.print(\"range\")\n        _ -> console.print(\"miss\")\n";
        let expected = ["min", "range"];
        assert_eq!(link_run(src), expected, "interp: Int.MIN pattern");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: Int.MIN pattern",
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

    /// (BUG-428) Generator lowering must not erase the source-level
    /// no-`yield`-inside-`region:` safety rule before type checking can enforce
    /// it.
    #[test]
    fn gen_fn_rejects_yield_inside_region_before_lowering() {
        for body in [
            "region:\n        yield 1\n        0",
            "region:\n        if true:\n            yield 1\n        0",
            "if true:\n        region:\n            yield 1\n            0",
        ] {
            let src = format!(
                "import iter\n\ngen fn bad() -> Iter(Int):\n    {body}\n    yield 2\n\nfn main(console: Console):\n    let xs: List(Int) = iter.collect(bad())\n    console.print(\"${{xs}}\")\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let err = crate::pipeline::link(vec![("main".into(), module)], "main")
                .expect_err("yield inside region must be rejected during generator lowering");
            assert!(
                err.message.contains("cannot `yield` inside `region:`")
                    && err.message.contains("generator frame"),
                "diagnostic should explain the region/generator safety rule, got: {}",
                err.message
            );
        }
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
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        let oob = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${bytes.at(b, 5)}\")\n";
        let (lmod, wasm) = compile(oob);
        assert!(
            interpreter::run_module(lmod, ".", Vec::new()).is_err(),
            "interpreter must error on OOB bytes index"
        );
        assert!(crate::run_wasm_bytes(&wasm).is_err(), "WASM must trap on OOB bytes index");
        // A negative index likewise traps (it used to read backwards into the heap).
        let neg = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${bytes.at(b, 0 - 1)}\")\n";
        let (nmod, nwasm) = compile(neg);
        assert!(
            interpreter::run_module(nmod, ".", Vec::new()).is_err(),
            "interpreter must error on negative bytes index"
        );
        assert!(crate::run_wasm_bytes(&nwasm).is_err(), "WASM must trap on negative bytes index");
        // In-bounds indexing still agrees.
        let ok = "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hi!\")\n    console.print(\"${bytes.at(b, 2)}\")\n";
        let expected = ["33"];
        assert_eq!(link_run(ok), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", ok)], "main"), expected, "wasm");
    }

    /// (SEC-043) The HTTP CRLF / header-injection validators trap LOUDLY and
    /// IDENTICALLY on both backends when a header value / request field carries a
    /// `\r`/`\n` (response/request splitting) or a header name is not an RFC 7230
    /// token — rather than emitting a corrupted, attacker-shaped wire message.
    /// A clean value passes on both backends.
    #[test]
    fn http_crlf_header_validators_trap_on_both_backends() {
        let prog = |call: &str| {
            format!("import http\n\nfn main(console: Console):\n    {call}\n    console.print(\"ok\")\n")
        };
        let server_prog = |call: &str| {
            format!("import server\n\nfn main(console: Console):\n    {call}\n    console.print(\"ok\")\n")
        };
        // A header VALUE with an embedded CRLF must error on both backends.
        let crlf_value = prog("http.check_header(\"x-test\", \"a\\r\\nInjected: 1\")");
        let linked = resolve_std_src(&crlf_value);
        assert!(
            interpreter::run_module(linked, ".", Vec::new()).is_err(),
            "interpreter must trap on a CRLF header value"
        );
        let bytes = codegen::compile_module_binary(&resolve_std_src(&crlf_value))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on a CRLF header value");

        // A header NAME with a space (not a token) must error on both backends.
        let bad_name = prog("http.check_header(\"bad name\", \"ok\")");
        assert!(
            interpreter::run_module(resolve_std_src(&bad_name), ".", Vec::new()).is_err(),
            "interpreter must trap on an invalid header name"
        );
        let bn = codegen::compile_module_binary(&resolve_std_src(&bad_name))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&bn).is_err(), "WASM must trap on an invalid header name");

        // A CR/LF in a request field (path/host/method) errors on both backends.
        let crlf_path = prog("http.check_field(\"request path\", \"/a\\nHost: evil\")");
        assert!(
            interpreter::run_module(resolve_std_src(&crlf_path), ".", Vec::new()).is_err(),
            "interpreter must trap on a CRLF path"
        );
        let cp = codegen::compile_module_binary(&resolve_std_src(&crlf_path))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&cp).is_err(), "WASM must trap on a CRLF path");

        // BUG-506: NUL and other non-CR/LF controls are also forbidden at this
        // raw HTTP rendering boundary.
        let nul_value = prog("let nul = string.from_code(0)\n    http.check_header(\"x-test\", \"a\" + nul + \"b\")");
        assert!(
            interpreter::run_module(resolve_std_src(&nul_value), ".", Vec::new()).is_err(),
            "interpreter must trap on a NUL header value"
        );
        let nv = codegen::compile_module_binary(&resolve_std_src(&nul_value))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&nv).is_err(), "WASM must trap on a NUL header value");

        let soh_path = prog("let soh = string.from_code(1)\n    http.check_request_field(\"request path\", \"/a\" + soh + \"b\")");
        assert!(
            interpreter::run_module(resolve_std_src(&soh_path), ".", Vec::new()).is_err(),
            "interpreter must trap on a SOH request field"
        );
        let sp = codegen::compile_module_binary(&resolve_std_src(&soh_path))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&sp).is_err(), "WASM must trap on a SOH request field");

        let del_value = prog("let del = string.from_code(127)\n    http.check_header(\"x-test\", \"a\" + del + \"b\")");
        assert!(
            interpreter::run_module(resolve_std_src(&del_value), ".", Vec::new()).is_err(),
            "interpreter must trap on a DEL header value"
        );
        let dv = codegen::compile_module_binary(&resolve_std_src(&del_value))
            .expect_lowered("lowers");
        assert!(crate::run_wasm_bytes(&dv).is_err(), "WASM must trap on a DEL header value");

        let nul_response = server_prog(
            "let nul = string.from_code(0)\n    let r = server.with_header(server.text(200, \"ok\"), \"x-test\", \"a\" + nul + \"b\")\n    let _wire = server.render(r)"
        );
        assert!(
            interpreter::run_module(resolve_std_src(&nul_response), ".", Vec::new()).is_err(),
            "interpreter must trap before rendering a response header with NUL"
        );
        let nr = codegen::compile_module_binary(&resolve_std_src(&nul_response))
            .expect_lowered("lowers");
        assert!(
            crate::run_wasm_bytes(&nr).is_err(),
            "WASM must trap before rendering a response header with NUL"
        );

        // A clean header + field passes on both backends (no false positives).
        let clean = prog("http.check_header(\"content-type\", \"application/json\")\n    http.check_header(\"x-tab\", \"a\\tb\")\n    http.check_request_field(\"request path\", \"/api/v1/users\")");
        assert_eq!(link_run(&clean), ["ok"], "interp accepts a clean header/path");
        assert_eq!(run_linked_on_wasm(&[("main", &clean)], "main"), ["ok"], "wasm accepts a clean header/path");
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

    /// (BUG-456) Encoding's canonical binary path is `Bytes`, not lossy `String`
    /// plumbing or hex detours. The payload includes `0xff`, so any accidental
    /// UTF-8 normalization changes the rendered byte list.
    #[test]
    fn encoding_bytes_codecs_round_trip_binary_on_both_backends() {
        let src = "import bytes\nimport encoding\nimport result\n\n\
                   fn main(console: Console):\n\
                   \x20   let raw = result.unwrap_or(encoding.hex_decode_bytes(\"4100ff2f\"), bytes.from_string(\"\"))\n\
                   \x20   console.print(encoding.hex_encode_bytes(raw))\n\
                   \x20   console.print(encoding.base64_encode_bytes(raw))\n\
                   \x20   console.print(encoding.base64url_encode_bytes(raw))\n\
                   \x20   let from_b64 = result.unwrap_or(encoding.base64_decode_bytes(\"QQD/Lw==\"), bytes.from_string(\"\"))\n\
                   \x20   console.print(\"${bytes.to_list(from_b64)}\")\n\
                   \x20   let from_url = result.unwrap_or(encoding.base64url_decode_bytes(\"QQD_Lw\"), bytes.from_string(\"\"))\n\
                   \x20   console.print(\"${bytes.to_list(from_url)}\")\n\
                   \x20   match encoding.base64url_decode_bytes(\"QQD/Lw==\"):\n\
                   \x20       Ok(_) -> console.print(\"bad\")\n\
                   \x20       Err(_) -> console.print(\"err\")\n";
        let want = ["4100ff2f", "QQD/Lw==", "QQD_Lw", "[65, 0, 255, 47]", "[65, 0, 255, 47]", "err"];
        assert_eq!(link_run(src), want, "interpreter byte codecs must preserve binary");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            want,
            "WASM byte codecs must preserve binary"
        );
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

    /// (SEC-043) `has_crlf` agrees on both backends for a control-bearing vs a
    /// clean value — the primitive the CRLF validators are built on.
    #[test]
    fn http_has_crlf_agrees_on_both_backends() {
        let prog = |v: &str| {
            format!(
                "import http\n\nfn main(console: Console):\n    console.print(\"${{http.has_crlf(\"{v}\")}}\")\n"
            )
        };
        for (value, want) in [("a\\r\\nb", "true"), ("plain", "false"), ("tab\\ttab", "false")] {
            let src = prog(value);
            assert_eq!(link_run(&src), [want], "interp has_crlf({value})");
            assert_eq!(run_linked_on_wasm(&[("main", &src)], "main"), [want], "wasm has_crlf({value})");
        }
    }

    /// HTTP/query hardening — the cluster of stdlib `http`/`server` fixes must behave
    /// identically on both backends (parity is prime):
    ///   BUG-236/352  query/form values are percent- AND `+`-decoded (`%E2%82%AC` -> €).
    ///   BUG-375      path params and the handler-visible path are percent-decoded,
    ///                while a `%2F` stays inside one segment (no forged separator).
    ///   BUG-268      a nested router's own middleware layers are preserved.
    ///   BUG-390      a request with conflicting Content-Length is rejected (400).
    ///   BUG-203      an overflowing response status code parses to 0, never traps.
    ///   BUG-269      a `chunked` response body is de-chunked.
    ///   BUG-358      the renderer drops a handler-supplied framing header (no dup CL).
    #[test]
    fn http_server_hardening_agrees_on_both_backends() {
        let src = r#"import server
import http
import option
from http import Request, Response

fn hi(req: Request) -> Response:
    server.text(200, "id=" + server.param_or(req, "id", "") + " path=" + server.path(req))

fn tag(inner: fn(Request) -> Response) -> fn(Request) -> Response:
    fn(req: Request):
        match inner(req):
            Response(c, h, b) -> Response(c, h, "[wrapped]" + b)

fn main(console: Console):
    let req = Request("POST", "/x", [], [], [], "q=a%20b&x=1+2&k&e=%E2%82%AC")
    console.print("${server.form_body(req)}")
    let app = server.router().get("/users/:id", hi)
    console.print(http.body(server.handle(app, Request("GET", "/users/a%20b", [], [], [], ""))))
    console.print("${http.status(server.handle(app, Request("GET", "/users/a%2Fb", [], [], [], "")))}")
    let sub = server.router().get("/inner", hi).layer(tag)
    let nested = server.router().nest("/api", sub)
    console.print(http.body(server.handle(nested, Request("GET", "/api/inner", [], [], [], ""))))
    match server.parse_request_response("POST /x HTTP/1.1\r\nContent-Length: 3\r\nContent-Length: 5\r\n\r\nabc"):
        Ok(_r) -> console.print("PARSED")
        Err(resp) -> console.print("rejected " + "${http.status(resp)}")
    match server.parse_request_response("POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ntest\r\n0\r\n\r\n"):
        Ok(_r) -> console.print("chunked-request=parsed")
        Err(resp) -> console.print("chunked-request=" + "${http.status(resp)}")
    match server.parse_request_response("POST /upload HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\nbody"):
        Ok(_r) -> console.print("gzip-request=parsed")
        Err(resp) -> console.print("gzip-request=" + "${http.status(resp)}")
    match server.parse_request_response("POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\n\r\nbody"):
        Ok(_r) -> console.print("mixed-framing=parsed")
        Err(resp) -> console.print("mixed-framing=" + "${http.status(resp)}")
    console.print("status=" + "${http.status(http.parse_response("HTTP/1.1 999999999999999999999999 X\r\n\r\nb"))}")
    console.print("chunked=" + http.body(http.parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n")))
    console.print("unicode-chunk=" + http.body(http.parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\né\r\n0\r\n\r\n")))
    match http.try_parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\né\r\n0\r\n\r\n"):
        Ok(resp) -> console.print("unicode-strict=" + http.body(resp))
        Err(e) -> console.print("unicode-strict=" + http.http_error_message(e))
    match http.try_parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nX\r\nhello\r\n0\r\n\r\n"):
        Ok(_) -> console.print("bad-size=parsed")
        Err(e) -> console.print("bad-size=" + http.http_error_message(e))
    match http.try_parse_response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhe"):
        Ok(_) -> console.print("truncated=parsed")
        Err(e) -> console.print("truncated=" + http.http_error_message(e))
    let r1 = server.with_header(server.text(200, "hi"), "content-length", "999")
    console.print("cl=" + "${server.render(r1).to_lower().count("content-length")}")
    console.print("${http.is_framing_header("Content-Length")}")
"#;
        let expected = vec![
            "[(q, a b), (x, 1 2), (k, ), (e, €)]".to_string(),
            "id=a b path=/users/a b".to_string(),
            "200".to_string(),
            "[wrapped]id= path=/api/inner".to_string(),
            "rejected 400".to_string(),
            "chunked-request=400".to_string(),
            "gzip-request=400".to_string(),
            "mixed-framing=400".to_string(),
            "status=0".to_string(),
            "chunked=hello world".to_string(),
            "unicode-chunk=é".to_string(),
            "unicode-strict=é".to_string(),
            "bad-size=chunked response has invalid chunk size `X`".to_string(),
            "truncated=chunked response ended before the declared chunk size".to_string(),
            "cl=1".to_string(),
            "true".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// BUG-438: malformed request lines are rejected before reaching handlers.
    /// BUG-432: paths are normalized (collapsed slashes, no trailing slash).
    #[test]
    fn server_request_line_validation_and_path_normalization() {
        let src = r#"import server
import http
from http import Request, Response

fn hi(req: Request) -> Response:
    server.text(200, "path=" + server.path(req))

fn main(console: Console):
    // BUG-438: malformed request lines rejected
    match server.parse_request("GET\r\n\r\n"):
        Ok(_r) -> console.print("bad: no target")
        Err(e) -> console.print(server.request_parse_error_message(e))
    match server.parse_request("GET /\r\n\r\n"):
        Ok(_r) -> console.print("bad: no version")
        Err(e) -> console.print(server.request_parse_error_message(e))
    match server.parse_request("\r\n\r\n"):
        Ok(_r) -> console.print("bad: empty line")
        Err(e) -> console.print(server.request_parse_error_message(e))
    // Valid request line succeeds
    match server.parse_request("GET / HTTP/1.1\r\n\r\n"):
        Ok(req) -> console.print("ok path=" + server.path(req))
        Err(_e) -> console.print("bad: valid rejected")
    // BUG-432: path normalization
    match server.parse_request("GET //api//coven/index HTTP/1.1\r\n\r\n"):
        Ok(req) -> console.print("norm=" + server.path(req))
        Err(_e) -> console.print("bad: norm rejected")
    match server.parse_request("GET /api/coven/index/ HTTP/1.1\r\n\r\n"):
        Ok(req) -> console.print("trail=" + server.path(req))
        Err(_e) -> console.print("bad: trail rejected")
    match server.parse_request("GET / HTTP/1.1\r\n\r\n"):
        Ok(req) -> console.print("root=" + server.path(req))
        Err(_e) -> console.print("bad: root rejected")
    // Router normalizes paths — double slashes and trailing slashes match
    let app = server.router().get("/api/items", hi)
    console.print(http.body(server.handle(app, Request("GET", "/api/items", [], [], [], ""))))
    console.print(http.body(server.handle(app, Request("GET", "/api//items", [], [], [], ""))))
    console.print(http.body(server.handle(app, Request("GET", "/api/items/", [], [], [], ""))))
"#;
        let expected = vec![
            "malformed request line".to_string(),
            "malformed request line".to_string(),
            "malformed request line".to_string(),
            "ok path=/".to_string(),
            "norm=/api/coven/index".to_string(),
            "trail=/api/coven/index".to_string(),
            "root=/".to_string(),
            "path=/api/items".to_string(),
            "path=/api/items".to_string(),
            "path=/api/items".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// Server accessors distinguish a present empty value from an absent one.
    /// BUG-464: the primary API returns Option; callers that want the old sentinel
    /// behavior must opt in with the `_or` helpers.
    #[test]
    fn server_accessors_return_option_for_absence_on_both_backends() {
        let src = r#"import server
from http import Request

fn show(console: Console, label: String, value: Option(String)):
    match value:
        Some(v) -> console.print(label + "=Some(" + v + ")")
        None -> console.print(label + "=None")

fn main(console: Console):
    let req = Request("POST", "/x", [("id", "")], [("code", ""), ("state", "ready")], [], "a=&b=2")
    show(console, "param-empty", server.param(req, "id"))
    show(console, "param-missing", server.param(req, "missing"))
    show(console, "query-empty", server.query(req, "code"))
    show(console, "query-present", server.query(req, "state"))
    show(console, "query-missing", server.query(req, "missing"))
    show(console, "form-empty", server.form_field(req, "a"))
    show(console, "form-present", server.form_field(req, "b"))
    show(console, "form-missing", server.form_field(req, "missing"))
    console.print("param_or=" + server.param_or(req, "missing", "fallback"))
    console.print("query_or=" + server.query_or(req, "missing", "fallback"))
    console.print("form_field_or=" + server.form_field_or(req, "missing", "fallback"))
"#;
        let expected = vec![
            "param-empty=Some()".to_string(),
            "param-missing=None".to_string(),
            "query-empty=Some()".to_string(),
            "query-present=Some(ready)".to_string(),
            "query-missing=None".to_string(),
            "form-empty=Some()".to_string(),
            "form-present=Some(2)".to_string(),
            "form-missing=None".to_string(),
            "param_or=fallback".to_string(),
            "query_or=fallback".to_string(),
            "form_field_or=fallback".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-234) A non-http(s) URL scheme (`ftp:`/`file:`/`gopher:`) is REJECTED with
    /// an Err rather than silently dialed as plaintext HTTP to the named host — the
    /// http client speaks only HTTP/1.1. Same rejection on both backends.
    #[test]
    fn http_rejects_non_http_schemes_on_both_backends() {
        let src = "import http\n\n\
                   fn main(net: Net, console: Console):\n\
                   \x20   for u in [\"ftp://h/x\", \"file:///etc/passwd\", \"gopher://h/1\"]:\n\
                   \x20       match http.get_url(net, u):\n\
                   \x20           Ok(_r) -> console.print(\"OK\")\n\
                   \x20           Err(_e) -> console.print(\"rejected\")\n";
        let want = ["rejected", "rejected", "rejected"];
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            want,
            "interpreter"
        );
        assert_eq!(run_linked_on_wasm_net(&[("main", src)], "main", &[]), want, "wasm");
    }

    /// (BUG-364 / BUG-255) The request-line validator rejects a SPACE (it would split
    /// the request line into extra tokens — request smuggling), and the response
    /// renderer rejects a status code outside 100..599. Both trap LOUDLY and
    /// identically on both backends rather than emit a malformed message.
    #[test]
    fn http_request_line_and_status_validation_trap_on_both_backends() {
        let cases = [
            "import http\n\nfn main(console: Console):\n    http.check_request_field(\"request path\", \"/a b\")\n    console.print(\"x\")\n",
            "import server\n\nfn main(console: Console):\n    console.print(server.render(server.status_only(700)))\n",
        ];
        for src in cases {
            assert!(
                interpreter::run_module(resolve_std_src(src), ".", Vec::new()).is_err(),
                "interpreter must trap: {src}"
            );
            let bytes = codegen::compile_module_binary(&resolve_std_src(src))
                .expect_lowered("lowers");
            assert!(crate::run_wasm_bytes(&bytes).is_err(), "wasm must trap: {src}");
        }
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

    /// (RFC-0047) `==` on a capability type is a compile-time error — capabilities
    /// are authority, not data. Direct and nested-in-a-container both error.
    #[test]
    fn capability_equality_is_a_compile_error() {
        let direct = "fn main(console: Console):\n    console.print(\"${console == console}\")\n";
        let e = typeck::check_str(direct).expect_err("`console == console` must be rejected");
        assert!(e.contains("not defined on capability types"), "teaching error, got: {e}");
        let in_tuple = "fn main(console: Console):\n    console.print(\"${(console, 1) == (console, 1)}\")\n";
        assert!(
            typeck::check_str(in_tuple).expect_err("cap in a tuple must be rejected")
                .contains("not defined on capability types"),
            "a capability nested in a tuple must be rejected too"
        );
        let in_sum = "type Resource:\n    Missing\n    Opened(Dir[Read])\n\nfn same(a: Resource, b: Resource) -> Bool:\n    a == b\n";
        assert!(
            typeck::check_str(in_sum).expect_err("cap in a nominal sum must be rejected")
                .contains("not defined on capability types"),
            "a capability nested in a GC-lowered sum must remain non-comparable"
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

    /// RFC-0005 Stage 4 (records slice): plain named-field and positional
    /// nominal aggregates may carry a migrated capability, lowered to typed GC
    /// structs. Construction, spread, field access through a nested record
    /// chain, `match` destructuring, and `var` place assignment all agree between
    /// the backends, and the authority never crosses the i64 slot. Nesting is
    /// also the BUG-566 regression: the
    /// classifier lives in one home now, so typeck and codegen cannot disagree
    /// about which records GC-lower (the old codegen copy missed nested records
    /// and ICE'd the encoder).
    #[test]
    fn plain_cap_record_runs_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_caprecord_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("greeting.txt"), "hello-record").expect("seed");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = "type Inner:\n    dir: Dir[Read]\n    tag: String\n\ntype Workspace:\n    inner: Inner\n    label: String\n    count: Int\n\ntype RootToken:\n    RootToken(Dir[Read])\n\ntype RootHandle:\n    RootHandle(RootToken, String)\n\ntype NamedAroundPositional:\n    token: RootToken\n    name: String\n\ntype PositionalAroundNamed:\n    PositionalAroundNamed(Inner, String)\n\nfn load(w: Workspace, name: String) -> String:\n    w.inner.dir.read(name)\n\nfn load_positional(h: RootHandle) -> String:\n    match h:\n        RootHandle(RootToken(dir), name) -> dir.read(name)\n\nfn load_named_positional(h: NamedAroundPositional) -> String:\n    match h.token:\n        RootToken(dir) -> dir.read(h.name)\n\nfn load_positional_named(h: PositionalAroundNamed) -> String:\n    match h:\n        PositionalAroundNamed(inner, name) -> inner.dir.read(name)\n\nfn relabel(w: Workspace, label: String) -> Workspace:\n    Workspace(label: label, ..w)\n\nfn main(console: Console, root: Dir[Read]):\n    let w = Workspace(Inner(root, \"t\"), \"main\", 1)\n    console.print(load(w, \"greeting.txt\"))\n    console.print(load_positional(RootHandle(RootToken(root), \"greeting.txt\")))\n    console.print(load_named_positional(NamedAroundPositional(RootToken(root), \"greeting.txt\")))\n    console.print(load_positional_named(PositionalAroundNamed(Inner(root, \"v\"), \"greeting.txt\")))\n    let x = relabel(w, \"alt\")\n    console.print(\"${x.label} ${x.count}\")\n    var y = Workspace(inner: Inner(root, \"u\"), label: \"named\", count: 2)\n    y.count = 40 + y.count\n    console.print(\"${y.label} ${y.count}\")\n    match y:\n        Workspace(i, lab, n) -> console.print(\"${lab} ${n} ${i.tag}\")\n";
        let want = vec![
            "hello-record".to_string(),
            "hello-record".to_string(),
            "hello-record".to_string(),
            "hello-record".to_string(),
            "alt 1".to_string(),
            "named 42".to_string(),
            "named 42 u".to_string(),
        ];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers plain cap-carrying records");
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

    /// RFC-0005 Stage 4 (sum slice): a non-generic nominal sum that carries a
    /// migrated capability uses one tagged GC struct with disjoint per-variant
    /// field bands. Wrong-variant patterns must test the tag before touching an
    /// inactive (possibly null) reference field. Recursive and mutually
    /// recursive sums use the same Wasm GC recursion group.
    #[test]
    fn capability_sum_runs_on_tagged_gc_backend() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_capsum_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("greeting.txt"), "hello-sum").expect("seed");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = r#"
type Resource:
    Missing(String)
    Opened(Dir[Read], String)
    Count(Int)
    Ratio(Float)
    Pair(String, String)

type FrozenHolder:
    FrozenHolder(frozen Resource)

type Tree:
    Empty
    Leaf(Dir[Read], String)
    Branch(Tree, Tree)

type Outer:
    OuterEmpty
    OuterInner(Inner)

type Inner:
    InnerCap(Dir[Read], String)
    InnerOuter(Outer)

fn resource_label(r: Resource) -> String:
    match r:
        Opened(_, name) -> "opened: " + name
        Missing(name) -> "missing: " + name
        Count(n) -> "count: ${n}"
        Ratio(x) -> "ratio: ${x}"
        Pair(a, b) -> "pair: ${a}:${b}"

fn keep_resource(r: Resource) -> Resource:
    return r

fn keep_qualified(r: frozen Resource) -> frozen Resource:
    return r

fn unwrap_frozen(h: FrozenHolder) -> Resource:
    match h:
        FrozenHolder(r) -> r

fn mark(console: Console, label: String) -> String:
    console.print(label)
    label

fn load_tree(t: Tree) -> String:
    match t:
        Leaf(dir, name) -> dir.read(name)
        Branch(Leaf(_, _), _) -> "wrong branch"
        Branch(Empty, Leaf(dir, name)) -> dir.read(name)
        Branch(_, _) -> "other branch"
        Empty -> "empty"

fn load_tree_or(t: Tree) -> String:
    match t:
        Leaf(dir, name) | Branch(_, Leaf(dir, name)) -> dir.read(name)
        _ -> "or-empty"

fn load_outer(o: Outer) -> String:
    match o:
        OuterInner(InnerCap(dir, name)) -> dir.read(name)
        OuterInner(InnerOuter(_)) -> "nested outer"
        OuterEmpty -> "empty outer"

fn main(console: Console, root: Dir[Read]):
    console.print(resource_label(keep_resource(Missing("absent"))))
    console.print(resource_label(keep_qualified(Missing("qualified"))))
    console.print(resource_label(unwrap_frozen(FrozenHolder(Missing("field")))))
    console.print(resource_label(Count(922337203685477580)))
    console.print(resource_label(Pair(mark(console, "left"), mark(console, "right"))))
    console.print(load_tree(Empty))
    console.print(load_tree(Branch(Empty, Leaf(root, "greeting.txt"))))
    console.print(load_tree_or(Empty))
    console.print(load_tree_or(Branch(Empty, Leaf(root, "greeting.txt"))))
    console.print(load_outer(OuterInner(InnerCap(root, "greeting.txt"))))
"#;
        let want = vec![
            "missing: absent".to_string(),
            "missing: qualified".to_string(),
            "missing: field".to_string(),
            "count: 922337203685477580".to_string(),
            "left".to_string(),
            "right".to_string(),
            "pair: left:right".to_string(),
            "empty".to_string(),
            "hello-sum".to_string(),
            "or-empty".to_string(),
            "hello-sum".to_string(),
            "hello-sum".to_string(),
        ];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers cap-carrying sums");
        let bin_again = codegen::compile_module_binary(&linked)
            .expect_lowered("the same module still lowers");
        assert_eq!(bin_again, bin, "GC aggregate IDs and binary output must be deterministic");
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

    /// RFC-0005 Stage 4 (tuple slice): a fully concrete tuple that transitively
    /// carries a migrated capability uses a deterministic typed GC-struct
    /// layout. This covers the direct ABI, numeric projection, `let` and
    /// `match` patterns, nested tuples, and tuples stored inside nominal GC
    /// aggregates without ever routing the authority through an i64 slot.
    #[test]
    fn capability_tuple_runs_on_gc_backend() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_captuple_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("greeting.txt"), "hello-tuple").expect("seed");
        let root_str = root.to_str().expect("utf8 root").to_string();
        let src = r#"
type ReadPair = (Dir[Read], String, Int)
type LabeledPair(a) = (Dir[Read], a, Int)

type Holder:
    Holder((Dir[Read], (String, Int)))

type Packet:
    Empty
    Packed((Dir[Read], (String, Int)))

fn keep(pair: (Dir[Read], String, Int)) -> (Dir[Read], String, Int):
    return pair

fn keep_alias(pair: ReadPair) -> ReadPair:
    pair

fn keep_generic_alias(pair: LabeledPair(String)) -> LabeledPair(String):
    pair

fn read_named(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn project(pair: (Dir[Read], String, Int)) -> String:
    read_named(pair.0, pair.1) + ":${pair.2}"

fn destructure(pair: (Dir[Read], String, Int)) -> String:
    let (dir, name, count) = pair
    dir.read(name) + ":${count}"

fn choose(pair: (Dir[Read], String, Int)) -> String:
    match pair:
        (dir, name, count) -> dir.read(name) + ":${count}"

fn keep_qualified(pair: (frozen Dir[Read], String)) -> (frozen Dir[Read], String):
    pair

fn qualified(pair: (frozen Dir[Read], String)) -> String:
    let (dir, name) = pair
    dir.read(name)

fn optional(pair: (Option(Dir[Read]), String)) -> String:
    match pair:
        (Some(dir), name) -> dir.read(name)
        (None, _) -> "none"

fn select(root: Dir[Read], labels: List(String)) -> (Dir[Read], String, Int):
    match list.at(labels, 0):
        "first" -> (root, "greeting.txt", 4)
        _ -> (root, "greeting.txt", 5)

fn nested(holder: Holder) -> String:
    match holder:
        Holder((dir, (name, count))) -> dir.read(name) + ":${count}"

fn packed(packet: Packet) -> String:
    match packet:
        Packed((dir, (name, count))) -> dir.read(name) + ":${count}"
        Empty -> "empty"

fn main(console: Console, root: Dir[Read]):
    let pair = keep((root, "greeting.txt", 1))
    console.print(project(pair))
    console.print(destructure(pair))
    console.print(choose(pair))
    console.print(project(keep_alias((root, "greeting.txt", 6))))
    console.print(project(keep_generic_alias((root, "greeting.txt", 7))))
    console.print(qualified(keep_qualified((root, "greeting.txt"))))
    console.print(optional((Some(root), "greeting.txt")))
    console.print(optional((None, "greeting.txt")))
    console.print(project(select(root, ["first"])))
    console.print(nested(Holder((root, ("greeting.txt", 2)))))
    console.print(packed(Empty))
    console.print(packed(Packed((root, ("greeting.txt", 3)))))
"#;
        let want = vec![
            "hello-tuple:1".to_string(),
            "hello-tuple:1".to_string(),
            "hello-tuple:1".to_string(),
            "hello-tuple:6".to_string(),
            "hello-tuple:7".to_string(),
            "hello-tuple".to_string(),
            "hello-tuple".to_string(),
            "none".to_string(),
            "hello-tuple:4".to_string(),
            "hello-tuple:2".to_string(),
            "empty".to_string(),
            "hello-tuple:3".to_string(),
        ];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers cap-carrying tuples");
        let bin_again = codegen::compile_module_binary(&linked)
            .expect_lowered("the same tuple module still lowers");
        assert_eq!(bin_again, bin, "GC tuple IDs and binary output must be deterministic");
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

    /// RFC-0005 Stage 3: a named sealed capability record can carry a migrated
    /// `Net` externref alongside ordinary data. The compiled backend lowers the
    /// record to a typed GC struct, so the carried authority never passes through
    /// the i64 slot/linear-memory representation.
    #[test]
    fn carried_state_capability_record_runs_on_gc_struct_backend() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "capability Postgres:\n    net: Net[Connect, Tcp]\n    table: String\n\nfn connect(net: Net[Connect, Tcp]) -> Postgres:\n    Postgres(net, \"public\")\n\nfn use_table(pg: Postgres, name: String) -> Postgres:\n    match pg:\n        Postgres(net, _) -> Postgres(net, name)\n\nfn count_rows(pg: Postgres, requested: String) -> String:\n    match pg:\n        Postgres(_, table) ->\n            if requested == table:\n                \"ok: counted rows in \" + requested\n            else:\n                \"denied: \" + requested + \" is outside this handle (scoped to \" + table + \")\"\n\nfn main(console: Console, net: Net):\n    let users = use_table(connect(net), \"users\")\n    console.print(count_rows(users, \"users\"))\n    console.print(count_rows(users, \"secrets\"))\n";
        let want = vec![
            "ok: counted rows in users".to_string(),
            "denied: secrets is outside this handle (scoped to users)".to_string(),
        ];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bin = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers cap-carrying records to GC structs");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bin,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
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

    /// (RFC-0032) `vm.par_map` stays correct when the native worker-VM fast path does
    /// NOT apply — a CAPTURING closure (here `fn(n): n + base`) would be unsound to run
    /// with a null environment in a separate worker VM, so the compiled backend must
    /// fall through to the sequential `List.map` body. Both backends must still agree.
    #[test]
    fn vm_par_map_capturing_closure_agrees() {
        let src = "import vm\n\nfn main(console: Console):\n    let base = 100\n    let ys = vm.par_map([1, 2, 3], fn(n): n + base)\n    console.print(\"${ys}\")\n";
        let expected = ["[101, 102, 103]"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// Host-capability operations are reachable via UFCS method syntax: `console.print(x)`
    /// lowers to the bare intrinsic `console.print(x)` — the same surface a library
    /// capability's own `impl` methods already get. The foundation for RFC-0011's
    /// "refinement is a method" model (`net.only(...)`, `dir.subtree(...)`). The method
    /// and free-function forms must agree on both backends.
    #[test]
    fn host_capability_ufcs_method_calls() {
        let src = "fn main(console: Console):\n    console.print(\"a\")\n    console.print(\"b\")\n";
        let expected = ["a", "b"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
        // The refinement verb `net.only(...)` (method) / `only(...)` (free) is exercised on
        // both backends by `net_only_refinement_verb_backends_agree` below.
    }

    /// RFC-0011: `std/policy` builds a typed `NetPolicy` (`Net.tcp(host, port)`)
    /// instead of a hand-written string, and `net.only(policy)` narrows the `Net` to it.
    /// The typed policy carries the same `host:port` pattern the host enforces, so both
    /// backends agree. The grant must admit the pattern.
    #[test]
    fn net_tcp_policy_narrows_on_both_backends() {
        let src = "fn main(net: Net, console: Console):\n    let db = net.only(Net.tcp(\"10.0.0.5\", 6379))\n    console.print(\"confined\")\n";
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

    /// (BUG-489) The blessed NetPolicy constructors reject impossible ports at
    /// the std boundary. Raw `NetPolicy(...)` remains a separate surface tracked
    /// by BUG-484, but `Net.tcp`/`Net.cidr` should only build meaningful policy
    /// values.
    #[test]
    fn net_policy_constructors_reject_out_of_range_ports_on_both_backends() {
        let ok = "fn main(console: Console):\n    console.print(Net.tcp(\"example.com\", 0).pattern)\n    console.print(Net.cidr(\"10.0.0.0/8\", 65535).pattern)\n";
        let expected = ["example.com:0", "10.0.0.0/8:65535"];
        assert_eq!(link_run(ok), expected, "interp: edge ports");
        assert_eq!(run_linked_on_wasm(&[("main", ok)], "main"), expected, "wasm: edge ports");

        for call in [
            "Net.tcp(\"example.com\", -1)",
            "Net.tcp(\"example.com\", 70000)",
            "Net.cidr(\"10.0.0.0/8\", -1)",
            "Net.cidr(\"10.0.0.0/8\", 70000)",
        ] {
            let src = format!("fn main(console: Console):\n    let p = {call}\n    console.print(p.pattern)\n");
            let linked = resolve_std_src(&src);
            typeck::check(&linked).expect("typecheck");
            let interp_err = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter must reject out-of-range NetPolicy port")
                .to_string();
            assert!(interp_err.contains("policy: net port must be in 0..65535"), "{call}: {interp_err}");

            let wasm = codegen::compile_module_binary(&linked)

                .expect_lowered("out-of-range NetPolicy program should lower");
            let wasm_err = crate::run_wasm_bytes(&wasm)
                .expect_err("WASM must reject out-of-range NetPolicy port")
                .to_string();
            assert!(wasm_err.contains("policy: net port must be in 0..65535"), "{call}: {wasm_err}");
        }
    }

    #[test]
    fn net_private_denies_internal_addresses_on_both_backends() {
        // RFC-0020: `net.deny(Net.private())` is the one-line SSRF/rebinding
        // defense — a connect to a private IP (here loopback) is refused at the
        // capability layer, identically on both backends. `connect` aborts on a
        // denied address, so a successful run means the deny held.
        let src = "fn main(net: Net, console: Console):\n    let safe = net.deny(Net.private())\n    console.print(\"denied private ranges\")\n";
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
    /// address set to a `NetPolicy` built by `policy`. It narrows identically on both
    /// backends. (The raw-string form survives only as a `--net`/config grant, not a
    /// language builtin — see `retired_restrict_builtin_is_rejected`.)
    #[test]
    fn net_only_refinement_verb_backends_agree() {
        let src = "fn main(net: Net, console: Console):\n    let m = net.only(Net.tcp(\"10.0.0.5\", 6379))\n    console.print(\"only\")\n";
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

    /// RFC-0020 step 1: the IPv6 SSRF/rebinding defense, end to end. A program granted `[::1]:80`
    /// that `net.deny(Net.private())` CANNOT connect to `[::1]:80` — the loopback is now
    /// CIDR-matched by the deny (before this, `Net.private()`'s IPv6 ranges only ever
    /// exact-matched, so an internal IPv6 slipped through). Refused identically on both backends
    /// (the allow-list check is the shared `net_allows`).
    #[test]
    fn net_deny_private_blocks_internal_ipv6_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "\
                   fn main(console: Console, net: Net):\n\
                   \x20   let safe = net.deny(Net.private())\n\
                   \x20   let s = safe.connect(\"[::1]:80\")\n\
                   \x20   s.send_line(\"x\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        // Interpreter: granted the loopback, then it denies the private ranges.
        assert!(
            interpreter::run_module(linked.clone(), ".", vec!["[::1]:80".into()]).is_err(),
            "interp must refuse an internal IPv6 connect after net.deny(private())"
        );
        // Compiled: same grant, same refusal.
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec!["[::1]:80".to_string()]),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(
            actor.run().is_err(),
            "compiled must refuse an internal IPv6 connect after net.deny(private())"
        );
    }

    /// RFC-0011: `Net.union(a, b)` builds a multi-endpoint `NetPolicy`, and
    /// `net.only(union(...))` narrows to the WHOLE set — so a further refinement to EITHER
    /// endpoint still succeeds (both are admitted). On both backends.
    #[test]
    fn net_only_union_admits_each_endpoint_backends_agree() {
        let src = "fn main(net: Net, console: Console):\n    let pair = net.only(Net.union(Net.tcp(\"10.0.0.5\", 6379), Net.tcp(\"10.0.0.6\", 6379)))\n    let a = pair.only(Net.tcp(\"10.0.0.5\", 6379))\n    let b = pair.only(Net.tcp(\"10.0.0.6\", 6379))\n    console.print(\"both\")\n";
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
        let src = "import cmp\n\ntype T derive(Show, PartialEq, Eq, PartialOrd, Ord):\n    x: Int\n    y: Int\n\nfn mk() -> Result(T, String):\n    Ok(T(1, 2))\n\nfn pair() -> (T, T):\n    (T(1, 2), T(3, 4))\n\nfn main(console: Console):\n    let base = T(1, 2)\n    match mk():\n        Ok(p) -> console.print(\"${p == base}\")\n        Err(_e) -> console.print(\"err\")\n    if let Ok(p) = mk():\n        console.print(\"${p < T(9, 9)}\")\n    let (a, b) = pair()\n    console.print(\"${a == b}\")\n    console.print(\"${a < b}\")\n";
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
        let after_own = "fn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    c.print(\"${drain(d)}\")\n    c.print(\"${list.length(d)}\")\n";
        let e1 = typeck::check_str(after_own).expect_err("reuse after own should fail");
        assert!(e1.to_string().contains("after it was moved"), "got: {e1:?}");
        // Reuse after an explicit `move`.
        let after_move = "fn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    c.print(\"${drain(move d)}\")\n    c.print(\"${list.length(d)}\")\n";
        assert!(
            typeck::check_str(after_move).is_err(),
            "reuse after move should fail"
        );
        // A `let` borrow does NOT consume — reuse is fine.
        let after_borrow = "fn peek(let xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    c.print(\"${peek(d)}\")\n    c.print(\"${list.length(d)}\")\n";
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
        let strs = "fn first_char(let s: String) -> String:\n    if s.char_count() > 0:\n        s.substring(0, 1)\n    else:\n        \"\"\nfn main(c: Console):\n    let txt = \"héllo\"\n    c.print(first_char(txt))\n    c.print(\"${txt.char_count()}\")\n";
        assert_eq!(interpreter::run(strs).expect("interp str"), ["h", "5"]);
        assert_eq!(run_linked_on_wasm(&[("main", strs)], "main"), ["h", "5"], "wasm str");

        let dict = "fn lookup(let d: Dict(String, Int)) -> Int:\n    dict.get_or(d, \"a\", -1)\nfn main(c: Console):\n    var m = dict.new()\n    dict.insert(m, \"a\", 42)\n    c.print(\"${lookup(m)}\")\n    c.print(\"${dict.length(m)}\")\n";
        assert_eq!(link_run(dict), ["42", "1"]);
        assert_eq!(run_linked_on_wasm(&[("main", dict)], "main"), ["42", "1"], "wasm dict");
    }

    /// `move` works in every value position (let value, list element, call
    /// argument), forcing a move; the moved binding can't be reused (rejected by
    /// the type checker, uniformly).
    #[test]
    fn convention_move_value_positions() {
        let prog = "fn main(console: Console):\n    let a = [1, 2, 3]\n    let b = move a\n    console.print(\"${list.length(b)}\")\n";
        assert_eq!(interpreter::run(prog).expect("interp"), ["3"]);
        assert_eq!(run_linked_on_wasm(&[("main", prog)], "main"), ["3"], "wasm");
        // Reuse after move is rejected everywhere.
        let reuse = "fn main(console: Console):\n    let a = [1, 2, 3]\n    let b = move a\n    console.print(\"${list.length(b) + list.length(a)}\")\n";
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
        let escapes = "fn id(let xs: List(Int)) -> List(Int):\n    xs\nfn main(c: Console):\n    c.print(\"${list.length(id([1, 2, 3]))}\")\n";
        let err = typeck::check_str(escapes).expect_err("escaping borrow must be rejected");
        assert!(err.to_string().contains("cannot be returned"), "{err}");
        // Reading it (no escape) is fine.
        let reads = "fn count(let xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    c.print(\"${count([1, 2, 3])}\")\n";
        assert!(typeck::check_str(reads).is_ok(), "a read-only borrow should check");
    }

    #[test]
    fn match_soundness_exhaustiveness_and_linearity() {
        // C3: an infinite scalar domain needs a catch-all — a guard-only match is
        // non-exhaustive and would trap at runtime, so it's rejected at check time.
        let guard_only = "fn f(n: Int) -> String:\n    match n:\n        m if m > 0 -> \"p\"\n        z if z < 0 -> \"n\"\nfn main(c: Console):\n    c.print(f(1))\n";
        let e = typeck::check_str(guard_only).expect_err("guard-only Int match must be rejected");
        assert!(e.to_string().contains("non-exhaustive match on `Int`"), "{e}");

        // C2: a single-field variant matched only with a narrower sub-pattern
        // (`Circle(Red)`) is rejected when an inner case (`Circle(Blue)`) is
        // missing — the recursive coverage check catches the nested hole.
        let nested = "type Color:\n    Red\n    Blue\ntype Shape:\n    Circle(Color)\n    Square\nfn f(s: Shape) -> Int:\n    match s:\n        Circle(Red) -> 1\n        Square -> 2\nfn main(c: Console):\n    c.print(\"${f(Square)}\")\n";
        let e = typeck::check_str(nested).expect_err("nested non-exhaustive match must be rejected");
        assert!(e.to_string().contains("non-exhaustive"), "{e}");

        // ...but the idiomatic `Some(V) / None` form — `Some` covered by
        // ENUMERATING the inner variants, no wholesale `Some(_)` — must still check
        // (the conservative earlier rule wrongly rejected this; the recursion does not).
        let some_enum = "type Msg:\n    A\n    B\nfn f(o: Option(Msg)) -> Int:\n    match o:\n        Some(A) -> 1\n        Some(B) -> 2\n        None -> 0\nfn main(c: Console):\n    c.print(\"${f(Some(A))}\")\n";
        assert!(typeck::check_str(some_enum).is_ok(), "idiomatic Some(V)/None must check");

        // C5: a pattern may not bind the same name twice (no equality patterns).
        let dup = "type P:\n    P(Int, Int)\nfn f(p: P) -> Int:\n    match p:\n        P(x, x) -> x\nfn main(c: Console):\n    c.print(\"${f(P(3, 4))}\")\n";
        let e = typeck::check_str(dup).expect_err("duplicate pattern binding must be rejected");
        assert!(e.to_string().contains("more than once"), "{e}");

        // Valid exhaustive / linear matches still check (no over-rejection).
        let ok = "type Shape:\n    Circle(Int)\n    Square\nfn f(s: Shape) -> Int:\n    match s:\n        Circle(r) -> r\n        Square -> 0\nfn g(n: Int) -> Int:\n    match n:\n        0 -> 0\n        _ -> 1\nfn main(c: Console):\n    c.print(\"${f(Circle(3)) + g(5)}\")\n";
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
        let forge = "import redis\nfn main(console: Console, net: Net):\n    let c = Conn(net)\n    console.print(\"${redis.ping(c)}\")\n";
        let e = format!("{:?}", link(mods(forge), "app").expect_err("forge must be rejected"));
        assert!(e.contains("sealed capability") && e.contains("construct"), "{e}");
        // Unwrapping (destructuring) it in another module is rejected too.
        let unwrap = "import redis\nfn main(console: Console, net: Net):\n    let c = redis.open(net)\n    match c:\n        Conn(n) -> console.print(\"x\")\n";
        let e2 = format!("{:?}", link(mods(unwrap), "app").expect_err("unwrap must be rejected"));
        assert!(e2.contains("destructure"), "{e2}");
        // The legitimate path — mint via the library, then use it — links fine.
        let ok = "import redis\nfn main(console: Console, net: Net):\n    let c = redis.open(net)\n    console.print(\"${redis.ping(c)}\")\n";
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
        let borrow_self = "type Counter:\n    Counter(Int)\nimpl Counter:\n    fn incremented(let self) -> Counter:\n        match self:\n            Counter(n) -> Counter(n + 1)\nfn main(c: Console):\n    let a = Counter(5)\n    match a.incremented():\n        Counter(n) -> c.print(\"${n}\")\n";
        // `own self` — consume the receiver.
        let own_self = "import list\ntype Buffer:\n    Buffer(List(Int))\nimpl Buffer:\n    fn drain(own self) -> Int:\n        match self:\n            Buffer(xs) -> list.sum(xs)\nfn main(c: Console):\n    let buf = Buffer([1, 2, 3])\n    c.print(\"${buf.drain()}\")\n";
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
        let src = "fn owned_first(xs: List(Int)) -> Int:\n    list.at(xs, 0) * 2\n\nfn borrowed_len(let ys: List(Int)) -> Int:\n    list.length(ys)\n\nfn report(let xs: List(Int)) -> Int:\n    borrowed_len(xs) + owned_first(xs)\n\nfn main(c: Console):\n    let data = [5, 6, 7]\n    c.print(\"${report(data)}\")\n    c.print(\"${list.length(data)}\")\n";
        assert_eq!(interpreter::run(src).expect("interp"), ["13", "3"]);
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), ["13", "3"], "wasm");
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

    /// (RFC-0034 L3) Closure devirtualization fires and is gated. A single-bound,
    /// never-reassigned closure local `f` is called with a direct `call $__lamw{i}`
    /// with `direct-call` on and falls back to `call_indirect` under `-direct-call`.
    /// The test disables closure elision so the newer threaded `__lamt` path does
    /// not subsume the boxed-closure optimization being tested here. This is the
    /// call-SHAPE proof the lever fired — devirt moves no
    /// heap, so there is no `witchy stats` counter; OUTPUT invariance (and the
    /// captures-through-a-direct-call case) is the differential sweep's job.
    #[test]
    fn devirtualizes_single_bound_closure_call() {
        use crate::opt::{self, Opt, OptSet};
        let path =
            std::env::temp_dir().join(format!("witchy_devirt_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "fn main(console: Console):\n    let f = fn(x: Int): x % 7\n    var i = 0\n    var acc = 0\n    while i < 20:\n        acc = acc + f(i)\n        i = i + 1\n    console.print(\"${acc}\")\n",
        )
        .expect("write temp source");

        let direct_base = OptSet::default_set().without(Opt::ClosureElide);
        opt::set_for_tests(Some(direct_base));
        let on = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat on");
        opt::set_for_tests(Some(direct_base.without(Opt::DirectCall)));
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
            "fn main(console: Console):\n    let xs = [10, 20, 30, 40, 50]\n    var total = 0\n    for i in 0..list.length(xs):\n        total = total + xs[i]\n    console.print(\"${total}\")\n",
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

    /// THE UNIQUENESS ANALYSIS, observable: an alias taken BEFORE the loop
    /// zeroes the ownership token once — the first push re-owns (one copy)
    /// and everything after runs in place. The old syntactic whitelist
    /// disqualified the variable outright (O(n²), memory-cap trap at this
    /// size). The alias still sees its snapshot.
    #[test]
    fn analysis_alias_before_loop_stays_linear() {
        let src = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    let snapshot = xs\n    var i = 0\n    while i < 50000:\n        list.push(xs, i)\n        i = i + 1\n    console.print(\"${snapshot}\")\n    console.print(\"${list.length(xs)}\")\n";
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
        let src = "fn main(console: Console):\n    var ys = []\n    var last = [9]\n    var j = 0\n    while j < 200:\n        list.push(ys, j)\n        last = ys\n        j = j + 1\n    console.print(\"${list.length(last)}\")\n";
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
        let src = "fn peek(xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    var ws = []\n    var m = 0\n    var probe = 0\n    while m < 3000:\n        list.push(ws, m)\n        probe = peek(ws)\n        m = m + 1\n    console.print(\"${probe}\")\n";
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
        let src = "fn same(xs: List(Int)) -> List(Int):\n    xs\n\nfn main(console: Console):\n    var xs = [1]\n    var i = 0\n    while i < 100:\n        list.push(xs, i)\n        i = i + 1\n    let held = same(xs)\n    list.push(xs, 999)\n    console.print(\"${list.length(held)}\")\n    console.print(\"${list.length(xs)}\")\n";
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
        let src = "fn main(console: Console):\n    var s = \"ab\"\n    var k = 0\n    while k < 5:\n        s = s + s\n        k = k + 1\n    console.print(\"${s.length()}\")\n    var d = dict.new()\n    var zs = [1]\n    dict.insert(d, \"snap\", zs)\n    list.push(zs, 2)\n    console.print(\"${list.length(dict.get_or(d, \"snap\", []))}\")\n    console.print(\"${list.length(zs)}\")\n";
        let want: Vec<String> = ["64", "1", "2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// A lambda body is its own analysis unit: an accumulator inside one gets
    /// its own ownership token (this used to emit an undeclared `__cap`
    /// local — a loud compile failure).
    #[test]
    fn analysis_lambda_accumulator_compiles() {
        let src = "fn main(console: Console):\n    let build = fn(n: Int):\n        var acc = [0]\n        var t = 0\n        while t < n:\n            list.push(acc, t)\n            t = t + 1\n        list.length(acc)\n    console.print(\"${build(1000)}\")\n";
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
        let src = "import show\nimport cmp\nimport list\n\ntype Point derive(Show, PartialEq, Eq, PartialOrd, Ord):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let a = Point(1, 2)\n    let b = Point(1, 3)\n    show.say(console, a)\n    console.print(\"${eq(a, Point(1, 2))} ${eq(a, b)}\")\n    console.print(\"${less(a, b)} ${less(b, a)}\")\n    console.print(\"${list.contains([a, b], Point(1, 3))}\")\n";
        let want: Vec<String> = ["Point(1, 2)", "true false", "true false", "true"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // A derive now routes to a user generator `derive_<name>`; with none in
        // scope it's a loud error at comptime (the generated call can't resolve).
        let bad = "type T derive(Serialize):\n    n: Int\n\nfn main(console: Console):\n    console.print(\"x\")\n";
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
        let src = "import cmp\n\ntype Pair(a, b) derive(PartialEq, Eq, PartialOrd, Ord):\n    first: a\n    second: b\n\nfn main(console: Console):\n    let m = cmp.max_of(Pair(1, 9), Pair(1, 4))\n    console.print(\"${m.first} ${m.second}\")\n    console.print(\"${less(Pair(1, 2), Pair(1, 3))} ${less(Pair(2, 0), Pair(1, 9))}\")\n";
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
            "import sibling\nimport json\nimport result\n\ntype Foo derive(Deserialize):\n    x: Int\n\nfn main(console: Console):\n    console.print(\"${sibling.helper()}\")\n",
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
        let head = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n";
        let paren = format!("{head}    for (k, v) in dict.pairs(d):\n        console.print(\"${{k}}=${{v}}\")\n");
        let unparen = format!("{head}    for k, v in dict.pairs(d):\n        console.print(\"${{k}}=${{v}}\")\n");
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

    /// GENERIC IMPLS COMPOSE: reflection now reaches `List`, `Option`, tuples, and
    /// generic records through ordinary `impl Reflect for List(a)` etc. — a generic
    /// consumer (`json.stringify`, `where a: Reflect`) calling a generic impl method
    /// monomorphizes per element. No builtins; identical on both backends.
    #[test]
    fn reflection_covers_lists_options_tuples_and_generic_records() {
        let src = "import json\nimport reflect\n\ntype Box(a) derive(Reflect):\n    item: a\n\nfn main(console: Console):\n    console.print(json.stringify([1, 2, 3]))\n    console.print(json.stringify(Some(\"x\")))\n    console.print(json.stringify((\"p\", 5)))\n    console.print(json.stringify([(\"a\", \"b\")]))\n    console.print(json.stringify(Box([1, 2])))\n";
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
        let src = "import convert\n\ntype Celsius:\n    deg: Int\n\nimpl From(Int) for Celsius:\n    fn from(value: Int) -> Celsius:\n        Celsius(value)\n\nfn main(console: Console):\n    let c: Celsius = (5).into()\n    let d = Celsius.from(9)\n    console.print(\"${c.deg} ${d.deg}\")\n";
        let want = vec!["5 9".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `From`/`Into` reach `Json`: `impl From(a) for Json where a: Reflect` means any
    /// reflectable value converts — `x.into()` / `Json.from(x)` — and `server.send`
    /// serializes any reflectable response. Both backends.
    #[test]
    fn into_json_via_from() {
        let src = "import json\nfrom json import Json\n\nfn main(console: Console):\n    let j: Json = [1, 2, 3].into()\n    console.print(json.encode(j))\n    console.print(json.encode(Json.from((\"x\", 5))))";
        let want = vec!["[1,2,3]".to_string(), "[\"x\",5]".to_string()];
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

    /// ANONYMOUS STRUCTS: `.{ field: expr, … }` is an ad-hoc reflectable record (a
    /// generic synthetic type carrying `derive(Reflect)`), so `json.stringify(.{…})`
    /// works on any field types — including a `List` of tuples — with no per-type
    /// boilerplate. Fields render in sorted order; `.{…}` round-trips through fmt.
    #[test]
    fn anonymous_structs_reflect_to_json() {
        let src = "import json\n\nfn main(console: Console):\n    let files = [(\"a\", \"x\"), (\"b\", \"y\")]\n    console.print(json.stringify(.{files: files}))\n    console.print(json.stringify(.{name: \"acme\", count: 5}))\n";
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

    /// Uniform `var` calls must preserve the element-type refinement that the
    /// former `xs = list.push(xs, value)` shape supplied through assignment.
    /// This is especially important for generated `Reflect` implementations,
    /// where leaving `xs` as a bare `List` produces an invalid specialization.
    #[test]
    fn var_call_refines_empty_list_for_generated_reflect_on_both_backends() {
        let src = "import json\n\nfn main(console: Console):\n    var rows = []\n    for name in [\"ada\"]:\n        rows.push(.{name: name, score: 7})\n    console.print(json.stringify(.{rows: rows}))\n";
        let want = vec!["{\"rows\":[{\"name\":\"ada\",\"score\":7}]}".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// REFLECTION: `json.stringify(x)` encodes ANY value with no `derive(Json)` —
    /// only `derive(Reflect)`, the one generated impl every reflective library
    /// consumes. Covers scalars, nested records, `List`, and `Option` (Some/None),
    /// identical on both backends (the generated `reflect` is ordinary witchy code).
    #[test]
    fn reflective_json_encode_without_derive() {
        let src = "import json\nimport reflect\n\ntype Point derive(Reflect):\n    x: Int\n    y: Int\n\ntype Line derive(Reflect):\n    head: Point\n    tail: Point\n    tags: List(String)\n    note: Option(String)\n\nfn main(console: Console):\n    console.print(json.stringify(Point(1, 2)))\n    console.print(json.stringify(Line(Point(0, 0), Point(3, 4), [\"a\", \"b\"], Some(\"hi\"))))\n    console.print(json.stringify(Line(Point(5, 6), Point(7, 8), [], None)))\n";
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
        let tree = "import json\nimport reflect\n\ntype Tree derive(Reflect):\n    Leaf(Int)\n    Node(List(Tree))\n\nfn main(console: Console):\n    console.print(json.stringify(Node([Leaf(1), Node([Leaf(2)])])))\n";
        let tw = vec!["{\"$variant\":\"Node\",\"$values\":[[{\"$variant\":\"Leaf\",\"$values\":[1]},{\"$variant\":\"Node\",\"$values\":[[{\"$variant\":\"Leaf\",\"$values\":[2]}]]}]]}".to_string()];
        assert_eq!(link_run(tree), tw, "interpreter (tree)");
        assert_eq!(wasm_run(tree), tw, "wasm (tree)");
        // An already-built Json embeds verbatim in an anonymous struct.
        let embed = "import json\nfrom json import Json\n\nfn main(console: Console):\n    let rec: Json = json.decode(\"{\\\"a\\\":1}\").unwrap_or(JsonNull)\n    console.print(json.stringify(.{record: rec, ok: true}))";
        let ew = vec!["{\"ok\":true,\"record\":{\"a\":1}}".to_string()];
        assert_eq!(link_run(embed), ew, "interpreter (embed)");
        assert_eq!(wasm_run(embed), ew, "wasm (embed)");
    }

    /// Reflection's built-in protocol matrix includes scalar-like `Duration` and
    /// common std containers `Result`/`Set`, so `json.stringify` and
    /// `reflect.debug` do not arbitrarily stop at a few older container types.
    #[test]
    fn reflection_protocol_covers_duration_result_and_set() {
        let src = "import json\nimport reflect\nimport set\nimport duration\n\nfn main(console: Console):\n    let ok: Result(Int, String) = Ok(7)\n    let err: Result(Int, String) = Err(\"bad\")\n    let s = set.from_list([2, 1, 2])\n    console.print(json.stringify(1500ms))\n    console.print(reflect.debug(duration.seconds(2)))\n    console.print(json.stringify(ok))\n    console.print(json.stringify(err))\n    console.print(json.stringify(s))\n    console.print(reflect.debug(s))\n";
        let want: Vec<String> = [
            "1500",
            "2000",
            "{\"$variant\":\"Ok\",\"$values\":[7]}",
            "{\"$variant\":\"Err\",\"$values\":[\"bad\"]}",
            "[2,1]",
            "[2, 1]",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `a ?? b` (RFC-0048) is THE fallback: `Option(T) ?? T -> T` and
    /// `Result(T, e) ?? T -> T`, short-circuiting (the fallback runs only on
    /// `None`/`Err`), chaining right-associatively — and `||` stays Bool-only
    /// logical-or. Both backends must agree: the wasm path is a store-once/
    /// tag-test value-if where the interpreter unwraps the runtime ctor, so
    /// this guards that they stay in sync.
    #[test]
    fn coalesce_fallback_both_backends() {
        let src = "import option\n\nfn find(b: Bool) -> Option(String):\n    if b: Some(\"hit\") else: None\n\nfn parse(s: String) -> Result(Int, String):\n    match s.parse_int():\n        Some(n) -> Ok(n)\n        None -> Err(\"bad int\")\n\nfn main(console: Console):\n    console.print(find(true) ?? \"fallback\")\n    console.print(find(false) ?? \"fallback\")\n    console.print(\"${parse(\"41\") ?? 0}\")\n    console.print(\"${parse(\"x\") ?? 9}\")\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    console.print(\"${d.get(\"a\") ?? d.get(\"b\") ?? 0}\")\n    console.print(\"${d.get(\"z\") ?? d.get(\"b\") ?? 5}\")\n    console.print(\"${Some(\"\") ?? \"x\"}\")\n    console.print(\"${false || true}\")\n";
        let want: Vec<String> = ["hit", "fallback", "41", "9", "1", "5", "", "true"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// The fallback side of `??` is LAZY: it must not run when the left is
    /// `Some`/`Ok` — observable through a printing side effect, on both backends.
    #[test]
    fn coalesce_fallback_is_lazy_both_backends() {
        let src = "import option\n\nfn side(console: Console, tag: String, v: Int) -> Int:\n    console.print(\"eval ${tag}\")\n    v\n\nfn main(console: Console):\n    let a = Some(1) ?? side(console, \"unreached\", 2)\n    console.print(\"${a}\")\n    let b = None ?? side(console, \"reached\", 3)\n    console.print(\"${b}\")\n";
        let want: Vec<String> =
            ["1", "eval reached", "3"].iter().map(|s| s.to_string()).collect();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter run");
        assert_eq!(interp, want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// RFC-0048's other half: the truthy fallback is GONE. `||` on a String (or
    /// any non-Bool) is a check-time teaching error pointing at `??`, and `??`
    /// on a non-Option/Result left side is rejected too.
    #[test]
    fn or_is_bool_only_teaching_errors() {
        let err = typeck::check_str(
            "fn main(console: Console):\n    console.print(\"\" || \"default\")\n",
        )
        .expect_err("String || must be rejected");
        assert!(
            err.contains("`||` is logical-or on Bool") && err.contains("use `??`"),
            "unexpected message: {err}"
        );
        let err = typeck::check_str(
            "fn main(console: Console):\n    let n = 1 ?? 2\n    console.print(\"${n}\")\n",
        )
        .expect_err("Int ?? must be rejected");
        assert!(
            err.contains("`??` unwraps an Option or a Result"),
            "unexpected message: {err}"
        );
    }

    // ---- RFC-0052: one pattern grammar ------------------------------------

    /// (RFC-0052) Integer range patterns `lo..hi` / `lo..=hi` as real nodes, on
    /// both backends — half-open and inclusive, with a catch-all.
    #[test]
    fn range_patterns_backends_agree() {
        let src = "fn classify(n: Int) -> String:\n    match n:\n        0..10 -> \"low\"\n        10..=20 -> \"mid\"\n        _ -> \"high\"\n\nfn main(console: Console):\n    console.print(classify(5))\n    console.print(classify(10))\n    console.print(classify(20))\n    console.print(classify(99))\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["low", "mid", "mid", "high"]);
    }

    /// (RFC-0052) Nested or-patterns `Some(1 | 2 | 3)` — impossible before this
    /// RFC (parse error) — parse, check, and run identically on both backends.
    #[test]
    fn nested_or_patterns_backends_agree() {
        let src = "fn f(o: Option(Int)) -> String:\n    match o:\n        Some(1 | 2 | 3) -> \"small\"\n        Some(n) -> \"big\"\n        None -> \"none\"\n\nfn main(console: Console):\n    console.print(f(Some(2)))\n    console.print(f(Some(9)))\n    console.print(f(None))\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["small", "big", "none"]);
    }

    /// (RFC-0052) Binding or-patterns `Circle(n) | Square(n)` — every alternative
    /// binds the same name; the arm body sees the matched alternative's value.
    #[test]
    fn binding_or_patterns_backends_agree() {
        let src = "type Shape:\n    Circle(Int)\n    Square(Int)\n\nfn size(s: Shape) -> Int:\n    match s:\n        Circle(n) | Square(n) -> n\n\nfn main(console: Console):\n    console.print(\"${size(Circle(3))}\")\n    console.print(\"${size(Square(7))}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["3", "7"]);
    }

    /// (RFC-0052) Duration literal patterns `1s`/`-1s` — exact ms equality — and
    /// the `-1s` negative-duration lexer/typeck fix, on both backends.
    #[test]
    fn duration_patterns_backends_agree() {
        let src = "fn f(d: Duration) -> String:\n    match d:\n        1s -> \"one\"\n        -1s -> \"neg\"\n        _ -> \"other\"\n\nfn main(console: Console):\n    console.print(f(1s))\n    console.print(f(-1s))\n    console.print(f(5s))\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["one", "neg", "other"]);
    }

    /// (RFC-0052) A Float SCRUTINEE bound to a variable pattern now compiles (the
    /// former check-passes/codegen-fails hole) and agrees on both backends.
    #[test]
    fn float_scrutinee_binding_backends_agree() {
        let src = "fn main(console: Console):\n    let r = match 1.5:\n        x -> x + 1.0\n    console.print(\"${r}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2.5"]);
    }

    /// (RFC-0052) `let` destructuring — nested tuples AND a single-variant record
    /// pattern — the same grammar as `match`, both backends.
    #[test]
    fn let_destructure_patterns_backends_agree() {
        let src = "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let ((a, b), c) = ((1, 2), 3)\n    console.print(\"${a} ${b} ${c}\")\n    let Point(px, py) = Point(10, 20)\n    console.print(\"${px} ${py}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["1 2 3", "10 20"]);
    }

    /// (RFC-0052) `for` and comprehension take the SAME pattern grammar: a tuple
    /// header destructures each element, on both backends.
    #[test]
    fn for_and_comprehension_patterns_backends_agree() {
        let src = "fn main(console: Console):\n    let pairs = [(1, 2), (3, 4)]\n    for (a, b) in pairs:\n        console.print(\"${a}+${b}\")\n    let sums = [a + b for (a, b) in pairs]\n    console.print(\"${sums}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["1+2", "3+4", "[3, 7]"]);
    }

    /// (RFC-0052) The refutability rule and literal-pattern edges — check-time
    /// teaching errors, message-pinned.
    #[test]
    fn pattern_refutability_and_literal_edges_errors() {
        // A refutable `let` (multi-variant ctor) points at `if let`.
        let err = typeck::check_str(
            "type Shape:\n    Circle(Int)\n    Square(Int)\n\nfn main(console: Console):\n    let Circle(r) = Circle(3)\n    console.print(\"${r}\")\n",
        )
        .expect_err("refutable let must be rejected");
        assert!(
            err.contains("can fail") && err.contains("if let"),
            "unexpected message: {err}"
        );
        // Float literal patterns are rejected with the precision-trap teaching error.
        let err = typeck::check_str(
            "fn main(console: Console):\n    match 1.5:\n        1.5 -> console.print(\"a\")\n        _ -> console.print(\"b\")\n",
        )
        .expect_err("float literal pattern must be rejected");
        assert!(
            err.contains("Float literals cannot be matched"),
            "unexpected message: {err}"
        );
        // Or-pattern alternatives must bind the same names at the same types.
        let err = typeck::check_str(
            "type T:\n    A(Int)\n    B(String)\n\nfn main(console: Console):\n    match A(1):\n        A(x) | B(x) -> console.print(\"${x}\")\n",
        )
        .expect_err("inconsistent or-binding types must be rejected");
        assert!(
            err.contains("or-pattern binding") && err.contains("inconsistent"),
            "unexpected message: {err}"
        );
    }

    /// REFLECTION, SECOND USE CASE: `reflect.debug(x)` renders any value from the
    /// SAME `reflect` that powers `json` — proving the engine is general, not a
    /// json-specific hack. Records, lists-in-fields, and scalars, both backends.
    #[test]
    fn reflective_debug_render_other_use_case() {
        let src = "import reflect\n\ntype Point derive(Reflect):\n    x: Int\n    y: Int\n\ntype Bag derive(Reflect):\n    items: List(Int)\n    label: String\n\nfn main(console: Console):\n    console.print(reflect.debug(Point(1, 2)))\n    console.print(reflect.debug(Bag([1, 2, 3], \"nums\")))\n    console.print(reflect.debug(42))\n";
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
        let src = r#"import meta
import json
from json import Json

type Point:
    x: Int
    y: Int

type User:
    name: String
    age: Int
    active: Bool

comptime:
    let ctor = fn(ty: meta.TypeExpr) -> String:
        match ty:
            meta.TNamed(name, _args) ->
                if name == "Int": "JsonInt"
                else if name == "String": "JsonString"
                else if name == "Bool": "JsonBool"
                else: "JsonNull"
            _ -> "JsonNull"
    for t in module_types:
        match t.kind:
            meta.TypeRecord ->
                emit("fn to_json_${t.name}(v: ${t.name}) -> Json:")
                var pairs = []
                for f in t.fields:
                    list.push(pairs, "(\"" + f.name + "\", " + ctor(f.type_expr) + "(v." + f.name + "))")
                emit("    JsonObject([" + list.join(pairs, ", ") + "])")
                emit("")
            _ -> Nil

fn main(console: Console):
    console.print(json.encode(to_json_Point(Point(1, 2))))
    console.print(json.encode(to_json_User(User("ann", 30, true))))"#;
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
        let src = "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"  host  \".trim(), \"localhost\")\n    let parts = \"port=8080\".split(\"=\")\n    dict.insert(d, list.at(parts, 0), list.at(parts, 1))\n    dict.insert(d, \"lit\" + \"eral\", \"joined\")\n    match d.get(\"host\"):\n        Some(v) -> console.print(\"host=\" + v)\n        None -> console.print(\"host MISSING\")\n    match d.get(\"port\"):\n        Some(v) -> console.print(\"port=\" + v)\n        None -> console.print(\"port MISSING\")\n    console.print(\"${dict.contains_key(d, \"literal\")}\")\n    console.print(\"${dict.length(d)}\")\n";
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
    /// RFC-0046 step 3: `list.contains`/`index_of` now carry a `where a: Eq`
    /// bound, so a record element type derives `Eq` (its content equality) to
    /// use them — which is exactly what makes them monomorphize on WASM.
    #[test]
    fn generic_equality_on_records_is_structural() {
        let src = "import list\nimport cmp\n\ntype Point derive(PartialEq, Eq):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let pts = [Point(1, 2), Point(3, 4)]\n    let probe = Point(1 + 2, 4)\n    console.print(\"${list.contains(pts, probe)}\")\n    console.print(\"${list.index_of(pts, Point(1, 2)) ?? -1}\")\n";
        let want: Vec<String> = ["true", "0"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
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

    /// THE OWN-ABI: `xs = grow(move xs, i)` is a linear pipeline — the
    /// ownership token crosses the call in both directions (an extra cap
    /// param and result), so a cross-function builder stays O(n). Without
    /// the transfer each call re-owned by copy: O(n²) — the reowns counter
    /// (not timing) is the proof. (The interpreter leg stays small: it
    /// clones at every call by design.)
    #[test]
    fn analysis_own_abi_pipelines_in_place() {
        let src = "fn grow(own xs: List(Int), n: Int) -> List(Int):\n    list.push(xs, n)\n    xs\n\nfn main(console: Console):\n    var xs = [0]\n    var i = 0\n    while i < 3000:\n        xs = grow(move xs, i)\n        i = i + 1\n    console.print(\"${list.length(xs)}\")\n    console.print(\"${list.at(xs, 3000)}\")\n";
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
        let src = "fn cap_at(own xs: List(Int), n: Int) -> List(Int):\n    if list.length(xs) >= n:\n        []\n    else:\n        list.push(xs, n)\n        xs\n\nfn main(console: Console):\n    var xs = [0]\n    var i = 0\n    while i < 50:\n        xs = cap_at(move xs, i)\n        i = i + 1\n    console.print(\"${xs}\")\n";
        let interp = link_run(src);
        assert_eq!(wasm_run(src), interp, "wasm must agree on the mixed paths");
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

    /// (BUG-558) A loop body that calls a user function with a heap-shaped `var`
    /// argument writes the callee's result back into an outer local. The loop
    /// watermark cannot rewind the heap after that write-back: the grown list is
    /// now observable outside the body and later allocations may otherwise
    /// overwrite its header.
    #[test]
    fn loop_watermark_rejects_outer_var_writeback() {
        let src = "type Buf:\n    items: List(Int)\n\nfn add(var b: Buf, x: Int) -> Nil:\n    list.push(b.items, x)\n    return\n\nfn main(console: Console):\n    var b = Buf(items: [])\n    var i = 0\n    while i < 16:\n        add(b, i)\n        i = i + 1\n    console.print(\"${list.at(b.items, 15)}\")\n    console.print(\"${list.length(b.items)}\")\n";
        let expected = vec!["15".to_string(), "16".to_string()];
        assert_eq!(link_run(src), expected, "interpreter");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// (BUG-558 sharpened) The same loop-watermark escape must be rejected when
    /// the `var` callee writes back a record field directly. The list case used
    /// to corrupt the length header; the dict case read garbage memory.
    #[test]
    fn loop_watermark_rejects_outer_var_record_field_writeback() {
        let list_src = "import list\n\ntype Buf:\n    items: List(Int)\n\nfn add(var b: Buf, x: Int):\n    list.push(b.items, x)\n\nfn main(console: Console):\n    var b = Buf([])\n    var i = 0\n    while i < 16:\n        add(b, i)\n        i = i + 1\n    console.print(\"${list.at(b.items, 15)}\")\n    console.print(\"${list.length(b.items)}\")\n";
        let list_expected = vec!["15".to_string(), "16".to_string()];
        assert_eq!(link_run(list_src), list_expected, "interpreter: list field");
        assert_eq!(wasm_run(list_src), list_expected, "wasm: list field");

        let dict_src = "import dict\n\ntype Tally:\n    counts: Dict(Int, Int)\n\nfn bump(var t: Tally, k: Int):\n    dict.insert(t.counts, k, k * 2)\n\nfn main(console: Console):\n    var t = Tally(dict.new())\n    var i = 0\n    while i < 50:\n        bump(t, i)\n        i = i + 1\n    console.print(\"${dict.get_or(t.counts, 49, 0)}\")\n    console.print(\"${dict.length(t.counts)}\")\n";
        let dict_expected = vec!["98".to_string(), "50".to_string()];
        assert_eq!(link_run(dict_src), dict_expected, "interpreter: dict field");
        assert_eq!(wasm_run(dict_src), dict_expected, "wasm: dict field");

        let sibling_src = "import dict\nimport set\n\ntype Bag:\n    counts: Dict(String, Int)\n    seen: Set(String)\n\nfn inc(n: Int) -> Int:\n    n + 1\n\nfn main(console: Console):\n    var bag = Bag(dict.new(), set.new())\n    var i = 0\n    while i < 16:\n        bag.counts.update(\"hit\", 0, inc)\n        bag.seen.insert(\"k${i}\")\n        i = i + 1\n    console.print(\"${dict.get_or(bag.counts, \"hit\", 0)}\")\n    console.print(\"${set.length(bag.seen)}\")\n";
        let sibling_expected = vec!["16".to_string(), "16".to_string()];
        assert_eq!(link_run(sibling_src), sibling_expected, "interpreter: sibling fields");
        assert_eq!(wasm_run(sibling_src), sibling_expected, "wasm: sibling fields");
    }

    /// Std containers may expose real inherent methods without reopening bare
    /// std functions or losing the owner-module in-place path. `List.push` and
    /// `List.concat` are declared as `impl List(a)` methods, but receiver calls
    /// resolve to `list.push`/`list.concat` when those owner functions exist.
    #[test]
    fn std_list_impl_methods_and_free_functions_coexist_on_both_backends() {
        let src = "import list\n\ntype Buf:\n    items: List(Int)\n\nfn main(console: Console):\n    var b = Buf([])\n    var i = 0\n    while i < 16:\n        b.items.push(i)\n        i = i + 1\n    console.print(\"${list.at(b.items, 15)}\")\n    console.print(\"${list.length(b.items)}\")\n\n    var xs = [1]\n    xs.push(2)\n    xs = xs.concat([3, 4])\n    console.print(\"${xs}\")\n\n    list.push(xs, 5)\n    let ys = list.concat(xs, [6])\n    console.print(\"${ys}\")\n";
        let expected = vec![
            "15".to_string(),
            "16".to_string(),
            "[1, 2, 3, 4]".to_string(),
            "[1, 2, 3, 4, 5, 6]".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter: std List impl/free methods");
        assert_eq!(wasm_run(src), expected, "wasm: std List impl/free methods");
    }

    /// The same owner-module method pattern applies to the other mutable core
    /// containers: receiver syntax is available, but the stable `dict.*`/`set.*`
    /// functions still exist and remain the in-place backend target.
    #[test]
    fn std_dict_set_impl_methods_and_free_functions_coexist_on_both_backends() {
        let src = "import dict\nimport set\n\ntype Bag:\n    counts: Dict(String, Int)\n    seen: Set(String)\n\nfn inc(n: Int) -> Int:\n    n + 1\n\nfn main(console: Console):\n    var bag = Bag(dict.new(), set.new())\n    var i = 0\n    while i < 20:\n        bag.counts.update(\"hit\", 0, inc)\n        bag.seen.insert(\"k${i}\")\n        i = i + 1\n    bag.counts.insert(\"extra\", 7)\n    bag.counts.remove(\"extra\")\n    bag.seen.remove(\"k0\")\n    console.print(\"${dict.get_or(bag.counts, \"hit\", 0)}\")\n    console.print(\"${dict.length(bag.counts)}\")\n    console.print(\"${set.length(bag.seen)}\")\n    console.print(\"${set.contains(bag.seen, \"k0\")}\")\n\n    var d = dict.new()\n    dict.insert(d, \"x\", 1)\n    dict.remove(d, \"missing\")\n    var s = set.new()\n    set.insert(s, \"x\")\n    set.remove(s, \"missing\")\n    console.print(\"${dict.get_or(d, \"x\", 0)}\")\n    console.print(\"${set.contains(s, \"x\")}\")\n";
        let expected = vec![
            "20".to_string(),
            "1".to_string(),
            "19".to_string(),
            "false".to_string(),
            "1".to_string(),
            "true".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter: std Dict/Set impl/free methods");
        assert_eq!(wasm_run(src), expected, "wasm: std Dict/Set impl/free methods");
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
        let src = "import list\nimport dict\nfn main(console: Console):\n    var acc = dict.new()\n    var i = 0\n    let base = [1, 2, 3, 4, 5]\n    while i < 2000:\n        let scratch = list.concat(base, base)\n        let n = list.length(scratch)\n        dict.insert(acc, i % 8, n)\n        i = i + 1\n    console.print(\"${dict.length(acc)}\")\n";
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


    // HEAP-TYPE MATRIX (RFC-0035 step 3 gate). Corpus 1-3 above only exercised RECORD/ADT
    // elements — the false confidence that let the reverted emission ship (5e9e167): it
    // assumed every i32 element was an offset-0 rc_alloc object, which was FALSE for the
    // header-less strings/lists/dicts from the direct-bump helpers. Phase A now routes every
    // value producer through $rc_alloc, so these element types are all headered. This matrix
    // is the gate that would have caught the revert: each element type the revert corrupted,
    // read-past-set_at / aliased / stored / match-on-read, must stay byte-identical across
    // interp == wasm == wasm(rc-floor). Authored FIRST, before re-applying the dup/drop
    // emission — a premature/wrong dec flips one of these red.


    /// (RFC-0051 I1 / SEC-039) Regression: the free-at-overwrite path must not free a
    /// non-owning-object pointer. The 7-line repro — `var t = "abc"; t = t.trim()`
    /// — reassigns a `var` whose FIRST buffer is a string LITERAL (a data-segment pointer
    /// BELOW `heap_base`, not an `$rc_alloc` object). Under `inplace + rc-floor` the
    /// free-at-overwrite emitted `$rc_free(old)` directly on that literal; `$rc_free` had
    /// NO `heap_base` guard (only `$dup`/`$drop` did), so it linked the literal into the
    /// free-list and corrupted its length word — a later `$rc_alloc` reuse handed out the
    /// poisoned pointer and `string.trim`'s result rendered MEGABYTES of raw heap
    /// (an in-guest disclosure). I1's categorical `ptr >= heap_base` floor on `$rc_free`
    /// (matching `$dup`/`$drop`) kills the class. Assert byte-identical output across the
    /// FULL opt sweep — the leak fired under `rc-floor` alone, so the sweep is the net.
    #[test]
    fn rc_free_at_overwrite_does_not_free_a_literal_sec_039() {
        use crate::opt::{self, Opt, OptSet};
        let src = "fn main(console: Console):\n    var xs = [3, 1, 2]\n    list.sort(xs)\n    console.print(\"${xs}\")\n    var t = \"abc\"\n    t = t.trim()\n    console.print(\"[${t}]\")\n";
        let oracle = link_run(src);
        assert_eq!(oracle, vec!["[1, 2, 3]", "[abc]"], "oracle shape changed");
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
            assert_eq!(out, oracle, "SEC-039: WITCHY_OPT={label} leaked/diverged (freed a non-object literal)");
        }
    }

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
            "import http\nfn main(console: Console, net: Net):\n    let res = http.get(net, \"127.0.0.1\", {port}, \"/greet\")\n    console.print(f\"{{http.status(res)}} {{http.body(res)}}\")\n"
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
            .expect_lowered("the binary path lowers this program");
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
            "import http\nfn main(console: Console, net: Net):\n    match http.try_get(net, \"127.0.0.1\", {port}, \"/\"):\n        Ok(_) -> console.print(\"ok\")\n        Err(_) -> console.print(\"err\")\n"
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
            .expect_lowered("the binary path lowers this program");
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

    /// `region:` Phase 1 (rfcs/regions.md): the syntax parses (with optional
    /// `-> T` ascription), the block's value escapes, scalar outer
    /// assignments are allowed, and both backends agree — a region NEVER
    /// changes observable behavior, only when memory is reclaimed.
    #[test]
    fn region_blocks_value_escape_and_parity() {
        let src = "\nfn main(console: Console):\n    let summary: String = region:\n        var parts = []\n        for i in 0..50:\n            list.push(parts, \"${i}\")\n        list.join(parts, \",\")\n    console.print(\"${summary.length()}\")\n    var n = 0\n    let direct = region -> Int:\n        n = n + 42\n        n\n    console.print(\"${direct}\")\n";
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
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\ntype Reading:\n    sensor: String\n    values: List(Int)\n\nfn main(console: Console):\n    let st = region -> Stack(Int):\n        Push(1, Push(2, Empty))\n    console.print(\"${st == Push(1, Push(2, Empty))}\")\n    let r = region -> Reading:\n        var vs = []\n        for i in 0..50:\n            list.push(vs, i * i)\n        Reading(sensor: \"t\" + \"0\", values: vs)\n    console.print(r.sensor)\n    console.print(\"${list.at(r.values, 49)}\")\n    let d = region -> Dict(String, Int):\n        var m = dict.new()\n        for i in 0..100:\n            dict.insert(m, \"k\" + \"${i}\", i)\n        m\n    console.print(\"${dict.get_or(d, \"k42\", 0 - 1)}\")\n    let shared = \"parent-side\"\n    let s = region -> String:\n        shared\n    console.print(s)\n    let nested = region -> Int:\n        let inner: String = region -> String:\n            \"abc\" + \"def\"\n        inner.length()\n    console.print(\"${nested}\")\n";
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
        let src = "fn main(console: Console):\n    var total = 0\n    var keep = []\n    for i in 0..100000:\n        let last = region -> Int:\n            var row = []\n            var j = 0\n            for j in 0..1000:\n                list.push(row, j)\n            list.at(row, 999)\n        total = total + last\n        list.push(keep, i)\n    console.print(\"${total}\")\n    console.print(\"${list.length(keep)}\")\n";
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
                .expect_lowered("the binary path lowers this program");
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
            "fn main(console: Console):\n    let shared = \"twelve chars\"\n    let s = region -> String:\n        shared\n    console.print(s)\n",
        );
        assert_eq!(out, vec!["twelve chars"]);
        assert_eq!(copied, 0, "parent passthrough must copy nothing");
        // Region-born value: exactly its own block (4-byte header + 6 bytes).
        let (out, copied) = run_and_count(
            "fn main(console: Console):\n    let s = region -> String:\n        \"abc\" + \"def\"\n    console.print(s)\n",
        );
        assert_eq!(out, vec!["abcdef"]);
        assert_eq!(copied, 10, "a region-born string copies header + bytes");
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

    /// (BUG-286, BUG-034) `derive(Deserialize)` composes `Option` at any depth —
    /// inside a `List` and nested in another `Option` — decoding JSON `null` to
    /// `None`. The generated code uses prelude `Result`/`Option` names without
    /// requiring redundant `import result` / `import option` lines.
    #[test]
    fn derive_deserialize_nested_option_backends_agree() {
        let src = "import json\n\ntype Rec derive(Deserialize):\n    xs: List(Option(Int))\n    oo: Option(Option(Int))\n\nfn main(console: Console):\n    match json.decode(\"{\\\"xs\\\": [1, null, 3], \\\"oo\\\": 7}\"):\n        Ok(j) -> match Rec.from_json(j):\n            Ok(r) -> console.print(\"${r.xs} ${r.oo}\")\n            Err(e) -> console.print(\"err\")\n        Err(e) -> console.print(\"parse\")\n";
        let want = ["[Some(1), None, Some(3)] Some(Some(7))"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-496) `derive(Deserialize)` must not bind decoded fields under source
    /// field names. Fields named like generator helpers (`j`) or constructors
    /// (`Ok`/`Err`/`Some`/`None`) decode normally, and later fields still read from
    /// the original JSON object.
    #[test]
    fn derive_deserialize_field_names_are_hygienic_on_both_backends() {
        let src = "import json\n\ntype Odd derive(Deserialize):\n    j: String\n    Ok: String\n    Err: String\n    Some: String\n    None: String\n    rest: Option(List(Option(Int)))\n\nfn main(console: Console):\n    match json.decode(\"{\\\"j\\\": \\\"jay\\\", \\\"Ok\\\": \\\"ok\\\", \\\"Err\\\": \\\"err\\\", \\\"Some\\\": \\\"some\\\", \\\"None\\\": \\\"none\\\", \\\"rest\\\": [1, null, 3]}\"):\n        Ok(doc) -> match Odd.from_json(doc):\n            Ok(r) ->\n                console.print(r.j + \":\" + r.Ok + \":\" + r.Err + \":\" + r.Some + \":\" + r.None)\n                console.print(\"${r.rest}\")\n            Err(e) -> console.print(\"err \" + json.deserialize_error_message(e))\n        Err(e) -> console.print(\"parse\")\n";
        let want = ["jay:ok:err:some:none", "Some([Some(1), None, Some(3)])"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-532) Tuple fields are outside the documented `derive(Deserialize)`
    /// contract for now. Reject them at the derive boundary instead of emitting a
    /// fallback call like `(Int, String).from_json(...)` and leaking `Tuple2` in a
    /// later type error.
    #[test]
    fn derive_deserialize_rejects_tuple_fields_without_generated_fallback_leak() {
        let src = "import json\nimport result\n\ntype PairBox derive(Deserialize):\n    pair: (Int, String)\n";
        let err = try_link_std(src).expect_err("tuple field must be rejected by derive");
        assert!(err.contains("derive(Deserialize)"), "{err}");
        assert!(err.contains("tuple field `pair`"), "{err}");
        assert!(!err.contains("Tuple2"), "must not leak generated tuple fallback: {err}");
        assert!(!err.contains("from_json"), "must not leak generated from_json fallback: {err}");
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

    /// (BUG-299) `derive(Show)` on a GENERIC type renders identically on both
    /// backends (was a check-passes/interp-runs/WASM-rejects split: the derived body
    /// routed through structural render). Now field-wise, matching interpolation
    /// byte-for-byte.
    #[test]
    fn derive_show_generic_backends_agree() {
        let src = "import show\n\ntype Box(a) derive(Show):\n    value: a\n\ntype Color derive(Show):\n    Red\n    Named(String)\n\ntype Score derive(Show):\n    n: Int\n    name: String\n\nfn main(console: Console):\n    console.print(show(Box(value: 42)))\n    console.print(show(Box(value: [1, 2, 3])))\n    console.print(show(Red))\n    console.print(show(Named(\"blue\")))\n    console.print(show(Score(n: 12, name: \"beta\")))\n";
        let want = ["Box(42)", "Box([1, 2, 3])", "Red", "Named(blue)", "Score(12, beta)"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-300) Field projection on a method-call-chain result compiles on both
    /// backends (`list.sort(xs).at(0).label`, `[..].at(0).label`) — the record type
    /// of the chain result comes from the type table, not a local-var-only map.
    #[test]
    fn field_projection_on_call_chain_backends_agree() {
        let src = "import cmp\n\ntype Top derive(Ord, PartialOrd, Eq, PartialEq):\n    label: String\n\nfn main(console: Console):\n    var xs = [Top(label: \"b\"), Top(label: \"a\")]\n    list.sort(xs)\n    console.print(xs.at(0).label)\n    console.print([Top(label: \"b\"), Top(label: \"a\")].at(0).label)\n    console.print(list.at([Top(label: \"b\"), Top(label: \"a\")], 0).label)\n";
        let want = ["a", "b", "b"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-315, RFC-0044 rule 3) An out-of-range (or negative) `xs[i] = v` /
    /// `list.set_at` / `list.update_at` is a runtime error on BOTH backends,
    /// matching the `xs[i]` READ trap — never a silent no-op. In-bounds still agrees.
    #[test]
    fn oob_list_set_at_traps_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("lowers");
            (linked, bytes)
        };
        for prog in [
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    xs[5] = 9\n    console.print(\"${xs}\")\n",
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    xs[0 - 1] = 9\n    console.print(\"${xs}\")\n",
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    list.set_at(xs, 5, 9)\n    console.print(\"${xs}\")\n",
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    list.update_at(xs, 9, fn(x: Int): x + 1)\n    console.print(\"${xs}\")\n",
        ] {
            let (lmod, wasm) = compile(prog);
            assert!(interpreter::run_module(lmod, ".", Vec::new()).is_err(), "interp must trap: {prog}");
            assert!(crate::run_wasm_bytes(&wasm).is_err(), "wasm must trap: {prog}");
        }
        let ok = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    xs[1] = 9\n    console.print(\"${xs}\")\n";
        assert_eq!(link_run(ok), ["[1, 9, 3]"], "interp in-bounds");
        assert_eq!(wasm_run(ok), ["[1, 9, 3]"], "wasm in-bounds");
    }

    /// (BUG-318) Anonymous-record `==` and `"${…}"` work on both backends — the
    /// structural eq/render build the shape from the inline field types.
    #[test]
    fn anonymous_record_eq_and_show_backends_agree() {
        let src = "fn main(console: Console):\n    let a = .{x: 1, y: \"hi\"}\n    let b = .{x: 1, y: \"hi\"}\n    let c = .{x: 2, y: \"hi\"}\n    console.print(\"${a == b}\")\n    console.print(\"${a == c}\")\n    console.print(\"${a}\")\n";
        let want = ["true", "false", ".{x: 1, y: hi}"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
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

    /// (BUG-399) `derive(Deserialize)` on a GENERIC record reconstructs on both
    /// backends: the impl carries its type params + a per-param `Deserialize` bound,
    /// and the caller ascribes the concrete type.
    #[test]
    fn derive_deserialize_generic_backends_agree() {
        let src = "import json\nimport result\n\ntype Inner derive(Deserialize):\n    n: Int\n\ntype Box(a) derive(Deserialize):\n    value: a\n\nfn main(console: Console):\n    match json.decode(\"{\\\"value\\\": {\\\"n\\\": 7}}\"):\n        Ok(j) ->\n            let r: Result(Box(Inner), json.DeserializeError) = Box.from_json(j)\n            match r:\n                Ok(b) -> console.print(\"${b.value.n}\")\n                Err(e) -> console.print(\"err\")\n        Err(e) -> console.print(\"parse\")\n";
        let want = ["7"];
        assert_eq!(link_run(src), want, "interp");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-407) A `region -> SomeRecord:` result reclaims on the compiled backend
    /// (records were silently falling back to a plain block with NO reclaim). The
    /// `__region_copy_bytes` counter proves the record's block is copied out (> 0,
    /// was 0), and the value agrees with the interpreter.
    #[test]
    fn region_copy_out_reclaims_record_result() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let p = region -> Point:\n        var acc = Point(x: 0, y: 0)\n        for i in [1, 2, 3, 4, 5]:\n            acc = Point(x: acc.x + i, y: acc.y + i * 2)\n        acc\n    console.print(\"${p.x}\")\n    console.print(\"${p.y}\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("lowers");
        let mut rt = Runtime::batch().expect("rt");
        let mut actor = rt.spawn(&bytes, Capabilities { print: true, quiet: true, ..Default::default() }, 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), vec!["15", "30"]);
        assert!(actor.region_copy_bytes().expect("counter") > 0, "record region must copy its block out (was 0 = plain-block fallback)");
        assert_eq!(link_run(src), vec!["15", "30"], "interp agrees on the value");
    }

    /// `region:` rejections: an outer pointer-typed assignment and a `yield`
    /// are type errors — the region's only pointer escape is its value.
    #[test]
    fn region_rejects_outer_pointer_assign_and_yield() {
        let leak = "fn main(console: Console):\n    var leak = [1]\n    let x = region:\n        list.push(leak, 2)\n        7\n    console.print(\"${x}\")\n";
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
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..3000:\n        dict.insert(d, \"k\" + \"${i}\", i * 2)\n    console.print(\"${dict.length(d)}\")\n    console.print(\"${dict.get_or(d, \"k2999\", 0 - 1)}\")\n    console.print(\"${dict.get_or(d, \"absent\", 0 - 1)}\")\n    console.print(\"${dict.contains_key(d, \"k1500\")}\")\n    dict.remove(d, \"k0\")\n    console.print(\"${dict.length(d)}\")\n    dict.insert(d, \"again\", 7)\n    console.print(\"${dict.get_or(d, \"again\", 0 - 1)}\")\n";
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
    /// trap on the compiled backend and fail here. The 50k run is compiled-only:
    /// the guard's teeth are that WASM heap trap, and the interpreter's
    /// clone-per-push is quadratic by design (it is the semantic oracle, not a
    /// perf target — see the arena watermark test), so the oracle verifies the
    /// same program at parity scale instead of burning a minute at 50k.
    #[test]
    fn list_reverse_flatten_flat_map_are_linear_at_scale() {
        let src = |n: u32| {
            format!(
                "fn main(console: Console):\n    var xs = []\n    for i in 0..{n}:\n        list.push(xs, i)\n    var r = xs\n    list.reverse(r)\n    console.print(\"${{list.at(r, 0)}}\")\n    console.print(\"${{list.at(r, {last})}}\")\n    console.print(\"${{list.flatten([[1, 2], [], [3]])}}\")\n    console.print(\"${{list.flat_map([1, 2, 3], fn(x: Int): [x, x * 10])}}\")\n",
                last = n - 1
            )
        };
        let want = |n: u32| -> Vec<String> {
            vec![
                (n - 1).to_string(),
                "0".to_string(),
                "[1, 2, 3]".to_string(),
                "[1, 10, 2, 20, 3, 30]".to_string(),
            ]
        };
        assert_eq!(link_run(&src(1000)), want(1000), "interpreter");
        assert_eq!(wasm_run(&src(1000)), want(1000), "compiled WASM must agree");
        assert_eq!(wasm_run(&src(50000)), want(50000), "compiled at 50k must stay linear");
    }

    /// IN-PLACE SET_AT: `xs = list.set_at(xs, i, v)` mutates the owned buffer's
    /// slot in place (O(1)) via `$list_set_cap`, instead of rebuilding the whole
    /// list each set — which is O(n^2) memory that traps the WASM bump allocator
    /// at ~10k. An aliased list keeps the copying set_at (the alias still sees the
    /// original); a set does not change the length. (An out-of-range index traps —
    /// see `oob_list_set_at_traps_on_both_backends`, BUG-315.)
    #[test]
    fn inplace_set_at_is_fast_and_alias_safe() {
        let src = |n: u32| {
            format!(
                "fn main(console: Console):\n    var xs = []\n    for i in 0..{n}:\n        list.push(xs, 0)\n    var k = 0\n    while k < {n}:\n        list.set_at(xs, k, k * 2)\n        k = k + 1\n    console.print(\"${{list.at(xs, {last})}}\")\n    list.set_at(xs, {last}, 7)\n    console.print(\"${{list.length(xs)}}\")\n    var ys = [1, 2, 3]\n    let alias = ys\n    list.set_at(ys, 1, 99)\n    console.print(\"${{list.at(ys, 1)}}\")\n    console.print(\"${{list.at(alias, 1)}}\")\n",
                last = n - 1
            )
        };
        let want = |n: u32| -> Vec<String> {
            vec![((n - 1) * 2).to_string(), n.to_string(), "99".to_string(), "2".to_string()]
        };
        // Parity (incl. alias semantics) on both backends at small n; the O(n^2)
        // rebuild trap is compiled-only, so only WASM pays the at-scale run.
        assert_eq!(link_run(&src(500)), want(500), "interpreter");
        assert_eq!(wasm_run(&src(500)), want(500), "compiled WASM must agree");
        assert_eq!(wasm_run(&src(5000)), want(5000), "compiled at 5k must stay in place");
    }

    /// IN-PLACE UPDATE_AT: `xs = list.update_at(xs, i, f)` applies the closure to
    /// the owned buffer's slot in place (O(1)) via `$list_update_cap`, instead of
    /// rebuilding the whole list each update (O(n^2), OOM-prone). Alias-safe (a
    /// shared list keeps the copy); an update does not change the length. (An
    /// out-of-range index traps — see `oob_list_set_at_traps_on_both_backends`, BUG-315.)
    #[test]
    fn inplace_update_at_is_fast_and_alias_safe() {
        let src = |n: u32| {
            format!(
                "fn main(console: Console):\n    var xs = []\n    for i in 0..{n}:\n        list.push(xs, 1)\n    var k = 0\n    while k < {n}:\n        list.update_at(xs, k, fn(v: Int): v + 1)\n        k = k + 1\n    console.print(\"${{list.at(xs, {last})}}\")\n    list.update_at(xs, {last}, fn(v: Int): v + 1)\n    console.print(\"${{list.length(xs)}}\")\n    var ys = [1, 2, 3]\n    let alias = ys\n    list.update_at(ys, 1, fn(v: Int): v + 100)\n    console.print(\"${{list.at(ys, 1)}}\")\n    console.print(\"${{list.at(alias, 1)}}\")\n",
                last = n - 1
            )
        };
        let want = |n: u32| -> Vec<String> {
            vec!["2".to_string(), n.to_string(), "102".to_string(), "2".to_string()]
        };
        // Parity (incl. alias semantics) on both backends at small n; the O(n^2)
        // rebuild trap is compiled-only, so only WASM pays the at-scale run.
        assert_eq!(link_run(&src(500)), want(500), "interpreter");
        assert_eq!(wasm_run(&src(500)), want(500), "compiled WASM must agree");
        assert_eq!(wasm_run(&src(5000)), want(5000), "compiled at 5k must stay in place");
    }

    /// IN-PLACE DICT INSERT: `d = dict.insert(d, k, v)` updates/appends into owned
    /// entry slack (no per-insert table copy); an aliased dict keeps the
    /// copying insert, so the alias still sees the original.
    #[test]
    fn inplace_dict_insert_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..2000:\n        dict.insert(d, i, i * 2)\n    console.print(\"${dict.length(d)}\")\n    console.print(\"${dict.get_or(d, 1999, 0 - 1)}\")\n    var e = dict.new()\n    let alias = e\n    dict.insert(e, 1, 10)\n    console.print(\"${dict.length(alias)}\")\n    console.print(\"${dict.length(e)}\")\n";
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
        let src = "fn main(console: Console):\n    var total = 0\n    for i in 0..200000:\n        var row = []\n        var j = 0\n        for j in 0..1000:\n            list.push(row, j)\n        total = total + list.at(row, 999)\n    console.print(\"${total}\")\n";
        assert_eq!(wasm_run(src), vec!["199800000"]);
    }

    /// IN-PLACE STRING APPEND: the builder pattern `s = s + piece` appends
    /// into owned byte slack (amortized O(1)); a literal-seeded alias keeps
    /// the copying path, so the interned literal is never mutated.
    #[test]
    fn inplace_string_append_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var s = \"\"\n    for i in 0..20000:\n        s = s + \"ab\"\n    console.print(\"${s.length()}\")\n    var t = \"seed\"\n    let alias = t\n    t = t + \"!\"\n    console.print(alias)\n    console.print(t)\n";
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
        let src = "fn main(console: Console):\n    var xs = []\n    for i in 0..50000:\n        list.push(xs, i)\n    console.print(\"${list.length(xs)}\")\n    console.print(\"${list.at(xs, 49999)}\")\n    var small = [1]\n    let alias = small\n    list.push(small, 2)\n    console.print(\"${alias}\")\n    console.print(\"${small}\")\n";
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
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..10000:\n        dict.insert(d, i, i)\n    console.print(\"${dict.length(d)}\")\n    var counts = dict.new()\n    for i in 0..30000:\n        dict.update(counts, i % 3, 0, fn(n: Int): n + 1)\n    console.print(\"${dict.get_or(counts, 0, 0)}\")\n    var small = dict.new()\n    dict.insert(small, 1, 10)\n    let alias = small\n    dict.insert(small, 2, 20)\n    console.print(\"${dict.length(alias)}\")\n    console.print(\"${dict.length(small)}\")\n";
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
        let src = "fn main(console: Console):\n    let add = fn(x: Int): x + 1\n    let twice = fn(y: Int): add(add(y))\n    console.print(\"${twice(3)}\")\n    var n = 10\n    let snap = fn(): n\n    n = 99\n    console.print(\"${snap()}\")\n";
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

    /// A negative `Int` that enters a list through a *generic* function (the
    /// element type is a type variable, so it crosses the i32 generic ABI) and is
    /// then read back through *concrete* `List(Int)` code must keep its sign on
    /// WASM. `to_slot` used to zero-extend, turning -1 into 4294967295 when a
    /// concrete reader loaded the i64 slot; it now sign-extends (pointers/Bools
    /// are < 2^31, so they're unaffected). Regression for the generic-list bug
    /// found via `list.repeat(-1, n)`.
    #[test]
    fn wasm_negative_int_survives_the_generic_list_abi() {
        let src = "fn fill(x: a, n: Int) -> List(a):\n    var out = []\n    var i = 0\n    while i < n:\n        list.push(out, x)\n        i = i + 1\n    out\n\nfn show(xs: List(Int)) -> String:\n    var out = \"\"\n    for v in xs:\n        out = out + \"${v}\" + \" \"\n    out\n\nfn main(console: Console):\n    console.print(show(fill(-1, 3)))\n";
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
        let src = "fn count_eq(xs: List(a), target: a) -> Int:\n    var n = 0\n    for x in xs:\n        if x == target:\n            n = n + 1\n    n\n\nfn b(s: String) -> String:\n    s + \"\"\n\nfn main(console: Console):\n    console.print(\"${count_eq([b(\"aa\"), b(\"bb\"), b(\"aa\")], b(\"aa\"))}\")\n";
        let want = vec!["2".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// (RFC-0047) A Dict keyed by `Float` is a compile-time error — keys require
    /// `Eq`, and `Float` is only `PartialEq` (NaN != NaN, so a NaN key is
    /// unretrievable and `0.1 + 0.2` is a precision trap). This closes the NaN-key
    /// hole wholesale (breaking change: Float keys used to compile and run). The
    /// error teaches the standard escapes (a scaled Int, or a String rendering).
    #[test]
    fn dict_float_keys_are_a_compile_error() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, 1.5, \"a\")\n    console.print(dict.get_or(d, 1.5, \"?\"))\n";
        let e = typeck::check(&resolve_std_src(src))
            .expect_err("a Float-keyed dict must be rejected")
            .to_string();
        assert!(
            e.contains("not a valid `Dict` key") && e.contains("Eq"),
            "teaching error naming the Eq requirement, got: {e}"
        );
        // The NaN case (the original hole) is rejected by the same type rule,
        // before any runtime lookup can silently miss.
        let nan = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, 0.0 / 0.0, \"nan\")\n    console.print(dict.get_or(d, 0.0 / 0.0, \"missing\"))\n";
        assert!(
            typeck::check(&resolve_std_src(nan)).expect_err("a NaN Float key must be rejected").to_string().contains("not a valid `Dict` key"),
            "the NaN-key hole is closed by the type rule"
        );
        // An Int-keyed dict (the suggested escape) still works on both backends.
        let ok = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, 3, \"a\")\n    console.print(dict.get_or(d, 3, \"?\"))\n";
        assert_eq!(interp(ok), vec!["a"], "interpreter (Int key)");
        assert_eq!(run_on_wasm(ok), vec!["a"], "compiled WASM (Int key)");
    }

    /// (RFC-0047) A `Set` of `Float` is likewise a compile-time error — members
    /// require `Eq`. The Set stdlib already documents this doctrine; the type rule
    /// makes it true.
    #[test]
    fn set_float_members_are_a_compile_error() {
        let src = "import set\n\nfn main(console: Console):\n    var s = set.new()\n    set.insert(s, 1.5)\n    console.print(\"${set.length(s)}\")\n";
        let linked = resolve_std_src(src);
        let e = typeck::check(&linked).expect_err("a Float-membered set must be rejected").to_string();
        assert!(e.contains("not a valid `Set` member") && e.contains("Eq"), "teaching error, got: {e}");
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

    /// `now` (Clock) and `get_env` (Env) compile to capability-gated host
    /// imports. `get_env` is deterministic given the process env, so both
    /// backends must agree exactly; `now` is wall-clock, so each backend is
    /// checked for plausibility instead. Also exercises a multi-capability
    /// `main` (Console + Env / Console + Clock), which codegen now accepts.
    #[test]
    fn clock_and_env_compile_to_wasm_and_agree() {
        let host_path = std::env::var("PATH").expect("the test process has PATH");
        let env_src = "import option\n\nfn main(console: Console, env: Env):\n    match env.get_env(\"PATH\"):\n        Some(v) -> console.print(\"got: \" + v)\n        None -> console.print(\"unset\")\n    match env.get_env(\"WITCHY_E2E_DEFINITELY_UNSET\"):\n        Some(v) -> console.print(\"got: \" + v)\n        None -> console.print(\"unset\")\n";
        let want = vec![format!("got: {host_path}"), "unset".to_string()];
        let module = parser::parse_module(env_src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        assert_eq!(link_run(env_src), want.clone(), "interpreter");
        assert_eq!(crate::run_wasm_bytes(&bytes).expect("wasm"), want, "compiled WASM must agree");

        // The clock: both backends must yield a plausible epoch-milliseconds.
        let clock_src = "fn main(console: Console, clock: Clock):\n    console.print(if clock.now() > 1500000000000: \"plausible\" else: \"implausible\")\n";
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

        let src = "fn main(console: Console, dir: Dir):\n    console.print(dir.read(\"a.txt\"))\n    console.print(\"${dir.exists(\"a.txt\")}\")\n    console.print(\"${dir.exists(\"missing.txt\")}\")\n    let sub = dir.subtree(\"sub\")\n    console.print(sub.read(\"b.txt\"))\n    dir.write(\"out.txt\", \"written\")\n    console.print(dir.read(\"out.txt\"))\n    dir.make_dir(\"made\")\n    console.print(\"${dir.is_dir(\"made\")}\")\n    for name in dir.list():\n        console.print(\"entry: \" + name)\n";
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
            .expect_lowered("the binary path lowers this program");
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
                "fn main(console: Console, dir: Dir):\n    console.print(dir.read(\"{bad}\"))\n"
            );
            assert!(interpreter::run_in(&esc, &root).is_err(), "interp must reject `{bad}`");
            let m = parser::parse_module(&esc).expect("parse");
            let wbytes = codegen::compile_module_binary(&m)
                .expect_lowered("the binary path lowers this program");
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

    /// RFC-0011: `dir.only(Dir.ext(...))` confines a `Dir` to an ENTRY policy —
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
        let ok_src = "fn main(console: Console, dir: Dir):\n    let txt = dir.only(Dir.ext(\".txt\"))\n    console.print(txt.read(\"ok.txt\"))\n";
        let want = vec!["hello".to_string()];
        assert_eq!(
            interpreter::run_module(resolve_std_src(ok_src), &root_str, Vec::new()).expect("interp"),
            want,
            "interpreter",
        );
        let bytes = codegen::compile_module_binary(&resolve_std_src(ok_src))
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt.spawn(&bytes, caps(), 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        // Denied: a `.key` through the same narrowed Dir is refused on both backends.
        let bad_src = "fn main(console: Console, dir: Dir):\n    let txt = dir.only(Dir.ext(\".txt\"))\n    console.print(txt.read(\"secret.key\"))\n";
        assert!(
            interpreter::run_module(resolve_std_src(bad_src), &root_str, Vec::new()).is_err(),
            "interp must refuse a .key",
        );
        let bbytes = codegen::compile_module_binary(&resolve_std_src(bad_src))
            .expect_lowered("the binary path lowers this program");
        let mut rt2 = Runtime::batch().expect("runtime");
        let mut a = rt2.spawn(&bbytes, caps(), 64).expect("spawn");
        assert!(a.run().is_err(), "WASM must refuse a .key");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RFC-0011: the `kind:` Dir entry policy. `dir.only(Dir.files())` admits a file
    /// read but DENIES opening a sub-directory; `dir.only(Dir.dirs())` is the mirror.
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
                .expect_lowered("the binary path lowers this program");
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
                .expect_lowered("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("runtime");
            let mut actor = rt.spawn(&bytes, caps(), 64).expect("spawn");
            assert!(actor.run().is_err(), "wasm should refuse: {src}");
        };

        // `files()`: read a file OK; opening a sub-directory DENIED (the DoD headline).
        ok_both(
            "fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.files())\n    console.print(d.read(\"ok.txt\"))\n",
            vec!["hello".to_string()],
        );
        err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.files())\n    let s = d.subtree(\"sub\")\n    console.print(\"unreached\")\n");

        // `dirs()`: open a sub-directory OK; reading a file DENIED (the mirror).
        ok_both(
            "fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.dirs())\n    let s = d.subtree(\"sub\")\n    console.print(\"traversed\")\n",
            vec!["traversed".to_string()],
        );
        err_both("fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.dirs())\n    console.print(d.read(\"ok.txt\"))\n");

        // An `ext`-only policy still traverses — kind gates directories, ext gates files.
        ok_both(
            "fn main(console: Console, dir: Dir):\n    let d = dir.only(Dir.ext(\".txt\"))\n    let s = d.subtree(\"sub\")\n    console.print(\"traversed\")\n",
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
        let src = "fn main(console: Console, dir: Dir):\n    let s = dir.subtree(\"sub\")\n    console.print(s.read(\"b.txt\"))\n    console.print(s.subtree(\"deep\").read(\"c.txt\"))\n";
        let want = vec!["beta".to_string(), "gamma".to_string()];

        let interp_out = interpreter::run_in(src, &root).expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
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
        let esc = "fn main(console: Console, dir: Dir):\n    console.print(dir.subtree(\"sub\").read(\"../a.txt\"))\n";
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

        let src = "fn main(console: Console, dir: Dir):\n    console.print(dir.read_file(\"note.txt\").read())\n    let out = dir.write_file(\"out.txt\")\n    out.write(\"beta\")\n    console.print(dir.read_file(\"out.txt\").read())\n";
        let want = vec!["alpha".to_string(), "beta".to_string()];
        let interp_out = interpreter::run_in(src, &root).expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
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
        let esc = "fn main(console: Console, dir: Dir):\n    console.print(dir.read_file(\"../escape.txt\").read())\n";
        assert!(interpreter::run_in(esc, &root).is_err(), "interp rejects `..` via open");
        let m = parser::parse_module(esc).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
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
        let src = "fn main(console: Console, first: File[Read], second: File[Read]):\n    console.print(first.read())\n    console.print(second.read())\n";
        let want = vec!["alpha".to_string(), "beta".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let interp_out = interpreter::run_module_files(module, &root, vec![a_txt.clone(), b_txt.clone()])
            .expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
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
        let src = "fn main(console: Console, runner: Exec, dir: Dir):\n    console.print(runner.exec(dir, \"greet\", \"a\\0b\", \"hi\"))\n";

        let interp_out = interpreter::run_in(src, &root).expect("interp");
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
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
        let esc = "fn main(console: Console, runner: Exec, dir: Dir):\n    console.print(runner.exec(dir, \"../escape\", \"\", \"\"))\n";
        assert!(interpreter::run_in(esc, &root).is_err(), "interp must reject escape");
        let m = parser::parse_module(esc).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
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
        let src = "fn main(console: Console, da: Dir, db: Dir):\n    console.print(da.read(\"f.txt\"))\n    console.print(db.read(\"f.txt\"))\n";
        let want = vec!["from-A".to_string(), "from-B".to_string()];

        let interp_out =
            interpreter::run_in_dirs(src, &[dir_a.clone(), dir_b.clone()]).expect("interp");
        assert_eq!(interp_out, want, "interpreter multi-dir");

        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
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
            "fn main(console: Console, net: Net):\n    let sock = net.connect(\"{addr}\")\n    sock.send_line(\"hello\")\n    console.print(sock.recv_line())\n    sock.close()\n"
        );
        let want = vec!["echo: hello".to_string()];
        assert_eq!(
            interpreter::run_with(&src, ".", vec![addr.clone()]).expect("interp"),
            want,
            "interpreter"
        );
        let module = parser::parse_module(&src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
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
        let bad = "fn main(console: Console, net: Net):\n    let sock = net.connect(\"127.0.0.1:1\")\n    console.print(\"connected\")\n";
        assert!(
            interpreter::run_with(bad, ".", vec![addr.clone()]).is_err(),
            "interp must reject a non-allowlisted address"
        );
        let m = parser::parse_module(bad).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
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
        let listener_src = "fn main(console: Console, net: Net):\n    let l = net.listen(\"127.0.0.1:39999\")\n    console.print(\"listening\")\n";
        let m = parser::parse_module(listener_src).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
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
        let client = "fn main(console: Console, net: Net):\n    let s = net.connect(\"127.0.0.1:1\")\n    console.print(\"x\")\n";
        let m = parser::parse_module(client).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
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
        let writer = "fn main(console: Console, dir: Dir):\n    dir.write(\"x.txt\", \"data\")\n    console.print(\"wrote\")\n";
        let module = parser::parse_module(writer).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
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
        let reader = "fn main(console: Console, dir: Dir):\n    console.print(dir.read(\"x.txt\"))\n";
        let m = parser::parse_module(reader).expect("parse");
        let wbytes = codegen::compile_module_binary(&m)
            .expect_lowered("the binary path lowers this program");
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
            "fn main(console: Console, clock: Clock):\n    console.print(\"${clock.now()}\")\n",
            "import option\n\nfn main(console: Console, env: Env):\n    match env.get_env(\"X\"):\n        Some(v) -> console.print(v)\n        None -> console.print(\"unset\")\n",
        ];
        for src in srcs {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
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
        let src = "type Pt:\n    x: Int\n    y: Int\ntype Bag:\n    items: List(Int)\nfn main(console: Console):\n    console.print(\"${[1, 2, 3] == [1, 2, 3]}\")\n    console.print(\"${[1, 2, 3] == [1, 9, 3]}\")\n    console.print(\"${[[1], [2]] == [[1], [2]]}\")\n    console.print(\"${(1, \"a\") == (1, \"a\")}\")\n    console.print(\"${(1, \"a\") != (1, \"b\")}\")\n    console.print(\"${Pt(1, 2) == Pt(1, 2)}\")\n    console.print(\"${Pt(1, 2) == Pt(3, 4)}\")\n    console.print(\"${[Pt(1, 2)] == [Pt(1, 2)]}\")\n    console.print(\"${Bag([1, 2]) == Bag([1, 2])}\")\n    console.print(\"${[\"a\", \"b\"] == [\"a\", \"b\"]}\")\n";
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

    /// (RFC-0047) A CUSTOM `PartialEq` impl is honored at EVERY depth — top level
    /// AND inside a `List`, `Option`, tuple, and as a `Dict` value. Before, a
    /// custom impl silently vanished below the surface (the container did a
    /// structural memcmp): `P(1) == P(2)` called the impl (`true`) but
    /// `[P(1)] == [P(2)]` was `false`. Both backends must now honor it uniformly.
    /// (The impl here is always-`true`, so any honored comparison yields `true`;
    /// a structural memcmp of differing fields would yield `false` — the tell.)
    #[test]
    fn custom_partial_eq_is_honored_at_every_depth() {
        let src = "type P:\n    P(Int)\n\nimpl PartialEq for P:\n    fn eq(self, other: P) -> Bool:\n        true\n\nfn main(console: Console):\n    console.print(\"${P(1) == P(2)}\")\n    console.print(\"${[P(1)] == [P(2)]}\")\n    console.print(\"${Some(P(1)) == Some(P(2))}\")\n    console.print(\"${(P(1), 0) == (P(2), 0)}\")\n    var a = dict.new()\n    dict.insert(a, 1, P(1))\n    var b = dict.new()\n    dict.insert(b, 1, P(2))\n    console.print(\"${a == b}\")\n";
        let want = vec![
            "true".to_string(), // top-level impl (as before)
            "true".to_string(), // inside a List — NEW: was false
            "true".to_string(), // inside an Option — NEW
            "true".to_string(), // inside a tuple — NEW
            "true".to_string(), // as a Dict value — NEW
        ];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), want, "compiled WASM must agree");
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

    /// (RFC-0047) The fast-path invariant: a `derive(PartialEq)` type keeps the
    /// STRUCTURAL comparison at every depth (no impl dispatch), so a program with
    /// no CUSTOM impl behaves exactly as before. A derived record differing in a
    /// field is unequal inside a container.
    #[test]
    fn derived_partial_eq_stays_structural_in_containers() {
        let src = "type Pt derive(PartialEq):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    console.print(\"${[Pt(1, 2), Pt(3, 4)] == [Pt(1, 2), Pt(3, 4)]}\")\n    console.print(\"${[Pt(1, 2)] == [Pt(9, 9)]}\")\n    console.print(\"${Some(Pt(1, 2)) == Some(Pt(1, 2))}\")\n";
        let want = vec!["true".to_string(), "false".to_string(), "true".to_string()];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), want, "compiled WASM must agree");
    }

    /// Structural `==` on sum types: nullary enums and concrete-field variants
    /// compare by tag (then by the matched variant's fields) on both backends.
    /// (Regression for the silent ADT pointer-compare divergence.)
    #[test]
    fn adt_structural_equality_agrees_on_both_backends() {
        let src = "type Color:\n    Red\n    Green\n    Blue\ntype Shape:\n    Circle(Int)\n    Square(Int)\nfn main(console: Console):\n    console.print(\"${Red == Red}\")\n    console.print(\"${Red == Blue}\")\n    console.print(\"${Circle(3) == Circle(3)}\")\n    console.print(\"${Circle(3) == Circle(4)}\")\n    console.print(\"${Circle(3) == Square(3)}\")\n    console.print(\"${[Red, Green] == [Red, Green]}\")\n";
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

    /// Interpolating a record field — `"${p.x}"` (scalar) and `"${p.tags}"`
    /// (compound) — renders on WASM, including inside a custom `Show` impl. A
    /// field access previously resolved to no value type, so `to_string` of it
    /// errored on the compiled backend even though the field's type is known.
    #[test]
    fn record_field_interpolation_renders_on_wasm() {
        let src = "type Post:\n    title: String\n    views: Int\n    tags: List(Int)\nfn main(console: Console):\n    let p = Post(\"hi\", 9, [1, 2, 3])\n    console.print(\"${p.title} (${p.views}): ${p.tags}\")\n";
        assert_eq!(run_on_wasm(src), vec!["hi (9): [1, 2, 3]".to_string()]);
    }

    /// `Option` `==` is structural on both backends: a single-parameter generic
    /// ADT is instantiated at the comparison site from a constructor literal
    /// (sound for both operands — the type checker guarantees they share a
    /// type). Dict `==` compares by key/value contents, not insertion order.
    /// (Closes the former loud-error gaps.)
    #[test]
    fn option_and_dict_equality_agree_on_both_backends() {
        let src = "import option\n\nfn pair(a: Int, b: Int) -> Dict(String, Int):\n    var d = dict.new()\n    dict.insert(d, \"k\", a)\n    dict.insert(d, \"j\", b)\n    d\n\nfn main(console: Console):\n    let none_i: Option(Int) = None\n    console.print(\"${Some(5) == Some(5)}\")\n    console.print(\"${Some(5) == Some(6)}\")\n    console.print(\"${Some(5) == None}\")\n    console.print(\"${none_i == None}\")\n    console.print(\"${Some(\"a\") == Some(\"a\")}\")\n    console.print(\"${Some(\"a\") == Some(\"b\")}\")\n    let a = pair(1, 2)\n    let b = pair(1, 2)\n    let c = pair(1, 9)\n    var rev = dict.new()\n    dict.insert(rev, \"j\", 2)\n    dict.insert(rev, \"k\", 1)\n    console.print(\"${a == b}\")\n    console.print(\"${a == c}\")\n    console.print(\"${a == rev}\")\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
            "true".to_string(),
            "true".to_string(),
            "false".to_string(),
            "true".to_string(),  // identical insert order + contents
            "false".to_string(), // differing value
            "true".to_string(),  // same pairs, different insertion order
        ];
        // Dict `==` now lowers on the binary path as a content comparison,
        // matching the interpreter and the std `PartialEq` contract.
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

    /// Indexing a list out of bounds must FAIL on both backends, not silently
    /// read adjacent heap on WASM. The compiled `$list_at` bounds-checks and traps
    /// (like division-by-zero), matching the interpreter's "index out of bounds"
    /// error. In-bounds indexing still agrees. (Regression for a silent OOB-read
    /// divergence.)
    #[test]
    fn list_index_out_of_bounds_errors_on_both_backends() {
        let oob = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    console.print(\"${list.at(xs, 5)}\")\n";
        let module = parser::parse_module(oob).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        assert!(interpreter::run(oob).is_err(), "interpreter must error on OOB index");
        assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on OOB index");
        // A negative index likewise traps (it used to read backwards into the heap).
        let neg = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    console.print(\"${list.at(xs, 0 - 1)}\")\n";
        let nmod = parser::parse_module(neg).expect("parse");
        let nbytes = codegen::compile_module_binary(&nmod)
            .expect_lowered("the binary path lowers this program");
        assert!(interpreter::run(neg).is_err(), "interpreter must error on negative index");
        assert!(crate::run_wasm_bytes(&nbytes).is_err(), "WASM must trap on negative index");
        // In-bounds indexing still agrees.
        let ok = "fn main(console: Console):\n    let xs = [10, 20, 30]\n    console.print(\"${list.at(xs, 2)}\")\n";
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
        let src = "fn main(console: Console):\n    console.print(\"[\" + \"  \\t\\n hi \\r\u{0b}\".trim() + \"]\")\n    console.print(\"[\" + \"\u{0c} x \u{0c}\".trim() + \"]\")\n    console.print(\"[\" + \"\u{a0}y\u{a0}\".trim() + \"]\")\n";
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

    /// RFC-0090 makes proper self-tail calls a language guarantee, not an
    /// optimizer choice. This depth is far beyond either backend's ordinary
    /// call stack, and the argument swap pins simultaneous parameter rebinding.
    #[test]
    fn proper_self_tail_calls_use_constant_stack_on_both_backends() {
        let src = r#"
fn swap_down(n: Int, a: Int, b: Int) -> Int:
    match n:
        0 -> ((a * 10) + b)
        _ -> swap_down((n - 1), b, a)

fn main(console: Console):
    console.print("${swap_down(5000001, 2, 7)}")
"#;
        let want = vec!["72".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter trampoline");
        assert_eq!(run_on_wasm(src), want, "compiled WIR loop");
    }

    /// RFC-0090 direct recursive components use one typed dispatcher. Different
    /// source signatures occupy disjoint banks, and every edge still stages its
    /// arguments before changing logical functions.
    #[test]
    fn proper_mutual_tail_calls_use_constant_stack_on_both_backends() {
        let src = r#"
fn left(own n: Int, a: Int, b: Int) -> Int:
    match n:
        0 -> ((a * 10) + b)
        _ -> right((n - 1), b, a, "right")

fn right(own n: Int, a: Int, b: Int, label: String) -> Int:
    if n == 0:
        return (a * 10) + b
    return left((n - 1), b, a)

fn main(console: Console):
    console.print("${left(250001, 2, 7)}")
"#;
        let want = vec!["72".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter SCC trampoline");
        assert_eq!(run_on_wasm(src), want, "compiled WIR SCC dispatcher");
    }

    /// A function parameter stays genuinely indirect: `drive` cannot know which
    /// closure-table slot `f` carries. The dynamic edge and `step -> drive` form a
    /// recursive component that both backends must trampoline without Wasm tail calls.
    #[test]
    fn proper_indirect_closure_cycle_uses_constant_stack_on_both_backends() {
        let src = r#"
type Bounce:
    Bounce(fn(Bounce, Int) -> Int)

fn drive(bounce: Bounce, n: Int) -> Int:
    match bounce:
        Bounce(f) -> f(bounce, n)

fn step(bounce: Bounce, n: Int) -> Int:
    if n == 0:
        5000000007
    else:
        drive(bounce, n - 1)

fn main(console: Console):
    let bounce = Bounce(step)
    console.print("${drive(bounce, 250001)}")
"#;
        let want = vec!["5000000007".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter callable trampoline");
        assert_eq!(run_on_wasm(src), want, "compiled typed table dispatcher");
    }

    #[test]
    fn proper_singleton_indirect_cycle_uses_constant_stack_on_both_backends() {
        let src = r#"
type Bounce:
    Bounce(fn(Bounce, Int) -> Int)

fn main(console: Console):
    let bounce = Bounce(fn(b: Bounce, n: Int) -> Int:
        if n == 0:
            9
        else:
            match b:
                Bounce(f) -> f(b, n - 1)
    )
    match bounce:
        Bounce(f) -> console.print("${f(bounce, 30001)}")
"#;
        let want = vec!["9".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter singleton trampoline");
        assert_eq!(run_on_wasm(src), want, "compiled singleton dispatcher");
    }

    #[test]
    fn proper_dynamic_cycle_survives_multiple_named_hops_on_both_backends() {
        let src = r#"
type Bounce:
    Bounce(fn(Bounce, Int) -> Int)

fn first(bounce: Bounce, n: Int) -> Int:
    second(bounce, n)

fn second(bounce: Bounce, n: Int) -> Int:
    match bounce:
        Bounce(f) -> f(bounce, n)

fn step(bounce: Bounce, n: Int) -> Int:
    if n == 0:
        5000000007
    else:
        first(bounce, n - 1)

fn main(console: Console):
    let bounce = Bounce(step)
    console.print("${first(bounce, 30001)}")
"#;
        let want = vec!["5000000007".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter dynamic chain");
        assert_eq!(run_on_wasm(src), want, "compiled three-member dispatcher");
    }

    #[test]
    fn proper_indirect_cycles_adapt_scalar_result_slots_on_both_backends() {
        let src = r#"
type StringBounce:
    StringBounce(fn(StringBounce, Int) -> String)

type BoolBounce:
    BoolBounce(fn(BoolBounce, Int) -> Bool)

type FloatBounce:
    FloatBounce(fn(FloatBounce, Int) -> Float)

fn drive_string(bounce: StringBounce, n: Int) -> String:
    match bounce:
        StringBounce(f) -> f(bounce, n)

fn drive_bool(bounce: BoolBounce, n: Int) -> Bool:
    match bounce:
        BoolBounce(f) -> f(bounce, n)

fn drive_float(bounce: FloatBounce, n: Int) -> Float:
    match bounce:
        FloatBounce(f) -> f(bounce, n)

fn main(console: Console):
    let answer = "done"
    let strings = StringBounce(fn(b: StringBounce, n: Int) -> String:
        if n == 0:
            answer
        else:
            drive_string(b, n - 1)
    )
    let bools = BoolBounce(fn(b: BoolBounce, n: Int) -> Bool:
        if n == 0:
            true
        else:
            drive_bool(b, n - 1)
    )
    let floats = FloatBounce(fn(b: FloatBounce, n: Int) -> Float:
        if n == 0:
            1.5
        else:
            drive_float(b, n - 1)
    )
    console.print(drive_string(strings, 30001))
    console.print("${drive_bool(bools, 30001)}")
    console.print("${drive_float(floats, 30001)}")
"#;
        let want = vec!["done".to_string(), "true".to_string(), "1.5".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter scalar envelopes");
        assert_eq!(run_on_wasm(src), want, "compiled scalar slot adaptation");
    }

    #[test]
    fn proper_indirect_dispatcher_preserves_outside_component_fallback() {
        let src = r#"
type Bounce:
    Bounce(fn(Bounce, Int) -> Int)

fn drive(bounce: Bounce, n: Int) -> Int:
    match bounce:
        Bounce(f) -> f(bounce, n)

fn finish(bounce: Bounce, n: Int) -> Int:
    99

fn step(bounce: Bounce, n: Int) -> Int:
    if n == 0:
        drive(Bounce(finish), 0)
    else:
        drive(bounce, n - 1)

fn main(console: Console):
    let bounce = Bounce(step)
    console.print("${drive(bounce, 30001)}")
"#;
        let want = vec!["99".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter outside target");
        assert_eq!(run_on_wasm(src), want, "compiled indirect fallback");
    }

    /// Tail lowering must preserve the ordinary call dispatcher. Stdlib
    /// intrinsic declarations are recursive placeholders, not executable
    /// recursion, including when reached from a function value.
    #[test]
    fn proper_tail_calls_preserve_intrinsic_dispatch_on_both_backends() {
        let src = r#"
import list
import vm

fn upper(s: String) -> String:
    s.to_upper()

fn parallel_once(xs: List(Int)) -> List(Int):
    vm.par_map(xs, fn(n: Int): n + 1)

fn invoke(f: fn(List(Int)) -> List(Int), xs: List(Int)) -> List(Int):
    f(xs)

fn main(console: Console):
    console.print(upper("witchy"))
    let shouted = ["a", "b"].map(fn(s: String): s.to_upper())
    console.print(shouted.join("-"))
    console.print("${invoke(parallel_once, [1, 2])}")
"#;
        let want = vec![
            "WITCHY".to_string(),
            "A-B".to_string(),
            "[2, 3]".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter builtin dispatch");
        assert_eq!(run_on_wasm(src), want, "compiled builtin dispatch");
    }

    /// Generic templates and bounded trait methods are specialized before WIR
    /// proper-call lowering, so their concrete recursive edges use the same loops.
    #[test]
    fn specialized_generic_and_trait_tail_calls_are_proper_on_both_backends() {
        let src = r#"
fn keep(value: a, n: Int) -> a:
    if n == 0:
        value
    else:
        keep(value, n - 1)

trait Countdown:
    fn down(self, n: Int) -> Int

type Counter:
    value: Int

impl Countdown for Counter:
    fn down(self, n: Int) -> Int:
        if n == 0:
            self.value
        else:
            self.down(n - 1)

fn bounded(value: a, n: Int) -> Int where a: Countdown:
    value.down(n)

fn main(console: Console):
    console.print(keep("generic", 100001))
    console.print("${bounded(Counter(11), 100001)}")
"#;
        let want = vec!["generic".to_string(), "11".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter specialized trampoline");
        assert_eq!(run_on_wasm(src), want, "compiled specialized loops");
    }

    /// RFC-0087 structured returns commit every final `var` value together. A
    /// callee-side `?` is ordinary early return, so mutations completed before
    /// propagation remain visible on both the success and error paths.
    #[test]
    fn rfc0087_multi_var_try_commits_partial_progress_on_both_backends() {
        let src = "import result\n\nfn step(var left: Int, var right: Int, r: Result(Int, String)) -> Result(Int, String):\n    left = left + 100\n    right = right + 10\n    let got = r?\n    left = left + got\n    right = right + got * 2\n    Ok(left + right)\n\nfn main(console: Console):\n    var a = 1\n    var b = 10\n    let ok = step(a, b, Ok(5))\n    console.print(\"${a}\")\n    console.print(\"${b}\")\n    console.print(\"${ok.unwrap_or(0)}\")\n\n    var c = 2\n    var d = 20\n    let failed = step(c, d, Err(\"stop\"))\n    console.print(\"${c}\")\n    console.print(\"${d}\")\n    console.print(\"${failed.unwrap_or(-1)}\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("value-returning multi-var functions are valid");
        let want = vec!["106", "30", "136", "102", "30", "-1"];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpreter"),
            want,
            "interpreter commits every var on success and callee-side ?",
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers multi-var structured returns");
        assert_eq!(
            crate::run_wasm_bytes(&bytes).expect("wasm run"),
            want,
            "compiled backend matches the interpreter's ? write-back",
        );
    }

    #[test]
    fn rfc0087_structured_return_spellings_and_caller_propagation_agree() {
        let src = r#"import option
import result

fn via_try(var left: Int, var right: Int, r: Result(Int, String)) -> Result(Int, String):
    left = left + 100
    right = right + 10
    let got = r?
    Ok(got)

fn via_explicit_return(var left: Int, var right: Int, r: Result(Int, String)) -> Result(Int, String):
    left = left + 100
    right = right + 10
    match r:
        Ok(got) -> Ok(got)
        Err(message) -> return Err(message)

fn via_tail_err(var left: Int, var right: Int) -> Result(Int, String):
    left = left + 100
    right = right + 10
    Err("stop")

fn option_receiver_try(var state: Option(Int), var count: Int) -> Option(Int):
    count = count + 1
    let value = state?
    state = Some(value + 1)
    Some(value)

fn update_or_none(var n: Int, succeeds: Bool) -> Option(Int):
    n = n + 10
    if succeeds:
        Some(n)
    else:
        None

fn caller_try(var n: Int) -> Option(Int):
    let value = update_or_none(n, false)?
    Some(value)

fn main(console: Console):
    var a = 1
    var b = 10
    let by_try = via_try(a, b, Err("stop"))
    console.print("${a} ${b} ${by_try}")

    var c = 1
    var d = 10
    let by_return = via_explicit_return(c, d, Err("stop"))
    console.print("${c} ${d} ${by_return}")

    var e = 1
    var f = 10
    let by_tail = via_tail_err(e, f)
    console.print("${e} ${f} ${by_tail}")

    var state: Option(Int) = None
    var count = 0
    let option_result = option_receiver_try(state, count)
    console.print("${state} ${count} ${option_result}")

    var propagated = 1
    let propagated_result = caller_try(propagated)
    console.print("${propagated} ${propagated_result}")

    var fallback_state = 2
    let fallback = update_or_none(fallback_state, false) ?? fallback_state + 100
    console.print("${fallback_state} ${fallback}")
"#;
        let want = [
            "101 20 Err(stop)",
            "101 20 Err(stop)",
            "101 20 Err(stop)",
            "None 1 None",
            "11 None",
            "12 112",
        ];
        assert_eq!(link_run(src), want, "interpreter structured returns");
        assert_eq!(wasm_run(src), want, "compiled structured returns");
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

    /// Dict `update` (single-lookup upsert) must agree on both backends, including
    /// nested updates and a big-`Int` value. WASM lowers it to a `$dict_update`
    /// helper that reads the current value (or default), applies the closure via
    /// `call_indirect`, and reinserts — equivalent to the interpreter's
    /// `dict.insert(d, k, f(dict.get_or(d, k, default)))`. (Regression for the
    /// interpreter-only dict-upsert gap.)
    #[test]
    fn dict_update_upsert_agrees_on_both_backends() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n    dict.update(d, \"a\", 0, fn(x: Int): x + 10)\n    dict.update(d, \"c\", 100, fn(x: Int): x + 1)\n    console.print(\"${dict.get_or(d, \"a\", -1)}\")\n    console.print(\"${dict.get_or(d, \"b\", -1)}\")\n    console.print(\"${dict.get_or(d, \"c\", -1)}\")\n    console.print(\"${dict.length(d)}\")\n    var counts = dict.new()\n    dict.update(counts, \"hit\", 0, fn(n: Int): n + 1)\n    dict.update(counts, \"hit\", 0, fn(n: Int): n + 1)\n    console.print(\"${dict.get_or(counts, \"hit\", -1)}\")\n";
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

    /// `std/dict` adds the compositional layer over the builtin Dict: a `get`
    /// returning `Option`, `from_pairs`, and the `map_values`/`filter`/`merge`
    /// transforms — verified against the builtin `size`/`get_or`.
    #[test]
    fn dict_module_higher_level_operations() {
        let src = r#"import dict

fn main(console: Console):
    let d = dict.from_pairs([("a", 1), ("b", 2), ("c", 3)])
    console.print("${dict.length(d)}")
    console.print(oi(d.get("b")))
    console.print(oi(d.get("z")))
    let m = d.merge(dict.from_pairs([("b", 20), ("d", 4)]))
    console.print("${dict.get_or(m, "b", 0)}" + "," + "${dict.get_or(m, "d", 0)}")
    let tens = d.map_values(fn(v: Int): v * 10)
    console.print(oi(tens.get("c")))
    let evens = d.filter(fn(k: String, v: Int): v % 2 == 0)
    console.print("${dict.length(evens)}")
    let fresh: Dict(String, Int) = dict.new()
    console.print(bs(fresh.is_empty()))

fn oi(o: Option(Int)) -> String:
    match o:
        Some(n) -> "${n}"
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

fn main(console: Console):
    match json.decode("{\"name\":\"acme\",\"n\":7,\"caps\":[\"Net\",\"Console\"],\"arr\":[\"a\",\"b\"]}"):
        Ok(d) ->
            console.print(opt(json.get_string(d, "name")))
            console.print(oi(json.get_int(d, "n")))
            console.print(list.join(json.get_strings(d, "caps"), ","))
            console.print("[" + list.join(json.get_strings(d, "absent"), ",") + "]")
        Err(e) -> console.print("err")

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"

fn oi(o: Option(Int)) -> String:
    match o:
        Some(n) -> "${n}"
        None -> "?"
"#;
        assert_eq!(link_run(src), vec!["acme", "7", "Net,Console", "[]"]);
    }

    /// `std/fs` parent_dir + (with a real Dir) the recursive collect — exercised
    /// here for the pure part to confirm the module's functions resolve on import.
    #[test]
    fn fs_module_parent_dir_resolves() {
        let src = "import fs\nimport option\nfn main(console: Console):\n    console.print(fs.parent_dir(\"a/b/c\") ?? \"<none>\")\n    console.print(fs.parent_dir(\"top\") ?? \"<none>\")\n";
        assert_eq!(link_run(src), vec!["a/b", "<none>"]);
    }

    /// `std/rights` matches capability strings rights-precisely (the logic the pm
    /// check/gate and coven's publish enforcement share): a bare kind covers any
    /// rights of that kind, a bracketed one only a subset — so `Net[Connect]` does
    /// NOT cover full `Net`.
    #[test]
    fn rights_module_covers_capabilities_rights_precisely() {
        let src = r#"import rights

fn main(console: Console):
    console.print(yes(rights.covers("Net", "Net[Listen]")))
    console.print(yes(rights.covers("Net[Connect]", "Net")))
    console.print(yes(rights.covers("Net[Connect, Tcp]", "Net[Connect]")))
    console.print(yes(rights.covers("Dir", "Console")))
    console.print(yes(rights.any_covers(["Console", "Dir[Read]"], "Dir[Read]")))
    console.print(list.join(rights.uncovered(["Net[Connect]"], ["Net", "Console"]), "|"))

fn yes(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            // `Net[Connect, Tcp]` does not cover `Net[Connect]`: the demanded
            // type admits every Connect transport, while the declared type is
            // Tcp-only.
            vec!["y", "n", "n", "n", "y", "Net|Console"]
        );
    }

    /// The `Clock` capability yields wall-clock time (ms since epoch) via `now`.
    /// Reading the clock is ambient nondeterminism, so it's capability-gated and
    /// surfaces in the footprint — not a pure builtin.
    #[test]
    fn clock_capability_yields_wall_clock_time() {
        let out = interp(
            "fn main(console: Console, clock: Clock):\n    console.print(\"${clock.now()}\")\n",
        );
        let ms: i64 = out[0].parse().expect("now should print an integer");
        assert!(ms > 1_600_000_000_000, "now should be ms since the Unix epoch (got {ms})");
        // `now` needs a Clock — calling it with another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let t = now(c)\n").is_err());
        // The Clock requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, clock: Clock):\n    let t = clock.now()\n")
                .expect("parse"),
        );
        assert!(fp.total.contains_key("Clock"), "Clock should appear in the footprint");
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

    /// (RFC-0038) A bare grantable capability granted to `main` mints an identical
    /// sealed record on BOTH backends: the interpreter builds a `Value::Ctor` from
    /// the grant fields; the compiled backend stages each field host-side and
    /// wraps them in a record via `mk{N}`. The two must agree bit-for-bit.
    #[test]
    fn grantable_user_cap_mints_identically_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "grantable capability UiRoot:\n    policy: String\n    app_id: String\n\nfn descr(u: UiRoot) -> String:\n    match u:\n        UiRoot(p, a) -> p + \"@\" + a\n\nfn main(console: Console, ui: UiRoot):\n    console.print(descr(ui))\n";
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
            .expect_lowered("the binary path lowers this program");
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
            "fn main(console: Console, env: Env):\n    match env.get_env(\"WITCHY_NOPE_UNSET_VAR\"):\n        Some(v) -> console.print(v)\n        None -> console.print(\"unset\")\n",
        );
        assert_eq!(out, vec!["unset"]);
        // `get_env` needs an Env capability — another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let x = get_env(c, \"X\")\n").is_err());
        // The Env requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, env: Env):\n    let x = env.get_env(\"X\")\n")
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
    var rev = xs
    list.reverse(rev)
    console.print((("${list.at(rev, 0)}" + ",") + "${list.at(rev, 5)}"))
    console.print((("${list.length(list.take(xs, 3))}" + ":") + "${list.at(list.take(xs, 3), 2)}"))
    console.print("${list.at(list.drop(xs, 4), 0)}")
    var sorted = xs
    list.sort_by(sorted, fn(a: Int, b: Int): (a < b))
    console.print((("${list.at(sorted, 0)}" + "..") + "${list.at(sorted, 5)}"))
    let pairs = list.zip([1, 2, 3], [10, 20, 30])
    let (pa, pb) = list.at(pairs, 1)
    console.print("${(pa + pb)}")
    let en = list.enumerate([100, 200])
    let (ei, ev) = list.at(en, 1)
    console.print("${((ei * 1000) + ev)}")
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    let evens = list.filter(xs, fn(n: Int): ((n % 2) == 0))
    console.print("${list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))}")
    console.print("${list.length(evens)}")
    console.print("${list.index_of(xs, 8)}")
    console.print("${list.contains(xs, 9)}")
    console.print("${list.any(xs, fn(n: Int): (n > 8))}")
    console.print("${list.all(xs, fn(n: Int): (n > 0))}")
    console.print("${list.sum(xs)}")
    console.print("${list.is_empty(xs)}")
    console.print("${list.is_empty(list.filter(xs, fn(n: Int): (n > 100)))}")
    console.print("${list.count_where(xs, fn(n: Int): ((n % 2) == 0))}")
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
    console.print("${unwrap(Wrap(42), 0)}")
    console.print(unwrap(Wrap("hello"), "none"))
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
    fn std_list_find_index_backends_agree() {
        // RFC-0049: `find_index` is deleted; `position` is the by-PREDICATE
        // Option-index form (it took over the role find_index vacated).
        // `?? -1` (RFC-0048) recovers the old sentinel for a compact assertion.
        let client = r#"
import list

fn main(console: Console):
    let xs = [3, 8, 1, 9, 4]
    console.print("${list.position(xs, fn(n: Int): (n > 5)) ?? -1}")
    console.print("${list.position(xs, fn(n: Int): (n > 100)) ?? -1}")
    console.print("${list.position(xs, fn(n: Int): (n == 1)) ?? -1}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "position diverged");
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
    console.print("${list.length(sums)}")
    console.print("${list.sum(sums)}")
    let spaced = list.intersperse([5, 6, 7], 0)
    console.print("${list.length(spaced)}")
    console.print("${list.sum(spaced)}")
    console.print("${list.length(list.intersperse([9], 0))}")
    console.print("${list.length(list.intersperse([], 0))}")
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
    console.print("${list.sum(list.take_while(xs, fn(n: Int): (n < 5)))}")
    console.print("${list.sum(list.drop_while(xs, fn(n: Int): (n < 5)))}")
    let threes = list.repeat(7, 3)
    console.print("${list.sum(threes)}")
    console.print("${list.length(threes)}")
    console.print("${list.length(list.repeat(9, 0))}")
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
    console.print("${list.length(flat)}")
    console.print("${list.sum(flat)}")
    let fm = list.flat_map([1, 2, 3], fn(n: Int): [n, (n * 10)])
    console.print("${list.length(fm)}")
    console.print("${list.sum(fm)}")
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
    fn std_option_combinators_backends_agree() {
        // is_none / and_then / filter behave identically in both backends.
        let client = r#"
import option

fn main(console: Console):
    let s = Some(5)
    console.print("${option.is_none(s)}")
    console.print("${option.is_none(option.filter(s, fn(n: Int): (n > 10)))}")
    let chained = option.and_then(s, fn(n: Int): Some((n * 2)))
    console.print("${option.unwrap_or(chained, 0)}")
    let kept = option.filter(s, fn(n: Int): (n > 0))
    console.print("${option.unwrap_or(kept, 0)}")
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
    console.print("${option.unwrap_or(option.flatten(nested(7)), (0 - 1))}")
    console.print("${option.unwrap_or(option.flatten(nested(0)), (0 - 1))}")
    match option.zip(Some(3), Some(4)):
        Some(pair) ->
            let (x, y) = pair
            console.print("${(x + y)}")
        None -> console.print("none")
    console.print("${option.is_none(option.zip(Some(1), None))}")
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
    console.print("${option.unwrap_or(option.or(Some(5), Some(9)), 0)}")
    console.print("${option.unwrap_or(option.or(None, Some(9)), 0)}")
    console.print("${option.unwrap_or(option.or_else(None, fn(): Some(7)), 0)}")
    console.print("${option.unwrap_or(option.or_else(Some(3), fn(): Some(7)), 0)}")
    console.print("${option.map_or(Some(10), 0, fn(x: Int): (x * 2))}")
    console.print("${option.map_or(None, 99, fn(x: Int): (x * 2))}")
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option or/map_or diverged");
        assert_eq!(compiled, vec!["5", "9", "7", "3", "20", "99"]);
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
    fn std_set_operations_backends_agree() {
        // Set ops dispatch through Eq (cross-module: set -> eq.member, both
        // bounded generics), so they are content-correct on both backends for
        // runtime-built strings and a user Eq type (Id), and dedupe along the way.
        let client = r#"
import set

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
    while (i < s.char_count()):
        acc = (acc + s.substring(i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let a = set.from_list([build("x"), build("y"), build("x")])
    let b = set.from_list([build("y"), build("z")])
    let u = set.union(a, b)
    let i = set.intersection(a, b)
    let d = set.difference(a, b)
    console.print(list.join(set.to_list(u), ","))
    console.print(list.join(set.to_list(i), ","))
    console.print(list.join(set.to_list(d), ","))
    console.print("${set.is_subset(set.from_list([build("y")]), a)}")
    console.print("${set.is_subset(set.from_list([build("z")]), a)}")
    let ids = set.union(set.from_list([Id(1), Id(2), Id(1)]), set.from_list([Id(2), Id(3)]))
    console.print("${set.length(ids)}")
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
        let client = "import set\nimport iter\nimport show\n\nfn main(console: Console):\n    var s = set.from_list([3, 1, 2, 3, 1])\n    console.print(\"${set.length(s)}\")\n    console.print(\"${set.contains(s, 2)}\")\n    var total = 0\n    for x in s:\n        total = (total + x)\n    console.print(\"${total}\")\n    set.remove(s, 2)\n    console.print(show(s))\n    let cs: Set(Int) = iter.collect(iter.range(1, 4))\n    console.print(show(cs))\n";
        let sources = [
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("iter", crate::bundled_module("iter").unwrap()),
            ("set", crate::bundled_module("set").unwrap()),
            ("show", crate::bundled_module("show").unwrap()),
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
    fn multi_statement_match_arm_body_indented() {
        // A match arm with a multi-statement body, brace-free: `Pat ->` opens an
        // indented block. Both backends agree.
        let client = "type Cmd:\n    Inc\n    Dec\n\nfn apply(n: Int, c: Cmd) -> Int:\n    match c:\n        Inc ->\n            let m = n + 1\n            m\n        Dec ->\n            n - 1\n\nfn main(console: Console):\n    console.print(\"${apply(10, Inc)}\")\n    console.print(\"${apply(10, Dec)}\")\n";
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
    console.print("${((q).x + (q).y)}")
    let r = Point(x: 5, y: 6, ..p)
    console.print("${((r).x + (r).y)}")
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
    console.print("${list.fold(signs, 0, fn(a: Int, b: Int): (a + b))}")
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
    console.print("${list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))}")
    console.print("${list.length(list.filter(xs, fn(n: Int): ((n % 2) == 0)))}")
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

    #[test]
    fn json_encode_pretty_backends_agree() {
        let client = r#"
import json
from json import Json
fn main(console: Console):
    let doc = JsonObject([("name", JsonString("witchy")), ("tags", JsonArray([JsonInt(1), JsonInt(2)])), ("empty", JsonArray([]))])
    console.print(json.encode_pretty(doc))"#;
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
        let src = "fn main(console: Console):\n    let cs = \"café\".chars()\n    console.print(\"${list.length(cs)}\")\n    console.print(list.at(cs, 0))\n    console.print(list.at(cs, 3))\n";
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
from json import Json
fn main(console: Console):
    match json.decode("{\"a\": 1, \"b\": 2}"):
        Ok(doc) ->
            match json.as_object(doc):
                Some(pairs) ->
                    for p in pairs:
                        let (k, _v) = p
                        console.print(k)
                None -> console.print("not object")
        Err(_e) -> console.print("err")
    console.print(if option.is_none(json.as_object(JsonInt(5))): "none" else: "some")"#;
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
fn show_ints(xs: List(Int)) -> String:
    list.join(list.map(xs, fn(n: Int): "${n}"), ",")
fn main(console: Console):
    console.print(show_ints(list.range_between(2, 6)))
    console.print(show_ints(list.range_between(5, 5)))
    console.print(show_ints(list.range_step(0, 10, 3)))
    console.print(show_ints(list.range_step(5, 0, -2)))
    console.print(show_ints(list.range_step(0, 5, 0)))
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
fn show_ints(xs: List(Int)) -> String:
    list.join(list.map(xs, fn(n: Int): "${n}"), ",")
fn main(console: Console):
    let sd1 = set.symmetric_difference(set.from_list([1, 2, 3]), set.from_list([2, 3, 4]))
    let sd2 = set.symmetric_difference(set.from_list([1, 1, 2]), set.from_list([2, 2, 3]))
    console.print(show_ints(set.to_list(sd1)))
    console.print(show_ints(set.to_list(sd2)))
    let d1a = set.from_list([1, 2])
    console.print(if set.is_disjoint(d1a, set.from_list([3, 4])): "yes" else: "no")
    console.print(if set.is_disjoint(d1a, set.from_list([2, 3])): "yes" else: "no")
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
    fn func_on_backends_agree() {
        // on(op, f) lifts op to act on projections — here sorting (name, age)
        // pairs by age via func.on_key(lt, snd).
        let client = r#"
import func
import list
fn fst(p: (String, Int)) -> String:
    let (a, _b) = p
    a
fn snd(p: (String, Int)) -> Int:
    let (_a, b) = p
    b
fn lt(a: Int, b: Int) -> Bool:
    a < b
fn main(console: Console):
    var people = [("alice", 30), ("bob", 25), ("carol", 35)]
    list.sort_by(people, func.on_key(lt, snd))
    console.print(list.join(list.map(people, fst), ","))
    let by_age = func.on_key(lt, snd)
    console.print(if by_age(("x", 1), ("y", 2)): "lt" else: "ge")
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
from json import Json
fn main(console: Console):
    let a = JsonObject([("name", JsonString("a")), ("x", JsonInt(1))])
    let b = JsonObject([("x", JsonInt(2)), ("y", JsonInt(3))])
    console.print(json.encode(json.merge(a, b)))
    console.print(json.encode(json.merge(a, JsonInt(9))))
    console.print(if json.contains_key(a, "x"): "T" else: "F")
    console.print(if json.contains_key(a, "z"): "T" else: "F")
    console.print(if json.contains_key(JsonInt(5), "x"): "T" else: "F")"#;
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
                Some(n) -> "int:" + "${n}"
                None -> "ok"
        Err(_e) -> "err"
fn main(console: Console):
    console.print(classify("[1, 2]"))
    console.print(classify("42  "))
    console.print(classify("1 2"))
    console.print(classify("true xyz"))
    console.print(classify("{}extra"))
    console.print(classify("  7"))
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
        Err(e) -> "err:" + json.decode_error_message(e)
fn main(console: Console):
    console.print(round_trip("10"))
    console.print(round_trip("-3"))
    console.print(round_trip("3.25"))
    console.print(round_trip("-0.5"))
    console.print(round_trip("1.5e3"))
    console.print(round_trip("{\"pi\": 3.25}"))
"#;
        let want: Vec<String> = ["10", "-3", "3.25", "-0.5", "1500.0", "{\"pi\":3.25}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(client), want, "interpreter");
        assert_eq!(wasm_run(client), want, "wasm");
    }

    #[test]
    fn json_long_fraction_does_not_wrap_backends_agree() {
        // BUG-241: the fractional tail used to fold digits into an i64
        // (`frac * 10 + digit`), so a long input-controlled fraction wrapped to a
        // wrong value (`0.<20 nines>` parsed as ~0.0776). It now folds over the
        // digit span into a Float like the integer part, so a long fraction rounds
        // to the nearest double instead of wrapping. Identical on both backends.
        let client = r#"
import json
fn rt(s: String) -> String:
    match json.decode(s):
        Ok(j) -> json.encode(j)
        Err(e) -> "err:" + json.decode_error_message(e)
fn main(console: Console):
    console.print(rt("0.99999999999999999999"))
    console.print(rt("1.99999999999999999999999999999999999999"))
    console.print(rt("0.1234567890123456789"))
    console.print(rt("3.14159"))
"#;
        let want: Vec<String> =
            ["1.0000000000000002", "2.0", "0.1234567890123457", "3.14159"]
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
    fn list_transpose_backends_agree() {
        // transpose swaps rows and columns; a ragged input is truncated to the
        // shortest row, and an empty input gives an empty result.
        let client = r#"
import list
fn show_row(r: List(Int)) -> String:
    list.join(list.map(r, fn(n: Int): "${n}"), ",")
fn show_grid(g: List(List(Int))) -> String:
    list.join(list.map(g, show_row), ";")
fn main(console: Console):
    console.print(show_grid(list.transpose([[1, 2, 3], [4, 5, 6]])))
    console.print(show_grid(list.transpose([[1, 2], [3, 4, 5]])))
    console.print(show_grid(list.transpose([])))
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
    fn result_partition_backends_agree() {
        // partition splits a list of Results into the Ok values and the Err
        // values, each in order.
        let client = r#"
import result
import list
fn main(console: Console):
    let (oks, errs) = result.partition([Ok(1), Err("a"), Ok(2), Err("b"), Ok(3)])
    console.print(list.join(list.map(oks, fn(n: Int): "${n}"), ","))
    console.print(list.join(errs, ","))
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
    fn duration_combinators_backends_agree() {
        // max/min/is_zero/abs over the Duration type (it has no Ord impl, so the
        // generic ord helpers don't apply).
        let client = r#"
import duration
fn main(console: Console):
    console.print(duration.human(duration.max(30s, 1m)))
    console.print(duration.human(duration.min(30s, 1m)))
    console.print("${duration.is_zero(0ms)}")
    console.print("${duration.is_zero(1s)}")
    console.print(duration.human(duration.abs(0s - 5s)))
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

    #[test]
    fn sandbox_runs_compiled_and_captures_output() {
        // `witchy sandbox` compiles to WASM and runs in the capability sandbox,
        // returning the program's output.
        let path = std::env::temp_dir().join(format!("witchy_sandbox_smoke_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "fn main(console: Console):\n    console.print(\"${6 * 7}\")\n",
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
            "import option\n\nfn main(console: Console, env: Env, dir: Dir[Read], args: List(String)) -> Int:\n    let path = list.at(args, 0)\n    let label = match env.get_env(\"PATH\"):\n        Some(v) -> v\n        None -> \"unlabeled\"\n    for line in dir.read(path).lines():\n        if line.contains(\"needle\"):\n            console.print(label + \": \" + line)\n    0\n",
        )
        .unwrap();
        let host_path = std::env::var("PATH").expect("the test process has PATH");
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
        assert_eq!(out, vec![format!("{host_path}: needle in here")]);
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
    fn examples_agree_under_inplace_and_forced_copy() {
        // Metamorphic, NO-ORACLE codegen check: the in-place update machinery and
        // the forced-copy fallback are two lowerings of the same program and must
        // produce identical output. This catches an in-place aliasing bug on the
        // compiled backend WITHOUT consulting the interpreter — the kind of
        // self-consistency guard that lets the differential oracle be retired.
        // Restricted to console-only, `main`-bearing programs so output is
        // self-contained and deterministic.
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
                    let compile_with = |force_copy: bool| {
                        codegen::set_force_copy_for_tests(Some(force_copy));
                        let bytes = codegen::compile_module_binary(&linked);
                        codegen::set_force_copy_for_tests(None);
                        bytes
                    };
                    if let (
                        codegen::LoweringOutcome::Lowered(inplace),
                        codegen::LoweringOutcome::Lowered(copy),
                    ) = (compile_with(false), compile_with(true)) {
                        let a = crate::run_wasm_bytes(&inplace);
                        let b = crate::run_wasm_bytes(&copy);
                        if a != b {
                            return Some(format!("{p}: in-place {a:?} vs forced-copy {b:?}"));
                        }
                    }
                    None
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
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
    var xs = [Item(2, "a"), Item(1, "b"), Item(2, "c"), Item(1, "d"), Item(2, "e")]
    list.sort_by(xs, fn(p: Item, q: Item): key(p) < key(q))
    for it in xs:
        console.print("${key(it)}" + tag(it))
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
    console.print("${result.unwrap_or(result.or(checked(5), Ok(9)), 0)}")
    console.print("${result.unwrap_or(result.or(checked((0 - 1)), Ok(9)), 0)}")
    console.print("${result.unwrap_or(result.or_else(checked((0 - 1)), fn(e: String): Ok(e.length())), 0)}")
    console.print("${result.map_or(checked(5), 0, fn(x: Int): (x * 2))}")
    console.print("${result.map_or(checked((0 - 1)), 99, fn(x: Int): (x * 2))}")
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
    console.print("${result.is_err(checked(5))}")
    console.print("${result.is_err(checked((0 - 1)))}")
    let chained = result.and_then(checked(5), fn(n: Int): Ok((n * 10)))
    console.print("${result.unwrap_or(chained, 0)}")
    let mapped = result.map_err(checked((0 - 1)), fn(s: String): s.length())
    console.print("${result.is_err(mapped)}")
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
        let wasm = crate::wir_encode::encode(&module, &[]);
        assert!(validates_wasm_gc(&wasm), "encoded module must validate");

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
        let src = "fn main(console: Console):\n    for x in [10, 20, 30]:\n        console.print(\"${x}\")\n    let f = fn(n: Int): n + 1\n    console.print(\"${f(5)}\")\n";
        let want = vec!["10".to_string(), "20".to_string(), "30".to_string(), "6".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        // Takes the binary path (closures lower) AND emits all loop iterations —
        // a mis-scoped capture would drop the loop to a single pass.
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("loop + closure must lower on the binary path");
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
        let src = "fn build(n: Int) -> List(Int):\n    var xs: List(Int) = []\n    for i in 0..n:\n        list.push(xs, i)\n    xs\n\nfn main(console: Console):\n    let ys = build(500)\n    console.print(\"${list.at(ys, 499)}\")\n";
        let want = vec!["499".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the accumulator program takes the WIR binary path");
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
        let src = "fn build(n: Int) -> List(Int):\n    var xs: List(Int) = []\n    for i in 0..n:\n        list.push(xs, i)\n    xs\n\nfn main(console: Console):\n    let ys = build(3)\n    console.print(\"${list.at(ys, 0)}\")\n    console.print(\"${list.at(ys, 1)}\")\n    console.print(\"${list.at(ys, 2)}\")\n";
        let want = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        assert_eq!(run_on_wasm(src), want, "legacy WAT path");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the accumulator program takes the WIR binary path");
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
        let src = "fn build(n: Int) -> Dict(String, Int):\n    var d = dict.new()\n    for i in 0..n:\n        dict.insert(d, \"k\" + \"${i}\", i)\n    d\n\nfn main(console: Console):\n    let m = build(500)\n    console.print(\"${dict.get_or(m, \"k499\", 0 - 1)}\")\n    console.print(\"${dict.length(m)}\")\n";
        let want = vec!["499".to_string(), "500".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the dict accumulator program takes the WIR binary path");
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
        let src = "fn build(n: Int) -> String:\n    var s = \"\"\n    var i = 0\n    while i < n:\n        s = s + \"x\"\n        i = i + 1\n    s\n\nfn main(console: Console):\n    let r = build(500)\n    console.print(\"${r.length()}\")\n";
        let want = vec!["500".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the string builder takes the WIR binary path");
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
        let src = "fn build(n: Int) -> Dict(String, Int):\n    var d = dict.new()\n    var i = 0\n    while i < n:\n        dict.update(d, \"k\" + \"${i % 10}\", 0, fn(c: Int): c + 1)\n        i = i + 1\n    d\n\nfn main(console: Console):\n    let d = build(500)\n    console.print(\"${dict.get_or(d, \"k0\", 0 - 1)}\")\n    console.print(\"${dict.length(d)}\")\n";
        let want = vec!["50".to_string(), "10".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the dict.update accumulator takes the WIR binary path");
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
        let src = "type Point:\n    x: Int\n    y: Int\n\ntype Line:\n    from: Point\n    to: Point\n\nfn main(console: Console):\n    let l = Line(Point(1, 2), Point(3, 4))\n    let p2 = Point(x: 100, ..(l).from)\n    console.print(\"${(p2).x}\")\n    console.print(\"${(p2).y}\")\n";
        let want = vec!["100".to_string(), "2".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower a RecordUpdate with an expression base");
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

    /// (RFC-0051 I2) The single-allocator invariant on LOWERED PROGRAMS: assemble
    /// full WIR modules (helpers + user code) for representative programs that
    /// exercise the heap-touching lowering shapes — accumulation (`list.push`/
    /// dict insert self-assigns → the `*_cap` in-place paths), string building,
    /// a scalar `region:` reclaim, a pointer `region:` copy-out, and a loop-arena
    /// reset — and walk every function body: any `SetGlobal { global: "heap" }`
    /// outside `$bump_alloc` and the named watermark REWINDS
    /// (`heap = __witchy_wm_*` / `heap = wm + copied_len`, which move `$heap`
    /// down to or below an already-ensured frontier) fails with the offending
    /// function's name. Because all WIR construction funnels through
    /// `assemble_wir_module`, the walk sees everything — including future
    /// helpers — so the `ensure()` convention cannot be silently forgotten
    /// (the `int_to_string` OOB class). Registry-wide helper coverage lives in
    /// witchy-wir's `single_allocator_invariant_holds_across_helper_registry`.
    #[test]
    fn single_allocator_invariant_holds_on_lowered_programs() {
        let progs = [
            // accumulators: list push / dict insert / string concat self-assigns
            "fn main(console: Console):\n    var xs = []\n    var d = dict.new()\n    var s = \"\"\n    for i in 0..50:\n        list.push(xs, i)\n        dict.insert(d, \"k${i}\", i)\n        s = s + \"x\"\n    list.set_at(xs, 0, 9)\n    console.print(\"${list.length(xs)} ${dict.length(d)} ${s.length()}\")\n",
            // scalar region reclaim (the watermark rewind exemption)
            "\nfn main(console: Console):\n    let n = region -> Int:\n        var parts = []\n        for i in 0..20:\n            list.push(parts, \"p${i}\")\n        list.length(parts)\n    console.print(\"${n}\")\n",
            // pointer region copy-out (the `heap = wm + copied_len` advance-rewind)
            "\nfn main(console: Console):\n    let summary: String = region:\n        var parts = []\n        for i in 0..20:\n            list.push(parts, \"p${i}\")\n        list.join(parts, \",\")\n    console.print(\"${summary.length()}\")\n",
        ];
        for src in progs {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let m = codegen::assemble_wir_module(&linked)
                .expect_lowered(&format!("expected the WIR binary path to handle:\n{src}"));
            let violations = witchy_wir::wir::heap_write_violations(&m);
            assert!(
                violations.is_empty(),
                "RFC-0051 I2 violated — these functions write `$heap` outside \
                 `$bump_alloc`/the watermark rewinds: {violations:?}\nprogram:\n{src}"
            );
        }
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

    /// Dict render on the binary path — the generated `$ts_dict_*` helper walks the
    /// `[count][key, value]…` entries (16-byte stride) emitting `{k: v, ...}` in
    /// insertion order. Kept OUT of the 3-way corpus because `dict.from_pairs` is a
    /// std fn the corpus's `check_str`/`run_on_wasm` leg can't resolve (like the
    /// encoding case); compared against the linked interpreter oracle directly.
    #[test]
    fn wir_dict_render_binary_path() {
        let src = "fn main(console: Console):\n    let d = dict.from_pairs([(\"a\", 1), (\"b\", 2), (\"c\", 3)])\n    console.print(\"${d}\")\n";
        let want = vec!["{a: 1, b: 2, c: 3}".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should render a dict");
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
        let src = "fn main(console: Console):\n    var d = dict.new()\n    dict.update(d, \"a\", 0, fn(c: Int): c + 1)\n    dict.update(d, \"a\", 0, fn(c: Int): c + 1)\n    dict.update(d, \"a\", 0, fn(c: Int): c + 1)\n    dict.update(d, \"b\", 0, fn(c: Int): c + 1)\n    console.print(\"${d}\")\n    console.print(\"${dict.get_or(d, \"a\", -1)}\")\n";
        let want = vec!["{a: 3, b: 1}".to_string(), "3".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower dict.update");
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

    /// The own-ABI (`own` buffer param threaded through `move`) on the binary
    /// path: `grow` takes `own xs`, appends in place, and returns it. The callee
    /// gains a trailing `$xs__cap` i32 param and a second i32 result (the
    /// ownership token); the self-call `xs = grow(move xs, i)` lowers to a
    /// CallStoreMulti capturing (value → xs, cap → xs__cap). (2-way: `list.push`
    /// isn't resolvable by the WAT-leg `check_str`, so compare binary vs oracle.)
    #[test]
    fn wir_own_abi_move_pipeline_binary_path() {
        let src = "fn grow(own xs: List(Int), n: Int) -> List(Int):\n    list.push(xs, n)\n    xs\n\nfn main(console: Console):\n    var xs = []\n    for i in 1..6:\n        xs = grow(move xs, i)\n    console.print(\"${xs}\")\n";
        let want = vec!["[1, 2, 3, 4, 5]".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower the own-ABI move pipeline");
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
        let src = "fn main(console: Console):\n    let build = fn(n: Int):\n        var acc = [0]\n        var t = 0\n        while t < n:\n            list.push(acc, t)\n            t = t + 1\n        list.length(acc)\n    console.print(\"${build(5)}\")\n";
        let want = vec!["6".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower a lambda-local accumulator");
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
        let src = "fn main(console: Console, dir: Dir[Read]):\n    console.print(\"${list.length(dir.list())}\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle dir list via the host imports");
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
    var xs = [3, 1, 4, 1, 5, 9, 2, 6]
    list.sort_by(xs, fn(a: Int, b: Int): (a < b))
    ((list.at(xs, 0) * 100) + list.at(xs, 7))
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
    fn to_string_respects_lambda_param_shadowing_on_wasm() {
        // The outer `x` is an Int; the lambda's `x` is a String param. `to_string`
        // inside the lambda must pass the String through, not run int_to_string on
        // the pointer — i.e. value-type tracking is scoped per lambda.
        let src = r#"
fn apply(f: fn(String) -> String, s: String) -> String:
    f(s)

fn main(console: Console):
    let x = 5
    console.print("${x}")
    console.print(apply(fn(x: String): "${x}", "hey"))
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
    fn dict_int_keys_on_wasm() {
        // Int-keyed Dict: keys compared with i32 equality (mode 0).
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, 1, 100)
    dict.insert(d, 2, 200)
    console.print("${dict.get_or(d, 1, 0)}")
    console.print("${dict.get_or(d, 2, 0)}")
    console.print("${dict.get_or(d, 3, (0 - 1))}")
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
        // A key with no `Eq` implementation errors clearly
        // rather than picking a wrong comparison.
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, console, 5)
    console.print("${dict.length(d)}")
"#;
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("the generic Dict surface remains type-checkable");
        let err = codegen::compile_module_binary(&linked).expect_rejected("should reject");
        assert!(
            err.to_string().contains("Dict key type"),
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
    console.print("${(Item(7, 6)).price}")
    console.print("${(lookup(true)).qty}")
    let items = [Item(1, 2), Item(3, 4)]
    console.print("${(list.at(items, 1)).qty}")
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
    console.print("${pick(true)}")
    console.print("${pick(false)}")
    console.print("${from_tag(0)}")
    console.print("${from_tag(9)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "10", "2", "5"]);
    }

    #[test]
    fn coalesce_unwraps_option_backends_agree() {
        // RFC-0048: `Option(T) ?? T` unwraps to `T` (None -> the default, evaluated
        // lazily; Some(x) -> x, present even when empty — `Some("") ?? "x"` is `""`,
        // not `"x"`, since there is no truthiness).
        let src = r#"
fn pick(b: Bool) -> Option(Int):
    if b: Some(36) else: None

fn empty() -> Option(String):
    Some("")

fn main(console: Console):
    console.print("${pick(true) ?? 0}")
    console.print("${pick(false) ?? 0}")
    console.print("${empty() ?? "x"}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["36", "0", ""]);
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
    console.print("${first_value([Item(3, 10), Item(5, 2)])}")
    let items = [Item(2, 4), Item(7, 1)]
    let second = list.at(items, 1)
    console.print("${((second).price + (second).qty)}")
    var total = 0
    for it in items:
        total = (total + (it).price)
    console.print("${total}")
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
    dict.insert(d, "apple", Item(3, 10))
    dict.insert(d, "bread", Item(2, 5))
    let it = dict.get_or(d, "apple", Item(0, 0))
    console.print("${((it).price * (it).qty)}")
    let missing = dict.get_or(d, "milk", Item(0, 0))
    console.print("${(missing).price}")
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
    dict.insert(d, "a", 1)
    dict.insert(d, "b", 2)
    dict.insert(d, "c", 3)
    var d2 = d
    dict.remove(d2, "b")
    console.print("${dict.length(d2)}")
    console.print("${if dict.contains_key(d2, "b"): 1 else: 0}")
    console.print("${dict.get_or(d2, "a", 0)}")
    console.print("${dict.get_or(d2, "c", 0)}")
    var d3 = d
    dict.remove(d3, "missing")
    console.print("${dict.length(d3)}")
    console.print("${dict.length(d)}")
    var nums = dict.new()
    dict.insert(nums, 10, 100)
    dict.insert(nums, 20, 200)
    var nums2 = nums
    dict.remove(nums2, 10)
    console.print("${dict.length(nums2)}")
    console.print("${dict.get_or(nums2, 20, 0)}")
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
    dict.insert(b, "x", 1)
    dict.remove(b, "x")
    dict.insert(b, "x", 5)
    console.print("${dict.get_or(b, "x", -1)}")
    console.print("${list.length(dict.keys(b))}")
    console.print("${dict.get_or(b, "x", -1)}")
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
    dict.insert(d, "a", 10)
    dict.insert(d, "b", 20)
    dict.insert(d, "c", 30)
    var ksum = 0
    for k in dict.keys(d):
        ksum = (ksum + k.length())
    console.print("${ksum}")
    var vsum = 0
    for v in dict.values(d):
        vsum = (vsum + v)
    console.print("${vsum}")
    var psum = 0
    for entry in dict.pairs(d):
        let (k, v) = entry
        psum = ((psum + k.length()) + v)
    console.print("${psum}")
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
    fn std_list_partition_unzip_backends_agree() {
        // partition splits by a predicate in one pass; unzip is the inverse of
        // zip. Both return tuples of lists, so this also exercises tuple-valued
        // returns from generic std functions across backends.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 4, 5, 6]
    let (evens, odds) = list.partition(xs, fn(n: Int): ((n % 2) == 0))
    console.print("${list.sum(evens)}")
    console.print("${list.sum(odds)}")
    let pairs = list.zip([10, 20, 30], [1, 2, 3])
    let (a, b) = list.unzip(pairs)
    console.print("${list.sum(a)}")
    console.print("${list.sum(b)}")
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
        list.push(out, i)
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
        list.push(out, (x * 2))
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
            .expect_lowered("the binary path lowers this program");
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
    net.connect("evil.test:80").send_line("x")
"#;
        let e = interpreter::run_with(connect_denied, ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not permitted"), "expected a connect denial, got: {e}");

        // narrowing to an address not already held is denied (can't widen).
        let restrict_denied = r#"
fn main(console: Console, net: Net):
    net.only(Net.tcp("evil.test", 80)).connect("evil.test:80").send_line("x")
"#;
        // `resolve_std_src` links `policy`; `run_module` grants the Net allow-list.
        let e = interpreter::run_module(resolve_std_src(restrict_denied), ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not in this Net"), "expected a restrict denial, got: {e}");

        // Attenuation is real: after narrowing to one address, a sibling that
        // was in the original grant is no longer reachable.
        let attenuated = r#"
fn main(console: Console, net: Net):
    let narrow = net.only(Net.tcp("a.test", 80))
    narrow.connect("b.test:80").send_line("x")
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

    /// A record SPREAD (`Point(x: 5, ..p)`) is validated exactly like plain
    /// construction: the named type must be a record, every override field declared,
    /// and none repeated. Skipping this let a repeated override reach the backends,
    /// where they disagreed on which wins (interpreter last, compiled first) — a
    /// silent divergence — and let an unknown type name through. A valid spread still
    /// links and runs identically on both backends.
    #[test]
    fn record_spread_rejects_duplicate_and_unknown_fields() {
        let link_err = |body: &str| -> String {
            let src = format!(
                "type Point:\n    x: Int\n    y: Int\nfn main(console: Console):\n    let p = Point(x: 1, y: 2)\n{body}"
            );
            let m = parser::parse_module(&src).expect("parse");
            crate::pipeline::link(vec![("main".into(), m)], "main")
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
        };
        assert!(
            link_err("    let q = Point(x: 7, x: 8, ..p)\n    console.print(\"${q.x}\")\n")
                .contains("set twice"),
            "a repeated override field in a spread must be rejected",
        );
        assert!(
            link_err("    let q = Bogus(x: 9, ..p)\n    console.print(\"${q.x}\")\n")
                .contains("not a record type"),
            "a spread over an unknown type name must be rejected",
        );
        let ok = "type Point:\n    x: Int\n    y: Int\nfn main(console: Console):\n    let p = Point(x: 1, y: 2)\n    let q = Point(x: 7, ..p)\n    console.print(\"${q.x}\" + \" \" + \"${q.y}\")\n";
        assert_eq!(link_run(ok), vec!["7 2"], "interpreter");
        assert_eq!(wasm_run(ok), vec!["7 2"], "wasm");
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
        list.push(fs, fn(x: Int): (x + i))
    let f0 = list.at(fs, 0)
    let f2 = list.at(fs, 2)
    console.print("${f0(10)}")
    console.print("${f2(10)}")
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
    console.print("${call0(fn(): 42)}")
    let base = 100
    console.print("${call0(fn(): (base + 1))}")
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
    console.print("${apply(h, 5)}")
    console.print("${apply(h, 20)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["12", "42"]); // (5+1)*2, (20+1)*2
    }

    #[test]
    fn captured_inferred_dict_keeps_key_and_value_widths() {
        let src = r#"
import dict

fn call0(f: fn() -> Int) -> Int:
    f()

fn main(console: Console):
    var d = dict.new()
    dict.insert(d, 5000000000, 9000000000)
    let captured = d
    let direct = fn():
        var total = 0
        for (k, v) in dict.pairs(captured):
            total = total + k + v
        total
    console.print("${direct()}")
    console.print("${call0(fn():
        var total = 0
        for (k, v) in dict.pairs(captured):
            total = total + k + v
        total
    )}")
"#;
        let want = vec!["14000000000", "14000000000"];
        assert_eq!(interp(src), want, "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM");
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

    // Integer division/modulo truncate toward zero, and their signs must agree
    // for negative operands across the i64 interpreter and i32 codegen (the
    // results here stay well within i32). Also locks in dict insert-overwrite,
    // removing an absent key, and `get_or`'s default path.
    #[test]
    fn negative_arithmetic_and_dict_mutation_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print("${(0 - (7 / 2))}")
    console.print("${((0 - 7) % 2)}")
    console.print("${(7 / (0 - 2))}")
    console.print("${(7 % (0 - 2))}")
    console.print("${((0 - 7) / (0 - 2))}")
    var d = dict.new()
    dict.insert(d, "k", 1)
    dict.insert(d, "k", 2)
    console.print("${dict.get_or(d, "k", 0)}")
    console.print("${dict.length(d)}")
    dict.remove(d, "missing")
    console.print("${dict.length(d)}")
    console.print("${dict.get_or(d, "absent", 99)}")
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
    console.print("${(list.at(fns, 0))(5)}")
    console.print("${(list.at(fns, 1))(5)}")
    let pick = true
    console.print("${(if pick: fn(x: Int): (x + 100) else: fn(x: Int): x)(7)}")
    let b = Box(fn(x: Int): (x * 3), 7)
    console.print("${((b).f)((b).n)}")
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
    console.print(describe(Yep(50)))
    console.print(describe(Yep(3)))
    console.print(describe(Nope))
    console.print("${if is_even(10): 1 else: 0}")
    console.print("${if is_odd(7): 1 else: 0}")
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
    console.print(classify((0 - 5)))
    console.print(classify(0))
    console.print(classify(200))
    console.print(classify(50))
    console.print(classify(100))
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
    console.print("${((l).from).x}")
    console.print("${((l).to).y}")
    let l2 = Line(from: Point(10, 20), ..l)
    console.print("${((l2).from).x}")
    console.print("${((l2).to).y}")
    console.print("${((l).from).x}")
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
    console.print("${sum_tree(t)}")
    console.print("${depth(t)}")
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
        console.print(n)
    let qtys = [it.qty * 10 for it in cart]
    for q in qtys:
        console.print("${q}")
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
    console.print("${list.length(triples)}")
    var total = 0
    for t in triples:
        let (a, b, c) = t
        total = total + c
    console.print("${total}")
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
        console.print("${p}")
    let upper = [x * 10 + y for x in [1, 2, 3] for y in [1, 2, 3] if y > x]
    for p in upper:
        console.print("${p}")
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
    console.print("${total}")
    var kept = 0
    for y in [1, 2, 3, 4]:
        match y:
            2 ->
                continue
            _ -> 0
        kept = (kept + y)
    console.print("${kept}")
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
    console.print("${sum}")
    var i = 0
    var found = 0
    while (i < 100):
        i = (i + 1)
        if (i < 10):
            continue
        found = i
        break
    console.print("${found}")
    var count = 0
    for a in [1, 2, 3]:
        for b in [1, 2, 3]:
            if (b == 2):
                break
            count = (count + 1)
    console.print("${count}")
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
        console.print("${i}")
    console.print("${list.length(0..=0)}")
    console.print("${list.length(5..=2)}")
    console.print("${list.length([n for n in 1..=4])}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "inclusive range diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "2", "3", "4", "5", "1", "0", "4"]);
    }

    #[test]
    fn range_operator_backends_agree() {
        let src = r#"
fn main(console: Console):
    for i in 0..5:
        console.print("${i}")
    let squares = [x * x for x in 1..5]
    for s in squares:
        console.print("${s}")
    console.print("${list.length(3..3)}")
    console.print("${list.length(2..(1 + 4))}")
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
        console.print("${s}")
    let evens = [n for n in [1, 2, 3, 4, 5, 6] if n % 2 == 0]
    for e in evens:
        console.print("${e}")
    console.print("${list.length([x for x in [] if x > 0])}")
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
        (n, "stop") -> ("stop@" + "${n}")
        (n, s) -> ((s + "=") + "${n}")

fn main(console: Console):
    console.print(quadrant(0, 0))
    console.print(quadrant(0, 5))
    console.print(quadrant(5, 0))
    console.print(quadrant(2, 3))
    console.print(describe((0, "x")))
    console.print(describe((7, "stop")))
    console.print(describe((4, "k")))
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
        list.push(fns, fn(x: Int): (x + captured))
        i = (i + 1)
    for f in fns:
        console.print("${f(10)}")
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
    console.print("${first_of(pi)}")
    console.print(first_of(ps))
    console.print(second_of(ps))
    console.print("${first_of(pm)}")
    console.print(second_of(pm))
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
    console.print("${(p2).x}")
    console.print("${(p2).y}")
    let cond = true
    let p3 = Point(y: 99, ..(if cond: (l).from else: (l).to))
    console.print("${(p3).x}")
    console.print("${(p3).y}")
    let l2 = Line(from: Point(x: 7, ..(l).to), ..l)
    console.print("${((l2).from).x}")
    console.print("${((l2).from).y}")
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
        console.print("${((p).x + (p).y)}")
    for q in [P(10, 1), P(20, 2)]:
        console.print("${(q).x}")
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
    console.print("${result}")
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
    console.print("${total}")
    let make_adder = fn(x: Int): fn(y: Int): (x + y)
    let add3 = make_adder(3)
    console.print("${add3(4)}")
    console.print("${(make_adder(100))(1)}")
    if ("abc" < "abcd"):
        console.print("lt1")
    else:
        console.print("ge1")
    if ("Z" < "a"):
        console.print("lt2")
    else:
        console.print("ge2")
    if ("" < "a"):
        console.print("lt3")
    else:
        console.print("ge3")
    if ("apple" < "apply"):
        console.print("lt4")
    else:
        console.print("ge4")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "closures/ordering diverged");
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
    #[test]
    fn std_json_get_path_backends_agree() {
        let client = r#"
import json
import option
from json import Json
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
            console.print(str_at(j, "user.name"))
            console.print("${int_at(j, "user.age")}")
            console.print(str_at(j, "user.missing"))
        Err(e) -> console.print(json.decode_error_message(e))"#;
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
        Ok(r) -> console.print(http.body(r))
        Err(e) -> console.print(http.http_error_message(e))
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
    console.print("${{http.status(r)}}")
    console.print(http.body(r))
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
    console.print("${{http.status(r)}}")
    console.print(http.body(r))
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
    console.print("${{http.status(r)}}")
    console.print(http.body(r))
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
    console.print(option.unwrap_or(http.header(r, "Content-Type"), "none"))
    console.print(option.unwrap_or(http.header(r, "x-custom"), "none"))
    console.print(option.unwrap_or(http.header(r, "Missing"), "none"))
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
from json import Json
fn main(console: Console):
    let j = JsonObject([
        ("name", JsonString("witchy")),
        ("version", JsonInt(1)),
        ("tags", JsonArray([JsonString("safe"), JsonString("fast")])),
        ("stable", JsonBool(false)),
        ("extra", JsonNull)
    ])
    console.print(json.encode(j))"#;
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
        Ok(j) -> console.print(json.encode(j))
        Err(e) -> console.print("error: " + json.decode_error_message(e))
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
from json import Json
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
            console.print(option.unwrap_or(json.as_string(field(j, "name")), "?"))
            console.print("${option.unwrap_or(json.as_int(field(j, "version")), 0)}")
            console.print("${elem_int(j, "items", 1)}")
        Err(e) -> console.print(json.decode_error_message(e))"#;
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
    console.print(classify(2))
    console.print(classify(5))
    console.print(classify(10))
    console.print("${side(Circle(5))}")
    console.print("${side(Square(7))}")
    console.print("${side(Rect(3, 4))}")
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
        console.print("${x}")
    console.print("${x}")
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
    console.print((b).label)
    console.print("${list.length((b).items)}")
    var total = 0
    for x in (b).items:
        total = (total + x)
    console.print("${total}")
    console.print("${list.at((b).items, 1)}")
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
    console.print("${((o).inner).v}")
    let o2 = Outer(inner: Inner((((o).inner).v + 1)), ..o)
    console.print("${((o2).inner).v}")
    console.print((o).name)
    console.print("${((o).inner).v}")
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
    console.print("${x}")
    console.print("${y}")
    var acc = 0
    var i = 1
    while (i < 5):
        bump_by(acc, i)
        i = (i + 1)
    console.print("${acc}")
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

    /// (RFC-0043 Failure 1) An unrelated `impl Bag: fn push(self, v) -> Bag`
    /// declared elsewhere in the program used to poison the whole-program name
    /// census: `push` entered the `shadowed` set, so a List `xs.push(1)`
    /// statement silently stopped writing back (`parity` agreed, so nothing
    /// caught it). Now resolution is PER RECEIVER TYPE: a List receiver resolves
    /// to `list.push` (a mutator), never to `Bag.push`, so the write-back fires.
    /// Both backends must print `[1]` — the census bug is dead by construction.
    #[test]
    fn rfc0043_unrelated_impl_no_longer_shadows_list_push() {
        let src = "import list\n\
                   type Bag:\n\
                   \x20   n: Int\n\
                   \n\
                   impl Bag:\n\
                   \x20   fn push(self, v: Int) -> Bag:\n\
                   \x20       Bag(self.n + v)\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   var xs = []\n\
                   \x20   xs.push(1)\n\
                   \x20   console.print(\"${xs}\")\n";
        let want = vec!["[1]".to_string()];
        assert_eq!(link_run(src), want, "interpreter: List push must write back despite `impl Bag: push`");
        assert_eq!(wasm_run(src), want, "compiled: List push must write back despite `impl Bag: push`");
    }

    /// (RFC-0043 Failure 2) The old census keyed write-back on the *generic
    /// declared* return type equalling the receiver's: `filter : List(a) ->
    /// List(a)` qualified and silently MUTATED, while `map : List(a) -> List(b)`
    /// did not and silently no-op'd — same syntax, opposite effects, no
    /// diagnostic. Neither is a mutator (`var` receiver) now, so a `filter`/`map`
    /// statement whose result is discarded is a LOUD compile error on both
    /// backends (the check the two backends share), naming the fix.
    #[test]
    fn rfc0043_filter_and_map_statements_are_discard_errors() {
        for method in ["filter", "map"] {
            let body = if method == "filter" {
                "xs.filter(fn(n: Int) -> Bool: n > 2)"
            } else {
                "xs.map(fn(n: Int) -> Int: n * 10)"
            };
            let src = format!(
                "import list\n\
                 fn main(console: Console):\n\
                 \x20   var xs = [1, 2, 3, 4]\n\
                 \x20   {body}\n\
                 \x20   console.print(\"${{xs}}\")\n"
            );
            let linked = resolve_std_src(&src);
            let err = typeck::check(&linked)
                .expect_err(&format!("a discarded `{method}` statement must be a compile error"))
                .to_string();
            assert!(
                err.contains(&format!("result of `{method}` is discarded")),
                "the {method} discard error must name the method and the fix, got: {err}"
            );
        }
    }

    /// (RFC-0043) `let _ = expr` is the explicit-discard escape: it turns the
    /// discard error off while running the call for its effects, and leaves the
    /// receiver untouched. Both backends run and agree (`xs` unchanged).
    #[test]
    fn rfc0043_let_underscore_is_the_discard_escape() {
        let src = "import list\n\
                   fn main(console: Console):\n\
                   \x20   var xs = [1, 2, 3, 4]\n\
                   \x20   let _ = xs.filter(fn(n: Int) -> Bool: n > 2)\n\
                   \x20   console.print(\"${xs}\")\n";
        let want = vec!["[1, 2, 3, 4]".to_string()];
        assert_eq!(link_run(src), want, "interpreter: `let _ =` discards and leaves xs unchanged");
        assert_eq!(wasm_run(src), want, "compiled: `let _ =` discards and leaves xs unchanged");
    }

    /// (RFC-0043) The real mutators still write back in statement form on both
    /// backends — the declared-`var`-receiver path is the same self-assign shape
    /// (`xs = list.push(xs, …)`) the uniqueness pass already optimizes, so
    /// push/insert/set_at/remove all mutate in place and agree.
    #[test]
    fn rfc0043_real_mutators_still_write_back_both_backends() {
        // list.push, list.set_at, dict.insert, dict.remove — one program, mixed.
        let src = "import list\nimport dict\n\
                   fn main(console: Console):\n\
                   \x20   var xs = [1, 2, 3]\n\
                   \x20   xs.push(4)\n\
                   \x20   xs.set_at(0, 9)\n\
                   \x20   console.print(\"${xs}\")\n\
                   \x20   var d = dict.new()\n\
                   \x20   d.insert(\"a\", 1)\n\
                   \x20   d.insert(\"b\", 2)\n\
                   \x20   d.remove(\"a\")\n\
                   \x20   console.print(\"${dict.contains_key(d, \"a\")}\")\n\
                   \x20   console.print(\"${dict.get_or(d, \"b\", 0)}\")\n";
        let want = vec!["[9, 2, 3, 4]".to_string(), "false".to_string(), "2".to_string()];
        assert_eq!(link_run(src), want, "interpreter: real mutators must write back");
        assert_eq!(wasm_run(src), want, "compiled: real mutators must write back");
    }

    /// (BUG-575 / RFC-0043) A match arm used in statement position inherits
    /// statement-position mutator semantics. The arm expression `out.push(value)`
    /// must write back exactly like a bare statement in a block; it is not a value
    /// result to be discarded by the surrounding `match`.
    #[test]
    fn rfc0043_match_arm_mutators_write_back_both_backends() {
        let src = "import list\nimport option\n\
                   fn collect(items: List(Option(String))) -> Result(List(String), String):\n\
                   \x20   var out: List(String) = []\n\
                   \x20   for item in items:\n\
                   \x20       match item:\n\
                   \x20           None -> return Err(\"missing\")\n\
                   \x20           Some(value) -> out.push(value)\n\
                   \x20   Ok(out)\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   match collect([Some(\"x\"), Some(\"y\")]):\n\
                   \x20       Ok(xs) -> console.print(\"${xs}\")\n\
                   \x20       Err(e) -> console.print(e)\n\
                   \x20   match collect([Some(\"ok\"), None]):\n\
                   \x20       Ok(xs) -> console.print(\"${xs}\")\n\
                   \x20       Err(e) -> console.print(e)\n";
        let want = vec!["[x, y]".to_string(), "missing".to_string()];
        assert_eq!(link_run(src), want, "interpreter: match-arm mutator writes back");
        assert_eq!(wasm_run(src), want, "compiled: match-arm mutator writes back");
    }

    /// (RFC-0043) A mutator in statement form on an IMMUTABLE `let` place has no
    /// `var` to write back to — a compile error naming the fix (declare `var`,
    /// or bind the result), not a silent discard.
    #[test]
    fn rfc0043_mutator_on_immutable_place_is_an_error() {
        let src = "import list\n\
                   fn main(console: Console):\n\
                   \x20   let xs = [1, 2, 3]\n\
                   \x20   xs.push(4)\n\
                   \x20   console.print(\"${xs}\")\n";
        let linked = resolve_std_src(src);
        let err = typeck::check(&linked)
            .expect_err("a mutator on a `let` place must be an error")
            .to_string();
        assert!(err.contains("immutable") && err.contains("`let`"),
            "the immutable-place error must explain the fix, got: {err}");
    }

    // ---- RFC-0087: uniform var write-back ----

    /// The ordinary result and `var` write-back are independent channels. Neither
    /// parameter position nor a result matching the mutable argument classifies
    /// the call, and both backends commit the same final values.
    #[test]
    fn rfc0087_former_row3_shapes_write_back_on_both_backends() {
        let non_first = "import list\n\
                         fn foo(x: Int, var xs: List(Int)) -> List(Int):\n\
                         \x20   list.push(xs, x)\n\
                         \x20   xs\n\
                         fn main(console: Console):\n\
                         \x20   var xs = [1]\n\
                         \x20   let ys = foo(9, xs)\n\
                         \x20   console.print(\"${xs}\")\n\
                         \x20   console.print(\"${ys}\")\n";
        let unrelated = "import list\n\
                         fn foo(var xs: List(Int)) -> Int:\n\
                         \x20   xs.push(9)\n\
                         \x20   list.length(xs)\n\
                         fn main(console: Console):\n\
                         \x20   var xs = [1]\n\
                         \x20   let n = foo(xs)\n\
                         \x20   console.print(\"${xs}\")\n\
                         \x20   console.print(\"${n}\")\n";
        for (src, want) in [
            (non_first, vec!["[1, 9]".to_string(), "[1, 9]".to_string()]),
            (unrelated, vec!["[1, 9]".to_string(), "2".to_string()]),
        ] {
            assert_eq!(link_run(src), want, "interpreter writes back and returns");
            assert_eq!(wasm_run(src), want, "compiled backend agrees");
        }
    }

    /// Return inference never selects mutation semantics. An inferred result uses
    /// the same move-in/move-out ABI as an explicitly annotated result.
    #[test]
    fn rfc0087_elided_var_result_writes_back_on_both_backends() {
        let elided = "import list\n\
                      fn bump(var xs: List(Int), by: Int):\n\
                      \x20   list.push(xs, by)\n\
                      \x20   list.length(xs)\n\
                      fn main(console: Console):\n\
                      \x20   var xs = [1, 2, 3]\n\
                      \x20   let n = bump(xs, 5)\n\
                      \x20   console.print(\"${xs}\")\n\
                      \x20   console.print(\"${n}\")\n";
        let want = vec!["[1, 2, 3, 5]".to_string(), "4".to_string()];
        assert_eq!(link_run(elided), want, "interpreter inferred return");
        assert_eq!(wasm_run(elided), want, "compiled inferred return");
    }

    /// RFC-0087's discard rule depends on the resolved `var` convention, not call
    /// syntax. Free and method calls both commit write-back when their result is
    /// discarded explicitly or implicitly.
    #[test]
    fn rfc0087_discard_rule_is_effect_based() {
        let free_std = "import list\n\
                        fn main(console: Console):\n\
                        \x20   var xs = [1, 2, 3]\n\
                        \x20   list.push(xs, 2)\n\
                        \x20   console.print(\"${xs}\")\n";
        let free_user = "import list\n\
                         fn bump(var xs: List(Int), by: Int) -> Int:\n\
                         \x20   xs.push(by)\n\
                         \x20   list.length(xs)\n\
                         fn main(console: Console):\n\
                         \x20   var xs = [1, 2, 3]\n\
                         \x20   bump(xs, 5)\n\
                         \x20   console.print(\"${xs}\")\n";
        for (src, want) in [(free_std, "[1, 2, 3, 2]"), (free_user, "[1, 2, 3, 5]")] {
            assert_eq!(link_run(src), [want], "interpreter commits free var call");
            assert_eq!(wasm_run(src), [want], "compiled backend commits free var call");
        }

        let escaped = "import list\n\
                       fn main(console: Console):\n\
                       \x20   var xs = [1, 2, 3]\n\
                       \x20   let _ = list.push(xs, 2)\n\
                       \x20   console.print(\"${xs}\")\n";
        let want = vec!["[1, 2, 3, 2]".to_string()];
        assert_eq!(link_run(escaped), want, "interpreter explicit discard still writes back");
        assert_eq!(wasm_run(escaped), want, "compiled explicit discard still writes back");
    }

    /// (RFC-0049) `dict.set_at` is deleted; the `d[k] = v` place-assign sugar is
    /// retargeted to `dict.insert` once the receiver's type is known (Dict), while
    /// `xs[i] = v` on a list still lowers to `list.set_at`. Both backends agree.
    #[test]
    fn rfc0049_dict_place_assign_retargets_to_insert() {
        let src = "import dict\n\
                   fn main(console: Console):\n\
                   \x20   var d = dict.new()\n\
                   \x20   d[\"a\"] = 1\n\
                   \x20   d[\"b\"] = 2\n\
                   \x20   d[\"a\"] = 9\n\
                   \x20   var xs = [1, 2, 3]\n\
                   \x20   xs[1] = 99\n\
                   \x20   console.print(\"${dict.get_or(d, \"a\", 0)}\")\n\
                   \x20   console.print(\"${dict.length(d)}\")\n\
                   \x20   console.print(\"${list.at(xs, 1)}\")\n";
        let expected = ["9", "2", "99"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// (RFC-0044 rule 1) `list.index_of` returns `Option(Int)` — `Some(i)` on a
    /// hit, `None` on a miss — never a -1 sentinel; `list.position` is the
    /// by-predicate Option search. `ascii.to_digit` is `Option(Int)` too. Both
    /// backends agree on the Option shape and the `??` fallback.
    #[test]
    fn rfc0044_lookup_absence_is_option() {
        let src = "import list\n\
                   import ascii\n\
                   fn main(console: Console):\n\
                   \x20   let xs = [10, 20, 30]\n\
                   \x20   console.print(\"${list.index_of(xs, 20) ?? -1}\")\n\
                   \x20   console.print(\"${list.index_of(xs, 99) ?? -1}\")\n\
                   \x20   console.print(\"${list.position(xs, fn(x: Int): x > 15) ?? -1}\")\n\
                   \x20   console.print(\"${ascii.to_digit(\"7\") ?? -1}\")\n\
                   \x20   console.print(\"${ascii.to_digit(\"z\") ?? -1}\")\n";
        let expected = ["1", "-1", "1", "7", "-1"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// (RFC-0044 rule 1) The `string` search/index family returns `Option`, not a
    /// `-1`/`""` sentinel: `index_of`/`last_index_of` -> `Option(Int)`, `char_at`
    /// -> `Option(String)`. The private raw scan intrinsic (`string.find`, keyed
    /// on the -1 ABI) still powers the tight std loops. Both backends agree.
    #[test]
    fn rfc0044_string_search_absence_is_option() {
        let src = "\
                   fn main(console: Console):\n\
                   \x20   console.print(\"${\"hello\".index_of(\"ll\") ?? -1}\")\n\
                   \x20   console.print(\"${\"hello\".index_of(\"z\") ?? -1}\")\n\
                   \x20   console.print(\"${\"a.b.c\".last_index_of(\".\") ?? -1}\")\n\
                   \x20   console.print(\"${\"abc\".last_index_of(\".\") ?? -1}\")\n\
                   \x20   console.print(\"${\"hi\".char_at(1) ?? \"?\"}\")\n\
                   \x20   console.print(\"${\"hi\".char_at(9) ?? \"?\"}\")\n\
                   \x20   console.print(\"${\"banana\".count(\"a\")}\")\n";
        let expected = ["2", "-1", "3", "-1", "i", "?", "3"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }

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

    /// std/json: `decode` rejects an overflowing exponent (BUG-241), an invalid
    /// string escape (BUG-243), a leading-zero number and a raw control character
    /// (BUG-244), and a duplicate object key (BUG-262); `float_of` accepts an
    /// integer JSON number as a Float (BUG-356), and finite JsonFloat values still
    /// encode as JSON numbers. Both backends agree.
    #[test]
    fn json_rejects_malformed_and_handles_floats_on_both_backends() {
        let src = "import json\n\
                   from json import Json\n\
                   fn dec(label: String, text: String, console: Console):\n\
                   \x20   match json.decode(text):\n\
                   \x20       Ok(j) -> console.print(label + \": \" + json.encode(j))\n\
                   \x20       Err(e) -> console.print(label + \": ERR\")\n\
                   fn main(console: Console):\n\
                   \x20   dec(\"exp_overflow\", \"1e9223372036854775808\", console)\n\
                   \x20   dec(\"exp_inf\", \"1e400\", console)\n\
                   \x20   dec(\"bad_escape\", \"\\\"a\\\\qb\\\"\", console)\n\
                   \x20   dec(\"leading_zero\", \"01\", console)\n\
                   \x20   dec(\"neg_leading_zero\", \"-01\", console)\n\
                   \x20   dec(\"zero_ok\", \"0\", console)\n\
                   \x20   dec(\"dup_key\", \"{\\\"a\\\":1,\\\"a\\\":2}\", console)\n\
                   \x20   dec(\"exp_ok\", \"1.5e3\", console)\n\
                   \x20   match json.float_of(JsonInt(1)):\n\
                   \x20       Ok(f) -> console.print(\"float_of_int: ${f}\")\n\
                   \x20       Err(e) -> console.print(\"float_of_int: ERR\")\n\
                   \x20   console.print(\"encode_finite: \" + json.encode(JsonFloat(1.5)))\n";
        let expected = [
            "exp_overflow: ERR",
            "exp_inf: ERR",
            "bad_escape: ERR",
            "leading_zero: ERR",
            "neg_leading_zero: ERR",
            "zero_ok: 0",
            "dup_key: ERR",
            "exp_ok: 1500.0",
            "float_of_int: 1.0",
            "encode_finite: 1.5",
        ];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// (BUG-374) JSON has no NaN/Infinity tokens, and Witchy already uses
    /// JsonNull for intentional null / Option.None. Encoding a non-finite Float
    /// must therefore be a loud boundary error, not silent data erasure to null.
    #[test]
    fn json_encode_rejects_nonfinite_floats_on_both_backends() {
        let cases = [
            (
                "encode_nan",
                "import json\nfrom json import Json\nfn main(console: Console):\n    console.print(json.encode(JsonFloat(0.0 / 0.0)))\n",
            ),
            (
                "encode_inf",
                "import json\nfrom json import Json\nfn main(console: Console):\n    console.print(json.encode(JsonFloat(1.0 / 0.0)))\n",
            ),
            (
                "encode_neg_inf",
                "import json\nfrom json import Json\nfn main(console: Console):\n    console.print(json.encode(JsonFloat(0.0 - (1.0 / 0.0))))\n",
            ),
            (
                "encode_nested_object",
                "import json\nfrom json import Json\nfn main(console: Console):\n    console.print(json.encode(JsonObject([(\"ratio\", JsonFloat(0.0 / 0.0))])))\n",
            ),
        ];
        for (label, src) in cases {
            let linked = resolve_std_src(src);
            typeck::check(&linked).unwrap_or_else(|e| panic!("{label} typecheck: {e}"));
            let ierr = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter must abort")
                .to_string();
            assert!(
                ierr.contains("json.encode: non-finite Float cannot be encoded as JSON"),
                "{label} interpreter mismatch: {ierr}"
            );
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered(&format!("{label}: the binary path lowers this program"));
            let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort").to_string();
            assert!(
                cerr.contains("json.encode: non-finite Float cannot be encoded as JSON"),
                "{label} compiled mismatch: {cerr}"
            );
        }

        let interpreter_only = [
            (
                "stringify_reflected",
                "import json\nfn main(console: Console):\n    console.print(json.stringify(.{ratio: 0.0 / 0.0}))\n",
            ),
            (
                "server_send_reflected",
                "import server\nfn main(console: Console):\n    let _r = server.send(200, .{ratio: 1.0 / 0.0})\n    console.print(\"unreachable\")\n",
            ),
        ];
        for (label, src) in interpreter_only {
            let linked = resolve_std_src(src);
            typeck::check(&linked).unwrap_or_else(|e| panic!("{label} typecheck: {e}"));
            let ierr = interpreter::run_module(linked, ".", Vec::new())
                .expect_err("public helper must abort before producing JSON")
                .to_string();
            assert!(
                ierr.contains("json.encode: non-finite Float cannot be encoded as JSON"),
                "{label} interpreter mismatch: {ierr}"
            );
        }
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

    /// REGRESSION (BUG-253): `xs.sort()` dispatches through `Ord`, so a list of
    /// derived-`Ord` records sorts (it used to fail 'expected Int' by binding the
    /// Int-only `list.sort`); Ints still sort. Identical on both backends.
    #[test]
    fn list_sort_orders_records_through_ord_backends_agree() {
        let src = "import list\ntype V derive(PartialEq, Eq, PartialOrd, Ord):\n    major: Int\n    minor: Int\nfn main(console: Console):\n    var values = [V(3, 1), V(1, 2), V(2, 0)]\n    values.sort()\n    for v in values:\n        console.print(\"${v.major}\" + \".\" + \"${v.minor}\")\n    var ints = [3, 1, 2, 5]\n    ints.sort()\n    console.print(\"${ints}\")\n";
        let expected = ["1.2", "2.0", "3.1", "[1, 2, 3, 5]"];
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
