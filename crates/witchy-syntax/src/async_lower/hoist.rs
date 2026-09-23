//! Nested `await` and `?` inside an async fn statement (RFC-0167 row K).
//!
//! The segment transform in the parent module suspends only at whole-statement
//! shapes: `let x = E.await`, a bare `E.await`, a tail `E.await`. Everything
//! else — `match f().await:`, `f().await + g().await`, `if f().await > 0:`,
//! `"${f().await}"`, `h(f().await)`, `f().await?` — is first rewritten here
//! into a run of fresh `let` temporaries bound just before the statement, in
//! evaluation order, so each suspension lands in a supported shape.
//!
//! Evaluation order is preserved: when a later operand suspends, every earlier
//! sibling that is not a literal or a plain local is spilled to its own
//! temporary first, so it is still evaluated before the suspension. A `?` in an
//! async fn cannot return from the continuation segment directly (the segment
//! returns a `Task`), so it becomes a match on the operand whose success arm
//! carries the rest of the statement sequence and whose failure arm returns the
//! failure as the fn's completed value.
//!
//! Positions that are evaluated conditionally are never hoisted above their
//! condition: an `await` in the right operand of `&&`/`||`, or inside an
//! `if`/`match` branch that is itself nested in a larger expression, is refused
//! with a targeted diagnostic. (A statement-level or tail `if`/`match` branch is
//! its own statement sequence and is lowered in place.)

use super::*;

/// One statement hoisted ahead of the statement being lowered.
pub(super) enum Hoisted {
    /// `let name = value`; `value` may be a whole-RHS `E.await`.
    Let { name: String, value: Expr },
    /// `?` on the local `scrutinee`: the remaining sequence continues in the
    /// success arm with the payload bound to `ok`; the failure arm returns the
    /// failure (`Err(e)` for a `Result`, `None` for an `Option`).
    Try { scrutinee: String, result: bool, ok: String, ok_ty: Option<Type> },
}

/// The hoisting state for one statement: what is bound before it, and the
/// types of the temporaries bound so far (a `?` on an awaited value needs the
/// awaited callee's declared result to know `Result` from `Option`).
#[derive(Default)]
struct Pending {
    out: Vec<Hoisted>,
    types: HashMap<String, Type>,
}

impl<'a> Ctx<'a> {
    /// Rewrite `stmt` so no `await`/`?` remains nested inside it. `None` when
    /// the statement is already in a supported shape (nothing to hoist), so the
    /// caller lowers it unchanged and any unsupported residue is refused there.
    pub(super) fn hoist_stmt(
        &mut self,
        stmt: &Stmt,
        scope: &[Local],
    ) -> Result<Option<(Vec<Hoisted>, Stmt)>, String> {
        if !stmt_needs(stmt) {
            return Ok(None);
        }
        let mut pending = Pending::default();
        let mut stmt = stmt.clone();
        match &mut stmt {
            Stmt::Let { value, .. } | Stmt::LetPattern { value, .. } | Stmt::Return(Some(value)) => {
                self.hoist_whole_rhs(value, scope, &mut pending)?
            }
            // An assignment has no suspension shape of its own: its awaited
            // right-hand side, too, becomes a temporary.
            Stmt::Assign { value, .. } => self.hoist_in_place(value, scope, &mut pending)?,
            Stmt::Expr(e) => match e {
                Expr::For { iter, .. } => self.hoist_in_place(iter, scope, &mut pending)?,
                Expr::If { cond, .. } => self.hoist_in_place(cond, scope, &mut pending)?,
                Expr::Match { scrutinee, .. } => self.hoist_in_place(scrutinee, scope, &mut pending)?,
                _ => self.hoist_whole_rhs(e, scope, &mut pending)?,
            },
            Stmt::Return(None) | Stmt::Yield(_) | Stmt::Break | Stmt::Continue => {}
        }
        if pending.out.is_empty() {
            return Ok(None);
        }
        Ok(Some((pending.out, stmt)))
    }

    /// A position where a whole `E.await` is itself supported: hoist only
    /// inside `E`.
    fn hoist_whole_rhs(&mut self, value: &mut Expr, scope: &[Local], pending: &mut Pending) -> Result<(), String> {
        match value {
            Expr::Unary { op: UnOp::Await, expr } => self.hoist_in_place(expr, scope, pending),
            _ => self.hoist_in_place(value, scope, pending),
        }
    }

    fn hoist_in_place(&mut self, e: &mut Expr, scope: &[Local], pending: &mut Pending) -> Result<(), String> {
        if !needs(e) {
            return Ok(());
        }
        match e {
            Expr::Unary { op: UnOp::Await, expr } => {
                self.hoist_in_place(expr, scope, pending)?;
                let name = self.fresh_tmp();
                if let Some(ty) = self.awaited_value_type(&**expr, scope) {
                    pending.types.insert(name.clone(), ty);
                }
                let value = std::mem::replace(e, Expr::Var(name.clone()));
                pending.out.push(Hoisted::Let { name, value });
            }
            Expr::Try(inner) => {
                self.hoist_in_place(inner, scope, pending)?;
                let Some((result, ok_ty)) = self.try_shape(&**inner, scope, pending) else {
                    return Err(self.err(
                        "`?` needs an operand whose `Result`/`Option` type is evident before type \
                         checking — bind the operand with an annotated `let` first",
                    ));
                };
                let scrutinee = match &**inner {
                    Expr::Var(name) => name.clone(),
                    _ => self.spill(&mut **inner, pending),
                };
                let ok = self.fresh_tmp();
                pending.out.push(Hoisted::Try { scrutinee, result, ok: ok.clone(), ok_ty });
                *e = Expr::Var(ok);
            }
            Expr::Binary { op: BinOp::And | BinOp::Or, lhs, rhs } => {
                if contains_await(rhs) {
                    return Err(self.err(
                        "`await` in the right operand of `&&`/`||` is not supported — it runs \
                         only when the left operand does not decide the result; nest an `if` \
                         instead",
                    ));
                }
                self.hoist_in_place(lhs, scope, pending)?;
            }
            Expr::If { cond, then_block, else_block } => {
                if block_contains_await(then_block) || else_block.as_ref().is_some_and(block_contains_await) {
                    return Err(self.err(
                        "`await` inside an `if` branch nested in a larger expression is not \
                         supported — make the `if` a statement of its own",
                    ));
                }
                self.hoist_in_place(cond, scope, pending)?;
            }
            Expr::Match { scrutinee, arms } => {
                if arms.iter().any(|arm| {
                    contains_await(&arm.body) || arm.guard.as_ref().is_some_and(contains_await)
                }) {
                    return Err(self.err(
                        "`await` inside a `match` arm nested in a larger expression is not \
                         supported — make the `match` a statement of its own",
                    ));
                }
                self.hoist_in_place(scrutinee, scope, pending)?;
            }
            _ => {
                let children = children_mut(e);
                let Some(last) = children.iter().rposition(|child| needs(child)) else {
                    return Ok(());
                };
                for (index, child) in children.into_iter().enumerate().take(last + 1) {
                    self.hoist_in_place(child, scope, pending)?;
                    // An earlier operand is evaluated before a later one
                    // suspends; a literal or plain local reads the same after.
                    if index < last && !trivial(child) {
                        self.spill(child, pending);
                    }
                }
            }
        }
        Ok(())
    }

    /// Bind `e` to a fresh temporary now, leaving the temporary in its place.
    fn spill(&mut self, e: &mut Expr, pending: &mut Pending) -> String {
        let name = self.fresh_tmp();
        let value = std::mem::replace(e, Expr::Var(name.clone()));
        pending.out.push(Hoisted::Let { name: name.clone(), value });
        name
    }

    /// Whether `?` on `operand` propagates a `Result` (`true`) or an `Option`
    /// (`false`), and its success payload type when known.
    fn try_shape(&self, operand: &Expr, scope: &[Local], pending: &Pending) -> Option<(bool, Option<Type>)> {
        let type_of = |e: &Expr| match e {
            Expr::Var(name) => pending.types.get(name).cloned().or_else(|| {
                scope.iter().rev().find(|local| &local.name == name).and_then(|local| local.ty.clone())
            }),
            _ => derive_type(e, scope, self.borrowed_shells),
        };
        // `e ? "msg"` always propagates a `Result(T, String)`.
        if let Expr::Call { name, args } = operand {
            if name == crate::intrinsics::TRY_CONTEXT && args.len() == 2 {
                return Some((true, type_of(&args[0]).and_then(|ty| propagated(&ty)).map(|(_, ok)| ok)));
            }
        }
        propagated(&type_of(operand)?).map(|(result, ok)| (result, Some(ok)))
    }
}

/// `Result(T, E)` -> `(true, T)`, `Option(T)` -> `(false, T)`.
fn propagated(ty: &Type) -> Option<(bool, Type)> {
    let Type::Named(name, args) = ty.unqualified() else {
        return None;
    };
    match (name.rsplit('.').next().unwrap_or(name), args.as_slice()) {
        ("Result", [ok, _]) => Some((true, ok.clone())),
        ("Option", [ok]) => Some((false, ok.clone())),
        _ => None,
    }
}

/// Splice the hoisted temporaries ahead of `stmt` and the rest of its
/// sequence. A `?` guard turns everything after it into its success arm.
pub(super) fn assemble(
    hoisted: Vec<Hoisted>,
    stmt: Stmt,
    line: u32,
    rest: &[Stmt],
    rest_lines: &[u32],
) -> (Vec<Stmt>, Vec<u32>) {
    let mut stmts = Vec::with_capacity(rest.len() + 1);
    let mut lines = Vec::with_capacity(rest.len() + 1);
    // A statement `x?` leaves only the payload temporary behind: a plain local
    // read, dropped unless it is the sequence's value.
    let bare_payload = matches!(&stmt, Stmt::Expr(Expr::Var(name)) if name.starts_with("__await"));
    if !(bare_payload && !rest.is_empty()) {
        stmts.push(stmt);
        lines.push(line);
    }
    stmts.extend_from_slice(rest);
    lines.extend_from_slice(rest_lines);
    for hoisted in hoisted.into_iter().rev() {
        match hoisted {
            Hoisted::Let { name, value } => {
                stmts.insert(0, Stmt::Let { name, ty: None, mutable: false, value });
                lines.insert(0, line);
            }
            Hoisted::Try { scrutinee, result, ok, ok_ty } => {
                let payload = format!("{ok}_payload");
                let failure = format!("{ok}_failure");
                let mut success_stmts = vec![Stmt::Let {
                    name: ok,
                    ty: ok_ty.filter(|ty| !mentions_type_variable(ty)),
                    mutable: false,
                    value: Expr::Var(payload.clone()),
                }];
                let mut success_lines = vec![line];
                success_stmts.append(&mut stmts);
                success_lines.append(&mut lines);
                let (success_ctor, failure_pattern, failure_value) = if result {
                    (
                        "Ok",
                        Pattern::Ctor { name: "Err".into(), args: vec![Pattern::Var(failure.clone())] },
                        Expr::Ctor { name: "Err".into(), args: vec![Expr::Var(failure)] },
                    )
                } else {
                    (
                        "Some",
                        Pattern::Ctor { name: "None".into(), args: vec![] },
                        Expr::Ctor { name: "None".into(), args: vec![] },
                    )
                };
                let guard = Expr::Match {
                    scrutinee: Box::new(Expr::Var(scrutinee)),
                    arms: vec![
                        MatchArm {
                            line,
                            pattern: Pattern::Ctor {
                                name: success_ctor.into(),
                                args: vec![Pattern::Var(payload)],
                            },
                            guard: None,
                            body: Expr::Block(Block { stmts: success_stmts, lines: success_lines, region: None }),
                        },
                        MatchArm {
                            line,
                            pattern: failure_pattern,
                            guard: None,
                            body: Expr::Block(Block {
                                stmts: vec![Stmt::Return(Some(failure_value))],
                                lines: vec![line],
                                region: None,
                            }),
                        },
                    ],
                };
                stmts = vec![Stmt::Expr(guard)];
                lines = vec![line];
            }
        }
    }
    (stmts, lines)
}

fn stmt_needs(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Let { value, .. } | Stmt::LetPattern { value, .. } | Stmt::Return(Some(value)) => {
            whole_rhs_needs(value)
        }
        Stmt::Assign { value, .. } => needs(value),
        Stmt::Expr(e) => match e {
            Expr::For { iter, .. } => as_recv_stream(iter).is_none() && needs(iter),
            Expr::If { cond, .. } => needs(cond),
            Expr::Match { scrutinee, .. } => needs(scrutinee),
            Expr::While { .. } | Expr::WhileLet { .. } | Expr::Block(_) => false,
            _ => whole_rhs_needs(e),
        },
        Stmt::Return(None) | Stmt::Yield(_) | Stmt::Break | Stmt::Continue => false,
    }
}

fn whole_rhs_needs(value: &Expr) -> bool {
    match value {
        Expr::Unary { op: UnOp::Await, expr } => needs(expr),
        _ => needs(value),
    }
}

/// Whether `e` has an `await` or `?` in an eagerly evaluated position (or an
/// `await` in a conditional position this pass must refuse by name). Closures,
/// loops and blocks are their own sequences and are left to the caller.
fn needs(e: &Expr) -> bool {
    match e {
        Expr::Unary { op: UnOp::Await, .. } | Expr::Try(_) => true,
        Expr::Binary { op: BinOp::And | BinOp::Or, lhs, rhs } => needs(lhs) || contains_await(rhs),
        Expr::If { cond, then_block, else_block } => {
            needs(cond)
                || block_contains_await(then_block)
                || else_block.as_ref().is_some_and(block_contains_await)
        }
        Expr::Match { scrutinee, arms } => {
            needs(scrutinee)
                || arms.iter().any(|arm| {
                    contains_await(&arm.body) || arm.guard.as_ref().is_some_and(contains_await)
                })
        }
        Expr::Block(_)
        | Expr::While { .. }
        | Expr::For { .. }
        | Expr::WhileLet { .. }
        | Expr::Lambda { .. } => false,
        _ => children(e).into_iter().any(needs),
    }
}

/// A value that reads the same whether evaluated before or after a later
/// operand suspends: a literal, a plain local, or a unary view/negation of one.
fn trivial(e: &Expr) -> bool {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) => true,
        Expr::Unary { op, expr } => !matches!(op, UnOp::Await) && trivial(expr),
        _ => false,
    }
}

/// The eagerly evaluated operands of an ordinary expression, in evaluation
/// order. Conditional and sequencing forms are handled by name in `needs`.
fn children(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Unary { expr, .. }
        | Expr::Field { base: expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => vec![&**expr],
        Expr::Index { base, index } => vec![&**base, &**index],
        Expr::Binary { lhs, rhs, .. } => vec![&**lhs, &**rhs],
        Expr::Range { lo, hi, .. } => vec![&**lo, &**hi],
        Expr::List(items) | Expr::Tuple(items) => items.iter().collect(),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => args.iter().collect(),
        Expr::LabeledCall { args, .. } => args.iter().map(|(_, arg)| arg).collect(),
        Expr::LabeledMethodCall { receiver, args, .. } => {
            std::iter::once(&**receiver).chain(args.iter().map(|(_, arg)| arg)).collect()
        }
        Expr::MethodCall { receiver, args, .. } | Expr::ExistentialCall { receiver, args, .. } => {
            std::iter::once(&**receiver).chain(args.iter()).collect()
        }
        Expr::Apply { func, args } => std::iter::once(&**func).chain(args.iter()).collect(),
        Expr::RecordUpdate { base, fields, .. } => {
            std::iter::once(&**base).chain(fields.iter().map(|(_, value)| value)).collect()
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().map(|(_, value)| value).chain(spread.as_deref()).collect()
        }
        _ => Vec::new(),
    }
}

/// [`children`], mutably and in the same order.
fn children_mut(e: &mut Expr) -> Vec<&mut Expr> {
    match e {
        Expr::Unary { expr, .. }
        | Expr::Field { base: expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => vec![&mut **expr],
        Expr::Index { base, index } => vec![&mut **base, &mut **index],
        Expr::Binary { lhs, rhs, .. } => vec![&mut **lhs, &mut **rhs],
        Expr::Range { lo, hi, .. } => vec![&mut **lo, &mut **hi],
        Expr::List(items) | Expr::Tuple(items) => items.iter_mut().collect(),
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            args.iter_mut().collect()
        }
        Expr::LabeledCall { args, .. } => args.iter_mut().map(|(_, arg)| arg).collect(),
        Expr::LabeledMethodCall { receiver, args, .. } => {
            std::iter::once(&mut **receiver).chain(args.iter_mut().map(|(_, arg)| arg)).collect()
        }
        Expr::MethodCall { receiver, args, .. } | Expr::ExistentialCall { receiver, args, .. } => {
            std::iter::once(&mut **receiver).chain(args.iter_mut()).collect()
        }
        Expr::Apply { func, args } => std::iter::once(&mut **func).chain(args.iter_mut()).collect(),
        Expr::RecordUpdate { base, fields, .. } => {
            std::iter::once(&mut **base).chain(fields.iter_mut().map(|(_, value)| value)).collect()
        }
        Expr::Record { fields, spread, .. } => fields
            .iter_mut()
            .map(|(_, value)| value)
            .chain(spread.as_deref_mut())
            .collect(),
        _ => Vec::new(),
    }
}
