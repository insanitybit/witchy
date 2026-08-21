//! String comparison, byte/character indexing, and search helpers.

mod host;
mod transform;

pub(super) use host::*;
pub(crate) use transform::*;

use crate::wir::*;

/// `$str_eq(a: i32, b: i32) -> i32` — content equality of two `[len][bytes]`
/// strings: same pointer → 1; different length → 0; else compare bytes. Mirrors
/// `STR_EQ_WAT`. Byte reads via `Load8U`; no heap/import/table.
pub(crate) fn str_eq_helper() -> WirFunc {
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
    let ptr_at = |base: &str| bin(BinOp::Add, bin(BinOp::Add, getl(base), i32c(4)), getl("i"));
    let byte_at = |base: &str| E::Load8U {
        ptr: Box::new(ptr_at(base)),
        offset: 0,
    };
    WirFunc {
        name: "str_eq".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Str },
            WirLocal { name: "b".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool], // i32
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "v1".into(), ty: WirTy::V128 },
            WirLocal { name: "v2".into(), ty: WirTy::V128 },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
        ],
        body: vec![
            // same pointer → equal
            N::If {
                cond: bin(BinOp::Eq, getl("a"), getl("b")),
                then_: vec![N::Return(Some(i32c(1)))],
                els: vec![],
                result: None,
            },
            // different length → not equal
            N::If {
                cond: bin(BinOp::Ne, load_i32(getl("a")), load_i32(getl("b"))),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "len".into(), value: load_i32(getl("a")) },
            N::SetLocal { local: "i".into(), value: i32c(0) },
            // Vector loop: 16 bytes per iteration (RFC-0140)
            N::Block {
                label: "vdone".into(),
                result: None,
                body: vec![N::Loop {
                    label: "vl".into(),
                    body: vec![
                        N::Br {
                            target: "vdone".into(),
                            cond: Some(bin(BinOp::Gt, bin(BinOp::Add, getl("i"), i32c(16)), getl("len"))),
                        },
                        N::SetLocal { local: "v1".into(), value: load_v128(ptr_at("a")) },
                        N::SetLocal { local: "v2".into(), value: load_v128(ptr_at("b")) },
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
                        N::SetLocal {
                            local: "i".into(),
                            value: bin(BinOp::Add, getl("i"), i32c(16)),
                        },
                        N::Br { target: "vl".into(), cond: None },
                    ],
                }],
            },
            // Scalar tail loop (< 16 remaining bytes)
            N::Block {
                label: "done".into(),
                result: None,
                body: vec![N::Loop {
                    label: "l".into(),
                    body: vec![
                        N::Br {
                            target: "done".into(),
                            cond: Some(bin(BinOp::Ge, getl("i"), getl("len"))),
                        },
                        N::If {
                            cond: bin(BinOp::Ne, byte_at("a"), byte_at("b")),
                            then_: vec![N::Return(Some(i32c(0)))],
                            els: vec![],
                            result: None,
                        },
                        N::SetLocal {
                            local: "i".into(),
                            value: bin(BinOp::Add, getl("i"), i32c(1)),
                        },
                        N::Br { target: "l".into(), cond: None },
                    ],
                }],
            },
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$str_cmp(a: i32, b: i32) -> i32` — byte-lexicographic comparison of two
/// `[len][bytes]` strings: negative if `a < b`, zero if equal, positive if
/// `a > b`. Compares up to the shorter length, then breaks ties by length.
/// Vectorized with 16-byte SIMD chunk comparisons and CTZ mismatch discovery.
pub(crate) fn str_cmp_helper() -> WirFunc {
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
    let ptr_at = |base: &str| bin(BinOp::Add, bin(BinOp::Add, getl(base), i32c(4)), getl("i"));
    let byte_at = |base: &str| E::Load8U {
        ptr: Box::new(ptr_at(base)),
        offset: 0,
    };
    let vec_loop = N::Block {
        label: "vdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "vl".into(),
            body: vec![
                N::Br {
                    target: "vdone".into(),
                    cond: Some(bin(BinOp::Gt, bin(BinOp::Add, getl("i"), i32c(16)), getl("n"))),
                },
                N::SetLocal { local: "va".into(), value: load_v128(ptr_at("a")) },
                N::SetLocal { local: "vb".into(), value: load_v128(ptr_at("b")) },
                N::SetLocal {
                    local: "mask".into(),
                    value: E::Vector {
                        op: VectorOp::I8x16Bitmask,
                        args: vec![E::Vector {
                            op: VectorOp::I8x16Eq,
                            args: vec![getl("va"), getl("vb")],
                        }],
                    },
                },
                N::If {
                    cond: bin(BinOp::Ne, getl("mask"), i32c(0xffff)),
                    then_: vec![
                        N::SetLocal {
                            local: "diff_pos".into(),
                            value: E::Unary {
                                op: UnOp::Ctz,
                                kind: Kind::I32,
                                arg: Box::new(bin(BinOp::Xor, getl("mask"), i32c(0xffff))),
                            },
                        },
                        N::SetLocal {
                            local: "i".into(),
                            value: bin(BinOp::Add, getl("i"), getl("diff_pos")),
                        },
                        N::Return(Some(bin(BinOp::Sub, byte_at("a"), byte_at("b")))),
                    ],
                    els: vec![],
                    result: None,
                },
                N::SetLocal {
                    local: "i".into(),
                    value: bin(BinOp::Add, getl("i"), i32c(16)),
                },
                N::Br { target: "vl".into(), cond: None },
            ],
        }],
    };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br {
                    target: "done".into(),
                    cond: Some(bin(BinOp::Ge, getl("i"), getl("n"))),
                },
                N::SetLocal { local: "ca".into(), value: byte_at("a") },
                N::SetLocal { local: "cb".into(), value: byte_at("b") },
                N::If {
                    cond: bin(BinOp::Ne, getl("ca"), getl("cb")),
                    then_: vec![N::Return(Some(bin(BinOp::Sub, getl("ca"), getl("cb"))))],
                    els: vec![],
                    result: None,
                },
                N::SetLocal {
                    local: "i".into(),
                    value: bin(BinOp::Add, getl("i"), i32c(1)),
                },
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "str_cmp".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Str },
            WirLocal { name: "b".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool], // i32
        locals: vec![
            WirLocal { name: "alen".into(), ty: WirTy::Bool },
            WirLocal { name: "blen".into(), ty: WirTy::Bool },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "ca".into(), ty: WirTy::Bool },
            WirLocal { name: "cb".into(), ty: WirTy::Bool },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
            WirLocal { name: "diff_pos".into(), ty: WirTy::Bool },
            WirLocal { name: "va".into(), ty: WirTy::V128 },
            WirLocal { name: "vb".into(), ty: WirTy::V128 },
        ],
        body: vec![
            N::SetLocal { local: "alen".into(), value: load_i32(getl("a")) },
            N::SetLocal { local: "blen".into(), value: load_i32(getl("b")) },
            // n = min(alen, blen)
            N::SetLocal { local: "n".into(), value: getl("blen") },
            N::If {
                cond: bin(BinOp::Lt, getl("alen"), getl("blen")),
                then_: vec![N::SetLocal { local: "n".into(), value: getl("alen") }],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "i".into(), value: i32c(0) },
            vec_loop,
            scan_loop,
            N::Push(bin(BinOp::Sub, getl("alen"), getl("blen"))),
        ],
        raw_body: None,
    }
}

/// `$find_byte(s: i32, sub: i32) -> i32` — index of the first occurrence of
/// `sub` in `s` (byte-wise), or `-1`; empty `sub` → 0. Vectorized for single-byte
/// searches with 16-way SIMD bitmask scanning (RFC-0140).
pub(crate) fn find_byte_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let load_v128 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::V128, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("i"), getl("j")))), offset: 4 };
    let sub_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("sub"), getl("j"))), offset: 4 };
    let cmp_loop = N::Block {
        label: "cmpdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cmp".into(),
            body: vec![
                N::Br { target: "cmpdone".into(), cond: Some(b(BinOp::Ge, getl("j"), getl("sublen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, sub_byte),
                    then_: vec![setl("match", i32c(0)), N::Br { target: "cmpdone".into(), cond: None }],
                    els: vec![],
                    result: None,
                },
                setl("j", b(BinOp::Add, getl("j"), i32c(1))),
                N::Br { target: "cmp".into(), cond: None },
            ],
        }],
    };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "scan".into(),
            body: vec![
                N::Br {
                    target: "done".into(),
                    cond: Some(b(BinOp::Gt, getl("i"), b(BinOp::Sub, getl("slen"), getl("sublen")))),
                },
                setl("match", i32c(1)),
                setl("j", i32c(0)),
                cmp_loop,
                N::If { cond: getl("match"), then_: vec![N::Return(Some(getl("i")))], els: vec![], result: None },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "scan".into(), cond: None },
            ],
        }],
    };
    // Single-byte accelerated SIMD needle search
    let single_byte_branch = vec![
        setl("needle", E::Load8U { ptr: Box::new(b(BinOp::Add, getl("sub"), i32c(4))), offset: 0 }),
        setl("target_splat", E::Vector {
            op: VectorOp::I8x16Splat,
            args: vec![getl("needle")],
        }),
        // 16-byte SIMD search loop
        N::Block {
            label: "vdone".into(),
            result: None,
            body: vec![N::Loop {
                label: "vl".into(),
                body: vec![
                    N::Br {
                        target: "vdone".into(),
                        cond: Some(b(BinOp::Gt, b(BinOp::Add, getl("i"), i32c(16)), getl("slen"))),
                    },
                    setl("v", load_v128(b(BinOp::Add, b(BinOp::Add, getl("s"), i32c(4)), getl("i")))),
                    setl("mask", E::Vector {
                        op: VectorOp::I8x16Bitmask,
                        args: vec![E::Vector {
                            op: VectorOp::I8x16Eq,
                            args: vec![getl("v"), getl("target_splat")],
                        }],
                    }),
                    N::If {
                        cond: getl("mask"),
                        then_: vec![
                            N::Return(Some(b(
                                BinOp::Add,
                                getl("i"),
                                E::Unary { op: UnOp::Ctz, kind: Kind::I32, arg: Box::new(getl("mask")) },
                            ))),
                        ],
                        els: vec![],
                        result: None,
                    },
                    setl("i", b(BinOp::Add, getl("i"), i32c(16))),
                    N::Br { target: "vl".into(), cond: None },
                ],
            }],
        },
        // Scalar tail loop for remaining bytes
        N::Block {
            label: "tdone".into(),
            result: None,
            body: vec![N::Loop {
                label: "tl".into(),
                body: vec![
                    N::Br {
                        target: "tdone".into(),
                        cond: Some(b(BinOp::Ge, getl("i"), getl("slen"))),
                    },
                    N::If {
                        cond: b(BinOp::Eq, E::Load8U { ptr: Box::new(b(BinOp::Add, b(BinOp::Add, getl("s"), i32c(4)), getl("i"))), offset: 0 }, getl("needle")),
                        then_: vec![N::Return(Some(getl("i")))],
                        els: vec![],
                        result: None,
                    },
                    setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                    N::Br { target: "tl".into(), cond: None },
                ],
            }],
        },
        N::Return(Some(i32c(-1))),
    ];
    WirFunc {
        name: "find_byte".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "sub".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "slen".into(), ty: WirTy::Bool },
            WirLocal { name: "sublen".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "j".into(), ty: WirTy::Bool },
            WirLocal { name: "match".into(), ty: WirTy::Bool },
            WirLocal { name: "needle".into(), ty: WirTy::Bool },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
            WirLocal { name: "v".into(), ty: WirTy::V128 },
            WirLocal { name: "target_splat".into(), ty: WirTy::V128 },
        ],
        body: vec![
            setl("slen", load(getl("s"))),
            setl("sublen", load(getl("sub"))),
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("sublen")) },
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            N::If {
                cond: b(BinOp::Eq, getl("sublen"), i32c(1)),
                then_: single_byte_branch,
                els: vec![],
                result: None,
            },
            scan_loop,
            N::Push(i32c(-1)),
        ],
        raw_body: None,
    }
}

/// `$starts_with(s, p) -> i32` — 1 iff string `s` begins with prefix `p`.
/// Vectorized with 16-byte SIMD chunk comparisons.
pub(crate) fn starts_with_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let load_v128 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::V128, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 };
    let p_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("p"), getl("i"))), offset: 4 };
    let vec_loop = N::Block {
        label: "vdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "vl".into(),
            body: vec![
                N::Br {
                    target: "vdone".into(),
                    cond: Some(b(BinOp::Gt, b(BinOp::Add, getl("i"), i32c(16)), getl("plen"))),
                },
                setl("vs", load_v128(b(BinOp::Add, b(BinOp::Add, getl("s"), i32c(4)), getl("i")))),
                setl("vp", load_v128(b(BinOp::Add, b(BinOp::Add, getl("p"), i32c(4)), getl("i")))),
                setl(
                    "mask",
                    E::Vector {
                        op: VectorOp::I8x16Bitmask,
                        args: vec![E::Vector {
                            op: VectorOp::I8x16Eq,
                            args: vec![getl("vs"), getl("vp")],
                        }],
                    },
                ),
                N::If {
                    cond: b(BinOp::Ne, getl("mask"), i32c(0xffff)),
                    then_: vec![N::Return(Some(i32c(0)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(16))),
                N::Br { target: "vl".into(), cond: None },
            ],
        }],
    };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("plen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, p_byte),
                    then_: vec![N::Return(Some(i32c(0)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "starts_with".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "p".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "plen".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
            WirLocal { name: "vs".into(), ty: WirTy::V128 },
            WirLocal { name: "vp".into(), ty: WirTy::V128 },
        ],
        body: vec![
            setl("plen", load(getl("p"))),
            N::If {
                cond: b(BinOp::Gt, getl("plen"), load(getl("s"))),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            vec_loop,
            scan_loop,
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$ends_with(s, p) -> i32` — 1 iff string `s` ends with suffix `p`.
/// Vectorized with 16-byte SIMD chunk comparisons.
pub(crate) fn ends_with_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let load_v128 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::V128, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U {
        ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("off"), getl("i")))),
        offset: 4,
    };
    let p_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("p"), getl("i"))), offset: 4 };
    let vec_loop = N::Block {
        label: "vdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "vl".into(),
            body: vec![
                N::Br {
                    target: "vdone".into(),
                    cond: Some(b(BinOp::Gt, b(BinOp::Add, getl("i"), i32c(16)), getl("plen"))),
                },
                setl("vs", load_v128(b(BinOp::Add, b(BinOp::Add, b(BinOp::Add, getl("s"), i32c(4)), getl("off")), getl("i")))),
                setl("vp", load_v128(b(BinOp::Add, b(BinOp::Add, getl("p"), i32c(4)), getl("i")))),
                setl(
                    "mask",
                    E::Vector {
                        op: VectorOp::I8x16Bitmask,
                        args: vec![E::Vector {
                            op: VectorOp::I8x16Eq,
                            args: vec![getl("vs"), getl("vp")],
                        }],
                    },
                ),
                N::If {
                    cond: b(BinOp::Ne, getl("mask"), i32c(0xffff)),
                    then_: vec![N::Return(Some(i32c(0)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(16))),
                N::Br { target: "vl".into(), cond: None },
            ],
        }],
    };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("plen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, p_byte),
                    then_: vec![N::Return(Some(i32c(0)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "ends_with".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "p".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "plen".into(), ty: WirTy::Bool },
            WirLocal { name: "off".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
            WirLocal { name: "vs".into(), ty: WirTy::V128 },
            WirLocal { name: "vp".into(), ty: WirTy::V128 },
        ],
        body: vec![
            setl("plen", load(getl("p"))),
            setl("off", b(BinOp::Sub, load(getl("s")), getl("plen"))),
            N::If {
                cond: b(BinOp::Lt, getl("off"), i32c(0)),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            vec_loop,
            scan_loop,
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$byte_to_char(s, bytelen) -> i32` — the count of UTF-8 *characters* in the
/// first `bytelen` bytes of `s`. Continuation bytes (`0b10xxxxxx`) don't start a
/// character, so they're skipped; every other byte increments the count.
/// `$char_count(s: i32) -> i32` — the number of Unicode scalars in `s`: just
/// `byte_to_char(s, len(s))`, reading the byte-length header itself so the caller
/// evaluates `s` once. Mirrors the `string.char_count` legacy emission.
pub(super) fn char_count_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    WirFunc {
        name: "char_count".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Bool], // i32
        locals: vec![],
        body: vec![N::Push(E::Call {
            func: "byte_to_char".into(),
            args: vec![
                E::GetLocal("s".into()),
                E::Load { ptr: Box::new(E::GetLocal("s".into())), kind: Kind::I32, offset: 0 },
            ],
        })],
        raw_body: None,
    }
}

pub(crate) fn byte_to_char_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("bytelen"))) },
                setl("b", byte),
                N::If {
                    cond: b(BinOp::Ne, b(BinOp::And, getl("b"), i32c(0xc0)), i32c(0x80)),
                    then_: vec![setl("count", b(BinOp::Add, getl("count"), i32c(1)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "byte_to_char".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "bytelen".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["i", "count", "b"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![setl("i", i32c(0)), setl("count", i32c(0)), scan_loop, N::Push(getl("count"))],
        raw_body: None,
    }
}

/// `$str_index_of(s, sub) -> i32` — the *character* index where `sub` first
/// occurs in `s`, or -1 if absent. `$find_byte` gives the byte offset; this maps
/// it back to a character index via `$byte_to_char`.
pub(crate) fn str_index_of_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    WirFunc {
        name: "str_index_of".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "sub".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![WirLocal { name: "bidx".into(), ty: WirTy::Bool }],
        body: vec![
            setl("bidx", E::Call { func: "find_byte".into(), args: vec![getl("s"), getl("sub")] }),
            N::If {
                cond: b(BinOp::Lt, getl("bidx"), i32c(0)),
                then_: vec![N::Push(i32c(-1))],
                els: vec![N::Push(E::Call {
                    func: "byte_to_char".into(),
                    args: vec![getl("s"), getl("bidx")],
                })],
                result: Some(WirTy::Bool),
            },
        ],
        raw_body: None,
    }
}

/// `$char_to_byte(s, n) -> i32` — the *byte* offset of the `n`-th character of
/// `s` (the inverse of `$byte_to_char`). Walks UTF-8 sequences, stepping the byte
/// cursor by 1/2/3/4 per character based on the lead byte, until `n` chars (or
/// the end) are consumed.
///
/// `n` is a *full-width i64* char index, and the walk clamps it to `[0, char_count]`
/// implicitly: a negative `n` stops the loop immediately (byte offset 0) and any `n`
/// beyond the last character runs the cursor to the byte length. The `count >= n`
/// guard is therefore compared in i64, so a huge index near the i64 extremes can't
/// wrap when narrowed — that was BUG-011, where the compiled backend narrowed the
/// index to i32 *before* clamping, so a large `end` wrapped to `< start` and yielded
/// `""` while the interpreter clamped in i64 and returned the whole string.
pub(crate) fn char_to_byte_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    // i64 sign-extend + i64 compare, for the full-width `count >= n` clamp guard.
    let ext = |e: E| E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // seqlen = b<0x80 ? 1 : b<0xe0 ? 2 : b<0xf0 ? 3 : 4 — nested if-statements
    // setting the `seqlen` local (avoids an expression-level conditional).
    let seqlen = N::If {
        cond: b(BinOp::LtU, getl("b"), i32c(0x80)),
        then_: vec![setl("seqlen", i32c(1))],
        els: vec![N::If {
            cond: b(BinOp::LtU, getl("b"), i32c(0xe0)),
            then_: vec![setl("seqlen", i32c(2))],
            els: vec![N::If {
                cond: b(BinOp::LtU, getl("b"), i32c(0xf0)),
                then_: vec![setl("seqlen", i32c(3))],
                els: vec![setl("seqlen", i32c(4))],
                result: None,
            }],
            result: None,
        }],
        result: None,
    };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("slen"))) },
                N::Br { target: "done".into(), cond: Some(b64(BinOp::Ge, ext(getl("count")), getl("n"))) },
                setl("b", E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 }),
                seqlen,
                setl("i", b(BinOp::Add, getl("i"), getl("seqlen"))),
                setl("count", b(BinOp::Add, getl("count"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "char_to_byte".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "n".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Bool],
        locals: ["slen", "i", "count", "b", "seqlen"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("slen", load(getl("s"))),
            setl("i", i32c(0)),
            setl("count", i32c(0)),
            scan_loop,
            N::Push(getl("i")),
        ],
        raw_body: None,
    }
}
