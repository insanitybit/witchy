//! Tail-call lowering result adaptation and symbol hygiene.

use std::collections::{HashMap, HashSet};

use crate::wir::{Kind, WirExpr, WirFunc, WirModule, WirNode, WirSeq};

pub(super) fn unique_function_name(module: &WirModule, added: &[WirFunc], stem: &str) -> String {
    let occupied = |candidate: &str| {
        module.funcs.iter().chain(added).any(|function| function.name == candidate)
    };
    if !occupied(stem) {
        return stem.to_string();
    }
    for suffix in 1usize.. {
        let candidate = format!("{stem}_{suffix}");
        if !occupied(&candidate) {
            return candidate;
        }
    }
    unreachable!("the function-name suffix space is finite")
}


pub(super) fn adapt_function_result_to_slot(seq: &mut WirSeq, kind: Kind) {
    adapt_explicit_returns_seq(seq, kind);
    if let Some(last) = seq.last_mut() {
        match last {
            WirNode::Source { body, .. } => adapt_tail_result_to_slot(body, kind),
            WirNode::Push(value) => wrap_to_slot(value, kind),
            _ => {}
        }
    }
}

fn adapt_tail_result_to_slot(seq: &mut WirSeq, kind: Kind) {
    if let Some(last) = seq.last_mut() {
        match last {
            WirNode::Source { body, .. } => adapt_tail_result_to_slot(body, kind),
            WirNode::Push(value) => wrap_to_slot(value, kind),
            _ => {}
        }
    }
}

fn wrap_to_slot(value: &mut WirExpr, kind: Kind) {
    let inner = std::mem::replace(value, WirExpr::ConstI32(0));
    *value = WirExpr::ToSlot(Box::new(inner), kind);
}

fn adapt_explicit_returns_seq(seq: &mut WirSeq, kind: Kind) {
    for node in seq {
        adapt_explicit_returns_node(node, kind);
    }
}

fn adapt_explicit_returns_node(node: &mut WirNode, kind: Kind) {
    match node {
        WirNode::Source { body, .. } => adapt_explicit_returns_seq(body, kind),
        WirNode::Return(Some(value)) => {
            adapt_explicit_returns_expr(value, kind);
            wrap_to_slot(value, kind);
        }
        WirNode::If { cond, then_, els, .. } => {
            adapt_explicit_returns_expr(cond, kind);
            adapt_explicit_returns_seq(then_, kind);
            adapt_explicit_returns_seq(els, kind);
        }
        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            adapt_explicit_returns_seq(body, kind);
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value) => adapt_explicit_returns_expr(value, kind),
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            adapt_explicit_returns_expr(ptr, kind);
            adapt_explicit_returns_expr(value, kind);
        }
        WirNode::ArraySet { array, index, value, .. } => {
            adapt_explicit_returns_expr(array, kind);
            adapt_explicit_returns_expr(index, kind);
            adapt_explicit_returns_expr(value, kind);
        }
        WirNode::CallStoreMulti { args, .. } => {
            for arg in args {
                adapt_explicit_returns_expr(arg, kind);
            }
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for arg in args {
                adapt_explicit_returns_expr(arg, kind);
            }
            adapt_explicit_returns_expr(index, kind);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            adapt_explicit_returns_expr(dest, kind);
            adapt_explicit_returns_expr(src, kind);
            adapt_explicit_returns_expr(len, kind);
        }
        WirNode::MemoryFill { dest, value, len } => {
            adapt_explicit_returns_expr(dest, kind);
            adapt_explicit_returns_expr(value, kind);
            adapt_explicit_returns_expr(len, kind);
        }
        WirNode::Br { cond: Some(cond), .. } => adapt_explicit_returns_expr(cond, kind),
        WirNode::Br { cond: None, .. }
        | WirNode::Return(None)
        | WirNode::Unreachable => {}
    }
}

fn adapt_explicit_returns_expr(expr: &mut WirExpr, kind: Kind) {
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
        | WirExpr::RefIsNull(inner) => adapt_explicit_returns_expr(inner, kind),
        WirExpr::Binary { lhs, rhs, .. } => {
            adapt_explicit_returns_expr(lhs, kind);
            adapt_explicit_returns_expr(rhs, kind);
        }
        WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. } => {
            for arg in args {
                adapt_explicit_returns_expr(arg, kind);
            }
        }
        WirExpr::ArrayNewFixed { items: args, .. } => {
            for arg in args {
                adapt_explicit_returns_expr(arg, kind);
            }
        }
        WirExpr::ArrayNew { value, len, .. } => {
            adapt_explicit_returns_expr(value, kind);
            adapt_explicit_returns_expr(len, kind);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            adapt_explicit_returns_expr(array, kind);
            adapt_explicit_returns_expr(index, kind);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for arg in args {
                adapt_explicit_returns_expr(arg, kind);
            }
            adapt_explicit_returns_expr(index, kind);
        }
        WirExpr::Control(node) => adapt_explicit_returns_node(node, kind),
        WirExpr::Seq(seq) => adapt_explicit_returns_seq(seq, kind),
        WirExpr::Vector { args, .. } => {
            for arg in args {
                adapt_explicit_returns_expr(arg, kind);
            }
        }
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::ConstV128(_)
        | WirExpr::StrPtr(_)
        | WirExpr::MemorySize
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::RefNull(_) => {}
    }
}

pub(super) fn rename_seq_locals(seq: &mut WirSeq, renames: &HashMap<String, String>) {
    for node in seq {
        rename_node_locals(node, renames);
    }
}

pub(in crate::wir_opt) fn rename_node_locals(
    node: &mut WirNode,
    renames: &HashMap<String, String>,
) {
    match node {
        WirNode::Source { body, .. } => rename_seq_locals(body, renames),
        WirNode::SetLocal { local, value } => {
            if let Some(replacement) = renames.get(local) {
                *local = replacement.clone();
            }
            rename_expr_locals(value, renames);
        }
        WirNode::If { cond, then_, els, .. } => {
            rename_expr_locals(cond, renames);
            rename_seq_locals(then_, renames);
            rename_seq_locals(els, renames);
        }
        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            rename_seq_locals(body, renames);
        }
        WirNode::SetGlobal { value, .. } | WirNode::Drop(value) | WirNode::Do(value)
        | WirNode::Push(value) | WirNode::Return(Some(value)) => {
            rename_expr_locals(value, renames);
        }
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            rename_expr_locals(ptr, renames);
            rename_expr_locals(value, renames);
        }
        WirNode::ArraySet { array, index, value, .. } => {
            rename_expr_locals(array, renames);
            rename_expr_locals(index, renames);
            rename_expr_locals(value, renames);
        }
        WirNode::CallStoreMulti { args, dests, .. } => {
            for arg in args {
                rename_expr_locals(arg, renames);
            }
            for dest in dests {
                if let Some(replacement) = renames.get(dest) {
                    *dest = replacement.clone();
                }
            }
        }
        WirNode::CallIndirectStoreMulti { args, index, dests, .. } => {
            for arg in args {
                rename_expr_locals(arg, renames);
            }
            rename_expr_locals(index, renames);
            for dest in dests {
                if let Some(replacement) = renames.get(dest) {
                    *dest = replacement.clone();
                }
            }
        }
        WirNode::MemoryCopy { dest, src, len } => {
            rename_expr_locals(dest, renames);
            rename_expr_locals(src, renames);
            rename_expr_locals(len, renames);
        }
        WirNode::MemoryFill { dest, value, len } => {
            rename_expr_locals(dest, renames);
            rename_expr_locals(value, renames);
            rename_expr_locals(len, renames);
        }
        WirNode::Br { cond: Some(expr), .. } => rename_expr_locals(expr, renames),
        WirNode::Br { cond: None, .. } | WirNode::Return(None) | WirNode::Unreachable => {}
    }
}

pub(in crate::wir_opt) fn rename_expr_locals(
    expr: &mut WirExpr,
    renames: &HashMap<String, String>,
) {
    match expr {
        WirExpr::GetLocal(local) => {
            if let Some(replacement) = renames.get(local) {
                *local = replacement.clone();
            }
        }
        WirExpr::ToSlot(inner, _) | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. } | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. } | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner) | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => rename_expr_locals(inner, renames),
        WirExpr::Binary { lhs, rhs, .. } => {
            rename_expr_locals(lhs, renames);
            rename_expr_locals(rhs, renames);
        }
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. } => {
            for arg in args {
                rename_expr_locals(arg, renames);
            }
        }
        WirExpr::ArrayNewFixed { items: args, .. } => {
            for arg in args {
                rename_expr_locals(arg, renames);
            }
        }
        WirExpr::ArrayNew { value, len, .. } => {
            rename_expr_locals(value, renames);
            rename_expr_locals(len, renames);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            rename_expr_locals(array, renames);
            rename_expr_locals(index, renames);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for arg in args {
                rename_expr_locals(arg, renames);
            }
            rename_expr_locals(index, renames);
        }
        WirExpr::Control(node) => rename_node_locals(node, renames),
        WirExpr::Seq(seq) => rename_seq_locals(seq, renames),
        WirExpr::Vector { args, .. } => {
            for arg in args {
                rename_expr_locals(arg, renames);
            }
        }
        WirExpr::ConstI64(_) | WirExpr::ConstF64(_) | WirExpr::ConstI32(_)
        | WirExpr::ConstV128(_) | WirExpr::StrPtr(_) | WirExpr::MemorySize | WirExpr::GetGlobal(_)
        | WirExpr::RefNull(_) => {}
    }
}

pub(super) fn unique_dispatch_label(functions: &[WirFunc], stem: &str) -> String {
    let mut labels = HashSet::new();
    for function in functions {
        collect_labels(&function.body, &mut labels);
    }
    if !labels.contains(stem) {
        return stem.to_string();
    }
    for suffix in 1usize.. {
        let candidate = format!("{stem}_{suffix}");
        if !labels.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("the label-name suffix space is finite")
}

fn collect_labels(seq: &WirSeq, labels: &mut HashSet<String>) {
    for node in seq {
        match node {
            WirNode::If { then_, els, .. } => {
                collect_labels(then_, labels);
                collect_labels(els, labels);
            }
            WirNode::Block { label, body, .. } | WirNode::Loop { label, body } => {
                labels.insert(label.clone());
                collect_labels(body, labels);
            }
            _ => {}
        }
    }
}

pub(super) fn unique_local_name(func: &WirFunc, stem: &str) -> String {
    let occupied = |candidate: &str| {
        func.params.iter().chain(&func.locals).any(|local| local.name == candidate)
    };
    if !occupied(stem) {
        return stem.to_string();
    }
    for suffix in 1usize.. {
        let candidate = format!("{stem}_{suffix}");
        if !occupied(&candidate) {
            return candidate;
        }
    }
    unreachable!("the local-name suffix space is finite")
}

pub(super) fn unique_label(func: &WirFunc, stem: &str) -> String {
    let mut labels = HashSet::new();
    collect_labels(&func.body, &mut labels);
    if !labels.contains(stem) {
        return stem.to_string();
    }
    for suffix in 1usize.. {
        let candidate = format!("{stem}_{suffix}");
        if !labels.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("the label-name suffix space is finite")
}
