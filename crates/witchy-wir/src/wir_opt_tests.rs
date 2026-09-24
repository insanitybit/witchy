
    use super::*;
    use crate::wir::{
        BinOp, Kind, WirExpr, WirFunc, WirImport,
        WirModule, WirNode, WirTy,
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
    fn strength_reduction_retains_effectful_zero_operands() {
        // RFC-0153 may fold identities only when the discarded operand is
        // total. Each expression below would have the same numeric result as
        // zero, but removing it would respectively lose a host effect, an
        // allocation, a trapping read, or a memory write.
        let cases = [
            (
                "host effect",
                BinOp::And,
                WirExpr::Seq(vec![
                    WirNode::Do(WirExpr::CallHost {
                        import: "print_int".into(),
                        args: vec![WirExpr::ConstI64(1)],
                    }),
                    WirNode::Push(WirExpr::ConstI64(7)),
                ]),
            ),
            (
                "allocation",
                BinOp::Mul,
                WirExpr::Seq(vec![
                    WirNode::Drop(WirExpr::MemoryGrow(Box::new(WirExpr::ConstI32(1)))),
                    WirNode::Push(WirExpr::ConstI64(7)),
                ]),
            ),
            (
                "trapping read",
                BinOp::And,
                WirExpr::Load {
                    ptr: Box::new(WirExpr::ConstI32(0)),
                    kind: Kind::I64,
                    offset: 0,
                },
            ),
            (
                "memory write",
                BinOp::Mul,
                WirExpr::Seq(vec![
                    WirNode::Store {
                        ptr: WirExpr::ConstI32(0),
                        value: WirExpr::ConstI64(1),
                        kind: Kind::I64,
                        offset: 0,
                    },
                    WirNode::Push(WirExpr::ConstI64(7)),
                ]),
            ),
        ];

        for (label, op, lhs) in cases {
            let mut module = module_with(func_returning(WirExpr::Binary {
                op,
                kind: Kind::I64,
                lhs: Box::new(lhs),
                rhs: Box::new(WirExpr::ConstI64(0)),
            }));
            crate::wir_opt::strength_reduce::simplify_integer_ranges(&mut module);
            assert!(
                matches!(
                    module.funcs[0].body.as_slice(),
                    [WirNode::Return(Some(WirExpr::Binary { op: actual_op, .. }))] if *actual_op == op
                ),
                "{label} must be evaluated instead of being folded to zero: {:?}",
                module.funcs[0].body,
            );
        }
    }
