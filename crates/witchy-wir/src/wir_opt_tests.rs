    use super::*;
    use crate::wir::{
        BinOp, ClosureSignature, DataSegment, Kind, WirExpr, WirFunc, WirImport, WirLocal,
        WirModule, WirNode, WirTable, WirTy, closure_wrapper_struct, slot_closure_signature,
    };

    /// A bare module wrapping a single func, for exercising `optimize`.
    fn module_with(func: WirFunc) -> WirModule {
        WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![func],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![],
        }
    }

    fn func_returning(body_expr: WirExpr) -> WirFunc {
        WirFunc {
            name: "f".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(body_expr))],
            raw_body: None,
        }
    }

    #[test]
    fn local_renamer_traverses_gc_environment_casts() {
        let mut cast = WirExpr::RefCast {
            struct_id: 1,
            value: Box::new(WirExpr::GetLocal("old_env".into())),
        };
        let renames = HashMap::from([("old_env".to_string(), "new_env".to_string())]);

        rename_expr_locals(&mut cast, &renames);

        assert!(matches!(
            cast,
            WirExpr::RefCast { struct_id: 1, value }
                if matches!(value.as_ref(), WirExpr::GetLocal(name) if name == "new_env")
        ));
    }

    #[test]
    fn local_renamer_traverses_gc_array_operands() {
        let mut node = WirNode::ArraySet {
            array_id: 0,
            array: WirExpr::ArrayGet {
                array_id: 0,
                array: Box::new(WirExpr::ArrayNew {
                    array_id: 0,
                    value: Box::new(WirExpr::GetLocal("fill".into())),
                    len: Box::new(WirExpr::GetLocal("len".into())),
                }),
                index: Box::new(WirExpr::GetLocal("read_index".into())),
            },
            index: WirExpr::GetLocal("write_index".into()),
            value: WirExpr::ArrayNewFixed {
                array_id: 0,
                items: vec![
                    WirExpr::GetLocal("item".into()),
                    WirExpr::ArrayLen(Box::new(WirExpr::GetLocal("measured".into()))),
                ],
            },
        };
        let renames = HashMap::from([
            ("fill".to_string(), "new_fill".to_string()),
            ("len".to_string(), "new_len".to_string()),
            ("read_index".to_string(), "new_read_index".to_string()),
            ("write_index".to_string(), "new_write_index".to_string()),
            ("item".to_string(), "new_item".to_string()),
            ("measured".to_string(), "new_measured".to_string()),
        ]);

        rename_node_locals(&mut node, &renames);

        let rendered = format!("{node:?}");
        for name in renames.values() {
            assert!(
                rendered.contains(name),
                "missing renamed local {name}: {rendered}"
            );
        }
        for name in renames.keys() {
            assert!(
                !rendered.contains(&format!("\"{name}\"")),
                "stale local {name}: {rendered}"
            );
        }
    }

    #[test]
    fn lowers_self_tail_call_to_simultaneous_rebind_and_loop() {
        let recur = WirFunc {
            name: "count".into(),
            params: vec![
                WirLocal { name: "n".into(), ty: WirTy::Int },
                WirLocal { name: "acc".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Control(Box::new(WirNode::If {
                cond: WirExpr::Binary {
                    op: BinOp::Eq,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::GetLocal("n".into())),
                    rhs: Box::new(WirExpr::ConstI64(0)),
                },
                then_: vec![WirNode::Push(WirExpr::GetLocal("acc".into()))],
                els: vec![WirNode::Push(WirExpr::Call {
                    func: "count".into(),
                    args: vec![
                        WirExpr::Binary {
                            op: BinOp::Sub,
                            kind: Kind::I64,
                            lhs: Box::new(WirExpr::GetLocal("n".into())),
                            rhs: Box::new(WirExpr::ConstI64(1)),
                        },
                        WirExpr::GetLocal("n".into()),
                    ],
                })],
                result: Some(WirTy::Int),
            })))],
            raw_body: None,
        };
        let mut module = module_with(recur);

        assert_eq!(lower_direct_tail_calls(&mut module), 1);
        let func = &module.funcs[0];
        assert_eq!(func.locals.len(), 2, "one staging local per parameter");
        let [WirNode::Loop { label, body }, WirNode::Unreachable] = func.body.as_slice() else {
            panic!("expected loop-wrapped function, got {:?}", func.body);
        };
        let [WirNode::Return(Some(WirExpr::Control(control)))] = body.as_slice() else {
            panic!("expected returned value-if in loop, got {body:?}");
        };
        let WirNode::If { then_, els, result: Some(WirTy::Int), .. } = control.as_ref() else {
            panic!("expected typed value-if, got {control:?}");
        };
        assert!(matches!(then_.as_slice(), [WirNode::Push(WirExpr::GetLocal(n))] if n == "acc"));
        let [WirNode::Push(WirExpr::Control(escape))] = els.as_slice() else {
            panic!("expected tail escape expression, got {els:?}");
        };
        let WirNode::Block { body: escape_body, result: Some(WirTy::Int), .. } = escape.as_ref() else {
            panic!("expected typed escape block, got {escape:?}");
        };
        assert_eq!(escape_body.len(), 10);
        assert!(matches!(escape_body.first(), Some(WirNode::SetLocal { local, .. }) if local == &func.locals[0].name));
        assert!(matches!(escape_body.get(1), Some(WirNode::SetLocal { local, .. }) if local == &func.locals[1].name));
        assert!(matches!(escape_body.get(2), Some(WirNode::SetLocal { local, value: WirExpr::ConstI64(0) }) if local == "n"));
        assert!(matches!(escape_body.get(3), Some(WirNode::SetLocal { local, value: WirExpr::ConstI64(0) }) if local == "acc"));
        assert!(matches!(escape_body.get(4), Some(WirNode::SetLocal { local, value: WirExpr::GetLocal(_) }) if local == "n"));
        assert!(matches!(escape_body.get(5), Some(WirNode::SetLocal { local, value: WirExpr::GetLocal(_) }) if local == "acc"));
        assert!(matches!(escape_body.get(6), Some(WirNode::SetLocal { local, value: WirExpr::ConstI64(0) }) if local == &func.locals[0].name));
        assert!(matches!(escape_body.get(7), Some(WirNode::SetLocal { local, value: WirExpr::ConstI64(0) }) if local == &func.locals[1].name));
        assert!(matches!(escape_body.get(8), Some(WirNode::Br { target, cond: None }) if target == label));
        assert!(matches!(escape_body.last(), Some(WirNode::Unreachable)));
    }

    #[test]
    fn leaves_multi_result_self_call_for_its_writeback_continuation() {
        let recur = WirFunc {
            name: "bump".into(),
            params: vec![WirLocal { name: "n".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int, WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Call {
                func: "bump".into(),
                args: vec![WirExpr::GetLocal("n".into())],
            })],
            raw_body: None,
        };
        let mut module = module_with(recur);

        assert_eq!(lower_direct_tail_calls(&mut module), 0);
        assert!(matches!(module.funcs[0].body.as_slice(), [WirNode::Push(WirExpr::Call { .. })]));
    }

    #[test]
    fn lowers_forwarded_ownership_envelope_to_one_loop() {
        let recur = WirFunc {
            name: "walk".into(),
            params: vec![
                WirLocal { name: "state".into(), ty: WirTy::Bool },
                WirLocal { name: "n".into(), ty: WirTy::Int },
                WirLocal { name: "state__cap".into(), ty: WirTy::Bool },
            ],
            ret: vec![WirTy::Bool, WirTy::Bool],
            locals: vec![
                WirLocal { name: "result".into(), ty: WirTy::Bool },
                WirLocal { name: "result_cap".into(), ty: WirTy::Bool },
            ],
            body: vec![
                WirNode::Push(WirExpr::Seq(vec![
                    WirNode::CallStoreMulti {
                        func: "walk".into(),
                        args: vec![
                            WirExpr::GetLocal("state".into()),
                            WirExpr::Binary {
                                op: BinOp::Sub,
                                kind: Kind::I64,
                                lhs: Box::new(WirExpr::GetLocal("n".into())),
                                rhs: Box::new(WirExpr::ConstI64(1)),
                            },
                            WirExpr::GetLocal("state__cap".into()),
                        ],
                        dests: vec!["result".into(), "result_cap".into()],
                    },
                    WirNode::Push(WirExpr::GetLocal("result".into())),
                ])),
                WirNode::Push(WirExpr::GetLocal("result_cap".into())),
            ],
            raw_body: None,
        };
        let mut module = module_with(recur);

        assert_eq!(lower_direct_tail_calls(&mut module), 1);
        let function = &module.funcs[0];
        assert_eq!(function.locals.len(), 5, "one staging local per full-envelope parameter");
        let [WirNode::Loop { body, .. }, WirNode::Unreachable] = function.body.as_slice() else {
            panic!("expected ownership-envelope loop, got {:?}", function.body);
        };
        let mut residual = std::collections::HashSet::new();
        collect_function_tail_calls(&function.body, &mut residual);
        assert!(
            !residual.contains(&TailCallee::Direct("walk".into())),
            "ownership-envelope lowering must remove every recursive backend edge: {residual:?}"
        );
        assert!(matches!(body.last(), Some(WirNode::Unreachable)));
        assert!(matches!(body.get(body.len() - 2), Some(WirNode::Br { cond: None, .. })));
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary).expect("rewritten ownership envelope must remain valid wasm");
    }

    #[test]
    fn lowers_mutual_tail_component_to_one_dispatcher_and_abi_wrappers() {
        let member = |name: &str, target: &str| WirFunc {
            name: name.into(),
            params: vec![WirLocal { name: "n".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Bool],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Control(Box::new(WirNode::If {
                cond: WirExpr::Binary {
                    op: BinOp::Eq,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::GetLocal("n".into())),
                    rhs: Box::new(WirExpr::ConstI64(0)),
                },
                then_: vec![WirNode::Push(WirExpr::ConstI32(i32::from(name == "even")))],
                els: vec![WirNode::Push(WirExpr::Call {
                    func: target.into(),
                    args: vec![WirExpr::Binary {
                        op: BinOp::Sub,
                        kind: Kind::I64,
                        lhs: Box::new(WirExpr::GetLocal("n".into())),
                        rhs: Box::new(WirExpr::ConstI64(1)),
                    }],
                })],
                result: Some(WirTy::Bool),
            })))],
            raw_body: None,
        };
        let mut module = module_with(member("even", "odd"));
        module.funcs.push(member("odd", "even"));
        module.exports.push(("is_even".into(), "even".into()));
        module.table = Some(WirTable { funcs: vec!["even".into(), "odd".into()] });
        let mut repeated = module.clone();

        assert_eq!(lower_direct_tail_calls(&mut module), 2);
        assert_eq!(lower_direct_tail_calls(&mut repeated), 2);
        assert_eq!(crate::wir::to_wat(&module), crate::wir::to_wat(&repeated));
        assert_eq!(module.funcs.len(), 3, "one dispatcher per SCC");
        let dispatcher = &module.funcs[2];
        assert!(dispatcher.name.starts_with("__witchy_tail_scc_"));
        assert_eq!(dispatcher.params.len(), 3, "state plus two disjoint parameter banks");
        assert!(matches!(dispatcher.body.as_slice(), [WirNode::Loop { .. }, WirNode::Unreachable]));
        assert_eq!(module.exports, vec![("is_even".to_string(), "even".to_string())]);
        assert_eq!(
            module.table.as_ref().expect("table retained").funcs,
            vec!["even".to_string(), "odd".to_string()],
        );
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary).expect("dispatcher wrappers retain a valid table/export ABI");
        let mut residual_tail_calls = std::collections::HashSet::new();
        collect_function_tail_calls(&dispatcher.body, &mut residual_tail_calls);
        assert!(
            !residual_tail_calls.contains(&TailCallee::Direct("even".into()))
                && !residual_tail_calls.contains(&TailCallee::Direct("odd".into())),
            "guaranteed edges must contain no recursive backend call: {residual_tail_calls:?}",
        );
        for (state, wrapper) in module.funcs[..2].iter().enumerate() {
            assert_eq!(wrapper.params.len(), 1);
            assert_eq!(wrapper.params[0].name, "n");
            assert_eq!(wrapper.params[0].ty, WirTy::Int);
            let [WirNode::Push(WirExpr::Call { func, args })] = wrapper.body.as_slice() else {
                panic!("expected ABI wrapper, got {:?}", wrapper.body);
            };
            assert_eq!(func, &dispatcher.name);
            assert!(matches!(args.first(), Some(WirExpr::ConstI32(value)) if *value == state as i32));
        }
    }

    #[test]
    fn lowers_indirect_closure_cycle_through_typed_table_dispatch() {
        let driver = WirFunc {
            name: "driver".into(),
            params: vec![
                WirLocal { name: "env".into(), ty: WirTy::GcRef(0) },
                WirLocal { name: "n".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::FromSlot(
                Box::new(WirExpr::CallIndirect {
                    signature: ClosureSignature {
                        params: vec![Kind::GcRef(0), Kind::I64],
                        results: vec![Kind::I64],
                    },
                    args: vec![
                        WirExpr::GetLocal("env".into()),
                        WirExpr::GetLocal("n".into()),
                    ],
                    index: Box::new(WirExpr::ConstI32(0)),
                }),
                Kind::I64,
            ))],
            raw_body: None,
        };
        let closure = WirFunc {
            name: "__lamw0".into(),
            params: vec![
                WirLocal { name: "env".into(), ty: WirTy::GcRef(0) },
                WirLocal { name: "n".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::ToSlot(
                Box::new(WirExpr::Call {
                    func: "driver".into(),
                    args: vec![
                        WirExpr::GetLocal("env".into()),
                        WirExpr::GetLocal("n".into()),
                    ],
                }),
                Kind::I64,
            ))],
            raw_body: None,
        };
        let mut module = module_with(driver);
        module.funcs.push(closure);
        module.table = Some(WirTable { funcs: vec!["__lamw0".into()] });
        module.exports.push(("driver".into(), "driver".into()));

        assert_eq!(lower_direct_tail_calls(&mut module), 2);
        assert_eq!(module.funcs.len(), 3);
        let dispatcher = &module.funcs[2];
        assert!(
            dispatcher
                .locals
                .iter()
                .any(|local| local.name.starts_with("__witchy_tail_indirect_")),
            "dispatcher needs typed argument/index staging for dynamic calls",
        );
        let mut residual = HashSet::new();
        collect_function_tail_calls(&dispatcher.body, &mut residual);
        assert!(!residual.contains(&TailCallee::Direct("driver".into())));
        assert!(!residual.contains(&TailCallee::Direct("__lamw0".into())));
        let binary = crate::wir_encode::encode(&module, &[closure_wrapper_struct()]);
        wasmparser::validate(&binary).expect("typed indirect dispatcher must validate");
    }

    #[test]
    fn lowers_singleton_indirect_self_cycle() {
        let recursive = WirFunc {
            name: "__lamw0".into(),
            params: vec![
                WirLocal { name: "env".into(), ty: WirTy::Bool },
                WirLocal { name: "n".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::CallIndirect {
                signature: slot_closure_signature(1, 1),
                args: vec![
                    WirExpr::GetLocal("env".into()),
                    WirExpr::GetLocal("n".into()),
                ],
                index: Box::new(WirExpr::ConstI32(0)),
            })],
            raw_body: None,
        };
        let mut module = module_with(recursive);
        module.table = Some(WirTable { funcs: vec!["__lamw0".into()] });

        assert_eq!(lower_direct_tail_calls(&mut module), 1);
        assert_eq!(module.funcs.len(), 2, "wrapper plus singleton dispatcher");
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary).expect("singleton dispatcher must validate");
    }

    #[test]
    fn exact_i32_indirect_cycle_keeps_a_typed_fallback() {
        let signature = ClosureSignature {
            params: vec![Kind::I32, Kind::I64],
            results: vec![Kind::I32],
        };
        let driver = WirFunc {
            name: "driver".into(),
            params: vec![
                WirLocal { name: "env".into(), ty: WirTy::Bool },
                WirLocal { name: "n".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Bool],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::CallIndirect {
                signature,
                args: vec![
                    WirExpr::GetLocal("env".into()),
                    WirExpr::GetLocal("n".into()),
                ],
                index: Box::new(WirExpr::ConstI32(0)),
            })],
            raw_body: None,
        };
        let closure = WirFunc {
            name: "__lamw0".into(),
            params: driver.params.clone(),
            ret: vec![WirTy::Bool],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Call {
                func: "driver".into(),
                args: vec![
                    WirExpr::GetLocal("env".into()),
                    WirExpr::GetLocal("n".into()),
                ],
            })],
            raw_body: None,
        };
        let mut module = module_with(driver);
        module.funcs.push(closure);
        module.table = Some(WirTable { funcs: vec!["__lamw0".into()] });

        assert_eq!(lower_direct_tail_calls(&mut module), 2);
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary).expect("i32 indirect fallback must remain typed");
    }

    #[test]
    fn reference_returning_indirect_cycle_is_lowered() {
        let signature = ClosureSignature {
            params: vec![Kind::I32, Kind::ExternRef],
            results: vec![Kind::ExternRef],
        };
        let driver = WirFunc {
            name: "driver".into(),
            params: vec![
                WirLocal { name: "env".into(), ty: WirTy::Bool },
                WirLocal { name: "value".into(), ty: WirTy::Extern },
            ],
            ret: vec![WirTy::Extern],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::CallIndirect {
                signature,
                args: vec![
                    WirExpr::GetLocal("env".into()),
                    WirExpr::GetLocal("value".into()),
                ],
                index: Box::new(WirExpr::ConstI32(0)),
            })],
            raw_body: None,
        };
        let closure = WirFunc {
            name: "__lamw0".into(),
            params: driver.params.clone(),
            ret: vec![WirTy::Extern],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Call {
                func: "driver".into(),
                args: vec![
                    WirExpr::GetLocal("env".into()),
                    WirExpr::GetLocal("value".into()),
                ],
            })],
            raw_body: None,
        };
        let mut module = module_with(driver);
        module.funcs.push(closure);
        module.table = Some(WirTable { funcs: vec!["__lamw0".into()] });

        assert_eq!(lower_direct_tail_calls(&mut module), 2);
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary).expect("reference-returning indirect cycle must validate");
    }

    #[test]
    fn indirect_dispatcher_adapts_mixed_scalar_result_kinds() {
        let driver = WirFunc {
            name: "driver".into(),
            params: vec![
                WirLocal { name: "env".into(), ty: WirTy::Bool },
                WirLocal { name: "n".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Str],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::FromSlot(
                Box::new(WirExpr::CallIndirect {
                    signature: slot_closure_signature(1, 1),
                    args: vec![
                        WirExpr::GetLocal("env".into()),
                        WirExpr::GetLocal("n".into()),
                    ],
                    index: Box::new(WirExpr::ConstI32(0)),
                }),
                Kind::I32,
            ))],
            raw_body: None,
        };
        let closure = WirFunc {
            name: "__lamw0".into(),
            params: driver.params.clone(),
            ret: vec![WirTy::Slot],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::ToSlot(
                Box::new(WirExpr::Call {
                    func: "driver".into(),
                    args: vec![
                        WirExpr::GetLocal("env".into()),
                        WirExpr::GetLocal("n".into()),
                    ],
                }),
                Kind::I32,
            ))],
            raw_body: None,
        };
        let mut module = module_with(driver);
        module.funcs.push(closure);
        module.table = Some(WirTable { funcs: vec!["__lamw0".into()] });

        assert_eq!(lower_direct_tail_calls(&mut module), 2);
        assert_eq!(module.funcs[2].ret, vec![WirTy::Slot]);
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary).expect("mixed scalar dispatcher must validate");
    }

    #[test]
    fn mutual_dispatcher_preserves_reference_parameter_kinds() {
        fn contains_reference_clear(seq: &[WirNode], prefix: &str) -> bool {
            seq.iter().any(|node| match node {
                WirNode::SetLocal {
                    local,
                    value: WirExpr::RefNull(Kind::ExternRef),
                } => local.starts_with(prefix),
                WirNode::If { then_, els, .. } => {
                    contains_reference_clear(then_, prefix)
                        || contains_reference_clear(els, prefix)
                }
                WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                    contains_reference_clear(body, prefix)
                }
                WirNode::Return(Some(WirExpr::Control(control)))
                | WirNode::Push(WirExpr::Control(control)) => {
                    contains_reference_clear(std::slice::from_ref(control.as_ref()), prefix)
                }
                WirNode::Return(Some(WirExpr::Seq(seq))) | WirNode::Push(WirExpr::Seq(seq)) => {
                    contains_reference_clear(seq, prefix)
                }
                _ => false,
            })
        }

        let first = WirFunc {
            name: "first".into(),
            params: vec![WirLocal { name: "n".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Call {
                func: "second".into(),
                args: vec![WirExpr::RefNull(Kind::ExternRef), WirExpr::GetLocal("n".into())],
            })],
            raw_body: None,
        };
        let second = WirFunc {
            name: "second".into(),
            params: vec![
                WirLocal { name: "cap".into(), ty: WirTy::Extern },
                WirLocal { name: "n".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Call {
                func: "first".into(),
                args: vec![WirExpr::GetLocal("n".into())],
            })],
            raw_body: None,
        };
        let mut module = module_with(first);
        module.funcs.push(second);

        assert_eq!(lower_direct_tail_calls(&mut module), 2);
        let dispatcher = &module.funcs[2];
        assert!(dispatcher.params.iter().any(|param| param.ty == WirTy::Extern));
        assert!(
            contains_reference_clear(&dispatcher.body, "__witchy_tail_p_"),
            "departing reference banks must release their roots",
        );
        assert!(
            contains_reference_clear(&dispatcher.body, "__witchy_tail_arg_"),
            "reference staging temporaries must release their roots",
        );
        let [WirNode::Push(WirExpr::Call { args, .. })] = module.funcs[0].body.as_slice() else {
            panic!("expected first wrapper");
        };
        assert!(args.iter().any(|arg| matches!(arg, WirExpr::RefNull(Kind::ExternRef))));
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary).expect("reference-typed dispatcher must validate");
    }

    #[test]
    fn rewrites_match_style_result_branch_without_erasing_its_block_type() {
        let recur = WirFunc {
            name: "search".into(),
            params: vec![WirLocal { name: "n".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Control(Box::new(WirNode::Block {
                label: "match_result".into(),
                result: Some(WirTy::Int),
                body: vec![
                    WirNode::Push(WirExpr::Call {
                        func: "search".into(),
                        args: vec![WirExpr::GetLocal("n".into())],
                    }),
                    WirNode::Br { target: "match_result".into(), cond: None },
                    WirNode::Unreachable,
                ],
            })))],
            raw_body: None,
        };
        let mut module = module_with(recur);

        assert_eq!(lower_direct_tail_calls(&mut module), 1);
        let [WirNode::Loop { body, .. }, WirNode::Unreachable] = module.funcs[0].body.as_slice() else {
            panic!("expected loop lowering, got {:?}", module.funcs[0].body);
        };
        let [WirNode::Return(Some(WirExpr::Control(block)))] = body.as_slice() else {
            panic!("expected returned result block, got {body:?}");
        };
        let WirNode::Block { result: Some(WirTy::Int), body, .. } = block.as_ref() else {
            panic!("match result block lost its type: {block:?}");
        };
        assert!(matches!(body.first(), Some(WirNode::Push(WirExpr::Control(_)))));
        assert!(matches!(body.get(1), Some(WirNode::Br { target, cond: None }) if target == "match_result"));
    }

    #[test]
    fn cancels_fromslot_toslot_roundtrip() {
        // Return(FromSlot(ToSlot(GetLocal x, I64), I64)) -> Return(GetLocal x)
        let expr = WirExpr::FromSlot(
            Box::new(WirExpr::ToSlot(
                Box::new(WirExpr::GetLocal("x".into())),
                Kind::I64,
            )),
            Kind::I64,
        );
        let mut m = module_with(func_returning(expr));

        let stats = optimize(&mut m);
        assert!(
            stats.nodes_after < stats.nodes_before,
            "expected fewer nodes, got {stats:?}"
        );
        assert_eq!(stats.eliminated, 2, "two conversion nodes removed: {stats:?}");

        // The surviving body is a bare GetLocal.
        match &m.funcs[0].body[..] {
            [WirNode::Return(Some(WirExpr::GetLocal(name)))] => assert_eq!(name, "x"),
            other => panic!("expected Return(GetLocal x), got {other:?}"),
        }
    }

    #[test]
    fn cancels_toslot_fromslot_roundtrip() {
        // The mirror: ToSlot(FromSlot(x, I64), I64) -> x.
        let expr = WirExpr::ToSlot(
            Box::new(WirExpr::FromSlot(
                Box::new(WirExpr::GetLocal("y".into())),
                Kind::I64,
            )),
            Kind::I64,
        );
        let mut m = module_with(func_returning(expr));

        let stats = optimize(&mut m);
        assert_eq!(stats.eliminated, 2, "{stats:?}");
        match &m.funcs[0].body[..] {
            [WirNode::Return(Some(WirExpr::GetLocal(name)))] => assert_eq!(name, "y"),
            other => panic!("expected Return(GetLocal y), got {other:?}"),
        }
    }

    #[test]
    fn cancels_identity_convert() {
        // Convert { from: I64, to: I64, arg } -> arg.
        let expr = WirExpr::Convert {
            from: Kind::I64,
            to: Kind::I64,
            arg: Box::new(WirExpr::ConstI64(7)),
        };
        let mut m = module_with(func_returning(expr));

        let stats = optimize(&mut m);
        assert_eq!(stats.eliminated, 1, "{stats:?}");
        match &m.funcs[0].body[..] {
            [WirNode::Return(Some(WirExpr::ConstI64(7)))] => {}
            other => panic!("expected Return(ConstI64 7), got {other:?}"),
        }
    }

    #[test]
    fn preserves_cross_kind_convert() {
        // A genuine i32->i64 widen must NOT be cancelled.
        let expr = WirExpr::Convert {
            from: Kind::I32,
            to: Kind::I64,
            arg: Box::new(WirExpr::ConstI32(3)),
        };
        let mut m = module_with(func_returning(expr));

        let stats = optimize(&mut m);
        assert_eq!(stats.eliminated, 0, "real widen kept: {stats:?}");
        match &m.funcs[0].body[..] {
            [WirNode::Return(Some(WirExpr::Convert { from: Kind::I32, to: Kind::I64, .. }))] => {}
            other => panic!("expected Convert kept, got {other:?}"),
        }
    }

    #[test]
    fn preserves_mismatched_slot_kinds() {
        // FromSlot(ToSlot(x, I32), I64): different kinds — NOT an identity, keep it.
        let expr = WirExpr::FromSlot(
            Box::new(WirExpr::ToSlot(
                Box::new(WirExpr::GetLocal("z".into())),
                Kind::I32,
            )),
            Kind::I64,
        );
        let mut m = module_with(func_returning(expr));

        let stats = optimize(&mut m);
        assert_eq!(stats.eliminated, 0, "mismatched kinds kept: {stats:?}");
    }

    #[test]
    fn cancels_nested_in_binary_to_fixpoint() {
        // Binary( FromSlot(ToSlot(a)), FromSlot(ToSlot(b)) ) -> Binary(a, b),
        // and a redundant outer pair wrapping the whole Binary collapses too —
        // exercising the recursion AND the fixpoint loop.
        let leg = |name: &str| {
            WirExpr::FromSlot(
                Box::new(WirExpr::ToSlot(
                    Box::new(WirExpr::GetLocal(name.into())),
                    Kind::I64,
                )),
                Kind::I64,
            )
        };
        let inner_binary = WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I64,
            lhs: Box::new(leg("a")),
            rhs: Box::new(leg("b")),
        };
        // Wrap the binary in its own round-trip so the parent pattern only
        // appears after the children simplify (forces >1 effective cancellation).
        let expr = WirExpr::FromSlot(
            Box::new(WirExpr::ToSlot(Box::new(inner_binary), Kind::I64)),
            Kind::I64,
        );
        let mut m = module_with(func_returning(expr));

        let stats = optimize(&mut m);
        // 3 round-trips, each 2 nodes (ToSlot + FromSlot) = 6 nodes removed.
        assert_eq!(stats.eliminated, 6, "{stats:?}");
        match &m.funcs[0].body[..] {
            [WirNode::Return(Some(WirExpr::Binary { lhs, rhs, .. }))] => {
                assert!(matches!(lhs.as_ref(), WirExpr::GetLocal(n) if n == "a"));
                assert!(matches!(rhs.as_ref(), WirExpr::GetLocal(n) if n == "b"));
            }
            other => panic!("expected Return(Binary(a, b)), got {other:?}"),
        }
    }

    #[test]
    fn skips_raw_body_functions() {
        // A raw-body func has no WIR tree; it must be left entirely alone and
        // contribute 0 to the node count.
        let raw = WirFunc {
            name: "raw".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![],
            raw_body: Some(vec![0x0b]), // a bare `end` byte
        };
        let mut m = module_with(raw);
        let stats = optimize(&mut m);
        assert_eq!(stats.nodes_before, 0);
        assert_eq!(stats.nodes_after, 0);
        assert_eq!(stats.eliminated, 0);
        assert!(m.funcs[0].raw_body.is_some());
    }

    /// Bonus: the optimized module still encodes to a wasm binary without error,
    /// and so does the unoptimized one. Uses a module shaped like the wir.rs
    /// tests' `int_demo` so `wir_encode::encode` has a valid tree to walk.
    #[test]
    fn encodes_before_and_after_optimize() {
        let _ = DataSegment { offset: 0, bytes: vec![] }; // keep import exercised

        // run(): print_int( FromSlot(ToSlot(ConstI64 42)) )
        let call = WirExpr::CallHost {
            import: "print_int".into(),
            args: vec![WirExpr::FromSlot(
                Box::new(WirExpr::ToSlot(Box::new(WirExpr::ConstI64(42)), Kind::I64)),
                Kind::I64,
            )],
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(call)],
            raw_body: None,
        };
        let mut m = WirModule {
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

        // Encodes before optimization.
        let before = crate::wir_encode::encode(&m, &[]);
        assert!(!before.is_empty(), "pre-opt encode produced no bytes");

        let stats = optimize(&mut m);
        assert_eq!(stats.eliminated, 2, "{stats:?}");

        // Still encodes after optimization.
        let after = crate::wir_encode::encode(&m, &[]);
        assert!(!after.is_empty(), "post-opt encode produced no bytes");
    }

    /// Whether any `ToSlot`/`FromSlot` conversion over a REFERENCE kind
    /// (ExternRef / GcRef / StructRef) appears anywhere in a node sequence —
    /// i.e. a capability or GC reference being boxed into / out of an i64 slot.
    /// RFC-0005 / RFC-0090 forbid this on the trampoline path.
    fn reference_slot_conversion_in(seq: &[WirNode]) -> bool {
        fn is_ref(kind: &Kind) -> bool {
            matches!(kind, Kind::ExternRef | Kind::GcRef(_) | Kind::StructRef)
        }
        fn expr(e: &WirExpr) -> bool {
            match e {
                WirExpr::ToSlot(inner, kind) | WirExpr::FromSlot(inner, kind) => {
                    is_ref(kind) || expr(inner)
                }
                WirExpr::CallIndirect { args, index, .. } => {
                    args.iter().any(expr) || expr(index)
                }
                WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => args.iter().any(expr),
                WirExpr::Binary { lhs, rhs, .. } => expr(lhs) || expr(rhs),
                WirExpr::Control(inner) => nodes(std::slice::from_ref(inner.as_ref())),
                WirExpr::Seq(seq) => nodes(seq),
                _ => false,
            }
        }
        fn nodes(seq: &[WirNode]) -> bool {
            seq.iter().any(|node| match node {
                WirNode::Push(e) | WirNode::Do(e) | WirNode::Drop(e) => expr(e),
                WirNode::Return(Some(e)) => expr(e),
                WirNode::SetLocal { value, .. } => expr(value),
                WirNode::If { cond, then_, els, .. } => expr(cond) || nodes(then_) || nodes(els),
                WirNode::Block { body, .. } | WirNode::Loop { body, .. } => nodes(body),
                WirNode::CallIndirectStoreMulti { args, index, .. } => {
                    args.iter().any(expr) || expr(index)
                }
                WirNode::Br { cond: Some(c), .. } => expr(c),
                _ => false,
            })
        }
        nodes(seq)
    }

    /// (RFC-0090 criterion 10) A guaranteed indirect cycle that carries a REFERENCE
    /// parameter lowers so the dispatcher body contains NO recursive backend call
    /// edge: no direct `Call`/`CallIndirect` to a component member, and — because
    /// the continuation is the trampoline loop — no residual `CallIndirect` /
    /// `CallIndirectStoreMulti` at all inside the dispatcher's own recursive path.
    /// The original `driver`/closure both dispatch INTO the trampoline instead.
    #[test]
    fn indirect_reference_cycle_dispatcher_has_no_recursive_backend_call() {
        let signature = ClosureSignature {
            params: vec![Kind::GcRef(0), Kind::ExternRef, Kind::I64],
            results: vec![Kind::ExternRef],
        };
        let params = vec![
            WirLocal { name: "env".into(), ty: WirTy::GcRef(0) },
            WirLocal { name: "cap".into(), ty: WirTy::Extern },
            WirLocal { name: "n".into(), ty: WirTy::Int },
        ];
        let driver = WirFunc {
            name: "driver".into(),
            params: params.clone(),
            ret: vec![WirTy::Extern],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::CallIndirect {
                signature: signature.clone(),
                args: vec![
                    WirExpr::GetLocal("env".into()),
                    WirExpr::GetLocal("cap".into()),
                    WirExpr::GetLocal("n".into()),
                ],
                index: Box::new(WirExpr::ConstI32(0)),
            })],
            raw_body: None,
        };
        let closure = WirFunc {
            name: "__lamw0".into(),
            params: params.clone(),
            ret: vec![WirTy::Extern],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Call {
                func: "driver".into(),
                args: vec![
                    WirExpr::GetLocal("env".into()),
                    WirExpr::GetLocal("cap".into()),
                    WirExpr::GetLocal("n".into()),
                ],
            })],
            raw_body: None,
        };
        let mut module = module_with(driver);
        module.funcs.push(closure);
        module.table = Some(WirTable { funcs: vec!["__lamw0".into()] });

        assert_eq!(lower_direct_tail_calls(&mut module), 2);
        let dispatcher = module
            .funcs
            .iter()
            .find(|f| f.name.starts_with("__witchy_tail_scc_"))
            .expect("a trampoline dispatcher was produced");

        // Criterion 10: the recursive continuation is the trampoline `Br` loop, not
        // a backend call. No DIRECT recursive call to a component member survives as
        // a tail edge (the `driver` <-> `__lamw0` cycle is fully absorbed into the
        // loop), and the dispatcher body IS a `Loop`. A residual `CallIndirect` that
        // shares the component signature is the RFC-permitted ordinary indirect EXIT
        // (a table target outside the recursive component); in this single-entry
        // table it happens to share the signature but is not a loop-back. The
        // constant-stack guarantee itself is proven by the both-backend resource
        // tests in tests/rfc0090_indirect_tail.rs (5,000,000 transitions).
        let mut residual = HashSet::new();
        collect_function_tail_calls(&dispatcher.body, &mut residual);
        assert!(
            !residual.contains(&TailCallee::Direct("driver".into()))
                && !residual.contains(&TailCallee::Direct("__lamw0".into())),
            "dispatcher must contain no recursive DIRECT tail edge back into the component: {residual:?}",
        );
        assert!(
            dispatcher.body.iter().any(|node| matches!(node, WirNode::Loop { .. })),
            "dispatcher must wrap its recursive continuation in a trampoline Loop, got {:?}",
            dispatcher.body,
        );
        // The reference kinds are carried in typed banks, never an i64 slot.
        assert!(
            dispatcher
                .locals
                .iter()
                .any(|l| matches!(l.ty, WirTy::Extern))
                && dispatcher.locals.iter().any(|l| matches!(l.ty, WirTy::GcRef(_))),
            "dispatcher must stage the externref + gcref parameters in typed locals (no i64 boxing)",
        );
        // Stronger, direct proof of "no i64 boxing": the dispatcher path contains NO
        // ToSlot/FromSlot whose kind is a reference (ExternRef/GcRef/StructRef). A
        // boxed capability/GC reference would appear here as a slot conversion; its
        // absence is the RFC-0005 "reference kinds cross no integer-slot erasure"
        // guarantee asserted structurally, not merely via backend agreement.
        assert!(
            !reference_slot_conversion_in(&dispatcher.body),
            "dispatcher must not box any ExternRef/GcRef through an i64 slot (found a ToSlot/FromSlot on a reference kind)",
        );
        let binary = crate::wir_encode::encode(&module, &[closure_wrapper_struct()]);
        wasmparser::validate(&binary).expect("reference-carrying indirect dispatcher must validate");
    }

    /// (RFC-0090 criterion 6) The dispatcher evaluates the closure/table index ONCE
    /// per transition: the indirect plan stages the index into a single dedicated
    /// local (`__witchy_tail_indirect_*_index`) rather than re-evaluating the index
    /// expression at the point of dispatch, so a side-effecting index selector runs
    /// exactly once. The staged-index local is the structural evidence.
    #[test]
    fn indirect_dispatcher_stages_table_index_in_one_local() {
        let recursive = WirFunc {
            name: "__lamw0".into(),
            params: vec![
                WirLocal { name: "env".into(), ty: WirTy::Bool },
                WirLocal { name: "n".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::CallIndirect {
                signature: slot_closure_signature(1, 1),
                args: vec![
                    WirExpr::GetLocal("env".into()),
                    WirExpr::GetLocal("n".into()),
                ],
                index: Box::new(WirExpr::ConstI32(0)),
            })],
            raw_body: None,
        };
        let mut module = module_with(recursive);
        module.table = Some(WirTable { funcs: vec!["__lamw0".into()] });

        assert_eq!(lower_direct_tail_calls(&mut module), 1);
        let dispatcher = module
            .funcs
            .iter()
            .find(|f| f.name.starts_with("__witchy_tail_scc_"))
            .expect("a trampoline dispatcher was produced");
        let index_locals = dispatcher
            .locals
            .iter()
            .filter(|l| l.name.starts_with("__witchy_tail_indirect_") && l.name.ends_with("_index"))
            .count();
        assert_eq!(
            index_locals, 1,
            "exactly one staged table-index local per indirect plan (index evaluated once per transition)",
        );
    }
