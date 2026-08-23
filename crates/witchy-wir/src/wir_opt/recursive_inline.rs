use crate::wir::{Kind, WirExpr, WirFunc, WirLocal, WirModule, WirNode, WirSeq, WirTy};
use std::collections::HashMap;

pub fn inline_recursive_calls(module: &mut WirModule) {
    let mut clones = HashMap::new();
    for func in &module.funcs {
        if func.raw_body.is_some() { continue; }
        if !is_scalar_only(func) { continue; }
        if !is_self_recursive(func) { continue; }
        if has_forbidden_nodes(&func.body) { continue; }
        let size = seq_size(&func.body);
        if size > 30 { continue; }
        clones.insert(func.name.clone(), func.clone());
    }

    for func in &mut module.funcs {
        if let Some(target) = clones.get(&func.name) {
            let mut changed = false;
            let mut new_locals = Vec::new();
            inline_seq(&mut func.body, target, &func.name, &func.params, &func.locals, &mut new_locals, &mut changed);
            if changed {
                func.locals.extend(new_locals);
            }
        }
    }
}

fn is_scalar_only(func: &WirFunc) -> bool {
    func.params.iter().all(|p| is_scalar(&p.ty)) && func.ret.iter().all(|r| is_scalar(r))
}
fn is_scalar(ty: &WirTy) -> bool { matches!(ty, WirTy::Int | WirTy::Float | WirTy::Bool | WirTy::V128 | WirTy::Slot) }
fn has_forbidden_nodes(seq: &WirSeq) -> bool { seq.iter().any(node_has_forbidden) }
fn node_has_forbidden(node: &WirNode) -> bool {
    match node {
        WirNode::Loop { .. } | WirNode::CallStoreMulti { .. } | WirNode::CallIndirectStoreMulti { .. } |
        WirNode::StructSet { .. } | WirNode::ArraySet { .. } => true,
        WirNode::Source { body, .. } | WirNode::Block { body, .. } => has_forbidden_nodes(body),
        WirNode::If { cond, then_, els, .. } => expr_has_forbidden(cond) || has_forbidden_nodes(then_) || has_forbidden_nodes(els),
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => expr_has_forbidden(value),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => expr_has_forbidden(ptr) || expr_has_forbidden(value),
        WirNode::MemoryCopy { dest, src, len } => expr_has_forbidden(dest) || expr_has_forbidden(src) || expr_has_forbidden(len),
        WirNode::MemoryFill { dest, value, len } => expr_has_forbidden(dest) || expr_has_forbidden(value) || expr_has_forbidden(len),
        WirNode::Drop(e) | WirNode::Do(e) | WirNode::Push(e) | WirNode::Return(Some(e)) | WirNode::Br { cond: Some(e), .. } => expr_has_forbidden(e),
        _ => false,
    }
}
fn expr_has_forbidden(expr: &WirExpr) -> bool {
    match expr {
        WirExpr::CallHost { .. } | WirExpr::CallIndirect { .. } | WirExpr::StructNew { .. } |
        WirExpr::ArrayNewFixed { .. } | WirExpr::ArrayNew { .. } | WirExpr::RefCast { .. } |
        WirExpr::RefCastNullable { .. } | WirExpr::RefIsNull { .. } | WirExpr::RefNull(_) => true,
        WirExpr::Control(node) => node_has_forbidden(node),
        WirExpr::Seq(seq) => has_forbidden_nodes(seq),
        WirExpr::Unary { arg, .. } | WirExpr::ToSlot(arg, _) | WirExpr::FromSlot(arg, _) |
        WirExpr::Convert { arg, .. } | WirExpr::Load { ptr: arg, .. } | WirExpr::Load8U { ptr: arg, .. } |
        WirExpr::MemoryGrow(arg) | WirExpr::StructGet { base: arg, .. } | WirExpr::ArrayLen(arg) => expr_has_forbidden(arg),
        WirExpr::Binary { lhs, rhs, .. } => expr_has_forbidden(lhs) || expr_has_forbidden(rhs),
        WirExpr::Call { args, .. } | WirExpr::Vector { args, .. } => args.iter().any(expr_has_forbidden),
        WirExpr::ArrayGet { array, index, .. } => expr_has_forbidden(array) || expr_has_forbidden(index),
        _ => false,
    }
}
fn seq_size(seq: &WirSeq) -> usize { seq.iter().map(node_size).sum() }
fn node_size(node: &WirNode) -> usize {
    1 + match node {
        WirNode::Source { body, .. } => seq_size(body),
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => expr_size(value),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => expr_size(ptr) + expr_size(value),
        WirNode::CallStoreMulti { args, .. } => args.iter().map(expr_size).sum(),
        WirNode::CallIndirectStoreMulti { args, index, .. } => args.iter().map(expr_size).sum::<usize>() + expr_size(index),
        WirNode::MemoryCopy { dest, src, len } => expr_size(dest) + expr_size(src) + expr_size(len),
        WirNode::MemoryFill { dest, value, len } => expr_size(dest) + expr_size(value) + expr_size(len),
        WirNode::If { cond, then_, els, .. } => expr_size(cond) + seq_size(then_) + seq_size(els),
        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => seq_size(body),
        WirNode::StructSet { base, value, .. } => expr_size(base) + expr_size(value),
        WirNode::ArraySet { array, index, value, .. } => expr_size(array) + expr_size(index) + expr_size(value),
        WirNode::Br { cond: Some(c), .. } => expr_size(c),
        WirNode::Drop(e) | WirNode::Do(e) | WirNode::Push(e) | WirNode::Return(Some(e)) => expr_size(e),
        _ => 0,
    }
}
fn expr_size(expr: &WirExpr) -> usize {
    1 + match expr {
        WirExpr::ToSlot(inner, _) | WirExpr::FromSlot(inner, _) | WirExpr::Unary { arg: inner, .. } |
        WirExpr::Convert { arg: inner, .. } | WirExpr::Load { ptr: inner, .. } | WirExpr::Load8U { ptr: inner, .. } => expr_size(inner),
        WirExpr::Binary { lhs, rhs, .. } => expr_size(lhs) + expr_size(rhs),
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => args.iter().map(expr_size).sum(),
        WirExpr::CallIndirect { args, index, .. } => args.iter().map(expr_size).sum::<usize>() + expr_size(index),
        WirExpr::MemoryGrow(pages) => expr_size(pages),
        WirExpr::Control(node) => node_size(node),
        WirExpr::Seq(nodes) => seq_size(nodes),
        WirExpr::StructNew { args, .. } => args.iter().map(expr_size).sum(),
        WirExpr::ArrayNewFixed { items, .. } => items.iter().map(expr_size).sum(),
        WirExpr::ArrayNew { value, len, .. } => expr_size(value) + expr_size(len),
        WirExpr::ArrayGet { array, index, .. } => expr_size(array) + expr_size(index),
        WirExpr::StructGet { base, .. } | WirExpr::RefCast { value: base, .. } | WirExpr::RefCastNullable { value: base, .. } |
        WirExpr::ArrayLen(base) | WirExpr::RefIsNull(base) => expr_size(base),
        WirExpr::Vector { args, .. } => args.iter().map(expr_size).sum(),
        _ => 0,
    }
}

fn inline_seq(seq: &mut WirSeq, target: &WirFunc, parent_name: &str, parent_params: &[WirLocal], parent_locals: &[WirLocal], new_locals: &mut Vec<WirLocal>, changed: &mut bool) {
    for node in seq { inline_node(node, target, parent_name, parent_params, parent_locals, new_locals, changed); }
}

fn inline_node(node: &mut WirNode, target: &WirFunc, parent_name: &str, parent_params: &[WirLocal], parent_locals: &[WirLocal], new_locals: &mut Vec<WirLocal>, changed: &mut bool) {
    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => inline_seq(body, target, parent_name, parent_params, parent_locals, new_locals, changed),
        WirNode::If { cond, then_, els, .. } => {
            inline_expr(cond, target, parent_name, parent_params, parent_locals, new_locals, changed);
            inline_seq(then_, target, parent_name, parent_params, parent_locals, new_locals, changed);
            inline_seq(els, target, parent_name, parent_params, parent_locals, new_locals, changed);
        }
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } | WirNode::Drop(value) | WirNode::Do(value) |
        WirNode::Push(value) | WirNode::Return(Some(value)) | WirNode::Br { cond: Some(value), .. } => inline_expr(value, target, parent_name, parent_params, parent_locals, new_locals, changed),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } | WirNode::StructSet { base: ptr, value, .. } => {
            inline_expr(ptr, target, parent_name, parent_params, parent_locals, new_locals, changed);
            inline_expr(value, target, parent_name, parent_params, parent_locals, new_locals, changed);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            inline_expr(dest, target, parent_name, parent_params, parent_locals, new_locals, changed);
            inline_expr(src, target, parent_name, parent_params, parent_locals, new_locals, changed);
            inline_expr(len, target, parent_name, parent_params, parent_locals, new_locals, changed);
        }
        WirNode::MemoryFill { dest, value, len } | WirNode::ArraySet { array: dest, index: value, value: len, .. } => {
            inline_expr(dest, target, parent_name, parent_params, parent_locals, new_locals, changed);
            inline_expr(value, target, parent_name, parent_params, parent_locals, new_locals, changed);
            inline_expr(len, target, parent_name, parent_params, parent_locals, new_locals, changed);
        }
        WirNode::CallStoreMulti { args, .. } => { for a in args { inline_expr(a, target, parent_name, parent_params, parent_locals, new_locals, changed); } }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for a in args { inline_expr(a, target, parent_name, parent_params, parent_locals, new_locals, changed); }
            inline_expr(index, target, parent_name, parent_params, parent_locals, new_locals, changed);
        }
        _ => {}
    }
}

fn replace_get_locals_expr(expr: &mut WirExpr, args_map: &HashMap<String, WirExpr>) {
    match expr {
        WirExpr::GetLocal(name) => {
            if let Some(arg) = args_map.get(name) {
                *expr = arg.clone();
            }
        }
        WirExpr::ToSlot(inner, _) | WirExpr::FromSlot(inner, _) | WirExpr::Unary { arg: inner, .. } |
        WirExpr::Convert { arg: inner, .. } | WirExpr::Load { ptr: inner, .. } | WirExpr::Load8U { ptr: inner, .. } |
        WirExpr::MemoryGrow(inner) | WirExpr::StructGet { base: inner, .. } | WirExpr::RefCast { value: inner, .. } |
        WirExpr::RefCastNullable { value: inner, .. } | WirExpr::ArrayLen(inner) | WirExpr::RefIsNull(inner) => {
            replace_get_locals_expr(inner, args_map);
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            replace_get_locals_expr(lhs, args_map);
            replace_get_locals_expr(rhs, args_map);
        }
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } | WirExpr::StructNew { args, .. } |
        WirExpr::ArrayNewFixed { items: args, .. } | WirExpr::Vector { args, .. } => {
            for a in args { replace_get_locals_expr(a, args_map); }
        }
        WirExpr::ArrayNew { value, len, .. } | WirExpr::ArrayGet { array: value, index: len, .. } => {
            replace_get_locals_expr(value, args_map);
            replace_get_locals_expr(len, args_map);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args { replace_get_locals_expr(a, args_map); }
            replace_get_locals_expr(index, args_map);
        }
        WirExpr::Control(node) => replace_get_locals_node(node, args_map),
        WirExpr::Seq(seq) => replace_get_locals_seq(seq, args_map),
        _ => {}
    }
}

fn replace_get_locals_node(node: &mut WirNode, args_map: &HashMap<String, WirExpr>) {
    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => replace_get_locals_seq(body, args_map),
        WirNode::If { cond, then_, els, .. } => {
            replace_get_locals_expr(cond, args_map);
            replace_get_locals_seq(then_, args_map);
            replace_get_locals_seq(els, args_map);
        }
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } | WirNode::Drop(value) | WirNode::Do(value) |
        WirNode::Push(value) | WirNode::Return(Some(value)) | WirNode::Br { cond: Some(value), .. } => replace_get_locals_expr(value, args_map),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } | WirNode::StructSet { base: ptr, value, .. } => {
            replace_get_locals_expr(ptr, args_map);
            replace_get_locals_expr(value, args_map);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            replace_get_locals_expr(dest, args_map);
            replace_get_locals_expr(src, args_map);
            replace_get_locals_expr(len, args_map);
        }
        WirNode::MemoryFill { dest, value, len } | WirNode::ArraySet { array: dest, index: value, value: len, .. } => {
            replace_get_locals_expr(dest, args_map);
            replace_get_locals_expr(value, args_map);
            replace_get_locals_expr(len, args_map);
        }
        WirNode::CallStoreMulti { args, .. } => { for a in args { replace_get_locals_expr(a, args_map); } }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for a in args { replace_get_locals_expr(a, args_map); }
            replace_get_locals_expr(index, args_map);
        }
        _ => {}
    }
}

fn replace_get_locals_seq(seq: &mut WirSeq, args_map: &HashMap<String, WirExpr>) {
    for node in seq { replace_get_locals_node(node, args_map); }
}

fn extract_single_expr(seq: &[WirNode]) -> Option<&WirExpr> {
    if seq.len() != 1 { return None; }
    match &seq[0] {
        WirNode::Push(expr) | WirNode::Return(Some(expr)) => Some(expr),
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => extract_single_expr(body),
        _ => None,
    }
}

fn contains_call_to(expr: &WirExpr, target: &str) -> bool {
    match expr {
        WirExpr::Call { func, .. } if func == target => true,
        WirExpr::Binary { lhs, rhs, .. } => contains_call_to(lhs, target) || contains_call_to(rhs, target),
        WirExpr::Unary { arg, .. } | WirExpr::ToSlot(arg, _) | WirExpr::FromSlot(arg, _) | WirExpr::Convert { arg, .. } => contains_call_to(arg, target),
        WirExpr::Call { args, .. } => args.iter().any(|a| contains_call_to(a, target)),
        WirExpr::Control(node) => node_contains_call_to(node, target),
        WirExpr::Seq(seq) => seq.iter().any(|n| node_contains_call_to(n, target)),
        _ => false,
    }
}

fn node_contains_call_to(node: &WirNode, target: &str) -> bool {
    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => body.iter().any(|n| node_contains_call_to(n, target)),
        WirNode::If { cond, then_, els, .. } => contains_call_to(cond, target) || then_.iter().any(|n| node_contains_call_to(n, target)) || els.iter().any(|n| node_contains_call_to(n, target)),
        WirNode::Push(e) | WirNode::Return(Some(e)) | WirNode::SetLocal { value: e, .. } | WirNode::SetGlobal { value: e, .. } => contains_call_to(e, target),
        _ => false,
    }
}

fn inline_expr(expr: &mut WirExpr, target: &WirFunc, parent_name: &str, parent_params: &[WirLocal], parent_locals: &[WirLocal], new_locals: &mut Vec<WirLocal>, changed: &mut bool) {
    if false && let WirExpr::Binary { lhs, rhs, kind, op } = expr {
        if contains_call_to(lhs, &target.name) || contains_call_to(rhs, &target.name) {
            let left_name = format!("lhs_{}", new_locals.len());
            let right_name = format!("rhs_{}", new_locals.len());
            
            new_locals.push(WirLocal { name: left_name.clone(), ty: WirTy::Int }); // Assuming Int for simplification, or we can get it from context. Wait!
            new_locals.push(WirLocal { name: right_name.clone(), ty: WirTy::Int });
            
            let t = match kind { Kind::F64 => WirTy::Float, _ => WirTy::Int };
            new_locals.last_mut().unwrap().ty = t.clone();
            let mut lhs_box = Box::new(WirExpr::ConstI64(0));
            let mut rhs_box = Box::new(WirExpr::ConstI64(0));
            std::mem::swap(lhs, &mut lhs_box);
            std::mem::swap(rhs, &mut rhs_box);
            
            let mut seq = vec![
                WirNode::SetLocal { local: left_name.clone(), value: *lhs_box },
                WirNode::SetLocal { local: right_name.clone(), value: *rhs_box },
                WirNode::Push(WirExpr::Binary {
                    op: *op,
                    kind: *kind,
                    lhs: Box::new(WirExpr::GetLocal(left_name)),
                    rhs: Box::new(WirExpr::GetLocal(right_name)),
                }),
            ];
            inline_seq(&mut seq, target, parent_name, parent_params, parent_locals, new_locals, changed);
            *expr = WirExpr::Seq(seq);
            return;
        }
    }

    match expr {
        WirExpr::ToSlot(inner, _) | WirExpr::FromSlot(inner, _) | WirExpr::Unary { arg: inner, .. } |
        WirExpr::Convert { arg: inner, .. } | WirExpr::Load { ptr: inner, .. } | WirExpr::Load8U { ptr: inner, .. } |
        WirExpr::MemoryGrow(inner) | WirExpr::StructGet { base: inner, .. } | WirExpr::RefCast { value: inner, .. } |
        WirExpr::RefCastNullable { value: inner, .. } | WirExpr::ArrayLen(inner) | WirExpr::RefIsNull(inner) => {
            inline_expr(inner, target, parent_name, parent_params, parent_locals, new_locals, changed);
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            inline_expr(lhs, target, parent_name, parent_params, parent_locals, new_locals, changed);
            inline_expr(rhs, target, parent_name, parent_params, parent_locals, new_locals, changed);
        }
        WirExpr::CallHost { args, .. } | WirExpr::StructNew { args, .. } | WirExpr::ArrayNewFixed { items: args, .. } |
        WirExpr::Vector { args, .. } => {
            for a in args { inline_expr(a, target, parent_name, parent_params, parent_locals, new_locals, changed); }
        }
        WirExpr::ArrayNew { value, len, .. } | WirExpr::ArrayGet { array: value, index: len, .. } => {
            inline_expr(value, target, parent_name, parent_params, parent_locals, new_locals, changed);
            inline_expr(len, target, parent_name, parent_params, parent_locals, new_locals, changed);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args { inline_expr(a, target, parent_name, parent_params, parent_locals, new_locals, changed); }
            inline_expr(index, target, parent_name, parent_params, parent_locals, new_locals, changed);
        }
        WirExpr::Control(node) => inline_node(node, target, parent_name, parent_params, parent_locals, new_locals, changed),
        WirExpr::Seq(seq) => inline_seq(seq, target, parent_name, parent_params, parent_locals, new_locals, changed),
        WirExpr::Call { func, args } => {
            for a in args.iter_mut() {
                inline_expr(a, target, parent_name, parent_params, parent_locals, new_locals, changed);
            }
            if func == &target.name {
                *changed = true;
                
                if target.locals.is_empty() {
                    if let Some(inner_expr) = extract_single_expr(&target.body) {
                        let mut cloned_expr = inner_expr.clone();
                        let mut args_map = HashMap::new();
                        let mut temp_args = Vec::new();
                        std::mem::swap(&mut temp_args, args);
                        for (param, arg) in target.params.iter().zip(temp_args.into_iter()) {
                            args_map.insert(param.name.clone(), arg);
                        }
                        replace_get_locals_expr(&mut cloned_expr, &args_map);
                        *expr = cloned_expr;
                        return;
                    }
                }
            }
        }
        _ => {}
    }
}
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
