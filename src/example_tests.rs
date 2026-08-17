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

    /// Link a single-`main` source (pulling in any imported std module) and run
    /// it on the interpreter — the path that resolves `import crypto`.
    fn link_run(src: &str) -> Vec<String> {
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        interpreter::run_module(linked, ".", Vec::new()).expect("run")
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
                let parsed = parse_std_import_cached(&name);
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
                let parsed = parse_std_import_cached(&name);
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


    /// Property tests: a `for` over a random range must compute exactly the same
    /// result as a Rust reference range, on BOTH backends (so they also agree
    /// with each other) — across sign, inclusive/exclusive, empty, and `continue`.
    mod range_for_properties {
        use super::interp;
        use proptest::prelude::*;
        use std::cell::RefCell;

        // `run_on_wasm` builds a fresh `Runtime` (a real Wasmtime `Engine::new()` —
        // Cranelift ISA setup, not free) per call. Each proptest case here calls it
        // once, and `with_cases(96)` means 96 fresh engines per test function —
        // enough to blow past the nextest per-test timeout. The generated programs
        // differ every case (so the *module* still compiles fresh each time, as it
        // must), but proptest runs cases sequentially on one thread, so the `Engine`
        // itself can be built once per thread and reused across all of a test
        // function's cases.
        thread_local! {
            static RUNTIME: RefCell<crate::runtime::Runtime> =
                RefCell::new(crate::runtime::Runtime::new().expect("runtime"));
        }

        fn run_on_wasm_cached(src: &str) -> Vec<String> {
            use crate::runtime::Capabilities;
            let linked = super::resolve_std_src(src);
            super::typeck::check(&linked).expect("typecheck");
            let bytes = super::codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            RUNTIME.with(|rt| {
                let mut rt = rt.borrow_mut();
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
            })
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

        #[test]
        fn range_for_boundaries_match_reference_on_both_backends() {
            for (lo, hi) in BOUNDARIES {
                for inclusive in [false, true] {
                    let (src, want) = sum_case(lo, hi, inclusive);
                    assert_eq!(interp(&src), want, "interpreter disagrees on:\n{src}");
                    assert_eq!(run_on_wasm_cached(&src), want, "compiled disagrees on:\n{src}");
                }
                let (src, want) = odds_case(lo, hi);
                assert_eq!(interp(&src), want, "interpreter disagrees on:\n{src}");
                assert_eq!(run_on_wasm_cached(&src), want, "compiled disagrees on:\n{src}");
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(16))]

            #[test]
            fn sum_matches_reference(lo in -300i64..300, len in 0i64..600, inclusive in any::<bool>()) {
                let (src, want) = sum_case(lo, lo + len, inclusive);
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm_cached(&src), want);
            }

            #[test]
            fn continue_skipping_odds_matches_reference(lo in -100i64..100, len in 0i64..300) {
                let (src, want) = odds_case(lo, lo + len);
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm_cached(&src), want);
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
    mod rfc0122_control_flow;
    mod rfc0122_references;
    mod rfc0122_wasm_list_carrier;
    mod rfc0122_wasm_exclusive_list_return;
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
    mod wir_binary;
    mod compiler_footprint;
    mod rfc0111_layout;
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
                            let bytes = codegen::compile_checked_module_binary(&checked)
                                .expect_lowered(&format!("{context} compiles to WASM"));
                            let compiled = crate::run_wasm_bytes(&bytes)
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





    /// The glamour rune's source, embedded so tests can `import glamour` without a
    /// sibling file on disk — the same trick `coven`'s server modules use.
    const GLAMOUR_SRC: &str = include_str!("../projects/glamour/src/glamour.witchy");


    /// `std/markdown`'s source, embedded so a test can `import markdown` (it `import glamour`
    /// transitively) without sibling files on disk.
    const MARKDOWN_SRC: &str = include_str!("../projects/glamour/src/markdown.witchy");

    /// `GLAMOUR_SRC` is a ~6k-line `const &str` baked in at compile time — its
    /// content can never differ within one process run. `glamour::glamour_run_both`/
    /// `markdown_run_both` and roughly a dozen tests that inline the same
    /// `parser::parse_module(GLAMOUR_SRC)` call (most sharply,
    /// `glamour_media_policy_css_and_loader_share_one_query_corpus`, which parses it
    /// once per corpus entry inside a loop) each re-parsed those ~6k lines from
    /// scratch with no reuse across calls — the same redundant-parse class as the
    /// rfc0087-census fix and the `resolve_std_src`/`try_link_std` fix above.
    /// Caching by a `OnceLock<Module>` is unconditionally safe here: a cache hit
    /// returns the identical AST a fresh parse would produce (2026-08-10).
    fn glamour_module_cached() -> ast::Module {
        static CACHE: std::sync::OnceLock<ast::Module> = std::sync::OnceLock::new();
        CACHE
            .get_or_init(|| parser::parse_module(GLAMOUR_SRC).expect("parse glamour"))
            .clone()
    }

    /// Like [`glamour_module_cached`] for `MARKDOWN_SRC`.
    fn markdown_module_cached() -> ast::Module {
        static CACHE: std::sync::OnceLock<ast::Module> = std::sync::OnceLock::new();
        CACHE
            .get_or_init(|| parser::parse_module(MARKDOWN_SRC).expect("parse markdown"))
            .clone()
    }


    // ---- RFC-0043: write-back by declaration (not the name census) ----

    // ---- RFC-0087: uniform var write-back ----
