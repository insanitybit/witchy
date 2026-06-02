//! A type checker for witchy.
//!
//! Annotation-driven checking with light Hindley-Milner-style unification for
//! the bits that aren't annotated (let bindings, match arms). It is deliberately
//! lenient where it lacks information (e.g. actor message constructors, which
//! aren't yet declared as types) so it never rejects a valid program — it
//! tightens as the type system grows.
//!
//! Capability safety is not a special case: `print` has type
//! `(Console, String) -> Nil`, and the only way to obtain a `Console` is to
//! receive one as a parameter — ultimately from `main`. So "this code may
//! perform output" is simply visible in its type, and code that never received
//! the capability cannot type-check a call that needs it.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::ast::{
    self, ActorDef, Block, Convention, Expr, Function, Item, MatchArm, Module, Pattern, Stmt,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Float,
    String,
    Bool,
    Nil,
    Console,
    Subject,
    Dir,
    Net,
    Socket,
    List(Box<Ty>),
    Named(String),
    Var(u32),
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Int => write!(f, "Int"),
            Ty::Float => write!(f, "Float"),
            Ty::String => write!(f, "String"),
            Ty::Bool => write!(f, "Bool"),
            Ty::Nil => write!(f, "Nil"),
            Ty::Console => write!(f, "Console"),
            Ty::Subject => write!(f, "Subject"),
            Ty::Dir => write!(f, "Dir"),
            Ty::Net => write!(f, "Net"),
            Ty::Socket => write!(f, "Socket"),
            Ty::List(e) => write!(f, "List({e})"),
            Ty::Named(n) => write!(f, "{n}"),
            Ty::Var(_) => write!(f, "?"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub message: String,
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "type error: {}", self.message)
    }
}

impl std::error::Error for TypeError {}

fn terr<T>(message: impl Into<String>) -> Result<T, TypeError> {
    Err(TypeError {
        message: message.into(),
    })
}

struct Checker {
    fn_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    ctor_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    adt_variants: HashMap<String, Vec<String>>,
    actor_field_sigs: HashMap<String, Vec<Ty>>,
    fn_conventions: HashMap<String, Vec<Convention>>,
    subst: HashMap<u32, Ty>,
    next_var: u32,
    /// Each binding carries its type and whether it is mutable.
    scopes: Vec<HashMap<String, (Ty, bool)>>,
    /// Bindings that have been consumed (moved out via a `sink` parameter) and
    /// may not be used again until reassigned. Flow-sensitive within a body.
    consumed: HashSet<String>,
}

impl Checker {
    fn fresh(&mut self) -> Ty {
        let v = self.next_var;
        self.next_var += 1;
        Ty::Var(v)
    }

    fn to_ty(&mut self, t: &ast::Type) -> Ty {
        let ast::Type::Named(name, args) = t;
        match name.as_str() {
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "String" => Ty::String,
            "Bool" => Ty::Bool,
            "Nil" => Ty::Nil,
            "Console" => Ty::Console,
            "Subject" => Ty::Subject,
            "Dir" => Ty::Dir,
            "Net" => Ty::Net,
            "Socket" => Ty::Socket,
            "List" => {
                let elem = match args.first() {
                    Some(a) => self.to_ty(a),
                    None => self.fresh(),
                };
                Ty::List(Box::new(elem))
            }
            _ => Ty::Named(name.clone()),
        }
    }

    fn resolve(&self, t: &Ty) -> Ty {
        match t {
            Ty::Var(v) => match self.subst.get(v) {
                Some(bound) => self.resolve(bound),
                None => t.clone(),
            },
            Ty::List(e) => Ty::List(Box::new(self.resolve(e))),
            _ => t.clone(),
        }
    }

    fn unify(&mut self, a: &Ty, b: &Ty) -> Result<(), TypeError> {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (&a, &b) {
            (Ty::Var(x), Ty::Var(y)) if x == y => Ok(()),
            (Ty::Var(x), other) | (other, Ty::Var(x)) => {
                self.subst.insert(*x, other.clone());
                Ok(())
            }
            (Ty::List(x), Ty::List(y)) => self.unify(x, y),
            (Ty::Named(x), Ty::Named(y)) if x == y => Ok(()),
            _ if a == b => Ok(()),
            _ => terr(format!("expected `{a}`, found `{b}`")),
        }
    }

    // --- scope helpers ---
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    fn define(&mut self, name: String, ty: Ty, mutable: bool) {
        self.scopes.last_mut().unwrap().insert(name, (ty, mutable));
    }
    fn lookup(&self, name: &str) -> Option<Ty> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .map(|(t, _)| t.clone())
    }
    fn is_mutable(&self, name: &str) -> Option<bool> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .map(|(_, m)| *m)
    }

    fn call_sig(&mut self, name: &str) -> Option<(Vec<Ty>, Ty)> {
        match name {
            "print" => Some((vec![Ty::Console, Ty::String], Ty::Nil)),
            "int_to_string" => Some((vec![Ty::Int], Ty::String)),
            "to_string" => {
                let a = self.fresh();
                Some((vec![a], Ty::String))
            }
            "send" => {
                let msg = self.fresh();
                Some((vec![Ty::Subject, msg], Ty::Nil))
            }
            "length" => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem))], Ty::Int))
            }
            "at" => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem.clone())), Ty::Int], elem))
            }
            "read" => Some((vec![Ty::Dir, Ty::String], Ty::String)),
            "subdir" => Some((vec![Ty::Dir, Ty::String], Ty::Dir)),
            "connect" => Some((vec![Ty::Net, Ty::String], Ty::Socket)),
            "restrict" => Some((vec![Ty::Net, Ty::String], Ty::Net)),
            "send_line" => Some((vec![Ty::Socket, Ty::String], Ty::Nil)),
            "recv_line" => Some((vec![Ty::Socket], Ty::String)),
            _ => self.fn_sigs.get(name).cloned(),
        }
    }

    // --- inference ---

    fn infer_block(&mut self, block: &Block) -> Result<Ty, TypeError> {
        self.push();
        let mut ty = Ty::Nil;
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { name, mutable, value } => {
                    let vt = self.infer(value)?;
                    self.define(name.clone(), vt, *mutable);
                    ty = Ty::Nil;
                }
                Stmt::Assign { name, value } => {
                    let vt = self.infer(value)?;
                    let Some(existing) = self.lookup(name) else {
                        self.pop();
                        return terr(format!("assignment to unbound variable `{name}`"));
                    };
                    if self.is_mutable(name) == Some(false) {
                        self.pop();
                        return terr(format!(
                            "cannot assign to `{name}`: it is immutable (declared with `let`)"
                        ));
                    }
                    self.unify(&existing, &vt)?;
                    self.consumed.remove(name); // reassignment re-initializes
                    ty = Ty::Nil;
                }
                Stmt::Expr(e) => {
                    ty = self.infer(e)?;
                }
            }
        }
        self.pop();
        Ok(ty)
    }

    fn infer(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        match expr {
            Expr::Int(_) => Ok(Ty::Int),
            Expr::Float(_) => Ok(Ty::Float),
            Expr::Str(_) => Ok(Ty::String),
            Expr::Bool(_) => Ok(Ty::Bool),
            Expr::List(items) => {
                let elem = self.fresh();
                for it in items {
                    let t = self.infer(it)?;
                    self.unify(&elem, &t)?;
                }
                Ok(Ty::List(Box::new(elem)))
            }
            Expr::Var(name) => {
                if self.consumed.contains(name) {
                    return terr(format!(
                        "use of `{name}` after it was moved (consumed by a `sink` parameter)"
                    ));
                }
                self.lookup(name)
                    .ok_or_else(|| TypeError { message: format!("unbound variable `{name}`") })
            }
            Expr::Call { name, args } => {
                let Some((params, ret)) = self.call_sig(name) else {
                    return terr(format!("call to unknown function `{name}`"));
                };
                if params.len() != args.len() {
                    return terr(format!(
                        "`{name}` expects {} argument(s) but got {}",
                        params.len(),
                        args.len()
                    ));
                }
                for (arg, param_ty) in args.iter().zip(&params) {
                    let at = self.infer(arg)?;
                    self.unify(param_ty, &at)
                        .map_err(|e| TypeError { message: format!("in call to `{name}`: {}", e.message) })?;
                }
                // Enforce conventions: `inout` needs a mutable variable; `sink`
                // consumes its argument (use-after-move becomes an error).
                if let Some(convs) = self.fn_conventions.get(name).cloned() {
                    for (arg, conv) in args.iter().zip(&convs) {
                        match conv {
                            Convention::Inout => match arg {
                                Expr::Var(v) if self.is_mutable(v) == Some(true) => {}
                                Expr::Var(v) => {
                                    return terr(format!(
                                        "`inout` argument `{v}` to `{name}` must be a mutable `var`"
                                    ))
                                }
                                _ => {
                                    return terr(format!(
                                        "`inout` argument to `{name}` must be a mutable variable"
                                    ))
                                }
                            },
                            Convention::Sink => {
                                if let Expr::Var(v) = arg {
                                    self.consumed.insert(v.clone());
                                }
                            }
                            Convention::Let => {}
                        }
                    }
                }
                Ok(ret)
            }
            Expr::Ctor { name, args } => {
                if let Some((fields, result)) = self.ctor_sigs.get(name).cloned() {
                    if fields.len() != args.len() {
                        return terr(format!(
                            "constructor `{name}` takes {} field(s) but got {}",
                            fields.len(),
                            args.len()
                        ));
                    }
                    for (arg, fty) in args.iter().zip(&fields) {
                        let at = self.infer(arg)?;
                        self.unify(fty, &at).map_err(|e| TypeError {
                            message: format!("in constructor `{name}`: {}", e.message),
                        })?;
                    }
                    Ok(result)
                } else {
                    // Unknown constructor (e.g. an actor message): still check
                    // its arguments, but don't constrain the result type.
                    for arg in args {
                        self.infer(arg)?;
                    }
                    Ok(self.fresh())
                }
            }
            Expr::Unary { expr, .. } => {
                let t = self.infer(expr)?;
                match self.resolve(&t) {
                    Ty::Float => {
                        self.unify(&t, &Ty::Float)?;
                        Ok(Ty::Float)
                    }
                    _ => {
                        self.unify(&t, &Ty::Int)?;
                        Ok(Ty::Int)
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => self.infer_binary(*op, lhs, rhs),
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                let ct = self.infer(cond)?;
                self.unify(&Ty::Bool, &ct)
                    .map_err(|e| TypeError { message: format!("`if` condition: {}", e.message) })?;
                let before = self.consumed.clone();
                let tt = self.infer_block(then_block)?;
                let consumed_then = std::mem::replace(&mut self.consumed, before.clone());
                match else_block {
                    Some(eb) => {
                        let et = self.infer_block(eb)?;
                        self.unify(&tt, &et).map_err(|e| TypeError {
                            message: format!("`if` branches disagree: {}", e.message),
                        })?;
                    }
                    None => {
                        self.unify(&tt, &Ty::Nil)?;
                    }
                }
                // A binding consumed on either path is treated as consumed after.
                self.consumed = &consumed_then | &self.consumed;
                Ok(tt)
            }
            Expr::Block(b) => self.infer_block(b),
            Expr::Match { scrutinee, arms } => self.infer_match(scrutinee, arms),
            Expr::Spawn { actor, args } => {
                let Some(field_tys) = self.actor_field_sigs.get(actor).cloned() else {
                    return terr(format!("cannot spawn unknown actor `{actor}`"));
                };
                if field_tys.len() != args.len() {
                    return terr(format!(
                        "spawn {actor}: expects {} argument(s) but got {}",
                        field_tys.len(),
                        args.len()
                    ));
                }
                for (arg, fty) in args.iter().zip(&field_tys) {
                    let at = self.infer(arg)?;
                    self.unify(fty, &at).map_err(|e| TypeError {
                        message: format!("spawning `{actor}`: {}", e.message),
                    })?;
                }
                Ok(Ty::Subject)
            }
        }
    }

    fn infer_binary(&mut self, op: ast::BinOp, lhs: &Expr, rhs: &Expr) -> Result<Ty, TypeError> {
        use ast::BinOp::*;
        let lt = self.infer(lhs)?;
        let rt = self.infer(rhs)?;
        match op {
            Add | Sub | Mul | Div => {
                let either_float =
                    matches!(self.resolve(&lt), Ty::Float) || matches!(self.resolve(&rt), Ty::Float);
                let num = if either_float { Ty::Float } else { Ty::Int };
                self.unify(&lt, &num)?;
                self.unify(&rt, &num)?;
                Ok(num)
            }
            Concat => {
                self.unify(&Ty::String, &lt)?;
                self.unify(&Ty::String, &rt)?;
                Ok(Ty::String)
            }
            Eq | NotEq => {
                self.unify(&lt, &rt)?;
                Ok(Ty::Bool)
            }
            Lt | LtEq | Gt | GtEq => {
                self.unify(&lt, &rt)?;
                Ok(Ty::Bool)
            }
        }
    }

    fn infer_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) -> Result<Ty, TypeError> {
        let st = self.infer(scrutinee)?;
        let result = self.fresh();
        let before = self.consumed.clone();
        let mut merged = before.clone();
        for arm in arms {
            self.consumed = before.clone();
            self.push();
            self.check_pattern(&arm.pattern, &st)?;
            if let Some(guard) = &arm.guard {
                let gt = self.infer(guard)?;
                self.unify(&Ty::Bool, &gt)
                    .map_err(|e| TypeError { message: format!("match guard: {}", e.message) })?;
            }
            let bt = self.infer(&arm.body)?;
            self.unify(&result, &bt).map_err(|e| TypeError {
                message: format!("match arms produce different types: {}", e.message),
            })?;
            self.pop();
            merged = &merged | &self.consumed;
        }
        self.consumed = merged;
        self.check_exhaustive(&st, arms)?;
        Ok(result)
    }

    fn check_pattern(&mut self, pat: &Pattern, expected: &Ty) -> Result<(), TypeError> {
        match pat {
            Pattern::Wildcard => Ok(()),
            Pattern::Var(name) => {
                self.define(name.clone(), expected.clone(), false);
                Ok(())
            }
            Pattern::Int(_) => self.unify(expected, &Ty::Int),
            Pattern::Str(_) => self.unify(expected, &Ty::String),
            Pattern::Bool(_) => self.unify(expected, &Ty::Bool),
            Pattern::Ctor { name, args } => {
                if let Some((fields, result)) = self.ctor_sigs.get(name).cloned() {
                    self.unify(expected, &result)?;
                    if fields.len() != args.len() {
                        return terr(format!(
                            "pattern `{name}` takes {} field(s) but matched {}",
                            fields.len(),
                            args.len()
                        ));
                    }
                    for (p, fty) in args.iter().zip(&fields) {
                        self.check_pattern(p, fty)?;
                    }
                    Ok(())
                } else {
                    // Unknown constructor pattern: bind sub-patterns freely.
                    for p in args {
                        let v = self.fresh();
                        self.check_pattern(p, &v)?;
                    }
                    Ok(())
                }
            }
        }
    }

    /// If the scrutinee is a known sum type, every variant must be covered (or a
    /// wildcard/variable arm must catch the rest).
    fn check_exhaustive(&self, scrut: &Ty, arms: &[MatchArm]) -> Result<(), TypeError> {
        let Ty::Named(adt) = self.resolve(scrut) else {
            return Ok(());
        };
        let Some(variants) = self.adt_variants.get(&adt) else {
            return Ok(());
        };
        let has_catchall = arms.iter().any(|a| {
            a.guard.is_none() && matches!(a.pattern, Pattern::Wildcard | Pattern::Var(_))
        });
        if has_catchall {
            return Ok(());
        }
        let covered: HashSet<&str> = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .filter_map(|a| match &a.pattern {
                Pattern::Ctor { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<&String> = variants.iter().filter(|v| !covered.contains(v.as_str())).collect();
        if missing.is_empty() {
            Ok(())
        } else {
            let names = missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
            terr(format!("non-exhaustive match on `{adt}`: missing {names}"))
        }
    }

    fn check_function(&mut self, func: &Function) -> Result<(), TypeError> {
        let (params, ret) = self.fn_sigs.get(&func.name).cloned().unwrap();
        self.scopes = vec![HashMap::new()];
        self.consumed.clear();
        for (param, ty) in func.params.iter().zip(&params) {
            self.define(param.name.clone(), ty.clone(), param.convention != Convention::Let);
        }
        let body = self.infer_block(&func.body)?;
        self.unify(&ret, &body).map_err(|e| TypeError {
            message: format!("function `{}` body: {}", func.name, e.message),
        })?;
        Ok(())
    }

    fn check_actor(&mut self, actor: &ActorDef) -> Result<(), TypeError> {
        for handler in &actor.handlers {
            self.scopes = vec![HashMap::new()];
            self.consumed.clear();
            for field in &actor.fields {
                let ty = self.to_ty(&field.ty);
                self.define(field.name.clone(), ty, field.mutable);
            }
            self.push();
            for param in &handler.params {
                let ty = param
                    .ty
                    .as_ref()
                    .map(|t| self.to_ty(t))
                    .unwrap_or_else(|| self.fresh());
                self.define(param.name.clone(), ty, param.convention != Convention::Let);
            }
            self.infer_block(&handler.body).map_err(|e| TypeError {
                message: format!("actor `{}` handler `{}`: {}", actor.name, handler.message, e.message),
            })?;
        }
        Ok(())
    }
}

/// Type-check a whole module. Returns the first error found.
pub fn check(module: &Module) -> Result<(), TypeError> {
    let mut c = Checker {
        fn_sigs: HashMap::new(),
        fn_conventions: HashMap::new(),
        ctor_sigs: HashMap::new(),
        adt_variants: HashMap::new(),
        actor_field_sigs: HashMap::new(),
        subst: HashMap::new(),
        next_var: 0,
        scopes: vec![HashMap::new()],
        consumed: HashSet::new(),
    };

    // Pass 1: collect all signatures so definitions can refer to each other.
    for item in &module.items {
        match item {
            Item::Function(f) => {
                let params = f
                    .params
                    .iter()
                    .map(|p| p.ty.as_ref().map(|t| c.to_ty(t)).unwrap_or_else(|| c.fresh()))
                    .collect();
                let ret = f.ret.as_ref().map(|t| c.to_ty(t)).unwrap_or_else(|| c.fresh());
                c.fn_sigs.insert(f.name.clone(), (params, ret));
                c.fn_conventions
                    .insert(f.name.clone(), f.params.iter().map(|p| p.convention).collect());
            }
            Item::Type(t) => {
                let mut names = Vec::new();
                for variant in &t.variants {
                    let fields = variant.fields.iter().map(|ft| c.to_ty(ft)).collect();
                    c.ctor_sigs
                        .insert(variant.name.clone(), (fields, Ty::Named(t.name.clone())));
                    names.push(variant.name.clone());
                }
                c.adt_variants.insert(t.name.clone(), names);
            }
            Item::Actor(a) => {
                // Fields without an initializer are supplied at spawn.
                let field_tys = a
                    .fields
                    .iter()
                    .filter(|f| f.init.is_none())
                    .map(|f| c.to_ty(&f.ty))
                    .collect();
                c.actor_field_sigs.insert(a.name.clone(), field_tys);
            }
        }
    }

    // Pass 2: check bodies.
    for item in &module.items {
        match item {
            Item::Function(f) => c.check_function(f)?,
            Item::Actor(a) => c.check_actor(a)?,
            Item::Type(_) => {}
        }
    }
    Ok(())
}

/// Convenience: parse then type-check.
pub fn check_str(src: &str) -> Result<(), String> {
    let module = crate::parser::parse_module(src).map_err(|e| e.to_string())?;
    check(&module).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_well_typed_program() {
        let src = r#"
            fn double(n: Int) -> Int { n * 2 }
            fn main(console: Console) {
              print(console, int_to_string(double(21)))
            }
        "#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_string_plus_int() {
        let src = r#"fn f() -> String { "a" <> 1 }"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_wrong_arity() {
        let src = r#"
            fn double(n: Int) -> Int { n * 2 }
            fn main() { double(1, 2) }
        "#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("argument"));
    }

    #[test]
    fn rejects_non_bool_if_condition() {
        let src = r#"fn f() -> Int { if 1 { 2 } else { 3 } }"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("if") || e.contains("Bool"));
    }

    #[test]
    fn rejects_return_type_mismatch() {
        let src = r#"fn f() -> Int { "not an int" }"#;
        assert!(check_str(src).is_err());
    }

    /// Capability safety as a type error: `print` needs a `Console`, and a
    /// `String` is not one. Only a `Console`-typed parameter (ultimately from
    /// `main`) can satisfy it.
    #[test]
    fn rejects_print_without_console_capability() {
        let src = r#"fn leak(s: String) -> Nil { print(s, s) }"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("Console"), "expected a Console error, got: {e}");
    }

    #[test]
    fn accepts_print_with_console_capability() {
        let src = r#"fn shout(console: Console, s: String) -> Nil { print(console, s) }"#;
        assert!(check_str(src).is_ok());
    }

    #[test]
    fn checks_adt_constructors_and_exhaustive_match() {
        let src = r#"
            type Event { Click(Int, Int) Closed }
            fn describe(e: Event) -> String {
              match e {
                Click(x, _) -> int_to_string(x)
                Closed -> "closed"
              }
            }
        "#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_non_exhaustive_match() {
        let src = r#"
            type Event { Click(Int, Int) Closed }
            fn describe(e: Event) -> String {
              match e {
                Closed -> "closed"
              }
            }
        "#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("non-exhaustive"), "got: {e}");
    }

    #[test]
    fn rejects_constructor_field_type_mismatch() {
        let src = r#"
            type Event { Click(Int, Int) Closed }
            fn f() -> Event { Click("not an int", 2) }
        "#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn accepts_the_actor_example() {
        let src = r#"
            actor Logger {
              console: Console
              var count: Int = 0
              on Log(msg: String) {
                count = count + 1
                print(console, "[" <> int_to_string(count) <> "] " <> msg)
              }
            }
            fn main(console: Console) {
              let logger = spawn Logger(console)
              send(logger, Log("hello"))
            }
        "#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_assignment_to_let() {
        let src = r#"fn main() { let x = 1  x = 2 }"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("immutable"), "got: {e}");
    }

    #[test]
    fn accepts_assignment_to_var() {
        let src = r#"fn main() { var x = 1  x = 2 }"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_inout_argument_that_is_immutable() {
        let src = r#"
            fn bump(inout n: Int) { n = n + 1 }
            fn main() { let x = 1  bump(x) }
        "#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("inout"), "got: {e}");
    }

    #[test]
    fn accepts_inout_argument_that_is_var() {
        let src = r#"
            fn bump(inout n: Int) { n = n + 1 }
            fn main() { var x = 1  bump(x) }
        "#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_use_after_sink_move() {
        let src = r#"
            fn take(sink s: String) -> String { s }
            fn main() {
              let x = "hi"
              let a = take(x)
              let b = take(x)
            }
        "#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("moved"), "got: {e}");
    }

    #[test]
    fn accepts_reassignment_after_sink_move() {
        let src = r#"
            fn take(sink s: String) -> String { s }
            fn main() {
              var x = "hi"
              take(x)
              x = "again"
              take(x)
            }
        "#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }
}
