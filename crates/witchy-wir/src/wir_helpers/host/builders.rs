//! Shared constructors for thin host-call WIR adapters.

use crate::wir::*;

/// Build a size-then-write host adapter for the `vm.*` builtins.
pub(in crate::wir_helpers) fn two_phase_helper(name: &str, params: &[&str], run_import: &str, write_import: &str) -> WirFunc {
    let typed = params.iter().map(|p| ((*p).to_string(), WirTy::Bool)).collect::<Vec<_>>();
    two_phase_helper_typed(name, &typed, run_import, write_import)
}

pub(in crate::wir_helpers) fn two_phase_helper_typed(
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

/// Like [`crate::wir_helpers::exec_helper`], but parameterized for build-only host operations that
/// stage a Witchy `String`: `len = host(args...)`; allocate `[len][bytes]`;
/// `fill_pending(res+4)`.
pub(in crate::wir_helpers) fn staged_string_helper(name: &str, params: &[&str], run_import: &str) -> WirFunc {
    let typed = params.iter().map(|p| ((*p).to_string(), WirTy::Bool)).collect::<Vec<_>>();
    staged_string_helper_typed(name, &typed, run_import)
}

pub(in crate::wir_helpers) fn staged_string_helper_typed(
    name: &str,
    params: &[(String, WirTy)],
    run_import: &str,
) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: name.into(),
        params: params.iter().map(|(name, ty)| WirLocal { name: name.clone(), ty: ty.clone() }).collect(),
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "len".into(),
                value: E::CallHost {
                    import: run_import.into(),
                    args: params.iter().map(|(name, _)| getl(name)).collect(),
                },
            },
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}
/// Build a compiler-introspection adapter returning a fresh JSON string.
pub(in crate::wir_helpers) fn compiler_introspect_helper(name: &str, import: &str, nargs: usize) -> WirFunc {
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
pub(in crate::wir_helpers) fn host_call_helper_ret(name: &str, import: &str, nargs: usize, ret: WirTy) -> WirFunc {
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
pub(in crate::wir_helpers) fn host_call_helper_typed(name: &str, import: &str, param_tys: &[WirTy], ret: WirTy) -> WirFunc {
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
pub(in crate::wir_helpers) fn host_void_helper(name: &str, import: &str, nargs: usize) -> WirFunc {
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
pub(in crate::wir_helpers) fn host_void_helper_typed(name: &str, import: &str, param_tys: &[WirTy]) -> WirFunc {
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
pub(in crate::wir_helpers) fn net_recv_helper(name: &str, len_import: &str, extra_n: bool) -> WirFunc {
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
