    fn rc_test_globals() -> Vec<WirGlobal> {
        vec![
            WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2048), export: None },
            WirGlobal { name: "heap_base".into(), kind: Kind::I32, mutable: false, init: GlobalInit::I32(2048), export: None },
            WirGlobal { name: "rc_freelist".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
            WirGlobal { name: "__rc_reused_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            WirGlobal { name: "__witchy_live_cells".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            WirGlobal { name: "__witchy_rc_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            WirGlobal { name: "__witchy_rc_alloc_requested_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            WirGlobal { name: "__witchy_bump_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            WirGlobal { name: "__witchy_rc_reuse_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            WirGlobal { name: "__witchy_rc_free_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
        ]
    }

    fn rc_test_module(free: WirFunc, run: WirFunc) -> WirModule {
        use crate::wir_helpers::{bump_alloc_helper, ensure_helper, rc_alloc_helper};
        print_int_module(
            vec![ensure_helper(false), bump_alloc_helper(), rc_alloc_helper(), free, run],
            rc_test_globals(),
        )
    }

    /// A quarantined cell's zero RC marker makes a repeated direct free a
    /// deterministic sanitizer trap rather than a second live-cell decrement.
    #[test]
    fn uaf_free_traps_on_repeated_free() {
        use crate::wir_helpers::rc_free_helper_for_test;
        let gl = |name: &str| WirExpr::GetLocal(name.into());
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("cell", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal {
                    local: "cell".into(),
                    value: WirExpr::Call { func: "rc_alloc".into(), args: vec![WirExpr::ConstI32(16)] },
                },
                WirNode::Do(WirExpr::Call { func: "rc_free".into(), args: vec![gl("cell")] }),
                WirNode::Do(WirExpr::Call { func: "rc_free".into(), args: vec![gl("cell")] }),
            ],
            raw_body: None,
        };
        let module = rc_test_module(rc_free_helper_for_test(true, false), run);
        assert_traps(&module);
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
        let module = print_int_module(
            vec![
                crate::wir_helpers::ensure_helper(false),
                crate::wir_helpers::list_push_cap_helper(),
                run,
            ],
            vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2060), export: None },
                WirGlobal {
                    name: "__witchy_reowns".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_reowns".into()),
                },
            ],
        );
        assert_traps(&module);
    }

    fn print_int_module(funcs: Vec<WirFunc>, globals: Vec<WirGlobal>) -> WirModule {
        let mut funcs = funcs;
        if funcs.iter().any(|func| func.name == "dict_get_or" || func.name == "dict_at") {
            if !funcs.iter().any(|func| func.name == "dict_find_bucket") {
                funcs.push(crate::wir_helpers::dict_find_bucket_helper());
            }
            if !funcs.iter().any(|func| func.name == "dict_hash") {
                funcs.push(crate::wir_helpers::dict_hash_helper());
            }
        }
        if funcs.iter().any(|func| func.name == "dict_insert" || func.name == "dict_insert_extract") {
            if !funcs.iter().any(|func| func.name == "bump_alloc") {
                funcs.push(crate::wir_helpers::bump_alloc_helper());
            }
            if !funcs.iter().any(|func| func.name == "dict_index_put") {
                funcs.push(crate::wir_helpers::dict_index_put_helper());
            }
            if !funcs.iter().any(|func| func.name == "dict_hash") {
                funcs.push(crate::wir_helpers::dict_hash_helper());
            }
            if !funcs.iter().any(|func| func.name == "dict_index_update_value") {
                funcs.push(crate::wir_helpers::dict_index_update_value_helper());
            }
            if !funcs.iter().any(|func| func.name == "dict_reindex") {
                funcs.push(crate::wir_helpers::dict_reindex_helper());
            }
        }
        if funcs.iter().any(|func| func.name == "dict_remove")
            && !funcs.iter().any(|func| func.name == "dict_reindex")
        {
            funcs.push(crate::wir_helpers::dict_reindex_helper());
            if !funcs.iter().any(|func| func.name == "bump_alloc") {
                funcs.push(crate::wir_helpers::bump_alloc_helper());
            }
            if !funcs.iter().any(|func| func.name == "dict_index_put") {
                funcs.push(crate::wir_helpers::dict_index_put_helper());
            }
            if !funcs.iter().any(|func| func.name == "dict_hash") {
                funcs.push(crate::wir_helpers::dict_hash_helper());
            }
        }
        let mut globals = globals;
        // Dictionary helpers may carry RFC-0143 attribution sites even in
        // synthetic encoder fixtures. Keep the fixture modules' global ABI in
        // step with the production assembly without repeating these rows in
        // every dictionary test.
        for (name, kind, init) in [
            ("__witchy_extract_active", Kind::I32, GlobalInit::I32(0)),
            ("__witchy_dict_hashes", Kind::I64, GlobalInit::I64(0)),
            ("__witchy_dict_probe_groups", Kind::I64, GlobalInit::I64(0)),
            ("__witchy_dict_h2_candidates", Kind::I64, GlobalInit::I64(0)),
            ("__witchy_dict_rebuilds", Kind::I64, GlobalInit::I64(0)),
            ("__witchy_dict_grows", Kind::I64, GlobalInit::I64(0)),
            ("__witchy_dict_order_bytes_moved", Kind::I64, GlobalInit::I64(0)),
            ("__witchy_dict_index_bytes", Kind::I64, GlobalInit::I64(0)),
        ] {
            if !globals.iter().any(|global| global.name == name) {
                globals.push(WirGlobal { name: name.into(), kind, mutable: true, init, export: None });
            }
        }
        WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs,
            memory_pages: 1,
            data: vec![],
            globals,
            table: None,
            exports: vec![("run".into(), "run".into())],
        }
    }
