use super::*;

impl Ctx<'_> {
    /// Lower a receive-until-closed loop onto the reusable one-way frame ABI.
    /// This is deliberately narrower than the ordinary segment loop: only a
    /// non-suspending body with scalar carried lanes is eligible.  The fallback
    /// remains the general recursive lowering, so roots, control escapes, and
    /// richer carried state keep their existing semantics.
    pub(super) fn try_lower_recv_frame_loop(
        &mut self,
        var: &str,
        src: &Expr,
        body: &Block,
        cont: Continuation<'_>,
    ) -> Result<Option<Expr>, String> {
        let body_bindings = crate::suspension::free_bindings_in_block(body, &HashSet::default());
        let body_uses_value = body_bindings.iter().any(|binding| binding == var);
        let scalar_captures = cont.scope.iter().filter(|local| {
            local.name != var && body_bindings.contains(&local.name)
        }).all(|local| matches!(local.ty.as_ref().map(Type::unqualified),
            Some(Type::Named(name, args)) if args.is_empty() && (name == "Int" || name == "Bool")));
        let typed_payload = crate::opt::enabled(crate::opt::Opt::ClosureElide) && scalar_captures && body_uses_value;
        if (typed_payload && (cont.rest.is_empty() || !matches!(src, Expr::Var(_))))
            || body.region.is_some()
            || body.stmts.iter().any(stmt_contains_await)
            || block_contains_return(body)
            || has_loop_escape(body)
            || (!typed_payload && (body_directly_matches_binding(body, var) || body_uses_value))
        {
            return Ok(None);
        }

        let mut used = crate::suspension::free_bindings(src);
        let mut body_bound = HashSet::default();
        body_bound.insert(var.to_string());
        used.extend(crate::suspension::free_bindings_in_block(body, &body_bound));
        for stmt in cont.rest {
            match stmt {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::Expr(value)
                | Stmt::Yield(value) => used.extend(crate::suspension::free_bindings(value)),
                Stmt::Return(Some(value)) => used.extend(crate::suspension::free_bindings(value)),
                _ => {}
            }
        }
        let used: HashSet<String> = used.into_iter().collect();
        let lanes: Vec<Local> = cont
            .scope
            .iter()
            .filter(|local| local.mutable && used.contains(&local.name))
            .filter(|local| {
                matches!(
                    local.ty.as_ref().map(Type::unqualified),
                    Some(Type::Named(name, args))
                        if args.is_empty() && (name == "Int" || name == "Bool")
                )
            })
            .cloned()
            .collect();
        // The reusable frame is the entire mutable carry set. Capturing a
        // mutable root that is not represented in it would let a ready drain
        // observe a stale value on the next iteration, so retain the ordinary
        // segment lowering for roots and other non-scalar state.
        if cont.scope.iter().any(|local| {
            local.mutable && used.contains(&local.name) && !lanes.iter().any(|lane| lane.name == local.name)
        }) {
            return Ok(None);
        }

        if typed_payload && lanes.iter().any(|lane| {
            lane.name == var || body.stmts.iter().any(|statement| {
                match statement {
                    Stmt::Let { name, .. } => name == &lane.name,
                    Stmt::LetPattern { pattern, .. } => {
                        let mut names = Vec::new();
                        pattern_binds(pattern, &mut names);
                        names.contains(&lane.name)
                    }
                    _ => false,
                }
            })
        }) {
            return Ok(None);
        }

        let frame = self.fresh_tmp();
        let option = self.fresh_tmp();
        let frame_ty = Type::Named("List".to_string(), vec![Type::Named("Int".to_string(), vec![])]);
        let frame_val = if lanes.is_empty() {
            call("list.repeat", vec![Expr::Int(0), Expr::Int(0)])
        } else {
            Expr::List(lanes.iter().map(|lane| encode_lane(Expr::Var(lane.name.clone()), lane)).collect())
        };
        let mut setup = vec![Stmt::Let {
            name: frame.clone(), ty: Some(frame_ty.clone()), mutable: false, value: frame_val,
        }];

        let mut some_stmts = Vec::new();
        for (index, lane) in lanes.iter().enumerate() {
            some_stmts.push(Stmt::Let {
                name: lane.name.clone(), ty: lane.ty.clone(), mutable: true,
                value: decode_lane(call("list.at", vec![Expr::Var(frame.clone()), Expr::Int(index as i64)]), lane),
            });
        }
        let mut some_lines = vec![first_line(&body.lines); some_stmts.len()];
        some_stmts.extend(body.stmts.iter().cloned());
        some_lines.extend((0..body.stmts.len()).map(|index| line_at(&body.lines, index)));
        for (index, lane) in lanes.iter().enumerate() {
            some_stmts.push(Stmt::Expr(call("list.set_at", vec![Expr::Var(frame.clone()), Expr::Int(index as i64), encode_lane(Expr::Var(lane.name.clone()), lane)])));
        }
        some_lines.extend(std::iter::repeat_n(body.lines.last().copied().unwrap_or(cont.line), lanes.len() + 1));
        some_stmts.push(Stmt::Expr(Expr::Bool(false)));
        let step_body = Expr::Match {
            scrutinee: Box::new(Expr::Var(option.clone())),
            arms: vec![
                MatchArm { line: 0, pattern: Pattern::Ctor { name: "Some".into(), args: vec![if typed_payload { Pattern::Var(var.to_string()) } else { Pattern::Wildcard }] }, guard: None, body: Expr::Block(Block { lines: some_lines, stmts: some_stmts, region: None }) },
                MatchArm { line: 0, pattern: Pattern::Ctor { name: "None".into(), args: vec![] }, guard: None, body: Expr::Bool(true) },
            ],
        };
        let step = self.fresh_tmp();
        setup.push(Stmt::Let {
            name: step.clone(), ty: None, mutable: false,
            value: Expr::Lambda {
                params: vec![
                    Param { name: option, ty: None, convention: if typed_payload { Convention::Own } else { Convention::Let }, default: None },
                    Param { name: frame.clone(), ty: Some(frame_ty.clone()), convention: Convention::Var, default: None },
                ],
                body: tail_block_at(step_body, first_line(&body.lines)), ret: Some(Type::Named("Bool".into(), vec![])), qualifiers: CallableQualifiers::ORDINARY,
            },
        });

        let done_frame = self.fresh_tmp();
        let mut done_stmts = Vec::new();
        for (index, lane) in lanes.iter().enumerate() {
            done_stmts.push(Stmt::Let {
                name: lane.name.clone(), ty: lane.ty.clone(), mutable: true,
                value: decode_lane(call("list.at", vec![Expr::Var(done_frame.clone()), Expr::Int(index as i64)]), lane),
            });
        }
        let done_expr = if typed_payload {
            done_stmts.push(Stmt::Expr(self.loop_exit(&lanes)));
            Expr::Block(Block { lines: vec![cont.line; done_stmts.len()], stmts: done_stmts, region: None })
        } else {
            done_stmts.extend(cont.rest.iter().cloned());
            self.go(&done_stmts, &vec![cont.line; done_stmts.len()], cont.scope, cont.tail)?
        };
        let done = self.fresh_tmp();
        setup.push(Stmt::Let {
            name: done.clone(), ty: None, mutable: false,
            value: Expr::Lambda {
                params: vec![Param { name: done_frame, ty: Some(frame_ty), convention: Convention::Let, default: None }],
                body: tail_block_at(done_expr, cont.line), ret: None, qualifiers: CallableQualifiers::ORDINARY,
            },
        });
        let arguments = vec![src.clone(), Expr::Var(frame), Expr::Var(step), Expr::Var(done)];
        setup.push(Stmt::Expr(if typed_payload {
            call("chan.recv_typed_frame_loop", arguments)
        } else {
            call("chan.recv_frame_loop", arguments)
        }));
        let execution = Expr::Block(Block { lines: vec![cont.line; setup.len()], stmts: setup, region: None });
        Ok(Some(if typed_payload { self.sequence_loop(execution, lanes, cont)? } else { execution }))
    }

}

fn has_loop_escape(block: &Block) -> bool {
    if block.stmts.iter().any(|statement| matches!(statement, Stmt::Break | Stmt::Continue)) {
        return true;
    }
    crate::ast::visit::visit_block(block, &mut |expression| {
        let direct = |block: &Block| block.stmts.iter()
            .any(|statement| matches!(statement, Stmt::Break | Stmt::Continue));
        let escape = match expression {
            Expr::If { then_block, else_block, .. } => direct(then_block) || else_block.as_ref().is_some_and(direct),
            Expr::Block(block) | Expr::Lambda { body: block, .. } | Expr::While { body: block, .. }
            | Expr::For { body: block, .. } | Expr::WhileLet { body: block, .. } => direct(block),
            _ => false,
        };
        if escape { Err(()) } else { Ok(()) }
    }).is_err()
}
