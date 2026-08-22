//! Read-only dictionary access and projection helpers.

use super::super::abort_nodes;
use crate::wir::*;
use witchy_syntax::diag::DiagTemplate;

/// `$dict_get_or(d, k, default, mode) -> i64` — the value slot for `k`, or
/// `default` when absent.
pub(crate) fn dict_get_or_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let bucket_index = || E::Load {
        ptr: Box::new(b(
            BinOp::Add,
            getl("idx"),
            b(BinOp::Add, i32c(20), b(BinOp::Add, getl("slots"), b(BinOp::Mul, getl("bucket"), i32c(4)))),
        )),
        kind: Kind::I32,
        offset: 0,
    };
    let dense_value_at_bucket = || E::Load {
        ptr: Box::new(b(
            BinOp::Add,
            getl("d"),
            b(
                BinOp::Mul,
                b(
                    BinOp::Sub,
                    bucket_index(),
                    i32c(1),
                ),
                i32c(16),
            ),
        )),
        kind: Kind::I64,
        offset: 12,
    };
    WirFunc {
        name: "dict_get_or".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "default".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int],
        locals: vec![
            WirLocal { name: "found".into(), ty: WirTy::Bool },
            WirLocal { name: "idx".into(), ty: WirTy::Bool },
            WirLocal { name: "bucket".into(), ty: WirTy::Bool },
            WirLocal { name: "slots".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "idx".into(), value: E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 } },
            N::If {
                cond: b(BinOp::And, b(BinOp::Ne, getl("idx"), i32c(0)), b(BinOp::Le, getl("mode"), i32c(2))),
                then_: vec![
                    N::SetLocal { local: "slots".into(), value: E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 } },
                    N::SetLocal { local: "bucket".into(), value: E::Call { func: "dict_find_bucket".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
                    N::If {
                        cond: b(BinOp::Ge, getl("bucket"), i32c(0)),
                        then_: vec![N::Return(Some(dense_value_at_bucket()))],
                        els: vec![N::Return(Some(getl("default")))],
                        result: None,
                    },
                ],
                els: vec![
                    N::SetLocal { local: "found".into(), value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
                    N::If {
                        cond: b(BinOp::Lt, getl("found"), i32c(0)),
                        then_: vec![N::Return(Some(getl("default")))],
                        els: vec![],
                        result: None,
                    },
                    N::Return(Some(E::Load { ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("found"), i32c(16)))), kind: Kind::I64, offset: 12 })),
                ],
                result: None,
            },
            N::Push(getl("default")),
        ],
        raw_body: None,
    }
}

/// `$dict_at(d, k, mode) -> i64` — the value slot for `k`, or a routed runtime
/// error when absent. This is the compiled half of strict `d[k]` reads.
pub(in crate::wir_helpers) fn dict_at_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let bucket_index = || E::Load {
        ptr: Box::new(b(
            BinOp::Add,
            getl("idx"),
            b(BinOp::Add, i32c(20), b(BinOp::Add, getl("slots"), b(BinOp::Mul, getl("bucket"), i32c(4)))),
        )),
        kind: Kind::I32,
        offset: 0,
    };
    let dense_value_at_bucket = || E::Load {
        ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, b(BinOp::Sub, bucket_index(), i32c(1)), i32c(16)))),
        kind: Kind::I64,
        offset: 12,
    };
    WirFunc {
        name: "dict_at".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int],
        locals: vec![
            WirLocal { name: "found".into(), ty: WirTy::Bool },
            WirLocal { name: "idx".into(), ty: WirTy::Bool },
            WirLocal { name: "bucket".into(), ty: WirTy::Bool },
            WirLocal { name: "slots".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "idx".into(), value: E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 } },
            N::If {
                cond: b(BinOp::And, b(BinOp::Ne, getl("idx"), i32c(0)), b(BinOp::Le, getl("mode"), i32c(2))),
                then_: vec![
                    N::SetLocal { local: "slots".into(), value: E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 } },
                    N::SetLocal { local: "bucket".into(), value: E::Call { func: "dict_find_bucket".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
                    N::If {
                        cond: b(BinOp::Lt, getl("bucket"), i32c(0)),
                        then_: abort_nodes(DiagTemplate::DictMissing, i64c(0), i64c(0), i32c(0)),
                        els: vec![N::Return(Some(dense_value_at_bucket()))],
                        result: None,
                    },
                ],
                els: vec![
                    N::SetLocal { local: "found".into(), value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
                    N::If {
                        cond: b(BinOp::Lt, getl("found"), i32c(0)),
                        then_: abort_nodes(DiagTemplate::DictMissing, i64c(0), i64c(0), i32c(0)),
                        els: vec![],
                        result: None,
                    },
                    N::Return(Some(E::Load { ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("found"), i32c(16)))), kind: Kind::I64, offset: 12 })),
                ],
                result: None,
            },
            N::Unreachable,
        ],
        raw_body: None,
    }
}

/// `$dict_has(d, k, mode) -> i32` — whether `k` is present.
pub(crate) fn dict_has_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    WirFunc {
        name: "dict_has".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![N::Push(E::Binary {
            op: BinOp::Ge,
            kind: Kind::I32,
            lhs: Box::new(E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] }),
            rhs: Box::new(E::ConstI32(0)),
        })],
        raw_body: None,
    }
}

/// Shared body for `$dict_keys` / `$dict_values`: copy each entry's slot at
/// `entry_off` (4 = key, 12 = value) into a fresh `count`-element list.
pub(crate) fn dict_project_helper(name: &str, entry_off: u32) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let dense_src = E::Load {
        ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("i"), i32c(16)))),
        kind: Kind::I64,
        offset: entry_off,
    };
    let output_ptr = b(BinOp::Add, getl("new"), b(BinOp::Mul, getl("i"), i32c(8)));
    let scan = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                N::If {
                    cond: b(BinOp::Ne, getl("idx"), i32c(0)),
                    // The dense entry is the owning source of truth. The
                    // carrier's value lane is maintenance metadata and may be
                    // shared across persistent replacements.
                    then_: vec![N::Store { ptr: output_ptr.clone(), value: dense_src.clone(), kind: Kind::I64, offset: 4 }],
                    els: vec![N::Store { ptr: output_ptr, value: dense_src.clone(), kind: Kind::I64, offset: 4 }],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: name.into(),
        params: vec![WirLocal { name: "d".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: ["count", "i", "new", "idx", "slots"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            setl("idx", E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 }),
            N::If {
                cond: b(BinOp::Ne, getl("idx"), i32c(0)),
                then_: vec![setl("slots", E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 })],
                els: vec![],
                result: None,
            },
            // (RFC-0016) allocate the projected list through `$rc_alloc` (header + reuse).
            setl("new", E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(8)))] }),
            N::Store { ptr: getl("new"), value: getl("count"), kind: Kind::I32, offset: 0 },
            N::If {
                cond: E::GetGlobal("__witchy_extract_active".into()),
                then_: vec![N::SetGlobal {
                    global: "__witchy_dict_order_bytes_moved".into(),
                    value: E::Binary {
                        op: BinOp::Add,
                        kind: Kind::I64,
                        lhs: Box::new(E::GetGlobal("__witchy_dict_order_bytes_moved".into())),
                        rhs: Box::new(E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(b(BinOp::Mul, getl("count"), i32c(8))) }),
                    },
                }],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            scan,
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$dict_pairs(d) -> i32` — a `List((K, V))`: one `[0][key][value]` tuple per
/// entry (20 bytes: i32 tag + two i64 slots), with the list holding the tuple
/// pointers. Reserves the list slots first, then allocates tuples after it.
pub(in crate::wir_helpers) fn dict_pairs_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let entry = |off: u32| E::Load {
        ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("i"), i32c(16)))),
        kind: Kind::I64,
        offset: off,
    };
    let scan = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                // (RFC-0016) each pair tuple via `$rc_alloc` (20 bytes: [tag][key][value]).
                setl("tup", E::Call { func: "rc_alloc".into(), args: vec![i32c(20)] }),
                N::Store { ptr: getl("tup"), value: i32c(0), kind: Kind::I32, offset: 0 },
                N::If {
                    cond: b(BinOp::Ne, getl("idx"), i32c(0)),
                    then_: vec![
                        N::Store { ptr: getl("tup"), value: entry(4), kind: Kind::I64, offset: 4 },
                        N::Store { ptr: getl("tup"), value: entry(12), kind: Kind::I64, offset: 12 },
                    ],
                    els: vec![
                        N::Store { ptr: getl("tup"), value: entry(4), kind: Kind::I64, offset: 4 },
                        N::Store { ptr: getl("tup"), value: entry(12), kind: Kind::I64, offset: 12 },
                    ],
                    result: None,
                },
                // list slot i ← tuple pointer (zero-extended into the i64 slot).
                N::Store {
                    ptr: b(BinOp::Add, getl("list"), b(BinOp::Mul, getl("i"), i32c(8))),
                    value: E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("tup")) },
                    kind: Kind::I64,
                    offset: 4,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_pairs".into(),
        params: vec![WirLocal { name: "d".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: ["count", "i", "list", "tup", "idx", "slots"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            setl("idx", E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 }),
            N::If {
                cond: b(BinOp::Ne, getl("idx"), i32c(0)),
                then_: vec![setl("slots", E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 })],
                els: vec![],
                result: None,
            },
            // (RFC-0016) allocate the list through `$rc_alloc` (header + reuse); it bumps
            // `$heap` past the list, so each pair tuple's rc_alloc lands in a distinct block
            // above it and never overlaps a written slot.
            setl("list", E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(8)))] }),
            N::Store { ptr: getl("list"), value: getl("count"), kind: Kind::I32, offset: 0 },
            N::If {
                cond: E::GetGlobal("__witchy_extract_active".into()),
                then_: vec![N::SetGlobal {
                    global: "__witchy_dict_order_bytes_moved".into(),
                    value: E::Binary {
                        op: BinOp::Add,
                        kind: Kind::I64,
                        lhs: Box::new(E::GetGlobal("__witchy_dict_order_bytes_moved".into())),
                        rhs: Box::new(E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(b(BinOp::Mul, getl("count"), i32c(24))) }),
                    },
                }],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            scan,
            N::Push(getl("list")),
        ],
        raw_body: None,
    }
}
