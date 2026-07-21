//! Encoding and cryptographic host adapters.

use crate::wir::*;

/// `$encoding(op, in) -> i32` — a thin wrapper over the host `encoding` import,
/// which does the actual hex/base64 transform over flat String/Bytes buffers.
/// Reserves a worst-case `2*len + 20` result buffer, lets the host write into
/// `res+4`, and caps the length header to what it returned. The first migrated
/// host-import helper.
pub(super) fn encoding_helper() -> WirFunc {
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
            // (RFC-0016) reserve the worst-case `2*len + 20` buffer through `$rc_alloc`;
            // the host writes into `res+4` and the length header caps to `n` (the block's
            // size header stays the worst case; the tail slack is unused).
            N::SetLocal {
                local: "res".into(),
                value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, b(BinOp::Mul, E::Load { ptr: Box::new(getl("in")), kind: Kind::I32, offset: 0 }, i32c(2)), i32c(20))] },
            },
            N::SetLocal {
                local: "n".into(),
                value: E::CallHost { import: "encoding".into(), args: vec![getl("op"), getl("in"), b(BinOp::Add, getl("res"), i32c(4))] },
            },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
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
pub(super) fn crypto_hash_helper(name: &str, import: &str, hexlen: i32, inputs: &[&str]) -> WirFunc {
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
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(hexlen + 4)] } },
            N::Store { ptr: getl("res"), value: i32c(hexlen), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: import.into(), args: host_args }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// A keyed crypto op on a `Secret` — `crypto.sign(key, msg)` / `crypto.public_key(key)`.
/// `key` is the opaque Secret externref; the host signs / derives the public key
/// with the never-exposed bytes and writes `hexlen` hex chars. (Separate from
/// `crypto_hash_helper`, whose inputs are all strings.)
pub(super) fn crypto_keyed_helper(name: &str, import: &str, hexlen: i32, has_msg: bool) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let mut params = vec![WirLocal { name: "key".into(), ty: WirTy::Extern }];
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
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(hexlen + 4)] } },
            N::Store { ptr: getl("res"), value: i32c(hexlen), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: import.into(), args: host_args }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$crypto_reveal(key) -> i32` — the raw bytes of the Secret externref as a fresh
/// String (lossy UTF-8). Identical staging to [`crate::wir_helpers::dir_read_helper`]:
/// the host `crypto_reveal_len` reads the host-side secret and reports its byte
/// length (staging the bytes), then `fill_pending` copies them into `res+4`.
/// Value secrets are revealed to external sinks; signing keys remain host-side.
pub(super) fn crypto_reveal_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "crypto_reveal".into(),
        params: vec![WirLocal { name: "key".into(), ty: WirTy::Extern }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "crypto_reveal_len".into(), args: vec![getl("key")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}
