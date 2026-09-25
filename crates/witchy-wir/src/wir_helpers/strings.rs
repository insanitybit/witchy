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
    let load_i32 = |p: E| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_length_read_place(p),
        kind: Kind::I32,
    };
    // A 16-byte SIMD read of the payload at `i`: a slice of sixteen stride-one
    // elements. Both operands have the same length on this path -- `str_eq` only
    // scans after the lengths matched -- so `len` is the count for either base.
    let load_v128 = |base: &str| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_payload_slice_place(
            getl(base), getl("i"), i32c(16), getl("len"), crate::safewir::AccessMode::Read,
        ),
        kind: Kind::V128,
    };
    let byte_at = |base: &str| E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl(base), getl("i"), getl("len"), crate::safewir::AccessMode::Read,
        ),
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
                        N::SetLocal { local: "v1".into(), value: load_v128("a") },
                        N::SetLocal { local: "v2".into(), value: load_v128("b") },
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
    let load_i32 = |p: E| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_length_read_place(p),
        kind: Kind::I32,
    };
    // Unlike `str_eq`, the two operands here may differ in length, so each
    // claim carries ITS OWN count -- `alen` for `a`, `blen` for `b`. The closures
    // take the count name for that reason; a single shared count would claim one
    // operand against the other's extent.
    let load_v128 = |base: &str, count: &str| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_payload_slice_place(
            getl(base), getl("i"), i32c(16), getl(count), crate::safewir::AccessMode::Read,
        ),
        kind: Kind::V128,
    };
    let byte_at = |base: &str, count: &str| E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl(base), getl("i"), getl(count), crate::safewir::AccessMode::Read,
        ),
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
                N::SetLocal { local: "va".into(), value: load_v128("a", "alen") },
                N::SetLocal { local: "vb".into(), value: load_v128("b", "blen") },
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
                        N::Return(Some(bin(BinOp::Sub, byte_at("a", "alen"), byte_at("b", "blen")))),
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
                N::SetLocal { local: "ca".into(), value: byte_at("a", "alen") },
                N::SetLocal { local: "cb".into(), value: byte_at("b", "blen") },
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
    let load = |p: E| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_length_read_place(p),
        kind: Kind::I32,
    };
    let load_v128 = |base: &str| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_payload_slice_place(
            getl(base), getl("i"), i32c(16), getl("slen"), crate::safewir::AccessMode::Read,
        ),
        kind: Kind::V128,
    };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // `s`'s byte sits at the COMPUTED index `i + j`, and a claim's index must be a
    // leaf, so the sum is bound to `k` at the top of the compare loop. The scan
    // guard is `i > slen - sublen` and the compare guard is `j >= sublen`, so
    // `i + j < slen` and `slen` -- `s`'s own length -- is the honest count.
    let s_byte = E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl("s"),
            getl("k"),
            getl("slen"),
            crate::safewir::AccessMode::Read,
        ),
    };
    let sub_byte = E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl("sub"),
            getl("j"),
            getl("sublen"),
            crate::safewir::AccessMode::Read,
        ),
    };
    let cmp_loop = N::Block {
        label: "cmpdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cmp".into(),
            body: vec![
                N::Br { target: "cmpdone".into(), cond: Some(b(BinOp::Ge, getl("j"), getl("sublen"))) },
                setl("k", b(BinOp::Add, getl("i"), getl("j"))),
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
        setl("needle", E::SafeLoad8U { place: crate::wir_helpers::layouts::byte_buffer_element_place(getl("sub"), i32c(0), getl("sublen"), crate::safewir::AccessMode::Read) }),
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
                    setl("v", load_v128("s")),
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
                        cond: b(BinOp::Eq, E::SafeLoad8U { place: crate::wir_helpers::layouts::byte_buffer_element_place(getl("s"), getl("i"), getl("slen"), crate::safewir::AccessMode::Read) }, getl("needle")),
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
            WirLocal { name: "k".into(), ty: WirTy::Bool },
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
    // A length read claims only the descriptor's header region: reading the word
    // is what ESTABLISHES the count, so a counted extent would be circular.
    let load = |p: E| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_length_read_place(p),
        kind: Kind::I32,
    };
    // A SIMD lane compare reads a sixteen-byte SLICE of each payload. `slen` and
    // `plen` bound their own object -- claiming `plen` for `s` would be a claim
    // about the wrong object, which is why `s`'s length is bound below rather
    // than borrowed. The loop guard is `i + 16 > plen` and the prologue has
    // established `plen <= slen`, so a sixteen-byte claim at `i` is honest for
    // both.
    let vec_lane = |root: &str, count: &str| {
        crate::wir_helpers::layouts::byte_buffer_payload_slice_place(
            getl(root),
            getl("i"),
            i32c(16),
            getl(count),
            crate::safewir::AccessMode::Read,
        )
    };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl("s"),
            getl("i"),
            getl("slen"),
            crate::safewir::AccessMode::Read,
        ),
    };
    let p_byte = E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl("p"),
            getl("i"),
            getl("plen"),
            crate::safewir::AccessMode::Read,
        ),
    };
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
                setl("vs", E::SafeLoad { place: vec_lane("s", "slen"), kind: Kind::V128 }),
                setl("vp", E::SafeLoad { place: vec_lane("p", "plen"), kind: Kind::V128 }),
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
            WirLocal { name: "slen".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
            WirLocal { name: "vs".into(), ty: WirTy::V128 },
            WirLocal { name: "vp".into(), ty: WirTy::V128 },
        ],
        body: vec![
            setl("plen", load(getl("p"))),
            // Bound rather than read inline: a claim operand must be a leaf the
            // validator can bind to a reaching definition, and every `s` access
            // below needs `s`'s own count.
            setl("slen", load(getl("s"))),
            N::If {
                cond: b(BinOp::Gt, getl("plen"), getl("slen")),
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
    let load = |p: E| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_length_read_place(p),
        kind: Kind::I32,
    };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // Every `s` access sits at the COMPUTED index `off + i`, and a claim's index
    // must be a leaf, so the sum is bound to `k` at the top of each loop. `s` is
    // claimed against `slen` -- its OWN length, bound below rather than borrowed
    // from `p`'s -- and `p` against `plen`. Both loops stop at `i >= plen`, and
    // `off = slen - plen`, so `off + i < slen` and `off + i + 16 <= slen` hold.
    let vec_lane = |root: &str, index: E, count: &str| {
        crate::wir_helpers::layouts::byte_buffer_payload_slice_place(
            getl(root), index, i32c(16), getl(count), crate::safewir::AccessMode::Read,
        )
    };
    let s_byte = E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl("s"),
            getl("k"),
            getl("slen"),
            crate::safewir::AccessMode::Read,
        ),
    };
    let p_byte = E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl("p"),
            getl("i"),
            getl("plen"),
            crate::safewir::AccessMode::Read,
        ),
    };
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
                setl("k", b(BinOp::Add, getl("off"), getl("i"))),
                setl("vs", E::SafeLoad { place: vec_lane("s", getl("k"), "slen"), kind: Kind::V128 }),
                setl("vp", E::SafeLoad { place: vec_lane("p", getl("i"), "plen"), kind: Kind::V128 }),
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
                setl("k", b(BinOp::Add, getl("off"), getl("i"))),
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
            WirLocal { name: "slen".into(), ty: WirTy::Bool },
            WirLocal { name: "off".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Bool },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
            WirLocal { name: "vs".into(), ty: WirTy::V128 },
            WirLocal { name: "vp".into(), ty: WirTy::V128 },
        ],
        body: vec![
            setl("plen", load(getl("p"))),
            // Bound rather than read inline: every `s` access below claims `s`'s own
            // count, and a claim operand must be a leaf.
            setl("slen", load(getl("s"))),
            setl("off", b(BinOp::Sub, getl("slen"), getl("plen"))),
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
                E::SafeLoad { place: crate::wir_helpers::layouts::byte_buffer_length_read_place(E::GetLocal("s".into())), kind: Kind::I32 },
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
    let byte = E::SafeLoad8U { place: crate::wir_helpers::layouts::byte_buffer_element_place(getl("s"), getl("i"), getl("bytelen"), crate::safewir::AccessMode::Read) };
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
/// `$char_to_byte_from(s, start_byte, n) -> i32` — the byte offset within `s`
/// after advancing `n` characters from `start_byte`. Clamps `n <= 0` to `start_byte`
/// and advances up to byte-length. Includes 8-byte word acceleration for ASCII runs.
pub(crate) fn char_to_byte_from_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let ext = |e: E| E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_length_read_place(p),
        kind: Kind::I32,
    };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };

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
                // 8-byte ASCII word fast path: if remaining bytes >= 8 and remaining chars >= 8
                N::If {
                    cond: E::Binary {
                        op: BinOp::And,
                        kind: Kind::I32,
                        lhs: Box::new(b(BinOp::Le, b(BinOp::Add, getl("i"), i32c(8)), getl("slen"))),
                        rhs: Box::new(b64(BinOp::Le, b64(BinOp::Add, ext(getl("count")), i64c(8)), getl("n"))),
                    },
                    then_: vec![
                        setl("w", E::SafeLoad { place: crate::wir_helpers::layouts::byte_buffer_payload_slice_place(getl("s"), getl("i"), i32c(8), getl("slen"), crate::safewir::AccessMode::Read), kind: Kind::I64 }),
                        N::If {
                            cond: b64(BinOp::Eq, b64(BinOp::And, getl("w"), i64c(0x8080808080808080u64 as i64)), i64c(0)),
                            then_: vec![
                                setl("i", b(BinOp::Add, getl("i"), i32c(8))),
                                setl("count", b(BinOp::Add, getl("count"), i32c(8))),
                                N::Br { target: "l".into(), cond: None },
                            ],
                            els: vec![],
                            result: None,
                        },
                    ],
                    els: vec![],
                    result: None,
                },
                setl("b", E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl("s"),
            getl("i"),
            getl("slen"),
            crate::safewir::AccessMode::Read,
        ),
    }),
                seqlen,
                setl("i", b(BinOp::Add, getl("i"), getl("seqlen"))),
                setl("count", b(BinOp::Add, getl("count"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };

    WirFunc {
        name: "char_to_byte_from".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "start_byte".into(), ty: WirTy::Bool },
            WirLocal { name: "n".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "slen".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "count".into(), ty: WirTy::Bool },
            WirLocal { name: "b".into(), ty: WirTy::Bool },
            WirLocal { name: "seqlen".into(), ty: WirTy::Bool },
            WirLocal { name: "w".into(), ty: WirTy::Int },
        ],
        body: vec![
            setl("slen", load(getl("s"))),
            setl("i", getl("start_byte")),
            N::If {
                cond: b(BinOp::Lt, getl("i"), i32c(0)),
                then_: vec![setl("i", i32c(0))],
                els: vec![N::If {
                    cond: b(BinOp::Gt, getl("i"), getl("slen")),
                    then_: vec![setl("i", getl("slen"))],
                    els: vec![],
                    result: None,
                }],
                result: None,
            },
            setl("count", i32c(0)),
            N::If {
                cond: b64(BinOp::Le, getl("n"), i64c(0)),
                then_: vec![N::Return(Some(getl("i")))],
                els: vec![],
                result: None,
            },
            scan_loop,
            N::Push(getl("i")),
        ],
        raw_body: None,
    }
}

/// `$char_to_byte(s: i32, n: i64) -> i32` — the byte offset of the `n`th character.
pub(crate) fn char_to_byte_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    WirFunc {
        name: "char_to_byte".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "n".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![
            N::Push(E::Call {
                func: "char_to_byte_from".into(),
                args: vec![getl("s"), i32c(0), getl("n")],
            }),
        ],
        raw_body: None,
    }
}

/// `$str_find_byte(s: i32, target: i32) -> i32` — SIMD 16-way accelerated byte search (RFC-0144).
/// Returns 0-based byte index of first occurrence of `target` in `s`, or -1 if not found.
pub(in crate::wir_helpers) fn str_find_byte_helper() -> WirFunc {
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
    let load_i32 = |p: E| E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_length_read_place(p),
        kind: Kind::I32,
    };
    let load_v128 = || E::SafeLoad {
        place: crate::wir_helpers::layouts::byte_buffer_payload_slice_place(
            getl("s"), getl("i"), i32c(16), getl("len"), crate::safewir::AccessMode::Read,
        ),
        kind: Kind::V128,
    };
    let byte_at = || E::SafeLoad8U {
        place: crate::wir_helpers::layouts::byte_buffer_element_place(
            getl("s"), getl("i"), getl("len"), crate::safewir::AccessMode::Read,
        ),
    };

    WirFunc {
        name: "str_find_byte".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "target".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "v".into(), ty: WirTy::V128 },
            WirLocal { name: "target_vec".into(), ty: WirTy::V128 },
            WirLocal { name: "mask".into(), ty: WirTy::Bool },
            WirLocal { name: "b".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: load_i32(getl("s")) },
            N::SetLocal { local: "i".into(), value: i32c(0) },
            N::SetLocal {
                local: "target_vec".into(),
                value: E::Vector {
                    op: VectorOp::I8x16Splat,
                    args: vec![getl("target")],
                },
            },
            // Vector loop: 16 bytes per cycle
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
                        N::SetLocal { local: "v".into(), value: load_v128() },
                        N::SetLocal {
                            local: "mask".into(),
                            value: E::Vector {
                                op: VectorOp::I8x16Bitmask,
                                args: vec![E::Vector {
                                    op: VectorOp::I8x16Eq,
                                    args: vec![getl("v"), getl("target_vec")],
                                }],
                            },
                        },
                        N::If {
                            cond: bin(BinOp::Ne, getl("mask"), i32c(0)),
                            then_: vec![
                                N::Return(Some(bin(
                                    BinOp::Add,
                                    getl("i"),
                                    E::Unary {
                                        op: UnOp::Ctz,
                                        kind: Kind::I32,
                                        arg: Box::new(getl("mask")),
                                    },
                                ))),
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
            },
            // Scalar tail
            N::Block {
                label: "sdone".into(),
                result: None,
                body: vec![N::Loop {
                    label: "sl".into(),
                    body: vec![
                        N::Br {
                            target: "sdone".into(),
                            cond: Some(bin(BinOp::Ge, getl("i"), getl("len"))),
                        },
                        N::If {
                            cond: bin(BinOp::Eq, byte_at(), getl("target")),
                            then_: vec![N::Return(Some(getl("i")))],
                            els: vec![],
                            result: None,
                        },
                        N::SetLocal {
                            local: "i".into(),
                            value: bin(BinOp::Add, getl("i"), i32c(1)),
                        },
                        N::Br { target: "sl".into(), cond: None },
                    ],
                }],
            },
            N::Push(i32c(-1)),
        ],
        raw_body: None,
    }
}

/// `$str_slice_fast(s: i32, start: i64, end: i64) -> i32` — byte-indexed O(1) substring slice (RFC-0144).
/// Clamps `start` and `end` to valid byte bounds in O(1) without iterating characters, then calls `$substr`.
pub(in crate::wir_helpers) fn str_slice_fast_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let b64 = |op: BinOp, l: E, r: E| E::Binary {
        op,
        kind: Kind::I64,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let ext = |e: E| E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
    let nar = |e: E| E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(e) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };

    WirFunc {
        name: "str_slice_fast".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "start".into(), ty: WirTy::Int },
            WirLocal { name: "end".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "slen".into(), ty: WirTy::Bool },
            WirLocal { name: "lo".into(), ty: WirTy::Bool },
            WirLocal { name: "hi".into(), ty: WirTy::Bool },
        ],
        body: vec![
            setl("slen", E::SafeLoad {
                    place: crate::wir_helpers::layouts::byte_buffer_length_read_place(getl("s")),
                    kind: Kind::I32,
                }),
            // lo = clamp(start, 0, slen)
            setl("lo", nar(getl("start"))),
            N::If {
                cond: b64(BinOp::Lt, getl("start"), E::ConstI64(0)),
                then_: vec![setl("lo", i32c(0))],
                els: vec![N::If {
                    cond: b64(BinOp::Gt, getl("start"), ext(getl("slen"))),
                    then_: vec![setl("lo", getl("slen"))],
                    els: vec![],
                    result: None,
                }],
                result: None,
            },
            // hi = clamp(end, lo, slen)
            setl("hi", nar(getl("end"))),
            N::If {
                cond: b64(BinOp::Lt, getl("end"), ext(getl("lo"))),
                then_: vec![setl("hi", getl("lo"))],
                els: vec![N::If {
                    cond: b64(BinOp::Gt, getl("end"), ext(getl("slen"))),
                    then_: vec![setl("hi", getl("slen"))],
                    els: vec![],
                    result: None,
                }],
                result: None,
            },
            N::If {
                cond: b(BinOp::Ge, getl("lo"), getl("hi")),
                then_: vec![N::Push(E::Call {
                    func: "substr".into(),
                    args: vec![getl("s"), i32c(0), i32c(0)],
                })],
                els: vec![N::Push(E::Call {
                    func: "substr".into(),
                    args: vec![getl("s"), getl("lo"), b(BinOp::Sub, getl("hi"), getl("lo"))],
                })],
                result: Some(WirTy::Str),
            },
        ],
        raw_body: None,
    }
}

/// `$str_slice_view(s, start, end) -> (root, start, len, ptr)` — byte-indexed
/// borrowed view. The final pointer is retained only for existing direct-view
/// consumers. Ownership-taking consumers must use the first three values,
/// which preserve the source allocation identity and the checked payload range.
/// Unlike `str_slice_fast` this helper never allocates or constructs an owning
/// string header.
pub(crate) fn str_slice_view_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let get = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let ext = |e: E| E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
    let nar = |e: E| E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(e) };
    let set = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    WirFunc {
        name: "str_slice_view".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "start".into(), ty: WirTy::Int },
            WirLocal { name: "end".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool, WirTy::Bool, WirTy::Bool],
        locals: vec![
            WirLocal { name: "slen".into(), ty: WirTy::Bool },
            WirLocal { name: "lo".into(), ty: WirTy::Bool },
            WirLocal { name: "hi".into(), ty: WirTy::Bool },
        ],
        body: vec![
            set("slen", E::SafeLoad { place: crate::wir_helpers::layouts::byte_buffer_length_read_place(get("s")), kind: Kind::I32 }),
            set("lo", nar(get("start"))),
            N::If { cond: b64(BinOp::Lt, get("start"), E::ConstI64(0)), then_: vec![set("lo", i32c(0))], els: vec![N::If { cond: b64(BinOp::Gt, get("start"), ext(get("slen"))), then_: vec![set("lo", get("slen"))], els: vec![], result: None }], result: None },
            set("hi", nar(get("end"))),
            N::If { cond: b64(BinOp::Lt, get("end"), ext(get("lo"))), then_: vec![set("hi", get("lo"))], els: vec![N::If { cond: b64(BinOp::Gt, get("end"), ext(get("slen"))), then_: vec![set("hi", get("slen"))], els: vec![], result: None }], result: None },
            N::Push(get("s")),
            N::Push(get("lo")),
            N::Push(b(BinOp::Sub, get("hi"), get("lo"))),
            N::Push(b(BinOp::Add, b(BinOp::Add, get("s"), i32c(4)), get("lo"))),
        ],
        raw_body: None,
    }
}

/// `$str_view_to_string(root, start, len) -> String` — materialize an owning
/// string from an authenticated borrowed range when an API explicitly requests
/// ownership. `root` remains the source allocation identity; `start` and `len`
/// are carried from `str_slice_view`, rather than reconstructed from an interior
/// payload address.
pub(crate) fn str_view_to_string_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let get = |n: &str| E::GetLocal(n.into());
    let add = |l, r| E::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "str_view_to_string".into(),
        params: vec![
            WirLocal { name: "root".into(), ty: WirTy::Str },
            WirLocal { name: "start".into(), ty: WirTy::Bool },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "out".into(), ty: WirTy::Bool },
            // Establish the dynamic extent of the source object before
            // claiming its payload slice. The validator binds this local to
            // the exact SafeLoad reaching definition.
            WirLocal { name: "root_len".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "root_len".into(),
                value: E::SafeLoad {
                    place: crate::wir_helpers::layouts::byte_buffer_length_read_place(get("root")),
                    kind: Kind::I32,
                },
            },
            N::SetLocal { local: "out".into(), value: E::Call { func: "rc_alloc".into(), args: vec![add(get("len"), E::ConstI32(4))] } },
            N::SafeStore {
                place: crate::wir_helpers::layouts::byte_buffer_length_place(get("out"), get("len")),
                value: get("len"),
                kind: Kind::I32,
            },
            N::SafeMemoryCopy {
                kind: crate::safewir_surface::SafeCopyKind::Disjoint,
                destination: crate::wir_helpers::layouts::byte_buffer_payload_slice_place(
                    get("out"), E::ConstI32(0), get("len"), get("len"), crate::safewir::AccessMode::Write,
                ),
                source: crate::wir_helpers::layouts::byte_buffer_payload_slice_place(
                    get("root"), get("start"), get("len"), get("root_len"), crate::safewir::AccessMode::Read,
                ),
                len: get("len"),
            },
            N::Push(get("out")),
        ],
        raw_body: None,
    }
}
