//! Runtime diagnostic helpers.

use crate::wir::*;
use witchy_syntax::diag::DiagTemplate;

/// Build the node sequence that routes a runtime abort through the
/// authority-free `__witchy_abort(template, a, b, str_ptr)` host import, then
/// traps. The trailing `Unreachable` keeps the site stack-typed because the
/// host call never returns.
pub fn abort_nodes(template: DiagTemplate, a: WirExpr, b: WirExpr, str_ptr: WirExpr) -> Vec<WirNode> {
    vec![
        WirNode::Do(WirExpr::CallHost {
            import: "__witchy_abort".into(),
            args: vec![WirExpr::ConstI32(template.id()), a, b, str_ptr],
        }),
        WirNode::Unreachable,
    ]
}
