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
use witchy_syntax::diag::DiagTemplate;

mod bytes;
pub use bytes::*;
mod collections;
pub use collections::*;
mod dict;
pub use dict::*;
mod memory;
pub use memory::*;
mod numeric;
pub use numeric::*;
mod strings;
pub use strings::*;
mod vm;
pub use vm::*;
mod registry;
pub use registry::{WirHelperSpec, wir_helper};

/// (RFC-0045) Build the node sequence that routes a runtime abort through the
/// always-linked, authority-free `__witchy_abort(template, a, b, str_ptr)` host
/// import, then traps. `a`/`b` are the i64 message holes (index, length — pass
/// `ConstI64(0)` when unused); `str_ptr` is a witchy string pointer (the junk
/// input / `fail` message, or `ConstI32(0)` when the template has no string
/// hole). The host renders the shared [`DiagTemplate`] and bails, so the
/// trailing `Unreachable` only keeps the site stack-typed (matching the
/// contract in `codegen`: the call never returns).
pub fn abort_nodes(template: DiagTemplate, a: WirExpr, b: WirExpr, str_ptr: WirExpr) -> Vec<WirNode> {
    vec![
        WirNode::Do(WirExpr::CallHost {
            import: "__witchy_abort".into(),
            args: vec![WirExpr::ConstI32(template.id()), a, b, str_ptr],
        }),
        WirNode::Unreachable,
    ]
}


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



/// `$encoding(op, in) -> i32` — a thin wrapper over the host `encoding` import,
/// which does the actual hex/base64 transform over flat String/Bytes buffers.
/// Reserves a worst-case `2*len + 20` result buffer, lets the host write into
/// `res+4`, and caps the length header to what it returned. The first migrated
/// host-import helper.
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
fn crypto_keyed_helper(name: &str, import: &str, hexlen: i32, has_msg: bool) -> WirFunc {
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

/// `$dir_read(h, rel) -> i32` — the contents of file `rel` under Dir externref `h`,
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
            WirLocal { name: "h".into(), ty: WirTy::Extern },
            WirLocal { name: "rel".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "dir_read_len".into(), args: vec![getl("h"), getl("rel")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$file_read(f) -> i32` — the contents of file capability `f` as a String
/// (RFC-0012/RFC-0005 Stage 2). A `File` is a leaf (no path), so this takes only
/// the unforgeable externref. Two-phase host
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
        params: vec![WirLocal { name: "f".into(), ty: WirTy::Extern }],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "file_read_len".into(), args: vec![getl("f")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$exec(h, path, args, stdin) -> i32` — spawn the executable `path` under Dir
/// externref `h` (confined like `dir_read`), passing the `\0`-joined argv `args` and
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
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "exec_run".into(), args: vec![getl("h"), getl("path"), getl("args"), getl("stdin")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse); it reserves + bumps `$heap`.
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$crypto_reveal(key) -> i32` — the raw bytes of the Secret externref as a fresh
/// String (lossy UTF-8). Identical staging to [`dir_read_helper`]: the host
/// `crypto_reveal_len` reads the host-side secret and reports its byte length
/// (staging the bytes), then `fill_pending` copies them into `res+4`. For
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

/// `$build_read(rel) -> i32` — the confined build file's contents as a fresh
/// string. Identical staging to [`dir_read_helper`], but the host length import
/// (`build_read_len`) resolves `rel` against the granted build *read roots*, not
/// a Dir handle. The source-level `BuildRead` receiver is zero-representation:
/// typeck requires it and import linking grants it, but no guest handle crosses
/// into the host import.
pub fn build_read_helper() -> WirFunc {
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
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
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
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$dir_list(h) -> i32` — the entries of Dir externref `h`, as a
/// `List(String)`. The host reports the total byte size of the marshaled list
/// (`dir_list_size`), then writes the whole `[count][ptr..]` + payload structure
/// into the reserved block (`write_pending_list`). Needs the Dir(Read) capability.
pub fn dir_list_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    WirFunc {
        name: "dir_list".into(),
        params: vec![WirLocal { name: "h".into(), ty: WirTy::Extern }],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "size".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "size".into(), value: E::CallHost { import: "dir_list_size".into(), args: vec![getl("h")] } },
            // (RFC-0016) allocate through `$rc_alloc` (header + free-list reuse).
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![getl("size")] } },
            N::Do(E::CallHost { import: "write_pending_list".into(), args: vec![getl("res")] }),
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// (RFC-0032) The shared two-phase size-then-write protocol behind every `vm.*` host
/// builtin (`vm.par_map`/`with_dir`/`serve`), mirroring `$dir_list`: `size = run(params…)`
/// (the host computes the result + reports its byte size); `ensure(size)`; `res = heap`;
/// `write(res)` (the host lays the result out at the reserved block); `heap += size`;
/// return `res`. The builtins differ ONLY in their params and the run/write host imports.
fn two_phase_helper(name: &str, params: &[&str], run_import: &str, write_import: &str) -> WirFunc {
    let typed = params.iter().map(|p| ((*p).to_string(), WirTy::Bool)).collect::<Vec<_>>();
    two_phase_helper_typed(name, &typed, run_import, write_import)
}

fn two_phase_helper_typed(
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

/// Like [`exec_helper`], but parameterized for build-only host operations that
/// stage a Witchy `String`: `len = host(args...)`; allocate `[len][bytes]`;
/// `fill_pending(res+4)`.
fn staged_string_helper(name: &str, params: &[&str], run_import: &str) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: name.into(),
        params: params.iter().map(|p| WirLocal { name: (*p).into(), ty: WirTy::Bool }).collect(),
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "len".into(),
                value: E::CallHost { import: run_import.into(), args: params.iter().map(|p| getl(p)).collect() },
            },
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::Push(getl("res")),
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
                    // (RFC-0016) the None wrapper `[tag=1]` via `$rc_alloc`.
                    N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(4)] } },
                    N::Store { ptr: getl("res"), value: i32c(1), kind: Kind::I32, offset: 0 },
                    N::Return(Some(getl("res"))),
                ],
                els: vec![],
                result: None,
            },
            // (RFC-0016) the value string, then the `Some[tag=0][ptr]` wrapper, via `$rc_alloc`.
            N::SetLocal { local: "str".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("str"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "env_fill".into(), args: vec![getl("name"), b(BinOp::Add, getl("str"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(12)] } },
            N::Store { ptr: getl("res"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store { ptr: getl("res"), value: E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("str")) }, kind: Kind::I64, offset: 4 },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$build_get_env(name) -> Option(String)` — build-time environment reads
/// are staged like ordinary `get_env`, but the host enforces the BuildEnv
/// allow-list carried by the sandbox grant. The source receiver is checked and
/// then dropped before the host ABI.
pub fn build_get_env_helper() -> WirFunc {
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
                    N::Store { ptr: getl("res"), value: i32c(1), kind: Kind::I32, offset: 0 },
                    N::Return(Some(getl("res"))),
                ],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "str".into(), value: E::Call { func: "rc_alloc".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] } },
            N::Store { ptr: getl("str"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost {
                import: "build_env_fill".into(),
                args: vec![getl("name"), b(BinOp::Add, getl("str"), i32c(4))],
            }),
            N::SetLocal { local: "res".into(), value: E::Call { func: "rc_alloc".into(), args: vec![i32c(12)] } },
            N::Store { ptr: getl("res"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store {
                ptr: getl("res"),
                value: E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("str")) },
                kind: Kind::I64,
                offset: 4,
            },
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

/// `$build_args() -> i32` — the `Args` list, sized by `args_size` and filled by
/// `write_pending_list`. Mirrors `BUILD_ARGS_WAT`. Calls `$ensure`; uses `$heap`.
pub fn build_args_helper() -> WirFunc {
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

/// Like [`host_call_helper`] but with explicit per-parameter types — for a host import
/// whose params aren't all the default i32 slot (e.g. `net_connect_pinned`'s i64 `port`).
fn host_call_helper_typed(name: &str, import: &str, param_tys: &[WirTy], ret: WirTy) -> WirFunc {
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

/// Like [`host_void_helper`] but with explicit per-parameter types.
fn host_void_helper_typed(name: &str, import: &str, param_tys: &[WirTy]) -> WirFunc {
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
fn net_recv_helper(name: &str, len_import: &str, extra_n: bool) -> WirFunc {
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
