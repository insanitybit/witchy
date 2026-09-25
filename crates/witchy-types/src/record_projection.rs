//! RFC-0098 checked structural-record projection lowering.
//!
//! Type checking authenticates each richer-to-poorer conversion against exact
//! anonymous-record identities and records it by expression identity. This pass
//! consumes those facts once, before the interpreter/Wasm split, and rewrites a
//! projection to ordinary typed language constructs:
//!
//! 1. evaluate the source into one compiler-private temporary;
//! 2. read only the target fields, in the target declaration order; and
//! 3. construct the exact target anonymous-record type.
//!
//! Backends therefore cannot rediscover width conformance from field names or
//! relabel one record layout as another.

use witchy_syntax::ast::{Block, Expr, Item, Module, Stmt, Type};

use crate::typeck::{TypeTable, TypedModule, annotate_checked, ty_to_ast};

pub(crate) fn lower_explicit_projections(typed: TypedModule) -> Result<TypedModule, String> {
    let expected = typed.table().record_projection_count();
    if expected == 0 {
        return Ok(typed);
    }

    let mut counter = 0usize;
    let (module, _stale_table, result) = typed.rewrite_into_module(|table, module| {
        rewrite_module(module, table, &mut counter)
    });
    let rewritten = result?;
    if rewritten != expected {
        return Err(format!(
            "record projection lowering consumed {rewritten} checked fact(s), expected {expected}"
        ));
    }
    annotate_checked(module).map_err(|error| {
        format!("record projection lowering produced an invalid typed program: {error}")
    })
}

fn projection_request(table: &TypeTable, expr: &Expr) -> Result<Option<(Type, Type)>, String> {
    let Some((target, source)) = table.record_projection(expr) else {
        return Ok(None);
    };
    let target = ty_to_ast(target)
        .ok_or_else(|| "record projection requires one fully resolved target type".to_string())?;
    let source = ty_to_ast(source)
        .ok_or_else(|| "record projection requires one fully resolved source type".to_string())?;
    Ok(Some((target, source)))
}

fn rewrite_module(
    module: &mut Module,
    table: &TypeTable,
    counter: &mut usize,
) -> Result<usize, String> {
    let mut rewritten = 0;
    for item in &mut module.items {
        match item {
            Item::Function(function) => {
                rewritten += rewrite_block(&mut function.body, table, counter)?;
            }
            Item::Trait(definition) => {
                for method in &mut definition.methods {
                    if let Some(default) = &mut method.default {
                        rewritten += rewrite_block(default, table, counter)?;
                    }
                }
            }
            Item::Impl(definition) => {
                for method in &mut definition.methods {
                    rewritten += rewrite_block(&mut method.body, table, counter)?;
                }
            }
            Item::Const { value, .. } => {
                rewritten += rewrite_expr(value, table, counter)?;
            }
            Item::Comptime(block) => {
                rewritten += rewrite_block(block, table, counter)?;
            }
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
    Ok(rewritten)
}

fn rewrite_block(
    block: &mut Block,
    table: &TypeTable,
    counter: &mut usize,
) -> Result<usize, String> {
    let mut rewritten = 0;
    for statement in &mut block.stmts {
        let expression = match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => Some(value),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => None,
        };
        if let Some(expression) = expression {
            rewritten += rewrite_expr(expression, table, counter)?;
        }
    }
    Ok(rewritten)
}

fn rewrite_expr(
    expression: &mut Expr,
    table: &TypeTable,
    counter: &mut usize,
) -> Result<usize, String> {
    let request = projection_request(table, expression)?;
    let mut rewritten = 0;

    match expression {
        Expr::List(items)
        | Expr::Tuple(items)
        | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                rewritten += rewrite_expr(item, table, counter)?;
            }
        }
        Expr::Call { args, .. } => {
            for argument in args {
                rewritten += rewrite_expr(argument, table, counter)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            rewritten += rewrite_expr(receiver, table, counter)?;
            for (_, argument) in args {
                rewritten += rewrite_expr(argument, table, counter)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                rewritten += rewrite_expr(argument, table, counter)?;
            }
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            rewritten += rewrite_expr(receiver, table, counter)?;
            for argument in args {
                rewritten += rewrite_expr(argument, table, counter)?;
            }
        }
        Expr::Apply { func, args } => {
            rewritten += rewrite_expr(func, table, counter)?;
            for argument in args {
                rewritten += rewrite_expr(argument, table, counter)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => {
            rewritten += rewrite_expr(expr, table, counter)?;
        }
        Expr::Lambda { body, .. } | Expr::Block(body) => {
            rewritten += rewrite_block(body, table, counter)?;
        }
        Expr::RecordUpdate { base, fields, .. } => {
            rewritten += rewrite_expr(base, table, counter)?;
            for (_, value) in fields {
                rewritten += rewrite_expr(value, table, counter)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewritten += rewrite_expr(value, table, counter)?;
            }
            if let Some(spread) = spread {
                rewritten += rewrite_expr(spread, table, counter)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewritten += rewrite_expr(lhs, table, counter)?;
            rewritten += rewrite_expr(rhs, table, counter)?;
        }
        Expr::If { cond, then_block, else_block } => {
            rewritten += rewrite_expr(cond, table, counter)?;
            rewritten += rewrite_block(then_block, table, counter)?;
            if let Some(else_block) = else_block {
                rewritten += rewrite_block(else_block, table, counter)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            rewritten += rewrite_expr(scrutinee, table, counter)?;
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    rewritten += rewrite_expr(guard, table, counter)?;
                }
                rewritten += rewrite_expr(&mut arm.body, table, counter)?;
            }
        }
        Expr::While { cond, body } => {
            rewritten += rewrite_expr(cond, table, counter)?;
            rewritten += rewrite_block(body, table, counter)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rewritten += rewrite_expr(scrutinee, table, counter)?;
            rewritten += rewrite_block(body, table, counter)?;
        }
        Expr::For { iter, body, .. } => {
            rewritten += rewrite_expr(iter, table, counter)?;
            rewritten += rewrite_block(body, table, counter)?;
        }
        Expr::Range { lo, hi, .. } => {
            rewritten += rewrite_expr(lo, table, counter)?;
            rewritten += rewrite_expr(hi, table, counter)?;
        }
        Expr::Index { base, index } => {
            rewritten += rewrite_expr(base, table, counter)?;
            rewritten += rewrite_expr(index, table, counter)?;
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }

    if let Some((target, source)) = request {
        let Type::Named(target_name, _) = target.unqualified() else {
            return Err("record projection target is not one exact named record".to_string());
        };
        let Type::Named(source_name, _) = source.unqualified() else {
            return Err("record projection source is not one exact named record".to_string());
        };
        let target_fields = witchy_syntax::ast::anon_record_field_names(target_name)
            .ok_or_else(|| "record projection target lost anonymous-record identity".to_string())?;
        let source_fields = witchy_syntax::ast::anon_record_field_names(source_name)
            .ok_or_else(|| "record projection source lost anonymous-record identity".to_string())?;
        if target_fields.len() >= source_fields.len()
            || !target_fields.iter().all(|field| source_fields.contains(field))
        {
            return Err("unchecked or malformed structural record projection fact".to_string());
        }

        let temp = format!("__record_projection_{}", *counter);
        *counter += 1;
        let old = std::mem::replace(expression, Expr::Bool(false));
        let payload = match old {
            Expr::As { expr, .. } => *expr,
            other => other,
        };
        let args = target_fields
            .iter()
            .map(|field| Expr::Field {
                base: Box::new(Expr::Var(temp.clone())),
                field: field.clone(),
            })
            .collect();
        *expression = Expr::Block(Block {
            stmts: vec![
                Stmt::Let {
                    name: temp,
                    ty: Some(source),
                    mutable: false,
                    value: payload,
                },
                Stmt::Expr(Expr::Ctor {
                    name: target_name.clone(),
                    args,
                }),
            ],
            lines: vec![u32::MAX, u32::MAX],
            region: None,
        });
        rewritten += 1;
    }

    Ok(rewritten)
}
