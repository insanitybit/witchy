//! Dict runtime helpers: the open-addressed hash-table operations the compiled
//! backend lowers `Dict` to — key equality/hashing, find/insert/update/remove,
//! and the in-place `*_cap` variants. Split out of `wir_helpers/mod.rs`; the
//! parent re-exports these so consumers keep using `wir_helpers::dict_*`.

mod lookup;
mod read;

pub(crate) use lookup::*;
pub(crate) use read::*;

use crate::wir::*;

/// `$dict_index_put(idx, slots, e, k, v, mode)` — record that entry `e` lives
/// at key `k` in the Swiss control/index carrier `idx`. The carrier stores
/// `[slot_count][ctrl bytes + mirrored tail][entry indices][key slots][value slots][order]`; an index value is
/// `e+1`, while `0` means empty. The control byte stores H2, with `0x80` empty.
/// This is the maintenance side of [`dict_find_helper`]'s probe; keeping the index
/// current turns dict insert/lookup from the linear-scan fallback into O(1).
/// Void (like `$ensure`) so callers invoke it through `$Do` with no leftover
/// stack value. Calls `$dict_hash`.
pub(crate) fn dict_index_put_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let ctrl_base = || b(BinOp::Add, getl("idx"), i32c(4));
    let index_base = || b(BinOp::Add, ctrl_base(), b(BinOp::Add, getl("slots"), i32c(16)));
    let ctrl_ptr = || b(BinOp::Add, ctrl_base(), getl("h"));
    let index_ptr = || b(BinOp::Add, index_base(), b(BinOp::Mul, getl("h"), i32c(4)));
    let key_ptr = || b(BinOp::Add, index_base(), b(BinOp::Add, b(BinOp::Mul, getl("slots"), i32c(4)), b(BinOp::Mul, getl("h"), i32c(8))));
    let value_ptr = || b(BinOp::Add, index_base(), b(BinOp::Add, b(BinOp::Mul, getl("slots"), i32c(12)), b(BinOp::Mul, getl("h"), i32c(8))));
    let order_ptr = || b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), b(BinOp::Add, b(BinOp::Mul, getl("slots"), i32c(21)), b(BinOp::Mul, getl("e"), i32c(4)))));
    let store_and_exit = vec![
        N::Store8 { ptr: ctrl_ptr(), value: getl("h2"), offset: 0 },
        N::Store { ptr: index_ptr(), value: b(BinOp::Add, getl("e"), i32c(1)), kind: Kind::I32, offset: 0 },
        N::Store { ptr: key_ptr(), value: getl("k"), kind: Kind::I64, offset: 0 },
        N::Store { ptr: value_ptr(), value: getl("v"), kind: Kind::I64, offset: 0 },
        N::Store { ptr: order_ptr(), value: getl("h"), kind: Kind::I32, offset: 0 },
        N::If {
            cond: b(BinOp::Lt, getl("h"), i32c(16)),
            then_: vec![N::Store8 { ptr: b(BinOp::Add, ctrl_base(), b(BinOp::Add, getl("slots"), getl("h"))), value: getl("h2"), offset: 0 }],
            els: vec![],
            result: None,
        },
        N::Br { target: "done".into(), cond: None },
    ];
    let probe = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "p".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("attempt"), getl("slots"))) },
                N::If {
                    cond: b(BinOp::Eq, E::Load8U { ptr: Box::new(ctrl_ptr()), offset: 0 }, i32c(0x80)),
                    then_: store_and_exit,
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
        name: "dict_index_put".into(),
        params: vec![
            WirLocal { name: "idx".into(), ty: WirTy::Bool },
            WirLocal { name: "slots".into(), ty: WirTy::Bool },
            WirLocal { name: "e".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "v".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![],
        locals: vec![
            WirLocal { name: "h".into(), ty: WirTy::Bool },
            WirLocal { name: "h2".into(), ty: WirTy::Bool },
            WirLocal { name: "attempt".into(), ty: WirTy::Bool },
            WirLocal { name: "hash".into(), ty: WirTy::Int },
        ],
        body: vec![
            setl("hash", E::Call { func: "dict_hash".into(), args: vec![getl("k"), getl("mode")] }),
            setl("h", b(BinOp::And, E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(getl("hash")) }, b(BinOp::Sub, getl("slots"), i32c(1)))),
            setl("h2", b(BinOp::And, E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(E::Binary { op: BinOp::ShrU, kind: Kind::I64, lhs: Box::new(getl("hash")), rhs: Box::new(E::ConstI64(57)) }) }, i32c(0x7f))),
            setl("attempt", i32c(0)),
            probe,
        ],
        raw_body: None,
    }
}

/// Update the carrier-resident value for an existing dense entry. This is used
/// by in-place/persistent replacement paths so shared immutable carriers never
/// expose an older value.
pub(crate) fn dict_index_update_value_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let index_base = b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), getl("slots")));
    let value_base = b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), b(BinOp::Mul, getl("slots"), i32c(13))));
    WirFunc {
        name: "dict_index_update_value".into(),
        params: vec![
            WirLocal { name: "idx".into(), ty: WirTy::Bool },
            WirLocal { name: "slots".into(), ty: WirTy::Bool },
            WirLocal { name: "e".into(), ty: WirTy::Bool },
            WirLocal { name: "v".into(), ty: WirTy::Int },
        ],
        ret: vec![],
        locals: vec![WirLocal { name: "h".into(), ty: WirTy::Bool }],
        body: vec![
            setl("h", i32c(0)),
            N::Block {
                label: "done".into(),
                result: None,
                body: vec![N::Loop {
                    label: "scan".into(),
                    body: vec![
                        N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("h"), getl("slots"))) },
                        N::If {
                            cond: b(BinOp::Eq, E::Load { ptr: Box::new(b(BinOp::Add, index_base.clone(), b(BinOp::Mul, getl("h"), i32c(4)))), kind: Kind::I32, offset: 0 }, b(BinOp::Add, getl("e"), i32c(1))),
                            then_: vec![
                                N::Store { ptr: b(BinOp::Add, value_base.clone(), b(BinOp::Mul, getl("h"), i32c(8))), value: getl("v"), kind: Kind::I64, offset: 0 },
                                N::Br { target: "done".into(), cond: None },
                            ],
                            els: vec![],
                            result: None,
                        },
                        setl("h", b(BinOp::Add, getl("h"), i32c(1))),
                        N::Br { target: "scan".into(), cond: None },
                    ],
                }],
            },
        ],
        raw_body: None,
    }
}

/// `$dict_reindex(d, cap, mode)` — rebuild the hidden open-addressing index
/// after a structural copy or grow. Rehashing existing entries is maintenance,
/// not a second semantic lookup. Compound key modes keep the index disabled.
/// A one-entry transient root stays dense: delaying promotion until two live
/// entries prevents insert/remove churn from allocating an unreclaimable carrier
/// for every ephemeral root. `dict.with_capacity` still provisions its carrier
/// immediately, so explicitly reserved tables retain their fast first lookup.
pub(crate) fn dict_reindex_helper() -> WirFunc {
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
    let entry = |index: E| b(BinOp::Add, getl("d"), b(BinOp::Mul, index, i32c(16)));
    let allocate_index = || N::SetLocal {
        local: "idx".into(),
        value: E::Call {
            func: "bump_alloc".into(),
            args: vec![b(
                BinOp::Add,
                i32c(20),
                b(BinOp::Mul, getl("slots"), i32c(25)),
            )],
        },
    };
    WirFunc {
        name: "dict_reindex".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![],
        locals: ["count", "slots", "idx", "i"]
            .iter()
            .map(|name| WirLocal { name: (*name).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            N::SetLocal { local: "idx".into(), value: i32c(0) },
            N::SetLocal {
                local: "count".into(),
                value: E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 },
            },
            N::If {
                cond: E::GetGlobal("__witchy_extract_active".into()),
                then_: vec![N::SetGlobal { global: "__witchy_dict_rebuilds".into(), value: E::Binary { op: BinOp::Add, kind: Kind::I64, lhs: Box::new(E::GetGlobal("__witchy_dict_rebuilds".into())), rhs: Box::new(E::ConstI64(1)) } }],
                els: vec![],
                result: None,
            },
            N::If {
                cond: b(
                    BinOp::And,
                    b(BinOp::And, b(BinOp::Gt, getl("cap"), i32c(0)), b(BinOp::Gt, getl("count"), i32c(1))),
                    b(BinOp::Le, getl("mode"), i32c(2)),
                ),
                then_: vec![
                    N::SetLocal { local: "slots".into(), value: i32c(16) },
                    N::Block {
                        label: "size_done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "size_loop".into(),
                            body: vec![
                                N::Br {
                                    target: "size_done".into(),
                                    cond: Some(b(
                                        BinOp::Ge,
                                        getl("slots"),
                                        b(BinOp::Mul, getl("cap"), i32c(2)),
                                    )),
                                },
                                N::SetLocal {
                                    local: "slots".into(),
                                    value: b(BinOp::Mul, getl("slots"), i32c(2)),
                                },
                                N::Br { target: "size_loop".into(), cond: None },
                            ],
                        }],
                    },
                    allocate_index(),
                    N::If {
                        cond: E::GetGlobal("__witchy_extract_active".into()),
                        then_: vec![N::SetGlobal {
                            global: "__witchy_dict_index_bytes".into(),
                            value: E::Binary {
                                op: BinOp::Add,
                                kind: Kind::I64,
                                lhs: Box::new(E::GetGlobal("__witchy_dict_index_bytes".into())),
                                rhs: Box::new(E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(b(BinOp::Add, i32c(20), b(BinOp::Mul, getl("slots"), i32c(25)))) }),
                            },
                        }],
                        els: vec![],
                        result: None,
                    },
                    N::Store { ptr: getl("idx"), value: getl("slots"), kind: Kind::I32, offset: 0 },
                    N::MemoryFill {
                        dest: b(BinOp::Add, getl("idx"), i32c(4)),
                        value: i32c(0x80),
                        len: b(BinOp::Add, getl("slots"), i32c(16)),
                    },
                    N::MemoryFill {
                        dest: b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), getl("slots"))),
                        value: i32c(0),
                        len: b(BinOp::Mul, getl("slots"), i32c(4)),
                    },
                    N::MemoryFill {
                        dest: b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), b(BinOp::Mul, getl("slots"), i32c(5)))),
                        value: i32c(0),
                        len: b(BinOp::Mul, getl("slots"), i32c(8)),
                    },
                    N::MemoryFill {
                        dest: b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), b(BinOp::Mul, getl("slots"), i32c(13)))),
                        value: i32c(0),
                        len: b(BinOp::Mul, getl("slots"), i32c(8)),
                    },
                    N::MemoryFill {
                        dest: b(BinOp::Add, getl("idx"), b(BinOp::Add, i32c(20), b(BinOp::Mul, getl("slots"), i32c(21)))),
                        value: i32c(0),
                        len: b(BinOp::Mul, getl("slots"), i32c(4)),
                    },
                    N::Store {
                        ptr: b(BinOp::Sub, getl("d"), i32c(4)),
                        value: getl("idx"),
                        kind: Kind::I32,
                        offset: 0,
                    },
                    N::SetLocal { local: "i".into(), value: i32c(0) },
                    N::Block {
                        label: "index_done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "index_loop".into(),
                            body: vec![
                                N::Br {
                                    target: "index_done".into(),
                                    cond: Some(b(BinOp::Ge, getl("i"), getl("count"))),
                                },
                                N::Do(E::Call {
                                    func: "dict_index_put".into(),
                                    args: vec![
                                        getl("idx"),
                                        getl("slots"),
                                        getl("i"),
                                        E::Load { ptr: Box::new(entry(getl("i"))), kind: Kind::I64, offset: 4 },
                                        E::Load { ptr: Box::new(entry(getl("i"))), kind: Kind::I64, offset: 12 },
                                        getl("mode"),
                                    ],
                                }),
                                N::SetLocal {
                                    local: "i".into(),
                                    value: b(BinOp::Add, getl("i"), i32c(1)),
                                },
                                N::Br { target: "index_loop".into(), cond: None },
                            ],
                        }],
                    },
                ],
                els: vec![N::Store {
                    ptr: b(BinOp::Sub, getl("d"), i32c(4)),
                    value: i32c(0),
                    kind: Kind::I32,
                    offset: 0,
                }],
                result: None,
            },
        ],
        raw_body: None,
    }
}

/// `$dict_new() -> i32` — an empty dict: 8 reserved bytes holding a zero hidden
/// word (at p-4) and a zero count (at p), with `p` returned.
pub(crate) fn dict_new_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dict_new".into(),
        params: vec![],
        ret: vec![WirTy::Bool],
        locals: vec![WirLocal { name: "p".into(), ty: WirTy::Bool }],
        body: vec![
            // (RFC-0016) `$rc_alloc(8)` reserves the [size] header + the dict's 8-byte
            // region (the hidden index word + count); `p = rc_res + 4` keeps the index
            // word at `p-4` exactly as before. `$rc_free` of a dict frees `p-4`.
            N::SetLocal {
                local: "p".into(),
                value: b(BinOp::Add, E::Call { func: "rc_alloc".into(), args: vec![i32c(8)] }, i32c(4)),
            },
            N::Store { ptr: b(BinOp::Sub, getl("p"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store { ptr: getl("p"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Push(getl("p")),
        ],
        raw_body: None,
    }
}

/// `$dict_with_capacity(cap: i64) -> i32` — an empty dict with pre-allocated capacity `cap`:
/// 8 reserved bytes (hidden index word at p-4, count at p) plus 16 bytes per preallocated entry.
pub(crate) fn dict_with_capacity_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dict_with_capacity".into(),
        params: vec![WirLocal { name: "cap".into(), ty: WirTy::Int }],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "c".into(), ty: WirTy::Bool },
            WirLocal { name: "p".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "c".into(),
                value: E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(getl("cap")) },
            },
            N::If {
                cond: b(BinOp::Lt, getl("c"), i32c(0)),
                then_: vec![N::SetLocal { local: "c".into(), value: i32c(0) }],
                els: vec![],
                result: None,
            },
            N::SetLocal {
                local: "p".into(),
                value: b(
                    BinOp::Add,
                    E::Call {
                        func: "rc_alloc".into(),
                        args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("c"), i32c(16)))],
                    },
                    i32c(4),
                ),
            },
            N::Store { ptr: b(BinOp::Sub, getl("p"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store { ptr: getl("p"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Push(getl("p")),
        ],
        raw_body: None,
    }
}

/// `$dict_insert(d, k, v, mode) -> i32` — a fresh dict like `d` with `k` set to
/// `v`: the matching entry's value replaced, or `(k, v)` appended. Copies the
/// dense entry block; persistent roots remain dense and leave Swiss-carrier
/// promotion to the later owned/in-place path.
pub(crate) fn dict_insert_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    WirFunc {
        name: "dict_insert".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "v".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["count", "found", "new", "bytes", "idx", "slots", "newidx"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            setl("found", E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] }),
            setl("bytes", b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(16)))),
            // (RFC-0016) allocate the copy through `$rc_alloc` (header + reuse); the hidden
            // index word sits at `new-4` inside the rc region (new = rc_res + 4). Worst-case
            // `24 + count*16` (one extra entry for the not-found append); rc_alloc bumps $heap.
            setl("new", b(BinOp::Add, E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(24), b(BinOp::Mul, getl("count"), i32c(16)))] }, i32c(4))),
            N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::MemoryCopy { dest: getl("new"), src: getl("d"), len: getl("bytes") },
            // Persistent copies intentionally discard carrier metadata.  The
            // carrier is an acceleration structure for owned roots, not part of
            // the dense value representation.
            setl("idx", E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 }),
            // Persistent roots deliberately remain dense.  The carrier belongs
            // to an owned/in-place root; copying it here can retain bucket
            // metadata whose entry projection no longer matches the new root.
            // The next in-place mutation will promote/rebuild from the dense
            // entries when it has proven ownership.
            setl("idx", i32c(0)),
            N::If {
                cond: b(BinOp::And,
                    b(BinOp::Ne, getl("idx"), i32c(0)),
                    b(BinOp::Le, getl("mode"), i32c(2))),
                then_: vec![
                    setl("slots", E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 }),
                    N::If {
                    cond: b(BinOp::Ge, getl("found"), i32c(0)),
                        then_: vec![
                            // Replacement changes only the dense value slot. The
                            // Swiss carrier's key/index/control metadata is
                            // immutable and can be shared by the new RC root;
                            // reads resolve the bucket back to that dense slot.
                            N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: getl("idx"), kind: Kind::I32, offset: 0 },
                        ],
                        els: vec![
                            setl("newidx", E::Call { func: "bump_alloc".into(), args: vec![b(BinOp::Add, i32c(20), b(BinOp::Mul, getl("slots"), i32c(25)))] }),
                            N::MemoryCopy { dest: getl("newidx"), src: getl("idx"), len: b(BinOp::Add, i32c(20), b(BinOp::Mul, getl("slots"), i32c(25))) },
                            N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: getl("newidx"), kind: Kind::I32, offset: 0 },
                            N::Do(E::Call { func: "dict_index_put".into(), args: vec![getl("newidx"), getl("slots"), getl("count"), getl("k"), getl("v"), getl("mode")] }),
                        ],
                        result: None,
                    },
                ],
                els: vec![],
                result: None,
            },
            N::If {
                cond: b(BinOp::Ge, getl("found"), i32c(0)),
                then_: vec![
                    // replace value slot of the found entry: new + 12 + found*16.
                    N::Store {
                        ptr: b(BinOp::Add, getl("new"), b(BinOp::Mul, getl("found"), i32c(16))),
                        value: getl("v"),
                        kind: Kind::I64,
                        offset: 12,
                    },
                    N::Push(getl("new")),
                ],
                els: vec![
                    N::Store { ptr: getl("new"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                    N::Store { ptr: b(BinOp::Add, getl("new"), getl("bytes")), value: getl("k"), kind: Kind::I64, offset: 0 },
                    N::Store { ptr: b(BinOp::Add, getl("new"), getl("bytes")), value: getl("v"), kind: Kind::I64, offset: 8 },
                    N::If {
                        cond: b(BinOp::And, b(BinOp::Eq, getl("idx"), i32c(0)), b(BinOp::Le, getl("mode"), i32c(2))),
                        then_: vec![N::Do(E::Call { func: "dict_reindex".into(), args: vec![getl("new"), b(BinOp::Add, getl("count"), i32c(1)), getl("mode")] })],
                        els: vec![],
                        result: None,
                    },
                    N::Push(getl("new")),
                ],
                result: Some(WirTy::Bool),
            },
        ],
        raw_body: None,
    }
}

/// `$dict_insert_extract(d, k, v, mode, cap, key_bias, value_bias)
///     -> (dict, present, old-slot, cap)` — one semantic key search with an
/// ownership-aware replace/append path. Leaf transfer is delegated to the
/// generic `$slot_take_or_dup` helper.
pub(crate) fn dict_insert_extract_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let entry = |base: &str, index: E| b(BinOp::Add, getl(base), b(BinOp::Mul, index, i32c(16)));
    let dup_leaf = |value: E, bias: &str| E::Call {
        func: "leaf_dup".into(),
        args: vec![value, getl(bias)],
    };
    WirFunc {
        name: "dict_insert_extract".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "v".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
            WirLocal { name: "key_bias".into(), ty: WirTy::Bool },
            WirLocal { name: "value_bias".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool, WirTy::Int, WirTy::Bool],
        locals: ["count", "found", "present", "new", "newcap", "bytes", "out_cap", "i", "idx"]
            .iter()
            .map(|name| WirLocal { name: (*name).into(), ty: WirTy::Bool })
            .chain(std::iter::once(WirLocal { name: "old".into(), ty: WirTy::Int }))
            .collect(),
        body: vec![
            N::SetLocal { local: "count".into(), value: E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 } },
            N::SetGlobal {
                global: "__witchy_extract_active".into(),
                value: b(
                    BinOp::Add,
                    E::GetGlobal("__witchy_extract_active".into()),
                    i32c(1),
                ),
            },
            N::SetGlobal {
                global: "__witchy_extract_searches".into(),
                value: E::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(E::GetGlobal("__witchy_extract_searches".into())),
                    rhs: Box::new(i64c(1)),
                },
            },
            N::SetLocal { local: "found".into(), value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
            N::SetGlobal {
                global: "__witchy_extract_active".into(),
                value: b(
                    BinOp::Sub,
                    E::GetGlobal("__witchy_extract_active".into()),
                    i32c(1),
                ),
            },
            N::SetLocal { local: "present".into(), value: b(BinOp::Ge, getl("found"), i32c(0)) },
            N::SetLocal { local: "old".into(), value: i64c(0) },
            N::SetLocal { local: "bytes".into(), value: b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(16))) },
            N::If {
                cond: b(BinOp::Gt, getl("cap"), i32c(0)),
                then_: vec![
                    N::SetLocal { local: "new".into(), value: getl("d") },
                    N::SetLocal { local: "out_cap".into(), value: getl("cap") },
                    N::If {
                        cond: getl("present"),
                        then_: vec![
                            N::SetLocal {
                                local: "old".into(),
                                value: E::Call {
                                    func: "slot_take_or_dup".into(),
                                    args: vec![
                                        b(BinOp::Add, entry("d", getl("found")), i32c(12)),
                                        i32c(1),
                                        getl("value_bias"),
                                    ],
                                },
                            },
                            N::Store {
                                ptr: entry("d", getl("found")),
                                value: dup_leaf(getl("v"), "value_bias"),
                                kind: Kind::I64,
                                offset: 12,
                            },
                            N::SetLocal { local: "idx".into(), value: E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 } },
                            N::If {
                                cond: b(BinOp::And, b(BinOp::Ne, getl("idx"), i32c(0)), b(BinOp::Le, getl("mode"), i32c(2))),
                                then_: vec![N::Do(E::Call { func: "dict_index_update_value".into(), args: vec![getl("idx"), E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 }, getl("found"), getl("v")] })],
                                els: vec![],
                                result: None,
                            },
                        ],
                        els: vec![N::If {
                            cond: b(BinOp::Gt, getl("cap"), getl("count")),
                            then_: vec![
                                N::Store { ptr: getl("d"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                                N::Store { ptr: entry("d", getl("count")), value: dup_leaf(getl("k"), "key_bias"), kind: Kind::I64, offset: 4 },
                                N::Store { ptr: entry("d", getl("count")), value: dup_leaf(getl("v"), "value_bias"), kind: Kind::I64, offset: 12 },
                                N::SetLocal {
                                    local: "idx".into(),
                                    value: E::Load {
                                        ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))),
                                        kind: Kind::I32,
                                        offset: 0,
                                    },
                                },
                                N::If {
                                    cond: b(
                                        BinOp::And,
                                        b(BinOp::Ne, getl("idx"), i32c(0)),
                                        b(BinOp::Le, getl("mode"), i32c(2)),
                                    ),
                                    then_: vec![N::Do(E::Call {
                                        func: "dict_index_put".into(),
                                        args: vec![
                                            getl("idx"),
                                            E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 },
                                            getl("count"),
                                            getl("k"),
                                            getl("v"),
                                            getl("mode"),
                                        ],
                                    })],
                                    els: vec![N::If {
                                        cond: b(BinOp::Le, getl("mode"), i32c(2)),
                                        then_: vec![N::Do(E::Call { func: "dict_reindex".into(), args: vec![getl("d"), getl("cap"), getl("mode")] })],
                                        els: vec![],
                                        result: None,
                                    }],
                                    result: None,
                                },
                            ],
                            els: vec![
                                N::SetLocal {
                                    local: "newcap".into(),
                                    value: b(BinOp::Mul, b(BinOp::Add, getl("count"), i32c(1)), i32c(2)),
                                },
                                N::If {
                                    cond: b(BinOp::Lt, getl("newcap"), i32c(8)),
                                    then_: vec![N::SetLocal { local: "newcap".into(), value: i32c(8) }],
                                    els: vec![],
                                    result: None,
                                },
                                N::SetLocal {
                                    local: "new".into(),
                                    value: b(BinOp::Add, E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("newcap"), i32c(16)))] }, i32c(4)),
                                },
                                N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
                                N::MemoryCopy { dest: getl("new"), src: getl("d"), len: getl("bytes") },
                                N::SetGlobal {
                                    global: "__witchy_extract_copied_bytes".into(),
                                    value: E::Binary {
                                        op: BinOp::Add,
                                        kind: Kind::I64,
                                        lhs: Box::new(E::GetGlobal("__witchy_extract_copied_bytes".into())),
                                        rhs: Box::new(E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("bytes")) }),
                                    },
                                },
                                N::Store { ptr: getl("new"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                                N::Store { ptr: entry("new", getl("count")), value: dup_leaf(getl("k"), "key_bias"), kind: Kind::I64, offset: 4 },
                                N::Store { ptr: entry("new", getl("count")), value: dup_leaf(getl("v"), "value_bias"), kind: Kind::I64, offset: 12 },
                                N::SetLocal { local: "out_cap".into(), value: getl("newcap") },
                                N::Do(E::Call { func: "dict_reindex".into(), args: vec![getl("new"), getl("newcap"), getl("mode")] }),
                                N::Do(E::Call { func: "rc_free".into(), args: vec![b(BinOp::Sub, getl("d"), i32c(4))] }),
                            ],
                            result: None,
                        }],
                        result: None,
                    },
                ],
                els: vec![
                    N::SetLocal {
                        local: "newcap".into(),
                        value: b(BinOp::Mul, b(BinOp::Add, getl("count"), i32c(1)), i32c(2)),
                    },
                    N::If {
                        cond: b(BinOp::Lt, getl("newcap"), i32c(8)),
                        then_: vec![N::SetLocal { local: "newcap".into(), value: i32c(8) }],
                        els: vec![],
                        result: None,
                    },
                    N::SetLocal {
                        local: "new".into(),
                        value: b(BinOp::Add, E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("newcap"), i32c(16)))] }, i32c(4)),
                    },
                    N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
                    N::MemoryCopy { dest: getl("new"), src: getl("d"), len: getl("bytes") },
                    N::SetGlobal {
                        global: "__witchy_extract_copied_bytes".into(),
                        value: E::Binary {
                            op: BinOp::Add,
                            kind: Kind::I64,
                            lhs: Box::new(E::GetGlobal("__witchy_extract_copied_bytes".into())),
                            rhs: Box::new(E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("bytes")) }),
                        },
                    },
                    N::SetLocal { local: "i".into(), value: i32c(0) },
                    N::Block {
                        label: "dup_done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "dup_loop".into(),
                            body: vec![
                                N::Br { target: "dup_done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                                N::Drop(dup_leaf(E::Load { ptr: Box::new(entry("new", getl("i"))), kind: Kind::I64, offset: 4 }, "key_bias")),
                                N::Drop(dup_leaf(E::Load { ptr: Box::new(entry("new", getl("i"))), kind: Kind::I64, offset: 12 }, "value_bias")),
                                N::SetLocal { local: "i".into(), value: b(BinOp::Add, getl("i"), i32c(1)) },
                                N::Br { target: "dup_loop".into(), cond: None },
                            ],
                        }],
                    },
                    N::SetLocal { local: "out_cap".into(), value: getl("newcap") },
                    N::If {
                        cond: getl("present"),
                        then_: vec![
                            N::SetLocal {
                                local: "old".into(),
                                value: E::Call {
                                    func: "slot_take_or_dup".into(),
                                    args: vec![
                                        b(BinOp::Add, entry("d", getl("found")), i32c(12)),
                                        i32c(0),
                                        getl("value_bias"),
                                    ],
                                },
                            },
                            N::Do(E::Call {
                                func: "leaf_drop".into(),
                                args: vec![
                                    E::Load { ptr: Box::new(entry("new", getl("found"))), kind: Kind::I64, offset: 12 },
                                    getl("value_bias"),
                                ],
                            }),
                            N::Store { ptr: entry("new", getl("found")), value: dup_leaf(getl("v"), "value_bias"), kind: Kind::I64, offset: 12 },
                        ],
                        els: vec![
                            N::Store { ptr: getl("new"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                            N::Store { ptr: entry("new", getl("count")), value: dup_leaf(getl("k"), "key_bias"), kind: Kind::I64, offset: 4 },
                            N::Store { ptr: entry("new", getl("count")), value: dup_leaf(getl("v"), "value_bias"), kind: Kind::I64, offset: 12 },
                        ],
                        result: None,
                    },
                    N::Do(E::Call { func: "dict_reindex".into(), args: vec![getl("new"), getl("newcap"), getl("mode")] }),
                ],
                result: None,
            },
            N::Push(getl("new")),
            N::Push(getl("present")),
            N::Push(getl("old")),
            N::Push(getl("out_cap")),
        ],
        raw_body: None,
    }
}

/// `$dict_insert_cap(d, k, v, mode, cap) -> (i32, i32)` — the in-place dict upsert.
/// With owned entry slack (`cap`, the shadow-local capacity), an existing key
/// updates its value slot in place and a new key appends an entry (count+1),
/// returning `d` + `cap`; otherwise the table is copied once at double capacity.
/// Bumps `$__witchy_reowns` when entered with a zero cap (the re-own signal).
/// Scalar-key paths maintain the hidden Swiss carrier on append, replacement,
/// and grow; compound/custom equality modes leave it zero and retain the
/// dense linear path. The multi-value early `return`s are restructured into `ret_ptr`/`ret_cap`
/// locals + a dual tail Push (WIR has no multi-value If/Return). Calls `$dict_find`
/// + `$ensure`; uses `$heap` + `$__witchy_reowns`.
pub(super) fn dict_insert_cap_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let entry = |base: &str, idx: &str| b(BinOp::Add, getl(base), b(BinOp::Mul, getl(idx), i32c(16)));
    let reowns_bump = N::If {
        cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("cap")) },
        then_: vec![N::SetGlobal {
            global: "__witchy_reowns".into(),
            value: E::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(E::GetGlobal("__witchy_reowns".into())),
                rhs: Box::new(i64c(1)),
            },
        }],
        els: vec![],
        result: None,
    };
    // found >= 0 && cap > 0: overwrite the existing value slot in place.
    let update_inplace = vec![
        N::Store { ptr: entry("d", "found"), value: getl("v"), kind: Kind::I64, offset: 12 },
        N::SetLocal { local: "idx".into(), value: E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 } },
        N::If {
            cond: b(BinOp::And, b(BinOp::Ne, getl("idx"), i32c(0)), b(BinOp::Le, getl("mode"), i32c(2))),
            then_: vec![N::Do(E::Call { func: "dict_index_update_value".into(), args: vec![getl("idx"), E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 }, getl("found"), getl("v")] })],
            els: vec![],
            result: None,
        },
        N::SetLocal { local: "ret_ptr".into(), value: getl("d") },
        N::SetLocal { local: "ret_cap".into(), value: getl("cap") },
    ];
    // found < 0 && cap > count: append a fresh entry into the owned slack.
    // (RFC-0005 step 2) Bound the in-place append against the buffer's REAL allocated
    // size. A dict `d` is `rc_alloc(...) + 4` (the hidden index word sits at `d-4`), so
    // its rc size header is at `[d-8]` (low 24 bits). The new entry at index `count`
    // stores its value up to byte `d + count*16 + 20`; the block runs to `d-4 + size`,
    // so the write is in-bounds iff `count*16 + 24 <= size`. `cap > count` gates this
    // path on the analysis's CLAIMED capacity; if it overstates the real allocation the
    // append lands past the block (silent corruption) — trap instead. Sound: a real
    // buffer of capacity `cap` has `size = 8 + cap*16`, so `count < cap` implies
    // `count*16 + 24 <= size`, and `cap > count >= 0` means the check only ever runs on
    // a real heap buffer (`cap >= 1`).
    let append_inplace = vec![
        N::If {
            cond: b(
                BinOp::GtU,
                b(BinOp::Add, b(BinOp::Mul, getl("count"), i32c(16)), i32c(24)),
                b(BinOp::And, E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(8))), kind: Kind::I32, offset: 0 }, i32c(super::RC_SIZE_MASK)),
            ),
            then_: vec![N::Unreachable],
            els: vec![],
            result: None,
        },
        N::Store { ptr: entry("d", "count"), value: getl("k"), kind: Kind::I64, offset: 4 },
        N::Store { ptr: entry("d", "count"), value: getl("v"), kind: Kind::I64, offset: 12 },
        N::Store { ptr: getl("d"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
        // Record the new entry (index == the old `count`) in the hash index. The
        // index was built at the last grow sized ≥ 2× cap, so it has a free slot.
        N::SetLocal { local: "idx".into(), value: E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 } },
        N::If {
            cond: b(
                BinOp::And,
                b(BinOp::Ne, getl("idx"), i32c(0)),
                b(BinOp::Le, getl("mode"), i32c(2)),
            ),
            then_: vec![N::Do(E::Call {
                func: "dict_index_put".into(),
                args: vec![
                    getl("idx"),
                    E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 },
                    getl("count"),
                    getl("k"),
                    getl("v"),
                    getl("mode"),
                ],
            })],
            els: vec![N::If {
                cond: b(BinOp::Le, getl("mode"), i32c(2)),
                then_: vec![N::Do(E::Call { func: "dict_reindex".into(), args: vec![getl("d"), getl("cap"), getl("mode")] })],
                els: vec![],
                result: None,
            }],
            result: None,
        },
        N::SetLocal { local: "ret_ptr".into(), value: getl("d") },
        N::SetLocal { local: "ret_cap".into(), value: getl("cap") },
    ];
    // else: copy to a doubled buffer (index word reset to 0), then upsert.
    let grow = vec![
        N::If {
            cond: E::GetGlobal("__witchy_extract_active".into()),
            then_: vec![N::SetGlobal { global: "__witchy_dict_grows".into(), value: E::Binary { op: BinOp::Add, kind: Kind::I64, lhs: Box::new(E::GetGlobal("__witchy_dict_grows".into())), rhs: Box::new(E::ConstI64(1)) } }],
            els: vec![],
            result: None,
        },
        N::SetLocal {
            local: "newcap".into(),
            value: b(BinOp::Mul, b(BinOp::Add, getl("count"), i32c(1)), i32c(2)),
        },
        N::If {
            cond: b(BinOp::Lt, getl("newcap"), i32c(8)),
            then_: vec![N::SetLocal { local: "newcap".into(), value: i32c(8) }],
            els: vec![],
            result: None,
        },
        // (RFC-0016) grow buffer via rc_alloc (header + reuse); index word at new-4
        // inside the rc region (new = rc_res + 4). rc_alloc bumps $heap.
        N::SetLocal {
            local: "new".into(),
            value: b(BinOp::Add, E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("newcap"), i32c(16)))] }, i32c(4)),
        },
        N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
        N::SetLocal { local: "bytes".into(), value: b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(16))) },
        N::MemoryCopy { dest: getl("new"), src: getl("d"), len: getl("bytes") },
        N::If {
            cond: b(BinOp::Ge, getl("found"), i32c(0)),
            then_: vec![N::Store { ptr: entry("new", "found"), value: getl("v"), kind: Kind::I64, offset: 12 }],
            els: vec![
                N::Store { ptr: entry("new", "count"), value: getl("k"), kind: Kind::I64, offset: 4 },
                N::Store { ptr: entry("new", "count"), value: getl("v"), kind: Kind::I64, offset: 12 },
                N::Store { ptr: getl("new"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
            ],
            result: None,
        },
        N::Do(E::Call { func: "dict_reindex".into(), args: vec![getl("new"), getl("newcap"), getl("mode")] }),
        N::SetLocal { local: "ret_ptr".into(), value: getl("new") },
        N::SetLocal { local: "ret_cap".into(), value: getl("newcap") },
    ];
    WirFunc {
        name: "dict_insert_cap".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "v".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool],
        locals: ["count", "found", "new", "bytes", "newcap", "ret_ptr", "ret_cap", "idx", "islots", "icount", "iptr", "ie"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            reowns_bump,
            N::SetLocal { local: "count".into(), value: E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 } },
            N::SetLocal {
                local: "found".into(),
                value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] },
            },
            N::If {
                cond: b(BinOp::And, b(BinOp::Ge, getl("found"), i32c(0)), b(BinOp::Gt, getl("cap"), i32c(0))),
                then_: update_inplace,
                els: vec![N::If {
                    cond: b(BinOp::And, b(BinOp::Lt, getl("found"), i32c(0)), b(BinOp::Gt, getl("cap"), getl("count"))),
                    then_: append_inplace,
                    els: grow,
                    result: None,
                }],
                result: None,
            },
            N::Push(getl("ret_ptr")),
            N::Push(getl("ret_cap")),
        ],
        raw_body: None,
    }
}

/// `$dict_update_cap(d, k, default, mode, clos, cap) -> (i32, i32)` — the in-place
/// upsert: single-pass lookup via `$dict_find`. On an existing key with owned
/// capacity (`found >= 0 && cap > 0`), reads the value in place, invokes the
/// updater closure, and updates the value slot directly without re-hashing or
/// re-probing the table (adapting the `hashbrown` Entry pattern). If absent or
/// unowned, computes the new value and delegates once to `$dict_insert_cap`.
pub(crate) fn dict_update_cap_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let entry = |idx: &str| b(BinOp::Add, getl("d"), b(BinOp::Mul, getl(idx), i32c(16)));
    let val_at = |idx: &str| E::Load { ptr: Box::new(entry(idx)), kind: Kind::I64, offset: 12 };
    let call_clos = |arg: E| E::CallIndirect {
        signature: gc_slot_closure_signature(1, 1),
        args: vec![getl("clos"), arg],
        index: Box::new(E::StructGet {
            struct_id: 0,
            field: CLOSURE_CODE_FIELD,
            base: Box::new(getl("clos")),
        }),
    };
    WirFunc {
        name: "dict_update_cap".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "default".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
            WirLocal { name: "clos".into(), ty: WirTy::GcRef(0) },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool],
        locals: vec![
            WirLocal { name: "found".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Int },
            WirLocal { name: "idx".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_ptr".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_cap".into(), ty: WirTy::Bool },
        ],
        body: vec![
            setl("found", E::Call {
                func: "dict_find".into(),
                args: vec![getl("d"), getl("k"), getl("mode")],
            }),
            N::If {
                cond: b(BinOp::And, b(BinOp::Ge, getl("found"), i32c(0)), b(BinOp::Gt, getl("cap"), i32c(0))),
                then_: vec![
                    setl("new", call_clos(val_at("found"))),
                    N::Store {
                        ptr: entry("found"),
                        value: getl("new"),
                        kind: Kind::I64,
                        offset: 12,
                    },
                    setl("idx", E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 }),
                    N::If {
                        cond: b(BinOp::And, b(BinOp::Ne, getl("idx"), i32c(0)), b(BinOp::Le, getl("mode"), i32c(2))),
                        then_: vec![N::Do(E::Call { func: "dict_index_update_value".into(), args: vec![getl("idx"), E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 }, getl("found"), getl("new")] })],
                        els: vec![],
                        result: None,
                    },
                    setl("ret_ptr", getl("d")),
                    setl("ret_cap", getl("cap")),
                ],
                els: vec![
                    N::If {
                        cond: b(BinOp::Ge, getl("found"), i32c(0)),
                        then_: vec![setl("new", call_clos(val_at("found")))],
                        els: vec![setl("new", call_clos(getl("default")))],
                        result: None,
                    },
                    N::CallStoreMulti {
                        func: "dict_insert_cap".into(),
                        args: vec![getl("d"), getl("k"), getl("new"), getl("mode"), getl("cap")],
                        dests: vec!["ret_ptr".into(), "ret_cap".into()],
                    },
                ],
                result: None,
            },
            N::Push(getl("ret_ptr")),
            N::Push(getl("ret_cap")),
        ],
        raw_body: None,
    }
}

/// `$dict_update_slice_cap(d, p, len, default, clos, cap) -> (i32, i32)` — update dictionary
/// using a raw borrowed string slice `(p, len)` with zero allocations on hit.
pub(crate) fn dict_update_slice_cap_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let entry = |idx: &str| b(BinOp::Add, getl("d"), b(BinOp::Mul, getl(idx), i32c(16)));
    let val_at = |idx: &str| E::Load { ptr: Box::new(entry(idx)), kind: Kind::I64, offset: 12 };
    let call_clos = |arg: E| E::CallIndirect {
        signature: gc_slot_closure_signature(1, 1),
        args: vec![getl("clos"), arg],
        index: Box::new(E::StructGet {
            struct_id: 0,
            field: CLOSURE_CODE_FIELD,
            base: Box::new(getl("clos")),
        }),
    };
    WirFunc {
        name: "dict_update_slice_cap".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "p".into(), ty: WirTy::Str },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "default".into(), ty: WirTy::Int },
            WirLocal { name: "clos".into(), ty: WirTy::GcRef(0) },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool],
        locals: vec![
            WirLocal { name: "found".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Int },
            WirLocal { name: "k_str".into(), ty: WirTy::Str },
            WirLocal { name: "idx".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_ptr".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_cap".into(), ty: WirTy::Bool },
        ],
        body: vec![
            setl("found", E::Call {
                func: "dict_find_slice".into(),
                args: vec![getl("d"), getl("p"), getl("len")],
            }),
            N::If {
                cond: b(BinOp::And, b(BinOp::Ge, getl("found"), i32c(0)), b(BinOp::Gt, getl("cap"), i32c(0))),
                then_: vec![
                    setl("new", call_clos(val_at("found"))),
                    N::Store {
                        ptr: entry("found"),
                        value: getl("new"),
                        kind: Kind::I64,
                        offset: 12,
                    },
                    setl("idx", E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 }),
                    N::If {
                        cond: b(BinOp::Ne, getl("idx"), i32c(0)),
                        then_: vec![N::Do(E::Call { func: "dict_index_update_value".into(), args: vec![getl("idx"), E::Load { ptr: Box::new(getl("idx")), kind: Kind::I32, offset: 0 }, getl("found"), getl("new")] })],
                        els: vec![],
                        result: None,
                    },
                    setl("ret_ptr", getl("d")),
                    setl("ret_cap", getl("cap")),
                ],
                els: vec![
                    N::If {
                        cond: b(BinOp::Ge, getl("found"), i32c(0)),
                        then_: vec![setl("new", call_clos(val_at("found")))],
                        els: vec![setl("new", call_clos(getl("default")))],
                        result: None,
                    },
                    setl("k_str", E::Call {
                        func: "rc_alloc".into(),
                        args: vec![b(BinOp::Add, getl("len"), i32c(4))],
                    }),
                    N::Store {
                        ptr: getl("k_str"),
                        value: getl("len"),
                        kind: Kind::I32,
                        offset: 0,
                    },
                    N::MemoryCopy {
                        dest: b(BinOp::Add, getl("k_str"), i32c(4)),
                        src: getl("p"),
                        len: getl("len"),
                    },
                    N::CallStoreMulti {
                        func: "dict_insert_cap".into(),
                        args: vec![
                            getl("d"),
                            E::ToSlot(Box::new(getl("k_str")), Kind::I32),
                            getl("new"),
                            i32c(1),
                            getl("cap"),
                        ],
                        dests: vec!["ret_ptr".into(), "ret_cap".into()],
                    },
                ],
                result: None,
            },
            N::Push(getl("ret_ptr")),
            N::Push(getl("ret_cap")),
        ],
        raw_body: None,
    }
}

/// `$dict_update(d, k, default, mode, clos) -> i32` — apply the updater closure
/// to the current value (or `default` when absent) and reinsert.
pub(crate) fn dict_update_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let entry = |idx: &str| b(BinOp::Add, getl("d"), b(BinOp::Mul, getl(idx), i32c(16)));
    let val_at = |idx: &str| E::Load { ptr: Box::new(entry(idx)), kind: Kind::I64, offset: 12 };
    let call_clos = |arg: E| E::CallIndirect {
        signature: gc_slot_closure_signature(1, 1),
        args: vec![getl("clos"), arg],
        index: Box::new(E::StructGet {
            struct_id: 0,
            field: CLOSURE_CODE_FIELD,
            base: Box::new(getl("clos")),
        }),
    };
    WirFunc {
        name: "dict_update".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "default".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
            WirLocal { name: "clos".into(), ty: WirTy::GcRef(0) },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "found".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Int },
        ],
        body: vec![
            setl("found", E::Call {
                func: "dict_find".into(),
                args: vec![getl("d"), getl("k"), getl("mode")],
            }),
            N::If {
                cond: b(BinOp::Ge, getl("found"), i32c(0)),
                then_: vec![setl("new", call_clos(val_at("found")))],
                els: vec![setl("new", call_clos(getl("default")))],
                result: None,
            },
            N::Push(E::Call {
                func: "dict_insert".into(),
                args: vec![getl("d"), getl("k"), getl("new"), getl("mode")],
            }),
        ],
        raw_body: None,
    }
}

/// `$dict_remove(d, k, mode) -> i32` — a fresh dict with the entry for `k`
/// dropped (unchanged if absent). Copies every entry whose key isn't `k`.
pub(crate) fn dict_remove_helper() -> WirFunc {
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
    let dst = b(BinOp::Add, getl("new"), b(BinOp::Mul, getl("n"), i32c(16)));
    let scan = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                N::If {
                    cond: E::Unary {
                        op: UnOp::Not,
                        kind: Kind::I32,
                        arg: Box::new(E::Call { func: "key_eq".into(), args: vec![entry(4), getl("k"), getl("mode")] }),
                    },
                    then_: vec![
                        N::Store { ptr: dst.clone(), value: entry(4), kind: Kind::I64, offset: 4 },
                        N::Store { ptr: dst.clone(), value: entry(12), kind: Kind::I64, offset: 12 },
                        N::If {
                            cond: E::GetGlobal("__witchy_extract_active".into()),
                            then_: vec![N::SetGlobal {
                                global: "__witchy_dict_order_bytes_moved".into(),
                                value: E::Binary {
                                    op: BinOp::Add,
                                    kind: Kind::I64,
                                    lhs: Box::new(E::GetGlobal("__witchy_dict_order_bytes_moved".into())),
                                    rhs: Box::new(E::ConstI64(16)),
                                },
                            }],
                            els: vec![],
                            result: None,
                        },
                        setl("n", b(BinOp::Add, getl("n"), i32c(1))),
                    ],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_remove".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["count", "i", "new", "n"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            // (RFC-0016) allocate via rc_alloc (header + reuse); the hidden index word
            // sits at new-4 inside the rc region (new = rc_res + 4).
            setl("new", b(BinOp::Add, E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("count"), i32c(16)))] }, i32c(4))),
            N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            setl("n", i32c(0)),
            scan,
            N::Store { ptr: getl("new"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::Do(E::Call { func: "dict_reindex".into(), args: vec![getl("new"), getl("n"), getl("mode")] }),
            // `$rc_alloc` reserved the FULL `count`-slot capacity (the size arg above)
            // and already advanced `$heap`, so the count-n slack stays reserved for a
            // later in-place insert — no manual bump.
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$dict_remove_extract(d, k, mode, cap, key_bias, value_bias)
///     -> (dict, present, old-slot, cap)` — locate once, transfer or retain the
/// old value through `$slot_take_or_dup`, and repair insertion order.
pub(crate) fn dict_remove_extract_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let entry = |base: &str, index: E| b(BinOp::Add, getl(base), b(BinOp::Mul, index, i32c(16)));
    let dup_leaf = |value: E, bias: &str| E::Call {
        func: "leaf_dup".into(),
        args: vec![value, getl(bias)],
    };
    WirFunc {
        name: "dict_remove_extract".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
            WirLocal { name: "key_bias".into(), ty: WirTy::Bool },
            WirLocal { name: "value_bias".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool, WirTy::Int, WirTy::Bool],
        locals: ["count", "found", "present", "new", "prefix", "suffix", "out_cap", "i"]
            .iter()
            .map(|name| WirLocal { name: (*name).into(), ty: WirTy::Bool })
            .chain(std::iter::once(WirLocal { name: "old".into(), ty: WirTy::Int }))
            .collect(),
        body: vec![
            N::SetLocal { local: "count".into(), value: E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 } },
            N::SetGlobal {
                global: "__witchy_extract_active".into(),
                value: b(
                    BinOp::Add,
                    E::GetGlobal("__witchy_extract_active".into()),
                    i32c(1),
                ),
            },
            N::SetGlobal {
                global: "__witchy_extract_searches".into(),
                value: E::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(E::GetGlobal("__witchy_extract_searches".into())),
                    rhs: Box::new(i64c(1)),
                },
            },
            N::SetLocal { local: "found".into(), value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
            N::SetGlobal {
                global: "__witchy_extract_active".into(),
                value: b(
                    BinOp::Sub,
                    E::GetGlobal("__witchy_extract_active".into()),
                    i32c(1),
                ),
            },
            N::SetLocal { local: "present".into(), value: b(BinOp::Ge, getl("found"), i32c(0)) },
            N::SetLocal { local: "old".into(), value: i64c(0) },
            N::If {
                cond: b(BinOp::Gt, getl("cap"), i32c(0)),
                then_: vec![
                    N::SetLocal { local: "new".into(), value: getl("d") },
                    N::SetLocal { local: "out_cap".into(), value: getl("cap") },
                    N::If {
                        cond: getl("present"),
                        then_: vec![
                            N::SetLocal {
                                local: "old".into(),
                                value: E::Call {
                                    func: "slot_take_or_dup".into(),
                                    args: vec![
                                        b(BinOp::Add, entry("d", getl("found")), i32c(12)),
                                        i32c(1),
                                        getl("value_bias"),
                                    ],
                                },
                            },
                            N::Do(E::Call {
                                func: "leaf_drop".into(),
                                args: vec![
                                    E::Load { ptr: Box::new(entry("d", getl("found"))), kind: Kind::I64, offset: 4 },
                                    getl("key_bias"),
                                ],
                            }),
                            N::Store { ptr: entry("d", getl("found")), value: i64c(0), kind: Kind::I64, offset: 4 },
                            N::SetLocal { local: "suffix".into(), value: b(BinOp::Mul, b(BinOp::Sub, b(BinOp::Sub, getl("count"), getl("found")), i32c(1)), i32c(16)) },
                            N::MemoryCopy {
                                dest: b(BinOp::Add, entry("d", getl("found")), i32c(4)),
                                src: b(BinOp::Add, entry("d", b(BinOp::Add, getl("found"), i32c(1))), i32c(4)),
                                len: getl("suffix"),
                            },
                            N::Store {
                                ptr: entry("d", b(BinOp::Sub, getl("count"), i32c(1))),
                                value: i64c(0),
                                kind: Kind::I64,
                                offset: 4,
                            },
                            N::Store {
                                ptr: entry("d", b(BinOp::Sub, getl("count"), i32c(1))),
                                value: i64c(0),
                                kind: Kind::I64,
                                offset: 12,
                            },
                            N::Store { ptr: getl("d"), value: b(BinOp::Sub, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                        ],
                        els: vec![],
                        result: None,
                    },
                ],
                els: vec![N::If {
                    cond: getl("present"),
                    then_: vec![
                        N::SetLocal {
                            local: "new".into(),
                            value: b(BinOp::Add, E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("count"), i32c(16)))] }, i32c(4)),
                        },
                        N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
                        N::SetLocal {
                            local: "old".into(),
                            value: E::Call {
                                func: "slot_take_or_dup".into(),
                                args: vec![
                                    b(BinOp::Add, entry("d", getl("found")), i32c(12)),
                                    i32c(0),
                                    getl("value_bias"),
                                ],
                            },
                        },
                        N::SetLocal { local: "prefix".into(), value: b(BinOp::Mul, getl("found"), i32c(16)) },
                        N::SetLocal { local: "suffix".into(), value: b(BinOp::Mul, b(BinOp::Sub, b(BinOp::Sub, getl("count"), getl("found")), i32c(1)), i32c(16)) },
                        N::Store { ptr: getl("new"), value: b(BinOp::Sub, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                        N::MemoryCopy { dest: b(BinOp::Add, getl("new"), i32c(4)), src: b(BinOp::Add, getl("d"), i32c(4)), len: getl("prefix") },
                        N::MemoryCopy {
                            dest: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), getl("prefix")),
                            src: b(BinOp::Add, entry("d", b(BinOp::Add, getl("found"), i32c(1))), i32c(4)),
                            len: getl("suffix"),
                        },
                        N::SetGlobal {
                            global: "__witchy_extract_copied_bytes".into(),
                            value: E::Binary {
                                op: BinOp::Add,
                                kind: Kind::I64,
                                lhs: Box::new(E::GetGlobal("__witchy_extract_copied_bytes".into())),
                                rhs: Box::new(E::Convert {
                                    from: Kind::I32,
                                    to: Kind::I64,
                                    arg: Box::new(b(BinOp::Add, getl("prefix"), getl("suffix"))),
                                }),
                            },
                        },
                        N::SetLocal { local: "i".into(), value: i32c(0) },
                        N::Block {
                            label: "dup_done".into(),
                            result: None,
                            body: vec![N::Loop {
                                label: "dup_loop".into(),
                                body: vec![
                                    N::Br {
                                        target: "dup_done".into(),
                                        cond: Some(b(
                                            BinOp::Ge,
                                            getl("i"),
                                            b(BinOp::Sub, getl("count"), i32c(1)),
                                        )),
                                    },
                                    N::Drop(dup_leaf(E::Load { ptr: Box::new(entry("new", getl("i"))), kind: Kind::I64, offset: 4 }, "key_bias")),
                                    N::Drop(dup_leaf(E::Load { ptr: Box::new(entry("new", getl("i"))), kind: Kind::I64, offset: 12 }, "value_bias")),
                                    N::SetLocal { local: "i".into(), value: b(BinOp::Add, getl("i"), i32c(1)) },
                                    N::Br { target: "dup_loop".into(), cond: None },
                                ],
                            }],
                        },
                        N::SetLocal { local: "out_cap".into(), value: getl("count") },
                    ],
                    els: vec![
                        N::SetLocal { local: "new".into(), value: getl("d") },
                        N::SetLocal { local: "out_cap".into(), value: i32c(0) },
                    ],
                    result: None,
                }],
                result: None,
            },
            N::If {
                cond: getl("present"),
                then_: vec![N::Do(E::Call {
                    func: "dict_reindex".into(),
                    args: vec![getl("new"), getl("out_cap"), getl("mode")],
                })],
                els: vec![],
                result: None,
            },
            N::Push(getl("new")),
            N::Push(getl("present")),
            N::Push(getl("old")),
            N::Push(getl("out_cap")),
        ],
        raw_body: None,
    }
}
