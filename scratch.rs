fn extract_single_expr(seq: &[WirNode]) -> Option<&WirExpr> {
    if seq.len() != 1 { return None; }
    match &seq[0] {
        WirNode::Push(expr) | WirNode::Return(Some(expr)) => Some(expr),
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => extract_single_expr(body),
        _ => None,
    }
}
