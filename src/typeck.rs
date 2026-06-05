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
    self, ActorDef, Block, Convention, Expr, Function, Item, MatchArm, Module, Pattern, Stmt, UnOp,
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
    Listener,
    List(Box<Ty>),
    Tuple(Vec<Ty>),
    /// A user-declared type, possibly with type arguments: `Option(Int)`,
    /// `Result(String, Error)`. Non-generic types carry an empty argument list.
    Named(String, Vec<Ty>),
    /// A function type: parameter types and a return type.
    Fn(Vec<Ty>, Box<Ty>),
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
            Ty::Listener => write!(f, "Listener"),
            Ty::List(e) => write!(f, "List({e})"),
            Ty::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
            Ty::Named(n, args) => {
                write!(f, "{n}")?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, t) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{t}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Ty::Fn(params, ret) => {
                write!(f, "fn(")?;
                for (i, t) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ") -> {ret}")
            }
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

/// Prefix a type error with where it occurred — the enclosing function (after
/// linking this is `module.func`, which also names the file) and source line.
/// `line == 0` means no line is available; an empty `func` omits the name.
fn at_loc(e: TypeError, line: u32, func: &str) -> TypeError {
    if line == 0 {
        return e;
    }
    let where_ = if func.is_empty() {
        format!("line {line}")
    } else {
        format!("`{func}`, line {line}")
    };
    TypeError {
        message: format!("{where_}: {}", e.message),
    }
}

/// Collect the type-parameter names (lowercase, argument-less) appearing in a
/// type expression, in order of first appearance. Used to infer the parameters
/// of a generic ADT from its variant field types.
fn collect_type_params(t: &ast::Type, acc: &mut Vec<String>) {
    match t {
        ast::Type::Tuple(ts) => {
            for x in ts {
                collect_type_params(x, acc);
            }
        }
        ast::Type::Fn(params, ret) => {
            for p in params {
                collect_type_params(p, acc);
            }
            collect_type_params(ret, acc);
        }
        ast::Type::Named(name, args) => {
            if args.is_empty() && name.chars().next().is_some_and(|c| c.is_lowercase()) {
                if !acc.contains(name) {
                    acc.push(name.clone());
                }
            } else {
                for a in args {
                    collect_type_params(a, acc);
                }
            }
        }
    }
}

/// A record type's layout: its type-parameter var ids (in order) and its fields
/// as `(name, type)`. Field types may mention the parameters, instantiated with
/// the value's actual type arguments on access.
type RecordInfo = (Vec<u32>, Vec<(String, Ty)>);

struct Checker {
    fn_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    ctor_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    /// Type-parameter var ids per constructor, so a generic ADT's constructors
    /// are instantiated fresh at each use (e.g. `Some(1)` vs `Some("x")`).
    ctor_typarams: HashMap<String, HashSet<u32>>,
    /// Record types: name -> (type-parameter var ids in order, fields). A field
    /// type may mention the parameters, which are instantiated with the value's
    /// actual type arguments on access.
    record_fields: HashMap<String, RecordInfo>,
    adt_variants: HashMap<String, Vec<String>>,
    actor_field_sigs: HashMap<String, Vec<Ty>>,
    fn_conventions: HashMap<String, Vec<Convention>>,
    /// Per-function type parameters (name, var id), from lowercase type names in
    /// signatures. Generalized: instantiated fresh at each call site.
    fn_typarams: HashMap<String, Vec<(String, u32)>>,
    subst: HashMap<u32, Ty>,
    next_var: u32,
    /// Each binding carries its type and whether it is mutable.
    scopes: Vec<HashMap<String, (Ty, bool)>>,
    /// Bindings that have been consumed (moved out via a `sink` parameter) and
    /// may not be used again until reassigned. Flow-sensitive within a body.
    consumed: HashSet<String>,
    /// The declared return type of the function currently being checked, so `?`
    /// can require the enclosing function to return a matching Result/Option.
    current_ret: Option<Ty>,
    /// Source line of the statement currently being checked, attached to errors
    /// so diagnostics point at a location. 0 means "no line known".
    cur_line: u32,
}

impl Checker {
    fn fresh(&mut self) -> Ty {
        let v = self.next_var;
        self.next_var += 1;
        Ty::Var(v)
    }

    fn to_ty(&mut self, t: &ast::Type) -> Ty {
        let (name, args) = match t {
            ast::Type::Named(name, args) => (name, args),
            ast::Type::Tuple(ts) => {
                return Ty::Tuple(ts.iter().map(|t| self.to_ty(t)).collect());
            }
            ast::Type::Fn(params, ret) => {
                return Ty::Fn(
                    params.iter().map(|t| self.to_ty(t)).collect(),
                    Box::new(self.to_ty(ret)),
                );
            }
        };
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
            "Listener" => Ty::Listener,
            "List" => {
                let elem = match args.first() {
                    Some(a) => self.to_ty(a),
                    None => self.fresh(),
                };
                Ty::List(Box::new(elem))
            }
            _ => Ty::Named(name.clone(), args.iter().map(|a| self.to_ty(a)).collect()),
        }
    }

    /// Like `to_ty`, but a lowercase, argument-less type name becomes a type
    /// *variable* (a parameter), shared within one signature via `vars`.
    fn to_ty_generic(&mut self, t: &ast::Type, vars: &mut HashMap<String, Ty>) -> Ty {
        match t {
            ast::Type::Tuple(ts) => {
                Ty::Tuple(ts.iter().map(|t| self.to_ty_generic(t, vars)).collect())
            }
            ast::Type::Fn(params, ret) => Ty::Fn(
                params.iter().map(|t| self.to_ty_generic(t, vars)).collect(),
                Box::new(self.to_ty_generic(ret, vars)),
            ),
            ast::Type::Named(name, args) => match name.as_str() {
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
                "Listener" => Ty::Listener,
                "List" => {
                    let elem = match args.first() {
                        Some(a) => self.to_ty_generic(a, vars),
                        None => self.fresh(),
                    };
                    Ty::List(Box::new(elem))
                }
                other
                    if args.is_empty()
                        && other.chars().next().is_some_and(|c| c.is_lowercase()) =>
                {
                    if let Some(v) = vars.get(other) {
                        v.clone()
                    } else {
                        let v = self.fresh();
                        vars.insert(other.to_string(), v.clone());
                        v
                    }
                }
                other => Ty::Named(
                    other.to_string(),
                    args.iter().map(|a| self.to_ty_generic(a, vars)).collect(),
                ),
            },
        }
    }

    /// Instantiate a polymorphic signature: replace its generalized type
    /// parameters with fresh vars, so each call site is independent. Other
    /// (inference) vars stay shared, keeping un-annotated functions monomorphic.
    fn instantiate(&mut self, params: &[Ty], ret: &Ty, typarams: &HashSet<u32>) -> (Vec<Ty>, Ty) {
        let mut fresh_map: HashMap<u32, Ty> = HashMap::new();
        for &v in typarams {
            // Checking the function's body may have *bound* the type-param var to
            // another (still-unbound) var — e.g. matching on the param. Key the
            // fresh substitution by that resolved representative, since
            // `subst_vars` resolves before it looks up the map; otherwise the
            // substitution would never apply and the function would behave
            // monomorphically across call sites. A param resolved to a concrete
            // type isn't generic, so skip it.
            if let Ty::Var(rv) = self.resolve(&Ty::Var(v)) {
                fresh_map.entry(rv).or_insert_with(|| self.fresh());
            }
        }
        let p = params.iter().map(|t| self.subst_vars(t, &fresh_map)).collect();
        let r = self.subst_vars(ret, &fresh_map);
        (p, r)
    }

    fn subst_vars(&self, t: &Ty, map: &HashMap<u32, Ty>) -> Ty {
        match self.resolve(t) {
            Ty::Var(v) => map.get(&v).cloned().unwrap_or(Ty::Var(v)),
            Ty::List(e) => Ty::List(Box::new(self.subst_vars(&e, map))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|x| self.subst_vars(x, map)).collect()),
            Ty::Named(n, args) => {
                Ty::Named(n, args.iter().map(|x| self.subst_vars(x, map)).collect())
            }
            Ty::Fn(params, ret) => Ty::Fn(
                params.iter().map(|x| self.subst_vars(x, map)).collect(),
                Box::new(self.subst_vars(&ret, map)),
            ),
            other => other,
        }
    }

    fn resolve(&self, t: &Ty) -> Ty {
        match t {
            Ty::Var(v) => match self.subst.get(v) {
                Some(bound) => self.resolve(bound),
                None => t.clone(),
            },
            Ty::List(e) => Ty::List(Box::new(self.resolve(e))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.resolve(t)).collect()),
            Ty::Named(n, args) => Ty::Named(n.clone(), args.iter().map(|t| self.resolve(t)).collect()),
            Ty::Fn(params, ret) => Ty::Fn(
                params.iter().map(|t| self.resolve(t)).collect(),
                Box::new(self.resolve(ret)),
            ),
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
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
                for (x, y) in xs.iter().zip(ys) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Ty::Named(x, xa), Ty::Named(y, ya)) if x == y && xa.len() == ya.len() => {
                for (p, q) in xa.iter().zip(ya) {
                    self.unify(p, q)?;
                }
                Ok(())
            }
            (Ty::Fn(xp, xr), Ty::Fn(yp, yr)) if xp.len() == yp.len() => {
                for (p, q) in xp.iter().zip(yp) {
                    self.unify(p, q)?;
                }
                self.unify(xr, yr)
            }
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
            "string_length" => Some((vec![Ty::String], Ty::Int)),
            "char_count" => Some((vec![Ty::String], Ty::Int)),
            "to_upper" | "to_lower" | "trim" => Some((vec![Ty::String], Ty::String)),
            "starts_with" | "contains" | "ends_with" => {
                Some((vec![Ty::String, Ty::String], Ty::Bool))
            }
            "index_of" => Some((vec![Ty::String, Ty::String], Ty::Int)),
            "split" => Some((
                vec![Ty::String, Ty::String],
                Ty::List(Box::new(Ty::String)),
            )),
            "replace" => Some((vec![Ty::String, Ty::String, Ty::String], Ty::String)),
            "substring" => Some((vec![Ty::String, Ty::Int, Ty::Int], Ty::String)),
            "int_to_float" => Some((vec![Ty::Int], Ty::Float)),
            "float_to_int" => Some((vec![Ty::Float], Ty::Int)),
            "sqrt" => Some((vec![Ty::Float], Ty::Float)),
            "string_to_int" => Some((vec![Ty::String], Ty::Int)),
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
            "push" => {
                let elem = self.fresh();
                Some((
                    vec![Ty::List(Box::new(elem.clone())), elem.clone()],
                    Ty::List(Box::new(elem)),
                ))
            }
            "concat" => {
                let elem = self.fresh();
                let list = Ty::List(Box::new(elem));
                Some((vec![list.clone(), list.clone()], list))
            }
            // Dict(k, v) is an ordinary parameterized Named type; these builtins
            // are generic in its key and value types.
            "dict_new" => {
                let k = self.fresh();
                let v = self.fresh();
                Some((vec![], Ty::Named("Dict".into(), vec![k, v])))
            }
            "insert" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                Some((vec![d.clone(), k, v], d))
            }
            "get_or" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                Some((vec![d, k, v.clone()], v))
            }
            "has" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v]);
                Some((vec![d, k], Ty::Bool))
            }
            "remove" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v]);
                Some((vec![d.clone(), k], d))
            }
            "keys" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v]);
                Some((vec![d], Ty::List(Box::new(k))))
            }
            "values" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k, v.clone()]);
                Some((vec![d], Ty::List(Box::new(v))))
            }
            "pairs" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                Some((vec![d], Ty::List(Box::new(Ty::Tuple(vec![k, v])))))
            }
            "size" => {
                let k = self.fresh();
                let v = self.fresh();
                Some((vec![Ty::Named("Dict".into(), vec![k, v])], Ty::Int))
            }
            "read" => Some((vec![Ty::Dir, Ty::String], Ty::String)),
            "subdir" => Some((vec![Ty::Dir, Ty::String], Ty::Dir)),
            "connect" => Some((vec![Ty::Net, Ty::String], Ty::Socket)),
            "restrict" => Some((vec![Ty::Net, Ty::String], Ty::Net)),
            "send_line" => Some((vec![Ty::Socket, Ty::String], Ty::Nil)),
            "send_bytes" => Some((vec![Ty::Socket, Ty::String], Ty::Nil)),
            "recv_line" => Some((vec![Ty::Socket], Ty::String)),
            "recv_all" => Some((vec![Ty::Socket], Ty::String)),
            "recv_bytes" => Some((vec![Ty::Socket, Ty::Int], Ty::String)),
            "listen" => Some((vec![Ty::Net, Ty::String], Ty::Listener)),
            "accept" => Some((vec![Ty::Listener], Ty::Socket)),
            "close" => Some((vec![Ty::Socket], Ty::Nil)),
            // User functions: instantiate generic type parameters fresh per call.
            _ => match self.fn_sigs.get(name).cloned() {
                Some((params, ret)) => {
                    let typarams: HashSet<u32> = self
                        .fn_typarams
                        .get(name)
                        .into_iter()
                        .flatten()
                        .map(|(_, id)| *id)
                        .collect();
                    Some(self.instantiate(&params, &ret, &typarams))
                }
                None => None,
            },
        }
    }

    // --- inference ---

    fn infer_block(&mut self, block: &Block) -> Result<Ty, TypeError> {
        self.push();
        let mut ty = Ty::Nil;
        for (i, stmt) in block.stmts.iter().enumerate() {
            if let Some(line) = block.lines.get(i) {
                self.cur_line = *line;
            }
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
                Stmt::LetTuple { names, value } => {
                    let vt = self.infer(value)?;
                    let elem_tys: Vec<Ty> = (0..names.len()).map(|_| self.fresh()).collect();
                    self.unify(&Ty::Tuple(elem_tys.clone()), &vt).map_err(|e| TypeError {
                        message: format!("tuple destructure: {}", e.message),
                    })?;
                    for (n, t) in names.iter().zip(elem_tys) {
                        self.define(n.clone(), t, false);
                    }
                    ty = Ty::Nil;
                }
                Stmt::Return(opt) => {
                    let t = match opt {
                        Some(e) => self.infer(e)?,
                        None => Ty::Nil,
                    };
                    if let Some(ret) = self.current_ret.clone() {
                        self.unify(&ret, &t).map_err(|e| TypeError {
                            message: format!("`return` value: {}", e.message),
                        })?;
                    }
                    // A return diverges: its position can satisfy any expected
                    // type, so contribute a fresh var (which unifies with anything).
                    ty = self.fresh();
                }
                Stmt::Expr(e) => {
                    ty = self.infer(e)?;
                }
                // `break`/`continue` diverge (control leaves the block), so like
                // `return` they contribute a fresh var that unifies with any
                // expected type — letting `match x { _ -> { break } ... }` work.
                // Misuse outside a loop is caught by codegen (no enclosing label).
                Stmt::Break | Stmt::Continue => {
                    ty = self.fresh();
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
            Expr::Tuple(items) => {
                let tys = items
                    .iter()
                    .map(|e| self.infer(e))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Ty::Tuple(tys))
            }
            Expr::Var(name) => {
                if self.consumed.contains(name) {
                    return terr(format!(
                        "use of `{name}` after it was moved (consumed by a `sink` parameter)"
                    ));
                }
                if let Some(t) = self.lookup(name) {
                    return Ok(t);
                }
                // A bare top-level function name used as a value is a first-class
                // function. Reject `inout`/`sink` functions, whose move-in/out
                // calling convention can't be expressed as a plain value.
                if let Some((params, ret)) = self.fn_sigs.get(name).cloned() {
                    if let Some(convs) = self.fn_conventions.get(name) {
                        if convs.iter().any(|c| *c != Convention::Let) {
                            return terr(format!(
                                "`{name}` takes an `inout`/`sink` parameter, so it can't be used as a function value"
                            ));
                        }
                    }
                    let typarams: HashSet<u32> = self
                        .fn_typarams
                        .get(name)
                        .into_iter()
                        .flatten()
                        .map(|(_, id)| *id)
                        .collect();
                    let (params, ret) = self.instantiate(&params, &ret, &typarams);
                    return Ok(Ty::Fn(params, Box::new(ret)));
                }
                terr(format!("unbound variable `{name}`"))
            }
            Expr::Lambda { params, body } => {
                self.push();
                let param_tys: Vec<Ty> = params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => self.to_ty(t),
                        None => self.fresh(),
                    })
                    .collect();
                for (p, ty) in params.iter().zip(&param_tys) {
                    self.define(p.name.clone(), ty.clone(), p.convention != Convention::Let);
                }
                let ret = self.infer_block(body)?;
                self.pop();
                Ok(Ty::Fn(param_tys, Box::new(ret)))
            }
            Expr::Call { name, args } => {
                // A local binding (parameter or `let`) holding a function value:
                // apply it. Handles both an explicit `fn(..)->..` type and an as
                // yet unconstrained variable (which we pin to a function type).
                if let Some(vty) = self.lookup(name) {
                    match self.resolve(&vty) {
                        Ty::Fn(param_tys, ret) => {
                            if param_tys.len() != args.len() {
                                return terr(format!(
                                    "`{name}` expects {} argument(s) but got {}",
                                    param_tys.len(),
                                    args.len()
                                ));
                            }
                            for (arg, pty) in args.iter().zip(&param_tys) {
                                let at = self.infer(arg)?;
                                self.unify(pty, &at).map_err(|e| TypeError {
                                    message: format!("in call to `{name}`: {}", e.message),
                                })?;
                            }
                            return Ok(*ret);
                        }
                        Ty::Var(_) => {
                            let mut argtys = Vec::new();
                            for arg in args {
                                argtys.push(self.infer(arg)?);
                            }
                            let ret = self.fresh();
                            self.unify(&vty, &Ty::Fn(argtys, Box::new(ret.clone())))?;
                            return Ok(ret);
                        }
                        _ => {} // a non-function local with this name: fall through
                    }
                }
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
            Expr::Apply { func, args } => {
                // The callee is an arbitrary expression of function type; unify
                // it with `fn(argtys) -> r` and yield `r`.
                let fty = self.infer(func)?;
                let mut argtys = Vec::new();
                for arg in args {
                    argtys.push(self.infer(arg)?);
                }
                let ret = self.fresh();
                self.unify(&fty, &Ty::Fn(argtys, Box::new(ret.clone())))
                    .map_err(|e| TypeError {
                        message: format!("in function application: {}", e.message),
                    })?;
                Ok(ret)
            }
            Expr::Ctor { name, args } => {
                if let Some((fields, result)) = self.ctor_sigs.get(name).cloned() {
                    let typarams = self.ctor_typarams.get(name).cloned().unwrap_or_default();
                    let (fields, result) = self.instantiate(&fields, &result, &typarams);
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
            Expr::Unary { op, expr } => {
                let t = self.infer(expr)?;
                match op {
                    UnOp::Not => {
                        self.unify(&Ty::Bool, &t)?;
                        Ok(Ty::Bool)
                    }
                    UnOp::Neg => match self.resolve(&t) {
                        Ty::Float => {
                            self.unify(&t, &Ty::Float)?;
                            Ok(Ty::Float)
                        }
                        _ => {
                            self.unify(&t, &Ty::Int)?;
                            Ok(Ty::Int)
                        }
                    },
                    UnOp::BitNot => {
                        self.unify(&Ty::Int, &t)?;
                        Ok(Ty::Int)
                    }
                }
            }
            Expr::Field { base, field } => {
                let bt = self.infer(base)?;
                let resolved = self.resolve(&bt);
                let Ty::Named(tyname, args) = &resolved else {
                    return terr(format!(
                        "field access `.{field}` requires a record, found `{resolved}`"
                    ));
                };
                let Some((params, fields)) = self.record_fields.get(tyname).cloned() else {
                    return terr(format!("type `{tyname}` is not a record, so it has no field `{field}`"));
                };
                let Some((_, fty)) = fields.iter().find(|(n, _)| n == field) else {
                    return terr(format!("record `{tyname}` has no field `{field}`"));
                };
                // Instantiate the field type with the value's actual type args.
                let map: HashMap<u32, Ty> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                Ok(self.subst_vars(fty, &map))
            }
            Expr::RecordUpdate { base, fields } => {
                let bt = self.infer(base)?;
                let resolved = self.resolve(&bt);
                let (tyname, args) = match &resolved {
                    Ty::Named(n, a) => (n.clone(), a.clone()),
                    other => {
                        return terr(format!("`update` requires a record, found `{other}`"))
                    }
                };
                let Some((params, rec_fields)) = self.record_fields.get(&tyname).cloned() else {
                    return terr(format!("type `{tyname}` is not a record"));
                };
                let map: HashMap<u32, Ty> =
                    params.into_iter().zip(args).collect();
                for (fname, vexpr) in fields {
                    let Some((_, fty)) = rec_fields.iter().find(|(n, _)| n == fname) else {
                        return terr(format!("record `{tyname}` has no field `{fname}`"));
                    };
                    let expected = self.subst_vars(fty, &map);
                    let vt = self.infer(vexpr)?;
                    self.unify(&expected, &vt).map_err(|e| TypeError {
                        message: format!("`update` of field `{fname}`: {}", e.message),
                    })?;
                }
                // The result is a record of the same type as the base.
                Ok(resolved)
            }
            Expr::Try(inner) => {
                let it = self.infer(inner)?;
                let resolved = self.resolve(&it);
                let (value_ty, expected_ret) = match &resolved {
                    Ty::Named(n, args) if n == "Result" && args.len() == 2 => {
                        let r = self.fresh();
                        (
                            args[0].clone(),
                            Ty::Named("Result".into(), vec![r, args[1].clone()]),
                        )
                    }
                    Ty::Named(n, args) if n == "Option" && args.len() == 1 => {
                        let r = self.fresh();
                        (args[0].clone(), Ty::Named("Option".into(), vec![r]))
                    }
                    other => {
                        return terr(format!(
                            "`?` expects a Result or Option, found `{other}`"
                        ))
                    }
                };
                let Some(ret) = self.current_ret.clone() else {
                    return terr("`?` can only be used inside a function returning Result or Option");
                };
                self.unify(&ret, &expected_ret).map_err(|e| TypeError {
                    message: format!(
                        "`?` propagates from a `{resolved}`, but the enclosing function returns a different type: {}",
                        e.message
                    ),
                })?;
                Ok(value_ty)
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
            Expr::While { cond, body } => {
                let ct = self.infer(cond)?;
                self.unify(&Ty::Bool, &ct).map_err(|e| TypeError {
                    message: format!("`while` condition: {}", e.message),
                })?;
                self.infer_block(body)?;
                Ok(Ty::Nil)
            }
            Expr::For { var, iter, body } => {
                let it = self.infer(iter)?;
                let elem = self.fresh();
                self.unify(&Ty::List(Box::new(elem.clone())), &it).map_err(|e| TypeError {
                    message: format!("`for` expects a List to iterate: {}", e.message),
                })?;
                self.push();
                self.define(var.clone(), elem, false);
                self.infer_block(body)?;
                self.pop();
                Ok(Ty::Nil)
            }
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
            Mod | BitAnd | BitOr | BitXor | Shl | Shr => {
                self.unify(&Ty::Int, &lt)?;
                self.unify(&Ty::Int, &rt)?;
                Ok(Ty::Int)
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
                // Ordering is defined only for the totally-ordered primitives.
                // Without a type-class mechanism, allowing it on arbitrary types
                // would type-check but crash at runtime, so reject it here.
                match self.resolve(&lt) {
                    Ty::Int | Ty::Float | Ty::String => Ok(Ty::Bool),
                    other => terr(format!(
                        "ordering comparison requires Int, Float, or String, found `{other}`"
                    )),
                }
            }
            And | Or => {
                self.unify(&Ty::Bool, &lt)?;
                self.unify(&Ty::Bool, &rt)?;
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
        self.check_unreachable(arms)?;
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
            Pattern::Tuple(pats) => {
                let elem_tys: Vec<Ty> = (0..pats.len()).map(|_| self.fresh()).collect();
                self.unify(expected, &Ty::Tuple(elem_tys.clone()))?;
                for (p, t) in pats.iter().zip(elem_tys) {
                    self.check_pattern(p, &t)?;
                }
                Ok(())
            }
            Pattern::List { elems, rest } => {
                let elem = self.fresh();
                self.unify(expected, &Ty::List(Box::new(elem.clone())))?;
                for p in elems {
                    self.check_pattern(p, &elem)?;
                }
                if let Some(Some(name)) = rest {
                    self.define(name.clone(), Ty::List(Box::new(elem)), false);
                }
                Ok(())
            }
            Pattern::Ctor { name, args } => {
                if let Some((fields, result)) = self.ctor_sigs.get(name).cloned() {
                    let typarams = self.ctor_typarams.get(name).cloned().unwrap_or_default();
                    let (fields, result) = self.instantiate(&fields, &result, &typarams);
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

    /// Flag arms that can never match because an earlier arm already covers
    /// them — dead code that is almost always a bug (a typo'd duplicate, or arms
    /// placed after a catch-all). Conservative: a guarded arm never establishes
    /// coverage (its guard may fail at runtime), and a constructor arm only
    /// covers its variant when all its fields are irrefutable (`_`/binding), so
    /// `Some(0)` followed by `Some(n)` is correctly left reachable.
    fn check_unreachable(&self, arms: &[MatchArm]) -> Result<(), TypeError> {
        let mut saturated = false;
        let mut ctors: HashSet<&str> = HashSet::new();
        let mut ints: HashSet<i64> = HashSet::new();
        let mut strs: HashSet<&str> = HashSet::new();
        let mut bools: HashSet<bool> = HashSet::new();
        for arm in arms {
            let already = saturated
                || match &arm.pattern {
                    Pattern::Ctor { name, .. } => ctors.contains(name.as_str()),
                    Pattern::Int(n) => ints.contains(n),
                    Pattern::Str(s) => strs.contains(s.as_str()),
                    Pattern::Bool(b) => bools.contains(b),
                    _ => false,
                };
            if already {
                return terr(format!(
                    "unreachable match arm: `{}` is already covered by an earlier arm",
                    describe_pattern(&arm.pattern)
                ));
            }
            if arm.guard.is_none() {
                match &arm.pattern {
                    Pattern::Wildcard | Pattern::Var(_) => saturated = true,
                    Pattern::Ctor { name, args }
                        if args
                            .iter()
                            .all(|p| matches!(p, Pattern::Wildcard | Pattern::Var(_))) =>
                    {
                        ctors.insert(name.as_str());
                    }
                    Pattern::Int(n) => {
                        ints.insert(*n);
                    }
                    Pattern::Str(s) => {
                        strs.insert(s.as_str());
                    }
                    Pattern::Bool(b) => {
                        bools.insert(*b);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// If the scrutinee is a known sum type, every variant must be covered (or a
    /// wildcard/variable arm must catch the rest). `Bool` is treated as a
    /// two-variant sum (`true`/`false`), so an incomplete Bool match is rejected
    /// just like an incomplete ADT match.
    fn check_exhaustive(&self, scrut: &Ty, arms: &[MatchArm]) -> Result<(), TypeError> {
        let resolved = self.resolve(scrut);
        let has_catchall = arms.iter().any(|a| {
            a.guard.is_none() && matches!(a.pattern, Pattern::Wildcard | Pattern::Var(_))
        });
        if has_catchall {
            return Ok(());
        }
        if matches!(resolved, Ty::Bool) {
            let covers = |want: bool| {
                arms.iter()
                    .any(|a| a.guard.is_none() && matches!(a.pattern, Pattern::Bool(b) if b == want))
            };
            if covers(true) && covers(false) {
                return Ok(());
            }
            return terr(
                "non-exhaustive match on `Bool`: cover both `true` and `false` (or add `_`)",
            );
        }
        let Ty::Named(adt, _) = resolved else {
            return Ok(());
        };
        let Some(variants) = self.adt_variants.get(&adt) else {
            return Ok(());
        };
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
        self.current_ret = Some(ret.clone());
        self.cur_line = 0;
        for (param, ty) in func.params.iter().zip(&params) {
            self.define(param.name.clone(), ty.clone(), param.convention != Convention::Let);
        }
        let body = self.infer_block(&func.body)?;
        self.unify(&ret, &body).map_err(|e| TypeError {
            message: format!("function `{}` body: {}", func.name, e.message),
        })?;
        // Soundness: a declared type parameter must stay free (truly generic).
        // If the body pinned it to a concrete type, the signature is misleading.
        if let Some(typarams) = self.fn_typarams.get(&func.name).cloned() {
            for (pname, v) in typarams {
                let resolved = self.resolve(&Ty::Var(v));
                if !matches!(resolved, Ty::Var(_)) {
                    return terr(format!(
                        "function `{}`: type parameter `{pname}` is used as `{resolved}`, so it isn't generic",
                        func.name
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_actor(&mut self, actor: &ActorDef) -> Result<(), TypeError> {
        for handler in &actor.handlers {
            self.scopes = vec![HashMap::new()];
            self.consumed.clear();
            self.current_ret = None;
            self.cur_line = 0;
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
    // Trait/impl declarations are desugared to ordinary functions first, so the
    // checker only ever sees plain functions (a no-op for trait-free modules).
    let lowered = crate::traits::lower(module.clone());
    let module = &lowered;
    let mut c = Checker {
        fn_sigs: HashMap::new(),
        fn_conventions: HashMap::new(),
        ctor_sigs: HashMap::new(),
        ctor_typarams: HashMap::new(),
        record_fields: HashMap::new(),
        adt_variants: HashMap::new(),
        actor_field_sigs: HashMap::new(),
        fn_typarams: HashMap::new(),
        subst: HashMap::new(),
        next_var: 0,
        scopes: vec![HashMap::new()],
        consumed: HashSet::new(),
        current_ret: None,
        cur_line: 0,
    };

    // Pass 1: collect all signatures so definitions can refer to each other.
    for item in &module.items {
        match item {
            Item::Function(f) => {
                let mut vars: HashMap<String, Ty> = HashMap::new();
                let params: Vec<Ty> = f
                    .params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => c.to_ty_generic(t, &mut vars),
                        None => c.fresh(),
                    })
                    .collect();
                let ret = match &f.ret {
                    Some(t) => c.to_ty_generic(t, &mut vars),
                    None => c.fresh(),
                };
                c.fn_sigs.insert(f.name.clone(), (params, ret));
                let typarams: Vec<(String, u32)> = vars
                    .into_iter()
                    .filter_map(|(name, ty)| match ty {
                        Ty::Var(v) => Some((name, v)),
                        _ => None,
                    })
                    .collect();
                c.fn_typarams.insert(f.name.clone(), typarams);
                c.fn_conventions
                    .insert(f.name.clone(), f.params.iter().map(|p| p.convention).collect());
            }
            Item::Type(t) => {
                // A type's parameters are the lowercase, argument-less names that
                // appear in its variants' field types, in order of first
                // appearance (so `type Option { Some(a) None }` has one param `a`).
                let mut param_names: Vec<String> = Vec::new();
                for variant in &t.variants {
                    for ft in &variant.fields {
                        collect_type_params(ft, &mut param_names);
                    }
                }
                let mut vars: HashMap<String, Ty> = HashMap::new();
                let mut typaram_ids: HashSet<u32> = HashSet::new();
                let mut params_in_order: Vec<u32> = Vec::new();
                let mut result_args: Vec<Ty> = Vec::new();
                for pn in &param_names {
                    let v = c.fresh();
                    if let Ty::Var(id) = v {
                        typaram_ids.insert(id);
                        params_in_order.push(id);
                    }
                    vars.insert(pn.clone(), v.clone());
                    result_args.push(v);
                }
                let result = Ty::Named(t.name.clone(), result_args);
                let mut names = Vec::new();
                for variant in &t.variants {
                    let fields: Vec<Ty> = variant
                        .fields
                        .iter()
                        .map(|ft| c.to_ty_generic(ft, &mut vars))
                        .collect();
                    // A record variant carries field names: remember them (with
                    // the type's parameters) so `value.field` can be typed.
                    if !variant.field_names.is_empty() {
                        let rec: Vec<(String, Ty)> = variant
                            .field_names
                            .iter()
                            .cloned()
                            .zip(fields.iter().cloned())
                            .collect();
                        c.record_fields
                            .insert(t.name.clone(), (params_in_order.clone(), rec));
                    }
                    c.ctor_sigs
                        .insert(variant.name.clone(), (fields, result.clone()));
                    c.ctor_typarams
                        .insert(variant.name.clone(), typaram_ids.clone());
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
            // Desugared to functions by `traits::lower` before this point.
            Item::Trait(_) | Item::Impl(_) => {}
        }
    }

    // Pass 2: check bodies.
    for item in &module.items {
        match item {
            Item::Function(f) => {
                c.check_function(f).map_err(|e| at_loc(e, c.cur_line, &f.name))?
            }
            // Actor handler errors already carry actor/handler context.
            Item::Actor(a) => c.check_actor(a).map_err(|e| at_loc(e, c.cur_line, ""))?,
            Item::Type(_) | Item::Trait(_) | Item::Impl(_) => {}
        }
    }
    Ok(())
}

/// Convenience: parse then type-check.
pub fn check_str(src: &str) -> Result<(), String> {
    let module = crate::parser::parse_module(src).map_err(|e| e.to_string())?;
    check(&module).map_err(|e| e.to_string())
}

/// A short, human-readable rendering of a pattern for diagnostics.
fn describe_pattern(p: &Pattern) -> String {
    match p {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Var(n) => n.clone(),
        Pattern::Int(n) => n.to_string(),
        Pattern::Str(s) => format!("\"{s}\""),
        Pattern::Bool(b) => b.to_string(),
        Pattern::Ctor { name, .. } => name.clone(),
        Pattern::Tuple(_) => "tuple pattern".to_string(),
        Pattern::List { .. } => "list pattern".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_well_typed_program() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main(console: Console):
    print(console, int_to_string(double(21)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_string_plus_int() {
        let src = r#"
fn f() -> String:
    ("a" <> 1)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn capabilities_do_not_leak_across_kinds() {
        // Holding one capability never confers another. A function given only a
        // Console cannot reach the network or the filesystem: `connect` demands
        // a Net and `read` demands a Dir, and a Console can't stand in for
        // either. Authority is per-kind and (with no capability constructors)
        // unforgeable — the heart of witchy's confinement guarantee.
        let net = check_str(r#"
fn f(c: Console) -> Nil:
    connect(c, "host")
"#).unwrap_err();
        assert!(net.contains("Net"), "expected a Net mismatch, got: {net}");
        let dir = check_str(r#"
fn f(c: Console) -> String:
    read(c, "/etc/passwd")
"#)
            .unwrap_err();
        assert!(dir.contains("Dir"), "expected a Dir mismatch, got: {dir}");
    }

    #[test]
    fn rejects_wrong_arity() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main():
    double(1, 2)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("argument"));
    }

    #[test]
    fn rejects_tuple_arity_mismatch() {
        assert!(check_str(r#"
fn main():
    let (a, b, c) = (1, 2)
"#).is_err());
    }

    #[test]
    fn accepts_tuple_destructure() {
        assert!(check_str(r#"
fn main():
    let (a, b) = (1, 2)
"#).is_ok());
    }

    #[test]
    fn generic_function_used_at_multiple_types() {
        let src = r#"
fn id(x: a) -> a:
    x

fn main(console: Console):
    print(console, id("hi"))
    print(console, int_to_string(id(5)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_over_constrained_type_param() {
        // `a` can't be generic if the body forces it to Int.
        assert!(check_str("fn bad(x: a) -> a { x + 1 }").is_err());
    }

    #[test]
    fn generic_adt_used_at_multiple_types() {
        // A generic `Box(a)` can be unwrapped at both Int and String.
        let src = r#"
type Box:
    Wrap(a)

fn unwrap_int(b: Box(Int)) -> Int:
    match b:
        Wrap(n) -> n

fn unwrap_str(b: Box(String)) -> String:
    match b:
        Wrap(s) -> s

fn main(console: Console):
    print(console, int_to_string(unwrap_int(Wrap(5))))
    print(console, unwrap_str(Wrap("hi")))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn generic_function_with_binding_body_at_multiple_types() {
        // The same generic function — whose body *binds* its type parameter (here
        // by matching on it) — called at two different types in one program. This
        // regressed previously: checking the body bound the type-param var, and
        // instantiation then reused that binding instead of a fresh one per call.
        let src = r#"
type Box:
    Wrap(a)

fn unwrap(b: Box(a), default: a) -> a:
    match b:
        Wrap(v) -> v

fn main(console: Console):
    print(console, int_to_string(unwrap(Wrap(5), 0)))
    print(console, unwrap(Wrap("hi"), "none"))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn early_return_type_checks_including_divergence() {
        // A guard `return` in an if-branch (no else) must not force the branch to
        // the function's return type — divergence is handled.
        let src = r#"
fn classify(n: Int) -> String:
    if (n < 0):
        return "neg"
    "nonneg"

fn only_return() -> Int:
    return 5
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_return_of_wrong_type() {
        assert!(check_str("fn f() -> Int { return \"x\" }").is_err());
    }

    #[test]
    fn type_errors_report_function_and_source_line() {
        // The mismatch is on the third line, inside function `f`.
        let src = r#"fn f() -> Int:
    let a = 1
    (a + "x")
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("line 3"), "expected a line number, got: {e}");
        assert!(e.contains("`f`"), "expected the function name, got: {e}");
    }

    #[test]
    fn ordering_allows_comparable_primitives() {
        assert!(check_str(r#"
fn f(a: Int, b: Int) -> Bool:
    (a < b)
"#).is_ok());
        assert!(check_str(r#"
fn f(a: Float, b: Float) -> Bool:
    (a >= b)
"#).is_ok());
        assert!(check_str(r#"
fn f(a: String, b: String) -> Bool:
    (a < b)
"#).is_ok());
    }

    #[test]
    fn rejects_ordering_on_non_primitives() {
        // These would type-check under bare unification but crash at runtime, so
        // the checker rejects them up front.
        assert!(check_str(r#"
fn f(a: Bool, b: Bool) -> Bool:
    (a < b)
"#).is_err());
        assert!(check_str(r#"
fn f(a: List(Int), b: List(Int)) -> Bool:
    (a < b)
"#).is_err());
        assert!(check_str(r#"
fn f(a: (Int, Int), b: (Int, Int)) -> Bool:
    (a < b)
"#).is_err());
    }

    #[test]
    fn equality_still_works_on_any_matching_type() {
        // `==` is unaffected — structural equality is defined for every value.
        assert!(check_str(r#"
fn f(a: (Int, Int), b: (Int, Int)) -> Bool:
    (a == b)
"#).is_ok());
    }

    #[test]
    fn dict_builtins_are_generic() {
        let src = r#"
fn tally(words: List(String)) -> Int:
    var d = dict_new()
    for w in words:
        d = insert(d, w, (get_or(d, w, 0) + 1))
    size(d)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_dict_key_type_mismatch() {
        // The dict's key type is fixed by the first insert (String here), so
        // looking it up with an Int key must fail.
        let src = r#"
fn f() -> Int:
    let d = insert(dict_new(), "a", 1)
    get_or(d, 2, 0)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn string_builtins_type() {
        let src = r#"
fn first_field(row: String) -> String:
    at(split(row, ","), 0)

fn has(s: String, sub: String) -> Bool:
    contains(s, sub)

fn fix(s: String) -> String:
    replace(s, "a", "b")
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_split_on_non_string() {
        assert!(check_str("fn f() -> List(String) { split(5, \",\") }").is_err());
    }

    #[test]
    fn push_and_concat_are_generic() {
        let src = r#"
fn ints() -> List(Int):
    push([1, 2], 3)

fn strs() -> List(String):
    concat(["a"], ["b", "c"])
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_push_element_type_mismatch() {
        // Pushing a String onto a List(Int) must fail.
        assert!(check_str("fn f() -> List(Int) { push([1, 2], \"x\") }").is_err());
    }

    #[test]
    fn higher_order_and_lambda_type() {
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    print(console, int_to_string(apply(fn(n: Int): (n + 1), 10)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn generic_higher_order_function() {
        // `apply` is generic over the value type `a`; the explicit fn-type
        // parameter keeps the type parameters free.
        let src = r#"
fn apply(f: fn(a) -> a, x: a) -> a:
    f(x)

fn main(console: Console):
    print(console, apply(fn(s: String): s, "hi"))
    print(console, int_to_string(apply(fn(n: Int): n, 5)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_lambda_argument_type_mismatch() {
        // Passing a `fn(Int)->Int` where a `fn(String)->String` is required fails.
        let src = r#"
fn run(f: fn(String) -> String, s: String) -> String:
    f(s)

fn main(console: Console):
    print(console, run(fn(n: Int): (n + 1), "x"))
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn record_update_types() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn bump(p: Point) -> Point:
    update p: x = ((p).x + 1)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_record_update_wrong_field_type() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn bad(p: Point) -> Point:
    update p: x = "no"
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_record_update_unknown_field() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn bad(p: Point) -> Point:
    update p: z = 1
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn record_field_access_types() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn sum(p: Point) -> Int:
    ((p).x + (p).y)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_unknown_record_field() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn f(p: Point) -> Int:
    (p).z
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_field_access_on_non_record() {
        assert!(check_str("fn f(n: Int) -> Int { n.x }").is_err());
    }

    #[test]
    fn generic_record_field_instantiates() {
        // `value`'s type is the parameter `a`; reading `.value` on a `Box(Int)`
        // must yield Int (and concatenating it as a string must fail).
        let ok = r#"
type Box:
    value: a

fn unwrap(b: Box(Int)) -> Int:
    (b).value
"#;
        assert!(check_str(ok).is_ok(), "{:?}", check_str(ok));
        let bad = r#"
type Box:
    value: a

fn unwrap(b: Box(Int)) -> String:
    (b).value
"#;
        assert!(check_str(bad).is_err());
    }

    #[test]
    fn list_pattern_binds_element_and_tail() {
        // `head` is the element type, `tail` is a list of the same element type.
        let src = r#"
fn f(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [head, ..tail] -> (head + f(tail))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_list_pattern_element_misuse() {
        // Binding a list element as Int then concatenating it as a String fails.
        let src = r#"
fn f(xs: List(Int)) -> String:
    match xs:
        [] -> ""
        [head, ..] -> (head <> "!")
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn for_in_binds_element_type() {
        let src = r#"
fn main(console: Console):
    for n in [1, 2, 3]:
        print(console, int_to_string(n))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_for_over_non_list() {
        let src = r#"
fn main(console: Console):
    for x in 5:
        print(console, "x")
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn try_operator_propagates_result() {
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn parse(s: String) -> Result(Int, String):
    Ok(string_to_int(s))

fn add(a: String, b: String) -> Result(Int, String):
    let x = (parse(a))?
    let y = (parse(b))?
    Ok((x + y))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_try_when_error_types_differ() {
        // `?` yields `Err(String)`, but the function returns `Result(Int, Int)`,
        // so the error types can't match.
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn src_fn() -> Result(Int, String):
    Err("x")

fn bad() -> Result(Int, Int):
    let v = (src_fn())?
    Ok(v)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_try_on_non_result() {
        // `?` on a plain Int is meaningless.
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn bad(n: Int) -> Result(Int, String):
    Ok((n)?)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_arm_after_catchall() {
        let src = r#"
fn f(n: Int) -> Int:
    match n:
        _ -> 0
        1 -> 2
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("unreachable"), "got: {e}");
    }

    #[test]
    fn rejects_duplicate_variant_arm() {
        let src = r#"
type Opt:
    Some(a)
    None

fn f(o: Opt(Int)) -> Int:
    match o:
        Some(x) -> x
        Some(y) -> y
        None -> 0
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("unreachable"), "got: {e}");
    }

    #[test]
    fn rejects_duplicate_literal_arm() {
        let src = r#"
fn f(n: Int) -> Int:
    match n:
        1 -> 1
        1 -> 2
        _ -> 0
"#;
        assert!(check_str(src).unwrap_err().contains("unreachable"));
    }

    #[test]
    fn allows_specific_then_general_constructor_arm() {
        // `Some(0)` is refutable, so a following `Some(n)` is still reachable —
        // the unreachable check must NOT flag this valid program.
        let src = r#"
type Opt:
    Some(a)
    None

fn f(o: Opt(Int)) -> Int:
    match o:
        Some(0) -> 1
        Some(n) -> n
        None -> 0
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn allows_guarded_arm_before_same_variant() {
        // A guarded arm may fail at runtime, so it does not cover its variant; a
        // later unguarded arm for that variant stays reachable.
        let src = r#"
type Opt:
    Some(a)
    None

fn f(o: Opt(Int)) -> Int:
    match o:
        Some(x) if (x > 0) -> 1
        Some(y) -> y
        None -> 0
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_non_exhaustive_bool_match() {
        let src = r#"
fn f(b: Bool) -> Int:
    match b:
        true -> 1
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("non-exhaustive") && e.contains("Bool"), "got: {e}");
    }

    #[test]
    fn allows_complete_bool_match() {
        assert!(check_str(r#"
fn f(b: Bool) -> Int:
    match b:
        true -> 1
        false -> 0
"#).is_ok());
        assert!(check_str(r#"
fn f(b: Bool) -> Int:
    match b:
        true -> 1
        _ -> 0
"#).is_ok());
    }

    #[test]
    fn rejects_generic_adt_type_mismatch() {
        // `Box(Int)` and `Box(String)` are distinct: passing one for the other
        // must fail to unify their type arguments.
        let src = r#"
type Box:
    Wrap(a)

fn need_int(b: Box(Int)) -> Int:
    match b:
        Wrap(n) -> n

fn main() -> Int:
    need_int(Wrap("nope"))
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn rejects_and_on_non_bool() {
        assert!(check_str("fn f() -> Bool { 1 && true }").is_err());
    }

    #[test]
    fn rejects_non_bool_if_condition() {
        let src = r#"
fn f() -> Int:
    if 1:
        2
    else:
        3
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("if") || e.contains("Bool"));
    }

    #[test]
    fn rejects_return_type_mismatch() {
        let src = r#"
fn f() -> Int:
    "not an int"
"#;
        assert!(check_str(src).is_err());
    }

    /// Capability safety as a type error: `print` needs a `Console`, and a
    /// `String` is not one. Only a `Console`-typed parameter (ultimately from
    /// `main`) can satisfy it.
    #[test]
    fn rejects_print_without_console_capability() {
        let src = r#"
fn leak(s: String) -> Nil:
    print(s, s)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("Console"), "expected a Console error, got: {e}");
    }

    #[test]
    fn accepts_print_with_console_capability() {
        let src = r#"
fn shout(console: Console, s: String) -> Nil:
    print(console, s)
"#;
        assert!(check_str(src).is_ok());
    }

    #[test]
    fn checks_adt_constructors_and_exhaustive_match() {
        let src = r#"
type Event:
    Click(Int, Int)
    Closed

fn describe(e: Event) -> String:
    match e:
        Click(x, _) -> int_to_string(x)
        Closed -> "closed"
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_non_exhaustive_match() {
        let src = r#"
type Event:
    Click(Int, Int)
    Closed

fn describe(e: Event) -> String:
    match e:
        Closed -> "closed"
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("non-exhaustive"), "got: {e}");
    }

    #[test]
    fn rejects_constructor_field_type_mismatch() {
        let src = r#"
type Event:
    Click(Int, Int)
    Closed

fn f() -> Event:
    Click("not an int", 2)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn accepts_the_actor_example() {
        let src = r#"
actor Logger:
    console: Console
    var count: Int = 0

impl Logger:
    on Log(msg: String):
        count = (count + 1)
        print(console, ((("[" <> int_to_string(count)) <> "] ") <> msg))

fn main(console: Console):
    let logger = spawn Logger(console)
    send(logger, Log("hello"))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_assignment_to_let() {
        let src = r#"
fn main():
    let x = 1
    x = 2
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("immutable"), "got: {e}");
    }

    #[test]
    fn accepts_assignment_to_var() {
        let src = r#"
fn main():
    var x = 1
    x = 2
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_inout_argument_that_is_immutable() {
        let src = r#"
fn bump(inout n: Int):
    n = (n + 1)

fn main():
    let x = 1
    bump(x)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("inout"), "got: {e}");
    }

    #[test]
    fn accepts_inout_argument_that_is_var() {
        let src = r#"
fn bump(inout n: Int):
    n = (n + 1)

fn main():
    var x = 1
    bump(x)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_use_after_sink_move() {
        let src = r#"
fn take(sink s: String) -> String:
    s

fn main():
    let x = "hi"
    let a = take(x)
    let b = take(x)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("moved"), "got: {e}");
    }

    #[test]
    fn accepts_reassignment_after_sink_move() {
        let src = r#"
fn take(sink s: String) -> String:
    s

fn main():
    var x = "hi"
    take(x)
    x = "again"
    take(x)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }
}
