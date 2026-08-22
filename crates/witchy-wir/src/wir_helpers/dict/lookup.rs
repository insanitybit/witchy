//! Dictionary key equality, hashing, and entry lookup.

use crate::wir::*;

/// `$key_eq(a, b, mode) -> i32` — slot equality under the key's compile-time
/// type: mode 0 = raw i64 (Int/Bool), 1 = `$str_eq` on the pointers (String),
/// else f64 (the slots reinterpreted as doubles).
pub(crate) fn key_eq_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let wrap = |n: &str| E::FromSlot(Box::new(getl(n)), Kind::I32);
    WirFunc {
        name: "key_eq".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Int },
            WirLocal { name: "b".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![N::If {
            cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("mode")) },
            then_: vec![N::Push(E::Binary {
                op: BinOp::Eq,
                kind: Kind::I64,
                lhs: Box::new(getl("a")),
                rhs: Box::new(getl("b")),
            })],
            els: vec![N::If {
                cond: E::Binary { op: BinOp::Eq, kind: Kind::I32, lhs: Box::new(getl("mode")), rhs: Box::new(i32c(1)) },
                then_: vec![N::Push(E::Call { func: "str_eq".into(), args: vec![wrap("a"), wrap("b")] })],
                els: vec![N::Push(E::Binary {
                    op: BinOp::Eq,
                    kind: Kind::F64,
                    lhs: Box::new(E::FromSlot(Box::new(getl("a")), Kind::F64)),
                    rhs: Box::new(E::FromSlot(Box::new(getl("b")), Kind::F64)),
                })],
                result: Some(WirTy::Bool),
            }],
            result: Some(WirTy::Bool),
        }],
        raw_body: None,
    }
}
/// `$dict_hash(k, mode) -> i64` — the full internal 64-bit mix.
/// FNV-1a over the bytes for string keys (mode 1, `k` = string pointer). Only
/// consulted by `$dict_find`'s (binary-path-dormant) hash probe.
pub(crate) fn dict_hash_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b32 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // String hashing (mode 1): a foldhash-inspired word-at-a-time mix. Reads the
    // bytes 8 at a time (an i64 word) and folds each into a 64-bit accumulator
    // with a multiply + xorshift — far faster than byte-by-byte FNV-1a (one
    // multiply per 8 bytes, not per byte) with better avalanche. WASM has no
    // 128-bit `folded_multiply`, so this uses the native i64 multiply. The hash
    // is internal to the dict's open-addressing index, so changing it only moves
    // keys between slots — observable dict behavior is unchanged. Not
    // DoS-resistant (fixed constants), which a value-semantic dict does not need.
    let c1 = i64c(-49064778989728563i64); // 0xff51afd7ed558ccd (murmur3 fmix)
    let c2 = i64c(-4265267296055464877i64);
    let vec_loop = N::Block {
        label: "vdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "vl".into(),
            body: vec![
                N::Br { target: "vdone".into(), cond: Some(b32(BinOp::Gt, b32(BinOp::Add, getl("i"), i32c(16)), getl("len"))) },
                setl("w", E::Load { ptr: Box::new(b32(BinOp::Add, getl("p"), getl("i"))), kind: Kind::I64, offset: 4 }),
                setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), getl("w")), c1.clone())),
                setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(32)))),
                setl("w", E::Load { ptr: Box::new(b32(BinOp::Add, getl("p"), getl("i"))), kind: Kind::I64, offset: 12 }),
                setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), getl("w")), c2.clone())),
                setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(32)))),
                setl("i", b32(BinOp::Add, getl("i"), i32c(16))),
                N::Br { target: "vl".into(), cond: None },
            ],
        }],
    };
    let word_loop = N::Block {
        label: "wdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "wl".into(),
            body: vec![
                N::Br { target: "wdone".into(), cond: Some(b32(BinOp::Gt, b32(BinOp::Add, getl("i"), i32c(8)), getl("len"))) },
                setl("w", E::Load { ptr: Box::new(b32(BinOp::Add, getl("p"), getl("i"))), kind: Kind::I64, offset: 4 }),
                setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), getl("w")), c1.clone())),
                setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(32)))),
                setl("i", b32(BinOp::Add, getl("i"), i32c(8))),
                N::Br { target: "wl".into(), cond: None },
            ],
        }],
    };
    let tail_loop = N::Block {
        label: "tdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "tl".into(),
            body: vec![
                N::Br { target: "tdone".into(), cond: Some(b32(BinOp::Ge, getl("i"), getl("len"))) },
                setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(E::Load8U { ptr: Box::new(b32(BinOp::Add, getl("p"), getl("i"))), offset: 4 }) }), c1.clone())),
                setl("i", b32(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "tl".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_hash".into(),
        params: vec![
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int],
        locals: vec![
            WirLocal { name: "x".into(), ty: WirTy::Int },
            WirLocal { name: "w".into(), ty: WirTy::Int },
            WirLocal { name: "p".into(), ty: WirTy::Bool },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("mode")) },
                then_: vec![
                    setl("x", getl("k")),
                    setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(33)))),
                    setl("x", b64(BinOp::Mul, getl("x"), i64c(-49064778989728563))),
                    setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(33)))),
                    N::Return(Some(getl("x"))),
                ],
                els: vec![],
                result: None,
            },
            setl("p", E::FromSlot(Box::new(getl("k")), Kind::I32)),
            setl("len", E::Load { ptr: Box::new(getl("p")), kind: Kind::I32, offset: 0 }),
            N::If {
                cond: E::GetGlobal("__witchy_extract_active".into()),
                then_: vec![N::SetGlobal { global: "__witchy_dict_hashes".into(), value: E::Binary { op: BinOp::Add, kind: Kind::I64, lhs: Box::new(E::GetGlobal("__witchy_dict_hashes".into())), rhs: Box::new(i64c(1)) } }],
                els: vec![],
                result: None,
            },
            setl("x", i64c(-7046029254386353131i64)), // 0x9e3779b97f4a7c15 (golden-ratio seed)
            setl("i", i32c(0)),
            vec_loop,
            word_loop,
            tail_loop,
            // Fold in the length, then a final avalanche so the low bits the
            // open-addressing index masks (`& (slots-1)`) are well mixed.
            setl("x", b64(BinOp::Xor, getl("x"), E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("len")) })),
            setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(32))), i64c(-4265267296055464877i64))),
            setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(29)))),
            N::Push(getl("x")),
        ],
        raw_body: None,
    }
}

/// `$dict_ctrl_h2(idx, h, h2) -> i32` — SIMD H2 membership for one control
/// group. The scalar probe still owns empty-lane termination; this helper only
/// turns the 16-byte equality into a candidate-lane bit test.
pub(crate) fn dict_ctrl_h2_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dict_ctrl_h2".into(),
        params: vec![
            WirLocal { name: "idx".into(), ty: WirTy::Bool },
            WirLocal { name: "h".into(), ty: WirTy::Bool },
            WirLocal { name: "h2".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "group".into(), ty: WirTy::Bool },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "group".into(), value: b(BinOp::And, getl("h"), i32c(-16)) },
            N::SetLocal {
                local: "mask".into(),
                value: E::Vector {
                    op: VectorOp::I8x16Bitmask,
                    args: vec![E::Vector {
                        op: VectorOp::I8x16Eq,
                        args: vec![
                            E::Load { ptr: Box::new(b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(4), getl("group")))), kind: Kind::V128, offset: 0 },
                            E::Vector { op: VectorOp::I8x16Splat, args: vec![getl("h2")] },
                        ],
                    }],
                },
            },
            N::Push(b(BinOp::Ne, b(BinOp::And, getl("mask"), b(BinOp::Shl, i32c(1), b(BinOp::And, getl("h"), i32c(15)))), i32c(0))),
        ],
        raw_body: None,
    }
}

/// `$dict_find(d, k, mode) -> i32` — the entry index of key `k`, or -1. Linear
/// scan when the hidden index word is 0; otherwise probe the Swiss control
/// bytes before loading candidate entry keys.
pub(crate) fn dict_find_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E, off: u32| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: off };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // key slot of entry `e`: d + 4 + e*16.
    let dense_key_at = |e: E| E::Load { ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, e, i32c(16)))), kind: Kind::I64, offset: 4 };
    let indexed_key_at = |h: E| E::Load {
        ptr: Box::new(b(
            BinOp::Add,
            getl("idx"),
            b(BinOp::Add, i32c(20), b(BinOp::Add, getl("slots"), b(BinOp::Add, b(BinOp::Mul, getl("slots"), i32c(4)), b(BinOp::Mul, h, i32c(8))))),
        )),
        kind: Kind::I64,
        offset: 0,
    };
    let keq = |e: E| E::Call { func: "key_eq".into(), args: vec![dense_key_at(e), getl("k"), getl("mode")] };
    let indexed_keq = |h: E| E::Call { func: "key_eq".into(), args: vec![indexed_key_at(h), getl("k"), getl("mode")] };
    let comparison_bump = || N::If {
        cond: E::GetGlobal("__witchy_extract_active".into()),
        then_: vec![N::SetGlobal {
            global: "__witchy_extract_key_comparisons".into(),
            value: E::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(E::GetGlobal("__witchy_extract_key_comparisons".into())),
                rhs: Box::new(E::ConstI64(1)),
            },
        }],
        els: vec![],
        result: None,
    };
    let linear = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                comparison_bump(),
                N::If { cond: keq(getl("i")), then_: vec![N::Return(Some(getl("i")))], els: vec![], result: None },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    let slot_at_h = load(
        b(BinOp::Add, b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), getl("slots"))), b(BinOp::Mul, getl("h"), i32c(4))),
        0,
    );
    let probe = N::Block {
        label: "miss".into(),
        result: None,
        body: vec![N::Loop {
            label: "p".into(),
            body: vec![
                N::Br { target: "miss".into(), cond: Some(b(BinOp::Ge, getl("attempt"), getl("slots"))) },
                N::If {
                    cond: E::GetGlobal("__witchy_extract_active".into()),
                    then_: vec![N::SetGlobal { global: "__witchy_dict_probe_groups".into(), value: E::Binary { op: BinOp::Add, kind: Kind::I64, lhs: Box::new(E::GetGlobal("__witchy_dict_probe_groups".into())), rhs: Box::new(E::ConstI64(1)) } }],
                    els: vec![],
                    result: None,
                },
                setl("ctrl", E::Load8U {
                    ptr: Box::new(b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(4), getl("h")))),
                    offset: 0,
                }),
                N::Br { target: "miss".into(), cond: Some(b(BinOp::Eq, getl("ctrl"), i32c(0x80))) },
                N::If {
                    cond: E::Call { func: "dict_ctrl_h2".into(), args: vec![getl("idx"), getl("h"), getl("h2")] },
                    then_: vec![
                        N::If {
                            cond: E::GetGlobal("__witchy_extract_active".into()),
                            then_: vec![N::SetGlobal { global: "__witchy_dict_h2_candidates".into(), value: E::Binary { op: BinOp::Add, kind: Kind::I64, lhs: Box::new(E::GetGlobal("__witchy_dict_h2_candidates".into())), rhs: Box::new(E::ConstI64(1)) } }],
                            els: vec![],
                            result: None,
                        },
                        setl("e", slot_at_h.clone()),
                        N::If {
                            cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("e")) },
                            then_: vec![N::Br { target: "miss".into(), cond: None }],
                            els: vec![
                                comparison_bump(),
                                N::If {
                                    cond: indexed_keq(getl("h")),
                                    then_: vec![N::Return(Some(b(BinOp::Sub, getl("e"), i32c(1))))],
                                    els: vec![],
                                    result: None,
                                },
                            ],
                            result: None,
                        },
                    ],
                    els: vec![],
                    result: None,
                },
                setl("h", b(BinOp::And, b(BinOp::Add, getl("h"), i32c(1)), b(BinOp::Sub, getl("slots"), i32c(1)))),
                setl("attempt", b(BinOp::Add, getl("attempt"), i32c(1))),
                N::Br { target: "p".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_find".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            "idx", "count", "i", "slots", "h", "h2", "attempt", "e", "ctrl",
        ].into_iter().map(|n| WirLocal { name: n.into(), ty: WirTy::Bool })
        .chain(std::iter::once(WirLocal { name: "hash".into(), ty: WirTy::Int }))
        .collect(),
        body: vec![
            setl("idx", load(b(BinOp::Sub, getl("d"), i32c(4)), 0)),
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("idx")) },
                then_: vec![
                    setl("count", load(getl("d"), 0)),
                    setl("i", i32c(0)),
                    linear,
                    N::Return(Some(i32c(-1))),
                ],
                els: vec![],
                result: None,
            },
            setl("slots", load(getl("idx"), 0)),
            setl("hash", E::Call { func: "dict_hash".into(), args: vec![getl("k"), getl("mode")] }),
            setl("h", b(BinOp::And, E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(getl("hash")) }, b(BinOp::Sub, getl("slots"), i32c(1)))),
            setl("h2", b(BinOp::And, E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(E::Binary { op: BinOp::ShrU, kind: Kind::I64, lhs: Box::new(getl("hash")), rhs: Box::new(E::ConstI64(57)) }) }, i32c(0x7f))),
            setl("attempt", i32c(0)),
            probe,
            N::Push(i32c(-1)),
        ],
        raw_body: None,
    }
}

/// `$dict_slice_eq(str_ptr, slice_ptr, slice_len) -> i32` — compare an existing
/// `[len: i32][bytes...]` string against a raw `(slice_ptr, slice_len)` slice.
pub(crate) fn dict_slice_eq_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let bin = |op: BinOp, l: E, r: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let load_i32 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let load_v128 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::V128, offset: 0 };
    let str_byte_at = |off: &str| E::Load8U {
        ptr: Box::new(bin(BinOp::Add, bin(BinOp::Add, getl("str_ptr"), i32c(4)), getl(off))),
        offset: 0,
    };
    let slice_byte_at = |off: &str| E::Load8U {
        ptr: Box::new(bin(BinOp::Add, getl("slice_ptr"), getl(off))),
        offset: 0,
    };
    WirFunc {
        name: "dict_slice_eq".into(),
        params: vec![
            WirLocal { name: "str_ptr".into(), ty: WirTy::Str },
            WirLocal { name: "slice_ptr".into(), ty: WirTy::Str },
            WirLocal { name: "slice_len".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "v1".into(), ty: WirTy::V128 },
            WirLocal { name: "v2".into(), ty: WirTy::V128 },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::If {
                cond: bin(BinOp::Ne, load_i32(getl("str_ptr")), getl("slice_len")),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "i".into(), value: i32c(0) },
            N::Block {
                label: "vdone".into(),
                result: None,
                body: vec![N::Loop {
                    label: "vl".into(),
                    body: vec![
                        N::Br {
                            target: "vdone".into(),
                            cond: Some(bin(BinOp::Gt, bin(BinOp::Add, getl("i"), i32c(16)), getl("slice_len"))),
                        },
                        N::SetLocal {
                            local: "v1".into(),
                            value: load_v128(bin(BinOp::Add, bin(BinOp::Add, getl("str_ptr"), i32c(4)), getl("i"))),
                        },
                        N::SetLocal {
                            local: "v2".into(),
                            value: load_v128(bin(BinOp::Add, getl("slice_ptr"), getl("i"))),
                        },
                        N::SetLocal {
                            local: "mask".into(),
                            value: E::Vector {
                                op: VectorOp::I8x16Bitmask,
                                args: vec![E::Vector {
                                    op: VectorOp::I8x16Eq,
                                    args: vec![getl("v1"), getl("v2")],
                                }],
                            },
                        },
                        N::If {
                            cond: bin(BinOp::Ne, getl("mask"), i32c(0xffff)),
                            then_: vec![N::Return(Some(i32c(0)))],
                            els: vec![],
                            result: None,
                        },
                        N::SetLocal { local: "i".into(), value: bin(BinOp::Add, getl("i"), i32c(16)) },
                        N::Br { target: "vl".into(), cond: None },
                    ],
                }],
            },
            N::Block {
                label: "wdone".into(),
                result: None,
                body: vec![N::Loop {
                    label: "wl".into(),
                    body: vec![
                        N::Br {
                            target: "wdone".into(),
                            cond: Some(bin(BinOp::Gt, bin(BinOp::Add, getl("i"), i32c(8)), getl("slice_len"))),
                        },
                        N::If {
                            cond: E::Binary {
                                op: BinOp::Ne,
                                kind: Kind::I64,
                                lhs: Box::new(E::Load {
                                    ptr: Box::new(bin(BinOp::Add, bin(BinOp::Add, getl("str_ptr"), i32c(4)), getl("i"))),
                                    kind: Kind::I64,
                                    offset: 0,
                                }),
                                rhs: Box::new(E::Load {
                                    ptr: Box::new(bin(BinOp::Add, getl("slice_ptr"), getl("i"))),
                                    kind: Kind::I64,
                                    offset: 0,
                                }),
                            },
                            then_: vec![N::Return(Some(i32c(0)))],
                            els: vec![],
                            result: None,
                        },
                        N::SetLocal { local: "i".into(), value: bin(BinOp::Add, getl("i"), i32c(8)) },
                        N::Br { target: "wl".into(), cond: None },
                    ],
                }],
            },
            N::Block {
                label: "tdone".into(),
                result: None,
                body: vec![N::Loop {
                    label: "tl".into(),
                    body: vec![
                        N::Br {
                            target: "tdone".into(),
                            cond: Some(bin(BinOp::Ge, getl("i"), getl("slice_len"))),
                        },
                        N::If {
                            cond: bin(BinOp::Ne, str_byte_at("i"), slice_byte_at("i")),
                            then_: vec![N::Return(Some(i32c(0)))],
                            els: vec![],
                            result: None,
                        },
                        N::SetLocal { local: "i".into(), value: bin(BinOp::Add, getl("i"), i32c(1)) },
                        N::Br { target: "tl".into(), cond: None },
                    ],
                }],
            },
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$dict_hash_slice(p, len) -> i64` — the full slice hash.
pub(crate) fn dict_hash_slice_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b32 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let c1 = i64c(-49064778989728563i64);
    let c2 = i64c(-4265267296055464877i64);
    let vec_loop = N::Block {
        label: "vdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "vl".into(),
            body: vec![
                N::Br { target: "vdone".into(), cond: Some(b32(BinOp::Gt, b32(BinOp::Add, getl("i"), i32c(16)), getl("len"))) },
                setl("w", E::Load { ptr: Box::new(b32(BinOp::Add, getl("p"), getl("i"))), kind: Kind::I64, offset: 0 }),
                setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), getl("w")), c1.clone())),
                setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(32)))),
                setl("w", E::Load { ptr: Box::new(b32(BinOp::Add, getl("p"), getl("i"))), kind: Kind::I64, offset: 8 }),
                setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), getl("w")), c2.clone())),
                setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(32)))),
                setl("i", b32(BinOp::Add, getl("i"), i32c(16))),
                N::Br { target: "vl".into(), cond: None },
            ],
        }],
    };
    let word_loop = N::Block {
        label: "wdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "wl".into(),
            body: vec![
                N::Br { target: "wdone".into(), cond: Some(b32(BinOp::Gt, b32(BinOp::Add, getl("i"), i32c(8)), getl("len"))) },
                setl("w", E::Load { ptr: Box::new(b32(BinOp::Add, getl("p"), getl("i"))), kind: Kind::I64, offset: 0 }),
                setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), getl("w")), c1.clone())),
                setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(32)))),
                setl("i", b32(BinOp::Add, getl("i"), i32c(8))),
                N::Br { target: "wl".into(), cond: None },
            ],
        }],
    };
    let tail_loop = N::Block {
        label: "tdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "tl".into(),
            body: vec![
                N::Br { target: "tdone".into(), cond: Some(b32(BinOp::Ge, getl("i"), getl("len"))) },
                setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(E::Load8U { ptr: Box::new(b32(BinOp::Add, getl("p"), getl("i"))), offset: 0 }) }), c1.clone())),
                setl("i", b32(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "tl".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_hash_slice".into(),
        params: vec![
            WirLocal { name: "p".into(), ty: WirTy::Str },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int],
        locals: vec![
            WirLocal { name: "x".into(), ty: WirTy::Int },
            WirLocal { name: "w".into(), ty: WirTy::Int },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::If {
                cond: E::GetGlobal("__witchy_extract_active".into()),
                then_: vec![N::SetGlobal { global: "__witchy_dict_hashes".into(), value: E::Binary { op: BinOp::Add, kind: Kind::I64, lhs: Box::new(E::GetGlobal("__witchy_dict_hashes".into())), rhs: Box::new(i64c(1)) } }],
                els: vec![],
                result: None,
            },
            setl("x", i64c(-7046029254386353131i64)),
            setl("i", i32c(0)),
            vec_loop,
            word_loop,
            tail_loop,
            setl("x", b64(BinOp::Xor, getl("x"), E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("len")) })),
            setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(32))), i64c(-4265267296055464877i64))),
            setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(29)))),
            N::Push(getl("x")),
        ],
        raw_body: None,
    }
}

/// `$dict_find_slice(d, p, len) -> i32` — find entry index for raw slice `(p, len)`.
pub(crate) fn dict_find_slice_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |ptr: E, off: u32| E::Load { ptr: Box::new(ptr), kind: Kind::I32, offset: off };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let dense_key_at = |e: E| E::FromSlot(Box::new(E::Load { ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, e, i32c(16)))), kind: Kind::I64, offset: 4 }), Kind::I32);
    let indexed_key_at = |h: E| {
        let ptr = b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), b(BinOp::Add, getl("slots"), b(BinOp::Add, b(BinOp::Mul, getl("slots"), i32c(4)), b(BinOp::Mul, h, i32c(8))))));
        E::FromSlot(Box::new(E::Load { ptr: Box::new(ptr), kind: Kind::I64, offset: 0 }), Kind::I32)
    };
    let keq = |e: E| E::Call { func: "dict_slice_eq".into(), args: vec![dense_key_at(e), getl("p"), getl("len")] };
    let indexed_keq = |h: E| E::Call { func: "dict_slice_eq".into(), args: vec![indexed_key_at(h), getl("p"), getl("len")] };
    let linear = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                N::If { cond: keq(getl("i")), then_: vec![N::Return(Some(getl("i")))], els: vec![], result: None },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    let ctrl_at_h = E::Load8U {
        ptr: Box::new(b(BinOp::Add, b(BinOp::Add, getl("idx"), i32c(4)), getl("h"))),
        offset: 0,
    };
    let slot_at_h = load(
        b(BinOp::Add, b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), getl("slots"))), b(BinOp::Mul, getl("h"), i32c(4))),
        0,
    );
    let probe = N::Block {
        label: "miss".into(),
        result: None,
        body: vec![N::Loop {
            label: "p".into(),
            body: vec![
                N::Br { target: "miss".into(), cond: Some(b(BinOp::Ge, getl("attempt"), getl("slots"))) },
                N::If {
                    cond: b(BinOp::Eq, ctrl_at_h.clone(), i32c(0x80)),
                    then_: vec![N::Return(Some(i32c(-1)))],
                    els: vec![N::If {
                        cond: E::Call { func: "dict_ctrl_h2".into(), args: vec![getl("idx"), getl("h"), getl("h2")] },
                        then_: vec![
                            setl("e", slot_at_h.clone()),
                            N::If {
                                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("e")) },
                                then_: vec![N::Return(Some(i32c(-1)))],
                                els: vec![N::If {
                                    cond: indexed_keq(getl("h")),
                                    then_: vec![N::Return(Some(b(BinOp::Sub, getl("e"), i32c(1))))],
                                    els: vec![],
                                    result: None,
                                }],
                                result: None,
                            },
                        ],
                        els: vec![],
                        result: None,
                    }],
                    result: None,
                },
                setl("h", b(BinOp::And, b(BinOp::Add, getl("h"), i32c(1)), b(BinOp::Sub, getl("slots"), i32c(1)))),
                setl("attempt", b(BinOp::Add, getl("attempt"), i32c(1))),
                N::Br { target: "p".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_find_slice".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "p".into(), ty: WirTy::Str },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            "idx", "count", "i", "slots", "h", "h2", "attempt", "e",
        ].into_iter().map(|n| WirLocal { name: n.into(), ty: WirTy::Bool })
        .chain(std::iter::once(WirLocal { name: "hash".into(), ty: WirTy::Int }))
        .collect(),
        body: vec![
            setl("idx", load(b(BinOp::Sub, getl("d"), i32c(4)), 0)),
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("idx")) },
                then_: vec![
                    setl("count", load(getl("d"), 0)),
                    setl("i", i32c(0)),
                    linear,
                    N::Return(Some(i32c(-1))),
                ],
                els: vec![],
                result: None,
            },
            setl("slots", load(getl("idx"), 0)),
            setl("hash", E::Call { func: "dict_hash_slice".into(), args: vec![getl("p"), getl("len")] }),
            setl("h", b(BinOp::And, E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(getl("hash")) }, b(BinOp::Sub, getl("slots"), i32c(1)))),
            setl("h2", b(BinOp::And, E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(E::Binary { op: BinOp::ShrU, kind: Kind::I64, lhs: Box::new(getl("hash")), rhs: Box::new(E::ConstI64(57)) }) }, i32c(0x7f))),
            setl("attempt", i32c(0)),
            probe,
            N::Push(i32c(-1)),
        ],
        raw_body: None,
    }
}

/// `$dict_get_slice_or(d, p, len, default) -> i64`
pub(crate) fn dict_get_slice_or_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let entry = |idx: &str| b(BinOp::Add, getl("d"), b(BinOp::Mul, getl(idx), i32c(16)));
    let val_at = |idx: &str| E::Load { ptr: Box::new(entry(idx)), kind: Kind::I64, offset: 12 };

    WirFunc {
        name: "dict_get_slice_or".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "p".into(), ty: WirTy::Str },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "default".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Int],
        locals: vec![WirLocal { name: "found".into(), ty: WirTy::Bool }],
        body: vec![
            N::SetLocal {
                local: "found".into(),
                value: E::Call {
                    func: "dict_find_slice".into(),
                    args: vec![getl("d"), getl("p"), getl("len")],
                },
            },
            N::If {
                cond: b(BinOp::Ge, getl("found"), i32c(0)),
                then_: vec![N::Push(val_at("found"))],
                els: vec![N::Push(getl("default"))],
                result: Some(WirTy::Int),
            },
        ],
        raw_body: None,
    }
}

/// `$dict_contains_slice(d, p, len) -> bool`
pub(crate) fn dict_contains_slice_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };

    WirFunc {
        name: "dict_contains_slice".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "p".into(), ty: WirTy::Str },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![WirLocal { name: "found".into(), ty: WirTy::Bool }],
        body: vec![
            N::SetLocal {
                local: "found".into(),
                value: E::Call {
                    func: "dict_find_slice".into(),
                    args: vec![getl("d"), getl("p"), getl("len")],
                },
            },
            N::Push(b(BinOp::Ge, getl("found"), i32c(0))),
        ],
        raw_body: None,
    }
}
