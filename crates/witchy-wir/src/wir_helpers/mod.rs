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
mod encoding;
pub use encoding::*;
mod host;
pub use host::*;
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
