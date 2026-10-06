    use crate::{ast, codegen, interpreter, parser, typeck};
    fn validates_wasm_gc(bytes: &[u8]) -> bool {
        let features = wasmparser::WasmFeatures::default()
            | wasmparser::WasmFeatures::GC
            | wasmparser::WasmFeatures::REFERENCE_TYPES
            | wasmparser::WasmFeatures::FUNCTION_REFERENCES;
        wasmparser::Validator::new_with_features(features)
            .validate_all(bytes)
            .is_ok()
    }

    fn interp(src: &str) -> Vec<String> {
        link_run(src)
    }

    pub(crate) fn is_nightly() -> bool {
        std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1")
            || std::env::var_os("WITCHY_FULL_SUITE").is_some_and(|v| v == "1")
            || std::env::var_os("WITCHY_FULL_SOAK_TESTS").is_some_and(|v| v == "1")
    }

    pub(crate) fn proptest_cases() -> u32 {
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8)
    }

    /// Link a single-`main` source (pulling in any imported std module) and run
    /// it on the interpreter — the path that resolves `import crypto`.
    fn link_run(src: &str) -> Vec<String> {
        let linked = prepared_module(src);
        interpreter::run_module(linked, ".", Vec::new()).expect("run")
    }

    /// Compile and run on Wasm with explicit CLI argument inputs.
    #[allow(dead_code)]
    fn run_on_wasm_args(src: &str, args: Vec<String>) -> Vec<String> {
        let artifact = admitted_artifact(src);
        run_admitted_with(&artifact, crate::runtime::Capabilities { args, ..full_grants(Vec::new()) }, 4)
    }

    /// The grants the compiled-path helpers give a program: output, the
    /// ambient Clock/Env, the working directory, and `net_allow`.
    fn full_grants(net_allow: Vec<String>) -> crate::runtime::Capabilities {
        crate::runtime::Capabilities {
            print: true,
            print_int: true,
            clock: true,
            env: true,
            dir_root: Some(std::path::PathBuf::from(".")),
            dir_read: true,
            dir_write: true,
            net_allow: Some(net_allow),
            net_connect: true,
            net_listen: true,
            ..Default::default()
        }
    }

    /// The admitted canonical artifact for a single-source program, checked
    /// against the bundled std. Cached per thread by source: hundreds of tests
    /// run the same fixtures.
    fn admitted_artifact(src: &str) -> std::rc::Rc<witchy_wir::AdmittedCanonicalArtifact> {
        thread_local! {
            static ARTIFACTS: std::cell::RefCell<std::collections::HashMap<String, std::rc::Rc<witchy_wir::AdmittedCanonicalArtifact>>> =
                std::cell::RefCell::new(std::collections::HashMap::new());
        }
        ARTIFACTS.with(|cache| {
            if let Some(artifact) = cache.borrow().get(src) {
                return artifact.clone();
            }
            let checked = checked_modules(&[("main", src)], "main");
            let artifact = std::rc::Rc::new(
                codegen::compile_checked_module_artifact(&checked)
                    .expect_lowered("the canonical pipeline lowers this program"),
            );
            let mut cache = cache.borrow_mut();
            if cache.len() >= 128 {
                cache.clear();
            }
            cache.insert(src.to_string(), artifact.clone());
            artifact
        })
    }

    /// Check a program of user modules exactly as `witchy <file>` does --
    /// std imports, comptime expansion and module owners included -- by
    /// writing it to a fresh directory and loading the entry file. A fixture
    /// that passes a bundled std module verbatim is resolved from std instead.
    fn checked_modules(modules: &[(&str, &str)], entry: &str) -> witchy_types::pipeline::CheckedModule {
        use std::hash::{Hash, Hasher};
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        modules.hash(&mut hasher);
        let dir = std::env::temp_dir().join(format!(
            "witchy-example-tests-{}-{}-{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            hasher.finish(),
        ));
        std::fs::create_dir_all(&dir).expect("fixture directory");
        for (name, src) in modules {
            if *name != entry && crate::bundled_module(name) == Some(*src) {
                continue;
            }
            std::fs::write(dir.join(format!("{name}.witchy")), src).expect("write fixture module");
        }
        let entry_path = dir.join(format!("{entry}.witchy"));
        let checked = crate::link_file_checked(entry_path.to_str().expect("UTF-8 fixture path"));
        let _ = std::fs::remove_dir_all(&dir);
        checked.unwrap_or_else(|error| panic!("type-check the program: {error}")).0
    }

    /// Run an admitted artifact on this thread's runtime under `caps`.
    fn run_admitted_with(
        artifact: &witchy_wir::AdmittedCanonicalArtifact,
        caps: crate::runtime::Capabilities,
        pages: usize,
    ) -> Vec<String> {
        with_batch_runtime(|rt| {
            let mut actor = rt.spawn_admitted(artifact, caps, pages).expect("spawn");
            actor.run().expect("run");
            actor.output()
        })
    }

    /// A bundled std module's parse result never changes within one process
    /// run (`crate::bundled_module(name)` is a deterministic, static lookup —
    /// unlike user source, there is no possibility of the same name meaning
    /// different content). `resolve_std_src`/`try_link_std` are the shared
    /// entry points behind hundreds of example/differential tests in this one
    /// binary, each of which re-parsed the same ~49 std modules from scratch
    /// with no reuse across calls; caching by name is unconditionally safe
    /// here (2026-08-10, same redundant-parse class as the rfc0087-census
    /// fix, which alone cut that test's wall time 34%).
    fn std_module_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, ast::Module>> {
        static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, ast::Module>>> =
            std::sync::OnceLock::new();
        CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    fn parse_std_import_cached(name: &str) -> ast::Module {
        let mut cache = std_module_cache().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cached) = cache.get(name) {
            return cached.clone();
        }
        let source = crate::bundled_module(name).expect("a bundled std module");
        let parsed = parser::parse_module(source).expect("parse std module");
        cache.insert(name.to_string(), parsed.clone());
        parsed
    }

    thread_local! {
        static WASM_RUNTIME: std::cell::RefCell<crate::runtime::Runtime> =
            std::cell::RefCell::new(crate::runtime::Runtime::batch_quick().expect("runtime"));

        static PREPARED_MODULES: std::cell::RefCell<std::collections::HashMap<String, ast::Module>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }

    /// One Wasmtime engine per test thread. `Runtime::new()` builds a Cranelift
    /// ISA and epoch-interruption config — tens to hundreds of ms — and the
    /// differential helpers used to pay that on every case. Batch-quick
    /// skips Speed-tier lowering: the programs are tiny and the suite
    /// compiles hundreds of them.
    fn with_batch_runtime<R>(f: impl FnOnce(&mut crate::runtime::Runtime) -> R) -> R {
        WASM_RUNTIME.with(|rt| f(&mut rt.borrow_mut()))
    }

    /// Linked+typechecked module cache on this thread. Reuses parsed, linked,
    /// and typechecked AST graphs across repeated invocations and proptest templates.
    fn prepared_module(src: &str) -> ast::Module {
        PREPARED_MODULES.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some(module) = slot.get(src) {
                return module.clone();
            }
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            if slot.len() >= 128 {
                slot.clear();
            }
            slot.insert(src.to_string(), linked.clone());
            linked
        })
    }

    fn cached_prepared_module(src: &str) -> Option<ast::Module> {
        PREPARED_MODULES.with(|slot| slot.borrow().get(src).cloned())
    }

    /// Resolve `src`'s `import`s against the bundled std and link the whole set —
    /// the source-level analog of the CLI's `link_file` / the lib's
    /// `resolve_std_only`. The COMPILED backend needs every reached std function
    /// present (only builtins are inlined), so a single-module `linker::link` is
    /// insufficient; the interpreter (`link_run`) tolerates it because it resolves
    /// std at run time, but `compile_module_binary` does not.
    fn resolve_std_src(src: &str) -> ast::Module {
        use std::collections::HashSet;
        let entry = parser::parse_module(src).expect("parse");
        let mut modules: Vec<(String, ast::Module)> = vec![("main".to_string(), entry)];
        let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
        let mut next = 0;
        while next < modules.len() {
            let imports = modules[next].1.imports.clone();
            next += 1;
            for name in imports {
                if !loaded.insert(name.clone()) {
                    continue;
                }
                let parsed = parse_std_import_cached(&name);
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
        use std::collections::HashSet;
        let entry = parser::parse_module(src).expect("parse");
        let mut modules: Vec<(String, ast::Module)> = vec![("main".to_string(), entry)];
        let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
        let mut next = 0;
        while next < modules.len() {
            let imports = modules[next].1.imports.clone();
            next += 1;
            for name in imports {
                if !loaded.insert(name.clone()) {
                    continue;
                }
                let parsed = parse_std_import_cached(&name);
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
        let full_sweep = std::env::var_os("WITCHY_FULL_EXAMPLES_SWEEP").is_some_and(|v| v == "1")
            || std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1");
        if !full_sweep {
            // Representative subset of examples covering primitives, control flow, functions, collections, and algorithms.
            let representative = [
                "examples/hello/src/hello.witchy",
                "examples/fib/src/fib.witchy",
                "examples/counter/src/counter.witchy",
                "examples/dice/src/dice.witchy",
                "examples/maze/src/maze.witchy",
                "examples/wordcount/src/wordcount.witchy",
                "examples/calculator/src/calculator.witchy",
            ];
            let mut out = Vec::new();
            for path_str in representative {
                let p = std::path::PathBuf::from(path_str);
                if p.exists() {
                    out.push(p);
                }
            }
            if !out.is_empty() {
                out.sort();
                return out;
            }
        }
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


    /// Property tests: a `for` over a random range must compute exactly the same
    /// result as a Rust reference range, on BOTH backends (so they also agree
    /// with each other) — across sign, inclusive/exclusive, empty, and `continue`.
    mod range_for_properties {
        use super::interp;

        // `run_on_wasm` builds a fresh `Runtime` (a real Wasmtime `Engine::new()` —
        // Cranelift ISA setup, not free) per call. Each proptest case here calls it
        // once, and `with_cases(96)` means 96 fresh engines per test function —
        // enough to blow past the nextest per-test timeout. The generated programs
        // differ every case (so the *module* still compiles fresh each time, as it
        // must), but proptest runs cases sequentially on one thread, so the `Engine`
        // itself can be built once per thread and reused across all of a test
        // function's cases.
        fn run_on_wasm_cached(src: &str) -> Vec<String> {
            super::run_on_wasm(src)
        }

        fn sum_case(lo: i64, hi: i64, inclusive: bool) -> (String, Vec<String>) {
            let op = if inclusive { "..=" } else { ".." };
            let src = format!(
                "fn main(console: Console):\n    var s = 0\n    for i in {lo}{op}{hi}:\n        s = s + i\n    console.print(\"${{s}}\")\n"
            );
            let reference: i64 = if inclusive { (lo..=hi).sum() } else { (lo..hi).sum() };
            (src, vec![reference.to_string()])
        }

        fn odds_case(lo: i64, hi: i64) -> (String, Vec<String>) {
            let src = format!(
                "fn main(console: Console):\n    var s = 0\n    for i in {lo}..{hi}:\n        if i % 2 != 0:\n            continue\n        s = s + i\n    console.print(\"${{s}}\")\n"
            );
            let reference: i64 = (lo..hi).filter(|x| x % 2 == 0).sum();
            (src, vec![reference.to_string()])
        }

        // Every case pays a full typecheck + codegen + Wasmtime spawn, so cases
        // are expensive rather than free: at 96 apiece these two functions cost
        // 111s of the suite's CPU and intermittently overran the per-test
        // timeout under load, turning a whole gate red for no defect.
        //
        // The behavior worth guarding here lives entirely at the boundaries, and
        // uniform sampling reached the most important one — the empty range —
        // in only about 15% of runs. Pin the boundaries as an explicit table and
        // keep a smaller random sweep for the interior: cheaper AND stricter
        // than resampling the interior 96 times.
        const BOUNDARIES: [(i64, i64); 6] = [
            (0, 0),       // empty exclusive at the origin
            (7, 3),       // reversed bounds: empty in both forms
            (0, 1),       // single element
            (-3, 3),      // crossing zero
            (-300, -297), // wholly negative
            (297, 300),   // the top of the generator's own range
        ];

        fn boundary_suite() -> (String, Vec<String>) {
            let mut source = String::from("fn main(console: Console):\n");
            let mut expected = Vec::with_capacity(BOUNDARIES.len() * 3);
            let mut case = 0;
            for (lo, hi) in BOUNDARIES {
                for inclusive in [false, true] {
                    let op = if inclusive { "..=" } else { ".." };
                    source.push_str(&format!(
                        "    var s{case} = 0\n    for i in {lo}{op}{hi}:\n        s{case} = s{case} + i\n    console.print(\"${{s{case}}}\")\n"
                    ));
                    let reference: i64 =
                        if inclusive { (lo..=hi).sum() } else { (lo..hi).sum() };
                    expected.push(reference.to_string());
                    case += 1;
                }
                source.push_str(&format!(
                    "    var s{case} = 0\n    for i in {lo}..{hi}:\n        if i % 2 != 0:\n            continue\n        s{case} = s{case} + i\n    console.print(\"${{s{case}}}\")\n"
                ));
                expected.push((lo..hi).filter(|x| x % 2 == 0).sum::<i64>().to_string());
                case += 1;
            }
            (source, expected)
        }

        #[test]
        fn range_for_boundaries_match_reference_on_both_backends() {
            // Keep every literal boundary in one module. The old loop compiled
            // and launched 18 separate modules on each backend even though the
            // cases share no setup or state.
            let (source, expected) = boundary_suite();
            assert_eq!(interp(&source), expected, "interpreter disagrees on:\n{source}");
            assert_eq!(
                run_on_wasm_cached(&source),
                expected,
                "compiled disagrees on:\n{source}"
            );
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
    mod capability_narrowing;
    mod cli_std_modules;
    mod std_result_list_combinators;
    mod record_match_integration;
    mod func_values;
    mod rc_corpus;
    mod crypto_jwt_oauth;
    mod rfc0046_dispatch;
    mod gen_async;
    mod closures;
    mod collections;
    mod match_control;
    mod structs;
    mod strings;
    mod host_modules;
    mod misc_semantics;
    mod syntax_forms;
    mod string_methods;
    mod container_places;
    mod regex_module;
    mod interpolation;
    mod try_result;
    mod abort_contract;
    mod duration_prng;
    mod equality;
    mod math_float;
    mod async_channels;
    mod sandbox_vm;
    mod compiler_footprint;
    mod example_sweeps;
    mod examples_programs;
    mod modes;
    mod must_consume;

    fn wasm_run(src: &str) -> Vec<String> {
        witchy_interp::compiler_natives::install();
        let artifact = admitted_artifact(src);
        let caps = crate::runtime::Capabilities { quiet: true, rand: true, ..full_grants(Vec::new()) };
        run_admitted_with(&artifact, caps, crate::RUN_MEMORY_PAGES)
    }



    // ---- RFC-0052: one pattern grammar ------------------------------------


    // HEAP-TYPE MATRIX (RFC-0035 step 3 gate). Corpus 1-3 above only exercised RECORD/ADT
    // elements — the false confidence that let the reverted emission ship (5e9e167): it
    // assumed every i32 element was an offset-0 rc_alloc object, which was FALSE for the
    // header-less strings/lists/dicts from the direct-bump helpers. Phase A now routes every
    // value producer through $rc_alloc, so these element types are all headered. This matrix
    // is the gate that would have caught the revert: each element type the revert corrupted,
    // read-past-set_at / aliased / stored / match-on-read, must stay byte-identical across
    // interp == wasm == wasm(rc-floor). Authored FIRST, before re-applying the dup/drop
    // emission — a premature/wrong dec flips one of these red.


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
    /// root docs, `spec/`, and `book/src/` (sorted for a stable manifest).
    fn doc_markdown_files() -> Vec<std::path::PathBuf> {
        let full_sweep = std::env::var_os("BLESS_EXAMPLES").is_some()
            || std::env::var_os("WITCHY_FULL_DOCS_SWEEP").is_some_and(|v| v == "1")
            || std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1");
        if full_sweep {
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
        } else {
            // Representative core set covering fundamentals, capabilities, async, and spec definitions.
            vec![
                "README.md".into(),
                "book/src/getting-started-hello.md".into(),
                "book/src/tour-functions.md".into(),
                "book/src/capabilities-authority.md".into(),
                "spec/language.md".into(),
            ]
        }
    }

    fn main_declares_console_read(module: &ast::Module) -> bool {
        module.items.iter().any(|item| {
            matches!(item, ast::Item::Function(function) if function.name == "main"
                && function.params.iter().any(|parameter| {
                    matches!(&parameter.ty, Some(ast::Type::Named(name, rights))
                        if name == "Console"
                            && rights.iter().any(|right| {
                                matches!(right, ast::Type::Named(name, args)
                                    if name == "Read" && args.is_empty())
                            }))
                }))
        })
    }

    /// (RFC-0041 Phase 3) Validate every documentation example and generate the
    /// runnable-example classification manifest as pretty JSON. Runnable examples
    /// execute on both backends; the manifest records the interpreter output. This
    /// is the single source of truth the runnable book reads, so the browser never
    /// re-derives classification.
    fn generate_examples_manifest() -> String {
        let files = doc_markdown_files();
        let browser_menu =
            witchy_caps::menu::HostMenu::parse(witchy_caps::menu::BROWSER_MENU)
                .expect("the checked-in browser host menu must be valid");
        let per_file: Vec<Vec<serde_json::Value>> = std::thread::scope(|s| {
            let handles: Vec<_> = files.iter().map(|file| {
                let browser_menu = &browser_menu;
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
                        let checked = witchy::resolve_std_only_checked(&snippet)
                            .unwrap_or_else(|e| panic!("{context} fails to link or type-check: {e}"));
                        let linked = checked.module();

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
                        let reads_console = main_declares_console_read(linked);
                        let uses_workers = footprint_module.imports.iter().any(|m| m == "vm")
                            || linked.imports.iter().any(|m| m == "vm");
                        let runnable =
                            has_main && console_only && !reads_console && !reads_argv && !uses_workers;
                        let linked_console_only = crate::capabilities::analyze(linked)
                            .total
                            .keys()
                            .all(|cap| *cap == "Console");
                        let parity_runnable =
                            has_main && linked_console_only && !reads_console && !reads_argv;
                        // (RFC-0102) `browser_runnable` is the program's typed host
                        // requirements checked against the published browser menu. Capability
                        // families/rights and non-capability facilities (argv/VM workers) all
                        // pass through that one subset check; the classifier owns no private
                        // host allowlist. This is a SUPERSET of `runnable` (which stays
                        // Console-only + output-pinned): a
                        // `browser_runnable`-but-not-`runnable` block has NO pinned `output`,
                        // because its result depends on real time or on host-supplied Dir/Env
                        // fixtures rather than a deterministic oracle run. The docs cell uses
                        // this to offer a Run button (empty fixtures) without claiming a golden.
                        let mut browser_requirements =
                            witchy_caps::menu::HostRequirements::from_cap_set(&fp.total)
                                .unwrap_or_else(|e| {
                                    panic!("{context} has invalid host requirements: {e}")
                                });
                        if reads_argv {
                            browser_requirements
                                .require_facility(witchy_caps::menu::HostFacility::Argv);
                        }
                        if uses_workers {
                            browser_requirements
                                .require_facility(witchy_caps::menu::HostFacility::Vm);
                        }
                        let browser_runnable =
                            has_main && browser_menu.check(&browser_requirements).portable();
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
                        let interpreted = if runnable || parity_runnable {
                            Some(
                                interpreter::run_checked_module(
                                    &checked,
                                    std::path::Path::new("."),
                                    Vec::new(),
                                )
                                .unwrap_or_else(|e| {
                                    panic!("{context} fails on the interpreter: {e}")
                                }),
                            )
                        } else {
                            None
                        };
                        if parity_runnable {
                            let artifact = codegen::compile_checked_module_artifact(&checked)
                                .expect_lowered(&format!("{context} compiles to WASM"));
                            let compiled = crate::run_admitted_artifact(&artifact)
                                .unwrap_or_else(|e| panic!("{context} fails on WASM: {e}"));
                            assert_eq!(
                                interpreted.as_ref().expect("parity examples run"),
                                &compiled,
                                "{context}: the backends DIVERGE"
                            );
                        }
                        let output = if runnable { interpreted.unwrap() } else { Vec::new() };
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

    /// End-to-end through the *compiled* path: type-check, compile through the
    /// canonical pipeline, run the admitted artifact with the output
    /// capabilities granted, and return what the program printed.
    fn run_on_wasm(src: &str) -> Vec<String> {
        let artifact = admitted_artifact(src);
        run_admitted_with(&artifact, full_grants(Vec::new()), 4)
    }





    /// Check a multi-module program, compile it through the canonical
    /// pipeline, run it with output capabilities, and return what it printed.
    fn run_linked_on_wasm(sources: &[(&str, &str)], entry: &str) -> Vec<String> {
        run_linked_on_wasm_net(sources, entry, &[])
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
        witchy_interp::compiler_natives::install();
        let checked = checked_modules(sources, entry);
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("the canonical pipeline lowers this program");
        let grants = full_grants(net_allow.iter().map(|s| (*s).to_string()).collect());
        run_admitted_with(&artifact, grants, 4)
    }





    // ---- RFC-0043: write-back by declaration (not the name census) ----

    // ---- RFC-0087: uniform var write-back ----
