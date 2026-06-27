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

/// `$ensure(size: i32)` — grow linear memory so `$heap + size` fits. Mirrors the
/// `ENSURE_WAT` helper: `need = heap + size; have = memory.size * 65536; if need
/// >u have: drop(memory.grow(ceil((need-have)/65536)))`. Uses the `$heap` global.
pub fn ensure_helper() -> WirFunc {
    let getl = |n: &str| WirExpr::GetLocal(n.into());
    let i32c = WirExpr::ConstI32;
    let bin = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    WirFunc {
        name: "ensure".into(),
        params: vec![WirLocal { name: "size".into(), ty: WirTy::Bool }],
        ret: vec![],
        locals: vec![
            WirLocal { name: "need".into(), ty: WirTy::Bool },
            WirLocal { name: "have".into(), ty: WirTy::Bool },
        ],
        body: vec![
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
        ],
        raw_body: None,
    }
}

/// The `$mk{n}` allocator for an `n`-field record/tuple/list: bump-allocate
/// `4 + 8n` bytes, store the i32 tag/length header then each i64 field slot,
/// advance `$heap`, return the pointer. Mirrors `wir_prelude::mk_helper` /
/// `codegen::mk_helper`. Calls `$ensure`; uses the `$heap` global.
pub fn mk_helper(n: usize) -> WirFunc {
    let size = 4 + 8 * n;
    let mut params = vec![WirLocal { name: "tag".into(), ty: WirTy::Bool }];
    for i in 0..n {
        params.push(WirLocal { name: format!("f{i}"), ty: WirTy::Int });
    }
    let mut body = vec![
        WirNode::Do(WirExpr::Call {
            func: "ensure".into(),
            args: vec![WirExpr::ConstI32(size as i32)],
        }),
        WirNode::SetLocal { local: "p".into(), value: WirExpr::GetGlobal("heap".into()) },
        // header: store the i32 tag at p+0.
        WirNode::Store {
            ptr: WirExpr::GetLocal("p".into()),
            value: WirExpr::GetLocal("tag".into()),
            kind: Kind::I32,
            offset: 0,
        },
    ];
    for i in 0..n {
        body.push(WirNode::Store {
            ptr: WirExpr::GetLocal("p".into()),
            value: WirExpr::GetLocal(format!("f{i}")),
            kind: Kind::I64,
            offset: (4 + 8 * i) as u32,
        });
    }
    // advance $heap past the allocation, then return the base pointer.
    body.push(WirNode::SetGlobal {
        global: "heap".into(),
        value: WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(WirExpr::GetLocal("p".into())),
            rhs: Box::new(WirExpr::ConstI32(size as i32)),
        },
    });
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

/// `$list_at(list: i32, i: i32) -> i64` — bounds-checked element read: trap on
/// `i < 0 || i >= len`, else load the i64 slot at `(list+4) + i*8`. Mirrors
/// `LIST_AT_WAT`. No heap/import/table.
pub fn list_at_helper() -> WirFunc {
    let getl = |n: &str| WirExpr::GetLocal(n.into());
    let i32c = WirExpr::ConstI32;
    let bin = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    WirFunc {
        name: "list_at".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int], // i64 slot
        locals: vec![],
        body: vec![
            WirNode::If {
                cond: bin(
                    BinOp::Or,
                    bin(BinOp::Lt, getl("i"), i32c(0)),
                    bin(
                        BinOp::Ge,
                        getl("i"),
                        WirExpr::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 },
                    ),
                ),
                then_: vec![WirNode::Unreachable],
                els: vec![],
                result: None,
            },
            WirNode::Push(WirExpr::Load {
                ptr: Box::new(bin(
                    BinOp::Add,
                    bin(BinOp::Add, getl("list"), i32c(4)),
                    bin(BinOp::Mul, getl("i"), i32c(8)),
                )),
                kind: Kind::I64,
                offset: 0,
            }),
        ],
        raw_body: None,
    }
}

/// `$int_to_string(n: i64) -> i32` — render a signed integer to a fresh witchy
/// string (`[i32 len][ascii]`). Mirrors `INT_TO_STRING_WAT`: `0` is a fast path;
/// otherwise count digits (a div-by-10 loop), allocate `[len][digits]`, write the
/// optional `-` then the digits back-to-front (a second div/rem loop). Calls
/// `$ensure`; uses the `$heap` global; byte writes via `Store8`.
pub fn int_to_string_helper() -> WirFunc {
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
    // n == 0 → the single ascii '0'.
    let then_zero = vec![
        N::Do(E::Call { func: "ensure".into(), args: vec![i32c(5)] }),
        N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
        N::Store { ptr: getl("res"), value: i32c(1), kind: Kind::I32, offset: 0 },
        N::Store8 { ptr: getl("res"), value: i32c(48), offset: 4 },
        N::SetGlobal {
            global: "heap".into(),
            value: bin(BinOp::Add, Kind::I32, getl("res"), i32c(5)),
        },
        N::Push(getl("res")),
    ];
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
    let else_nonzero = vec![
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
        N::Do(E::Call {
            func: "ensure".into(),
            args: vec![bin(BinOp::Add, Kind::I32, i32c(4), getl("len"))],
        }),
        N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
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
        N::SetGlobal {
            global: "heap".into(),
            value: bin(BinOp::Add, Kind::I32, bin(BinOp::Add, Kind::I32, getl("res"), i32c(4)), getl("len")),
        },
        N::Push(getl("res")),
    ];
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

/// `$str_eq(a: i32, b: i32) -> i32` — content equality of two `[len][bytes]`
/// strings: same pointer → 1; different length → 0; else compare bytes. Mirrors
/// `STR_EQ_WAT`. Byte reads via `Load8U`; no heap/import/table.
pub fn str_eq_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let bin = |op: BinOp, l: E, r: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let load_i32 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    // byte a[4+i] vs b[4+i]
    let byte_at = |base: &str| E::Load8U {
        ptr: Box::new(bin(BinOp::Add, bin(BinOp::Add, getl(base), i32c(4)), getl("i"))),
        offset: 0,
    };
    WirFunc {
        name: "str_eq".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Str },
            WirLocal { name: "b".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool], // i32
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
        ],
        body: vec![
            // same pointer → equal
            N::If {
                cond: bin(BinOp::Eq, getl("a"), getl("b")),
                then_: vec![N::Return(Some(i32c(1)))],
                els: vec![],
                result: None,
            },
            // different length → not equal
            N::If {
                cond: bin(BinOp::Ne, load_i32(getl("a")), load_i32(getl("b"))),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "len".into(), value: load_i32(getl("a")) },
            N::SetLocal { local: "i".into(), value: i32c(0) },
            N::Block {
                label: "done".into(),
                result: None,
                body: vec![N::Loop {
                    label: "l".into(),
                    body: vec![
                        N::Br {
                            target: "done".into(),
                            cond: Some(bin(BinOp::Ge, getl("i"), getl("len"))),
                        },
                        N::If {
                            cond: bin(BinOp::Ne, byte_at("a"), byte_at("b")),
                            then_: vec![N::Return(Some(i32c(0)))],
                            els: vec![],
                            result: None,
                        },
                        N::SetLocal {
                            local: "i".into(),
                            value: bin(BinOp::Add, getl("i"), i32c(1)),
                        },
                        N::Br { target: "l".into(), cond: None },
                    ],
                }],
            },
            N::Push(i32c(1)),
        ],
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
            // NaN on either side → trap (matches the interpreter).
            N::If {
                cond: E::Binary {
                    op: BinOp::Or,
                    kind: Kind::I32,
                    lhs: Box::new(is_nan("a")),
                    rhs: Box::new(is_nan("b")),
                },
                then_: vec![N::Unreachable],
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

/// `$str_cmp(a: i32, b: i32) -> i32` — byte-lexicographic comparison of two
/// `[len][bytes]` strings: negative if `a < b`, zero if equal, positive if
/// `a > b`. Compares up to the shorter length, then breaks ties by length.
/// Mirrors `STR_CMP_WAT`; byte reads via `Load8U`, no heap/import/table.
pub fn str_cmp_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let bin = |op: BinOp, l: E, r: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let load_i32 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let byte_at = |base: &str| E::Load8U {
        ptr: Box::new(bin(BinOp::Add, bin(BinOp::Add, getl(base), i32c(4)), getl("i"))),
        offset: 0,
    };
    WirFunc {
        name: "str_cmp".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Str },
            WirLocal { name: "b".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool], // i32
        locals: vec![
            WirLocal { name: "alen".into(), ty: WirTy::Bool },
            WirLocal { name: "blen".into(), ty: WirTy::Bool },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "ca".into(), ty: WirTy::Bool },
            WirLocal { name: "cb".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "alen".into(), value: load_i32(getl("a")) },
            N::SetLocal { local: "blen".into(), value: load_i32(getl("b")) },
            // n = min(alen, blen)
            N::SetLocal { local: "n".into(), value: getl("blen") },
            N::If {
                cond: bin(BinOp::Lt, getl("alen"), getl("blen")),
                then_: vec![N::SetLocal { local: "n".into(), value: getl("alen") }],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "i".into(), value: i32c(0) },
            N::Block {
                label: "done".into(),
                result: None,
                body: vec![N::Loop {
                    label: "l".into(),
                    body: vec![
                        N::Br {
                            target: "done".into(),
                            cond: Some(bin(BinOp::Ge, getl("i"), getl("n"))),
                        },
                        N::SetLocal { local: "ca".into(), value: byte_at("a") },
                        N::SetLocal { local: "cb".into(), value: byte_at("b") },
                        N::If {
                            cond: bin(BinOp::Ne, getl("ca"), getl("cb")),
                            then_: vec![N::Return(Some(bin(BinOp::Sub, getl("ca"), getl("cb"))))],
                            els: vec![],
                            result: None,
                        },
                        N::SetLocal {
                            local: "i".into(),
                            value: bin(BinOp::Add, getl("i"), i32c(1)),
                        },
                        N::Br { target: "l".into(), cond: None },
                    ],
                }],
            },
            N::Push(bin(BinOp::Sub, getl("alen"), getl("blen"))),
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
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![add(i32c(4), add(getl("alen"), getl("blen")))],
            }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
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
            // heap = res + 4 + alen + blen
            N::SetGlobal {
                global: "heap".into(),
                value: add(add(getl("res"), i32c(4)), add(getl("alen"), getl("blen"))),
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

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
    let inplace = vec![
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
        N::Do(E::Call {
            func: "ensure".into(),
            args: vec![b32(BinOp::Add, i32c(4), b32(BinOp::Mul, getl("newcap"), i32c(8)))],
        }),
        N::SetLocal { local: "new".into(), value: E::GetGlobal("heap".into()) },
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
        N::SetGlobal {
            global: "heap".into(),
            value: b32(
                BinOp::Add,
                b32(BinOp::Add, getl("new"), i32c(4)),
                b32(BinOp::Mul, getl("newcap"), i32c(8)),
            ),
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
        N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("newcap"), i32c(8)))] }),
        N::SetLocal { local: "new".into(), value: E::GetGlobal("heap".into()) },
        N::Store { ptr: getl("new"), value: getl("len"), kind: Kind::I32, offset: 0 },
        N::MemoryCopy { dest: b(BinOp::Add, getl("new"), i32c(4)), src: b(BinOp::Add, getl("list"), i32c(4)), len: b(BinOp::Mul, getl("len"), i32c(8)) },
        N::Store { ptr: slot("new"), value: getl("x"), kind: Kind::I64, offset: 0 },
        N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, getl("newcap"), i32c(8))) },
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
            type_arity: 1,
            args: vec![getl("clos"), E::Load { ptr: Box::new(slot("list")), kind: Kind::I64, offset: 0 }],
            index: Box::new(E::Load { ptr: Box::new(getl("clos")), kind: Kind::I32, offset: 0 }),
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
        N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("newcap"), i32c(8)))] }),
        N::SetLocal { local: "nb".into(), value: E::GetGlobal("heap".into()) },
        N::Store { ptr: getl("nb"), value: getl("len"), kind: Kind::I32, offset: 0 },
        N::MemoryCopy { dest: b(BinOp::Add, getl("nb"), i32c(4)), src: b(BinOp::Add, getl("list"), i32c(4)), len: b(BinOp::Mul, getl("len"), i32c(8)) },
        N::Store { ptr: b(BinOp::Add, b(BinOp::Add, getl("nb"), i32c(4)), b(BinOp::Mul, getl("index"), i32c(8))), value: getl("nv"), kind: Kind::I64, offset: 0 },
        N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("nb"), i32c(4)), b(BinOp::Mul, getl("newcap"), i32c(8))) },
        N::SetLocal { local: "ret_ptr".into(), value: getl("nb") },
        N::SetLocal { local: "ret_cap".into(), value: getl("newcap") },
    ];
    WirFunc {
        name: "list_update_cap".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "index".into(), ty: WirTy::Bool },
            WirLocal { name: "clos".into(), ty: WirTy::Bool },
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
    let inplace = vec![
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
        N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), getl("newcap"))] }),
        N::SetLocal { local: "new".into(), value: E::GetGlobal("heap".into()) },
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
        N::SetGlobal {
            global: "heap".into(),
            value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), getl("newcap")),
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
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, b(BinOp::Add, getl("len"), i32c(1)), i32c(8)))],
            }),
            N::SetLocal { local: "new".into(), value: E::GetGlobal("heap".into()) },
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
            N::SetGlobal {
                global: "heap".into(),
                value: b(
                    BinOp::Add,
                    b(BinOp::Add, getl("new"), i32c(4)),
                    b(BinOp::Mul, b(BinOp::Add, getl("len"), i32c(1)), i32c(8)),
                ),
            },
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$find_byte(s: i32, sub: i32) -> i32` — index of the first occurrence of
/// `sub` in `s` (byte-wise), or `-1`; empty `sub` → 0. Mirrors `FIND_BYTE_WAT`
/// (a scan loop with an inner byte-compare loop; the inner mismatch `br` lives
/// inside an `if`, which the encoder must count as a branch frame). No
/// heap/import/table.
pub fn find_byte_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("i"), getl("j")))), offset: 4 };
    let sub_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("sub"), getl("j"))), offset: 4 };
    let cmp_loop = N::Block {
        label: "cmpdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cmp".into(),
            body: vec![
                N::Br { target: "cmpdone".into(), cond: Some(b(BinOp::Ge, getl("j"), getl("sublen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, sub_byte),
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
                    cond: Some(b(BinOp::Gt, getl("i"), b(BinOp::Sub, getl("slen"), getl("sublen")))),
                },
                setl("match", i32c(1)),
                setl("j", i32c(0)),
                cmp_loop,
                N::If { cond: getl("match"), then_: vec![N::Return(Some(getl("i")))], els: vec![], result: None },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "scan".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "find_byte".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "sub".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: ["slen", "sublen", "i", "j", "match"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("slen", load(getl("s"))),
            setl("sublen", load(getl("sub"))),
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("sublen")) },
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            scan_loop,
            N::Push(i32c(-1)),
        ],
        raw_body: None,
    }
}

/// `$starts_with(s, p) -> i32` — 1 iff string `s` begins with prefix `p`.
/// Byte-compares `p`'s bytes against `s`'s leading bytes; bails to 0 the moment a
/// byte differs or `p` is longer than `s`.
pub fn starts_with_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 };
    let p_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("p"), getl("i"))), offset: 4 };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("plen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, p_byte),
                    then_: vec![N::Return(Some(i32c(0)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "starts_with".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "p".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: ["plen", "i"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("plen", load(getl("p"))),
            N::If {
                cond: b(BinOp::Gt, getl("plen"), load(getl("s"))),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            scan_loop,
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$ends_with(s, p) -> i32` — 1 iff string `s` ends with suffix `p`.
/// Like `$starts_with`, but the comparison window into `s` is shifted by
/// `off = len(s) - len(p)`; bails to 0 if `p` is longer than `s`.
pub fn ends_with_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U {
        ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("off"), getl("i")))),
        offset: 4,
    };
    let p_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("p"), getl("i"))), offset: 4 };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("plen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, p_byte),
                    then_: vec![N::Return(Some(i32c(0)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "ends_with".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "p".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: ["plen", "off", "i"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("plen", load(getl("p"))),
            setl("off", b(BinOp::Sub, load(getl("s")), getl("plen"))),
            N::If {
                cond: b(BinOp::Lt, getl("off"), i32c(0)),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            scan_loop,
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$byte_to_char(s, bytelen) -> i32` — the count of UTF-8 *characters* in the
/// first `bytelen` bytes of `s`. Continuation bytes (`0b10xxxxxx`) don't start a
/// character, so they're skipped; every other byte increments the count.
/// `$char_count(s: i32) -> i32` — the number of Unicode scalars in `s`: just
/// `byte_to_char(s, len(s))`, reading the byte-length header itself so the caller
/// evaluates `s` once. Mirrors the `string.char_count` legacy emission.
pub fn char_count_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    WirFunc {
        name: "char_count".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Bool], // i32
        locals: vec![],
        body: vec![N::Push(E::Call {
            func: "byte_to_char".into(),
            args: vec![
                E::GetLocal("s".into()),
                E::Load { ptr: Box::new(E::GetLocal("s".into())), kind: Kind::I32, offset: 0 },
            ],
        })],
        raw_body: None,
    }
}

pub fn byte_to_char_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("bytelen"))) },
                setl("b", byte),
                N::If {
                    cond: b(BinOp::Ne, b(BinOp::And, getl("b"), i32c(0xc0)), i32c(0x80)),
                    then_: vec![setl("count", b(BinOp::Add, getl("count"), i32c(1)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "byte_to_char".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "bytelen".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["i", "count", "b"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![setl("i", i32c(0)), setl("count", i32c(0)), scan_loop, N::Push(getl("count"))],
        raw_body: None,
    }
}

/// `$str_index_of(s, sub) -> i32` — the *character* index where `sub` first
/// occurs in `s`, or -1 if absent. `$find_byte` gives the byte offset; this maps
/// it back to a character index via `$byte_to_char`.
pub fn str_index_of_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    WirFunc {
        name: "str_index_of".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "sub".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![WirLocal { name: "bidx".into(), ty: WirTy::Bool }],
        body: vec![
            setl("bidx", E::Call { func: "find_byte".into(), args: vec![getl("s"), getl("sub")] }),
            N::If {
                cond: b(BinOp::Lt, getl("bidx"), i32c(0)),
                then_: vec![N::Push(i32c(-1))],
                els: vec![N::Push(E::Call {
                    func: "byte_to_char".into(),
                    args: vec![getl("s"), getl("bidx")],
                })],
                result: Some(WirTy::Bool),
            },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![add(i32c(4), getl("len"))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::MemoryCopy {
                dest: add(getl("res"), i32c(4)),
                src: add(add(getl("src"), i32c(4)), getl("start")),
                len: getl("len"),
            },
            N::SetGlobal {
                global: "heap".into(),
                value: add(add(getl("res"), i32c(4)), getl("len")),
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$char_to_byte(s, n) -> i32` — the *byte* offset of the `n`-th character of
/// `s` (the inverse of `$byte_to_char`). Walks UTF-8 sequences, stepping the byte
/// cursor by 1/2/3/4 per character based on the lead byte, until `n` chars (or
/// the end) are consumed.
pub fn char_to_byte_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // seqlen = b<0x80 ? 1 : b<0xe0 ? 2 : b<0xf0 ? 3 : 4 — nested if-statements
    // setting the `seqlen` local (avoids an expression-level conditional).
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
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("slen"))) },
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("count"), getl("n"))) },
                setl("b", E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 }),
                seqlen,
                setl("i", b(BinOp::Add, getl("i"), getl("seqlen"))),
                setl("count", b(BinOp::Add, getl("count"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "char_to_byte".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["slen", "i", "count", "b", "seqlen"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("slen", load(getl("s"))),
            setl("i", i32c(0)),
            setl("count", i32c(0)),
            scan_loop,
            N::Push(getl("i")),
        ],
        raw_body: None,
    }
}

/// `$str_substring(s, start, end) -> i32` — the substring of `s` between the
/// *character* indices `start` and `end`. Maps both ends to byte offsets via
/// `$char_to_byte`, then `$substr`s the byte slice; an empty slice when the
/// bounds cross.
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
            WirLocal { name: "start".into(), ty: WirTy::Bool },
            WirLocal { name: "end".into(), ty: WirTy::Bool },
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
                N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), cap_slots.clone())] }),
                setl("result", E::GetGlobal("heap".into())),
                N::SetGlobal {
                    global: "heap".into(),
                    value: b(BinOp::Add, b(BinOp::Add, getl("result"), i32c(4)), cap_slots),
                },
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
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("slen"), i32c(8)))],
            }),
            setl("result", E::GetGlobal("heap".into())),
            N::SetGlobal {
                global: "heap".into(),
                value: b(BinOp::Add, b(BinOp::Add, getl("result"), i32c(4)), b(BinOp::Mul, getl("slen"), i32c(8))),
            },
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
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, total.clone(), i32c(8)))],
            }),
            setl("new", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("new"), value: total.clone(), kind: Kind::I32, offset: 0 },
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
            N::SetGlobal {
                global: "heap".into(),
                value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, total, i32c(8))),
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
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), getl("len"))] }),
            setl("res", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            scan_loop,
            N::SetGlobal {
                global: "heap".into(),
                value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")),
            },
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
                // overflow: acc >u (limit - d) / 10  ->  trap.
                N::If {
                    cond: b64(
                        BinOp::GtU,
                        getl("acc"),
                        b64(BinOp::DivU, b64(BinOp::Sub, getl("limit"), digit()), i64c(10)),
                    ),
                    then_: vec![N::Unreachable],
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
                then_: vec![N::Unreachable],
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
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(8)] }),
            N::SetLocal { local: "p".into(), value: b(BinOp::Add, E::GetGlobal("heap".into()), i32c(4)) },
            N::Store { ptr: b(BinOp::Sub, getl("p"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store { ptr: getl("p"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("p"), i32c(4)) },
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
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, i32c(24), b(BinOp::Mul, getl("count"), i32c(16)))],
            }),
            setl("found", E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] }),
            setl("bytes", b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(16)))),
            setl("new", b(BinOp::Add, E::GetGlobal("heap".into()), i32c(4))),
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
                    N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("new"), getl("bytes")) },
                    N::Push(getl("new")),
                ],
                els: vec![
                    N::Store { ptr: getl("new"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                    N::Store { ptr: b(BinOp::Add, getl("new"), getl("bytes")), value: getl("k"), kind: Kind::I64, offset: 0 },
                    N::Store { ptr: b(BinOp::Add, getl("new"), getl("bytes")), value: getl("v"), kind: Kind::I64, offset: 8 },
                    N::SetGlobal {
                        global: "heap".into(),
                        value: b(BinOp::Add, b(BinOp::Add, getl("new"), getl("bytes")), i32c(16)),
                    },
                    N::Push(getl("new")),
                ],
                result: Some(WirTy::Bool),
            },
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
    let append_inplace = vec![
        N::Store { ptr: entry("d", "count"), value: getl("k"), kind: Kind::I64, offset: 4 },
        N::Store { ptr: entry("d", "count"), value: getl("v"), kind: Kind::I64, offset: 12 },
        N::Store { ptr: getl("d"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
        // Record the new entry (index == the old `count`) in the hash index. The
        // index was built at the last grow sized ≥ 2× cap, so it has a free slot.
        N::SetLocal { local: "idx".into(), value: E::Load { ptr: Box::new(b(BinOp::Sub, getl("d"), i32c(4))), kind: Kind::I32, offset: 0 } },
        N::If {
            cond: getl("idx"),
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
        N::Do(E::Call {
            func: "ensure".into(),
            args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("newcap"), i32c(16)))],
        }),
        N::SetLocal { local: "new".into(), value: b(BinOp::Add, E::GetGlobal("heap".into()), i32c(4)) },
        N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
        N::SetLocal { local: "bytes".into(), value: b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(16))) },
        N::MemoryCopy { dest: getl("new"), src: getl("d"), len: getl("bytes") },
        N::SetGlobal {
            global: "heap".into(),
            value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, getl("newcap"), i32c(16))),
        },
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
        // Build a fresh hash index for the copied buffer, sized to the smallest
        // power of two ≥ 2× newcap (and ≥ 16) so subsequent in-place appends into
        // the slack never overflow it (load factor stays ≤ 0.5). Populated by
        // probing every live entry; this O(count) build amortizes against the copy.
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
        N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("islots"), i32c(4)))] }),
        N::SetLocal { local: "iptr".into(), value: E::GetGlobal("heap".into()) },
        N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("iptr"), b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("islots"), i32c(4)))) },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(8)))] }),
            setl("new", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("new"), value: getl("count"), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            scan,
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, getl("count"), i32c(8))) },
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
                setl("tup", E::GetGlobal("heap".into())),
                N::Store { ptr: getl("tup"), value: i32c(0), kind: Kind::I32, offset: 0 },
                N::Store { ptr: getl("tup"), value: entry(4), kind: Kind::I64, offset: 4 },
                N::Store { ptr: getl("tup"), value: entry(12), kind: Kind::I64, offset: 12 },
                N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("tup"), i32c(20)) },
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
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(8))), b(BinOp::Mul, getl("count"), i32c(20)))],
            }),
            setl("list", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("list"), value: getl("count"), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("list"), i32c(4)), b(BinOp::Mul, getl("count"), i32c(8))) },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("count"), i32c(16)))] }),
            setl("new", b(BinOp::Add, E::GetGlobal("heap".into()), i32c(4))),
            N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            setl("n", i32c(0)),
            scan,
            N::Store { ptr: getl("new"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, getl("n"), i32c(16))) },
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

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
        setl("res", E::GetGlobal("heap".into())),
        setl("dst", b(BinOp::Add, getl("res"), i32c(4))),
        N::MemoryCopy { dest: getl("dst"), src: to_bytes.clone(), len: getl("tlen") },
        setl("dst", b(BinOp::Add, getl("dst"), getl("tlen"))),
        setl("src", i32c(0)),
        empty_loop,
        setl("reslen", b(BinOp::Sub, getl("dst"), b(BinOp::Add, getl("res"), i32c(4)))),
        N::Store { ptr: getl("res"), value: getl("reslen"), kind: Kind::I32, offset: 0 },
        N::SetGlobal { global: "heap".into(), value: getl("dst") },
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
            N::Do(E::Call {
                func: "ensure".into(),
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
            setl("res", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("res"), value: getl("reslen"), kind: Kind::I32, offset: 0 },
            setl("dst", b(BinOp::Add, getl("res"), i32c(4))),
            setl("src", i32c(0)),
            fill_loop,
            N::SetGlobal { global: "heap".into(), value: getl("dst") },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$encoding(op, in) -> i32` — a thin wrapper over the host `encoding` import,
/// which does the actual hex/base64 transform (op 0 hex-encode, 1 hex-decode,
/// 2 base64-encode, 3 base64-decode, 4 base64url-of-hex). Reserves a worst-case
/// `2*len + 20` result buffer, lets the host write into `res+4`, and caps the
/// length header to what it returned. The first migrated host-import helper.
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
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, b(BinOp::Mul, E::Load { ptr: Box::new(getl("in")), kind: Kind::I32, offset: 0 }, i32c(2)), i32c(20))],
            }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::SetLocal {
                local: "n".into(),
                value: E::CallHost { import: "encoding".into(), args: vec![getl("op"), getl("in"), b(BinOp::Add, getl("res"), i32c(4))] },
            },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("n")) },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(hexlen + 4)] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: i32c(hexlen), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: import.into(), args: host_args }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), i32c(hexlen + 4)) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// A keyed crypto op on a `Secret` — `crypto.sign(key, msg)` / `crypto.public_key(key)`.
/// `key` is the Secret HANDLE (an i32 index into the host secret table); the host
/// signs / derives the public key with the never-exposed bytes and writes `hexlen`
/// hex chars. (Separate from `crypto_hash_helper`, whose inputs are all strings.)
fn crypto_keyed_helper(name: &str, import: &str, hexlen: i32, has_msg: bool) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let mut params = vec![WirLocal { name: "key".into(), ty: WirTy::Bool }];
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
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(hexlen + 4)] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: i32c(hexlen), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: import.into(), args: host_args }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), i32c(hexlen + 4)) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$dir_read(h, rel) -> i32` — the contents of file `rel` under dir handle `h`,
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
            WirLocal { name: "h".into(), ty: WirTy::Bool },
            WirLocal { name: "rel".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "dir_read_len".into(), args: vec![getl("h"), getl("rel")] } },
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$file_read(f) -> i32` — the contents of file handle `f` as a String (RFC-0012).
/// A `File` is a leaf (no path), so this takes only the handle. Two-phase host
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
        params: vec![WirLocal { name: "f".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "file_read_len".into(), args: vec![getl("f")] } },
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$exec(h, path, args, stdin) -> i32` — spawn the executable `path` under Dir
/// handle `h` (confined like `dir_read`), passing the `\0`-joined argv `args` and
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
            WirLocal { name: "h".into(), ty: WirTy::Bool },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$crypto_reveal(key) -> i32` — the raw bytes of the secret at handle `key` as
/// a fresh String (lossy UTF-8). Identical staging to [`dir_read_helper`]: the
/// host `crypto_reveal_len` reads the host-side secret and reports its byte
/// length (staging the bytes), then `fill_pending` copies them into `res+4`. For
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
        params: vec![WirLocal { name: "key".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "crypto_reveal_len".into(), args: vec![getl("key")] } },
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$build_read(h, rel) -> i32` — the confined build file's contents as a fresh
/// string. Identical staging to [`dir_read_helper`], but the host length import
/// (`build_read_len`) resolves `rel` against the granted build *read roots*, not
/// a Dir handle. The build sandbox's read side.
pub fn build_read_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "build_read".into(),
        params: vec![
            WirLocal { name: "h".into(), ty: WirTy::Bool },
            WirLocal { name: "rel".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "build_read_len".into(), args: vec![getl("h"), getl("rel")] } },
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")) },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$dir_list(h) -> i32` — the entries of directory handle `h`, as a
/// `List(String)`. The host reports the total byte size of the marshaled list
/// (`dir_list_size`), then writes the whole `[count][ptr..]` + payload structure
/// into the reserved block (`write_pending_list`). Needs the Dir(Read) capability.
pub fn dir_list_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dir_list".into(),
        params: vec![WirLocal { name: "h".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "size".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "size".into(), value: E::CallHost { import: "dir_list_size".into(), args: vec![getl("h")] } },
            N::Do(E::Call { func: "ensure".into(), args: vec![getl("size")] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Do(E::CallHost { import: "write_pending_list".into(), args: vec![getl("res")] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), getl("size")) },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("newlen"), i32c(8)))] }),
            N::SetLocal { local: "new".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("new"), value: getl("newlen"), kind: Kind::I32, offset: 0 },
            N::MemoryCopy {
                dest: b(BinOp::Add, getl("new"), i32c(4)),
                src: b(BinOp::Add, b(BinOp::Add, getl("list"), i32c(4)), b(BinOp::Mul, getl("k"), i32c(8))),
                len: b(BinOp::Mul, getl("newlen"), i32c(8)),
            },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, getl("newlen"), i32c(8))) },
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
                    N::Do(E::Call { func: "ensure".into(), args: vec![i32c(4)] }),
                    N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
                    N::Store { ptr: getl("res"), value: i32c(1), kind: Kind::I32, offset: 0 },
                    N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), i32c(4)) },
                    N::Return(Some(getl("res"))),
                ],
                els: vec![],
                result: None,
            },
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "str".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("str"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "env_fill".into(), args: vec![getl("name"), b(BinOp::Add, getl("str"), i32c(4))] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("str"), i32c(4)), getl("len")) },
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(12)] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store { ptr: getl("res"), value: E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("str")) }, kind: Kind::I64, offset: 4 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), i32c(12)) },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(516)] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::SetLocal { local: "n".into(), value: E::CallHost { import: "float_to_str".into(), args: vec![getl("x"), b(BinOp::Add, getl("res"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("n")) },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(8)] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::SetLocal {
                local: "n".into(),
                value: E::CallHost {
                    import: "string_from_code".into(),
                    args: vec![getl("cp"), b(BinOp::Add, getl("res"), i32c(4))],
                },
            },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::SetGlobal {
                global: "heap".into(),
                value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("n")),
            },
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
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
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
            N::Do(E::Call { func: "ensure".into(), args: vec![getl("size")] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Do(E::CallHost { import: "write_pending_list".into(), args: vec![getl("res")] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), getl("size")) },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::SetGlobal {
                global: "heap".into(),
                value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")),
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// A thin host-import wrapper `$name(a0..a{nargs-1}) -> i32` = `CallHost(import,
/// [a0..])`. Routing an inline host call through a registered helper keeps the
/// user body free of direct `CallHost`s — so the capability-minimal prune isn't
/// deferred (`no_direct_host` stays true) — and declares the import via
/// `import_deps`. Used for the 2-arg `Dir` ops (subdir/exists/is_dir).
fn host_call_helper(name: &str, import: &str, nargs: usize) -> WirFunc {
    host_call_helper_ret(name, import, nargs, WirTy::Bool)
}

/// Like [`host_call_helper`] but with an explicit result type — for host imports
/// whose result isn't the default i32 handle/pointer (e.g. `now` returns an i64).
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
    let mut params = vec![WirLocal { name: "s".into(), ty: WirTy::Bool }];
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
            N::Do(E::Call { func: "ensure".into(), args: vec![add(getl("len"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![add(getl("res"), i32c(4))] }),
            N::SetGlobal {
                global: "heap".into(),
                value: add(add(getl("res"), i32c(4)), getl("len")),
            },
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
            N::Do(E::Call { func: "ensure".into(), args: vec![getl("size")] }),
            N::SetLocal { local: "n".into(), value: E::GetGlobal("heap".into()) },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("n"), getl("size")) },
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

/// A WIR-native prelude helper plus the module-level resources it needs (so a
/// pruned module declares only the imports/globals/table its reached helpers
/// actually touch — capability-minimal).
pub struct WirHelperSpec {
    pub func: WirFunc,
    /// Other prelude helpers this one calls (transitively pulled in).
    pub helper_deps: &'static [&'static str],
    /// Host imports (the `witchy` field names) this helper calls directly.
    pub import_deps: &'static [&'static str],
    /// Whether it reads/writes the `$heap` / `$__witchy_reowns` globals.
    pub uses_heap: bool,
    /// Whether it does a `call_indirect` (needs table 0).
    pub uses_table: bool,
}

/// Look up a runtime helper by name, returning its [`WirHelperSpec`]: the
/// function plus the other helpers and host imports it depends on. Returns
/// `None` for a name with no WIR-native helper, in which case `wir_encode` falls
/// back to the raw-body prelude blob (`wir_prelude`).
pub fn wir_helper(name: &str) -> Option<WirHelperSpec> {
    match name {
        "print_str" => Some(WirHelperSpec {
            func: print_str_helper(),
            helper_deps: &[],
            import_deps: &["print"],
            uses_heap: false,
            uses_table: false,
        }),
        "ensure" => Some(WirHelperSpec {
            func: ensure_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_at" => Some(WirHelperSpec {
            func: list_at_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "int_to_string" => Some(WirHelperSpec {
            func: int_to_string_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "str_eq" => Some(WirHelperSpec {
            func: str_eq_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "find_byte" => Some(WirHelperSpec {
            func: find_byte_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "starts_with" => Some(WirHelperSpec {
            func: starts_with_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "ends_with" => Some(WirHelperSpec {
            func: ends_with_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "byte_to_char" => Some(WirHelperSpec {
            func: byte_to_char_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "char_count" => Some(WirHelperSpec {
            func: char_count_helper(),
            helper_deps: &["byte_to_char"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "substr" => Some(WirHelperSpec {
            func: substr_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "char_to_byte" => Some(WirHelperSpec {
            func: char_to_byte_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "str_substring" => Some(WirHelperSpec {
            func: str_substring_helper(),
            helper_deps: &["char_to_byte", "substr"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "is_ws" => Some(WirHelperSpec {
            func: is_ws_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "trim" => Some(WirHelperSpec {
            func: trim_helper(),
            helper_deps: &["is_ws", "substr"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "str_index_of" => Some(WirHelperSpec {
            func: str_index_of_helper(),
            helper_deps: &["find_byte", "byte_to_char"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "concat" => Some(WirHelperSpec {
            func: concat_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_push_cap" => Some(WirHelperSpec {
            func: list_push_cap_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_push" => Some(WirHelperSpec {
            func: list_push_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "split" => Some(WirHelperSpec {
            func: split_helper(),
            helper_deps: &["ensure", "substr", "list_push"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "str_chars" => Some(WirHelperSpec {
            func: str_chars_helper(),
            helper_deps: &["ensure", "byte_to_char", "str_substring", "list_push"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_concat" => Some(WirHelperSpec {
            func: list_concat_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_drop" => Some(WirHelperSpec {
            func: list_drop_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "ascii_case" => Some(WirHelperSpec {
            func: ascii_case_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "match_at" => Some(WirHelperSpec {
            func: match_at_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "encoding" => Some(WirHelperSpec {
            func: encoding_helper(),
            helper_deps: &["ensure"],
            import_deps: &["encoding"],
            uses_heap: true,
            uses_table: false,
        }),
        "string_from_code" => Some(WirHelperSpec {
            func: string_from_code_helper(),
            helper_deps: &["ensure"],
            import_deps: &["string_from_code"],
            uses_heap: true,
            uses_table: false,
        }),
        "build_args" => Some(WirHelperSpec {
            func: build_args_helper(),
            helper_deps: &["ensure"],
            import_deps: &["args_size", "write_pending_list"],
            uses_heap: true,
            uses_table: false,
        }),
        "compiler_footprint" => Some(WirHelperSpec {
            func: compiler_introspect_helper("compiler_footprint", "compiler_footprint_len", 1),
            helper_deps: &["ensure"],
            import_deps: &["compiler_footprint_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "compiler_diff" => Some(WirHelperSpec {
            func: compiler_introspect_helper("compiler_diff", "compiler_diff_len", 2),
            helper_deps: &["ensure"],
            import_deps: &["compiler_diff_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "compiler_doc" => Some(WirHelperSpec {
            func: compiler_introspect_helper("compiler_doc", "compiler_doc_len", 2),
            helper_deps: &["ensure"],
            import_deps: &["compiler_doc_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "now" => Some(WirHelperSpec {
            func: host_call_helper_ret("now", "now", 0, WirTy::Int),
            helper_deps: &[],
            import_deps: &["now"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_subdir" => Some(WirHelperSpec {
            func: host_call_helper("dir_subdir", "dir_subdir", 2),
            helper_deps: &[],
            import_deps: &["dir_subdir"],
            uses_heap: false,
            uses_table: false,
        }),
        // RFC-0012: `dir.open`/`dir.create` navigate a Dir to a confined File
        // handle (i32); `file_write` writes a File handle (void). Each wraps its
        // host import so user code stays free of direct CallHosts.
        "dir_open" => Some(WirHelperSpec {
            func: host_call_helper("dir_open", "dir_open", 2),
            helper_deps: &[],
            import_deps: &["dir_open"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_create" => Some(WirHelperSpec {
            func: host_call_helper("dir_create", "dir_create", 2),
            helper_deps: &[],
            import_deps: &["dir_create"],
            uses_heap: false,
            uses_table: false,
        }),
        "file_write" => Some(WirHelperSpec {
            func: host_void_helper("file_write", "file_write", 2),
            helper_deps: &[],
            import_deps: &["file_write"],
            uses_heap: false,
            uses_table: false,
        }),
        // Resolve a named secret to its host-table handle (an i32 index, or -1
        // if absent). Wraps the `secretstore_lookup` host import so user code
        // stays free of direct CallHosts; the bytes never enter the guest.
        "secretstore_lookup" => Some(WirHelperSpec {
            func: host_call_helper("secretstore_lookup", "secretstore_lookup", 1),
            helper_deps: &[],
            import_deps: &["secretstore_lookup"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_exists" => Some(WirHelperSpec {
            func: host_call_helper("dir_exists", "dir_exists", 2),
            helper_deps: &[],
            import_deps: &["dir_exists"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_is_dir" => Some(WirHelperSpec {
            func: host_call_helper("dir_is_dir", "dir_is_dir", 2),
            helper_deps: &[],
            import_deps: &["dir_is_dir"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_write" => Some(WirHelperSpec {
            func: host_void_helper("dir_write", "dir_write", 3),
            helper_deps: &[],
            import_deps: &["dir_write"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_append" => Some(WirHelperSpec {
            func: host_void_helper("dir_append", "dir_append", 3),
            helper_deps: &[],
            import_deps: &["dir_append"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_make_dir" => Some(WirHelperSpec {
            func: host_void_helper("dir_make_dir", "dir_make_dir", 2),
            helper_deps: &[],
            import_deps: &["dir_make_dir"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_connect" => Some(WirHelperSpec {
            func: host_call_helper("net_connect", "net_connect", 2),
            helper_deps: &[],
            import_deps: &["net_connect"],
            uses_heap: false,
            uses_table: false,
        }),
        // Fallible dial: returns the socket handle, or the `-1` sentinel on a
        // failed connect (the codegen `try_connect` case wraps that as
        // `Option(Socket)`'s `None`). A capability violation still traps host-side.
        "net_try_connect" => Some(WirHelperSpec {
            func: host_call_helper("net_try_connect", "net_try_connect", 2),
            helper_deps: &[],
            import_deps: &["net_try_connect"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_listen" => Some(WirHelperSpec {
            func: host_call_helper("net_listen", "net_listen", 2),
            helper_deps: &[],
            import_deps: &["net_listen"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_accept" => Some(WirHelperSpec {
            func: host_call_helper("net_accept", "net_accept", 1),
            helper_deps: &[],
            import_deps: &["net_accept"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_restrict" => Some(WirHelperSpec {
            func: host_call_helper("net_restrict", "net_restrict", 2),
            helper_deps: &[],
            import_deps: &["net_restrict"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_deny" => Some(WirHelperSpec {
            func: host_call_helper("net_deny", "net_deny", 2),
            helper_deps: &[],
            import_deps: &["net_deny"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_send_line" => Some(WirHelperSpec {
            func: host_void_helper("net_send_line", "net_send_line", 2),
            helper_deps: &[],
            import_deps: &["net_send_line"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_send_bytes" => Some(WirHelperSpec {
            func: host_void_helper("net_send_bytes", "net_send_bytes", 2),
            helper_deps: &[],
            import_deps: &["net_send_bytes"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_close" => Some(WirHelperSpec {
            func: host_void_helper("net_close", "net_close", 1),
            helper_deps: &[],
            import_deps: &["net_close"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_recv_line" => Some(WirHelperSpec {
            func: net_recv_helper("net_recv_line", "net_recv_line_len", false),
            helper_deps: &["ensure"],
            import_deps: &["net_recv_line_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "net_recv_all" => Some(WirHelperSpec {
            func: net_recv_helper("net_recv_all", "net_recv_all_len", false),
            helper_deps: &["ensure"],
            import_deps: &["net_recv_all_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "net_recv_bytes" => Some(WirHelperSpec {
            func: net_recv_helper("net_recv_bytes", "net_recv_bytes_len", true),
            helper_deps: &["ensure"],
            import_deps: &["net_recv_bytes_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        // The region globals ($rcopy_wm/$rcopy_base/$rcopy_delta/$__region_copy_bytes)
        // this touches are declared by `assemble` when `cg.uses_region` is set.
        "rcopy_str" => Some(WirHelperSpec {
            func: rcopy_str_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "float_to_str" => Some(WirHelperSpec {
            func: float_to_str_helper(),
            helper_deps: &["ensure"],
            import_deps: &["float_to_str"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sha256" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha256", "crypto.sha256", 64, &["in"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.sha256"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sha512" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha512", "crypto.sha512", 128, &["in"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.sha512"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sha3_256" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha3_256", "crypto.sha3_256", 64, &["in"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.sha3_256"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_hmac_sha256" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_hmac_sha256", "crypto.hmac_sha256", 64, &["key", "msg"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.hmac_sha256"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_rune_hash" => Some(WirHelperSpec {
            // paths + contents are List(String) pointers; the host hashes them
            // into a fixed 71-char digest.
            func: crypto_hash_helper("crypto_rune_hash", "crypto.rune_hash", 71, &["paths", "contents"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.rune_hash"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sign" => Some(WirHelperSpec {
            // The Secret capability: the host signs `msg` with the never-exposed
            // seed and writes a 128-char hex signature.
            func: crypto_keyed_helper("crypto_sign", "crypto.sign", 128, true),
            helper_deps: &["ensure"],
            import_deps: &["crypto.sign"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_public_key" => Some(WirHelperSpec {
            // No input — the host writes the seed's 64-char hex public key.
            func: crypto_keyed_helper("crypto_public_key", "crypto.public_key", 64, false),
            helper_deps: &["ensure"],
            import_deps: &["crypto.public_key"],
            uses_heap: true,
            uses_table: false,
        }),
        // The P-256 ECDSA verifies read three string headers (pubkey, message,
        // signature) and return an i32 bool — no capability, no allocation.
        "crypto_ecdsa_p256_verify" => Some(WirHelperSpec {
            func: host_call_helper("crypto_ecdsa_p256_verify", "crypto.ecdsa_p256_verify", 3),
            helper_deps: &[],
            import_deps: &["crypto.ecdsa_p256_verify"],
            uses_heap: false,
            uses_table: false,
        }),
        "crypto_ecdsa_p256_verify_hex" => Some(WirHelperSpec {
            func: host_call_helper("crypto_ecdsa_p256_verify_hex", "crypto.ecdsa_p256_verify_hex", 3),
            helper_deps: &[],
            import_deps: &["crypto.ecdsa_p256_verify_hex"],
            uses_heap: false,
            uses_table: false,
        }),
        "crypto_rsa_pkcs1_sha256_verify" => Some(WirHelperSpec {
            func: host_call_helper("crypto_rsa_pkcs1_sha256_verify", "crypto.rsa_pkcs1_sha256_verify", 3),
            helper_deps: &[],
            import_deps: &["crypto.rsa_pkcs1_sha256_verify"],
            uses_heap: false,
            uses_table: false,
        }),
        // ed25519 signature verify — three string headers → i32 bool, no
        // capability. Reached by the self-hosted package manager (coven/pm).
        "crypto_ed25519_verify" => Some(WirHelperSpec {
            func: host_call_helper("crypto_ed25519_verify", "crypto.ed25519_verify", 3),
            helper_deps: &[],
            import_deps: &["crypto.ed25519_verify"],
            uses_heap: false,
            uses_table: false,
        }),
        "regex_match_spans" => Some(WirHelperSpec {
            func: regex_match_spans_helper(),
            helper_deps: &["ensure"],
            import_deps: &["regex_match_spans_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "dir_read" => Some(WirHelperSpec {
            func: dir_read_helper(),
            helper_deps: &["ensure"],
            import_deps: &["dir_read_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "file_read" => Some(WirHelperSpec {
            func: file_read_helper(),
            helper_deps: &["ensure"],
            import_deps: &["file_read_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "exec" => Some(WirHelperSpec {
            func: exec_helper(),
            helper_deps: &["ensure"],
            import_deps: &["exec_run", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_reveal" => Some(WirHelperSpec {
            func: crypto_reveal_helper(),
            helper_deps: &["ensure"],
            import_deps: &["crypto_reveal_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "build_read" => Some(WirHelperSpec {
            func: build_read_helper(),
            helper_deps: &["ensure"],
            import_deps: &["build_read_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "build_out_write" => Some(WirHelperSpec {
            func: host_void_helper("build_out_write", "build_out_write", 3),
            helper_deps: &[],
            import_deps: &["build_out_write"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_list" => Some(WirHelperSpec {
            func: dir_list_helper(),
            helper_deps: &["ensure"],
            import_deps: &["dir_list_size", "write_pending_list"],
            uses_heap: true,
            uses_table: false,
        }),
        "get_env" => Some(WirHelperSpec {
            func: get_env_helper(),
            helper_deps: &["ensure"],
            import_deps: &["env_len", "env_fill"],
            uses_heap: true,
            uses_table: false,
        }),
        "replace" => Some(WirHelperSpec {
            func: replace_helper(),
            helper_deps: &["ensure", "match_at"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "str_to_int" => Some(WirHelperSpec {
            func: str_to_int_helper(),
            helper_deps: &["is_ws"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "key_eq" => Some(WirHelperSpec {
            func: key_eq_helper(),
            helper_deps: &["str_eq"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_hash" => Some(WirHelperSpec {
            func: dict_hash_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_find" => Some(WirHelperSpec {
            func: dict_find_helper(),
            helper_deps: &["key_eq", "dict_hash"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_new" => Some(WirHelperSpec {
            func: dict_new_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_insert" => Some(WirHelperSpec {
            func: dict_insert_helper(),
            helper_deps: &["ensure", "dict_find"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_get_or" => Some(WirHelperSpec {
            func: dict_get_or_helper(),
            helper_deps: &["dict_find"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_update" => Some(WirHelperSpec {
            func: dict_update_helper(),
            helper_deps: &["dict_get_or", "dict_insert"],
            import_deps: &[],
            uses_heap: true,
            uses_table: true,
        }),
        "dict_insert_cap" => Some(WirHelperSpec {
            func: dict_insert_cap_helper(),
            helper_deps: &["dict_find", "ensure", "dict_index_put"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_index_put" => Some(WirHelperSpec {
            func: dict_index_put_helper(),
            helper_deps: &["dict_hash"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "str_append_cap" => Some(WirHelperSpec {
            func: str_append_cap_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_set_cap" => Some(WirHelperSpec {
            func: list_set_cap_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_update_cap" => Some(WirHelperSpec {
            func: list_update_cap_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: true,
        }),
        "dict_update_cap" => Some(WirHelperSpec {
            func: dict_update_cap_helper(),
            helper_deps: &["dict_get_or", "dict_insert_cap"],
            import_deps: &[],
            uses_heap: true,
            uses_table: true,
        }),
        "dict_has" => Some(WirHelperSpec {
            func: dict_has_helper(),
            helper_deps: &["dict_find"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_keys" => Some(WirHelperSpec {
            func: dict_project_helper("dict_keys", 4),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_values" => Some(WirHelperSpec {
            func: dict_project_helper("dict_values", 12),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_pairs" => Some(WirHelperSpec {
            func: dict_pairs_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_remove" => Some(WirHelperSpec {
            func: dict_remove_helper(),
            helper_deps: &["ensure", "key_eq"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "f_lt" => Some(WirHelperSpec {
            func: float_cmp_helper("f_lt", BinOp::Lt),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "f_le" => Some(WirHelperSpec {
            func: float_cmp_helper("f_le", BinOp::Le),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "f_gt" => Some(WirHelperSpec {
            func: float_cmp_helper("f_gt", BinOp::Gt),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "f_ge" => Some(WirHelperSpec {
            func: float_cmp_helper("f_ge", BinOp::Ge),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "str_cmp" => Some(WirHelperSpec {
            func: str_cmp_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        _ => {
            // `$mk{n}`: the n-field aggregate allocators (each calls `$ensure`).
            // The WAT path emits one for any arity a tuple/record/closure needs, so
            // the registry must too — a 9-field record or a closure with 8+ captures
            // would otherwise reach an undeclared `$mk9`. The bound is a sanity cap
            // on parsing, far above any realistic aggregate.
            if let Some(rest) = name.strip_prefix("mk") {
                if let Ok(n) = rest.parse::<usize>() {
                    if n <= 256 {
                        return Some(WirHelperSpec {
                            func: mk_helper(n),
                            helper_deps: &["ensure"],
                            import_deps: &[],
                            uses_heap: true,
                            uses_table: false,
                        });
                    }
                }
            }
            None
        }
    }
}

