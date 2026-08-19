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

