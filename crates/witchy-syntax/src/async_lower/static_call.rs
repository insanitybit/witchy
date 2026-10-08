use super::*;

pub(super) fn lower(mut function: Function, counter: &mut usize, state_counter: &mut usize,
    shells: &BorrowedShellCatalog) -> Result<Option<(Function, Vec<Function>)>, String>
{
    if !crate::opt::enabled(crate::opt::Opt::ClosureElide) || function.body.region.is_some()
        || !function.type_params.is_empty() || !function.bounds.is_empty()
        || function.params.iter().any(|parameter| parameter.default.is_some()
            || parameter.convention != Convention::Let
            || parameter.ty.as_ref().is_none_or(|ty| shells.type_is_borrowed(ty)))
        || block_contains_return(&function.body) || recv_frame::has_loop_escape(&function.body)
    { return Ok(None); }
    let awaits = function.body.stmts.iter().enumerate().filter(|(_, statement)| stmt_contains_await(statement))
        .collect::<Vec<_>>();
    let [(split, Stmt::Expr(awaited))] = awaits.as_slice() else { return Ok(None) };
    let split = *split;
    let Some(Expr::Call { name, args }) = as_await(awaited) else { return Ok(None) };
    if name != "chan.send" || args.len() != 2 { return Ok(None); }
    let suffix = &function.body.stmts[split + 1..];
    let has_suffix = !suffix.is_empty();
    let unit = Type::Tuple(Vec::new());
    if function.ret.as_ref().is_some_and(|result| result != &unit)
        || (has_suffix && function.ret.as_ref() != Some(&unit))
    { return Ok(None); }
    let mut bindings = Vec::<Local>::new();
    let suffix_block = Block { stmts: suffix.to_vec(), lines: Vec::new(), region: None };
    let suffix_reads = crate::suspension::free_bindings_in_block(&suffix_block, &HashSet::default());
    let mut names = function.params.iter().map(|parameter| parameter.name.clone()).collect::<HashSet<_>>();
    for statement in &function.body.stmts[..split] {
        match statement {
            Stmt::Let { name, ty, mutable, .. } => {
                if !names.insert(name.clone()) { return Ok(None); }
                if suffix_reads.contains(name) {
                    let Some(ty) = ty.as_ref().filter(|ty| matches!(ty,
                        Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Bool")))
                    else { return Ok(None) };
                    bindings.push(Local { name: name.clone(), source_binding: None, ty: Some(ty.clone()),
                        mutable: *mutable, borrowed_view: false, returns_view: false });
                }
            }
            Stmt::LetPattern { pattern, .. } => {
                let mut bound = Vec::new();
                pattern_binds(pattern, &mut bound);
                if bound.iter().any(|name| suffix_reads.contains(name) || !names.insert(name.clone())) { return Ok(None); }
            }
            _ => {}
        }
    }
    for statement in suffix {
        let mut bound = Vec::new();
        match statement {
            Stmt::Let { name, .. } => bound.push(name.clone()),
            Stmt::LetPattern { pattern, .. } => pattern_binds(pattern, &mut bound),
            _ => {}
        }
        if bound.iter().any(|name| !names.insert(name.clone())) { return Ok(None); }
    }
    tag_source_awaits(&mut function.body)?;
    let Stmt::Expr(awaited) = &function.body.stmts[split] else { unreachable!() };
    let identity = source_await_id(awaited).ok_or("static async send lost its source await identity")?;
    let operand = as_await(awaited).ok_or("static async send lost its source operand")?.clone();
    let line = line_at(&function.body.lines, split);
    let source_name = sanitize(&function.name);
    let mut fresh = || {
        let ordinal = *counter;
        *counter += 1;
        format!("__async_call_{source_name}_{ordinal}")
    };
    let init_name = fresh();
    let prepare_name = fresh();
    let resume_name = fresh();
    let frame = fresh();
    let pc = fresh();
    let prepared = fresh();
    let state_type = Type::Tuple(std::iter::once(named("Int"))
        .chain(bindings.iter().map(|binding| binding.ty.clone().expect("checked state field"))).collect());
    let make_block = |stmts: Vec<Stmt>| Block { lines: vec![line; stmts.len()], stmts, region: None };
    let make_function = |name: String, params: Vec<Param>, ret: Option<Type>, body: Block, attributes: Vec<String>| Function {
        line, public: false, pure: false, comptime_only: false, attributes, name,
        type_params: Vec::new(), params, ret, body, bounds: Vec::new(), is_gen: false, is_async: false,
    };
    let state_param = |convention| Param { name: frame.clone(), ty: Some(state_type.clone()), convention, default: None };
    let state_value = |phase: i64| Expr::Tuple(std::iter::once(Expr::Int(phase))
        .chain(bindings.iter().map(|binding| Expr::Var(binding.name.clone()))).collect());
    let mut loads = |count: usize, mutable_pc: bool| {
        let fields = std::iter::once(pc.clone()).chain(bindings.iter().map(|binding| binding.name.clone()))
            .map(|_| fresh()).collect::<Vec<_>>();
        let mut statements = vec![Stmt::LetPattern { pattern: Pattern::Tuple(fields.iter().cloned().map(Pattern::Var).collect()),
            value: Expr::Var(frame.clone()) }];
        statements.push(Stmt::Let { name: pc.clone(), ty: Some(named("Int")), mutable: mutable_pc,
            value: Expr::Var(fields[0].clone()) });
        statements.extend(bindings.iter().take(count).zip(&fields[1..]).map(|(binding, field)| Stmt::Let {
            name: binding.name.clone(), ty: binding.ty.clone(), mutable: binding.mutable, value: Expr::Var(field.clone()),
        }));
        statements
    };
    let initial = Expr::Tuple(std::iter::once(Expr::Int(0)).chain(bindings.iter().map(|binding| {
        if binding.ty == Some(named("Bool")) { Expr::Bool(false) } else { Expr::Int(0) }
    })).collect());
    let init = make_function(init_name.clone(), function.params.clone(), Some(state_type.clone()),
        make_block(vec![Stmt::Expr(initial)]), function.attributes.clone());
    let mut preparing = function.body.stmts[..split].to_vec();
    preparing.push(Stmt::Let { name: prepared.clone(), ty: None, mutable: false,
        value: call("task.__prepare_send", vec![mark_source_task_operand(identity, operand)]) });
    preparing.push(Stmt::Assign { name: frame.clone(), value: state_value(1), place_depth: 0 });
    preparing.push(Stmt::Expr(if has_suffix {
        Expr::Ctor { name: "Some".into(), args: vec![Expr::Var(prepared)] }
    } else { Expr::Var(prepared) }));
    let mut prepare_body = loads(0, false);
    if has_suffix {
        prepare_body.push(Stmt::Expr(Expr::If {
            cond: Box::new(Expr::Binary { op: BinOp::Eq, lhs: Box::new(Expr::Var(pc.clone())), rhs: Box::new(Expr::Int(0)) }),
            then_block: make_block(preparing),
            else_block: Some(make_block(vec![Stmt::Expr(Expr::Ctor { name: "None".into(), args: Vec::new() })])),
        }));
    } else { prepare_body.extend(preparing); }
    let mut callback_params = function.params.clone();
    callback_params.push(state_param(Convention::Var));
    let prepare = make_function(prepare_name.clone(), callback_params.clone(),
        None, make_block(prepare_body), function.attributes.clone());
    let mut resuming = loads(bindings.len(), false);
    resuming.extend_from_slice(&function.body.stmts[split + 1..]);
    resuming.push(Stmt::Assign { name: frame.clone(), value: state_value(2), place_depth: 0 });
    resuming.push(Stmt::Expr(Expr::Tuple(Vec::new())));
    let mut resume_attributes = function.attributes.clone();
    if has_suffix {
        resume_attributes.push(crate::suspension::FRAME_FUNCTION_ATTRIBUTE.to_owned());
        resume_attributes.push(crate::suspension::frame_state_attribute(*state_counter));
        *state_counter += 1;
        resume_attributes.push(crate::suspension::source_suspension_attribute(identity));
    }
    let resume = make_function(resume_name.clone(), callback_params, Some(unit.clone()),
        make_block(resuming), resume_attributes);
    let callback = |target: &str, ret: Option<Type>| {
        let mut arguments = function.params.iter().map(|parameter| Expr::Var(parameter.name.clone())).collect::<Vec<_>>();
        arguments.push(Expr::Var(frame.clone()));
        Expr::Lambda { params: vec![state_param(Convention::Var)],
            body: make_block(vec![Stmt::Expr(call(target, arguments))]),
            ret, qualifiers: CallableQualifiers::ORDINARY }
    };
    let finish = Expr::Lambda { params: vec![state_param(Convention::Let)],
        body: make_block(vec![Stmt::Expr(call("task.ready_unit", Vec::new()))]),
        ret: None, qualifiers: CallableQualifiers::ORDINARY };
    let mut producer_arguments = vec![
        call(&init_name, function.params.iter().map(|parameter| Expr::Var(parameter.name.clone())).collect()),
        callback(&prepare_name, None),
    ];
    if has_suffix { producer_arguments.push(callback(&resume_name, Some(unit.clone()))); }
    producer_arguments.push(finish);
    let producer = call(if has_suffix { "task.__producer_state_frame" } else { "task.__terminal_producer_state_frame" },
        producer_arguments);
    function.body = make_block(vec![Stmt::Expr(call("task.lazy", vec![Expr::Lambda { params: Vec::new(),
        body: make_block(vec![Stmt::Expr(producer)]), ret: None, qualifiers: CallableQualifiers::ORDINARY }]))]);
    function.ret = Some(Type::Named("task.Task".into(), vec![unit]));
    function.is_async = false;
    function.attributes.push(crate::suspension::FRAME_ENTRY_ATTRIBUTE.to_owned());
    function.attributes.push(crate::suspension::frame_state_attribute(*state_counter));
    function.attributes.push(crate::suspension::async_region_attribute(*state_counter));
    *state_counter += 1;
    function.attributes.extend(function.params.iter().enumerate().map(|(index, _)|
        crate::suspension::frame_binding_attribute(index, u32::try_from(index).expect("async parameter ordinal exceeds u32"))));
    let mut segments = vec![init, prepare];
    if has_suffix { segments.push(resume); }
    Ok(Some((function, segments)))
}
