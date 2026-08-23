fn is_self_recursive(func: &WirFunc) -> bool {
    node_contains_call_to_func(&func.body, &func.name)
}
fn node_contains_call_to_func(seq: &WirSeq, target: &str) -> bool {
    seq.iter().any(|n| match n {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => node_contains_call_to_func(body, target),
        WirNode::If { cond, then_, els, .. } => expr_contains_call_to_func(cond, target) || node_contains_call_to_func(then_, target) || node_contains_call_to_func(els, target),
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } | WirNode::Drop(value) | WirNode::Do(value) |
        WirNode::Push(value) | WirNode::Return(Some(value)) | WirNode::Br { cond: Some(value), .. } => expr_contains_call_to_func(value, target),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } | WirNode::StructSet { base: ptr, value, .. } => expr_contains_call_to_func(ptr, target) || expr_contains_call_to_func(value, target),
        WirNode::MemoryCopy { dest, src, len } | WirNode::MemoryFill { dest, value: src, len } | WirNode::ArraySet { array: dest, index: src, value: len, .. } => expr_contains_call_to_func(dest, target) || expr_contains_call_to_func(src, target) || expr_contains_call_to_func(len, target),
        WirNode::CallStoreMulti { args, .. } => args.iter().any(|a| expr_contains_call_to_func(a, target)),
        WirNode::CallIndirectStoreMulti { args, index, .. } => args.iter().any(|a| expr_contains_call_to_func(a, target)) || expr_contains_call_to_func(index, target),
        _ => false,
    })
}
fn expr_contains_call_to_func(expr: &WirExpr, target: &str) -> bool {
    match expr {
        WirExpr::Call { func, .. } if func == target => true,
        WirExpr::Binary { lhs, rhs, .. } => expr_contains_call_to_func(lhs, target) || expr_contains_call_to_func(rhs, target),
        WirExpr::Unary { arg, .. } | WirExpr::ToSlot(arg, _) | WirExpr::FromSlot(arg, _) | WirExpr::Convert { arg, .. } |
        WirExpr::Load { ptr: arg, .. } | WirExpr::Load8U { ptr: arg, .. } | WirExpr::MemoryGrow(arg) |
        WirExpr::StructGet { base: arg, .. } | WirExpr::RefCast { value: arg, .. } | WirExpr::RefCastNullable { value: arg, .. } |
        WirExpr::ArrayLen(arg) | WirExpr::RefIsNull(arg) => expr_contains_call_to_func(arg, target),
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } | WirExpr::StructNew { args, .. } | WirExpr::ArrayNewFixed { items: args, .. } | WirExpr::Vector { args, .. } => args.iter().any(|a| expr_contains_call_to_func(a, target)),
        WirExpr::ArrayNew { value, len, .. } | WirExpr::ArrayGet { array: value, index: len, .. } => expr_contains_call_to_func(value, target) || expr_contains_call_to_func(len, target),
        WirExpr::CallIndirect { args, index, .. } => args.iter().any(|a| expr_contains_call_to_func(a, target)) || expr_contains_call_to_func(index, target),
        WirExpr::Control(node) => node_contains_call_to_func(&vec![*node.clone()], target), // simplification
        WirExpr::Seq(seq) => node_contains_call_to_func(seq, target),
        _ => false,
    }
}
