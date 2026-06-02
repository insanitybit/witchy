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
    "length",
    "at",
    "send",
    "read",
    "subdir",
    "connect",
    "restrict",
    "send_line",
    "recv_line",
];

type FnTable = HashMap<String, HashSet<String>>;

/// Link `modules` (each a name + parsed module) into one flat module, with
/// `entry` the module holding `main`.
pub fn link(modules: Vec<(String, Module)>, entry: &str) -> Result<Module, LinkError> {
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
                    rewrite_block(&mut f2.body, mname, &m.imports, &fns)?;
                    items.push(Item::Function(f2));
                }
                Item::Actor(a) => {
                    let mut a2 = a.clone();
                    for field in &mut a2.fields {
                        if let Some(init) = &mut field.init {
                            rewrite_expr(init, mname, &m.imports, &fns)?;
                        }
                    }
                    for h in &mut a2.handlers {
                        rewrite_block(&mut h.body, mname, &m.imports, &fns)?;
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

fn rewrite_block(b: &mut Block, m: &str, imps: &[String], fns: &FnTable) -> Result<(), LinkError> {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } => {
                rewrite_expr(value, m, imps, fns)?
            }
            Stmt::Expr(e) => rewrite_expr(e, m, imps, fns)?,
        }
    }
    Ok(())
}

fn rewrite_expr(e: &mut Expr, m: &str, imps: &[String], fns: &FnTable) -> Result<(), LinkError> {
    match e {
        Expr::Call { name, args } => {
            *name = resolve_call(name, m, imps, fns)?;
            for a in args {
                rewrite_expr(a, m, imps, fns)?;
            }
        }
        Expr::Ctor { args, .. } | Expr::List(args) | Expr::Spawn { args, .. } => {
            for a in args {
                rewrite_expr(a, m, imps, fns)?;
            }
        }
        Expr::Unary { expr, .. } => rewrite_expr(expr, m, imps, fns)?,
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, m, imps, fns)?;
            rewrite_expr(rhs, m, imps, fns)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            rewrite_expr(cond, m, imps, fns)?;
            rewrite_block(then_block, m, imps, fns)?;
            if let Some(b) = else_block {
                rewrite_block(b, m, imps, fns)?;
            }
        }
        Expr::Block(b) => rewrite_block(b, m, imps, fns)?,
        Expr::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, m, imps, fns)?;
            for arm in arms {
                if let Some(g) = &mut arm.guard {
                    rewrite_expr(g, m, imps, fns)?;
                }
                rewrite_expr(&mut arm.body, m, imps, fns)?;
            }
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) => {}
    }
    Ok(())
}

fn resolve_call(name: &str, m: &str, imps: &[String], fns: &FnTable) -> Result<String, LinkError> {
    if BUILTINS.contains(&name) {
        return Ok(name.to_string());
    }
    if let Some((modname, fname)) = name.split_once('.') {
        if !imps.iter().any(|i| i == modname) {
            return lerr(format!(
                "module `{m}` calls `{modname}.{fname}` but does not `import {modname}`"
            ));
        }
        match fns.get(modname) {
            Some(s) if s.contains(fname) => Ok(name.to_string()),
            _ => lerr(format!("module `{modname}` has no function `{fname}`")),
        }
    } else {
        match fns.get(m) {
            Some(s) if s.contains(name) => Ok(format!("{m}.{name}")),
            _ => lerr(format!("unknown function `{name}` in module `{m}`")),
        }
    }
}
