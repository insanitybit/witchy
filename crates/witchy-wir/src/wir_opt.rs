//! WIR semantic lowering and peephole optimization. Recursive proper calls become
//! typed loops/state machines before redundant slot/kind conversions are removed.
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


use std::collections::{HashMap, HashSet};

use crate::wir::{Kind, WirExpr, WirFunc, WirLocal, WirModule, WirNode, WirSeq, WirTy};

/// Lower recursive direct proper calls to typed state machines.
///
/// This is a semantic lowering, not an optional optimization. Multi-result
/// functions are left alone because their caller-side write-back/ownership
/// envelope is real residual work and is not yet a proper tail edge.
pub fn lower_direct_tail_calls(module: &mut WirModule) -> usize {
    let mut count = lower_mutual_tail_components(module);
    count += module.funcs.iter_mut().map(lower_func_self_tail_calls).sum::<usize>();
    count
}

#[derive(Clone)]
struct TailTarget {
    state: Option<i32>,
    params: Vec<WirLocal>,
    temps: Vec<WirLocal>,
    locals: Vec<WirLocal>,
    result_ty: WirTy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct IndirectSig {
    type_arity: usize,
    result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TailCallee {
    Direct(String),
    Indirect(IndirectSig),
}

#[derive(Clone)]
struct IndirectPlan {
    args: Vec<WirLocal>,
    index: WirLocal,
    targets: Vec<(i32, TailTarget)>,
}

struct TailCtx {
    targets: HashMap<String, TailTarget>,
    indirect: HashMap<IndirectSig, IndirectPlan>,
    source_bank: Vec<WirLocal>,
    state_local: Option<String>,
    loop_label: String,
}

fn lower_func_self_tail_calls(func: &mut WirFunc) -> usize {
    if func.ret.len() != 1 {
        return lower_func_self_tail_envelope(func);
    }
    let result_ty = &func.ret[0];
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
        targets: HashMap::from([(
            func.name.clone(),
            TailTarget {
                state: None,
                params: func.params.clone(),
                temps: temps.clone(),
                locals: func.locals.clone(),
                result_ty: result_ty.clone(),
            },
        )]),
        indirect: HashMap::new(),
        source_bank: func.params.iter().chain(&func.locals).cloned().collect(),
        state_local: None,
        loop_label: loop_label.clone(),
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

/// Lower the canonical ownership-token envelope emitted for an `own` aggregate
/// tail call. Unlike arbitrary multi-result calls, this pair is forwarded
/// unchanged: `(declared value, ownership token)` becomes the current function's
/// complete result, so no caller-side write-back remains.
fn lower_func_self_tail_envelope(func: &mut WirFunc) -> usize {
    if func.raw_body.is_some()
        || !matches!(func.ret.as_slice(), [_, token] if token.kind() == Kind::I32)
    {
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
    let target = TailTarget {
        state: None,
        params: func.params.clone(),
        temps: temps.clone(),
        locals: func.locals.clone(),
        result_ty: func.ret[0].clone(),
    };
    let ctx = TailCtx {
        targets: HashMap::from([(func.name.clone(), target.clone())]),
        indirect: HashMap::new(),
        source_bank: func.params.iter().chain(&func.locals).cloned().collect(),
        state_local: None,
        loop_label: loop_label.clone(),
    };
    let mut body = func.body.clone();
    if !rewrite_forwarded_envelope_tail(&mut body, &func.name, &target, &ctx) {
        return 0;
    }

    func.locals.extend(temps);
    func.body = vec![WirNode::Loop { label: loop_label, body }, WirNode::Unreachable];
    1
}

fn rewrite_forwarded_envelope_tail(
    seq: &mut WirSeq,
    function: &str,
    target: &TailTarget,
    ctx: &TailCtx,
) -> bool {
    if seq.len() < 2 {
        return false;
    }
    let cap_local = match seq.last() {
        Some(WirNode::Push(WirExpr::GetLocal(local))) => local.clone(),
        _ => return false,
    };
    let value_index = seq.len() - 2;
    let (args, value_local) = match &mut seq[value_index] {
        WirNode::Push(WirExpr::Seq(inner)) if inner.len() == 2 => {
            let value_local = match &inner[1] {
                WirNode::Push(WirExpr::GetLocal(local)) => local.clone(),
                _ => return false,
            };
            match &mut inner[0] {
                WirNode::CallStoreMulti { func, args, dests }
                    if func == function
                        && dests.as_slice() == [value_local.clone(), cap_local.clone()]
                        && args.len() == target.params.len() =>
                {
                    (std::mem::take(args), value_local)
                }
                _ => return false,
            }
        }
        _ => return false,
    };
    debug_assert!(!value_local.is_empty());
    seq.truncate(value_index);
    seq.extend(tail_transition_nodes(target, args, &[], ctx));
    true
}

fn lower_mutual_tail_components(module: &mut WirModule) -> usize {
    let function_count = module.funcs.len();
    let by_name: HashMap<_, _> = module
        .funcs
        .iter()
        .enumerate()
        .filter(|(_, function)| function.raw_body.is_none() && slot_adaptable_result(function))
        .map(|(index, function)| (function.name.clone(), index))
        .collect();
    let mut graph = vec![Vec::new(); function_count];
    let mut has_indirect_tail = vec![false; function_count];
    for (index, function) in module.funcs.iter().enumerate() {
        if function.raw_body.is_some() || !slot_adaptable_result(function) {
            continue;
        }
        let mut targets = HashSet::new();
        collect_function_tail_calls(&function.body, &mut targets);
        has_indirect_tail[index] = targets
            .iter()
            .any(|target| matches!(target, TailCallee::Indirect(_)));
        let mut edges: Vec<_> = targets
            .into_iter()
            .flat_map(|target| -> Vec<usize> {
                match target {
                    TailCallee::Direct(target) => {
                        by_name.get(&target).copied().into_iter().collect()
                    }
                    TailCallee::Indirect(signature) => module
                        .table
                        .iter()
                        .flat_map(|table| table.funcs.iter())
                        .filter_map(|target| by_name.get(target).copied())
                        .filter(|target| {
                            indirect_signature_matches(&module.funcs[*target], signature)
                        })
                        .collect(),
                }
            })
            .collect();
        edges.sort_unstable();
        graph[index] = edges;
    }

    let components = strongly_connected_components(&graph);
    let mut dispatchers = Vec::new();
    let mut count = 0;
    for component in components.into_iter().filter(|component| {
        component.len() > 1
            || component
                .first()
                .is_some_and(|member| {
                    has_indirect_tail[*member] && graph[*member].contains(member)
                })
    }) {
        let originals: Vec<_> = component
            .iter()
            .map(|index| module.funcs[*index].clone())
            .collect();
        let dispatcher_name = unique_function_name(
            module,
            &dispatchers,
            &format!("__witchy_tail_scc_{}", dispatchers.len()),
        );
        let table_slots: HashMap<_, _> = module
            .table
            .iter()
            .flat_map(|table| table.funcs.iter().enumerate())
            .map(|(index, name)| (name.clone(), index as i32))
            .collect();
        let (dispatcher, rewritten) =
            build_tail_dispatcher(&dispatcher_name, &originals, &table_slots);
        if rewritten == 0 {
            continue;
        }
        for ((index, original), state) in component.iter().zip(&originals).zip(0i32..) {
            module.funcs[*index] = tail_entry_wrapper(original, &dispatcher_name, state, &originals);
        }
        dispatchers.push(dispatcher);
        count += rewritten;
    }
    module.funcs.extend(dispatchers);
    count
}

fn strongly_connected_components(graph: &[Vec<usize>]) -> Vec<Vec<usize>> {
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

fn unique_function_name(module: &WirModule, added: &[WirFunc], stem: &str) -> String {
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

fn slot_adaptable_result(function: &WirFunc) -> bool {
    matches!(
        function.ret.as_slice(),
        [result] if matches!(result.kind(), Kind::I32 | Kind::I64 | Kind::F64)
    )
}

fn indirect_signature_matches(function: &WirFunc, signature: IndirectSig) -> bool {
    function.params.len() == signature.type_arity + 1
        && function.params.first().is_some_and(|param| param.ty.kind() == Kind::I32)
        && function.params[1..].iter().all(|param| param.ty.kind() == Kind::I64)
        && function.ret.len() == signature.result_count
        && function.ret.iter().all(|result| result.kind() == Kind::I64)
}

fn build_tail_dispatcher(
    name: &str,
    functions: &[WirFunc],
    table_slots: &HashMap<String, i32>,
) -> (WirFunc, usize) {
    let state_local = "__witchy_tail_state".to_string();
    let first_result = functions[0].ret[0].clone();
    let dispatcher_result = if functions
        .iter()
        .all(|function| function.ret[0].kind() == first_result.kind())
    {
        first_result
    } else {
        WirTy::Slot
    };
    // `Bool` is the WIR's neutral i32 carrier; the internal tag is not exposed as
    // a Witchy Bool and may use values above one for larger components.
    let mut params = vec![WirLocal { name: state_local.clone(), ty: WirTy::Bool }];
    let mut locals = Vec::new();
    let mut bodies = Vec::new();
    let mut targets = HashMap::new();

    for (state, function) in (0i32..).zip(functions) {
        let renamed_params: Vec<_> = function
            .params
            .iter()
            .enumerate()
            .map(|(index, param)| WirLocal {
                name: format!("__witchy_tail_p_{state}_{index}"),
                ty: param.ty.clone(),
            })
            .collect();
        let renamed_locals: Vec<_> = function
            .locals
            .iter()
            .enumerate()
            .map(|(index, local)| WirLocal {
                name: format!("__witchy_tail_l_{state}_{index}"),
                ty: local.ty.clone(),
            })
            .collect();
        let temps: Vec<_> = function
            .params
            .iter()
            .enumerate()
            .map(|(index, param)| WirLocal {
                name: format!("__witchy_tail_arg_{state}_{index}"),
                ty: param.ty.clone(),
            })
            .collect();
        let renames: HashMap<_, _> = function
            .params
            .iter()
            .zip(&renamed_params)
            .chain(function.locals.iter().zip(&renamed_locals))
            .map(|(old, new)| (old.name.clone(), new.name.clone()))
            .collect();
        let mut body = function.body.clone();
        rename_seq_locals(&mut body, &renames);
        params.extend(renamed_params.clone());
        locals.extend(renamed_locals.clone());
        locals.extend(temps.clone());
        targets.insert(
            function.name.clone(),
            TailTarget {
                state: Some(state),
                params: renamed_params,
                temps,
                locals: renamed_locals.clone(),
                result_ty: function.ret[0].clone(),
            },
        );
        bodies.push(body);
    }

    let mut indirect_signatures = HashSet::new();
    for (function, body) in functions.iter().zip(&bodies) {
        let mut callees = HashSet::new();
        collect_function_tail_calls(body, &mut callees);
        indirect_signatures.extend(callees.into_iter().filter_map(|callee| match callee {
            TailCallee::Indirect(signature)
                if function.ret.len() == signature.result_count
                    && slot_adaptable_result(function) =>
            {
                Some(signature)
            }
            _ => None,
        }));
    }
    let mut indirect_signatures: Vec<_> = indirect_signatures.into_iter().collect();
    indirect_signatures.sort_by_key(|signature| (signature.type_arity, signature.result_count));
    let mut indirect = HashMap::new();
    for (plan_index, signature) in indirect_signatures.into_iter().enumerate() {
        let args: Vec<_> = std::iter::once(WirLocal {
            name: format!("__witchy_tail_indirect_{plan_index}_env"),
            ty: WirTy::Bool,
        })
        .chain((0..signature.type_arity).map(|index| WirLocal {
            name: format!("__witchy_tail_indirect_{plan_index}_arg_{index}"),
            ty: WirTy::Int,
        }))
        .collect();
        let index = WirLocal {
            name: format!("__witchy_tail_indirect_{plan_index}_index"),
            ty: WirTy::Bool,
        };
        let mut plan_targets: Vec<_> = functions
            .iter()
            .filter(|function| indirect_signature_matches(function, signature))
            .filter_map(|function| {
                Some((
                    *table_slots.get(&function.name)?,
                    targets.get(&function.name)?.clone(),
                ))
            })
            .collect();
        plan_targets.sort_by_key(|(table_index, _)| *table_index);
        if !plan_targets.is_empty() {
            locals.extend(args.iter().cloned());
            locals.push(index.clone());
            indirect.insert(signature, IndirectPlan { args, index, targets: plan_targets });
        }
    }

    let loop_label = unique_dispatch_label(functions, "__witchy_tail_dispatch_loop");
    let mut count = 0;
    for (function, body) in functions.iter().zip(&mut bodies) {
        let source = targets.get(&function.name).expect("SCC member has a target bank");
        let ctx = TailCtx {
            targets: targets.clone(),
            indirect: indirect.clone(),
            source_bank: source.params.iter().chain(&source.locals).cloned().collect(),
            state_local: Some(state_local.clone()),
            loop_label: loop_label.clone(),
        };
        count += rewrite_explicit_returns_seq(body, &ctx);
        count += rewrite_function_tail(body, &ctx);
        if function.ret[0].kind() != dispatcher_result.kind() {
            adapt_function_result_to_slot(body, function.ret[0].kind());
        }
    }

    let mut selection = vec![WirNode::Unreachable];
    for (state, body) in bodies.into_iter().enumerate().rev() {
        selection = vec![WirNode::If {
            cond: WirExpr::Binary {
                op: crate::wir::BinOp::Eq,
                kind: Kind::I32,
                lhs: Box::new(WirExpr::GetLocal(state_local.clone())),
                rhs: Box::new(WirExpr::ConstI32(state as i32)),
            },
            then_: body,
            els: selection,
            result: None,
        }];
    }
    (
        WirFunc {
            name: name.to_string(),
            params,
            ret: vec![dispatcher_result],
            locals,
            body: vec![WirNode::Loop { label: loop_label, body: selection }, WirNode::Unreachable],
            raw_body: None,
        },
        count,
    )
}

fn tail_entry_wrapper(
    original: &WirFunc,
    dispatcher: &str,
    state: i32,
    functions: &[WirFunc],
) -> WirFunc {
    let first_kind = functions[0].ret[0].kind();
    let dispatcher_kind = if functions
        .iter()
        .all(|function| function.ret[0].kind() == first_kind)
    {
        first_kind
    } else {
        Kind::I64
    };
    let mut args = vec![WirExpr::ConstI32(state)];
    for function in functions {
        if function.name == original.name {
            args.extend(original.params.iter().map(|param| WirExpr::GetLocal(param.name.clone())));
        } else {
            args.extend(function.params.iter().map(|param| default_value(&param.ty)));
        }
    }
    let call = WirExpr::Call { func: dispatcher.to_string(), args };
    let result = if dispatcher_kind == original.ret[0].kind() {
        call
    } else {
        WirExpr::FromSlot(Box::new(call), original.ret[0].kind())
    };
    WirFunc {
        name: original.name.clone(),
        params: original.params.clone(),
        ret: original.ret.clone(),
        locals: Vec::new(),
        body: vec![WirNode::Push(result)],
        raw_body: None,
    }
}

fn default_value(ty: &WirTy) -> WirExpr {
    match ty.kind() {
        Kind::I64 => WirExpr::ConstI64(0),
        Kind::F64 => WirExpr::ConstF64(0.0),
        Kind::I32 => WirExpr::ConstI32(0),
        kind @ (Kind::ExternRef | Kind::StructRef | Kind::GcRef(_)) => WirExpr::RefNull(kind),
    }
}

fn collect_function_tail_calls(seq: &WirSeq, out: &mut HashSet<TailCallee>) {
    collect_explicit_return_calls_seq(seq, out);
    let Some(last) = seq.last() else { return };
    if let WirNode::Push(expr) | WirNode::Return(Some(expr)) = last {
        collect_tail_calls_expr(expr, out);
    }
}

fn collect_tail_calls_seq(seq: &WirSeq, out: &mut HashSet<TailCallee>) {
    collect_explicit_return_calls_seq(seq, out);
    let Some(last) = seq.last() else { return };
    match last {
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
        WirExpr::CallIndirect { type_arity, result_count, .. } => {
            out.insert(TailCallee::Indirect(IndirectSig {
                type_arity: *type_arity,
                result_count: *result_count,
            }));
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

fn adapt_function_result_to_slot(seq: &mut WirSeq, kind: Kind) {
    adapt_explicit_returns_seq(seq, kind);
    if let Some(WirNode::Push(value)) = seq.last_mut() {
        wrap_to_slot(value, kind);
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
        WirExpr::CallIndirect { args, index, .. } => {
            for arg in args {
                adapt_explicit_returns_expr(arg, kind);
            }
            adapt_explicit_returns_expr(index, kind);
        }
        WirExpr::Control(node) => adapt_explicit_returns_node(node, kind),
        WirExpr::Seq(seq) => adapt_explicit_returns_seq(seq, kind),
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::StrPtr(_)
        | WirExpr::MemorySize
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::RefNull(_) => {}
    }
}

fn rename_seq_locals(seq: &mut WirSeq, renames: &HashMap<String, String>) {
    for node in seq {
        rename_node_locals(node, renames);
    }
}

fn rename_node_locals(node: &mut WirNode, renames: &HashMap<String, String>) {
    match node {
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

fn rename_expr_locals(expr: &mut WirExpr, renames: &HashMap<String, String>) {
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
        WirExpr::CallIndirect { args, index, .. } => {
            for arg in args {
                rename_expr_locals(arg, renames);
            }
            rename_expr_locals(index, renames);
        }
        WirExpr::Control(node) => rename_node_locals(node, renames),
        WirExpr::Seq(seq) => rename_seq_locals(seq, renames),
        WirExpr::ConstI64(_) | WirExpr::ConstF64(_) | WirExpr::ConstI32(_)
        | WirExpr::StrPtr(_) | WirExpr::MemorySize | WirExpr::GetGlobal(_)
        | WirExpr::RefNull(_) => {}
    }
}

fn unique_dispatch_label(functions: &[WirFunc], stem: &str) -> String {
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

fn rewrite_function_tail(seq: &mut WirSeq, ctx: &TailCtx) -> usize {
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

fn rewrite_tail_value_seq(seq: &mut WirSeq, ctx: &TailCtx) -> usize {
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

fn rewrite_tail_value_expr(expr: &mut WirExpr, ctx: &TailCtx) -> usize {
    match expr {
        WirExpr::Call { func, args }
            if ctx.targets.get(func).is_some_and(|target| args.len() == target.params.len()) => {
            let target = ctx.targets.get(func).cloned().expect("guarded tail target");
            let args = std::mem::take(args);
            *expr = tail_transition_expr(&target, args, &[], ctx);
            1
        }
        WirExpr::CallIndirect { type_arity, result_count, args, index }
            if ctx.indirect.contains_key(&IndirectSig {
                type_arity: *type_arity,
                result_count: *result_count,
            }) =>
        {
            let signature = IndirectSig {
                type_arity: *type_arity,
                result_count: *result_count,
            };
            let plan = ctx.indirect.get(&signature).cloned().expect("guarded indirect plan");
            if args.len() != plan.args.len() {
                return 0;
            }
            let staged_args = std::mem::take(args);
            let staged_index = std::mem::replace(index, Box::new(WirExpr::ConstI32(0)));
            let mut seq = Vec::with_capacity(plan.args.len() + 2);
            for (arg, temp) in staged_args.into_iter().zip(&plan.args) {
                seq.push(WirNode::SetLocal { local: temp.name.clone(), value: arg });
            }
            seq.push(WirNode::SetLocal {
                local: plan.index.name.clone(),
                value: *staged_index,
            });
            let fallback = WirExpr::CallIndirect {
                type_arity: signature.type_arity,
                result_count: signature.result_count,
                args: plan
                    .args
                    .iter()
                    .map(|temp| WirExpr::GetLocal(temp.name.clone()))
                    .collect(),
                index: Box::new(WirExpr::GetLocal(plan.index.name.clone())),
            };
            let mut choice = fallback;
            let mut cleanup = plan.args.clone();
            cleanup.push(plan.index.clone());
            for (table_index, target) in plan.targets.iter().rev() {
                let transition = tail_transition_expr(
                    target,
                    plan.args
                        .iter()
                        .map(|temp| WirExpr::GetLocal(temp.name.clone()))
                        .collect(),
                    &cleanup,
                    ctx,
                );
                choice = WirExpr::Control(Box::new(WirNode::If {
                    cond: WirExpr::Binary {
                        op: crate::wir::BinOp::Eq,
                        kind: Kind::I32,
                        lhs: Box::new(WirExpr::GetLocal(plan.index.name.clone())),
                        rhs: Box::new(WirExpr::ConstI32(*table_index)),
                    },
                    then_: vec![WirNode::Push(transition)],
                    els: vec![WirNode::Push(choice)],
                    result: Some(WirTy::Slot),
                }));
            }
            seq.push(WirNode::Push(choice));
            *expr = WirExpr::Seq(seq);
            1
        }
        WirExpr::ToSlot(inner, kind) | WirExpr::FromSlot(inner, kind)
            if matches!(kind, Kind::I32 | Kind::I64 | Kind::F64) =>
        {
            rewrite_tail_value_expr(inner, ctx)
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

fn tail_transition_expr(
    target: &TailTarget,
    args: Vec<WirExpr>,
    cleanup: &[WirLocal],
    ctx: &TailCtx,
) -> WirExpr {
    let body = tail_transition_nodes(target, args, cleanup, ctx);
    WirExpr::Control(Box::new(WirNode::Block {
        label: "__witchy_tail_escape".into(),
        result: Some(target.result_ty.clone()),
        body,
    }))
}

fn tail_transition_nodes(
    target: &TailTarget,
    args: Vec<WirExpr>,
    cleanup: &[WirLocal],
    ctx: &TailCtx,
) -> WirSeq {
    let mut body = Vec::with_capacity(args.len() * 2 + cleanup.len() + 2);
    for (arg, temp) in args.into_iter().zip(&target.temps) {
        body.push(WirNode::SetLocal { local: temp.name.clone(), value: arg });
    }
    for local in &ctx.source_bank {
        body.push(WirNode::SetLocal {
            local: local.name.clone(),
            value: default_value(&local.ty),
        });
    }
    for (param, temp) in target.params.iter().zip(&target.temps) {
        body.push(WirNode::SetLocal {
            local: param.name.clone(),
            value: WirExpr::GetLocal(temp.name.clone()),
        });
    }
    for temp in target.temps.iter().chain(cleanup) {
        body.push(WirNode::SetLocal {
            local: temp.name.clone(),
            value: default_value(&temp.ty),
        });
    }
    for local in &target.locals {
        body.push(WirNode::SetLocal {
            local: local.name.clone(),
            value: default_value(&local.ty),
        });
    }
    if let (Some(state), Some(state_local)) = (target.state, &ctx.state_local) {
        body.push(WirNode::SetLocal {
            local: state_local.clone(),
            value: WirExpr::ConstI32(state),
        });
    }
    body.push(WirNode::Br { target: ctx.loop_label.clone(), cond: None });
    body.push(WirNode::Unreachable);
    body
}

/// Match lowering leaves each selected arm as `Push(value); br $result` inside
/// nested blocks. Rewrite only those value positions, retaining the result block
/// and its type for every non-recursive arm.
fn rewrite_result_branches(seq: &mut WirSeq, target: &str, ctx: &TailCtx) -> usize {
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

fn rewrite_explicit_returns_seq(seq: &mut WirSeq, ctx: &TailCtx) -> usize {
    seq.iter_mut().map(|node| rewrite_explicit_returns_node(node, ctx)).sum()
}

fn rewrite_explicit_returns_node(node: &mut WirNode, ctx: &TailCtx) -> usize {
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

fn rewrite_explicit_returns_expr(expr: &mut WirExpr, ctx: &TailCtx) -> usize {
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
        WirExpr::StructGet { base, .. }
        | WirExpr::RefCast { value: base, .. }
        | WirExpr::RefIsNull(base) => simplify_expr(base, changed),
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
        WirExpr::StructGet { base, .. }
        | WirExpr::RefCast { value: base, .. }
        | WirExpr::RefIsNull(base) => expr_size(base),
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
