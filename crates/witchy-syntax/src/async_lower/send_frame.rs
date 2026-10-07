use super::*;

impl Ctx<'_> {
    pub(super) fn try_lower_send_frame_loop(&mut self, cond: &Expr, body: &Block,
        cont: Continuation<'_>) -> Result<Option<Expr>, String>
    {
        if !crate::opt::enabled(crate::opt::Opt::ClosureElide) || body.region.is_some()
            || block_contains_return(body) || recv_frame::has_loop_escape(body)
        { return Ok(None); }
        let suspensions = body.stmts.iter().enumerate().filter(|(_, statement)| stmt_contains_await(statement))
            .collect::<Vec<_>>();
        let [(split, Stmt::Expr(awaited))] = suspensions.as_slice() else { return Ok(None) };
        let Some(inner) = as_await(awaited) else { return Ok(None) };
        let Expr::Call { name, args } = inner else { return Ok(None) };
        if name != "chan.send" || args.len() != 2 || !matches!(&args[0], Expr::Var(_)) {
            return Ok(None);
        }
        let Some(identity) = source_await_id(awaited) else {
            return Err(self.err("producer await lost its checked identity"));
        };
        let mut used = crate::suspension::free_bindings(cond);
        used.extend(crate::suspension::free_bindings_in_block(body, &HashSet::default()));
        let carried = cont.scope.iter().filter(|local| used.contains(&local.name)).cloned().collect::<Vec<_>>();
        let mut names = HashSet::default();
        if carried.iter().any(|local| !names.insert(local.name.clone())) { return Ok(None); }
        let outer_lanes = carried.iter().filter(|local| local.mutable).cloned().collect::<Vec<_>>();
        let scalar = |local: &Local| matches!(local.ty.as_ref(),
            Some(Type::Named(name, args)) if args.is_empty() && (name == "Int" || name == "Bool"));
        if outer_lanes.iter().any(|local| !scalar(local))
            || carried.iter().any(|local| local.borrowed_view || local.returns_view)
        { return Ok(None); }
        let suffix = Block { stmts: body.stmts[*split + 1..].to_vec(), lines: Vec::new(), region: None };
        let suffix_uses = crate::suspension::free_bindings_in_block(&suffix, &HashSet::default());
        let mut prefix_scope = cont.scope.to_vec();
        let mut inner_lanes = Vec::new();
        let mut prefix_names = HashSet::default();
        for statement in &body.stmts[..*split] {
            let local = match statement {
                Stmt::Let { name, ty, mutable, value } => Local { name: name.clone(), source_binding: None,
                    ty: ty.clone().or_else(|| derive_type(value, &prefix_scope, self.borrowed_shells)),
                    mutable: *mutable, borrowed_view: result_is_borrowed_view(value, &prefix_scope, self.view_fns, self.borrowed_shells),
                    returns_view: callable_returns_view(value, &prefix_scope, self.view_fns, self.borrowed_shells) },
                Stmt::LetPattern { pattern, .. } => {
                    let mut names = Vec::new();
                    pattern_binds(pattern, &mut names);
                    if names.iter().any(|name| suffix_uses.contains(name)
                        || outer_lanes.iter().any(|lane| &lane.name == name)) { return Ok(None); }
                    continue;
                }
                _ => continue,
            };
            if !prefix_names.insert(local.name.clone())
                || outer_lanes.iter().any(|lane| lane.name == local.name) { return Ok(None); }
            if suffix_uses.contains(&local.name) {
                if !scalar(&local) || local.borrowed_view || local.returns_view { return Ok(None); }
                inner_lanes.push(local.clone());
            }
            prefix_scope.push(local);
        }
        let mut lanes = outer_lanes.clone();
        lanes.extend(inner_lanes);
        for statement in &body.stmts[*split + 1..] {
            let mut names = Vec::new();
            match statement {
                Stmt::Let { name, .. } => names.push(name.clone()),
                Stmt::LetPattern { pattern, .. } => pattern_binds(pattern, &mut names),
                _ => {}
            }
            if names.iter().any(|name| lanes.iter().any(|lane| &lane.name == name)) { return Ok(None); }
        }
        let frame_type = Type::Named("List".into(), vec![named("Int")]);
        let frame = self.fresh_tmp();
        let base = self.fresh_tmp();
        let lane_index = |index: usize, offset: bool| if offset {
            Expr::Binary { op: BinOp::Add, lhs: Box::new(Expr::Var(base.clone())),
                rhs: Box::new(Expr::Int(index as i64)) }
        } else { Expr::Int(index as i64) };
        let loads = |frame: &str, offset: bool, count: usize| lanes.iter().take(count).enumerate().map(|(index, lane)| Stmt::Let {
            name: lane.name.clone(), ty: lane.ty.clone(), mutable: lane.mutable,
            value: decode_lane(call("list.at", vec![Expr::Var(frame.into()), lane_index(index, offset)]), lane),
        }).collect::<Vec<_>>();
        let stores = |frame: &str| lanes.iter().enumerate().map(|(index, lane)| Stmt::Expr(call("list.set_at",
            vec![Expr::Var(frame.into()), lane_index(index, true), encode_lane(Expr::Var(lane.name.clone()), lane)])))
            .collect::<Vec<_>>();
        let mut preparing = body.stmts[..*split].to_vec();
        let mut preparing_lines = (0..*split).map(|index| line_at(&body.lines, index)).collect::<Vec<_>>();
        let send_line = line_at(&body.lines, *split);
        let prepared = self.fresh_tmp();
        preparing.push(Stmt::Let { name: prepared.clone(), ty: None, mutable: false,
            value: call("task.__prepare_send", vec![mark_source_task_operand(identity, inner.clone())]) });
        preparing.extend(stores(&frame));
        preparing.push(Stmt::Expr(Expr::Ctor { name: "Some".into(), args: vec![Expr::Var(prepared)] }));
        preparing_lines.extend(std::iter::repeat_n(send_line, lanes.len() + 2));
        let mut prepare_body = loads(&frame, true, outer_lanes.len());
        prepare_body.push(Stmt::Expr(Expr::If { cond: Box::new(cond.clone()),
            then_block: Block { lines: preparing_lines, stmts: preparing, region: None },
            else_block: Some(tail_block_at(Expr::Ctor { name: "None".into(), args: vec![] }, cont.line)),
        }));
        let prepare = Expr::Lambda { params: vec![Param { name: base.clone(), ty: Some(named("Int")),
            convention: Convention::Let, default: None }, Param { name: frame.clone(), ty: Some(frame_type.clone()),
            convention: Convention::Var, default: None }], body: Block {
                lines: vec![cont.line; prepare_body.len()], stmts: prepare_body, region: None },
            ret: None, qualifiers: CallableQualifiers::ORDINARY };
        let resume_name = self.fresh_seg();
        let mut resuming = loads(&frame, true, lanes.len());
        let mut resuming_lines = vec![cont.line; resuming.len()];
        resuming.extend_from_slice(&body.stmts[*split + 1..]);
        resuming_lines.extend((*split + 1..body.stmts.len()).map(|index| line_at(&body.lines, index)));
        resuming.extend(stores(&frame));
        resuming.push(Stmt::Expr(Expr::Bool(false)));
        resuming_lines.extend(std::iter::repeat_n(body.lines.last().copied().unwrap_or(send_line), lanes.len() + 1));
        let mut attributes = self.frame_attributes();
        attributes.push(crate::suspension::source_suspension_attribute(identity));
        self.segments.push(Function { line: cont.line, public: false, pure: false, comptime_only: false,
            attributes, name: resume_name.clone(), type_params: Vec::new(),
            params: carried.iter().filter(|local| !local.mutable).map(local_to_param)
                .chain([Param { name: base.clone(), ty: Some(named("Int")), convention: Convention::Let, default: None },
                    Param { name: frame.clone(), ty: Some(frame_type.clone()),
                        convention: Convention::Var, default: None }]).collect(),
            ret: Some(named("Bool")), body: Block { lines: resuming_lines,
                stmts: resuming, region: None }, bounds: vec![], is_gen: false, is_async: false });
        let mut resume_args = carried.iter().filter(|local| !local.mutable)
            .map(|local| Expr::Var(local.name.clone())).collect::<Vec<_>>();
        resume_args.push(Expr::Var(base.clone()));
        resume_args.push(Expr::Var(frame.clone()));
        let resume = Expr::Lambda { params: vec![Param { name: base.clone(), ty: Some(named("Int")),
            convention: Convention::Let, default: None }, Param { name: frame.clone(), ty: Some(frame_type.clone()),
            convention: Convention::Var, default: None }],
            body: tail_block_at(call(&resume_name, resume_args), cont.line), ret: Some(named("Bool")),
            qualifiers: CallableQualifiers::ORDINARY };
        let mut finishing = loads(&frame, false, outer_lanes.len());
        finishing.push(Stmt::Expr(self.loop_exit(&outer_lanes)));
        let finish = Expr::Lambda { params: vec![Param { name: frame.clone(), ty: Some(frame_type),
            convention: Convention::Let, default: None }], body: Block {
                lines: vec![cont.line; finishing.len()], stmts: finishing, region: None },
            ret: None, qualifiers: CallableQualifiers::ORDINARY };
        let initial = if lanes.is_empty() { call("list.repeat", vec![Expr::Int(0), Expr::Int(0)]) }
            else { Expr::List(lanes.iter().enumerate().map(|(index, lane)| if index < outer_lanes.len() {
                encode_lane(Expr::Var(lane.name.clone()), lane)
            } else { Expr::Int(0) }).collect()) };
        Ok(Some(self.sequence_loop(call("task.__producer_frame", vec![initial, prepare, resume, finish]), outer_lanes, cont)?))
    }
}
