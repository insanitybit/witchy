//! Console-output host helpers.

use crate::wir::*;

/// `$print_str(s: i32)` — write a Witchy string to the host `print` import.
pub fn print_str_helper() -> WirFunc {
    WirFunc {
        name: "print_str".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![],
        locals: vec![],
        body: vec![WirNode::Do(WirExpr::CallHost {
            import: "print".into(),
            args: vec![
                WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::GetLocal("s".into())),
                    rhs: Box::new(WirExpr::ConstI32(4)),
                },
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
