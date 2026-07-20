//! Flat byte-buffer construction, access, slicing, and conversion helpers.

use super::abort_nodes;
use crate::wir::*;
use witchy_syntax::diag::DiagTemplate;

/// `$bytes_at(b: i32, i: i32) -> i64` — bounds-checked byte read over the flat
/// `[i32 len][bytes…]` layout: trap on `i < 0 || i >= len`, else zero-extend the
/// byte at `b + 4 + i`. Matches the interpreter's "bytes index out of bounds"
/// error (an unchecked `load8_u` here was SEC-038). No heap/import/table.
pub fn bytes_at_helper() -> WirFunc {
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
    // The index is i64 and checked in i64 (see `list_at_helper` — the interpreter
    // indexes with `i as usize`, so an out-of-i32-range index must trap and carry
    // its true value); narrowed to i32 only after the check passes.
    let len_i64 = || WirExpr::Convert {
        from: Kind::I32,
        to: Kind::I64,
        arg: Box::new(WirExpr::Load { ptr: Box::new(getl("b")), kind: Kind::I32, offset: 0 }),
    };
    let i_i32 = || WirExpr::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(getl("i")) };
    WirFunc {
        name: "bytes_at".into(),
        params: vec![
            WirLocal { name: "b".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Int], // i64 byte value 0..=255
        locals: vec![],
        body: vec![
            WirNode::If {
                // i64 comparisons yield i32 — combine with `i32.or`.
                cond: bin32(
                    BinOp::Or,
                    bin64(BinOp::Lt, getl("i"), i64c(0)),
                    bin64(BinOp::Ge, getl("i"), len_i64()),
                ),
                // (RFC-0045) `bytes index {i} out of bounds (length {len})`.
                then_: abort_nodes(DiagTemplate::BytesIndexOob, getl("i"), len_i64(), i32c(0)),
                els: vec![],
                result: None,
            },
            WirNode::Push(WirExpr::Convert {
                from: Kind::I32,
                to: Kind::I64,
                arg: Box::new(WirExpr::Load8U {
                    ptr: Box::new(bin32(BinOp::Add, getl("b"), i_i32())),
                    offset: 4,
                }),
            }),
        ],
        raw_body: None,
    }
}

/// `$bytes_from_list(xs) -> i32` — build a flat `Bytes` buffer from a
/// `List(Int)`. The public `std/bytes.from_list` wrapper validates every slot is
/// in `0..=255`; this helper performs the representation copy.
pub fn bytes_from_list_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load_len = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let slot_ptr = || b(BinOp::Add, b(BinOp::Add, getl("xs"), i32c(4)), b(BinOp::Mul, getl("i"), i32c(8)));
    let byte_value = || E::Convert {
        from: Kind::I64,
        to: Kind::I32,
        arg: Box::new(E::Load { ptr: Box::new(slot_ptr()), kind: Kind::I64, offset: 0 }),
    };
    WirFunc {
        name: "bytes_from_list".into(),
        params: vec![WirLocal { name: "xs".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Str],
        locals: ["len", "res", "i"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("len", load_len(getl("xs"))),
            setl("res", E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            N::Block {
                label: "done".into(),
                result: None,
                body: vec![N::Loop {
                    label: "l".into(),
                    body: vec![
                        N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("len"))) },
                        N::Store8 {
                            ptr: b(BinOp::Add, getl("res"), getl("i")),
                            value: byte_value(),
                            offset: 4,
                        },
                        setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                        N::Br { target: "l".into(), cond: None },
                    ],
                }],
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$bytes_slice(src, start, end) -> i32` — a fresh `Bytes` (a `[len][bytes]`
/// record) holding the *byte* range `[start, end)` of `src`, clamped exactly like
/// the interpreter's `__bytes_slice` (`lo = max(start, 0)`, `hi = min(end, len)`
/// then `hi = max(hi, lo)`), then delegating to `$substr(src, lo, hi - lo)`.
/// `Bytes` is byte-indexed with no UTF-8 contract, so this must NOT go through the
/// char-indexed `$str_substring` (that mangled multibyte payloads — the
/// backends diverged: interpreter byte-indexed, compiled char-indexed).
///
/// `start`/`end` are i64 and clamped in i64 (the interpreter clamps the full
/// `Int` before narrowing to a `usize`; narrowing to i32 first would wrap a
/// large positive bound negative — e.g. `slice(b, 0, 2^31)` then read as empty,
/// closing the same out-of-i32-range hole as `$bytes_at`/`$list_at`). Only after
/// clamping into `[0, len]` — where the bounds provably fit i32 for any non-empty
/// result — are `lo` and the count narrowed to i32 for the `$substr` ABI.
pub fn bytes_slice_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i64c = E::ConstI64;
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // `len` header is an i32; widen (unsigned) to i64 so all clamping is in i64.
    let len_i64 = |p: E| E::Convert {
        from: Kind::I32,
        to: Kind::I64,
        arg: Box::new(E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 }),
    };
    let to_i32 = |v: E| E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(v) };
    WirFunc {
        name: "bytes_slice".into(),
        params: vec![
            WirLocal { name: "src".into(), ty: WirTy::Str },
            WirLocal { name: "start".into(), ty: WirTy::Int },
            WirLocal { name: "end".into(), ty: WirTy::Int },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Int },
            WirLocal { name: "lo".into(), ty: WirTy::Int },
            WirLocal { name: "hi".into(), ty: WirTy::Int },
        ],
        body: vec![
            setl("len", len_i64(getl("src"))),
            // lo = max(start, 0)
            setl("lo", getl("start")),
            N::If {
                // i64 comparison yields i32 (0/1) — fine for `If`'s cond.
                cond: b64(BinOp::Lt, getl("lo"), i64c(0)),
                then_: vec![setl("lo", i64c(0))],
                els: vec![],
                result: None,
            },
            // lo = min(lo, len) — keep `lo` inside `[0, len]` so the narrowed
            // `$substr` pointer is never wild for an empty (count 0) result. The
            // interpreter's `b.get(lo..hi)` simply yields empty when `lo` is past
            // the end, so a large out-of-range `start` must clamp here, not trap.
            N::If {
                cond: b64(BinOp::Gt, getl("lo"), getl("len")),
                then_: vec![setl("lo", getl("len"))],
                els: vec![],
                result: None,
            },
            // hi = min(end, len)
            setl("hi", getl("end")),
            N::If {
                cond: b64(BinOp::Gt, getl("hi"), getl("len")),
                then_: vec![setl("hi", getl("len"))],
                els: vec![],
                result: None,
            },
            // hi = max(hi, lo)  (also covers a negative `end`)
            N::If {
                cond: b64(BinOp::Lt, getl("hi"), getl("lo")),
                then_: vec![setl("hi", getl("lo"))],
                els: vec![],
                result: None,
            },
            // Narrow to i32 only now: for any non-empty result `lo < hi <= len`
            // (both fit i32); an empty result has count 0, so `$substr` reads
            // nothing even if `lo` was a huge out-of-range bound.
            N::Push(E::Call {
                func: "substr".into(),
                args: vec![
                    getl("src"),
                    to_i32(getl("lo")),
                    to_i32(b64(BinOp::Sub, getl("hi"), getl("lo"))),
                ],
            }),
        ],
        raw_body: None,
    }
}

/// `$bytes_to_string(b) -> i32` — lossy UTF-8 normalization of a `Bytes`
/// (`bytes.to_string`): allocate a `3*len + 4` worst-case buffer (each invalid
/// byte becomes U+FFFD, 3 bytes), hand the input to the pure `encoding` host op 7
/// (whose read applies `String::from_utf8_lossy`, byte-identical to the
/// interpreter), and cap the length header to the bytes written. Must NOT be the
/// raw identity: `Bytes` has no UTF-8 contract, so returning invalid bytes verbatim
/// diverged from the interpreter (which substitutes U+FFFD).
pub fn bytes_to_string_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let host = witchy_syntax::intrinsics::wir_host_call(
        witchy_syntax::intrinsics::ENCODING_UTF8_LOSSY,
    )
    .expect("lossy UTF-8 encoding host call is cataloged");
    debug_assert_eq!(host.helper, "encoding");
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    WirFunc {
        name: "bytes_to_string".into(),
        params: vec![WirLocal { name: "b".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "res".into(), ty: WirTy::Bool },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
        ],
        body: vec![
            // worst case: every byte is invalid -> one U+FFFD (3 bytes) each.
            N::SetLocal {
                local: "res".into(),
                value: E::Call {
                    func: "rc_alloc".into(),
                    args: vec![b(BinOp::Add, b(BinOp::Mul, load(getl("b")), i32c(3)), i32c(4))],
                },
            },
            N::SetLocal {
                local: "n".into(),
                value: E::CallHost {
                    import: host.helper.into(),
                    args: vec![
                        i32c(host.selector),
                        getl("b"),
                        b(BinOp::Add, getl("res"), i32c(4)),
                    ],
                },
            },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}
