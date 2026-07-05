//! Lower named-field record construction (`Expr::Record`) to the positional
//! forms the rest of the compiler understands, using each record type's declared
//! field order:
//!   `Point(x: 1, y: 2)`      -> `Point(1, 2)`             (a positional `Ctor`)
//!   `Point(x: 5, ..p)`       -> a `RecordUpdate` of `p` with `x = 5`
//! Runs before type-checking and codegen, so those stages only ever see plain
//! constructors and record updates. The formatter, which never lowers, keeps the
//! `Record` node and prints the named form back.

use crate::ast::*;
use std::collections::{HashMap, HashSet};

type Orders = HashMap<String, Vec<String>>;

/// Rewrite every `Expr::Record` in the module. Errors on an unknown record type,
/// a field that the type doesn't declare, a repeated field, or (for construction
/// without a spread) a missing field. A no-op for modules with no named-field
/// construction.
pub fn lower(mut module: Module) -> Result<Module, String> {
    // `derive(...)` expands FIRST (additive impl items), so the generated
    // impls flow through every later stage exactly like handwritten ones.
    // This is the single funnel every entry point (link, check, compile,
    // interpret) already passes through.
    crate::derive::expand(&mut module)?;
    let orders = field_orders(&module);
    for item in &mut module.items {
        match item {
            Item::Function(f) => lower_block(&mut f.body, &orders)?,
            Item::Impl(im) => {
                for meth in &mut im.methods {
                    lower_block(&mut meth.body, &orders)?;
                }
            }
            Item::Const { value, .. } => lower_expr(value, &orders)?,
            Item::Type(_) | Item::Trait(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(module)
}

/// Record-constructor name -> its declared field names, in order.
fn field_orders(module: &Module) -> Orders {
    let mut out = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            for v in &t.variants {
                if !v.field_names.is_empty() {
                    out.insert(v.name.clone(), v.field_names.clone());
                }
            }
        }
    }
    out
}

/// Reorder named `fields` to the constructor's declared order, or fold a spread
/// into a `RecordUpdate`.
fn build(
    name: String,
    fields: Vec<(String, Expr)>,
    spread: Option<Box<Expr>>,
    orders: &Orders,
) -> Result<Expr, String> {
    // Both construction and spread must name a real record type and give each field
    // at most once — enforced identically. Skipping this on the spread path let a
    // repeated override (`Point(x: 7, x: 8, ..p)`) reach the backends unchecked, where
    // interpreter and compiled disagreed on which wins (last vs first) — a silent
    // parity divergence — and let an unknown type name (`Bogus(x: 9, ..p)`) through.
    let Some(decl) = orders.get(&name) else {
        return Err(format!(
            "`{name}(field: value, ...)` is named-field construction, but `{name}` is not a record type"
        ));
    };
    let mut seen = HashSet::new();
    for (fname, _) in &fields {
        if !decl.iter().any(|d| d == fname) {
            return Err(format!("record `{name}` has no field `{fname}`"));
        }
        if !seen.insert(fname.clone()) {
            return Err(format!("field `{fname}` is set twice in `{name}(...)`"));
        }
    }
    if let Some(base) = spread {
        // The base supplies every field not overridden here, so no missing-field check.
        return Ok(Expr::RecordUpdate { base, fields });
    }
    let mut by_name: HashMap<String, Expr> = fields.into_iter().collect();
    let mut args = Vec::with_capacity(decl.len());
    for field in decl {
        match by_name.remove(field) {
            Some(v) => args.push(v),
            None => {
                return Err(format!("`{name}(...)` is missing field `{field}`"));
            }
        }
    }
    Ok(Expr::Ctor { name, args })
}

fn lower_block(b: &mut Block, orders: &Orders) -> Result<(), String> {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Expr(value)
            | Stmt::Yield(value)
            | Stmt::Return(Some(value)) => lower_expr(value, orders)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn lower_expr(e: &mut Expr, orders: &Orders) -> Result<(), String> {
    match e {
        Expr::Record { name, fields, spread } => {
            for (_, v) in fields.iter_mut() {
                lower_expr(v, orders)?;
            }
            if let Some(s) = spread {
                lower_expr(s, orders)?;
            }
            *e = build(name.clone(), std::mem::take(fields), spread.take(), orders)?;
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        Expr::List(xs)
        | Expr::Tuple(xs)
        | Expr::Call { args: xs, .. }
        | Expr::Ctor { args: xs, .. } => {
            for x in xs {
                lower_expr(x, orders)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, x) in args {
                lower_expr(x, orders)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            lower_expr(receiver, orders)?;
            for a in args {
                lower_expr(a, orders)?;
            }
        }
        Expr::Apply { func, args } => {
            lower_expr(func, orders)?;
            for a in args {
                lower_expr(a, orders)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => lower_expr(expr, orders)?,
        Expr::Index { base, index } => {
            lower_expr(base, orders)?;
            lower_expr(index, orders)?;
        }
        Expr::Range { lo, hi, .. } => {
            lower_expr(lo, orders)?;
            lower_expr(hi, orders)?;
        }
        Expr::RecordUpdate { base, fields } => {
            lower_expr(base, orders)?;
            for (_, v) in fields {
                lower_expr(v, orders)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            lower_expr(lhs, orders)?;
            lower_expr(rhs, orders)?;
        }
        Expr::If { cond, then_block, else_block } => {
            lower_expr(cond, orders)?;
            lower_block(then_block, orders)?;
            if let Some(b) = else_block {
                lower_block(b, orders)?;
            }
        }
        Expr::While { cond, body } => {
            lower_expr(cond, orders)?;
            lower_block(body, orders)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            lower_expr(scrutinee, orders)?;
            lower_block(body, orders)?;
        }
        Expr::For { iter, body, .. } => {
            lower_expr(iter, orders)?;
            lower_block(body, orders)?;
        }
        Expr::Match { scrutinee, arms } => {
            lower_expr(scrutinee, orders)?;
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    lower_expr(g, orders)?;
                }
                lower_expr(&mut arm.body, orders)?;
            }
        }
        Expr::Lambda { body, .. } => lower_block(body, orders)?,
        Expr::Block(b) => lower_block(b, orders)?,
    }
    Ok(())
}
