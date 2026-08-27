//! RFC-0154 Call-Graph Specialization: Exact Callable Devirtualization,
//! Capture Scalar Replacement, and Budgeted Inlining.

use std::collections::{HashMap, HashSet};
use crate::wir::{
    WirExpr, WirFunc, WirLocal, WirModule, WirNode, WirSeq,
    CLOSURE_CODE_FIELD,
};

pub fn inline_direct_calls(module: &mut WirModule) {
    let mut changed = true;
    let mut pass = 0;
    while changed && pass < 10 {
        changed = false;
        pass += 1;
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
        if func.raw_body.is_some() {
            continue;
        }
        let mut closures = HashMap::new();
        devirt_seq(&mut func.body, &funcs, &mut closures, changed);
    }
}

fn devirt_seq(
    seq: &mut WirSeq,
    funcs: &[String],
    closures: &mut HashMap<String, String>,
    changed: &mut bool,
) {
    for node in seq {
        devirt_node(node, funcs, closures, changed);
    }
}

fn closure_target(value: &WirExpr, funcs: &[String]) -> Option<String> {
    let WirExpr::StructNew { struct_id: 0, args } = value else {
        return None;
    };
    let Some(WirExpr::ConstI32(index)) = args.first() else {
        return None;
    };
    funcs.get(*index as usize).cloned()
}

fn collect_assigned_locals_seq(seq: &WirSeq, assigned: &mut HashSet<String>) {
    for node in seq {
        collect_assigned_locals_node(node, assigned);
    }
}

fn collect_assigned_locals_node(node: &WirNode, assigned: &mut HashSet<String>) {
    match node {
        WirNode::SetLocal { local, value } => {
            assigned.insert(local.clone());
            collect_assigned_locals_expr(value, assigned);
        }
        WirNode::CallStoreMulti { args, dests, .. } => {
            assigned.extend(dests.iter().cloned());
            for argument in args {
                collect_assigned_locals_expr(argument, assigned);
            }
        }
        WirNode::CallIndirectStoreMulti { args, index, dests, .. } => {
            assigned.extend(dests.iter().cloned());
            for argument in args {
                collect_assigned_locals_expr(argument, assigned);
            }
            collect_assigned_locals_expr(index, assigned);
        }
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            collect_assigned_locals_seq(body, assigned);
        }
        WirNode::If { cond, then_, els, .. } => {
            collect_assigned_locals_expr(cond, assigned);
            collect_assigned_locals_seq(then_, assigned);
            collect_assigned_locals_seq(els, assigned);
        }
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            collect_assigned_locals_expr(ptr, assigned);
            collect_assigned_locals_expr(value, assigned);
        }
        WirNode::SafeStore { place, value, .. }
        | WirNode::SafeStore8 { place, value } => {
            place.for_each_wir_expr(|expr| collect_assigned_locals_expr(expr, assigned));
            collect_assigned_locals_expr(value, assigned);
        }
        WirNode::SafeKernel(operation) => {
            operation.for_each_wir_expr(|expr| collect_assigned_locals_expr(expr, assigned));
        }
        WirNode::ArraySet { array, index, value, .. } => {
            collect_assigned_locals_expr(array, assigned);
            collect_assigned_locals_expr(index, assigned);
            collect_assigned_locals_expr(value, assigned);
        }
        WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => collect_assigned_locals_expr(value, assigned),
        WirNode::MemoryCopy { dest, src, len } => {
            collect_assigned_locals_expr(dest, assigned);
            collect_assigned_locals_expr(src, assigned);
            collect_assigned_locals_expr(len, assigned);
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            destination.for_each_wir_expr(|expr| collect_assigned_locals_expr(expr, assigned));
            source.for_each_wir_expr(|expr| collect_assigned_locals_expr(expr, assigned));
            collect_assigned_locals_expr(len, assigned);
        }
        WirNode::MemoryFill { dest, value, len } => {
            collect_assigned_locals_expr(dest, assigned);
            collect_assigned_locals_expr(value, assigned);
            collect_assigned_locals_expr(len, assigned);
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            destination.for_each_wir_expr(|expr| collect_assigned_locals_expr(expr, assigned));
            collect_assigned_locals_expr(value, assigned);
            collect_assigned_locals_expr(len, assigned);
        }
        WirNode::Br { cond: None, .. }
        | WirNode::Return(None)
        | WirNode::Unreachable => {}
    }
}

fn collect_assigned_locals_expr(expr: &WirExpr, assigned: &mut HashSet<String>) {
    match expr {
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => collect_assigned_locals_expr(inner, assigned),
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            place.for_each_wir_expr(|inner| collect_assigned_locals_expr(inner, assigned));
        }
        WirExpr::SafeKernel(operation) => {
            operation.for_each_wir_expr(|inner| collect_assigned_locals_expr(inner, assigned));
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            collect_assigned_locals_expr(lhs, assigned);
            collect_assigned_locals_expr(rhs, assigned);
        }
        WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. } => {
            for argument in args {
                collect_assigned_locals_expr(argument, assigned);
            }
        }
        WirExpr::ArrayNew { value, len, .. } => {
            collect_assigned_locals_expr(value, assigned);
            collect_assigned_locals_expr(len, assigned);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            collect_assigned_locals_expr(array, assigned);
            collect_assigned_locals_expr(index, assigned);
        }
        WirExpr::Control(node) => collect_assigned_locals_node(node, assigned),
        WirExpr::Seq(seq) => collect_assigned_locals_seq(seq, assigned),
        WirExpr::CallIndirect { args, index, .. } => {
            for argument in args {
                collect_assigned_locals_expr(argument, assigned);
            }
            collect_assigned_locals_expr(index, assigned);
        }
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::ConstV128(_)
        | WirExpr::StrPtr(_)
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::MemorySize
        | WirExpr::RefNull(_) => {}
    }
}

fn devirt_node(
    node: &mut WirNode,
    funcs: &[String],
    closures: &mut HashMap<String, String>,
    changed: &mut bool,
) {
    match node {
        WirNode::SetLocal { local, value } => {
            devirt_expr(value, funcs, closures, changed);
            if let Some(target) = closure_target(value, funcs) {
                closures.insert(local.clone(), target);
            } else {
                closures.remove(local);
            }
        }
        WirNode::Source { body, .. } => {
            devirt_seq(body, funcs, closures, changed);
        }
        WirNode::Block { body, .. } => {
            let mut assigned = HashSet::new();
            collect_assigned_locals_seq(body, &mut assigned);
            for local in &assigned {
                closures.remove(local);
            }
            let mut body_closures = closures.clone();
            devirt_seq(body, funcs, &mut body_closures, changed);
        }
        WirNode::Loop { body, .. } => {
            let mut assigned = HashSet::new();
            collect_assigned_locals_seq(body, &mut assigned);
            for local in &assigned {
                closures.remove(local);
            }
            let mut body_closures = closures.clone();
            devirt_seq(body, funcs, &mut body_closures, changed);
        }
        WirNode::If { cond, then_, els, .. } => {
            devirt_expr(cond, funcs, closures, changed);
            let mut then_closures = closures.clone();
            let mut else_closures = closures.clone();
            devirt_seq(then_, funcs, &mut then_closures, changed);
            devirt_seq(els, funcs, &mut else_closures, changed);
            closures.retain(|local, target| {
                then_closures.get(local) == Some(target)
                    && else_closures.get(local) == Some(target)
            });
        }
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            devirt_expr(ptr, funcs, closures, changed);
            devirt_expr(value, funcs, closures, changed);
        }
        WirNode::SafeKernel(operation) => {
            operation.for_each_wir_expr_mut(|expr| devirt_expr(expr, funcs, closures, changed));
        }
        WirNode::SafeStore { place, value, .. }
        | WirNode::SafeStore8 { place, value } => {
            place.for_each_wir_expr_mut(|expr| devirt_expr(expr, funcs, closures, changed));
            devirt_expr(value, funcs, closures, changed);
        }
        WirNode::ArraySet { array, index, value, .. } => {
            devirt_expr(array, funcs, closures, changed);
            devirt_expr(index, funcs, closures, changed);
            devirt_expr(value, funcs, closures, changed);
        }
        WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => {
            devirt_expr(value, funcs, closures, changed);
        }
        WirNode::CallStoreMulti { args, dests, .. } => {
            for a in args {
                devirt_expr(a, funcs, closures, changed);
            }
            for destination in dests {
                closures.remove(destination);
            }
        }
        WirNode::CallIndirectStoreMulti { args, index, dests, .. } => {
            for a in args {
                devirt_expr(a, funcs, closures, changed);
            }
            devirt_expr(index, funcs, closures, changed);
            for destination in dests {
                closures.remove(destination);
            }
        }
        WirNode::MemoryCopy { dest, src, len } => {
            devirt_expr(dest, funcs, closures, changed);
            devirt_expr(src, funcs, closures, changed);
            devirt_expr(len, funcs, closures, changed);
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            destination
                .for_each_wir_expr_mut(|expr| devirt_expr(expr, funcs, closures, changed));
            source.for_each_wir_expr_mut(|expr| devirt_expr(expr, funcs, closures, changed));
            devirt_expr(len, funcs, closures, changed);
        }
        WirNode::MemoryFill { dest, value, len } => {
            devirt_expr(dest, funcs, closures, changed);
            devirt_expr(value, funcs, closures, changed);
            devirt_expr(len, funcs, closures, changed);
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            destination
                .for_each_wir_expr_mut(|expr| devirt_expr(expr, funcs, closures, changed));
            devirt_expr(value, funcs, closures, changed);
            devirt_expr(len, funcs, closures, changed);
        }
        _ => {}
    }
}

fn devirt_expr(
    expr: &mut WirExpr,
    funcs: &[String],
    closures: &mut HashMap<String, String>,
    changed: &mut bool,
) {
    match expr {
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => {
            devirt_expr(inner, funcs, closures, changed);
        }
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            place.for_each_wir_expr_mut(|inner| devirt_expr(inner, funcs, closures, changed));
        }
        WirExpr::SafeKernel(operation) => {
            operation.for_each_wir_expr_mut(|inner| devirt_expr(inner, funcs, closures, changed));
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            devirt_expr(lhs, funcs, closures, changed);
            devirt_expr(rhs, funcs, closures, changed);
        }
        WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. } => {
            for a in args {
                devirt_expr(a, funcs, closures, changed);
            }
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
            for a in args.iter_mut() {
                devirt_expr(a, funcs, closures, changed);
            }
            devirt_expr(index, funcs, closures, changed);

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

fn do_inlining(module: &mut WirModule, changed: &mut bool) {
    let mut inlineable = HashMap::new();
    for func in &module.funcs {
        if func.raw_body.is_none() && seq_size(&func.body) < 50 {
            if !has_call_seq(&func.body) {
                inlineable.insert(func.name.clone(), func.clone());
            }
        }
    }

    for func in &mut module.funcs {
        if func.raw_body.is_some() {
            continue;
        }
        let mut new_locals = Vec::new();
        inline_in_seq(&mut func.body, &inlineable, &func.name, &mut new_locals, changed);
        if !new_locals.is_empty() {
            func.locals.extend(new_locals);
        }
    }
}

fn inline_in_seq(
    seq: &mut WirSeq,
    inlineable: &HashMap<String, WirFunc>,
    caller_name: &str,
    new_locals: &mut Vec<WirLocal>,
    changed: &mut bool,
) {
    for node in seq {
        inline_in_node(node, inlineable, caller_name, new_locals, changed);
    }
}

fn inline_in_node(
    node: &mut WirNode,
    inlineable: &HashMap<String, WirFunc>,
    caller_name: &str,
    new_locals: &mut Vec<WirLocal>,
    changed: &mut bool,
) {
    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            inline_in_seq(body, inlineable, caller_name, new_locals, changed);
        }
        WirNode::If { cond, then_, els, .. } => {
            inline_in_expr(cond, inlineable, caller_name, new_locals, changed);
            inline_in_seq(then_, inlineable, caller_name, new_locals, changed);
            inline_in_seq(els, inlineable, caller_name, new_locals, changed);
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => {
            inline_in_expr(value, inlineable, caller_name, new_locals, changed);
        }
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            inline_in_expr(ptr, inlineable, caller_name, new_locals, changed);
            inline_in_expr(value, inlineable, caller_name, new_locals, changed);
        }
        WirNode::SafeStore { place, value, .. }
        | WirNode::SafeStore8 { place, value } => {
            place.for_each_wir_expr_mut(|expr| {
                inline_in_expr(expr, inlineable, caller_name, new_locals, changed);
            });
            inline_in_expr(value, inlineable, caller_name, new_locals, changed);
        }
        WirNode::SafeKernel(operation) => {
            operation.for_each_wir_expr_mut(|expr| {
                inline_in_expr(expr, inlineable, caller_name, new_locals, changed);
            });
        }
        WirNode::ArraySet { array, index, value, .. } => {
            inline_in_expr(array, inlineable, caller_name, new_locals, changed);
            inline_in_expr(index, inlineable, caller_name, new_locals, changed);
            inline_in_expr(value, inlineable, caller_name, new_locals, changed);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            inline_in_expr(dest, inlineable, caller_name, new_locals, changed);
            inline_in_expr(src, inlineable, caller_name, new_locals, changed);
            inline_in_expr(len, inlineable, caller_name, new_locals, changed);
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            destination.for_each_wir_expr_mut(|expr| {
                inline_in_expr(expr, inlineable, caller_name, new_locals, changed);
            });
            source.for_each_wir_expr_mut(|expr| {
                inline_in_expr(expr, inlineable, caller_name, new_locals, changed);
            });
            inline_in_expr(len, inlineable, caller_name, new_locals, changed);
        }
        WirNode::MemoryFill { dest, value, len } => {
            inline_in_expr(dest, inlineable, caller_name, new_locals, changed);
            inline_in_expr(value, inlineable, caller_name, new_locals, changed);
            inline_in_expr(len, inlineable, caller_name, new_locals, changed);
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            destination.for_each_wir_expr_mut(|expr| {
                inline_in_expr(expr, inlineable, caller_name, new_locals, changed);
            });
            inline_in_expr(value, inlineable, caller_name, new_locals, changed);
            inline_in_expr(len, inlineable, caller_name, new_locals, changed);
        }
        _ => {}
    }
}

fn inline_in_expr(
    expr: &mut WirExpr,
    inlineable: &HashMap<String, WirFunc>,
    caller_name: &str,
    new_locals: &mut Vec<WirLocal>,
    changed: &mut bool,
) {
    match expr {
        WirExpr::Call { func, args } => {
            for a in args.iter_mut() {
                inline_in_expr(a, inlineable, caller_name, new_locals, changed);
            }
            if func != caller_name {
                if let Some(target) = inlineable.get(func) {
                    if let Some(single_expr) = extract_single_expr(&target.body) {
                        if target.locals.is_empty() {
                            let mut cloned_expr = single_expr.clone();
                            let mut args_map = HashMap::new();
                            for (param, arg) in target.params.iter().zip(args.iter()) {
                                args_map.insert(param.name.clone(), arg.clone());
                            }
                            replace_get_locals_expr(&mut cloned_expr, &args_map);
                            simplify_struct_ops(&mut cloned_expr);
                            *expr = cloned_expr;
                            *changed = true;
                        }
                    }
                }
            }
        }
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => {
            inline_in_expr(inner, inlineable, caller_name, new_locals, changed);
            simplify_struct_ops(expr);
        }
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            place.for_each_wir_expr_mut(|inner| {
                inline_in_expr(inner, inlineable, caller_name, new_locals, changed);
                simplify_struct_ops(inner);
            });
        }
        WirExpr::SafeKernel(operation) => {
            operation.for_each_wir_expr_mut(|inner| {
                inline_in_expr(inner, inlineable, caller_name, new_locals, changed);
                simplify_struct_ops(inner);
            });
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            inline_in_expr(lhs, inlineable, caller_name, new_locals, changed);
            inline_in_expr(rhs, inlineable, caller_name, new_locals, changed);
        }
        WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. } => {
            for a in args {
                inline_in_expr(a, inlineable, caller_name, new_locals, changed);
            }
        }
        WirExpr::ArrayNew { value, len, .. } => {
            inline_in_expr(value, inlineable, caller_name, new_locals, changed);
            inline_in_expr(len, inlineable, caller_name, new_locals, changed);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            inline_in_expr(array, inlineable, caller_name, new_locals, changed);
            inline_in_expr(index, inlineable, caller_name, new_locals, changed);
        }
        WirExpr::Control(node) => {
            inline_in_node(node, inlineable, caller_name, new_locals, changed);
        }
        WirExpr::Seq(seq) => {
            inline_in_seq(seq, inlineable, caller_name, new_locals, changed);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args {
                inline_in_expr(a, inlineable, caller_name, new_locals, changed);
            }
            inline_in_expr(index, inlineable, caller_name, new_locals, changed);
        }
        _ => {}
    }
}

fn extract_single_expr(seq: &WirSeq) -> Option<WirExpr> {
    if seq.len() == 1 {
        match &seq[0] {
            WirNode::Push(e) | WirNode::Return(Some(e)) => Some(e.clone()),
            _ => None,
        }
    } else {
        None
    }
}

fn replace_get_locals_expr(expr: &mut WirExpr, args_map: &HashMap<String, WirExpr>) {
    match expr {
        WirExpr::GetLocal(name) => {
            if let Some(subst) = args_map.get(name) {
                *expr = subst.clone();
            }
        }
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => {
            replace_get_locals_expr(inner, args_map);
        }
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            place.for_each_wir_expr_mut(|inner| replace_get_locals_expr(inner, args_map));
        }
        WirExpr::SafeKernel(operation) => {
            operation.for_each_wir_expr_mut(|inner| replace_get_locals_expr(inner, args_map));
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            replace_get_locals_expr(lhs, args_map);
            replace_get_locals_expr(rhs, args_map);
        }
        WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. } => {
            for a in args {
                replace_get_locals_expr(a, args_map);
            }
        }
        WirExpr::ArrayNew { value, len, .. } => {
            replace_get_locals_expr(value, args_map);
            replace_get_locals_expr(len, args_map);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            replace_get_locals_expr(array, args_map);
            replace_get_locals_expr(index, args_map);
        }
        WirExpr::Seq(seq) => {
            for node in seq {
                replace_get_locals_node(node, args_map);
            }
        }
        WirExpr::Control(node) => {
            replace_get_locals_node(node, args_map);
        }
        _ => {}
    }
}

fn replace_get_locals_node(node: &mut WirNode, args_map: &HashMap<String, WirExpr>) {
    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            for n in body {
                replace_get_locals_node(n, args_map);
            }
        }
        WirNode::If { cond, then_, els, .. } => {
            replace_get_locals_expr(cond, args_map);
            for n in then_ {
                replace_get_locals_node(n, args_map);
            }
            for n in els {
                replace_get_locals_node(n, args_map);
            }
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => {
            replace_get_locals_expr(value, args_map);
        }
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            replace_get_locals_expr(ptr, args_map);
            replace_get_locals_expr(value, args_map);
        }
        WirNode::SafeStore { place, value, .. }
        | WirNode::SafeStore8 { place, value } => {
            place.for_each_wir_expr_mut(|expr| replace_get_locals_expr(expr, args_map));
            replace_get_locals_expr(value, args_map);
        }
        WirNode::SafeKernel(operation) => {
            operation.for_each_wir_expr_mut(|expr| replace_get_locals_expr(expr, args_map));
        }
        WirNode::ArraySet { array, index, value, .. } => {
            replace_get_locals_expr(array, args_map);
            replace_get_locals_expr(index, args_map);
            replace_get_locals_expr(value, args_map);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            replace_get_locals_expr(dest, args_map);
            replace_get_locals_expr(src, args_map);
            replace_get_locals_expr(len, args_map);
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            destination.for_each_wir_expr_mut(|expr| replace_get_locals_expr(expr, args_map));
            source.for_each_wir_expr_mut(|expr| replace_get_locals_expr(expr, args_map));
            replace_get_locals_expr(len, args_map);
        }
        WirNode::MemoryFill { dest, value, len } => {
            replace_get_locals_expr(dest, args_map);
            replace_get_locals_expr(value, args_map);
            replace_get_locals_expr(len, args_map);
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            destination.for_each_wir_expr_mut(|expr| replace_get_locals_expr(expr, args_map));
            replace_get_locals_expr(value, args_map);
            replace_get_locals_expr(len, args_map);
        }
        WirNode::CallStoreMulti { args, .. } => {
            for arg in args {
                replace_get_locals_expr(arg, args_map);
            }
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for arg in args {
                replace_get_locals_expr(arg, args_map);
            }
            replace_get_locals_expr(index, args_map);
        }
        _ => {}
    }
}

pub fn simplify_struct_ops(expr: &mut WirExpr) {
    match expr {
        WirExpr::StructGet { struct_id, field, base } => {
            simplify_struct_ops(base);
            if let WirExpr::StructNew { struct_id: new_id, args } = base.as_ref() {
                if struct_id == new_id {
                    if let Some(arg) = args.get(*field as usize) {
                        *expr = arg.clone();
                        simplify_struct_ops(expr);
                    }
                }
            }
        }
        WirExpr::RefCast { struct_id, value } | WirExpr::RefCastNullable { struct_id, value } => {
            simplify_struct_ops(value);
            if let WirExpr::StructNew { struct_id: new_id, .. } = value.as_ref() {
                if struct_id == new_id {
                    *expr = *value.clone();
                    simplify_struct_ops(expr);
                }
            }
        }
        WirExpr::Unary { arg, .. }
        | WirExpr::ToSlot(arg, _)
        | WirExpr::FromSlot(arg, _)
        | WirExpr::Convert { arg, .. }
        | WirExpr::MemoryGrow(arg)
        | WirExpr::ArrayLen(arg)
        | WirExpr::RefIsNull(arg) => {
            simplify_struct_ops(arg);
        }
        WirExpr::Load { ptr: arg, .. }
        | WirExpr::Load8U { ptr: arg, .. } => {
            simplify_struct_ops(arg);
        }
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            place.for_each_wir_expr_mut(simplify_struct_ops);
        }
        WirExpr::SafeKernel(operation) => {
            operation.for_each_wir_expr_mut(simplify_struct_ops);
        }
        WirExpr::Binary { lhs, rhs, .. } => {
            simplify_struct_ops(lhs);
            simplify_struct_ops(rhs);
        }
        WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. } => {
            for a in args {
                simplify_struct_ops(a);
            }
        }
        WirExpr::ArrayNew { value, len, .. } => {
            simplify_struct_ops(value);
            simplify_struct_ops(len);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            simplify_struct_ops(array);
            simplify_struct_ops(index);
        }
        _ => {}
    }
}

fn has_call_seq(seq: &WirSeq) -> bool {
    seq.iter().any(has_call_node)
}

fn safe_place_has_call(place: &crate::safewir_surface::SafePlaceExpr) -> bool {
    let mut found = false;
    place.for_each_wir_expr(|expr| found |= has_call_expr(expr));
    found
}

fn has_call_node(node: &WirNode) -> bool {
    match node {
        WirNode::CallStoreMulti { .. } | WirNode::CallIndirectStoreMulti { .. } => true,
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            has_call_seq(body)
        }
        WirNode::If { cond, then_, els, .. } => {
            has_call_expr(cond) || has_call_seq(then_) || has_call_seq(els)
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => has_call_expr(value),
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            has_call_expr(ptr) || has_call_expr(value)
        }
        WirNode::SafeStore { place, value, .. }
        | WirNode::SafeStore8 { place, value } => {
            safe_place_has_call(place) || has_call_expr(value)
        }
        WirNode::SafeKernel(operation) => {
            let mut has_call = false;
            operation.for_each_wir_expr(|expr| has_call |= has_call_expr(expr));
            has_call
        }
        WirNode::ArraySet { array, index, value, .. } => {
            has_call_expr(array) || has_call_expr(index) || has_call_expr(value)
        }
        WirNode::MemoryCopy { dest, src, len } => {
            has_call_expr(dest) || has_call_expr(src) || has_call_expr(len)
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            safe_place_has_call(destination)
                || safe_place_has_call(source)
                || has_call_expr(len)
        }
        WirNode::MemoryFill { dest, value, len } => {
            has_call_expr(dest) || has_call_expr(value) || has_call_expr(len)
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            safe_place_has_call(destination) || has_call_expr(value) || has_call_expr(len)
        }
        _ => false,
    }
}

fn has_call_expr(expr: &WirExpr) -> bool {
    match expr {
        WirExpr::Call { .. } | WirExpr::CallHost { .. } | WirExpr::CallIndirect { .. } => true,
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => has_call_expr(inner),
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            safe_place_has_call(place)
        }
        WirExpr::SafeKernel(operation) => {
            let mut has_call = false;
            operation.for_each_wir_expr(|expr| has_call |= has_call_expr(expr));
            has_call
        }
        WirExpr::Binary { lhs, rhs, .. } => has_call_expr(lhs) || has_call_expr(rhs),
        WirExpr::ArrayNew { value, len, .. } => has_call_expr(value) || has_call_expr(len),
        WirExpr::ArrayGet { array, index, .. } => has_call_expr(array) || has_call_expr(index),
        WirExpr::Control(node) => has_call_node(node),
        WirExpr::Seq(seq) => has_call_seq(seq),
        WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. } => args.iter().any(has_call_expr),
        _ => false,
    }
}

pub fn seq_size(seq: &WirSeq) -> usize {
    seq.iter().map(node_size).sum()
}

fn node_size(node: &WirNode) -> usize {
    1 + match node {
        WirNode::Source { body, .. } => seq_size(body),
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => expr_size(value),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
            expr_size(ptr) + expr_size(value)
        }
        WirNode::SafeStore { place, value, .. }
        | WirNode::SafeStore8 { place, value } => safe_place_size(place) + expr_size(value),
        WirNode::SafeKernel(operation) => {
            let mut size = 0;
            operation.for_each_wir_expr(|expr| size += expr_size(expr));
            size
        }
        WirNode::CallStoreMulti { args, .. } => args.iter().map(expr_size).sum(),
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            args.iter().map(expr_size).sum::<usize>() + expr_size(index)
        }
        WirNode::MemoryCopy { dest, src, len } => {
            expr_size(dest) + expr_size(src) + expr_size(len)
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            safe_place_size(destination) + safe_place_size(source) + expr_size(len)
        }
        WirNode::MemoryFill { dest, value, len } => {
            expr_size(dest) + expr_size(value) + expr_size(len)
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            safe_place_size(destination) + expr_size(value) + expr_size(len)
        }
        WirNode::If { cond, then_, els, .. } => {
            expr_size(cond) + seq_size(then_) + seq_size(els)
        }
        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => seq_size(body),
        WirNode::StructSet { base, value, .. } => expr_size(base) + expr_size(value),
        WirNode::ArraySet { array, index, value, .. } => {
            expr_size(array) + expr_size(index) + expr_size(value)
        }
        WirNode::Br { cond: Some(c), .. } => expr_size(c),
        WirNode::Drop(e) | WirNode::Do(e) | WirNode::Push(e) | WirNode::Return(Some(e)) => {
            expr_size(e)
        }
        _ => 0,
    }
}

fn expr_size(expr: &WirExpr) -> usize {
    1 + match expr {
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. } => expr_size(inner),
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => safe_place_size(place),
        WirExpr::SafeKernel(operation) => {
            let mut size = 0;
            operation.for_each_wir_expr(|expr| size += expr_size(expr));
            size
        }
        WirExpr::Binary { lhs, rhs, .. } => expr_size(lhs) + expr_size(rhs),
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => {
            args.iter().map(expr_size).sum()
        }
        WirExpr::CallIndirect { args, index, .. } => {
            args.iter().map(expr_size).sum::<usize>() + expr_size(index)
        }
        WirExpr::MemoryGrow(pages) => expr_size(pages),
        WirExpr::Control(node) => node_size(node),
        WirExpr::Seq(nodes) => seq_size(nodes),
        WirExpr::StructNew { args, .. } => args.iter().map(expr_size).sum(),
        WirExpr::ArrayNewFixed { items, .. } => items.iter().map(expr_size).sum(),
        WirExpr::ArrayNew { value, len, .. } => expr_size(value) + expr_size(len),
        WirExpr::ArrayGet { array, index, .. } => expr_size(array) + expr_size(index),
        WirExpr::StructGet { base, .. }
        | WirExpr::RefCast { value: base, .. }
        | WirExpr::RefCastNullable { value: base, .. }
        | WirExpr::ArrayLen(base)
        | WirExpr::RefIsNull(base) => expr_size(base),
        WirExpr::Vector { args, .. } => args.iter().map(expr_size).sum(),
        _ => 0,
    }
}

fn safe_place_size(place: &crate::safewir_surface::SafePlaceExpr) -> usize {
    let mut size = 0;
    place.for_each_wir_expr(|expr| size += expr_size(expr));
    size
}

fn prune_unused_functions(module: &mut WirModule, changed: &mut bool) {
    let mut called = HashSet::new();
    if let Some(t) = &module.table {
        for f in &t.funcs {
            called.insert(f.clone());
        }
    }
    for (_export_name, func_name) in &module.exports {
        called.insert(func_name.clone());
    }
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

fn collect_calls_seq(seq: &WirSeq, called: &mut HashSet<String>) {
    for node in seq {
        collect_calls_node(node, called);
    }
}

fn collect_calls_node(node: &WirNode, called: &mut HashSet<String>) {
    match node {
        WirNode::CallStoreMulti { func, args, .. } => {
            called.insert(func.clone());
            for a in args {
                collect_calls_expr(a, called);
            }
        }
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            collect_calls_seq(body, called);
        }
        WirNode::If { cond, then_, els, .. } => {
            collect_calls_expr(cond, called);
            collect_calls_seq(then_, called);
            collect_calls_seq(els, called);
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => collect_calls_expr(value, called),
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            collect_calls_expr(ptr, called);
            collect_calls_expr(value, called);
        }
        WirNode::SafeStore { place, value, .. }
        | WirNode::SafeStore8 { place, value } => {
            place.for_each_wir_expr(|expr| collect_calls_expr(expr, called));
            collect_calls_expr(value, called);
        }
        WirNode::SafeKernel(operation) => {
            operation.for_each_wir_expr(|expr| collect_calls_expr(expr, called));
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
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            destination.for_each_wir_expr(|expr| collect_calls_expr(expr, called));
            source.for_each_wir_expr(|expr| collect_calls_expr(expr, called));
            collect_calls_expr(len, called);
        }
        WirNode::MemoryFill { dest, value, len } => {
            collect_calls_expr(dest, called);
            collect_calls_expr(value, called);
            collect_calls_expr(len, called);
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            destination.for_each_wir_expr(|expr| collect_calls_expr(expr, called));
            collect_calls_expr(value, called);
            collect_calls_expr(len, called);
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for a in args {
                collect_calls_expr(a, called);
            }
            collect_calls_expr(index, called);
        }
        _ => {}
    }
}

fn collect_calls_expr(expr: &WirExpr, called: &mut HashSet<String>) {
    match expr {
        WirExpr::Call { func, args } => {
            called.insert(func.clone());
            for a in args {
                collect_calls_expr(a, called);
            }
        }
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => collect_calls_expr(inner, called),
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            place.for_each_wir_expr(|inner| collect_calls_expr(inner, called));
        }
        WirExpr::SafeKernel(operation) => {
            operation.for_each_wir_expr(|inner| collect_calls_expr(inner, called));
        }
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
        WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. }
        | WirExpr::CallHost { args, .. } => {
            for a in args {
                collect_calls_expr(a, called);
            }
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args {
                collect_calls_expr(a, called);
            }
            collect_calls_expr(index, called);
        }
        _ => {}
    }
}
