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
        let module = print_int_module(vec![run], vec![]);
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
        let module = print_int_module(vec![run], vec![]);
        assert_agrees(&module, &["65", "66"]);
    }

    /// `$list_push_cap` in-place path: a cap-2 list `[1][10]` with one slot to
    /// spare appends `20` in place (no grow), returns (same ptr, cap). Exercises
    /// the migrated multi-value helper + CallStoreMulti end-to-end.
    #[test]
    fn list_push_cap_in_place() {
        use crate::wir_helpers::{ensure_helper, list_push_cap_helper};
        use WirExpr::*;
        let conv = |e: WirExpr| Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
        let pi = |e: WirExpr| WirNode::Do(CallHost { import: "print_int".into(), args: vec![e] });
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("rp", WirTy::Bool), local("rc", WirTy::Bool)],
            body: vec![
                // list at 2048: len=1, elem0=10 (cap 2 → one slot to spare). The
                // `$rc_alloc` size header at `[2048-4]` records the real allocation
                // (4-byte len word + 2 i64 slots = 20 bytes) so the RFC-0005 in-place
                // bounds check (in `$list_push_cap`) sees the true capacity — a real
                // list always carries this header.
                WirNode::Store { ptr: ConstI32(2044), value: ConstI32(20), kind: Kind::I32, offset: 0 },
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
        let module = print_int_module(
            vec![ensure_helper(false), list_push_cap_helper(), run],
            vec![
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
        );
        // Agreement gate: the encoder AND the WAT printer (multi-value func +
        // CallStoreMulti arm) both run identically.
        assert_agrees(&module, &["2", "20", "2048", "2"]);
    }

    /// RFC-0005 step 2: the in-place `$list_push_cap` bounds check TRAPS when the `cap`
    /// token overstates the buffer's real allocation. A cap-1 buffer (size header 12 =
    /// 4-byte len word + one i64 slot), appended to as if `cap` were 2, would write a
    /// SECOND element PAST the block — silent heap corruption. The check converts that
    /// into a loud trap, exactly as `$list_at` traps an out-of-bounds read.
    #[test]
    fn list_push_cap_traps_on_overstated_cap() {
        use WirExpr::*;
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("rp", WirTy::Bool), local("rc", WirTy::Bool)],
            body: vec![
                // list at 2048: REAL allocation is 12 bytes (len word + ONE i64 slot), len=1.
                WirNode::Store { ptr: ConstI32(2044), value: ConstI32(12), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI32(1), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI64(10), kind: Kind::I64, offset: 4 },
                WirNode::SetGlobal { global: "heap".into(), value: ConstI32(2060) },
                // cap=2 LIES: the real element capacity is 1. The in-place append at index
                // 1 would land past the block, so the bounds check must trap.
                WirNode::CallStoreMulti {
                    func: "list_push_cap".into(),
                    args: vec![ConstI32(2048), ConstI64(20), ConstI32(2)],
                    dests: vec!["rp".into(), "rc".into()],
                },
            ],
            raw_body: None,
        };
        let module = print_int_module(
            vec![
                crate::wir_helpers::ensure_helper(false),
                crate::wir_helpers::list_push_cap_helper(),
                run,
            ],
            vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2060), export: None },
                WirGlobal {
                    name: "__witchy_reowns".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_reowns".into()),
                },
            ],
        );
        assert_traps(&module);
    }

    /// RFC-0005 step 2: the in-place `$str_append_cap` bounds check TRAPS when `cap`
    /// overstates the buffer's real allocation. A 6-byte buffer (4-byte len word + 2
    /// spare bytes) appended to as if it had room for 5 more would copy PAST the block;
    /// the check traps before the copy. (Only the len/plen headers matter — the trap
    /// fires before any byte is read.)
    #[test]
    fn str_append_cap_traps_on_overstated_cap() {
        use WirExpr::*;
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("rp", WirTy::Bool), local("rc", WirTy::Bool)],
            body: vec![
                // s at 2048: REAL allocation is 6 bytes (len word + 2 bytes), len=2.
                WirNode::Store { ptr: ConstI32(2044), value: ConstI32(6), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI32(2), kind: Kind::I32, offset: 0 },
                // piece at 2064: plen=3.
                WirNode::Store { ptr: ConstI32(2064), value: ConstI32(3), kind: Kind::I32, offset: 0 },
                WirNode::SetGlobal { global: "heap".into(), value: ConstI32(2080) },
                // cap=5 LIES: real capacity is 2. need = 2+3 = 5; in-place fires (cap >=
                // need), and the copy would run to byte s+4+5 = 9, past the 6-byte block.
                WirNode::CallStoreMulti {
                    func: "str_append_cap".into(),
                    args: vec![ConstI32(2048), ConstI32(2064), ConstI32(5)],
                    dests: vec!["rp".into(), "rc".into()],
                },
            ],
            raw_body: None,
        };
        let module = print_int_module(
            vec![
                crate::wir_helpers::ensure_helper(false),
                crate::wir_helpers::str_append_cap_helper(),
                run,
            ],
            vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2080), export: None },
                WirGlobal {
                    name: "__witchy_reowns".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_reowns".into()),
                },
            ],
        );
        assert_traps(&module);
    }

    /// RFC-0005 step 2: the in-place `$dict_insert_cap` APPEND bounds check TRAPS when
    /// `cap` overstates the buffer's real allocation. An empty dict whose rc size header
    /// (`[d-8]`) is too small for even one entry (an entry at index `count` needs
    /// `count*16+24` bytes) is appended to with cap=1; the check traps instead of writing
    /// the entry past the block. (A dict is `rc_alloc(..)+4`, so the hidden index word is
    /// at `d-4` and the size header at `d-8`; setting the index word to 0 makes the find a
    /// linear scan and skips `dict_index_put`.)
    #[test]
    fn dict_insert_cap_traps_on_overstated_cap() {
        use WirExpr::*;
        // d = 2052: size header [2044]=20 (< the 24 one entry needs), index word
        // [2048]=0 (linear-scan find, no index update), count [2052]=0.
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![local("rp", WirTy::Bool), local("rc", WirTy::Bool)],
            body: vec![
                WirNode::Store { ptr: ConstI32(2044), value: ConstI32(20), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2048), value: ConstI32(0), kind: Kind::I32, offset: 0 },
                WirNode::Store { ptr: ConstI32(2052), value: ConstI32(0), kind: Kind::I32, offset: 0 },
                WirNode::SetGlobal { global: "heap".into(), value: ConstI32(2200) },
                // dict_insert_cap(d=2052, k=5, v=9, mode=0 int-keys, cap=1): key not found,
                // cap(1) > count(0) → append_inplace → the check fires (24 > 20).
                WirNode::CallStoreMulti {
                    func: "dict_insert_cap".into(),
                    args: vec![ConstI32(2052), ConstI64(5), ConstI64(9), ConstI32(0), ConstI32(1)],
                    dests: vec!["rp".into(), "rc".into()],
                },
            ],
            raw_body: None,
        };
        let mut funcs = helper_closure("dict_insert_cap");
        funcs.push(run);
        let module = print_int_module(
            funcs,
            vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2200), export: None },
                WirGlobal {
                    name: "__witchy_reowns".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__witchy_reowns".into()),
                },
            ],
        );
        assert_traps(&module);
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
        let module = print_int_module(vec![pair, run], vec![]);
        assert_agrees(&module, &["10", "20"]);
    }

    /// The WIR-native `$int_to_string` helper renders signed integers correctly
    /// (zero, positive multi-digit, negative) — printed via `print` so the host
    /// reads the produced `[len][ascii]` string. Both paths agree.
    #[test]
    fn int_to_string_helper_renders() {
        use crate::wir_helpers::{ensure_helper, int_to_string_helper, print_str_helper};
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
            funcs: vec![ensure_helper(false), int_to_string_helper(false), print_str_helper(), run],
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
        use crate::wir_helpers::ensure_helper;
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
            funcs: vec![ensure_helper(false), run],
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
        use crate::wir_helpers::find_byte_helper;
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
        use crate::wir_helpers::starts_with_helper;
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
        use crate::wir_helpers::ends_with_helper;
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
        use crate::wir_helpers::{byte_to_char_helper, find_byte_helper, str_index_of_helper};
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
        use crate::wir_helpers::{ensure_helper, substr_helper};
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
            funcs: vec![ensure_helper(false), substr_helper(), run],
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
        use crate::wir_helpers::{char_to_byte_helper, ensure_helper, str_substring_helper, substr_helper};
        let mk_str = |s: &str| {
            let mut b = (s.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(s.as_bytes());
            b
        };
        let data = vec![
            DataSegment { offset: 200, bytes: mk_str("hello world") },
            DataSegment { offset: 220, bytes: mk_str("héllo") },
        ];
        // `start`/`end` ride the full-width i64 index path (BUG-011); `s` is the i32 ptr.
        let ss = |s: i32, a: i64, b: i64| WirExpr::Call {
            func: "str_substring".into(),
            args: vec![WirExpr::ConstI32(s), WirExpr::ConstI64(a), WirExpr::ConstI64(b)],
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
                ensure_helper(false),
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
        use crate::wir_helpers::{ensure_helper, is_ws_helper, substr_helper, trim_helper};
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
            funcs: vec![ensure_helper(false), substr_helper(), is_ws_helper(), trim_helper(), run],
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
        use crate::wir_helpers::{ensure_helper, list_push_helper};
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
            funcs: vec![ensure_helper(false), list_push_helper(), run],
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
        use crate::wir_helpers::{ensure_helper, list_push_helper, split_helper, substr_helper};
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
            funcs: vec![ensure_helper(false), substr_helper(), list_push_helper(), split_helper(), run],
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
        use crate::wir_helpers::{
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
                ensure_helper(false),
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
        use crate::wir_helpers::{ensure_helper, list_concat_helper};
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
            funcs: vec![ensure_helper(false), list_concat_helper(), run],
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
        use crate::wir_helpers::{ascii_case_helper, ensure_helper};
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
            funcs: vec![ensure_helper(false), ascii_case_helper(), run],
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
        use crate::wir_helpers::{is_ws_helper, str_to_int_helper};
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
            imports: vec![
                WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] },
                // (RFC-0045) `str_to_int` routes its parse-failure aborts through
                // `__witchy_abort`, so the import must be declared for the encoder.
                WirImport {
                    name: "__witchy_abort".into(),
                    params: vec![Kind::I32, Kind::I64, Kind::I64, Kind::I32],
                    results: vec![],
                },
            ],
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
        use crate::wir_helpers::{
            dict_find_helper, dict_get_or_helper, dict_has_helper, dict_hash_helper,
            dict_insert_helper, dict_new_helper, ensure_helper, key_eq_helper, rc_alloc_helper,
            str_eq_helper,
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
        let module = print_int_module(
            vec![
                ensure_helper(false),
                rc_alloc_helper(),
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
            vec![
                WirGlobal {
                    name: "heap".into(),
                    kind: Kind::I32,
                    mutable: true,
                    init: GlobalInit::I32(1024),
                    export: None,
                },
                WirGlobal {
                    name: "rc_freelist".into(),
                    kind: Kind::I32,
                    mutable: true,
                    init: GlobalInit::I32(0),
                    export: None,
                },
                WirGlobal {
                    name: "__rc_reused_bytes".into(),
                    kind: Kind::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: None,
                },
            ],
        );
        assert_agrees(&module, &["111", "200", "-1", "1", "0", "2"]);
    }

    /// RFC-0088's dictionary oracle counts structural search invocations, not
    /// key comparisons. A test double for `$dict_find` returns a configured
    /// entry position and increments `searches`; every present/missing insert
    /// and remove must consume exactly one result.
    #[test]
    fn dict_extract_helpers_perform_one_semantic_search() {
        use crate::wir_helpers::{
            bump_alloc_helper, dict_hash_helper, dict_index_put_helper,
            dict_insert_extract_helper, dict_reindex_helper, dict_remove_extract_helper,
            ensure_helper, leaf_drop_helper, leaf_dup_helper, rc_alloc_helper, rc_drop_helper,
            rc_dup_helper, rc_free_helper, slot_take_or_dup_helper,
        };
        let gl = |name: &str| WirExpr::GetLocal(name.into());
        let gi = |name: &str| WirExpr::GetGlobal(name.into());
        let pi = |value: WirExpr| {
            WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![value] })
        };
        let i32_to_i64 = |value: WirExpr| WirExpr::Convert {
            from: Kind::I32,
            to: Kind::I64,
            arg: Box::new(value),
        };
        let find = WirFunc {
            name: "dict_find".into(),
            params: vec![
                local("d", WirTy::Bool),
                local("k", WirTy::Int),
                local("mode", WirTy::Bool),
            ],
            ret: vec![WirTy::Bool],
            locals: vec![],
            body: vec![
                WirNode::SetGlobal {
                    global: "searches".into(),
                    value: WirExpr::Binary {
                        op: BinOp::Add,
                        kind: Kind::I64,
                        lhs: Box::new(gi("searches")),
                        rhs: Box::new(WirExpr::ConstI64(1)),
                    },
                },
                WirNode::Push(gi("find_result")),
            ],
            raw_body: None,
        };
        let mut body = vec![
            // One-entry source dictionary at 2048: key=7, value=70.
            WirNode::Store { ptr: WirExpr::ConstI32(2044), value: WirExpr::ConstI32(0), kind: Kind::I32, offset: 0 },
            WirNode::Store { ptr: WirExpr::ConstI32(2048), value: WirExpr::ConstI32(1), kind: Kind::I32, offset: 0 },
            WirNode::Store { ptr: WirExpr::ConstI32(2048), value: WirExpr::ConstI64(7), kind: Kind::I64, offset: 4 },
            WirNode::Store { ptr: WirExpr::ConstI32(2048), value: WirExpr::ConstI64(70), kind: Kind::I64, offset: 12 },
        ];
        let mut probe = |helper: &str, find_result: i32, args: Vec<WirExpr>| {
            body.extend([
                WirNode::SetGlobal { global: "searches".into(), value: WirExpr::ConstI64(0) },
                WirNode::SetGlobal { global: "find_result".into(), value: WirExpr::ConstI32(find_result) },
                WirNode::CallStoreMulti {
                    func: helper.into(),
                    args,
                    dests: vec!["out".into(), "present".into(), "old".into(), "cap".into()],
                },
                pi(gi("searches")),
                pi(i32_to_i64(gl("present"))),
                pi(gl("old")),
                pi(i32_to_i64(WirExpr::Load { ptr: Box::new(gl("out")), kind: Kind::I32, offset: 0 })),
            ]);
        };
        probe(
            "dict_insert_extract",
            0,
            vec![WirExpr::ConstI32(2048), WirExpr::ConstI64(7), WirExpr::ConstI64(71), WirExpr::ConstI32(0), WirExpr::ConstI32(0), WirExpr::ConstI32(-1), WirExpr::ConstI32(-1)],
        );
        probe(
            "dict_insert_extract",
            -1,
            vec![WirExpr::ConstI32(2048), WirExpr::ConstI64(8), WirExpr::ConstI64(80), WirExpr::ConstI32(0), WirExpr::ConstI32(0), WirExpr::ConstI32(-1), WirExpr::ConstI32(-1)],
        );
        probe(
            "dict_remove_extract",
            0,
            vec![WirExpr::ConstI32(2048), WirExpr::ConstI64(7), WirExpr::ConstI32(0), WirExpr::ConstI32(0), WirExpr::ConstI32(-1), WirExpr::ConstI32(-1)],
        );
        probe(
            "dict_remove_extract",
            -1,
            vec![WirExpr::ConstI32(2048), WirExpr::ConstI64(9), WirExpr::ConstI32(0), WirExpr::ConstI32(0), WirExpr::ConstI32(-1), WirExpr::ConstI32(-1)],
        );
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![
                local("out", WirTy::Bool),
                local("present", WirTy::Bool),
                local("old", WirTy::Int),
                local("cap", WirTy::Bool),
            ],
            body,
            raw_body: None,
        };
        let module = print_int_module(
            vec![
                ensure_helper(false),
                bump_alloc_helper(),
                rc_alloc_helper(),
                rc_free_helper(),
                rc_dup_helper(),
                rc_drop_helper(),
                leaf_dup_helper(),
                leaf_drop_helper(),
                slot_take_or_dup_helper(),
                dict_hash_helper(),
                dict_index_put_helper(),
                dict_reindex_helper(),
                find,
                dict_insert_extract_helper(),
                dict_remove_extract_helper(),
                run,
            ],
            vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(4096), export: None },
                WirGlobal { name: "rc_freelist".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
                WirGlobal { name: "__rc_reused_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_live_cells".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_bump_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_reuse_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_free_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_region_rewind_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_active".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
                WirGlobal { name: "__witchy_extract_searches".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_key_comparisons".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_copied_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_retains".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_extract_drops".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "heap_base".into(), kind: Kind::I32, mutable: false, init: GlobalInit::I32(2048), export: None },
                WirGlobal { name: "searches".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "find_result".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
            ],
        );
        assert_agrees(
            &module,
            &[
                "1", "1", "70", "1",
                "1", "0", "0", "2",
                "1", "1", "70", "0",
                "1", "0", "0", "1",
            ],
        );
    }

    #[test]
    fn dict_keys_values_remove_int_keys() {
        use crate::wir_helpers::{
            dict_find_helper, dict_get_or_helper, dict_hash_helper, dict_insert_helper,
            dict_new_helper, dict_project_helper, dict_remove_helper, ensure_helper, key_eq_helper,
            rc_alloc_helper, str_eq_helper,
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
        let module = print_int_module(
            vec![
                ensure_helper(false),
                rc_alloc_helper(),
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
            vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(1024), export: None },
                WirGlobal { name: "rc_freelist".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
                WirGlobal { name: "__rc_reused_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            ],
        );
        assert_agrees(&module, &["3", "1", "3", "200", "2", "100", "-1"]);
    }

    #[test]
    fn replace_rewrites_matches() {
        use crate::wir_helpers::{ensure_helper, match_at_helper, replace_helper};
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
            funcs: vec![ensure_helper(false), match_at_helper(), replace_helper(), run],
            memory_pages: 1,
            data,
            globals: vec![WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(1024), export: None }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["5", "76", "76", "6", "98", "5", "45", "97"]);
    }
    fn print_int_module(funcs: Vec<WirFunc>, globals: Vec<WirGlobal>) -> WirModule {
        WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs,
            memory_pages: 1,
            data: vec![],
            globals,
            table: None,
            exports: vec![("run".into(), "run".into())],
        }
    }
