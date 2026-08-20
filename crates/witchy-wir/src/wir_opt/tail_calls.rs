//! Proper-tail-call lowering to typed WIR state machines.

mod analysis;
mod hygiene;

pub(in crate::wir_opt) use analysis::collect_function_tail_calls;
use analysis::{
    dispatcher_results_compatible, has_single_result, indirect_signature_matches,
    strongly_connected_components,
};
#[cfg(test)]
pub(in crate::wir_opt) use hygiene::{rename_expr_locals, rename_node_locals};
use hygiene::{
    adapt_function_result_to_slot, rename_seq_locals, unique_dispatch_label,
    unique_function_name, unique_label, unique_local_name,
};

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::wir::{
    ClosureSignature, Kind, WirExpr, WirFunc, WirLocal, WirModule, WirNode, WirSeq, WirTy,
};

/// Lower recursive direct proper calls to typed state machines.
///
/// This is a semantic lowering, not an optional optimization. A multi-result
/// call participates only when local provenance proves that its complete
/// write-back/ownership envelope forwards unchanged; reconstruction remains
/// real caller-side work.
pub fn lower_direct_tail_calls(module: &mut WirModule) -> usize {
    let mut count = lower_mutual_envelope_components(module);
    count += lower_mutual_tail_components(module);
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::wir_opt) enum TailCallee {
    Direct(String),
    Indirect(ClosureSignature),
}

#[derive(Clone)]
struct IndirectPlan {
    args: Vec<WirLocal>,
    index: WirLocal,
    targets: Vec<(i32, TailTarget)>,
    dispatch_state: i32,
}

struct TailCtx {
    targets: HashMap<String, TailTarget>,
    indirect: HashMap<ClosureSignature, IndirectPlan>,
    source_bank: Vec<WirLocal>,
    reset_locals_at_loop: bool,
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
    let loop_locals = func.locals.clone();
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
        source_bank: func.params.clone(),
        reset_locals_at_loop: true,
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
    let mut loop_body = reset_local_nodes(&loop_locals);
    loop_body.extend(body);
    func.body = vec![WirNode::Loop { label: loop_label, body: loop_body }, WirNode::Unreachable];
    count
}

/// Lower a self call whose complete multi-result envelope is forwarded unchanged.
/// This covers ownership tokens and RFC-0087 `var` write-backs. The recognizer
/// follows only local-to-local copies from `CallStoreMulti` destinations to the
/// function epilogue; a computed reconstruction therefore remains non-tail.
fn lower_func_self_tail_envelope(func: &mut WirFunc) -> usize {
    if func.raw_body.is_some() || func.ret.len() < 2 {
        return 0;
    }

    let envelope_len = func.ret.len() - 1;
    if func.body.len() <= envelope_len {
        return 0;
    }
    let envelope_start = func.body.len() - envelope_len;
    let Some(envelope_locals) = forwarded_envelope_locals(func) else { return 0 };

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
    let loop_locals = func.locals.clone();
    let ctx = TailCtx {
        targets: HashMap::from([(func.name.clone(), target.clone())]),
        indirect: HashMap::new(),
        source_bank: func.params.clone(),
        reset_locals_at_loop: true,
        state_local: None,
        loop_label: loop_label.clone(),
    };
    let mut body = func.body.clone();
    let primary_index = envelope_start - 1;
    let has_normal_tail = matches!(
        body[primary_index],
        WirNode::Push(_) | WirNode::Return(Some(_))
    );
    let normal_count = match &mut body[primary_index] {
        WirNode::Push(expr) | WirNode::Return(Some(expr)) => {
            rewrite_forwarded_envelope_expr(expr, &func.name, &envelope_locals, &target, &ctx)
        }
        _ => 0,
    };
    let normal_exit = if has_normal_tail {
        let result_local = WirLocal {
            name: unique_local_name(func, "__witchy_tail_result"),
            ty: func.ret[0].clone(),
        };
        let result_value =
            match std::mem::replace(&mut body[primary_index], WirNode::Unreachable) {
                WirNode::Push(value) | WirNode::Return(Some(value)) => value,
                _ => unreachable!("the envelope primary was checked above"),
            };
        body[primary_index] = WirNode::SetLocal {
            local: result_local.name.clone(),
            value: result_value,
        };
        body.truncate(envelope_start);
        let exit_label = unique_label(func, "__witchy_tail_exit");
        body.push(WirNode::Br { target: exit_label.clone(), cond: None });
        Some((result_local, exit_label))
    } else {
        body.truncate(envelope_start);
        None
    };

    let count = normal_count
        + rewrite_forwarded_envelope_returns_seq(
            &mut body,
            &func.name,
            &envelope_locals,
            &target,
            &ctx,
        );
    if count == 0 {
        return 0;
    }

    let mut loop_body = reset_local_nodes(&loop_locals);
    loop_body.extend(body);
    func.locals.extend(temps);
    if let Some((result_local, exit_label)) = normal_exit {
        func.locals.push(result_local.clone());
        let mut final_body = vec![WirNode::Block {
            label: exit_label,
            result: None,
            body: vec![WirNode::Loop { label: loop_label, body: loop_body }, WirNode::Unreachable],
        }];
        final_body.push(WirNode::Push(WirExpr::GetLocal(result_local.name)));
        final_body.extend(
            envelope_locals
                .into_iter()
                .map(|local| WirNode::Push(WirExpr::GetLocal(local))),
        );
        func.body = final_body;
    } else {
        func.body = vec![WirNode::Loop { label: loop_label, body: loop_body }, WirNode::Unreachable];
    }
    count
}

fn forwarded_envelope_locals(func: &WirFunc) -> Option<Vec<String>> {
    let envelope_len = func.ret.len().checked_sub(1)?;
    if envelope_len == 0 || func.body.len() <= envelope_len {
        return None;
    }
    let envelope_start = func.body.len() - envelope_len;
    func.body[envelope_start..]
        .iter()
        .zip(&func.ret[1..])
        .map(|(node, result)| {
            let WirNode::Push(WirExpr::GetLocal(local)) = node else { return None };
            let local_ty = func
                .params
                .iter()
                .chain(&func.locals)
                .find(|candidate| candidate.name == *local)
                .map(|candidate| candidate.ty.kind())?;
            (local_ty == result.kind()).then(|| local.clone())
        })
        .collect()
}

fn rewrite_forwarded_envelope_returns_seq(
    seq: &mut WirSeq,
    function: &str,
    envelope_locals: &[String],
    target: &TailTarget,
    ctx: &TailCtx,
) -> usize {
    let mut count = seq
        .iter_mut()
        .map(|node| {
            rewrite_forwarded_envelope_returns_node(
                node,
                function,
                envelope_locals,
                target,
                ctx,
            )
        })
        .sum();

    let mut index = 0;
    while index < seq.len() {
        if !matches!(seq[index], WirNode::Return(None))
            || index < envelope_locals.len() + 1
        {
            index += 1;
            continue;
        }
        let value_index = index - envelope_locals.len() - 1;
        let outputs_match = seq[value_index + 1..index]
            .iter()
            .zip(envelope_locals)
            .all(|(node, expected)| {
                matches!(node, WirNode::Push(WirExpr::GetLocal(local)) if local == expected)
            });
        if !outputs_match {
            index += 1;
            continue;
        }
        let args = match &seq[value_index] {
            WirNode::Push(WirExpr::Seq(inner)) => forwarded_envelope_args(
                inner,
                function,
                envelope_locals,
                target.params.len(),
            ),
            _ => None,
        };
        let Some(args) = args else {
            index += 1;
            continue;
        };
        let transition = tail_transition_nodes(target, args, &[], ctx);
        let transition_len = transition.len();
        seq.splice(value_index..=index, transition);
        count += 1;
        index = value_index + transition_len;
    }
    count
}

fn rewrite_forwarded_envelope_returns_node(
    node: &mut WirNode,
    function: &str,
    envelope_locals: &[String],
    target: &TailTarget,
    ctx: &TailCtx,
) -> usize {
    match node {
        WirNode::If { then_, els, .. } => {
            rewrite_forwarded_envelope_returns_seq(
                then_,
                function,
                envelope_locals,
                target,
                ctx,
            ) + rewrite_forwarded_envelope_returns_seq(
                els,
                function,
                envelope_locals,
                target,
                ctx,
            )
        }
        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            rewrite_forwarded_envelope_returns_seq(
                body,
                function,
                envelope_locals,
                target,
                ctx,
            )
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value)) => rewrite_forwarded_envelope_returns_expr(
            value,
            function,
            envelope_locals,
            target,
            ctx,
        ),
        _ => 0,
    }
}

fn rewrite_forwarded_envelope_returns_expr(
    expr: &mut WirExpr,
    function: &str,
    envelope_locals: &[String],
    target: &TailTarget,
    ctx: &TailCtx,
) -> usize {
    match expr {
        WirExpr::Control(node) => rewrite_forwarded_envelope_returns_node(
            node,
            function,
            envelope_locals,
            target,
            ctx,
        ),
        WirExpr::Seq(seq) => rewrite_forwarded_envelope_returns_seq(
            seq,
            function,
            envelope_locals,
            target,
            ctx,
        ),
        _ => 0,
    }
}

fn rewrite_forwarded_envelope_expr(
    expr: &mut WirExpr,
    function: &str,
    envelope_locals: &[String],
    target: &TailTarget,
    ctx: &TailCtx,
) -> usize {
    match expr {
        WirExpr::Seq(seq) => {
            if let Some(args) = forwarded_envelope_args(
                seq,
                function,
                envelope_locals,
                target.params.len(),
            ) {
                *expr = tail_transition_expr(target, args, &[], ctx);
                1
            } else {
                rewrite_forwarded_envelope_seq(seq, function, envelope_locals, target, ctx)
            }
        }
        WirExpr::Control(node) => match node.as_mut() {
            WirNode::If { then_, els, result: Some(_), .. } => {
                rewrite_forwarded_envelope_seq(
                    then_,
                    function,
                    envelope_locals,
                    target,
                    ctx,
                ) + rewrite_forwarded_envelope_seq(
                    els,
                    function,
                    envelope_locals,
                    target,
                    ctx,
                )
            }
            WirNode::Block { body, result: Some(_), .. } => {
                rewrite_forwarded_envelope_seq(
                    body,
                    function,
                    envelope_locals,
                    target,
                    ctx,
                )
            }
            _ => 0,
        },
        _ => 0,
    }
}

fn rewrite_forwarded_envelope_seq(
    seq: &mut WirSeq,
    function: &str,
    envelope_locals: &[String],
    target: &TailTarget,
    ctx: &TailCtx,
) -> usize {
    let Some(value_index) = seq.iter().rposition(|node| matches!(node, WirNode::Push(_))) else {
        return 0;
    };
    if !seq[value_index + 1..]
        .iter()
        .all(|node| discardable_tail_reset(node, envelope_locals, ctx))
    {
        return 0;
    }

    let args = match &seq[value_index] {
        WirNode::Push(WirExpr::Seq(inner)) => forwarded_envelope_args(
            inner,
            function,
            envelope_locals,
            target.params.len(),
        ),
        _ => None,
    };
    if let Some(args) = args {
        seq.truncate(value_index);
        seq.extend(tail_transition_nodes(target, args, &[], ctx));
        return 1;
    }

    match &mut seq[value_index] {
        WirNode::Push(expr) => {
            rewrite_forwarded_envelope_expr(expr, function, envelope_locals, target, ctx)
        }
        _ => 0,
    }
}

fn forwarded_envelope_args(
    seq: &WirSeq,
    function: &str,
    envelope_locals: &[String],
    parameter_count: usize,
) -> Option<Vec<WirExpr>> {
    let WirNode::CallStoreMulti { func, args, dests } = seq.first()? else { return None };
    if func != function
        || args.len() != parameter_count
        || dests.len() != envelope_locals.len() + 1
    {
        return None;
    }

    let mut origins: HashMap<String, usize> = dests
        .iter()
        .enumerate()
        .map(|(index, local)| (local.clone(), index))
        .collect();
    for (index, node) in seq[1..].iter().enumerate() {
        let is_last = index + 2 == seq.len();
        match node {
            WirNode::SetLocal { local, value: WirExpr::GetLocal(source) } => {
                let origin = origins.get(source).copied();
                origins.remove(local);
                if let Some(origin) = origin {
                    origins.insert(local.clone(), origin);
                }
            }
            // (RFC-0110 criterion 6/9) The direct-storage `var` lowering fires an
            // ownership counter increment in the write-back epilogue. It is a pure
            // side-effect on a synthesized global — orthogonal to the envelope
            // dataflow this recognizer tracks — so skip it transparently. When the
            // tail transition fires, the epilogue (counter included) is replaced by
            // loop-arg forwarding: no direct-storage commit occurs in the loop form,
            // so dropping the counter there is correct and matches the lever-off
            // oracle, which also counts zero.
            WirNode::SetGlobal { global, .. }
                if global == "__witchy_direct_storage_var_accesses" => {}
            WirNode::Push(WirExpr::GetLocal(local))
                if is_last && origins.get(local) == Some(&0) => {}
            _ => return None,
        }
    }
    if envelope_locals
        .iter()
        .enumerate()
        .all(|(index, local)| origins.get(local) == Some(&(index + 1)))
    {
        Some(args.clone())
    } else {
        None
    }
}

fn discardable_tail_reset(node: &WirNode, envelope_locals: &[String], ctx: &TailCtx) -> bool {
    let WirNode::SetLocal { local, value } = node else { return false };
    !envelope_locals.contains(local)
        && ctx.source_bank.iter().any(|candidate| candidate.name == *local)
        && match value {
            WirExpr::ConstI32(0) | WirExpr::ConstI64(0) => true,
            WirExpr::ConstF64(value) => *value == 0.0,
            _ => false,
        }
}

/// Form SCCs only from multi-result edges whose destination provenance reaches
/// every source epilogue local unchanged.
fn lower_mutual_envelope_components(module: &mut WirModule) -> usize {
    let function_count = module.funcs.len();
    let candidates: Vec<_> = module
        .funcs
        .iter()
        .enumerate()
        .filter(|(_, function)| {
            function.raw_body.is_none() && forwarded_envelope_locals(function).is_some()
        })
        .map(|(index, _)| index)
        .collect();
    let mut graph = vec![Vec::new(); function_count];
    for &source in &candidates {
        for &target in &candidates {
            if module.funcs[source].ret == module.funcs[target].ret
                && has_forwarded_envelope_edge(&module.funcs[source], &module.funcs[target])
            {
                graph[source].push(target);
            }
        }
    }

    let components = strongly_connected_components(&graph);
    let mut dispatchers = Vec::new();
    let mut count = 0;
    for component in components.into_iter().filter(|component| component.len() > 1) {
        let originals: Vec<_> = component
            .iter()
            .map(|index| module.funcs[*index].clone())
            .collect();
        if originals
            .windows(2)
            .any(|pair| pair[0].ret != pair[1].ret)
        {
            continue;
        }
        let dispatcher_name = unique_function_name(
            module,
            &dispatchers,
            &format!("__witchy_tail_envelope_scc_{}", dispatchers.len()),
        );
        let (dispatcher, rewritten) =
            build_envelope_dispatcher(&dispatcher_name, &originals);
        if rewritten == 0 {
            continue;
        }
        for ((index, original), state) in component.iter().zip(&originals).zip(0i32..) {
            module.funcs[*index] =
                envelope_entry_wrapper(original, &dispatcher_name, state, &originals);
        }
        dispatchers.push(dispatcher);
        count += rewritten;
    }
    module.funcs.extend(dispatchers);
    count
}

fn has_forwarded_envelope_edge(source: &WirFunc, target: &WirFunc) -> bool {
    let Some(envelope_locals) = forwarded_envelope_locals(source) else { return false };
    let envelope_start = source.body.len() - envelope_locals.len();
    let mut body = source.body.clone();
    let temps: Vec<_> = target
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| WirLocal {
            name: format!("__witchy_tail_probe_{index}"),
            ty: param.ty.clone(),
        })
        .collect();
    let tail_target = TailTarget {
        state: None,
        params: target.params.clone(),
        temps,
        locals: target.locals.clone(),
        result_ty: target.ret[0].clone(),
    };
    let ctx = TailCtx {
        targets: HashMap::from([(target.name.clone(), tail_target.clone())]),
        indirect: HashMap::new(),
        source_bank: source.params.iter().chain(&source.locals).cloned().collect(),
        reset_locals_at_loop: false,
        state_local: None,
        loop_label: "__witchy_tail_probe_loop".into(),
    };
    let primary_index = envelope_start - 1;
    let normal = match &mut body[primary_index] {
        WirNode::Push(expr) | WirNode::Return(Some(expr)) => rewrite_forwarded_envelope_expr(
            expr,
            &target.name,
            &envelope_locals,
            &tail_target,
            &ctx,
        ),
        _ => 0,
    };
    normal
        + rewrite_forwarded_envelope_returns_seq(
            &mut body,
            &target.name,
            &envelope_locals,
            &tail_target,
            &ctx,
        )
        > 0
}

/// Build a resultless loop/exit region, then re-emit the shared typed envelope
/// after the region. Explicit multi-value returns can leave the dispatcher
/// directly; fallthrough exits stage their values in `result_locals` first.
fn build_envelope_dispatcher(name: &str, functions: &[WirFunc]) -> (WirFunc, usize) {
    let state_local = "__witchy_tail_state".to_string();
    let mut params = vec![WirLocal { name: state_local.clone(), ty: WirTy::Bool }];
    let mut locals = Vec::new();
    let mut bodies = Vec::new();
    let mut envelopes = HashMap::new();
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
        let envelope: Vec<String> = forwarded_envelope_locals(function)
            .expect("dispatcher member has a complete envelope")
            .into_iter()
            .map(|local| renames.get(&local).cloned().unwrap_or(local))
            .collect();
        envelopes.insert(function.name.clone(), envelope);
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

    let result_locals: Vec<_> = functions[0]
        .ret
        .iter()
        .enumerate()
        .map(|(index, ty)| WirLocal {
            name: format!("__witchy_tail_result_{index}"),
            ty: ty.clone(),
        })
        .collect();
    locals.extend(result_locals.clone());
    let loop_label = unique_dispatch_label(functions, "__witchy_tail_dispatch_loop");
    let exit_label = unique_dispatch_label(functions, "__witchy_tail_dispatch_exit");
    let mut count = 0;

    for (function, body) in functions.iter().zip(&mut bodies) {
        let envelope = envelopes
            .get(&function.name)
            .expect("dispatcher member has renamed envelope");
        let envelope_start = body.len() - envelope.len();
        let primary_index = envelope_start - 1;
        let source = targets.get(&function.name).expect("dispatcher source bank");
        let ctx = TailCtx {
            targets: targets.clone(),
            indirect: HashMap::new(),
            source_bank: source.params.iter().chain(&source.locals).cloned().collect(),
            reset_locals_at_loop: false,
            state_local: Some(state_local.clone()),
            loop_label: loop_label.clone(),
        };
        for (target_name, target) in &targets {
            count += match &mut body[primary_index] {
                WirNode::Push(expr) | WirNode::Return(Some(expr)) => {
                    rewrite_forwarded_envelope_expr(
                        expr,
                        target_name,
                        envelope,
                        target,
                        &ctx,
                    )
                }
                _ => 0,
            };
        }
        body[primary_index] = match std::mem::replace(
            &mut body[primary_index],
            WirNode::Unreachable,
        ) {
            WirNode::Push(value) | WirNode::Return(Some(value)) => WirNode::SetLocal {
                local: result_locals[0].name.clone(),
                value,
            },
            other => other,
        };
        body.truncate(envelope_start);
        for (result, source_local) in result_locals[1..].iter().zip(envelope) {
            body.push(WirNode::SetLocal {
                local: result.name.clone(),
                value: WirExpr::GetLocal(source_local.clone()),
            });
        }
        body.push(WirNode::Br { target: exit_label.clone(), cond: None });
        for (target_name, target) in &targets {
            count += rewrite_forwarded_envelope_returns_seq(
                body,
                target_name,
                envelope,
                target,
                &ctx,
            );
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
    let mut body = vec![WirNode::Block {
        label: exit_label,
        result: None,
        body: vec![WirNode::Loop { label: loop_label, body: selection }, WirNode::Unreachable],
    }];
    body.extend(
        result_locals
            .iter()
            .map(|local| WirNode::Push(WirExpr::GetLocal(local.name.clone()))),
    );
    (
        WirFunc {
            name: name.to_string(),
            params,
            ret: functions[0].ret.clone(),
            locals,
            body,
            raw_body: None,
        },
        count,
    )
}

fn envelope_entry_wrapper(
    original: &WirFunc,
    dispatcher: &str,
    state: i32,
    functions: &[WirFunc],
) -> WirFunc {
    let mut args = vec![WirExpr::ConstI32(state)];
    for function in functions {
        if function.name == original.name {
            args.extend(original.params.iter().map(|param| WirExpr::GetLocal(param.name.clone())));
        } else {
            args.extend(function.params.iter().map(|param| default_value(&param.ty)));
        }
    }
    let locals: Vec<_> = original
        .ret
        .iter()
        .enumerate()
        .map(|(index, ty)| WirLocal {
            name: format!("__witchy_tail_wrapper_result_{index}"),
            ty: ty.clone(),
        })
        .collect();
    let mut body = vec![WirNode::CallStoreMulti {
        func: dispatcher.to_string(),
        args,
        dests: locals.iter().map(|local| local.name.clone()).collect(),
    }];
    body.extend(
        locals
            .iter()
            .map(|local| WirNode::Push(WirExpr::GetLocal(local.name.clone()))),
    );
    WirFunc {
        name: original.name.clone(),
        params: original.params.clone(),
        ret: original.ret.clone(),
        locals,
        body,
        raw_body: None,
    }
}

fn lower_mutual_tail_components(module: &mut WirModule) -> usize {
    let function_count = module.funcs.len();
    let by_name: HashMap<_, _> = module
        .funcs
        .iter()
        .enumerate()
        .filter(|(_, function)| function.raw_body.is_none() && has_single_result(function))
        .map(|(index, function)| (function.name.clone(), index))
        .collect();
    let mut graph = vec![Vec::new(); function_count];
    let mut has_indirect_tail = vec![false; function_count];
    for (index, function) in module.funcs.iter().enumerate() {
        if function.raw_body.is_some() || !has_single_result(function) {
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
                            indirect_signature_matches(&module.funcs[*target], &signature)
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
        if !dispatcher_results_compatible(&originals) {
            continue;
        }
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
        let (dispatcher, rewritten, entry_targets) =
            build_tail_dispatcher(&dispatcher_name, &originals, &table_slots);
        if rewritten == 0 {
            continue;
        }
        let dispatcher_kind = dispatcher.ret[0].kind();
        for ((index, original), state) in component.iter().zip(&originals).zip(0i32..) {
            module.funcs[*index] = tail_entry_wrapper(
                original,
                &dispatcher_name,
                state,
                &dispatcher.params[1..],
                dispatcher_kind,
                entry_targets
                    .get(&original.name)
                    .expect("every dispatcher member has an entry target"),
            );
        }
        dispatchers.push(dispatcher);
        count += rewritten;
    }
    module.funcs.extend(dispatchers);
    count
}

fn carrier_ty(kind: Kind) -> WirTy {
    match kind {
        Kind::I32 => WirTy::Bool,
        Kind::I64 => WirTy::Int,
        Kind::F64 => WirTy::Float,
        Kind::ExternRef => WirTy::Extern,
        Kind::StructRef => WirTy::StructRef,
        Kind::AnyRef => WirTy::AnyRef,
        Kind::GcRef(id) => WirTy::GcRef(id),
    }
}

fn build_tail_dispatcher(
    name: &str,
    functions: &[WirFunc],
    table_slots: &HashMap<String, i32>,
) -> (WirFunc, usize, HashMap<String, TailTarget>) {
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
    // Only one source function is active in a dispatcher iteration. Reuse one
    // typed parameter/local/temp bank sized to the largest member instead of
    // concatenating every member's bank. Besides reducing code size, this keeps
    // large closure-table SCCs below WebAssembly's 1,000-parameter and
    // 50,000-local implementation limits. Exact reference kinds remain separate
    // keys, so pooling never erases a capability or GC type.
    let param_bank = pooled_local_bank(
        functions.iter().map(|function| function.params.as_slice()),
        "__witchy_tail_p",
    );
    let local_bank = pooled_local_bank(
        functions.iter().map(|function| function.locals.as_slice()),
        "__witchy_tail_l",
    );
    let temp_bank = clone_local_bank(&param_bank, "__witchy_tail_arg");
    let shared_params = flatten_local_bank(&param_bank);
    let mut params = vec![WirLocal { name: state_local.clone(), ty: WirTy::Bool }];
    params.extend(shared_params.iter().cloned());
    let shared_locals = flatten_local_bank(&local_bank);
    let mut locals = shared_locals.clone();
    locals.extend(flatten_local_bank(&temp_bank));
    let mut bodies = Vec::new();
    let mut targets = HashMap::new();

    for (state, function) in (0i32..).zip(functions) {
        let renamed_params = assign_local_bank(&function.params, &param_bank);
        let renamed_locals = assign_local_bank(&function.locals, &local_bank);
        let temps = assign_local_bank(&function.params, &temp_bank);
        let renames: HashMap<_, _> = function
            .params
            .iter()
            .zip(&renamed_params)
            .chain(function.locals.iter().zip(&renamed_locals))
            .map(|(old, new)| (old.name.clone(), new.name.clone()))
            .collect();
        let mut body = function.body.clone();
        rename_seq_locals(&mut body, &renames);
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
                if function.ret.len() == 1 && signature.results.len() == 1 =>
            {
                Some(signature)
            }
            _ => None,
        }));
    }
    let mut indirect_signatures: Vec<_> = indirect_signatures.into_iter().collect();
    indirect_signatures.sort();
    let mut indirect = HashMap::new();
    let mut next_dispatch_state = i32::try_from(functions.len())
        .expect("a tail-call component cannot contain more than i32::MAX functions");
    for (plan_index, signature) in indirect_signatures.into_iter().enumerate() {
        let args: Vec<_> = signature
            .params
            .iter()
            .enumerate()
            .map(|(index, kind)| WirLocal {
                name: format!("__witchy_tail_indirect_{plan_index}_arg_{index}"),
                ty: carrier_ty(*kind),
            })
            .collect();
        let index = WirLocal {
            name: format!("__witchy_tail_indirect_{plan_index}_index"),
            ty: WirTy::Bool,
        };
        let mut plan_targets: Vec<_> = functions
            .iter()
            .filter(|function| indirect_signature_matches(function, &signature))
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
            indirect.insert(
                signature,
                IndirectPlan {
                    args,
                    index,
                    targets: plan_targets,
                    dispatch_state: next_dispatch_state,
                },
            );
            next_dispatch_state = next_dispatch_state
                .checked_add(1)
                .expect("a tail-call dispatcher cannot contain more than i32::MAX states");
        }
    }

    let loop_label = unique_dispatch_label(functions, "__witchy_tail_dispatch_loop");
    let mut count = 0;
    for (function, body) in functions.iter().zip(&mut bodies) {
        let ctx = TailCtx {
            targets: targets.clone(),
            indirect: indirect.clone(),
            source_bank: shared_params.clone(),
            reset_locals_at_loop: true,
            state_local: Some(state_local.clone()),
            loop_label: loop_label.clone(),
        };
        count += rewrite_explicit_returns_seq(body, &ctx);
        count += rewrite_function_tail(body, &ctx);
        if function.ret[0].kind() != dispatcher_result.kind() {
            adapt_function_result_to_slot(body, function.ret[0].kind());
        }
    }

    // Route every indirect proper edge through one dispatcher state per exact
    // callable signature. Previously each call site expanded the complete
    // table-target choice and every target-bank transition in place. In a
    // closure-heavy module that made the dispatcher quadratic in call sites x
    // possible targets and could exceed the WebAssembly per-function byte
    // limit. The call site now only stages its operands and selects this shared
    // state; the target choice and bank transfer are emitted once here.
    let shared_ctx = TailCtx {
        targets: targets.clone(),
        indirect: indirect.clone(),
        source_bank: Vec::new(),
        reset_locals_at_loop: true,
        state_local: Some(state_local.clone()),
        loop_label: loop_label.clone(),
    };
    let mut shared_plans: Vec<_> = indirect.values().cloned().collect();
    shared_plans.sort_by_key(|plan| plan.dispatch_state);
    for plan in shared_plans {
        debug_assert_eq!(
            usize::try_from(plan.dispatch_state).ok(),
            Some(bodies.len()),
            "shared indirect states must follow the source-function states densely",
        );
        bodies.push(indirect_dispatch_body(&plan, &dispatcher_result, &shared_ctx));
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
    let mut loop_body: WirSeq = shared_locals
        .iter()
        .map(|local| WirNode::SetLocal {
            local: local.name.clone(),
            value: default_value(&local.ty),
        })
        .collect();
    loop_body.extend(selection);
    let dispatcher = WirFunc {
        name: name.to_string(),
        params,
        ret: vec![dispatcher_result],
        locals,
        body: vec![WirNode::Loop { label: loop_label, body: loop_body }, WirNode::Unreachable],
        raw_body: None,
    };
    (dispatcher, count, targets)
}

fn pooled_local_bank<'a>(
    groups: impl Iterator<Item = &'a [WirLocal]>,
    prefix: &str,
) -> BTreeMap<Kind, Vec<WirLocal>> {
    let mut maxima = BTreeMap::<Kind, usize>::new();
    for group in groups {
        let mut counts = HashMap::<Kind, usize>::new();
        for local in group {
            *counts.entry(local.ty.kind()).or_default() += 1;
        }
        for (kind, count) in counts {
            maxima
                .entry(kind)
                .and_modify(|maximum| *maximum = (*maximum).max(count))
                .or_insert(count);
        }
    }
    maxima
        .into_iter()
        .enumerate()
        .map(|(kind_index, (kind, count))| {
            let locals = (0..count)
                .map(|index| WirLocal {
                    name: format!("{prefix}_{kind_index}_{index}"),
                    ty: carrier_ty(kind),
                })
                .collect();
            (kind, locals)
        })
        .collect()
}

fn clone_local_bank(
    source: &BTreeMap<Kind, Vec<WirLocal>>,
    prefix: &str,
) -> BTreeMap<Kind, Vec<WirLocal>> {
    source
        .iter()
        .enumerate()
        .map(|(kind_index, (&kind, locals))| {
            let locals = locals
                .iter()
                .enumerate()
                .map(|(index, _)| WirLocal {
                    name: format!("{prefix}_{kind_index}_{index}"),
                    ty: carrier_ty(kind),
                })
                .collect();
            (kind, locals)
        })
        .collect()
}

fn flatten_local_bank(bank: &BTreeMap<Kind, Vec<WirLocal>>) -> Vec<WirLocal> {
    bank.values().flatten().cloned().collect()
}

fn assign_local_bank(
    source: &[WirLocal],
    bank: &BTreeMap<Kind, Vec<WirLocal>>,
) -> Vec<WirLocal> {
    let mut offsets = HashMap::<Kind, usize>::new();
    source
        .iter()
        .map(|local| {
            let kind = local.ty.kind();
            let offset = offsets.entry(kind).or_default();
            let assigned = bank
                .get(&kind)
                .and_then(|locals| locals.get(*offset))
                .cloned()
                .expect("a pooled local bank covers every member-local kind");
            *offset += 1;
            assigned
        })
        .collect()
}

fn tail_entry_wrapper(
    original: &WirFunc,
    dispatcher: &str,
    state: i32,
    dispatcher_params: &[WirLocal],
    dispatcher_kind: Kind,
    target: &TailTarget,
) -> WirFunc {
    let mut args = vec![WirExpr::ConstI32(state)];
    let original_by_slot: HashMap<_, _> = target
        .params
        .iter()
        .zip(&original.params)
        .map(|(slot, original)| (slot.name.as_str(), original))
        .collect();
    for slot in dispatcher_params {
        args.push(match original_by_slot.get(slot.name.as_str()) {
            Some(original) => WirExpr::GetLocal(original.name.clone()),
            None => default_value(&slot.ty),
        });
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
        kind @ (Kind::ExternRef | Kind::StructRef | Kind::AnyRef | Kind::GcRef(_)) => {
            WirExpr::RefNull(kind)
        }
    }
}

fn rewrite_function_tail(seq: &mut WirSeq, ctx: &TailCtx) -> usize {
    let Some(last) = seq.last_mut() else { return 0 };
    match last {
        WirNode::Source { body, .. } => rewrite_function_tail(body, ctx),
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
            WirNode::Source { body, .. } => rewrite_tail_value_seq(body, ctx),
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
        WirExpr::CallIndirect { signature, args, index }
            if ctx.indirect.contains_key(signature) =>
        {
            let result_ty = carrier_ty(signature.results[0]);
            let plan = ctx.indirect.get(signature).cloned().expect("guarded indirect plan");
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
            for local in &ctx.source_bank {
                seq.push(WirNode::SetLocal {
                    local: local.name.clone(),
                    value: default_value(&local.ty),
                });
            }
            seq.push(WirNode::SetLocal {
                local: ctx
                    .state_local
                    .clone()
                    .expect("an indirect dispatcher plan belongs to a state machine"),
                value: WirExpr::ConstI32(plan.dispatch_state),
            });
            seq.push(WirNode::Br { target: ctx.loop_label.clone(), cond: None });
            seq.push(WirNode::Unreachable);
            *expr = WirExpr::Control(Box::new(WirNode::Block {
                label: "__witchy_tail_indirect_escape".into(),
                result: Some(result_ty),
                body: seq,
            }));
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

fn indirect_dispatch_body(
    plan: &IndirectPlan,
    dispatcher_result: &WirTy,
    ctx: &TailCtx,
) -> WirSeq {
    let signature_result = plan
        .targets
        .first()
        .map(|(_, target)| target.result_ty.kind())
        .expect("an indirect dispatcher plan has at least one in-component target");
    let fallback = WirExpr::CallIndirect {
        signature: ClosureSignature {
            params: plan.args.iter().map(|arg| arg.ty.kind()).collect(),
            results: vec![signature_result],
        },
        args: plan.args.iter().map(|arg| WirExpr::GetLocal(arg.name.clone())).collect(),
        index: Box::new(WirExpr::GetLocal(plan.index.name.clone())),
    };
    let fallback = if signature_result == dispatcher_result.kind() {
        fallback
    } else {
        debug_assert_eq!(dispatcher_result.kind(), Kind::I64);
        WirExpr::ToSlot(Box::new(fallback), signature_result)
    };
    let mut choice = vec![WirNode::Return(Some(fallback))];
    let mut cleanup = plan.args.clone();
    cleanup.push(plan.index.clone());
    for (table_index, target) in plan.targets.iter().rev() {
        choice = vec![WirNode::If {
            cond: WirExpr::Binary {
                op: crate::wir::BinOp::Eq,
                kind: Kind::I32,
                lhs: Box::new(WirExpr::GetLocal(plan.index.name.clone())),
                rhs: Box::new(WirExpr::ConstI32(*table_index)),
            },
            then_: tail_transition_nodes(
                target,
                plan.args.iter().map(|arg| WirExpr::GetLocal(arg.name.clone())).collect(),
                &cleanup,
                ctx,
            ),
            els: choice,
            result: None,
        }];
    }
    choice
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
    if !ctx.reset_locals_at_loop {
        for local in &target.locals {
            body.push(WirNode::SetLocal {
                local: local.name.clone(),
                value: default_value(&local.ty),
            });
        }
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

fn reset_local_nodes(locals: &[WirLocal]) -> WirSeq {
    locals
        .iter()
        .map(|local| WirNode::SetLocal {
            local: local.name.clone(),
            value: default_value(&local.ty),
        })
        .collect()
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
            WirNode::Source { body, .. } => rewrite_result_branches(body, target, ctx),
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
        WirNode::Source { body, .. } => rewrite_explicit_returns_seq(body, ctx),
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
        WirNode::ArraySet { array, index, value, .. } => {
            rewrite_explicit_returns_expr(array, ctx)
                + rewrite_explicit_returns_expr(index, ctx)
                + rewrite_explicit_returns_expr(value, ctx)
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
        | WirExpr::RefCastNullable { value: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner) => rewrite_explicit_returns_expr(inner, ctx),
        WirExpr::Binary { lhs, rhs, .. } => {
            rewrite_explicit_returns_expr(lhs, ctx) + rewrite_explicit_returns_expr(rhs, ctx)
        }
        WirExpr::Call { args, .. }
        | WirExpr::CallHost { args, .. }
        | WirExpr::StructNew { args, .. } => {
            args.iter_mut().map(|arg| rewrite_explicit_returns_expr(arg, ctx)).sum()
        }
        WirExpr::ArrayNewFixed { items: args, .. } => {
            args.iter_mut().map(|arg| rewrite_explicit_returns_expr(arg, ctx)).sum()
        }
        WirExpr::ArrayNew { value, len, .. } => {
            rewrite_explicit_returns_expr(value, ctx)
                + rewrite_explicit_returns_expr(len, ctx)
        }
        WirExpr::ArrayGet { array, index, .. } => {
            rewrite_explicit_returns_expr(array, ctx)
                + rewrite_explicit_returns_expr(index, ctx)
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
