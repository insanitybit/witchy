    use super::*;
    use crate::wir::{
        BinOp, ClosureSignature, DataSegment, GlobalInit, Kind, UnOp, WirExpr, WirFunc, WirGlobal,
        WirImport, WirLocal, WirModule, WirNode, WirTable, WirTy, closure_wrapper_struct,
        slot_closure_signature,
    };
    use std::sync::{Arc, Mutex};

    fn local(name: &str, ty: WirTy) -> WirLocal {
        WirLocal { name: name.into(), ty }
    }

    /// Instantiate a wasm binary and run its `run` export, capturing scalar and
    /// string output as ordered lines. (Copied from `wir.rs`'s test setup.)
    fn run_binary(binary: &[u8]) -> Vec<String> {
        // Fuel-capped so a buggy helper loop TRAPS fast instead of hanging the
        // suite (a runaway $find_byte/$str_eq once spun a test for 70 minutes).
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config).expect("engine");
        let m = wasmtime::Module::new(&engine, binary)
            .unwrap_or_else(|e| panic!("encoded module invalid: {e:#}"));
        let out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = wasmtime::Linker::new(&engine);
        let o = out.clone();
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
                o.lock().unwrap().push(n.to_string());
            })
            .unwrap();
        let o = out.clone();
        linker
            .func_wrap("witchy", "print_float", move |n: f64| {
                o.lock().unwrap().push(n.to_string());
            })
            .unwrap();
        let o = out.clone();
        linker
            .func_wrap(
                "witchy",
                "print",
                move |mut caller: wasmtime::Caller<'_, ()>, ptr: i32, len: i32| {
                    let mem = caller.get_export("memory").and_then(|e| e.into_memory()).unwrap();
                    let data = mem.data(&caller);
                    let s = String::from_utf8_lossy(&data[ptr as usize..(ptr + len) as usize])
                        .into_owned();
                    o.lock().unwrap().push(s);
                },
            )
            .unwrap();
        // (RFC-0045) `__witchy_abort` is always linked; a helper that routes an
        // abort through it (e.g. `str_to_int`) declares the import, so define a
        // trapping stub matching the real host's never-returns contract.
        linker
            .func_wrap(
                "witchy",
                "__witchy_abort",
                |_: i32, _: i64, _: i64, _: i32| -> wasmtime::Result<()> {
                    wasmtime::bail!("runtime error (test harness abort)")
                },
            )
            .unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        store.set_fuel(500_000_000).expect("fuel"); // ~5e8 ops — ample for tests, traps runaways
        let inst = linker.instantiate(&mut store, &m).expect("instantiate");
        let run = inst.get_typed_func::<(), ()>(&mut store, "run").expect("run export");
        run.call(&mut store, ()).expect("run (or fuel-exhausted — likely a runaway loop)");
        out.lock().unwrap().clone()
    }

    /// Run a module via the binary encoder.
    fn run_encoded(module: &WirModule) -> Vec<String> {
        run_binary(&encode(module, &[]))
    }

    /// Assert the encoder output runs identically to the expected lines. (Was
    /// also a binary-vs-`to_wat` agreement gate; the WAT leg is retired with the
    /// `wat` crate — `to_wat` is now only emit-wat's display, not an exec path.)
    fn assert_agrees(module: &WirModule, expected: &[&str]) {
        let exp: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(run_encoded(&with_rc_floor(module)), exp, "binary output mismatch");
    }

    /// Assert the module TRAPS when run (e.g. the RFC-0005 in-place bounds check fires).
    fn assert_traps(module: &WirModule) {
        let binary = encode(&with_rc_floor(module), &[]);
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        let engine = wasmtime::Engine::new(&config).expect("engine");
        let m = wasmtime::Module::new(&engine, &binary).expect("encoded module invalid");
        let mut linker = wasmtime::Linker::new(&engine);
        linker.func_wrap("witchy", "print_int", |_: i64| {}).unwrap();
        linker
            .func_wrap("witchy", "print", |_: wasmtime::Caller<'_, ()>, _: i32, _: i32| {})
            .unwrap();
        linker
            .func_wrap(
                "witchy",
                "__witchy_abort",
                |_: i32, _: i64, _: i64, _: i32| -> wasmtime::Result<()> {
                    wasmtime::bail!("runtime error (test harness abort)")
                },
            )
            .unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        store.set_fuel(500_000_000).expect("fuel");
        let inst = linker.instantiate(&mut store, &m).expect("instantiate");
        let run = inst.get_typed_func::<(), ()>(&mut store, "run").expect("run export");
        assert!(
            run.call(&mut store, ()).is_err(),
            "expected a trap, but the module ran to completion"
        );
    }

    /// The transitive helper closure of `root` (itself + every helper it calls, via the
    /// registry's `helper_deps`) — so a synthetic test can pull a helper with a large
    /// dependency chain (e.g. `dict_insert_cap` -> `dict_find`/`dict_index_put`/
    /// `key_eq`/`str_eq`/…) without hand-listing every dep.
    fn helper_closure(root: &str) -> Vec<WirFunc> {
        let mut seen = std::collections::BTreeSet::new();
        let mut queue = vec![root.to_string()];
        let mut out = vec![];
        while let Some(n) = queue.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            if let Some(spec) = crate::wir_helpers::wir_helper(&n) {
                for d in spec.helper_deps {
                    queue.push((*d).to_string());
                }
                out.push(spec.func);
            }
        }
        out
    }

    /// (RFC-0016) The list/string/dict/`mk` allocators are routed through `$rc_alloc`,
    /// so a hand-assembled module using one needs the allocator + its globals present.
    /// Inject them idempotently (by name, so func/global resolution is unaffected and
    /// runtime output is unchanged) — only when a routed helper is actually present —
    /// so each test need not list `rc_alloc`/`rc_freelist`/`__rc_reused_bytes` itself.
    fn with_rc_floor(module: &WirModule) -> WirModule {
        const RC_USERS: &[&str] = &[
            "substr", "concat", "list_push", "list_concat", "ascii_case", "dict_new", "dict_remove",
            "dict_insert", "dict_keys", "dict_values", "dict_pairs",
            "list_push_cap", "list_set_cap", "list_update_cap", "str_append_cap", "list_drop",
            "dict_insert_cap",
            "int_to_string", "split", "str_chars",
            // batch 3: host-import + worst-case string/list producers (all route through rc_alloc)
            "replace", "encoding", "dir_read", "file_read", "exec", "crypto_reveal", "build_read",
            "regex_match_spans", "dir_list", "net_resolve", "get_env", "float_to_str", "string_from_code", "build_args",
            "crypto_sha256", "crypto_sha512", "crypto_sha3_256", "crypto_hmac_sha256", "crypto_rune_hash",
            "crypto_sign", "crypto_public_key", "compiler_footprint", "compiler_diff", "compiler_doc",
            "compiler_doc_result_json",
            "net_recv_line", "net_recv_all", "net_recv_bytes",
            "vm_par_map", "vm_par_map_bytes", "vm_serve", "vm_with_dir",
        ];
        let mut m = module.clone();
        let uses_rc = m.funcs.iter().any(|f| RC_USERS.contains(&f.name.as_str()));
        if uses_rc && !m.funcs.iter().any(|f| f.name == "rc_alloc") {
            if !m.funcs.iter().any(|f| f.name == "ensure") {
                m.funcs.insert(0, crate::wir_helpers::ensure_helper(false));
            }
            let pos = m.funcs.len().saturating_sub(1); // before the trailing `run`
            m.funcs.insert(pos, crate::wir_helpers::rc_alloc_helper());
        }
        // (RFC-0051 I2) `$rc_alloc`'s bump-miss path (and `$dict_insert_cap`'s index
        // rebuild) delegate to `$bump_alloc`, the single ensure-prefixed allocator.
        if uses_rc && !m.funcs.iter().any(|f| f.name == "bump_alloc") {
            let pos = m.funcs.len().saturating_sub(1);
            m.funcs.insert(pos, crate::wir_helpers::bump_alloc_helper());
        }
        if uses_rc {
            for (name, kind, init) in [
                ("heap", Kind::I32, GlobalInit::I32(1024)),
                ("rc_freelist", Kind::I32, GlobalInit::I32(0)),
                ("__rc_reused_bytes", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_live_cells", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_rc_alloc_calls", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_bump_alloc_calls", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_rc_reuse_calls", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_rc_free_calls", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_region_rewind_calls", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_extract_active", Kind::I32, GlobalInit::I32(0)),
                ("__witchy_extract_searches", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_extract_key_comparisons", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_extract_copied_bytes", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_extract_retains", Kind::I64, GlobalInit::I64(0)),
                ("__witchy_extract_drops", Kind::I64, GlobalInit::I64(0)),
            ] {
                if !m.globals.iter().any(|g| g.name == name) {
                    m.globals.push(WirGlobal { name: name.into(), kind, mutable: true, init, export: None });
                }
            }
            if !m.globals.iter().any(|g| g.name == "heap_base") {
                m.globals.push(WirGlobal {
                    name: "heap_base".into(),
                    kind: Kind::I32,
                    mutable: false,
                    init: GlobalInit::I32(1024),
                    export: None,
                });
            }
        }
        m
    }

    /// Module with one Int-returning func + a `run` that prints its result.
    /// (Mirrors `wir.rs`'s `int_demo`.)
    fn int_demo(f: WirFunc, call: WirExpr) -> WirModule {
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![call],
            })],
            raw_body: None,
        };
        WirModule {
            imports: vec![
                WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] },
                WirImport {
                    name: "print".into(),
                    params: vec![Kind::I32, Kind::I32],
                    results: vec![],
                },
            ],
            funcs: vec![f, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        }
    }


    mod runtime_helpers {
        use super::*;
        include!("wir_encode_tests/runtime_helpers.rs");
    }

    mod core {
        use super::*;
        include!("wir_encode_tests/core.rs");
    }
