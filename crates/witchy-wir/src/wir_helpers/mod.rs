//! `wir_helpers` — the runtime-helper library.
//!
//! Every stdlib primitive the compiled backend leans on — list/dict/string ops,
//! crypto, encoding, capability host calls — expressed as a [`WirFunc`] rather
//! than a raw wasm body. Expressing each as WIR lets the encoder re-index it by
//! name, so a module emits only the helpers it actually reaches and imports only
//! their authority (capability-correct, and no `wat` in the build).
//!
//! [`wir_helper`] is the by-name dispatcher: given a helper name it returns the
//! [`WirHelperSpec`] (the function plus its helper/import dependencies), which is
//! how `codegen` resolves a module's reachable helper set.

use crate::wir::*;
use witchy_syntax::diag::DiagTemplate;

mod bytes;
pub use bytes::*;
mod collections;
pub use collections::*;
mod dict;
pub use dict::*;
mod memory;
pub use memory::*;
mod strings;
pub use strings::*;
mod vm;
pub use vm::*;
mod registry;
pub use registry::{WirHelperSpec, wir_helper};

/// (RFC-0045) Build the node sequence that routes a runtime abort through the
/// always-linked, authority-free `__witchy_abort(template, a, b, str_ptr)` host
/// import, then traps. `a`/`b` are the i64 message holes (index, length — pass
/// `ConstI64(0)` when unused); `str_ptr` is a witchy string pointer (the junk
/// input / `fail` message, or `ConstI32(0)` when the template has no string
/// hole). The host renders the shared [`DiagTemplate`] and bails, so the
/// trailing `Unreachable` only keeps the site stack-typed (matching the
/// contract in `codegen`: the call never returns).
pub fn abort_nodes(template: DiagTemplate, a: WirExpr, b: WirExpr, str_ptr: WirExpr) -> Vec<WirNode> {
    vec![
        WirNode::Do(WirExpr::CallHost {
            import: "__witchy_abort".into(),
            args: vec![WirExpr::ConstI32(template.id()), a, b, str_ptr],
        }),
        WirNode::Unreachable,
    ]
}


/// `$print_str(s: i32)` — write a witchy string (a `[i32 len][utf-8]` record at
/// `s`) to the host `print` import: `print(s + 4, [s])`. The ONLY authority it
/// needs is `print`, so a module whose only helper is this imports nothing else.
pub fn print_str_helper() -> WirFunc {
    WirFunc {
        name: "print_str".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![],
        locals: vec![],
        body: vec![WirNode::Do(WirExpr::CallHost {
            import: "print".into(),
            args: vec![
                // ptr = s + 4 (skip the 4-byte length header)
                WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::GetLocal("s".into())),
                    rhs: Box::new(WirExpr::ConstI32(4)),
                },
                // len = [s] (the i32 length header)
                WirExpr::Load {
                    ptr: Box::new(WirExpr::GetLocal("s".into())),
                    kind: Kind::I32,
                    offset: 0,
                },
            ],
        })],
        raw_body: None,
    }
}



// Helpers below realize a confined slice *view* (RFC-0028 confined Views): a
// `let w = list.slice(src, lo, hi)` whose copy was elided keeps only `src`, `lo`,
// `hi`, and reads through them. Both recompute the clamped window
// (`off = max(lo,0)`, `end = min(hi, len(src))`) on each call from i32 args, so
// `lo`/`hi` are stored verbatim once at the binding and the result matches the
// interpreter reading the materialized copy. `i32` is encoded as `WirTy::Bool`.
fn view_clamp_setup(getl: &dyn Fn(&str) -> WirExpr) -> Vec<WirNode> {
    let i32c = WirExpr::ConstI32;
    let bin = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    vec![
        // srclen = len(src) (the i32 element-count header)
        WirNode::SetLocal {
            local: "srclen".into(),
            value: WirExpr::Load { ptr: Box::new(getl("src")), kind: Kind::I32, offset: 0 },
        },
        // off = max(lo, 0)
        WirNode::SetLocal { local: "off".into(), value: getl("lo") },
        WirNode::If {
            cond: bin(BinOp::Lt, getl("off"), i32c(0)),
            then_: vec![WirNode::SetLocal { local: "off".into(), value: i32c(0) }],
            els: vec![],
            result: None,
        },
        // end = min(hi, srclen)
        WirNode::SetLocal { local: "end".into(), value: getl("hi") },
        WirNode::If {
            cond: bin(BinOp::Gt, getl("end"), getl("srclen")),
            then_: vec![WirNode::SetLocal { local: "end".into(), value: getl("srclen") }],
            els: vec![],
            result: None,
        },
        // len = end - off  (may be negative when the window is empty)
        WirNode::SetLocal {
            local: "len".into(),
            value: bin(BinOp::Sub, getl("end"), getl("off")),
        },
    ]
}

/// `$list_at_view(src: i32, lo: i32, hi: i32, j: i32) -> i64` — bounds-checked
/// read of a confined slice view: clamp the window, trap on `j < 0 || j >= len`,
/// else load the i64 slot at `(src+4) + (off+j)*8`. A negative `len` (empty
/// window) traps every `j >= 0`, matching an out-of-range read of an empty copy.
pub fn list_at_view_helper() -> WirFunc {
    let getl = |n: &str| WirExpr::GetLocal(n.into());
    let i32c = WirExpr::ConstI32;
    let i64c = WirExpr::ConstI64;
    let bin = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
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
    // `len` (the clamped window size, i32) sign-extended for the i64 check/message.
    let len_i64 = || WirExpr::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("len")) };
    // `j` narrowed to i32 — valid only inside the checked range.
    let j_i32 = || WirExpr::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(getl("j")) };
    let mut body = view_clamp_setup(&getl);
    // A negative `len` (empty window) reads as 0 for both the bound and the message.
    body.push(WirNode::If {
        cond: bin(BinOp::Lt, getl("len"), i32c(0)),
        then_: vec![WirNode::SetLocal { local: "len".into(), value: i32c(0) }],
        els: vec![],
        result: None,
    });
    body.push(WirNode::If {
        // The index is i64 (matching `$list_at`): an out-of-i32-range `j` must trap
        // and carry its true value, not wrap. i64 comparisons yield i32 -> `i32.or`.
        cond: bin(
            BinOp::Or,
            bin64(BinOp::Lt, getl("j"), i64c(0)),
            bin64(BinOp::Ge, getl("j"), len_i64()),
        ),
        // (RFC-0045) A view read past its clamped window is an out-of-range read of
        // the materialized copy the interpreter holds: `list index {j} out of
        // bounds (length {len})`.
        then_: abort_nodes(DiagTemplate::ListIndexOob, getl("j"), len_i64(), i32c(0)),
        els: vec![],
        result: None,
    });
    body.push(WirNode::Push(WirExpr::Load {
        ptr: Box::new(bin(
            BinOp::Add,
            bin(BinOp::Add, getl("src"), i32c(4)),
            bin(BinOp::Mul, bin(BinOp::Add, getl("off"), j_i32()), i32c(8)),
        )),
        kind: Kind::I64,
        offset: 0,
    }));
    WirFunc {
        name: "list_at_view".into(),
        params: vec![
            WirLocal { name: "src".into(), ty: WirTy::Bool },
            WirLocal { name: "lo".into(), ty: WirTy::Bool },
            WirLocal { name: "hi".into(), ty: WirTy::Bool },
            WirLocal { name: "j".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Int], // i64 slot
        locals: vec![
            WirLocal { name: "srclen".into(), ty: WirTy::Bool },
            WirLocal { name: "off".into(), ty: WirTy::Bool },
            WirLocal { name: "end".into(), ty: WirTy::Bool },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
        ],
        body,
        raw_body: None,
    }
}

/// `$list_len_view(src: i32, lo: i32, hi: i32) -> i32` — the element count of a
/// confined slice view: `max(0, min(hi, len(src)) - max(lo, 0))`. Matches the
/// length of the materialized `list.slice` copy.
pub fn list_len_view_helper() -> WirFunc {
    let getl = |n: &str| WirExpr::GetLocal(n.into());
    let i32c = WirExpr::ConstI32;
    let bin = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let mut body = view_clamp_setup(&getl);
    // clamp the (possibly negative) window length to 0 for an empty view
    body.push(WirNode::If {
        cond: bin(BinOp::Lt, getl("len"), i32c(0)),
        then_: vec![WirNode::SetLocal { local: "len".into(), value: i32c(0) }],
        els: vec![],
        result: None,
    });
    body.push(WirNode::Push(getl("len")));
    WirFunc {
        name: "list_len_view".into(),
        params: vec![
            WirLocal { name: "src".into(), ty: WirTy::Bool },
            WirLocal { name: "lo".into(), ty: WirTy::Bool },
            WirLocal { name: "hi".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool], // i32 count
        locals: vec![
            WirLocal { name: "srclen".into(), ty: WirTy::Bool },
            WirLocal { name: "off".into(), ty: WirTy::Bool },
            WirLocal { name: "end".into(), ty: WirTy::Bool },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
        ],
        body,
        raw_body: None,
    }
}

/// `$int_to_string(n: i64) -> i32` — render a signed integer to a fresh witchy
/// string (`[i32 len][ascii]`). Mirrors `INT_TO_STRING_WAT`: `0` is a fast path;
/// otherwise count digits (a div-by-10 loop), allocate `[len][digits]`, write the
/// optional `-` then the digits back-to-front (a second div/rem loop). Calls
/// `$ensure`; uses the `$heap` global; byte writes via `Store8`.
pub fn int_to_string_helper(checked: bool) -> WirFunc {
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
pub fn float_cmp_helper(name: &str, op: BinOp) -> WirFunc {
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
pub fn int_div_helper() -> WirFunc {
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
pub fn int_rem_helper() -> WirFunc {
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
pub fn float_to_int_helper() -> WirFunc {
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


/// `$concat(a: i32, b: i32) -> i32` — allocate a fresh `[alen+blen][a..b..]`
/// string and `memory.copy` both operands in. Mirrors `CONCAT_WAT`. Calls
/// `$ensure`; uses the `$heap` global.
pub fn concat_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let add = |l: E, r: E| E::Binary {
        op: BinOp::Add,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let load_i32 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    WirFunc {
        name: "concat".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Str },
            WirLocal { name: "b".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str], // i32 pointer
        locals: vec![
            WirLocal { name: "alen".into(), ty: WirTy::Bool },
            WirLocal { name: "blen".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "alen".into(), value: load_i32(getl("a")) },
            N::SetLocal { local: "blen".into(), value: load_i32(getl("b")) },
            // (RFC-0016) allocate through `$rc_alloc` (header at res-4 + free-list reuse).
            N::SetLocal {
                local: "res".into(),
                value: E::Call {
                    func: "rc_alloc".into(),
                    args: vec![add(i32c(4), add(getl("alen"), getl("blen")))],
                },
            },
            // header: total length at res+0
            N::Store {
                ptr: getl("res"),
                value: add(getl("alen"), getl("blen")),
                kind: Kind::I32,
                offset: 0,
            },
            // copy a's bytes to res+4
            N::MemoryCopy {
                dest: add(getl("res"), i32c(4)),
                src: add(getl("a"), i32c(4)),
                len: getl("alen"),
            },
            // copy b's bytes to res+4+alen
            N::MemoryCopy {
                dest: add(add(getl("res"), i32c(4)), getl("alen")),
                src: add(getl("b"), i32c(4)),
                len: getl("blen"),
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

// ---------------------------------------------------------------------------
// (RFC-0051 / RFC-0073) THE IN-PLACE `*_cap` FAMILY — retained, and CLOSED to
// extension. Each `self_*` shape recognizer in
// `crates/witchy-lower/src/analysis.rs` (self_push_elem, self_insert_args,
// self_update_args, self_set_at, self_update_at, self_concat_pieces) pairs
// with one `*_cap` helper here: list_push_cap, dict_insert_cap,
// dict_update_cap, list_set_cap, list_update_cap, str_append_cap. RFC-0051
// (I3) measured deleting this family and found the general reclamation path
// perf-negative (OOM-traps several benchmarks), so the family is load-bearing
// — but the forward rule stands: add NO new per-method fast paths; the
// general ownership mechanism (let/var/own + escape analysis, RFC-0016) must
// absorb every NEW operation. The recognizers' contracts are unit-tested in
// analysis.rs `shape_matcher_tests`. Full rationale: RFC-0051 and the
// retention note at the top of analysis.rs.
// ---------------------------------------------------------------------------

/// `$list_push_cap(list: i32, x: i64, cap: i32) -> (i32, i32)` — the in-place
/// list append: if `cap > len` mutate `list` in place (return it + `cap`), else
/// grow to a doubled buffer (return the new ptr + newcap). Increments the
/// observable `$__witchy_reowns` counter when entered with a zero cap token (the
/// re-own signal). Mirrors `LIST_PUSH_CAP_WAT`; the multi-value early `return` is
/// restructured into `ret_ptr`/`ret_cap` locals + a dual tail `Push` (WIR has no
/// multi-value `If`/`Return`). Calls `$ensure`; uses `$heap` + `$__witchy_reowns`.
pub fn list_push_cap_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b32 = |op: BinOp, l: E, r: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    // cap == 0 → bump the re-own counter.
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
    // cap > len: mutate `list` in place.
    // (RFC-0005 step 2) Bound the in-place write against the buffer's REAL allocated
    // size (`[list-4]`, low 24 bits — the `$rc_alloc` header). `cap > len` gates this
    // path, but `cap` is the ownership analysis's CLAIM; a false negative could hand us
    // a `cap` that overstates the real allocation, and the element store at index `len`
    // writes bytes `[list+len*8+4, list+len*8+12)` — past the block, silently corrupting
    // adjacent heap. Trap instead, exactly as `$list_at` traps an out-of-bounds read: a
    // miscompile becomes a loud, parity-identical error, never a different silent answer.
    // Sound: when `cap` equals the true capacity, `len < cap` implies `len*8+12 <= size`,
    // so a correct program never trips it. The header read is safe here — the in-place
    // path only ever runs on a heap-allocated unique buffer.
    let inplace = vec![
        N::If {
            cond: b32(
                BinOp::GtU,
                b32(BinOp::Add, b32(BinOp::Mul, getl("len"), i32c(8)), i32c(12)),
                b32(
                    BinOp::And,
                    E::Load { ptr: Box::new(b32(BinOp::Sub, getl("list"), i32c(4))), kind: Kind::I32, offset: 0 },
                    i32c(RC_SIZE_MASK),
                ),
            ),
            then_: vec![N::Unreachable],
            els: vec![],
            result: None,
        },
        N::Store {
            ptr: b32(BinOp::Add, getl("list"), b32(BinOp::Mul, getl("len"), i32c(8))),
            value: getl("x"),
            kind: Kind::I64,
            offset: 4,
        },
        N::Store {
            ptr: getl("list"),
            value: b32(BinOp::Add, getl("len"), i32c(1)),
            kind: Kind::I32,
            offset: 0,
        },
        N::SetLocal { local: "ret_ptr".into(), value: getl("list") },
        N::SetLocal { local: "ret_cap".into(), value: getl("cap") },
    ];
    // else: grow to a doubled buffer.
    let grow = vec![
        N::SetLocal {
            local: "newcap".into(),
            value: b32(BinOp::Mul, b32(BinOp::Add, getl("len"), i32c(1)), i32c(2)),
        },
        N::If {
            cond: b32(BinOp::Lt, getl("newcap"), i32c(8)),
            then_: vec![N::SetLocal { local: "newcap".into(), value: i32c(8) }],
            els: vec![],
            result: None,
        },
        // (RFC-0016) allocate the grown buffer through `$rc_alloc` (header at new-8/new-4
        // + free-list reuse); it reserves + bumps `$heap`, so the manual ensure/bump go.
        N::SetLocal {
            local: "new".into(),
            value: E::Call { func: "rc_alloc".into(), args: vec![b32(BinOp::Add, i32c(4), b32(BinOp::Mul, getl("newcap"), i32c(8)))] },
        },
        N::Store {
            ptr: getl("new"),
            value: b32(BinOp::Add, getl("len"), i32c(1)),
            kind: Kind::I32,
            offset: 0,
        },
        N::MemoryCopy {
            dest: b32(BinOp::Add, getl("new"), i32c(4)),
            src: b32(BinOp::Add, getl("list"), i32c(4)),
            len: b32(BinOp::Mul, getl("len"), i32c(8)),
        },
        N::Store {
            ptr: b32(BinOp::Add, getl("new"), b32(BinOp::Mul, getl("len"), i32c(8))),
            value: getl("x"),
            kind: Kind::I64,
            offset: 4,
        },
        N::SetLocal { local: "ret_ptr".into(), value: getl("new") },
        N::SetLocal { local: "ret_cap".into(), value: getl("newcap") },
    ];
    WirFunc {
        name: "list_push_cap".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "x".into(), ty: WirTy::Int }, // i64 slot
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool], // (result i32 i32)
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Bool },
            WirLocal { name: "newcap".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_ptr".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_cap".into(), ty: WirTy::Bool },
        ],
        body: vec![
            reowns_bump,
            N::SetLocal {
                local: "len".into(),
                value: E::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 },
            },
            N::If {
                cond: b32(BinOp::Gt, getl("cap"), getl("len")),
                then_: inplace,
                els: grow,
                result: None,
            },
            N::Push(getl("ret_ptr")),
            N::Push(getl("ret_cap")),
        ],
        raw_body: None,
    }
}

/// `$list_set_cap(list, index, x, cap) -> (i32, i32)` — the in-place element
/// setter mirroring [`list_push_cap_helper`]. `list.set_at` returns a copy with
/// slot `index` replaced; with owned slack (`cap > 0`) the owned buffer is
/// mutated in place (slot at `list + 4 + index*8`) and returned with its cap,
/// else the buffer is copied once to a doubled buffer (re-own, bumping
/// `$__witchy_reowns`) and the slot written there. An out-of-range index leaves
/// the list unchanged (no copy, no trap — matching the stdlib semantics).
/// Calls `$ensure`; uses `$heap` + `$__witchy_reowns`.
pub fn list_set_cap_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    // element slot pointer for `index` in buffer `base`: base + 4 + index*8.
    let slot = |base: &str| b(BinOp::Add, b(BinOp::Add, getl(base), i32c(4)), b(BinOp::Mul, getl("index"), i32c(8)));
    let inplace = vec![N::Store { ptr: slot("list"), value: getl("x"), kind: Kind::I64, offset: 0 }];
    let grow = vec![
        N::SetGlobal {
            global: "__witchy_reowns".into(),
            value: E::Binary { op: BinOp::Add, kind: Kind::I64, lhs: Box::new(E::GetGlobal("__witchy_reowns".into())), rhs: Box::new(i64c(1)) },
        },
        N::SetLocal { local: "newcap".into(), value: b(BinOp::Mul, getl("len"), i32c(2)) },
        N::If { cond: b(BinOp::Lt, getl("newcap"), i32c(8)), then_: vec![N::SetLocal { local: "newcap".into(), value: i32c(8) }], els: vec![], result: None },
        N::SetLocal { local: "new".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("newcap"), i32c(8)))] } },
        N::Store { ptr: getl("new"), value: getl("len"), kind: Kind::I32, offset: 0 },
        N::MemoryCopy { dest: b(BinOp::Add, getl("new"), i32c(4)), src: b(BinOp::Add, getl("list"), i32c(4)), len: b(BinOp::Mul, getl("len"), i32c(8)) },
        N::Store { ptr: slot("new"), value: getl("x"), kind: Kind::I64, offset: 0 },
        N::SetLocal { local: "ret_ptr".into(), value: getl("new") },
        N::SetLocal { local: "ret_cap".into(), value: getl("newcap") },
    ];
    WirFunc {
        name: "list_set_cap".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "index".into(), ty: WirTy::Bool },
            WirLocal { name: "x".into(), ty: WirTy::Int },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Bool },
            WirLocal { name: "newcap".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_ptr".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_cap".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 } },
            N::SetLocal { local: "ret_ptr".into(), value: getl("list") },
            N::SetLocal { local: "ret_cap".into(), value: getl("cap") },
            N::If {
                cond: b(BinOp::And, b(BinOp::Ge, getl("index"), i32c(0)), b(BinOp::Lt, getl("index"), getl("len"))),
                then_: vec![N::If { cond: b(BinOp::Gt, getl("cap"), i32c(0)), then_: inplace, els: grow, result: None }],
                els: vec![],
                result: None,
            },
            N::Push(getl("ret_ptr")),
            N::Push(getl("ret_cap")),
        ],
        raw_body: None,
    }
}

/// `$list_update_cap(list, index, clos, cap) -> (i32, i32)` — the in-place
/// element updater: apply the closure to the current element and store the
/// result back, mirroring [`list_set_cap_helper`] (in place with owned slack,
/// else copy-and-reown, bumping `$__witchy_reowns`). The closure call mirrors
/// `$dict_update_cap` (`CallIndirect` on the closure's code index, the element
/// passed as the i64 slot). An out-of-range index leaves the list unchanged and
/// does not call the closure. Calls `$ensure`; uses `$heap` + `$__witchy_reowns`.
pub fn list_update_cap_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let slot = |base: &str| b(BinOp::Add, b(BinOp::Add, getl(base), i32c(4)), b(BinOp::Mul, getl("index"), i32c(8)));
    // nv = clos(load(slot(list))): apply the updater to the current element.
    let call_clos = N::SetLocal {
        local: "nv".into(),
        value: E::CallIndirect {
            signature: gc_slot_closure_signature(1, 1),
            args: vec![getl("clos"), E::Load { ptr: Box::new(slot("list")), kind: Kind::I64, offset: 0 }],
            index: Box::new(E::StructGet {
                struct_id: 0,
                field: CLOSURE_CODE_FIELD,
                base: Box::new(getl("clos")),
            }),
        },
    };
    let inplace = vec![N::Store { ptr: slot("list"), value: getl("nv"), kind: Kind::I64, offset: 0 }];
    let grow = vec![
        N::SetGlobal {
            global: "__witchy_reowns".into(),
            value: E::Binary { op: BinOp::Add, kind: Kind::I64, lhs: Box::new(E::GetGlobal("__witchy_reowns".into())), rhs: Box::new(i64c(1)) },
        },
        N::SetLocal { local: "newcap".into(), value: b(BinOp::Mul, getl("len"), i32c(2)) },
        N::If { cond: b(BinOp::Lt, getl("newcap"), i32c(8)), then_: vec![N::SetLocal { local: "newcap".into(), value: i32c(8) }], els: vec![], result: None },
        N::SetLocal { local: "nb".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("newcap"), i32c(8)))] } },
        N::Store { ptr: getl("nb"), value: getl("len"), kind: Kind::I32, offset: 0 },
        N::MemoryCopy { dest: b(BinOp::Add, getl("nb"), i32c(4)), src: b(BinOp::Add, getl("list"), i32c(4)), len: b(BinOp::Mul, getl("len"), i32c(8)) },
        N::Store { ptr: b(BinOp::Add, b(BinOp::Add, getl("nb"), i32c(4)), b(BinOp::Mul, getl("index"), i32c(8))), value: getl("nv"), kind: Kind::I64, offset: 0 },
        N::SetLocal { local: "ret_ptr".into(), value: getl("nb") },
        N::SetLocal { local: "ret_cap".into(), value: getl("newcap") },
    ];
    WirFunc {
        name: "list_update_cap".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "index".into(), ty: WirTy::Bool },
            WirLocal { name: "clos".into(), ty: WirTy::GcRef(0) },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "nv".into(), ty: WirTy::Int },
            WirLocal { name: "nb".into(), ty: WirTy::Bool },
            WirLocal { name: "newcap".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_ptr".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_cap".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 } },
            N::SetLocal { local: "ret_ptr".into(), value: getl("list") },
            N::SetLocal { local: "ret_cap".into(), value: getl("cap") },
            N::If {
                cond: b(BinOp::And, b(BinOp::Ge, getl("index"), i32c(0)), b(BinOp::Lt, getl("index"), getl("len"))),
                then_: vec![
                    call_clos,
                    N::If { cond: b(BinOp::Gt, getl("cap"), i32c(0)), then_: inplace, els: grow, result: None },
                ],
                els: vec![],
                result: None,
            },
            N::Push(getl("ret_ptr")),
            N::Push(getl("ret_cap")),
        ],
        raw_body: None,
    }
}

/// `$str_append_cap(s: i32, piece: i32, cap: i32) -> (i32, i32)` — the in-place
/// string builder: a String is `[len(i32)][bytes]`. If the owned byte slack
/// (`cap`) covers `len + plen`, copy `piece`'s bytes into `s` in place and bump
/// its length (return `s` + `cap`); else grow to a doubled buffer. Bumps
/// `$__witchy_reowns` on a zero cap. Mirrors `STR_APPEND_CAP_WAT`; multi-value
/// early `return` restructured into `ret_ptr`/`ret_cap` + a dual tail Push.
/// Calls `$ensure`; uses `$heap` + `$__witchy_reowns`.
pub fn str_append_cap_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
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
    // cap >= need: append `piece`'s bytes at s+4+len in place.
    // (RFC-0005 step 2) Bound the in-place copy against the buffer's REAL allocated
    // size (`[s-4]`, low 24 bits). `cap >= need` gates this path on the analysis's
    // CLAIMED capacity; a false negative could overstate it, and the copy would write
    // `[s+4+need)` past the block, silently corrupting adjacent heap. Trap instead.
    // Sound: with a correct `cap >= need` and `cap` == the real capacity, `4+need <=
    // size`, so a correct program never trips it. GUARD on `cap != 0`: an EMPTY
    // reown (`cap == 0`) reaches this path only for `need == 0` (a no-op append onto
    // an interned/static `""` whose `[s-4]` is NOT an rc header), so it copies zero
    // bytes and must not be bounds-checked; a positive `cap` is always a real heap
    // buffer, where an overflow is possible and the header is valid.
    let inplace = vec![
        N::If {
            cond: getl("cap"),
            then_: vec![N::If {
                cond: b(
                    BinOp::GtU,
                    b(BinOp::Add, i32c(4), getl("need")),
                    b(BinOp::And, load(b(BinOp::Sub, getl("s"), i32c(4))), i32c(RC_SIZE_MASK)),
                ),
                then_: vec![N::Unreachable],
                els: vec![],
                result: None,
            }],
            els: vec![],
            result: None,
        },
        N::MemoryCopy {
            dest: b(BinOp::Add, b(BinOp::Add, getl("s"), i32c(4)), getl("len")),
            src: b(BinOp::Add, getl("piece"), i32c(4)),
            len: getl("plen"),
        },
        N::Store { ptr: getl("s"), value: getl("need"), kind: Kind::I32, offset: 0 },
        N::SetLocal { local: "ret_ptr".into(), value: getl("s") },
        N::SetLocal { local: "ret_cap".into(), value: getl("cap") },
    ];
    // else: copy `s` then `piece` into a fresh doubled buffer.
    let grow = vec![
        N::SetLocal { local: "newcap".into(), value: b(BinOp::Mul, getl("need"), i32c(2)) },
        N::If {
            cond: b(BinOp::Lt, getl("newcap"), i32c(16)),
            then_: vec![N::SetLocal { local: "newcap".into(), value: i32c(16) }],
            els: vec![],
            result: None,
        },
        N::SetLocal { local: "new".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), getl("newcap"))] } },
        N::Store { ptr: getl("new"), value: getl("need"), kind: Kind::I32, offset: 0 },
        N::MemoryCopy {
            dest: b(BinOp::Add, getl("new"), i32c(4)),
            src: b(BinOp::Add, getl("s"), i32c(4)),
            len: getl("len"),
        },
        N::MemoryCopy {
            dest: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), getl("len")),
            src: b(BinOp::Add, getl("piece"), i32c(4)),
            len: getl("plen"),
        },
        N::SetLocal { local: "ret_ptr".into(), value: getl("new") },
        N::SetLocal { local: "ret_cap".into(), value: getl("newcap") },
    ];
    WirFunc {
        name: "str_append_cap".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Bool },
            WirLocal { name: "piece".into(), ty: WirTy::Bool },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool],
        locals: ["len", "plen", "need", "new", "newcap", "ret_ptr", "ret_cap"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            reowns_bump,
            N::SetLocal { local: "len".into(), value: load(getl("s")) },
            N::SetLocal { local: "plen".into(), value: load(getl("piece")) },
            N::SetLocal { local: "need".into(), value: b(BinOp::Add, getl("len"), getl("plen")) },
            N::If {
                cond: b(BinOp::Ge, getl("cap"), getl("need")),
                then_: inplace,
                els: grow,
                result: None,
            },
            N::Push(getl("ret_ptr")),
            N::Push(getl("ret_cap")),
        ],
        raw_body: None,
    }
}

/// `$list_push(list: i32, x: i64) -> i32` — the non-in-place append: always
/// allocates a fresh `(len+1)`-element buffer, copies the existing elements,
/// writes `x` in the new tail slot, and returns the new pointer. (The in-place
/// optimization lives in `$list_push_cap`; this is the plain fallback used by
/// helpers like `$split`/`$str_chars` that build lists internally.)
pub fn list_push_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "list_push".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "x".into(), ty: WirTy::Int }, // i64 slot
        ],
        ret: vec![WirTy::Bool], // i32 pointer
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "len".into(),
                value: E::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 },
            },
            // (RFC-0016) allocate the new buffer through `$rc_alloc` (header at new-4,
            // bumps `$heap`) so a confined list overwritten by a fresh push is freeable.
            N::SetLocal {
                local: "new".into(),
                value: E::Call {
                    func: "rc_alloc".into(),
                    args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, b(BinOp::Add, getl("len"), i32c(1)), i32c(8)))],
                },
            },
            N::Store {
                ptr: getl("new"),
                value: b(BinOp::Add, getl("len"), i32c(1)),
                kind: Kind::I32,
                offset: 0,
            },
            N::MemoryCopy {
                dest: b(BinOp::Add, getl("new"), i32c(4)),
                src: b(BinOp::Add, getl("list"), i32c(4)),
                len: b(BinOp::Mul, getl("len"), i32c(8)),
            },
            N::Store {
                ptr: b(BinOp::Add, getl("new"), b(BinOp::Mul, getl("len"), i32c(8))),
                value: getl("x"),
                kind: Kind::I64,
                offset: 4,
            },
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}


/// `$substr(src, start, len) -> i32` — a fresh string holding `len` bytes of
/// `src` starting at *byte* offset `start`. Allocates `4 + len` via `$ensure`,
/// writes the length header, `memory.copy`s the slice, and bumps `$heap`.
pub fn substr_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let add = |l: E, r: E| E::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "substr".into(),
        params: vec![
            WirLocal { name: "src".into(), ty: WirTy::Str },
            WirLocal { name: "start".into(), ty: WirTy::Bool },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Str],
        locals: vec![WirLocal { name: "res".into(), ty: WirTy::Bool }],
        body: vec![
            // (RFC-0016) allocate through `$rc_alloc` (header + reuse).
            N::SetLocal {
                local: "res".into(),
                value: E::Call { func: "rc_alloc".into(), args: vec![add(i32c(4), getl("len"))] },
            },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::MemoryCopy {
                dest: add(getl("res"), i32c(4)),
                src: add(add(getl("src"), i32c(4)), getl("start")),
                len: getl("len"),
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}



/// `$str_substring(s, start, end) -> i32` — the substring of `s` between the
/// *character* indices `start` and `end` (both full-width i64). Maps both ends to
/// byte offsets via `$char_to_byte`, which clamps each index to `[0, char_count]`
/// in i64 (mirroring the interpreter's `max(0).min(len)`), then `$substr`s the byte
/// slice; an empty slice when the bounds cross.
pub fn str_substring_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let c2b = |idx: &str| E::Call { func: "char_to_byte".into(), args: vec![getl("s"), getl(idx)] };
    WirFunc {
        name: "str_substring".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "start".into(), ty: WirTy::Int },
            WirLocal { name: "end".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "lo".into(), ty: WirTy::Bool },
            WirLocal { name: "hi".into(), ty: WirTy::Bool },
        ],
        body: vec![
            setl("lo", c2b("start")),
            setl("hi", c2b("end")),
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

/// `$is_ws(b) -> i32` — 1 iff byte `b` is ASCII whitespace (space, tab, LF, CR,
/// VT, FF). A pure OR of equalities, no loop.
pub fn is_ws_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let or = |l: E, r: E| E::Binary { op: BinOp::Or, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let eq = |c: i32| E::Binary {
        op: BinOp::Eq,
        kind: Kind::I32,
        lhs: Box::new(getl("b")),
        rhs: Box::new(i32c(c)),
    };
    WirFunc {
        name: "is_ws".into(),
        params: vec![WirLocal { name: "b".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![N::Push(or(
            eq(32),
            or(eq(9), or(eq(10), or(eq(13), or(eq(11), eq(12))))),
        ))],
        raw_body: None,
    }
}

/// `$trim(s) -> i32` — `s` with leading and trailing ASCII whitespace removed.
/// Advances `lo` past leading whitespace and pulls `hi` in past trailing
/// whitespace, then `$substr`s the `[lo, hi)` byte window.
pub fn trim_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let not = |e: E| E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(e) };
    let is_ws_at = |idx: E| E::Call {
        func: "is_ws".into(),
        args: vec![E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), idx)), offset: 4 }],
    };
    let lo_loop = N::Block {
        label: "lodone".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "lodone".into(), cond: Some(b(BinOp::Ge, getl("lo"), getl("hi"))) },
                N::Br { target: "lodone".into(), cond: Some(not(is_ws_at(getl("lo")))) },
                setl("lo", b(BinOp::Add, getl("lo"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    let hi_loop = N::Block {
        label: "hidone".into(),
        result: None,
        body: vec![N::Loop {
            label: "h".into(),
            body: vec![
                N::Br { target: "hidone".into(), cond: Some(b(BinOp::Le, getl("hi"), getl("lo"))) },
                N::Br {
                    target: "hidone".into(),
                    cond: Some(not(is_ws_at(b(BinOp::Sub, getl("hi"), i32c(1))))),
                },
                setl("hi", b(BinOp::Sub, getl("hi"), i32c(1))),
                N::Br { target: "h".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "trim".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Str],
        locals: ["len", "lo", "hi"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("len", load(getl("s"))),
            setl("lo", i32c(0)),
            setl("hi", getl("len")),
            lo_loop,
            hi_loop,
            N::Push(E::Call {
                func: "substr".into(),
                args: vec![getl("s"), getl("lo"), b(BinOp::Sub, getl("hi"), getl("lo"))],
            }),
        ],
        raw_body: None,
    }
}

/// `$split(s, sep) -> i32` — a `List(String)` of the pieces of `s` between
/// occurrences of `sep`. Empty `sep` yields `[s]`. Mirrors `$find_byte`'s
/// scan/compare loop nest; on each match it `$substr`s the piece and `$list_push`es
/// it, then `$substr`s the trailing piece after the loop. The substr pointer is
/// zero-extended into the list's i64 slot (a pointer, so the sign of the extend
/// is immaterial — the reader `i32.wrap_i64`s it back).
pub fn split_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let ext = |e: E| E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
    // Write the piece `substr(s, start, len)` straight into slot `count` of the PRE-SIZED
    // `result` buffer, then bump `count`. No `$list_push` reallocation (that allocated a fresh
    // `len+1` buffer and memcpy'd the whole list every piece — O(pieces²) to build); slots are
    // written directly, exactly like `$str_chars`.
    let push_piece = |start: E, len: E| -> Vec<N> {
        vec![
            N::Store {
                ptr: b(BinOp::Add, getl("result"), b(BinOp::Mul, getl("count"), i32c(8))),
                value: ext(E::Call { func: "substr".into(), args: vec![getl("s"), start, len] }),
                kind: Kind::I64,
                offset: 4,
            },
            setl("count", b(BinOp::Add, getl("count"), i32c(1))),
        ]
    };
    let s_byte = E::Load8U {
        ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("i"), getl("j")))),
        offset: 4,
    };
    let sep_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("sep"), getl("j"))), offset: 4 };
    let cmp_loop = N::Block {
        label: "cmpdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cmp".into(),
            body: vec![
                N::Br { target: "cmpdone".into(), cond: Some(b(BinOp::Ge, getl("j"), getl("seplen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, sep_byte),
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
                    cond: Some(b(BinOp::Gt, getl("i"), b(BinOp::Sub, getl("slen"), getl("seplen")))),
                },
                setl("match", i32c(1)),
                setl("j", i32c(0)),
                cmp_loop,
                N::If {
                    cond: getl("match"),
                    then_: {
                        let mut t = push_piece(getl("start"), b(BinOp::Sub, getl("i"), getl("start")));
                        t.push(setl("i", b(BinOp::Add, getl("i"), getl("seplen"))));
                        t.push(setl("start", getl("i")));
                        t
                    },
                    els: vec![setl("i", b(BinOp::Add, getl("i"), i32c(1)))],
                    result: None,
                },
                N::Br { target: "scan".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "split".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "sep".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool], // i32 list pointer
        locals: ["slen", "seplen", "result", "count", "start", "i", "j", "match"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: {
            // pieces <= slen + 1, so reserve that many slots up front (a `with_capacity`) and
            // CLAIM the region — each piece's `substr` then allocates above the buffer and never
            // clobbers a written slot.
            let cap_slots = b(BinOp::Mul, b(BinOp::Add, getl("slen"), i32c(1)), i32c(8));
            let mut body = vec![
                setl("slen", load(getl("s"))),
                setl("seplen", load(getl("sep"))),
                // (RFC-0016) claim the buffer region through `$rc_alloc` (header + slots,
                // bumps `$heap` past them) so each piece's `substr` allocates above it.
                setl("result", E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), cap_slots)] }),
                setl("count", i32c(0)),
                // empty sep -> [s]: slot 0 = s, length 1.
                N::If {
                    cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("seplen")) },
                    then_: vec![
                        N::Store { ptr: getl("result"), value: ext(getl("s")), kind: Kind::I64, offset: 4 },
                        N::Store { ptr: getl("result"), value: i32c(1), kind: Kind::I32, offset: 0 },
                        N::Return(Some(getl("result"))),
                    ],
                    els: vec![],
                    result: None,
                },
                setl("start", i32c(0)),
                setl("i", i32c(0)),
                scan_loop,
            ];
            // the trailing piece [start, slen), then write the real length.
            body.extend(push_piece(getl("start"), b(BinOp::Sub, getl("slen"), getl("start"))));
            body.push(N::Store { ptr: getl("result"), value: getl("count"), kind: Kind::I32, offset: 0 });
            body.push(N::Push(getl("result")));
            body
        },
        raw_body: None,
    }
}

/// `$str_chars(s) -> i32` — a `List(String)` of `s`'s individual characters.
/// Counts characters via `$byte_to_char`, then `$str_substring`s each single-char
/// `[i, i+1)` window and `$list_push`es it (the substring handles multibyte
/// characters correctly).
pub fn str_chars_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let ext = |e: E| E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
    // seqlen = lead<0x80 ? 1 : lead<0xe0 ? 2 : lead<0xf0 ? 3 : 4 — the UTF-8 byte width of
    // the character whose lead byte is `b` (same branching as `$char_to_byte`).
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
    // ONE pass over the UTF-8 BYTES, writing each char straight into a PRE-SIZED buffer.
    // The character count is at most the byte length, so we reserve `byte_len` slots up front
    // (a Rust `Vec::with_capacity`) and `i64.store` each `substr(s, bi, seqlen)` directly into
    // the next slot — no per-char reallocation. The OLD body grew the list with the copying
    // `$list_push` (a fresh `len+1` buffer + memcpy every char), so building the list was
    // O(n^2); combined with the previous O(j) char-indexed read it was doubly quadratic. This
    // matters because every text/JSON parser scans `string.chars(s)` then indexes it in O(1).
    //
    // The reservation claims the buffer region (bumps `$heap` past `byte_len` slots) BEFORE the
    // loop, so the per-char `substr` allocations land ABOVE the buffer and never clobber a slot
    // already written. Multibyte input leaves a few unused tail slots (count < byte_len); that
    // slack is harmless and reclaimed when the bump pointer resets between calls.
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("bi"), getl("slen"))) },
                setl("b", E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("bi"))), offset: 4 }),
                seqlen,
                // result[count] = substr(s, bi, seqlen)  — the slot store's `offset: 4` skips the
                // length header, so the effective address is `result + 4 + count*8`.
                N::Store {
                    ptr: b(BinOp::Add, getl("result"), b(BinOp::Mul, getl("count"), i32c(8))),
                    value: ext(E::Call {
                        func: "substr".into(),
                        args: vec![getl("s"), getl("bi"), getl("seqlen")],
                    }),
                    kind: Kind::I64,
                    offset: 4,
                },
                setl("count", b(BinOp::Add, getl("count"), i32c(1))),
                setl("bi", b(BinOp::Add, getl("bi"), getl("seqlen"))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "str_chars".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Bool], // i32 list pointer
        locals: ["slen", "bi", "b", "seqlen", "count", "result"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("slen", load(getl("s"))),
            // Reserve header + `byte_len` slots (an upper bound on the char count) and CLAIM the
            // region, so each char's `substr` allocates above it.
            // (RFC-0016) claim the buffer region through `$rc_alloc` — it reserves the
            // header + slots and bumps `$heap` past them, so each char's `substr` still
            // allocates ABOVE the buffer (or a distinct free-list block) and never
            // clobbers a written slot.
            setl("result", E::Call {
                func: "rc_alloc".into(),
                args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("slen"), i32c(8)))],
            }),
            setl("count", i32c(0)),
            setl("bi", i32c(0)),
            scan_loop,
            // The real length is the actual character count.
            N::Store { ptr: getl("result"), value: getl("count"), kind: Kind::I32, offset: 0 },
            N::Push(getl("result")),
        ],
        raw_body: None,
    }
}

/// `$list_concat(a, b) -> i32` — a fresh list holding `a`'s elements followed by
/// `b`'s. Like the string `$concat`, but elements are 8-byte slots: allocate
/// `(alen+blen)` slots, `memory.copy` each source array in turn, bump `$heap`.
pub fn list_concat_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let total = b(BinOp::Add, getl("alen"), getl("blen"));
    WirFunc {
        name: "list_concat".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Bool },
            WirLocal { name: "b".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool], // i32 list pointer
        locals: ["alen", "blen", "new"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("alen", load(getl("a"))),
            setl("blen", load(getl("b"))),
            // (RFC-0016) allocate through `$rc_alloc` (header at new-4 + free-list reuse).
            setl(
                "new",
                E::Call {
                    func: "rc_alloc".into(),
                    args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, total.clone(), i32c(8)))],
                },
            ),
            N::Store { ptr: getl("new"), value: total, kind: Kind::I32, offset: 0 },
            // a's elements → new+4
            N::MemoryCopy {
                dest: b(BinOp::Add, getl("new"), i32c(4)),
                src: b(BinOp::Add, getl("a"), i32c(4)),
                len: b(BinOp::Mul, getl("alen"), i32c(8)),
            },
            // b's elements → new+4 + alen*8
            N::MemoryCopy {
                dest: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, getl("alen"), i32c(8))),
                src: b(BinOp::Add, getl("b"), i32c(4)),
                len: b(BinOp::Mul, getl("blen"), i32c(8)),
            },
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$ascii_case(s, up) -> i32` — `s` with ASCII letters cased: `up != 0`
/// uppercases (`a`–`z` → `A`–`Z`), else lowercases. Non-letters and non-ASCII
/// bytes copy through unchanged (byte-wise, so multibyte UTF-8 is preserved).
pub fn ascii_case_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let in_range = |lo: i32, hi: i32| b(BinOp::And, b(BinOp::GeU, getl("b"), i32c(lo)), b(BinOp::LeU, getl("b"), i32c(hi)));
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("len"))) },
                setl("b", E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 }),
                N::If {
                    cond: getl("up"),
                    then_: vec![N::If {
                        cond: in_range(97, 122),
                        then_: vec![setl("b", b(BinOp::Sub, getl("b"), i32c(32)))],
                        els: vec![],
                        result: None,
                    }],
                    els: vec![N::If {
                        cond: in_range(65, 90),
                        then_: vec![setl("b", b(BinOp::Add, getl("b"), i32c(32)))],
                        els: vec![],
                        result: None,
                    }],
                    result: None,
                },
                N::Store8 { ptr: b(BinOp::Add, getl("res"), getl("i")), value: getl("b"), offset: 4 },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "ascii_case".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "up".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Str],
        locals: ["len", "i", "res", "b"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("len", load(getl("s"))),
            // (RFC-0016) allocate through `$rc_alloc` (header + reuse).
            setl("res", E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), getl("len"))] }),
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            scan_loop,
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$str_to_int(s) -> i64` — parse a (optionally signed) decimal integer,
/// tolerating leading/trailing ASCII whitespace. Traps (like Rust's checked
/// parse) on overflow, on no digits, or on trailing non-whitespace garbage —
/// matching the interpreter oracle, which errors on the same inputs.
pub fn str_to_int_helper() -> WirFunc {
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

// --- Dict helpers ------------------------------------------------------------
// A Dict pointer `d` addresses an i32 `count` at offset 0, then `count` 16-byte
// entries (i64 key at entry+0, i64 value at entry+8); entry i is at d+4+i*16.
// A hidden word at d-4 is 0 (linear scan) or an open-addressing index pointer.
// On the binary path only the non-`_cap` helpers are migrated, and none of them
// build an index, so d-4 stays 0 and `$dict_find` always takes the linear path —
// but the hash path is ported faithfully anyway so the helper is correct if a
// future cap-insert migration starts hanging an index.


/// `$match_at(s, from, pos) -> i32` — 1 iff `from` occurs in `s` starting at
/// byte offset `pos`. Bails to 0 if `from` would run off the end or any byte
/// differs.
pub fn match_at_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("pos"), getl("j")))), offset: 4 };
    let from_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("from"), getl("j"))), offset: 4 };
    let scan = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("j"), getl("flen"))) },
                N::If { cond: b(BinOp::Ne, s_byte, from_byte), then_: vec![N::Return(Some(i32c(0)))], els: vec![], result: None },
                setl("j", b(BinOp::Add, getl("j"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "match_at".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "from".into(), ty: WirTy::Str },
            WirLocal { name: "pos".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["flen", "j"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("flen", load(getl("from"))),
            N::If {
                cond: b(BinOp::Gt, b(BinOp::Add, getl("pos"), getl("flen")), load(getl("s"))),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("j", i32c(0)),
            scan,
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$replace(s, from, to) -> i32` — `s` with every occurrence of `from` replaced
/// by `to`. Empty `from` inserts `to` between every character (and at both ends),
/// stepping by UTF-8 sequence length. Otherwise counts matches via `$match_at`,
/// allocates the exact result, then copies through replacing each match.
pub fn replace_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_off = |off: E| b(BinOp::Add, b(BinOp::Add, getl("s"), i32c(4)), off);
    let to_bytes = b(BinOp::Add, getl("to"), i32c(4));
    let match_here = || E::Call { func: "match_at".into(), args: vec![getl("s"), getl("from"), getl("src")] };
    // seqlen(b) into `clen` — UTF-8 lead-byte classification.
    let seqlen = N::If {
        cond: b(BinOp::LtU, getl("b"), i32c(0x80)),
        then_: vec![setl("clen", i32c(1))],
        els: vec![N::If {
            cond: b(BinOp::LtU, getl("b"), i32c(0xe0)),
            then_: vec![setl("clen", i32c(2))],
            els: vec![N::If {
                cond: b(BinOp::LtU, getl("b"), i32c(0xf0)),
                then_: vec![setl("clen", i32c(3))],
                els: vec![setl("clen", i32c(4))],
                result: None,
            }],
            result: None,
        }],
        result: None,
    };
    // --- empty-`from` branch: insert `to` around every character. ---
    let empty_loop = N::Block {
        label: "cdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cl".into(),
            body: vec![
                N::Br { target: "cdone".into(), cond: Some(b(BinOp::Ge, getl("src"), getl("slen"))) },
                setl("b", E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("src"))), offset: 4 }),
                seqlen,
                N::MemoryCopy { dest: getl("dst"), src: s_off(getl("src")), len: getl("clen") },
                setl("dst", b(BinOp::Add, getl("dst"), getl("clen"))),
                N::MemoryCopy { dest: getl("dst"), src: to_bytes.clone(), len: getl("tlen") },
                setl("dst", b(BinOp::Add, getl("dst"), getl("tlen"))),
                setl("src", b(BinOp::Add, getl("src"), getl("clen"))),
                N::Br { target: "cl".into(), cond: None },
            ],
        }],
    };
    let empty_branch = vec![
        // `res` is the buffer allocated up front through `$rc_alloc`.
        setl("dst", b(BinOp::Add, getl("res"), i32c(4))),
        N::MemoryCopy { dest: getl("dst"), src: to_bytes.clone(), len: getl("tlen") },
        setl("dst", b(BinOp::Add, getl("dst"), getl("tlen"))),
        setl("src", i32c(0)),
        empty_loop,
        setl("reslen", b(BinOp::Sub, getl("dst"), b(BinOp::Add, getl("res"), i32c(4)))),
        N::Store { ptr: getl("res"), value: getl("reslen"), kind: Kind::I32, offset: 0 },
        N::Return(Some(getl("res"))),
    ];
    // --- non-empty `from`: count matches, then fill. ---
    let count_loop = N::Block {
        label: "countdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cl2".into(),
            body: vec![
                N::Br { target: "countdone".into(), cond: Some(b(BinOp::Gt, b(BinOp::Add, getl("src"), getl("flen")), getl("slen"))) },
                N::If {
                    cond: match_here(),
                    then_: vec![setl("cnt", b(BinOp::Add, getl("cnt"), i32c(1))), setl("src", b(BinOp::Add, getl("src"), getl("flen")))],
                    els: vec![setl("src", b(BinOp::Add, getl("src"), i32c(1)))],
                    result: None,
                },
                N::Br { target: "cl2".into(), cond: None },
            ],
        }],
    };
    let fill_loop = N::Block {
        label: "filldone".into(),
        result: None,
        body: vec![N::Loop {
            label: "fl".into(),
            body: vec![
                N::Br { target: "filldone".into(), cond: Some(b(BinOp::Ge, getl("src"), getl("slen"))) },
                N::If {
                    cond: match_here(),
                    then_: vec![
                        N::MemoryCopy { dest: getl("dst"), src: to_bytes.clone(), len: getl("tlen") },
                        setl("dst", b(BinOp::Add, getl("dst"), getl("tlen"))),
                        setl("src", b(BinOp::Add, getl("src"), getl("flen"))),
                    ],
                    els: vec![
                        N::Store8 { ptr: getl("dst"), value: E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("src"))), offset: 4 }, offset: 0 },
                        setl("dst", b(BinOp::Add, getl("dst"), i32c(1))),
                        setl("src", b(BinOp::Add, getl("src"), i32c(1))),
                    ],
                    result: None,
                },
                N::Br { target: "fl".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "replace".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "from".into(), ty: WirTy::Str },
            WirLocal { name: "to".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: ["slen", "flen", "tlen", "cnt", "src", "dst", "res", "reslen", "b", "clen"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("slen", load(getl("s"))),
            setl("flen", load(getl("from"))),
            setl("tlen", load(getl("to"))),
            // (RFC-0016) reserve the worst-case buffer (`4 + slen + (slen+1)*tlen`) through
            // `$rc_alloc` up front; both branches fill into `res` and write the true length
            // header (the block's size header stays the worst case; tail slack unused).
            setl("res", E::Call {
                func: "rc_alloc".into(),
                args: vec![b(BinOp::Add, b(BinOp::Add, i32c(4), getl("slen")), b(BinOp::Mul, b(BinOp::Add, getl("slen"), i32c(1)), getl("tlen")))],
            }),
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("flen")) },
                then_: empty_branch,
                els: vec![],
                result: None,
            },
            setl("cnt", i32c(0)),
            setl("src", i32c(0)),
            count_loop,
            setl("reslen", b(BinOp::Add, getl("slen"), b(BinOp::Mul, getl("cnt"), b(BinOp::Sub, getl("tlen"), getl("flen"))))),
            N::Store { ptr: getl("res"), value: getl("reslen"), kind: Kind::I32, offset: 0 },
            setl("dst", b(BinOp::Add, getl("res"), i32c(4))),
            setl("src", i32c(0)),
            fill_loop,
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$encoding(op, in) -> i32` — a thin wrapper over the host `encoding` import,
/// which does the actual hex/base64 transform over flat String/Bytes buffers.
/// Reserves a worst-case `2*len + 20` result buffer, lets the host write into
/// `res+4`, and caps the length header to what it returned. The first migrated
/// host-import helper.
pub fn encoding_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "encoding".into(),
        params: vec![
            WirLocal { name: "op".into(), ty: WirTy::Bool },
            WirLocal { name: "in".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "res".into(), ty: WirTy::Bool },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
        ],
        body: vec![
            // (RFC-0016) reserve the worst-case `2*len + 20` buffer through `$rc_alloc`;
            // the host writes into `res+4` and the length header caps to `n` (the block's
            // size header stays the worst case; the tail slack is unused).
            N::SetLocal {
                local: "res".into(),
                value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, b(BinOp::Mul, E::Load { ptr: Box::new(getl("in")), kind: Kind::I32, offset: 0 }, i32c(2)), i32c(20))] },
            },
            N::SetLocal {
                local: "n".into(),
                value: E::CallHost { import: "encoding".into(), args: vec![getl("op"), getl("in"), b(BinOp::Add, getl("res"), i32c(4))] },
            },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// Shared body for the fixed-length crypto digests: reserve `hexlen+4` bytes,
/// write the length header, hand the inputs + `res+4` to the host `import`, and
/// bump `$heap`. `inputs` are the string-pointer params (one for the plain
/// hashes, two — key, msg — for HMAC). The crypto imports are host-provided
/// unconditionally (hashing needs no capability).
fn crypto_hash_helper(name: &str, import: &str, hexlen: i32, inputs: &[&str]) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let mut host_args: Vec<E> = inputs.iter().map(|n| getl(n)).collect();
    host_args.push(b(BinOp::Add, getl("res"), i32c(4)));
    WirFunc {
        name: name.into(),
        params: inputs.iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Str }).collect(),
        ret: vec![WirTy::Str],
        locals: vec![WirLocal { name: "res".into(), ty: WirTy::Bool }],
        body: vec![
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(hexlen + 4)] } },
            N::Store { ptr: getl("res"), value: i32c(hexlen), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: import.into(), args: host_args }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// A keyed crypto op on a `Secret` — `crypto.sign(key, msg)` / `crypto.public_key(key)`.
/// `key` is the opaque Secret externref; the host signs / derives the public key
/// with the never-exposed bytes and writes `hexlen` hex chars. (Separate from
/// `crypto_hash_helper`, whose inputs are all strings.)
fn crypto_keyed_helper(name: &str, import: &str, hexlen: i32, has_msg: bool) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let mut params = vec![WirLocal { name: "key".into(), ty: WirTy::Extern }];
    let mut host_args: Vec<E> = vec![getl("key")];
    if has_msg {
        params.push(WirLocal { name: "msg".into(), ty: WirTy::Str });
        host_args.push(getl("msg"));
    }
    host_args.push(b(BinOp::Add, getl("res"), i32c(4)));
    WirFunc {
        name: name.into(),
        params,
        ret: vec![WirTy::Str],
        locals: vec![WirLocal { name: "res".into(), ty: WirTy::Bool }],
        body: vec![
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(hexlen + 4)] } },
            N::Store { ptr: getl("res"), value: i32c(hexlen), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: import.into(), args: host_args }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$dir_read(h, rel) -> i32` — the contents of file `rel` under Dir externref `h`,
/// as a String. Two-phase host protocol: `dir_read_len` reads the file and
/// reports its byte length (staging the bytes host-side), then `fill_pending`
/// copies the staged bytes into `res+4`. Needs the Dir(Read) capability.
pub fn dir_read_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dir_read".into(),
        params: vec![
            WirLocal { name: "h".into(), ty: WirTy::Extern },
            WirLocal { name: "rel".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "dir_read_len".into(), args: vec![getl("h"), getl("rel")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$file_read(f) -> i32` — the contents of file capability `f` as a String
/// (RFC-0012/RFC-0005 Stage 2). A `File` is a leaf (no path), so this takes only
/// the unforgeable externref. Two-phase host
/// protocol identical to [`dir_read_helper`]: `file_read_len` reads the file and
/// reports its byte length (staging the bytes host-side), then `fill_pending`
/// copies the staged bytes into `res+4`. Needs a `File[Read]` capability.
pub fn file_read_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "file_read".into(),
        params: vec![WirLocal { name: "f".into(), ty: WirTy::Extern }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "file_read_len".into(), args: vec![getl("f")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$exec(h, path, args, stdin) -> i32` — spawn the executable `path` under Dir
/// externref `h` (confined like `dir_read`), passing the `\0`-joined argv `args` and
/// `stdin`, returning the payload string `"<exit_code>\n<stdout><stderr>"`.
/// Two-phase host protocol identical to [`dir_read_helper`]: `exec_run` runs the
/// process and reports the staged payload's byte length, then `fill_pending`
/// copies it into `res+4`. Needs the `Exec` capability.
pub fn exec_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "exec".into(),
        params: vec![
            WirLocal { name: "h".into(), ty: WirTy::Extern },
            WirLocal { name: "path".into(), ty: WirTy::Str },
            WirLocal { name: "args".into(), ty: WirTy::Str },
            WirLocal { name: "stdin".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "exec_run".into(), args: vec![getl("h"), getl("path"), getl("args"), getl("stdin")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$crypto_reveal(key) -> i32` — the raw bytes of the Secret externref as a fresh
/// String (lossy UTF-8). Identical staging to [`dir_read_helper`]: the host
/// `crypto_reveal_len` reads the host-side secret and reports its byte length
/// (staging the bytes), then `fill_pending` copies them into `res+4`. For
/// value secrets (tokens, passwords) handed to an external sink — signing keys
/// are used via `sign`/`public_key`, not revealed.
pub fn crypto_reveal_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "crypto_reveal".into(),
        params: vec![WirLocal { name: "key".into(), ty: WirTy::Extern }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "crypto_reveal_len".into(), args: vec![getl("key")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$build_read(rel) -> i32` — the confined build file's contents as a fresh
/// string. Identical staging to [`dir_read_helper`], but the host length import
/// (`build_read_len`) resolves `rel` against the granted build *read roots*, not
/// a Dir handle. The source-level `BuildRead` receiver is zero-representation:
/// typeck requires it and import linking grants it, but no guest handle crosses
/// into the host import.
pub fn build_read_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "build_read".into(),
        params: vec![WirLocal { name: "rel".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "build_read_len".into(), args: vec![getl("rel")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$regex_match_spans(pat, text) -> i32` — the regex engine's encoded match
/// spans (`"s,e;s,e;…"`, "" on no match). The host (`regex_match_spans_len`,
/// the same native `regex.match_spans` the interpreter uses) reports the byte
/// length and stages the bytes; `$fill_pending` copies them into a fresh
/// `[len][bytes]` String. The variable-length string host-wrapper shape.
pub fn regex_match_spans_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "regex_match_spans".into(),
        params: vec![
            WirLocal { name: "pat".into(), ty: WirTy::Str },
            WirLocal { name: "text".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "len".into(),
                value: E::CallHost {
                    import: "regex_match_spans_len".into(),
                    args: vec![getl("pat"), getl("text")],
                },
            },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$dir_list(h) -> i32` — the entries of Dir externref `h`, as a
/// `List(String)`. The host reports the total byte size of the marshaled list
/// (`dir_list_size`), then writes the whole `[count][ptr..]` + payload structure
/// into the reserved block (`write_pending_list`). Needs the Dir(Read) capability.
pub fn dir_list_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    WirFunc {
        name: "dir_list".into(),
        params: vec![WirLocal { name: "h".into(), ty: WirTy::Extern }],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "size".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "size".into(), value: E::CallHost { import: "dir_list_size".into(), args: vec![getl("h")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![getl("size")] } },
            N::Do(E::CallHost { import: "write_pending_list".into(), args: vec![getl("res")] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// (RFC-0032) The shared two-phase size-then-write protocol behind every `vm.*` host
/// builtin (`vm.par_map`/`with_dir`/`serve`), mirroring `$dir_list`: `size = run(params…)`
/// (the host computes the result + reports its byte size); `ensure(size)`; `res = heap`;
/// `write(res)` (the host lays the result out at the reserved block); `heap += size`;
/// return `res`. The builtins differ ONLY in their params and the run/write host imports.
fn two_phase_helper(name: &str, params: &[&str], run_import: &str, write_import: &str) -> WirFunc {
    let typed = params.iter().map(|p| ((*p).to_string(), WirTy::Bool)).collect::<Vec<_>>();
    two_phase_helper_typed(name, &typed, run_import, write_import)
}

fn two_phase_helper_typed(
    name: &str,
    params: &[(String, WirTy)],
    run_import: &str,
    write_import: &str,
) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    WirFunc {
        name: name.into(),
        params: params.iter().map(|(p, ty)| WirLocal { name: p.clone(), ty: ty.clone() }).collect(),
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "size".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "size".into(),
                value: E::CallHost { import: run_import.into(), args: params.iter().map(|(p, _)| getl(p)).collect() },
            },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![getl("size")] } },
            N::Do(E::CallHost { import: write_import.into(), args: vec![getl("res")] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// Like [`exec_helper`], but parameterized for build-only host operations that
/// stage a Witchy `String`: `len = host(args...)`; allocate `[len][bytes]`;
/// `fill_pending(res+4)`.
fn staged_string_helper(name: &str, params: &[&str], run_import: &str) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: name.into(),
        params: params.iter().map(|p| WirLocal { name: (*p).into(), ty: WirTy::Bool }).collect(),
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "len".into(),
                value: E::CallHost { import: run_import.into(), args: params.iter().map(|p| getl(p)).collect() },
            },
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}


/// `$list_drop(list, k) -> i32` — a fresh list with the first `k` elements
/// dropped. Allocates `(len-k)` slots and `memory.copy`s the tail. Used by the
/// `[a, ..rest]` list pattern to bind the tail.
pub fn list_drop_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "list_drop".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "newlen".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "newlen".into(), value: b(BinOp::Sub, E::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 }, getl("k")) },
            N::SetLocal { local: "new".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("newlen"), i32c(8)))] } },
            N::Store { ptr: getl("new"), value: getl("newlen"), kind: Kind::I32, offset: 0 },
            N::MemoryCopy {
                dest: b(BinOp::Add, getl("new"), i32c(4)),
                src: b(BinOp::Add, b(BinOp::Add, getl("list"), i32c(4)), b(BinOp::Mul, getl("k"), i32c(8))),
                len: b(BinOp::Mul, getl("newlen"), i32c(8)),
            },
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$get_env(name) -> i32` — the value of env var `name` as an `Option(String)`
/// (`[tag][payload]`: tag 0 = Some with the string pointer in the i64 slot at +4,
/// tag 1 = None). `env_len` reports the value's length (or <0 if absent); on
/// presence `env_fill` copies the bytes. Needs the Env capability. (Reachable on
/// the binary path now that `match` on its Option result lowers via the
/// constructor-pattern arm.)
pub fn get_env_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "get_env".into(),
        params: vec![WirLocal { name: "name".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Bool],
        locals: ["len", "str", "res"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "env_len".into(), args: vec![getl("name")] } },
            N::If {
                cond: b(BinOp::Lt, getl("len"), i32c(0)),
                then_: vec![
                    // (RFC-0016) the None wrapper `[tag=1]` via `$rc_alloc`.
                    N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(4)] } },
                    N::Store { ptr: getl("res"), value: i32c(1), kind: Kind::I32, offset: 0 },
                    N::Return(Some(getl("res"))),
                ],
                els: vec![],
                result: None,
            },
            // (RFC-0016) the value string, then the `Some[tag=0][ptr]` wrapper, via `$rc_alloc`.
            N::SetLocal { local: "str".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("str"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "env_fill".into(), args: vec![getl("name"), b(BinOp::Add, getl("str"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(12)] } },
            N::Store { ptr: getl("res"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store { ptr: getl("res"), value: E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("str")) }, kind: Kind::I64, offset: 4 },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$build_get_env(name) -> Option(String)` — build-time environment reads
/// are staged like ordinary `get_env`, but the host enforces the BuildEnv
/// allow-list carried by the sandbox grant. The source receiver is checked and
/// then dropped before the host ABI.
pub fn build_get_env_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "build_get_env".into(),
        params: vec![WirLocal { name: "name".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Bool],
        locals: ["len", "str", "res"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            N::SetLocal {
                local: "len".into(),
                value: E::CallHost {
                    import: "build_env_len".into(),
                    args: vec![getl("name")],
                },
            },
            N::If {
                cond: b(BinOp::Lt, getl("len"), i32c(0)),
                then_: vec![
                    N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(4)] } },
                    N::Store { ptr: getl("res"), value: i32c(1), kind: Kind::I32, offset: 0 },
                    N::Return(Some(getl("res"))),
                ],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "str".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("str"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost {
                import: "build_env_fill".into(),
                args: vec![getl("name"), b(BinOp::Add, getl("str"), i32c(4))],
            }),
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(12)] } },
            N::Store { ptr: getl("res"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store {
                ptr: getl("res"),
                value: E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("str")) },
                kind: Kind::I64,
                offset: 4,
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}


/// `$float_to_str(x) -> i32` — render f64 `x` to a String via the host
/// `float_to_str` import (writes into a reserved 512-byte buffer, returns the
/// length). Used by the `$ts` renderer for Float fields. The import is ungated.
pub fn float_to_str_helper() -> WirFunc {
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

/// `$string_from_code(cp: i64) -> i32` — the single-character string for a Unicode
/// scalar `cp`, filled by the `string_from_code` host import into a fresh
/// `[len][bytes]` heap cell. Mirrors `STRING_FROM_CODE_WAT` (and the `float_to_str`
/// host-wrapper shape). Calls `$ensure`; uses `$heap` + the host import.
pub fn string_from_code_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "string_from_code".into(),
        params: vec![WirLocal { name: "cp".into(), ty: WirTy::Int }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "res".into(), ty: WirTy::Bool },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
        ],
        body: vec![
            // (RFC-0016) reserve the worst-case 8-byte cell through `$rc_alloc`; the host
            // writes n<=4 bytes and the length header caps to n (block size stays 8).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(8)] } },
            N::SetLocal {
                local: "n".into(),
                value: E::CallHost {
                    import: "string_from_code".into(),
                    args: vec![getl("cp"), b(BinOp::Add, getl("res"), i32c(4))],
                },
            },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$build_args() -> i32` — the `Args` list, sized by `args_size` and filled by
/// `write_pending_list`. Mirrors `BUILD_ARGS_WAT`. Calls `$ensure`; uses `$heap`.
pub fn build_args_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    WirFunc {
        name: "build_args".into(),
        params: vec![],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "size".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "size".into(), value: E::CallHost { import: "args_size".into(), args: vec![] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![getl("size")] } },
            N::Do(E::CallHost { import: "write_pending_list".into(), args: vec![getl("res")] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$compiler_footprint(src: i32) -> i32` (and the 2-arg `$compiler_diff`): the
/// host computes a JSON byte length, then fills a fresh `[len][bytes]` cell.
/// Mirrors `COMPILER_FOOTPRINT_WAT` / `COMPILER_DIFF_WAT`. `name`/`import` select
/// which; `nargs` is 1 (footprint) or 2 (diff). Calls `$ensure`; uses `$heap`.
fn compiler_introspect_helper(name: &str, import: &str, nargs: usize) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let params: Vec<WirLocal> = (0..nargs)
        .map(|i| WirLocal { name: format!("a{i}"), ty: WirTy::Bool })
        .collect();
    let host_args: Vec<E> = (0..nargs).map(|i| getl(&format!("a{i}"))).collect();
    WirFunc {
        name: name.into(),
        params,
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: import.into(), args: host_args } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// A thin host-import wrapper `$name(a0..a{nargs-1}) -> T` = `CallHost(import,
/// [a0..])`. Routing an inline host call through a registered helper keeps the
/// user body free of direct `CallHost`s — so the capability-minimal prune isn't
/// deferred (`no_direct_host` stays true) — and declares the import via
/// `import_deps`. The default parameter type is the legacy i32/pointer slot;
/// use `host_call_helper_typed` for externref or i64 parameters.
fn host_call_helper_ret(name: &str, import: &str, nargs: usize, ret: WirTy) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let params: Vec<WirLocal> = (0..nargs)
        .map(|i| WirLocal { name: format!("a{i}"), ty: WirTy::Bool })
        .collect();
    let host_args: Vec<E> = (0..nargs).map(|i| E::GetLocal(format!("a{i}"))).collect();
    WirFunc {
        name: name.into(),
        params,
        ret: vec![ret],
        locals: vec![],
        body: vec![N::Push(E::CallHost { import: import.into(), args: host_args })],
        raw_body: None,
    }
}

/// Like [`host_call_helper`] but with explicit per-parameter types — for a host import
/// whose params aren't all the default i32 slot (e.g. `net_connect_pinned`'s i64 `port`).
fn host_call_helper_typed(name: &str, import: &str, param_tys: &[WirTy], ret: WirTy) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let params: Vec<WirLocal> = param_tys
        .iter()
        .enumerate()
        .map(|(i, ty)| WirLocal { name: format!("a{i}"), ty: ty.clone() })
        .collect();
    let host_args: Vec<E> = (0..param_tys.len()).map(|i| E::GetLocal(format!("a{i}"))).collect();
    WirFunc {
        name: name.into(),
        params,
        ret: vec![ret],
        locals: vec![],
        body: vec![N::Push(E::CallHost { import: import.into(), args: host_args })],
        raw_body: None,
    }
}

/// Like [`host_call_helper`] but for a VOID host import (no result): perform the
/// effect, then yield `Nil` (`i32.const 0`) so the call expression has a value —
/// the binary-path analogue of the WAT path's `{args} call $h  i32.const 0`.
fn host_void_helper(name: &str, import: &str, nargs: usize) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let params: Vec<WirLocal> = (0..nargs)
        .map(|i| WirLocal { name: format!("a{i}"), ty: WirTy::Bool })
        .collect();
    let host_args: Vec<E> = (0..nargs).map(|i| E::GetLocal(format!("a{i}"))).collect();
    WirFunc {
        name: name.into(),
        params,
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![
            N::Do(E::CallHost { import: import.into(), args: host_args }),
            N::Push(E::ConstI32(0)),
        ],
        raw_body: None,
    }
}

/// Like [`host_void_helper`] but with explicit per-parameter types.
fn host_void_helper_typed(name: &str, import: &str, param_tys: &[WirTy]) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let params: Vec<WirLocal> = param_tys
        .iter()
        .enumerate()
        .map(|(i, ty)| WirLocal { name: format!("a{i}"), ty: ty.clone() })
        .collect();
    let host_args: Vec<E> = (0..param_tys.len()).map(|i| E::GetLocal(format!("a{i}"))).collect();
    WirFunc {
        name: name.into(),
        params,
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![
            N::Do(E::CallHost { import: import.into(), args: host_args }),
            N::Push(E::ConstI32(0)),
        ],
        raw_body: None,
    }
}

/// `$net_recv_<kind>(s: i32 [, n: i64]) -> i32` — read a length-prefixed string off
/// socket `s`: ask the host for the byte count (`$<len_import>`), `$ensure` room, write
/// the `[len]` header, `$fill_pending` the bytes into the buffer, bump `$heap` past the
/// cell, and return the string pointer. Mirrors the WAT `NET_RECV_*` prelude bodies.
fn net_recv_helper(name: &str, len_import: &str, extra_n: bool) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let add = |l: E, r: E| E::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let mut params = vec![WirLocal { name: "s".into(), ty: WirTy::Extern }];
    let mut len_args = vec![getl("s")];
    if extra_n {
        params.push(WirLocal { name: "n".into(), ty: WirTy::Int });
        len_args.push(getl("n"));
    }
    WirFunc {
        name: name.into(),
        params,
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "len".into(),
                value: E::CallHost { import: len_import.into(), args: len_args },
            },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![add(getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![add(getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$rcopy_str(p: i32) -> i32` — the region copy-out for a String. If `p` is below
/// `$rcopy_wm` it's parent-side (shared, not copied) → return it. Otherwise copy the
/// `[len][bytes]` cell to a fresh block above the live data (counting the bytes in
/// `$__region_copy_bytes`), and return the pointer PRE-BIASED by `$rcopy_delta` to its
/// post-slide address. Mirrors `EqShape::Str` in `ensure_rcopy_helper`. The compound
/// shapes (List/Tuple/Record/Adt/Dict) get their own generated rcopy helpers later.
pub fn rcopy_str_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load_i32 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    WirFunc {
        name: "rcopy_str".into(),
        params: vec![WirLocal { name: "p".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "n".into(), ty: WirTy::Bool },
            WirLocal { name: "size".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::If {
                cond: b(BinOp::LtU, getl("p"), E::GetGlobal("rcopy_wm".into())),
                then_: vec![N::Return(Some(getl("p")))],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "size".into(), value: b(BinOp::Add, i32c(4), load_i32(getl("p"))) },
            // (SEC-037) Allocate the copy through `$rc_alloc` so it carries the `[rc][size]`
            // header — otherwise rc-floor's free-at-overwrite could reclaim this header-less
            // buffer and corrupt the free-list (OOB). rc_alloc ensures + reserves the header and
            // returns the object base, exactly the pointer the bump path returned.
            N::SetLocal { local: "n".into(), value: E::Call { func: "rc_alloc".into(), args: vec![getl("size")] } },
            N::SetGlobal {
                global: "__region_copy_bytes".into(),
                value: E::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(E::GetGlobal("__region_copy_bytes".into())),
                    rhs: Box::new(E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("size")) }),
                },
            },
            N::MemoryCopy { dest: getl("n"), src: getl("p"), len: getl("size") },
            N::Push(b(BinOp::Sub, getl("n"), E::GetGlobal("rcopy_delta".into()))),
        ],
        raw_body: None,
    }
}
