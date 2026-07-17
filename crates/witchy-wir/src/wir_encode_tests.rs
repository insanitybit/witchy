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

    /// Instantiate a wasm binary and run its `run` export, capturing `print_int`
    /// and `print` output as ordered lines. (Copied from `wir.rs`'s test setup.)
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

    /// The #35 memory primitives — Store / Load(offset) / MemoryCopy / MemoryFill /
    /// MemorySize / MemoryGrow and an unsigned compare — encode AND print-via-WAT
    /// identically and run correctly. These are the nodes the allocation helpers
    /// ($ensure/$concat/$mkN) lower to.
    #[test]
    fn memory_ops_roundtrip() {
        use WirExpr::*;
        let i32c = |n: i32| ConstI32(n);
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                // mem[16] = 99 (i64), then copy those 8 bytes to mem[32].
                WirNode::Store { ptr: i32c(16), value: ConstI64(99), kind: Kind::I64, offset: 0 },
                WirNode::MemoryCopy { dest: i32c(32), src: i32c(16), len: i32c(8) },
                // print the copied value (99).
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Load { ptr: Box::new(i32c(32)), kind: Kind::I64, offset: 0 }],
                }),
                // memory.size >u 0  →  1.
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Binary {
                            op: BinOp::GtU,
                            kind: Kind::I32,
                            lhs: Box::new(MemorySize),
                            rhs: Box::new(i32c(0)),
                        }),
                    }],
                }),
                // memory.grow(1) returns the previous size in pages (1).
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(MemoryGrow(Box::new(i32c(1)))),
                    }],
                }),
                // memory.fill mem[64..72] = 0, then read one byte back (0).
                WirNode::MemoryFill { dest: i32c(64), value: i32c(0), len: i32c(8) },
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Load { ptr: Box::new(i32c(64)), kind: Kind::I32, offset: 0 }),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["99", "1", "1", "0"]);
    }

    /// Byte-level Store8 / Load8U round-trip through both paths: write two bytes,
    /// read them back zero-extended. These back `$int_to_string` and the string
    /// helpers.
    #[test]
    fn byte_ops_roundtrip() {
        use WirExpr::*;
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::Store8 { ptr: ConstI32(100), value: ConstI32(65), offset: 0 },
                WirNode::Store8 { ptr: ConstI32(100), value: ConstI32(66), offset: 1 },
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Load8U { ptr: Box::new(ConstI32(100)), offset: 0 }),
                    }],
                }),
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Load8U { ptr: Box::new(ConstI32(101)), offset: 0 }),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["65", "66"]);
    }

    /// `$list_push_cap` in-place path: a cap-2 list `[1][10]` with one slot to
    /// spare appends `20` in place (no grow), returns (same ptr, cap). Exercises
    /// the migrated multi-value helper + CallStoreMulti end-to-end.
    #[test]
    fn list_push_cap_in_place() {
        use crate::wir_helpers::{ensure_helper, list_push_cap_helper};
        use WirExpr::*;
        let conv = |e: WirExpr| Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let pi = |e: WirExpr| WirNode::Do(CallHost { import: "print_int".into(), args: vec![e] });
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("rp", WirTy::Bool), local("rc", WirTy::Bool)],
            body: vec![
                // list at 2048: len=1, elem0=10 (cap 2 → one slot to spare). The
                // `$rc_alloc` size header at `[2048-4]` records the real allocation
                // (4-byte len word + 2 i64 slots = 20 bytes) so the RFC-0005 in-place
                // bounds check (in `$list_push_cap`) sees the true capacity — a real
                // list always carries this header.
                WirNode::Store { ptr: ConstI32(2044), value: ConstI32(20), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI32(1), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI64(10), kind: Kind::I64, offset: 4 },
                WirNode::SetGlobal { global: "heap".into(), value: ConstI32(2068) },
                WirNode::CallStoreMulti {
                    func: "list_push_cap".into(),
                    args: vec![ConstI32(2048), ConstI64(20), ConstI32(2)],
                    dests: vec!["rp".into(), "rc".into()],
                },
                // new len (2), the appended elem (20), the returned ptr (2048) & cap (2)
                pi(conv(Load { ptr: Box::new(ConstI32(2048)), kind: Kind::I32, offset: 0 })),
                pi(Load { ptr: Box::new(ConstI32(2060)), kind: Kind::I64, offset: 0 }),
                pi(conv(GetLocal("rp".into()))),
                pi(conv(GetLocal("rc".into()))),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(false), list_push_cap_helper(), run],
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal {
                    name: "heap".into(),
                    kind: Kind::I32,
                    mutable: true,
                    init: GlobalInit::I32(2068),
                    export: None,
                },
                WirGlobal {
                    name: "__witchy_reowns".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_reowns".into()),
                },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // Agreement gate: the encoder AND the WAT printer (multi-value func +
        // CallStoreMulti arm) both run identically.
        assert_agrees(&module, &["2", "20", "2048", "2"]);
    }

    /// RFC-0005 step 2: the in-place `$list_push_cap` bounds check TRAPS when the `cap`
    /// token overstates the buffer's real allocation. A cap-1 buffer (size header 12 =
    /// 4-byte len word + one i64 slot), appended to as if `cap` were 2, would write a
    /// SECOND element PAST the block — silent heap corruption. The check converts that
    /// into a loud trap, exactly as `$list_at` traps an out-of-bounds read.
    #[test]
    fn list_push_cap_traps_on_overstated_cap() {
        use WirExpr::*;
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("rp", WirTy::Bool), local("rc", WirTy::Bool)],
            body: vec![
                // list at 2048: REAL allocation is 12 bytes (len word + ONE i64 slot), len=1.
                WirNode::Store { ptr: ConstI32(2044), value: ConstI32(12), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI32(1), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI64(10), kind: Kind::I64, offset: 4 },
                WirNode::SetGlobal { global: "heap".into(), value: ConstI32(2060) },
                // cap=2 LIES: the real element capacity is 1. The in-place append at index
                // 1 would land past the block, so the bounds check must trap.
                WirNode::CallStoreMulti {
                    func: "list_push_cap".into(),
                    args: vec![ConstI32(2048), ConstI64(20), ConstI32(2)],
                    dests: vec!["rp".into(), "rc".into()],
                },
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] }],
            funcs: vec![
                crate::wir_helpers::ensure_helper(false),
                crate::wir_helpers::list_push_cap_helper(),
                run,
            ],
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2060), export: None },
                WirGlobal {
                    name: "__witchy_reowns".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_reowns".into()),
                },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_traps(&module);
    }

    /// RFC-0005 step 2: the in-place `$str_append_cap` bounds check TRAPS when `cap`
    /// overstates the buffer's real allocation. A 6-byte buffer (4-byte len word + 2
    /// spare bytes) appended to as if it had room for 5 more would copy PAST the block;
    /// the check traps before the copy. (Only the len/plen headers matter — the trap
    /// fires before any byte is read.)
    #[test]
    fn str_append_cap_traps_on_overstated_cap() {
        use WirExpr::*;
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("rp", WirTy::Bool), local("rc", WirTy::Bool)],
            body: vec![
                // s at 2048: REAL allocation is 6 bytes (len word + 2 bytes), len=2.
                WirNode::Store { ptr: ConstI32(2044), value: ConstI32(6), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI32(2), kind: Kind::I32, offset: 0 },
                // piece at 2064: plen=3.
                WirNode::Store { ptr: ConstI32(2064), value: ConstI32(3), kind: Kind::I32, offset: 0 },
                WirNode::SetGlobal { global: "heap".into(), value: ConstI32(2080) },
                // cap=5 LIES: real capacity is 2. need = 2+3 = 5; in-place fires (cap >=
                // need), and the copy would run to byte s+4+5 = 9, past the 6-byte block.
                WirNode::CallStoreMulti {
                    func: "str_append_cap".into(),
                    args: vec![ConstI32(2048), ConstI32(2064), ConstI32(5)],
                    dests: vec!["rp".into(), "rc".into()],
                },
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] }],
            funcs: vec![
                crate::wir_helpers::ensure_helper(false),
                crate::wir_helpers::str_append_cap_helper(),
                run,
            ],
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2080), export: None },
                WirGlobal {
                    name: "__witchy_reowns".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_reowns".into()),
                },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_traps(&module);
    }

    /// RFC-0005 step 2: the in-place `$dict_insert_cap` APPEND bounds check TRAPS when
    /// `cap` overstates the buffer's real allocation. An empty dict whose rc size header
    /// (`[d-8]`) is too small for even one entry (an entry at index `count` needs
    /// `count*16+24` bytes) is appended to with cap=1; the check traps instead of writing
    /// the entry past the block. (A dict is `rc_alloc(..)+4`, so the hidden index word is
    /// at `d-4` and the size header at `d-8`; setting the index word to 0 makes the find a
    /// linear scan and skips `dict_index_put`.)
    #[test]
    fn dict_insert_cap_traps_on_overstated_cap() {
        use WirExpr::*;
        // d = 2052: size header [2044]=20 (< the 24 one entry needs), index word
        // [2048]=0 (linear-scan find, no index update), count [2052]=0.
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("rp", WirTy::Bool), local("rc", WirTy::Bool)],
            body: vec![
                WirNode::Store { ptr: ConstI32(2044), value: ConstI32(20), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI32(0), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2052), value: ConstI32(0), kind: Kind::I32, offset: 0 },
                WirNode::SetGlobal { global: "heap".into(), value: ConstI32(2200) },
                // dict_insert_cap(d=2052, k=5, v=9, mode=0 int-keys, cap=1): key not found,
                // cap(1) > count(0) → append_inplace → the check fires (24 > 20).
                WirNode::CallStoreMulti {
                    func: "dict_insert_cap".into(),
                    args: vec![ConstI32(2052), ConstI64(5), ConstI64(9), ConstI32(0), ConstI32(1)],
                    dests: vec!["rp".into(), "rc".into()],
                },
            ],
            raw_body: None,
        };
        let mut funcs = helper_closure("dict_insert_cap");
        funcs.push(run);
        let module = WirModule {
            imports: vec![WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] }],
            funcs,
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2200), export: None },
                WirGlobal {
                    name: "__witchy_reowns".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_reowns".into()),
                },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_traps(&module);
    }

    /// CallStoreMulti calls a MULTI-result function and stores each result into a
    /// local (reverse pop order). Exercises both the new node and a multi-value
    /// function (`pair` leaves two i32s via dual tail Push) — the shape the
    /// in-place cap ABI ($list_push_cap) uses. Encoder path (the production sink).
    #[test]
    fn call_store_multi_roundtrip() {
        let pair = WirFunc {
            name: "pair".into(),
            params: vec![],
            ret: vec![WirTy::Bool, WirTy::Bool], // (result i32 i32)
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::ConstI32(10)),
                WirNode::Push(WirExpr::ConstI32(20)),
            ],
            raw_body: None,
        };
        let pi = |name: &str| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert {
                    from: Kind::I32,
                    to: Kind::I64,
                    arg: Box::new(WirExpr::GetLocal(name.into())),
                }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("a", WirTy::Bool), local("b", WirTy::Bool)],
            body: vec![
                WirNode::CallStoreMulti {
                    func: "pair".into(),
                    args: vec![],
                    dests: vec!["a".into(), "b".into()],
                },
                pi("a"),
                pi("b"),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![pair, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["10", "20"]);
    }

    /// The WIR-native `$int_to_string` helper renders signed integers correctly
    /// (zero, positive multi-digit, negative) — printed via `print` so the host
    /// reads the produced `[len][ascii]` string. Both paths agree.
    #[test]
    fn int_to_string_helper_renders() {
        use crate::wir_helpers::{ensure_helper, int_to_string_helper, print_str_helper};
        // run(): print_str(int_to_string(N)) for a few N.
        let mut body = Vec::new();
        for n in [0i64, 7, 4096, -123] {
            body.push(WirNode::Do(WirExpr::Call {
                func: "print_str".into(),
                args: vec![WirExpr::Call {
                    func: "int_to_string".into(),
                    args: vec![WirExpr::ConstI64(n)],
                }],
            }));
        }
        let run = WirFunc { name: "run".into(), params: vec![], ret: vec![], locals: vec![], body, raw_body: None };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print".into(),
                params: vec![Kind::I32, Kind::I32],
                results: vec![],
            }],
            funcs: vec![ensure_helper(false), int_to_string_helper(false), print_str_helper(), run],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["0", "7", "4096", "-123"]);
    }

    /// The WIR-native `$ensure` helper grows memory correctly: with `$heap = 0`,
    /// `ensure(100000)` needs >1 page (65536 bytes), so memory grows from 1 to 2
    /// pages. Runs identically through the encoder and the WAT path.
    #[test]
    fn ensure_helper_grows_memory() {
        use crate::wir_helpers::ensure_helper;
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::Do(WirExpr::Call {
                    func: "ensure".into(),
                    args: vec![WirExpr::ConstI32(100_000)],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::MemorySize),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(false), run],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(0),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["2"]);
    }

    /// `$find_byte` (a scan loop with an inner byte-compare loop whose mismatch
    /// `br` lives inside an `if`) finds substrings correctly — the regression test
    /// for the encoder counting `if` as a branch frame. Fuel-capped, so a
    /// mis-encoded `br` traps instead of spinning.
    #[test]
    fn find_byte_finds_substrings() {
        use crate::wir_helpers::find_byte_helper;
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 100, bytes: mk_str("hello") },
            DataSegment { offset: 120, bytes: mk_str("ell") },
            DataSegment { offset: 140, bytes: mk_str("xyz") },
            DataSegment { offset: 160, bytes: mk_str("") },
        ];
        let fb = |s: i32, sub: i32| WirExpr::Call {
            func: "find_byte".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(sub)],
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(fb(100, 120)), pi(fb(100, 140)), pi(fb(100, 160))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![find_byte_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "ell" is at index 1 of "hello"; "xyz" absent (-1); "" → 0.
        assert_agrees(&module, &["1", "-1", "0"]);
    }

    #[test]
    fn starts_with_matches_prefixes() {
        use crate::wir_helpers::starts_with_helper;
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 100, bytes: mk_str("hello") },
            DataSegment { offset: 120, bytes: mk_str("hel") },
            DataSegment { offset: 140, bytes: mk_str("lo") },
            DataSegment { offset: 160, bytes: mk_str("") },
        ];
        let sw = |s: i32, p: i32| WirExpr::Call {
            func: "starts_with".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(p)],
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(sw(100, 120)), pi(sw(100, 140)), pi(sw(100, 160))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![starts_with_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "hello" starts with "hel" → 1; with "lo" → 0; with "" → 1.
        assert_agrees(&module, &["1", "0", "1"]);
    }

    #[test]
    fn ends_with_matches_suffixes() {
        use crate::wir_helpers::ends_with_helper;
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 100, bytes: mk_str("hello") },
            DataSegment { offset: 120, bytes: mk_str("llo") },
            DataSegment { offset: 140, bytes: mk_str("hel") },
            DataSegment { offset: 160, bytes: mk_str("") },
        ];
        let ew = |s: i32, p: i32| WirExpr::Call {
            func: "ends_with".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(p)],
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(ew(100, 120)), pi(ew(100, 140)), pi(ew(100, 160))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ends_with_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "hello" ends with "llo" → 1; with "hel" → 0; with "" → 1.
        assert_agrees(&module, &["1", "0", "1"]);
    }

    #[test]
    fn str_index_of_returns_char_index() {
        use crate::wir_helpers::{byte_to_char_helper, find_byte_helper, str_index_of_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 100, bytes: mk_str("hello") },
            DataSegment { offset: 120, bytes: mk_str("ll") },
            DataSegment { offset: 140, bytes: mk_str("xyz") },
            // "héllo": the 'é' is two UTF-8 bytes, so "llo" sits at *byte* 3 but
            // *char* index 2 — exercising byte_to_char's continuation-byte skip.
            DataSegment { offset: 160, bytes: mk_str("héllo") },
            DataSegment { offset: 180, bytes: mk_str("llo") },
        ];
        let ix = |s: i32, sub: i32| WirExpr::Call {
            func: "str_index_of".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(sub)],
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(ix(100, 120)), pi(ix(100, 140)), pi(ix(160, 180))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![find_byte_helper(), byte_to_char_helper(), str_index_of_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "ll" is char 2 of "hello"; "xyz" absent (-1); "llo" is char 2 of "héllo".
        assert_agrees(&module, &["2", "-1", "2"]);
    }

    #[test]
    fn substr_copies_a_byte_slice() {
        use crate::wir_helpers::{ensure_helper, substr_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![DataSegment { offset: 200, bytes: mk_str("hello") }];
        let sub = |src: i32, start: i32, len: i32| WirExpr::Call {
            func: "substr".into(),
            args: vec![WirExpr::ConstI32(src), WirExpr::ConstI32(start), WirExpr::ConstI32(len)],
        };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool)],
            body: vec![
                // "ell" = bytes 1..4 of "hello": length 3, first byte 'e' (101).
                WirNode::SetLocal { local: "r1".into(), value: sub(200, 1, 3) },
                pi(load_i32(WirExpr::GetLocal("r1".into()))),
                pi(byte0(WirExpr::GetLocal("r1".into()))),
                // "he" = bytes 0..2: length 2, first byte 'h' (104).
                WirNode::SetLocal { local: "r2".into(), value: sub(200, 0, 2) },
                pi(load_i32(WirExpr::GetLocal("r2".into()))),
                pi(byte0(WirExpr::GetLocal("r2".into()))),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(false), substr_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["3", "101", "2", "104"]);
    }

    #[test]
    fn str_substring_slices_by_char_index() {
        use crate::wir_helpers::{char_to_byte_helper, ensure_helper, str_substring_helper, substr_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("hello world") },
            DataSegment { offset: 220, bytes: mk_str("héllo") },
        ];
        // `start`/`end` ride the full-width i64 index path (BUG-011); `s` is the i32 ptr.
        let ss = |s: i32, a: i64, b: i64| WirExpr::Call {
            func: "str_substring".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI64(a), WirExpr::ConstI64(b)],
        };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool)],
            body: vec![
                // "hello" = chars 0..5 of "hello world": 5 bytes, first byte 'h' (104).
                WirNode::SetLocal { local: "r1".into(), value: ss(200, 0, 5) },
                pi(load_i32(WirExpr::GetLocal("r1".into()))),
                pi(byte0(WirExpr::GetLocal("r1".into()))),
                // "él" = chars 1..3 of "héllo": é is 2 bytes so the slice is 3
                // bytes, first byte 0xc3 (195) — the char→byte mapping at work.
                WirNode::SetLocal { local: "r2".into(), value: ss(220, 1, 3) },
                pi(load_i32(WirExpr::GetLocal("r2".into()))),
                pi(byte0(WirExpr::GetLocal("r2".into()))),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![
                ensure_helper(false),
                substr_helper(),
                char_to_byte_helper(),
                str_substring_helper(),
                run,
            ],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["5", "104", "3", "195"]);
    }

    #[test]
    fn trim_strips_surrounding_whitespace() {
        use crate::wir_helpers::{ensure_helper, is_ws_helper, substr_helper, trim_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("  hi  ") },
            DataSegment { offset: 220, bytes: mk_str("abc") },
            DataSegment { offset: 240, bytes: mk_str("   ") },
        ];
        let tr = |s: i32| WirExpr::Call { func: "trim".into(), args: vec![WirExpr::ConstI32(s)] };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool), local("r3", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "r1".into(), value: tr(200) },
                pi(load_i32(gl("r1"))),
                pi(byte0(gl("r1"))),
                WirNode::SetLocal { local: "r2".into(), value: tr(220) },
                pi(load_i32(gl("r2"))),
                pi(byte0(gl("r2"))),
                // all-whitespace trims to empty (length 0).
                WirNode::SetLocal { local: "r3".into(), value: tr(240) },
                pi(load_i32(gl("r3"))),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(false), substr_helper(), is_ws_helper(), trim_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "  hi  " → "hi" (len 2, 'h'=104); "abc" → "abc" (len 3, 'a'=97); "   " → "" (len 0).
        assert_agrees(&module, &["2", "104", "3", "97", "0"]);
    }

    #[test]
    fn list_push_appends_elements() {
        use crate::wir_helpers::{ensure_helper, list_push_helper};
        // An empty list is a bare i32 length header of 0.
        let data = vec![DataSegment { offset: 200, bytes: 0u32.to_le_bytes().to_vec() }];
        let push = |list: WirExpr, x: i64| WirExpr::Call {
            func: "list_push".into(),
            args: vec![list, WirExpr::ConstI64(x)],
        };
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let len_of = |p: WirExpr| WirExpr::Convert {
            from: Kind::I32,
            to: Kind::I64,
            arg: Box::new(WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 }),
        };
        let elem = |p: WirExpr, off: u32| WirExpr::Load { ptr: Box::new(p), kind: Kind::I64, offset: off };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "r1".into(), value: push(WirExpr::ConstI32(200), 42) },
                WirNode::SetLocal { local: "r2".into(), value: push(gl("r1"), 99) },
                pi(len_of(gl("r2"))),      // length 2
                pi(elem(gl("r2"), 4)),     // element 0 → 42
                pi(elem(gl("r2"), 12)),    // element 1 → 99
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(false), list_push_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["2", "42", "99"]);
    }

    #[test]
    fn split_breaks_on_separator() {
        use crate::wir_helpers::{ensure_helper, list_push_helper, split_helper, substr_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("a,b,c") },
            DataSegment { offset: 220, bytes: mk_str(",") },
        ];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let wrap = |e: WirExpr| WirExpr::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(e) };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        // The string pointer held in list slot `off` (an i64), wrapped to i32.
        let elem_ptr = |off: u32| wrap(WirExpr::Load { ptr: Box::new(gl("r")), kind: Kind::I64, offset: off });
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal {
                    local: "r".into(),
                    value: WirExpr::Call {
                        func: "split".into(),
                        args: vec![WirExpr::ConstI32(200), WirExpr::ConstI32(220)],
                    },
                },
                pi(to_i64(load_i32(gl("r")))),         // list length 3
                pi(to_i64(load_i32(elem_ptr(4)))),     // piece 0 "a" length 1
                pi(to_i64(byte0(elem_ptr(4)))),        // piece 0 first byte 'a' 97
                pi(to_i64(byte0(elem_ptr(20)))),       // piece 2 "c" first byte 'c' 99
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(false), substr_helper(), list_push_helper(), split_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "a,b,c".split(",") → ["a","b","c"]: 3 pieces; piece0="a", piece2="c".
        assert_agrees(&module, &["3", "1", "97", "99"]);
    }

    #[test]
    fn str_chars_splits_into_characters() {
        use crate::wir_helpers::{
            byte_to_char_helper, char_to_byte_helper, ensure_helper, list_push_helper,
            str_chars_helper, str_substring_helper, substr_helper,
        };
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        // "héllo": 5 characters but 6 bytes (é is two bytes).
        let data = vec![DataSegment { offset: 200, bytes: mk_str("héllo") }];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let wrap = |e: WirExpr| WirExpr::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(e) };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let elem_ptr = |off: u32| wrap(WirExpr::Load { ptr: Box::new(gl("r")), kind: Kind::I64, offset: off });
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal {
                    local: "r".into(),
                    value: WirExpr::Call { func: "str_chars".into(), args: vec![WirExpr::ConstI32(200)] },
                },
                pi(to_i64(load_i32(gl("r")))),       // 5 characters
                pi(to_i64(load_i32(elem_ptr(4)))),   // char 0 "h" length 1
                pi(to_i64(byte0(elem_ptr(4)))),      // char 0 first byte 'h' 104
                pi(to_i64(load_i32(elem_ptr(12)))),  // char 1 "é" length 2 (bytes)
                pi(to_i64(byte0(elem_ptr(12)))),     // char 1 first byte 0xc3 195
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![
                ensure_helper(false),
                substr_helper(),
                char_to_byte_helper(),
                str_substring_helper(),
                byte_to_char_helper(),
                list_push_helper(),
                str_chars_helper(),
                run,
            ],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["5", "1", "104", "2", "195"]);
    }

    #[test]
    fn list_concat_joins_two_lists() {
        use crate::wir_helpers::{ensure_helper, list_concat_helper};
        // A list is an i32 length header followed by 8-byte (i64) element slots.
        let mk_list = |xs: &[i64]| {
            let mut b = (xs.len() as u32).to_le_bytes().to_vec();
            for x in xs {
                b.extend_from_slice(&x.to_le_bytes());
            }
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_list(&[10, 20]) },
            DataSegment { offset: 240, bytes: mk_list(&[30]) },
        ];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let elem = |off: u32| WirExpr::Load { ptr: Box::new(gl("r")), kind: Kind::I64, offset: off };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal {
                    local: "r".into(),
                    value: WirExpr::Call {
                        func: "list_concat".into(),
                        args: vec![WirExpr::ConstI32(200), WirExpr::ConstI32(240)],
                    },
                },
                pi(to_i64(WirExpr::Load { ptr: Box::new(gl("r")), kind: Kind::I32, offset: 0 })), // length 3
                pi(elem(4)),  // 10
                pi(elem(12)), // 20
                pi(elem(20)), // 30
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(false), list_concat_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // [10,20] ++ [30] = [10,20,30].
        assert_agrees(&module, &["3", "10", "20", "30"]);
    }

    #[test]
    fn ascii_case_changes_letter_case() {
        use crate::wir_helpers::{ascii_case_helper, ensure_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![DataSegment { offset: 200, bytes: mk_str("aB1") }];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let cased = |up: i32| WirExpr::Call {
            func: "ascii_case".into(),
            args: vec![WirExpr::ConstI32(200), WirExpr::ConstI32(up)],
        };
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        // byte `off` of the string pointer held in local `name`.
        let byte = |name: &'static str, off: u32| {
            to_i64(WirExpr::Load8U { ptr: Box::new(gl(name)), offset: 4 + off })
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("u", WirTy::Bool), local("d", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "u".into(), value: cased(1) },
                pi(byte("u", 0)), // 'a'→'A' = 65
                pi(byte("u", 1)), // 'B' stays = 66
                WirNode::SetLocal { local: "d".into(), value: cased(0) },
                pi(byte("d", 0)), // 'a' stays = 97
                pi(byte("d", 1)), // 'B'→'b' = 98
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(false), ascii_case_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["65", "66", "97", "98"]);
    }

    #[test]
    fn str_to_int_parses_signed_decimals() {
        use crate::wir_helpers::{is_ws_helper, str_to_int_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("123") },
            DataSegment { offset: 220, bytes: mk_str("-45") },
            DataSegment { offset: 240, bytes: mk_str("  7  ") },
            DataSegment { offset: 260, bytes: mk_str("+9") },
        ];
        let parse = |off: i32| WirExpr::Call { func: "str_to_int".into(), args: vec![WirExpr::ConstI32(off)] };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(parse(200)), pi(parse(220)), pi(parse(240)), pi(parse(260))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![
                WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] },
                // (RFC-0045) `str_to_int` routes its parse-failure aborts through
                // `__witchy_abort`, so the import must be declared for the encoder.
                WirImport {
                    name: "__witchy_abort".into(),
                    params: vec![Kind::I32, Kind::I64, Kind::I64, Kind::I32],
                    results: vec![],
                },
            ],
            funcs: vec![is_ws_helper(), str_to_int_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "123"→123, "-45"→-45, "  7  "→7 (whitespace trimmed), "+9"→9.
        assert_agrees(&module, &["123", "-45", "7", "9"]);
    }

    #[test]
    fn dict_insert_get_has_int_keys() {
        use crate::wir_helpers::{
            dict_find_helper, dict_get_or_helper, dict_has_helper, dict_hash_helper,
            dict_insert_helper, dict_new_helper, ensure_helper, key_eq_helper, rc_alloc_helper,
            str_eq_helper,
        };
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        // dict_insert(d, k, v, mode=0): re-bind `d` to the fresh map.
        let ins = |k: i64, v: i64| WirNode::SetLocal {
            local: "d".into(),
            value: WirExpr::Call {
                func: "dict_insert".into(),
                args: vec![gl("d"), WirExpr::ConstI64(k), WirExpr::ConstI64(v), WirExpr::ConstI32(0)],
            },
        };
        let get = |k: i64| WirExpr::Call {
            func: "dict_get_or".into(),
            args: vec![gl("d"), WirExpr::ConstI64(k), WirExpr::ConstI64(-1), WirExpr::ConstI32(0)],
        };
        let has = |k: i64| WirExpr::Convert {
            from: Kind::I32,
            to: Kind::I64,
            arg: Box::new(WirExpr::Call {
                func: "dict_has".into(),
                args: vec![gl("d"), WirExpr::ConstI64(k), WirExpr::ConstI32(0)],
            }),
        };
        let pi = |e: WirExpr| WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] });
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("d", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "d".into(), value: WirExpr::Call { func: "dict_new".into(), args: vec![] } },
                ins(1, 100),
                ins(2, 200),
                ins(1, 111), // update existing key 1
                pi(get(1)),  // 111
                pi(get(2)),  // 200
                pi(get(3)),  // -1 (absent → default)
                pi(has(2)),  // 1
                pi(has(5)),  // 0
                // final count = 2 (key 1 was updated, not appended).
                pi(WirExpr::Convert {
                    from: Kind::I32,
                    to: Kind::I64,
                    arg: Box::new(WirExpr::Load { ptr: Box::new(gl("d")), kind: Kind::I32, offset: 0 }),
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![
                ensure_helper(false),
                rc_alloc_helper(),
                str_eq_helper(),
                key_eq_helper(),
                dict_hash_helper(),
                dict_find_helper(),
                dict_new_helper(),
                dict_insert_helper(),
                dict_get_or_helper(),
                dict_has_helper(),
                run,
            ],
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal {
                    name: "heap".into(),
                    kind: Kind::I32,
                    mutable: true,
                    init: GlobalInit::I32(1024),
                    export: None,
                },
                WirGlobal {
                    name: "rc_freelist".into(),
                    kind: Kind::I32,
                    mutable: true,
                    init: GlobalInit::I32(0),
                    export: None,
                },
                WirGlobal {
                    name: "__rc_reused_bytes".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: None,
                },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["111", "200", "-1", "1", "0", "2"]);
    }

    /// RFC-0088's dictionary oracle counts structural search invocations, not
    /// key comparisons. A test double for `$dict_find` returns a configured
    /// entry position and increments `searches`; every present/missing insert
    /// and remove must consume exactly one result.
    #[test]
    fn dict_extract_helpers_perform_one_semantic_search() {
        use crate::wir_helpers::{
            bump_alloc_helper, dict_hash_helper, dict_index_put_helper,
            dict_insert_extract_helper, dict_reindex_helper, dict_remove_extract_helper,
            ensure_helper, leaf_drop_helper, leaf_dup_helper, rc_alloc_helper, rc_drop_helper,
            rc_dup_helper, rc_free_helper, slot_take_or_dup_helper,
        };
        let gl = |name: &str| WirExpr::GetLocal(name.into());
        let gi = |name: &str| WirExpr::GetGlobal(name.into());
        let pi = |value: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![value] })
        };
        let i32_to_i64 = |value: WirExpr| WirExpr::Convert {
            from: Kind::I32,
            to: Kind::I64,
            arg: Box::new(value),
        };
        let find = WirFunc {
            name: "dict_find".into(),
            params: vec![
                local("d", WirTy::Bool),
                local("k", WirTy::Int),
                local("mode", WirTy::Bool),
            ],
            ret: vec![WirTy::Bool],
            locals: vec![],
            body: vec![
                WirNode::SetGlobal {
                    global: "searches".into(),
                    value: WirExpr::Binary {
                        op: BinOp::Add,
                        kind: Kind::I64,
                        lhs: Box::new(gi("searches")),
                        rhs: Box::new(WirExpr::ConstI64(1)),
                    },
                },
                WirNode::Push(gi("find_result")),
            ],
            raw_body: None,
        };
        let mut body = vec![
            // One-entry source dictionary at 2048: key=7, value=70.
            WirNode::Store { ptr: WirExpr::ConstI32(2044), value: WirExpr::ConstI32(0), kind: Kind::I32, offset: 0 },
            WirNode::Store { ptr: WirExpr::ConstI32(2048), value: WirExpr::ConstI32(1), kind: Kind::I32, offset: 0 },
            WirNode::Store { ptr: WirExpr::ConstI32(2048), value: WirExpr::ConstI64(7), kind: Kind::I64, offset: 4 },
            WirNode::Store { ptr: WirExpr::ConstI32(2048), value: WirExpr::ConstI64(70), kind: Kind::I64, offset: 12 },
        ];
        let mut probe = |helper: &str, find_result: i32, args: Vec<WirExpr>| {
            body.extend([
                WirNode::SetGlobal { global: "searches".into(), value: WirExpr::ConstI64(0) },
                WirNode::SetGlobal { global: "find_result".into(), value: WirExpr::ConstI32(find_result) },
                WirNode::CallStoreMulti {
                    func: helper.into(),
                    args,
                    dests: vec!["out".into(), "present".into(), "old".into(), "cap".into()],
                },
                pi(gi("searches")),
                pi(i32_to_i64(gl("present"))),
                pi(gl("old")),
                pi(i32_to_i64(WirExpr::Load { ptr: Box::new(gl("out")), kind: Kind::I32, offset: 0 })),
            ]);
        };
        probe(
            "dict_insert_extract",
            0,
            vec![WirExpr::ConstI32(2048), WirExpr::ConstI64(7), WirExpr::ConstI64(71), WirExpr::ConstI32(0), WirExpr::ConstI32(0), WirExpr::ConstI32(-1), WirExpr::ConstI32(-1)],
        );
        probe(
            "dict_insert_extract",
            -1,
            vec![WirExpr::ConstI32(2048), WirExpr::ConstI64(8), WirExpr::ConstI64(80), WirExpr::ConstI32(0), WirExpr::ConstI32(0), WirExpr::ConstI32(-1), WirExpr::ConstI32(-1)],
        );
        probe(
            "dict_remove_extract",
            0,
            vec![WirExpr::ConstI32(2048), WirExpr::ConstI64(7), WirExpr::ConstI32(0), WirExpr::ConstI32(0), WirExpr::ConstI32(-1), WirExpr::ConstI32(-1)],
        );
        probe(
            "dict_remove_extract",
            -1,
            vec![WirExpr::ConstI32(2048), WirExpr::ConstI64(9), WirExpr::ConstI32(0), WirExpr::ConstI32(0), WirExpr::ConstI32(-1), WirExpr::ConstI32(-1)],
        );
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![
                local("out", WirTy::Bool),
                local("present", WirTy::Bool),
                local("old", WirTy::Int),
                local("cap", WirTy::Bool),
            ],
            body,
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] }],
            funcs: vec![
                ensure_helper(false),
                bump_alloc_helper(),
                rc_alloc_helper(),
                rc_free_helper(),
                rc_dup_helper(),
                rc_drop_helper(),
                leaf_dup_helper(),
                leaf_drop_helper(),
                slot_take_or_dup_helper(),
                dict_hash_helper(),
                dict_index_put_helper(),
                dict_reindex_helper(),
                find,
                dict_insert_extract_helper(),
                dict_remove_extract_helper(),
                run,
            ],
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(4096), export: None },
                WirGlobal { name: "rc_freelist".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
                WirGlobal { name: "__rc_reused_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_live_cells".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_bump_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_reuse_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_free_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_region_rewind_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_active".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
                WirGlobal { name: "__witchy_extract_searches".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_key_comparisons".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_copied_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_retains".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_drops".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "heap_base".into(), kind: Kind::I32, mutable: false, init: GlobalInit::I32(2048), export: None },
                WirGlobal { name: "searches".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "find_result".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(
            &module,
            &[
                "1", "1", "70", "1",
                "1", "0", "0", "2",
                "1", "1", "70", "0",
                "1", "0", "0", "1",
            ],
        );
    }

    #[test]
    fn dict_keys_values_remove_int_keys() {
        use crate::wir_helpers::{
            dict_find_helper, dict_get_or_helper, dict_hash_helper, dict_insert_helper,
            dict_new_helper, dict_project_helper, dict_remove_helper, ensure_helper, key_eq_helper,
            rc_alloc_helper, str_eq_helper,
        };
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let ins = |k: i64, v: i64| WirNode::SetLocal {
            local: "d".into(),
            value: WirExpr::Call {
                func: "dict_insert".into(),
                args: vec![gl("d"), WirExpr::ConstI64(k), WirExpr::ConstI64(v), WirExpr::ConstI32(0)],
            },
        };
        let call1 = |f: &str, a: &str| WirExpr::Call { func: f.into(), args: vec![gl(a)] };
        // list element `idx` (an i64 slot) of the list pointer held in `name`.
        let elem = |name: &'static str, idx: u32| WirExpr::Load { ptr: Box::new(gl(name)), kind: Kind::I64, offset: 4 + idx * 8 };
        let len = |name: &'static str| WirExpr::Convert {
            from: Kind::I32,
            to: Kind::I64,
            arg: Box::new(WirExpr::Load { ptr: Box::new(gl(name)), kind: Kind::I32, offset: 0 }),
        };
        let get = |name: &'static str, k: i64| WirExpr::Call {
            func: "dict_get_or".into(),
            args: vec![gl(name), WirExpr::ConstI64(k), WirExpr::ConstI64(-1), WirExpr::ConstI32(0)],
        };
        let pi = |e: WirExpr| WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] });
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("d", WirTy::Bool), local("ks", WirTy::Bool), local("vs", WirTy::Bool), local("rm", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "d".into(), value: WirExpr::Call { func: "dict_new".into(), args: vec![] } },
                ins(1, 100),
                ins(2, 200),
                ins(3, 300),
                WirNode::SetLocal { local: "ks".into(), value: call1("dict_keys", "d") },
                WirNode::SetLocal { local: "vs".into(), value: call1("dict_values", "d") },
                WirNode::SetLocal {
                    local: "rm".into(),
                    value: WirExpr::Call { func: "dict_remove".into(), args: vec![gl("d"), WirExpr::ConstI64(2), WirExpr::ConstI32(0)] },
                },
                pi(len("ks")),     // 3 keys
                pi(elem("ks", 0)), // key 1
                pi(elem("ks", 2)), // key 3
                pi(elem("vs", 1)), // value 200
                pi(WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(WirExpr::Load { ptr: Box::new(gl("rm")), kind: Kind::I32, offset: 0 }) }), // remove count 2
                pi(get("rm", 1)),  // 100 (kept)
                pi(get("rm", 2)),  // -1 (removed)
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] }],
            funcs: vec![
                ensure_helper(false),
                rc_alloc_helper(),
                str_eq_helper(),
                key_eq_helper(),
                dict_hash_helper(),
                dict_find_helper(),
                dict_new_helper(),
                dict_insert_helper(),
                dict_get_or_helper(),
                dict_project_helper("dict_keys", 4),
                dict_project_helper("dict_values", 12),
                dict_remove_helper(),
                run,
            ],
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(1024), export: None },
                WirGlobal { name: "rc_freelist".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
                WirGlobal { name: "__rc_reused_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["3", "1", "3", "200", "2", "100", "-1"]);
    }

    #[test]
    fn replace_rewrites_matches() {
        use crate::wir_helpers::{ensure_helper, match_at_helper, replace_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("hello") },
            DataSegment { offset: 220, bytes: mk_str("l") },
            DataSegment { offset: 240, bytes: mk_str("L") },
            DataSegment { offset: 260, bytes: mk_str("aaa") },
            DataSegment { offset: 280, bytes: mk_str("a") },
            DataSegment { offset: 300, bytes: mk_str("bb") },
            DataSegment { offset: 320, bytes: mk_str("ab") },
            DataSegment { offset: 340, bytes: mk_str("") },
            DataSegment { offset: 360, bytes: mk_str("-") },
        ];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let rep = |s: i32, f: i32, t: i32| WirExpr::Call {
            func: "replace".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(f), WirExpr::ConstI32(t)],
        };
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let len = |name: &'static str| to_i64(WirExpr::Load { ptr: Box::new(gl(name)), kind: Kind::I32, offset: 0 });
        let byte = |name: &'static str, off: u32| to_i64(WirExpr::Load8U { ptr: Box::new(gl(name)), offset: 4 + off });
        let pi = |e: WirExpr| WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] });
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool), local("r3", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "r1".into(), value: rep(200, 220, 240) }, // "hello"/l→L = "heLLo"
                pi(len("r1")),       // 5
                pi(byte("r1", 2)),   // 'L' 76
                pi(byte("r1", 3)),   // 'L' 76
                WirNode::SetLocal { local: "r2".into(), value: rep(260, 280, 300) }, // "aaa"/a→bb = "bbbbbb"
                pi(len("r2")),       // 6
                pi(byte("r2", 0)),   // 'b' 98
                WirNode::SetLocal { local: "r3".into(), value: rep(320, 340, 360) }, // "ab"/""→- = "-a-b-"
                pi(len("r3")),       // 5
                pi(byte("r3", 0)),   // '-' 45
                pi(byte("r3", 1)),   // 'a' 97
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] }],
            funcs: vec![ensure_helper(false), match_at_helper(), replace_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(1024), export: None }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["5", "76", "76", "6", "98", "5", "45", "97"]);
    }

    #[test]
    fn arithmetic_roundtrips() {
        // fn add() -> Int: (2 + 3) * 4 == 20
        let add = WirFunc {
            name: "add".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Binary {
                op: BinOp::Mul,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::ConstI64(2)),
                    rhs: Box::new(WirExpr::ConstI64(3)),
                }),
                rhs: Box::new(WirExpr::ConstI64(4)),
            }))],
            raw_body: None,
        };
        let m = int_demo(add, WirExpr::Call { func: "add".into(), args: vec![] });
        assert_agrees(&m, &["20"]);
    }

    #[test]
    fn if_with_result_roundtrips() {
        // fn pick(b: Bool) -> Int: if b: 10 else: 20
        let pick = WirFunc {
            name: "pick".into(),
            params: vec![local("b", WirTy::Bool)],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::If {
                cond: WirExpr::GetLocal("b".into()),
                then_: vec![WirNode::Push(WirExpr::ConstI64(10))],
                els: vec![WirNode::Push(WirExpr::ConstI64(20))],
                result: Some(WirTy::Int),
            }],
            raw_body: None,
        };
        let m_true = int_demo(
            pick.clone(),
            WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(1)] },
        );
        assert_agrees(&m_true, &["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_agrees(&m_false, &["20"]);
    }

    #[test]
    fn loop_spine_roundtrips() {
        // fn sum_to(n) -> Int: sum of 0..n (Block/Loop/Br spine).
        let i_lt_n = WirExpr::Binary {
            op: BinOp::Lt,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("i".into())),
            rhs: Box::new(WirExpr::GetLocal("n".into())),
        };
        let not_i_lt_n = WirExpr::Binary {
            op: BinOp::Eq,
            kind: Kind::I32,
            lhs: Box::new(i_lt_n),
            rhs: Box::new(WirExpr::ConstI32(0)),
        };
        let sum_to = WirFunc {
            name: "sum_to".into(),
            params: vec![local("n", WirTy::Int)],
            ret: vec![WirTy::Int],
            locals: vec![local("total", WirTy::Int), local("i", WirTy::Int)],
            body: vec![
                WirNode::SetLocal { local: "total".into(), value: WirExpr::ConstI64(0) },
                WirNode::SetLocal { local: "i".into(), value: WirExpr::ConstI64(0) },
                WirNode::Block {
                    label: "exit".into(),
                    result: None,
                    body: vec![WirNode::Loop {
                        label: "head".into(),
                        body: vec![
                            WirNode::Br { target: "exit".into(), cond: Some(not_i_lt_n) },
                            WirNode::SetLocal {
                                local: "total".into(),
                                value: WirExpr::Binary {
                                    op: BinOp::Add,
                                    kind: Kind::I64,
                                    lhs: Box::new(WirExpr::GetLocal("total".into())),
                                    rhs: Box::new(WirExpr::GetLocal("i".into())),
                                },
                            },
                            WirNode::SetLocal {
                                local: "i".into(),
                                value: WirExpr::Binary {
                                    op: BinOp::Add,
                                    kind: Kind::I64,
                                    lhs: Box::new(WirExpr::GetLocal("i".into())),
                                    rhs: Box::new(WirExpr::ConstI64(1)),
                                },
                            },
                            WirNode::Br { target: "head".into(), cond: None },
                        ],
                    }],
                },
                WirNode::Return(Some(WirExpr::GetLocal("total".into()))),
            ],
            raw_body: None,
        };
        // sum 0..5 = 10
        let m = int_demo(
            sum_to,
            WirExpr::Call { func: "sum_to".into(), args: vec![WirExpr::ConstI64(5)] },
        );
        assert_agrees(&m, &["10"]);
    }

    // A `(kind, value, expect)` round-trip table; single-element by design today.
    #[test]
    #[allow(clippy::single_element_loop)]
    fn slot_conversions_roundtrip() {
        // FromSlot(ToSlot(x, k), k) == x.
        for (kind, value, expect) in [(Kind::I64, WirExpr::ConstI64(42), "42")] {
            let f = WirFunc {
                name: "rt".into(),
                params: vec![],
                ret: vec![WirTy::Int],
                locals: vec![],
                body: vec![WirNode::Return(Some(WirExpr::FromSlot(
                    Box::new(WirExpr::ToSlot(Box::new(value), kind)),
                    kind,
                )))],
                raw_body: None,
            };
            let m = int_demo(f, WirExpr::Call { func: "rt".into(), args: vec![] });
            assert_agrees(&m, &[expect]);
        }
    }

    #[test]
    fn unary_ops_roundtrip() {
        // fn neg() -> Int: -(5) == -5
        let neg = WirFunc {
            name: "neg".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Unary {
                op: UnOp::Neg,
                kind: Kind::I64,
                arg: Box::new(WirExpr::ConstI64(5)),
            }))],
            raw_body: None,
        };
        let m = int_demo(neg, WirExpr::Call { func: "neg".into(), args: vec![] });
        assert_agrees(&m, &["-5"]);

        // fn bnot() -> Int: ~0 == -1
        let bnot = WirFunc {
            name: "bnot".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Unary {
                op: UnOp::BitNot,
                kind: Kind::I64,
                arg: Box::new(WirExpr::ConstI64(0)),
            }))],
            raw_body: None,
        };
        let m = int_demo(bnot, WirExpr::Call { func: "bnot".into(), args: vec![] });
        assert_agrees(&m, &["-1"]);
    }

    #[test]
    fn control_value_if_roundtrips() {
        // value-`if` in expression position (the `&&`/`||`/if-expr shape).
        let pick = WirFunc {
            name: "pick".into(),
            params: vec![local("b", WirTy::Bool)],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Control(Box::new(WirNode::If {
                cond: WirExpr::GetLocal("b".into()),
                then_: vec![WirNode::Push(WirExpr::ConstI64(10))],
                els: vec![WirNode::Push(WirExpr::ConstI64(20))],
                result: Some(WirTy::Int),
            }))))],
            raw_body: None,
        };
        let m_true = int_demo(
            pick.clone(),
            WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(1)] },
        );
        assert_agrees(&m_true, &["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_agrees(&m_false, &["20"]);
    }

    #[test]
    fn string_print_roundtrips() {
        // console.print("hi"): data [2,0,0,0,'h','i'] at offset 8.
        let mut bytes = (2u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"hi");
        let print_call = WirExpr::CallHost {
            import: "print".into(),
            args: vec![
                WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::StrPtr(8)),
                    rhs: Box::new(WirExpr::ConstI32(4)),
                },
                WirExpr::Load { ptr: Box::new(WirExpr::StrPtr(8)), kind: Kind::I32, offset: 0 },
            ],
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(print_call)],
            raw_body: None,
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print".into(),
                params: vec![Kind::I32, Kind::I32],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![DataSegment { offset: 8, bytes }],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["hi"]);
    }

    // --- M3-obstacle additions: globals, table+call_indirect, multi-value,
    //     raw-body splice ---------------------------------------------------

    #[test]
    fn mutable_global_set_get_roundtrips() {
        // A mutable i64 global `$counter` initialized to 5. `run`:
        //   counter = counter + 37   (global.get; +; global.set)
        //   print_int(counter)       (global.get)  -> 42
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::SetGlobal {
                    global: "counter".into(),
                    value: WirExpr::Binary {
                        op: BinOp::Add,
                        kind: Kind::I64,
                        lhs: Box::new(WirExpr::GetGlobal("counter".into())),
                        rhs: Box::new(WirExpr::ConstI64(37)),
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::GetGlobal("counter".into())],
                }),
            ],
            raw_body: None,
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal {
                name: "counter".into(),
                kind: Kind::I64,
                mutable: true,
                init: GlobalInit::I64(5),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["42"]);
    }

    #[test]
    fn table_call_indirect_roundtrips() {
        // A lifted lambda `$__lam0` with the `$clos1` signature
        // `(param i32 env) (param i64 arg) (result i64)`: returns arg + 100.
        // A closure record `[code_index=0]` lives at offset 0 in memory (data
        // segment of 4 zero bytes). `run` reads its code index and `call_indirect
        // (type $clos1)` with the record as env and a literal slot arg of 5 -> 105.
        let lam0 = WirFunc {
            name: "__lam0".into(),
            params: vec![local("env", WirTy::Capability), local("arg", WirTy::Slot)],
            ret: vec![WirTy::Slot],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::GetLocal("arg".into())),
                rhs: Box::new(WirExpr::ConstI64(100)),
            }))],
            raw_body: None,
        };
        // The closure record at offset 0 holds the code index (0) as its i32
        // header. `call_indirect` args = [env ptr, slot arg]; index = i32.load env.
        let call = WirExpr::CallIndirect {
            signature: slot_closure_signature(1, 1),
            args: vec![WirExpr::ConstI32(0), WirExpr::ToSlot(Box::new(WirExpr::ConstI64(5)), Kind::I64)],
            index: Box::new(WirExpr::Load {
                ptr: Box::new(WirExpr::ConstI32(0)),
                kind: Kind::I32,
                offset: 0,
            }),
        };
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
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![lam0, run],
            memory_pages: 1,
            // 4 zero bytes at offset 0: the closure record's code index (slot 0).
            data: vec![DataSegment { offset: 0, bytes: vec![0, 0, 0, 0] }],
            globals: vec![],
            table: Some(WirTable { funcs: vec!["__lam0".into()] }),
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["105"]);
    }

    #[test]
    fn table_call_indirect_multi_result_roundtrips() {
        let lam0 = WirFunc {
            name: "__lam0".into(),
            params: vec![local("env", WirTy::Capability), local("arg", WirTy::Slot)],
            ret: vec![WirTy::Slot, WirTy::Slot],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::GetLocal("arg".into())),
                    rhs: Box::new(WirExpr::ConstI64(1)),
                }),
                WirNode::Push(WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::GetLocal("arg".into())),
                    rhs: Box::new(WirExpr::ConstI64(100)),
                }),
                WirNode::Return(None),
            ],
            raw_body: None,
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("result", WirTy::Slot), local("writeback", WirTy::Slot)],
            body: vec![
                WirNode::CallIndirectStoreMulti {
                    signature: slot_closure_signature(1, 2),
                    args: vec![WirExpr::ConstI32(0), WirExpr::ConstI64(5)],
                    index: WirExpr::Load {
                        ptr: Box::new(WirExpr::ConstI32(0)),
                        kind: Kind::I32,
                        offset: 0,
                    },
                    dests: vec!["result".into(), "writeback".into()],
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::GetLocal("result".into())],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::GetLocal("writeback".into())],
                }),
            ],
            raw_body: None,
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![lam0, run],
            memory_pages: 1,
            data: vec![DataSegment { offset: 0, bytes: vec![0, 0, 0, 0] }],
            globals: vec![],
            table: Some(WirTable { funcs: vec!["__lam0".into()] }),
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["6", "105"]);
    }

    #[test]
    fn exact_indirect_signatures_distinguish_gc_and_linear_environments() {
        let typed = ClosureSignature {
            params: vec![Kind::GcRef(0), Kind::I64],
            results: vec![Kind::I64],
        };
        let gc_lambda = WirFunc {
            name: "gc_lambda".into(),
            params: vec![local("env", WirTy::GcRef(0)), local("arg", WirTy::Int)],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::GetLocal("arg".into())),
                rhs: Box::new(WirExpr::ConstI64(1)),
            })],
            raw_body: None,
        };
        let linear_lambda = WirFunc {
            name: "linear_lambda".into(),
            params: vec![local("env", WirTy::Bool), local("arg", WirTy::Int)],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::GetLocal("arg".into())),
                rhs: Box::new(WirExpr::ConstI64(100)),
            })],
            raw_body: None,
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("closure", WirTy::GcRef(0))],
            body: vec![
                WirNode::SetLocal {
                    local: "closure".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![
                            WirExpr::ConstI32(0),
                            WirExpr::ConstI32(0),
                            WirExpr::RefNull(Kind::StructRef),
                        ],
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::CallIndirect {
                        signature: typed,
                        args: vec![
                            WirExpr::GetLocal("closure".into()),
                            WirExpr::ConstI64(5),
                        ],
                        index: Box::new(WirExpr::ConstI32(0)),
                    }],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::CallIndirect {
                        signature: slot_closure_signature(1, 1),
                        args: vec![WirExpr::ConstI32(0), WirExpr::ConstI64(5)],
                        index: Box::new(WirExpr::ConstI32(1)),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![gc_lambda, linear_lambda, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: Some(WirTable { funcs: vec!["gc_lambda".into(), "linear_lambda".into()] }),
            exports: vec![("run".into(), "run".into())],
        };

        assert_eq!(run_binary(&encode(&module, &[closure_wrapper_struct()])), vec!["6", "105"]);
    }

    #[test]
    fn typed_indirect_multi_result_preserves_reference_kinds() {
        let lambda = WirFunc {
            name: "lambda".into(),
            params: vec![local("env", WirTy::GcRef(0))],
            ret: vec![WirTy::Extern, WirTy::GcRef(0)],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::RefNull(Kind::ExternRef)),
                WirNode::Push(WirExpr::GetLocal("env".into())),
                WirNode::Return(None),
            ],
            raw_body: None,
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![
                local("closure", WirTy::GcRef(0)),
                local("cap", WirTy::Extern),
                local("returned", WirTy::GcRef(0)),
            ],
            body: vec![
                WirNode::SetLocal {
                    local: "closure".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![
                            WirExpr::ConstI32(0),
                            WirExpr::ConstI32(0),
                            WirExpr::RefNull(Kind::StructRef),
                        ],
                    },
                },
                WirNode::CallIndirectStoreMulti {
                    signature: ClosureSignature {
                        params: vec![Kind::GcRef(0)],
                        results: vec![Kind::ExternRef, Kind::GcRef(0)],
                    },
                    args: vec![WirExpr::GetLocal("closure".into())],
                    index: WirExpr::ConstI32(0),
                    dests: vec!["cap".into(), "returned".into()],
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::RefIsNull(Box::new(WirExpr::GetLocal(
                            "cap".into(),
                        )))),
                    }],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::RefIsNull(Box::new(WirExpr::GetLocal(
                            "returned".into(),
                        )))),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![lambda, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: Some(WirTable { funcs: vec!["lambda".into()] }),
            exports: vec![("run".into(), "run".into())],
        };

        assert_eq!(run_binary(&encode(&module, &[closure_wrapper_struct()])), vec!["1", "0"]);
    }

    #[test]
    fn multi_value_result_roundtrips() {
        // fn pair() -> (Int, Int): leaves 30 then 12 on the stack — a 2-result
        // func exercising a `(result i64 i64)` signature in the type section.
        // `run` calls $pair (two i64s on the stack), `i64.add`s them, and prints
        // the sum -> 42. The node-tree `WirExpr` model can't reference a call's
        // multiple stack results directly, so `run` is spliced as a raw body
        // (which also covers the splice + multi-value paths together).
        let pair = WirFunc {
            name: "pair".into(),
            params: vec![],
            ret: vec![WirTy::Int, WirTy::Int],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::ConstI64(30)),
                WirNode::Push(WirExpr::ConstI64(12)),
            ],
            raw_body: None,
        };
        // Index space: import print_int = 0; defined funcs pair = 1, run = 2.
        let mut run_fn = Function::new_with_locals_types(Vec::<ValType>::new());
        run_fn.instruction(&Instruction::Call(1)); // call $pair -> [30, 12]
        run_fn.instruction(&Instruction::I64Add); // -> 42
        run_fn.instruction(&Instruction::Call(0)); // call $print_int
        run_fn.instruction(&Instruction::End);
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![],
            raw_body: Some(run_fn.into_raw_body()),
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![pair, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_eq!(run_encoded(&m), vec!["42".to_string()]);
    }

    #[test]
    fn raw_body_splice_roundtrips() {
        // A func `magic` whose body is supplied as pre-encoded wasm bytes (the
        // splice path): `i64.const 99; end`. `run` calls it and prints -> 99.
        // The raw body is `Function::into_raw_body()` output (locals + instrs +
        // End, NO length prefix) — exactly what `CodeSection::raw` consumes.
        let mut magic_fn = Function::new_with_locals_types(Vec::<ValType>::new());
        magic_fn.instruction(&Instruction::I64Const(99));
        magic_fn.instruction(&Instruction::End);
        let magic = WirFunc {
            name: "magic".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![], // ignored: raw_body wins
            raw_body: Some(magic_fn.into_raw_body()),
        };
        let m = int_demo(magic, WirExpr::Call { func: "magic".into(), args: vec![] });
        // No WAT path for a raw-body func, so assert on the encoder output only.
        assert_eq!(run_encoded(&m), vec!["99".to_string()]);
    }

    /// (RFC-0005 Stage 1) The encoder round-trips a hand-built module that uses the
    /// new GC-struct + externref infrastructure end to end: a struct type
    /// `{externref, i64}`, a function whose signature carries an `externref` param
    /// AND a `(ref null $0)` param, and the `StructNew`/`StructGet`/`StructSet`/
    /// `RefNull` opcodes. Instantiating it in wasmtime with GC + function-references
    /// enabled (as the real runtime keeps reference types / GC on) proves the emitted
    /// bytes VALIDATE; running `run` proves the struct field write-then-read executes.
    /// This is the mechanism the cap-carrying-aggregate lowering (Stage 4) will use;
    /// nothing in the production path emits these yet.
    #[test]
    fn gc_struct_and_externref_round_trip() {
        use crate::wir::WirStructDef;

        // struct $0 { field0: externref (a capability), field1: i64 (a scalar) }.
        let structs = vec![WirStructDef {
            fields: vec![Kind::ExternRef, Kind::I64],
            mutable: true,
        }];

        // fn takes_cap(cap: externref, agg: (ref null $0)) -> ()  — present but
        // uncalled, so its reference-typed SIGNATURE must still validate.
        let takes_cap = WirFunc {
            name: "takes_cap".into(),
            params: vec![local("cap", WirTy::Extern), local("agg", WirTy::GcRef(0))],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Drop(WirExpr::StructGet {
                struct_id: 0,
                field: 1,
                base: Box::new(WirExpr::GetLocal("agg".into())),
            })],
            raw_body: None,
        };

        // fn run() -> ():
        //   s = struct.new $0 (ref.null extern, i64.const 42)   // a null cap + 42
        //   struct.set $0 1 s 99                                 // overwrite the scalar
        //   print_int (struct.get $0 1 s)                        // -> 99
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("s", WirTy::GcRef(0))],
            body: vec![
                WirNode::SetLocal {
                    local: "s".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![WirExpr::RefNull(Kind::ExternRef), WirExpr::ConstI64(42)],
                    },
                },
                WirNode::StructSet {
                    struct_id: 0,
                    field: 1,
                    base: WirExpr::GetLocal("s".into()),
                    value: WirExpr::ConstI64(99),
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::StructGet {
                        struct_id: 0,
                        field: 1,
                        base: Box::new(WirExpr::GetLocal("s".into())),
                    }],
                }),
            ],
            raw_body: None,
        };

        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![takes_cap, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };

        let binary = encode(&module, &structs);
        assert_eq!(binary, encode_with_gc(&module, &structs, &[]));

        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config).expect("engine");
        let m = wasmtime::Module::new(&engine, &binary)
            .unwrap_or_else(|e| panic!("GC-struct module invalid: {e:#}"));
        let out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = wasmtime::Linker::new(&engine);
        let o = out.clone();
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
                o.lock().unwrap().push(n.to_string());
            })
            .unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let inst = linker.instantiate(&mut store, &m).expect("instantiate");
        let run = inst.get_typed_func::<(), ()>(&mut store, "run").expect("run export");
        run.call(&mut store, ()).expect("run");
        assert_eq!(*out.lock().unwrap(), vec!["99".to_string()]);
    }

    /// RFC-0005 reference-storage substrate: GC arrays can hold concrete GC
    /// references without crossing the linear-memory slot ABI. Exercise both
    /// allocation forms plus indexed read/write and length after the optimizer
    /// has traversed the complete array expression tree.
    #[test]
    fn gc_reference_array_round_trip() {
        use crate::wir::{WirArrayDef, WirStructDef};

        // GC type 0 is the payload struct, type 1 is a holder that points
        // forward to array type 2, and type 2 is array definition 0. The
        // forward edge requires the encoder's explicit recursion group.
        let structs = vec![
            WirStructDef {
                fields: vec![Kind::ExternRef, Kind::I64],
                mutable: true,
            },
            WirStructDef {
                fields: vec![Kind::GcRef(2)],
                mutable: true,
            },
        ];
        let arrays = vec![WirArrayDef { element: Kind::GcRef(0) }];
        let payload = |value| WirExpr::StructNew {
            struct_id: 0,
            args: vec![WirExpr::RefNull(Kind::ExternRef), WirExpr::ConstI64(value)],
        };
        let item = |array: &str, index| WirExpr::StructGet {
            struct_id: 0,
            field: 1,
            base: Box::new(WirExpr::ArrayGet {
                array_id: 0,
                array: Box::new(WirExpr::GetLocal(array.into())),
                index: Box::new(WirExpr::ConstI32(index)),
            }),
        };

        let takes_array = WirFunc {
            name: "takes_array".into(),
            params: vec![local("items", WirTy::GcRef(2))],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Drop(WirExpr::ArrayLen(Box::new(
                WirExpr::GetLocal("items".into()),
            )))],
            raw_body: None,
        };

        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![
                local("repeated", WirTy::GcRef(2)),
                local("fixed", WirTy::GcRef(2)),
                local("holder", WirTy::GcRef(1)),
            ],
            body: vec![
                WirNode::SetLocal {
                    local: "repeated".into(),
                    value: WirExpr::ArrayNew {
                        array_id: 0,
                        value: Box::new(WirExpr::RefNull(Kind::GcRef(0))),
                        len: Box::new(WirExpr::ConstI32(3)),
                    },
                },
                WirNode::ArraySet {
                    array_id: 0,
                    array: WirExpr::GetLocal("repeated".into()),
                    index: WirExpr::ConstI32(1),
                    value: payload(99),
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![item("repeated", 1)],
                }),
                WirNode::SetLocal {
                    local: "holder".into(),
                    value: WirExpr::StructNew {
                        struct_id: 1,
                        args: vec![WirExpr::GetLocal("repeated".into())],
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::ToSlot(
                        Box::new(WirExpr::ArrayLen(Box::new(WirExpr::StructGet {
                            struct_id: 1,
                            field: 0,
                            base: Box::new(WirExpr::GetLocal("holder".into())),
                        }))),
                        Kind::I32,
                    )],
                }),
                WirNode::SetLocal {
                    local: "fixed".into(),
                    value: WirExpr::ArrayNewFixed {
                        array_id: 0,
                        items: vec![payload(7), payload(8)],
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![item("fixed", 0)],
                }),
            ],
            raw_body: None,
        };
        let mut module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![takes_array, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };

        crate::wir_opt::optimize(&mut module);
        let binary = encode_with_gc(&module, &structs, &arrays);
        assert_eq!(run_binary(&binary), vec!["99", "3", "7"]);
    }

    /// RFC-0005 Stage 4 closure substrate: a uniform closure wrapper can hold
    /// either the legacy linear environment or an erased GC payload. The lifted
    /// body recovers its statically assigned payload with `ref.cast` before
    /// reading capability-bearing fields.
    #[test]
    fn closure_wrapper_erases_and_recovers_a_gc_environment() {
        use crate::wir::{
            CLOSURE_CODE_FIELD, CLOSURE_GC_ENV_FIELD, CLOSURE_LINEAR_ENV_FIELD, WirStructDef,
            closure_wrapper_struct,
        };

        let structs = vec![
            closure_wrapper_struct(),
            WirStructDef {
                fields: vec![Kind::ExternRef, Kind::I64],
                mutable: true,
            },
        ];
        let wrapper = WirExpr::GetLocal("closure".into());
        let payload = WirExpr::RefCast {
            struct_id: 1,
            value: Box::new(WirExpr::StructGet {
                struct_id: 0,
                field: CLOSURE_GC_ENV_FIELD,
                base: Box::new(wrapper.clone()),
            }),
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("erased", WirTy::StructRef), local("closure", WirTy::GcRef(0))],
            body: vec![
                WirNode::SetLocal {
                    local: "erased".into(),
                    value: WirExpr::StructNew {
                        struct_id: 1,
                        args: vec![WirExpr::RefNull(Kind::ExternRef), WirExpr::ConstI64(42)],
                    },
                },
                WirNode::SetLocal {
                    local: "closure".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![
                            WirExpr::ConstI32(7),
                            WirExpr::ConstI32(17),
                            WirExpr::GetLocal("erased".into()),
                        ],
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::StructGet {
                            struct_id: 0,
                            field: CLOSURE_LINEAR_ENV_FIELD,
                            base: Box::new(WirExpr::GetLocal("closure".into())),
                        }),
                    }],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::StructGet {
                            struct_id: 0,
                            field: CLOSURE_CODE_FIELD,
                            base: Box::new(wrapper),
                        }),
                    }],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::StructGet {
                        struct_id: 1,
                        field: 1,
                        base: Box::new(payload),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };

        assert_eq!(run_binary(&encode(&module, &structs)), vec!["17", "7", "42"]);

        let invalid_mutation = WirFunc {
            name: "mutate_wrapper".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("closure", WirTy::GcRef(0))],
            body: vec![
                WirNode::SetLocal {
                    local: "closure".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![
                            WirExpr::ConstI32(7),
                            WirExpr::ConstI32(17),
                            WirExpr::RefNull(Kind::StructRef),
                        ],
                    },
                },
                WirNode::StructSet {
                    struct_id: 0,
                    field: CLOSURE_CODE_FIELD,
                    base: WirExpr::GetLocal("closure".into()),
                    value: WirExpr::ConstI32(8),
                },
            ],
            raw_body: None,
        };
        let invalid_module = WirModule {
            imports: vec![],
            funcs: vec![invalid_mutation],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "mutate_wrapper".into())],
        };
        let invalid_binary = encode(&invalid_module, &structs);
        let error = match wasmparser::validate(&invalid_binary) {
            Ok(_) => panic!("the encoded closure wrapper fields must be immutable"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("immutable"),
            "unexpected validation error: {error}"
        );
    }

    #[test]
    fn gc_environment_casts_trap_on_null_or_wrong_payload() {
        use crate::wir::WirStructDef;

        let cast_func = |name: &str, value: WirExpr| WirFunc {
            name: name.into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Drop(WirExpr::RefCast {
                struct_id: 0,
                value: Box::new(value),
            })],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![],
            funcs: vec![
                cast_func("cast_null", WirExpr::RefNull(Kind::StructRef)),
                cast_func(
                    "cast_wrong",
                    WirExpr::StructNew { struct_id: 1, args: vec![WirExpr::ConstI32(0)] },
                ),
            ],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![
                ("cast_null".into(), "cast_null".into()),
                ("cast_wrong".into(), "cast_wrong".into()),
            ],
        };
        let structs = vec![
            WirStructDef {
                fields: vec![Kind::I64],
                mutable: true,
            },
            WirStructDef {
                fields: vec![Kind::I32],
                mutable: true,
            },
        ];
        let binary = encode(&module, &structs);
        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config).expect("engine");
        let wasm = wasmtime::Module::new(&engine, binary).expect("module validates");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &wasm, &[]).expect("instantiate");

        for name in ["cast_null", "cast_wrong"] {
            let func = instance.get_typed_func::<(), ()>(&mut store, name).expect(name);
            assert!(func.call(&mut store, ()).is_err(), "{name} must trap");
        }
    }
