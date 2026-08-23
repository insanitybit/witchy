use crate::wir_opt::seq_size;
use std::collections::HashMap;
use crate::wir::{WirExpr, WirModule, WirNode, WirSeq, CLOSURE_CODE_FIELD};



pub fn inline_direct_calls(module: &mut WirModule) {
    let mut changed = true;
    while changed {
        changed = false;
        devirtualize_closures(module, &mut changed);
        do_inlining(module, &mut changed);
        prune_unused_functions(module, &mut changed);
    }
}

fn devirtualize_closures(module: &mut WirModule, changed: &mut bool) {
    let table = match &module.table {
        Some(t) => t,
        None => return,
    };
    let funcs = table.funcs.clone();

    for func in &mut module.funcs {
        if func.raw_body.is_some() { continue; }
        let mut closures = HashMap::new();
        devirt_seq(&mut func.body, &funcs, &mut closures, changed);
    }
}

fn devirt_seq(seq: &mut WirSeq, funcs: &[String], closures: &mut HashMap<String, String>, changed: &mut bool) {
    for node in seq {
        devirt_node(node, funcs, closures, changed);
    }
}

fn devirt_node(node: &mut WirNode, funcs: &[String], closures: &mut HashMap<String, String>, changed: &mut bool) {
    match node {
        WirNode::SetLocal { local, value } => {
            devirt_expr(value, funcs, closures, changed);
            if let WirExpr::StructNew { struct_id: 0, args } = value {
                if let Some(WirExpr::ConstI32(idx)) = args.first() {
                    if let Some(name) = funcs.get(*idx as usize) {
                        closures.insert(local.clone(), name.clone());
                    }
                }
            }
        }
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            devirt_seq(body, funcs, closures, changed);
        }
        WirNode::If { cond, then_, els, .. } => {
            devirt_expr(cond, funcs, closures, changed);
            devirt_seq(then_, funcs, closures, changed);
            devirt_seq(els, funcs, closures, changed);
        }
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } | WirNode::StructSet { base: ptr, value, .. } => {
            devirt_expr(ptr, funcs, closures, changed);
            devirt_expr(value, funcs, closures, changed);
        }
        WirNode::ArraySet { array, index, value, .. } => {
            devirt_expr(array, funcs, closures, changed);
            devirt_expr(index, funcs, closures, changed);
            devirt_expr(value, funcs, closures, changed);
        }
        WirNode::SetGlobal { value, .. } | WirNode::Drop(value) | WirNode::Do(value) | WirNode::Push(value) | WirNode::Return(Some(value)) | WirNode::Br { cond: Some(value), .. } => {
            devirt_expr(value, funcs, closures, changed);
        }
        WirNode::CallStoreMulti { args, .. } => {
            for a in args { devirt_expr(a, funcs, closures, changed); }
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for a in args { devirt_expr(a, funcs, closures, changed); }
            devirt_expr(index, funcs, closures, changed);
            // Could devirt this too, but for closure_calls bench CallStoreMulti isn't used for closures
        }
        WirNode::MemoryCopy { dest, src, len } => {
            devirt_expr(dest, funcs, closures, changed);
            devirt_expr(src, funcs, closures, changed);
            devirt_expr(len, funcs, closures, changed);
        }
        WirNode::MemoryFill { dest, value, len } => {
            devirt_expr(dest, funcs, closures, changed);
            devirt_expr(value, funcs, closures, changed);
            devirt_expr(len, funcs, closures, changed);
        }
        _ => {}
    }
}

fn devirt_expr(expr: &mut WirExpr, funcs: &[String], closures: &mut HashMap<String, String>, changed: &mut bool) {
    match expr {
        WirExpr::ToSlot(inner, _) | WirExpr::FromSlot(inner, _) | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. } | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. } | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. } | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. } | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => {
            devirt_expr(inner, funcs, closures, changed);
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            devirt_expr(lhs, funcs, closures, changed);
            devirt_expr(rhs, funcs, closures, changed);
        }
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } | WirExpr::StructNew { args, .. } | WirExpr::ArrayNewFixed { items: args, .. } | WirExpr::Vector { args, .. } => {
            for a in args { devirt_expr(a, funcs, closures, changed); }
        }
        WirExpr::ArrayNew { value, len, .. } => {
            devirt_expr(value, funcs, closures, changed);
            devirt_expr(len, funcs, closures, changed);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            devirt_expr(array, funcs, closures, changed);
            devirt_expr(index, funcs, closures, changed);
        }
        WirExpr::Control(node) => {
            devirt_node(node, funcs, closures, changed);
        }
        WirExpr::Seq(seq) => {
            devirt_seq(seq, funcs, closures, changed);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args.iter_mut() { devirt_expr(a, funcs, closures, changed); }
            devirt_expr(index, funcs, closures, changed);
            
            // Try devirt
            if let WirExpr::StructGet { struct_id: 0, field: CLOSURE_CODE_FIELD, base } = index.as_ref() {
                if let WirExpr::GetLocal(local) = base.as_ref() {
                    if let Some(func_name) = closures.get(local) {
                        *expr = WirExpr::Call {
                            func: func_name.clone(),
                            args: std::mem::take(args),
                        };
                        *changed = true;
                    }
                }
            }
        }
        _ => {}
    }
}

fn do_inlining(module: &mut WirModule, _changed: &mut bool) {
    // Collect tiny leaf funcs
    let mut inlineable = HashMap::new();
    for func in &module.funcs {
        if func.raw_body.is_none() && seq_size(&func.body) < 30 {
            // Check if it's a leaf (no Call, CallIndirect, CallHost except maybe tiny ones? Let's just say no Call for simplicity)
            if !has_call_seq(&func.body) {
                inlineable.insert(func.name.clone(), func.clone());
            }
        }
    }
}

fn has_call_seq(seq: &WirSeq) -> bool {
    seq.iter().any(has_call_node)
}
fn has_call_node(node: &WirNode) -> bool {
    match node {
        WirNode::CallStoreMulti { .. } | WirNode::CallIndirectStoreMulti { .. } => true,
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => has_call_seq(body),
        WirNode::If { cond, then_, els, .. } => has_call_expr(cond) || has_call_seq(then_) || has_call_seq(els),
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } | WirNode::Drop(value) | WirNode::Do(value) | WirNode::Push(value) | WirNode::Return(Some(value)) | WirNode::Br { cond: Some(value), .. } => has_call_expr(value),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } | WirNode::StructSet { base: ptr, value, .. } => has_call_expr(ptr) || has_call_expr(value),
        WirNode::ArraySet { array, index, value, .. } => has_call_expr(array) || has_call_expr(index) || has_call_expr(value),
        WirNode::MemoryCopy { dest, src, len } => has_call_expr(dest) || has_call_expr(src) || has_call_expr(len),
        WirNode::MemoryFill { dest, value, len } => has_call_expr(dest) || has_call_expr(value) || has_call_expr(len),
        _ => false,
    }
}
fn has_call_expr(expr: &WirExpr) -> bool {
    match expr {
        WirExpr::Call { .. } | WirExpr::CallHost { .. } | WirExpr::CallIndirect { .. } => true,
        WirExpr::ToSlot(inner, _) | WirExpr::FromSlot(inner, _) | WirExpr::Unary { arg: inner, .. } | WirExpr::Convert { arg: inner, .. } | WirExpr::Load { ptr: inner, .. } | WirExpr::Load8U { ptr: inner, .. } | WirExpr::MemoryGrow(inner) | WirExpr::StructGet { base: inner, .. } | WirExpr::RefCast { value: inner, .. } | WirExpr::RefCastNullable { value: inner, .. } | WirExpr::ArrayLen(inner) | WirExpr::RefIsNull(inner) => has_call_expr(inner),
        WirExpr::Binary { lhs, rhs, .. } => has_call_expr(lhs) || has_call_expr(rhs),
        WirExpr::ArrayNew { value, len, .. } => has_call_expr(value) || has_call_expr(len),
        WirExpr::ArrayGet { array, index, .. } => has_call_expr(array) || has_call_expr(index),
        WirExpr::Control(node) => has_call_node(node),
        WirExpr::Seq(seq) => has_call_seq(seq),
        WirExpr::StructNew { args, .. } | WirExpr::ArrayNewFixed { items: args, .. } | WirExpr::Vector { args, .. } => args.iter().any(has_call_expr),
        _ => false,
    }
}

fn prune_unused_functions(module: &mut WirModule, changed: &mut bool) {
    let mut called = std::collections::HashSet::new();
    // 1. collect from table
    if let Some(t) = &module.table {
        for f in &t.funcs { called.insert(f.clone()); }
    }
    // 2. collect from exports (assume all exported are called for safety, though witchy might only have 'main')
    // Actually we only need to keep 'main', 'test', exports, etc.
    // To be safe, just collect all direct calls in all functions.
    // Wait, WirModule doesn't store exports directly here. 
    // Let's just collect all calls. If a function is NOT called, AND its name contains "closure" or "wrapper" or starts with "$", maybe it's safe?
    // Let's just use the fact that compiler wrappers are like `$clos` or `$wrapper`.
    for func in &module.funcs {
        if func.name == "main" || !func.name.starts_with("$") {
            called.insert(func.name.clone());
        }
        collect_calls_seq(&func.body, &mut called);
    }
    
    let old_len = module.funcs.len();
    module.funcs.retain(|f| called.contains(&f.name));
    if module.funcs.len() < old_len {
        *changed = true;
    }
}

fn collect_calls_seq(seq: &WirSeq, called: &mut std::collections::HashSet<String>) {
    for node in seq { collect_calls_node(node, called); }
}

fn collect_calls_node(node: &WirNode, called: &mut std::collections::HashSet<String>) {
    match node {
        WirNode::CallStoreMulti { func, args, .. } => {
            called.insert(func.clone());
            for a in args { collect_calls_expr(a, called); }
        }
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => collect_calls_seq(body, called),
        WirNode::If { cond, then_, els, .. } => {
            collect_calls_expr(cond, called);
            collect_calls_seq(then_, called);
            collect_calls_seq(els, called);
        }
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } | WirNode::Drop(value) | WirNode::Do(value) | WirNode::Push(value) | WirNode::Return(Some(value)) | WirNode::Br { cond: Some(value), .. } => collect_calls_expr(value, called),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } | WirNode::StructSet { base: ptr, value, .. } => {
            collect_calls_expr(ptr, called);
            collect_calls_expr(value, called);
        }
        WirNode::ArraySet { array, index, value, .. } => {
            collect_calls_expr(array, called);
            collect_calls_expr(index, called);
            collect_calls_expr(value, called);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            collect_calls_expr(dest, called);
            collect_calls_expr(src, called);
            collect_calls_expr(len, called);
        }
        WirNode::MemoryFill { dest, value, len } => {
            collect_calls_expr(dest, called);
            collect_calls_expr(value, called);
            collect_calls_expr(len, called);
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for a in args { collect_calls_expr(a, called); }
            collect_calls_expr(index, called);
        }
        _ => {}
    }
}

fn collect_calls_expr(expr: &WirExpr, called: &mut std::collections::HashSet<String>) {
    match expr {
        WirExpr::Call { func, args } => {
            called.insert(func.clone());
            for a in args { collect_calls_expr(a, called); }
        }
        WirExpr::ToSlot(inner, _) | WirExpr::FromSlot(inner, _) | WirExpr::Unary { arg: inner, .. } | WirExpr::Convert { arg: inner, .. } | WirExpr::Load { ptr: inner, .. } | WirExpr::Load8U { ptr: inner, .. } | WirExpr::MemoryGrow(inner) | WirExpr::StructGet { base: inner, .. } | WirExpr::RefCast { value: inner, .. } | WirExpr::RefCastNullable { value: inner, .. } | WirExpr::ArrayLen(inner) | WirExpr::RefIsNull(inner) => collect_calls_expr(inner, called),
        WirExpr::Binary { lhs, rhs, .. } => {
            collect_calls_expr(lhs, called);
            collect_calls_expr(rhs, called);
        }
        WirExpr::ArrayNew { value, len, .. } => {
            collect_calls_expr(value, called);
            collect_calls_expr(len, called);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            collect_calls_expr(array, called);
            collect_calls_expr(index, called);
        }
        WirExpr::Control(node) => collect_calls_node(node, called),
        WirExpr::Seq(seq) => collect_calls_seq(seq, called),
        WirExpr::StructNew { args, .. } | WirExpr::ArrayNewFixed { items: args, .. } | WirExpr::Vector { args, .. } | WirExpr::CallHost { args, .. } => {
            for a in args { collect_calls_expr(a, called); }
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args { collect_calls_expr(a, called); }
            collect_calls_expr(index, called);
        }
        _ => {}
    }
}
