//! Host-backed string construction and region-copy helpers.

use crate::wir::*;

/// `$string_from_code(cp: i64) -> String` — in-WASM pure UTF-8 codepoint encoder.
pub(in crate::wir_helpers) fn string_from_code_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let store8 = |offset: u32, val: E| N::Store8 {
        ptr: getl("res"),
        value: val,
        offset: 4 + offset,
    };

    WirFunc {
        name: "string_from_code".into(),
        params: vec![WirLocal { name: "cp".into(), ty: WirTy::Int }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "res".into(), ty: WirTy::Bool },
            WirLocal { name: "c".into(), ty: WirTy::Bool },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
        ],
        body: vec![
            setl("res", E::Call { func: "rc_alloc".into(), args: vec![i32c(8)] }),
            N::If {
                cond: E::Binary {
                    op: BinOp::Or,
                    kind: Kind::I32,
                    lhs: Box::new(E::Binary {
                        op: BinOp::Or,
                        kind: Kind::I32,
                        lhs: Box::new(b64(BinOp::Lt, getl("cp"), i64c(0))),
                        rhs: Box::new(b64(BinOp::Gt, getl("cp"), i64c(0x10FFFF))),
                    }),
                    rhs: Box::new(E::Binary {
                        op: BinOp::And,
                        kind: Kind::I32,
                        lhs: Box::new(b64(BinOp::Ge, getl("cp"), i64c(0xD800))),
                        rhs: Box::new(b64(BinOp::Le, getl("cp"), i64c(0xDFFF))),
                    }),
                },
                then_: vec![
                    store8(0, i32c(0xEF)),
                    store8(1, i32c(0xBF)),
                    store8(2, i32c(0xBD)),
                    setl("n", i32c(3)),
                ],
                els: vec![
                    setl("c", E::Convert { from: Kind::I64, to: Kind::I32, arg: Box::new(getl("cp")) }),
                    N::If {
                        cond: b(BinOp::LtU, getl("c"), i32c(0x80)),
                        then_: vec![
                            store8(0, getl("c")),
                            setl("n", i32c(1)),
                        ],
                        els: vec![N::If {
                            cond: b(BinOp::LtU, getl("c"), i32c(0x800)),
                            then_: vec![
                                store8(0, b(BinOp::Or, i32c(0xC0), b(BinOp::ShrU, getl("c"), i32c(6)))),
                                store8(1, b(BinOp::Or, i32c(0x80), b(BinOp::And, getl("c"), i32c(0x3F)))),
                                setl("n", i32c(2)),
                            ],
                            els: vec![N::If {
                                cond: b(BinOp::LtU, getl("c"), i32c(0x10000)),
                                then_: vec![
                                    store8(0, b(BinOp::Or, i32c(0xE0), b(BinOp::ShrU, getl("c"), i32c(12)))),
                                    store8(1, b(BinOp::Or, i32c(0x80), b(BinOp::And, b(BinOp::ShrU, getl("c"), i32c(6)), i32c(0x3F)))),
                                    store8(2, b(BinOp::Or, i32c(0x80), b(BinOp::And, getl("c"), i32c(0x3F)))),
                                    setl("n", i32c(3)),
                                ],
                                els: vec![
                                    store8(0, b(BinOp::Or, i32c(0xF0), b(BinOp::ShrU, getl("c"), i32c(18)))),
                                    store8(1, b(BinOp::Or, i32c(0x80), b(BinOp::And, b(BinOp::ShrU, getl("c"), i32c(12)), i32c(0x3F)))),
                                    store8(2, b(BinOp::Or, i32c(0x80), b(BinOp::And, b(BinOp::ShrU, getl("c"), i32c(6)), i32c(0x3F)))),
                                    store8(3, b(BinOp::Or, i32c(0x80), b(BinOp::And, getl("c"), i32c(0x3F)))),
                                    setl("n", i32c(4)),
                                ],
                                result: None,
                            }],
                            result: None,
                        }],
                        result: None,
                    },
                ],
                result: None,
            },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$rcopy_str(p: i32) -> i32` — copy a region-local String into parent memory.
pub(in crate::wir_helpers) fn rcopy_str_helper() -> WirFunc {
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
