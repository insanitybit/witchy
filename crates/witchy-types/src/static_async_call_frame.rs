use std::collections::BTreeMap;
use witchy_syntax::ast::{Convention, Expr, Function, Item, Module, Stmt, UnOp};

pub(crate) const OPERAND_PREFIX: &str = "__source_static_call_operand_";

pub(crate) fn caller_edges(module: &Module, source: &crate::typeck::TypedModule,
    continuations: &BTreeMap<String, usize>, marked: &BTreeMap<usize, usize>)
    -> Result<Vec<(usize, usize)>, String>
{
    let functions = module.items.iter().filter_map(|item| match item {
        Item::Function(function) => Some((function.name.as_str(), function)), _ => None,
    }).collect::<BTreeMap<_, _>>();
    let mut edges = Vec::new();
    for caller in functions.values() {
        witchy_syntax::ast::visit::visit_block(&caller.body, &mut |expression| {
            let Expr::Call { name, args } = expression else { return Ok(()) };
            if !crate::suspension_carrier::call_family(name, "task.__producer_state_frame") { return Ok(()); }
            let [_, Expr::Lambda { body: prepare, .. }, Expr::Lambda { body: resume, .. }, _] = args.as_slice()
                else { return Ok(()) };
            let mut markers = Vec::new();
            witchy_syntax::ast::visit::visit_block(prepare, &mut |expression| {
                if let Expr::Block(block) = expression
                    && matches!(block.stmts.first(), Some(Stmt::Let { name, .. }) if name.starts_with(OPERAND_PREFIX))
                { markers.push((expression, block)); }
                Ok::<_, String>(())
            })?;
            if markers.is_empty() { return Ok(()); }
            let [(operand, marker)] = markers.as_slice() else { return Err("static caller has duplicate decomposition markers".into()) };
            let [Stmt::Let { name, value: Expr::Int(identity), .. }, Stmt::Expr(Expr::Call { name: init, args: arguments })]
                = marker.stmts.as_slice() else { return Err("static caller has malformed decomposition marker".into()) };
            let suspension = name.strip_prefix(OPERAND_PREFIX).and_then(|value| value.parse::<u32>().ok())
                .filter(|value| i64::from(*value) == *identity).ok_or("static caller has invalid source suspension")?;
            let proof = *marked.get(&(*operand as *const Expr as usize)).ok_or("static caller lacks original helper await proof")?;
            let [Stmt::Expr(Expr::Call { name: resume, .. })] = resume.stmts.as_slice()
                else { return Err("static caller lacks named source continuation".into()) };
            if continuations.get(resume) != Some(&proof) { return Err("static caller resume differs from original await".into()); }
            let original = original_function(source, caller)?;
            let mut awaits = Vec::new();
            witchy_syntax::ast::visit::visit_block(&original.body, &mut |expression| {
                if let Expr::Unary { op: UnOp::Await, expr } = expression { awaits.push(expr.as_ref()); }
                Ok::<_, String>(())
            })?;
            let Some(Expr::Call { name: helper, args: source_arguments }) = awaits.get(suspension as usize).copied()
                else { return Err("static caller source await is not a named helper call".into()) };
            let initializer = functions.get(init.as_str()).ok_or("static caller initializer is absent")?;
            let callee = original_function(source, initializer)?;
            if callee.name != *helper || arguments.len() != source_arguments.len()
                || initializer.params != callee.params || callee.params.iter().any(|parameter| parameter.convention != Convention::Let)
            { return Err("static caller initializer differs from original callee binding".into()); }
            let [Stmt::Expr(Expr::Tuple(fields))] = initializer.body.stmts.as_slice()
                else { return Err("static caller initializer performs source effects".into()) };
            if !fields.iter().all(|field| matches!(field, Expr::Int(_) | Expr::Bool(_))) {
                return Err("static caller initializer performs source effects".into());
            }
            let Some(Stmt::Expr(Expr::If { then_block, .. })) = prepare.stmts.last()
                else { return Err("static caller does not prepare at its original loop poll".into()) };
            let positions = then_block.stmts.iter().enumerate().filter_map(|(index, statement)| match statement {
                Stmt::Let { name, mutable: true, value, .. } if std::ptr::eq(value, *operand) => Some((index, name)),
                _ => None,
            }).collect::<Vec<_>>();
            let [(position, state)] = positions.as_slice()
                else { return Err("static caller does not initialize one fresh callee state".into()) };
            let first = position.checked_sub(arguments.len()).ok_or("static caller lacks ordered argument evaluations")?;
            for ((statement, actual), expected) in then_block.stmts[first..*position].iter().zip(arguments).zip(source_arguments) {
                let Stmt::Let { name, mutable: false, value, .. } = statement
                    else { return Err("static caller changes argument evaluation order".into()) };
                if !matches!(actual, Expr::Var(argument) if argument == name) || value != expected {
                    return Err("static caller changes original argument evaluation".into());
                }
            }
            let Some(Stmt::Let { value: Expr::Call { name: preparing, args: prepared_args }, .. }) = then_block.stmts.get(position + 1)
                else { return Err("static caller does not prepare after original argument evaluation".into()) };
            let preparing = functions.get(preparing.as_str()).ok_or("static caller named preparation is absent")?;
            if original_function(source, preparing)?.name != callee.name
                || prepared_args.len() != arguments.len() + 1
                || prepared_args[..arguments.len()] != *arguments
                || !matches!(prepared_args.last(), Some(Expr::Var(name)) if name == *state)
            { return Err("static caller preparation changes original callee arguments".into()); }
            let entry = functions.get(helper.as_str()).ok_or("static caller original callee entry is absent")?;
            validate_terminal_entry(entry, init, &preparing.name)?;
            edges.push((*operand as *const Expr as usize, proof));
            Ok::<_, String>(())
        })?;
    }
    Ok(edges)
}

fn original_function<'a>(source: &'a crate::typeck::TypedModule, function: &Function) -> Result<&'a Function, String> {
    let (item, method) = witchy_syntax::suspension::async_surface(function).ok_or("static call frame lacks original callable identity")?;
    let module = witchy_syntax::suspension::async_surface_module(function).ok_or("static call frame lacks original module identity")?;
    let callable = crate::async_control_flow::AsyncCallableId { module: module.to_owned(),
        surface: witchy_syntax::async_surface::AsyncSurfaceId { item: u32::try_from(item).map_err(|_| "static call source item exceeds u32")?,
            method: method.map(u32::try_from).transpose().map_err(|_| "static call source method exceeds u32")? } };
    crate::typed_async_lowering::typed_source_function(source, &callable).ok_or_else(|| "static call frame lost retained typed source".into())
}

fn validate_terminal_entry(entry: &Function, initializer: &str, prepare: &str) -> Result<(), String> {
    let mut matched = 0;
    witchy_syntax::ast::visit::visit_block(&entry.body, &mut |expression| {
        if let Expr::Call { name, args } = expression
            && crate::suspension_carrier::call_family(name, "task.__terminal_producer_state_frame")
            && let [Expr::Call { name: init, .. }, Expr::Lambda { body, .. }, _] = args.as_slice()
            && init == initializer
            && matches!(body.stmts.as_slice(), [Stmt::Expr(Expr::Call { name, .. })] if name == prepare)
        { matched += 1; }
        Ok::<_, String>(())
    })?;
    if matched == 1 { Ok(()) } else { Err("static caller does not use the exact terminal callee recipe".into()) }
}

pub(crate) fn validate_checked(checked: &crate::pipeline::CheckedModule) -> Result<(), String> {
    let mut identities = Vec::new();
    for item in &checked.module().items {
        let Item::Function(function) = item else { continue };
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if let Expr::Block(block) = expression
                && let Some(Stmt::Let { name, .. }) = block.stmts.first()
                && let Some(identity) = name.strip_prefix(OPERAND_PREFIX)
            {
                let suspension = identity.parse::<u32>().map_err(|_| "static caller lost source suspension")?;
                let (item, method) = witchy_syntax::suspension::async_surface(function).ok_or("static caller lost source callable")?;
                let module = witchy_syntax::suspension::async_surface_module(function).ok_or("static caller lost source module")?;
                identities.push((module.to_owned(), u32::try_from(item).map_err(|_| "static caller source item exceeds u32")?,
                    method.map(u32::try_from).transpose().map_err(|_| "static caller source method exceeds u32")?, suspension));
            }
            Ok::<_, String>(())
        })?;
    }
    if identities.is_empty() { return Ok(()); }
    let input = crate::typed_async_lowering::TypedAsyncLoweringInput::from_checked(checked)?;
    for (module, item, method, suspension) in identities {
        let bindings = input.static_calls.iter().filter(|binding| binding.await_proof.source_identity()
            == (module.as_str(), item, method, suspension)).collect::<Vec<_>>();
        let [binding] = bindings.as_slice() else { return Err("static caller lacks one exact typed callee/frame binding".into()) };
        if !input.static_call_is_acyclic(binding) || binding.arguments.iter().any(|argument| argument.convention != Convention::Let) {
            return Err("static caller decomposition is recursive or changes parameter transfer".into());
        }
    }
    Ok(())
}

pub(crate) fn assert_argument_forgery_refused(checked: &crate::pipeline::CheckedModule) {
    fn inventory(module: &Module, checked: &crate::pipeline::CheckedModule) -> (BTreeMap<String, usize>, BTreeMap<usize, usize>) {
        let proofs = checked.canonical_source().expect("canonical source").view().task_await_proofs();
        let mut continuations = BTreeMap::new();
        let mut marked = BTreeMap::new();
        for item in &module.items {
            let Item::Function(function) = item else { continue };
            let Some((item, method)) = witchy_syntax::suspension::async_surface(function) else { continue };
            let Some(module) = witchy_syntax::suspension::async_surface_module(function) else { continue };
            let proof = |suspension| proofs.iter().position(|proof| proof.source_identity()
                == (module, item as u32, method.map(|method| method as u32), suspension)).expect("source proof");
            if let Some(suspension) = witchy_syntax::suspension::source_suspension(function) {
                continuations.insert(function.name.clone(), proof(suspension));
            }
            witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
                if let Expr::Block(block) = expression
                    && let Some(Stmt::Let { name, .. }) = block.stmts.first()
                    && let Some(suspension) = name.strip_prefix(OPERAND_PREFIX)
                { marked.insert(expression as *const Expr as usize, proof(suspension.parse().expect("source suspension"))); }
                Ok::<_, ()>(())
            }).expect("infallible forgery inventory");
        }
        (continuations, marked)
    }
    let source = checked.typed_async_source().expect("retained typed source");
    let module = checked.module();
    let (continuations, marked) = inventory(module, checked);
    assert_eq!(caller_edges(module, source, &continuations, &marked).expect("original caller edges").len(), 1);
    for mutation in 0..3 {
        let mut module = module.clone();
        let mut changed = false;
        for item in &mut module.items {
            let Item::Function(function) = item else { continue };
            witchy_syntax::ast::visit::visit_block_mut(&mut function.body, &mut |expression| {
                if changed { return Ok(()); }
                if let Expr::If { then_block, .. } = expression {
                    let position = then_block.stmts.iter().position(|statement| matches!(statement,
                        Stmt::Let { value: Expr::Block(block), .. } if matches!(block.stmts.first(),
                            Some(Stmt::Let { name, .. }) if name.starts_with(OPERAND_PREFIX))));
                    if let Some(position) = position {
                        match mutation {
                            0 => {
                                let Stmt::Let { value, .. } = &mut then_block.stmts[position - 1] else { panic!("ordered argument") };
                                *value = Expr::Int(-99);
                            }
                            1 => {
                                let Stmt::Let { value: Expr::Block(block), .. } = &mut then_block.stmts[position] else { unreachable!() };
                                let Stmt::Expr(Expr::Call { args, .. }) = &mut block.stmts[1] else { unreachable!() };
                                args.reverse();
                            }
                            _ => {
                                let Stmt::Let { value: Expr::Call { args, .. }, .. } = &mut then_block.stmts[position + 1] else { unreachable!() };
                                args.reverse();
                            }
                        }
                        changed = true;
                    }
                }
                Ok::<_, ()>(())
            }).expect("infallible caller forgery");
        }
        assert!(changed);
        let (continuations, marked) = inventory(&module, checked);
        assert!(caller_edges(&module, source, &continuations, &marked).is_err());
    }
}
