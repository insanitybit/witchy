    use super::*;
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

    #[test]
    fn string_operation_catalog_names_live_wir_helpers() {
        use witchy_syntax::intrinsics;

        for name in intrinsics::STRING_OPERATIONS {
            let spec = intrinsics::lookup(name).expect("cataloged string operation");
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
        assert_eq!(
            intrinsics::declared_wir_helper(intrinsics::LIST_PUSH, "list_push_cap"),
            Some("list_push_cap")
        );
        assert_eq!(
            intrinsics::declared_wir_helper(intrinsics::GENERATED_LIST_PUSH, "list_push_cap"),
            Some("list_push_cap")
        );
    }

    #[test]
    fn math_operation_catalog_names_live_wir_helpers() {
        use witchy_syntax::intrinsics;

        for name in intrinsics::MATH_OPERATIONS {
            let spec = intrinsics::lookup(name).expect("cataloged math operation");
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
    fn list_operation_catalog_names_live_wir_helpers() {
        use witchy_syntax::intrinsics;

        for name in intrinsics::LIST_OPERATIONS {
            let spec = intrinsics::lookup(name).expect("cataloged list operation");
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
    fn dict_operation_catalog_names_live_wir_helpers() {
        use witchy_syntax::intrinsics;

        for name in intrinsics::DICT_OPERATIONS {
            let spec = intrinsics::lookup(name).expect("cataloged dict operation");
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
        assert_eq!(
            intrinsics::declared_wir_helper(intrinsics::DICT_INSERT, "dict_insert_cap"),
            Some("dict_insert_cap")
        );
        assert_eq!(
            intrinsics::declared_wir_helper(intrinsics::DICT_UPDATE, "dict_update_cap"),
            Some("dict_update_cap")
        );
    }

    #[test]
    fn crypto_operation_catalog_names_live_wir_helpers() {
        use witchy_syntax::intrinsics;

        for name in intrinsics::CRYPTO_OPERATIONS {
            let spec = intrinsics::lookup(name).expect("cataloged crypto operation");
            assert!(
                witchy_types::typeck::intrinsic(name),
                "{} must not compile its native std placeholder",
                spec.name
            );
            let helper = intrinsics::sole_wir_helper(name)
                .expect("every crypto operation has one WIR helper");
            assert!(
                witchy_wir::wir_helpers::wir_helper(helper).is_some(),
                "{} names missing WIR helper {}",
                spec.name,
                helper
            );
        }
    }

    #[test]
    fn regex_operation_catalog_names_its_native_wir_helper() {
        use witchy_syntax::intrinsics;

        let spec = intrinsics::lookup(intrinsics::REGEX_MATCH_SPANS)
            .expect("cataloged regex operation");
        assert!(witchy_types::typeck::intrinsic(spec.name));
        let helper = intrinsics::sole_wir_helper(spec.name)
            .expect("regex operation has one WIR helper");
        assert_eq!(helper, "regex_match_spans");
        assert!(witchy_wir::wir_helpers::wir_helper(helper).is_some());
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
        let bytes = compile_module_binary(&module)
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

    /// Run a float program with a capturing `print_float`.
    fn run_float(src: &str) -> f64 {
        let module = parse_module(src).expect("parse");
        let bytes = compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let engine = gc_wasmtime_engine();
        let wt = WtModule::new(&engine, &bytes).expect("valid wasm");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        define_abort(&mut linker);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_float", move |x: f64| {
                *sink.lock().unwrap() = Some(x);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().take().expect("printed a float")
    }

    #[test]
    fn compiles_floats() {
        let src = r#"
fn half(x: Float) -> Float:
    (x / 2.0)

fn main() -> Float:
    (half(7.0) + 1.5)
"#;
        assert_eq!(run_float(src), 5.0); // 3.5 + 1.5
    }

    #[test]
    fn float_valued_if_compiles() {
        // An `if/else` whose branches are Float must yield an f64 result (the
        // `if` result type follows the branch kind, not a hardcoded i32).
        let src = r#"
fn pick(a: Float, b: Float) -> Float:
    if (a < b):
        a
    else:
        b

fn main() -> Float:
    (pick(2.5, 7.5) + pick(9.0, 1.0))
"#;
        assert_eq!(run_float(src), 3.5); // min(2.5,7.5)=2.5 + min(9.0,1.0)=1.0
    }

    #[test]
    fn large_int_literals_compile() {
        // Compiled Int is i64, so a literal beyond the 32-bit range round-trips
        // (it no longer wraps or is rejected), matching the interpreter.
        assert_eq!(run_int("fn main() -> Int:\n    3000000000\n"), 3_000_000_000);
        assert_eq!(
            run_int("fn main() -> Int:\n    9000000000000\n"),
            9_000_000_000_000
        );
    }

    #[test]
    fn integer_min_remainder_negative_one_is_zero() {
        assert_eq!(
            run_int(
                "fn main() -> Int:\n    let min = (0 - 9223372036854775807) - 1\n    min % (0 - 1)\n"
            ),
            0
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
    fn nested_dict_view_roots_the_returned_subplace_layout() {
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
        assert!(
            before.rfind("local.get $v").is_some_and(|view| {
                before.rfind("local.get $holder").is_none_or(|owner| view > owner)
            }),
            "the Dict -4 bias must apply to the returned view pointer, not Holder: {count}",
        );
        assert!(
            before[root.saturating_sub(180)..].contains("i32.const 4"),
            "the returned Dict layout, not the Holder parameter layout, selects the root bias: {count}",
        );
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
    fn float_record_field_compiles() {
        // 8-byte heap slots hold an f64 field; float_to_int reads it back.
        let src = r#"
type Vec2:
    x: Float
    y: Float

fn main() -> Int:
    let v = Vec2(1.5, 2.5)
    math.to_int((v).x)
"#;
        assert_eq!(run_int(src), 1);
    }

    #[test]
    fn float_list_element_compiles() {
        // 8-byte heap slots hold an f64, so floats now live in lists.
        let src = r#"
fn main() -> Int:
    let xs = [1.5, 2.5]
    list.length(xs)
"#;
        assert_eq!(run_int(src), 2);
    }

    #[test]
    fn compiles_non_capturing_closure() {
        // A non-capturing lambda passed to a higher-order function: lifted to a
        // table slot, then invoked via `call_indirect`.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    apply(fn(n: Int): (n * n), 9)
"#;
        assert_eq!(run_int(src), 81);
    }

    #[test]
    fn compiles_multiple_closures() {
        // Two distinct lambdas take distinct table slots and call_indirect each.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let a = apply(fn(n: Int): (n + 1), 10)
    let b = apply(fn(n: Int): (n * 3), 10)
    (a + b)
"#;
        assert_eq!(run_int(src), 41); // 11 + 30
    }

    #[test]
    fn closure_can_call_global_function() {
        // A lambda calling a top-level function is still non-capturing.
        let src = r#"
fn dbl(x: Int) -> Int:
    (x * 2)

fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    apply(fn(n: Int): (dbl(n) + 1), 4)
"#;
        assert_eq!(run_int(src), 9); // dbl(4) + 1
    }

    #[test]
    fn lambda_diagnostic_identity_includes_its_source_owner() {
        let module = parse_module("fn main():\n    let f = fn(n: Int): n + 1\n    f(1)\n").unwrap();
        let Item::Function(main) = &module.items[0] else { panic!("function") };
        let Stmt::Let { value: Expr::Lambda { params, body, .. }, .. } = &main.body.stmts[0] else {
            panic!("lambda binding")
        };

        assert_ne!(
            Codegen::lambda_content_key("left.make", params, body),
            Codegen::lambda_content_key("right.make", params, body)
        );
    }

    #[test]
    fn compiles_capturing_closure() {
        // The lambda reads `k` from the enclosing scope: captured by value into
        // the closure's heap environment, then read back via the env prologue.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let k = 100
    apply(fn(n: Int): (n + k), 5)
"#;
        assert_eq!(run_int(src), 105);
    }

    #[test]
    fn lambda_scope_swap_isolates_per_unit_state() {
        let type_table = witchy_types::typeck::TypeTable::default();
        let mut cg = Codegen::new(&type_table, witchy_types::loans::LoanFacts::default());
        let outer = "outer".to_string();
        let inner = "inner".to_string();

        cg.field_caps.insert(outer.clone());
        cg.field_push_safe.insert((outer.clone(), "items".into()));
        cg.local_types.insert(outer.clone(), Type::Named("Int".into(), vec![]));
        cg.local_dict_value_valtype.insert(outer.clone(), ValType::Int);
        cg.local_dict_key_valtype.insert(outer.clone(), ValType::Str);
        cg.local_list_elem_list_valtype.insert(outer.clone(), ValType::Bool);
        cg.local_list_nesting.insert(outer.clone(), (2, NestBottom::Scalar(ValType::Int)));

        let saved = cg.swap_out_scope();
        assert!(cg.field_caps.is_empty());
        assert!(cg.field_push_safe.is_empty());
        assert!(cg.local_types.is_empty());
        assert!(cg.local_dict_value_valtype.is_empty());
        assert!(cg.local_dict_key_valtype.is_empty());
        assert!(cg.local_list_elem_list_valtype.is_empty());
        assert!(cg.local_list_nesting.is_empty());

        cg.field_caps.insert(inner.clone());
        cg.field_push_safe.insert((inner.clone(), "items".into()));
        cg.local_types.insert(inner.clone(), Type::Named("String".into(), vec![]));
        cg.local_dict_value_valtype.insert(inner.clone(), ValType::Str);
        cg.local_dict_key_valtype.insert(inner.clone(), ValType::Int);
        cg.local_list_elem_list_valtype.insert(inner.clone(), ValType::Float);
        cg.local_list_nesting.insert(inner.clone(), (3, NestBottom::Scalar(ValType::Str)));
        cg.restore_scope(saved);

        assert!(cg.field_caps.contains(&outer) && !cg.field_caps.contains(&inner));
        assert!(cg.field_push_safe.contains(&(outer.clone(), "items".into())));
        assert!(!cg.field_push_safe.contains(&(inner.clone(), "items".into())));
        assert!(cg.local_types.contains_key(&outer) && !cg.local_types.contains_key(&inner));
        assert!(cg.local_dict_value_valtype.contains_key(&outer));
        assert!(!cg.local_dict_value_valtype.contains_key(&inner));
        assert!(cg.local_dict_key_valtype.contains_key(&outer));
        assert!(!cg.local_dict_key_valtype.contains_key(&inner));
        assert!(cg.local_list_elem_list_valtype.contains_key(&outer));
        assert!(!cg.local_list_elem_list_valtype.contains_key(&inner));
        assert!(cg.local_list_nesting.contains_key(&outer));
        assert!(!cg.local_list_nesting.contains_key(&inner));
    }

    #[test]
    fn captured_local_metadata_is_reinstalled_explicitly() {
        let type_table = witchy_types::typeck::TypeTable::default();
        let mut cg = Codegen::new(&type_table, witchy_types::loans::LoanFacts::default());
        let name = "captured".to_string();

        cg.locals.insert(name.clone(), Kind::I64);
        cg.local_records.insert(name.clone(), "Record".into());
        cg.local_list_elem.insert(name.clone(), "Element".into());
        cg.local_payload_records.insert(name.clone(), "Payload".into());
        cg.local_val_types.insert(name.clone(), ValType::Int);
        cg.local_types.insert(name.clone(), Type::Named("Int".into(), vec![]));
        cg.local_list_elem_valtype.insert(name.clone(), ValType::Float);
        cg.local_list_elem_tuple
            .insert(name.clone(), vec![ValType::Int, ValType::Str]);
        cg.local_tuple_slots.insert(name.clone(), vec![ValType::Bool]);
        cg.local_shape.insert(name.clone(), EqShape::List(Box::new(EqShape::Int)));
        cg.local_payload_valtype.insert(name.clone(), ValType::Str);
        cg.local_dict_value_valtype.insert(name.clone(), ValType::Int);
        cg.local_dict_key_valtype.insert(name.clone(), ValType::Str);
        cg.local_list_elem_list_valtype.insert(name.clone(), ValType::Bool);
        cg.local_list_nesting
            .insert(name.clone(), (2, NestBottom::Scalar(ValType::Int)));
        cg.local_fn_ret_kind.insert(name.clone(), Kind::F64);

        let capture = cg.capture_info(&name);
        let _saved = cg.swap_out_scope();
        cg.install_capture_info(&capture);

        assert!(cg.locals.get(&name) == Some(&Kind::I64));
        assert_eq!(cg.local_records.get(&name).map(String::as_str), Some("Record"));
        assert_eq!(cg.local_list_elem.get(&name).map(String::as_str), Some("Element"));
        assert_eq!(cg.local_payload_records.get(&name).map(String::as_str), Some("Payload"));
        assert!(cg.local_val_types.get(&name) == Some(&ValType::Int));
        assert_eq!(cg.local_types.get(&name), Some(&Type::Named("Int".into(), vec![])));
        assert!(cg.local_list_elem_valtype.get(&name) == Some(&ValType::Float));
        assert!(cg.local_list_elem_tuple.get(&name) == Some(&vec![ValType::Int, ValType::Str]));
        assert!(cg.local_tuple_slots.get(&name) == Some(&vec![ValType::Bool]));
        assert!(cg.local_shape.get(&name) == Some(&EqShape::List(Box::new(EqShape::Int))));
        assert!(cg.local_payload_valtype.get(&name) == Some(&ValType::Str));
        assert!(cg.local_dict_value_valtype.get(&name) == Some(&ValType::Int));
        assert!(cg.local_dict_key_valtype.get(&name) == Some(&ValType::Str));
        assert!(cg.local_list_elem_list_valtype.get(&name) == Some(&ValType::Bool));
        assert!(
            cg.local_list_nesting.get(&name)
                == Some(&(2, NestBottom::Scalar(ValType::Int)))
        );
        assert!(cg.local_fn_ret_kind.get(&name) == Some(&Kind::F64));
    }

    #[test]
    fn top_level_function_clears_payload_record_refinements() {
        let module = parse_module("fn clean() -> Int:\n    0\n").expect("parse");
        let Item::Function(function) = &module.items[0] else {
            panic!("function item")
        };
        let type_table = witchy_types::typeck::TypeTable::default();
        let mut cg = Codegen::new(&type_table, witchy_types::loans::LoanFacts::default());
        cg.local_payload_records.insert("stale".into(), "Record".into());

        cg.compile_function(function).expect("compile clean function");

        assert!(!cg.local_payload_records.contains_key("stale"));
    }

    #[test]
    fn closure_captures_multiple_vars() {
        // Several captures land in distinct environment slots.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let a = 3
    let b = 7
    let c = 11
    apply(fn(n: Int): (((n * a) + b) - c), 10)
"#;
        assert_eq!(run_int(src), 26); // 10*3 + 7 - 11
    }

    #[test]
    fn closure_captures_record_field() {
        // Capturing a record value: the env carries the heap pointer, and field
        // access still resolves inside the lambda.
        let src = r#"
type Point:
    x: Int
    y: Int

fn apply(f: fn(Int) -> Int, n: Int) -> Int:
    f(n)

fn main() -> Int:
    let p = Point(4, 9)
    apply(fn(n: Int): (n + ((p).x * (p).y)), 1)
"#;
        assert_eq!(run_int(src), 37); // 1 + 4*9
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
            err.to_string().contains("assigns `total`"),
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
import vm

fn worker(dir: Dir, input: Bytes) -> Bytes:
    input

fn main(dir: Dir, input: Bytes) -> Bytes:
    let callback = worker
    vm.with_dir(dir, callback, input)
"#,
            ),
            (
                "vm.serve",
                r#"
import vm

fn worker(state: Bytes, request: Bytes) -> Bytes:
    state

fn main(init: Bytes, requests: List(Bytes)) -> List(Bytes):
    let callback = worker
    vm.serve(init, requests, callback)
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

    fn run_str(src: &str) -> Vec<String> {
        let module = parse_module(src).expect("parse");
        let bytes = compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().clone()
    }

    #[test]
    fn compiles_arithmetic() {
        assert_eq!(run_int(r#"
fn main() -> Int:
    (1 + (2 * 3))
"#), 7);
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
    fn full_int_program() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn fib(n: Int) -> Int:
    if (n < 2):
        n
    else:
        (fib((n - 1)) + fib((n - 2)))

fn main() -> Int:
    let a = double(21)
    let b = fib(10)
    (a + b)
"#;
        assert_eq!(run_int(src), 97);
    }

    #[test]
    fn compiles_int_float_conversions() {
        // math.to_float(7) / 2.0 = 3.5; math.to_int(3.5) = 3
        assert_eq!(
            run_int("fn main() -> Int:\n    math.to_int(math.to_float(7) / 2.0)\n"),
            3
        );
    }

    #[test]
    fn compiles_string_length() {
        assert_eq!(run_int(r#"
fn main() -> Int:
    string.length("hello")
"#), 5);
    }

    #[test]
    fn compiles_while_and_mod() {
        // sum of multiples of 3 below 10: 0 + 3 + 6 + 9
        let src = r#"
fn main() -> Int:
    var i = 0
    var total = 0
    while (i < 10):
        if ((i % 3) == 0):
            total = (total + i)
        i = (i + 1)
    total
"#;
        assert_eq!(run_int(src), 18);
    }

    #[test]
    fn compiles_boolean_ops() {
        let src = r#"
fn in_range(n: Int) -> Int:
    if ((n > 0) && (n < 10)):
        1
    else:
        0

fn main() -> Int:
    ((in_range(5) + in_range(50)) + in_range((-3)))
"#;
        assert_eq!(run_int(src), 1); // 1 + 0 + 0
    }

    #[test]
    fn compiles_boolean_not() {
        assert_eq!(run_int("fn main() -> Int:\n    if !(1 == 2): 7 else: 0\n"), 7);
    }

    #[test]
    fn compiles_match_with_guards() {
        let src = r#"
fn sign(n: Int) -> Int:
    match n:
        0 -> 0
        m if (m > 0) -> 1
        _ -> (0 - 1)

fn main() -> Int:
    ((sign(5) + sign((-3))) + sign(0))
"#;
        assert_eq!(run_int(src), 0); // 1 + (-1) + 0
    }

    #[test]
    fn compiles_adts_and_constructor_patterns() {
        // Constructors become heap records [tag][fields...]; ctor patterns load
        // the tag and bind fields.
        let src = r#"
type Shape:
    Circle(Int)
    Square(Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> ((3 * r) * r)
        Square(w) -> (w * w)

fn main() -> Int:
    (area(Circle(10)) + area(Square(5)))
"#;
        assert_eq!(run_int(src), 325);
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
    fn compiles_lists() {
        let src = r#"
fn main() -> Int:
    let xs = [10, 20, 30]
    ((list.length(xs) + list.at(xs, 0)) + list.at(xs, 2))
"#;
        assert_eq!(run_int(src), 43); // 3 + 10 + 30
    }

    #[test]
    fn compiles_nested_constructor_patterns() {
        let src = r#"
type Point:
    Point(Int, Int)

type Shape:
    Dot(Point)
    Pair(Point, Point)

fn x_of(s: Shape) -> Int:
    match s:
        Dot(Point(x, _)) -> x
        Pair(Point(x, _), _) -> x

fn main() -> Int:
    (x_of(Dot(Point(7, 9))) + x_of(Pair(Point(3, 0), Point(0, 0))))
"#;
        assert_eq!(run_int(src), 10); // 7 + 3
    }

    #[test]
    fn compiles_string_patterns() {
        let src = r#"
fn classify(s: String) -> Int:
    match s:
        "yes" -> 1
        "no" -> 0
        _ -> (0 - 1)

fn main() -> Int:
    ((classify("yes") + classify("no")) + classify("maybe"))
"#;
        assert_eq!(run_int(src), 0); // 1 + 0 + (-1)
    }

    #[test]
    fn compiles_match_and_recursion() {
        let src = r#"
fn fact(n: Int) -> Int:
    match n:
        0 -> 1
        _ -> (n * fact((n - 1)))

fn main() -> Int:
    fact(5)
"#;
        assert_eq!(run_int(src), 120);
    }

    #[test]
    fn compiles_deep_self_tail_recursion_without_wasm_stack_growth() {
        let src = r#"
fn swap_down(n: Int, a: Int, b: Int) -> Int:
    if (n == 0):
        ((a * 10) + b)
    else:
        swap_down((n - 1), b, a)

fn main() -> Int:
    swap_down(1000001, 2, 7)
"#;
        assert_eq!(run_int(src), 72);
    }

    #[test]
    fn compiles_deep_mutual_tail_recursion_without_wasm_stack_growth() {
        let src = r#"
fn even(own n: Int) -> Int:
    if n == 0:
        1
    else:
        odd(n - 1)

fn odd(own n: Int) -> Int:
    if n == 0:
        0
    else:
        even(n - 1)

fn main() -> Int:
    even(5000000)
"#;
        assert_eq!(run_int(src), 1);
    }

    #[test]
    fn compiles_nested_explicit_return_as_a_tail_edge() {
        let src = r#"
fn down(n: Int) -> Int:
    if (n > 0):
        return down((n - 1))
    9

fn main() -> Int:
    down(5000000)
"#;
        assert_eq!(run_int(src), 9);
    }

    #[test]
    fn compiles_match_arm_self_tail_edge_without_erasing_result_block() {
        let src = r#"
fn down(n: Int) -> Int:
    match n:
        0 -> 9
        _ -> down((n - 1))

fn main() -> Int:
    down(5000000)
"#;
        assert_eq!(run_int(src), 9);
    }

    #[test]
    fn compiles_coalesce_fallback_as_a_tail_edge() {
        let src = r#"
fn down(n: Int) -> Int:
    (if (n == 0): Some(9) else: None) ?? down((n - 1))

fn main() -> Int:
    down(5000000)
"#;
        assert_eq!(run_int(src), 9);
    }

    #[test]
    fn compiles_var_writeback() {
        // `var` compiles to move-in / move-out: bump returns the updated n,
        // and the caller writes it back into x.
        let src = r#"
fn bump(var n: Int):
    n = (n + 1)

fn main() -> Int:
    var x = 41
    bump(x)
    bump(x)
    x
"#;
        assert_eq!(run_int(src), 43);
    }

    #[test]
    fn compiles_string_concatenation() {
        let src = r#"
fn shout(name: String) -> String:
    ("hello, " + name)

fn main(console: Console):
    console.print(shout("witchy"))
"#;
        assert_eq!(run_str(src), vec!["hello, witchy"]);
    }

    #[test]
    fn compiles_int_to_string() {
        let src = r#"
fn main(console: Console):
    console.print("${12345}")
"#;
        assert_eq!(run_str(src), vec!["12345"]);
    }

    #[test]
    fn int_to_string_handles_zero() {
        let src = r#"
fn main(console: Console):
    console.print("${0}")
"#;
        assert_eq!(run_str(src), vec!["0"]);
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
