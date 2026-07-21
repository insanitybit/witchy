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
    mod strings;
    mod traits_generics;
    mod wir_binary;
    mod compiler_footprint;
    mod example_sweeps;
    mod examples_programs;
    mod modes;

    fn wasm_run(src: &str) -> Vec<String> {
        witchy_interp::compiler_natives::install();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
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

    // ---- RFC-0052: one pattern grammar ------------------------------------

    /// (RFC-0052) A Float SCRUTINEE bound to a variable pattern now compiles (the
    /// former check-passes/codegen-fails hole) and agrees on both backends.
    #[test]
    fn float_scrutinee_binding_backends_agree() {
        let src = "fn main(console: Console):\n    let r = match 1.5:\n        x -> x + 1.0\n    console.print(\"${r}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2.5"]);
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

    fn assert_fn_compiles(src: &str) {
        assert!(typeck::check_str(src).is_ok(), "{:?}", typeck::check_str(src));
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        Module::new(&wasm_gc_engine(), &bytes).expect("valid wasm");
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


    /// The glamour rune's source, embedded so tests can `import glamour` without a
    /// sibling file on disk — the same trick `coven`'s server modules use.
    const GLAMOUR_SRC: &str = include_str!("../projects/glamour/src/glamour.witchy");


    /// `std/markdown`'s source, embedded so a test can `import markdown` (it `import glamour`
    /// transitively) without sibling files on disk.
    const MARKDOWN_SRC: &str = include_str!("../projects/glamour/src/markdown.witchy");


    // ---- RFC-0043: write-back by declaration (not the name census) ----

    // ---- RFC-0087: uniform var write-back ----

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
