//! Dict runtime helpers: the open-addressed hash-table operations the compiled
//! backend lowers `Dict` to — key equality/hashing, find/insert/update/remove,
//! and the in-place `*_cap` variants. Split out of `wir_helpers/mod.rs`; the
//! parent re-exports these so consumers keep using `wir_helpers::dict_*`.

use crate::wir::*;
use super::abort_nodes;
use witchy_syntax::diag::DiagTemplate;

/// `$key_eq(a, b, mode) -> i32` — slot equality under the key's compile-time
/// type: mode 0 = raw i64 (Int/Bool), 1 = `$str_eq` on the pointers (String),
/// else f64 (the slots reinterpreted as doubles).
pub fn key_eq_helper() -> WirFunc {
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

/// `$dict_hash(k, mode) -> i32` — a 64-bit bit-mix for scalar keys (mode 0),
/// FNV-1a over the bytes for string keys (mode 1, `k` = string pointer). Only
/// consulted by `$dict_find`'s (binary-path-dormant) hash probe.
pub fn dict_hash_helper() -> WirFunc {
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
        ret: vec![WirTy::Bool],
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
                    N::Return(Some(E::FromSlot(Box::new(getl("x")), Kind::I32))),
                ],
                els: vec![],
                result: None,
            },
            setl("p", E::FromSlot(Box::new(getl("k")), Kind::I32)),
            setl("len", E::Load { ptr: Box::new(getl("p")), kind: Kind::I32, offset: 0 }),
            setl("x", i64c(-7046029254386353131i64)), // 0x9e3779b97f4a7c15 (golden-ratio seed)
            setl("i", i32c(0)),
            word_loop,
            tail_loop,
            // Fold in the length, then a final avalanche so the low bits the
            // open-addressing index masks (`& (slots-1)`) are well mixed.
            setl("x", b64(BinOp::Xor, getl("x"), E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("len")) })),
            setl("x", b64(BinOp::Mul, b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(32))), i64c(-4265267296055464877i64))),
            setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(29)))),
            N::Push(E::FromSlot(Box::new(getl("x")), Kind::I32)),
        ],
        raw_body: None,
    }
}

/// `$dict_find(d, k, mode) -> i32` — the entry index of key `k`, or -1. Linear
/// scan when the hidden index word is 0 (always, on the binary path); otherwise
/// an open-addressing probe over the hash table.
pub fn dict_find_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E, off: u32| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: off };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // key slot of entry `e`: d + 4 + e*16.
    let key_at = |e: E| E::Load { ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, e, i32c(16)))), kind: Kind::I64, offset: 4 };
    let keq = |e: E| E::Call { func: "key_eq".into(), args: vec![key_at(e), getl("k"), getl("mode")] };
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
    // slot value at index table position h: idx + 4 + h*4.
    let slot_at_h = load(b(BinOp::Add, b(BinOp::Add, getl("idx"), i32c(4)), b(BinOp::Mul, getl("h"), i32c(4))), 0);
    let probe = N::Block {
        label: "miss".into(),
        result: None,
        body: vec![N::Loop {
            label: "p".into(),
            body: vec![
                setl("e", slot_at_h),
                N::Br { target: "miss".into(), cond: Some(E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("e")) }) },
                N::If {
                    cond: keq(b(BinOp::Sub, getl("e"), i32c(1))),
                    then_: vec![N::Return(Some(b(BinOp::Sub, getl("e"), i32c(1))))],
                    els: vec![],
                    result: None,
                },
                setl("h", b(BinOp::And, b(BinOp::Add, getl("h"), i32c(1)), b(BinOp::Sub, getl("slots"), i32c(1)))),
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
        locals: ["idx", "count", "i", "slots", "h", "e"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
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
            setl("h", b(BinOp::And, E::Call { func: "dict_hash".into(), args: vec![getl("k"), getl("mode")] }, b(BinOp::Sub, getl("slots"), i32c(1)))),
            probe,
            N::Push(i32c(-1)),
        ],
        raw_body: None,
    }
}

/// `$dict_index_put(idx, slots, e, k, mode) -> i32` — record that entry `e` lives
/// at key `k` in the open-addressing hash index `idx` (slot holds `e+1`, with `0`
/// meaning empty) by probing from `hash(k) & (slots-1)`. The index is always sized
/// to ≥ 2× the dict's entry capacity, so it is never more than half full and an
/// empty slot is always reached — the probe cannot loop forever. The returned i32
/// is unused (a uniform value-returning helper so callers can invoke it via `$Do`).
/// This is the maintenance side of [`dict_find_helper`]'s probe; keeping the index
/// current turns dict insert/lookup from the linear-scan fallback into O(1).
/// Void (like `$ensure`) so callers invoke it through `$Do` with no leftover
/// stack value. Calls `$dict_hash`.
pub fn dict_index_put_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // slot pointer for the current probe position h: idx + 4 + h*4.
    let sp = || b(BinOp::Add, b(BinOp::Add, getl("idx"), i32c(4)), b(BinOp::Mul, getl("h"), i32c(4)));
    let store_and_exit = vec![
        N::Store { ptr: sp(), value: b(BinOp::Add, getl("e"), i32c(1)), kind: Kind::I32, offset: 0 },
        N::Br { target: "done".into(), cond: None },
    ];
    let probe = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "p".into(),
            body: vec![
                N::If {
                    cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(E::Load { ptr: Box::new(sp()), kind: Kind::I32, offset: 0 }) },
                    then_: store_and_exit,
                    els: vec![],
                    result: None,
                },
                setl("h", b(BinOp::And, b(BinOp::Add, getl("h"), i32c(1)), b(BinOp::Sub, getl("slots"), i32c(1)))),
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
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![],
        locals: vec![WirLocal { name: "h".into(), ty: WirTy::Bool }],
        body: vec![
            setl("h", b(BinOp::And, E::Call { func: "dict_hash".into(), args: vec![getl("k"), getl("mode")] }, b(BinOp::Sub, getl("slots"), i32c(1)))),
            probe,
        ],
        raw_body: None,
    }
}

/// `$dict_new() -> i32` — an empty dict: 8 reserved bytes holding a zero hidden
/// word (at p-4) and a zero count (at p), with `p` returned.
pub fn dict_new_helper() -> WirFunc {
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

/// `$dict_insert(d, k, v, mode) -> i32` — a fresh dict like `d` with `k` set to
/// `v`: the matching entry's value replaced, or `(k, v)` appended. Copies the
/// existing block (resetting the hidden index word to 0), then writes in place.
pub fn dict_insert_helper() -> WirFunc {
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
        locals: ["count", "found", "new", "bytes"]
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
                    N::Push(getl("new")),
                ],
                result: Some(WirTy::Bool),
            },
        ],
        raw_body: None,
    }
}

/// `$dict_insert_extract(d, k, v, mode) -> (dict, present, old-slot)` — the
/// copy-correct upsert baseline with exactly one semantic key search. The
/// structural search result drives both displaced-value selection and repair.
pub fn dict_insert_extract_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let entry = |base: &str, index: E| b(BinOp::Add, getl(base), b(BinOp::Mul, index, i32c(16)));
    WirFunc {
        name: "dict_insert_extract".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "v".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool, WirTy::Int],
        locals: ["count", "found", "present", "new", "bytes"]
            .iter()
            .map(|name| WirLocal { name: (*name).into(), ty: WirTy::Bool })
            .chain(std::iter::once(WirLocal { name: "old".into(), ty: WirTy::Int }))
            .collect(),
        body: vec![
            N::SetLocal { local: "count".into(), value: E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 } },
            N::SetLocal { local: "found".into(), value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
            N::SetLocal { local: "present".into(), value: b(BinOp::Ge, getl("found"), i32c(0)) },
            N::SetLocal { local: "old".into(), value: i64c(0) },
            N::If {
                cond: getl("present"),
                then_: vec![N::SetLocal {
                    local: "old".into(),
                    value: E::Load { ptr: Box::new(entry("d", getl("found"))), kind: Kind::I64, offset: 12 },
                }],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "bytes".into(), value: b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(16))) },
            N::SetLocal {
                local: "new".into(),
                value: b(BinOp::Add, E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(24), b(BinOp::Mul, getl("count"), i32c(16)))] }, i32c(4)),
            },
            N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::MemoryCopy { dest: getl("new"), src: getl("d"), len: getl("bytes") },
            N::If {
                cond: getl("present"),
                then_: vec![N::Store { ptr: entry("new", getl("found")), value: getl("v"), kind: Kind::I64, offset: 12 }],
                els: vec![
                    N::Store { ptr: getl("new"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                    N::Store { ptr: entry("new", getl("count")), value: getl("k"), kind: Kind::I64, offset: 4 },
                    N::Store { ptr: entry("new", getl("count")), value: getl("v"), kind: Kind::I64, offset: 12 },
                ],
                result: None,
            },
            N::Push(getl("new")),
            N::Push(getl("present")),
            N::Push(getl("old")),
        ],
        raw_body: None,
    }
}

/// `$dict_insert_cap(d, k, v, mode, cap) -> (i32, i32)` — the in-place dict upsert.
/// With owned entry slack (`cap`, the shadow-local capacity), an existing key
/// updates its value slot in place and a new key appends an entry (count+1),
/// returning `d` + `cap`; otherwise the table is copied once at double capacity.
/// Bumps `$__witchy_reowns` when entered with a zero cap (the re-own signal).
/// Mirrors `DICT_INSERT_CAP_WAT` MINUS the hash-index maintenance: the binary
/// path never builds the `d-4` index (no `dict_index_*` helpers), so the index
/// word stays 0 and `$dict_find` linear-scans — correct, same values as the WAT
/// path. The multi-value early `return`s are restructured into `ret_ptr`/`ret_cap`
/// locals + a dual tail Push (WIR has no multi-value If/Return). Calls `$dict_find`
/// + `$ensure`; uses `$heap` + `$__witchy_reowns`.
pub fn dict_insert_cap_helper() -> WirFunc {
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
                    getl("mode"),
                ],
            })],
            els: vec![],
            result: None,
        },
        N::SetLocal { local: "ret_ptr".into(), value: getl("d") },
        N::SetLocal { local: "ret_cap".into(), value: getl("cap") },
    ];
    // else: copy to a doubled buffer (index word reset to 0), then upsert.
    let grow = vec![
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
        // Build a fresh hash index only for modes `$dict_hash` understands
        // (0 = bits, 1 = string, 2 = float). Structural key modes use the hidden
        // index word's zero value and `dict_find`'s linear scan; hashing a record
        // pointer as a string would be both wrong and unsafe.
        N::If {
            cond: b(BinOp::Le, getl("mode"), i32c(2)),
            then_: vec![
                N::SetLocal { local: "icount".into(), value: E::Load { ptr: Box::new(getl("new")), kind: Kind::I32, offset: 0 } },
                N::SetLocal { local: "islots".into(), value: i32c(16) },
                N::Block {
                    label: "isz".into(),
                    result: None,
                    body: vec![N::Loop {
                        label: "isl".into(),
                        body: vec![
                            N::Br { target: "isz".into(), cond: Some(b(BinOp::Ge, getl("islots"), b(BinOp::Mul, getl("newcap"), i32c(2)))) },
                            N::SetLocal { local: "islots".into(), value: b(BinOp::Mul, getl("islots"), i32c(2)) },
                            N::Br { target: "isl".into(), cond: None },
                        ],
                    }],
                },
                // (RFC-0051 I2) Allocate the index block through `$bump_alloc` — the single
                // ensure-prefixed allocator — instead of a raw ensure+bump pair here. The
                // index block is header-less scratch (rebuilt on every grow), so it takes
                // the bump core, not `$rc_alloc`.
                N::SetLocal {
                    local: "iptr".into(),
                    value: E::Call {
                        func: "bump_alloc".into(),
                        args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("islots"), i32c(4)))],
                    },
                },
                N::Store { ptr: getl("iptr"), value: getl("islots"), kind: Kind::I32, offset: 0 },
                N::MemoryFill { dest: b(BinOp::Add, getl("iptr"), i32c(4)), value: i32c(0), len: b(BinOp::Mul, getl("islots"), i32c(4)) },
                N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: getl("iptr"), kind: Kind::I32, offset: 0 },
                N::SetLocal { local: "ie".into(), value: i32c(0) },
                N::Block {
                    label: "ipd".into(),
                    result: None,
                    body: vec![N::Loop {
                        label: "ipl".into(),
                        body: vec![
                            N::Br { target: "ipd".into(), cond: Some(b(BinOp::Ge, getl("ie"), getl("icount"))) },
                            N::Do(E::Call {
                                func: "dict_index_put".into(),
                                args: vec![
                                    getl("iptr"),
                                    getl("islots"),
                                    getl("ie"),
                                    E::Load { ptr: Box::new(entry("new", "ie")), kind: Kind::I64, offset: 4 },
                                    getl("mode"),
                                ],
                            }),
                            N::SetLocal { local: "ie".into(), value: b(BinOp::Add, getl("ie"), i32c(1)) },
                            N::Br { target: "ipl".into(), cond: None },
                        ],
                    }],
                },
            ],
            els: vec![],
            result: None,
        },
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
/// upsert: apply the updater closure to the current value (or `default`) and
/// reinsert via `$dict_insert_cap` (so an owned dict mutates in place). The
/// closure call mirrors the non-cap `$dict_update`; the (ptr, cap) pair from
/// `$dict_insert_cap` is captured into locals and re-pushed (WIR can't tail a
/// multi-value call). Calls `$dict_get_or` + `$dict_insert_cap`; uses the table.
pub fn dict_update_cap_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    WirFunc {
        name: "dict_update_cap".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "default".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
            WirLocal { name: "clos".into(), ty: WirTy::Bool },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool],
        locals: vec![
            WirLocal { name: "new".into(), ty: WirTy::Int },
            WirLocal { name: "ret_ptr".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_cap".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "new".into(),
                value: E::CallIndirect {
                    result_count: 1,
                    type_arity: 1,
                    args: vec![
                        getl("clos"),
                        E::Call {
                            func: "dict_get_or".into(),
                            args: vec![getl("d"), getl("k"), getl("default"), getl("mode")],
                        },
                    ],
                    index: Box::new(E::Load { ptr: Box::new(getl("clos")), kind: Kind::I32, offset: 0 }),
                },
            },
            N::CallStoreMulti {
                func: "dict_insert_cap".into(),
                args: vec![getl("d"), getl("k"), getl("new"), getl("mode"), getl("cap")],
                dests: vec!["ret_ptr".into(), "ret_cap".into()],
            },
            N::Push(getl("ret_ptr")),
            N::Push(getl("ret_cap")),
        ],
        raw_body: None,
    }
}

/// `$dict_get_or(d, k, default, mode) -> i64` — the value slot for `k`, or
/// `default` when absent.
pub fn dict_get_or_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dict_get_or".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "default".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int],
        locals: vec![WirLocal { name: "found".into(), ty: WirTy::Bool }],
        body: vec![
            N::SetLocal { local: "found".into(), value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
            N::If {
                cond: b(BinOp::Lt, getl("found"), i32c(0)),
                then_: vec![N::Return(Some(getl("default")))],
                els: vec![],
                result: None,
            },
            // value slot: d + 12 + found*16.
            N::Push(E::Load {
                ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("found"), i32c(16)))),
                kind: Kind::I64,
                offset: 12,
            }),
        ],
        raw_body: None,
    }
}

/// `$dict_at(d, k, mode) -> i64` — the value slot for `k`, or a routed runtime
/// error when absent. This is the compiled half of strict `d[k]` reads.
pub fn dict_at_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dict_at".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int],
        locals: vec![WirLocal { name: "found".into(), ty: WirTy::Bool }],
        body: vec![
            N::SetLocal { local: "found".into(), value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
            N::If {
                cond: b(BinOp::Lt, getl("found"), i32c(0)),
                then_: abort_nodes(DiagTemplate::DictMissing, i64c(0), i64c(0), i32c(0)),
                els: vec![],
                result: None,
            },
            // value slot: d + 12 + found*16.
            N::Push(E::Load {
                ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("found"), i32c(16)))),
                kind: Kind::I64,
                offset: 12,
            }),
        ],
        raw_body: None,
    }
}

/// `$dict_update(d, k, default, mode, clos) -> i32` — apply the updater closure
/// to the current value (or `default` when absent) and reinsert. The closure is
/// a 1-arg `$clos1` (`(param i32 env)(param i64 v)(result i64)`): its env pointer
/// is the closure record itself and its code index is the record's first word.
pub fn dict_update_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    WirFunc {
        name: "dict_update".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "default".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
            WirLocal { name: "clos".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![WirLocal { name: "new".into(), ty: WirTy::Int }],
        body: vec![
            N::SetLocal {
                local: "new".into(),
                value: E::CallIndirect {
                    result_count: 1,
                    type_arity: 1,
                    args: vec![
                        getl("clos"),
                        E::Call {
                            func: "dict_get_or".into(),
                            args: vec![getl("d"), getl("k"), getl("default"), getl("mode")],
                        },
                    ],
                    index: Box::new(E::Load {
                        ptr: Box::new(getl("clos")),
                        kind: Kind::I32,
                        offset: 0,
                    }),
                },
            },
            N::Push(E::Call {
                func: "dict_insert".into(),
                args: vec![getl("d"), getl("k"), getl("new"), getl("mode")],
            }),
        ],
        raw_body: None,
    }
}

/// `$dict_has(d, k, mode) -> i32` — whether `k` is present.
pub fn dict_has_helper() -> WirFunc {
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
pub fn dict_project_helper(name: &str, entry_off: u32) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let src = E::Load {
        ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("i"), i32c(16)))),
        kind: Kind::I64,
        offset: entry_off,
    };
    let scan = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                N::Store { ptr: b(BinOp::Add, getl("new"), b(BinOp::Mul, getl("i"), i32c(8))), value: src, kind: Kind::I64, offset: 4 },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: name.into(),
        params: vec![WirLocal { name: "d".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: ["count", "i", "new"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            // (RFC-0016) allocate the projected list through `$rc_alloc` (header + reuse).
            setl("new", E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(8)))] }),
            N::Store { ptr: getl("new"), value: getl("count"), kind: Kind::I32, offset: 0 },
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
pub fn dict_pairs_helper() -> WirFunc {
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
                N::Store { ptr: getl("tup"), value: entry(4), kind: Kind::I64, offset: 4 },
                N::Store { ptr: getl("tup"), value: entry(12), kind: Kind::I64, offset: 12 },
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
        locals: ["count", "i", "list", "tup"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            // (RFC-0016) allocate the list through `$rc_alloc` (header + reuse); it bumps
            // `$heap` past the list, so each pair tuple's rc_alloc lands in a distinct block
            // above it and never overlaps a written slot.
            setl("list", E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(8)))] }),
            N::Store { ptr: getl("list"), value: getl("count"), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            scan,
            N::Push(getl("list")),
        ],
        raw_body: None,
    }
}

/// `$dict_remove(d, k, mode) -> i32` — a fresh dict with the entry for `k`
/// dropped (unchanged if absent). Copies every entry whose key isn't `k`.
pub fn dict_remove_helper() -> WirFunc {
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
            // `$rc_alloc` reserved the FULL `count`-slot capacity (the size arg above)
            // and already advanced `$heap`, so the count-n slack stays reserved for a
            // later in-place insert — no manual bump.
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$dict_remove_extract(d, k, mode) -> (dict, present, old-slot)` — locate the
/// entry once, return its old value, and repair insertion order by copying the
/// two ranges around the selected index.
pub fn dict_remove_extract_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let entry = |base: &str, index: E| b(BinOp::Add, getl(base), b(BinOp::Mul, index, i32c(16)));
    WirFunc {
        name: "dict_remove_extract".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool, WirTy::Int],
        locals: ["count", "found", "present", "new", "prefix", "suffix"]
            .iter()
            .map(|name| WirLocal { name: (*name).into(), ty: WirTy::Bool })
            .chain(std::iter::once(WirLocal { name: "old".into(), ty: WirTy::Int }))
            .collect(),
        body: vec![
            N::SetLocal { local: "count".into(), value: E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 } },
            N::SetLocal { local: "found".into(), value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
            N::SetLocal { local: "present".into(), value: b(BinOp::Ge, getl("found"), i32c(0)) },
            N::SetLocal { local: "old".into(), value: i64c(0) },
            N::SetLocal {
                local: "new".into(),
                value: b(BinOp::Add, E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("count"), i32c(16)))] }, i32c(4)),
            },
            N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::If {
                cond: getl("present"),
                then_: vec![
                    N::SetLocal { local: "old".into(), value: E::Load { ptr: Box::new(entry("d", getl("found"))), kind: Kind::I64, offset: 12 } },
                    N::SetLocal { local: "prefix".into(), value: b(BinOp::Mul, getl("found"), i32c(16)) },
                    N::SetLocal { local: "suffix".into(), value: b(BinOp::Mul, b(BinOp::Sub, b(BinOp::Sub, getl("count"), getl("found")), i32c(1)), i32c(16)) },
                    N::Store { ptr: getl("new"), value: b(BinOp::Sub, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                    N::MemoryCopy { dest: b(BinOp::Add, getl("new"), i32c(4)), src: b(BinOp::Add, getl("d"), i32c(4)), len: getl("prefix") },
                    N::MemoryCopy {
                        dest: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), getl("prefix")),
                        src: b(BinOp::Add, entry("d", b(BinOp::Add, getl("found"), i32c(1))), i32c(4)),
                        len: getl("suffix"),
                    },
                ],
                els: vec![N::MemoryCopy {
                    dest: getl("new"),
                    src: getl("d"),
                    len: b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(16))),
                }],
                result: None,
            },
            N::Push(getl("new")),
            N::Push(getl("present")),
            N::Push(getl("old")),
        ],
        raw_body: None,
    }
}
