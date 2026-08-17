//! Loan-root and loan-event collection: free functions that walk a function
//! body's AST alongside the type-checker's `LoanFacts` to gather the exact
//! owner-object roots to declare and the loan-event keys to assert. Root
//! extraction goes through `Codegen::loan_root`, which consumes the checked
//! `LoanOwnerRoot`; this walker never reconstructs ownership from a logical
//! view local or projected storage type. Split out of
//! `codegen/mod.rs` as an incremental break-up of that file; these are free
//! functions over the AST with only static `Codegen`/`LoanRoot` references.

use super::*;

pub(crate) fn collect_loan_roots(
    block: &Block,
    facts: &witchy_types::loans::LoanFacts,
    out: &mut Vec<LoanRoot>,
) -> Result<(), CodegenError> {
    for stmt in &block.stmts {
        for event in facts.opens_after(stmt) {
            if let Some(root) = Codegen::loan_root(event)? {
                out.push(root);
            }
        }
        // Loop-element loans are seeded from the iterator at the nested
        // block entry rather than opened by a statement in that body.  Their
        // owner roots must still have Wasm locals for retain/release lowering.
        for event in facts.active_at(stmt) {
            if let Some(root) = Codegen::loan_root(event)? {
                out.push(root);
            }
        }
        let value = match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Yield(value)
            | Stmt::Expr(value) => Some(value),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => None,
        };
        if let Some(value) = value {
            collect_loan_roots_expr(value, facts, out)?;
        }
    }
    Ok(())
}

pub(crate) fn collect_loan_event_keys(
    block: &Block,
    facts: &witchy_types::loans::LoanFacts,
    out: &mut HashSet<usize>,
) {
    for stmt in &block.stmts {
        if let Some(key) = facts.event_key(stmt) {
            out.insert(key);
        }
        let value = match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Yield(value)
            | Stmt::Expr(value) => Some(value),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => None,
        };
        if let Some(value) = value {
            collect_loan_event_keys_expr(value, facts, out);
        }
    }
}

fn collect_loan_event_keys_expr(
    expr: &Expr,
    facts: &witchy_types::loans::LoanFacts,
    out: &mut HashSet<usize>,
) {
    match expr {
        Expr::If { cond, then_block, else_block } => {
            collect_loan_event_keys_expr(cond, facts, out);
            collect_loan_event_keys(then_block, facts, out);
            if let Some(block) = else_block {
                collect_loan_event_keys(block, facts, out);
            }
        }
        Expr::While { cond, body } => {
            collect_loan_event_keys_expr(cond, facts, out);
            collect_loan_event_keys(body, facts, out);
        }
        Expr::For { iter, body, .. } => {
            collect_loan_event_keys_expr(iter, facts, out);
            collect_loan_event_keys(body, facts, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_loan_event_keys_expr(scrutinee, facts, out);
            collect_loan_event_keys(body, facts, out);
        }
        Expr::Block(block) => collect_loan_event_keys(block, facts, out),
        Expr::Match { scrutinee, arms } => {
            collect_loan_event_keys_expr(scrutinee, facts, out);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_loan_event_keys_expr(guard, facts, out);
                }
                collect_loan_event_keys_expr(&arm.body, facts, out);
            }
        }
        // A lambda is a separate compile unit with its own identity assertion.
        Expr::Lambda { .. } => {}
        Expr::List(items) | Expr::Tuple(items) => {
            items.iter().for_each(|item| collect_loan_event_keys_expr(item, facts, out));
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            args.iter().for_each(|arg| collect_loan_event_keys_expr(arg, facts, out));
        }
        Expr::LabeledCall { args, .. } => {
            args.iter().for_each(|(_, arg)| collect_loan_event_keys_expr(arg, facts, out));
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            collect_loan_event_keys_expr(receiver, facts, out);
            args.iter().for_each(|(_, arg)| collect_loan_event_keys_expr(arg, facts, out));
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            collect_loan_event_keys_expr(receiver, facts, out);
            args.iter().for_each(|arg| collect_loan_event_keys_expr(arg, facts, out));
        }
        Expr::Apply { func, args } => {
            collect_loan_event_keys_expr(func, facts, out);
            args.iter().for_each(|arg| collect_loan_event_keys_expr(arg, facts, out));
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => collect_loan_event_keys_expr(expr, facts, out),
        Expr::Binary { lhs, rhs, .. }
        | Expr::Index { base: lhs, index: rhs }
        | Expr::Range { lo: lhs, hi: rhs, .. } => {
            collect_loan_event_keys_expr(lhs, facts, out);
            collect_loan_event_keys_expr(rhs, facts, out);
        }
        Expr::RecordUpdate { base, fields, .. } => {
            collect_loan_event_keys_expr(base, facts, out);
            fields.iter().for_each(|(_, value)| collect_loan_event_keys_expr(value, facts, out));
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().for_each(|(_, value)| collect_loan_event_keys_expr(value, facts, out));
            if let Some(spread) = spread {
                collect_loan_event_keys_expr(spread, facts, out);
            }
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}

fn collect_loan_roots_expr(
    expr: &Expr,
    facts: &witchy_types::loans::LoanFacts,
    out: &mut Vec<LoanRoot>,
) -> Result<(), CodegenError> {
    match expr {
        Expr::If { cond, then_block, else_block } => {
            collect_loan_roots_expr(cond, facts, out)?;
            collect_loan_roots(then_block, facts, out)?;
            if let Some(block) = else_block {
                collect_loan_roots(block, facts, out)?;
            }
        }
        Expr::While { cond, body } => {
            collect_loan_roots_expr(cond, facts, out)?;
            collect_loan_roots(body, facts, out)?;
        }
        Expr::For { iter, body, .. } => {
            collect_loan_roots_expr(iter, facts, out)?;
            collect_loan_roots(body, facts, out)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_loan_roots_expr(scrutinee, facts, out)?;
            collect_loan_roots(body, facts, out)?;
        }
        Expr::Block(block) => collect_loan_roots(block, facts, out)?,
        Expr::Match { scrutinee, arms } => {
            collect_loan_roots_expr(scrutinee, facts, out)?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_loan_roots_expr(guard, facts, out)?;
                }
                collect_loan_roots_expr(&arm.body, facts, out)?;
            }
        }
        // A lambda is a separate compile unit with its own root locals.
        Expr::Lambda { .. } => {}
        Expr::List(items) | Expr::Tuple(items) => {
            for item in items {
                collect_loan_roots_expr(item, facts, out)?;
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for arg in args {
                collect_loan_roots_expr(arg, facts, out)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args {
                collect_loan_roots_expr(arg, facts, out)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            collect_loan_roots_expr(receiver, facts, out)?;
            for (_, arg) in args {
                collect_loan_roots_expr(arg, facts, out)?;
            }
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            collect_loan_roots_expr(receiver, facts, out)?;
            for arg in args {
                collect_loan_roots_expr(arg, facts, out)?;
            }
        }
        Expr::Apply { func, args } => {
            collect_loan_roots_expr(func, facts, out)?;
            for arg in args {
                collect_loan_roots_expr(arg, facts, out)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
            collect_loan_roots_expr(expr, facts, out)?;
        }
        Expr::Field { base, .. } => collect_loan_roots_expr(base, facts, out)?,
        Expr::Binary { lhs, rhs, .. }
        | Expr::Index { base: lhs, index: rhs }
        | Expr::Range { lo: lhs, hi: rhs, .. } => {
            collect_loan_roots_expr(lhs, facts, out)?;
            collect_loan_roots_expr(rhs, facts, out)?;
        }
        Expr::RecordUpdate { base, fields, .. } => {
            collect_loan_roots_expr(base, facts, out)?;
            for (_, value) in fields {
                collect_loan_roots_expr(value, facts, out)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                collect_loan_roots_expr(value, facts, out)?;
            }
            if let Some(spread) = spread {
                collect_loan_roots_expr(spread, facts, out)?;
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}
