//! Host-backed string construction and region-copy helpers.

use crate::wir::*;

/// `$string_from_code(cp: i64) -> String` through the Unicode host adapter.
pub(in crate::wir_helpers) fn string_from_code_helper() -> WirFunc {
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
