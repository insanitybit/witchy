    use super::*;
    use witchy_syntax::parser::parse_module;
    use std::sync::{Arc, Mutex};
    use wasmtime::{Caller, Engine, Linker, Module as WtModule, Store};

    fn gc_wasm_features() -> wasmparser::WasmFeatures {
        wasmparser::WasmFeatures::default()
            | wasmparser::WasmFeatures::GC
            | wasmparser::WasmFeatures::REFERENCE_TYPES
            | wasmparser::WasmFeatures::FUNCTION_REFERENCES
    }

    fn gc_wasm_payloads(
        wasm: &[u8],
    ) -> impl Iterator<Item = wasmparser::Result<wasmparser::Payload<'_>>> {
        wasmparser::Validator::new_with_features(gc_wasm_features())
            .validate_all(wasm)
            .expect("valid Wasm GC module");
        wasmparser::Parser::new(0).parse_all(wasm)
    }

    fn gc_wasmtime_engine() -> Engine {
        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        Engine::new(&config).expect("Wasm GC engine")
    }

    fn assert_catalog_names_live_wir_helpers(names: &[&str]) {
        for &name in names {
            let spec = witchy_syntax::intrinsics::lookup(name).expect("cataloged operation");
            assert!(
                witchy_types::typeck::intrinsic(name),
                "{} must not compile its self-recursive std placeholder",
                spec.name
            );
            for helper in spec.wir_helpers {
                assert!(
                    witchy_wir::wir_helpers::wir_helper(helper).is_some(),
                    "{} names missing WIR helper {}",
                    spec.name,
                    helper
                );
            }
        }
    }

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

    fn assert_partial_capture_reaches_wir_fallback(source: &str) {
        let module = parse_module(source).expect("parse partial-capture fallback fixture");
        let unsupported = compile_module_binary(&module)
            .expect_unsupported("partial WIR capture must remain a fallback, not an identity error");
        assert!(
            unsupported.message.contains("reachable functions do not fully lower to WIR"),
            "the whole-unit fallback must survive partial loan-fact consumption: {unsupported}",
        );
    }

    fn beyond_wir_assignment_scratch_pool() -> String {
        (0..=SCRUT_POOL).fold("values".to_string(), |value, _| {
            format!("list.__set_at({value}, 0, 0)")
        })
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

