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


use crate::wir::{WirExpr, WirFunc, WirLocal, WirModule, WirNode, WirSeq};

/// Lower direct self-tail calls to parameter rebinding plus a loop.
///
/// This is a semantic lowering, not an optional optimization. Multi-result
/// functions are left alone because their caller-side write-back/ownership
/// envelope is real residual work and is not yet a proper tail edge.
pub fn lower_self_tail_calls(module: &mut WirModule) -> usize {
    module.funcs.iter_mut().map(lower_func_self_tail_calls).sum()
}

struct TailCtx<'a> {
    function: &'a str,
    params: &'a [WirLocal],
    temps: &'a [WirLocal],
    loop_label: &'a str,
    result_ty: crate::wir::WirTy,
}

fn lower_func_self_tail_calls(func: &mut WirFunc) -> usize {
    let [result_ty] = func.ret.as_slice() else { return 0 };
    if func.raw_body.is_some() {
        return 0;
    }

    let loop_label = unique_local_name(func, "__witchy_tail_loop");
    let temps: Vec<_> = func
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| WirLocal {
            name: unique_local_name(func, &format!("__witchy_tail_arg_{index}")),
            ty: param.ty.clone(),
        })
        .collect();
    let ctx = TailCtx {
        function: &func.name,
        params: &func.params,
        temps: &temps,
        loop_label: &loop_label,
        result_ty: result_ty.clone(),
    };

    let mut body = func.body.clone();
    let mut count = rewrite_explicit_returns_seq(&mut body, &ctx);
    count += rewrite_function_tail(&mut body, &ctx);
    if count == 0 {
        return 0;
    }

    func.locals.extend(temps);
    func.body = vec![WirNode::Loop { label: loop_label, body }, WirNode::Unreachable];
    count
}

fn unique_local_name(func: &WirFunc, stem: &str) -> String {
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

fn rewrite_function_tail(seq: &mut WirSeq, ctx: &TailCtx<'_>) -> usize {
    let Some(last) = seq.last_mut() else { return 0 };
    match last {
        WirNode::Push(expr) => {
            let count = rewrite_tail_value_expr(expr, ctx);
            let expr = match std::mem::replace(last, WirNode::Unreachable) {
                WirNode::Push(expr) => expr,
                _ => unreachable!(),
            };
            *last = WirNode::Return(Some(expr));
            count
        }
        WirNode::Return(Some(expr)) => rewrite_tail_value_expr(expr, ctx),
        _ => 0,
    }
}

fn rewrite_tail_value_seq(seq: &mut WirSeq, ctx: &TailCtx<'_>) -> usize {
    let explicit = rewrite_explicit_returns_seq(seq, ctx);
    let Some(last) = seq.last_mut() else { return explicit };
    explicit
        + match last {
            WirNode::Push(expr) | WirNode::Return(Some(expr)) => {
                rewrite_tail_value_expr(expr, ctx)
            }
            WirNode::If { then_, els, result: Some(_), .. } => {
                rewrite_tail_value_seq(then_, ctx) + rewrite_tail_value_seq(els, ctx)
            }
            WirNode::Block { label, result: Some(_), body } => {
                rewrite_result_branches(body, label, ctx)
            }
            _ => 0,
        }
}

fn rewrite_tail_value_expr(expr: &mut WirExpr, ctx: &TailCtx<'_>) -> usize {
    match expr {
        WirExpr::Call { func, args }
            if func == ctx.function && args.len() == ctx.params.len() =>
        {
            let args = std::mem::take(args);
            let mut body = Vec::with_capacity(args.len() * 2 + 2);
            for (arg, temp) in args.into_iter().zip(ctx.temps) {
                body.push(WirNode::SetLocal { local: temp.name.clone(), value: arg });
            }
            for (param, temp) in ctx.params.iter().zip(ctx.temps) {
                body.push(WirNode::SetLocal {
                    local: param.name.clone(),
                    value: WirExpr::GetLocal(temp.name.clone()),
                });
            }
            body.push(WirNode::Br { target: ctx.loop_label.to_string(), cond: None });
            body.push(WirNode::Unreachable);
            *expr = WirExpr::Control(Box::new(WirNode::Block {
                label: "__witchy_tail_escape".into(),
                result: Some(ctx.result_ty.clone()),
                body,
            }));
            1
        }
        WirExpr::Control(node) => match node.as_mut() {
            WirNode::If { then_, els, result: Some(_), .. } => {
                rewrite_tail_value_seq(then_, ctx) + rewrite_tail_value_seq(els, ctx)
            }
            WirNode::Block { label, result: Some(_), body } => {
                rewrite_result_branches(body, label, ctx)
            }
            _ => 0,
        },
        WirExpr::Seq(seq) => rewrite_tail_value_seq(seq, ctx),
        _ => 0,
    }
}

/// Match lowering leaves each selected arm as `Push(value); br $result` inside
/// nested blocks. Rewrite only those value positions, retaining the result block
/// and its type for every non-recursive arm.
fn rewrite_result_branches(seq: &mut WirSeq, target: &str, ctx: &TailCtx<'_>) -> usize {
    let mut count = rewrite_explicit_returns_seq(seq, ctx);
    let mut index = 0;
    while index + 1 < seq.len() {
        let branches_to_result = matches!(
            &seq[index + 1],
            WirNode::Br { target: branch_target, cond: None } if branch_target == target
        );
        if branches_to_result && let WirNode::Push(expr) = &mut seq[index] {
            count += rewrite_tail_value_expr(expr, ctx);
        }
        index += 1;
    }
    for node in seq {
        count += match node {
            WirNode::If { then_, els, .. } => {
                rewrite_result_branches(then_, target, ctx)
                    + rewrite_result_branches(els, target, ctx)
            }
            WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                rewrite_result_branches(body, target, ctx)
            }
            _ => 0,
        };
    }
    count
}

fn rewrite_explicit_returns_seq(seq: &mut WirSeq, ctx: &TailCtx<'_>) -> usize {
    seq.iter_mut().map(|node| rewrite_explicit_returns_node(node, ctx)).sum()
}

fn rewrite_explicit_returns_node(node: &mut WirNode, ctx: &TailCtx<'_>) -> usize {
    match node {
        WirNode::Return(Some(expr)) => rewrite_tail_value_expr(expr, ctx),
        WirNode::If { cond, then_, els, .. } => {
            rewrite_explicit_returns_expr(cond, ctx)
                + rewrite_explicit_returns_seq(then_, ctx)
                + rewrite_explicit_returns_seq(els, ctx)
        }
        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            rewrite_explicit_returns_seq(body, ctx)
        }
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => {
            rewrite_explicit_returns_expr(value, ctx)
        }
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
            rewrite_explicit_returns_expr(ptr, ctx) + rewrite_explicit_returns_expr(value, ctx)
        }
        WirNode::CallStoreMulti { args, .. } => {
            args.iter_mut().map(|arg| rewrite_explicit_returns_expr(arg, ctx)).sum()
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            args.iter_mut()
                .map(|arg| rewrite_explicit_returns_expr(arg, ctx))
                .sum::<usize>()
                + rewrite_explicit_returns_expr(index, ctx)
        }
        WirNode::MemoryCopy { dest, src, len } => {
            rewrite_explicit_returns_expr(dest, ctx)
                + rewrite_explicit_returns_expr(src, ctx)
                + rewrite_explicit_returns_expr(len, ctx)
        }
        WirNode::MemoryFill { dest, value, len } => {
            rewrite_explicit_returns_expr(dest, ctx)
                + rewrite_explicit_returns_expr(value, ctx)
                + rewrite_explicit_returns_expr(len, ctx)
        }
        WirNode::StructSet { base, value, .. } => {
            rewrite_explicit_returns_expr(base, ctx) + rewrite_explicit_returns_expr(value, ctx)
        }
        WirNode::Br { cond: Some(expr), .. }
        | WirNode::Drop(expr)
        | WirNode::Do(expr)
        | WirNode::Push(expr) => rewrite_explicit_returns_expr(expr, ctx),
        WirNode::Br { cond: None, .. } | WirNode::Return(None) | WirNode::Unreachable => 0,
    }
}

fn rewrite_explicit_returns_expr(expr: &mut WirExpr, ctx: &TailCtx<'_>) -> usize {
    match expr {
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::MemoryGrow(inner)
        | WirExpr::StructGet { base: inner, .. }
        | WirExpr::RefIsNull(inner) => rewrite_explicit_returns_expr(inner, ctx),
        WirExpr::Binary { lhs, rhs, .. } => {
            rewrite_explicit_returns_expr(lhs, ctx) + rewrite_explicit_returns_expr(rhs, ctx)
        }
        WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. } => {
            args.iter_mut().map(|arg| rewrite_explicit_returns_expr(arg, ctx)).sum()
        }
        WirExpr::CallIndirect { args, index, .. } => {
            args.iter_mut()
                .map(|arg| rewrite_explicit_returns_expr(arg, ctx))
                .sum::<usize>()
                + rewrite_explicit_returns_expr(index, ctx)
        }
        WirExpr::Control(node) => rewrite_explicit_returns_node(node, ctx),
        WirExpr::Seq(seq) => rewrite_explicit_returns_seq(seq, ctx),
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::StrPtr(_)
        | WirExpr::MemorySize
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::RefNull(_) => 0,
    }
}

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
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, changed);
            }
            simplify_expr(index, changed);
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
        WirNode::StructSet { base, value, .. } => {
            simplify_expr(base, changed);
            simplify_expr(value, changed);
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
        WirExpr::StructNew { args, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, changed);
            }
        }
        WirExpr::StructGet { base, .. } | WirExpr::RefIsNull(base) => simplify_expr(base, changed),
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::StrPtr(_)
        | WirExpr::MemorySize
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::RefNull(_) => {}
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
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            args.iter().map(expr_size).sum::<usize>() + expr_size(index)
        }
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
        WirNode::StructSet { base, value, .. } => expr_size(base) + expr_size(value),
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
        WirExpr::StructNew { args, .. } => args.iter().map(expr_size).sum(),
        WirExpr::StructGet { base, .. } | WirExpr::RefIsNull(base) => expr_size(base),
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::StrPtr(_)
        | WirExpr::MemorySize
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::RefNull(_) => 0,
    }
}

#[cfg(test)]
#[cfg(feature = "native")]
#[path = "wir_opt_tests.rs"]
mod tests;
