//! Environment and build-argument host adapters.

use crate::wir::*;

/// `$build_get_env(name) -> Option(String)` — build-time environment reads
/// are staged through the pending buffer, and the host enforces the BuildEnv
/// allow-list carried by the sandbox grant. The source receiver is checked and
/// then dropped before the host ABI.
pub(in crate::wir_helpers) fn build_get_env_helper() -> WirFunc {
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
                    N::SafeStore {
                place: crate::wir_helpers::layouts::byte_buffer_length_place(getl("res"), i32c(1)),
                value: i32c(1),
                kind: Kind::I32,
            },
                    N::Return(Some(getl("res"))),
                ],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "str".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::SafeStore {
                place: crate::wir_helpers::layouts::byte_buffer_length_place(getl("str"), getl("len")),
                value: getl("len"),
                kind: Kind::I32,
            },
            N::Do(E::CallHost {
                import: "build_env_fill".into(),
                args: vec![getl("name"), b(BinOp::Add, getl("str"), i32c(4))],
            }),
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(12)] } },
            N::SafeStore {
                place: crate::wir_helpers::layouts::byte_buffer_length_place(getl("res"), i32c(0)),
                value: i32c(0),
                kind: Kind::I32,
            },
            N::SafeStore {
                // A twelve-byte `[u32 tag][i64 slot]` block, which this helper
                // already treats as byte-buffer-shaped: its tag goes through
                // `byte_buffer_length_place`. The slot is therefore the whole
                // eight-byte payload.
                place: crate::wir_helpers::layouts::byte_buffer_payload_slice_place(
                    getl("res"), i32c(0), i32c(8), i32c(8), crate::safewir::AccessMode::Write,
                ),
                value: E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("str")) },
                kind: Kind::I64,
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}
/// `$build_args() -> i32` — the `Args` list, sized by `args_size` and filled by
/// `write_pending_list`.
pub(in crate::wir_helpers) fn build_args_helper() -> WirFunc {
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
