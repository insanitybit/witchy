use super::*;

impl Ctx<'_> {
    pub(super) fn try_lower_receive_state_loop(&mut self, cond: &Expr, body: &Block,
        cont: Continuation<'_>) -> Result<Option<Expr>, String>
    {
        if !crate::opt::enabled(crate::opt::Opt::ClosureElide) || body.region.is_some()
            || contains_await(cond) || block_contains_return(body) || recv_frame::has_loop_escape(body) { return Ok(None); }
        let suspensions = body.stmts.iter().enumerate().filter(|(_, s)| stmt_contains_await(s)).collect::<Vec<_>>();
        let [(split, statement)] = suspensions.as_slice() else { return Ok(None) };
        let (awaited, binding, replacement) = match statement {
            Stmt::Let { name, mutable: false, value, .. } => (value, name.clone(), None),
            Stmt::Expr(Expr::Match { scrutinee, arms }) => {
                let binding = self.fresh_tmp();
                (&**scrutinee, binding.clone(), Some(Stmt::Expr(Expr::Match {
                    scrutinee: Box::new(Expr::Var(binding)), arms: arms.clone() })))
            }
            _ => return Ok(None),
        };
        let Some(inner) = as_await(awaited) else { return Ok(None) };
        let Expr::Call { name, args } = inner else { return Ok(None) };
        if name != "chan.recv" || args.len() != 1 || !matches!(&args[0], Expr::Var(_)) { return Ok(None); }
        let Some(Type::Named(_, payloads)) = derive_type(&args[0], cont.scope, self.borrowed_shells) else { return Ok(None) };
        let [payload] = payloads.as_slice() else { return Ok(None) };
        if mentions_type_variable(payload) { return Ok(None); }
        let Some(identity) = source_await_id(awaited) else { return Err(self.err("receive await lost its checked identity")); };
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
        let mut suffix = Vec::new();
        if let Some(replacement) = replacement { suffix.push(replacement); }
        suffix.extend_from_slice(&body.stmts[*split + 1..]);
        let suffix_block = Block { stmts: suffix.clone(), lines: Vec::new(), region: None };
        if block_contains_await(&suffix_block) { return Ok(None); }
        if all_names.contains(&binding) { return Ok(None); }
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
        let channel = self.fresh_tmp();
        preparing.push(Stmt::Let { name: channel.clone(), ty: Some(named("Int")), mutable: false,
            value: call("task.__prepare_recv", vec![mark_source_task_operand(identity, inner.clone())]) });
        preparing.push(store());
        preparing.push(Stmt::Expr(Expr::Ctor { name: "Some".into(), args: vec![Expr::Var(channel)] }));
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
        let result_ty = Type::Named("Option".into(), vec![payload.clone()]);
        let mut resume_params = vec![Param { name: binding.clone(), ty: Some(result_ty.clone()), convention: Convention::Own, default: None }];
        resume_params.extend(carried.iter().filter(|l| !l.mutable).map(local_to_param));
        resume_params.push(param());
        self.segments.push(Function { line: cont.line, public: false, pure: false, comptime_only: false, attributes,
            name: resume_name.clone(), type_params: vec![], params: resume_params, ret: Some(Type::Tuple(vec![])),
            body: Block { lines: vec![cont.line; resume_body.len()], stmts: resume_body, region: None },
            bounds: vec![], is_gen: false, is_async: false });
        let mut resume_args = vec![Expr::Var(binding.clone())];
        resume_args.extend(carried.iter().filter(|l| !l.mutable).map(|l| Expr::Var(l.name.clone())));
        resume_args.push(Expr::Var(state.clone()));
        let resume = Expr::Lambda { params: vec![Param { name: binding, ty: Some(result_ty), convention: Convention::Own, default: None }, param()],
            body: tail_block_at(call(&resume_name, resume_args), cont.line), ret: Some(Type::Tuple(vec![])), qualifiers: CallableQualifiers::ORDINARY };
        let mut finish_body = loads();
        finish_body.push(Stmt::Expr(self.loop_exit(&state_locals)));
        let finish = Expr::Lambda { params: vec![Param { name: state.clone(), ty: Some(state_ty), convention: Convention::Let, default: None }],
            body: Block { lines: vec![cont.line; finish_body.len()], stmts: finish_body, region: None }, ret: None, qualifiers: CallableQualifiers::ORDINARY };
        let initial = Expr::Tuple(state_locals.iter().map(|l| Expr::Var(l.name.clone())).collect());
        Ok(Some(self.sequence_loop(call("chan.__recv_state_frame", vec![initial, prepare, resume, finish]), state_locals, cont)?))
    }
}
