//! Module linker.
//!
//! Combines a set of named modules into one flat `Module`, qualifying each
//! module's function names (`mod.func`) and rewriting call sites so an
//! unqualified call resolves to the same module and a `mod.func` call resolves
//! to an imported module. Importing is purely declarative: it brings names into
//! scope, runs no code, and confers no authority — a dependency can only act
//! through capabilities the caller passes to its functions (visible in their
//! types) or by being spawned as an actor with a grant.
//!
//! v1: functions are module-scoped; types/constructors/actors share one global
//! namespace.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::ast::*;

#[derive(Debug, Clone, PartialEq)]
pub struct LinkError {
    pub message: String,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "link error: {}", self.message)
    }
}

impl std::error::Error for LinkError {}

fn lerr<T>(message: impl Into<String>) -> Result<T, LinkError> {
    Err(LinkError {
        message: message.into(),
    })
}

const BUILTINS: &[&str] = &[
    "print",
    "print_int",
    "print_float",
    "to_string",
    "int_to_string",
    "string_length",
    "to_upper",
    "to_lower",
    "trim",
    "starts_with",
    "contains",
    "ends_with",
    "index_of",
    "split",
    "replace",
    "substring",
    "int_to_float",
    "float_to_int",
    "sqrt",
    "string_to_int",
    "length",
    "char_count",
    "at",
    "push",
    "concat",
    "dict_new",
    "insert",
    "get_or",
    "has",
    "keys",
    "values",
    "pairs",
    "size",
    "send",
    "read",
    "subdir",
    "connect",
    "restrict",
    "send_line",
    "recv_line",
];

type FnTable = HashMap<String, HashSet<String>>;

/// The source of a bundled standard-library module, if `name` is one. This is
/// the canonical std registry: the linker treats it as a built-in search path,
/// and the CLI/test harness resolve `import` against it too.
pub fn std_source(name: &str) -> Option<&'static str> {
    match name {
        "list" => Some(include_str!("../std/list.witchy")),
        "string" => Some(include_str!("../std/string.witchy")),
        "math" => Some(include_str!("../std/math.witchy")),
        "result" => Some(include_str!("../std/result.witchy")),
        "option" => Some(include_str!("../std/option.witchy")),
        "func" => Some(include_str!("../std/func.witchy")),
        _ => None,
    }
}

/// Link `modules` (each a name + parsed module) into one flat module, with
/// `entry` the module holding `main`.
pub fn link(mut modules: Vec<(String, Module)>, entry: &str) -> Result<Module, LinkError> {
    // Pull in any imported standard-library module not already provided (the
    // std registry is a built-in search path), transitively — so a std module
    // can import another (e.g. `list` importing `option`) and callers need not
    // list the dependency explicitly. Locally provided modules take precedence:
    // a name already present is never overridden by the bundled copy.
    let mut i = 0;
    while i < modules.len() {
        let imports = modules[i].1.imports.clone();
        for imp in imports {
            if !modules.iter().any(|(n, _)| n == &imp) {
                if let Some(src) = std_source(&imp) {
                    let m = crate::parser::parse_module(src).map_err(|e| LinkError {
                        message: format!("std module `{imp}`: {e}"),
                    })?;
                    modules.push((imp.clone(), m));
                }
            }
        }
        i += 1;
    }

    let mut fns: FnTable = HashMap::new();
    for (name, m) in &modules {
        let mut names = HashSet::new();
        for item in &m.items {
            if let Item::Function(f) = item {
                names.insert(f.name.clone());
            }
        }
        fns.insert(name.clone(), names);
    }

    if !modules.iter().any(|(n, _)| n == entry) {
        return lerr(format!("entry module `{entry}` not found"));
    }
    for (name, m) in &modules {
        for imp in &m.imports {
            if !fns.contains_key(imp) {
                return lerr(format!("module `{name}` imports unknown module `{imp}`"));
            }
        }
    }

    let mut items = Vec::new();
    for (mname, m) in &modules {
        for item in &m.items {
            match item {
                Item::Function(f) => {
                    let mut f2 = f.clone();
                    f2.name = if mname == entry && f.name == "main" {
                        "main".to_string()
                    } else {
                        format!("{mname}.{}", f.name)
                    };
                    let mut bound = HashSet::new();
                    for p in &f2.params {
                        bound.insert(p.name.clone());
                    }
                    collect_bound_block(&f2.body, &mut bound);
                    rewrite_block(&mut f2.body, mname, &m.imports, &fns, &bound)?;
                    items.push(Item::Function(f2));
                }
                Item::Actor(a) => {
                    let mut a2 = a.clone();
                    for field in &mut a2.fields {
                        if let Some(init) = &mut field.init {
                            rewrite_expr(init, mname, &m.imports, &fns, &HashSet::new())?;
                        }
                    }
                    for h in &mut a2.handlers {
                        let mut bound = HashSet::new();
                        for p in &h.params {
                            bound.insert(p.name.clone());
                        }
                        collect_bound_block(&h.body, &mut bound);
                        rewrite_block(&mut h.body, mname, &m.imports, &fns, &bound)?;
                    }
                    items.push(Item::Actor(a2));
                }
                Item::Type(t) => items.push(Item::Type(t.clone())),
            }
        }
    }
    Ok(Module {
        imports: Vec::new(),
        items,
    })
}

/// Collect every name bound as a local within a block — `let`/`var` bindings,
/// tuple destructurings, `for` loop variables, lambda parameters, and `match`
/// pattern bindings (recursively, including nested blocks/expressions). Used so
/// the linker never mistakes a local that shadows a same-module function name
/// for a first-class reference to that function.
fn collect_bound_block(b: &Block, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                out.insert(name.clone());
                collect_bound_expr(value, out);
            }
            Stmt::LetTuple { names, value } => {
                for n in names {
                    out.insert(n.clone());
                }
                collect_bound_expr(value, out);
            }
            Stmt::Assign { value, .. } => collect_bound_expr(value, out),
            Stmt::Return(Some(e)) | Stmt::Expr(e) => collect_bound_expr(e, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_pattern_vars(p: &Pattern, out: &mut HashSet<String>) {
    match p {
        Pattern::Var(n) => {
            out.insert(n.clone());
        }
        Pattern::Ctor { args, .. } | Pattern::Tuple(args) => {
            for a in args {
                collect_pattern_vars(a, out);
            }
        }
        Pattern::List { elems, rest } => {
            for e in elems {
                collect_pattern_vars(e, out);
            }
            if let Some(Some(name)) = rest {
                out.insert(name.clone());
            }
        }
        Pattern::Wildcard | Pattern::Int(_) | Pattern::Str(_) | Pattern::Bool(_) => {}
    }
}

fn collect_bound_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Lambda { params, body } => {
            for p in params {
                out.insert(p.name.clone());
            }
            collect_bound_block(body, out);
        }
        Expr::For { var, iter, body } => {
            out.insert(var.clone());
            collect_bound_expr(iter, out);
            collect_bound_block(body, out);
        }
        Expr::Match { scrutinee, arms } => {
            collect_bound_expr(scrutinee, out);
            for arm in arms {
                collect_pattern_vars(&arm.pattern, out);
                if let Some(g) = &arm.guard {
                    collect_bound_expr(g, out);
                }
                collect_bound_expr(&arm.body, out);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            collect_bound_expr(cond, out);
            collect_bound_block(then_block, out);
            if let Some(b) = else_block {
                collect_bound_block(b, out);
            }
        }
        Expr::While { cond, body } => {
            collect_bound_expr(cond, out);
            collect_bound_block(body, out);
        }
        Expr::Block(b) => collect_bound_block(b, out),
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args)
        | Expr::Spawn { args, .. } => {
            for a in args {
                collect_bound_expr(a, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_bound_expr(func, out);
            for a in args {
                collect_bound_expr(a, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_bound_expr(lhs, out);
            collect_bound_expr(rhs, out);
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            collect_bound_expr(expr, out)
        }
        Expr::RecordUpdate { base, fields } => {
            collect_bound_expr(base, out);
            for (_, v) in fields {
                collect_bound_expr(v, out);
            }
        }
        Expr::Var(_) | Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
    }
}

fn rewrite_block(
    b: &mut Block,
    m: &str,
    imps: &[String],
    fns: &FnTable,
    bound: &HashSet<String>,
) -> Result<(), LinkError> {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetTuple { value, .. } => rewrite_expr(value, m, imps, fns, bound)?,
            Stmt::Return(Some(e)) | Stmt::Expr(e) => rewrite_expr(e, m, imps, fns, bound)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn rewrite_expr(
    e: &mut Expr,
    m: &str,
    imps: &[String],
    fns: &FnTable,
    bound: &HashSet<String>,
) -> Result<(), LinkError> {
    match e {
        Expr::Call { name, args } => {
            *name = resolve_call(name, m, imps, fns)?;
            for a in args {
                rewrite_expr(a, m, imps, fns, bound)?;
            }
        }
        // A bare name matching a same-module function is a first-class reference
        // to it; qualify it like a call — unless it is shadowed by a local of the
        // same name (a parameter, `let`, loop variable, or pattern binding).
        Expr::Var(name) => {
            if !bound.contains(name.as_str())
                && fns.get(m).is_some_and(|s| s.contains(name.as_str()))
            {
                *name = format!("{m}.{name}");
            }
        }
        Expr::Apply { func, args } => {
            rewrite_expr(func, m, imps, fns, bound)?;
            for a in args {
                rewrite_expr(a, m, imps, fns, bound)?;
            }
        }
        Expr::Ctor { args, .. } | Expr::List(args) | Expr::Tuple(args) | Expr::Spawn { args, .. } => {
            for a in args {
                rewrite_expr(a, m, imps, fns, bound)?;
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            rewrite_expr(expr, m, imps, fns, bound)?
        }
        Expr::RecordUpdate { base, fields } => {
            rewrite_expr(base, m, imps, fns, bound)?;
            for (_, value) in fields {
                rewrite_expr(value, m, imps, fns, bound)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, m, imps, fns, bound)?;
            rewrite_expr(rhs, m, imps, fns, bound)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            rewrite_expr(cond, m, imps, fns, bound)?;
            rewrite_block(then_block, m, imps, fns, bound)?;
            if let Some(b) = else_block {
                rewrite_block(b, m, imps, fns, bound)?;
            }
        }
        Expr::Lambda { body, .. } => rewrite_block(body, m, imps, fns, bound)?,
        Expr::Block(b) => rewrite_block(b, m, imps, fns, bound)?,
        Expr::While { cond, body } => {
            rewrite_expr(cond, m, imps, fns, bound)?;
            rewrite_block(body, m, imps, fns, bound)?;
        }
        Expr::For { iter, body, .. } => {
            rewrite_expr(iter, m, imps, fns, bound)?;
            rewrite_block(body, m, imps, fns, bound)?;
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, m, imps, fns, bound)?;
            for arm in arms {
                if let Some(g) = &mut arm.guard {
                    rewrite_expr(g, m, imps, fns, bound)?;
                }
                rewrite_expr(&mut arm.body, m, imps, fns, bound)?;
            }
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
    }
    Ok(())
}

fn resolve_call(name: &str, m: &str, imps: &[String], fns: &FnTable) -> Result<String, LinkError> {
    if let Some((modname, fname)) = name.split_once('.') {
        if !imps.iter().any(|i| i == modname) {
            return lerr(format!(
                "module `{m}` calls `{modname}.{fname}` but does not `import {modname}`"
            ));
        }
        return match fns.get(modname) {
            Some(s) if s.contains(fname) => Ok(name.to_string()),
            _ => lerr(format!("module `{modname}` has no function `{fname}`")),
        };
    }
    // A function defined in THIS module wins over a builtin of the same name, so
    // e.g. `list.contains` is reachable as a bare `contains` inside `list` (a
    // builtin would otherwise shadow it). Checked before BUILTINS for that
    // reason.
    if fns.get(m).is_some_and(|s| s.contains(name)) {
        return Ok(format!("{m}.{name}"));
    }
    if BUILTINS.contains(&name) {
        return Ok(name.to_string());
    }
    // Not a function here and not a builtin: a local binding being applied (e.g.
    // a lambda parameter). Leave it unqualified; the type checker decides.
    Ok(name.to_string())
}
