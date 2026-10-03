//! Filesystem, process, build-input, and regex host adapters.

use crate::wir::*;

/// `$exec(exec, h, path, args, stdin) -> i32` — spawn the executable `path`
/// through the attenuable Exec authority and under Dir externref `h` (confined
/// like `dir.read`), passing the `\0`-joined argv `args` and `stdin`, returning
/// the payload string `"<exit_code>\n<stdout><stderr>"`.
/// Two-phase pending-buffer protocol: `exec_run` runs the
/// process and reports the staged payload's byte length, then `fill_pending`
/// copies it into `res+4`. Needs the `Exec` capability.
pub(in crate::wir_helpers) fn exec_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "exec".into(),
        params: vec![
            WirLocal { name: "exec".into(), ty: WirTy::Extern },
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
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "exec_run".into(), args: vec![getl("exec"), getl("h"), getl("path"), getl("args"), getl("stdin")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::SafeStore {
                place: crate::wir_helpers::layouts::byte_buffer_length_place(getl("res"), getl("len")),
                value: getl("len"),
                kind: Kind::I32,
            },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$build_read(rel) -> String` through the confined build-input host adapter.
pub(in crate::wir_helpers) fn build_read_helper() -> WirFunc {
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
            N::SafeStore {
                place: crate::wir_helpers::layouts::byte_buffer_length_place(getl("res"), getl("len")),
                value: getl("len"),
                kind: Kind::I32,
            },
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
pub(in crate::wir_helpers) fn regex_match_spans_helper() -> WirFunc {
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
            N::SafeStore {
                place: crate::wir_helpers::layouts::byte_buffer_length_place(getl("res"), getl("len")),
                value: getl("len"),
                kind: Kind::I32,
            },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}
