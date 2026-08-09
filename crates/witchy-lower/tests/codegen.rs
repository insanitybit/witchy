//! Black-box codegen tests: everything here drives the crate's PUBLIC API
//! (compile_module_binary / assemble_wir_module / analysis reports) plus
//! wasmtime/wasmparser to inspect the produced module. Moved out of
//! src/codegen_tests.rs (audit 2026-08-08): integration tests don't need
//! crate internals and don't belong in the library's compilation unit. The
//! three tests that DO exercise private internals (loan_root/loan_region/
//! lambda_content_key) remain in src/codegen_tests.rs. The body keeps its
//! original one-level indentation inside `mod tests` so the embedded witchy
//! source literals (indentation-sensitive!) are byte-identical to the
//! pre-move file.

mod tests {
    use witchy_lower::codegen::*;
    use witchy_lower::analysis;
    use witchy_syntax::ast::*;

    // Mirrors the private `codegen::SCRUT_POOL` (16): the per-depth scrutinee
    // save-slot pool. The scratch-pool helper builds a match nested one level
    // BEYOND the pool on purpose; if the pool size changes, update this
    // (the in-crate tests in src/codegen_tests.rs pin the behavior itself).
    const SCRUT_POOL: usize = 16;
    fn beyond_wir_assignment_scratch_pool() -> String {
        (0..=SCRUT_POOL).fold("values".to_string(), |value, _| {
            format!("list.__set_at({value}, 0, 0)")
        })
    }
    fn assert_partial_capture_reaches_wir_fallback(source: &str) {
        let module = parse_module(source).expect("parse partial-capture fallback fixture");
        let unsupported = compile_module_binary(&module)
            .expect_unsupported("partial WIR capture must remain a fallback, not an identity error");
        assert!(
            unsupported.message.contains("reachable functions do not fully lower to WIR"),
            "the whole-unit fallback must survive partial loan-fact consumption: {unsupported}",
        );
    }
    use witchy_syntax::parser::parse_module;
    use std::sync::{Arc, Mutex};
    use wasmtime::{Caller, Engine, Linker, Module as WtModule, Store};

    fn gc_wasm_features() -> wasmparser::WasmFeatures {
        wasmparser::WasmFeatures::default()
            | wasmparser::WasmFeatures::GC
            | wasmparser::WasmFeatures::REFERENCE_TYPES
            | wasmparser::WasmFeatures::FUNCTION_REFERENCES
    }

    fn gc_wasm_payloads(
        wasm: &[u8],
    ) -> impl Iterator<Item = wasmparser::Result<wasmparser::Payload<'_>>> {
        wasmparser::Validator::new_with_features(gc_wasm_features())
            .validate_all(wasm)
            .expect("valid Wasm GC module");
        wasmparser::Parser::new(0).parse_all(wasm)
    }

    fn gc_wasmtime_engine() -> Engine {
        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        Engine::new(&config).expect("Wasm GC engine")
    }

    fn assert_catalog_names_live_wir_helpers(names: &[&str]) {
        for &name in names {
            let spec = witchy_syntax::intrinsics::lookup(name).expect("cataloged operation");
            assert!(
                witchy_types::typeck::intrinsic(name),
                "{} must not compile its self-recursive std placeholder",
                spec.name
            );
            for helper in spec.wir_helpers {
                assert!(
                    witchy_wir::wir_helpers::wir_helper(helper).is_some(),
                    "{} names missing WIR helper {}",
                    spec.name,
                    helper
                );
            }
        }
    }

    #[test]
    fn operation_catalog_names_live_wir_helpers() {
        use witchy_syntax::intrinsics;

        for operations in [
            intrinsics::STRING_OPERATIONS,
            intrinsics::MATH_OPERATIONS,
            intrinsics::LIST_OPERATIONS,
            intrinsics::DICT_OPERATIONS,
            intrinsics::CRYPTO_OPERATIONS,
            intrinsics::SECRETSTORE_OPERATIONS,
            &[intrinsics::REGEX_MATCH_SPANS],
        ] {
            assert_catalog_names_live_wir_helpers(operations);
        }
    }

    #[test]
    fn operation_catalog_special_helper_routes_are_explicit() {
        use witchy_syntax::intrinsics;

        assert_eq!(
            intrinsics::declared_wir_helper(intrinsics::LIST_PUSH, "list_push_cap"),
            Some("list_push_cap")
        );
        assert_eq!(
            intrinsics::declared_wir_helper(intrinsics::GENERATED_LIST_PUSH, "list_push_cap"),
            Some("list_push_cap")
        );
        assert_eq!(
            intrinsics::declared_wir_helper(intrinsics::DICT_INSERT, "dict_insert_cap"),
            Some("dict_insert_cap")
        );
        assert_eq!(
            intrinsics::declared_wir_helper(intrinsics::DICT_UPDATE, "dict_update_cap"),
            Some("dict_update_cap")
        );

        for name in intrinsics::CRYPTO_OPERATIONS {
            assert!(
                intrinsics::sole_wir_helper(name).is_some(),
                "{name} must have exactly one WIR helper"
            );
        }

        let helper = intrinsics::sole_wir_helper(intrinsics::REGEX_MATCH_SPANS)
            .expect("regex operation has one WIR helper");
        assert_eq!(helper, "regex_match_spans");

        for name in intrinsics::SECRETSTORE_OPERATIONS {
            let helper = intrinsics::sole_wir_helper(name)
                .expect("SecretStore operation has one WIR helper");
            assert_eq!(helper, "secretstore_lookup");
        }
    }

    /// (RFC-0045) Define the always-linked, authority-free `__witchy_abort` import
    /// so a module that routes an abort through it (float ordering, list/bytes OOB,
    /// str_to_int, `fail`) instantiates in these minimal test linkers. The body
    /// traps, matching the real host's `bail!` contract (the call never returns).
    fn define_abort<T: 'static>(linker: &mut Linker<T>) {
        linker
            .func_wrap(
                "witchy",
                "__witchy_abort",
                |_: Caller<'_, T>, _t: i32, _a: i64, _b: i64, _s: i32| -> wasmtime::Result<()> {
                    wasmtime::bail!("runtime error (test harness abort)")
                },
            )
            .unwrap();
        // Checked-heap builds register every descriptor allocation. The full
        // runtime poisons and sweeps these redzones; lowering tests need only an
        // authority-free sink so the instrumented module can instantiate.
        linker
            .func_wrap(
                "witchy",
                "heap_register",
                |_: Caller<'_, T>, _start: i32, _end: i32| {},
            )
            .unwrap();
        linker
            .func_wrap(
                "witchy",
                "heap_frontier",
                |_: Caller<'_, T>, _frontier: i32| {},
            )
            .unwrap();
    }

    fn import_param_counts(wasm: &[u8]) -> std::collections::BTreeMap<String, usize> {
        let mut func_param_counts = Vec::new();
        let mut imports = Vec::new();
        for payload in gc_wasm_payloads(wasm) {
            match payload.expect("valid wasm") {
                wasmparser::Payload::TypeSection(reader) => {
                    for group in reader {
                        for ty in group.expect("recursive type group").into_types() {
                            let params = match ty.composite_type.inner {
                                wasmparser::CompositeInnerType::Func(func) => {
                                    Some(func.params().len())
                                }
                                _ => None,
                            };
                            func_param_counts.push(params);
                        }
                    }
                }
                wasmparser::Payload::ImportSection(reader) => {
                    for imp in reader.into_imports() {
                        let imp = imp.expect("import");
                        if let wasmparser::TypeRef::Func(idx) = imp.ty {
                            imports.push((imp.name.to_string(), idx as usize));
                        }
                    }
                }
                _ => {}
            }
        }
        imports
            .into_iter()
            .map(|(name, idx)| {
                let params = func_param_counts
                    .get(idx)
                    .and_then(|params| *params)
                    .unwrap_or_else(|| panic!("missing function type {idx} for import {name}"));
                (name, params)
            })
            .collect()
    }

    fn no_comptime_expansion(
        _name: &str,
        _module: &mut witchy_syntax::ast::Module,
        _modules: &[(String, witchy_syntax::ast::Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        Ok(witchy_syntax::origin::OriginTable::default())
    }

    fn link_test_modules(
        modules: Vec<(String, witchy_syntax::ast::Module)>,
        entry: &str,
        user_modules: &std::collections::HashSet<String>,
    ) -> witchy_syntax::ast::Module {
        witchy_syntax::linker::link_with_user_modules(
            modules,
            entry,
            no_comptime_expansion,
            user_modules,
        )
        .expect("link lowering test modules")
    }

    fn link_list_app(source: &str) -> witchy_syntax::ast::Module {
        let list_module = parse_module(
            witchy_syntax::linker::bundled_source("list").expect("bundled list module"),
        )
        .expect("parse bundled list module");
        let app = parse_module(source).expect("parse list app");
        link_test_modules(
            vec![("list".into(), list_module), ("app".into(), app)],
            "app",
            &std::collections::HashSet::from(["app".to_string()]),
        )
    }

    #[test]
    fn public_lowering_outcome_distinguishes_success_and_rejection() {
        let lowered = parse_module("fn main() -> Int:\n    1\n").expect("parse lowered module");
        assert!(matches!(
            compile_module_binary(&lowered),
            LoweringOutcome::Lowered(_)
        ));

        let rejected = parse_module("fn helper() -> Int:\n    1\n")
            .expect("parse rejected module");
        let error = compile_module_binary(&rejected)
            .expect_rejected("module without an entrypoint should be rejected");
        assert!(
            error.message.contains("neither a `main` entrypoint nor a string export"),
            "unexpected rejection: {error}"
        );
    }

    #[test]
    fn incomplete_glamour_export_family_is_rejected() {
        let module = parse_module(
            "grantable capability UiRoot:\n    policy: String\n\n\
             type State:\n    State(Int)\n\n\
             @browser\npub fn glamour_init(_root: UiRoot, _input: Bytes) -> State:\n    State(0)\n",
        )
        .expect("parse partial Glamour application");
        let error = compile_module_binary(&module)
            .expect_rejected("partial RFC-0108 family must not become a host ABI");
        assert!(
            error.message.contains("incomplete RFC-0108 application export family")
                && error.message.contains("glamour_dispatch"),
            "unexpected rejection: {error}"
        );
    }

    #[test]
    fn build_module_is_zero_ambient() {
        // A compiled build step imports ONLY its build host functions — none of
        // the runtime authority. That's the structural zero-ambient guarantee:
        // the dangerous host functions don't exist for the guest to call.
        let module = parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    out.write_out(\"x.witchy\", schema.read_build(\"a.proto\"))\n",
        )
        .expect("parse");
        let wasm = compile_build_module(&module).expect_lowered("compile build module");
        let mut imports = Vec::new();
        let mut exports = Vec::new();
        for payload in gc_wasm_payloads(&wasm) {
            match payload.expect("valid wasm") {
                wasmparser::Payload::ImportSection(reader) => {
                    for imp in reader.into_imports() {
                        imports.push(imp.expect("import").name.to_string());
                    }
                }
                wasmparser::Payload::ExportSection(reader) => {
                    for ex in reader {
                        exports.push(ex.expect("export").name.to_string());
                    }
                }
                _ => {}
            }
        }
        assert!(exports.iter().any(|e| e == "run"), "build entrypoint becomes the run export");
        assert!(imports.iter().any(|i| i == "build_out_write"), "write_out import present");
        assert!(imports.iter().any(|i| i == "build_read_len"), "read_build import present");
        let params = import_param_counts(&wasm);
        assert_eq!(params.get("build_out_write"), Some(&2), "BuildOut receiver must not cross the host ABI");
        assert_eq!(params.get("build_read_len"), Some(&1), "BuildRead receiver must not cross the host ABI");
        // No runtime-authority imports leaked in.
        for forbidden in ["dir_write", "dir_read_len", "net_connect", "net_listen", "print", "now", "now_monotonic", "crypto.sign"] {
            assert!(!imports.iter().any(|i| i == forbidden), "build module must not import `{forbidden}`: {imports:?}");
        }
    }

    #[test]
    fn grantful_build_primitives_compile_to_build_imports_only() {
        let module = parse_module(
            "import option\nfn build(out: BuildOut, env: BuildEnv, dl: BuildNet, cc: BuildExec):\n    let v = match env.get_build_env(\"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"x.witchy\", v + dl.fetch_build(\"127.0.0.1:9\", \"/schema\") + cc.run_tool(\"cat\", \"input\"))\n",
        )
        .expect("parse");
        let wasm = compile_build_module(&module).expect_lowered("compile build module");
        let mut imports = Vec::new();
        for payload in gc_wasm_payloads(&wasm) {
            if let wasmparser::Payload::ImportSection(reader) = payload.expect("valid wasm") {
                for imp in reader.into_imports() {
                    imports.push(imp.expect("import").name.to_string());
                }
            }
        }

        for needed in [
            "build_out_write",
            "build_env_len",
            "build_env_fill",
            "build_fetch_len",
            "build_exec_run",
        ] {
            assert!(imports.iter().any(|i| i == needed), "build import `{needed}` missing: {imports:?}");
        }
        for forbidden in ["env_len", "env_fill", "exec_run", "net_connect", "net_try_connect"] {
            assert!(
                !imports.iter().any(|i| i == forbidden),
                "build primitive must not lower to runtime import `{forbidden}`: {imports:?}"
            );
        }
        let params = import_param_counts(&wasm);
        assert_eq!(params.get("build_env_len"), Some(&1), "BuildEnv receiver must not cross the host ABI");
        assert_eq!(params.get("build_env_fill"), Some(&2), "BuildEnv receiver must not cross the host ABI");
        assert_eq!(params.get("build_fetch_len"), Some(&2), "BuildNet receiver must not cross the host ABI");
        assert_eq!(params.get("build_exec_run"), Some(&2), "BuildExec receiver must not cross the host ABI");
    }

    fn run_int(src: &str) -> i64 {
        let module = parse_module(src).expect("parse");
        run_int_module(&module)
    }

    fn run_int_module(module: &witchy_syntax::ast::Module) -> i64 {
        let bytes = compile_module_binary(module)
            .expect_lowered("the binary path lowers this program");
        let engine = gc_wasmtime_engine();
        let wt = WtModule::new(&engine, &bytes).expect("valid wasm");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        define_abort(&mut linker);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
                *sink.lock().unwrap() = Some(n);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().take().expect("printed a value")
    }

    fn run_int_with_i64_globals(
        source: &str,
        names: &[&str],
    ) -> (i64, std::collections::BTreeMap<String, i64>) {
        let module = parse_module(source).expect("parse counter program");
        run_int_module_with_i64_globals(&module, names)
    }

    fn run_int_module_with_i64_globals(
        module: &witchy_syntax::ast::Module,
        names: &[&str],
    ) -> (i64, std::collections::BTreeMap<String, i64>) {
        let bytes = compile_module_binary(module)
            .expect_lowered("the counter program lowers");
        let engine = gc_wasmtime_engine();
        let wt = WtModule::new(&engine, &bytes).expect("valid wasm");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        define_abort(&mut linker);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
                *sink.lock().unwrap() = Some(n);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).expect("instantiate counter program");
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("run export")
            .call(&mut store, ())
            .expect("run counter program");
        let mut counters = std::collections::BTreeMap::new();
        for &name in names {
            let value = instance
                .get_global(&mut store, name)
                .unwrap_or_else(|| panic!("missing counter export `{name}`"))
                .get(&mut store);
            let wasmtime::Val::I64(value) = value else {
                panic!("counter export `{name}` is not i64: {value:?}");
            };
            counters.insert(name.to_string(), value);
        }
        let printed = captured.lock().unwrap().take().expect("printed a value");
        (printed, counters)
    }

    fn run_int_with_layout_metrics(
        source: &str,
    ) -> (i64, std::collections::BTreeMap<String, i64>, i32) {
        let module = parse_module(source).expect("parse layout metric program");
        let bytes = compile_module_binary(&module)
            .expect_lowered("the layout metric program lowers");
        let engine = gc_wasmtime_engine();
        let wt = WtModule::new(&engine, &bytes).expect("valid layout metric wasm");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        define_abort(&mut linker);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
                *sink.lock().unwrap() = Some(n);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).expect("instantiate layout metrics");
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("run export")
            .call(&mut store, ())
            .expect("run layout metric program");
        let mut counters = std::collections::BTreeMap::new();
        for name in [
            "__witchy_rc_headers_emitted",
            "__witchy_rc_headers_elided",
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_rc_alloc_calls",
            "__witchy_bump_alloc_calls",
        ] {
            let value = instance
                .get_global(&mut store, name)
                .unwrap_or_else(|| panic!("missing layout counter `{name}`"))
                .get(&mut store);
            let wasmtime::Val::I64(value) = value else {
                panic!("layout counter `{name}` is not i64: {value:?}");
            };
            counters.insert(name.to_string(), value);
        }
        let heap = instance
            .get_global(&mut store, "__heap")
            .expect("heap export")
            .get(&mut store);
        let wasmtime::Val::I32(heap) = heap else {
            panic!("heap export is not i32: {heap:?}");
        };
        let printed = captured.lock().unwrap().take().expect("printed a value");
        (printed, counters, heap)
    }

    #[test]
    fn proven_unique_packed_list_elides_exactly_one_rc_header() {
        let source = r#"
mode opt

type Point packed:
    x: Int
    y: Int

fn main() -> Int:
    let points = [Point(2, 3), Point(5, 7)]
    points[0].x * 10 + points[0].y + points[1].x * 10 + points[1].y
"#;
        let release = witchy_syntax::opt::OptSet::default_set();
        witchy_syntax::opt::set_for_tests(Some(release));
        let optimized = run_int_with_layout_metrics(source);

        witchy_syntax::opt::set_for_tests(Some(
            release.without(witchy_syntax::opt::Opt::RcElide),
        ));
        let rc_backed = run_int_with_layout_metrics(source);

        witchy_syntax::opt::set_for_tests(Some(
            release.without(witchy_syntax::opt::Opt::RcFloor),
        ));
        let rc_floor_off = run_int_with_layout_metrics(source);

        witchy_syntax::opt::set_for_tests(Some(
            release.without(witchy_syntax::opt::Opt::Unbox),
        ));
        let unbox_off = run_int_with_layout_metrics(source);

        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::none()));
        let none = run_int_with_layout_metrics(source);
        witchy_syntax::opt::set_for_tests(None);

        for (label, run) in [
            ("release", &optimized),
            ("-rc-elide", &rc_backed),
            ("-rc-floor", &rc_floor_off),
            ("-unbox", &unbox_off),
            ("none", &none),
        ] {
            assert_eq!(run.0, 80, "value parity under {label}");
        }
        assert_eq!(optimized.1["__witchy_rc_headers_emitted"], 0);
        assert_eq!(optimized.1["__witchy_rc_headers_elided"], 1);
        assert_eq!(optimized.1["__witchy_packed_alloc_calls"], 1);
        assert_eq!(optimized.1["__witchy_rc_alloc_calls"], 0);
        assert_eq!(rc_backed.1["__witchy_rc_headers_emitted"], 1);
        assert_eq!(rc_backed.1["__witchy_rc_headers_elided"], 0);
        assert_eq!(rc_backed.1["__witchy_packed_alloc_calls"], 1);
        assert_eq!(rc_backed.1["__witchy_rc_alloc_calls"], 1);
        assert_eq!(rc_floor_off.1["__witchy_rc_headers_emitted"], 0);
        assert_eq!(rc_floor_off.1["__witchy_rc_headers_elided"], 1);
        assert_eq!(rc_floor_off.1["__witchy_rc_alloc_calls"], 0);
        assert_eq!(rc_floor_off.2, optimized.2, "drop-floor deopt keeps header-free bytes");
        assert_eq!(rc_backed.2 - optimized.2, 8, "one physical [rc,size] header");
        assert_eq!(
            optimized.1["__witchy_packed_alloc_bytes"],
            rc_backed.1["__witchy_packed_alloc_bytes"],
            "descriptor payload bytes stay identical",
        );
        for deopt in [&unbox_off, &none] {
            assert_eq!(deopt.1["__witchy_rc_headers_emitted"], 0);
            assert_eq!(deopt.1["__witchy_rc_headers_elided"], 0);
        }
    }

    #[test]
    fn header_elision_falls_back_at_ownership_and_domain_boundaries() {
        let cases = [
            (
                "call-return",
                r#"
mode opt
type Point packed:
    x: Int
fn relay(points: List(Point)) -> List(Point):
    points
fn main() -> Int:
    let points = relay([Point(4), Point(9)])
    var total = 0
    for point in points:
        total = total + point.x
    total
"#,
                13,
            ),
            (
                "alias",
                r#"
mode opt
type Point packed:
    x: Int
fn main() -> Int:
    let points = [Point(4), Point(9)]
    let alias = points
    var total = 0
    for point in alias:
        total = total + point.x
    total
"#,
                13,
            ),
            (
                "normal-storage",
                r#"
mode opt
type Point packed:
    x: Int
type Holder:
    points: List(Point)
fn main() -> Int:
    let points = [Point(4), Point(9)]
    let holder = Holder(points)
    13
"#,
                13,
            ),
            (
                "nested-constructor",
                r#"
mode opt
type Point packed:
    x: Int
fn main() -> Int:
    let points = if true: [Point(4), Point(9)] else: [Point(0)]
    var total = 0
    for point in points:
        total = total + point.x
    total
"#,
                13,
            ),
            (
                "borrow-root-and-lifetime",
                r#"
mode opt
type Point packed:
    x: Int
fn view(points: let('a) List(Point)) -> View(List(Point), 'a):
    points
fn main() -> Int:
    let points = [Point(4), Point(9)]
    let borrowed = view(points)
    var total = 0
    for point in borrowed:
        total = total + point.x
    total
"#,
                13,
            ),
        ];
        let release = witchy_syntax::opt::OptSet::default_set();
        for (label, source, expected) in cases {
            witchy_syntax::opt::set_for_tests(Some(release));
            let (value, counters, _) = run_int_with_layout_metrics(source);
            witchy_syntax::opt::set_for_tests(None);
            assert_eq!(value, expected, "fallback value for {label}");
            assert_eq!(counters["__witchy_rc_headers_elided"], 0, "{label}");
            assert!(counters["__witchy_rc_headers_emitted"] > 0, "{label}: {counters:?}");
        }
    }

    #[test]
    fn compiled_packed_callable_carries_one_canonical_layout_bundle() {
        let source = r#"
mode opt
type Point packed:
    x: Int
fn relay(points: List(Point)) -> List(Point):
    points
fn main() -> Int:
    let points = relay([Point(4)])
    4
"#;
        let module = parse_module(source).expect("parse layout bundle program");
        let bytes = compile_module_binary(&module).expect_lowered("compile layout bundle program");
        let sections = wasmparser::Parser::new(0)
            .parse_all(&bytes)
            .filter_map(|payload| match payload.expect("valid bundled Wasm") {
                wasmparser::Payload::CustomSection(section)
                    if section.name() == "witchy.layouts" =>
                {
                    Some(section.data().to_vec())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(sections.len(), 1, "new binaries carry exactly one layout section");
        let (bundle, interner) = witchy_wir::layout::LayoutBundle::decode_canonical(&sections[0])
            .expect("compiler emits a canonical, dependency-complete bundle");
        let callable_list = bundle
            .roots()
            .iter()
            .copied()
            .find(|id| {
                interner
                    .get(*id)
                    .is_some_and(|descriptor| matches!(descriptor.kind(), witchy_wir::layout::LayoutKind::PackedList { .. }))
            })
            .expect("relay's exact packed List(Point) callable layout is a root");
        assert!(interner.get(callable_list).is_some());

        let scalar = parse_module("fn main() -> Int:\n    1\n")
            .expect("parse scalar layout bundle program");
        let scalar_bytes =
            compile_module_binary(&scalar).expect_lowered("compile empty layout bundle program");
        let scalar_sections = wasmparser::Parser::new(0)
            .parse_all(&scalar_bytes)
            .filter_map(|payload| match payload.expect("valid scalar Wasm") {
                wasmparser::Payload::CustomSection(section)
                    if section.name() == "witchy.layouts" =>
                {
                    Some(section.data().to_vec())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(scalar_sections.len(), 1, "scalar binaries carry one empty bundle");
        let (empty, _) = witchy_wir::layout::LayoutBundle::decode_canonical(&scalar_sections[0])
            .expect("empty bundle is canonical");
        assert!(empty.roots().is_empty());
        assert_eq!(empty.descriptors().count(), 0);
    }

    #[test]
    fn declared_packed_values_cross_direct_and_stored_boundaries() {
        let source = r#"
mode opt

type Point packed:
    x: Int
    y: Int

type Holder:
    points: List(Point)

fn make() -> Point:
    Point(7, 11)

fn score(point: Point) -> Int:
    point.x * 10 + point.y

fn relay(points: List(Point)) -> List(Point):
    points

fn stored(holder: Holder) -> Int:
    list.at(holder.points, 1).y

fn main() -> Int:
    let points = relay([Point(1, 2), Point(3, 4)])
    score(make()) * 100 + list.at(points, 0).x * 10 + stored(Holder(points))
"#;
        assert_eq!(run_int(source), 8114);

        let module = parse_module(source).expect("parse packed boundary program");
        let wir = assemble_wir_module(&module)
            .expect_lowered("direct packed boundaries lower to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        assert!(wat.contains("__witchy_packed_record_"), "record descriptor helper: {wat}");
        assert!(wat.contains("__witchy_packed_list_"), "list descriptor helper: {wat}");
        assert!(wat.contains("call $relay"), "list pointer crosses the direct call: {wat}");
        assert!(wat.contains("call $score"), "record pointer crosses the direct call: {wat}");
    }

    #[test]
    fn declared_packed_direct_boundaries_have_zero_adapter_work() {
        let source = r#"
mode opt

type Point packed:
    x: Int
    y: Int

fn make() -> Point:
    Point(7, 11)

fn relay(point: Point) -> Point:
    point

fn make_points() -> List(Point):
    [Point(3, 4), Point(5, 6)]

fn relay_points(points: List(Point)) -> List(Point):
    points

fn score(point: Point) -> Int:
    point.x * 10 + point.y

fn main() -> Int:
    let points = relay_points(make_points())
    score(relay(make())) + list.at(points, 0).x + list.at(points, 1).y
"#;
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
        ];
        let (result, counters) = run_int_with_i64_globals(source, &names);
        assert_eq!(result, 90);
        assert_eq!(counters["__witchy_packed_alloc_calls"], 2);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 56);

        let module = parse_module(source).expect("parse structural counter program");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("counter program lowers to WIR"),
        );
        assert!(wat.contains("call $relay"), "record crosses a direct call: {wat}");
        assert!(wat.contains("call $relay_points"), "list crosses a direct call: {wat}");
        assert!(!wat.contains("call $mk2"), "no legacy record/list reshape: {wat}");
        assert!(
            !wat.contains("__witchy_packed_boxed_elements")
                && !wat.contains("__witchy_packed_reshaped_bytes"),
            "zero-only adapter counters stay absent until real adapters have increment sites: {wat}"
        );
        if witchy_wir::wir_helpers::heap_check_enabled() {
            assert!(wat.contains("call $heap_register"), "checked descriptor allocations register redzones: {wat}");
        }
    }

    #[test]
    fn packed_own_boundaries_use_layout_ownership_not_legacy_capacity_slots() {
        let source = r#"
mode opt

type Point packed:
    x: Int

type Token packed:
    Empty
    Value(Int)

fn point_score(own point: Point) -> Int:
    point.x

fn token_score(own token: Token) -> Int:
    match token:
        Empty -> 3
        Value(value) -> value

fn point_count(own points: List(Point)) -> Int:
    list.length(points)

fn main() -> Int:
    point_score(Point(7)) * 100 + token_score(Value(11)) * 10
        + point_count([Point(1), Point(2)])
"#;
        assert_eq!(run_int(source), 812);
        let module = parse_module(source).expect("parse packed own boundaries");
        let typed = witchy_types::typeck::annotate_checked(module.clone())
            .expect("annotate packed ownership boundary");
        let access = witchy_types::access::checked_facts(typed.module(), typed.table())
            .expect("checked packed ownership facts");
        for function in ["point_score", "token_score", "point_count"] {
            let fact = analysis::call_ownership_fact(
                access.declaration(function).expect("declared ownership fact"),
            );
            assert_eq!(
                fact.consuming_state_param(),
                Some(0),
                "analysis must retain the logical LayoutDependent consuming state for {function}",
            );
            assert_eq!(
                fact.own_capacity_param(),
                None,
                "an exact LayoutDependent value is not a uniform container capacity channel",
            );
        }
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module)
                .expect_lowered("packed record, sum, and list own boundaries lower exactly"),
        );
        for function in ["point_score", "token_score", "point_count"] {
            let start = wat
                .find(&format!("(func ${function}"))
                .unwrap_or_else(|| panic!("missing {function}: {wat}"));
            let tail = &wat[start..];
            let signature = tail.lines().next().expect("function signature line");
            assert!(
                !signature.contains("__witchy_owncap") && !signature.contains("__cap"),
                "LayoutDependent own `{function}` must not use the legacy capacity ABI: {signature}"
            );
            assert_eq!(
                signature.matches("(param $").count(),
                1,
                "the exact descriptor pointer is the complete own ABI: {signature}"
            );
        }
        assert!(wat.contains("call $point_score"));
        assert!(wat.contains("call $token_score"));
        assert!(wat.contains("call $point_count"));
    }

    #[test]
    fn declared_packed_direct_boundaries_match_pinned_oracle() {
        let source = r#"
mode opt

type Point packed:
    x: Int
    y: Int

type Holder:
    points: List(Point)

fn make() -> Point:
    Point(7, 11)

fn relay(points: List(Point)) -> List(Point):
    points

fn answer() -> Int:
    let points = relay([Point(1, 2), Point(3, 4)])
    make().x * 1000 + list.at(points, 0).x * 100 + list.length(Holder(points).points)

fn main(console: Console):
    console.print("${answer()}")
"#;
        let module = parse_module(source).expect("parse packed parity program");
        let bytes = compile_module_binary(&module)
            .expect_lowered("compiled backend lowers packed parity program");
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        let compiled = captured.lock().unwrap().clone();
        let oracle = vec!["7102".to_string()];
        assert_eq!(compiled, oracle, "compiled expected value");
    }

    #[test]
    fn declared_packed_layout_survives_user_module_linking() {
        let model = parse_module(r#"
mode opt

type Point packed:
    x: Int
    y: Int

pub fn relay(points: List(Point)) -> List(Point):
    points

pub fn origin() -> Point:
    Point(5, 8)
"#).expect("parse model module");
        let app = parse_module(r#"
mode opt
from model import Point, relay, origin

fn score(point: Point) -> Int:
    point.x * 10 + point.y

fn main() -> Int:
    let points = relay([Point(2, 3), Point(7, 11)])
    score(origin()) * 100 + list.at(points, 1).x * 10 + list.at(points, 0).y
"#).expect("parse app module");
        let linked = link_test_modules(
            vec![("model".into(), model), ("app".into(), app)],
            "app",
            &std::collections::HashSet::from(["model".to_string(), "app".to_string()]),
        );
        assert_eq!(run_int_module(&linked), 5873);
    }

    #[test]
    fn closed_generic_helper_specializes_for_packed_layout() {
        let source = r#"
mode opt

import list

type Point packed:
    x: Int
    y: Int

fn relay(first: a, second: a) -> List(a):
    var values: List(a) = [first]
    list.push(values, second)
    let repeated: a = list.at(values, 0)
    list.push(values, repeated)
    values

fn main() -> Int:
    let points: List(Point) = relay(Point(2, 3), Point(7, 11))
    list.length(points) * 1000
        + list.at(points, 0).x * 100
        + list.at(points, 1).y * 10
        + list.at(points, 2).x
"#;
        let logical_module = parse_module(source).expect("parse generic packed helper");
        let module = link_list_app(source);
        assert_eq!(run_int_module(&module), 3312);
        let (_, logical_specializations) =
            witchy_types::traits::lower_for_wasm_with_specializations(module.clone())
                .into_parts();
        assert!(
            logical_specializations.values().any(|identity| identity
                .types()
                .iter()
                // The linked fixture qualifies user types with their module
                // (`app.Point`); the logical identity keeps that Point identity.
                .any(|ty| ty.as_str() == "Point" || ty.as_str().ends_with(".Point"))),
            "the generic instance retains its logical Point identity"
        );

        let unpacked = parse_module(
            "type Point:\n    x: Int\n    y: Int\n",
        )
        .expect("parse logical reflection oracle");
        assert_eq!(
            witchy_syntax::reflect::module_type_info_exprs(&logical_module)
                .expect("reflect packed declaration logically"),
            witchy_syntax::reflect::module_type_info_exprs(&unpacked)
                .expect("reflect unpacked declaration logically"),
            "public logical reflection never exposes the packed/header layout"
        );

        let wir = assemble_wir_module(&module)
            .expect_lowered("closed generic helper specializes its packed signature");
        let wat = witchy_wir::wir::to_wat(&wir);
        assert!(wat.contains("__witchy_packed_list_"), "packed constructor retained: {wat}");
        assert!(
            // The linked fixture module-qualifies the callable; the packed
            // composite physical instance is `app.relay` specialized on Point.
            wat.contains("call $app.relay__app_2ePoint__phys0"),
            "the call selects the composite physical generic instance: {wat}"
        );
    }

    #[test]
    fn declared_packed_list_push_preserves_layout_and_counts_growth() {
        let list_module = parse_module(
            witchy_syntax::linker::bundled_source("list")
                .expect("bundled list module"),
        )
        .expect("parse descriptor list mutation module");
        let mutation_app = parse_module(r#"
mode opt
import list
type Point packed:
    x: Int
fn main() -> Int:
    var points = []
    list.push(points, Point(1))
    list.push(points, Point(2))
    list.push(points, Point(3))
    list.push(points, Point(4))
    list.length(points) * 1000
        + list.at(points, 0).x * 100
        + list.at(points, 1).x * 10
        + list.at(points, 2).x
        + list.at(points, 3).x
"#).expect("parse descriptor list mutation");
        let mutation = link_test_modules(
            vec![("list".into(), list_module), ("app".into(), mutation_app)],
            "app",
            &std::collections::HashSet::from(["app".to_string()]),
        );
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
        ];
        let (result, counters) = run_int_module_with_i64_globals(&mutation, &names);
        assert_eq!(result, 4127);
        // Empty buffer: 8 bytes. First push grows capacity to two (24 bytes),
        // the second reuses slack, the third grows to six (56 bytes), and the
        // fourth reuses slack: 3 descriptor allocations, 88 logical bytes.
        assert_eq!(counters["__witchy_packed_alloc_calls"], 3);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 88);

        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&mutation).expect_lowered("descriptor list mutation lowers"),
        );
        assert!(wat.contains("__witchy_packed_list_push_"), "descriptor push helper is reachable: {wat}");
        assert!(wat.contains("memory.copy"), "growth copies packed bytes at descriptor stride: {wat}");
        assert!(!wat.contains("call $list_push_cap"), "legacy slot push is absent: {wat}");
        assert!(!wat.contains("call $mk1"), "packed elements are never boxed: {wat}");
        if witchy_wir::wir_helpers::heap_check_enabled() {
            assert!(wat.contains("call $heap_register"), "growth allocation registers its redzone: {wat}");
        }
    }

    #[test]
    fn confined_counted_packed_list_streams_exact_storage_and_cursor() {
        let source = r#"
mode opt
import list

type Point packed:
    x: Int
    y: Int

fn answer() -> Int:
    var points = []
    for i in 0..9:
        list.push(points, Point(i, i * 3))
    var total = 0
    for point in points:
        total = total + point.x * 7 + point.y
    total * 100 + list.length(points)

fn main(console: Console):
    console.print("${answer()}")
"#;
        let module = link_list_app(source);
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
        ];
        let bytes = compile_module_binary(&module).expect_lowered("compile confined packed stream");
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("run export")
            .call(&mut store, ())
            .expect("run confined packed stream");
        let compiled = captured.lock().unwrap().clone();
        assert_eq!(compiled, vec!["36009".to_string()]);
        let counters: std::collections::HashMap<&str, i64> = names
            .iter()
            .map(|&name| {
                let value = instance
                    .get_global(&mut store, name)
                    .unwrap_or_else(|| panic!("missing counter `{name}`"))
                    .get(&mut store);
                let wasmtime::Val::I64(value) = value else {
                    panic!("counter `{name}` is not i64")
                };
                (name, value)
            })
            .collect();
        assert_eq!(counters["__witchy_packed_alloc_calls"], 1);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 152);

        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("confined packed stream lowers"),
        );
        let start = wat.find("(func $app.answer").expect("linked answer function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let answer = &tail[..end];
        assert_eq!(answer.matches("call $rc_alloc").count(), 1, "one exact reservation: {answer}");
        assert!(
            !answer.contains("call $__witchy_packed_list_push_")
                && !answer.contains("call $__witchy_packed_record_"),
            "the builder writes both descriptor fields directly: {answer}"
        );
        assert!(
            answer.contains("local.set $__forptr_point")
                && answer.contains("local.set $__forendptr_point")
                && !answer.contains("local.get $__fori_point"),
            "the packed consumer walks a stride-aware pointer cursor: {answer}"
        );
        assert_eq!(
            answer.matches("local.set $point\n").count(),
            4,
            "the enabled loop-unroll lever emits four packed cursor lanes: {answer}"
        );
        if witchy_wir::wir_helpers::heap_check_enabled() {
            assert!(answer.contains("call $heap_register"), "exact packed storage is checked: {answer}");
        }

        let unroll_off = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::LoopUnroll);
        witchy_syntax::opt::set_for_tests(Some(unroll_off));
        let scalar_module = link_list_app(source);
        let scalar_bytes = compile_module_binary(&scalar_module)
            .expect_lowered("compile scalar packed cursor");
        let scalar_wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&scalar_module).expect_lowered("scalar packed cursor lowers"),
        );
        witchy_syntax::opt::set_for_tests(None);
        let (mut scalar_store, scalar_instance, scalar_captured) =
            instantiate_with_print(&scalar_bytes);
        scalar_instance
            .get_typed_func::<(), ()>(&mut scalar_store, "run")
            .expect("scalar cursor run export")
            .call(&mut scalar_store, ())
            .expect("run scalar packed cursor");
        assert_eq!(*scalar_captured.lock().unwrap(), vec!["36009".to_string()]);
        let start = scalar_wat
            .find("(func $app.answer")
            .expect("scalar linked answer function");
        let tail = &scalar_wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let scalar_answer = &tail[..end];
        assert_eq!(
            scalar_answer.matches("local.set $point\n").count(),
            1,
            "-loop-unroll emits one packed cursor lane: {scalar_answer}"
        );

        let deopt = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::BoundsElide);
        witchy_syntax::opt::set_for_tests(Some(deopt));
        let deopt_module = link_list_app(source);
        let deopt_bytes = compile_module_binary(&deopt_module)
            .expect_lowered("compile packed stream deopt");
        let deopt_wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&deopt_module).expect_lowered("packed stream deopt lowers"),
        );
        witchy_syntax::opt::set_for_tests(None);
        let (mut deopt_store, deopt_instance, deopt_captured) =
            instantiate_with_print(&deopt_bytes);
        deopt_instance
            .get_typed_func::<(), ()>(&mut deopt_store, "run")
            .expect("deopt run export")
            .call(&mut deopt_store, ())
            .expect("run packed stream deopt");
        assert_eq!(*deopt_captured.lock().unwrap(), vec!["36009".to_string()]);
        let deopt_allocations = deopt_instance
            .get_global(&mut deopt_store, "__witchy_packed_alloc_calls")
            .expect("deopt packed allocation counter")
            .get(&mut deopt_store);
        let wasmtime::Val::I64(deopt_allocations) = deopt_allocations else {
            panic!("deopt packed allocation counter is not i64")
        };
        assert_eq!(deopt_allocations, 1);
        assert!(
            deopt_wat.contains("local.get $__fori_point"),
            "disabling bounds elision keeps indexed packed traversal: {deopt_wat}"
        );
    }

    #[test]
    fn declared_packed_list_push_copies_at_an_alias_dirty_site() {
        let list_module = parse_module(
            witchy_syntax::linker::bundled_source("list")
                .expect("bundled list module"),
        )
        .expect("parse list module");
        let app = parse_module(r#"
mode opt
import list
type Point packed:
    x: Int
fn main() -> Int:
    var points = [Point(1)]
    let alias = points
    list.push(points, Point(2))
    list.length(alias) * 1000
        + list.length(points) * 100
        + list.at(alias, 0).x * 10
        + list.at(points, 1).x
"#).expect("parse alias-dirty descriptor push");
        let module = link_test_modules(
            vec![("list".into(), list_module), ("app".into(), app)],
            "app",
            &std::collections::HashSet::from(["app".to_string()]),
        );
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
        ];
        let (result, counters) = run_int_module_with_i64_globals(&module, &names);
        assert_eq!(result, 1212, "the alias retains the old one-element value");
        assert_eq!(counters["__witchy_packed_alloc_calls"], 2);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 56);
    }

    #[test]
    fn declared_packed_closed_sum_matches_pinned_oracle_without_adapters() {
        let source = r#"
mode opt

type Token packed:
    Skip
    Value(Int)

fn make(i: Int) -> Token:
    if i % 3 == 0: Skip else: Value((i * 7 + 3) % 101)

fn score(token: Token) -> Int:
    match token:
        Skip -> 1
        Value(value) -> value

fn main() -> Int:
    var total = 0
    for i in 0..500000:
        total = total + score(make(i))
    total
"#;
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_destination_candidates_forwarded",
            "__witchy_region_rewind_calls",
        ];
        let (result, counters) = run_int_with_i64_globals(source, &names);
        assert_eq!(result, 16833142);
        assert_eq!(counters["__witchy_packed_alloc_calls"], 1);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 16);
        assert_eq!(counters["__witchy_destination_candidates_forwarded"], 500_000);
        assert_eq!(counters["__witchy_region_rewind_calls"], 0);

        let module = parse_module(source).expect("parse packed closed-sum oracle");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("packed closed sum lowers"),
        );
        assert!(
            !wat.contains("__witchy_packed_sum_destination_"),
            "destination-forwarded variants store their canonical descriptor inline: {wat}"
        );
        assert!(wat.contains("call $make"), "packed sum crosses a direct result boundary: {wat}");
        assert!(wat.contains("call $score"), "packed sum crosses a direct parameter boundary: {wat}");
        assert!(wat.contains("i32.load8_u"), "match dispatch reads the descriptor tag width: {wat}");
        assert!(!wat.contains("call $mk1"), "payload is never boxed into a legacy record: {wat}");
        let make_start = wat.find("(func $make").expect("make function");
        let make_tail = &wat[make_start..];
        let make_end = make_tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(make_tail.len());
        let make = &make_tail[..make_end];
        assert!(
            make.contains("i32.store8") && make.contains("i64.store offset=8"),
            "the producer writes the canonical tag and payload inline: {make}"
        );
        let start = wat.find("(func $main").expect("main function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let main = &tail[..end];
        assert_eq!(
            main.matches("call $__witchy_destination_scratch_").count(),
            1,
            "one caller scratch allocation is initialized before the hot loop: {main}"
        );
        assert_eq!(
            main.matches("global.set $__witchy_destination_candidates_forwarded").count(),
            1,
            "the exact forwarded total is committed once after the hot loop: {main}"
        );
        assert!(
            main.contains("local.set $__witchy_counter_batch_destination_0"),
            "the hot loop aggregates its destination count in a local: {main}"
        );
        assert!(
            !main.contains("global.set $__witchy_region_rewind_calls"),
            "a fully destination-forwarded transient sum needs no loop watermark: {main}"
        );
        assert!(
            !main.contains("global.set $heap"),
            "the destination-forwarded hot loop does not rewind the heap: {main}"
        );
        assert!(!main.contains("call $rc_alloc"), "the hot caller has no direct allocator: {main}");
        if witchy_wir::wir_helpers::heap_check_enabled() {
            assert!(wat.contains("call $heap_register"), "sum allocations register checked redzones: {wat}");
        }
    }

    #[test]
    fn packed_closed_sum_equality_uses_descriptor_payload_offsets() {
        let source = r#"
mode opt

type Token packed derive(PartialEq):
    Empty
    Pair(Bool, Int)
    Other(Int)

fn same(a: Token, b: Token) -> Bool:
    a == b

fn different(a: Token, b: Token) -> Bool:
    a != b

fn main() -> Int:
    if same(Pair(true, 7), Pair(true, 7)) && different(Pair(false, 7), Pair(true, 7)) && !same(Pair(true, 8), Pair(true, 7)) && same(Empty, Empty) && different(Pair(true, 7), Other(7)):
        1
    else:
        0
"#;
        assert_eq!(run_int(source), 1);

        let module = parse_module(source).expect("parse packed closed-sum equality");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module)
                .expect_lowered("packed closed-sum equality lowers from its descriptor"),
        );
        let start = wat
            .find("(func $__witchy_layout_eq_")
            .expect("descriptor equality helper");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let helper = &tail[..end];
        assert!(helper.contains("i32.load8_u"), "the descriptor's byte tag/bool loads are used: {helper}");
        assert!(helper.contains("i32.const 8"), "the padded Bool payload offset comes from VariantLayout: {helper}");
        assert!(helper.contains("i32.const 16"), "the aligned Int payload offset comes from VariantLayout: {helper}");
        assert!(helper.contains("i64.load"), "the Int child descriptor selects an i64 load: {helper}");
        assert!(!helper.contains("i32.const 4\n"), "legacy 4+8*i enum slots are not used: {helper}");
    }

    #[test]
    fn packed_closed_sum_equality_uses_descriptor_tag16_width() {
        let mut source = String::from("mode opt\n\ntype Wide packed derive(PartialEq):\n");
        for variant in 0..257 {
            source.push_str(&format!("    V{variant}\n"));
        }
        source.push_str(
            "\nfn same(a: Wide, b: Wide) -> Bool:\n    a == b\n\nfn main() -> Int:\n    if same(V256, V256): 1 else: 0\n",
        );
        let module = parse_module(&source).expect("parse Tag16 packed closed-sum equality");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module)
                .expect_lowered("Tag16 packed closed-sum equality lowers from its descriptor"),
        );
        let start = wat
            .find("(func $__witchy_layout_eq_")
            .expect("descriptor Tag16 equality helper");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let helper = &tail[..end];
        assert!(
            helper.contains("i32.load8_u offset=1") && helper.contains("i32.shl"),
            "Tag16 equality reconstructs the descriptor's two-byte tag: {helper}"
        );
        assert!(!helper.contains("i32.load\n"), "Tag16 equality does not use a legacy i32 tag load: {helper}");
    }

    #[test]
    fn packed_closed_sum_with_nested_custom_equality_rejects_loudly() {
        let source = r#"
mode opt

type Inner packed:
    id: Int
    noise: Int

impl PartialEq for Inner:
    fn eq(self, other: Inner) -> Bool:
        self.id == other.id

type Outer packed derive(PartialEq):
    SomeInner(Inner)
    Empty

fn main() -> Int:
    if SomeInner(Inner(7, 1)) == SomeInner(Inner(7, 99)):
        1
    else:
        0
"#;
        let module = parse_module(source).expect("parse nested custom packed equality");
        let error = compile_module_binary(&module)
            .expect_rejected("descriptor equality cannot erase nested custom semantics");
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("declared packed layout")
                && diagnostic.contains("aggregate binary operation"),
            "nested custom equality must reject instead of comparing structurally: {diagnostic}"
        );
    }

    #[test]
    fn escaping_packed_sum_result_keeps_the_allocating_fallback() {
        let source = r#"
mode opt

type Token packed:
    Skip
    Value(Int)

fn make(value: Int) -> Token:
    if value % 2 == 0: Skip else: Value(value)

fn escape(token: Token) -> Token:
    token

fn score(token: Token) -> Int:
    match token:
        Skip -> 1
        Value(value) -> value

fn main() -> Int:
    var total = 0
    for i in 0..6:
        total = total + score(escape(make(i)))
    total
"#;
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_destination_candidates_forwarded",
        ];
        let (result, counters) = run_int_with_i64_globals(source, &names);
        assert_eq!(result, 12);
        assert_eq!(counters["__witchy_packed_alloc_calls"], 6);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 96);
        assert_eq!(counters["__witchy_destination_candidates_forwarded"], 0);
    }

    #[test]
    fn confined_local_closed_sum_is_scalar_replaced_through_match() {
        let source = r#"
mode opt

type Token packed:
    Skip
    Value(Int)

fn answer() -> Int:
    var total = 0
    for i in 0..500000:
        let token = if i % 3 == 0: Skip else: Value((i * 7 + 3) % 101)
        match token:
            Skip -> total = total + 1
            Value(value) -> total = total + value
    total

fn main(console: Console):
    console.print("${answer()}")
"#;
        let module = parse_module(source).expect("parse confined closed sum");
        let bytes = compile_module_binary(&module)
            .expect_lowered("compiled backend lowers confined closed sum");
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("run export")
            .call(&mut store, ())
            .expect("run confined closed sum");
        let compiled = captured.lock().unwrap().clone();
        let oracle = vec!["16833142".to_string()];
        assert_eq!(compiled, oracle, "compiled expected value");

        for name in [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_destination_candidates_forwarded",
            "__witchy_region_rewind_calls",
        ] {
            let value = instance
                .get_global(&mut store, name)
                .unwrap_or_else(|| panic!("missing counter export `{name}`"))
                .get(&mut store);
            let wasmtime::Val::I64(actual) = value else {
                panic!("counter export `{name}` is not i64: {value:?}");
            };
            assert_eq!(actual, 0, "scalar replacement leaves `{name}` untouched");
        }

        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("confined closed sum lowers to WIR"),
        );
        let start = wat.find("(func $answer").expect("answer function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let answer = &tail[..end];
        assert!(
            !answer.contains("$token__witchy_sum_tag")
                && !answer.contains("$token__witchy_sum_payload_0"),
            "an adjacent pure constructor and match need no sum locals: {answer}"
        );
        assert!(
            !answer.contains("call $__witchy_packed_sum_")
                && !answer.contains("i32.load8_u")
                && !answer.contains("i64.load offset=8"),
            "the hot loop has no sum constructor helper or tag/payload loads: {answer}"
        );
        assert!(
            !answer.contains("global.set $__witchy_region_rewind_calls")
                && !answer.contains("global.set $heap"),
            "an allocation-free scalar loop needs no region watermark: {answer}"
        );

        let deopt = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::Sroa);
        witchy_syntax::opt::set_for_tests(Some(deopt));
        let deopt_wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("closed sum deopt lowers"),
        );
        witchy_syntax::opt::set_for_tests(None);
        let deopt_start = deopt_wat.find("(func $answer").expect("deopt answer function");
        let deopt_answer = &deopt_wat[deopt_start..];
        assert!(
            deopt_answer.contains("call $__witchy_packed_sum_")
                && deopt_answer.contains("i32.load8_u"),
            "disabling SROA retains the materialized constructor/match path: {deopt_answer}"
        );
    }

    #[test]
    fn nonadjacent_confined_closed_sum_keeps_scalar_dispatch_fallback() {
        let source = r#"
mode opt

type Token packed:
    Skip
    Value(Int)

fn main() -> Int:
    var total = 0
    for i in 0..6:
        let token = if i % 2 == 0: Skip else: Value(i)
        total = total + 0
        match token:
            Skip -> total = total + 1
            Value(value) -> total = total + value
    total
"#;
        assert_eq!(run_int(source), 12);

        let module = parse_module(source).expect("parse nonadjacent closed sum");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("nonadjacent closed sum lowers to WIR"),
        );
        assert!(
            wat.contains("$token__witchy_sum_tag")
                && wat.contains("$token__witchy_sum_payload_0"),
            "an intervening statement retains scalar tag/payload dispatch: {wat}"
        );
        assert!(
            !wat.contains("call $__witchy_packed_sum_") && !wat.contains("i32.load8_u"),
            "the fallback remains allocation-free scalar SROA: {wat}"
        );
    }

    #[test]
    fn aliased_local_closed_sum_keeps_materialized_fallback() {
        let source = r#"
mode opt

type Token packed:
    Skip
    Value(Int)

fn main() -> Int:
    var total = 0
    for i in 0..6:
        let token = if i % 2 == 0: Skip else: Value(i)
        let alias = token
        match alias:
            Skip -> total = total + 1
            Value(value) -> total = total + value
    total
"#;
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_destination_candidates_forwarded",
            "__witchy_region_rewind_calls",
        ];
        let (result, counters) = run_int_with_i64_globals(source, &names);
        assert_eq!(result, 12);
        assert_eq!(counters["__witchy_packed_alloc_calls"], 6);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 96);
        assert_eq!(counters["__witchy_destination_candidates_forwarded"], 0);
        assert_eq!(counters["__witchy_region_rewind_calls"], 6);

        let module = parse_module(source).expect("parse aliased closed sum");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("aliased closed sum lowers to WIR"),
        );
        assert!(
            !wat.contains("$token__witchy_sum_tag"),
            "a whole-value alias disqualifies scalar replacement: {wat}"
        );
        assert!(
            wat.contains("call $__witchy_packed_sum_") && wat.contains("i32.load8_u"),
            "the alias-safe fallback materializes and dispatches the sum: {wat}"
        );
    }

    #[test]
    fn scalar_closed_sum_with_allocating_payload_keeps_loop_watermark() {
        let source = r#"
mode opt

type Point packed:
    x: Int

type Choice packed:
    Empty
    PointValue(Point)

fn main() -> Int:
    var total = 0
    for i in 0..6:
        let choice = if i % 2 == 0: Empty else: PointValue(Point(i))
        match choice:
            Empty -> total = total + 1
            PointValue(point) -> total = total + point.x
    total
"#;
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_destination_candidates_forwarded",
            "__witchy_region_rewind_calls",
        ];
        let (result, counters) = run_int_with_i64_globals(source, &names);
        assert_eq!(result, 12);
        assert_eq!(counters["__witchy_packed_alloc_calls"], 3);
        assert_eq!(counters["__witchy_destination_candidates_forwarded"], 0);
        assert_eq!(counters["__witchy_region_rewind_calls"], 6);

        let module = parse_module(source).expect("parse scalar sum with nested payload");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module)
                .expect_lowered("scalar sum with nested payload lowers to WIR"),
        );
        assert!(wat.contains("$choice__witchy_sum_tag"), "the outer sum is scalarized: {wat}");
        assert!(
            wat.contains("global.set $__witchy_region_rewind_calls")
                && wat.contains("global.set $heap"),
            "the nested Point allocation keeps per-iteration reclamation: {wat}"
        );
    }

    #[test]
    fn confined_literal_counted_list_builder_reserves_and_stores_directly() {
        let source = r#"
mode opt
import list

fn answer() -> Int:
    var values = []
    for i in 5..14:
        list.push(values, i * 3)
    var total = 0
    for value in values:
        total = total + value
    total * 100 + list.length(values)

fn main(console: Console):
    console.print("${answer()}")
"#;
        let module = link_list_app(source);
        let bytes = compile_module_binary(&module)
            .expect_lowered("compiled backend lowers direct list builder");
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("run export")
            .call(&mut store, ())
            .expect("run direct list builder");
        let compiled = captured.lock().unwrap().clone();
        let oracle = vec!["24309".to_string()];
        assert_eq!(compiled, oracle, "compiled expected value");

        for name in [
            "__witchy_reowns",
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_region_rewind_calls",
        ] {
            let value = instance
                .get_global(&mut store, name)
                .unwrap_or_else(|| panic!("missing counter export `{name}`"))
                .get(&mut store);
            let wasmtime::Val::I64(actual) = value else {
                panic!("counter export `{name}` is not i64: {value:?}");
            };
            assert_eq!(actual, 0, "direct builder leaves `{name}` untouched");
        }

        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("direct list builder lowers to WIR"),
        );
        let start = wat.find("(func $app.answer").expect("linked answer function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let answer = &tail[..end];
        assert_eq!(answer.matches("call $rc_alloc").count(), 1, "one exact reservation: {answer}");
        assert!(
            !answer.contains("call $mk0") && !answer.contains("call $list_push_cap"),
            "the direct builder has neither empty allocation nor growth helper: {answer}"
        );
        assert!(
            answer.contains("i64.store") && answer.contains("i32.store"),
            "the loop writes scalar slots and commits the final length: {answer}"
        );
        assert!(
            answer.contains("local.set $__forptr_value")
                && answer.contains("local.set $__forendptr_value")
                && !answer.contains("local.get $__fori_value"),
            "the read-only consumer walks a hoisted pointer/end pair: {answer}"
        );
        assert_eq!(
            answer.matches("local.set $value\n").count(),
            4,
            "the scalar pointer loop is unrolled four-wide with guarded lanes: {answer}"
        );
        if witchy_wir::wir_helpers::heap_check_enabled() {
            assert!(answer.contains("call $heap_register"), "the exact reservation is checked: {answer}");
        }
    }

    #[test]
    fn aliased_list_builder_keeps_growth_fallback() {
        let source = r#"
mode opt
import list

fn main() -> Int:
    var values = []
    let alias = values
    for i in 0..4:
        list.push(values, i + 1)
    list.length(alias) * 100 + list.length(values) * 10 + list.at(values, 3)
"#;
        let names = [
            "__witchy_reowns",
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
        ];
        let module = link_list_app(source);
        let (result, counters) = run_int_module_with_i64_globals(&module, &names);
        assert_eq!(result, 44);
        assert_eq!(counters["__witchy_reowns"], 1);
        assert_eq!(counters["__witchy_packed_alloc_calls"], 0);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 0);

        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("aliased list builder lowers to WIR"),
        );
        assert!(wat.contains("call $mk0") && wat.contains("call $list_push_cap"), "fallback grows normally: {wat}");
    }

    #[test]
    fn list_iteration_that_reads_its_source_keeps_indexed_fallback() {
        let source = r#"
mode opt

fn main() -> Int:
    let values = [1, 2, 3]
    var total = 0
    for value in values:
        total = total + value + list.length(values)
    total
"#;
        assert_eq!(run_int(source), 15);
        let module = parse_module(source).expect("parse source-reading list iteration");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module)
                .expect_lowered("source-reading list iteration lowers to WIR"),
        );
        assert!(
            wat.contains("local.get $__fori_value")
                && !wat.contains("local.set $__forendptr_value"),
            "an observable source read keeps indexed iteration: {wat}"
        );
    }

    #[test]
    fn owned_consumer_result_cannot_alias_reused_destination_scratch() {
        let source = r#"
mode opt

type Token packed:
    Skip
    Value(Int)

fn make(value: Int) -> Token:
    if value % 2 == 0:
        Skip
    else:
        Value(value)

fn pass(own token: Token) -> Token:
    token

fn score(let token: Token) -> Int:
    match token:
        Skip -> 9
        Value(value) -> value

fn main(console: Console):
    var held = pass(make(1))
    var total = 0
    for i in 2..5:
        let previous = held
        held = pass(make(i))
        total = total * 10 + score(previous)
    let result = total * 10 + score(held)
    console.print("${result}")
"#;
        let module = parse_module(source).expect("parse own-consumer alias regression");
        let bytes = compile_module_binary(&module)
            .expect_lowered("compiled backend lowers own-consumer alias regression");
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("run export")
            .call(&mut store, ())
            .expect("run own-consumer alias regression");
        let compiled = captured.lock().unwrap().clone();
        let oracle = vec!["1939".to_string()];
        assert_eq!(compiled, oracle, "compiled expected value");

        for (name, expected) in [
            ("__witchy_packed_alloc_calls", 4),
            ("__witchy_packed_alloc_bytes", 64),
            ("__witchy_destination_candidates_forwarded", 0),
        ] {
            let value = instance
                .get_global(&mut store, name)
                .unwrap_or_else(|| panic!("missing counter export `{name}`"))
                .get(&mut store);
            let wasmtime::Val::I64(actual) = value else {
                panic!("counter export `{name}` is not i64: {value:?}");
            };
            assert_eq!(actual, expected, "counter `{name}`");
        }

        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module)
                .expect_lowered("own-consumer alias regression lowers to WIR"),
        );
        assert!(
            !wat.contains("__witchy_destination_scratch_"),
            "an own consumer that returns its argument must allocate each producer result: {wat}"
        );
        if witchy_wir::wir_helpers::heap_check_enabled() {
            assert!(
                wat.contains("call $heap_register"),
                "each allocating fallback retains checked-heap registration: {wat}"
            );
        }
    }

    #[test]
    fn nested_packed_sum_result_declines_destination_forwarding() {
        let source = r#"
mode opt

type Point packed:
    x: Int
    y: Int

type Choice packed:
    Empty
    PointValue(Point)

fn make(value: Int) -> Choice:
    if value % 2 == 0:
        Empty
    else:
        PointValue(Point(value, value + 10))

fn score(choice: Choice) -> Int:
    match choice:
        Empty -> 1
        PointValue(point) -> point.x * 10 + point.y

fn main() -> Int:
    var total = 0
    for i in 0..4:
        total = total + score(make(i))
    total
"#;
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_destination_candidates_forwarded",
        ];
        let (result, counters) = run_int_with_i64_globals(source, &names);
        assert_eq!(result, 66);
        assert_eq!(counters["__witchy_packed_alloc_calls"], 6);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 128);
        assert_eq!(counters["__witchy_destination_candidates_forwarded"], 0);
        let module = parse_module(source).expect("parse nested destination exclusion");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module)
                .expect_lowered("nested fixed results retain the allocating fallback"),
        );
        assert!(
            !wat.contains("__witchy_destination_scratch_")
                && !wat.contains("__witchy_packed_sum_destination_"),
            "no destination claim is made until nested children construct in place: {wat}"
        );
    }

    #[test]
    fn unique_packed_result_forwards_an_exact_dead_destination() {
        let source = r#"
mode opt

type Pair packed:
    left: Int
    right: Int

fn build(value: Int) -> unique Pair:
    Pair(value * 7 + 3, value * 11 + 5)

fn main() -> Int:
    var current = build(0)
    var total = 0
    for i in 1..1000000:
        current = build(i)
        total = total + (current.left + current.right) % 101
    total + current.left % 17
"#;
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_destination_candidates_forwarded",
        ];
        let (result, counters) = run_int_with_i64_globals(source, &names);
        assert_eq!(result, 49_999_959);
        assert_eq!(counters["__witchy_packed_alloc_calls"], 0);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 0);
        assert_eq!(counters["__witchy_destination_candidates_forwarded"], 999_999);

        let module = parse_module(source).expect("parse destination-forwarding oracle");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("destination oracle lowers"),
        );
        let start = wat.find("(func $main").expect("main function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let main = &tail[..end];
        assert!(
            main.contains("call $build$scalar_result"),
            "loop calls the scalar-result companion: {main}"
        );
        assert_eq!(
            main.matches("global.set $__witchy_destination_candidates_forwarded").count(),
            1,
            "the exact forwarded total is committed once after the hot loop: {main}"
        );
        assert!(
            main.contains("local.set $__witchy_counter_batch_destination_0"),
            "the hot loop aggregates its destination count in a local: {main}"
        );
        assert!(
            !main.contains("call $rc_alloc")
                && !main.contains("i64.load")
                && !main.contains("i64.store"),
            "the hot caller keeps both record fields in locals: {main}"
        );
        assert!(
            !main.contains("call $__witchy_packed_record_destination_"),
            "the constructor remains behind the direct build boundary: {main}"
        );
        assert!(
            !wat.contains("__witchy_packed_record_destination_"),
            "the builder stores its canonical descriptor fields inline: {wat}"
        );
        let build_start = wat.find("(func $build (").expect("build function");
        let build_tail = &wat[build_start..];
        let build_end = build_tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(build_tail.len());
        let build = &build_tail[..build_end];
        assert!(
            build.contains("i64.store") && build.contains("i64.store offset=8"),
            "the producer writes both canonical record fields inline: {build}"
        );
        if witchy_wir::wir_helpers::heap_check_enabled() {
            assert!(
                wat.contains("call $heap_register"),
                "the zero-destination fallback remains checked: {wat}"
            );
        }

        let deopt = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::Sroa);
        witchy_syntax::opt::set_for_tests(Some(deopt));
        let (deopt_result, deopt_counters) = run_int_with_i64_globals(source, &names);
        assert_eq!(deopt_result, 49_999_959);
        assert_eq!(deopt_counters["__witchy_packed_alloc_calls"], 1);
        assert_eq!(deopt_counters["__witchy_packed_alloc_bytes"], 16);
        assert_eq!(
            deopt_counters["__witchy_destination_candidates_forwarded"],
            999_999
        );
        let deopt_module = parse_module(source).expect("parse scalar-result deopt oracle");
        let deopt_wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&deopt_module).expect_lowered("scalar-result deopt lowers"),
        );
        witchy_syntax::opt::set_for_tests(None);
        let deopt_main = &deopt_wat[deopt_wat.find("(func $main").expect("deopt main")..];
        assert!(
            !deopt_main.contains("call $build$scalar_result"),
            "disabling SROA keeps the destination-pointer call: {deopt_main}"
        );
    }

    #[test]
    fn unique_packed_destination_requires_complete_constructor_returns() {
        let complete = r#"
mode opt

type Pair packed:
    left: Int
    right: Int

fn choose(flag: Bool, value: Int) -> unique Pair:
    if flag:
        Pair(value, value + 1)
    else:
        Pair(value + 100, value + 1)

fn main() -> Int:
    var current = choose(true, 0)
    var total = 0
    for i in 1..6:
        current = choose(i % 2 == 0, i)
        total = total + current.left + current.right
    total
"#;
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_destination_candidates_forwarded",
        ];
        let (result, counters) = run_int_with_i64_globals(complete, &names);
        assert_eq!(result, 335);
        assert_eq!(
            counters["__witchy_packed_alloc_calls"],
            1,
            "only the initial live destination is allocated; each replacement reuses it"
        );
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 16);
        assert_eq!(counters["__witchy_destination_candidates_forwarded"], 5);

        let module = parse_module(complete).expect("parse complete destination returns");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("complete destination returns lower"),
        );
        let choose_start = wat.find("(func $choose (").expect("choose function");
        let choose_tail = &wat[choose_start..];
        let choose_end = choose_tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(choose_tail.len());
        let choose = &choose_tail[..choose_end];
        assert!(
            choose
                .lines()
                .next()
                .is_some_and(|signature| signature.contains("(param $__witchy_destination i32)")),
            "complete constructor returns must select the destination ABI: {choose}"
        );

        let early_return = r#"
mode opt

type Pair packed:
    left: Int
    right: Int

fn choose(flag: Bool, value: Int) -> unique Pair:
    if flag:
        return Pair(value, value + 1)
    Pair(value + 100, value + 1)

fn main() -> Int:
    var pair = choose(true, 7)
    pair = choose(false, 8)
    pair.left + pair.right
"#;
        let (result, counters) = run_int_with_i64_globals(early_return, &names);
        assert_eq!(result, 117);
        assert_eq!(counters["__witchy_packed_alloc_calls"], 2);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 32);
        assert_eq!(counters["__witchy_destination_candidates_forwarded"], 0);

        let module = parse_module(early_return).expect("parse early-return destination fallback");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("early-return destination fallback lowers"),
        );
        let choose_start = wat.find("(func $choose (").expect("early choose function");
        let choose_tail = &wat[choose_start..];
        let choose_end = choose_tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(choose_tail.len());
        let choose = &choose_tail[..choose_end];
        assert!(
            !choose
                .lines()
                .next()
                .is_some_and(|signature| signature.contains("(param $__witchy_destination i32)"))
                && !choose.contains("local.get $__witchy_destination"),
            "mixed early-return control flow must decline the destination ABI: {choose}"
        );
    }

    #[test]
    fn unique_packed_result_allocates_when_the_old_value_escapes() {
        let source = r#"
mode opt

type Pair packed:
    left: Int
    right: Int

fn build(value: Int) -> unique Pair:
    Pair(value, value + 1)

fn main() -> Int:
    var current = build(7)
    let alias = current
    current = build(11)
    alias.left * 1000 + current.right
"#;
        let names = [
            "__witchy_packed_alloc_calls",
            "__witchy_packed_alloc_bytes",
            "__witchy_destination_candidates_forwarded",
        ];
        let (result, counters) = run_int_with_i64_globals(source, &names);
        assert_eq!(result, 7_012, "the alias retains the pre-assignment value");
        assert_eq!(counters["__witchy_packed_alloc_calls"], 2);
        assert_eq!(counters["__witchy_packed_alloc_bytes"], 32);
        assert_eq!(counters["__witchy_destination_candidates_forwarded"], 0);

        let module = parse_module(source).expect("parse escaping destination fallback");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("escaping destination fallback lowers"),
        );
        let start = wat.find("(func $main").expect("main function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let main = &tail[..end];
        assert!(
            main.contains("call $build")
                && !main.contains("call $build$scalar_result")
                && main.contains("i64.load"),
            "an observable alias keeps the allocating pointer ABI: {main}"
        );
    }

    #[test]
    fn allocation_free_loop_omits_region_watermark_through_nested_calls() {
        let source = r#"
mode opt

fn leaf(value: Int) -> Int:
    value * 7 + 3

fn middle(value: Int) -> Int:
    leaf(value)

fn main() -> Int:
    let anchor = [1]
    var total = 0
    for i in 0..1000:
        total = total + middle(i) % 13
    total + list.length(anchor) - 1
"#;
        let (result, counters) =
            run_int_with_i64_globals(source, &["__witchy_region_rewind_calls"]);
        assert_eq!(result, 5_997);
        assert_eq!(counters["__witchy_region_rewind_calls"], 0);

        let module = parse_module(source).expect("parse allocation-free loop");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("allocation-free loop lowers"),
        );
        let start = wat.find("(func $main").expect("main function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let main = &tail[..end];
        assert!(main.contains("call $middle"), "nested scalar call remains direct: {main}");
        assert!(
            !main.contains("global.set $__witchy_region_rewind_calls"),
            "allocation-free loop has no per-iteration reset counter: {main}"
        );
        assert!(
            !main.contains("global.get $heap"),
            "allocation-free loop has no watermark capture: {main}"
        );
        assert_eq!(
            main.matches("local.set $i").count(),
            4,
            "eligible counted range has four statically emitted lanes: {main}"
        );
        assert_eq!(
            main.matches("br_if $fe").count(),
            1,
            "an exact literal lane multiple needs one group guard: {main}"
        );
        assert!(
            main.contains("i64.const 4\n    i64.add\n    local.set $__forctr_i"),
            "the exact group advances its counter once by four: {main}"
        );
    }

    #[test]
    fn counted_range_unroll_preserves_remainders_and_extreme_inclusive_bounds() {
        let source = r#"
mode opt

fn sum(lo: Int, hi: Int) -> Int:
    var total = 0
    for i in lo..hi:
        total = total + i
    total

fn count_inclusive(lo: Int, hi: Int) -> Int:
    var total = 0
    for i in lo..=hi:
        total = total + 1
    total

fn main() -> Int:
    var errors = 0
    errors = errors + sum(-3, -5) * sum(-3, -5)
    errors = errors + sum(-3, -3) * sum(-3, -3)
    errors = errors + (sum(-3, -2) + 3) * (sum(-3, -2) + 3)
    errors = errors + (sum(-3, 0) + 6) * (sum(-3, 0) + 6)
    errors = errors + (sum(-3, 2) + 5) * (sum(-3, 2) + 5)
    errors = errors + (sum(0, 7) - 21) * (sum(0, 7) - 21)
    errors = errors + (sum(0, 8) - 28) * (sum(0, 8) - 28)
    errors = errors + count_inclusive(3, 2) * count_inclusive(3, 2)
    errors = errors + (count_inclusive(3, 3) - 1) * (count_inclusive(3, 3) - 1)
    errors = errors + (count_inclusive(3, 6) - 4) * (count_inclusive(3, 6) - 4)
    errors = errors + (count_inclusive(9223372036854775805, 9223372036854775807) - 3) * (count_inclusive(9223372036854775805, 9223372036854775807) - 3)
    errors
"#;
        assert_eq!(run_int(source), 0);

        let module = parse_module(source).expect("parse counted-range property cases");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("counted-range properties lower"),
        );
        let start = wat.find("(func $sum").expect("sum function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let sum = &tail[..end];
        assert_eq!(
            sum.matches("local.set $i").count(),
            4,
            "safe dynamic range lowers to four guarded lanes: {sum}"
        );
        assert_eq!(
            sum.matches("br_if $fe").count(),
            4,
            "a dynamic range retains one remainder guard per lane: {sum}"
        );

        let unroll_off = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::LoopUnroll);
        witchy_syntax::opt::set_for_tests(Some(unroll_off));
        let deopt_result = run_int(source);
        let deopt_module = parse_module(source).expect("parse de-opt counted-range cases");
        let deopt_wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&deopt_module)
                .expect_lowered("de-opt counted ranges retain scalar lowering"),
        );
        witchy_syntax::opt::set_for_tests(None);
        assert_eq!(deopt_result, 0, "-loop-unroll preserves the oracle");
        let start = deopt_wat.find("(func $sum").expect("de-opt sum function");
        let tail = &deopt_wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let deopt_sum = &tail[..end];
        assert_eq!(
            deopt_sum.matches("local.set $i").count(),
            1,
            "-loop-unroll emits one scalar lane: {deopt_sum}"
        );
        assert_eq!(
            deopt_sum.matches("br_if $fe").count(),
            1,
            "-loop-unroll emits one scalar guard: {deopt_sum}"
        );

        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::none()));
        let none_result = run_int(source);
        witchy_syntax::opt::set_for_tests(None);
        assert_eq!(none_result, 0, "WITCHY_OPT=none disables loop unrolling");
    }

    #[test]
    fn counted_range_with_break_or_continue_declines_unrolling() {
        let source = r#"
mode opt

fn main() -> Int:
    var total = 0
    for i in 0..10:
        if i == 2:
            continue
        if i == 7:
            break
        total = total + i
    total
"#;
        assert_eq!(run_int(source), 19);
        let module = parse_module(source).expect("parse controlled counted range");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("controlled range lowers"),
        );
        let start = wat.find("(func $main").expect("main function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let main = &tail[..end];
        assert_eq!(
            main.matches("local.set $i").count(),
            1,
            "source-level loop control retains the scalar lowering: {main}"
        );
    }

    #[test]
    fn allocating_branch_keeps_region_watermark_through_call_summary() {
        let source = r#"
mode opt

type Cell:
    value: Int

fn maybe_cell(flag: Bool, value: Int) -> Int:
    if flag:
        let cell = Cell(value)
        cell.value
    else:
        value

fn main() -> Int:
    var total = 0
    for i in 0..7:
        total = total + maybe_cell(i % 2 == 0, i)
    total
"#;
        let (result, counters) =
            run_int_with_i64_globals(source, &["__witchy_region_rewind_calls"]);
        assert_eq!(result, 21);
        assert_eq!(counters["__witchy_region_rewind_calls"], 7);

        let module = parse_module(source).expect("parse allocating branch loop");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("allocating branch loop lowers"),
        );
        let start = wat.find("(func $main").expect("main function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let main = &tail[..end];
        assert_eq!(
            main.matches("global.set $__witchy_region_rewind_calls").count(),
            1,
            "the exact rewind total is committed once after the loop: {main}"
        );
        assert!(
            main.contains("local.set $__witchy_counter_batch_rewind_0"),
            "the allocating loop aggregates rewind instrumentation locally: {main}"
        );
    }

    #[test]
    fn unknown_loop_call_keeps_region_watermark() {
        let source = r#"
mode opt

fn main() -> Int:
    let transform = fn(value: Int) -> Int: value + 1
    var total = 0
    for i in 0..5:
        total = total + transform(i)
    total
"#;
        let (result, counters) =
            run_int_with_i64_globals(source, &["__witchy_region_rewind_calls"]);
        assert_eq!(result, 15);
        assert_eq!(counters["__witchy_region_rewind_calls"], 5);

        let module = parse_module(source).expect("parse unknown-call loop");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("unknown-call loop lowers"),
        );
        let start = wat.find("(func $main").expect("main function");
        let tail = &wat[start..];
        let end = tail[1..]
            .find("\n  (func $")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        let main = &tail[..end];
        assert_eq!(
            main.matches("local.set $i").count(),
            1,
            "indirect/unknown effects retain scalar iteration order: {main}"
        );
    }

    #[test]
    fn specialized_region_results_reject_before_uniform_slot_copy() {
        let cases = [
            (
                "padded packed record",
                r#"
mode opt
type Padded packed:
    enabled: Bool
    value: Int
fn main() -> Int:
    let result: Padded = region:
        Padded(true, 41)
    result.value
"#,
                "operation=fields(count=2) size=fixed(16) ownership=none",
            ),
            (
                "packed record list",
                r#"
mode opt
type Point packed:
    x: Int
    y: Int
fn main() -> Int:
    let points = region -> List(Point):
        [Point(1, 2), Point(3, 4)]
    list.at(points, 1).y
"#,
                "operation=packed-elements(",
            ),
            (
                "packed closed sum",
                r#"
mode opt
type Token packed:
    Empty
    Value(Bool, Int)
fn main() -> Int:
    let token = region -> Token:
        Value(true, 7)
    match token:
        Empty -> 0
        Value(_, value) -> value
"#,
                "operation=variants(count=2,tag=tag8) size=fixed(24) ownership=none",
            ),
            (
                "nested packed record",
                r#"
mode opt
type Point packed:
    x: Int
    y: Int
type Nested packed:
    live: Bool
    point: Point
fn main() -> Int:
    let nested = region -> Nested:
        Nested(true, Point(5, 9))
    nested.point.y
"#,
                "operation=fields(count=2) size=fixed(24) ownership=none",
            ),
        ];

        for (name, source, operation_detail) in cases {
            let module = parse_module(source)
                .unwrap_or_else(|error| panic!("parse {name}: {error}"));
            let error = compile_module_binary(&module)
                .expect_rejected(&format!("{name} must not enter uniform-slot copy-out"));
            let diagnostic = error.to_string();
            assert!(
                diagnostic.contains("declared packed LayoutId")
                    && diagnostic.contains("cannot leave `region:`")
                    && diagnostic.contains("legacy uniform-slot copy path")
                    && diagnostic.contains("descriptor-driven region copy")
                    && diagnostic.contains("cannot be boxed or reshaped")
                    && diagnostic.contains(operation_detail),
                "{name}: {diagnostic}",
            );
            if name == "packed record list" {
                assert!(
                    diagnostic.contains("size=dynamic(base=8,stride=16)")
                        && diagnostic.contains("ownership=root-buffer"),
                    "{name}: {diagnostic}",
                );
            }
            let layout = diagnostic
                .split_once("LayoutId ")
                .and_then(|(_, rest)| rest.split_once(' '))
                .map(|(layout, _)| layout)
                .expect("diagnostic carries the exact LayoutId");
            assert_eq!(layout.len(), 64, "{name}: {diagnostic}");
            assert!(
                layout.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "{name}: {diagnostic}",
            );
        }
    }

    #[test]
    fn ordinary_region_result_keeps_legacy_copy_path() {
        let source = r#"
type Point:
    x: Int
    y: Int
fn main() -> Int:
    let point = region -> Point:
        Point(5, 7)
    point.x + point.y
"#;
        assert_eq!(run_int(source), 12);
        let module = parse_module(source).expect("parse ordinary region result");
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module)
                .expect_lowered("ordinary region copy-out remains supported"),
        );
        assert!(wat.contains("rcopy_rec_Point"), "ordinary region copy helper: {wat}");
    }

    #[test]
    fn declared_packed_closed_sum_rejects_variable_layout_payload_loudly() {
        let module = parse_module(r#"
mode opt
type Bad packed:
    Empty
    Text(String)
fn main() -> Int:
    0
"#).expect("parse invalid packed closed sum");
        let error = compile_module_binary(&module)
            .expect_rejected("a String payload has no fixed inline descriptor");
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("packed") && diagnostic.contains("Bad"), "{diagnostic}");
        assert!(diagnostic.contains("non-packable field"), "exact exclusion reason: {diagnostic}");
    }

    #[test]
    fn unsupported_packed_first_class_boundary_rejects_loudly() {

        let first_class = parse_module(r#"
mode opt
type Point packed:
    x: Int
fn relay(points: List(Point)) -> List(Point):
    points
fn invoke(f: fn(List(Point)) -> List(Point), points: List(Point)) -> List(Point):
    f(points)
fn main() -> Int:
    list.length(invoke(relay, [Point(1)]))
"#).expect("parse first-class rejection");
        let error = compile_module_binary(&first_class)
            .expect_rejected("packed function values require the stage-3 physical signature");
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("declared packed layout")
                && diagnostic.contains("first-class function call")
                && diagnostic.contains("LayoutId"),
            "unexpected first-class diagnostic: {diagnostic}",
        );
    }

    #[test]
    fn scalar_input_packed_result_is_exact_only_across_direct_calls() {
        let direct = r#"
mode opt
type Point packed:
    x: Int
    y: Int
fn build(value: Int) -> Point:
    Point(value + 1, value + 2)
fn main() -> Int:
    let point = build(40)
    point.x * 10 + point.y
"#;
        assert_eq!(run_int(direct), 452);
        let direct_module = parse_module(direct).expect("parse direct packed result");
        let direct_wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&direct_module)
                .expect_lowered("a direct scalar-input packed result carries its LayoutId"),
        );
        assert!(direct_wat.contains("call $build"), "exact direct result: {direct_wat}");

        let first_class = parse_module(r#"
mode opt
type Point packed:
    x: Int
fn build(value: Int) -> Point:
    Point(value)
fn invoke(f: fn(Int) -> Point, value: Int) -> Point:
    f(value)
fn main() -> Int:
    invoke(build, 7).x
"#).expect("parse scalar-input first-class packed result");
        let first_class_error = compile_module_binary(&first_class)
            .expect_rejected("a packed result needs a physical first-class signature");
        assert!(
            first_class_error.to_string().contains("first-class function call")
                && first_class_error.to_string().contains("LayoutId"),
            "exact result-only function-value rejection: {first_class_error}",
        );

        let closure = parse_module(r#"
mode opt
type Point packed:
    x: Int
fn main() -> Int:
    let build = fn(value: Int) -> Point: Point(value)
    build(9).x
"#).expect("parse scalar-input closure packed result");
        let indirect = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::DirectCall)
            .without(witchy_syntax::opt::Opt::ClosureElide);
        witchy_syntax::opt::set_for_tests(Some(indirect));
        let closure_result = compile_module_binary(&closure);
        witchy_syntax::opt::set_for_tests(None);
        let closure_error = closure_result
            .expect_rejected("a packed closure result needs a physical Apply signature");
        assert!(
            closure_error.to_string().contains("first-class function call")
                && closure_error.to_string().contains("LayoutId"),
            "exact result-only Apply rejection: {closure_error}",
        );

        let existential = parse_module(r#"
mode opt
type Point packed:
    x: Int
trait Maker:
    fn make(let self, value: Int) -> Point
type Seed:
    Seed
impl Maker for Seed:
    fn make(let self, value: Int) -> Point:
        Point(value)
fn main() -> Int:
    let maker: dyn Maker = Seed
    maker.make(11).x
"#).expect("parse existential packed result");
        let existential_error = compile_module_binary(&existential)
            .expect_rejected("a packed trait result needs a physical witness signature");
        assert!(
            existential_error.to_string().contains("trait/existential method `make`")
                && existential_error.to_string().contains("LayoutId"),
            "exact result-only trait rejection: {existential_error}",
        );
    }

    #[test]
    fn composite_callable_results_register_exact_layouts_before_boundary_lowering() {
        let direct = r#"
mode opt
type Point packed:
    x: Int
fn build(value: Int) -> (Point, Int):
    (Point(value), value + 1)
fn main() -> Int:
    let pair = build(7)
    78
"#;
        assert_eq!(run_int(direct), 78);
        let direct_module = parse_module(direct).expect("parse direct composite result");
        let direct_wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&direct_module)
                .expect_lowered("a direct composite result carries its exact tuple LayoutId"),
        );
        assert!(direct_wat.contains("call $build"), "exact direct tuple result: {direct_wat}");
        assert!(
            direct_wat.contains("__witchy_packed_record_") && !direct_wat.contains("call $mk2"),
            "the direct composite uses its descriptor rather than the uniform tuple ABI: {direct_wat}"
        );

        let compile_indirect = |source: &str| {
            let module = parse_module(source).expect("parse indirect composite result");
            let indirect = witchy_syntax::opt::OptSet::default_set()
                .without(witchy_syntax::opt::Opt::DirectCall)
                .without(witchy_syntax::opt::Opt::ClosureElide);
            witchy_syntax::opt::set_for_tests(Some(indirect));
            let result = compile_module_binary(&module);
            witchy_syntax::opt::set_for_tests(None);
            result
        };

        let tuple_error = compile_indirect(r#"
mode opt
type Point packed:
    x: Int
fn main() -> Int:
    let build = fn(value: Int) -> (Point, Int): (Point(value), value + 1)
    let pair = build(9)
    0
"#).expect_rejected("a closure tuple result needs an exact physical signature");
        assert!(
            tuple_error.to_string().contains("first-class function call")
                && tuple_error.to_string().contains("LayoutId"),
            "tuple result cannot silently reshape: {tuple_error}"
        );

        let list_tuple_error = compile_indirect(r#"
mode opt
type Point packed:
    x: Int
fn main() -> Int:
    let build = fn(value: Int) -> List((Point, Int)):
        [(Point(value), value + 1)]
    let values = build(9)
    list.length(values)
"#).expect_rejected("a closure list-of-composite result needs an exact signature");
        assert!(
            list_tuple_error.to_string().contains("first-class function call")
                && list_tuple_error.to_string().contains("LayoutId"),
            "list-of-tuple result cannot silently reshape: {list_tuple_error}"
        );

        let nested_list_error = compile_indirect(r#"
mode opt
type Point packed:
    x: Int
fn main() -> Int:
    let build = fn(value: Int) -> List(List(Point)):
        [[Point(value)]]
    let values = build(9)
    list.length(values)
"#).expect_rejected("a dynamic-inline nested packed list is not representable");
        assert!(
            nested_list_error.to_string().contains("declared packed layout rejected")
                && nested_list_error.to_string().contains("dynamic"),
            "nested lists reject instead of selecting the uniform ABI: {nested_list_error}"
        );

        let nested_callable = parse_module(r#"
mode opt
type Point packed:
    x: Int
fn invoke(build: fn(Int) -> (Point, Int), value: Int) -> Int:
    let pair = build(value)
    0
fn make(value: Int) -> (Point, Int):
    (Point(value), value + 1)
fn main() -> Int:
    invoke(make, 9)
"#).expect("parse nested callable result type");
        let nested_callable_error = compile_module_binary(&nested_callable)
            .expect_rejected("a nested fn result position needs an exact signature");
        assert!(
            nested_callable_error.to_string().contains("first-class function call")
                && nested_callable_error.to_string().contains("LayoutId"),
            "nested fn result type is registered before lowering: {nested_callable_error}"
        );
    }

    #[test]
    fn literal_nontrapping_integer_divisors_stay_raw() {
        let module = parse_module(
            "fn main() -> Int:\n    let quotient = 9 / 3\n    quotient + (7 % 4)\n",
        )
        .expect("parse");
        let wir = assemble_wir_module(&module)
            .expect_lowered("the binary path lowers this program");
        let wat = witchy_wir::wir::to_wat(&wir);
        assert!(wat.contains("i64.div_s"));
        assert!(wat.contains("i64.rem_s"));
        assert!(!wat.contains("(func $int_div"));
        assert!(!wat.contains("(func $int_rem"));
    }

    #[test]
    fn borrowed_owner_root_is_released_after_explicit_return_value_evaluation() {
        let module = parse_module(
            "mode opt\n\n\
             fn view(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\n\
             fn finish(xs: let('a) List(Int)) -> Int:\n    let w = view(xs)\n    return list.length(w)\n\n\
             fn main() -> Int:\n    finish([1, 2])\n",
        )
        .expect("parse");
        let wir = assemble_wir_module(&module)
            .expect_lowered("borrowed-view program lowers to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        let start = wat.find("(func $finish").expect("finish function");
        let tail = &wat[start..];
        let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
        let finish = &tail[..end];

        let eval = finish.find("local.set $__witchy_call_result_i64").expect("return evaluated");
        let release = finish.find("call $rc_drop").expect("borrow root released");
        let ret = finish.rfind("return").expect("explicit return");
        assert!(finish.contains("__loan_root_w__xs"), "hidden owner root is declared: {finish}");
        assert!(eval < release && release < ret, "evaluate, release, return ordering: {finish}");
    }

    #[test]
    fn borrowed_owner_root_is_released_on_try_early_return() {
        let module = parse_module(
            "mode opt\n\n\
             fn view(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\n\
             fn fail() -> Result(Int, String):\n    Err(\"stop\")\n\n\
             fn finish(xs: let('a) List(Int)) -> Result(Int, String):\n    let w = view(xs)\n    let n = fail()?\n    Ok(list.length(w) + n)\n\n\
             fn main() -> Int:\n    match finish([1, 2]):\n        Ok(n) -> n\n        Err(_) -> 0\n",
        )
        .expect("parse");
        let wir = assemble_wir_module(&module)
            .expect_lowered("borrowed-view try program lowers to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        let start = wat.find("(func $finish").expect("finish function");
        let tail = &wat[start..];
        let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
        let finish = &tail[..end];
        let drops = finish.match_indices("call $rc_drop").count();

        assert_eq!(finish.match_indices("call $rc_dup").count(), 1, "one root opened: {finish}");
        assert!(drops >= 2, "both `?` failure and success paths release the root: {finish}");
    }

    #[test]
    fn alternative_view_origins_share_one_owner_root() {
        let module = parse_module(
            "mode opt\n\nfn first(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\nfn second(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\nfn count(xs: List(Int), pick: Bool) -> Int:\n    let w = if pick: first(xs) else: second(xs)\n    list.length(w)\n\nfn main() -> Int:\n    count([1], true)\n",
        )
        .expect("parse");
        let wir = assemble_wir_module(&module)
            .expect_lowered("alternative view origins lower");
        let wat = witchy_wir::wir::to_wat(&wir);
        let start = wat.find("(func $count").expect("count function");
        let tail = &wat[start..];
        let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
        let count = &tail[..end];
        assert_eq!(count.match_indices("call $rc_dup").count(), 1, "one owner root: {count}");
        assert_eq!(count.match_indices("call $rc_drop").count(), 1, "one owner release: {count}");
    }

    #[test]
    fn nested_dict_view_roots_the_owner_object_base_not_the_projected_pointer() {
        let module = parse_module(
            "mode opt\n\ntype Holder:\n    values: Dict(Int, Int)\n\nfn view(holder: let('a) Holder) -> View(Dict(Int, Int), 'a):\n    holder.values\n\nfn count(holder: Holder) -> Int:\n    let v = view(holder)\n    dict.length(v)\n\nfn main() -> Int:\n    count(Holder(dict.new()))\n",
        )
        .expect("parse");
        let wir = assemble_wir_module(&module)
            .expect_lowered("nested Dict view lowers to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        let start = wat.find("(func $count").expect("count function");
        let tail = &wat[start..];
        let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
        let count = &tail[..end];
        let root = count.find("local.set $__loan_root_v__holder").expect("root assignment");
        let before = &count[..root];
        let owner = before
            .rfind("    local.get $holder\n")
            .filter(|owner| {
                before.rfind("    local.get $v\n").is_none_or(|view| *owner > view)
            })
            .expect("the retained root must come from Holder, never the projected Dict view");
        assert!(
            !before[owner..].contains("i32.const 4"),
            "a projected Dict bias must not be applied to the containing Holder root: {count}",
        );
    }

    #[test]
    fn direct_dict_view_roots_the_dict_owner_with_its_own_layout_bias() {
        let module = parse_module(
            "mode opt\n\nfn view(values: let('a) Dict(Int, Int)) -> View(Dict(Int, Int), 'a):\n    values\n\nfn count(values: Dict(Int, Int)) -> Int:\n    let v = view(values)\n    dict.length(v)\n\nfn main() -> Int:\n    count(dict.new())\n",
        )
        .expect("parse");
        let wir = assemble_wir_module(&module)
            .expect_lowered("direct Dict view lowers to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        let start = wat.find("(func $count").expect("count function");
        let tail = &wat[start..];
        let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
        let count = &tail[..end];
        let root = count.find("local.set $__loan_root_v__values").expect("root assignment");
        let before = &count[..root];
        let owner = before
            .rfind("    local.get $values\n")
            .filter(|owner| {
                before.rfind("    local.get $v\n").is_none_or(|view| *owner > view)
            })
            .expect("the direct owner, not the returned view, supplies the retained root");
        assert!(
            before[owner..].contains("i32.const 4"),
            "the direct Dict owner's own pointer representation supplies its -4 base bias: {count}",
        );
    }

    #[test]
    fn projected_dict_argument_keeps_the_containing_owner_base() {
        let module = parse_module(
            "mode opt\n\ntype Holder:\n    values: Dict(Int, Int)\n\nfn view(values: let('a) Dict(Int, Int)) -> View(Dict(Int, Int), 'a):\n    values\n\nfn count(holder: Holder) -> Int:\n    let v = view(holder.values)\n    dict.length(v)\n\nfn main() -> Int:\n    count(Holder(dict.new()))\n",
        )
        .expect("parse");
        let typed = witchy_types::typeck::annotate_checked(module.clone())
            .expect("annotate Holder root fixture");
        let checked_facts = witchy_types::loans::facts_with_types(typed.module(), typed.table())
            .expect("checked Holder loan facts");
        let checked_count = typed
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "count" => Some(function),
                _ => None,
            })
            .expect("checked count function");
        let checked_root = checked_facts.opens_after(&checked_count.body.stmts[0])[0].owner_root();
        assert_eq!(
            checked_root.direct_storage_type,
            Some(Type::Named("Holder".into(), vec![])),
            "the caller's checked root type must beat the callee's projected Dict storage type",
        );
        let wir = assemble_wir_module(&module)
            .expect_lowered("projected Dict argument lowers to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        let start = wat.find("(func $count").expect("count function");
        let tail = &wat[start..];
        let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
        let count = &tail[..end];
        let root = count.find("local.set $__loan_root_v__holder").expect("root assignment");
        let before = &count[..root];
        let owner = before.rfind("    local.get $holder\n").expect("owner base");
        assert!(
            !before[owner..].contains("i32.const 4"),
            "the callee's Dict type must not bias the projected argument's Holder base: {count}",
        );
    }

    #[test]
    fn flat_partial_capture_balances_unit_facts_before_fallback() {
        // Arbitrarily deep nested updates are valid and handled by the fallback
        // backend. Exceed the WIR sink's fixed scratch pool before the tail so
        // its walk consumes a strict prefix of the statement identities.
        let update = beyond_wir_assignment_scratch_pool();
        assert_partial_capture_reaches_wir_fallback(&format!(
            "mode opt\n\nfn main() -> Int:\n    let values = [0]\n    let updated = {update}\n    0\n"
        ));
    }

    #[test]
    fn nested_partial_capture_balances_unit_facts_before_fallback() {
        let update = beyond_wir_assignment_scratch_pool();
        assert_partial_capture_reaches_wir_fallback(&format!(
            "mode opt\n\nfn main() -> Int:\n    let values = [0]\n    let updated = if true:\n        {update}\n    else:\n        values\n    0\n"
        ));
    }

    #[test]
    fn lambda_local_view_declares_and_releases_its_root() {
        let module = parse_module(
            "mode opt\n\nfn view(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\nfn main() -> Int:\n    let f = fn() -> Int:\n        let xs = [1, 2]\n        let w = view(xs)\n        list.length(w)\n    f()\n",
        )
        .expect("parse");
        let wir = assemble_wir_module(&module)
            .expect_lowered("lambda-local borrowed view lowers to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        let start = wat.find("(func $__lam").expect("lifted lambda");
        let tail = &wat[start..];
        let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
        let lambda = &tail[..end];
        assert!(lambda.contains("__loan_root_w__xs"), "lambda root local: {lambda}");
        assert!(lambda.contains("call $rc_dup"), "lambda opens root: {lambda}");
        assert!(lambda.contains("call $rc_drop"), "lambda closes root: {lambda}");
    }

    #[test]
    fn inferred_callable_lambda_param_keeps_its_checked_result_width() {
        let module = parse_module(
            "fn strict() -> Int:\n    0\n\n\
             fn invoke(callback: fn(fn() -> Int) -> Int) -> Int:\n    callback(strict)\n\n\
             fn main() -> Int:\n    invoke(fn(callback): callback())\n",
        )
        .expect("parse inferred callable lambda");

        compile_module_binary(&module)
            .expect_lowered("checked callable metadata keeps the nested Int result as i64");
    }

    #[test]
    fn closure_assigning_captured_var_is_rejected() {
        // By-value capture cannot propagate a write back to the outer binding, so
        // assigning a captured variable is rejected rather than diverging.
        let src = r#"
fn run(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)
fn main() -> Int:
    var total = 0
    let add = fn(n: Int):
        total = total + n
    run(add, 5)
"#;
        let module = parse_module(src).expect("parse");
        let err = compile_module_binary(&module)
            .expect_rejected("should reject outer assignment");
        assert!(
            err.to_string()
                .contains("closure cannot assign to the captured variable `total`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn isolated_vm_calls_reject_indirect_callbacks_without_typecheck() {
        fn no_comptime(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let cases = [
            (
                "vm.with_dir",
                r#"
import bytes
import vm

fn worker(dir: Dir, input: Bytes) -> Bytes:
    input

fn invoke(dir: Dir, input: Bytes) -> Bytes:
    let callback = worker
    vm.with_dir(dir, callback, input)

fn main(dir: Dir):
    let _ = invoke(dir, bytes.from_string("input"))
"#,
            ),
            (
                "vm.serve",
                r#"
import bytes
import vm

fn worker(state: Bytes, request: Bytes) -> Bytes:
    state

fn invoke(init: Bytes, requests: List(Bytes)) -> List(Bytes):
    let callback = worker
    vm.serve(init, requests, callback)

fn main():
    let initial = bytes.from_string("state")
    let requests = [bytes.from_string("request")]
    let _ = invoke(initial, requests)
"#,
            ),
        ];

        for (api, source) in cases {
            let entry = parse_module(source).expect("parse");
            let module = witchy_syntax::linker::link(
                vec![("main".to_string(), entry)],
                "main",
                no_comptime,
            )
            .expect("link bundled std without checking types");
            let error = compile_module_binary(&module)
                .expect_rejected("unchecked codegen must preserve the isolation contract");
            let diagnostic = error.to_string();
            assert!(
                diagnostic.contains(api)
                    && diagnostic.contains("bare top-level function")
                    && diagnostic.contains("isolated worker-VM boundary"),
                "unexpected diagnostic for {api}: {diagnostic}"
            );
        }
    }

    #[test]
    fn record_dict_keys_without_eq_are_rejected() {
        let src = r#"
type Key:
    Key(Int)

fn main() -> Int:
    var d = dict.new()
    d = dict.__insert(d, Key(1), 1)
    dict.get_or(d, Key(1), 0)
"#;
        let module = parse_module(src).expect("parse");
        let err = compile_module_binary(&module)
            .expect_rejected("plain record key should not lower");
        assert!(
            err.to_string().contains("resolved Eq compound key"),
            "unexpected diagnostic: {err}"
        );
    }

    /// Build a wasmtime instance whose `print` captures strings from memory.
    fn instantiate_with_print(
        bytes: &[u8],
    ) -> (Store<()>, wasmtime::Instance, Arc<Mutex<Vec<String>>>) {
        let engine = gc_wasmtime_engine();
        let wt = WtModule::new(&engine, bytes).expect("valid wasm");
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = Linker::new(&engine);
        define_abort(&mut linker);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap(
                "witchy",
                "print",
                move |mut caller: Caller<'_, ()>, ptr: i32, len: i32| {
                    let mem = caller.get_export("memory").unwrap().into_memory().unwrap();
                    let data = mem.data(&caller);
                    let bytes = &data[ptr as usize..(ptr + len) as usize];
                    sink.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(bytes).into_owned());
                },
            )
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        (store, instance, captured)
    }

    fn run_authenticated_dynamic(src: &str) -> Vec<String> {
        use witchy_types::runtime_type::{
            AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
        };

        fn no_expand(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let module = parse_module(src).expect("parse Dynamic Wasm fixture");
        let workspace = PackageCoordinate::new(
            PackageSource::Workspace,
            "example/dynamic-test",
            "0.1.0",
        )
        .expect("workspace coordinate");
        let toolchain = PackageCoordinate::new(
            PackageSource::Toolchain,
            "witchy/stdlib",
            "0.1.0",
        )
        .expect("toolchain coordinate");
        let mut assignments = vec![(
            "main".to_string(),
            ModuleLoadIdentity::new(workspace, ["main"]).expect("main owner"),
        )];
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|std_module| {
            (
                (*std_module).to_string(),
                ModuleLoadIdentity::new(toolchain.clone(), ["std", *std_module])
                    .expect("std owner"),
            )
        }));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
            .expect("authenticated owners");
        let checked = witchy_types::pipeline::link_checked_authenticated(
            vec![("main".into(), module)],
            "main",
            no_expand,
            owners,
        )
        .expect("authenticated Dynamic checked link");
        let bytes = compile_checked_module_binary(&checked)
            .expect_lowered("authenticated Dynamic fixture lowers");
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().clone()
    }

    #[test]
    fn dynamic_descriptor_exact_decode_and_mismatch_match_the_interpreter() {
        let output = run_authenticated_dynamic(
            "import dynamic\nimport reflect\n\ntype User:\n    name: String\n    age: Int\n\ntype Box(a):\n    Box(a)\n\nimpl Reflect for User:\n    fn reflect(self) -> reflect.Mirror:\n        reflect.MNil\n\nimpl Reflect for Box(a):\n    fn reflect(self) -> reflect.Mirror:\n        reflect.MNil\n\nfn main(console: Console):\n    let value = dynamic.dynamic(7)\n    console.print(dynamic.type_name(dynamic.type_of(value)))\n    console.print(dynamic.type_name(dynamic.runtime_type(Int)))\n    console.print(dynamic.type_name(dynamic.runtime_type(User)))\n    console.print(dynamic.type_name(dynamic.runtime_type(Box(Int))))\n    console.print(dynamic.type_name(dynamic.runtime_type(.{age: Int, name: String})))\n    console.print(dynamic.type_name(dynamic.runtime_type(.[Count(Int) | Label(String)])))\n    let exact: Option(Int) = dynamic.try_decode(value)\n    match exact:\n        Some(number) -> console.print(\"${number}\")\n        None -> console.print(\"missing-int\")\n    let mismatch: Option(String) = dynamic.try_decode(value)\n    match mismatch:\n        Some(text) -> console.print(text)\n        None -> console.print(\"none\")\n    let decoded: Result(Int, dynamic.DynamicError) = dynamic.decode(value)\n    match decoded:\n        Ok(number) -> console.print(\"decoded-${number}\")\n        Err(_) -> console.print(\"decode-failed\")\n    let wrong: Result(String, dynamic.DynamicError) = dynamic.decode(value)\n    match wrong:\n        Ok(text) -> console.print(text)\n        Err(dynamic.TypeMismatch(actual)) -> console.print(\"mismatch-${dynamic.type_name(actual)}\")\n        Err(_) -> console.print(\"unexpected-dynamic-error\")\n    let person = dynamic.dynamic(User(\"Ada\", 42))\n    let decoded_person: Option(User) = dynamic.try_decode(person)\n    match decoded_person:\n        Some(user) -> console.print(user.name)\n        None -> console.print(\"missing-user\")\n    let words = dynamic.dynamic([\"alpha\", \"beta\"])\n    let decoded_words: Option(List(String)) = dynamic.try_decode(words)\n    match decoded_words:\n        Some(items) -> console.print(list.at(items, 1))\n        None -> console.print(\"missing-words\")\n    let boxed = dynamic.dynamic(Box(11))\n    let decoded_box: Option(Box(Int)) = dynamic.try_decode(boxed)\n    match decoded_box:\n        Some(Box(number)) -> console.print(\"box-${number}\")\n        None -> console.print(\"missing-box\")\n    let record = dynamic.dynamic(.{name: \"Nia\", age: 9})\n    let decoded_record: Option(.{age: Int, name: String}) = dynamic.try_decode(record)\n    match decoded_record:\n        Some(found) -> console.print(found.name)\n        None -> console.print(\"missing-record\")\n    let choice: .[Count(Int) | Label(String)] = .Count(5)\n    let encoded_choice = dynamic.dynamic(choice)\n    let decoded_choice: Option(.[Count(Int) | Label(String)]) = dynamic.try_decode(encoded_choice)\n    match decoded_choice:\n        Some(.Count(number)) -> console.print(\"count-${number}\")\n        Some(.Label(label)) -> console.print(label)\n        None -> console.print(\"missing-choice\")\n",
        );
        assert_eq!(
            output,
            ["Int", "Int", "User", "Box(Int)", ".{age: Int, name: String}", ".[Count(Int) | Label(String)]", "7", "none", "decoded-7", "mismatch-Int", "Ada", "beta", "box-11", "Nia", "count-5"]
        );
    }

    #[test]
    fn existential_pack_uses_a_typed_payload_box_and_erased_wrapper() {
        let src = r#"
trait Render:
    fn render(self) -> Int

type Label:
    Label(Int)

impl Render for Label:
    fn render(self) -> Int:
        match self:
            Label(value) -> value

fn main() -> Nil:
    let item: dyn Render = Label(42)
    Nil
"#;
        let module = parse_module(src).expect("parse");
        let bytes = compile_module_binary(&module)
            .expect_lowered("a closed existential construction lowers to Wasm GC");
        let mut struct_news = 0usize;
        for payload in gc_wasm_payloads(&bytes) {
            if let wasmparser::Payload::CodeSectionEntry(body) = payload.expect("valid wasm") {
                let mut operators = body.get_operators_reader().expect("operators");
                while !operators.eof() {
                    if matches!(operators.read().expect("operator"), wasmparser::Operator::StructNew { .. }) {
                        struct_news += 1;
                    }
                }
            }
        }
        assert!(
            struct_news >= 2,
            "one concrete payload box and one erased existential wrapper must be allocated"
        );
    }

    #[test]
    fn existential_dispatch_uses_a_closed_typed_table_adapter() {
        let src = r#"
trait Render:
    fn render(self) -> Int

type Label:
    Label(Int)

impl Render for Label:
    fn render(self) -> Int:
        match self:
            Label(value) -> value

fn main() -> Int:
    let item: dyn Render = Label(42)
    item.render()
"#;
        let module = parse_module(src).expect("parse");
        let bytes = compile_module_binary(&module)
            .expect_lowered("a closed existential dispatch lowers to a Wasm table adapter");
        let mut indirect_calls = 0usize;
        for payload in gc_wasm_payloads(&bytes) {
            if let wasmparser::Payload::CodeSectionEntry(body) = payload.expect("valid wasm") {
                let mut operators = body.get_operators_reader().expect("operators");
                while !operators.eof() {
                    if matches!(operators.read().expect("operator"), wasmparser::Operator::CallIndirect { .. }) {
                        indirect_calls += 1;
                    }
                }
            }
        }
        assert!(indirect_calls >= 1, "existential dispatch must use a table call");
        assert_eq!(run_int(src), 42);
    }

    #[test]
    fn existential_adapters_reserve_table_slots_before_closures() {
        let src = r#"
trait Render:
    fn render(self) -> Int

type Label:
    Label(Int)

impl Render for Label:
    fn render(self) -> Int:
        match self:
            Label(value) -> value

fn main() -> Int:
    let item: dyn Render = Label(42)
    let increment = fn (value: Int) -> Int:
        value + 1
    increment(item.render())
"#;
        assert_eq!(run_int(src), 43);
    }

    #[test]
    fn existential_list_dispatches_each_concrete_witness() {
        let src = r#"
trait Render:
    fn render(self) -> Int

type Label:
    Label(Int)

type Badge:
    Badge(Int)

impl Render for Label:
    fn render(self) -> Int:
        match self:
            Label(value) -> value

impl Render for Badge:
    fn render(self) -> Int:
        match self:
            Badge(value) -> value * 10

fn main() -> Int:
    let items: List(dyn Render) = [Label(4), Badge(3)]
    items[0].render() + items[1].render()
"#;
        assert_eq!(run_int(src), 34);
    }

    #[test]
    fn existential_footprint_tracks_every_reachable_adapter_and_its_authority() {
        let source = |constructions: &str| format!(r#"
trait Work:
    fn run(let self, console: Console) -> Int

type Quiet:
    Quiet

type Loud:
    Loud

impl Work for Quiet:
    fn run(let self, console: Console) -> Int:
        1

impl Work for Loud:
    fn run(let self, console: Console) -> Int:
        console.print("loud")
        2

fn main(console: Console) -> Int:
{constructions}
"#);
        let wat = |source: String| {
            let module = parse_module(&source).expect("parse existential footprint fixture");
            let wir = assemble_wir_module(&module)
                .expect_lowered("reachable witness adapters lower to WIR");
            witchy_wir::wir::to_wat(&wir)
        };

        let quiet = wat(source(
            "    let item: dyn Work = Quiet\n    item.run(console)\n",
        ));
        assert_eq!(quiet.match_indices("(func $__dynw").count(), 1, "{quiet}");
        assert!(
            !quiet.contains("(import \"witchy\" \"print\""),
            "an unreachable authority-using witness must not widen imports: {quiet}"
        );

        let all = wat(source(
            "    let items: List(dyn Work) = [Quiet, Loud]\n    items[0].run(console) + items[1].run(console)\n",
        ));
        assert_eq!(
            all.match_indices("(func $__dynw").count(),
            2,
            "each reachable closed construction needs one adapter: {all}"
        );
        assert!(
            all.contains("(import \"witchy\" \"print\""),
            "the reachable Loud witness must widen the compiled authority footprint: {all}"
        );

        let again = wat(source(
            "    let items: List(dyn Work) = [Quiet, Loud]\n    items[0].run(console) + items[1].run(console)\n",
        ));
        assert_eq!(all, again, "witness and authority footprint must be deterministic");
    }

    #[test]
    fn existential_var_receiver_writes_back_a_reboxed_payload() {
        let src = r#"
trait Counter:
    fn replace(var self, value: Int) -> Int
    fn read(let self) -> Int

type Box:
    Box(Int)

impl Counter for Box:
    fn replace(var self, value: Int) -> Int:
        match self:
            Box(old) ->
                self = Box(value)
                old

    fn read(let self) -> Int:
        match self:
            Box(value) -> value

fn main() -> Int:
    var item: dyn Counter = Box(4)
    item.replace(9) + item.read()
"#;
        assert_eq!(run_int(src), 13);
    }

    #[test]
    fn existential_own_receiver_consumes_the_erased_payload() {
        let src = r#"
trait Consume:
    fn take(own self) -> Int

type Box:
    Box(Int)

impl Consume for Box:
    fn take(own self) -> Int:
        match self:
            Box(value) -> value

fn main() -> Int:
    let item: dyn Consume = Box(27)
    item.take()
"#;
        assert_eq!(run_int(src), 27);
    }

    #[test]
    fn existential_var_argument_writes_back_through_the_typed_table_abi() {
        let src = r#"
trait Counter:
    fn bump(let self, var value: Int) -> Int

type Box:
    Box(Int)

impl Counter for Box:
    fn bump(let self, var value: Int) -> Int:
        value = value + 1
        match self:
            Box(base) -> base + value

fn main() -> Int:
    let item: dyn Counter = Box(4)
    var value = 7
    item.bump(value) + value
"#;
        assert_eq!(run_int(src), 20);
    }

    #[test]
    fn existential_unique_result_and_var_capacity_round_trip() {
        let src = r#"
mode opt

trait Lists:
    fn revise(let self, var values: unique List(Int)) -> unique List(Int)

type Box:
    Box(Int)

impl Lists for Box:
    fn revise(let self, var values: unique List(Int)) -> unique List(Int):
        values = [2, 3, 4]
        match self:
            Box(base) -> [base, base + 1]

fn main() -> Int:
    let item: dyn Lists = Box(7)
    var values = [1]
    var result = item.revise(values)
    list.length(values) * 100 + list.at(values, 2) * 10 + list.at(result, 1)
"#;
        assert_eq!(run_int(src), 348);
    }

    #[test]
    fn existential_var_receiver_and_argument_commit_in_one_table_result() {
        let src = r#"
trait Counter:
    fn adjust(var self, var value: Int) -> Int
    fn read(let self) -> Int

type Box:
    Box(Int)

impl Counter for Box:
    fn adjust(var self, var value: Int) -> Int:
        self = Box(value)
        value = value + 1
        value

    fn read(let self) -> Int:
        match self:
            Box(value) -> value

fn main() -> Int:
    var item: dyn Counter = Box(1)
    var value = 8
    item.adjust(value) + item.read() + value
"#;
        assert_eq!(run_int(src), 26);
    }

    #[test]
    fn renames_calls_to_shadowed_local_closures() {
        // A called LOCAL closure (`f(x)`, where `f` is bound by a match pattern)
        // must keep its call site when alpha-rename gives it a unique name. Both
        // arms bind `f`; the second is renamed so the two don't alias one WASM
        // local, and the body's `f(x)` has to follow that rename. Before the fix
        // the `Call` name was assumed to always be a global, so the renamed local
        // lost its call site and compiled to a trap / unknown-function error —
        // the bug that blocked `chan.address` (Recv + Whoami both bind `cont`).
        let src = r#"
type Box:
    A(fn(Int) -> Int)
    B(fn(Int) -> Int)

fn dbl(n: Int) -> Int:
    (n + n)

fn apply_it(b: Box, x: Int) -> Int:
    match b:
        A(f) -> f(x)
        B(f) -> f(x)

fn main() -> Int:
    (apply_it(A(dbl), 5) + apply_it(B(dbl), 10))
"#;
        assert_eq!(run_int(src), 30);
    }

    #[test]
    fn indirect_unique_result_and_var_capacity_round_trip() {
        let src = r#"
mode opt

fn build() -> unique List(Int):
    [1, 2]

fn append(var xs: unique List(Int)) -> Nil:
    xs = [1, 2, 3]
    return

fn main() -> Int:
    let make = build
    var xs = make()
    let update = append
    update(xs)
    list.length(xs) * 10 + list.at(xs, 2)
"#;
        let indirect = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::ClosureElide)
            .without(witchy_syntax::opt::Opt::DirectCall);
        witchy_syntax::opt::set_for_tests(Some(indirect));
        let result = run_int(src);
        witchy_syntax::opt::set_for_tests(None);
        assert_eq!(result, 33);

        let (_, indirect_calls, _) = call_shape(src, indirect);
        assert!(
            indirect_calls >= 2,
            "both first-class calls must retain table dispatch in the inverse configuration"
        );
    }

    #[test]
    fn named_function_value_unique_result_keeps_its_capacity_result() {
        let src = r#"
mode opt

fn build() -> unique List(Int):
    [1, 2]

fn main() -> Int:
    let make = build
    var xs = make()
    list.length(xs) * 10 + list.at(xs, 1)
"#;
        let indirect = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::ClosureElide)
            .without(witchy_syntax::opt::Opt::DirectCall);
        witchy_syntax::opt::set_for_tests(Some(indirect));
        let result = run_int(src);
        witchy_syntax::opt::set_for_tests(None);
        assert_eq!(result, 22);
    }

    #[test]
    fn named_function_value_var_capacity_keeps_its_writeback_result() {
        let src = r#"
mode opt

fn append(var xs: unique List(Int)) -> Nil:
    xs = [1, 2, 3]
    return

fn main() -> Int:
    var xs = [0]
    let update = append
    update(xs)
    list.length(xs) * 10 + list.at(xs, 2)
"#;
        let indirect = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::ClosureElide)
            .without(witchy_syntax::opt::Opt::DirectCall);
        witchy_syntax::opt::set_for_tests(Some(indirect));
        let result = run_int(src);
        witchy_syntax::opt::set_for_tests(None);
        assert_eq!(result, 33);
    }

    #[test]
    fn lambda_literal_ownership_envelope_round_trips() {
        let src = r#"
mode opt

fn main() -> Int:
    let make = fn() -> unique List(Int):
        [4]
    var xs = make()
    let update = fn(var ys: unique List(Int)) -> Nil:
        ys = [4, 5]
        return
    update(xs)
    list.length(xs) * 10 + list.at(xs, 1)
"#;
        let indirect = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::ClosureElide)
            .without(witchy_syntax::opt::Opt::DirectCall);
        witchy_syntax::opt::set_for_tests(Some(indirect));
        let result = run_int(src);
        witchy_syntax::opt::set_for_tests(None);
        assert_eq!(result, 25);

        let (_, indirect_calls, _) = call_shape(src, indirect);
        assert!(indirect_calls >= 2, "both lambda calls must keep table dispatch");
    }

    #[test]
    fn indirect_own_capacity_round_trips_into_unique_result() {
        let src = r#"
mode opt

fn forward(own values: unique List(Int)) -> unique List(Int):
    move values

fn main() -> Int:
    var values = [5, 6]
    let transfer = forward
    var result = transfer(move values)
    list.length(result) * 10 + list.at(result, 1)
"#;
        let indirect = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::ClosureElide)
            .without(witchy_syntax::opt::Opt::DirectCall);
        witchy_syntax::opt::set_for_tests(Some(indirect));
        let result = run_int(src);
        witchy_syntax::opt::set_for_tests(None);
        assert_eq!(result, 26);

        let (_, indirect_calls, _) = call_shape(src, indirect);
        assert!(indirect_calls >= 1, "the own transfer must retain table dispatch");
    }

    #[test]
    fn compiles_nested_var_place_with_annotated_element_kind() {
        // A value-returning `var` call has a multi-result ABI. Local inference
        // must use the checker-resolved RHS type so the ordinary result and a
        // nested-place write-back retain their distinct WIR kinds.
        let src = r#"
type State:
    rows: List(List(Int))

fn bump(var n: Int) -> Int:
    n = n + 10
    n * 2

fn main() -> Int:
    var state = State([[1, 2], [3, 4]])
    let result: Int = bump(state.rows[0][1])
    let updated: Int = state.rows[0][1]
    if updated == 12 && result == 24: 1 else: 0
"#;
        assert_eq!(run_int(src), 1);
    }

    /// (BUG-008) Compile `src` under the optimization set `opt` and report the two
    /// representation signals the `direct-call`, `bounds-elide`, and `closure-elide`
    /// levers move: direct callees, indirect-call count, and GC struct allocations.
    /// Callee indices are resolved through the emitted name section
    /// (imports first, then defined funcs — the order `wir_encode` writes), so a
    /// devirtualized closure call shows up as a direct call to `__lamw{i}` and a
    /// checked list access as a direct call to `list_at`. This inspects the raw
    /// witchy-emitted wasm (`compile_module_binary` runs no Binaryen), so the shape
    /// is the lever's own doing, not a downstream inliner's.
    fn call_shape(
        src: &str,
        opt: witchy_syntax::opt::OptSet,
    ) -> (std::collections::HashSet<String>, usize, Vec<u32>) {
        use std::collections::{HashMap, HashSet};
        witchy_syntax::opt::set_for_tests(Some(opt));
        let module = parse_module(src).expect("parse");
        let compiled = compile_module_binary(&module);
        witchy_syntax::opt::set_for_tests(None);
        let bytes = compiled.expect_lowered("the binary path lowers this program");

        let mut names: HashMap<u32, String> = HashMap::new();
        let mut called: Vec<u32> = Vec::new();
        let mut indirect = 0usize;
        let mut gc_struct_news = Vec::new();
        for payload in gc_wasm_payloads(&bytes) {
            match payload.expect("valid wasm") {
                wasmparser::Payload::CustomSection(reader) => {
                    if let wasmparser::KnownCustom::Name(section) = reader.as_known() {
                        for sub in section {
                            if let wasmparser::Name::Function(map) = sub.expect("name subsection") {
                                for naming in map {
                                    let naming = naming.expect("naming");
                                    names.insert(naming.index, naming.name.to_string());
                                }
                            }
                        }
                    }
                }
                wasmparser::Payload::CodeSectionEntry(body) => {
                    for op in body.get_operators_reader().expect("operators") {
                        match op.expect("operator") {
                            wasmparser::Operator::Call { function_index } => called.push(function_index),
                            wasmparser::Operator::CallIndirect { .. } => indirect += 1,
                            wasmparser::Operator::StructNew { struct_type_index } => {
                                gc_struct_news.push(struct_type_index);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        let direct: HashSet<String> = called
            .into_iter()
            .map(|i| names.get(&i).cloned().unwrap_or_else(|| format!("#{i}")))
            .collect();
        (direct, indirect, gc_struct_news)
    }

    fn assert_one_boxed_gc_closure(struct_news: &[u32], context: &str) {
        let distinct: std::collections::HashSet<_> = struct_news.iter().copied().collect();
        assert_eq!(
            struct_news.len(),
            2,
            "{context}: one boxed capturing closure allocates one typed GC environment and one closure wrapper (got {struct_news:?})",
        );
        assert_eq!(
            distinct.len(),
            2,
            "{context}: the typed GC environment and closure wrapper use distinct struct types (got {struct_news:?})",
        );
    }

    #[test]
    fn devirtualizes_single_bound_closure_call() {
        // (RFC-0034 L3 / BUG-008) A closure local bound by exactly one `let` and never
        // reassigned reaches the same lambda at every call, so the default-on
        // `direct-call` lever lowers `g(x)` to a DIRECT `call $__lamw{i}` (recovering
        // the lifted body's index at compile time) instead of a `call_indirect` through
        // the closure record's runtime code-index word. `g` captures `k`, so the env
        // still flows — the devirt is sound for capturing closures too. This proof
        // explicitly disables `closure-elide`, which otherwise subsumes the site with
        // a threaded `__lamt` call. Asserting on the emitted call SHAPE is the firing
        // proof: a call-shape lever moves no heap, so
        // there is no `witchy stats` counter to check (opt.rs registry note).
        let src = r#"
fn main() -> Int:
    let k = 10
    let g = fn(x: Int): (x + k)
    (g(5) + g(7))
"#;
        let direct_base = witchy_syntax::opt::OptSet::default_set()
            .without(witchy_syntax::opt::Opt::ClosureElide);
        let (on, on_indirect, _) = call_shape(src, direct_base);
        assert!(
            on.iter().any(|n| n.starts_with("__lamw")),
            "direct-call ON: the single-bound closure call devirtualizes to `call $__lamw` (got {on:?})",
        );
        assert_eq!(
            on_indirect, 0,
            "direct-call ON: no `call_indirect` remains for the sole closure call",
        );

        // Inverse guard: remove ONLY `direct-call` and the SAME program must revert to
        // an indirect call — proving the shape is this lever's doing, not incidental
        // codegen (an always-`__lamw` emitter would pass the ON case and lie here).
        let off_set = direct_base.without(witchy_syntax::opt::Opt::DirectCall);
        let (off, off_indirect, _) = call_shape(src, off_set);
        assert!(
            !off.iter().any(|n| n.starts_with("__lamw")),
            "-direct-call: the closure call is NOT devirtualized (got {off:?})",
        );
        assert!(
            off_indirect >= 1,
            "-direct-call: the closure call stays `call_indirect` (indirect={off_indirect})",
        );
    }

    #[test]
    fn elides_bounds_check_in_counted_loop() {
        // (RFC-0034 L2 / BUG-008) Inside `for i in 0..list.length(xs)` over an
        // unreassigned `xs`, the compiler-managed counter satisfies `0 <= i < length(xs)`
        // by construction, so the default-on `bounds-elide` lever lowers `list.at(xs, i)`
        // to a direct UNCHECKED load — dropping the `call $list_at` helper that carries
        // the `i < 0 || i >= len` trap guard. With the lever off, every access keeps its
        // checked `$list_at` call (the de-opt reference the differential sweep compares).
        let src = r#"
fn main() -> Int:
    let xs = [3, 1, 4, 1, 5]
    var t = 0
    for i in 0..list.length(xs):
        t = (t + list.at(xs, i))
    t
"#;
        let default = witchy_syntax::opt::OptSet::default_set();
        let (on, _, _) = call_shape(src, default);
        assert!(
            !on.contains("list_at"),
            "bounds-elide ON: the counted-loop access is an unchecked load, no `call $list_at` (got {on:?})",
        );

        // Inverse guard: remove ONLY `bounds-elide` and the checked `$list_at` helper
        // call returns — proving the elision is this lever's doing.
        let off_set = default.without(witchy_syntax::opt::Opt::BoundsElide);
        let (off, _, _) = call_shape(src, off_set);
        assert!(
            off.contains("list_at"),
            "-bounds-elide: the access keeps its checked `call $list_at` guard (got {off:?})",
        );
    }

    /// Run `src` on the COMPILED backend under a specific optimization set (for value-
    /// parity checks across the `closure-elide` lever).
    fn run_str_opt(src: &str, opt: witchy_syntax::opt::OptSet) -> Vec<String> {
        witchy_syntax::opt::set_for_tests(Some(opt));
        let module = parse_module(src).expect("parse");
        let bytes = compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        witchy_syntax::opt::set_for_tests(None);
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().clone()
    }

    #[test]
    fn elides_nonescaping_closure_env() {
        // (RFC-0062 tier-1) `g` is bound by exactly one `let`, captures `k`, and is used
        // ONLY as a direct-call callee (`g(5)`, `g(7)`) — it never escapes. Under the
        // `closure-elide` lever its typed GC environment and wrapper are ELIDED, and
        // the call becomes a direct `call $__lamt{i}` that threads the capture `k` as a
        // leading argument (no env pointer, no per-call env load). The firing proof is the
        // emitted representation: no `struct.new`, `__lamt` present, and `__lamw`
        // (the boxed-devirt body) absent.
        let src = r#"
fn main() -> Int:
    let k = 10
    let g = fn(x: Int): (x + k)
    (g(5) + g(7))
"#;
        let on = witchy_syntax::opt::OptSet::default_set();
        let (on_calls, on_indirect, on_struct_news) = call_shape(src, on);
        assert!(
            on_struct_news.is_empty(),
            "closure-elide ON: no typed GC environment or closure wrapper is allocated (got {on_struct_news:?})",
        );
        assert!(
            on_calls.iter().any(|n| n.starts_with("__lamt")),
            "closure-elide ON: the closure body is called directly, captures threaded (`__lamt`) (got {on_calls:?})",
        );
        assert!(
            !on_calls.iter().any(|n| n.starts_with("__lamw")),
            "closure-elide ON: no boxed env-devirt body (`__lamw`) remains (got {on_calls:?})",
        );
        assert_eq!(on_indirect, 0, "closure-elide ON: no `call_indirect` for an elided closure");

        // Inverse guard: remove ONLY `closure-elide` and the SAME program reverts to the
        // boxed closure — a typed GC environment plus closure wrapper and a devirtualized
        // `call $__lamw` — proving the elision is this lever's doing.
        let off = on.without(witchy_syntax::opt::Opt::ClosureElide);
        let (off_calls, _, off_struct_news) = call_shape(src, off);
        assert_one_boxed_gc_closure(&off_struct_news, "-closure-elide");
        assert!(
            off_calls.iter().any(|n| n.starts_with("__lamw"))
                && !off_calls.iter().any(|n| n.starts_with("__lamt")),
            "-closure-elide: the closure stays boxed (`__lamw`, no `__lamt`) (got {off_calls:?})",
        );
    }

    #[test]
    fn keeps_env_for_escaping_closure() {
        // (RFC-0062 default-deny) `g` is passed WHOLE into `apply_it` — it escapes the
        // frame, so even under `closure-elide` its typed GC environment and immutable
        // wrapper MUST stay allocated and no `__lamt` threaded body is emitted. This is
        // the firing proof's
        // negative half: the lever fires ONLY when the escape oracle proves confinement.
        let src = r#"
fn apply_it(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let k = 10
    let g = fn(x: Int): (x + k)
    apply_it(g, 5)
"#;
        let on = witchy_syntax::opt::OptSet::default_set();
        let (calls, _, struct_news) = call_shape(src, on);
        assert_one_boxed_gc_closure(&struct_news, "closure-elide ON but ESCAPING");
        assert!(
            !calls.iter().any(|n| n.starts_with("__lamt")),
            "closure-elide ON but ESCAPING: no threaded body — the closure stays boxed (got {calls:?})",
        );
    }

    #[test]
    fn elided_closure_matches_boxed_output() {
        // (RFC-0062 parity) The allocation strategy is unobservable: an elided closure and
        // a boxed one must produce identical output. Covers a capture that is read AND a
        // closure invoked many times in a loop (the hot-path shape the lever targets).
        let src = r#"
fn main(console: Console):
    let base = 100
    let f = fn(x: Int): (x + base)
    var total = 0
    var i = 0
    while (i < 5):
        total = (total + f(i))
        i = (i + 1)
    console.print("${total}")
"#;
        let on_set = witchy_syntax::opt::OptSet::default_set();
        let off_set = on_set.without(witchy_syntax::opt::Opt::ClosureElide);
        let on = run_str_opt(src, on_set);
        let off = run_str_opt(src, off_set);
        // 100+0 + 101+... => (100*5) + (0+1+2+3+4) = 500 + 10 = 510.
        assert_eq!(on, vec!["510".to_string()], "elided closure computes the right value");
        assert_eq!(on, off, "elided and boxed closures produce identical output (parity)");
    }

    #[test]
    fn borrowed_shell_scalar_update_keeps_one_root_for_the_whole_mutable_shell() {
        let source = "mode opt\n\n\
             type Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n\n\
             fn make(input: let('a) String) -> Cursor('a):\n    Cursor(input, 0)\n\n\
             fn main() -> Int:\n    let input = \"root\"\n    var cursor = make(input)\n    cursor.offset = cursor.offset + 1\n    cursor.offset\n";
        assert_eq!(run_int(source), 1);

        let module = parse_module(source).expect("parse borrowed shell mutation");
        let wir = assemble_wir_module(&module)
            .expect_lowered("borrowed shell scalar mutation lowers to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        let start = wat.find("(func $main").expect("main function");
        let tail = &wat[start..];
        let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
        let main = &tail[..end];
        assert!(main.contains("__loan_root_cursor__input"), "checked root local: {main}");
        assert_eq!(main.match_indices("call $rc_dup").count(), 1, "one root retain: {main}");
        assert_eq!(main.match_indices("call $rc_drop").count(), 1, "one root release: {main}");
    }

    #[test]
    fn borrowed_shell_field_replacement_retires_then_retains_the_new_root() {
        let source = "mode opt\n\n\
             type Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n\n\
             fn make(input: let('a) String) -> Cursor('a):\n    Cursor(input, 0)\n\n\
             fn replace_cursor(left: let('a) String, right: let('a) String) -> Int:\n    var cursor = make(left)\n    cursor.view = right\n    cursor.offset\n\n\
             fn main() -> Int:\n    let left = \"left\"\n    let right = \"right\"\n    replace_cursor(left, right)\n";
        assert_eq!(run_int(source), 0);

        let module = parse_module(source).expect("parse borrowed shell root transition");
        let wir = assemble_wir_module(&module)
            .expect_lowered("borrowed shell root transition lowers to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        let start = wat.find("(func $replace_cursor").expect("replace function");
        let tail = &wat[start..];
        let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
        let replace = &tail[..end];
        assert!(replace.contains("__loan_root_cursor__left"), "old root local: {replace}");
        assert!(replace.contains("__loan_root_cursor__right"), "new root local: {replace}");
        assert_eq!(replace.match_indices("call $rc_dup").count(), 2, "one retain per root: {replace}");
        assert_eq!(replace.match_indices("call $rc_drop").count(), 2, "one release per root: {replace}");
        assert!(
            replace.find("call $rc_drop").expect("retired root drop")
                < replace.rfind("call $rc_dup").expect("replacement root retain"),
            "the old root closes before the replacement root opens: {replace}"
        );
    }

    #[test]
    fn borrowed_shell_roots_balance_on_explicit_and_branch_returns() {
        let source = "mode opt\n\n\
             type Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n\n\
             fn make(input: let('a) String) -> Cursor('a):\n    Cursor(input, 7)\n\n\
             fn early(input: let('a) String) -> Int:\n    var cursor = make(input)\n    return cursor.offset\n\n\
             fn branch(input: let('a) String, take: Bool) -> Int:\n    var cursor = make(input)\n    if take:\n        return cursor.offset\n    cursor.offset\n\n\
             fn looped(input: let('a) String) -> Int:\n    var cursor = make(input)\n    var i = 0\n    while (i < 3):\n        i = i + 1\n    cursor.offset + i\n\n\
             fn main() -> Int:\n    let input = \"root\"\n    early(input) + branch(input, true) + branch(input, false) + looped(input)\n";
        assert_eq!(run_int(source), 31);

        let module = parse_module(source).expect("parse aggregate root lifecycle fixture");
        let wir = assemble_wir_module(&module)
            .expect_lowered("aggregate root lifecycle lowers to WIR");
        let wat = witchy_wir::wir::to_wat(&wir);
        for function in ["early", "branch", "looped"] {
            let start = wat.find(&format!("(func ${function}")).expect("lifecycle function");
            let tail = &wat[start..];
            let end = tail[1..].find("\n  (func $").map(|n| n + 1).unwrap_or(tail.len());
            let body = &tail[..end];
            assert!(body.contains("__loan_root_cursor__input"), "root local in {function}: {body}");
            assert!(body.contains("call $rc_dup"), "retain in {function}: {body}");
            assert!(body.contains("call $rc_drop"), "release in {function}: {body}");
        }
    }
}
