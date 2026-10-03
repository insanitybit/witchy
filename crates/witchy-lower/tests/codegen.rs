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

    fn expect_view_rejection(source: &str, context: &str, expected: &str) {
        let module = parse_module(source).expect("parse immutable-view fixture");
        let error = compile_module_binary(&module).expect_rejected(context);
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains(expected),
            "{context}: expected diagnostic containing {expected:?}, got {diagnostic}"
        );
    }
    use witchy_syntax::parser::parse_module;
    #[cfg(feature = "native-test-runtime")]
    use std::sync::{Arc, Mutex};
    #[cfg(feature = "native-test-runtime")]
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

    #[cfg(feature = "native-test-runtime")]
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
            intrinsics::REGEX_OPERATIONS,
        ] {
            assert_catalog_names_live_wir_helpers(operations);
        }
    }

    /// (RFC-0045) Define the always-linked, authority-free `__witchy_abort` import
    /// so a module that routes an abort through it (float ordering, list/bytes OOB,
    /// str_to_int, `fail`) instantiates in these minimal test linkers. The body
    /// traps, matching the real host's `bail!` contract (the call never returns).
    #[cfg(feature = "native-test-runtime")]
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
        import_param_types(wasm).into_iter()
            .map(|(name, params)| (name, params.len())).collect()
    }

    fn import_param_types(wasm: &[u8]) -> std::collections::BTreeMap<String, Vec<wasmparser::ValType>> {
        let mut func_param_types = Vec::new();
        let mut imports = Vec::new();
        for payload in gc_wasm_payloads(wasm) {
            match payload.expect("valid wasm") {
                wasmparser::Payload::TypeSection(reader) => {
                    for group in reader {
                        for ty in group.expect("recursive type group").into_types() {
                            let params = match ty.composite_type.inner {
                                wasmparser::CompositeInnerType::Func(func) => {
                                    Some(func.params().to_vec())
                                }
                                _ => None,
                            };
                            func_param_types.push(params);
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
                let params = func_param_types
                    .get(idx)
                    .and_then(|params| params.as_ref())
                    .unwrap_or_else(|| panic!("missing function type {idx} for import {name}"));
                (name, params.clone())
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
            witchy_syntax::linker::ComptimeExpander::new(no_comptime_expansion),
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

    fn link_bytes_app(source: &str) -> witchy_syntax::ast::Module {
        let bytes_module = parse_module(
            witchy_syntax::linker::bundled_source("bytes").expect("bundled bytes module"),
        )
        .expect("parse bundled bytes module");
        let app = parse_module(source).expect("parse bytes app");
        link_test_modules(
            vec![("bytes".into(), bytes_module), ("app".into(), app)],
            "app",
            &std::collections::HashSet::from(["app".to_string()]),
        )
    }

    fn link_bundled_app(source: &str) -> witchy_syntax::ast::Module {
        let app = parse_module(source).expect("parse bundled consumer app");
        witchy_syntax::linker::link(
            vec![("app".to_string(), app)],
            "app",
            witchy_syntax::linker::ComptimeExpander::new(no_comptime_expansion),
        )
        .expect("link bundled consumer app")
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
        use witchy_types::runtime_type::{
            AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
        };
        use witchy_wir::CanonicalEntryCapability::{BuildEnv, BuildExec, BuildOut};

        let module = parse_module(
            "import option\nfn build(out: BuildOut, env: BuildEnv, dl: BuildNet, cc: BuildExec):\n    let v = match env.get_build_env(\"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"x.witchy\", v + dl.fetch_build(\"127.0.0.1:9\", \"/schema\") + cc.run_tool(\"cat\", \"input\"))\n",
        )
        .expect("parse");
        // The old fixture erased capability receivers and used global staging.
        // It must not regain authority through the raw test-only entry point.
        compile_build_module(&module).expect_unsupported("grantful raw build route is retired");

        // BuildNet is still unsupported by this production baseline. Prove the
        // admitted BuildEnv/BuildExec family through the real source recipe;
        // do not revive its raw ABI merely to keep the old fixture green.
        let source = "import option\nfn build(out: BuildOut, env: BuildEnv, cc: BuildExec):\n    let v = match env.get_build_env(\"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"x.witchy\", v + cc.run_tool(\"cat\", \"input\"))\n";
        let raw_supported = parse_module(source).expect("parse supported build recipe source");
        compile_build_module(&raw_supported)
            .expect_unsupported("supported BuildEnv/BuildExec source must still reject the retired raw route");
        let workspace = PackageCoordinate::new(
            PackageSource::Workspace, "example/build-import-test", "0.1.0",
        ).expect("workspace coordinate");
        let toolchain = PackageCoordinate::new(
            PackageSource::Toolchain, "witchy/stdlib", "0.1.0",
        ).expect("toolchain coordinate");
        let mut assignments = vec![(
            "main".to_owned(),
            ModuleLoadIdentity::new(workspace, ["main"]).expect("main owner"),
        )];
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
            (*name).to_owned(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).expect("std owner"),
        )));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
            .expect("authenticated fixture ownership");
        let users = std::collections::HashSet::from(["main".to_owned()]);
        let recipe = witchy_types::pipeline::check_build_recipe_from_source_bundle(
            vec![("main".to_owned(), source.as_bytes().to_vec())],
            "main", witchy_syntax::linker::ComptimeExpander::new(no_comptime_expansion), &users, owners,
        ).expect("check original build source and generated recipe");
        let artifact = witchy_lower::codegen::compile_checked_build_recipe_artifact(&recipe)
            .expect_lowered("admit grantful build recipe");
        assert_eq!(artifact.entry().expect("build entry").capabilities(), &[BuildOut, BuildEnv, BuildExec]);
        let wasm = artifact.wasm();
        let mut imports = Vec::new();
        for payload in gc_wasm_payloads(&wasm) {
            if let wasmparser::Payload::ImportSection(reader) = payload.expect("valid wasm") {
                for imp in reader.into_imports() {
                    imports.push(imp.expect("import").name.to_string());
                }
            }
        }

        for needed in [
            "build_out_write_at",
            "build_env_at",
            "build_response_is_some",
            "build_read_response_size",
            "build_read_response_into",
            "build_exec_run",
        ] {
            assert!(imports.iter().any(|i| i == needed), "build import `{needed}` missing: {imports:?}");
        }
        for forbidden in [
            "env_len", "env_fill", "exec_run", "exec_run_at", "env_get_at", "env_response_status",
            "net_connect", "net_try_connect",
            "build_out_write", "build_env_len", "build_env_fill", "build_fetch_len",
        ] {
            assert!(
                !imports.iter().any(|i| i == forbidden),
                "build primitive must not lower to runtime import `{forbidden}`: {imports:?}"
            );
        }
        let params = import_param_types(wasm);
        for (name, count) in [("build_out_write_at", 5), ("build_env_at", 3), ("build_exec_run", 5)] {
            let types = params.get(name).expect("required capability import");
            assert_eq!(types.len(), count, "checked slice ABI for {name}");
            assert_eq!(types.first(), Some(&wasmparser::ValType::EXTERNREF),
                "{name} must carry a capability reference, not an integer");
            assert!(types[1..].iter().all(|ty| *ty == wasmparser::ValType::I32),
                "{name} must use explicit pointer/length lanes after its capability");
        }
    }

    #[cfg(feature = "native-test-runtime")]
    fn run_int(src: &str) -> i64 {
        let module = parse_module(src).expect("parse");
        run_int_module(&module)
    }

    #[cfg(feature = "native-test-runtime")]
    fn run_int_with_opt(src: &str, opt: witchy_syntax::opt::OptSet) -> i64 {
        witchy_syntax::opt::set_for_tests(Some(opt));
        let result = run_int(src);
        witchy_syntax::opt::set_for_tests(None);
        result
    }

    #[cfg(feature = "native-test-runtime")]
    fn run_int_module_with_opt(
        module: &witchy_syntax::ast::Module,
        opt: witchy_syntax::opt::OptSet,
    ) -> i64 {
        witchy_syntax::opt::set_for_tests(Some(opt));
        let result = run_int_module(module);
        witchy_syntax::opt::set_for_tests(None);
        result
    }

    #[cfg(feature = "native-test-runtime")]
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

    #[cfg(feature = "native-test-runtime")]
    fn run_int_with_i64_globals(
        source: &str,
        names: &[&str],
    ) -> (i64, std::collections::BTreeMap<String, i64>) {
        let module = parse_module(source).expect("parse counter program");
        run_int_module_with_i64_globals(&module, names)
    }

    #[cfg(feature = "native-test-runtime")]
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

    #[cfg(feature = "native-test-runtime")]
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
    fn borrowed_list_iteration_rejects_until_view_receivers_are_supported() {
        expect_view_rejection(
            r#"
mode opt
type Point packed:
    x: Int
fn view(points: &List(Point)) -> &List(Point):
    points
fn main() -> Int:
    let points = [Point(4), Point(9)]
    let borrowed = view(&points)
    var total = 0
    for point in borrowed:
        total = total + point.x
    total
"#,
            "immutable List iteration remains fail-closed until view receivers are supported",
            "view storage has no recursive checked carrier with a lawful snapshot proof",
        );
    }

    #[test]
    fn borrowed_string_slice_rejects_until_aggregate_view_carriers_are_admitted() {
        let string_module = parse_module(
            witchy_syntax::linker::bundled_source("string").expect("bundled string module"),
        ).expect("parse string module");
        let app = parse_module(r#"
mode opt
import string
fn main() -> Int:
    let owned = "abcdef"
    let view = string.as_str(&owned)
    let s = string.slice(view, 1, 4)
    let owned_again = string.to_string(s)
    string.length(owned_again)
"#).expect("parse slice app");
        let module = link_test_modules(
            vec![("string".into(), string_module), ("app".into(), app)],
            "app",
            &std::collections::HashSet::from(["app".to_string()]),
        );
        let error = compile_module_binary(&module)
            .expect_rejected("String view materialization is fail-closed until aggregate carriers are admitted");
        assert!(
            error.to_string().contains("parameter type erases that relation"),
            "unexpected immutable String view rejection: {error}"
        );
    }

    #[test]
    fn packed_closed_sum_with_nested_custom_equality_rejects_loudly() {
        let source = r#"
mode opt

type Inner packed:
    id: Int
    noise: Int

impl PartialEq for Inner:
    fn eq(let self, let other: Inner) -> Bool:
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

    #[cfg(feature = "native-test-runtime")]
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
                witchy_syntax::linker::ComptimeExpander::new(no_comptime),
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
    #[cfg(feature = "native-test-runtime")]
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

    fn compile_authenticated_dynamic(src: &str) -> LoweringOutcome<Vec<u8>> {
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
            witchy_syntax::linker::ComptimeExpander::new(no_expand),
            owners,
        )
        .expect("authenticated Dynamic checked link");
        compile_checked_module_binary(&checked)
    }

    #[test]
    fn dynamic_descriptor_without_source_witness_rejects_fail_closed() {
        let error = compile_authenticated_dynamic(
            "import dynamic\nimport reflect\n\ntype User:\n    name: String\n    age: Int\n\ntype Box(a):\n    Box(a)\n\nimpl Reflect for User:\n    fn reflect(self) -> reflect.Mirror:\n        reflect.MNil\n\nimpl Reflect for Box(a):\n    fn reflect(self) -> reflect.Mirror:\n        reflect.MNil\n\nfn main(console: Console):\n    let value = dynamic.dynamic(7)\n    console.print(dynamic.type_name(dynamic.type_of(value)))\n    console.print(dynamic.type_name(dynamic.runtime_type(Int)))\n    console.print(dynamic.type_name(dynamic.runtime_type(User)))\n    console.print(dynamic.type_name(dynamic.runtime_type(Box(Int))))\n    console.print(dynamic.type_name(dynamic.runtime_type(.{age: Int, name: String})))\n    console.print(dynamic.type_name(dynamic.runtime_type(.[Count(Int) | Label(String)])))\n    let exact: Option(Int) = dynamic.try_decode(value)\n    match exact:\n        Some(number) -> console.print(\"${number}\")\n        None -> console.print(\"missing-int\")\n    let mismatch: Option(String) = dynamic.try_decode(value)\n    match mismatch:\n        Some(text) -> console.print(text)\n        None -> console.print(\"none\")\n    let decoded: Result(Int, dynamic.DynamicError) = dynamic.decode(value)\n    match decoded:\n        Ok(number) -> console.print(\"decoded-${number}\")\n        Err(_) -> console.print(\"decode-failed\")\n    let wrong: Result(String, dynamic.DynamicError) = dynamic.decode(value)\n    match wrong:\n        Ok(text) -> console.print(text)\n        Err(dynamic.TypeMismatch(actual)) -> console.print(\"mismatch-${dynamic.type_name(actual)}\")\n        Err(_) -> console.print(\"unexpected-dynamic-error\")\n    let person = dynamic.dynamic(User(\"Ada\", 42))\n    let decoded_person: Option(User) = dynamic.try_decode(person)\n    match decoded_person:\n        Some(user) -> console.print(user.name)\n        None -> console.print(\"missing-user\")\n    let words = dynamic.dynamic([\"alpha\", \"beta\"])\n    let decoded_words: Option(List(String)) = dynamic.try_decode(words)\n    match decoded_words:\n        Some(items) -> console.print(list.at(items, 1))\n        None -> console.print(\"missing-words\")\n    let boxed = dynamic.dynamic(Box(11))\n    let decoded_box: Option(Box(Int)) = dynamic.try_decode(boxed)\n    match decoded_box:\n        Some(Box(number)) -> console.print(\"box-${number}\")\n        None -> console.print(\"missing-box\")\n    let record = dynamic.dynamic(.{name: \"Nia\", age: 9})\n    let decoded_record: Option(.{age: Int, name: String}) = dynamic.try_decode(record)\n    match decoded_record:\n        Some(found) -> console.print(found.name)\n        None -> console.print(\"missing-record\")\n    let choice: .[Count(Int) | Label(String)] = .Count(5)\n    let encoded_choice = dynamic.dynamic(choice)\n    let decoded_choice: Option(.[Count(Int) | Label(String)]) = dynamic.try_decode(encoded_choice)\n    match decoded_choice:\n        Some(.Count(number)) -> console.print(\"count-${number}\")\n        Some(.Label(label)) -> console.print(label)\n        None -> console.print(\"missing-choice\")\n",
        );
        let error = error.expect_rejected(
            "Dynamic descriptor aggregate decoding remains fail-closed until its canonical source witness is admitted",
        );
        assert!(
            error.to_string().contains("MissingCheckedSourceWitness"),
            "unexpected Dynamic rejection: {error}"
        );
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

    #[cfg(feature = "native-test-runtime")]
    #[test]
    fn list_element_value_is_not_a_sequence_length_fact() {
        // An element value is arbitrary user data, not a proof that it is a
        // valid index.  Treating `xs[0]` as an upper bound for `xs` would make
        // the optimized loop write past the one-element list instead of taking
        // the same observable bounds trap as the deoptimized path.
        let source = r#"
fn main() -> Int:
    var xs = [99]
    let upper = xs[0]
    var i = 0
    while i < upper:
        xs[i] = 0
        i = i + 1
    0
"#;
        let module = link_list_app(source);
        let default = witchy_syntax::opt::OptSet::default_set();
        let none = witchy_syntax::opt::OptSet::none();
        let optimized = compiled_module_trap_with_opt(&module, default);
        let deoptimized = compiled_module_trap_with_opt(&module, none);
        assert!(optimized.contains("list_at"), "optimized path preserves bounds trap: {optimized}");
        assert!(deoptimized.contains("list_at"), "deoptimized path preserves bounds trap: {deoptimized}");
    }

    #[cfg(feature = "native-test-runtime")]
    #[test]
    fn poisoned_loop_carried_range_value_retains_bounds_trap() {
        // The loop invariant is invalidated by the out-of-domain write before
        // the carried value is refreshed.  A later iteration therefore must
        // retain its checked access and trap rather than treating `99` as an
        // index in the original 0..n value domain.
        let src = r#"
mode opt
import list

fn poisoned_loop(let n: Int) -> Int:
    var source = []
    for i in 0..n:
        list.push(source, i)
    var copied = []
    for i in 0..n:
        list.push(copied, 0)
    for i in 0..n:
        copied[i] = source[i]
    var k = copied[7]
    while k != 0:
        copied[0] = 99
        var lo = 0
        var hi = k
        while lo < hi:
            let a = copied[lo]
            let b = copied[hi]
            copied[lo] = b
            copied[hi] = a
            lo = lo + 1
            hi = hi - 1
        k = copied[0]
    0

fn main() -> Int:
    poisoned_loop(8)
"#;
        let module = link_list_app(src);
        let trap = compiled_module_trap_with_opt(&module, witchy_syntax::opt::OptSet::default_set());
        assert!(trap.contains("list_at"), "poisoned carried value retains its bounds trap: {trap}");
    }

    fn optimized_wir_wat(source: &str, opt: witchy_syntax::opt::OptSet) -> String {
        witchy_syntax::opt::set_for_tests(Some(opt));
        let module = parse_module(source).expect("parse indexed-kernel fixture");
        let wir = assemble_wir_module(&module).expect_lowered("lower indexed-kernel fixture");
        witchy_syntax::opt::set_for_tests(None);
        witchy_wir::wir::to_wat(&wir)
    }

    #[cfg(feature = "native-test-runtime")]
    #[test]
    fn rfc0153_full_deoptimization_matrix_preserves_results_and_traps() {
        // Criterion 9 is intentionally a whole-pipeline check.  The family
        // tests above isolate bounds-elision; this matrix also disables every
        // other optimizer so no RFC-0153 proof leaks into the semantic oracle.
        let optimized = witchy_syntax::opt::OptSet::default_set();
        let deoptimized = witchy_syntax::opt::OptSet::none();
        let result_cases = [
            ("paired binary cursors", r#"
fn main() -> Int:
    var xs = [2, 3, 5, 7, 11]
    var lo = 0
    var hi = list.length(xs) - 1
    while lo < hi:
        let left = xs[lo]
        xs[lo] = xs[hi]
        xs[hi] = left
        lo = lo + 1
        hi = hi - 1
    xs[0] * 100 + xs[4]
"#, 1102),
            ("nested matrix cursor", r#"
fn main() -> Int:
    let row = [2, 3, 5]
    var total = 0
    for row_index in 0..2:
        for column in 0..list.length(row):
            total = total + row[column] + row_index
    total
"#, 23),
            ("masked histogram", r#"
fn main() -> Int:
    var bins = [0, 0, 0, 0]
    let raw = -5
    bins[raw & 3] = 7
    bins[raw & 3]
"#, 7),
            ("bounded serializer", r#"
fn main() -> Int:
    var output = [0, 0, 0, 0]
    var checksum = 0
    for cursor in 0..list.length(output):
        output[cursor] = 65 + cursor
        checksum = checksum + output[cursor]
    checksum
"#, 266),
            ("numeric wrap and early break", r#"
fn main() -> Int:
    var parity = 0
    for i in 0..8:
        parity = parity + i % 2
    var steps = 0
    var cursor = 0
    while cursor < 8:
        if cursor == 3:
            break
        steps = steps + 1
        cursor = cursor + 1
    let wrapped = 9223372036854775807 + 1
    if wrapped < 0: parity + steps else: 0
"#, 7),
            ("empty range", r#"
fn main() -> Int:
    var total = 17
    for i in 4..4:
        total = total + i
    total
"#, 17),
            ("maximum no-wrap loop", r#"
fn main() -> Int:
    var cursor = 9223372036854775806
    var steps = 0
    while cursor < 9223372036854775807:
        cursor = cursor + 1
        steps = steps + 1
    if cursor == 9223372036854775807: steps else: 0
"#, 1),
            ("wrapping loop with break", r#"
fn main() -> Int:
    var cursor = 9223372036854775807
    var steps = 0
    while cursor >= 0:
        cursor = cursor + 1
        steps = steps + 1
        if steps == 1:
            break
    if cursor < 0: steps else: 0
"#, 1),
            ("zero-step false entry", r#"
fn main() -> Int:
    var cursor = 1
    var total = 23
    while cursor < 1:
        total = total + cursor
        cursor = cursor + 0
    total
"#, 23),
            ("same-address forwarding", r#"
fn main() -> Int:
    var values = [3, 2]
    var i = 0
    var total = 0
    while i < 2:
        values[i] = values[i] - 1
        total = total + values[i]
        i = i + 1
    total
"#, 3),
        ];
        for (label, source, expected) in result_cases {
            assert_eq!(run_int_with_opt(source, optimized), expected, "optimized {label}");
            assert_eq!(run_int_with_opt(source, deoptimized), expected, "fully deoptimized {label}");
        }

        let byte_result = link_bytes_app(r#"
import bytes
fn main() -> Int:
    let input = bytes.from_string("ABC")
    var total = 0
    for i in 0..bytes.length(input):
        total = total + bytes.at(input, i)
    total
"#);
        assert_eq!(run_int_module_with_opt(&byte_result, optimized), 198, "optimized byte parser");
        assert_eq!(run_int_module_with_opt(&byte_result, deoptimized), 198, "fully deoptimized byte parser");

        let trap_cases = [
            ("paired binary cursor", r#"
fn main() -> Int:
    let xs = [2, 3]
    let hi = list.length(xs)
    xs[hi]
"#),
            ("matrix width", r#"
fn main() -> Int:
    let row = [7]
    var total = 0
    for column in 0..2:
        total = total + row[column]
    total
"#),
            ("raw histogram index", r#"
fn main() -> Int:
    let bins = [0, 0, 0, 0]
    bins[-1]
"#),
            ("undersized serializer", r#"
fn main() -> Int:
    var output = [0]
    for cursor in 0..2:
        output[cursor] = cursor
    0
"#),
            ("forwarding store", r#"
fn main() -> Int:
    var values = [3]
    values[1] = 0
    values[0]
"#),
        ];
        for (label, source) in trap_cases {
            for (mode, opt) in [("optimized", optimized), ("fully deoptimized", deoptimized)] {
                let trap = compiled_trap(source, opt);
                assert!(trap.contains("list_at"), "{mode} {label} retains its list trap: {trap}");
            }
        }

        let byte_trap = link_bytes_app(r#"
import bytes
fn main() -> Int:
    let input = bytes.from_string("A")
    bytes.at(input, 1)
"#);
        for (mode, opt) in [("optimized", optimized), ("fully deoptimized", deoptimized)] {
            let trap = compiled_module_trap_with_opt(&byte_trap, opt);
            assert!(trap.contains("!main"), "{mode} byte parser retains its trap: {trap}");
        }
    }

    #[test]
    fn brainfuck_dynamic_tape_pointer_retains_byte_bounds_guard() {
        // The repository's Brainfuck interpreter is a real byte consumer, but
        // its tape pointer is controlled by the program being interpreted. It
        // is therefore deliberately a negative RFC-0153 case: optimizing the
        // code cursor must not turn a separate dynamic tape access unchecked.
        let module = link_bytes_app(include_str!("../../../examples/brainfuck/src/brainfuck.witchy"));
        let default = witchy_syntax::opt::OptSet::default_set();
        witchy_syntax::opt::set_for_tests(Some(default));
        let wat = witchy_wir::wir::to_wat(
            &assemble_wir_module(&module).expect_lowered("lower Brainfuck byte fallback"),
        );
        witchy_syntax::opt::set_for_tests(None);
        let run = wat.split("(func $app.run").nth(1).expect("Brainfuck run WAT");
        let run = run.split("(func $").next().expect("Brainfuck run body");
        assert!(run.contains("i32.load8_u"), "Brainfuck still performs byte reads:\n{run}");
        assert!(
            run.contains("call $__witchy_abort"),
            "the dynamic tape pointer keeps its byte bounds guard:\n{run}",
        );
    }

    fn optimizer_policy_payloads(source: &str) -> Vec<Vec<u8>> {
        let module = parse_module(source).expect("parse optimizer-policy fixture");
        let bytes = compile_module_binary(&module).expect_lowered("lower optimizer-policy fixture");
        wasmparser::Parser::new(0)
            .parse_all(&bytes)
            .filter_map(|payload| match payload.expect("valid optimizer-policy wasm") {
                wasmparser::Payload::CustomSection(section)
                    if section.name() == witchy_wir::optimizer_policy::SECTION_NAME =>
                {
                    Some(section.data().to_vec())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn recursive_and_closure_modules_forbid_post_encoding_byte_optimization() {
        let recursive = r#"
fn kernel() -> Int:
    let source = [10, 20, 30]
    var destination = [0, 0, 0]
    for i in 0..2:
        destination[i] = source[i + 1]
    destination[0] + destination[1]

fn recurse(n: Int) -> Int:
    if n <= 0: 0 else: 1 + recurse(n - 1)

fn main() -> Int:
    kernel() + recurse(2)
"#;
        assert_eq!(
            optimizer_policy_payloads(recursive),
            vec![witchy_wir::optimizer_policy::PRESERVE_RAW_PAYLOAD.to_vec()],
            "a source/user call-graph cycle is still a compiler artifact and must not permit byte-level post-admission optimization",
        );

        let closure = r#"
fn main() -> Int:
    let source = [10, 20, 30]
    var destination = [0, 0, 0]
    for i in 0..2:
        destination[i] = source[i + 1]
    let add = fn(n: Int): n + 1
    add(destination[0] + destination[1])
"#;
        assert_eq!(
            optimizer_policy_payloads(closure),
            vec![witchy_wir::optimizer_policy::PRESERVE_RAW_PAYLOAD.to_vec()],
            "a module with lifted closure structure is still a compiler artifact and must not permit byte-level post-admission optimization",
        );
    }

    #[cfg(feature = "native-test-runtime")]
    fn compiled_trap(source: &str, opt: witchy_syntax::opt::OptSet) -> String {
        witchy_syntax::opt::set_for_tests(Some(opt));
        let module = parse_module(source).expect("parse indexed trap fixture");
        let result = compiled_module_trap(&module);
        witchy_syntax::opt::set_for_tests(None);
        result
    }

    #[cfg(feature = "native-test-runtime")]
    fn compiled_module_trap(module: &witchy_syntax::ast::Module) -> String {
        let bytes = compile_module_binary(&module).expect_lowered("lower indexed trap fixture");
        let engine = gc_wasmtime_engine();
        let wt = WtModule::new(&engine, &bytes).expect("valid indexed trap wasm");
        let mut linker = Linker::new(&engine);
        define_abort(&mut linker);
        linker.func_wrap("witchy", "print_int", |_n: i64| {}).unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).expect("instantiate indexed trap");
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("run export")
            .call(&mut store, ())
            .expect_err("fixture must trap")
            .to_string()
    }

    #[cfg(feature = "native-test-runtime")]
    fn compiled_module_trap_with_opt(
        module: &witchy_syntax::ast::Module,
        opt: witchy_syntax::opt::OptSet,
    ) -> String {
        witchy_syntax::opt::set_for_tests(Some(opt));
        let result = compiled_module_trap(module);
        witchy_syntax::opt::set_for_tests(None);
        result
    }

    #[test]
    fn forced_copy_dict_update_without_source_witness_rejects_fail_closed() {
        let default = witchy_syntax::opt::OptSet::default_set();
        witchy_syntax::opt::set_for_tests(Some(default.without(witchy_syntax::opt::Opt::InPlace)));
        let app = parse_module(include_str!("../../../benchmarks/knucleotide.witchy"))
            .expect("parse forced-copy knucleotide fixture");
        let checked = witchy_types::pipeline::link_checked(
            vec![("app".to_string(), app)],
            "app",
            witchy_syntax::linker::ComptimeExpander::new(no_comptime_expansion),
        )
        .expect("link forced-copy knucleotide fixture");
        let result = compile_checked_module_binary(&checked);
        witchy_syntax::opt::set_for_tests(None);
        let error = result.expect_rejected("forced-copy knucleotide remains fail-closed without a checked source witness");
        assert!(
            error.to_string().contains("MissingCheckedSourceWitness"),
            "unexpected forced-copy rejection: {error}"
        );
    }

    #[test]
    fn sequence_forwarding_declines_alias_and_control_boundaries() {
        let different_offset = r#"
fn main() -> Int:
    var values = [3, 2]
    var i = 0
    var total = 0
    while i < 1:
        values[i] = values[i] - 1
        total = total + values[i + 1]
        i = i + 1
    total
"#;
        let offset_wat = optimized_wir_wat(
            different_offset,
            witchy_syntax::opt::OptSet::default_set(),
        );
        assert!(!offset_wat.contains("__seq_forward_"), "a different affine lane is not forwarded");

        let different_root = r#"
fn main() -> Int:
    var left = [3]
    let right = [9]
    var i = 0
    while i < 1:
        left[i] = left[i] - 1
        i = right[i]
    i
"#;
        let root_wat = optimized_wir_wat(
            different_root,
            witchy_syntax::opt::OptSet::default_set(),
        );
        assert!(!root_wat.contains("__seq_forward_"), "a different root is not forwarded");

        let intervening_call = r#"
fn observe() -> ():
    return

fn main() -> Int:
    var values = [3]
    var i = 0
    while i < 1:
        values[i] = values[i] - 1
        observe()
        i = values[i]
    i
"#;
        let call_wat = optimized_wir_wat(
            intervening_call,
            witchy_syntax::opt::OptSet::default_set(),
        );
        assert!(!call_wat.contains("__seq_forward_"), "an intervening call declines forwarding");

        let conditional_store = r#"
fn main() -> Int:
    var values = [3]
    var i = 0
    while i < 1:
        if i == 0:
            values[i] = values[i] - 1
        i = values[i]
    i
"#;
        let conditional_wat = optimized_wir_wat(
            conditional_store,
            witchy_syntax::opt::OptSet::default_set(),
        );
        assert!(!conditional_wat.contains("__seq_forward_"), "a conditional store cannot seed its successor");
    }

    #[test]
    fn sequence_plan_rejects_root_escape_and_length_mutation() {
        let escaped = r#"
fn observe(let values: List(Int)) -> ():
    return

fn main() -> Int:
    let values = [1, 2, 3]
    var total = 0
    for i in 0..list.length(values):
        observe(values)
        total = total + values[i]
    total
"#;
        let escaped_wat = optimized_wir_wat(escaped, witchy_syntax::opt::OptSet::default_set());
        assert!(!escaped_wat.contains("__seq_base_"), "an opaque root call rejects the plan");

        let resized = r#"
import list

fn main() -> Int:
    var values = [1, 2, 3]
    var total = 0
    for i in 0..3:
        values = list.__push(values, i)
        total = total + values[i]
    total
"#;
        let resized_wat = optimized_wir_wat(resized, witchy_syntax::opt::OptSet::default_set());
        assert!(!resized_wat.contains("__seq_base_"), "length mutation rejects the plan");
    }

    #[cfg(feature = "native-test-runtime")]
    #[test]
    fn sequence_affine_offset_overflow_retains_checked_trap() {
        let source = r#"
fn main() -> Int:
    let values = [7]
    var total = 0
    for i in 0..1:
        total = total + values[i + 9223372036854775807]
    total
"#;
        let default = witchy_syntax::opt::OptSet::default_set();
        let optimized = compiled_trap(source, default);
        let deoptimized = compiled_trap(
            source,
            default.without(witchy_syntax::opt::Opt::BoundsElide),
        );
        assert!(optimized.contains("list_at"), "overflowing affine lane keeps checked trap: {optimized}");
        assert!(deoptimized.contains("list_at"), "deoptimized affine lane traps: {deoptimized}");

        let wat = optimized_wir_wat(source, default);
        assert!(wat.contains("__seq_base_"), "the stable-root plan is installed");
        assert!(
            wat.contains("call $list_at"),
            "an offset whose byte displacement overflows cannot enter direct cursor arithmetic:\n{wat}",
        );
    }

    /// Run `src` on the COMPILED backend under a specific optimization set (for value-
    /// parity checks across the `closure-elide` lever).
    #[cfg(feature = "native-test-runtime")]
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

}

#[path = "codegen/canonical_layouts.rs"]
mod canonical_layouts;
#[path = "codegen/canonical_pipeline.rs"]
mod canonical_pipeline;
