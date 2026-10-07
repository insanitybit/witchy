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
        let lanes = carried.iter().filter(|local| local.mutable).cloned().collect::<Vec<_>>();
        let scalar = |local: &Local| matches!(local.ty.as_ref().map(Type::unqualified),
            Some(Type::Named(name, args)) if args.is_empty() && (name == "Int" || name == "Bool"));
        if lanes.iter().any(|local| !scalar(local))
            || carried.iter().any(|local| local.borrowed_view || local.returns_view)
            || body.stmts.iter().any(|statement| matches!(statement, Stmt::Let { .. } | Stmt::LetPattern { .. }))
        { return Ok(None); }
        let frame_type = Type::Named("List".into(), vec![named("Int")]);
        let frame = self.fresh_tmp();
        let loads = |frame: &str| lanes.iter().enumerate().map(|(index, lane)| Stmt::Let {
            name: lane.name.clone(), ty: lane.ty.clone(), mutable: true,
            value: decode_lane(call("list.at", vec![Expr::Var(frame.into()), Expr::Int(index as i64)]), lane),
        }).collect::<Vec<_>>();
        let stores = |frame: &str| lanes.iter().enumerate().map(|(index, lane)| Stmt::Expr(call("list.set_at",
            vec![Expr::Var(frame.into()), Expr::Int(index as i64), encode_lane(Expr::Var(lane.name.clone()), lane)])))
            .collect::<Vec<_>>();
        let mut preparing = body.stmts[..*split].to_vec();
        let prepared = self.fresh_tmp();
        preparing.push(Stmt::Let { name: prepared.clone(), ty: None, mutable: false,
            value: call("task.__prepare_send", vec![mark_source_task_operand(identity, inner.clone())]) });
        preparing.extend(stores(&frame));
        preparing.push(Stmt::Expr(Expr::Ctor { name: "Some".into(), args: vec![Expr::Var(prepared)] }));
        let mut prepare_body = loads(&frame);
        prepare_body.push(Stmt::Expr(Expr::If { cond: Box::new(cond.clone()),
            then_block: Block { lines: vec![cont.line; preparing.len()], stmts: preparing, region: None },
            else_block: Some(tail_block_at(Expr::Ctor { name: "None".into(), args: vec![] }, cont.line)),
        }));
        let prepare = Expr::Lambda { params: vec![Param { name: frame.clone(), ty: Some(frame_type.clone()),
            convention: Convention::Var, default: None }], body: Block {
                lines: vec![cont.line; prepare_body.len()], stmts: prepare_body, region: None },
            ret: None, qualifiers: CallableQualifiers::ORDINARY };
        let resume_name = self.fresh_seg();
        let mut resuming = loads(&frame);
        resuming.extend_from_slice(&body.stmts[*split + 1..]);
        resuming.extend(stores(&frame));
        resuming.push(Stmt::Expr(Expr::Bool(false)));
        let mut attributes = self.frame_attributes();
        attributes.push(crate::suspension::source_suspension_attribute(identity));
        self.segments.push(Function { line: cont.line, public: false, pure: false, comptime_only: false,
            attributes, name: resume_name.clone(), type_params: Vec::new(),
            params: carried.iter().filter(|local| !local.mutable).map(local_to_param)
                .chain(std::iter::once(Param { name: frame.clone(), ty: Some(frame_type.clone()),
                    convention: Convention::Var, default: None })).collect(),
            ret: Some(named("Bool")), body: Block { lines: vec![cont.line; resuming.len()],
                stmts: resuming, region: None }, bounds: vec![], is_gen: false, is_async: false });
        let mut resume_args = carried.iter().filter(|local| !local.mutable)
            .map(|local| Expr::Var(local.name.clone())).collect::<Vec<_>>();
        resume_args.push(Expr::Var(frame.clone()));
        let resume = Expr::Lambda { params: vec![Param { name: frame.clone(), ty: Some(frame_type.clone()),
            convention: Convention::Var, default: None }],
            body: tail_block_at(call(&resume_name, resume_args), cont.line), ret: Some(named("Bool")),
            qualifiers: CallableQualifiers::ORDINARY };
        let mut finishing = loads(&frame);
        finishing.push(Stmt::Expr(self.loop_exit(&lanes)));
        let finish = Expr::Lambda { params: vec![Param { name: frame.clone(), ty: Some(frame_type),
            convention: Convention::Let, default: None }], body: Block {
                lines: vec![cont.line; finishing.len()], stmts: finishing, region: None },
            ret: None, qualifiers: CallableQualifiers::ORDINARY };
        let initial = if lanes.is_empty() { call("list.repeat", vec![Expr::Int(0), Expr::Int(0)]) }
            else { Expr::List(lanes.iter().map(|lane| encode_lane(Expr::Var(lane.name.clone()), lane)).collect()) };
        Ok(Some(self.sequence_loop(call("task.__producer_frame", vec![initial, prepare, resume, finish]), lanes, cont)?))
    }
}
