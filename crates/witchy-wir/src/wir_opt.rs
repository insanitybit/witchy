//! WIR peephole optimizer — eliminates provably-redundant slot/kind conversions
//! introduced by naive nested-expression lowering.
//!
//! The universal value model carries every value as an i64 slot, converting in
//! and out with [`WirExpr::ToSlot`] / [`WirExpr::FromSlot`] and widening/narrowing
//! kinds with [`WirExpr::Convert`]. Lowering each sub-expression independently
//! produces redundant pairs:
//!
//!   * `FromSlot(ToSlot(x, k), k)` — value `x` round-tripped through a slot and
//!     back at the same kind, an identity.
//!   * `ToSlot(FromSlot(x, k), k)` — the mirror: a slot read out as kind `k` then
//!     immediately re-packed at the same kind, an identity.
//!   * `Convert { from: k, to: k, arg }` — a same-kind widen/narrow, an identity
//!     (and even cross-kind `Convert`s touching f64 are no-ops in the encoder,
//!     but we only cancel the provably-safe `from == to` case here).
//!
//! This is a pure structural rewrite: it rebuilds the tree bottom-up, cancelling
//! the patterns above, and repeats to a fixpoint so cancellations exposed by an
//! inner rewrite are themselves taken. It never changes the observable runtime
//! value — only removes conversions that compose to the identity.
//!
//! Functions with a [`WirFunc::raw_body`] are skipped entirely: their body is
//! pre-encoded wasm bytes with no WIR tree to walk.

#![allow(dead_code)]

use crate::wir::{WirExpr, WirModule, WirNode, WirSeq};

/// Counts from one [`optimize`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OptStats {
    /// Total WIR nodes (expr + statement nodes) across all walked funcs, before.
    pub nodes_before: usize,
    /// Total WIR nodes after the rewrite reached its fixpoint.
    pub nodes_after: usize,
    /// `nodes_before - nodes_after` — the count of nodes removed.
    pub eliminated: usize,
    /// How many full module passes ran before reaching the fixpoint.
    pub passes: usize,
}

/// Run the redundant-conversion elimination over every node-walked function in
/// `module` to a fixpoint, in place. Raw-body functions are left untouched.
/// Returns the before/after node counts.
pub fn optimize(module: &mut WirModule) -> OptStats {
    let nodes_before = module_size(module);

    let mut passes = 0;
    loop {
        let mut changed = false;
        for func in &mut module.funcs {
            // Raw-body funcs are pre-encoded wasm bytes — no WIR tree to walk.
            if func.raw_body.is_some() {
                continue;
            }
            simplify_seq(&mut func.body, &mut changed);
        }
        passes += 1;
        if !changed {
            break;
        }
    }

    let nodes_after = module_size(module);
    OptStats {
        nodes_before,
        nodes_after,
        eliminated: nodes_before - nodes_after,
        passes,
    }
}

/// Simplify every node in a sequence in place, marking `changed` if any rewrite
/// fired.
fn simplify_seq(seq: &mut WirSeq, changed: &mut bool) {
    for node in seq.iter_mut() {
        simplify_node(node, changed);
    }
}

/// Simplify a statement node: recurse into its child expressions/sequences.
fn simplify_node(node: &mut WirNode, changed: &mut bool) {
    match node {
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => {
            simplify_expr(value, changed);
        }
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
            simplify_expr(ptr, changed);
            simplify_expr(value, changed);
        }
        WirNode::CallStoreMulti { args, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, changed);
            }
        }
        WirNode::MemoryCopy { dest, src, len } => {
            simplify_expr(dest, changed);
            simplify_expr(src, changed);
            simplify_expr(len, changed);
        }
        WirNode::MemoryFill { dest, value, len } => {
            simplify_expr(dest, changed);
            simplify_expr(value, changed);
            simplify_expr(len, changed);
        }
        WirNode::If { cond, then_, els, .. } => {
            simplify_expr(cond, changed);
            simplify_seq(then_, changed);
            simplify_seq(els, changed);
        }
        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            simplify_seq(body, changed);
        }
        WirNode::Br { cond: Some(c), .. } => simplify_expr(c, changed),
        WirNode::Drop(e) | WirNode::Do(e) | WirNode::Push(e) | WirNode::Return(Some(e)) => {
            simplify_expr(e, changed);
        }
        WirNode::Br { cond: None, .. } | WirNode::Return(None) | WirNode::Unreachable => {}
    }
}

/// Simplify an expression in place: first rewrite all children bottom-up, then
/// apply the cancellation rules at this node (looping locally so a cancellation
/// that re-exposes a parent pattern is taken before returning).
fn simplify_expr(expr: &mut WirExpr, changed: &mut bool) {
    // 1. Recurse into children first (bottom-up).
    match expr {
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. } => simplify_expr(inner, changed),
        WirExpr::Binary { lhs, rhs, .. } => {
            simplify_expr(lhs, changed);
            simplify_expr(rhs, changed);
        }
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, changed);
            }
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, changed);
            }
            simplify_expr(index, changed);
        }
        WirExpr::MemoryGrow(pages) => simplify_expr(pages, changed),
        WirExpr::Control(node) => simplify_node(node, changed),
        WirExpr::Seq(nodes) => simplify_seq(nodes, changed),
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::StrPtr(_)
        | WirExpr::MemorySize
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_) => {}
    }

    // 2. Apply cancellation rules at this node, repeating while one fires so a
    //    cascade (e.g. an outer pair revealed once the inner one collapses) is
    //    fully taken here.
    while let Some(replacement) = cancel(expr) {
        *expr = replacement;
        *changed = true;
    }
}

/// If `expr` is a redundant conversion at its outer node, return its simplified
/// form (the value the conversions compose to). Returns `None` if no rule fires.
///
/// Rules (all preserve the exact runtime value):
///   * `FromSlot(ToSlot(x, k), k)` -> `x`
///   * `ToSlot(FromSlot(x, k), k)` -> `x`
///   * `Convert { from: k, to: k, arg }` -> `arg`
fn cancel(expr: &WirExpr) -> Option<WirExpr> {
    match expr {
        // FromSlot(ToSlot(x, k), k) -> x  (same kind on both legs)
        WirExpr::FromSlot(inner, outer_kind) => {
            if let WirExpr::ToSlot(x, inner_kind) = inner.as_ref() {
                if inner_kind == outer_kind {
                    return Some((**x).clone());
                }
            }
            None
        }
        // ToSlot(FromSlot(x, k), k) -> x  (same kind on both legs)
        WirExpr::ToSlot(inner, outer_kind) => {
            if let WirExpr::FromSlot(x, inner_kind) = inner.as_ref() {
                if inner_kind == outer_kind {
                    return Some((**x).clone());
                }
            }
            None
        }
        // Convert { from: k, to: k, arg } -> arg  (identity widen/narrow)
        WirExpr::Convert { from, to, arg } if from == to => Some((**arg).clone()),
        _ => None,
    }
}

// --- node-counting (a simple recursive size walk) ----------------------------

/// Total WIR node count across every node-walked function (raw-body funcs
/// contribute 0 — they have no tree). Counts each statement node and each
/// expression node once.
fn module_size(module: &WirModule) -> usize {
    module
        .funcs
        .iter()
        .filter(|f| f.raw_body.is_none())
        .map(|f| seq_size(&f.body))
        .sum()
}

fn seq_size(seq: &WirSeq) -> usize {
    seq.iter().map(node_size).sum()
}

fn node_size(node: &WirNode) -> usize {
    1 + match node {
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => expr_size(value),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
            expr_size(ptr) + expr_size(value)
        }
        WirNode::CallStoreMulti { args, .. } => args.iter().map(expr_size).sum(),
        WirNode::MemoryCopy { dest, src, len } => {
            expr_size(dest) + expr_size(src) + expr_size(len)
        }
        WirNode::MemoryFill { dest, value, len } => {
            expr_size(dest) + expr_size(value) + expr_size(len)
        }
        WirNode::If { cond, then_, els, .. } => {
            expr_size(cond) + seq_size(then_) + seq_size(els)
        }
        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => seq_size(body),
        WirNode::Br { cond: Some(c), .. } => expr_size(c),
        WirNode::Drop(e) | WirNode::Do(e) | WirNode::Push(e) | WirNode::Return(Some(e)) => {
            expr_size(e)
        }
        WirNode::Br { cond: None, .. } | WirNode::Return(None) | WirNode::Unreachable => 0,
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
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::StrPtr(_)
        | WirExpr::MemorySize
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_) => 0,
    }
}

#[cfg(test)]
#[cfg(feature = "native")]
#[path = "wir_opt_tests.rs"]
mod tests;
