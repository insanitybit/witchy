    // --- M3-obstacle additions: globals, table+call_indirect, multi-value,
    //     raw-body splice ---------------------------------------------------

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