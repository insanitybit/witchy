//! Local item reachability for compile-time helper programs.
//!
//! `comptime:` blocks and tagged literals both execute a synthetic Witchy module.
//! That module should contain the helper code the generator actually calls, but
//! not the consumer's whole runtime module. Keeping this traversal shared prevents
//! the two compile-time paths from drifting.

use std::collections::{HashMap, HashSet};

use witchy_syntax::ast::{
    collect_type_names, Block, Expr, Function, Item, Pattern, Stmt, Type,
};

/// The names of every local item reachable from a root function.
pub(crate) fn reachable_from_function(items: &[Item], root: &str) -> HashSet<String> {
    let ctx = Reachability::new(items);
    let mut keep = HashSet::new();
    let mut work = Vec::new();
    ctx.push_ref(root, &mut keep, &mut work);
    ctx.drain(&mut keep, &mut work);
    keep
}

/// The names of every local item reachable from a root block.
pub(crate) fn reachable_from_block(items: &[Item], root: &Block) -> HashSet<String> {
    let ctx = Reachability::new(items);
    let mut keep = HashSet::new();
    let mut work = Vec::new();
    let mut names = HashSet::new();
    collect_refs_block(root, &mut names);
    for name in names {
        ctx.push_ref(&name, &mut keep, &mut work);
    }
    ctx.drain(&mut keep, &mut work);
    keep
}

struct Reachability<'a> {
    fns: HashMap<&'a str, &'a Function>,
    types: HashMap<&'a str, &'a witchy_syntax::ast::TypeDef>,
    aliases: HashMap<&'a str, &'a Type>,
    ctor_owner: HashMap<&'a str, &'a str>,
}

impl<'a> Reachability<'a> {
    fn new(items: &'a [Item]) -> Self {
        let mut fns = HashMap::new();
        let mut types = HashMap::new();
        let mut aliases = HashMap::new();
        let mut ctor_owner = HashMap::new();
        for item in items {
            match item {
                Item::Function(f) => {
                    fns.insert(f.name.as_str(), f);
                }
                Item::Type(t) => {
                    types.insert(t.name.as_str(), t);
                    for v in &t.variants {
                        ctor_owner.insert(v.name.as_str(), t.name.as_str());
                    }
                }
                Item::TypeAlias { name, ty, .. } => {
                    aliases.insert(name.as_str(), ty);
                }
                _ => {}
            }
        }
        Self { fns, types, aliases, ctor_owner }
    }

    fn push_ref(&self, name: &str, keep: &mut HashSet<String>, work: &mut Vec<String>) {
        let mut enqueue = |n: &str| {
            if keep.insert(n.to_string()) {
                work.push(n.to_string());
            }
        };
        if self.fns.contains_key(name) {
            enqueue(name);
        }
        if self.types.contains_key(name) {
            enqueue(name);
        }
        if self.aliases.contains_key(name) {
            enqueue(name);
        }
        if let Some(owner) = self.ctor_owner.get(name) {
            enqueue(owner);
        }
    }

    fn drain(&self, keep: &mut HashSet<String>, work: &mut Vec<String>) {
        while let Some(name) = work.pop() {
            if let Some(f) = self.fns.get(name.as_str()) {
                let mut names = HashSet::new();
                collect_refs_block(&f.body, &mut names);
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        collect_type_names(t, &mut names);
                    }
                }
                if let Some(t) = &f.ret {
                    collect_type_names(t, &mut names);
                }
                for r in names {
                    self.push_ref(&r, keep, work);
                }
            }
            if let Some(t) = self.types.get(name.as_str()) {
                let mut names = HashSet::new();
                for v in &t.variants {
                    for field in &v.fields {
                        collect_type_names(field, &mut names);
                    }
                }
                for r in names {
                    self.push_ref(&r, keep, work);
                }
            }
            if let Some(t) = self.aliases.get(name.as_str()) {
                let mut names = HashSet::new();
                collect_type_names(t, &mut names);
                for r in names {
                    self.push_ref(&r, keep, work);
                }
            }
        }
    }
}

/// Collect every name a block references: callees, variables, constructors,
/// constructor names in patterns, and types named in annotations.
fn collect_refs_block(b: &Block, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(t) = ty {
                    collect_type_names(t, out);
                }
                collect_refs_expr(value, out);
            }
            Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                collect_refs_expr(value, out)
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => collect_refs_expr(e, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_refs_pattern(p: &Pattern, out: &mut HashSet<String>) {
    match p {
        Pattern::Ctor { name, args } => {
            out.insert(name.clone());
            for a in args {
                collect_refs_pattern(a, out);
            }
        }
        Pattern::AnonCtor { args, .. } => {
            for a in args {
                collect_refs_pattern(a, out);
            }
        }
        Pattern::Tuple(args) | Pattern::List { elems: args, .. } | Pattern::Or(args) => {
            for a in args {
                collect_refs_pattern(a, out);
            }
        }
        Pattern::Wildcard
        | Pattern::Var(_)
        | Pattern::Int(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Duration(_)
        | Pattern::IntRange { .. } => {}
    }
}

fn collect_refs_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Call { name, args } | Expr::Ctor { name, args } => {
            out.insert(name.clone());
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::AnonCtor { args, .. } => {
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::LabeledCall { name, args } => {
            out.insert(name.clone());
            for (_, a) in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::Var(name) => {
            out.insert(name.clone());
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_refs_expr(receiver, out);
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_refs_expr(func, out);
            for a in args {
                collect_refs_expr(a, out);
            }
        }
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                collect_refs_expr(x, out);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            collect_refs_expr(expr, out)
        }
        Expr::As { expr, ty } => {
            collect_refs_expr(expr, out);
            collect_type_names(ty, out);
        }
        Expr::ExistentialPack { expr, ty, .. } => {
            collect_refs_expr(expr, out);
            collect_type_names(ty, out);
        }
        Expr::ExistentialCall { receiver, args, ty, result, .. } => {
            collect_refs_expr(receiver, out);
            for arg in args {
                collect_refs_expr(arg, out);
            }
            collect_type_names(ty, out);
            collect_type_names(result, out);
        }
        Expr::RecordUpdate { base, fields, .. } => {
            collect_refs_expr(base, out);
            for (_, v) in fields {
                collect_refs_expr(v, out);
            }
        }
        Expr::Record { name, fields, spread } => {
            out.insert(name.clone());
            for (_, v) in fields {
                collect_refs_expr(v, out);
            }
            if let Some(s) = spread {
                collect_refs_expr(s, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_refs_expr(lhs, out);
            collect_refs_expr(rhs, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_refs_expr(cond, out);
            collect_refs_block(then_block, out);
            if let Some(b) = else_block {
                collect_refs_block(b, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_refs_expr(scrutinee, out);
            for arm in arms {
                collect_refs_pattern(&arm.pattern, out);
                if let Some(g) = &arm.guard {
                    collect_refs_expr(g, out);
                }
                collect_refs_expr(&arm.body, out);
            }
        }
        Expr::While { cond, body } => {
            collect_refs_expr(cond, out);
            collect_refs_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_refs_expr(iter, out);
            collect_refs_block(body, out);
        }
        Expr::Range { lo, hi, .. } => {
            collect_refs_expr(lo, out);
            collect_refs_expr(hi, out);
        }
        Expr::Index { base, index } => {
            collect_refs_expr(base, out);
            collect_refs_expr(index, out);
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            collect_refs_pattern(pattern, out);
            collect_refs_expr(scrutinee, out);
            collect_refs_block(body, out);
        }
        Expr::Lambda { params, body, ret } => {
            for p in params {
                if let Some(t) = &p.ty {
                    collect_type_names(t, out);
                }
            }
            if let Some(t) = ret {
                collect_type_names(t, out);
            }
            collect_refs_block(body, out);
        }
        Expr::Block(b) => collect_refs_block(b, out),
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
    }
}
