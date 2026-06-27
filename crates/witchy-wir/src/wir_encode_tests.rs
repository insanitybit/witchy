    use super::*;
    use crate::wir::{
        BinOp, DataSegment, GlobalInit, Kind, UnOp, WirExpr, WirFunc, WirGlobal, WirImport,
        WirLocal, WirModule, WirNode, WirTable, WirTy,
    };
    use std::sync::{Arc, Mutex};

    fn local(name: &str, ty: WirTy) -> WirLocal {
        WirLocal { name: name.into(), ty }
    }

    /// Instantiate a wasm binary and run its `run` export, capturing `print_int`
    /// and `print` output as ordered lines. (Copied from `wir.rs`'s test setup.)
    fn run_binary(binary: &[u8]) -> Vec<String> {
        // Fuel-capped so a buggy helper loop TRAPS fast instead of hanging the
        // suite (a runaway $find_byte/$str_eq once spun a test for 70 minutes).
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        let engine = wasmtime::Engine::new(&config).expect("engine");
        let m = wasmtime::Module::new(&engine, binary)
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
                    let s = String::from_utf8_lossy(&data[ptr as usize..(ptr + len) as usize])
                        .into_owned();
                    o.lock().unwrap().push(s);
                },
            )
            .unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        store.set_fuel(500_000_000).expect("fuel"); // ~5e8 ops — ample for tests, traps runaways
        let inst = linker.instantiate(&mut store, &m).expect("instantiate");
        let run = inst.get_typed_func::<(), ()>(&mut store, "run").expect("run export");
        run.call(&mut store, ()).expect("run (or fuel-exhausted — likely a runaway loop)");
        out.lock().unwrap().clone()
    }

    /// Run a module via the binary encoder.
    fn run_encoded(module: &WirModule) -> Vec<String> {
        run_binary(&encode(module))
    }

    /// Assert the encoder output runs identically to the expected lines. (Was
    /// also a binary-vs-`to_wat` agreement gate; the WAT leg is retired with the
    /// `wat` crate — `to_wat` is now only emit-wat's display, not an exec path.)
    fn assert_agrees(module: &WirModule, expected: &[&str]) {
        let exp: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(run_encoded(module), exp, "binary output mismatch");
    }

    /// Module with one Int-returning func + a `run` that prints its result.
    /// (Mirrors `wir.rs`'s `int_demo`.)
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

    /// The #35 memory primitives — Store / Load(offset) / MemoryCopy / MemoryFill /
    /// MemorySize / MemoryGrow and an unsigned compare — encode AND print-via-WAT
    /// identically and run correctly. These are the nodes the allocation helpers
    /// ($ensure/$concat/$mkN) lower to.
    #[test]
    fn memory_ops_roundtrip() {
        use WirExpr::*;
        let i32c = |n: i32| ConstI32(n);
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                // mem[16] = 99 (i64), then copy those 8 bytes to mem[32].
                WirNode::Store { ptr: i32c(16), value: ConstI64(99), kind: Kind::I64, offset: 0 },
                WirNode::MemoryCopy { dest: i32c(32), src: i32c(16), len: i32c(8) },
                // print the copied value (99).
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Load { ptr: Box::new(i32c(32)), kind: Kind::I64, offset: 0 }],
                }),
                // memory.size >u 0  →  1.
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Binary {
                            op: BinOp::GtU,
                            kind: Kind::I32,
                            lhs: Box::new(MemorySize),
                            rhs: Box::new(i32c(0)),
                        }),
                    }],
                }),
                // memory.grow(1) returns the previous size in pages (1).
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(MemoryGrow(Box::new(i32c(1)))),
                    }],
                }),
                // memory.fill mem[64..72] = 0, then read one byte back (0).
                WirNode::MemoryFill { dest: i32c(64), value: i32c(0), len: i32c(8) },
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Load { ptr: Box::new(i32c(64)), kind: Kind::I32, offset: 0 }),
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
        assert_agrees(&module, &["99", "1", "1", "0"]);
    }

    /// Byte-level Store8 / Load8U round-trip through both paths: write two bytes,
    /// read them back zero-extended. These back `$int_to_string` and the string
    /// helpers.
    #[test]
    fn byte_ops_roundtrip() {
        use WirExpr::*;
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::Store8 { ptr: ConstI32(100), value: ConstI32(65), offset: 0 },
                WirNode::Store8 { ptr: ConstI32(100), value: ConstI32(66), offset: 1 },
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Load8U { ptr: Box::new(ConstI32(100)), offset: 0 }),
                    }],
                }),
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Load8U { ptr: Box::new(ConstI32(101)), offset: 0 }),
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
        assert_agrees(&module, &["65", "66"]);
    }

    /// `$list_push_cap` in-place path: a cap-2 list `[1][10]` with one slot to
    /// spare appends `20` in place (no grow), returns (same ptr, cap). Exercises
    /// the migrated multi-value helper + CallStoreMulti end-to-end.
    #[test]
    fn list_push_cap_in_place() {
        use crate::wir::{ensure_helper, list_push_cap_helper};
        use WirExpr::*;
        let conv = |e: WirExpr| Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let pi = |e: WirExpr| WirNode::Do(CallHost { import: "print_int".into(), args: vec![e] });
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("rp", WirTy::Bool), local("rc", WirTy::Bool)],
            body: vec![
                // list at 2048: len=1, elem0=10 (cap 2 → one slot to spare)
                WirNode::Store { ptr: ConstI32(2048), value: ConstI32(1), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI64(10), kind: Kind::I64, offset: 4 },
                WirNode::SetGlobal { global: "heap".into(), value: ConstI32(2068) },
                WirNode::CallStoreMulti {
                    func: "list_push_cap".into(),
                    args: vec![ConstI32(2048), ConstI64(20), ConstI32(2)],
                    dests: vec!["rp".into(), "rc".into()],
                },
                // new len (2), the appended elem (20), the returned ptr (2048) & cap (2)
                pi(conv(Load { ptr: Box::new(ConstI32(2048)), kind: Kind::I32, offset: 0 })),
                pi(Load { ptr: Box::new(ConstI32(2060)), kind: Kind::I64, offset: 0 }),
                pi(conv(GetLocal("rp".into()))),
                pi(conv(GetLocal("rc".into()))),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(), list_push_cap_helper(), run],
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal {
                    name: "heap".into(),
                    kind: Kind::I32,
                    mutable: true,
                    init: GlobalInit::I32(2068),
                    export: None,
                },
                WirGlobal {
                    name: "__witchy_reowns".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_reowns".into()),
                },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // Agreement gate: the encoder AND the WAT printer (multi-value func +
        // CallStoreMulti arm) both run identically.
        assert_agrees(&module, &["2", "20", "2048", "2"]);
    }

    /// CallStoreMulti calls a MULTI-result function and stores each result into a
    /// local (reverse pop order). Exercises both the new node and a multi-value
    /// function (`pair` leaves two i32s via dual tail Push) — the shape the
    /// in-place cap ABI ($list_push_cap) uses. Encoder path (the production sink).
    #[test]
    fn call_store_multi_roundtrip() {
        let pair = WirFunc {
            name: "pair".into(),
            params: vec![],
            ret: vec![WirTy::Bool, WirTy::Bool], // (result i32 i32)
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::ConstI32(10)),
                WirNode::Push(WirExpr::ConstI32(20)),
            ],
            raw_body: None,
        };
        let pi = |name: &str| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert {
                    from: Kind::I32,
                    to: Kind::I64,
                    arg: Box::new(WirExpr::GetLocal(name.into())),
                }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("a", WirTy::Bool), local("b", WirTy::Bool)],
            body: vec![
                WirNode::CallStoreMulti {
                    func: "pair".into(),
                    args: vec![],
                    dests: vec!["a".into(), "b".into()],
                },
                pi("a"),
                pi("b"),
            ],
            raw_body: None,
        };
        let module = WirModule {
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
        assert_agrees(&module, &["10", "20"]);
    }

    /// The WIR-native `$int_to_string` helper renders signed integers correctly
    /// (zero, positive multi-digit, negative) — printed via `print` so the host
    /// reads the produced `[len][ascii]` string. Both paths agree.
    #[test]
    fn int_to_string_helper_renders() {
        use crate::wir::{ensure_helper, int_to_string_helper, print_str_helper};
        // run(): print_str(int_to_string(N)) for a few N.
        let mut body = Vec::new();
        for n in [0i64, 7, 4096, -123] {
            body.push(WirNode::Do(WirExpr::Call {
                func: "print_str".into(),
                args: vec![WirExpr::Call {
                    func: "int_to_string".into(),
                    args: vec![WirExpr::ConstI64(n)],
                }],
            }));
        }
        let run = WirFunc { name: "run".into(), params: vec![], ret: vec![], locals: vec![], body, raw_body: None };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print".into(),
                params: vec![Kind::I32, Kind::I32],
                results: vec![],
            }],
            funcs: vec![ensure_helper(), int_to_string_helper(), print_str_helper(), run],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["0", "7", "4096", "-123"]);
    }

    /// The WIR-native `$ensure` helper grows memory correctly: with `$heap = 0`,
    /// `ensure(100000)` needs >1 page (65536 bytes), so memory grows from 1 to 2
    /// pages. Runs identically through the encoder and the WAT path.
    #[test]
    fn ensure_helper_grows_memory() {
        use crate::wir::ensure_helper;
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::Do(WirExpr::Call {
                    func: "ensure".into(),
                    args: vec![WirExpr::ConstI32(100_000)],
                }),
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(WirExpr::MemorySize),
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
            funcs: vec![ensure_helper(), run],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(0),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["2"]);
    }

    /// `$find_byte` (a scan loop with an inner byte-compare loop whose mismatch
    /// `br` lives inside an `if`) finds substrings correctly — the regression test
    /// for the encoder counting `if` as a branch frame. Fuel-capped, so a
    /// mis-encoded `br` traps instead of spinning.
    #[test]
    fn find_byte_finds_substrings() {
        use crate::wir::find_byte_helper;
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 100, bytes: mk_str("hello") },
            DataSegment { offset: 120, bytes: mk_str("ell") },
            DataSegment { offset: 140, bytes: mk_str("xyz") },
            DataSegment { offset: 160, bytes: mk_str("") },
        ];
        let fb = |s: i32, sub: i32| WirExpr::Call {
            func: "find_byte".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(sub)],
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(fb(100, 120)), pi(fb(100, 140)), pi(fb(100, 160))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![find_byte_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "ell" is at index 1 of "hello"; "xyz" absent (-1); "" → 0.
        assert_agrees(&module, &["1", "-1", "0"]);
    }

    #[test]
    fn starts_with_matches_prefixes() {
        use crate::wir::starts_with_helper;
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 100, bytes: mk_str("hello") },
            DataSegment { offset: 120, bytes: mk_str("hel") },
            DataSegment { offset: 140, bytes: mk_str("lo") },
            DataSegment { offset: 160, bytes: mk_str("") },
        ];
        let sw = |s: i32, p: i32| WirExpr::Call {
            func: "starts_with".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(p)],
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(sw(100, 120)), pi(sw(100, 140)), pi(sw(100, 160))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![starts_with_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "hello" starts with "hel" → 1; with "lo" → 0; with "" → 1.
        assert_agrees(&module, &["1", "0", "1"]);
    }

    #[test]
    fn ends_with_matches_suffixes() {
        use crate::wir::ends_with_helper;
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 100, bytes: mk_str("hello") },
            DataSegment { offset: 120, bytes: mk_str("llo") },
            DataSegment { offset: 140, bytes: mk_str("hel") },
            DataSegment { offset: 160, bytes: mk_str("") },
        ];
        let ew = |s: i32, p: i32| WirExpr::Call {
            func: "ends_with".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(p)],
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(ew(100, 120)), pi(ew(100, 140)), pi(ew(100, 160))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ends_with_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "hello" ends with "llo" → 1; with "hel" → 0; with "" → 1.
        assert_agrees(&module, &["1", "0", "1"]);
    }

    #[test]
    fn str_index_of_returns_char_index() {
        use crate::wir::{byte_to_char_helper, find_byte_helper, str_index_of_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 100, bytes: mk_str("hello") },
            DataSegment { offset: 120, bytes: mk_str("ll") },
            DataSegment { offset: 140, bytes: mk_str("xyz") },
            // "héllo": the 'é' is two UTF-8 bytes, so "llo" sits at *byte* 3 but
            // *char* index 2 — exercising byte_to_char's continuation-byte skip.
            DataSegment { offset: 160, bytes: mk_str("héllo") },
            DataSegment { offset: 180, bytes: mk_str("llo") },
        ];
        let ix = |s: i32, sub: i32| WirExpr::Call {
            func: "str_index_of".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(sub)],
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(ix(100, 120)), pi(ix(100, 140)), pi(ix(160, 180))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![find_byte_helper(), byte_to_char_helper(), str_index_of_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "ll" is char 2 of "hello"; "xyz" absent (-1); "llo" is char 2 of "héllo".
        assert_agrees(&module, &["2", "-1", "2"]);
    }

    #[test]
    fn substr_copies_a_byte_slice() {
        use crate::wir::{ensure_helper, substr_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![DataSegment { offset: 200, bytes: mk_str("hello") }];
        let sub = |src: i32, start: i32, len: i32| WirExpr::Call {
            func: "substr".into(),
            args: vec![WirExpr::ConstI32(src), WirExpr::ConstI32(start), WirExpr::ConstI32(len)],
        };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool)],
            body: vec![
                // "ell" = bytes 1..4 of "hello": length 3, first byte 'e' (101).
                WirNode::SetLocal { local: "r1".into(), value: sub(200, 1, 3) },
                pi(load_i32(WirExpr::GetLocal("r1".into()))),
                pi(byte0(WirExpr::GetLocal("r1".into()))),
                // "he" = bytes 0..2: length 2, first byte 'h' (104).
                WirNode::SetLocal { local: "r2".into(), value: sub(200, 0, 2) },
                pi(load_i32(WirExpr::GetLocal("r2".into()))),
                pi(byte0(WirExpr::GetLocal("r2".into()))),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(), substr_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["3", "101", "2", "104"]);
    }

    #[test]
    fn str_substring_slices_by_char_index() {
        use crate::wir::{char_to_byte_helper, ensure_helper, str_substring_helper, substr_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("hello world") },
            DataSegment { offset: 220, bytes: mk_str("héllo") },
        ];
        let ss = |s: i32, a: i32, b: i32| WirExpr::Call {
            func: "str_substring".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(a), WirExpr::ConstI32(b)],
        };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool)],
            body: vec![
                // "hello" = chars 0..5 of "hello world": 5 bytes, first byte 'h' (104).
                WirNode::SetLocal { local: "r1".into(), value: ss(200, 0, 5) },
                pi(load_i32(WirExpr::GetLocal("r1".into()))),
                pi(byte0(WirExpr::GetLocal("r1".into()))),
                // "él" = chars 1..3 of "héllo": é is 2 bytes so the slice is 3
                // bytes, first byte 0xc3 (195) — the char→byte mapping at work.
                WirNode::SetLocal { local: "r2".into(), value: ss(220, 1, 3) },
                pi(load_i32(WirExpr::GetLocal("r2".into()))),
                pi(byte0(WirExpr::GetLocal("r2".into()))),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![
                ensure_helper(),
                substr_helper(),
                char_to_byte_helper(),
                str_substring_helper(),
                run,
            ],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["5", "104", "3", "195"]);
    }

    #[test]
    fn trim_strips_surrounding_whitespace() {
        use crate::wir::{ensure_helper, is_ws_helper, substr_helper, trim_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("  hi  ") },
            DataSegment { offset: 220, bytes: mk_str("abc") },
            DataSegment { offset: 240, bytes: mk_str("   ") },
        ];
        let tr = |s: i32| WirExpr::Call { func: "trim".into(), args: vec![WirExpr::ConstI32(s)] };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) }],
            })
        };
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool), local("r3", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "r1".into(), value: tr(200) },
                pi(load_i32(gl("r1"))),
                pi(byte0(gl("r1"))),
                WirNode::SetLocal { local: "r2".into(), value: tr(220) },
                pi(load_i32(gl("r2"))),
                pi(byte0(gl("r2"))),
                // all-whitespace trims to empty (length 0).
                WirNode::SetLocal { local: "r3".into(), value: tr(240) },
                pi(load_i32(gl("r3"))),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(), substr_helper(), is_ws_helper(), trim_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "  hi  " → "hi" (len 2, 'h'=104); "abc" → "abc" (len 3, 'a'=97); "   " → "" (len 0).
        assert_agrees(&module, &["2", "104", "3", "97", "0"]);
    }

    #[test]
    fn list_push_appends_elements() {
        use crate::wir::{ensure_helper, list_push_helper};
        // An empty list is a bare i32 length header of 0.
        let data = vec![DataSegment { offset: 200, bytes: 0u32.to_le_bytes().to_vec() }];
        let push = |list: WirExpr, x: i64| WirExpr::Call {
            func: "list_push".into(),
            args: vec![list, WirExpr::ConstI64(x)],
        };
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let len_of = |p: WirExpr| WirExpr::Convert {
            from: Kind::I32,
            to: Kind::I64,
            arg: Box::new(WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 }),
        };
        let elem = |p: WirExpr, off: u32| WirExpr::Load { ptr: Box::new(p), kind: Kind::I64, offset: off };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "r1".into(), value: push(WirExpr::ConstI32(200), 42) },
                WirNode::SetLocal { local: "r2".into(), value: push(gl("r1"), 99) },
                pi(len_of(gl("r2"))),      // length 2
                pi(elem(gl("r2"), 4)),     // element 0 → 42
                pi(elem(gl("r2"), 12)),    // element 1 → 99
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(), list_push_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["2", "42", "99"]);
    }

    #[test]
    fn split_breaks_on_separator() {
        use crate::wir::{ensure_helper, list_push_helper, split_helper, substr_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("a,b,c") },
            DataSegment { offset: 220, bytes: mk_str(",") },
        ];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let wrap = |e: WirExpr| WirExpr::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(e) };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        // The string pointer held in list slot `off` (an i64), wrapped to i32.
        let elem_ptr = |off: u32| wrap(WirExpr::Load { ptr: Box::new(gl("r")), kind: Kind::I64, offset: off });
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal {
                    local: "r".into(),
                    value: WirExpr::Call {
                        func: "split".into(),
                        args: vec![WirExpr::ConstI32(200), WirExpr::ConstI32(220)],
                    },
                },
                pi(to_i64(load_i32(gl("r")))),         // list length 3
                pi(to_i64(load_i32(elem_ptr(4)))),     // piece 0 "a" length 1
                pi(to_i64(byte0(elem_ptr(4)))),        // piece 0 first byte 'a' 97
                pi(to_i64(byte0(elem_ptr(20)))),       // piece 2 "c" first byte 'c' 99
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(), substr_helper(), list_push_helper(), split_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "a,b,c".split(",") → ["a","b","c"]: 3 pieces; piece0="a", piece2="c".
        assert_agrees(&module, &["3", "1", "97", "99"]);
    }

    #[test]
    fn str_chars_splits_into_characters() {
        use crate::wir::{
            byte_to_char_helper, char_to_byte_helper, ensure_helper, list_push_helper,
            str_chars_helper, str_substring_helper, substr_helper,
        };
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        // "héllo": 5 characters but 6 bytes (é is two bytes).
        let data = vec![DataSegment { offset: 200, bytes: mk_str("héllo") }];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let wrap = |e: WirExpr| WirExpr::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(e) };
        let load_i32 = |p: WirExpr| WirExpr::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let elem_ptr = |off: u32| wrap(WirExpr::Load { ptr: Box::new(gl("r")), kind: Kind::I64, offset: off });
        let byte0 = |p: WirExpr| WirExpr::Load8U { ptr: Box::new(p), offset: 4 };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal {
                    local: "r".into(),
                    value: WirExpr::Call { func: "str_chars".into(), args: vec![WirExpr::ConstI32(200)] },
                },
                pi(to_i64(load_i32(gl("r")))),       // 5 characters
                pi(to_i64(load_i32(elem_ptr(4)))),   // char 0 "h" length 1
                pi(to_i64(byte0(elem_ptr(4)))),      // char 0 first byte 'h' 104
                pi(to_i64(load_i32(elem_ptr(12)))),  // char 1 "é" length 2 (bytes)
                pi(to_i64(byte0(elem_ptr(12)))),     // char 1 first byte 0xc3 195
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![
                ensure_helper(),
                substr_helper(),
                char_to_byte_helper(),
                str_substring_helper(),
                byte_to_char_helper(),
                list_push_helper(),
                str_chars_helper(),
                run,
            ],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["5", "1", "104", "2", "195"]);
    }

    #[test]
    fn list_concat_joins_two_lists() {
        use crate::wir::{ensure_helper, list_concat_helper};
        // A list is an i32 length header followed by 8-byte (i64) element slots.
        let mk_list = |xs: &[i64]| {
            let mut b = (xs.len() as u32).to_le_bytes().to_vec();
            for x in xs {
                b.extend_from_slice(&x.to_le_bytes());
            }
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_list(&[10, 20]) },
            DataSegment { offset: 240, bytes: mk_list(&[30]) },
        ];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let elem = |off: u32| WirExpr::Load { ptr: Box::new(gl("r")), kind: Kind::I64, offset: off };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal {
                    local: "r".into(),
                    value: WirExpr::Call {
                        func: "list_concat".into(),
                        args: vec![WirExpr::ConstI32(200), WirExpr::ConstI32(240)],
                    },
                },
                pi(to_i64(WirExpr::Load { ptr: Box::new(gl("r")), kind: Kind::I32, offset: 0 })), // length 3
                pi(elem(4)),  // 10
                pi(elem(12)), // 20
                pi(elem(20)), // 30
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(), list_concat_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // [10,20] ++ [30] = [10,20,30].
        assert_agrees(&module, &["3", "10", "20", "30"]);
    }

    #[test]
    fn ascii_case_changes_letter_case() {
        use crate::wir::{ascii_case_helper, ensure_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![DataSegment { offset: 200, bytes: mk_str("aB1") }];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let cased = |up: i32| WirExpr::Call {
            func: "ascii_case".into(),
            args: vec![WirExpr::ConstI32(200), WirExpr::ConstI32(up)],
        };
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        // byte `off` of the string pointer held in local `name`.
        let byte = |name: &'static str, off: u32| {
            to_i64(WirExpr::Load8U { ptr: Box::new(gl(name)), offset: 4 + off })
        };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("u", WirTy::Bool), local("d", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "u".into(), value: cased(1) },
                pi(byte("u", 0)), // 'a'→'A' = 65
                pi(byte("u", 1)), // 'B' stays = 66
                WirNode::SetLocal { local: "d".into(), value: cased(0) },
                pi(byte("d", 0)), // 'a' stays = 97
                pi(byte("d", 1)), // 'B'→'b' = 98
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![ensure_helper(), ascii_case_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["65", "66", "97", "98"]);
    }

    #[test]
    fn str_to_int_parses_signed_decimals() {
        use crate::wir::{is_ws_helper, str_to_int_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("123") },
            DataSegment { offset: 220, bytes: mk_str("-45") },
            DataSegment { offset: 240, bytes: mk_str("  7  ") },
            DataSegment { offset: 260, bytes: mk_str("+9") },
        ];
        let parse = |off: i32| WirExpr::Call { func: "str_to_int".into(), args: vec![WirExpr::ConstI32(off)] };
        let pi = |e: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] })
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![pi(parse(200)), pi(parse(220)), pi(parse(240)), pi(parse(260))],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![is_ws_helper(), str_to_int_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        // "123"→123, "-45"→-45, "  7  "→7 (whitespace trimmed), "+9"→9.
        assert_agrees(&module, &["123", "-45", "7", "9"]);
    }

    #[test]
    fn dict_insert_get_has_int_keys() {
        use crate::wir::{
            dict_find_helper, dict_get_or_helper, dict_has_helper, dict_hash_helper,
            dict_insert_helper, dict_new_helper, ensure_helper, key_eq_helper, str_eq_helper,
        };
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        // dict_insert(d, k, v, mode=0): re-bind `d` to the fresh map.
        let ins = |k: i64, v: i64| WirNode::SetLocal {
            local: "d".into(),
            value: WirExpr::Call {
                func: "dict_insert".into(),
                args: vec![gl("d"), WirExpr::ConstI64(k), WirExpr::ConstI64(v), WirExpr::ConstI32(0)],
            },
        };
        let get = |k: i64| WirExpr::Call {
            func: "dict_get_or".into(),
            args: vec![gl("d"), WirExpr::ConstI64(k), WirExpr::ConstI64(-1), WirExpr::ConstI32(0)],
        };
        let has = |k: i64| WirExpr::Convert {
            from: Kind::I32,
            to: Kind::I64,
            arg: Box::new(WirExpr::Call {
                func: "dict_has".into(),
                args: vec![gl("d"), WirExpr::ConstI64(k), WirExpr::ConstI32(0)],
            }),
        };
        let pi = |e: WirExpr| WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] });
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("d", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "d".into(), value: WirExpr::Call { func: "dict_new".into(), args: vec![] } },
                ins(1, 100),
                ins(2, 200),
                ins(1, 111), // update existing key 1
                pi(get(1)),  // 111
                pi(get(2)),  // 200
                pi(get(3)),  // -1 (absent → default)
                pi(has(2)),  // 1
                pi(has(5)),  // 0
                // final count = 2 (key 1 was updated, not appended).
                pi(WirExpr::Convert {
                    from: Kind::I32,
                    to: Kind::I64,
                    arg: Box::new(WirExpr::Load { ptr: Box::new(gl("d")), kind: Kind::I32, offset: 0 }),
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
            funcs: vec![
                ensure_helper(),
                str_eq_helper(),
                key_eq_helper(),
                dict_hash_helper(),
                dict_find_helper(),
                dict_new_helper(),
                dict_insert_helper(),
                dict_get_or_helper(),
                dict_has_helper(),
                run,
            ],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal {
                name: "heap".into(),
                kind: Kind::I32,
                mutable: true,
                init: GlobalInit::I32(1024),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["111", "200", "-1", "1", "0", "2"]);
    }

    #[test]
    fn dict_keys_values_remove_int_keys() {
        use crate::wir::{
            dict_find_helper, dict_get_or_helper, dict_hash_helper, dict_insert_helper,
            dict_new_helper, dict_project_helper, dict_remove_helper, ensure_helper, key_eq_helper,
            str_eq_helper,
        };
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let ins = |k: i64, v: i64| WirNode::SetLocal {
            local: "d".into(),
            value: WirExpr::Call {
                func: "dict_insert".into(),
                args: vec![gl("d"), WirExpr::ConstI64(k), WirExpr::ConstI64(v), WirExpr::ConstI32(0)],
            },
        };
        let call1 = |f: &str, a: &str| WirExpr::Call { func: f.into(), args: vec![gl(a)] };
        // list element `idx` (an i64 slot) of the list pointer held in `name`.
        let elem = |name: &'static str, idx: u32| WirExpr::Load { ptr: Box::new(gl(name)), kind: Kind::I64, offset: 4 + idx * 8 };
        let len = |name: &'static str| WirExpr::Convert {
            from: Kind::I32,
            to: Kind::I64,
            arg: Box::new(WirExpr::Load { ptr: Box::new(gl(name)), kind: Kind::I32, offset: 0 }),
        };
        let get = |name: &'static str, k: i64| WirExpr::Call {
            func: "dict_get_or".into(),
            args: vec![gl(name), WirExpr::ConstI64(k), WirExpr::ConstI64(-1), WirExpr::ConstI32(0)],
        };
        let pi = |e: WirExpr| WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] });
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("d", WirTy::Bool), local("ks", WirTy::Bool), local("vs", WirTy::Bool), local("rm", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "d".into(), value: WirExpr::Call { func: "dict_new".into(), args: vec![] } },
                ins(1, 100),
                ins(2, 200),
                ins(3, 300),
                WirNode::SetLocal { local: "ks".into(), value: call1("dict_keys", "d") },
                WirNode::SetLocal { local: "vs".into(), value: call1("dict_values", "d") },
                WirNode::SetLocal {
                    local: "rm".into(),
                    value: WirExpr::Call { func: "dict_remove".into(), args: vec![gl("d"), WirExpr::ConstI64(2), WirExpr::ConstI32(0)] },
                },
                pi(len("ks")),     // 3 keys
                pi(elem("ks", 0)), // key 1
                pi(elem("ks", 2)), // key 3
                pi(elem("vs", 1)), // value 200
                pi(WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(WirExpr::Load { ptr: Box::new(gl("rm")), kind: Kind::I32, offset: 0 }) }), // remove count 2
                pi(get("rm", 1)),  // 100 (kept)
                pi(get("rm", 2)),  // -1 (removed)
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] }],
            funcs: vec![
                ensure_helper(),
                str_eq_helper(),
                key_eq_helper(),
                dict_hash_helper(),
                dict_find_helper(),
                dict_new_helper(),
                dict_insert_helper(),
                dict_get_or_helper(),
                dict_project_helper("dict_keys", 4),
                dict_project_helper("dict_values", 12),
                dict_remove_helper(),
                run,
            ],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(1024), export: None }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["3", "1", "3", "200", "2", "100", "-1"]);
    }

    #[test]
    fn replace_rewrites_matches() {
        use crate::wir::{ensure_helper, match_at_helper, replace_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("hello") },
            DataSegment { offset: 220, bytes: mk_str("l") },
            DataSegment { offset: 240, bytes: mk_str("L") },
            DataSegment { offset: 260, bytes: mk_str("aaa") },
            DataSegment { offset: 280, bytes: mk_str("a") },
            DataSegment { offset: 300, bytes: mk_str("bb") },
            DataSegment { offset: 320, bytes: mk_str("ab") },
            DataSegment { offset: 340, bytes: mk_str("") },
            DataSegment { offset: 360, bytes: mk_str("-") },
        ];
        let gl = |n: &str| WirExpr::GetLocal(n.into());
        let rep = |s: i32, f: i32, t: i32| WirExpr::Call {
            func: "replace".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI32(f), WirExpr::ConstI32(t)],
        };
        let to_i64 = |e: WirExpr| WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let len = |name: &'static str| to_i64(WirExpr::Load { ptr: Box::new(gl(name)), kind: Kind::I32, offset: 0 });
        let byte = |name: &'static str, off: u32| to_i64(WirExpr::Load8U { ptr: Box::new(gl(name)), offset: 4 + off });
        let pi = |e: WirExpr| WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![e] });
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("r1", WirTy::Bool), local("r2", WirTy::Bool), local("r3", WirTy::Bool)],
            body: vec![
                WirNode::SetLocal { local: "r1".into(), value: rep(200, 220, 240) }, // "hello"/l→L = "heLLo"
                pi(len("r1")),       // 5
                pi(byte("r1", 2)),   // 'L' 76
                pi(byte("r1", 3)),   // 'L' 76
                WirNode::SetLocal { local: "r2".into(), value: rep(260, 280, 300) }, // "aaa"/a→bb = "bbbbbb"
                pi(len("r2")),       // 6
                pi(byte("r2", 0)),   // 'b' 98
                WirNode::SetLocal { local: "r3".into(), value: rep(320, 340, 360) }, // "ab"/""→- = "-a-b-"
                pi(len("r3")),       // 5
                pi(byte("r3", 0)),   // '-' 45
                pi(byte("r3", 1)),   // 'a' 97
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] }],
            funcs: vec![ensure_helper(), match_at_helper(), replace_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(1024), export: None }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["5", "76", "76", "6", "98", "5", "45", "97"]);
    }

    #[test]
    fn arithmetic_roundtrips() {
        // fn add() -> Int: (2 + 3) * 4 == 20
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
        assert_agrees(&m, &["20"]);
    }

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
        // print(console, "hi"): data [2,0,0,0,'h','i'] at offset 8.
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
            type_arity: 1,
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
