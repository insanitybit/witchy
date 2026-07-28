    #[test]
    fn if_with_result_roundtrips() {
        // fn pick(b: Bool) -> Int: if b: 10 else: 20
        let pick = WirFunc {
            name: "pick".into(),
            params: vec![local("b", WirTy::Bool)],
            ret: vec![WirTy::Int],
            locals: vec![],
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
        assert_agrees(&m_true, &["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_agrees(&m_false, &["20"]);
    }

    #[test]
    fn loop_spine_roundtrips() {
        // fn sum_to(n) -> Int: sum of 0..n (Block/Loop/Br spine).
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
        // sum 0..5 = 10
        let m = int_demo(
            sum_to,
            WirExpr::Call { func: "sum_to".into(), args: vec![WirExpr::ConstI64(5)] },
        );
        assert_agrees(&m, &["10"]);
    }

    // A `(kind, value, expect)` round-trip table; single-element by design today.
    #[test]
    #[allow(clippy::single_element_loop)]
    fn slot_conversions_roundtrip() {
        // FromSlot(ToSlot(x, k), k) == x.
        for (kind, value, expect) in [(Kind::I64, WirExpr::ConstI64(42), "42")] {
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
            assert_agrees(&m, &[expect]);
        }
    }

    #[test]
    fn unary_ops_roundtrip() {
        // fn neg() -> Int: -(5) == -5
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
        assert_agrees(&m, &["-5"]);

        // fn bnot() -> Int: ~0 == -1
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
        assert_agrees(&m, &["-1"]);
    }

    #[test]
    fn control_value_if_roundtrips() {
        // value-`if` in expression position (the `&&`/`||`/if-expr shape).
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
        assert_agrees(&m_true, &["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_agrees(&m_false, &["20"]);
    }

    #[test]
    fn string_print_roundtrips() {
        // console.print("hi"): data [2,0,0,0,'h','i'] at offset 8.
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
        assert_agrees(&m, &["hi"]);
    }

    // --- M3-obstacle additions: globals, table+call_indirect, multi-value,
    //     raw-body splice ---------------------------------------------------

    #[test]
    fn mutable_global_set_get_roundtrips() {
        // A mutable i64 global `$counter` initialized to 5. `run`:
        //   counter = counter + 37   (global.get; +; global.set)
        //   print_int(counter)       (global.get)  -> 42
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::SetGlobal {
                    global: "counter".into(),
                    value: WirExpr::Binary {
                        op: BinOp::Add,
                        kind: Kind::I64,
                        lhs: Box::new(WirExpr::GetGlobal("counter".into())),
                        rhs: Box::new(WirExpr::ConstI64(37)),
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::GetGlobal("counter".into())],
                }),
            ],
            raw_body: None,
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal {
                name: "counter".into(),
                kind: Kind::I64,
                mutable: true,
                init: GlobalInit::I64(5),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["42"]);
    }

    #[test]
    fn table_call_indirect_roundtrips() {
        // A lifted lambda `$__lam0` with the `$clos1` signature
        // `(param i32 env) (param i64 arg) (result i64)`: returns arg + 100.
        // A closure record `[code_index=0]` lives at offset 0 in memory (data
        // segment of 4 zero bytes). `run` reads its code index and `call_indirect
        // (type $clos1)` with the record as env and a literal slot arg of 5 -> 105.
        let lam0 = WirFunc {
            name: "__lam0".into(),
            params: vec![local("env", WirTy::Capability), local("arg", WirTy::Slot)],
            ret: vec![WirTy::Slot],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::GetLocal("arg".into())),
                rhs: Box::new(WirExpr::ConstI64(100)),
            }))],
            raw_body: None,
        };
        // The closure record at offset 0 holds the code index (0) as its i32
        // header. `call_indirect` args = [env ptr, slot arg]; index = i32.load env.
        let call = WirExpr::CallIndirect {
            signature: slot_closure_signature(1, 1),
            args: vec![WirExpr::ConstI32(0), WirExpr::ToSlot(Box::new(WirExpr::ConstI64(5)), Kind::I64)],
            index: Box::new(WirExpr::Load {
                ptr: Box::new(WirExpr::ConstI32(0)),
                kind: Kind::I32,
                offset: 0,
            }),
        };
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
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![lam0, run],
            memory_pages: 1,
            // 4 zero bytes at offset 0: the closure record's code index (slot 0).
            data: vec![DataSegment { offset: 0, bytes: vec![0, 0, 0, 0] }],
            globals: vec![],
            table: Some(WirTable { funcs: vec!["__lam0".into()] }),
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["105"]);
    }

    #[test]
    fn table_call_indirect_multi_result_roundtrips() {
        let lam0 = WirFunc {
            name: "__lam0".into(),
            params: vec![local("env", WirTy::Capability), local("arg", WirTy::Slot)],
            ret: vec![WirTy::Slot, WirTy::Slot],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::GetLocal("arg".into())),
                    rhs: Box::new(WirExpr::ConstI64(1)),
                }),
                WirNode::Push(WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::GetLocal("arg".into())),
                    rhs: Box::new(WirExpr::ConstI64(100)),
                }),
                WirNode::Return(None),
            ],
            raw_body: None,
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("result", WirTy::Slot), local("writeback", WirTy::Slot)],
            body: vec![
                WirNode::CallIndirectStoreMulti {
                    signature: slot_closure_signature(1, 2),
                    args: vec![WirExpr::ConstI32(0), WirExpr::ConstI64(5)],
                    index: WirExpr::Load {
                        ptr: Box::new(WirExpr::ConstI32(0)),
                        kind: Kind::I32,
                        offset: 0,
                    },
                    dests: vec!["result".into(), "writeback".into()],
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::GetLocal("result".into())],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::GetLocal("writeback".into())],
                }),
            ],
            raw_body: None,
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![lam0, run],
            memory_pages: 1,
            data: vec![DataSegment { offset: 0, bytes: vec![0, 0, 0, 0] }],
            globals: vec![],
            table: Some(WirTable { funcs: vec!["__lam0".into()] }),
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["6", "105"]);
    }

    #[test]
    fn exact_indirect_signatures_distinguish_gc_and_linear_environments() {
        let typed = ClosureSignature {
            params: vec![Kind::GcRef(0), Kind::I64],
            results: vec![Kind::I64],
        };
        let gc_lambda = WirFunc {
            name: "gc_lambda".into(),
            params: vec![local("env", WirTy::GcRef(0)), local("arg", WirTy::Int)],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::GetLocal("arg".into())),
                rhs: Box::new(WirExpr::ConstI64(1)),
            })],
            raw_body: None,
        };
        let linear_lambda = WirFunc {
            name: "linear_lambda".into(),
            params: vec![local("env", WirTy::Bool), local("arg", WirTy::Int)],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Push(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::GetLocal("arg".into())),
                rhs: Box::new(WirExpr::ConstI64(100)),
            })],
            raw_body: None,
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("closure", WirTy::GcRef(0))],
            body: vec![
                WirNode::SetLocal {
                    local: "closure".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![
                            WirExpr::ConstI32(0),
                            WirExpr::ConstI32(0),
                            WirExpr::RefNull(Kind::StructRef),
                        ],
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::CallIndirect {
                        signature: typed,
                        args: vec![
                            WirExpr::GetLocal("closure".into()),
                            WirExpr::ConstI64(5),
                        ],
                        index: Box::new(WirExpr::ConstI32(0)),
                    }],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::CallIndirect {
                        signature: slot_closure_signature(1, 1),
                        args: vec![WirExpr::ConstI32(0), WirExpr::ConstI64(5)],
                        index: Box::new(WirExpr::ConstI32(1)),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![gc_lambda, linear_lambda, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: Some(WirTable { funcs: vec!["gc_lambda".into(), "linear_lambda".into()] }),
            exports: vec![("run".into(), "run".into())],
        };

        assert_eq!(run_binary(&encode(&module, &[closure_wrapper_struct()])), vec!["6", "105"]);
    }

    #[test]
    fn typed_indirect_multi_result_preserves_reference_kinds() {
        let lambda = WirFunc {
            name: "lambda".into(),
            params: vec![local("env", WirTy::GcRef(0))],
            ret: vec![WirTy::Extern, WirTy::GcRef(0)],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::RefNull(Kind::ExternRef)),
                WirNode::Push(WirExpr::GetLocal("env".into())),
                WirNode::Return(None),
            ],
            raw_body: None,
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![
                local("closure", WirTy::GcRef(0)),
                local("cap", WirTy::Extern),
                local("returned", WirTy::GcRef(0)),
            ],
            body: vec![
                WirNode::SetLocal {
                    local: "closure".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![
                            WirExpr::ConstI32(0),
                            WirExpr::ConstI32(0),
                            WirExpr::RefNull(Kind::StructRef),
                        ],
                    },
                },
                WirNode::CallIndirectStoreMulti {
                    signature: ClosureSignature {
                        params: vec![Kind::GcRef(0)],
                        results: vec![Kind::ExternRef, Kind::GcRef(0)],
                    },
                    args: vec![WirExpr::GetLocal("closure".into())],
                    index: WirExpr::ConstI32(0),
                    dests: vec!["cap".into(), "returned".into()],
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::RefIsNull(Box::new(WirExpr::GetLocal(
                            "cap".into(),
                        )))),
                    }],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::RefIsNull(Box::new(WirExpr::GetLocal(
                            "returned".into(),
                        )))),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![lambda, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: Some(WirTable { funcs: vec!["lambda".into()] }),
            exports: vec![("run".into(), "run".into())],
        };

        assert_eq!(run_binary(&encode(&module, &[closure_wrapper_struct()])), vec!["1", "0"]);
    }

    #[test]
    fn multi_value_result_roundtrips() {
        // fn pair() -> (Int, Int): leaves 30 then 12 on the stack — a 2-result
        // func exercising a `(result i64 i64)` signature in the type section.
        // `run` calls $pair (two i64s on the stack), `i64.add`s them, and prints
        // the sum -> 42. The node-tree `WirExpr` model can't reference a call's
        // multiple stack results directly, so `run` is spliced as a raw body
        // (which also covers the splice + multi-value paths together).
        let pair = WirFunc {
            name: "pair".into(),
            params: vec![],
            ret: vec![WirTy::Int, WirTy::Int],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::ConstI64(30)),
                WirNode::Push(WirExpr::ConstI64(12)),
            ],
            raw_body: None,
        };
        // Index space: import print_int = 0; defined funcs pair = 1, run = 2.
        let mut run_fn = Function::new_with_locals_types(Vec::<ValType>::new());
        run_fn.instruction(&Instruction::Call(1)); // call $pair -> [30, 12]
        run_fn.instruction(&Instruction::I64Add); // -> 42
        run_fn.instruction(&Instruction::Call(0)); // call $print_int
        run_fn.instruction(&Instruction::End);
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![],
            raw_body: Some(run_fn.into_raw_body()),
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![pair, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_eq!(run_encoded(&m), vec!["42".to_string()]);
    }

    #[test]
    fn raw_body_splice_roundtrips() {
        // A func `magic` whose body is supplied as pre-encoded wasm bytes (the
        // splice path): `i64.const 99; end`. `run` calls it and prints -> 99.
        // The raw body is `Function::into_raw_body()` output (locals + instrs +
        // End, NO length prefix) — exactly what `CodeSection::raw` consumes.
        let mut magic_fn = Function::new_with_locals_types(Vec::<ValType>::new());
        magic_fn.instruction(&Instruction::I64Const(99));
        magic_fn.instruction(&Instruction::End);
        let magic = WirFunc {
            name: "magic".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![], // ignored: raw_body wins
            raw_body: Some(magic_fn.into_raw_body()),
        };
        let m = int_demo(magic, WirExpr::Call { func: "magic".into(), args: vec![] });
        // No WAT path for a raw-body func, so assert on the encoder output only.
        assert_eq!(run_encoded(&m), vec!["99".to_string()]);
    }

    /// (RFC-0005 Stage 1) The encoder round-trips a hand-built module that uses the
    /// new GC-struct + externref infrastructure end to end: a struct type
    /// `{externref, i64}`, a function whose signature carries an `externref` param
    /// AND a `(ref null $0)` param, and the `StructNew`/`StructGet`/`StructSet`/
    /// `RefNull` opcodes. Instantiating it in wasmtime with GC + function-references
    /// enabled (as the real runtime keeps reference types / GC on) proves the emitted
    /// bytes VALIDATE; running `run` proves the struct field write-then-read executes.
    /// This is the mechanism the cap-carrying-aggregate lowering (Stage 4) will use;
    /// nothing in the production path emits these yet.
    #[test]
    fn gc_struct_and_externref_round_trip() {
        use crate::wir::WirStructDef;

        // struct $0 { field0: externref (a capability), field1: i64 (a scalar) }.
        let structs = vec![WirStructDef {
            fields: vec![Kind::ExternRef, Kind::I64],
            mutable: true,
        }];

        // fn takes_cap(cap: externref, agg: (ref null $0)) -> ()  — present but
        // uncalled, so its reference-typed SIGNATURE must still validate.
        let takes_cap = WirFunc {
            name: "takes_cap".into(),
            params: vec![local("cap", WirTy::Extern), local("agg", WirTy::GcRef(0))],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Drop(WirExpr::StructGet {
                struct_id: 0,
                field: 1,
                base: Box::new(WirExpr::GetLocal("agg".into())),
            })],
            raw_body: None,
        };

        // fn run() -> ():
        //   s = struct.new $0 (ref.null extern, i64.const 42)   // a null cap + 42
        //   struct.set $0 1 s 99                                 // overwrite the scalar
        //   print_int (struct.get $0 1 s)                        // -> 99
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("s", WirTy::GcRef(0))],
            body: vec![
                WirNode::SetLocal {
                    local: "s".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![WirExpr::RefNull(Kind::ExternRef), WirExpr::ConstI64(42)],
                    },
                },
                WirNode::StructSet {
                    struct_id: 0,
                    field: 1,
                    base: WirExpr::GetLocal("s".into()),
                    value: WirExpr::ConstI64(99),
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::StructGet {
                        struct_id: 0,
                        field: 1,
                        base: Box::new(WirExpr::GetLocal("s".into())),
                    }],
                }),
            ],
            raw_body: None,
        };

        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![takes_cap, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };

        let binary = encode(&module, &structs);
        assert_eq!(binary, encode_with_gc(&module, &structs, &[]));

        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config).expect("engine");
        let m = wasmtime::Module::new(&engine, &binary)
            .unwrap_or_else(|e| panic!("GC-struct module invalid: {e:#}"));
        let out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = wasmtime::Linker::new(&engine);
        let o = out.clone();
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
                o.lock().unwrap().push(n.to_string());
            })
            .unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let inst = linker.instantiate(&mut store, &m).expect("instantiate");
        let run = inst.get_typed_func::<(), ()>(&mut store, "run").expect("run export");
        run.call(&mut store, ()).expect("run");
        assert_eq!(*out.lock().unwrap(), vec!["99".to_string()]);
    }

    /// RFC-0005 reference-storage substrate: GC arrays can hold concrete GC
    /// references without crossing the linear-memory slot ABI. Exercise both
    /// allocation forms plus indexed read/write and length after the optimizer
    /// has traversed the complete array expression tree.
    #[test]
    fn gc_reference_array_round_trip() {
        use crate::wir::{WirArrayDef, WirStructDef};

        // GC type 0 is the payload struct, type 1 is a holder that points
        // forward to array type 2, and type 2 is array definition 0. The
        // forward edge requires the encoder's explicit recursion group.
        let structs = vec![
            WirStructDef {
                fields: vec![Kind::ExternRef, Kind::I64],
                mutable: true,
            },
            WirStructDef {
                fields: vec![Kind::GcRef(2)],
                mutable: true,
            },
        ];
        let arrays = vec![WirArrayDef { element: Kind::GcRef(0) }];
        let payload = |value| WirExpr::StructNew {
            struct_id: 0,
            args: vec![WirExpr::RefNull(Kind::ExternRef), WirExpr::ConstI64(value)],
        };
        let item = |array: &str, index| WirExpr::StructGet {
            struct_id: 0,
            field: 1,
            base: Box::new(WirExpr::ArrayGet {
                array_id: 0,
                array: Box::new(WirExpr::GetLocal(array.into())),
                index: Box::new(WirExpr::ConstI32(index)),
            }),
        };

        let takes_array = WirFunc {
            name: "takes_array".into(),
            params: vec![local("items", WirTy::GcRef(2))],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Drop(WirExpr::ArrayLen(Box::new(
                WirExpr::GetLocal("items".into()),
            )))],
            raw_body: None,
        };

        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![
                local("repeated", WirTy::GcRef(2)),
                local("fixed", WirTy::GcRef(2)),
                local("holder", WirTy::GcRef(1)),
            ],
            body: vec![
                WirNode::SetLocal {
                    local: "repeated".into(),
                    value: WirExpr::ArrayNew {
                        array_id: 0,
                        value: Box::new(WirExpr::RefNull(Kind::GcRef(0))),
                        len: Box::new(WirExpr::ConstI32(3)),
                    },
                },
                WirNode::ArraySet {
                    array_id: 0,
                    array: WirExpr::GetLocal("repeated".into()),
                    index: WirExpr::ConstI32(1),
                    value: payload(99),
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![item("repeated", 1)],
                }),
                WirNode::SetLocal {
                    local: "holder".into(),
                    value: WirExpr::StructNew {
                        struct_id: 1,
                        args: vec![WirExpr::GetLocal("repeated".into())],
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::ToSlot(
                        Box::new(WirExpr::ArrayLen(Box::new(WirExpr::StructGet {
                            struct_id: 1,
                            field: 0,
                            base: Box::new(WirExpr::GetLocal("holder".into())),
                        }))),
                        Kind::I32,
                    )],
                }),
                WirNode::SetLocal {
                    local: "fixed".into(),
                    value: WirExpr::ArrayNewFixed {
                        array_id: 0,
                        items: vec![payload(7), payload(8)],
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![item("fixed", 0)],
                }),
            ],
            raw_body: None,
        };
        let mut module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![takes_array, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };

        crate::wir_opt::optimize(&mut module);
        let binary = encode_with_gc(&module, &structs, &arrays);
        assert_eq!(run_binary(&binary), vec!["99", "3", "7"]);
    }

    /// RFC-0005 Stage 4 closure substrate: a uniform closure wrapper can hold
    /// either the legacy linear environment or an erased GC payload. The lifted
    /// body recovers its statically assigned payload with `ref.cast` before
    /// reading capability-bearing fields.
    #[test]
    fn closure_wrapper_erases_and_recovers_a_gc_environment() {
        use crate::wir::{
            CLOSURE_CODE_FIELD, CLOSURE_GC_ENV_FIELD, CLOSURE_LINEAR_ENV_FIELD, WirStructDef,
            closure_wrapper_struct,
        };

        let structs = vec![
            closure_wrapper_struct(),
            WirStructDef {
                fields: vec![Kind::ExternRef, Kind::I64],
                mutable: true,
            },
        ];
        let wrapper = WirExpr::GetLocal("closure".into());
        let payload = WirExpr::RefCast {
            struct_id: 1,
            value: Box::new(WirExpr::StructGet {
                struct_id: 0,
                field: CLOSURE_GC_ENV_FIELD,
                base: Box::new(wrapper.clone()),
            }),
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("erased", WirTy::StructRef), local("closure", WirTy::GcRef(0))],
            body: vec![
                WirNode::SetLocal {
                    local: "erased".into(),
                    value: WirExpr::StructNew {
                        struct_id: 1,
                        args: vec![WirExpr::RefNull(Kind::ExternRef), WirExpr::ConstI64(42)],
                    },
                },
                WirNode::SetLocal {
                    local: "closure".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![
                            WirExpr::ConstI32(7),
                            WirExpr::ConstI32(17),
                            WirExpr::GetLocal("erased".into()),
                        ],
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::StructGet {
                            struct_id: 0,
                            field: CLOSURE_LINEAR_ENV_FIELD,
                            base: Box::new(WirExpr::GetLocal("closure".into())),
                        }),
                    }],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::StructGet {
                            struct_id: 0,
                            field: CLOSURE_CODE_FIELD,
                            base: Box::new(wrapper),
                        }),
                    }],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::StructGet {
                        struct_id: 1,
                        field: 1,
                        base: Box::new(payload),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
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

        assert_eq!(run_binary(&encode(&module, &structs)), vec!["17", "7", "42"]);

        let invalid_mutation = WirFunc {
            name: "mutate_wrapper".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("closure", WirTy::GcRef(0))],
            body: vec![
                WirNode::SetLocal {
                    local: "closure".into(),
                    value: WirExpr::StructNew {
                        struct_id: 0,
                        args: vec![
                            WirExpr::ConstI32(7),
                            WirExpr::ConstI32(17),
                            WirExpr::RefNull(Kind::StructRef),
                        ],
                    },
                },
                WirNode::StructSet {
                    struct_id: 0,
                    field: CLOSURE_CODE_FIELD,
                    base: WirExpr::GetLocal("closure".into()),
                    value: WirExpr::ConstI32(8),
                },
            ],
            raw_body: None,
        };
        let invalid_module = WirModule {
            imports: vec![],
            funcs: vec![invalid_mutation],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "mutate_wrapper".into())],
        };
        let invalid_binary = encode(&invalid_module, &structs);
        let error = match wasmparser::validate(&invalid_binary) {
            Ok(_) => panic!("the encoded closure wrapper fields must be immutable"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("immutable"),
            "unexpected validation error: {error}"
        );
    }

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
