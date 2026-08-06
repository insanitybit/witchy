//! Type-variable and closure-devirtualization analysis helpers.
//!
//! Free functions extracted verbatim from `codegen/mod.rs`: the type-var
//! unification/inspection family (`unify_type_vars`, `type_has_var`, ...) plus the
//! devirtualization scan, bounds-elide pattern check, and the reachability
//! fn-ref walk. Behavior is unchanged.

use super::EqShape;
use witchy_syntax::ast::{Block, Expr, Stmt, Type};
use witchy_syntax::intrinsics;
use witchy_syntax::lambda_scan::collect_pattern_vars;
use foldhash::{HashMap, HashSet};

/// If `ty` is a bare type-parameter (lowercase, argument-less name), return it.
/// Pin type variables in `ty` by structurally matching it against a resolved
/// shape: a bare var takes the whole shape, `List(a)` against a list shape
/// pins `a` to the element, tuples pin pairwise. First pin wins.
pub(super) fn unify_type_vars(ty: &Type, shape: &EqShape, subst: &mut HashMap<String, EqShape>) {
    if let Some(v) = bare_type_var(ty) {
        subst.entry(v).or_insert_with(|| shape.clone());
        return;
    }
    match (ty, shape) {
        (Type::Named(n, args), EqShape::List(inner)) if n == "List" => {
            if let Some(a) = args.first() {
                unify_type_vars(a, inner, subst);
            }
        }
        (Type::Tuple(ts), EqShape::Tuple(ss)) => {
            for (t, s) in ts.iter().zip(ss) {
                unify_type_vars(t, s, subst);
            }
        }
        _ => {}
    }
}

fn bare_type_var(ty: &Type) -> Option<String> {
    match ty {
        Type::Named(n, args)
            if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) && !n.contains('.') =>
        {
            Some(n.clone())
        }
        _ => None,
    }
}

/// If `ret` is `Option(a)` or `Result(a, _)` whose payload `a` is a bare
/// type-parameter, return that parameter's name — used to spot the
/// `fn(List(a),..) -> Option(a)` shape.
pub(crate) fn payload_type_var(ret: &Option<Type>) -> Option<String> {
    if let Some(Type::Named(n, args)) = ret {
        if (n == "Option" || n == "Result") && !args.is_empty() {
            return bare_type_var(&args[0]);
        }
    }
    None
}

/// If `ret` is `List(a)` whose element `a` is a bare type-parameter, return it —
/// used to spot the `fn(List(a),..) -> List(a)` shape.
pub(crate) fn list_elem_type_var(ret: &Option<Type>) -> Option<String> {
    if let Some(Type::Named(n, args)) = ret {
        if n == "List" && args.len() == 1 {
            return bare_type_var(&args[0]);
        }
    }
    None
}

/// The index of the first parameter typed `List(tv)` for the given type-var `tv`.
pub(crate) fn list_param_of_var(params: &[witchy_syntax::ast::Param], tv: &str) -> Option<usize> {
    params.iter().position(|p| {
        matches!(&p.ty, Some(Type::Named(n, targs))
            if n == "List" && targs.len() == 1 && bare_type_var(&targs[0]).as_deref() == Some(tv))
    })
}

/// The index of the first parameter typed `fn(..) -> tv` (a function returning
/// the given type-var `tv`).
pub(crate) fn fn_param_returning_var(params: &[witchy_syntax::ast::Param], tv: &str) -> Option<usize> {
    params.iter().position(|p| {
        matches!(&p.ty, Some(Type::Fn(_, ret, _)) if bare_type_var(ret).as_deref() == Some(tv))
    })
}

/// Variables eligible for IN-PLACE push (`xs = push(xs, e)` appends into
/// exclusively-owned slack instead of copying the list): every appearance of
/// the variable in the body must be a self-push reassignment, a read through
/// `at`/`length`, a `for` iteration, or a plain reassignment (which resets
/// the tracked capacity). Anything else — passed to a function, stored in a
/// structure, returned, captured by a lambda, compared — can alias the
/// buffer, so the variable keeps the copying push. This is the linear-update
/// optimization: value semantics are preserved because no one else can
/// observe the mutated block.
/// Does the type mention a bare lowercase type variable anywhere?
pub(crate) fn type_has_var(t: &Type) -> bool {
    match t {
        Type::Qualified(_, inner) => type_has_var(inner),
        Type::Named(n, args) => {
            (args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) && !n.contains('.'))
                || args.iter().any(type_has_var)
        }
        Type::Dyn(_, args) => args.iter().any(type_has_var),
        Type::Tuple(ts) => ts.iter().any(type_has_var),
        Type::Fn(ps, r, _) => ps.iter().any(type_has_var) || type_has_var(r),
        Type::RecordCompose { base, fields } => {
            type_has_var(base) || fields.iter().any(|(_, field)| type_has_var(field))
        }
    }
}

/// (RFC-0034 L3) Names eligible for closure devirtualization in a unit: a name
/// introduced by EXACTLY ONE `let` and never otherwise re-introduced or reassigned,
/// so every call through it provably reaches the same value. A devirt site only ever
/// fires for a name whose single `let` bound a lambda (the binding-recorder checks
/// that), so this need not inspect the RHS — it only has to guarantee the name is not
/// MUTABLE OR SHADOWED: any reassignment (`f = …`), a second `let`, a tuple/pattern/
/// for-var/lambda-param binding of the same name, all disqualify it. Conservative by
/// construction (default ineligible); the walk is exhaustive (no wildcard arm) so a
/// future `Expr`/`Stmt` variant that could rebind a name is a compile error, not a
/// silent unsound devirt.
#[derive(Default)]
struct DevirtScan {
    /// `let name = …` occurrences, by name (a count, so a second `let` excludes it).
    let_bind: HashMap<String, u32>,
    /// Names introduced by any NON-`let` binder (tuple destructure, `for` var, lambda
    /// param, match/while-let pattern) — a single one disqualifies the name.
    other_bind: HashSet<String>,
    /// Names reassigned via `name = …` — a single one disqualifies the name.
    reassigned: HashSet<String>,
}

impl DevirtScan {
    fn walk_block(&mut self, b: &Block) {
        for stmt in &b.stmts {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    *self.let_bind.entry(name.clone()).or_insert(0) += 1;
                    self.walk_expr(value);
                }
                Stmt::Assign { name, value } => {
                    self.reassigned.insert(name.clone());
                    self.walk_expr(value);
                }
                Stmt::LetPattern { pattern, value } => {
                    let mut names = Vec::new();
                    witchy_syntax::ast::pattern_binds(pattern, &mut names);
                    for n in names {
                        self.other_bind.insert(n);
                    }
                    self.walk_expr(value);
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.walk_expr(e),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn walk_expr(&mut self, e: &Expr) {
        match e {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_)
            | Expr::TaggedLit { .. } => {}
            Expr::List(xs) | Expr::Tuple(xs) => xs.iter().for_each(|x| self.walk_expr(x)),
            Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                args.iter().for_each(|a| self.walk_expr(a))
            }
            Expr::LabeledCall { args, .. } => {
                args.iter().for_each(|(_, a)| self.walk_expr(a))
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                self.walk_expr(receiver);
                args.iter().for_each(|(_, a)| self.walk_expr(a))
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.walk_expr(receiver);
                args.iter().for_each(|a| self.walk_expr(a));
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                self.walk_expr(receiver);
                args.iter().for_each(|a| self.walk_expr(a));
            }
            Expr::Apply { func, args } => {
                self.walk_expr(func);
                args.iter().for_each(|a| self.walk_expr(a));
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. } => {
                self.walk_expr(expr)
            }
            Expr::Field { base, .. } => self.walk_expr(base),
            Expr::Lambda { params, body, .. } => {
                for p in params {
                    self.other_bind.insert(p.name.clone());
                }
                self.walk_block(body);
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                self.walk_expr(base);
                fields.iter().for_each(|(_, v)| self.walk_expr(v));
            }
            Expr::Record { fields, spread, .. } => {
                fields.iter().for_each(|(_, v)| self.walk_expr(v));
                if let Some(s) = spread {
                    self.walk_expr(s);
                }
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::Index { base: lhs, index: rhs }
            | Expr::Range { lo: lhs, hi: rhs, .. } => {
                self.walk_expr(lhs);
                self.walk_expr(rhs);
            }
            Expr::If { cond, then_block, else_block } => {
                self.walk_expr(cond);
                self.walk_block(then_block);
                if let Some(eb) = else_block {
                    self.walk_block(eb);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    collect_pattern_vars(&arm.pattern, &mut self.other_bind);
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g);
                    }
                    self.walk_expr(&arm.body);
                }
            }
            Expr::Block(b) => self.walk_block(b),
            Expr::While { cond, body } => {
                self.walk_expr(cond);
                self.walk_block(body);
            }
            Expr::For { var, iter, body } => {
                self.other_bind.insert(var.clone());
                self.walk_expr(iter);
                self.walk_block(body);
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                collect_pattern_vars(pattern, &mut self.other_bind);
                self.walk_expr(scrutinee);
                self.walk_block(body);
            }
        }
    }
}

pub(super) fn collect_devirt_eligible(body: &Block) -> HashSet<String> {
    let mut s = DevirtScan::default();
    s.walk_block(body);
    let DevirtScan { let_bind, other_bind, reassigned } = s;
    let_bind
        .into_iter()
        .filter(|(n, c)| *c == 1 && !other_bind.contains(n) && !reassigned.contains(n))
        .map(|(n, _)| n)
        .collect()
}

/// (RFC-0034 L2) Is `for var in lo..hi` the bounds-elidable pattern
/// `for i in 0..list.length(xs)`, with `xs` and the loop var unshadowed and
/// unreassigned in `body`? If so, returns the `(index-var, list-var)` pair to register
/// while lowering the body, so a `list.at(xs, i)` there lowers to an unchecked load.
///
/// Soundness: the for-counter is compiler-managed (set to the counter each iteration,
/// advancing `lo, lo+1, …`), so inside the body `lo ≤ i < hi`. With `lo ≥ 0` and
/// `hi = list.length(xs)`, that is exactly `0 ≤ i < length(xs)` — in range — PROVIDED
/// the length we proved cannot change: `xs` must not be reassigned (which would rebind
/// it, possibly to a shorter list) nor re-bound by a shadowing `let`/tuple/for/param/
/// pattern (which would make `xs` at the access a different value than the one whose
/// length bounds the loop). `i` likewise must not be reassigned/shadowed in the body
/// (it would no longer equal the counter). The walk that proves this (`DevirtScan`) is
/// exhaustive. Half-open only: an inclusive `0..=length(xs)` would let `i == length`
/// (OOB), so it is rejected. Conservative everywhere — any deviation keeps the checked
/// access. Gated on `bounds-elide`; off ⇒ None ⇒ the access keeps its trap guard (the
/// de-opt reference the differential sweep compares against).
pub(super) fn bounds_elide_pair(var: &str, lo: &Expr, hi: &Expr, inclusive: bool, body: &Block) -> Option<(String, String)> {
    if inclusive || !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::BoundsElide) {
        return None;
    }
    match lo {
        Expr::Int(k) if *k >= 0 => {}
        _ => return None,
    }
    let xs = match hi {
        Expr::Call { name, args } if name == intrinsics::LIST_LENGTH && args.len() == 1 => match &args[0] {
            Expr::Var(x) => x.clone(),
            _ => return None,
        },
        _ => return None,
    };
    let mut scan = DevirtScan::default();
    scan.walk_block(body);
    let stable = |n: &str| {
        !scan.let_bind.contains_key(n) && !scan.other_bind.contains(n) && !scan.reassigned.contains(n)
    };
    (stable(&xs) && stable(var)).then_some((var.to_string(), xs))
}

pub(crate) fn collect_fn_refs_block(b: &Block, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                collect_fn_refs_expr(value, out)
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => collect_fn_refs_expr(e, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_fn_refs_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        // A range survives only inside a `for` iterator; scan its bounds for
        // referenced functions (e.g. `0..len(xs)`). The other sugar nodes are
        // fully lowered before codegen.
        Expr::Range { lo, hi, .. } => {
            collect_fn_refs_expr(lo, out);
            collect_fn_refs_expr(hi, out);
        }
        Expr::Index { .. }
        | Expr::WhileLet { .. }
        | Expr::MethodCall { .. }
        | Expr::Record { .. }
        | Expr::LabeledCall { .. }
        | Expr::LabeledMethodCall { .. } => {
            unreachable!("range/index sugar is lowered before codegen (parser::lower_sugar_module)")
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            collect_fn_refs_expr(receiver, out);
            for arg in args {
                collect_fn_refs_expr(arg, out);
            }
        }
        Expr::Call { name, args } => {
            out.insert(name.clone());
            for a in args {
                collect_fn_refs_expr(a, out);
            }
        }
        Expr::Var(name) => {
            out.insert(name.clone());
        }
        Expr::Apply { func, args } => {
            collect_fn_refs_expr(func, out);
            for a in args {
                collect_fn_refs_expr(a, out);
            }
        }
        Expr::Ctor { args, .. }
        | Expr::AnonCtor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for a in args {
                collect_fn_refs_expr(a, out);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => {
            collect_fn_refs_expr(expr, out)
        }
        Expr::RecordUpdate { name: _, base, fields } => {
            collect_fn_refs_expr(base, out);
            for (_, v) in fields {
                collect_fn_refs_expr(v, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_fn_refs_expr(lhs, out);
            collect_fn_refs_expr(rhs, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_fn_refs_expr(cond, out);
            collect_fn_refs_block(then_block, out);
            if let Some(b) = else_block {
                collect_fn_refs_block(b, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_fn_refs_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_fn_refs_expr(g, out);
                }
                collect_fn_refs_expr(&arm.body, out);
            }
        }
        Expr::While { cond, body } => {
            collect_fn_refs_expr(cond, out);
            collect_fn_refs_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_fn_refs_expr(iter, out);
            collect_fn_refs_block(body, out);
        }
        Expr::Lambda { body, .. } => collect_fn_refs_block(body, out),
        Expr::Block(b) => collect_fn_refs_block(b, out),
        Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
    }
}
