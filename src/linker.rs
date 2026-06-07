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
    "int_to_duration",
    "duration_to_int",
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
    "exists",
    "subdir",
    "connect",
    "restrict",
    "send_line",
    "recv_line",
    "send_bytes",
    "recv_all",
    "recv_bytes",
    "listen",
    "accept",
    "close",
];

type FnTable = HashMap<String, HashSet<String>>;

/// The source of a bundled standard-library module, if `name` is one. This is
/// the canonical std registry: the linker treats it as a built-in search path,
/// and the CLI/test harness resolve `import` against it too.
/// Names of all bundled standard-library modules.
pub const STD_MODULES: &[&str] = &[
    "list", "string", "math", "result", "option", "func", "ord", "eq", "ascii", "set", "server",
    "show", "http", "json", "url", "duration", "random", "regex", "crypto", "compiler", "toml",
];

/// The bundled std modules that export a `pub fn` of the given name — used to
/// suggest a missing `import` when a call names an unimported stdlib function.
pub fn std_modules_for_function(fn_name: &str) -> Vec<&'static str> {
    let needle = format!("pub fn {fn_name}(");
    STD_MODULES
        .iter()
        .copied()
        .filter(|m| std_source(m).is_some_and(|s| s.contains(&needle)))
        .collect()
}

/// Every `pub fn` exported by a bundled std module, as `(function, module)`.
fn std_pub_fns() -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for m in STD_MODULES {
        if let Some(src) = std_source(m) {
            for line in src.lines() {
                if let Some(rest) = line.trim_start().strip_prefix("pub fn ") {
                    if let Some(paren) = rest.find('(') {
                        out.push((rest[..paren].trim().to_string(), *m));
                    }
                }
            }
        }
    }
    out
}

/// The closest std-library function name to `name` within a small edit distance —
/// used to suggest a likely-misspelled stdlib call. Returns `(function, module)`.
pub fn closest_std_function(name: &str) -> Option<(String, &'static str)> {
    if name.len() < 3 {
        return None; // too short for a meaningful suggestion
    }
    let mut best: Option<(usize, String, &'static str)> = None;
    for (cand, m) in std_pub_fns() {
        if cand == name {
            continue;
        }
        let d = levenshtein(name, &cand);
        // Require the edit to be small relative to the name, so short names don't
        // match everything.
        if d <= 2 && d < name.len() && best.as_ref().is_none_or(|(bd, _, _)| d < *bd) {
            best = Some((d, cand, m));
        }
    }
    best.map(|(_, c, m)| (c, m))
}

/// The closest bundled std-module name to `name` within a small edit distance —
/// used to suggest a correction for a misspelled `import`.
pub fn closest_std_module(name: &str) -> Option<&'static str> {
    if name.len() < 3 {
        return None; // too short for a meaningful suggestion
    }
    let mut best: Option<(usize, &'static str)> = None;
    for m in STD_MODULES {
        let d = levenshtein(name, m);
        if d <= 2 && d < name.len() && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, m));
        }
    }
    best.map(|(_, m)| m)
}

/// Levenshtein edit distance (two-row dynamic programming).
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

pub fn std_source(name: &str) -> Option<&'static str> {
    match name {
        "list" => Some(include_str!("../std/list.witchy")),
        "string" => Some(include_str!("../std/string.witchy")),
        "math" => Some(include_str!("../std/math.witchy")),
        "result" => Some(include_str!("../std/result.witchy")),
        "option" => Some(include_str!("../std/option.witchy")),
        "func" => Some(include_str!("../std/func.witchy")),
        "ord" => Some(include_str!("../std/ord.witchy")),
        "eq" => Some(include_str!("../std/eq.witchy")),
        "ascii" => Some(include_str!("../std/ascii.witchy")),
        "set" => Some(include_str!("../std/set.witchy")),
        "server" => Some(include_str!("../std/server.witchy")),
        "show" => Some(include_str!("../std/show.witchy")),
        "http" => Some(include_str!("../std/http.witchy")),
        "json" => Some(include_str!("../std/json.witchy")),
        "url" => Some(include_str!("../std/url.witchy")),
        "duration" => Some(include_str!("../std/duration.witchy")),
        "random" => Some(include_str!("../std/random.witchy")),
        "regex" => Some(include_str!("../std/regex.witchy")),
        "crypto" => Some(include_str!("../std/crypto.witchy")),
        "compiler" => Some(include_str!("../std/compiler.witchy")),
        "toml" => Some(include_str!("../std/toml.witchy")),
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

    // Reject cyclic constant/alias definitions with a clear message before
    // resolution turns them into dangling self-references.
    for (name, m) in &modules {
        if let Some(c) = crate::consts::find_cycle(m) {
            return lerr(format!("module `{name}`: constant `{c}` is defined cyclically"));
        }
        if let Some(c) = crate::aliases::find_cycle(m) {
            return lerr(format!("module `{name}`: type alias `{c}` is defined cyclically"));
        }
    }

    // Expand type aliases and inline top-level constants per module before
    // merging, so their use sites (and any function calls inside constant values)
    // are qualified along with the bodies they expand into — no `Item::TypeAlias`
    // or `Item::Const` reaches later stages.
    modules = modules
        .into_iter()
        .map(|(n, m)| (n, crate::aliases::resolve(crate::consts::inline(m))))
        .collect();

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
                // Constants and aliases were resolved per-module above, so none
                // remain here.
                Item::Const { .. } | Item::TypeAlias { .. } => {}
                // Traits/impls are carried into the merged module and desugared
                // after linking (see `crate::traits`). Their method bodies are
                // rewritten here, in their defining module's context, so calls
                // inside them resolve like any other function body.
                Item::Trait(t) => {
                    let mut t2 = t.clone();
                    for ms in &mut t2.methods {
                        if let Some(body) = &mut ms.default {
                            let mut bound = HashSet::new();
                            for p in &ms.params {
                                bound.insert(p.name.clone());
                            }
                            collect_bound_block(body, &mut bound);
                            rewrite_block(body, mname, &m.imports, &fns, &bound)?;
                        }
                    }
                    items.push(Item::Trait(t2));
                }
                Item::Impl(im) => {
                    let mut im2 = im.clone();
                    for method in &mut im2.methods {
                        let mut bound = HashSet::new();
                        for p in &method.params {
                            bound.insert(p.name.clone());
                        }
                        collect_bound_block(&method.body, &mut bound);
                        rewrite_block(&mut method.body, mname, &m.imports, &fns, &bound)?;
                    }
                    items.push(Item::Impl(im2));
                }
            }
        }
    }
    let mut module = Module {
        imports: Vec::new(),
        items,
        import_lines: Vec::new(),
        item_lines: Vec::new(),
    };
    resolve_methods(&mut module);
    Ok(module)
}

/// The (nominal) type name of a `Type`, if it has one.
fn type_name(t: &Type) -> Option<String> {
    match t {
        Type::Named(n, _) => Some(n.clone()),
        _ => None,
    }
}

/// Per-function nominal signature: first-parameter type name and return type
/// name, used to resolve overloaded UFCS method calls by the receiver's type.
struct FnSig {
    first_param: Option<String>,
    ret: Option<String>,
}

/// Resolve UFCS method calls the linker left unqualified because the bare name is
/// provided by several imported modules (e.g. `get` in http/server/json). For
/// each such `name(receiver, ...)`, pick the `mod.name` whose first parameter
/// type matches the receiver's nominal type. The receiver's type is read from
/// the function it calls (its return type) or a literal — so chains like
/// `router().get(...).layer(...)` resolve left to right. A receiver whose type
/// can't be determined (e.g. a plain variable) is left for the type checker.
fn resolve_methods(module: &mut Module) {
    let mut sig: HashMap<String, FnSig> = HashMap::new();
    // base method name -> the qualified function names providing it.
    let mut by_base: HashMap<String, Vec<String>> = HashMap::new();
    for item in &module.items {
        if let Item::Function(f) = item {
            let first_param = f.params.first().and_then(|p| p.ty.as_ref()).and_then(type_name);
            let ret = f.ret.as_ref().and_then(type_name);
            sig.insert(f.name.clone(), FnSig { first_param, ret });
            if let Some((_, base)) = f.name.rsplit_once('.') {
                by_base.entry(base.to_string()).or_default().push(f.name.clone());
            }
        }
    }
    for item in &mut module.items {
        match item {
            Item::Function(f) => {
                let mut vars = param_vars(&f.params);
                resolve_in_block(&mut f.body, &sig, &by_base, &mut vars);
            }
            Item::Actor(a) => {
                for field in &mut a.fields {
                    if let Some(init) = &mut field.init {
                        resolve_in_expr(init, &sig, &by_base, &mut HashMap::new());
                    }
                }
                for h in &mut a.handlers {
                    let mut vars = param_vars(&h.params);
                    resolve_in_block(&mut h.body, &sig, &by_base, &mut vars);
                }
            }
            Item::Trait(t) => {
                for ms in &mut t.methods {
                    if let Some(body) = &mut ms.default {
                        let mut vars = param_vars(&ms.params);
                        resolve_in_block(body, &sig, &by_base, &mut vars);
                    }
                }
            }
            Item::Impl(im) => {
                for method in &mut im.methods {
                    let mut vars = param_vars(&method.params);
                    resolve_in_block(&mut method.body, &sig, &by_base, &mut vars);
                }
            }
            Item::Type(_) | Item::Const { .. } | Item::TypeAlias { .. } => {}
        }
    }
}

/// Seed a variable-type scope from a function's parameters (nominal types only).
fn param_vars(params: &[Param]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for p in params {
        if let Some(n) = p.ty.as_ref().and_then(type_name) {
            m.insert(p.name.clone(), n);
        }
    }
    m
}

/// The nominal type an expression evaluates to, where the linker can tell: a
/// call's return type, a literal's type, or a variable whose type was tracked.
fn expr_nominal_type(
    e: &Expr,
    sig: &HashMap<String, FnSig>,
    vars: &HashMap<String, String>,
) -> Option<String> {
    match e {
        Expr::Call { name, .. } => sig.get(name).and_then(|s| s.ret.clone()),
        Expr::Var(n) => vars.get(n).cloned(),
        Expr::Int(_) => Some("Int".to_string()),
        Expr::Float(_) => Some("Float".to_string()),
        Expr::Duration(_) => Some("Duration".to_string()),
        Expr::Str(_) => Some("String".to_string()),
        Expr::Bool(_) => Some("Bool".to_string()),
        _ => None,
    }
}

fn resolve_in_block(
    b: &mut Block,
    sig: &HashMap<String, FnSig>,
    by_base: &HashMap<String, Vec<String>>,
    vars: &mut HashMap<String, String>,
) {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                resolve_in_expr(value, sig, by_base, vars);
                // Track the binding's nominal type so later `name.method(...)`
                // calls (a step-by-step builder) resolve by it.
                if let Some(t) = expr_nominal_type(value, sig, vars) {
                    vars.insert(name.clone(), t);
                }
            }
            Stmt::Assign { name, value } => {
                resolve_in_expr(value, sig, by_base, vars);
                match expr_nominal_type(value, sig, vars) {
                    Some(t) => {
                        vars.insert(name.clone(), t);
                    }
                    None => {
                        vars.remove(name);
                    }
                }
            }
            Stmt::LetTuple { value, .. } => resolve_in_expr(value, sig, by_base, vars),
            Stmt::Return(Some(e)) | Stmt::Expr(e) => resolve_in_expr(e, sig, by_base, vars),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn resolve_in_expr(
    e: &mut Expr,
    sig: &HashMap<String, FnSig>,
    by_base: &HashMap<String, Vec<String>>,
    vars: &mut HashMap<String, String>,
) {
    match e {
        Expr::Call { name, args } => {
            // Resolve nested receivers/arguments first, so a chained receiver's
            // call is already resolved when we read its return type.
            for a in args.iter_mut() {
                resolve_in_expr(a, sig, by_base, vars);
            }
            if !name.contains('.') && !sig.contains_key(name.as_str()) {
                if let Some(cands) = by_base.get(name.as_str()) {
                    if cands.len() > 1 {
                        if let Some(recv) = args.first().and_then(|a| expr_nominal_type(a, sig, vars))
                        {
                            let matches: Vec<&String> = cands
                                .iter()
                                .filter(|c| {
                                    sig.get(*c).and_then(|s| s.first_param.as_deref())
                                        == Some(recv.as_str())
                                })
                                .collect();
                            if let [only] = matches.as_slice() {
                                *name = (*only).clone();
                            }
                        }
                    }
                }
            }
        }
        Expr::Apply { func, args } => {
            resolve_in_expr(func, sig, by_base, vars);
            for a in args.iter_mut() {
                resolve_in_expr(a, sig, by_base, vars);
            }
        }
        Expr::Ctor { args, .. } | Expr::List(args) | Expr::Tuple(args) | Expr::Spawn { args, .. } => {
            for a in args.iter_mut() {
                resolve_in_expr(a, sig, by_base, vars);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            resolve_in_expr(expr, sig, by_base, vars)
        }
        Expr::Binary { lhs, rhs, .. } => {
            resolve_in_expr(lhs, sig, by_base, vars);
            resolve_in_expr(rhs, sig, by_base, vars);
        }
        Expr::RecordUpdate { base, fields } => {
            resolve_in_expr(base, sig, by_base, vars);
            for (_, v) in fields.iter_mut() {
                resolve_in_expr(v, sig, by_base, vars);
            }
        }
        Expr::If { cond, then_block, else_block } => {
            resolve_in_expr(cond, sig, by_base, vars);
            resolve_in_block(then_block, sig, by_base, vars);
            if let Some(b) = else_block {
                resolve_in_block(b, sig, by_base, vars);
            }
        }
        Expr::Block(b) => resolve_in_block(b, sig, by_base, vars),
        Expr::While { cond, body } => {
            resolve_in_expr(cond, sig, by_base, vars);
            resolve_in_block(body, sig, by_base, vars);
        }
        Expr::For { iter, body, .. } => {
            resolve_in_expr(iter, sig, by_base, vars);
            resolve_in_block(body, sig, by_base, vars);
        }
        Expr::Match { scrutinee, arms } => {
            resolve_in_expr(scrutinee, sig, by_base, vars);
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    resolve_in_expr(g, sig, by_base, vars);
                }
                resolve_in_expr(&mut arm.body, sig, by_base, vars);
            }
        }
        Expr::Lambda { body, .. } => resolve_in_block(body, sig, by_base, vars),
        Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_) => {}
    }
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
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            collect_bound_expr(expr, out)
        }
        Expr::RecordUpdate { base, fields } => {
            collect_bound_expr(base, out);
            for (_, v) in fields {
                collect_bound_expr(v, out);
            }
        }
        Expr::Var(_) | Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
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
            *name = resolve_call(name, m, imps, fns, bound)?;
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
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
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
        Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
    }
    Ok(())
}

fn resolve_call(
    name: &str,
    m: &str,
    imps: &[String],
    fns: &FnTable,
    bound: &HashSet<String>,
) -> Result<String, LinkError> {
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
    // A bare name (e.g. from `recv.method(...)` UFCS sugar) may name a function
    // in an imported module. Resolve it when exactly one import provides it and
    // it isn't shadowed by a local binding; ambiguity across imports is an error.
    if !bound.contains(name) {
        let mut providers: Vec<&str> = Vec::new();
        for imp in imps {
            if fns.get(imp).is_some_and(|s| s.contains(name)) {
                providers.push(imp.as_str());
            }
        }
        // Exactly one import provides it -> resolve. If several do, it's an
        // overloaded method name (e.g. http.get / server.get / json.get): leave
        // it unqualified for the post-link `resolve_methods` pass to pick by the
        // receiver's type.
        if providers.len() == 1 {
            return Ok(format!("{}.{name}", providers[0]));
        }
    }
    // Not a function here and not a builtin: a local binding being applied (e.g.
    // a lambda parameter). Leave it unqualified; the type checker decides.
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_near_miss_std_module_and_function() {
        assert_eq!(closest_std_module("lst"), Some("list"));
        assert_eq!(closest_std_module("str/ng"), Some("string"));
        assert_eq!(closest_std_module("zzz"), None); // not close to anything
        assert_eq!(closest_std_module("qq"), None); // too short to over-match

        // `map` lives in list (and option); a near miss resolves to a real name.
        assert!(closest_std_function("mep").is_some());
        assert_eq!(closest_std_function("zzzzzz"), None);
    }
}
