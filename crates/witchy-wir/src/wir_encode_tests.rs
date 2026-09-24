    use super::*;
    use crate::wir::{
        GlobalInit, Kind, WirExpr, WirFunc, WirGlobal,
        WirImport, WirLocal, WirModule, WirNode, WirTy,
    };

    fn local(name: &str, ty: WirTy) -> WirLocal {
        WirLocal { name: name.into(), ty }
    }

    /// Assert the module TRAPS when run (e.g. the RFC-0005 in-place bounds check fires).
    fn assert_traps(module: &WirModule) {
        let binary = crate::wir_encode::try_encode_with_gc_source_map(
            &with_rc_floor(module),
            &[],
            &[],
        )
        .expect("the migration bridge encodes a helper-bearing module")
        .wasm;
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        config.wasm_simd(true);
        config.wasm_relaxed_simd(true);
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
                ("__witchy_rc_alloc_requested_bytes", Kind::I64, GlobalInit::I64(0)),
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


    mod runtime_helpers {
        use super::*;
        include!("wir_encode_tests/runtime_helpers.rs");
    }

    mod core {
        use super::*;
        include!("wir_encode_tests/core.rs");
    }
