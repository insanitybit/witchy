//! Allocating and capacity-aware string transformation helpers.

use crate::layout::RC_SIZE_MASK;
use crate::wir::*;

/// `$concat(a: i32, b: i32) -> i32` — allocate a fresh `[alen+blen][a..b..]`
/// string and `memory.copy` both operands in. Mirrors `CONCAT_WAT`. Calls
/// `$ensure`; uses the `$heap` global.
pub(in crate::wir_helpers) fn concat_helper() -> WirFunc {
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

/// `$str_append_cap(s: i32, piece: i32, cap: i32) -> (i32, i32)` — the in-place
/// string builder: a String is `[len(i32)][bytes]`. If the owned byte slack
/// (`cap`) covers `len + plen`, copy `piece`'s bytes into `s` in place and bump
/// its length (return `s` + `cap`); else grow to a doubled buffer. Bumps
/// `$__witchy_reowns` on a zero cap. Mirrors `STR_APPEND_CAP_WAT`; multi-value
/// early `return` restructured into `ret_ptr`/`ret_cap` + a dual tail Push.
/// Calls `$ensure`; uses `$heap` + `$__witchy_reowns`.
pub(crate) fn str_append_cap_helper() -> WirFunc {
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

/// `$substr(src, start, len) -> i32` — a fresh string holding `len` bytes of
/// `src` starting at *byte* offset `start`. Allocates `4 + len` via `$ensure`,
/// writes the length header, `memory.copy`s the slice, and bumps `$heap`.
pub(crate) fn substr_helper() -> WirFunc {
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
pub(crate) fn str_substring_helper() -> WirFunc {
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
pub(crate) fn is_ws_helper() -> WirFunc {
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
pub(crate) fn trim_helper() -> WirFunc {
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
pub(crate) fn split_helper() -> WirFunc {
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
pub(crate) fn str_chars_helper() -> WirFunc {
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

/// `$ascii_case(s, up) -> i32` — `s` with ASCII letters cased: `up != 0`
/// uppercases (`a`–`z` → `A`–`Z`), else lowercases. Non-letters and non-ASCII
/// bytes copy through unchanged (byte-wise, so multibyte UTF-8 is preserved).
pub(crate) fn ascii_case_helper() -> WirFunc {
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
pub(crate) fn match_at_helper() -> WirFunc {
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
pub(crate) fn replace_helper() -> WirFunc {
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
