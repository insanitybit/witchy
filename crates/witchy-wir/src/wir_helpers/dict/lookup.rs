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
/// `$dict_hash(k, mode) -> i32` — a 64-bit bit-mix for scalar keys (mode 0),
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
pub(crate) fn dict_find_helper() -> WirFunc {
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
                comparison_bump(),
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
