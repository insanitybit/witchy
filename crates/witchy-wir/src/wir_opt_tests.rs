    use super::*;
    use crate::wir::{
        BinOp, DataSegment, Kind, WirExpr, WirFunc, WirImport, WirLocal, WirModule, WirNode, WirTy,
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

        assert_eq!(lower_self_tail_calls(&mut module), 1);
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
        assert!(matches!(escape_body.get(4), Some(WirNode::Br { target, cond: None }) if target == label));
        assert!(matches!(escape_body.first(), Some(WirNode::SetLocal { local, .. }) if local == &func.locals[0].name));
        assert!(matches!(escape_body.get(1), Some(WirNode::SetLocal { local, .. }) if local == &func.locals[1].name));
        assert!(matches!(escape_body.get(2), Some(WirNode::SetLocal { local, value: WirExpr::GetLocal(_) }) if local == "n"));
        assert!(matches!(escape_body.get(3), Some(WirNode::SetLocal { local, value: WirExpr::GetLocal(_) }) if local == "acc"));
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

        assert_eq!(lower_self_tail_calls(&mut module), 0);
        assert!(matches!(module.funcs[0].body.as_slice(), [WirNode::Push(WirExpr::Call { .. })]));
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

        assert_eq!(lower_self_tail_calls(&mut module), 1);
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
