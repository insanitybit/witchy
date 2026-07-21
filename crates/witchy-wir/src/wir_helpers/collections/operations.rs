//! List views, mutation, concatenation, and pattern-tail helpers.

use super::super::abort_nodes;
use crate::layout::RC_SIZE_MASK;
use crate::wir::*;
use witchy_syntax::diag::DiagTemplate;

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
pub(in crate::wir_helpers) fn list_at_view_helper() -> WirFunc {
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
pub(in crate::wir_helpers) fn list_len_view_helper() -> WirFunc {
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

// ---------------------------------------------------------------------------
// (RFC-0051 / RFC-0073) THE IN-PLACE `*_cap` FAMILY — retained, and CLOSED to
// extension. Each `self_*` shape recognizer in
// `crates/witchy-lower/src/analysis.rs` (self_push_elem, self_insert_args,
// self_update_args, self_set_at, self_update_at, self_concat_pieces) pairs with
// one `*_cap` helper across the list, dict, and string modules: list_push_cap,
// dict_insert_cap, dict_update_cap, list_set_cap, list_update_cap,
// str_append_cap. RFC-0051
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
pub(crate) fn list_push_cap_helper() -> WirFunc {
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
pub(in crate::wir_helpers) fn list_set_cap_helper() -> WirFunc {
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
pub(crate) fn list_update_cap_helper() -> WirFunc {
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

/// `$list_push(list: i32, x: i64) -> i32` — the non-in-place append: always
/// allocates a fresh `(len+1)`-element buffer, copies the existing elements,
/// writes `x` in the new tail slot, and returns the new pointer. (The in-place
/// optimization lives in `$list_push_cap`; this is the plain fallback used by
/// helpers like `$split`/`$str_chars` that build lists internally.)
pub(crate) fn list_push_helper() -> WirFunc {
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


/// `$list_concat(a, b) -> i32` — a fresh list holding `a`'s elements followed by
/// `b`'s. Like the string `$concat`, but elements are 8-byte slots: allocate
/// `(alen+blen)` slots, `memory.copy` each source array in turn, bump `$heap`.
pub(crate) fn list_concat_helper() -> WirFunc {
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

/// `$list_drop(list, k) -> i32` — a fresh list with the first `k` elements
/// dropped. Allocates `(len-k)` slots and `memory.copy`s the tail. Used by the
/// `[a, ..rest]` list pattern to bind the tail.
pub(in crate::wir_helpers) fn list_drop_helper() -> WirFunc {
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
