//! Tail-call graph and tail-position analysis.

use std::collections::HashSet;

use crate::wir::{ClosureSignature, Kind, WirExpr, WirFunc, WirNode, WirSeq};

use super::TailCallee;

pub(super) fn strongly_connected_components(graph: &[Vec<usize>]) -> Vec<Vec<usize>> {
    fn visit(node: usize, graph: &[Vec<usize>], seen: &mut [bool], order: &mut Vec<usize>) {
        if std::mem::replace(&mut seen[node], true) {
            return;
        }
        for &next in &graph[node] {
            visit(next, graph, seen, order);
        }
        order.push(node);
    }
    fn collect(node: usize, graph: &[Vec<usize>], seen: &mut [bool], out: &mut Vec<usize>) {
        if std::mem::replace(&mut seen[node], true) {
            return;
        }
        out.push(node);
        for &next in &graph[node] {
            collect(next, graph, seen, out);
        }
    }

    let mut seen = vec![false; graph.len()];
    let mut order = Vec::with_capacity(graph.len());
    for node in 0..graph.len() {
        visit(node, graph, &mut seen, &mut order);
    }
    let mut reverse = vec![Vec::new(); graph.len()];
    for (source, targets) in graph.iter().enumerate() {
        for &target in targets {
            reverse[target].push(source);
        }
    }
    seen.fill(false);
    let mut components = Vec::new();
    for node in order.into_iter().rev() {
        if !seen[node] {
            let mut component = Vec::new();
            collect(node, &reverse, &mut seen, &mut component);
            components.push(component);
        }
    }
    for component in &mut components {
        component.sort_unstable();
    }
    components.sort_unstable_by_key(|component| component.first().copied());
    components
}


fn slot_adaptable_result(function: &WirFunc) -> bool {
    matches!(
        function.ret.as_slice(),
        [result] if matches!(result.kind(), Kind::I32 | Kind::I64 | Kind::F64)
    )
}

pub(super) fn has_single_result(function: &WirFunc) -> bool {
    function.ret.len() == 1
}

pub(super) fn dispatcher_results_compatible(functions: &[WirFunc]) -> bool {
    let Some(first) = functions.first().and_then(|function| function.ret.first()) else {
        return false;
    };
    functions.iter().all(|function| {
        function.ret.first().is_some_and(|result| result.kind() == first.kind())
    }) || functions.iter().all(slot_adaptable_result)
}

pub(super) fn indirect_signature_matches(function: &WirFunc, signature: &ClosureSignature) -> bool {
    function.params.iter().map(|param| param.ty.kind()).eq(signature.params.iter().copied())
        && function.ret.iter().map(|result| result.kind()).eq(signature.results.iter().copied())
}


pub(in crate::wir_opt) fn collect_function_tail_calls(
    seq: &WirSeq,
    out: &mut HashSet<TailCallee>,
) {
    collect_tail_calls_seq(seq, out);
}

fn collect_tail_calls_seq(seq: &WirSeq, out: &mut HashSet<TailCallee>) {
    collect_explicit_return_calls_seq(seq, out);
    let Some(last) = seq.last() else { return };
    match last {
        WirNode::Source { body, .. } => collect_tail_calls_seq(body, out),
        WirNode::Push(expr) | WirNode::Return(Some(expr)) => collect_tail_calls_expr(expr, out),
        WirNode::If { then_, els, result: Some(_), .. } => {
            collect_tail_calls_seq(then_, out);
            collect_tail_calls_seq(els, out);
        }
        WirNode::Block { label, result: Some(_), body } => {
            collect_result_branch_calls(body, label, out);
        }
        _ => {}
    }
}

fn collect_tail_calls_expr(expr: &WirExpr, out: &mut HashSet<TailCallee>) {
    match expr {
        WirExpr::Call { func, .. } => {
            out.insert(TailCallee::Direct(func.clone()));
        }
        WirExpr::CallIndirect { signature, .. } => {
            out.insert(TailCallee::Indirect(signature.clone()));
        }
        WirExpr::ToSlot(inner, kind) | WirExpr::FromSlot(inner, kind)
            if matches!(kind, Kind::I32 | Kind::I64 | Kind::F64) =>
        {
            collect_tail_calls_expr(inner, out);
        }
        WirExpr::Control(node) => match node.as_ref() {
            WirNode::If { then_, els, result: Some(_), .. } => {
                collect_tail_calls_seq(then_, out);
                collect_tail_calls_seq(els, out);
            }
            WirNode::Block { label, result: Some(_), body } => {
                collect_result_branch_calls(body, label, out);
            }
            _ => {}
        },
        WirExpr::Seq(seq) => collect_tail_calls_seq(seq, out),
        _ => {}
    }
}

fn collect_result_branch_calls(seq: &WirSeq, target: &str, out: &mut HashSet<TailCallee>) {
    collect_explicit_return_calls_seq(seq, out);
    for pair in seq.windows(2) {
        if let [WirNode::Push(expr), WirNode::Br { target: branch_target, cond: None }] = pair
            && branch_target == target
        {
            collect_tail_calls_expr(expr, out);
        }
    }
    for node in seq {
        match node {
            WirNode::Source { body, .. } => {
                collect_result_branch_calls(body, target, out);
            }
            WirNode::If { then_, els, .. } => {
                collect_result_branch_calls(then_, target, out);
                collect_result_branch_calls(els, target, out);
            }
            WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                collect_result_branch_calls(body, target, out);
            }
            _ => {}
        }
    }
}

fn collect_explicit_return_calls_seq(seq: &WirSeq, out: &mut HashSet<TailCallee>) {
    for node in seq {
        collect_explicit_return_calls_node(node, out);
    }
}

fn collect_explicit_return_calls_node(node: &WirNode, out: &mut HashSet<TailCallee>) {
    match node {
        WirNode::Source { body, .. } => collect_explicit_return_calls_seq(body, out),
        WirNode::Return(Some(expr)) => collect_tail_calls_expr(expr, out),
        WirNode::If { cond, then_, els, .. } => {
            collect_explicit_return_calls_expr(cond, out);
            collect_explicit_return_calls_seq(then_, out);
            collect_explicit_return_calls_seq(els, out);
        }
        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            collect_explicit_return_calls_seq(body, out);
        }
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value) | WirNode::Do(value) | WirNode::Push(value) => {
            collect_explicit_return_calls_expr(value, out);
        }
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            collect_explicit_return_calls_expr(ptr, out);
            collect_explicit_return_calls_expr(value, out);
        }
        WirNode::ArraySet { array, index, value, .. } => {
            collect_explicit_return_calls_expr(array, out);
            collect_explicit_return_calls_expr(index, out);
            collect_explicit_return_calls_expr(value, out);
        }
        WirNode::CallStoreMulti { args, .. } => {
            for arg in args {
                collect_explicit_return_calls_expr(arg, out);
            }
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for arg in args {
                collect_explicit_return_calls_expr(arg, out);
            }
            collect_explicit_return_calls_expr(index, out);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            collect_explicit_return_calls_expr(dest, out);
            collect_explicit_return_calls_expr(src, out);
            collect_explicit_return_calls_expr(len, out);
        }
        WirNode::MemoryFill { dest, value, len } => {
            collect_explicit_return_calls_expr(dest, out);
            collect_explicit_return_calls_expr(value, out);
            collect_explicit_return_calls_expr(len, out);
        }
        WirNode::Br { cond: Some(expr), .. } => collect_explicit_return_calls_expr(expr, out),
        WirNode::Br { cond: None, .. } | WirNode::Return(None) | WirNode::Unreachable => {}
    }
}

fn collect_explicit_return_calls_expr(expr: &WirExpr, out: &mut HashSet<TailCallee>) {
    match expr {
        WirExpr::ToSlot(inner, _) | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. } | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. } | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner) | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => collect_explicit_return_calls_expr(inner, out),
        WirExpr::Binary { lhs, rhs, .. } => {
            collect_explicit_return_calls_expr(lhs, out);
            collect_explicit_return_calls_expr(rhs, out);
        }
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. } => {
            for arg in args {
                collect_explicit_return_calls_expr(arg, out);
            }
        }
        WirExpr::ArrayNewFixed { items: args, .. } => {
            for arg in args {
                collect_explicit_return_calls_expr(arg, out);
            }
        }
        WirExpr::ArrayNew { value, len, .. } => {
            collect_explicit_return_calls_expr(value, out);
            collect_explicit_return_calls_expr(len, out);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            collect_explicit_return_calls_expr(array, out);
            collect_explicit_return_calls_expr(index, out);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for arg in args {
                collect_explicit_return_calls_expr(arg, out);
            }
            collect_explicit_return_calls_expr(index, out);
        }
        WirExpr::Control(node) => collect_explicit_return_calls_node(node, out),
        WirExpr::Seq(seq) => collect_explicit_return_calls_seq(seq, out),
        WirExpr::ConstI64(_) | WirExpr::ConstF64(_) | WirExpr::ConstI32(_)
        | WirExpr::StrPtr(_) | WirExpr::MemorySize | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_) | WirExpr::RefNull(_) => {}
    }
}
