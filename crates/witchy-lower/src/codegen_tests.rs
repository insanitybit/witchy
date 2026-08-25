    use super::*;
    use witchy_syntax::parser::parse_module;

    #[test]
    fn projected_place_from_a_direct_dict_root_keeps_the_dict_layout_bias() {
        use witchy_types::loans::{
            LoanEvent, LoanOwnerRoot, LoanPlace, LoanProjection, LoanProjectionStep,
        };
        use witchy_wir::wir::{
            WirExpr as W, WirFunc, WirLocal, WirModule, WirNode as N, WirTy,
        };

        let int = Type::Named("Int".into(), vec![]);
        let event = LoanEvent::from_checked_place(
            "v".into(),
            LoanPlace {
                root: LoanOwnerRoot {
                    local: "values".into(),
                    direct_storage_type: Some(Type::Named(
                        "Dict".into(),
                        vec![int.clone(), int.clone()],
                    )),
                },
                projection: LoanProjection {
                    steps: vec![LoanProjectionStep::Index(0)],
                },
                storage_type: int,
            },
            LoanProjection::default(),
            "view".into(),
        );
        assert!(!event.projection.steps.is_empty(), "the checked place is projected");
        let root = Codegen::loan_root(&event)
            .expect("checked root classification")
            .expect("a direct Dict root is retainable");
        let root_local = root.local.clone();
        let module = WirModule {
            imports: vec![],
            funcs: vec![WirFunc {
                name: "projected_dict_root".into(),
                params: vec![WirLocal { name: "values".into(), ty: WirTy::Bool }],
                ret: vec![WirTy::Int],
                locals: vec![WirLocal { name: root_local.clone(), ty: WirTy::Bool }],
                body: vec![
                    N::SetLocal { local: root_local, value: Codegen::loan_region(&root) },
                    N::Push(W::ConstI64(0)),
                ],
                raw_body: None,
            }],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![],
        };
        let wat = witchy_wir::wir::to_wat(&module);
        assert!(
            wat.contains("local.get $values\n    i32.const 4\n    i32.sub"),
            "a projection must not erase the direct Dict root's -4 base bias: {wat}",
        );
    }

    #[test]
    fn missing_checked_root_type_is_a_hard_codegen_error_not_an_omitted_root() {
        use witchy_types::loans::{LoanEvent, LoanOwnerRoot, LoanPlace, LoanProjection};

        let event = LoanEvent::from_checked_place(
            "view".into(),
            LoanPlace {
                root: LoanOwnerRoot {
                    local: "owner".into(),
                    direct_storage_type: None,
                },
                projection: LoanProjection::default(),
                storage_type: Type::Named("Int".into(), vec![]),
            },
            LoanProjection::default(),
            "view".into(),
        );
        let error = match Codegen::loan_root(&event) {
            Err(error) => error,
            Ok(_) => panic!("a missing checked root type must not silently omit retain/release"),
        };
        assert!(
            error.message.contains("has no exact checked root-local type"),
            "missing root-layout evidence must be a hard codegen error: {error}",
        );

        let scalar_event = LoanEvent::from_checked_place(
            "scalar_view".into(),
            LoanPlace {
                root: LoanOwnerRoot {
                    local: "scalar".into(),
                    direct_storage_type: Some(Type::Named("Int".into(), vec![])),
                },
                projection: LoanProjection::default(),
                storage_type: Type::Named("Int".into(), vec![]),
            },
            LoanProjection::default(),
            "scalar_view".into(),
        );
        assert!(
            matches!(Codegen::loan_root(&scalar_event), Ok(None)),
            "a checked scalar root is intentionally non-retainable, unlike missing evidence",
        );
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
    fn intrinsic_contiguous_op_classification_matches_rfc0151() {
        use witchy_syntax::intrinsics::{ContiguousOpClass, IntrinsicId, ScalarValidation};

        assert_eq!(
            IntrinsicId::BytesAt.contiguous_op(),
            Some(ContiguousOpClass::ContiguousRead { width: 1 })
        );
        assert_eq!(
            IntrinsicId::BytesSetAt.contiguous_op(),
            Some(ContiguousOpClass::ContiguousWrite { width: 1 })
        );
        assert_eq!(
            IntrinsicId::BytesPush.contiguous_op(),
            Some(ContiguousOpClass::AppendScalar {
                width: 1,
                validation: ScalarValidation::ByteRange,
            })
        );
        assert_eq!(
            IntrinsicId::BytesAppend.contiguous_op(),
            Some(ContiguousOpClass::AppendSlice { width: 1 })
        );
        assert_eq!(
            IntrinsicId::BytesReserve.contiguous_op(),
            Some(ContiguousOpClass::Reserve)
        );
        assert_eq!(
            IntrinsicId::BytesToString.contiguous_op(),
            Some(ContiguousOpClass::Finish)
        );
        assert_eq!(IntrinsicId::ChannelOpen.contiguous_op(), None);
        assert_eq!(IntrinsicId::GeneratedListPush.contiguous_op(), None);
    }

    #[test]
    fn linear_build_plan_lifecycle_and_invalidation() {
        use crate::codegen::linear_builder::{LinearBuildPlan, LinearValueKind};
        use witchy_syntax::intrinsics::{ContiguousOpClass, ScalarValidation};

        let mut plan = LinearBuildPlan::new(
            "buf".into(),
            LinearValueKind::Bytes,
            1,
            0,
        )
        .with_locals(
            Some("buf_len".into()),
            Some("buf_cap".into()),
            Some("buf_cur".into()),
            Some("__cap_buf".into()),
        );

        assert!(plan.is_valid_at_epoch(0));
        assert!(!plan.is_valid_at_epoch(1));
        assert!(plan.is_direct_byte_eligible());
        assert!(!plan.is_string_builder_eligible());

        assert!(plan.contiguous_op_applicable(ContiguousOpClass::ContiguousRead { width: 1 }));
        assert!(plan.contiguous_op_applicable(ContiguousOpClass::ContiguousWrite { width: 1 }));
        assert!(plan.contiguous_op_applicable(ContiguousOpClass::AppendScalar {
            width: 1,
            validation: ScalarValidation::ByteRange,
        }));
        assert!(!plan.contiguous_op_applicable(ContiguousOpClass::AppendScalar {
            width: 1,
            validation: ScalarValidation::Utf8Scalar,
        }));
        assert!(!plan.contiguous_op_applicable(ContiguousOpClass::ContiguousRead { width: 2 }));

        assert!(!plan.range_is_byte_proven("ch"));
        plan.record_range_fact("ch".into(), 0, 255);
        assert!(plan.range_is_byte_proven("ch"));

        plan.record_range_fact("wide".into(), -10, 300);
        assert!(!plan.range_is_byte_proven("wide"));
    }

    #[test]
    fn bytes_direct_set_at_range_check_elimination() {
        use crate::codegen::linear_builder::{LinearBuildPlan, LinearValueKind};
        use witchy_syntax::ast::Expr;
        use witchy_wir::wir::{WirExpr, WirNode};

        let src = "fn main(console: Console):\n    let buf = 0\n";
        let module = parse_module(src).expect("parse");
        let typed = witchy_types::typeck::annotate_checked(module).expect("typeck");
        let (loans, access) = (
            witchy_types::loans::LoanFacts::default(),
            witchy_types::access::checked_facts(typed.module(), typed.table()).expect("access facts"),
        );
        let mut codegen = Codegen::new(typed.module(), typed.table(), loans, access);
        codegen.locals.insert("buf".into(), Kind::I32);
        codegen.locals.insert("val".into(), Kind::I64);

        let b_expr = Expr::Var("buf".into());
        let idx_expr = Expr::Int(0);
        let const_val = Expr::Int(65);
        let dyn_val = Expr::Var("val".into());

        // 1. Constant value (65) is proven in 0..=255 -> value range check is elided (only 1 If for index bounds)
        let wir_const = codegen.lower_bytes_set_at(&b_expr, &idx_expr, &const_val).expect("lower const");
        if let WirExpr::Seq(seq) = wir_const {
            let if_count = seq.iter().filter(|node| matches!(node, WirNode::If { .. })).count();
            assert_eq!(if_count, 1, "constant in 0..=255 must elide value range check (only 1 index bounds check)");
        } else {
            panic!("expected WirExpr::Seq");
        }

        // 2. Unproven dynamic variable -> has 2 If nodes (value range check + index bounds check)
        let wir_dyn = codegen.lower_bytes_set_at(&b_expr, &idx_expr, &dyn_val).expect("lower dyn");
        if let WirExpr::Seq(seq) = wir_dyn {
            let if_count = seq.iter().filter(|node| matches!(node, WirNode::If { .. })).count();
            assert_eq!(if_count, 2, "unproven dynamic variable must retain value range check (2 If nodes)");
        } else {
            panic!("expected WirExpr::Seq");
        }

        // 3. Proven range fact on dynamic variable in active plan -> elides value range check (1 If node)
        let mut plan = LinearBuildPlan::new("buf".into(), LinearValueKind::Bytes, 1, 0);
        plan.record_range_fact("val".into(), 0, 255);
        codegen.note_linear_builder(plan);

        let wir_proven = codegen.lower_bytes_set_at(&b_expr, &idx_expr, &dyn_val).expect("lower proven");
        if let WirExpr::Seq(seq) = wir_proven {
            let if_count = seq.iter().filter(|node| matches!(node, WirNode::If { .. })).count();
            assert_eq!(if_count, 1, "variable with proven range fact 0..=255 must elide value range check (1 If node)");
        } else {
            panic!("expected WirExpr::Seq");
        }
    }

    #[test]
    fn scheduler_frame_allocation_whitelist_requires_the_sealed_step_layout() {
        assert_eq!(
            task_compiler_owned_frame_entry_ctor("PullReadyFrameLoop", "N9:task.Step[N3:Nil[]]"),
            Some(true)
        );
        assert_eq!(
            task_compiler_owned_frame_entry_ctor("Pull2FrameLoop", "N9:task.Step[N3:Nil[]]"),
            Some(false)
        );
        assert_eq!(
            task_compiler_owned_frame_entry_ctor("PullReadyFrameLoop", "N5:Other[]"),
            None,
            "a source spelling alone must never enter scheduler allocation attribution"
        );
        assert_eq!(
            task_compiler_owned_frame_entry_ctor("WaitFrameLoop", "N9:task.Step[N3:Nil[]]"),
            None,
            "in-place parked-frame retags are not allocations"
        );
    }
