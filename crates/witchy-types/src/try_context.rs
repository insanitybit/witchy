//! The type-directed desugar of contextual try, `e ? "msg"`.
//!
//! The parser turns `e ? "msg"` into `(__try_ctx(e, msg))?`; the checker types
//! `__try_ctx` generically over an `Option(T)` or `Result(T, String)` operand.
//! Both lowering routes (the canonical SafeWIR source preparation and the
//! legacy codegen front-end) consume this one rewrite, so the intrinsic never
//! reaches a backend.

use witchy_syntax::ast::{BinOp, Block, Convention, Expr, Item, Module, Param, Stmt, Type};
use witchy_syntax::intrinsics;

use crate::typeck::{TypeTable, ty_to_ast};

/// The `e ? "msg"` desugar (`__try_ctx(operand, msg)`) rewritten to a concrete
/// std call by the operand's type: `Option` -> `option.ok_or(operand, msg)`,
/// `Result` -> `result.map_err(operand, fn(__ctx_err): msg + ": " + __ctx_err)`.
/// The `+` stays `Add`; the later `flip_string_add_module` turns it into `Concat`.
/// Returns true if any node was rewritten (so the caller re-annotates, since
/// moved/new nodes change the address-keyed `TypeTable`).
pub fn rewrite_try_ctx_module(m: &mut Module, table: &TypeTable) -> bool {
    fn replacement(is_option: bool, operand: Expr, msg: Expr) -> Expr {
        if is_option {
            return Expr::Call { name: "option.ok_or".into(), args: vec![operand, msg] };
        }
        Expr::Call {
            name: "result.map_err".into(),
            args: vec![
                operand,
                Expr::Lambda {
                    params: vec![Param {
                        name: "__ctx_err".into(),
                        ty: None,
                        convention: Convention::default(),
                        default: None,
                    }],
                    body: Block {
                        stmts: vec![Stmt::Expr(Expr::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(Expr::Binary {
                                op: BinOp::Add,
                                lhs: Box::new(msg),
                                rhs: Box::new(Expr::Str(": ".into())),
                            }),
                            rhs: Box::new(Expr::Var("__ctx_err".into())),
                        })],
                        lines: vec![0],
                        region: None,
                    },
                    ret: None,
                    qualifiers: Default::default(),
                },
            ],
        }
    }
    fn walk_expr(e: &mut Expr, table: &TypeTable, changed: &mut bool) {
        match e {
            Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
            | Expr::AnonCtor { args: xs, .. }
            | Expr::Call { args: xs, .. } => {
                for x in xs {
                    walk_expr(x, table, changed);
                }
            }
            Expr::Apply { func, args } => {
                walk_expr(func, table, changed);
                for a in args {
                    walk_expr(a, table, changed);
                }
            }
            Expr::MethodCall { receiver, args, .. }
            | Expr::ExistentialCall { receiver, args, .. } => {
                walk_expr(receiver, table, changed);
                for a in args {
                    walk_expr(a, table, changed);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                walk_expr(lhs, table, changed);
                walk_expr(rhs, table, changed);
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => walk_expr(expr, table, changed),
            Expr::Range { lo, hi, .. } => {
                walk_expr(lo, table, changed);
                walk_expr(hi, table, changed);
            }
            Expr::Index { base, index } => {
                walk_expr(base, table, changed);
                walk_expr(index, table, changed);
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                walk_expr(receiver, table, changed);
                for (_, a) in args {
                    walk_expr(a, table, changed);
                }
            }
            Expr::LabeledCall { .. } => {
                unreachable!("RFC-0056: labeled calls are lowered to positional Call before codegen")
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    walk_expr(v, table, changed);
                }
                if let Some(sp) = spread {
                    walk_expr(sp, table, changed);
                }
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                walk_expr(base, table, changed);
                for (_, v) in fields {
                    walk_expr(v, table, changed);
                }
            }
            Expr::If { cond, then_block, else_block } => {
                walk_expr(cond, table, changed);
                walk_block(then_block, table, changed);
                if let Some(b) = else_block {
                    walk_block(b, table, changed);
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk_expr(scrutinee, table, changed);
                for a in arms {
                    if let Some(g) = &mut a.guard {
                        walk_expr(g, table, changed);
                    }
                    walk_expr(&mut a.body, table, changed);
                }
            }
            Expr::While { cond, body } => {
                walk_expr(cond, table, changed);
                walk_block(body, table, changed);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                walk_expr(scrutinee, table, changed);
                walk_block(body, table, changed);
            }
            Expr::For { iter, body, .. } => {
                walk_expr(iter, table, changed);
                walk_block(body, table, changed);
            }
            Expr::Lambda { body, .. } => walk_block(body, table, changed),
            Expr::Block(b) => walk_block(b, table, changed),
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        }
        // After recursing into children, rewrite this node if it is `__try_ctx`.
        // Read the operand type BEFORE moving (the table is keyed by node address).
        let is_try = matches!(e, Expr::Call { name, args } if name == intrinsics::TRY_CONTEXT && args.len() == 2);
        if is_try {
            let is_option = if let Expr::Call { args, .. } = &*e {
                matches!(
                    table.type_of(&args[0]).and_then(ty_to_ast),
                    Some(Type::Named(n, _)) if n == "Option"
                )
            } else {
                false
            };
            if let Expr::Call { args, .. } = std::mem::replace(e, Expr::Bool(false)) {
                let mut it = args.into_iter();
                let operand = it.next().unwrap();
                let msg = it.next().unwrap();
                *e = replacement(is_option, operand, msg);
                *changed = true;
            }
        }
    }
    fn walk_block(b: &mut Block, table: &TypeTable, changed: &mut bool) {
        for st in &mut b.stmts {
            match st {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Yield(value) => walk_expr(value, table, changed),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }
    let mut changed = false;
    for item in &mut m.items {
        match item {
            Item::Function(f) => walk_block(&mut f.body, table, &mut changed),
            Item::Impl(im) => {
                for f in &mut im.methods {
                    walk_block(&mut f.body, table, &mut changed);
                }
            }
            Item::Trait(t) => {
                for msig in &mut t.methods {
                    if let Some(b) = &mut msig.default {
                        walk_block(b, table, &mut changed);
                    }
                }
            }
            Item::Const { value, .. } => walk_expr(value, table, &mut changed),
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    changed
}
