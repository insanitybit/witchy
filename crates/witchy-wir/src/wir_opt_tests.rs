    use std::collections::{HashMap, HashSet};

    use super::*;
    use super::tail_calls::{
        TailCallee, collect_function_tail_calls, rename_expr_locals, rename_node_locals,
    };
    use crate::wir::{
        BinOp, ClosureSignature, Kind, WirExpr, WirFunc, WirImport, WirLocal,
        WirModule, WirNode, WirTable, WirTy, CLOSURE_CODE_FIELD, closure_wrapper_struct,
        slot_closure_signature,
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

    fn closure_value(index: i32) -> WirExpr {
        WirExpr::StructNew {
            struct_id: 0,
            args: vec![WirExpr::ConstI32(index)],
        }
    }

    fn indirect_closure_call(local: &str) -> WirExpr {
        WirExpr::CallIndirect {
            signature: ClosureSignature {
                params: vec![],
                results: vec![],
            },
            args: vec![],
            index: Box::new(WirExpr::StructGet {
                struct_id: 0,
                field: CLOSURE_CODE_FIELD,
                base: Box::new(WirExpr::GetLocal(local.into())),
            }),
        }
    }

    #[test]
    fn closure_devirtualization_kills_loop_carried_rebinds_but_keeps_stable_locals() {
        let mut module = module_with(WirFunc {
            name: "main".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::SetLocal {
                    local: "stable".into(),
                    value: closure_value(0),
                },
                WirNode::SetLocal {
                    local: "rebound".into(),
                    value: closure_value(0),
                },
                WirNode::Loop {
                    label: "again".into(),
                    body: vec![
                        WirNode::Do(indirect_closure_call("stable")),
                        WirNode::Do(indirect_closure_call("rebound")),
                        WirNode::If {
                            cond: WirExpr::ConstI32(1),
                            then_: vec![WirNode::SetLocal {
                                local: "rebound".into(),
                                value: closure_value(1),
                            }],
                            els: vec![],
                            result: None,
                        },
                    ],
                },
                WirNode::Do(indirect_closure_call("rebound")),
            ],
            raw_body: None,
        });
        module.table = Some(WirTable {
            funcs: vec!["first".into(), "second".into()],
        });

        inline_direct_calls(&mut module);

        let [_, _, WirNode::Loop { body, .. }, WirNode::Do(after)] =
            module.funcs[0].body.as_slice()
        else {
            panic!("unexpected optimized body: {:?}", module.funcs[0].body);
        };
        assert!(
            matches!(&body[0], WirNode::Do(WirExpr::Call { func, .. }) if func == "first"),
            "a closure never assigned by the loop remains directly callable: {body:?}",
        );
        assert!(
            matches!(&body[1], WirNode::Do(WirExpr::CallIndirect { .. })),
            "a loop-carried closure rebind must retain dynamic dispatch: {body:?}",
        );
        assert!(
            matches!(after, WirExpr::CallIndirect { .. }),
            "a closure assigned by a maybe-zero-iteration loop is unknown after the loop: {after:?}",
        );
    }

    #[test]
    fn closure_devirtualization_kills_block_rebinds_across_conditional_exit() {
        let mut module = module_with(WirFunc {
            name: "main".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::SetLocal {
                    local: "stable".into(),
                    value: closure_value(0),
                },
                WirNode::SetLocal {
                    local: "rebound".into(),
                    value: closure_value(0),
                },
                WirNode::Block {
                    label: "exit".into(),
                    result: None,
                    body: vec![
                        WirNode::Br {
                            target: "exit".into(),
                            cond: Some(WirExpr::ConstI32(1)),
                        },
                        WirNode::SetLocal {
                            local: "rebound".into(),
                            value: closure_value(1),
                        },
                        WirNode::Do(indirect_closure_call("rebound")),
                    ],
                },
                WirNode::Do(indirect_closure_call("stable")),
                WirNode::Do(indirect_closure_call("rebound")),
            ],
            raw_body: None,
        });
        module.table = Some(WirTable {
            funcs: vec!["first".into(), "second".into()],
        });

        inline_direct_calls(&mut module);

        let [_, _, WirNode::Block { body, .. }, WirNode::Do(stable), WirNode::Do(rebound)] =
            module.funcs[0].body.as_slice()
        else {
            panic!("unexpected optimized body: {:?}", module.funcs[0].body);
        };
        assert!(
            matches!(&body[2], WirNode::Do(WirExpr::Call { func, .. }) if func == "second"),
            "the fallthrough path may learn the closure assigned after the branch: {body:?}",
        );
        assert!(
            matches!(stable, WirExpr::Call { func, .. } if func == "first"),
            "a closure never assigned by the block remains directly callable: {stable:?}",
        );
        assert!(
            matches!(rebound, WirExpr::CallIndirect { .. }),
            "a conditional block exit must join the incoming and rebound targets: {rebound:?}",
        );
    }

    #[test]
    fn closure_devirtualization_kills_conditional_multi_result_destinations() {
        let mut module = module_with(WirFunc {
            name: "main".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::SetLocal {
                    local: "closure".into(),
                    value: closure_value(0),
                },
                WirNode::If {
                    cond: WirExpr::ConstI32(1),
                    then_: vec![WirNode::CallStoreMulti {
                        func: "replace".into(),
                        args: vec![],
                        dests: vec!["closure".into()],
                    }],
                    els: vec![],
                    result: None,
                },
                WirNode::Do(indirect_closure_call("closure")),
            ],
            raw_body: None,
        });
        module.table = Some(WirTable {
            funcs: vec!["first".into()],
        });

        inline_direct_calls(&mut module);

        assert!(
            matches!(&module.funcs[0].body[2], WirNode::Do(WirExpr::CallIndirect { .. })),
            "a conditional call destination cannot retain its incoming closure target: {:?}",
            module.funcs[0].body,
        );
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
    fn self_tail_local_bank_resets_once_per_iteration() {
        let recur = WirFunc {
            name: "recur".into(),
            params: vec![WirLocal { name: "value".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: (0..4)
                .map(|index| WirLocal {
                    name: format!("scratch_{index}"),
                    ty: WirTy::Int,
                })
                .collect(),
            body: vec![WirNode::Return(Some(WirExpr::Call {
                func: "recur".into(),
                args: vec![WirExpr::GetLocal("value".into())],
            }))],
            raw_body: None,
        };
        let mut module = module_with(recur);

        assert_eq!(lower_direct_tail_calls(&mut module), 1);
        let [WirNode::Loop { body, .. }, WirNode::Unreachable] =
            module.funcs[0].body.as_slice()
        else {
            panic!("expected loop-wrapped function, got {:?}", module.funcs[0].body);
        };
        for (index, node) in body[..4].iter().enumerate() {
            assert!(
                matches!(node, WirNode::SetLocal { local, value: WirExpr::ConstI64(0) }
                    if local == &format!("scratch_{index}")),
                "local bank must reset once at the loop header: {body:?}"
            );
        }
        let WirNode::Return(Some(WirExpr::Control(escape))) = &body[4] else {
            panic!("expected tail escape after loop-local resets, got {body:?}");
        };
        let WirNode::Block { body: escape_body, .. } = escape.as_ref() else {
            panic!("expected typed escape block, got {escape:?}");
        };
        assert_eq!(escape_body.len(), 6, "the edge must not repeat the four local resets");
        assert!(escape_body.iter().all(|node| !matches!(
            node,
            WirNode::SetLocal { local, .. } if local.starts_with("scratch_")
        )));
    }

    #[test]
    fn source_wrappers_preserve_self_tail_call_lowering() {
        let recur = WirFunc {
            name: "count".into(),
            params: vec![WirLocal { name: "n".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Source {
                line: 7,
                body: vec![WirNode::Push(WirExpr::Call {
                    func: "count".into(),
                    args: vec![WirExpr::GetLocal("n".into())],
                })],
            }],
            raw_body: None,
        };
        let mut module = module_with(recur);

        assert_eq!(lower_direct_tail_calls(&mut module), 1);
        let [WirNode::Loop { body, .. }, WirNode::Unreachable] = module.funcs[0].body.as_slice()
        else {
            panic!("expected loop-wrapped function, got {:?}", module.funcs[0].body);
        };
        assert!(body.iter().any(|node| matches!(node, WirNode::Source { line: 7, .. })));
        let mut residual = std::collections::HashSet::new();
        collect_function_tail_calls(&module.funcs[0].body, &mut residual);
        assert!(!residual.contains(&TailCallee::Direct("count".into())));
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
        assert_eq!(
            function.locals.len(),
            6,
            "three argument staging locals plus the declared-result exit local"
        );
        let [WirNode::Block { body: exit_body, .. }, WirNode::Push(_), WirNode::Push(_)] =
            function.body.as_slice()
        else {
            panic!(
                "expected an ownership-envelope loop and result epilogue, got {:?}",
                function.body
            );
        };
        let [WirNode::Loop { body, .. }, WirNode::Unreachable] = exit_body.as_slice() else {
            panic!("expected a loop inside the resultless exit block, got {exit_body:?}");
        };
        let mut residual = std::collections::HashSet::new();
        collect_function_tail_calls(&function.body, &mut residual);
        assert!(
            !residual.contains(&TailCallee::Direct("walk".into())),
            "ownership-envelope lowering must remove every recursive backend edge: {residual:?}"
        );
        assert!(matches!(body.last(), Some(WirNode::Br { cond: None, .. })));
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary).expect("rewritten ownership envelope must remain valid wasm");
    }

    #[test]
    fn lowers_mutual_forwarded_ownership_envelopes_to_one_dispatcher() {
        let member = |name: &str, target: &str| WirFunc {
            name: name.into(),
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
                        func: target.into(),
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
        let mut module = module_with(member("even", "odd"));
        module.funcs.push(member("odd", "even"));

        assert_eq!(lower_direct_tail_calls(&mut module), 2);
        let dispatcher = module
            .funcs
            .iter()
            .find(|function| function.name.starts_with("__witchy_tail_envelope_scc_"))
            .expect("one multi-result ownership-envelope dispatcher");
        assert_eq!(dispatcher.ret, [WirTy::Bool, WirTy::Bool]);
        assert!(
            dispatcher.body.iter().any(|node| matches!(node, WirNode::Block { body, .. }
                if body.iter().any(|node| matches!(node, WirNode::Loop { .. })))),
            "the mutual ownership continuation must be one resultless loop"
        );
        let mut residual = HashSet::new();
        collect_function_tail_calls(&dispatcher.body, &mut residual);
        assert!(
            !residual.contains(&TailCallee::Direct("even".into()))
                && !residual.contains(&TailCallee::Direct("odd".into())),
            "the dispatcher must contain no recursive backend edge: {residual:?}"
        );
        for wrapper in module.funcs.iter().filter(|function| function.name == "even" || function.name == "odd") {
            assert!(matches!(wrapper.body.first(), Some(WirNode::CallStoreMulti { func, dests, .. })
                if func == &dispatcher.name && dests.len() == 2));
        }
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary)
            .expect("mutual ownership-envelope dispatcher must remain valid wasm");
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
        assert_eq!(
            dispatcher.params.len(),
            2,
            "state plus one shared typed parameter bank",
        );
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
    fn default_optimizer_preserves_operands_with_observable_evaluation() {
        let effectful = WirExpr::Binary {
            op: BinOp::Mul,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::ConstI64(0)),
            rhs: Box::new(WirExpr::Call {
                func: "effectful_value".into(),
                args: vec![],
            }),
        };
        let trapping = WirExpr::Binary {
            op: BinOp::And,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::Load {
                ptr: Box::new(WirExpr::ConstI32(-1)),
                kind: Kind::I64,
                offset: 0,
            }),
            rhs: Box::new(WirExpr::ConstI64(0)),
        };
        let func = WirFunc {
            name: "preserve_evaluation".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Drop(effectful), WirNode::Drop(trapping)],
            raw_body: None,
        };
        let mut module = module_with(func);

        optimize(&mut module);

        assert!(matches!(
            module.funcs[0].body.as_slice(),
            [
                WirNode::Drop(WirExpr::Binary {
                    op: BinOp::Mul,
                    rhs,
                    ..
                }),
                WirNode::Drop(WirExpr::Binary {
                    op: BinOp::And,
                    lhs,
                    ..
                }),
            ] if matches!(rhs.as_ref(), WirExpr::Call { func, .. } if func == "effectful_value")
                && matches!(lhs.as_ref(), WirExpr::Load { .. })
        ));
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
        assert_eq!(module.funcs.len(), 2, "wrapper plus singleton dispatcher");
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
        let binary = crate::wir_encode::encode(&module, &[]);
        wasmparser::validate(&binary).expect("singleton dispatcher must validate");
    }

    #[test]
    fn evaluation_properties_pure_constants_and_locals() {
        let c = WirExpr::ConstI64(42);
        let prop_c = evaluate_expr(&c);
        assert!(prop_c.total);
        assert!(!prop_c.may_trap);
        assert_eq!(prop_c.host_effect, HostEffect::None);
        assert!(prop_c.is_safe_to_eliminate());

        let l = WirExpr::GetLocal("x".into());
        let prop_l = evaluate_expr(&l);
        assert!(prop_l.total);
        assert!(!prop_l.may_trap);
        assert!(prop_l.is_safe_to_eliminate());
    }

    #[test]
    fn evaluation_properties_division_and_remainder() {
        // x / 2 is trap-free when x is total
        let safe_div = WirExpr::Binary {
            op: BinOp::Div,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("x".into())),
            rhs: Box::new(WirExpr::ConstI64(2)),
        };
        let prop_safe = evaluate_expr(&safe_div);
        assert!(prop_safe.total);
        assert!(!prop_safe.may_trap);

        // x / y may trap on y == 0
        let unsafe_div = WirExpr::Binary {
            op: BinOp::Div,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("x".into())),
            rhs: Box::new(WirExpr::GetLocal("y".into())),
        };
        let prop_unsafe = evaluate_expr(&unsafe_div);
        assert!(!prop_unsafe.total);
        assert!(prop_unsafe.may_trap);
        assert!(!prop_unsafe.is_safe_to_eliminate());
    }

    #[test]
    fn evaluation_properties_memory_and_host_effects() {
        let load = WirExpr::Load {
            ptr: Box::new(WirExpr::GetLocal("ptr".into())),
            kind: Kind::I64,
            offset: 0,
        };
        let prop_load = evaluate_expr(&load);
        assert!(!prop_load.total);
        assert!(prop_load.may_trap);
        assert!(prop_load.memory.reads().is_some());

        let host = WirExpr::CallHost {
            import: "print".into(),
            args: vec![WirExpr::ConstI64(10)],
        };
        let prop_host = evaluate_expr(&host);
        assert!(!prop_host.total);
        assert_eq!(prop_host.host_effect, HostEffect::Present);
        assert!(!prop_host.is_safe_to_eliminate());
    }

    #[test]
    fn evaluation_properties_reordering_legality() {
        let expr1 = WirExpr::ConstI64(1);
        let expr2 = WirExpr::GetLocal("x".into());
        let prop1 = evaluate_expr(&expr1);
        let prop2 = evaluate_expr(&expr2);
        assert!(prop1.is_safe_to_reorder_with(&prop2));

        // Two trapping expressions cannot be reordered (trap order must be preserved)
        let trap1 = WirExpr::Binary {
            op: BinOp::Div,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("a".into())),
            rhs: Box::new(WirExpr::GetLocal("b".into())),
        };
        let trap2 = WirExpr::Binary {
            op: BinOp::Div,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("c".into())),
            rhs: Box::new(WirExpr::GetLocal("d".into())),
        };
        let prop_trap1 = evaluate_expr(&trap1);
        let prop_trap2 = evaluate_expr(&trap2);
        assert!(!prop_trap1.is_safe_to_reorder_with(&prop_trap2));
    }

    #[test]
    fn range_fact_lattice_meet_join_widen() {
        use crate::wir_opt::range_facts::RangeFact;

        let c5 = RangeFact::from_constant(5);
        let c10 = RangeFact::from_constant(10);
        let non_neg = RangeFact::NonNegative { upper: None, no_wrap: true };
        let pos = RangeFact::Positive { upper: None, no_wrap: true };
        let int_0_10 = RangeFact::from_bounds(0, 10, true);
        let int_5_15 = RangeFact::from_bounds(5, 15, true);

        // Meet (intersection)
        assert_eq!(int_0_10.meet(&int_5_15), RangeFact::from_bounds(5, 10, true));
        assert_eq!(non_neg.meet(&c5), c5);
        assert_eq!(pos.meet(&RangeFact::from_bounds(-5, 5, true)), RangeFact::from_bounds(1, 5, true));

        // Join (union across merges)
        assert_eq!(int_0_10.join(&int_5_15), RangeFact::from_bounds(0, 15, true));
        assert_eq!(c5.join(&c10), RangeFact::from_bounds(5, 10, true));

        // Widening (loop convergence)
        let initial_loop_fact = RangeFact::from_bounds(0, 10, true);
        let iteration_growing_up = RangeFact::from_bounds(0, 11, true);
        let widened = initial_loop_fact.widen(&iteration_growing_up);
        assert_eq!(widened, RangeFact::NonNegative { upper: None, no_wrap: true });
    }

    #[test]
    fn range_fact_transfer_functions() {
        use crate::wir_opt::range_facts::{compute_fact, RangeEnv, RangeFact};

        let mut env = RangeEnv::new();
        env.set("x".into(), RangeFact::from_bounds(0, 100, true));
        env.set("y".into(), RangeFact::from_bounds(1, 5, true));

        // x + y
        let add_expr = WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("x".into())),
            rhs: Box::new(WirExpr::GetLocal("y".into())),
        };
        let add_fact = compute_fact(&add_expr, &env);
        assert_eq!(add_fact, RangeFact::from_bounds(1, 105, true));

        // x * 2 (positive constant multiplication)
        let mul_expr = WirExpr::Binary {
            op: BinOp::Mul,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("x".into())),
            rhs: Box::new(WirExpr::ConstI64(2)),
        };
        let mul_fact = compute_fact(&mul_expr, &env);
        assert_eq!(mul_fact, RangeFact::from_bounds(0, 200, true));

        // x / 2 (positive constant division)
        let div_expr = WirExpr::Binary {
            op: BinOp::Div,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("x".into())),
            rhs: Box::new(WirExpr::ConstI64(2)),
        };
        let div_fact = compute_fact(&div_expr, &env);
        assert_eq!(div_fact, RangeFact::from_bounds(0, 50, true));

        // x & 7 (masking)
        let and_expr = WirExpr::Binary {
            op: BinOp::And,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("x".into())),
            rhs: Box::new(WirExpr::ConstI64(7)),
        };
        let and_fact = compute_fact(&and_expr, &env);
        assert_eq!(and_fact, RangeFact::from_bounds(0, 7, true));
    }

    #[test]
    fn range_fact_branch_refinement() {
        use crate::wir_opt::range_facts::{refine_condition, RangeEnv, RangeFact};

        let env = RangeEnv::new();

        // cond: x < 10
        let lt_expr = WirExpr::Binary {
            op: BinOp::Lt,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("x".into())),
            rhs: Box::new(WirExpr::ConstI64(10)),
        };
        let true_facts = refine_condition(&lt_expr, true, &env);
        assert_eq!(true_facts.len(), 1);
        assert_eq!(true_facts[0].0, "x");
        assert_eq!(true_facts[0].1.hi_bound(), Some(9));

        let false_facts = refine_condition(&lt_expr, false, &env);
        assert_eq!(false_facts.len(), 1);
        assert_eq!(false_facts[0].0, "x");
        assert_eq!(false_facts[0].1.lo_bound(), Some(10));

        // cond: x == 42
        let eq_expr = WirExpr::Binary {
            op: BinOp::Eq,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("x".into())),
            rhs: Box::new(WirExpr::ConstI64(42)),
        };
        let eq_true = refine_condition(&eq_expr, true, &env);
        assert_eq!(eq_true[0].1, RangeFact::Constant(42));
    }

    #[test]
    fn redundant_comparison_simplification() {
        let func = WirFunc {
            name: "test_compare".into(),
            params: vec![WirLocal { name: "x".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![
                // If x > 10:
                WirNode::If {
                    cond: WirExpr::Binary {
                        op: BinOp::Gt,
                        kind: Kind::I64,
                        lhs: Box::new(WirExpr::GetLocal("x".into())),
                        rhs: Box::new(WirExpr::ConstI64(10)),
                    },
                    then_: vec![
                        // Inside then, x is known >= 11, so x > 5 is unconditionally true (1)
                        WirNode::Push(WirExpr::Binary {
                            op: BinOp::Gt,
                            kind: Kind::I64,
                            lhs: Box::new(WirExpr::GetLocal("x".into())),
                            rhs: Box::new(WirExpr::ConstI64(5)),
                        }),
                    ],
                    els: vec![
                        // Inside else, x is <= 10, so x > 20 is unconditionally false (0)
                        WirNode::Push(WirExpr::Binary {
                            op: BinOp::Gt,
                            kind: Kind::I64,
                            lhs: Box::new(WirExpr::GetLocal("x".into())),
                            rhs: Box::new(WirExpr::ConstI64(20)),
                        }),
                    ],
                    result: Some(WirTy::Int),
                },
            ],
            raw_body: None,
        };

        let mut module = module_with(func.clone());
        crate::wir_opt::strength_reduce::simplify_integer_ranges(&mut module);

        let simplified = &module.funcs[0];
        let WirNode::If { then_, els, .. } = &simplified.body[0] else { panic!() };
        assert!(matches!(&then_[0], WirNode::Push(WirExpr::ConstI64(1))));
        assert!(matches!(&els[0], WirNode::Push(WirExpr::ConstI64(0))));
    }

    #[test]
    fn range_opt_stats_deterministic_payload_and_diagnostics() {
        use crate::wir_opt::stats::{LoopRejectionReason, RangeOptStats};

        let mut stats = RangeOptStats::new();
        stats.record_fact_created();
        stats.record_fact_created();
        stats.record_fact_joined();
        stats.record_fact_widened();
        stats.record_fact_dropped();
        stats.record_loop_accepted();
        stats.record_loop_rejected(LoopRejectionReason::PotentialEscape);
        stats.record_loop_rejected(LoopRejectionReason::TrapDecline);
        stats.record_bounds_check_removed();
        stats.record_bounds_check_retained();
        stats.record_header_hoisted();
        stats.record_cursorized_access();
        stats.record_arithmetic_rewrite();

        assert_eq!(stats.facts_created, 2);
        assert_eq!(stats.facts_joined, 1);
        assert_eq!(stats.facts_widened, 1);
        assert_eq!(stats.facts_dropped, 1);
        assert_eq!(stats.loops_accepted, 1);
        assert_eq!(stats.bounds_checks_removed, 1);
        assert_eq!(stats.bounds_checks_retained, 1);
        assert_eq!(stats.headers_hoisted, 1);
        assert_eq!(stats.cursorized_accesses, 1);
        assert_eq!(stats.arithmetic_rewrites, 1);
        assert_eq!(stats.escape_declines, 1);
        assert_eq!(stats.trap_declines, 1);

        let summary = stats.format_summary();
        assert!(summary.contains("RangeOptStats:"));
        assert!(summary.contains("facts(created=2, joined=1, widened=1, dropped=1)"));
        assert!(summary.contains("loops(accepted=1"));
        assert!(summary.contains("bounds_checks(removed=1, retained=1)"));
    }

    #[test]
    fn call_site_summary_and_stats_extraction() {
        use crate::wir_opt::call_summary::{
            collect_call_site_summaries, CallOptStats, OwnershipEnvelope,
            RecursionComponent, WirConstant,
        };

        let func = WirFunc {
            name: "compute".into(),
            params: vec![WirLocal { name: "x".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![
                WirNode::Loop {
                    label: "loop_0".into(),
                    body: vec![
                        WirNode::Push(WirExpr::Call {
                            func: "compute".into(),
                            args: vec![WirExpr::ConstI64(42)],
                        }),
                    ],
                },
                WirNode::CallStoreMulti {
                    func: "helper".into(),
                    args: vec![WirExpr::GetLocal("x".into())],
                    dests: vec!["a".into(), "b".into()],
                },
            ],
            raw_body: None,
        };

        let summaries = collect_call_site_summaries(&func);
        assert_eq!(summaries.len(), 2);

        // First call site is self-recursive within loop (loop depth 1 -> frequency hint 11)
        let s1 = &summaries[0];
        assert_eq!(s1.exact_target.as_deref(), Some("compute"));
        assert_eq!(s1.recursion_component, RecursionComponent::SelfRecursive);
        assert_eq!(s1.known_constants, vec![Some(WirConstant::I64(42))]);
        assert_eq!(s1.weighted_frequency_hint, 11);
        assert!(s1.is_exact_and_specializable());

        // Second call site is multi-lane store
        let s2 = &summaries[1];
        assert_eq!(s2.exact_target.as_deref(), Some("helper"));
        assert_eq!(s2.ownership_envelope, OwnershipEnvelope::MultiLane(2));
        assert_eq!(s2.recursion_component, RecursionComponent::None);

        let mut stats = CallOptStats::new();
        stats.record_exact_edge();
        stats.record_devirtualized_edge();
        stats.record_scalarized_capture();
        stats.record_adapter_removed();
        stats.record_clone_created();
        stats.record_clone_shared();
        stats.record_inlined_direct_edge();
        stats.record_expanded_recursive_edge();
        stats.record_reject_budget();
        stats.record_reject_effect();
        stats.record_reject_suspension();
        stats.record_reject_unknown_envelope();

        let formatted = stats.format_summary();
        assert!(formatted.contains("CallOptStats:"));
        assert!(formatted.contains("exact_edges=1"));
        assert!(formatted.contains("devirt=1"));
        assert!(formatted.contains("scalar_caps=1"));
        assert!(formatted.contains("adapters_removed=1"));
        assert!(formatted.contains("clones(created=1, shared=1)"));
        assert!(formatted.contains("inlined=1"));
        assert!(formatted.contains("recursive_expanded=1"));
    }

    #[test]
    fn devirtualizes_and_inlines_closure_call_and_eliminates_allocation() {
        use crate::wir::{ClosureSignature, WirTable};
        use crate::wir_opt::inline_direct_calls;

        // Lambda function: fn(clos, x) -> x % 7
        let lambda = WirFunc {
            name: "lambda_0".into(),
            params: vec![
                WirLocal { name: "clos".into(), ty: WirTy::StructRef },
                WirLocal { name: "x".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Binary {
                op: BinOp::Rem,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::GetLocal("x".into())),
                rhs: Box::new(WirExpr::ConstI64(7)),
            })],
            raw_body: None,
        };

        // Caller function:
        // let f = closure(lambda_0)
        // call_indirect(f, 42)
        let caller = WirFunc {
            name: "caller".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![
                WirLocal { name: "f".into(), ty: WirTy::StructRef },
            ],
            body: vec![
                WirNode::SetLocal {
                    local: "f".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![
                            WirExpr::ConstI32(0), // table index 0
                            WirExpr::ConstI32(0),
                            WirExpr::RefNull(Kind::StructRef),
                        ],
                    },
                },
                WirNode::Push(WirExpr::CallIndirect {
                    signature: ClosureSignature {
                        params: vec![Kind::StructRef, Kind::I64],
                        results: vec![Kind::I64],
                    },
                    args: vec![
                        WirExpr::GetLocal("f".into()),
                        WirExpr::ConstI64(42),
                    ],
                    index: Box::new(WirExpr::StructGet {
                        struct_id: 0,
                        field: CLOSURE_CODE_FIELD,
                        base: Box::new(WirExpr::GetLocal("f".into())),
                    }),
                }),
            ],
            raw_body: None,
        };

        let mut module = WirModule {
            imports: vec![],
            funcs: vec![caller, lambda],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: Some(WirTable {
                funcs: vec!["lambda_0".into()],
            }),
            exports: vec![("main".into(), "caller".into())],
        };

        inline_direct_calls(&mut module);

        // After optimization, caller should have directly inlined `42 % 7`
        let caller_opt = &module.funcs[0];
        assert_eq!(caller_opt.body.len(), 2);
        let [WirNode::SetLocal { local, .. }, WirNode::Push(WirExpr::Binary { op, lhs, rhs, .. })] = caller_opt.body.as_slice() else {
            panic!("Expected inlined binary op in caller, got {:?}", caller_opt.body);
        };
        assert_eq!(local, "f");
        assert_eq!(*op, BinOp::Rem);
        assert!(matches!(lhs.as_ref(), WirExpr::ConstI64(42)));
        assert!(matches!(rhs.as_ref(), WirExpr::ConstI64(7)));
    }

    #[test]
    fn specializes_function_by_constant_arguments_and_shares_clones() {
        use crate::wir::{BinOp, Kind};

        let helper = WirFunc {
            name: "multiply_add".into(),
            params: vec![
                WirLocal { name: "factor".into(), ty: WirTy::Int },
                WirLocal { name: "x".into(), ty: WirTy::Int },
            ],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Binary {
                op: BinOp::Mul,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::GetLocal("factor".into())),
                rhs: Box::new(WirExpr::GetLocal("x".into())),
            })],
            raw_body: None,
        };

        let caller = WirFunc {
            name: "caller".into(),
            params: vec![WirLocal { name: "a".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![
                // First call with constant factor 7
                WirNode::Push(WirExpr::Call {
                    func: "multiply_add".into(),
                    args: vec![WirExpr::ConstI64(7), WirExpr::GetLocal("a".into())],
                }),
                // Second call also with constant factor 7 (should share clone)
                WirNode::Push(WirExpr::Call {
                    func: "multiply_add".into(),
                    args: vec![WirExpr::ConstI64(7), WirExpr::ConstI64(10)],
                }),
                // Third call with constant factor 7 and variable a (should share same clone)
                WirNode::Push(WirExpr::Call {
                    func: "multiply_add".into(),
                    args: vec![WirExpr::ConstI64(7), WirExpr::GetLocal("a".into())],
                }),
            ],
            raw_body: None,
        };

        let mut module = module_with(helper);
        module.funcs.push(caller);

        crate::wir_opt::specialize_by_call_site_facts(&mut module);

        // Exactly one specialized clone was created (multiply_add_spec_1)
        assert_eq!(module.funcs.len(), 3);
        let spec_func = &module.funcs[2];
        assert_eq!(spec_func.name, "multiply_add_spec_1");

        // The caller function's calls to multiply_add with const 7 were redirected to multiply_add_spec_1
        let caller_func = &module.funcs[1];
        match &caller_func.body[0] {
            WirNode::Push(WirExpr::Call { func, .. }) => {
                assert_eq!(func, "multiply_add_spec_1");
            }
            _ => panic!("expected Call in caller node 0"),
        }
        match &caller_func.body[2] {
            WirNode::Push(WirExpr::Call { func, .. }) => {
                assert_eq!(func, "multiply_add_spec_1");
            }
            _ => panic!("expected Call in caller node 2"),
        }
    }
