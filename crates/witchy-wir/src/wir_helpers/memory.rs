//! Linear-memory allocation, reference counting, and ownership-transfer helpers.

use crate::wir::*;

/// `$ensure(size: i32)` — grow linear memory so `$heap + size` fits. Mirrors the
/// `ENSURE_WAT` helper: `need = heap + size; have = memory.size * 65536; if need
/// >u have: drop(memory.grow(ceil((need-have)/65536)))`. Uses the `$heap` global.
pub fn ensure_helper(checked: bool) -> WirFunc {
    let getl = |n: &str| WirExpr::GetLocal(n.into());
    let i32c = WirExpr::ConstI32;
    let bin = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let mut body = vec![
        WirNode::SetLocal {
            local: "need".into(),
            value: bin(BinOp::Add, WirExpr::GetGlobal("heap".into()), getl("size")),
        },
        WirNode::SetLocal {
            local: "have".into(),
            value: bin(BinOp::Mul, WirExpr::MemorySize, i32c(65536)),
        },
        WirNode::If {
            cond: bin(BinOp::GtU, getl("need"), getl("have")),
            then_: vec![WirNode::Drop(WirExpr::MemoryGrow(Box::new(bin(
                BinOp::DivU,
                bin(BinOp::Add, bin(BinOp::Sub, getl("need"), getl("have")), i32c(65535)),
                i32c(65536),
            ))))],
            els: vec![],
            result: None,
        },
    ];
    // (RFC-0023) Every allocator funnels through `$ensure` before writing, so this is the
    // chokepoint at which to reclaim stale redzones: hand the host the current `$heap`
    // (the allocation's base) so it can drop any registered object whose redzone sits
    // at/above it — i.e. space a region/watermark reset is about to reuse.
    if checked {
        body.insert(
            0,
            WirNode::Do(WirExpr::CallHost {
                import: "heap_frontier".into(),
                args: vec![WirExpr::GetGlobal("heap".into())],
            }),
        );
    }
    WirFunc {
        name: "ensure".into(),
        params: vec![WirLocal { name: "size".into(), ty: WirTy::Bool }],
        ret: vec![],
        locals: vec![
            WirLocal { name: "need".into(), ty: WirTy::Bool },
            WirLocal { name: "have".into(), ty: WirTy::Bool },
        ],
        body,
        raw_body: None,
    }
}

/// (RFC-0023) `$__heap_reclaim(wm: i32)` — tell the checked-heap shadow that everything
/// at or above `wm` is being reclaimed (drop those redzones). Emitted by the `region:`
/// pointer copy-out just before it slides its result down over the body's allocations
/// via a raw `memory.copy` (which bypasses `$ensure`, so the shadow wouldn't otherwise
/// learn of the reuse). Routed through this helper rather than an inline `CallHost` so
/// the capability-minimal prune isn't deferred (`no_direct_host` stays true).
pub fn heap_reclaim_helper() -> WirFunc {
    WirFunc {
        name: "__heap_reclaim".into(),
        params: vec![WirLocal { name: "wm".into(), ty: WirTy::Bool }],
        ret: vec![],
        locals: vec![],
        body: vec![WirNode::Do(WirExpr::CallHost {
            import: "heap_frontier".into(),
            args: vec![WirExpr::GetLocal("wm".into())],
        })],
        raw_body: None,
    }
}

pub use crate::layout::{slot_offset, slot_record_size, HEAP_REDZONE, RC_SIZE_MASK};

/// (RFC-0023) Whether the opt-in checked heap is selected for this compile. Read from
/// the environment like the other codegen toggles (`WITCHY_OPT`, `WIRDIAG`), so a
/// single `WITCHY_HEAP_CHECK=1` makes both the codegen instrument allocations and the
/// runtime sweep their redzones.
pub fn heap_check_enabled() -> bool {
    std::env::var_os("WITCHY_HEAP_CHECK").is_some_and(|v| v == "1")
}

/// (RFC-0037 §3) Whether the opt-in use-after-free sanitizer is selected. Read from the
/// environment like the other codegen toggles. When set (and `rc-floor` is on, since it
/// only alters `$rc_free`), a freed block is POISONED and NOT relinked for reuse, so the
/// poison is never overwritten and any use-after-free reads the trap pattern
/// deterministically — turning a fragile "maybe the reuse happened to overwrite before the
/// stale read" divergence into a guaranteed one. Output-preserving on a CORRECT program (a
/// correctly-freed block is never read again), so it only ever surfaces real bugs. Trades
/// reclamation for detection (freed memory leaks), so it is a debug-only test mode.
pub fn uaf_check_enabled() -> bool {
    std::env::var_os("WITCHY_UAF_CHECK").is_some_and(|v| v == "1")
}

/// (RFC-0037 §3) Whether the opt-in type-confusion sanitizer is selected. A debug-only mode,
/// off the production path: each `$rc_alloc`'d object carries an 8-bit TYPE TAG packed into the
/// HIGH BYTE of its allocation-size header word (`ptr-4`, whose low 24 bits are the size —
/// objects are ≪ 2^24 bytes). A typed read (`list.at` / `.field` / `match`) asserts the tag
/// matches the statically-expected type and traps on mismatch, catching a layout / `unbox`
/// confusion at the access site rather than three statements later. Every reader of the size
/// masks the tag off (`& RC_SIZE_MASK`), so the tag never perturbs reuse / reclamation math.
pub fn type_check_enabled() -> bool {
    std::env::var_os("WITCHY_TYPE_CHECK").is_some_and(|v| v == "1")
}

/// (RFC-0051 I1 step 3) Whether the dup/drop plausibility heuristic is compiled as a
/// FIRE-AND-REPORT debug assertion rather than a silent skip. When set, a pointer that
/// reaches `$rc_dup`/`$rc_drop` at/above `heap_base` but with an IMPLAUSIBLE header
/// (size ∉ [1,2^20) or rc ∉ [1,2^24)) is exactly an I1 emission-invariant violation —
/// codegen dup'd/dropped a value whose static type is NOT an owning object reference
/// (a view/slice/scalar). Instead of silently skipping (which leaks but hides the bug),
/// the guest TRAPS there, so the `WITCHY_WASM_BACKTRACE` name section names the offending
/// function. This is a SEPARATE flag from `WITCHY_HEAP_CHECK` on purpose: the checked-heap
/// fuzz is a HARD gate, and I1's typed emission is not yet airtight (the SEC-037 view-dup
/// residual still reaches a dup site under `rc-floor` — minigrep fires this assertion),
/// so folding the hard trap into the always-gated flag would red the gate on a
/// leak-safe, heuristic-masked residual. Once I1's typed emission closes SEC-037 at its
/// source and this fires zero times across the fuzzer + examples + e2e, the whole
/// `header_ok` check is deleted and this flag with it (RFC-0051 Design I1 step 3).
pub fn rc_assert_enabled() -> bool {
    std::env::var_os("WITCHY_RC_ASSERT").is_some_and(|v| v == "1")
}

/// The `$mk{n}` allocator for an `n`-field record/tuple/list: bump-allocate
/// `slot_record_size(n)` bytes, store the i32 tag/length header then each i64 field slot,
/// advance `$heap`, return the pointer. Mirrors `wir_prelude::mk_helper` /
/// `codegen::mk_helper`. Calls `$ensure`; uses the `$heap` global.
pub fn mk_helper(n: usize, checked: bool) -> WirFunc {
    let size = slot_record_size(n);
    // (RFC-0023) When checked, reserve a trailing redzone the host poisons via
    // `heap_register` and sweeps after the run — so an overrun past this object's end
    // is caught. The object layout `[p, p+size)` and the returned `p` are unchanged,
    // so a correct program behaves identically; only `$heap` advances by `rz` more.
    let rz = if checked { HEAP_REDZONE as i32 } else { 0 };
    // (RFC-0037 §3) Under WITCHY_TYPE_CHECK the caller rides an 8-bit TYPE tag in the high
    // byte of the `tag` argument; we mask it off the offset-0 variant word and stamp it into
    // the alloc header's high byte (p-4). Off the sanitizer the high byte is 0, so both the
    // mask and the header write are identity — production is untouched.
    let type_tagged = type_check_enabled();
    let mut params = vec![WirLocal { name: "tag".into(), ty: WirTy::Bool }];
    for i in 0..n {
        params.push(WirLocal { name: format!("f{i}"), ty: WirTy::Int });
    }
    let mut body = vec![
        // (RFC-0016) Allocate through the central `$rc_alloc`: it reserves a 4-byte
        // `[size]` header before the object and reuses a freed block when one fits,
        // so a confined value's buffer can later be `$rc_free`d and recycled. The
        // returned pointer is the object base (header at `p-4`) — readers unchanged.
        WirNode::SetLocal {
            local: "p".into(),
            value: WirExpr::Call { func: "rc_alloc".into(), args: vec![WirExpr::ConstI32(size + rz)] },
        },
        // header: store the i32 variant tag at p+0 (low 24 bits; the high byte, if any, is the
        // debug type tag, masked off here so the discriminant readers see only the variant).
        WirNode::Store {
            ptr: WirExpr::GetLocal("p".into()),
            value: WirExpr::Binary {
                op: BinOp::And,
                kind: Kind::I32,
                lhs: Box::new(WirExpr::GetLocal("tag".into())),
                rhs: Box::new(WirExpr::ConstI32(RC_SIZE_MASK)),
            },
            kind: Kind::I32,
            offset: 0,
        },
    ];
    if type_tagged {
        // Stamp the TYPE tag (the tag arg's high byte) into the alloc header's high byte at p-4,
        // preserving the size in the low 24 bits. Identity when the high byte is 0 (sanitizer off).
        let p_minus_4 = || WirExpr::Binary {
            op: BinOp::Sub,
            kind: Kind::I32,
            lhs: Box::new(WirExpr::GetLocal("p".into())),
            rhs: Box::new(WirExpr::ConstI32(4)),
        };
        body.push(WirNode::Store {
            ptr: p_minus_4(),
            value: WirExpr::Binary {
                op: BinOp::Or,
                kind: Kind::I32,
                lhs: Box::new(WirExpr::Binary {
                    op: BinOp::And,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::Load { ptr: Box::new(p_minus_4()), kind: Kind::I32, offset: 0 }),
                    rhs: Box::new(WirExpr::ConstI32(RC_SIZE_MASK)),
                }),
                rhs: Box::new(WirExpr::Binary {
                    op: BinOp::And,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::GetLocal("tag".into())),
                    rhs: Box::new(WirExpr::ConstI32(!RC_SIZE_MASK)),
                }),
            },
            kind: Kind::I32,
            offset: 0,
        });
    }
    for i in 0..n {
        body.push(WirNode::Store {
            ptr: WirExpr::GetLocal("p".into()),
            value: WirExpr::GetLocal(format!("f{i}")),
            kind: Kind::I64,
            offset: slot_offset(i) as u32,
        });
    }
    // (RFC-0023) Hand the live object `[p, p+size)` to the host shadow, which poisons
    // the redzone `[p+size, p+size+rz)`; the post-run sweep then proves it survived.
    if checked {
        body.push(WirNode::Do(WirExpr::CallHost {
            import: "heap_register".into(),
            args: vec![
                WirExpr::GetLocal("p".into()),
                WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::GetLocal("p".into())),
                    rhs: Box::new(WirExpr::ConstI32(size)),
                },
            ],
        }));
    }
    // `$rc_alloc` already advanced `$heap` past the allocation + its header; just
    // return the object base pointer.
    body.push(WirNode::Push(WirExpr::GetLocal("p".into())));
    WirFunc {
        name: format!("mk{n}"),
        params,
        ret: vec![WirTy::Bool], // i32 pointer
        locals: vec![WirLocal { name: "p".into(), ty: WirTy::Bool }],
        body,
        raw_body: None,
    }
}

/// (RFC-0051 I2) `$bump_alloc(size: i32) -> i32` — THE single allocator: the only
/// construct in the entire compiled module that advances `$heap`, and it is
/// `$ensure`-prefixed by construction. Everything that needs fresh bytes calls it —
/// `$rc_alloc`'s bump-miss path, the worker-VM `$__galloc`, and the dict index
/// rebuild — so the "remember to call ensure()" convention (the `int_to_string`
/// OOB class) is closed structurally: a workspace test walks every assembled WIR
/// function and fails on any other `$heap` write (the codegen watermark REWINDS,
/// which reset `$heap` to a previously captured `__witchy_wm_*` value and never
/// advance it, are the one shape-exempted case). Returns the old frontier.
fn increment_counter(name: &str) -> WirNode {
    WirNode::SetGlobal {
        global: name.into(),
        value: WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetGlobal(name.into())),
            rhs: Box::new(WirExpr::ConstI64(1)),
        },
    }
}

pub fn bump_alloc_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    WirFunc {
        name: "bump_alloc".into(),
        params: vec![WirLocal { name: "size".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: vec![WirLocal { name: "p".into(), ty: WirTy::Bool }],
        body: vec![
            increment_counter("__witchy_bump_alloc_calls"),
            N::Do(E::Call { func: "ensure".into(), args: vec![E::GetLocal("size".into())] }),
            N::SetLocal { local: "p".into(), value: E::GetGlobal("heap".into()) },
            N::SetGlobal {
                global: "heap".into(),
                value: E::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(E::GetLocal("p".into())),
                    rhs: Box::new(E::GetLocal("size".into())),
                },
            },
            N::Push(E::GetLocal("p".into())),
        ],
        raw_body: None,
    }
}

/// (RFC-0016) `$rc_alloc(size: i32) -> i32` — the central heap allocator: reuse a
/// freed block from the size-classed free-list (first-fit: the first block whose
/// stored byte-size ≥ `size`), else bump `$heap` like the inline allocators did.
/// When nothing has been freed the list is empty, so this is just `ensure`+bump —
/// byte-for-byte the old behavior — which is why routing the allocators through it
/// is transparent until the RC-floor `$rc_free` calls (gated, codegen-emitted)
/// start populating the list. Header layout: `[rc:i32 @obj-8][size:i32 @obj-4]` before
/// the returned object pointer; a freed block links via the object's first word (`@obj+0`,
/// dead once freed). `rc` is the RFC-0035 per-object refcount (1 on alloc / reuse), read
/// only by the gated `$dup`/`$drop`; `size` drives the free-list reuse scan.
pub fn rc_alloc_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let load = |p: E, off: u32| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: off };
    let not = |x: E| E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(x) };
    let scan = N::Block {
        label: "miss".into(),
        result: None,
        body: vec![N::Loop {
            label: "fl".into(),
            body: vec![
                // empty list / walked off the end → bump path below.
                N::Br { target: "miss".into(), cond: Some(not(getl("cur"))) },
                N::If {
                    // the block's allocated size is in its header at `cur-4` (low 24 bits;
                    // the high byte is the optional debug type tag, masked off here).
                    cond: b(BinOp::Ge, b(BinOp::And, load(b(BinOp::Sub, getl("cur"), i32c(4)), 0), i32c(RC_SIZE_MASK)), getl("size")),
                    then_: vec![
                        // unlink `cur`: head if prev==0, else prev.next.
                        N::If {
                            cond: not(getl("prev")),
                            then_: vec![N::SetGlobal { global: "rc_freelist".into(), value: load(getl("cur"), 0) }],
                            els: vec![N::Store { ptr: getl("prev"), value: load(getl("cur"), 0), kind: Kind::I32, offset: 0 }],
                            result: None,
                        },
                        // (RFC-0016) DoD counter: a reused block (its size at `cur-4`)
                        // is bytes recycled rather than freshly bumped.
                        N::SetGlobal {
                            global: "__rc_reused_bytes".into(),
                            value: E::Binary {
                                op: BinOp::Add,
                                kind: Kind::I64,
                                lhs: Box::new(E::GetGlobal("__rc_reused_bytes".into())),
                                rhs: Box::new(E::Convert {
                                    from: Kind::I32,
                                    to: Kind::I64,
                                    arg: Box::new(b(BinOp::And, load(b(BinOp::Sub, getl("cur"), i32c(4)), 0), i32c(RC_SIZE_MASK))),
                                }),
                            },
                        },
                        increment_counter("__witchy_rc_reuse_calls"),
                        // (RFC-0035) a recycled block re-enters life owned by one holder:
                        // reset its refcount word (at `cur-8`) to 1. Off the RC path the
                        // word is simply never read, so this is inert there.
                        N::Store {
                            ptr: b(BinOp::Sub, getl("cur"), i32c(8)),
                            value: i32c(1),
                            kind: Kind::I32,
                            offset: 0,
                        },
                        N::Return(Some(getl("cur"))),
                    ],
                    els: vec![],
                    result: None,
                },
                setl("prev", getl("cur")),
                setl("cur", load(getl("cur"), 0)),
                N::Br { target: "fl".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "rc_alloc".into(),
        params: vec![WirLocal { name: "size".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: ["cur", "prev", "base"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            increment_counter("__witchy_rc_alloc_calls"),
            // (RFC-0035) one more live object — both the reuse and the bump paths below return
            // a cell, so count it once here (the reuse path re-lives a cell that `$rc_free` had
            // decremented). Off the RC path no `$rc_free` runs, so it is inert bookkeeping.
            N::SetGlobal {
                global: "__witchy_live_cells".into(),
                value: E::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(E::GetGlobal("__witchy_live_cells".into())),
                    rhs: Box::new(E::ConstI64(1)),
                },
            },
            setl("cur", E::GetGlobal("rc_freelist".into())),
            setl("prev", i32c(0)),
            scan,
            // miss: bump the arena via `$bump_alloc` (RFC-0051 I2: the ONE construct
            // that advances `$heap`, ensure-prefixed by construction), reserving an
            // 8-byte `[rc:i32][size:i32]` header BEFORE the object. `size` stays at
            // object-4 (so `$rc_free`, the reuse scan, and the reused-bytes counter
            // are byte-for-byte unchanged); the new refcount word sits at object-8,
            // initialized to 1 (the allocating owner). The returned object pointer is
            // `base+8`; every in-object reader is relative to it and so is unaffected.
            // Off the RC path (`$dup`/`$drop` not emitted) the refcount is written but
            // never read — inert, +4 bytes/object.
            setl("base", E::Call { func: "bump_alloc".into(), args: vec![b(BinOp::Add, getl("size"), i32c(8))] }),
            N::Store { ptr: getl("base"), value: i32c(1), kind: Kind::I32, offset: 0 },
            N::Store { ptr: getl("base"), value: getl("size"), kind: Kind::I32, offset: 4 },
            N::Push(b(BinOp::Add, getl("base"), i32c(8))),
        ],
        raw_body: None,
    }
}

/// (RFC-0016) `$rc_free(ptr: i32)` — return a dead, uniquely-owned heap block to the
/// free-list for reuse by `$rc_alloc`. The block already carries its allocated size
/// in its header at `ptr-4` (written by `$rc_alloc`), so freeing just links it in:
/// store the old list head into the block's first word (the object is dead, ≥4 bytes,
/// so there is room) and make this block the new head. The caller (the codegen
/// free-at-overwrite rule, gated `WITCHY_OPT=rc-floor`) only needs the pointer — no
/// size — and is responsible for soundness: it only frees a block the escape oracle
/// proved confined + unaliased and distinct from the freshly-built result.
pub fn rc_free_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    // (RFC-0035) one fewer live object — this cell is now dead.
    let dec_live = N::SetGlobal {
        global: "__witchy_live_cells".into(),
        value: E::Binary {
            op: BinOp::Sub,
            kind: Kind::I64,
            lhs: Box::new(E::GetGlobal("__witchy_live_cells".into())),
            rhs: Box::new(E::ConstI64(1)),
        },
    };

    if uaf_check_enabled() {
        // (RFC-0037 §3) UAF sanitizer variant: fill the freed payload with a POISON pattern,
        // then relink the block for reuse exactly as the normal path does. This is STRICTLY
        // MORE detection than the plain differential, never less:
        //   * if the block is later reused, the new owner overwrites the poison with its own
        //     data — so the existing "reuse corrupts a still-aliased value" divergence is
        //     preserved unchanged; and
        //   * if the block is NOT reused before a stale read (the FRAGILE case the plain
        //     differential misses — G3), the read sees POISON, a wrong value (→ DIVERGE) or,
        //     read as a length, a fast out-of-bounds wasm trap (also a DIVERGE vs the interp).
        // On a CORRECT program a freed block is never read again, so poisoning changes no
        // output — zero false positives. The allocated size is in the header at `ptr-4`;
        // poison every whole word of `[ptr, ptr+size)` when that size is sane (a guard against
        // a freed buffer that predates the rc header, whose `ptr-4` is not a real size).
        let i32c = E::ConstI32;
        let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let load = |p: E, off: u32| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: off };
        const POISON: i32 = 0xDEAD_BEEFu32 as i32;
        const POISON_LIMIT: i32 = 1 << 20; // 1 MiB — larger than any test object; guards a bogus header.
        let poison_loop = N::Block {
            label: "pz_done".into(),
            result: None,
            body: vec![N::Loop {
                label: "pz".into(),
                body: vec![
                    // stop before the store would reach the next block's header at ptr+size.
                    N::Br {
                        target: "pz_done".into(),
                        cond: Some(b(BinOp::Gt, b(BinOp::Add, getl("i"), i32c(4)), getl("size"))),
                    },
                    N::Store {
                        ptr: b(BinOp::Add, getl("ptr"), getl("i")),
                        value: i32c(POISON),
                        kind: Kind::I32,
                        offset: 0,
                    },
                    N::SetLocal { local: "i".into(), value: b(BinOp::Add, getl("i"), i32c(4)) },
                    N::Br { target: "pz".into(), cond: None },
                ],
            }],
        };
        return WirFunc {
            name: "rc_free".into(),
            params: vec![WirLocal { name: "ptr".into(), ty: WirTy::Bool }],
            ret: vec![],
            locals: ["size", "i"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
            // (RFC-0051 I1) The same categorical `ptr >= heap_base` floor as the release
            // path: a below-`heap_base` pointer is a literal/immediate/handle, never an
            // `$rc_alloc` object, so neither poison nor relink applies (its `[ptr-4]` is
            // not a real size header). Guarding here keeps the sanitizer honest — it
            // never poisons the static data segment.
            body: vec![increment_counter("__witchy_rc_free_calls"), N::If {
                cond: b(BinOp::GeU, getl("ptr"), E::GetGlobal("heap_base".into())),
                then_: vec![
                    N::SetLocal { local: "size".into(), value: b(BinOp::And, load(b(BinOp::Sub, getl("ptr"), i32c(4)), 0), i32c(RC_SIZE_MASK)) },
                    // Poison only a sane-sized payload; `LeU` also rejects a negative (→ huge
                    // unsigned) bogus size. size==0 makes the loop a no-op.
                    N::If {
                        cond: b(BinOp::LeU, getl("size"), i32c(POISON_LIMIT)),
                        then_: vec![N::SetLocal { local: "i".into(), value: i32c(0) }, poison_loop],
                        els: vec![],
                        result: None,
                    },
                    // Relink for reuse (identical to the normal path). The freelist link occupies
                    // word 0, overwriting the poison there; words 4.. stay poisoned until reuse.
                    N::Store { ptr: getl("ptr"), value: E::GetGlobal("rc_freelist".into()), kind: Kind::I32, offset: 0 },
                    N::SetGlobal { global: "rc_freelist".into(), value: getl("ptr") },
                    dec_live,
                ],
                els: vec![],
                result: None,
            }],
            raw_body: None,
        };
    }

    // (RFC-0051 I1) `$rc_free` is called directly by the free-at-overwrite path
    // (codegen `x = f(x)`), NOT only via `$rc_drop` — so it needs the SAME categorical
    // `ptr >= heap_base` floor that `$dup`/`$drop` carry. A below-`heap_base` pointer is
    // NEVER an `$rc_alloc` object: it is a string/data-segment LITERAL, an immediate, or
    // a capability handle. Freeing one (the SEC-039 leak: `var t = "abc"; t = trim(t)`
    // freed the literal into the free-list, corrupting its length word → a later reuse
    // handed out the poisoned pointer → megabytes of heap disclosed) is unsound. The
    // guard makes it a no-op; `__witchy_live_cells` is only incremented by `$rc_alloc`,
    // so a skipped free of a non-object is correct bookkeeping, not a lost decrement.
    let bin = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "rc_free".into(),
        params: vec![WirLocal { name: "ptr".into(), ty: WirTy::Bool }],
        ret: vec![],
        locals: vec![],
        body: vec![increment_counter("__witchy_rc_free_calls"), N::If {
            cond: bin(BinOp::GeU, getl("ptr"), E::GetGlobal("heap_base".into())),
            then_: vec![
                N::Store { ptr: getl("ptr"), value: E::GetGlobal("rc_freelist".into()), kind: Kind::I32, offset: 0 },
                N::SetGlobal { global: "rc_freelist".into(), value: getl("ptr") },
                dec_live,
            ],
            els: vec![],
            result: None,
        }],
        raw_body: None,
    }
}

/// (RFC-0035) `$rc_dup(ptr: i32)` — the Perceus dup: record one more live reference to
/// the heap object whose `$rc_alloc` region starts at `ptr` (refcount at `ptr-8`). The
/// `ptr >= heap_base` guard means only a REAL refcounted heap object is touched: scalars
/// (Bool is also `i32`), nullary/immediate values, capability handles and static-data
/// pointers all sit below `heap_base` and are no-ops. So codegen may emit this for any
/// `i32`-kinded value — the guard is the soundness floor, not mere defence-in-depth.
/// Emitted (gated `rc-floor`) where a heap value is aliased into a second live holder —
/// a container element read out into a binding, a binding copied.
pub fn rc_dup_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let rc_addr = || b(BinOp::Sub, getl("ptr"), i32c(8));
    let rc_load = || E::Load { ptr: Box::new(rc_addr()), kind: Kind::I32, offset: 0 };
    // (SEC-037) The size word `[ptr-4]` (low 24 bits) — a genuine `$rc_alloc` object always has a
    // small, plausible size here; a VIEW/slice pointer (into a parent) or a mis-typed scalar dup'd
    // as a pointer has arbitrary data, so its "size" is implausible. Guarding the increment on a
    // plausible size means `$rc_dup` touches `[ptr-8]` ONLY on real object bases — never corrupting
    // a view's/parent's data (the minigrep/pm OOB). A real object always passes, so no dup is lost.
    // `(v-1) <=U (hi-2)` ⇔ `1 <= v <= hi-1` (also rejects v==0, which underflows to a huge unsigned).
    let in_1_to = |v: E, hi: i32| b(BinOp::LeU, b(BinOp::Sub, v, i32c(1)), i32c(hi - 2));
    let size_load = || b(BinOp::And, E::Load { ptr: Box::new(b(BinOp::Sub, getl("ptr"), i32c(4))), kind: Kind::I32, offset: 0 }, i32c(RC_SIZE_MASK));
    // Two-factor plausibility: a genuine object always has size ∈ [1, 2^20) at ptr-4 AND rc ∈
    // [1, 2^24) at ptr-8, so it ALWAYS passes (no dup lost). A view/scalar must have BOTH words
    // coincidentally in range to slip through — vanishingly unlikely, and it only ever SKIPS a dup.
    let header_ok = || b(BinOp::And, in_1_to(size_load(), 1 << 20), in_1_to(rc_load(), 1 << 24));
    // (RFC-0051 I1 step 3) Demote the runtime plausibility heuristic to a debug assertion.
    // Under WITCHY_RC_ASSERT the else-branch — a pointer at/above `heap_base` whose header
    // is IMPLAUSIBLE — is exactly an I1 emission-invariant violation (codegen dup'd a
    // non-owning value: a view/slice/scalar). The assertion TRAPS there (fire-and-report:
    // the WITCHY_WASM_BACKTRACE name section names the offending guest function) instead of
    // silently skipping, so a violation surfaces at the site rather than leaking quietly.
    // In release the else stays EMPTY (the leak-safe interim backstop): once I1's typed
    // emission is proven — zero fires across the fuzzer + examples + e2e — the whole
    // `header_ok` check is deleted and only `ptr >= heap_base` remains.
    let dup_store = N::Store {
        ptr: rc_addr(),
        value: b(BinOp::Add, rc_load(), i32c(1)),
        kind: Kind::I32,
        offset: 0,
    };
    let dup_body = if rc_assert_enabled() {
        N::If { cond: header_ok(), then_: vec![dup_store], els: vec![N::Unreachable], result: None }
    } else {
        N::If { cond: header_ok(), then_: vec![dup_store], els: vec![], result: None }
    };
    WirFunc {
        name: "rc_dup".into(),
        params: vec![WirLocal { name: "ptr".into(), ty: WirTy::Bool }],
        // Returns `ptr` so it wraps a read expression in place: `$rc_dup(<read>)`.
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![
            // NESTED (not `&&`): WIR `And` evaluates both operands, so the header loads must be
            // guarded by `ptr >= heap_base` first — else a small scalar `ptr` would read `[ptr-8]`
            // out of bounds and trap. Only inside the heap-pointer guard do we read the header.
            N::If {
                cond: b(BinOp::GeU, getl("ptr"), E::GetGlobal("heap_base".into())),
                then_: vec![dup_body],
                els: vec![],
                result: None,
            },
            N::Push(getl("ptr")),
        ],
        raw_body: None,
    }
}

/// `$leaf_dup(value, rc_bias) -> i64` — retain an RC-backed universal-slot
/// value and return it unchanged. `rc_bias` is -1 for a trivial leaf, 0 for an
/// ordinary RC pointer, and 4 for a Dict pointer following its hidden index.
pub fn leaf_dup_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |name: &str| E::GetLocal(name.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b32 = |op: BinOp, lhs: E, rhs: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    };
    let b64 = |op: BinOp, lhs: E, rhs: E| E::Binary {
        op,
        kind: Kind::I64,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    };
    let ptr = || E::Convert {
        from: Kind::I64,
        to: Kind::I32,
        arg: Box::new(getl("value")),
    };
    WirFunc {
        name: "leaf_dup".into(),
        params: vec![
            WirLocal { name: "value".into(), ty: WirTy::Int },
            WirLocal { name: "rc_bias".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int],
        locals: vec![],
        body: vec![
            N::If {
                cond: b32(
                    BinOp::And,
                    b32(BinOp::Ge, getl("rc_bias"), i32c(0)),
                    b64(BinOp::Ne, getl("value"), i64c(0)),
                ),
                then_: vec![N::If {
                    cond: b32(
                        BinOp::GeU,
                        b32(BinOp::Sub, ptr(), getl("rc_bias")),
                        E::GetGlobal("heap_base".into()),
                    ),
                    then_: vec![
                        N::SetGlobal {
                            global: "__witchy_extract_retains".into(),
                            value: E::Binary {
                                op: BinOp::Add,
                                kind: Kind::I64,
                                lhs: Box::new(E::GetGlobal("__witchy_extract_retains".into())),
                                rhs: Box::new(i64c(1)),
                            },
                        },
                        N::Drop(E::Call {
                            func: "rc_dup".into(),
                            args: vec![b32(BinOp::Sub, ptr(), getl("rc_bias"))],
                        }),
                    ],
                    els: vec![],
                    result: None,
                }],
                els: vec![],
                result: None,
            },
            N::Push(getl("value")),
        ],
        raw_body: None,
    }
}

/// `$leaf_drop(value, rc_bias)` — release one RC-backed universal-slot value.
/// It is the exact inverse of [`leaf_dup_helper`] for initialized leaves.
pub fn leaf_drop_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |name: &str| E::GetLocal(name.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b32 = |op: BinOp, lhs: E, rhs: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    };
    let b64 = |op: BinOp, lhs: E, rhs: E| E::Binary {
        op,
        kind: Kind::I64,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    };
    let ptr = || E::Convert {
        from: Kind::I64,
        to: Kind::I32,
        arg: Box::new(getl("value")),
    };
    WirFunc {
        name: "leaf_drop".into(),
        params: vec![
            WirLocal { name: "value".into(), ty: WirTy::Int },
            WirLocal { name: "rc_bias".into(), ty: WirTy::Bool },
        ],
        ret: vec![],
        locals: vec![],
        body: vec![N::If {
            cond: b32(
                BinOp::And,
                b32(BinOp::Ge, getl("rc_bias"), i32c(0)),
                b64(BinOp::Ne, getl("value"), i64c(0)),
            ),
            then_: vec![N::If {
                cond: b32(
                    BinOp::GeU,
                    b32(BinOp::Sub, ptr(), getl("rc_bias")),
                    E::GetGlobal("heap_base".into()),
                ),
                then_: vec![
                    N::SetGlobal {
                        global: "__witchy_extract_drops".into(),
                        value: E::Binary {
                            op: BinOp::Add,
                            kind: Kind::I64,
                            lhs: Box::new(E::GetGlobal("__witchy_extract_drops".into())),
                            rhs: Box::new(i64c(1)),
                        },
                    },
                    N::Do(E::Call {
                        func: "rc_drop".into(),
                        args: vec![b32(BinOp::Sub, ptr(), getl("rc_bias"))],
                    }),
                ],
                els: vec![],
                result: None,
            }],
            els: vec![],
            result: None,
        }],
        raw_body: None,
    }
}

/// `$slot_take_or_dup(addr, unique, rc_bias) -> i64` — the ownership half of
/// structural extraction. `addr` identifies an initialized universal slot.
/// A unique container transfers the slot (and clears it before structural
/// repair); a shared container retains an RC-backed leaf before returning it.
/// `rc_bias` is -1 for trivial leaves, 0 for ordinary RC pointers, and 4 for a
/// Dict value whose exposed pointer follows its hidden index word.
pub fn slot_take_or_dup_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |name: &str| E::GetLocal(name.into());
    let i64c = E::ConstI64;
    WirFunc {
        name: "slot_take_or_dup".into(),
        params: vec![
            WirLocal { name: "addr".into(), ty: WirTy::Bool },
            WirLocal { name: "unique".into(), ty: WirTy::Bool },
            WirLocal { name: "rc_bias".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int],
        locals: vec![WirLocal { name: "old".into(), ty: WirTy::Int }],
        body: vec![
            N::SetLocal {
                local: "old".into(),
                value: E::Load { ptr: Box::new(getl("addr")), kind: Kind::I64, offset: 0 },
            },
            N::If {
                cond: getl("unique"),
                then_: vec![N::Store {
                    ptr: getl("addr"),
                    value: i64c(0),
                    kind: Kind::I64,
                    offset: 0,
                }],
                els: vec![N::SetLocal {
                    local: "old".into(),
                    value: E::Call {
                        func: "leaf_dup".into(),
                        args: vec![getl("old"), getl("rc_bias")],
                    },
                }],
                result: None,
            },
            N::Push(getl("old")),
        ],
        raw_body: None,
    }
}

/// (RFC-0035) `$rc_drop(ptr: i32)` — the Perceus drop: release one live reference to the
/// heap object at `$rc_alloc` region `ptr` (refcount at `ptr-8`). At count 1 (the last
/// reference) the block is returned to the free-list via `$rc_free`; otherwise the count
/// is decremented. The `ptr >= heap_base` guard no-ops on any non-heap `i32` (scalar,
/// immediate, capability handle, static-data pointer). SOUNDNESS: this frees ONLY at a
/// count that reached 1 through matched `$rc_dup`s — a missed dup would keep the count
/// too low, so codegen must dup at EVERY aliasing point (the ⊥-keeps-the-count floor
/// governs the drop side: a missed drop leaks, never frees live). Freeing is shell-only
/// for now — a child heap value held by the freed block leaks (sound); recursive `$rdrop`
/// is a later brick. Emitted (gated `rc-floor`) at a heap value's last use / a slot overwrite.
pub fn rc_drop_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let rc_addr = || b(BinOp::Sub, getl("ptr"), i32c(8));
    let rc_load = || E::Load { ptr: Box::new(rc_addr()), kind: Kind::I32, offset: 0 };
    // (RFC-0051 I1) The SYMMETRIC interim guard: the same 2-factor plausibility check
    // `$rc_dup` carries. A genuine `$rc_alloc` object always has size ∈ [1, 2^20) at
    // `ptr-4` AND rc ∈ [1, 2^24) at `ptr-8`, so it ALWAYS passes (no drop is ever lost).
    // A view/slice/mis-typed pointer above `heap_base` that reaches a drop site must have
    // BOTH header words coincidentally in range to slip through — and the direction of
    // error is the SAFE one: a skipped drop LEAKS, it never frees live data or underflows
    // a neighbor's count (which a raw `[ptr-8]--` on a non-object would do). This is the
    // drop-side of the SEC-037 mitigation; I1's typed emission makes it dead code, at
    // which point it is demoted to the `WITCHY_HEAP_CHECK` trap-and-report assertion.
    // `(v-1) <=U (hi-2)` ⇔ `1 <= v <= hi-1` (also rejects v==0, which underflows huge).
    let in_1_to = |v: E, hi: i32| b(BinOp::LeU, b(BinOp::Sub, v, i32c(1)), i32c(hi - 2));
    let size_load = || b(BinOp::And, E::Load { ptr: Box::new(b(BinOp::Sub, getl("ptr"), i32c(4))), kind: Kind::I32, offset: 0 }, i32c(RC_SIZE_MASK));
    let header_ok = || b(BinOp::And, in_1_to(size_load(), 1 << 20), in_1_to(rc_load(), 1 << 24));
    let dec_or_free = N::If {
        cond: b(BinOp::Le, rc_load(), i32c(1)),
        then_: vec![N::Do(E::Call { func: "rc_free".into(), args: vec![getl("ptr")] })],
        els: vec![N::Store {
            ptr: rc_addr(),
            value: b(BinOp::Sub, rc_load(), i32c(1)),
            kind: Kind::I32,
            offset: 0,
        }],
        result: None,
    };
    // (RFC-0051 I1 step 3) Same demotion as `$rc_dup`: under WITCHY_RC_ASSERT an implausible
    // header at a drop site (an I1 violation — codegen dropped a non-owning value) TRAPS and
    // reports; in release the else is empty (skip = leak, the safe interim direction).
    let drop_body = if rc_assert_enabled() {
        N::If { cond: header_ok(), then_: vec![dec_or_free], els: vec![N::Unreachable], result: None }
    } else {
        N::If { cond: header_ok(), then_: vec![dec_or_free], els: vec![], result: None }
    };
    WirFunc {
        name: "rc_drop".into(),
        params: vec![WirLocal { name: "ptr".into(), ty: WirTy::Bool }],
        ret: vec![],
        locals: vec![],
        body: vec![N::If {
            // NESTED (not `&&`): the header loads must be guarded by `ptr >= heap_base`
            // first, else a small scalar `ptr` reads `[ptr-8]`/`[ptr-4]` out of bounds.
            cond: b(BinOp::GeU, getl("ptr"), E::GetGlobal("heap_base".into())),
            then_: vec![drop_body],
            els: vec![],
            result: None,
        }],
        raw_body: None,
    }
}
