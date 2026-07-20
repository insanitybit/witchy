//! List access and ownership-aware extraction helpers.

mod operations;

pub use operations::*;

use super::abort_nodes;
use crate::wir::*;
use witchy_syntax::diag::DiagTemplate;

/// `$list_at(list: i32, i: i64) -> i64` — bounds-checked element read: trap on
/// `i < 0 || i >= len`, else load the i64 slot at `(list+4) + i*8`.
///
/// The index is i64 (the witchy `Int` width) and the check is done in i64: the
/// interpreter indexes with the full `i as usize`, so an out-of-`i32`-range index
/// (which would WRAP to an in-range i32 if narrowed first) must still trap — and
/// its true value must appear in the message. Only after the check passes (so
/// `0 <= i < len <= i32::MAX`) is `i` narrowed to i32 for the address arithmetic.
pub fn list_at_helper() -> WirFunc {
    let getl = |n: &str| WirExpr::GetLocal(n.into());
    let i32c = WirExpr::ConstI32;
    let i64c = WirExpr::ConstI64;
    let bin32 = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let bin64 = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
        op,
        kind: Kind::I64,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    // len as i64 (the i32 element-count header sign-extended) — the check's bound.
    let len_i64 = || WirExpr::Convert {
        from: Kind::I32,
        to: Kind::I64,
        arg: Box::new(WirExpr::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 }),
    };
    // i narrowed to i32 — valid only inside the checked range.
    let i_i32 = || WirExpr::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(getl("i")) };
    WirFunc {
        name: "list_at".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Int], // i64 slot
        locals: vec![],
        body: vec![
            WirNode::If {
                // Both comparisons yield i32 (wasm `i64.lt_s`/`i64.ge_s` -> i32), so
                // combine them with `i32.or`.
                cond: bin32(
                    BinOp::Or,
                    bin64(BinOp::Lt, getl("i"), i64c(0)),
                    bin64(BinOp::Ge, getl("i"), len_i64()),
                ),
                // (RFC-0045) Route the OOB abort through `__witchy_abort` with the
                // TRUE i64 index and the list length, so the compiled trap carries
                // the interpreter's `list index {i} out of bounds (length {len})`.
                then_: abort_nodes(DiagTemplate::ListIndexOob, getl("i"), len_i64(), i32c(0)),
                els: vec![],
                result: None,
            },
            WirNode::Push(WirExpr::Load {
                ptr: Box::new(bin32(
                    BinOp::Add,
                    bin32(BinOp::Add, getl("list"), i32c(4)),
                    bin32(BinOp::Mul, i_i32(), i32c(8)),
                )),
                kind: Kind::I64,
                offset: 0,
            }),
        ],
        raw_body: None,
    }
}

/// `$list_pop_extract(list, cap, rc_bias) -> (list, present, old-slot, cap)`.
/// A live ownership token repairs the list in place; zero takes the CoW path.
/// Selection and repair share one length read, while
/// [`super::slot_take_or_dup_helper`]
/// owns transfer-versus-retain behavior for the selected leaf.
pub fn list_pop_extract_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    WirFunc {
        name: "list_pop_extract".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
            WirLocal { name: "rc_bias".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool, WirTy::Int, WirTy::Bool],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Bool },
            WirLocal { name: "present".into(), ty: WirTy::Bool },
            WirLocal { name: "out_cap".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "old".into(), ty: WirTy::Int },
        ],
        body: vec![
            N::SetLocal {
                local: "len".into(),
                value: E::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 },
            },
            N::SetLocal { local: "present".into(), value: b(BinOp::Gt, getl("len"), i32c(0)) },
            N::SetLocal { local: "old".into(), value: i64c(0) },
            N::If {
                cond: b(BinOp::Gt, getl("cap"), i32c(0)),
                then_: vec![
                    N::SetLocal { local: "new".into(), value: getl("list") },
                    N::SetLocal { local: "out_cap".into(), value: getl("cap") },
                    N::If {
                        cond: getl("present"),
                        then_: vec![
                            N::SetLocal {
                                local: "old".into(),
                                value: E::Call {
                                    func: "slot_take_or_dup".into(),
                                    args: vec![
                                        b(
                                            BinOp::Add,
                                            b(BinOp::Add, getl("list"), i32c(4)),
                                            b(BinOp::Mul, b(BinOp::Sub, getl("len"), i32c(1)), i32c(8)),
                                        ),
                                        i32c(1),
                                        getl("rc_bias"),
                                    ],
                                },
                            },
                            N::Store {
                                ptr: getl("list"),
                                value: b(BinOp::Sub, getl("len"), i32c(1)),
                                kind: Kind::I32,
                                offset: 0,
                            },
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
                            value: E::Call {
                                func: "rc_alloc".into(),
                                args: vec![b(
                                    BinOp::Add,
                                    i32c(4),
                                    b(BinOp::Mul, b(BinOp::Sub, getl("len"), i32c(1)), i32c(8)),
                                )],
                            },
                        },
                        N::Store {
                            ptr: getl("new"),
                            value: b(BinOp::Sub, getl("len"), i32c(1)),
                            kind: Kind::I32,
                            offset: 0,
                        },
                        N::MemoryCopy {
                            dest: b(BinOp::Add, getl("new"), i32c(4)),
                            src: b(BinOp::Add, getl("list"), i32c(4)),
                            len: b(BinOp::Mul, b(BinOp::Sub, getl("len"), i32c(1)), i32c(8)),
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
                                    arg: Box::new(b(
                                        BinOp::Mul,
                                        b(BinOp::Sub, getl("len"), i32c(1)),
                                        i32c(8),
                                    )),
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
                                            b(BinOp::Sub, getl("len"), i32c(1)),
                                        )),
                                    },
                                    N::Drop(E::Call {
                                        func: "leaf_dup".into(),
                                        args: vec![
                                            E::Load {
                                                ptr: Box::new(b(
                                                    BinOp::Add,
                                                    b(BinOp::Add, getl("new"), i32c(4)),
                                                    b(BinOp::Mul, getl("i"), i32c(8)),
                                                )),
                                                kind: Kind::I64,
                                                offset: 0,
                                            },
                                            getl("rc_bias"),
                                        ],
                                    }),
                                    N::SetLocal {
                                        local: "i".into(),
                                        value: b(BinOp::Add, getl("i"), i32c(1)),
                                    },
                                    N::Br { target: "dup_loop".into(), cond: None },
                                ],
                            }],
                        },
                        N::SetLocal {
                            local: "old".into(),
                            value: E::Call {
                                func: "slot_take_or_dup".into(),
                                args: vec![
                                    b(
                                        BinOp::Add,
                                        b(BinOp::Add, getl("list"), i32c(4)),
                                        b(BinOp::Mul, b(BinOp::Sub, getl("len"), i32c(1)), i32c(8)),
                                    ),
                                    i32c(0),
                                    getl("rc_bias"),
                                ],
                            },
                        },
                        N::SetLocal {
                            local: "out_cap".into(),
                            value: b(BinOp::Sub, getl("len"), i32c(1)),
                        },
                    ],
                    els: vec![
                        N::SetLocal { local: "new".into(), value: getl("list") },
                        N::SetLocal { local: "out_cap".into(), value: i32c(0) },
                    ],
                    result: None,
                }],
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
