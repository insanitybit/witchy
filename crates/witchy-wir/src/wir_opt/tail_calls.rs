//! Proper-tail-call lowering to typed WIR state machines.

use std::collections::{HashMap, HashSet};

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
pub(super) enum TailCallee {
    Direct(String),
    Indirect(ClosureSignature),
}

#[derive(Clone)]
struct IndirectPlan {
    args: Vec<WirLocal>,
    index: WirLocal,
    targets: Vec<(i32, TailTarget)>,
}

struct TailCtx {
    targets: HashMap<String, TailTarget>,
    indirect: HashMap<ClosureSignature, IndirectPlan>,
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
    let ctx = TailCtx {
        targets: HashMap::from([(func.name.clone(), target.clone())]),
        indirect: HashMap::new(),
        source_bank: func.params.iter().chain(&func.locals).cloned().collect(),
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

    func.locals.extend(temps);
    if let Some((result_local, exit_label)) = normal_exit {
        func.locals.push(result_local.clone());
        let mut final_body = vec![WirNode::Block {
            label: exit_label,
            result: None,
            body: vec![WirNode::Loop { label: loop_label, body }, WirNode::Unreachable],
        }];
        final_body.push(WirNode::Push(WirExpr::GetLocal(result_local.name)));
        final_body.extend(
            envelope_locals
                .into_iter()
                .map(|local| WirNode::Push(WirExpr::GetLocal(local))),
        );
        func.body = final_body;
    } else {
        func.body = vec![WirNode::Loop { label: loop_label, body }, WirNode::Unreachable];
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

fn has_single_result(function: &WirFunc) -> bool {
    function.ret.len() == 1
}

fn dispatcher_results_compatible(functions: &[WirFunc]) -> bool {
    let Some(first) = functions.first().and_then(|function| function.ret.first()) else {
        return false;
    };
    functions.iter().all(|function| {
        function.ret.first().is_some_and(|result| result.kind() == first.kind())
    }) || functions.iter().all(slot_adaptable_result)
}

fn indirect_signature_matches(function: &WirFunc, signature: &ClosureSignature) -> bool {
    function.params.iter().map(|param| param.ty.kind()).eq(signature.params.iter().copied())
        && function.ret.iter().map(|result| result.kind()).eq(signature.results.iter().copied())
}

fn carrier_ty(kind: Kind) -> WirTy {
    match kind {
        Kind::I32 => WirTy::Bool,
        Kind::I64 => WirTy::Int,
        Kind::F64 => WirTy::Float,
        Kind::ExternRef => WirTy::Extern,
        Kind::StructRef => WirTy::StructRef,
        Kind::GcRef(id) => WirTy::GcRef(id),
    }
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

pub(super) fn collect_function_tail_calls(seq: &WirSeq, out: &mut HashSet<TailCallee>) {
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

pub(super) fn rename_node_locals(node: &mut WirNode, renames: &HashMap<String, String>) {
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

pub(super) fn rename_expr_locals(expr: &mut WirExpr, renames: &HashMap<String, String>) {
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

fn unique_label(func: &WirFunc, stem: &str) -> String {
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
        WirExpr::CallIndirect { signature, args, index }
            if ctx.indirect.contains_key(signature) =>
        {
            let signature = signature.clone();
            let result_ty = carrier_ty(signature.results[0]);
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
                signature,
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
                    result: Some(result_ty.clone()),
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
