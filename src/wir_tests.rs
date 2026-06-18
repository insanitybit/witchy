    use super::*;

    fn local(name: &str, ty: WirTy) -> WirLocal {
        WirLocal { name: name.into(), ty }
    }

    /// Encode a WIR module to a wasm binary and run its `run` export, capturing
    /// `print_int` and `print` output as ordered lines. (Runs the actual binary
    /// the codegen path emits — `wir_encode::encode` — not the `to_wat` display.)
    fn run_capture(module: &WirModule) -> Vec<String> {
        use std::sync::{Arc, Mutex};
        let binary = crate::wir_encode::encode(module);
        let engine = wasmtime::Engine::default();
        let m = wasmtime::Module::new(&engine, &binary)
            .unwrap_or_else(|e| panic!("encoded module invalid: {e}"));
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
                    let s =
                        String::from_utf8_lossy(&data[ptr as usize..(ptr + len) as usize]).into_owned();
                    o.lock().unwrap().push(s);
                },
            )
            .unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let inst = linker.instantiate(&mut store, &m).expect("instantiate");
        let run = inst.get_typed_func::<(), ()>(&mut store, "run").expect("run export");
        run.call(&mut store, ()).expect("run");
        out.lock().unwrap().clone()
    }

    /// Module with one Int-returning func + a `run` that prints its result.
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

    #[test]
    fn arithmetic_roundtrips() {
        // fn add() -> Int: (2 + 3) * 4   == 20
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
        assert_eq!(run_capture(&m), vec!["20"]);
    }

    #[test]
    fn if_with_result_roundtrips() {
        // fn pick(b: Bool) -> Int: if b: 10 else: 20  (each arm returns)
        let pick = WirFunc {
            name: "pick".into(),
            params: vec![local("b", WirTy::Bool)],
            ret: vec![WirTy::Int],
            locals: vec![],
            // value-`if`: each branch leaves an i64; the if's value is the result.
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
        assert_eq!(run_capture(&m_true), vec!["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_eq!(run_capture(&m_false), vec!["20"]);
    }

    #[test]
    fn loop_spine_roundtrips() {
        // fn sum_to(n: Int) -> Int:   (sum of 0..n)
        //   var total = 0; var i = 0
        //   block $exit: loop $head:
        //     br $exit if !(i < n); total += i; i += 1; br $head
        //   total
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
        // sum 0..5 = 0+1+2+3+4 = 10
        let m = int_demo(
            sum_to,
            WirExpr::Call { func: "sum_to".into(), args: vec![WirExpr::ConstI64(5)] },
        );
        assert_eq!(run_capture(&m), vec!["10"]);
    }

    // A `(kind, value, expect)` table that round-trips each Kind; one case is
    // active today (the F64 case is noted inline), so the loop is currently
    // single-element by design.
    #[test]
    #[allow(clippy::single_element_loop)]
    fn slot_conversions_roundtrip() {
        // FromSlot(ToSlot(x, k), k) == x for each Kind — the conversion nodes the
        // headline optimization (§3.2) will cancel.
        for (kind, value, expect) in [
            (Kind::I64, WirExpr::ConstI64(42), "42"),
            // F64 5.0 reinterpreted both ways, then truncated to i64 for print.
        ] {
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
            assert_eq!(run_capture(&m), vec![expect.to_string()]);
        }
    }

    #[test]
    fn unary_ops_roundtrip() {
        // fn neg() -> Int: -(5) == -5  (exercises the `0 - x` operand ordering)
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
        assert_eq!(run_capture(&m), vec!["-5"]);

        // fn bnot() -> Int: ~0 == -1  (x ^ -1)
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
        assert_eq!(run_capture(&m), vec!["-1"]);
    }

    #[test]
    fn control_value_if_roundtrips() {
        // A value-`if` in *expression* position (the `&&`/`||` and if-expr shape):
        // fn pick(b) -> Int: return (if b { 10 } else { 20 })
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
        assert_eq!(run_capture(&m_true), vec!["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_eq!(run_capture(&m_false), vec!["20"]);
    }

    #[test]
    fn string_print_roundtrips() {
        // A self-contained `print(console, "hi")`: data `[2,0,0,0,'h','i']` at
        // offset 8; print is called with (ptr+4, load_len). Exercises StrPtr,
        // Load, and a void CallHost.
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
        assert_eq!(run_capture(&m), vec!["hi"]);
    }
