//! Generic read-only and mutating expression visitors over the AST.
//!
//! ONE recursion skeleton, error-generic, shared by every pass that needs
//! "call me on every Expr" (glamour metadata scans, existential checks, ...).
//! Before this module the identical ~290-line family was copy-pasted per
//! consumer and the copies drifted variant-by-variant; add new Expr variants
//! HERE and every consumer inherits the traversal.

use super::{Block, Expr, Item, Module, Stmt};


pub fn visit_module_exprs<E>(
    module: &Module,
    visitor: &mut impl FnMut(&Expr) -> Result<(), E>,
) -> Result<(), E> {
    for item in &module.items {
        match item {
            Item::Function(function) => visit_block(&function.body, visitor)?,
            Item::Impl(definition) => {
                for method in &definition.methods {
                    visit_block(&method.body, visitor)?;
                }
            }
            Item::Trait(definition) => {
                for method in &definition.methods {
                    if let Some(body) = &method.default {
                        visit_block(body, visitor)?;
                    }
                }
            }
            Item::Const { value, .. } => visit_expr(value, visitor)?,
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(())
}

pub fn visit_block<E>(
    block: &Block,
    visitor: &mut impl FnMut(&Expr) -> Result<(), E>,
) -> Result<(), E> {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => visit_expr(value, visitor)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

pub fn visit_expr<E>(
    expression: &Expr,
    visitor: &mut impl FnMut(&Expr) -> Result<(), E>,
) -> Result<(), E> {
    visitor(expression)?;
    match expression {
        Expr::List(values) | Expr::Tuple(values) => {
            for value in values {
                visit_expr(value, visitor)?;
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for argument in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            visit_expr(receiver, visitor)?;
            for (_, argument) in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            visit_expr(receiver, visitor)?;
            for argument in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::Apply { func, args } => {
            visit_expr(func, visitor)?;
            for argument in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => visit_expr(expr, visitor)?,
        Expr::Field { base, .. } => visit_expr(base, visitor)?,
        Expr::Lambda { body, .. } | Expr::Block(body) => visit_block(body, visitor)?,
        Expr::RecordUpdate { base, fields, .. } => {
            visit_expr(base, visitor)?;
            for (_, value) in fields {
                visit_expr(value, visitor)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                visit_expr(value, visitor)?;
            }
            if let Some(spread) = spread {
                visit_expr(spread, visitor)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit_expr(lhs, visitor)?;
            visit_expr(rhs, visitor)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            visit_expr(cond, visitor)?;
            visit_block(then_block, visitor)?;
            if let Some(else_block) = else_block {
                visit_block(else_block, visitor)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            visit_expr(scrutinee, visitor)?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_expr(guard, visitor)?;
                }
                visit_expr(&arm.body, visitor)?;
            }
        }
        Expr::While { cond, body } => {
            visit_expr(cond, visitor)?;
            visit_block(body, visitor)?;
        }
        Expr::For { iter, body, .. } => {
            visit_expr(iter, visitor)?;
            visit_block(body, visitor)?;
        }
        Expr::Range { lo, hi, .. } => {
            visit_expr(lo, visitor)?;
            visit_expr(hi, visitor)?;
        }
        Expr::Index { base, index } => {
            visit_expr(base, visitor)?;
            visit_expr(index, visitor)?;
        }
        Expr::WhileLet {
            scrutinee, body, ..
        } => {
            visit_expr(scrutinee, visitor)?;
            visit_block(body, visitor)?;
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

pub fn visit_block_mut<E>(
    block: &mut Block,
    visitor: &mut impl FnMut(&mut Expr) -> Result<(), E>,
) -> Result<(), E> {
    for statement in &mut block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => visit_expr_mut(value, visitor)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

pub fn visit_expr_mut<E>(
    expression: &mut Expr,
    visitor: &mut impl FnMut(&mut Expr) -> Result<(), E>,
) -> Result<(), E> {
    visitor(expression)?;
    match expression {
        Expr::List(values) | Expr::Tuple(values) => {
            for value in values {
                visit_expr_mut(value, visitor)?;
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for argument in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            visit_expr_mut(receiver, visitor)?;
            for (_, argument) in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            visit_expr_mut(receiver, visitor)?;
            for argument in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::Apply { func, args } => {
            visit_expr_mut(func, visitor)?;
            for argument in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => visit_expr_mut(expr, visitor)?,
        Expr::Field { base, .. } => visit_expr_mut(base, visitor)?,
        Expr::Lambda { body, .. } | Expr::Block(body) => visit_block_mut(body, visitor)?,
        Expr::RecordUpdate { base, fields, .. } => {
            visit_expr_mut(base, visitor)?;
            for (_, value) in fields {
                visit_expr_mut(value, visitor)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                visit_expr_mut(value, visitor)?;
            }
            if let Some(spread) = spread {
                visit_expr_mut(spread, visitor)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit_expr_mut(lhs, visitor)?;
            visit_expr_mut(rhs, visitor)?;
        }
        Expr::If { cond, then_block, else_block } => {
            visit_expr_mut(cond, visitor)?;
            visit_block_mut(then_block, visitor)?;
            if let Some(else_block) = else_block {
                visit_block_mut(else_block, visitor)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            visit_expr_mut(scrutinee, visitor)?;
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    visit_expr_mut(guard, visitor)?;
                }
                visit_expr_mut(&mut arm.body, visitor)?;
            }
        }
        Expr::While { cond, body } => {
            visit_expr_mut(cond, visitor)?;
            visit_block_mut(body, visitor)?;
        }
        Expr::For { iter, body, .. } => {
            visit_expr_mut(iter, visitor)?;
            visit_block_mut(body, visitor)?;
        }
        Expr::Range { lo, hi, .. } => {
            visit_expr_mut(lo, visitor)?;
            visit_expr_mut(hi, visitor)?;
        }
        Expr::Index { base, index } => {
            visit_expr_mut(base, visitor)?;
            visit_expr_mut(index, visitor)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            visit_expr_mut(scrutinee, visitor)?;
            visit_block_mut(body, visitor)?;
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
