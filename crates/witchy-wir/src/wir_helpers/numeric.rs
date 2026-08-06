//! Numeric parsing, formatting, conversion, comparison, and checked arithmetic.

use super::abort_nodes;
use crate::layout::HEAP_REDZONE;
use crate::wir::*;
use witchy_syntax::diag::DiagTemplate;

/// `$int_to_string(n: i64) -> i32` — render a signed integer to a fresh witchy
/// string (`[i32 len][ascii]`). Mirrors `INT_TO_STRING_WAT`: `0` is a fast path;
/// otherwise count digits (a div-by-10 loop), allocate `[len][digits]`, write the
/// optional `-` then the digits back-to-front (a second div/rem loop). Calls
/// `$ensure`; uses the `$heap` global; byte writes via `Store8`.
pub(crate) fn int_to_string_helper(checked: bool) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let bin = |op: BinOp, k: Kind, l: E, r: E| E::Binary {
        op,
        kind: k,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    // (RFC-0023) This string builder — exact-size, no spare capacity — is the home of
    // the motivating `int_to_string` OOB. When checked, reserve+register a redzone so
    // the post-run sweep proves the digit writes stayed inside the object.
    let rz = if checked { HEAP_REDZONE as i32 } else { 0 };
    let reg = |start: E, end: E| {
        N::Do(E::CallHost { import: "heap_register".into(), args: vec![start, end] })
    };
    // n == 0 → the single ascii '0' (object `[res, res+5)`).
    // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves
    // `size+8` and bumps `$heap` by `size`, so `heap` lands at exactly `res+(5+rz)` —
    // the same point the manual bump reached, keeping the redzone registration valid.
    let mut then_zero = vec![
        N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(5 + rz)] } },
        N::Store { ptr: getl("res"), value: i32c(1), kind: Kind::I32, offset: 0 },
        N::Store8 { ptr: getl("res"), value: i32c(48), offset: 4 },
    ];
    if checked {
        then_zero.push(reg(getl("res"), bin(BinOp::Add, Kind::I32, getl("res"), i32c(5))));
    }
    then_zero.push(N::Push(getl("res")));
    // Count digits of `t` (mutated to 0): `while t != 0 { ndigits++; t /= 10 }`.
    let count_loop = N::Block {
        label: "b1".into(),
        result: None,
        body: vec![N::Loop {
            label: "l1".into(),
            body: vec![
                N::Br { target: "b1".into(), cond: Some(bin(BinOp::Eq, Kind::I64, getl("t"), i64c(0))) },
                N::SetLocal {
                    local: "ndigits".into(),
                    value: bin(BinOp::Add, Kind::I32, getl("ndigits"), i32c(1)),
                },
                N::SetLocal {
                    local: "t".into(),
                    value: bin(BinOp::DivU, Kind::I64, getl("t"), i64c(10)),
                },
                N::Br { target: "l1".into(), cond: None },
            ],
        }],
    };
    // Write digits back-to-front at `p` (decremented): `store8(p, t%10 + '0')`.
    let write_loop = N::Block {
        label: "b2".into(),
        result: None,
        body: vec![N::Loop {
            label: "l2".into(),
            body: vec![
                N::Br { target: "b2".into(), cond: Some(bin(BinOp::Eq, Kind::I64, getl("t"), i64c(0))) },
                N::Store8 {
                    ptr: getl("p"),
                    value: bin(
                        BinOp::Add,
                        Kind::I32,
                        E::Convert {
                            from: Kind::I64,
                            to: Kind::I32,
                            arg: Box::new(bin(BinOp::RemU, Kind::I64, getl("t"), i64c(10))),
                        },
                        i32c(48),
                    ),
                    offset: 0,
                },
                N::SetLocal {
                    local: "p".into(),
                    value: bin(BinOp::Sub, Kind::I32, getl("p"), i32c(1)),
                },
                N::SetLocal {
                    local: "t".into(),
                    value: bin(BinOp::DivU, Kind::I64, getl("t"), i64c(10)),
                },
                N::Br { target: "l2".into(), cond: None },
            ],
        }],
    };
    let mut else_nonzero = vec![
        N::SetLocal { local: "neg".into(), value: bin(BinOp::Lt, Kind::I64, getl("n"), i64c(0)) },
        // mag = neg ? -n : n
        N::SetLocal {
            local: "mag".into(),
            value: E::Control(Box::new(N::If {
                cond: getl("neg"),
                then_: vec![N::Push(bin(BinOp::Sub, Kind::I64, i64c(0), getl("n")))],
                els: vec![N::Push(getl("n"))],
                result: Some(WirTy::Int),
            })),
        },
        N::SetLocal { local: "ndigits".into(), value: i32c(0) },
        N::SetLocal { local: "t".into(), value: getl("mag") },
        count_loop,
        N::SetLocal {
            local: "len".into(),
            value: bin(BinOp::Add, Kind::I32, getl("ndigits"), getl("neg")),
        },
        N::SetLocal {
            local: "res".into(),
            value: E::Call { func: "rc_alloc".into(), args: vec![bin(BinOp::Add, Kind::I32, i32c(4 + rz), getl("len"))] },
        },
        N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
        N::If {
            cond: getl("neg"),
            then_: vec![N::Store8 { ptr: getl("res"), value: i32c(45), offset: 4 }],
            els: vec![],
            result: None,
        },
        // p = res + 4 + len - 1 (the last digit's byte)
        N::SetLocal {
            local: "p".into(),
            value: bin(
                BinOp::Sub,
                Kind::I32,
                bin(BinOp::Add, Kind::I32, bin(BinOp::Add, Kind::I32, getl("res"), i32c(4)), getl("len")),
                i32c(1),
            ),
        },
        N::SetLocal { local: "t".into(), value: getl("mag") },
        write_loop,
    ];
    // object end = res + 4 + len; rebuilt per use (WirExpr is moved, not cloned).
    let str_end = || {
        bin(BinOp::Add, Kind::I32, bin(BinOp::Add, Kind::I32, getl("res"), i32c(4)), getl("len"))
    };
    if checked {
        else_nonzero.push(reg(getl("res"), str_end()));
    }
    else_nonzero.push(N::Push(getl("res")));
    WirFunc {
        name: "int_to_string".into(),
        params: vec![WirLocal { name: "n".into(), ty: WirTy::Int }],
        ret: vec![WirTy::Str], // i32 pointer
        locals: vec![
            WirLocal { name: "mag".into(), ty: WirTy::Int },
            WirLocal { name: "t".into(), ty: WirTy::Int },
            WirLocal { name: "ndigits".into(), ty: WirTy::Bool },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
            WirLocal { name: "p".into(), ty: WirTy::Bool },
            WirLocal { name: "neg".into(), ty: WirTy::Bool },
        ],
        body: vec![N::If {
            cond: bin(BinOp::Eq, Kind::I64, getl("n"), i64c(0)),
            then_: then_zero,
            els: else_nonzero,
            result: Some(WirTy::Str),
        }],
        raw_body: None,
    }
}


/// `$f_lt`/`$f_le`/`$f_gt`/`$f_ge`(a: f64, b: f64) -> i32 — a NaN-trapping float
/// ordering compare. Witchy errors on ordering a NaN (the interpreter oracle
/// traps), so each helper first traps (`unreachable`) when either operand is NaN
/// (`x != x`), then does the plain `f64.{lt,le,gt,ge}`. Mirrors `FLOAT_ORD_WAT`
/// with the NaN guard inlined (the binary sink is independent of the WAT one).
pub(super) fn float_cmp_helper(name: &str, op: BinOp) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let is_nan = |n: &str| E::Binary {
        op: BinOp::Ne,
        kind: Kind::F64,
        lhs: Box::new(getl(n)),
        rhs: Box::new(getl(n)),
    };
    WirFunc {
        name: name.into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Float },
            WirLocal { name: "b".into(), ty: WirTy::Float },
        ],
        ret: vec![WirTy::Bool], // i32
        locals: vec![],
        body: vec![
            // NaN on either side → abort (matches the interpreter's
            // `cannot compare NaN`, routed through `__witchy_abort`, RFC-0045).
            N::If {
                cond: E::Binary {
                    op: BinOp::Or,
                    kind: Kind::I32,
                    lhs: Box::new(is_nan("a")),
                    rhs: Box::new(is_nan("b")),
                },
                then_: abort_nodes(DiagTemplate::NanOrder, E::ConstI64(0), E::ConstI64(0), E::ConstI32(0)),
                els: vec![],
                result: None,
            },
            N::Push(E::Binary {
                op,
                kind: Kind::F64,
                lhs: Box::new(getl("a")),
                rhs: Box::new(getl("b")),
            }),
        ],
        raw_body: None,
    }
}

/// Guarded Witchy integer division. WebAssembly traps directly on zero and on
/// `Int::MIN / -1`; route both through the shared language diagnostics first.
pub(super) fn int_div_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let eq = |lhs: E, rhs: E| E::Binary {
        op: BinOp::Eq,
        kind: Kind::I64,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    };
    WirFunc {
        name: "int_div".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Int },
            WirLocal { name: "b".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Int],
        locals: vec![],
        body: vec![
            N::If {
                cond: eq(getl("b"), E::ConstI64(0)),
                then_: abort_nodes(
                    DiagTemplate::DivisionByZero,
                    E::ConstI64(0),
                    E::ConstI64(0),
                    E::ConstI32(0),
                ),
                els: vec![],
                result: None,
            },
            N::If {
                cond: E::Binary {
                    op: BinOp::And,
                    kind: Kind::I32,
                    lhs: Box::new(eq(getl("a"), E::ConstI64(i64::MIN))),
                    rhs: Box::new(eq(getl("b"), E::ConstI64(-1))),
                },
                then_: abort_nodes(
                    DiagTemplate::DivisionOverflow,
                    E::ConstI64(0),
                    E::ConstI64(0),
                    E::ConstI32(0),
                ),
                els: vec![],
                result: None,
            },
            N::Push(E::Binary {
                op: BinOp::Div,
                kind: Kind::I64,
                lhs: Box::new(getl("a")),
                rhs: Box::new(getl("b")),
            }),
        ],
        raw_body: None,
    }
}

/// Guarded Witchy integer remainder. `Int::MIN % -1` is defined as zero on
/// both backends; only a zero divisor aborts.
pub(super) fn int_rem_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    WirFunc {
        name: "int_rem".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Int },
            WirLocal { name: "b".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Int],
        locals: vec![],
        body: vec![
            N::If {
                cond: E::Binary {
                    op: BinOp::Eq,
                    kind: Kind::I64,
                    lhs: Box::new(getl("b")),
                    rhs: Box::new(E::ConstI64(0)),
                },
                then_: abort_nodes(
                    DiagTemplate::ModuloByZero,
                    E::ConstI64(0),
                    E::ConstI64(0),
                    E::ConstI32(0),
                ),
                els: vec![],
                result: None,
            },
            N::Push(E::Binary {
                op: BinOp::Rem,
                kind: Kind::I64,
                lhs: Box::new(getl("a")),
                rhs: Box::new(getl("b")),
            }),
        ],
        raw_body: None,
    }
}

/// `$float_to_int(x: f64) -> i64` — `math.to_int`. Finite values and infinities
/// keep the WebAssembly saturating conversion policy, but NaN is a Witchy
/// runtime error instead of silently becoming 0.
pub(super) fn float_to_int_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let is_nan = E::Binary {
        op: BinOp::Ne,
        kind: Kind::F64,
        lhs: Box::new(getl("x")),
        rhs: Box::new(getl("x")),
    };
    WirFunc {
        name: "float_to_int".into(),
        params: vec![WirLocal { name: "x".into(), ty: WirTy::Float }],
        ret: vec![WirTy::Int],
        locals: vec![],
        body: vec![
            N::If {
                cond: is_nan,
                then_: abort_nodes(DiagTemplate::NanToInt, E::ConstI64(0), E::ConstI64(0), E::ConstI32(0)),
                els: vec![],
                result: None,
            },
            N::Push(E::Unary {
                op: UnOp::ToInt,
                kind: Kind::I64,
                arg: Box::new(getl("x")),
            }),
        ],
        raw_body: None,
    }
}

/// `$str_to_int(s) -> i64` — parse a (optionally signed) decimal integer,
/// tolerating leading/trailing ASCII whitespace. Traps (like Rust's checked
/// parse) on overflow, on no digits, or on trailing non-whitespace garbage —
/// matching the interpreter oracle, which errors on the same inputs.
pub(crate) fn str_to_int_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b32 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let byte = || E::Load8U { ptr: Box::new(b32(BinOp::Add, getl("s"), getl("i"))), offset: 4 };
    let not = |e: E| E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(e) };
    let is_ws_b = || not(E::Call { func: "is_ws".into(), args: vec![getl("b")] });
    let inc_i = || setl("i", b32(BinOp::Add, getl("i"), i32c(1)));
    // digit magnitude (b - '0') widened to i64.
    let digit = || E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(b32(BinOp::Sub, getl("b"), i32c(48))) };
    let ws_skip = |done: &str, l: &str| N::Block {
        label: done.into(),
        result: None,
        body: vec![N::Loop {
            label: l.into(),
            body: vec![
                N::Br { target: done.into(), cond: Some(b32(BinOp::Ge, getl("i"), getl("len"))) },
                setl("b", byte()),
                N::Br { target: done.into(), cond: Some(is_ws_b()) },
                inc_i(),
                N::Br { target: l.into(), cond: None },
            ],
        }],
    };
    let digit_loop = N::Block {
        label: "digdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "dig".into(),
            body: vec![
                N::Br { target: "digdone".into(), cond: Some(b32(BinOp::Ge, getl("i"), getl("len"))) },
                setl("b", byte()),
                N::Br {
                    target: "digdone".into(),
                    cond: Some(b32(BinOp::Or, b32(BinOp::LtU, getl("b"), i32c(48)), b32(BinOp::GtU, getl("b"), i32c(57)))),
                },
                // overflow: acc >u (limit - d) / 10  ->  abort (RFC-0045:
                // `cannot parse `{s}` as an Int`, carrying the string pointer).
                N::If {
                    cond: b64(
                        BinOp::GtU,
                        getl("acc"),
                        b64(BinOp::DivU, b64(BinOp::Sub, getl("limit"), digit()), i64c(10)),
                    ),
                    then_: abort_nodes(DiagTemplate::ParseInt, i64c(0), i64c(0), getl("s")),
                    els: vec![],
                    result: None,
                },
                setl("acc", b64(BinOp::Add, b64(BinOp::Mul, getl("acc"), i64c(10)), digit())),
                setl("got", i32c(1)),
                inc_i(),
                N::Br { target: "dig".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "str_to_int".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Int],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "b".into(), ty: WirTy::Bool },
            WirLocal { name: "acc".into(), ty: WirTy::Int },
            WirLocal { name: "neg".into(), ty: WirTy::Bool },
            WirLocal { name: "got".into(), ty: WirTy::Bool },
            WirLocal { name: "limit".into(), ty: WirTy::Int },
        ],
        body: vec![
            setl("len", load(getl("s"))),
            setl("i", i32c(0)),
            setl("acc", i64c(0)),
            setl("neg", i32c(0)),
            setl("got", i32c(0)),
            ws_skip("wsdone", "ws"),
            // optional sign
            N::If {
                cond: b32(BinOp::Lt, getl("i"), getl("len")),
                then_: vec![
                    setl("b", byte()),
                    N::If {
                        cond: b32(BinOp::Eq, getl("b"), i32c(45)),
                        then_: vec![setl("neg", i32c(1)), inc_i()],
                        els: vec![N::If {
                            cond: b32(BinOp::Eq, getl("b"), i32c(43)),
                            then_: vec![inc_i()],
                            els: vec![],
                            result: None,
                        }],
                        result: None,
                    },
                ],
                els: vec![],
                result: None,
            },
            // magnitude bound: |i64::MIN| for negatives, i64::MAX otherwise.
            N::If {
                cond: getl("neg"),
                then_: vec![setl("limit", i64c(i64::MIN))],
                els: vec![setl("limit", i64c(i64::MAX))],
                result: None,
            },
            digit_loop,
            ws_skip("twsdone", "tws"),
            // must have consumed at least one digit and reached the end.
            N::If {
                cond: b32(BinOp::Or, not(getl("got")), b32(BinOp::Lt, getl("i"), getl("len"))),
                then_: abort_nodes(DiagTemplate::ParseInt, i64c(0), i64c(0), getl("s")),
                els: vec![],
                result: None,
            },
            N::If {
                cond: getl("neg"),
                then_: vec![N::Push(b64(BinOp::Sub, i64c(0), getl("acc")))],
                els: vec![N::Push(getl("acc"))],
                result: Some(WirTy::Int),
            },
        ],
        raw_body: None,
    }
}

/// `$float_to_str(x) -> i32` — render f64 `x` to a String via the host
/// `float_to_str` import (writes into a reserved 512-byte buffer, returns the
/// length). Used by the `$ts` renderer for Float fields. The import is ungated.
pub(super) fn float_to_str_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "float_to_str".into(),
        params: vec![WirLocal { name: "x".into(), ty: WirTy::Float }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "res".into(), ty: WirTy::Bool },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
        ],
        body: vec![
            // (RFC-0016) reserve the worst-case 516-byte buffer through `$rc_alloc`; the
            // host writes n<=512 bytes and the length header caps to n (the block's size
            // header stays 516 — a valid upper bound; the tail slack is unused).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(516)] } },
            N::SetLocal { local: "n".into(), value: E::CallHost { import: "float_to_str".into(), args: vec![getl("x"), b(BinOp::Add, getl("res"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}
