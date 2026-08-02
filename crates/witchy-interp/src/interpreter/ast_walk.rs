//! AST-walk helpers for the interpreter's optimization/aliasing analysis:
//! free-identifier collection over expressions/blocks, mention tests, and the
//! `<>` concat-spine detector. Pure syntactic walkers, no evaluator state.

use witchy_syntax::ast::{BinOp, Block, Expr, Stmt};

/// Walk every identifier an expression can possibly resolve through the
/// environment: variable reads, call names (a closure in a variable), method
/// names, assignment targets. Binders (params, patterns, loop variables) are
/// deliberately included-by-omission — we never report them, but we DO walk
/// the scopes they govern, so the scan over-approximates. Over-approximation
/// is safe for both users (closure capture keeps an extra binding; the
/// in-place fast path stands down).
pub(super) fn idents_in_expr(e: &Expr, f: &mut dyn FnMut(&str)) {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) => {}
        // No identifier children (its holes are unparsed source); gone before this runs.
        Expr::TaggedLit { .. } => {}
        Expr::Var(n) => f(n),
        Expr::List(items) | Expr::Tuple(items) => {
            for it in items {
                idents_in_expr(it, f);
            }
        }
        Expr::Call { name, args } => {
            f(name);
            for a in args {
                idents_in_expr(a, f);
            }
        }
        // (RFC-0056) Lowered before evaluation; recurse defensively (this scan
        // over-approximates, so a stray traversal here is harmless).
        Expr::LabeledCall { name, args } => {
            f(name);
            for (_, a) in args {
                idents_in_expr(a, f);
            }
        }
        Expr::LabeledMethodCall { receiver, method, args } => {
            f(method);
            idents_in_expr(receiver, f);
            for (_, a) in args {
                idents_in_expr(a, f);
            }
        }
        Expr::MethodCall { receiver, method, args } => {
            f(method);
            idents_in_expr(receiver, f);
            for a in args {
                idents_in_expr(a, f);
            }
        }
        Expr::Apply { func, args } => {
            idents_in_expr(func, f);
            for a in args {
                idents_in_expr(a, f);
            }
        }
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for a in args {
                idents_in_expr(a, f);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
            idents_in_expr(expr, f)
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            idents_in_expr(receiver, f);
            for arg in args {
                idents_in_expr(arg, f);
            }
        }
        Expr::Field { base, .. } => idents_in_expr(base, f),
        Expr::Lambda { body, .. } => idents_in_block(body, f),
        Expr::RecordUpdate { name: _, base, fields } => {
            idents_in_expr(base, f);
            for (_, fe) in fields {
                idents_in_expr(fe, f);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, fe) in fields {
                idents_in_expr(fe, f);
            }
            if let Some(s) = spread {
                idents_in_expr(s, f);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            idents_in_expr(lhs, f);
            idents_in_expr(rhs, f);
        }
        Expr::If { cond, then_block, else_block } => {
            idents_in_expr(cond, f);
            idents_in_block(then_block, f);
            if let Some(b) = else_block {
                idents_in_block(b, f);
            }
        }
        Expr::Match { scrutinee, arms } => {
            idents_in_expr(scrutinee, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    idents_in_expr(g, f);
                }
                idents_in_expr(&arm.body, f);
            }
        }
        Expr::Block(b) => idents_in_block(b, f),
        Expr::While { cond, body } => {
            idents_in_expr(cond, f);
            idents_in_block(body, f);
        }
        Expr::For { iter, body, .. } => {
            idents_in_expr(iter, f);
            idents_in_block(body, f);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            idents_in_expr(scrutinee, f);
            idents_in_block(body, f);
        }
        Expr::Range { lo, hi, .. } => {
            idents_in_expr(lo, f);
            idents_in_expr(hi, f);
        }
        Expr::Index { base, index } => {
            idents_in_expr(base, f);
            idents_in_expr(index, f);
        }
    }
}

pub(super) fn idents_in_block(b: &Block, f: &mut dyn FnMut(&str)) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::LetPattern { value, .. } => idents_in_expr(value, f),
            Stmt::Assign { name, value } => {
                f(name);
                idents_in_expr(value, f);
            }
            Stmt::Return(opt) => {
                if let Some(e) = opt {
                    idents_in_expr(e, f);
                }
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::Expr(e) | Stmt::Yield(e) => idents_in_expr(e, f),
        }
    }
}

/// Does the expression mention `name` anywhere it could resolve through the
/// environment? Conservative (over-approximates); used to guard the in-place
/// accumulation fast path.
pub(super) fn expr_mentions(e: &Expr, name: &str) -> bool {
    let mut found = false;
    idents_in_expr(e, &mut |n| {
        if n == name {
            found = true;
        }
    });
    found
}

/// If `e` is a `<>` chain whose leftmost operand is exactly `Var(name)`
/// (`name + a + b` parses left-associated), return the right operands in
/// evaluation order; otherwise None.
pub(super) fn concat_spine<'a>(mut e: &'a Expr, name: &str) -> Option<Vec<&'a Expr>> {
    let mut rights = Vec::new();
    loop {
        match e {
            Expr::Binary { op: BinOp::Add, lhs, rhs } => {
                rights.push(&**rhs);
                e = lhs;
            }
            Expr::Var(v) if v == name => {
                rights.reverse();
                return Some(rights);
            }
            _ => return None,
        }
    }
}
