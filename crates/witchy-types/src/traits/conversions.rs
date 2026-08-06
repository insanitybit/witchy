//! `From`/`TryFrom` error-conversion rewriting for trait lowering.
//!
//! Extracted verbatim from `traits.rs` (RFC-0046 typed trait dispatch). When a
//! `?` propagates an error whose type differs from the enclosing function's
//! `Result` error type, and a `from`-style conversion function exists between
//! them, the `?` operand is wrapped in `result.map_err(|e| convert(e))` so the
//! error type lines up. `rewrite_try_from_conversions` is the sole entry point.

use foldhash::HashSet;

use witchy_syntax::ast::*;

#[derive(Clone)]
struct FromConversion {
    src: Type,
    dst: Type,
    func: String,
}

fn build_from_conversions(
    items: &[Item],
    from_conversion_fns: &HashSet<String>,
) -> Vec<FromConversion> {
    items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) if from_conversion_fns.contains(&f.name) && f.params.len() == 1 => {
                Some(FromConversion {
                    src: f.params[0].ty.clone()?,
                    dst: f.ret.clone()?,
                    func: f.name.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

fn result_error_from_ret(ret: &Option<Type>) -> Option<Type> {
    match ret {
        Some(Type::Named(n, args)) if n == "Result" && args.len() == 2 => Some(args[1].clone()),
        _ => None,
    }
}

fn result_error_from_ty(ty: &crate::typeck::Ty) -> Option<Type> {
    match crate::typeck::ty_to_ast(ty)? {
        Type::Named(n, args) if n == "Result" && args.len() == 2 => Some(args[1].clone()),
        _ => None,
    }
}

pub(super) fn rewrite_try_from_conversions(
    items: &mut [Item],
    table: &crate::typeck::TypeTable,
    from_conversion_fns: &HashSet<String>,
) {
    let conversions = build_from_conversions(items, from_conversion_fns);
    if conversions.is_empty() {
        return;
    }
    for item in items {
        let Item::Function(f) = item else { continue };
        let Some(dst_err) = result_error_from_ret(&f.ret) else { continue };
        rewrite_try_from_block(&mut f.body, &dst_err, &conversions, table);
    }
}

fn rewrite_try_from_block(
    block: &mut Block,
    dst_err: &Type,
    conversions: &[FromConversion],
    table: &crate::typeck::TypeTable,
) {
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Yield(value)
            | Stmt::Expr(value) => rewrite_try_from_expr(value, dst_err, conversions, table),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn rewrite_try_from_expr(
    expr: &mut Expr,
    dst_err: &Type,
    conversions: &[FromConversion],
    table: &crate::typeck::TypeTable,
) {
    match expr {
        Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
        | Expr::AnonCtor { args: xs, .. }
        | Expr::Call { args: xs, .. } => {
            for x in xs {
                rewrite_try_from_expr(x, dst_err, conversions, table);
            }
        }
        Expr::Apply { func, args } => {
            rewrite_try_from_expr(func, dst_err, conversions, table);
            for arg in args {
                rewrite_try_from_expr(arg, dst_err, conversions, table);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            rewrite_try_from_expr(receiver, dst_err, conversions, table);
            for arg in args {
                rewrite_try_from_expr(arg, dst_err, conversions, table);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            rewrite_try_from_expr(receiver, dst_err, conversions, table);
            for (_, arg) in args {
                rewrite_try_from_expr(arg, dst_err, conversions, table);
            }
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            rewrite_try_from_expr(receiver, dst_err, conversions, table);
            for arg in args {
                rewrite_try_from_expr(arg, dst_err, conversions, table);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_try_from_expr(lhs, dst_err, conversions, table);
            rewrite_try_from_expr(rhs, dst_err, conversions, table);
        }
        Expr::Unary { expr, .. }
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => {
            rewrite_try_from_expr(expr, dst_err, conversions, table);
        }
        Expr::Try(inner) => {
            rewrite_try_from_expr(inner, dst_err, conversions, table);
            let Some(src_err) = table.type_of(inner).and_then(result_error_from_ty) else {
                return;
            };
            if &src_err == dst_err {
                return;
            }
            let Some(conv) = conversions.iter().find(|c| c.src == src_err && c.dst == *dst_err) else {
                return;
            };
            let err = "__try_err".to_string();
            let operand = std::mem::replace(inner.as_mut(), Expr::Bool(false));
            **inner = Expr::Call {
                name: "result.map_err".to_string(),
                args: vec![
                    operand,
                    Expr::Lambda {
                        params: vec![Param {
                            name: err.clone(),
                            ty: Some(src_err),
                            convention: Convention::default(),
                            default: None,
                        }],
                        body: Block {
                            stmts: vec![Stmt::Expr(Expr::Call {
                                name: conv.func.clone(),
                                args: vec![Expr::Var(err)],
                            })],
                            lines: vec![0],
                            region: None,
                        },
                        ret: Some(dst_err.clone()),
                    },
                ],
            };
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_try_from_expr(lo, dst_err, conversions, table);
            rewrite_try_from_expr(hi, dst_err, conversions, table);
        }
        Expr::Index { base, index } => {
            rewrite_try_from_expr(base, dst_err, conversions, table);
            rewrite_try_from_expr(index, dst_err, conversions, table);
        }
        Expr::LabeledCall { .. } => {
            unreachable!("RFC-0056: labeled calls are lowered to positional Call before traits")
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewrite_try_from_expr(value, dst_err, conversions, table);
            }
            if let Some(spread) = spread {
                rewrite_try_from_expr(spread, dst_err, conversions, table);
            }
        }
        Expr::RecordUpdate { base, fields, .. } => {
            rewrite_try_from_expr(base, dst_err, conversions, table);
            for (_, value) in fields {
                rewrite_try_from_expr(value, dst_err, conversions, table);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            rewrite_try_from_expr(cond, dst_err, conversions, table);
            rewrite_try_from_block(then_block, dst_err, conversions, table);
            if let Some(block) = else_block {
                rewrite_try_from_block(block, dst_err, conversions, table);
            }
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_try_from_expr(scrutinee, dst_err, conversions, table);
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    rewrite_try_from_expr(guard, dst_err, conversions, table);
                }
                rewrite_try_from_expr(&mut arm.body, dst_err, conversions, table);
            }
        }
        Expr::While { cond, body } => {
            rewrite_try_from_expr(cond, dst_err, conversions, table);
            rewrite_try_from_block(body, dst_err, conversions, table);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rewrite_try_from_expr(scrutinee, dst_err, conversions, table);
            rewrite_try_from_block(body, dst_err, conversions, table);
        }
        Expr::For { iter, body, .. } => {
            rewrite_try_from_expr(iter, dst_err, conversions, table);
            rewrite_try_from_block(body, dst_err, conversions, table);
        }
        Expr::Lambda { body, ret, .. } => {
            if let Some(lambda_dst) = result_error_from_ret(ret) {
                rewrite_try_from_block(body, &lambda_dst, conversions, table);
            }
        }
        Expr::Block(body) => {
            rewrite_try_from_block(body, dst_err, conversions, table);
        }
        Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
        | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}
