use std::collections::BTreeMap;
use witchy_syntax::ast::{Expr, Function, Item, Module, Stmt};

pub(crate) fn named_producer_edges(module: &Module, source: &crate::typeck::TypedModule,
    continuations: &BTreeMap<String, usize>, marked: &BTreeMap<usize, usize>)
    -> Result<Vec<(usize, usize)>, String>
{
    let functions = module.items.iter().filter_map(|item| match item {
        Item::Function(function) => Some((function.name.as_str(), function)), _ => None,
    }).collect::<BTreeMap<_, _>>();
    let mut edges = Vec::new();
    for function in functions.values().filter(|function| function.attributes.iter()
        .any(|attribute| attribute == witchy_syntax::suspension::FRAME_ENTRY_ATTRIBUTE))
    {
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            let Expr::Call { name, args } = expression else { return Ok(()) };
            let terminal = crate::suspension_carrier::call_family(name, "task.__terminal_producer_state_frame");
            if !terminal && !crate::suspension_carrier::call_family(name, "task.__producer_state_frame") { return Ok(()); }
            let (initial, prepare, resume, finish) = match (terminal, args.as_slice()) {
                (true, [initial, prepare, finish]) => (initial, prepare, None, finish),
                (false, [initial, prepare, resume, finish]) => (initial, prepare, Some(resume), finish),
                _ => return Err("named producer has an invalid factory arity".into()),
            };
            let Expr::Call { name: init, args: initial } = initial else { return Ok(()) };
            let Some((prepare, state)) = callback_target(prepare, function) else { return Ok(()) };
            let Expr::Lambda { params: finishing, body: finish_body, qualifiers, .. } = finish else {
                return Err("named producer has no exact unit completion callback".into());
            };
            if !matches!(finishing.as_slice(), [parameter] if parameter.ty == state.ty
                && parameter.convention == witchy_syntax::ast::Convention::Let)
                || qualifiers.once
                || !matches!(finish_body.stmts.as_slice(), [Stmt::Expr(Expr::Call { name, args })]
                    if crate::suspension_carrier::call_family(name, "task.ready_unit") && args.is_empty())
            { return Err("named producer has no exact unit completion callback".into()); }
            let resume = resume.map(|resume| callback_target(resume, function)
                .ok_or("named producer has no exact resume callback")).transpose()?;
            let initializer = functions.get(init.as_str()).ok_or("named producer has no checked initializer")?;
            let preparing = functions.get(prepare).ok_or("named producer has no checked preparation")?;
            let mut parameters = function.params.clone();
            parameters.push(state.clone());
            if !same_source(module, function, initializer) || !same_source(module, function, preparing)
                || preparing.params != parameters
                || initializer.params != function.params || initial.len() != function.params.len()
                || !initial.iter().zip(&function.params).all(|(argument, parameter)|
                    matches!(argument, Expr::Var(name) if name == &parameter.name))
            { return Err("named producer factory differs from its checked source parameter scope".into()); }
            let [Stmt::Expr(Expr::Tuple(fields))] = initializer.body.stmts.as_slice() else {
                return Err("named producer initializer is not effect-neutral state construction".into());
            };
            if !fields.iter().all(|field| matches!(field, Expr::Int(_) | Expr::Bool(_))) {
                return Err("named producer initializer is not effect-neutral state construction".into());
            }
            let mut operands = Vec::new();
            witchy_syntax::ast::visit::visit_block(&preparing.body, &mut |expression| {
                if let Expr::Call { name, args } = expression
                    && crate::suspension_carrier::call_family(name, "task.__prepare_send")
                {
                    let [operand @ Expr::Block(block)] = args.as_slice() else {
                        return Err("named producer preparation lacks its marked task operand".into());
                    };
                    let [Stmt::Let { name, value: Expr::Int(identity), .. }, Stmt::Expr(Expr::Call { name: send, args })]
                        = block.stmts.as_slice() else {
                        return Err("named producer preparation has an invalid source-task marker".into());
                    };
                    let Some(encoded) = name.strip_prefix(witchy_syntax::suspension::SOURCE_TASK_OPERAND_PREFIX) else {
                        return Err("named producer preparation has no source-task marker".into());
                    };
                    if encoded.parse::<u32>().ok().map(i64::from) != Some(*identity)
                        || !crate::suspension_carrier::call_family(send, "chan.send") || args.len() != 2
                    { return Err("named producer preparation differs from its original source send".into()); }
                    operands.push(operand as *const Expr as usize);
                }
                Ok::<_, String>(())
            })?;
            let [operand] = operands.as_slice() else {
                return Err("named producer preparation lacks one unique original source send".into());
            };
            let proof = *marked.get(operand).ok_or("named producer has no exact original source await proof")?;
            if let Some((resume, resume_state)) = resume {
                let resuming = functions.get(resume).ok_or("named producer has no checked resume")?;
                if !same_source(module, function, resuming) || state != resume_state || resuming.params != parameters
                    || continuations.get(resume) != Some(&proof)
                { return Err("named producer resume differs from its original source continuation".into()); }
                edges.push((*operand, proof));
            } else {
                if continuations.values().any(|continuation| *continuation == proof) {
                    return Err("terminal producer has a nonterminal source continuation".into());
                }
                let (item, method) = witchy_syntax::suspension::async_surface(function)
                    .ok_or("terminal producer has no original source callable")?;
                let module = witchy_syntax::suspension::async_surface_module(function)
                    .ok_or("terminal producer has no original source module")?;
                let callable = crate::async_control_flow::AsyncCallableId { module: module.to_owned(),
                    surface: witchy_syntax::async_surface::AsyncSurfaceId {
                        item: u32::try_from(item).map_err(|_| "terminal producer source item exceeds u32")?,
                        method: method.map(u32::try_from).transpose().map_err(|_| "terminal producer source method exceeds u32")?,
                    }};
                let original = crate::typed_async_lowering::typed_source_function(source, &callable)
                    .ok_or("terminal producer has no exact retained source callable")?;
                if !matches!(original.body.stmts.last(), Some(Stmt::Expr(Expr::Unary {
                    op: witchy_syntax::ast::UnOp::Await, expr,
                })) if matches!(expr.as_ref(), Expr::Call { name, args }
                    if crate::suspension_carrier::call_family(name, "chan.send") && args.len() == 2))
                { return Err("terminal producer does not complete at its original source send".into()); }
            }
            Ok::<_, String>(())
        })?;
    }
    Ok(edges)
}

fn same_source(module: &Module, left: &Function, right: &Function) -> bool {
    witchy_syntax::suspension::async_surface(left).is_some()
        && witchy_syntax::suspension::async_surface(left) == witchy_syntax::suspension::async_surface(right)
        && witchy_syntax::suspension::async_surface_module(left) == witchy_syntax::suspension::async_surface_module(right)
        && witchy_canonical_source::source::source_execution_mode(left, &module.modes)
            == witchy_canonical_source::source::source_execution_mode(right, &module.modes)
}

fn callback_target<'a>(expression: &'a Expr, factory: &Function) -> Option<(&'a str, &'a witchy_syntax::ast::Param)> {
    let Expr::Lambda { params, body, qualifiers, .. } = expression else { return None };
    let [state] = params.as_slice() else { return None };
    if state.convention != witchy_syntax::ast::Convention::Var || qualifiers.once
        || factory.params.iter().any(|parameter| parameter.name == state.name)
    { return None; }
    let [Stmt::Expr(Expr::Call { name, args })] = body.stmts.as_slice() else { return None };
    if args.len() != factory.params.len() + 1
        || !args.iter().zip(factory.params.iter().chain(std::iter::once(state))).all(|(argument, parameter)|
            matches!(argument, Expr::Var(name) if name == &parameter.name))
    { return None; }
    Some((name, state))
}
