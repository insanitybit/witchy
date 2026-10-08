use super::*;

impl Ctx<'_> {
    pub(super) fn try_lower_static_call_loop(&mut self, cond: &Expr, body: &Block,
        cont: Continuation<'_>) -> Result<Option<Expr>, String>
    {
        if !crate::opt::enabled(crate::opt::Opt::ClosureElide) || !self.static_call_eligible || body.region.is_some()
            || contains_await(cond) || block_contains_return(body) || recv_frame::has_loop_escape(body) { return Ok(None); }
        let suspensions = body.stmts.iter().enumerate().filter(|(_, s)| stmt_contains_await(s)).collect::<Vec<_>>();
        let [(split, statement)] = suspensions.as_slice() else { return Ok(None) };
        let Stmt::Expr(awaited) = statement else { return Ok(None) };
        let Some(inner) = as_await(awaited) else { return Ok(None) };
        let Expr::Call { name, args } = inner else { return Ok(None) };
        if cont.scope.iter().any(|local| &local.name == name) { return Ok(None); }
        for statement in &body.stmts[..*split] {
            let mut bound = Vec::new();
            match statement {
                Stmt::Let { name, .. } => bound.push(name.clone()),
                Stmt::LetPattern { pattern, .. } => pattern_binds(pattern, &mut bound),
                _ => {}
            }
            if bound.contains(name) { return Ok(None); }
        }
        let Some(callee) = self.static_calls.terminal.get(name).cloned() else { return Ok(None) };
        if callee.parameters.len() != args.len() { return Ok(None); }
        let Some(identity) = source_await_id(awaited) else { return Err(self.err("static call await lost its checked identity")); };
        let mut used = crate::suspension::free_bindings(cond);
        used.extend(crate::suspension::free_bindings_in_block(body, &HashSet::default()));
        let carried = cont.scope.iter().filter(|local| used.contains(&local.name)).cloned().collect::<Vec<_>>();
        let state_locals = carried.iter().filter(|local| local.mutable).cloned().collect::<Vec<_>>();
        fn plain(ty: &Type) -> bool {
            match ty {
                Type::Named(name, args) if args.is_empty() => name == "Int" || name == "Bool",
                Type::Named(name, args) if name == "List" => matches!(args.as_slice(), [Type::Named(n, a)] if n == "Int" && a.is_empty()),
                _ => false,
            }
        }
        if state_locals.is_empty() || carried.iter().any(|local| local.borrowed_view || local.returns_view)
            || state_locals.iter().any(|local| !local.ty.as_ref().is_some_and(plain)) { return Ok(None); }
        let mut all_names = HashSet::default();
        if carried.iter().any(|local| !all_names.insert(local.name.clone())) { return Ok(None); }
        let suffix = body.stmts[*split + 1..].to_vec();
        let suffix_block = Block { stmts: suffix.clone(), lines: Vec::new(), region: None };
        if block_contains_await(&suffix_block) { return Ok(None); }
        let suffix_used = crate::suspension::free_bindings_in_block(&suffix_block, &HashSet::default());
        for statement in &body.stmts[..*split] {
            let mut names = Vec::new();
            match statement {
                Stmt::Let { name, .. } => names.push(name.clone()),
                Stmt::LetPattern { pattern, .. } => pattern_binds(pattern, &mut names),
                _ => {}
            }
            if names.iter().any(|name| suffix_used.contains(name) || all_names.contains(name)) { return Ok(None); }
        }
        for statement in &body.stmts[*split + 1..] {
            let mut names = Vec::new();
            match statement {
                Stmt::Let { name, .. } => names.push(name.clone()),
                Stmt::LetPattern { pattern, .. } => pattern_binds(pattern, &mut names),
                _ => {}
            }
            if names.iter().any(|name| all_names.contains(name)) { return Ok(None); }
        }
        let state_ty = Type::Tuple(state_locals.iter().map(|l| l.ty.clone().unwrap()).collect());
        let state = self.fresh_tmp();
        let temporary_names = state_locals.iter().map(|_| self.fresh_tmp()).collect::<Vec<_>>();
        let loads = || {
            let mut stmts = vec![Stmt::LetPattern { pattern: Pattern::Tuple(temporary_names.iter().cloned().map(Pattern::Var).collect()),
                value: Expr::Var(state.clone()) }];
            stmts.extend(state_locals.iter().zip(&temporary_names).map(|(local, temp)| Stmt::Let {
                name: local.name.clone(), ty: local.ty.clone(), mutable: true, value: Expr::Var(temp.clone()) }));
            stmts
        };
        let store = || Stmt::Assign { name: state.clone(), place_depth: 0, value: Expr::Tuple(state_locals.iter().map(|l| Expr::Var(l.name.clone())).collect()) };
        let param = || Param { name: state.clone(), ty: Some(state_ty.clone()), convention: Convention::Var, default: None };
        let mut preparing = body.stmts[..*split].to_vec();
        let argument_names = args.iter().map(|_| self.fresh_tmp()).collect::<Vec<_>>();
        for ((argument, parameter), name) in args.iter().zip(&callee.parameters).zip(&argument_names) {
            preparing.push(Stmt::Let { name: name.clone(), ty: parameter.ty.clone(), mutable: false,
                value: argument.clone() });
        }
        let call_arguments = argument_names.iter().cloned().map(Expr::Var).collect::<Vec<_>>();
        let callee_state = self.fresh_tmp();
        let marker = Expr::Block(Block { lines: vec![cont.line; 2], region: None, stmts: vec![
            Stmt::Let { name: format!("__source_static_call_operand_{identity}"), ty: Some(named("Int")),
                mutable: false, value: Expr::Int(i64::from(identity)) },
            Stmt::Expr(call(&callee.initializer, call_arguments.clone())),
        ] });
        preparing.push(Stmt::Let { name: callee_state.clone(), ty: Some(callee.state), mutable: true, value: marker });
        let mut prepare_arguments = call_arguments;
        prepare_arguments.push(Expr::Var(callee_state));
        let pending = self.fresh_tmp();
        preparing.push(Stmt::Let { name: pending.clone(), ty: None, mutable: false,
            value: call(&callee.prepare, prepare_arguments) });
        preparing.push(store());
        preparing.push(Stmt::Expr(Expr::Ctor { name: "Some".into(), args: vec![Expr::Var(pending)] }));
        let mut prepare_body = loads();
        prepare_body.push(Stmt::Expr(Expr::If { cond: Box::new(cond.clone()),
            then_block: Block { lines: vec![cont.line; preparing.len()], stmts: preparing, region: None },
            else_block: Some(Block { lines: vec![cont.line; 2], stmts: vec![store(), Stmt::Expr(Expr::Ctor { name: "None".into(), args: vec![] })], region: None }) }));
        let prepare = Expr::Lambda { params: vec![param()], ret: None, qualifiers: CallableQualifiers::ORDINARY,
            body: Block { lines: vec![cont.line; prepare_body.len()], stmts: prepare_body, region: None } };
        let resume_name = self.fresh_seg();
        let mut resume_body = loads();
        resume_body.extend(suffix);
        resume_body.push(store());
        let mut attributes = self.frame_attributes();
        attributes.push(crate::suspension::source_suspension_attribute(identity));
        let mut resume_params = carried.iter().filter(|l| !l.mutable).map(local_to_param).collect::<Vec<_>>();
        resume_params.push(param());
        self.segments.push(Function { line: cont.line, public: false, pure: false, comptime_only: false, attributes,
            name: resume_name.clone(), type_params: vec![], params: resume_params, ret: Some(Type::Tuple(vec![])),
            body: Block { lines: vec![cont.line; resume_body.len()], stmts: resume_body, region: None },
            bounds: vec![], is_gen: false, is_async: false });
        let mut resume_args = carried.iter().filter(|l| !l.mutable).map(|l| Expr::Var(l.name.clone())).collect::<Vec<_>>();
        resume_args.push(Expr::Var(state.clone()));
        let resume = Expr::Lambda { params: vec![param()],
            body: tail_block_at(call(&resume_name, resume_args), cont.line), ret: Some(Type::Tuple(vec![])), qualifiers: CallableQualifiers::ORDINARY };
        let mut finish_body = loads();
        finish_body.push(Stmt::Expr(self.loop_exit(&state_locals)));
        let finish = Expr::Lambda { params: vec![Param { name: state.clone(), ty: Some(state_ty), convention: Convention::Let, default: None }],
            body: Block { lines: vec![cont.line; finish_body.len()], stmts: finish_body, region: None }, ret: None, qualifiers: CallableQualifiers::ORDINARY };
        let initial = Expr::Tuple(state_locals.iter().map(|l| Expr::Var(l.name.clone())).collect());
        Ok(Some(self.sequence_loop(call("task.__producer_state_frame", vec![initial, prepare, resume, finish]), state_locals, cont)?))
    }
}
