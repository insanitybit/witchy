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
    self, Block, CapRestrict, Convention, Expr, Function, Item, MatchArm, Module, Pattern,
    RestrictMode, Stmt, UnOp,
};

/// The operations a `Dir` capability permits. Decomposing the capability by
/// right makes the footprint distinguish read-only from writing code, and an op
/// that needs a right it wasn't granted is a compile-time error. Bare `Dir` is
/// the full set; `Dir[Read]`/`Dir[Write]` narrow it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirRights {
    pub read: bool,
    pub write: bool,
}

impl DirRights {
    pub fn full() -> Self {
        DirRights { read: true, write: true }
    }
}

impl fmt::Display for DirRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.read, self.write) {
            (true, true) => write!(f, "Dir"),
            (true, false) => write!(f, "Dir[Read]"),
            (false, true) => write!(f, "Dir[Write]"),
            (false, false) => write!(f, "Dir[]"),
        }
    }
}

/// The actor kind a `Subject(Name)` targets, or `None` for a bare, untyped
/// `Subject`. Only the first type argument's name is read.
fn subject_target(args: &[ast::Type]) -> Option<String> {
    match args.first() {
        Some(ast::Type::Named(n, _)) => Some(n.clone()),
        _ => None,
    }
}

/// Interpret a `Dir`'s type arguments as its rights. Bare `Dir` (no args) is the
/// full set; `Dir[Read]`/`Dir[Write]`/`Dir[Read, Write]` narrow it.
fn dir_rights(args: &[ast::Type]) -> DirRights {
    if args.is_empty() {
        return DirRights::full();
    }
    let mut r = DirRights { read: false, write: false };
    for a in args {
        if let ast::Type::Named(n, _) = a {
            match n.as_str() {
                "Read" => r.read = true,
                "Write" => r.write = true,
                _ => {}
            }
        }
    }
    r
}

/// The rights a `Net` capability permits, on two independent axes. **Verbs**:
/// `Connect` lets code dial out (`connect`, `restrict`); `Listen` lets it accept
/// inbound (`listen`) — distinguishing a client from a server. **Transports**:
/// `Tcp`/`Udp`/`Uds` — though only TCP is implemented at runtime, so `connect`/
/// `listen` require `Tcp`; `Udp`/`Uds` are type-level markers that keep the
/// taxonomy expressible (and auditable) even though the transport isn't.
///
/// Each axis defaults independently: an unmentioned axis is *full*. Bare `Net` is
/// full verbs + full transports; `Net[Connect]` is connect-only over all
/// transports; `Net[Tcp]` is all verbs over TCP only; `Net[Connect, Tcp]` is both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetRights {
    pub connect: bool,
    pub listen: bool,
    pub tcp: bool,
    pub udp: bool,
    pub uds: bool,
}

impl NetRights {
    pub fn full() -> Self {
        NetRights { connect: true, listen: true, tcp: true, udp: true, uds: true }
    }

    fn verbs_full(&self) -> bool {
        self.connect && self.listen
    }

    fn transports_full(&self) -> bool {
        self.tcp && self.udp && self.uds
    }
}

impl fmt::Display for NetRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.verbs_full() && self.transports_full() {
            return write!(f, "Net");
        }
        // List only the narrowed axes; an axis at its full set is omitted (so
        // `Net[Connect]` reads as "connect-only, any transport").
        let mut parts: Vec<&str> = Vec::new();
        if !self.verbs_full() {
            if self.connect {
                parts.push("Connect");
            }
            if self.listen {
                parts.push("Listen");
            }
        }
        if !self.transports_full() {
            if self.tcp {
                parts.push("Tcp");
            }
            if self.udp {
                parts.push("Udp");
            }
            if self.uds {
                parts.push("Uds");
            }
        }
        write!(f, "Net[{}]", parts.join(", "))
    }
}

/// Interpret a `Net`'s type arguments as its rights. Bare `Net` (no args) is the
/// full set. Each axis defaults to full independently: `Net[Connect]` keeps all
/// transports, `Net[Tcp]` keeps all verbs. Unrecognized markers are ignored.
fn net_rights(args: &[ast::Type]) -> NetRights {
    if args.is_empty() {
        return NetRights::full();
    }
    let mut r = NetRights { connect: false, listen: false, tcp: false, udp: false, uds: false };
    let (mut saw_verb, mut saw_transport) = (false, false);
    for a in args {
        if let ast::Type::Named(n, _) = a {
            match n.as_str() {
                "Connect" => (r.connect, saw_verb) = (true, true),
                "Listen" => (r.listen, saw_verb) = (true, true),
                "Tcp" => (r.tcp, saw_transport) = (true, true),
                "Udp" => (r.udp, saw_transport) = (true, true),
                "Uds" => (r.uds, saw_transport) = (true, true),
                _ => {}
            }
        }
    }
    if !saw_verb {
        (r.connect, r.listen) = (true, true);
    }
    if !saw_transport {
        (r.tcp, r.udp, r.uds) = (true, true, true);
    }
    r
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Float,
    /// A length of time (from a duration literal like `30s`). A distinct type:
    /// it does not mix with `Int` under arithmetic.
    Duration,
    String,
    Bool,
    Nil,
    Console,
    Clock,
    Env,
    Secret,
    /// A handle to an actor's mailbox. The optional name is the actor kind it
    /// targets (`Subject(Counter)`); `None` is an untyped subject (bare
    /// `Subject`), which accepts any message a handler declares. `spawn` yields
    /// a typed subject, so `send`/`ask` to the wrong message is a compile error.
    Subject(Option<String>),
    Dir(DirRights),
    Net(NetRights),
    Socket,
    Listener,
    /// Build-time capabilities — a parallel set to the runtime caps, granted only
    /// to a rune's `build` entrypoint and enforced in a zero-ambient build
    /// sandbox. Kind-only (the specific tool/host/dir/var is the consumer's grant,
    /// not the type); see docs/build-time-execution-plan.md.
    BuildOut,
    BuildRead,
    BuildEnv,
    BuildNet,
    BuildExec,
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
            Ty::Duration => write!(f, "Duration"),
            Ty::String => write!(f, "String"),
            Ty::Bool => write!(f, "Bool"),
            Ty::Nil => write!(f, "Nil"),
            Ty::Console => write!(f, "Console"),
            Ty::Clock => write!(f, "Clock"),
            Ty::Env => write!(f, "Env"),
            Ty::Secret => write!(f, "Secret"),
            Ty::Subject(None) => write!(f, "Subject"),
            Ty::Subject(Some(a)) => write!(f, "Subject({a})"),
            Ty::Dir(r) => write!(f, "{r}"),
            Ty::Net(r) => write!(f, "{r}"),
            Ty::Socket => write!(f, "Socket"),
            Ty::Listener => write!(f, "Listener"),
            Ty::BuildOut => write!(f, "BuildOut"),
            Ty::BuildRead => write!(f, "BuildRead"),
            Ty::BuildEnv => write!(f, "BuildEnv"),
            Ty::BuildNet => write!(f, "BuildNet"),
            Ty::BuildExec => write!(f, "BuildExec"),
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

/// Reject two top-level functions with the same name. Witchy has no
/// free-function overloading — a second definition silently overwrites the first
/// (in both the linker's and the checker's name tables), so the duplicate is
/// always a bug (a typo or a copy/paste). Methods live in `impl` blocks and are
/// dispatched by receiver type, so they are not affected. Names may be
/// module-qualified (`main.f`) after linking; the message shows the bare name.
fn check_unique_functions(module: &Module) -> Result<(), TypeError> {
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for (idx, item) in module.items.iter().enumerate() {
        if let Item::Function(f) = item {
            let line = module.item_lines.get(idx).copied().unwrap_or(0);
            if let Some(&first) = seen.get(f.name.as_str()) {
                let bare = f.name.rsplit('.').next().unwrap_or(&f.name);
                let where_ = if first != 0 && line != 0 {
                    format!(" (lines {first} and {line})")
                } else {
                    String::new()
                };
                return terr(format!(
                    "function `{bare}` is defined more than once{where_}; \
                     top-level function names must be unique"
                ));
            }
            seen.insert(f.name.as_str(), line);
        }
    }
    Ok(())
}

/// The type names the checker knows without a declaration: primitives, host
/// capabilities, and the built-in generics. Mirrors the named arms of
/// `to_ty_generic` plus the opaque generics the checker itself produces
/// (`Option`/`Result`/`Dict`). Any other named type must be declared (a `type`
/// or an actor) or be a lowercase generic parameter.
const BUILTIN_TYPE_NAMES: &[&str] = &[
    "Int", "Float", "Duration", "String", "Bool", "Nil", "Console", "Clock", "Env", "Secret",
    "Subject", "Dir", "Net", "Socket", "Listener", "List", "Option", "Result", "Dict",
    "BuildOut", "BuildRead", "BuildEnv", "BuildNet", "BuildExec",
];

/// Validate that every named type in `t` is known — a builtin, a declared type,
/// or a lowercase generic parameter — so a typo like `fn f(x: Flarb)` is a clear
/// "unknown type" error rather than an opaque type that mis-unifies later.
fn validate_type(t: &ast::Type, known: &HashSet<&str>) -> Result<(), TypeError> {
    match t {
        ast::Type::Tuple(ts) => ts.iter().try_for_each(|x| validate_type(x, known)),
        ast::Type::Fn(params, ret) => {
            params.iter().try_for_each(|p| validate_type(p, known))?;
            validate_type(ret, known)
        }
        ast::Type::Named(n, args) => {
            // `Dir`/`Net` carry capability *rights* (`Dir[Read]`, `Net[Connect]`)
            // in their arguments, not types — those are checked elsewhere.
            if n == "Dir" || n == "Net" {
                return Ok(());
            }
            if known.contains(n.as_str()) {
                args.iter().try_for_each(|a| validate_type(a, known))
            } else if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) {
                // A lowercase, argument-less name is a generic type parameter.
                Ok(())
            } else {
                terr(format!("unknown type `{n}`"))
            }
        }
    }
}

/// Reject references to undeclared types in function and actor signatures. The
/// set of known names is the builtins plus every `type`/actor declared in the
/// module; lowercase argument-less names are generic parameters.
fn check_type_names(module: &Module) -> Result<(), TypeError> {
    let mut known: HashSet<&str> = BUILTIN_TYPE_NAMES.iter().copied().collect();
    for item in &module.items {
        match item {
            Item::Type(t) => {
                known.insert(t.name.as_str());
            }
            _ => {}
        }
    }
    for item in &module.items {
        let in_ctx = |e: TypeError, ctx: &str| TypeError {
            message: format!("in `{}`: {}", ctx.rsplit('.').next().unwrap_or(ctx), e.message),
        };
        match item {
            Item::Function(f) => {
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        validate_type(t, &known).map_err(|e| in_ctx(e, &f.name))?;
                    }
                }
                if let Some(t) = &f.ret {
                    validate_type(t, &known).map_err(|e| in_ctx(e, &f.name))?;
                }
            }
            // A type's variant field types must also be known. The type's own
            // name (and any sibling type) is already in `known`, so recursive and
            // mutually-recursive types check out; lowercase fields are its params.
            Item::Type(t) => {
                for variant in &t.variants {
                    for field in &variant.fields {
                        validate_type(field, &known).map_err(|e| in_ctx(e, &t.name))?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether `t` names a host capability that `main` may receive as a root
/// authority (the rights of `Dir`/`Net` don't matter here — any are grantable).
pub(crate) fn is_capability_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, _)
        if matches!(n.as_str(), "Console" | "Clock" | "Env" | "Dir" | "Net" | "Secret"))
}

/// Whether `t` is a *build-time* capability — the parallel set granted only to a
/// rune's `build` entrypoint, never to `main`. Kept distinct from the runtime
/// capabilities on purpose: the two axes are granted and gated separately.
pub(crate) fn is_build_capability_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, _)
        if matches!(n.as_str(), "BuildOut" | "BuildRead" | "BuildEnv" | "BuildNet" | "BuildExec"))
}

/// Whether `t` is `List(String)` — the command-line-arguments parameter `main`
/// may declare.
pub(crate) fn is_args_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, args)
        if n == "List"
            && matches!(args.as_slice(), [ast::Type::Named(s, inner)] if s == "String" && inner.is_empty()))
}

/// Validate `main`'s signature: every parameter must be a host capability or the
/// `List(String)` args parameter, since `main` is the program's root actor and
/// only the host's granted authority can enter there. Catches at check time what
/// would otherwise be a runtime error when the root capabilities are minted. A
/// module without `main` is a library and passes.
fn check_main_signature(module: &Module) -> Result<(), TypeError> {
    let Some(main) = module.items.iter().find_map(|it| match it {
        Item::Function(f) if f.name == "main" => Some(f),
        _ => None,
    }) else {
        return Ok(());
    };
    for p in &main.params {
        if matches!(&p.ty, Some(t) if is_capability_type(t) || is_args_type(t)) {
            continue;
        }
        let found = match &p.ty {
            Some(t) => format!("has type `{}`", crate::format::type_str(t)),
            None => "has no type annotation".to_string(),
        };
        return terr(format!(
            "`main` parameter `{}` {found}, but `main` may only take host capabilities \
             (Console, Clock, Env, Dir, Net, Secret) or `List(String)` for command-line args",
            p.name
        ));
    }
    Ok(())
}

/// Validate a rune's build entrypoint. The build step is the top-level `fn build`
/// whose first parameter is `BuildOut`; it runs in a zero-ambient build sandbox,
/// so it may take *only* build-time capabilities — a runtime capability (or
/// anything else) in its signature is an error. A `build` function that does not
/// take `BuildOut` is treated as an ordinary function, not the entrypoint, so
/// existing code that happens to define `fn build(...)` is unaffected.
fn check_build_signature(module: &Module) -> Result<(), TypeError> {
    let Some(build) = build_entrypoint(module) else {
        return Ok(());
    };
    for p in &build.params {
        if matches!(&p.ty, Some(t) if is_build_capability_type(t)) {
            continue;
        }
        let found = match &p.ty {
            Some(t) => format!("has type `{}`", crate::format::type_str(t)),
            None => "has no type annotation".to_string(),
        };
        return terr(format!(
            "`build` parameter `{}` {found}, but a build step may only take build-time \
             capabilities (BuildOut, BuildRead, BuildEnv, BuildNet, BuildExec)",
            p.name
        ));
    }
    Ok(())
}

/// Whether `src` parses to a module shipping a build entrypoint — how the
/// package manager decides a dependency's `build` module is a build *step* (run
/// separately, confined) rather than library API (linked into the consumer).
/// (The PM is bin-only, so the lib target sees no caller.)
#[allow(dead_code)]
pub(crate) fn build_entrypoint_src(src: &str) -> bool {
    crate::parser::parse_module(src)
        .map(|m| build_entrypoint(&m).is_some())
        .unwrap_or(false)
}

/// The rune's build entrypoint, if any: a top-level `fn build` whose first
/// parameter is a `BuildOut`. Returns `None` for a `build` function that isn't
/// shaped like an entrypoint (so it's just an ordinary function).
pub(crate) fn build_entrypoint(module: &Module) -> Option<&Function> {
    module.items.iter().find_map(|it| match it {
        // The linker qualifies non-`main` functions as `mod.name`, so match on the
        // unqualified tail.
        Item::Function(f)
            if f.name.rsplit('.').next() == Some("build")
                && matches!(f.params.first(), Some(p) if matches!(&p.ty, Some(t) if is_build_capability_type(t))) =>
        {
            Some(f)
        }
        _ => None,
    })
}

/// Collect the type-parameter names (lowercase, argument-less) appearing in a
/// type expression, in order of first appearance. Used to infer the parameters
/// of a generic ADT from its variant field types.
/// A `let`-borrowed parameter may not BE the function's result: every block
/// tail and `return` expression is checked for the bare parameter (through
/// if/match/block tails). Everything else copies by value semantics, so this
/// is the whole escape surface — previously enforced only by the native
/// backend's borrow checker, now a language rule.
fn borrow_escape_check(func: &Function) -> Result<(), TypeError> {
    let borrowed: Vec<&str> = func
        .params
        .iter()
        .filter(|p| p.convention == Convention::Borrow)
        .map(|p| p.name.as_str())
        .collect();
    if borrowed.is_empty() {
        return Ok(());
    }
    fn check_result_expr(e: &Expr, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        match e {
            Expr::Var(v) if borrowed.contains(&v.as_str()) => terr(format!(
                "in `{fname}`: the `let`-borrowed parameter `{v}` cannot be returned — a borrow must not outlive the call (drop the `let`, or take it `own`)"
            )),
            Expr::If { then_block, else_block, .. } => {
                check_result_block(then_block, borrowed, fname)?;
                if let Some(b) = else_block {
                    check_result_block(b, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Match { arms, .. } => {
                for arm in arms {
                    check_result_expr(&arm.body, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Block(b) => check_result_block(b, borrowed, fname),
            _ => Ok(()),
        }
    }
    fn check_result_block(b: &Block, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        if let Some(Stmt::Expr(tail)) = b.stmts.last() {
            check_result_expr(tail, borrowed, fname)?;
        }
        Ok(())
    }
    // Every `return` expression, anywhere in the body.
    fn scan_returns_block(b: &Block, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        for stmt in &b.stmts {
            match stmt {
                Stmt::Return(Some(e)) => check_result_expr(e, borrowed, fname)?,
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetTuple { value, .. }
                | Stmt::Expr(value)
                | Stmt::Yield(value) => scan_returns_expr(value, borrowed, fname)?,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }
    fn scan_returns_expr(e: &Expr, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        match e {
            Expr::If { cond, then_block, else_block } => {
                scan_returns_expr(cond, borrowed, fname)?;
                scan_returns_block(then_block, borrowed, fname)?;
                if let Some(b) = else_block {
                    scan_returns_block(b, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Match { scrutinee, arms } => {
                scan_returns_expr(scrutinee, borrowed, fname)?;
                for arm in arms {
                    scan_returns_expr(&arm.body, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::While { cond, body } => {
                scan_returns_expr(cond, borrowed, fname)?;
                scan_returns_block(body, borrowed, fname)
            }
            Expr::For { iter, body, .. } => {
                scan_returns_expr(iter, borrowed, fname)?;
                scan_returns_block(body, borrowed, fname)
            }
            Expr::Block(b) => scan_returns_block(b, borrowed, fname),
            Expr::Binary { lhs, rhs, .. } => {
                scan_returns_expr(lhs, borrowed, fname)?;
                scan_returns_expr(rhs, borrowed, fname)
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
                scan_returns_expr(expr, borrowed, fname)
            }
            Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args {
                    scan_returns_expr(a, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Apply { func: f2, args } => {
                scan_returns_expr(f2, borrowed, fname)?;
                for a in args {
                    scan_returns_expr(a, borrowed, fname)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    check_result_block(&func.body, &borrowed, &func.name)?;
    scan_returns_block(&func.body, &borrowed, &func.name)
}

/// Types a region assignment may freely write through to the outer scope:
/// copied by value, never pointer-backed.
fn is_scalar_ty(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float | Ty::Bool | Ty::Duration)
}

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
    /// When annotating (see `annotate`): expression identity -> inferred type,
    /// finalized against the ending substitution. Key = `&Expr as *const _`.
    type_record: Option<HashMap<usize, Ty>>,
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
    /// Message name -> the declared handler parameter-type lists, across every
    /// actor (`on Log(line: String)` registers `Log -> [[Some(String)]]`).
    /// `send(subject, Msg(...))` is validated against these.
    actor_handler_sigs: HashMap<String, Vec<Vec<Option<ast::Type>>>>,
    /// Actor kind -> the message names it handles. Used to check a `send`/`ask`
    /// to a *typed* `Subject(Actor)` targets a message that actor declares.
    actor_messages: HashMap<String, std::collections::HashSet<String>>,
    fn_conventions: HashMap<String, Vec<Convention>>,
    /// Per-function type parameters (name, var id), from lowercase type names in
    /// signatures. Generalized: instantiated fresh at each call site.
    fn_typarams: HashMap<String, Vec<(String, u32)>>,
    subst: HashMap<u32, Ty>,
    next_var: u32,
    /// Each binding carries its type and whether it is mutable.
    scopes: Vec<HashMap<String, (Ty, bool)>>,
    /// Parallel to `scopes`: names hidden in each frame by a `retain`/`without`
    /// capability firewall. A lookup that reaches a hidden name in a frame stops
    /// as if the name were unbound, even if an outer frame still defines it — so a
    /// block can be sealed against capabilities its callers might hold. An inner
    /// re-binding (a fresh `let`) shadows the tombstone normally.
    hidden: Vec<HashSet<String>>,
    /// Bindings that have been consumed (moved out via a `sink` parameter) and
    /// may not be used again until reassigned. Flow-sensitive within a body.
    consumed: HashSet<String>,
    /// One entry per ACTIVE `region:` block, holding the names declared
    /// inside it — an assignment to a name outside the innermost region must
    /// be scalar (a region's only pointer-escape is its value).
    region_locals: Vec<HashSet<String>>,
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

    // `&mut self` because resolving an unknown type can mint fresh type vars via
    // `self.fresh()`, despite the `to_` name.
    #[allow(clippy::wrong_self_convention)]
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
            "Duration" => Ty::Duration,
            "String" => Ty::String,
            "Bool" => Ty::Bool,
            "Nil" => Ty::Nil,
            "Console" => Ty::Console,
            "Clock" => Ty::Clock,
            "Env" => Ty::Env,
            "Secret" => Ty::Secret,
            "Subject" => Ty::Subject(subject_target(args)),
            "Dir" => Ty::Dir(dir_rights(args)),
            "Net" => Ty::Net(net_rights(args)),
            "Socket" => Ty::Socket,
            "Listener" => Ty::Listener,
            "BuildOut" => Ty::BuildOut,
            "BuildRead" => Ty::BuildRead,
            "BuildEnv" => Ty::BuildEnv,
            "BuildNet" => Ty::BuildNet,
            "BuildExec" => Ty::BuildExec,
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
    #[allow(clippy::wrong_self_convention)]
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
                "Duration" => Ty::Duration,
                "String" => Ty::String,
                "Bool" => Ty::Bool,
                "Nil" => Ty::Nil,
                "Console" => Ty::Console,
            "Clock" => Ty::Clock,
            "Env" => Ty::Env,
            "Secret" => Ty::Secret,
                "Subject" => Ty::Subject(subject_target(args)),
                "Dir" => Ty::Dir(dir_rights(args)),
                "Net" => Ty::Net(net_rights(args)),
                "Socket" => Ty::Socket,
                "Listener" => Ty::Listener,
                "BuildOut" => Ty::BuildOut,
                "BuildRead" => Ty::BuildRead,
                "BuildEnv" => Ty::BuildEnv,
                "BuildNet" => Ty::BuildNet,
                "BuildExec" => Ty::BuildExec,
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
                // Occurs check: binding x := T where x appears inside T would
                // build an infinite type (e.g. unifying `a` with `List(a)`).
                if self.occurs(*x, other) {
                    return terr(format!(
                        "cannot construct the infinite type: a type variable occurs within `{other}`"
                    ));
                }
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
            // A bare `Subject` (untyped) accepts any actor's subject; two typed
            // subjects must name the same actor.
            (Ty::Subject(x), Ty::Subject(y)) => match (x, y) {
                (None, _) | (_, None) => Ok(()),
                (Some(p), Some(q)) if p == q => Ok(()),
                _ => terr(format!("expected `{a}`, found `{b}`")),
            },
            _ if a == b => Ok(()),
            _ => terr(format!("expected `{a}`, found `{b}`")),
        }
    }

    /// Whether type variable `x` occurs anywhere inside `t` (after resolving
    /// intermediate variables) — the standard infinite-type guard.
    fn occurs(&self, x: u32, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Var(y) => x == y,
            Ty::List(inner) => self.occurs(x, &inner),
            Ty::Tuple(items) => items.iter().any(|i| self.occurs(x, i)),
            Ty::Named(_, args) => args.iter().any(|a| self.occurs(x, a)),
            Ty::Fn(params, ret) => {
                params.iter().any(|p| self.occurs(x, p)) || self.occurs(x, &ret)
            }
            _ => false,
        }
    }

    // --- scope helpers ---
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
        self.hidden.push(HashSet::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
        self.hidden.pop();
    }
    fn define(&mut self, name: String, ty: Ty, mutable: bool) {
        // A fresh binding un-hides the name in this frame: re-declaring a name
        // that an outer firewall dropped is a legitimate shadow, not a leak.
        if let Some(h) = self.hidden.last_mut() {
            h.remove(&name);
        }
        if let Some(r) = self.region_locals.last_mut() {
            r.insert(name.clone());
        }
        self.scopes.last_mut().unwrap().insert(name, (ty, mutable));
    }
    /// Walk frames inner→outer, returning the binding at the first frame that
    /// either defines or *hides* the name. Hiding wins (returns `None`) even when
    /// an outer frame still defines the name — that is the firewall.
    fn resolve_binding(&self, name: &str) -> Option<&(Ty, bool)> {
        for (vars, hidden) in self.scopes.iter().rev().zip(self.hidden.iter().rev()) {
            if let Some(b) = vars.get(name) {
                return Some(b);
            }
            if hidden.contains(name) {
                return None;
            }
        }
        None
    }
    fn lookup(&self, name: &str) -> Option<Ty> {
        self.resolve_binding(name).map(|(t, _)| t.clone())
    }
    fn is_mutable(&self, name: &str) -> Option<bool> {
        self.resolve_binding(name).map(|(_, m)| *m)
    }
    /// True if `name` is currently hidden by a firewall frame that no closer
    /// frame re-binds — used only to give a precise diagnostic (the name is in
    /// scope, just walled off) instead of a bare "unbound variable".
    fn is_firewalled(&self, name: &str) -> bool {
        for (vars, hidden) in self.scopes.iter().rev().zip(self.hidden.iter().rev()) {
            if vars.contains_key(name) {
                return false;
            }
            if hidden.contains(name) {
                return true;
            }
        }
        false
    }

    /// Is `t` one of the capability types (the unforgeable authority values)?
    fn is_capability(&self, t: &Ty) -> bool {
        matches!(
            self.resolve(t),
            Ty::Console
                | Ty::Clock
                | Ty::Env
                | Ty::Secret
                | Ty::Subject(_)
                | Ty::Dir(_)
                | Ty::Net(_)
                | Ty::Socket
                | Ty::Listener
                | Ty::BuildOut
                | Ty::BuildRead
                | Ty::BuildEnv
                | Ty::BuildNet
                | Ty::BuildExec
        )
    }

    /// Names currently bound to a capability and still visible (not already
    /// firewalled by an enclosing block). Inner bindings shadow outer ones.
    fn visible_capabilities(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut caps = Vec::new();
        for scope in self.scopes.iter().rev() {
            for name in scope.keys() {
                if !seen.insert(name.clone()) {
                    continue;
                }
                if let Some(t) = self.lookup(name) {
                    if self.is_capability(&t) {
                        caps.push(name.clone());
                    }
                }
            }
        }
        caps
    }

    /// Apply a `retain`/`without` firewall to the current (just-pushed) block
    /// frame: record the dropped capability names in the frame's hidden-set so
    /// later lookups inside the block treat them as unbound. Every named
    /// capability must actually be a visible capability — naming a non-capability
    /// or an out-of-scope name is an error, since it almost certainly means the
    /// author misremembered what authority the block holds.
    fn apply_restrict(&mut self, r: &CapRestrict) -> Result<(), TypeError> {
        let visible = self.visible_capabilities();
        let visible_set: HashSet<&str> = visible.iter().map(String::as_str).collect();
        for name in &r.names {
            if !visible_set.contains(name.as_str()) {
                let (kw, verb) = match r.mode {
                    RestrictMode::Retain => ("retain", "retain"),
                    RestrictMode::Without => ("without", "drop"),
                };
                let msg = if self.lookup(name).is_some() {
                    format!("`{name}` is not a capability, so it can't appear in a `{kw}` block")
                } else {
                    let mut msg = format!("no capability `{name}` is in scope to {verb} here");
                    // A capitalized name is probably the capability's TYPE
                    // (`without Dir:`) — point at the binding of that type.
                    if name.chars().next().is_some_and(|c| c.is_uppercase()) {
                        let same_type: Vec<&String> = visible
                            .iter()
                            .filter(|b| {
                                self.lookup(b).is_some_and(|t| {
                                    let shown = t.to_string();
                                    shown == *name
                                        || shown
                                            .split(['[', '('])
                                            .next()
                                            .is_some_and(|head| head == name)
                                })
                            })
                            .collect();
                        if let [binding] = same_type.as_slice() {
                            msg.push_str(&format!(
                                " — `{kw}` names the binding, not its type; did you mean `{binding}`?"
                            ));
                        }
                    }
                    msg
                };
                return terr(msg);
            }
        }
        let to_hide: Vec<String> = match r.mode {
            RestrictMode::Without => r.names.clone(),
            RestrictMode::Retain => {
                let keep: HashSet<&str> = r.names.iter().map(String::as_str).collect();
                visible.into_iter().filter(|c| !keep.contains(c.as_str())).collect()
            }
        };
        let frame = self.hidden.last_mut().unwrap();
        for n in to_hide {
            frame.insert(n);
        }
        Ok(())
    }

    fn call_sig(&mut self, name: &str) -> Option<(Vec<Ty>, Ty)> {
        match name {
            "print" => Some((vec![Ty::Console, Ty::String], Ty::Nil)),
            "now" => Some((vec![Ty::Clock], Ty::Int)),
            "get_env" => Some((vec![Ty::Env, Ty::String], Ty::Named("Option".into(), vec![Ty::String]))),
            // Build-time host operations (the build sandbox provides these). Each
            // consumes a build capability; the specific tool/dir/host/var is the
            // consumer's grant, not part of the type.
            "write_out" => Some((vec![Ty::BuildOut, Ty::String, Ty::String], Ty::Nil)),
            "read_build" => Some((vec![Ty::BuildRead, Ty::String], Ty::String)),
            "get_build_env" => {
                Some((vec![Ty::BuildEnv, Ty::String], Ty::Named("Option".into(), vec![Ty::String])))
            }
            "fetch_build" => Some((vec![Ty::BuildNet, Ty::String, Ty::String], Ty::String)),
            "run_tool" => Some((vec![Ty::BuildExec, Ty::String, Ty::String], Ty::String)),
            "string.length" => Some((vec![Ty::String], Ty::Int)),
            "string.char_count" => Some((vec![Ty::String], Ty::Int)),
            "string.to_upper" | "string.to_lower" | "string.trim" => Some((vec![Ty::String], Ty::String)),
            // Abort with a message (the primitive behind std/testing).
            "fail" => Some((vec![Ty::String], Ty::Nil)),
            "string.starts_with" | "string.contains" | "string.ends_with" => {
                Some((vec![Ty::String, Ty::String], Ty::Bool))
            }
            "string.index_of" => Some((vec![Ty::String, Ty::String], Ty::Int)),
            "string.split" => Some((
                vec![Ty::String, Ty::String],
                Ty::List(Box::new(Ty::String)),
            )),
            // The characters of a string, each as a single-char String — built in
            // one O(n) pass (unlike a char_at loop), so callers can index chars in
            // O(1). The primitive behind a fast `std/string.to_chars`.
            "string.chars" => Some((vec![Ty::String], Ty::List(Box::new(Ty::String)))),
            "string.replace" => Some((vec![Ty::String, Ty::String, Ty::String], Ty::String)),
            "string.substring" => Some((vec![Ty::String, Ty::Int, Ty::Int], Ty::String)),
            "string.from_code" => Some((vec![Ty::Int], Ty::String)),
            "math.to_float" => Some((vec![Ty::Int], Ty::Float)),
            "math.to_int" => Some((vec![Ty::Float], Ty::Int)),
            // Duration <-> Int(milliseconds) bridge for the std `duration` module.
            "int_to_duration" => Some((vec![Ty::Int], Ty::Duration)),
            "duration_to_int" => Some((vec![Ty::Duration], Ty::Int)),
            "math.sqrt" => Some((vec![Ty::Float], Ty::Float)),
            "string.to_int" => Some((vec![Ty::String], Ty::Int)),
            "__render" => {
                let a = self.fresh();
                Some((vec![a], Ty::String))
            }
            "send" => {
                let msg = self.fresh();
                Some((vec![Ty::Subject(None), msg], Ty::Nil))
            }
            // `ask` is normally routed to `check_ask` (deep message validation);
            // this fallback signature covers any path that reaches `call_sig`.
            "ask" => {
                let msg = self.fresh();
                Some((vec![Ty::Subject(None), msg], Ty::Int))
            }
            // `reply(v)` hands a value back to an `ask`er. v1 replies are Int.
            "reply" => Some((vec![Ty::Int], Ty::Nil)),
            "list.length" => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem))], Ty::Int))
            }
            "list.at" => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem.clone())), Ty::Int], elem))
            }
            "list.push" => {
                let elem = self.fresh();
                Some((
                    vec![Ty::List(Box::new(elem.clone())), elem.clone()],
                    Ty::List(Box::new(elem)),
                ))
            }
            "list.concat" => {
                let elem = self.fresh();
                let list = Ty::List(Box::new(elem));
                Some((vec![list.clone(), list.clone()], list))
            }
            // Dict(k, v) is an ordinary parameterized Named type; these builtins
            // are generic in its key and value types.
            "dict.new" => {
                let k = self.fresh();
                let v = self.fresh();
                Some((vec![], Ty::Named("Dict".into(), vec![k, v])))
            }
            "dict.insert" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                Some((vec![d.clone(), k, v], d))
            }
            "dict.get_or" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                Some((vec![d, k, v.clone()], v))
            }
            // dict.update(dict, key, default, f) -> dict: a single-lookup upsert. `f`
            // maps the current value (or `default` when the key is absent) to the
            // new value — like Go's `m[k]++` in one operation.
            "dict.update" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                let f = Ty::Fn(vec![v.clone()], Box::new(v.clone()));
                Some((vec![d.clone(), k, v, f], d))
            }
            "dict.has" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v]);
                Some((vec![d, k], Ty::Bool))
            }
            "dict.remove" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v]);
                Some((vec![d.clone(), k], d))
            }
            "dict.keys" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v]);
                Some((vec![d], Ty::List(Box::new(k))))
            }
            "dict.values" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k, v.clone()]);
                Some((vec![d], Ty::List(Box::new(v))))
            }
            "dict.pairs" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                Some((vec![d], Ty::List(Box::new(Ty::Tuple(vec![k, v])))))
            }
            "dict.size" => {
                let k = self.fresh();
                let v = self.fresh();
                Some((vec![Ty::Named("Dict".into(), vec![k, v])], Ty::Int))
            }
            // `read`/`write`/`exists`/`subdir`/`read_only`/`write_only` are handled
            // by `check_dir_op`; `connect`/`listen`/`restrict`/`connect_only`/
            // `listen_only` by `check_net_op` (their rights are enforced per-op).
            "send_line" => Some((vec![Ty::Socket, Ty::String], Ty::Nil)),
            "send_bytes" => Some((vec![Ty::Socket, Ty::String], Ty::Nil)),
            "recv_line" => Some((vec![Ty::Socket], Ty::String)),
            "recv_all" => Some((vec![Ty::Socket], Ty::String)),
            "recv_bytes" => Some((vec![Ty::Socket, Ty::Int], Ty::String)),
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

    /// Validate `send(subject, Msg(args...))`: the message constructor must be a
    /// handler some actor declares (`on Msg(...)`), with a matching argument
    /// count; when exactly one declared signature matches the arity, each
    /// argument is checked against its annotated parameter type. (Previously the
    /// message was a fresh type variable — field-count and type mistakes only
    /// surfaced at runtime.)
    fn check_send(&mut self, args: &[Expr]) -> Result<Ty, TypeError> {
        self.check_delivery("send", args, Ty::Nil)
    }

    /// Validate `ask(subject, Msg(args...))`: same message check as `send`, but
    /// `ask` is synchronous request/response and yields the handler's reply.
    /// v1 replies are `Int` (the common "report a count back" case).
    fn check_ask(&mut self, args: &[Expr]) -> Result<Ty, TypeError> {
        self.check_delivery("ask", args, Ty::Int)
    }

    fn check_delivery(&mut self, verb: &str, args: &[Expr], ret: Ty) -> Result<Ty, TypeError> {
        let subj = self.infer(&args[0])?;
        self.unify(&subj, &Ty::Subject(None)).map_err(|e| TypeError {
            message: format!("in call to `{verb}`: {}", e.message),
        })?;
        // A typed subject (`Subject(Counter)`, e.g. from `spawn Counter()`)
        // names the actor it targets, so the message must be one that actor
        // actually handles — caught here, at compile time, not at delivery.
        let target_actor = match self.resolve(&subj) {
            Ty::Subject(Some(a)) => Some(a),
            _ => None,
        };
        if let Expr::Ctor { name, args: margs } = &args[1] {
            if let Some(actor) = &target_actor {
                if let Some(msgs) = self.actor_messages.get(actor) {
                    if !msgs.contains(name) {
                        return terr(format!(
                            "actor `{actor}` has no handler `on {name}(...)` — `{verb}` to a `Subject({actor})` must use one of its messages"
                        ));
                    }
                }
            }
            if let Some(sigs) = self.actor_handler_sigs.get(name).cloned() {
                let matching: Vec<&Vec<Option<ast::Type>>> =
                    sigs.iter().filter(|s| s.len() == margs.len()).collect();
                if matching.is_empty() {
                    let arities: Vec<String> =
                        sigs.iter().map(|s| s.len().to_string()).collect();
                    return terr(format!(
                        "message `{name}` takes {} argument(s), but {} were sent",
                        arities.join(" or "),
                        margs.len()
                    ));
                }
                if let [sig] = matching.as_slice() {
                    let sig = (*sig).clone();
                    for (a, pt) in margs.iter().zip(&sig) {
                        let at = self.infer(a)?;
                        if let Some(t) = pt {
                            let want = self.to_ty(t);
                            self.unify(&want, &at).map_err(|e| TypeError {
                                message: format!("in message `{name}`: {}", e.message),
                            })?;
                        }
                    }
                } else {
                    for a in margs {
                        self.infer(a)?;
                    }
                }
            } else if !self.actor_handler_sigs.is_empty() {
                return terr(format!(
                    "no actor declares a handler `on {name}(...)` — the message would never be delivered"
                ));
            } else {
                for a in margs {
                    self.infer(a)?;
                }
            }
        } else {
            self.infer(&args[1])?;
        }
        Ok(ret)
    }

    /// Resolve a call's first argument as a `Dir` capability and yield its rights.
    /// An unconstrained variable defaults to the full right-set (bare `Dir`).
    fn dir_cap_rights(&mut self, name: &str, arg: &Expr) -> Result<DirRights, TypeError> {
        let cap = self.infer(arg)?;
        match self.resolve(&cap) {
            Ty::Dir(r) => Ok(r),
            Ty::Var(_) => {
                self.unify(&cap, &Ty::Dir(DirRights::full()))?;
                Ok(DirRights::full())
            }
            other => terr(format!(
                "`{name}` expects a `Dir` capability but got `{other}`"
            )),
        }
    }

    /// Type-check a directory-capability op, enforcing that the `Dir`'s rights
    /// permit the verb: `read`/`exists`/`subdir`/`list` need `Read`; `write`/
    /// `append`/`make_dir` need `Write`. (Narrowing is done with the `as`
    /// ascription, not per-op builtins.) Returns `Ok(None)` when `name` is not
    /// a Dir op.
    fn check_dir_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        let arity = match name {
            "list" => 1,
            "read" | "exists" | "is_dir" | "subdir" | "make_dir" => 2,
            "write" | "append" => 3,
            _ => return Ok(None),
        };
        if args.len() != arity {
            return terr(format!(
                "`{name}` expects {arity} argument(s) but got {}",
                args.len()
            ));
        }
        let rights = self.dir_cap_rights(name, &args[0])?;
        // The trailing arguments (path, and for `write` the content) are strings.
        for arg in &args[1..] {
            let at = self.infer(arg)?;
            self.unify(&Ty::String, &at).map_err(|e| TypeError {
                message: format!("in call to `{name}`: {}", e.message),
            })?;
        }
        let ret = match name {
            "read" => {
                if !rights.read {
                    return terr(format!("`read` needs `Read` but the capability is `{rights}`"));
                }
                Ty::String
            }
            "exists" => {
                if !rights.read {
                    return terr(format!(
                        "`exists` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::Bool
            }
            "is_dir" => {
                if !rights.read {
                    return terr(format!(
                        "`is_dir` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::Bool
            }
            "subdir" => {
                if !rights.read {
                    return terr(format!(
                        "`subdir` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::Dir(rights)
            }
            "list" => {
                if !rights.read {
                    return terr(format!("`list` needs `Read` but the capability is `{rights}`"));
                }
                Ty::List(Box::new(Ty::String))
            }
            "write" | "append" => {
                if !rights.write {
                    return terr(format!(
                        "`{name}` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::Nil
            }
            "make_dir" => {
                if !rights.write {
                    return terr(format!(
                        "`make_dir` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::Nil
            }
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

    /// Resolve a call's first argument as a `Net` capability and yield its verbs.
    /// An unconstrained variable defaults to the full set (bare `Net`).
    fn net_cap_rights(&mut self, name: &str, arg: &Expr) -> Result<NetRights, TypeError> {
        let cap = self.infer(arg)?;
        match self.resolve(&cap) {
            Ty::Net(r) => Ok(r),
            Ty::Var(_) => {
                self.unify(&cap, &Ty::Net(NetRights::full()))?;
                Ok(NetRights::full())
            }
            other => terr(format!(
                "`{name}` expects a `Net` capability but got `{other}`"
            )),
        }
    }

    /// Type-check a network-capability op, enforcing the `Net`'s rights permit it:
    /// `connect` needs `Connect` (+`Tcp`); `listen` needs `Listen` (+`Tcp`);
    /// `restrict` is verb-neutral address attenuation (preserves the rights set).
    /// (Narrowing is done with the `as` ascription, not per-verb builtins.)
    /// Returns `Ok(None)` when `name` is not a Net op.
    fn check_net_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        let arity = match name {
            "connect" | "try_connect" | "listen" | "restrict" => 2,
            _ => return Ok(None),
        };
        if args.len() != arity {
            return terr(format!(
                "`{name}` expects {arity} argument(s) but got {}",
                args.len()
            ));
        }
        let rights = self.net_cap_rights(name, &args[0])?;
        // The trailing argument (an address) is a string.
        for arg in &args[1..] {
            let at = self.infer(arg)?;
            self.unify(&Ty::String, &at).map_err(|e| TypeError {
                message: format!("in call to `{name}`: {}", e.message),
            })?;
        }
        let ret = match name {
            "connect" => {
                if !rights.connect {
                    return terr(format!(
                        "`connect` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`connect` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Socket
            }
            // Like `connect` but total: returns `Option(Socket)` — `None` on a failed
            // dial instead of trapping. Lets a server (e.g. a proxy) survive a down
            // upstream. Same rights as `connect`.
            "try_connect" => {
                if !rights.connect {
                    return terr(format!(
                        "`try_connect` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`try_connect` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Named("Option".into(), vec![Ty::Socket])
            }
            "listen" => {
                if !rights.listen {
                    return terr(format!(
                        "`listen` needs `Listen` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`listen` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Listener
            }
            // Attenuating the address set leaves the rights (verbs + transports) intact.
            "restrict" => Ty::Net(rights),
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

    /// Check a capability narrowing ascription (`src as target`): the target must
    /// be the *same* capability with a subset of `src`'s rights. Narrowing only
    /// drops rights — you can never ascribe a right the source doesn't hold. An
    /// unconstrained source is pinned to the target.
    fn check_narrow(&mut self, src: &Ty, target: &Ty) -> Result<(), TypeError> {
        let resolved = self.resolve(src);
        let ok = match (&resolved, target) {
            (Ty::Dir(s), Ty::Dir(t)) => (!t.read || s.read) && (!t.write || s.write),
            (Ty::Net(s), Ty::Net(t)) => {
                (!t.connect || s.connect)
                    && (!t.listen || s.listen)
                    && (!t.tcp || s.tcp)
                    && (!t.udp || s.udp)
                    && (!t.uds || s.uds)
            }
            (Ty::Console, Ty::Console) => true,
            // An unconstrained source: pin it to the ascribed capability.
            (Ty::Var(_), Ty::Dir(_) | Ty::Net(_) | Ty::Console) => {
                return self.unify(src, target).map_err(|e| TypeError {
                    message: format!("in `as` ascription: {}", e.message),
                });
            }
            _ => {
                return terr(format!(
                    "`as` narrows a capability to a subset of its rights; cannot ascribe `{resolved}` as `{target}`"
                ));
            }
        };
        if !ok {
            return terr(format!(
                "`as` can only drop rights: `{target}` is not a subset of `{resolved}`"
            ));
        }
        Ok(())
    }

    /// Bind a call argument (`actual`) to a parameter (`expected`), allowing a
    /// broader capability to stand in for a narrower one — implicit directional
    /// narrowing, so a full `Net` satisfies a `Net[Connect]` parameter (more
    /// authority can always be used where less is asked). The callee is still
    /// type-bounded to the parameter's rights, so this stays sound: it cannot do
    /// (or re-pass) more than its declared type permits. Everything else (Vars,
    /// non-caps, exact caps, a too-narrow argument) falls back to unification.
    fn coerce_arg(&mut self, expected: &Ty, actual: &Ty) -> Result<(), TypeError> {
        let coercible = match (self.resolve(expected), self.resolve(actual)) {
            // `want`'s rights must be a subset of what the argument `has`.
            (Ty::Dir(want), Ty::Dir(has)) => (!want.read || has.read) && (!want.write || has.write),
            (Ty::Net(want), Ty::Net(has)) => {
                (!want.connect || has.connect)
                    && (!want.listen || has.listen)
                    && (!want.tcp || has.tcp)
                    && (!want.udp || has.udp)
                    && (!want.uds || has.uds)
            }
            _ => false,
        };
        if coercible {
            Ok(())
        } else {
            self.unify(expected, actual)
        }
    }

    // --- inference ---

    fn infer_block(&mut self, block: &Block) -> Result<Ty, TypeError> {
        let is_region = block.region.is_some();
        if is_region {
            self.region_locals.push(HashSet::new());
        }
        let result = self.infer_block_inner(block);
        if is_region {
            self.region_locals.pop();
        }
        let ty = result?;
        if let Some(ann) = &block.region {
            if let Some(want) = &ann.ty {
                let want_ty = self.to_ty(want);
                self.unify(&ty, &want_ty).map_err(|e| TypeError {
                    message: format!("region value: {}", e.message),
                })?;
            }
        }
        Ok(ty)
    }

    fn infer_block_inner(&mut self, block: &Block) -> Result<Ty, TypeError> {
        self.push();
        if let Some(r) = &block.restrict {
            if let Err(e) = self.apply_restrict(r) {
                self.pop();
                return Err(e);
            }
        }
        let mut ty = Ty::Nil;
        for (i, stmt) in block.stmts.iter().enumerate() {
            if let Some(line) = block.lines.get(i) {
                self.cur_line = *line;
            }
            match stmt {
                Stmt::Let { name, ty: decl, mutable, value } => {
                    let vt = self.infer(value)?;
                    // An ascription is a unification constraint: it pins type
                    // variables the RHS leaves open (`let xs: List(Int) = []`,
                    // a return-position type variable) and errors at THIS line
                    // when the RHS disagrees.
                    if let Some(decl) = decl {
                        let want = self.to_ty(decl);
                        self.unify(&want, &vt).map_err(|e| TypeError {
                            message: format!(
                                "`{name}` is declared `{want}` but the value disagrees: {}",
                                e.message
                            ),
                        })?;
                    }
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
                    if let Some(scope) = self.region_locals.last() {
                        if !scope.contains(name) && !is_scalar_ty(&self.resolve(&existing)) {
                            self.pop();
                            return terr(format!(
                                "cannot assign `{name}` inside `region:`: it is declared outside the region and is not a scalar — a region's only escape is its value"
                            ));
                        }
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
                Stmt::Yield(e) => {
                    if !self.region_locals.is_empty() {
                        self.pop();
                        return terr(
                            "cannot `yield` inside `region:`: the generator frame outlives the region".to_string(),
                        );
                    }
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
        let t = self.infer_inner(expr)?;
        if let Some(rec) = &mut self.type_record {
            rec.insert(expr as *const Expr as usize, t.clone());
        }
        Ok(t)
    }

    fn infer_inner(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        match expr {
            Expr::Int(_) => Ok(Ty::Int),
            Expr::Float(_) => Ok(Ty::Float),
            Expr::Duration(_) => Ok(Ty::Duration),
            Expr::Str(_) => Ok(Ty::String),
            Expr::Bool(_) => Ok(Ty::Bool),
            // A range lowers to a list-building block; type it as that block.
            Expr::Range { lo, hi, inclusive } => {
                let d = crate::parser::desugar_range((**lo).clone(), (**hi).clone(), *inclusive);
                self.infer(&d)
            }
            // A subscript lowers to an `list.at(base, index)` call; type it as that.
            Expr::Index { base, index } => {
                let d = crate::parser::desugar_index((**base).clone(), (**index).clone());
                self.infer(&d)
            }
            Expr::MethodCall { method, .. } => {
                // Trait lowering resolves every method call (impl, trait
                // bound, or static); one that survives is unresolvable.
                terr(format!(
                    "cannot resolve the method call `.{method}(…)` — methods come from \
                     `impl` blocks; a plain function is called as `{method}(value, …)`"
                ))
            }
            // Named-field record construction is lowered by `crate::records`
            // before type-checking.
            Expr::Record { .. } => {
                unreachable!("Expr::Record is lowered by crate::records before typeck")
            }
            // `while let` lowers to a `while true` over a match; type that.
            Expr::WhileLet { pattern, scrutinee, body } => {
                let d = crate::parser::desugar_while_let(
                    pattern.clone(),
                    (**scrutinee).clone(),
                    body.clone(),
                );
                self.infer(&d)
            }
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
                // function. Reject `var`/`own` functions, whose move-in/out
                // calling convention can't be expressed as a plain value.
                if let Some((params, ret)) = self.fn_sigs.get(name).cloned() {
                    if let Some(convs) = self.fn_conventions.get(name) {
                        if convs.iter().any(|c| *c != Convention::Let) {
                            return terr(format!(
                                "`{name}` takes a `var`/`own` parameter, so it can't be used as a function value"
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
                if self.is_firewalled(name) {
                    return terr(format!(
                        "`{name}` is walled off in this block by a `retain`/`without` and can't be used here"
                    ));
                }
                terr(format!("unbound variable `{name}`"))
            }
            Expr::Lambda { params, body } => {
                // Closures capture by value, so an assignment to a captured
                // (outer) variable cannot propagate out: the interpreter would
                // silently mutate a private copy while the compiled backends can't
                // express it at all. Reject it uniformly here (using codegen's
                // exact capture/assignment scan) so every backend agrees.
                let outer = crate::codegen::lambda_outer_assigns(params, body);
                if !outer.is_empty() {
                    return terr(format!(
                        "a closure cannot assign to the captured variable `{}` (captures are by value, so the write would be lost) — return the new value or use a `var` parameter instead",
                        outer.join("`, `")
                    ));
                }
                self.push();
                let param_tys: Vec<Ty> = params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => self.to_ty(t),
                        None => self.fresh(),
                    })
                    .collect();
                for (p, ty) in params.iter().zip(&param_tys) {
                    self.define(p.name.clone(), ty.clone(), p.convention.binds_mutable());
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
                                self.coerce_arg(pty, &at).map_err(|e| TypeError {
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
                if let Some(t) = self.check_dir_op(name, args)? {
                    return Ok(t);
                }
                if let Some(t) = self.check_net_op(name, args)? {
                    return Ok(t);
                }
                if name == "send" && args.len() == 2 {
                    return self.check_send(args);
                }
                if name == "ask" && args.len() == 2 {
                    return self.check_ask(args);
                }
                let Some((params, ret)) = self.call_sig(name) else {
                    // `to_string` was removed from the surface: interpolation
                    // IS the rendering (it desugars to the internal __render).
                    if name == "to_string" || name == "int_to_string" {
                        return terr(format!(
                            "`{name}` was removed — render with `\"${{x}}\"` \
                             interpolation (it works on every value), or \
                             `say(console, x)` to print a `Show` value"
                        ));
                    }
                    // A retired global builtin: name the module-qualified
                    // spelling that replaced it (the one-cut migration).
                    if let Some(moved) = moved_builtin(name) {
                        return terr(format!(
                            "`{name}` moved to `{moved}` — pure data operations are \
                             module-qualified now (no import needed; the core modules \
                             are always available)"
                        ));
                    }
                    // If the name is an unimported stdlib function, point the way;
                    // otherwise suggest a near-miss stdlib name (a likely typo).
                    let hint = match crate::linker::std_modules_for_function(name).as_slice() {
                        [m] => format!(" — did you forget `import {m}`?"),
                        many if !many.is_empty() => {
                            format!(" — did you forget to import one of: {}?", many.join(", "))
                        }
                        _ => match crate::linker::closest_std_function(name) {
                            Some((cand, m)) => format!(" — did you mean `{cand}` (`import {m}`)?"),
                            None => String::new(),
                        },
                    };
                    return terr(format!("call to unknown function `{name}`{hint}"));
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
                    self.coerce_arg(param_ty, &at)
                        .map_err(|e| TypeError { message: format!("in call to `{name}`: {}", e.message) })?;
                }
                // Enforce conventions: a `var` parameter needs a mutable variable;
                // `own` consumes its argument (use-after-move becomes an error).
                if let Some(convs) = self.fn_conventions.get(name).cloned() {
                    for (arg, conv) in args.iter().zip(&convs) {
                        match conv {
                            Convention::Inout => match arg {
                                Expr::Var(v) if self.is_mutable(v) == Some(true) => {}
                                Expr::Var(v) => {
                                    return terr(format!(
                                        "argument `{v}` to `{name}` is passed to a `var` parameter, so it must be a mutable `var`"
                                    ))
                                }
                                _ => {
                                    return terr(format!(
                                        "the argument to a `var` parameter of `{name}` must be a mutable variable"
                                    ))
                                }
                            },
                            Convention::Sink => {
                                if let Expr::Var(v) = arg {
                                    self.consumed.insert(v.clone());
                                }
                            }
                            // An owned value or an immutable borrow: no call-site
                            // obligation (a borrow's no-escape rule is enforced at
                            // native-lowering time by Rust's borrow checker).
                            Convention::Let | Convention::Borrow => {}
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
                        // A capability field accepts a broader argument (a full
                        // `Net` into a `Net[Connect]` field), like a call boundary.
                        self.coerce_arg(fty, &at).map_err(|e| TypeError {
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
                    // `move x` has the type of `x`, and consumes it: it's a
                    // use-site ownership transfer, so any later use of the moved
                    // variable is an error on every backend (matching native's
                    // actual move). `infer(expr)` above already rejected moving an
                    // already-consumed binding.
                    UnOp::Move => {
                        if let Expr::Var(v) = expr.as_ref() {
                            self.consumed.insert(v.clone());
                        }
                        Ok(t)
                    }
                    // `await e` has the type of `e` (Phase 1: the awaited value is
                    // the value itself; suspension is invisible to the type).
                    UnOp::Await => Ok(t),
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
                // `pair.0` — a tuple element, by position.
                if let Ok(i) = field.parse::<usize>() {
                    let Ty::Tuple(ts) = &resolved else {
                        return terr(format!(
                            "element access `.{field}` requires a tuple, found `{resolved}`"
                        ));
                    };
                    let Some(t) = ts.get(i) else {
                        return terr(format!(
                            "tuple `{resolved}` has no element `.{i}` (it has {})",
                            ts.len()
                        ));
                    };
                    return Ok(t.clone());
                }
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
            Expr::As { expr, ty } => {
                let src = self.infer(expr)?;
                let target = self.to_ty(ty);
                self.check_narrow(&src, &target)?;
                Ok(target)
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
        }
    }

    fn infer_binary(&mut self, op: ast::BinOp, lhs: &Expr, rhs: &Expr) -> Result<Ty, TypeError> {
        use ast::BinOp::*;
        let lt = self.infer(lhs)?;
        let rt = self.infer(rhs)?;
        match op {
            Add | Sub | Mul | Div => {
                let lr = self.resolve(&lt);
                let rr = self.resolve(&rt);
                // `+` concatenates strings — type-directed, never coercing: a
                // String operand demands a String on the other side, and a
                // mixed `+` names interpolation as the fix.
                if matches!(lr, Ty::String) || matches!(rr, Ty::String) {
                    if !matches!(op, Add) {
                        return terr("only `+` is defined on String (it concatenates)");
                    }
                    for other in [&lr, &rr] {
                        if !matches!(other, Ty::String | Ty::Var(_)) {
                            return terr(format!(
                                "cannot `+` a String and `{other}` — render the value \
                                 into the string instead: `\"${{a}}${{b}}\"`"
                            ));
                        }
                    }
                    self.unify(&Ty::String, &lt)?;
                    self.unify(&Ty::String, &rt)?;
                    return Ok(Ty::String);
                }
                // Duration arithmetic: durations add/subtract with each other,
                // scale by an Int (multiply / divide), and Duration / Duration is
                // their Int ratio. Mixing a Duration with a bare Int under +/-
                // is a type error.
                if matches!(lr, Ty::Duration) || matches!(rr, Ty::Duration) {
                    return match op {
                        Add | Sub => {
                            self.unify(&Ty::Duration, &lt)?;
                            self.unify(&Ty::Duration, &rt)?;
                            Ok(Ty::Duration)
                        }
                        Mul => {
                            if matches!(lr, Ty::Duration) {
                                self.unify(&Ty::Int, &rt)?;
                            } else {
                                self.unify(&Ty::Int, &lt)?;
                            }
                            Ok(Ty::Duration)
                        }
                        Div => {
                            if matches!(lr, Ty::Duration) && matches!(rr, Ty::Duration) {
                                Ok(Ty::Int)
                            } else if matches!(lr, Ty::Duration) {
                                self.unify(&Ty::Int, &rt)?;
                                Ok(Ty::Duration)
                            } else {
                                terr("an Int cannot be divided by a Duration")
                            }
                        }
                        _ => unreachable!(),
                    };
                }
                let either_float = matches!(lr, Ty::Float) || matches!(rr, Ty::Float);
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
                // Surface `<>` is gone; the OP survives as compiler-internal IR
                // (codegen flips string `+` to it after annotation), so the
                // annotating run types it and only the user-facing check
                // teaches.
                if self.type_record.is_some() {
                    self.unify(&Ty::String, &lt)?;
                    self.unify(&Ty::String, &rt)?;
                    return Ok(Ty::String);
                }
                terr("`<>` was removed — `+` concatenates strings, and `\"${a}${b}\"` interpolates")
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
                    Ty::Int | Ty::Float | Ty::String | Ty::Duration => Ok(Ty::Bool),
                    other => terr(format!(
                        "ordering comparison requires Int, Float, String, or Duration, found `{other}`"
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
        borrow_escape_check(func)?;
        let (params, ret) = self.fn_sigs.get(&func.name).cloned().unwrap();
        self.scopes = vec![HashMap::new()];
        self.hidden = vec![HashSet::new()];
        self.consumed.clear();
        self.current_ret = Some(ret.clone());
        self.cur_line = 0;
        for (param, ty) in func.params.iter().zip(&params) {
            self.define(param.name.clone(), ty.clone(), param.convention.binds_mutable());
        }
        let body = self.infer_block(&func.body)?;
        // A broader capability may be returned where a narrower one is declared
        // (`-> Net[Connect]` returning a full `Net`), mirroring call-argument
        // narrowing; `coerce_arg` falls back to unification for everything else.
        self.coerce_arg(&ret, &body).map_err(|e| TypeError {
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

}

/// Type-check a whole module. Returns the first error found.
pub fn check(module: &Module) -> Result<(), TypeError> {
    // Catch duplicate top-level functions before lowering, while `impl` methods
    // are still distinct from free functions (so overloaded methods aren't
    // mistaken for duplicates) and source lines are still available.
    check_unique_functions(module)?;

    // Lower named-field record construction (a no-op once the linker has done so,
    // but covers single-module paths like `check_str`).
    let recs = crate::records::lower(module.clone()).map_err(|message| TypeError { message })?;

    // Trait/impl declarations are desugared to ordinary functions first, so the
    // checker only ever sees plain functions (a no-op for trait-free modules).
    // The checked flavor surfaces unsatisfiable dispatch ("`Float` does not
    // implement `Show`") instead of a post-lowering unknown-function error.
    let lowered = crate::traits::lower_checked(recs).map_err(|message| TypeError { message })?;
    run_check(&lowered, false).map(|_| ())
}

/// The resolved-type side table produced by `annotate`: expression identity
/// (`&Expr as *const _`) -> the concrete `Ty` the checker inferred, finalized
/// against the ending substitution. Entries exist only where the type is
/// fully concrete (no free variables) — consumers fall back where it is not.
#[derive(Default)]
pub struct TypeTable {
    types: HashMap<usize, Ty>,
}

impl TypeTable {
    pub fn type_of(&self, e: &Expr) -> Option<&Ty> {
        self.types.get(&(e as *const Expr as usize))
    }
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

/// Convert a resolved checker type to the surface `ast::Type` shape the
/// backends' type-directed machinery (eq/to_string shapes, valtypes)
/// consumes. None where no surface form exists (functions, free variables).
pub fn ty_to_ast(t: &Ty) -> Option<crate::ast::Type> {
    use crate::ast::Type as T;
    Some(match t {
        Ty::Int => T::Named("Int".into(), Vec::new()),
        Ty::Float => T::Named("Float".into(), Vec::new()),
        Ty::Duration => T::Named("Duration".into(), Vec::new()),
        Ty::String => T::Named("String".into(), Vec::new()),
        Ty::Bool => T::Named("Bool".into(), Vec::new()),
        Ty::Nil => T::Named("Nil".into(), Vec::new()),
        Ty::Console => T::Named("Console".into(), Vec::new()),
        Ty::Clock => T::Named("Clock".into(), Vec::new()),
        Ty::Env => T::Named("Env".into(), Vec::new()),
        Ty::Secret => T::Named("Secret".into(), Vec::new()),
        // The actor name is erased for the backends — a Subject is always an
        // i32 id, regardless of which actor it targets.
        Ty::Subject(_) => T::Named("Subject".into(), Vec::new()),
        Ty::Dir(_) => T::Named("Dir".into(), Vec::new()),
        Ty::Net(_) => T::Named("Net".into(), Vec::new()),
        Ty::Socket => T::Named("Socket".into(), Vec::new()),
        Ty::Listener => T::Named("Listener".into(), Vec::new()),
        Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv | Ty::BuildNet | Ty::BuildExec => {
            return None
        }
        Ty::List(e) => T::Named("List".into(), vec![ty_to_ast(e)?]),
        Ty::Tuple(ts) => {
            T::Tuple(ts.iter().map(ty_to_ast).collect::<Option<Vec<_>>>()?)
        }
        Ty::Named(n, args) => T::Named(
            n.clone(),
            args.iter().map(ty_to_ast).collect::<Option<Vec<_>>>()?,
        ),
        Ty::Fn(..) | Ty::Var(_) => return None,
    })
}

fn ty_has_var(t: &Ty) -> bool {
    match t {
        Ty::Var(_) => true,
        Ty::List(e) => ty_has_var(e),
        Ty::Tuple(ts) => ts.iter().any(ty_has_var),
        Ty::Named(_, args) => args.iter().any(ty_has_var),
        Ty::Fn(ps, r) => ps.iter().any(ty_has_var) || ty_has_var(r),
        _ => false,
    }
}

/// Annotate an ALREADY-LOWERED module instance (the exact AST a consumer will
/// keep walking): the typed-lowering keystone (docs/language-evolution.md
/// Phase 0). Best-effort by contract — consumers only annotate modules that
/// already passed `check`, so any error here yields an empty table and the
/// consumer's own fallbacks apply.
pub fn annotate(module: &Module) -> TypeTable {
    match run_check(module, true) {
        Ok(Some(table)) => table,
        Err(e) => {
            if std::env::var_os("WITCHY_DEBUG_ANNOTATE").is_some() {
                eprintln!("annotate: checker error on lowered module: {e}");
            }
            TypeTable::default()
        }
        _ => TypeTable::default(),
    }
}

fn run_check(module: &Module, record: bool) -> Result<Option<TypeTable>, TypeError> {
    let module = &module;
    let mut c = Checker {
        type_record: if record { Some(HashMap::new()) } else { None },
        fn_sigs: HashMap::new(),
        fn_conventions: HashMap::new(),
        ctor_sigs: HashMap::new(),
        ctor_typarams: HashMap::new(),
        record_fields: HashMap::new(),
        adt_variants: HashMap::new(),
        actor_handler_sigs: HashMap::new(),
        actor_messages: HashMap::new(),
        fn_typarams: HashMap::new(),
        subst: HashMap::new(),
        next_var: 0,
        scopes: vec![HashMap::new()],
        hidden: vec![HashSet::new()],
        consumed: HashSet::new(),
        region_locals: Vec::new(),
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
                // A type's parameters: explicit ones (`type Step(m, a):`) FIX the
                // order; otherwise infer them from the variant field types in order
                // of first appearance (so `type Option { Some(a) None }` has one
                // param `a`). Explicit params are required when a constructor omits
                // one (e.g. `Done(a)` for `Step(m, a)`): inference would drop the
                // omitted `m` from that constructor's result type, mis-aligning it.
                let mut param_names: Vec<String> = t.params.clone();
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
            // Desugared to functions by `traits::lower` and constants inlined by
            // `crate::consts` before this point.
            Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }

    // Reject typo'd / undeclared type names in signatures before they become
    // opaque types that mis-unify with a confusing message later.
    check_type_names(module)?;

    // `main` is the root actor: its parameters are where the host's authority
    // enters, so they must be capabilities (or the args list) — validate before
    // diving into bodies so a malformed entry point is reported up front.
    check_main_signature(module)?;
    // A rune's `build` entrypoint is the root of the build sandbox; its parameters
    // are where build-time authority enters, so they must be build capabilities.
    check_build_signature(module)?;

    // Pass 2: check bodies. `where`-bounded templates are checked through
    // their monomorphic instantiations (trait-method calls in a template
    // body only resolve once the bound variable is concrete) — the lowering
    // extracts them before `check` historically; `annotate` runs earlier in
    // the pipeline and skips them here for the same reason.
    for item in &module.items {
        match item {
            Item::Function(f) if !f.bounds.is_empty() => {}
            Item::Function(f) => {
                c.check_function(f).map_err(|e| at_loc(e, c.cur_line, &f.name))?
            }
            Item::Type(_) | Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    if let Some(rec) = c.type_record.take() {
        let mut types = HashMap::new();
        for (k, ty) in rec {
            let resolved = c.resolve(&ty);
            if !ty_has_var(&resolved) {
                types.insert(k, resolved);
            }
        }
        return Ok(Some(TypeTable { types }));
    }
    Ok(None)
}

/// The module-qualified NATIVE INTRINSICS: declared in std as self-recursive
/// placeholders (signatures for the checker), intercepted by name on both
/// backends, never templated by monomorphization, never compiled as bodies.
pub fn intrinsic(name: &str) -> bool {
    matches!(
        name,
        "list.push" | "list.at" | "list.length" | "list.concat"
            | "dict.new" | "dict.insert" | "dict.get_or" | "dict.has" | "dict.remove"
            | "dict.update" | "dict.keys" | "dict.values" | "dict.pairs" | "dict.size"
            | "string.split" | "string.trim" | "string.contains" | "string.starts_with"
            | "string.ends_with" | "string.replace" | "string.index_of" | "string.substring"
            | "string.length" | "string.char_count" | "string.chars" | "string.to_upper"
            | "string.to_lower" | "string.to_int"
            | "math.to_float" | "math.to_int" | "math.sqrt"
    )
}

/// The retired global builtins and the module-qualified spellings that
/// replaced them (docs/language-evolution.md Phase 2 — one cut, no aliases).
pub fn moved_builtin(bare: &str) -> Option<&'static str> {
    Some(match bare {
        "push" => "list.push",
        "at" => "list.at",
        "length" => "list.length",
        "concat" => "list.concat",
        "dict_new" => "dict.new",
        "insert" => "dict.insert",
        "get_or" => "dict.get_or",
        "has" => "dict.has",
        "remove" => "dict.remove",
        "update" => "dict.update",
        "keys" => "dict.keys",
        "values" => "dict.values",
        "pairs" => "dict.pairs",
        "size" => "dict.size",
        "split" => "string.split",
        "trim" => "string.trim",
        "contains" => "string.contains",
        "starts_with" => "string.starts_with",
        "ends_with" => "string.ends_with",
        "replace" => "string.replace",
        "index_of" => "string.index_of",
        "substring" => "string.substring",
        "string_length" => "string.length",
        "char_count" => "string.char_count",
        "string_chars" => "string.chars",
        "to_chars" => "string.chars",
        "to_upper" => "string.to_upper",
        "to_lower" => "string.to_lower",
        "string_to_int" => "string.to_int",
        "int_to_float" => "math.to_float",
        "float_to_int" => "math.to_int",
        "sqrt" => "math.sqrt",
        _ => return None,
    })
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
    fn undeclared_type_names_are_rejected() {
        // A typo'd type used to become an opaque type that mis-unified later
        // ("expected `Flarb`, found `Int`"); now it's a clear "unknown type".
        let param = check_str("fn f(x: Flarb) -> Int:\n    1\n").unwrap_err();
        assert!(param.contains("unknown type `Flarb`"), "{param}");
        // Caught in nested positions too (this used to slip through entirely).
        let nested = check_str("fn f(xs: List(Flarb)) -> Int:\n    1\n").unwrap_err();
        assert!(nested.contains("unknown type `Flarb`"), "{nested}");
        // Builtins, capability rights, generics, Option, and declared types pass.
        check_str("fn id(x: a) -> a:\n    x\n").expect("a generic parameter is valid");
        check_str("fn g(dir: Dir[Read], o: Option(Int)) -> Int:\n    0\n")
            .expect("caps with rights and Option are valid");
        check_str("type Color:\n    Red\nfn name(c: Color) -> String:\n    \"r\"\n")
            .expect("a declared type is valid");
        // A variant field referencing an unknown type is caught too.
        let field = check_str("type Wrap:\n    Wrap(Flarb)\n").unwrap_err();
        assert!(field.contains("unknown type `Flarb`"), "{field}");
        // Recursive, generic, and Option-typed fields remain valid.
        check_str("type Tree:\n    Leaf\n    Node(Tree, Int, Tree)\n").expect("recursive type is valid");
        check_str("type Box:\n    Box(a)\n").expect("generic type is valid");
    }

    #[test]
    fn capability_firewall_drops_and_retains() {
        // `without c:` drops `c` inside the block — using it is an error, but the
        // sibling capability is untouched.
        let drop_clock =
            "fn main(console: Console, clock: Clock):\n    without clock:\n        print(console, \"ok\")\n";
        check_str(drop_clock).expect("console still usable when only clock is dropped");
        let use_dropped =
            "fn main(console: Console, clock: Clock):\n    without clock:\n        let t = now(clock)\n        print(console, __render(t))\n";
        let err = check_str(use_dropped).expect_err("using a dropped capability must fail");
        assert!(err.contains("walled off"), "got: {err}");

        // `retain c:` keeps only `c`; every other capability is hidden.
        let retain_console =
            "fn main(console: Console, clock: Clock):\n    retain console:\n        print(console, \"ok\")\n";
        check_str(retain_console).expect("the retained capability is usable");
        let retain_drops_rest =
            "fn main(console: Console, clock: Clock):\n    retain console:\n        let t = now(clock)\n        print(console, __render(t))\n";
        let err = check_str(retain_drops_rest).expect_err("a non-retained capability must be hidden");
        assert!(err.contains("walled off"), "got: {err}");

        // `retain:` with no names is a full sandbox — even `console` is gone.
        let sandbox = "fn main(console: Console):\n    retain:\n        print(console, \"nope\")\n";
        let err = check_str(sandbox).expect_err("an empty retain drops every capability");
        assert!(err.contains("walled off"), "got: {err}");
    }

    #[test]
    fn build_entrypoint_takes_only_build_capabilities() {
        // A valid build step: build caps only.
        check_str("fn build(out: BuildOut, schema: BuildRead):\n    write_out(out, \"x.witchy\", read_build(schema, \"a.proto\"))\n")
            .expect("a build step taking build caps is valid");
        // A runtime capability in `build` is rejected — the build sandbox grants
        // only build-time authority.
        let err = check_str("fn build(out: BuildOut, net: Net):\n    write_out(out, \"x\", \"y\")\n")
            .expect_err("a runtime cap in build must be rejected");
        assert!(err.contains("build step may only take build-time capabilities"), "{err}");
        // And `main` may not take a build capability.
        let err = check_str("fn main(console: Console, out: BuildOut):\n    print(console, \"no\")\n")
            .expect_err("a build cap in main must be rejected");
        assert!(err.contains("`main` may only take host capabilities"), "{err}");
        // A `build` function with no build cap is an ordinary function, not the
        // entrypoint, so it isn't subject to the build-signature rule.
        check_str("fn build(x: Int) -> Int:\n    x + 1\n")
            .expect("a plain `build` function is not the build entrypoint");
    }

    #[test]
    fn capability_firewall_validates_its_names() {
        // Naming a non-capability binding is rejected — it almost certainly means
        // the author misremembered what authority the block holds.
        let not_cap = "fn main(console: Console):\n    let x = 5\n    without x:\n        print(console, \"hi\")\n";
        let err = check_str(not_cap).expect_err("a non-capability can't be firewalled");
        assert!(err.contains("not a capability"), "got: {err}");
        // Naming something not in scope at all is rejected too.
        let absent = "fn main(console: Console):\n    without clock:\n        print(console, \"hi\")\n";
        let err = check_str(absent).expect_err("an out-of-scope name can't be firewalled");
        assert!(err.contains("no capability `clock` is in scope"), "got: {err}");
    }

    #[test]
    fn capability_firewall_is_sealed_against_outer_caps() {
        // The point of the firewall: a `retain` block sees exactly the named
        // capabilities even though the outer scope holds more. Adding `clock` to
        // `main` must NOT make it reachable inside `retain console`.
        let src =
            "fn main(console: Console, clock: Clock):\n    retain console:\n        now(clock)\n        print(console, \"x\")\n";
        let err = check_str(src).expect_err("retain must seal the block from outer clock");
        assert!(err.contains("walled off"), "got: {err}");
        // A nested re-binding legitimately shadows the firewall: re-using the name
        // for a fresh value is fine (you still can't reach the dropped capability).
        let shadow =
            "fn use_int(n: Int):\n    fail(__render(n))\nfn main(console: Console):\n    without console:\n        let console = 42\n        use_int(console)\n";
        check_str(shadow).expect("re-binding a dropped name to a fresh value is allowed");
    }

    #[test]
    fn duplicate_top_level_functions_are_rejected() {
        // Two functions with the same name silently overwrote each other; now it's
        // a check-time error that names the function and (unlinked) the lines.
        let err = check_str("fn g(x: Int) -> Int:\n    1\nfn g(x: Int) -> Int:\n    2\n").unwrap_err();
        assert!(err.contains("function `g` is defined more than once"), "{err}");
        assert!(err.contains("lines 1 and 3"), "{err}");
        // Distinct names are fine.
        check_str("fn a() -> Int:\n    1\nfn b() -> Int:\n    2\n").expect("distinct names are valid");
        // Methods with the same name on different types are dispatched by receiver,
        // not duplicates — they must still type-check.
        let methods = "type A:\n    A\ntype B:\n    B\nimpl A:\n    fn tag(self) -> Int:\n        1\nimpl B:\n    fn tag(self) -> Int:\n        2\n";
        check_str(methods).expect("same-named methods on different types are not duplicates");
    }

    #[test]
    fn occurs_check_rejects_infinite_types() {
        // Unifying `a` with `List(a)` (the classic omega shape) must be a clear
        // check-time error, not an infinite type silently bound in the subst.
        let omega = "fn omega(x: a) -> a:\n    omega([x])\n";
        let err = check_str(omega).expect_err("infinite type must be rejected");
        assert!(err.contains("infinite type"), "got: {err}");
        // A legitimate generic that nests its argument in a list is fine when
        // the return type grows with it.
        check_str("fn wrap(x: a) -> List(a):\n    [x]\n").expect("wrap is valid");
    }

    #[test]
    fn main_signature_is_validated_at_check_time() {
        // A non-capability `main` parameter is a check-time error (it used to slip
        // through `witchy check` and only fail when capabilities were minted).
        let bad = check_str("fn main(x: Int):\n    print_int(x)\n").unwrap_err();
        assert!(bad.contains("`main` parameter `x` has type `Int`"), "{bad}");
        assert!(bad.contains("host capabilities"), "{bad}");
        // The args parameter must be `List(String)`, not any other list.
        let bad_args = check_str("fn main(args: List(Int)):\n    print_int(0)\n").unwrap_err();
        assert!(bad_args.contains("`List(Int)`"), "{bad_args}");
        // An untyped parameter is flagged too.
        let untyped = check_str("fn main(x):\n    x\n").unwrap_err();
        assert!(untyped.contains("has no type annotation"), "{untyped}");
        // Capabilities (with or without rights) and the args list are all valid.
        check_str("fn main(console: Console, dir: Dir[Read], args: List(String)):\n    print(console, \"ok\")\n")
            .expect("capabilities + args is a valid main");
        // A module without `main` is a library and passes.
        check_str("fn helper() -> Int:\n    5\n").expect("a library is valid");
    }

    #[test]
    fn unknown_stdlib_function_suggests_import() {
        // Calling an unimported stdlib function points at the module to import.
        let err = check_str("fn main(console: Console):\n    print(console, __render(minimum([1], 0)))\n")
            .expect_err("minimum is unimported");
        assert!(err.contains("import ord"), "{err}");
        // A genuine typo (no stdlib match) gets no misleading hint.
        let typo = check_str("fn main(console: Console):\n    frobnicate()\n")
            .expect_err("frobnicate is unknown");
        assert!(!typo.contains("did you forget"), "{typo}");
        assert!(!typo.contains("did you mean"), "{typo}");
        // A near-miss of a stdlib name suggests the correction.
        let near = check_str("fn main(console: Console):\n    let ys = mep([1], fn(x: Int): x)\n    print(console, \"ok\")\n")
            .expect_err("mep is a typo of map");
        assert!(near.contains("did you mean `map`"), "{near}");
    }

    #[test]
    fn accepts_a_well_typed_program() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main(console: Console):
    print(console, __render(double(21)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_string_plus_int() {
        let src = r#"
fn f() -> String:
    ("a" + 1)
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
    print(console, __render(id(5)))
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_over_constrained_type_param() {
        // `a` can't be generic if the body forces it to Int.
        assert!(check_str("fn bad(x: a) -> a { x + 1 }").is_err());
    }

    #[test]
    fn duration_is_a_distinct_type() {
        // Durations combine with durations, scale by an Int, divide to an Int
        // ratio, and compare; mixing with a bare Int under +/- is rejected.
        assert!(check_str("fn f() -> Duration:\n    30s + 1m\n").is_ok());
        assert!(check_str("fn f() -> Duration:\n    2 * 1h\n").is_ok());
        assert!(check_str("fn f() -> Int:\n    1h / 1m\n").is_ok());
        assert!(check_str("fn f() -> Bool:\n    30s > 1m\n").is_ok());
        assert!(check_str("fn f(d: Duration) -> Duration:\n    d + 5s\n").is_ok());
        // A Duration is not an Int.
        assert!(check_str("fn f() -> Duration:\n    30s + 5\n").is_err());
        assert!(check_str("fn f() -> Int:\n    30s\n").is_err());
        assert!(check_str("fn f() -> Duration:\n    30s + true\n").is_err());
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
    print(console, __render(unwrap_int(Wrap(5))))
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
    print(console, __render(unwrap(Wrap(5), 0)))
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
    var d = dict.new()
    for w in words:
        d = dict.insert(d, w, (dict.get_or(d, w, 0) + 1))
    dict.size(d)
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_dict_key_type_mismatch() {
        // The dict's key type is fixed by the first insert (String here), so
        // looking it up with an Int key must fail.
        let src = r#"
fn f() -> Int:
    let d = dict.insert(dict.new(), "a", 1)
    dict.get_or(d, 2, 0)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn string_builtins_type() {
        let src = r#"
fn first_field(row: String) -> String:
    list.at(string.split(row, ","), 0)

fn has(s: String, sub: String) -> Bool:
    string.contains(s, sub)

fn fix(s: String) -> String:
    string.replace(s, "a", "b")
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_split_on_non_string() {
        assert!(check_str("fn f() -> List(String) { string.split(5, \",\") }").is_err());
    }

    #[test]
    fn push_and_concat_are_generic() {
        let src = r#"
fn ints() -> List(Int):
    list.push([1, 2], 3)

fn strs() -> List(String):
    list.concat(["a"], ["b", "c"])
"#;
        assert!(check_str(src).is_ok(), "{:?}", check_str(src));
    }

    #[test]
    fn rejects_push_element_type_mismatch() {
        // Pushing a String onto a List(Int) must fail.
        assert!(check_str("fn f() -> List(Int) { list.push([1, 2], \"x\") }").is_err());
    }

    #[test]
    fn higher_order_and_lambda_type() {
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    print(console, __render(apply(fn(n: Int): (n + 1), 10)))
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
    print(console, __render(apply(fn(n: Int): n, 5)))
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
    Point(x: ((p).x + 1), ..p)
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
    Point(x: "no", ..p)
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
    Point(z: 1, ..p)
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn named_field_record_construction() {
        // Full named construction in any order is accepted (it lowers to the
        // positional constructor); positional construction still works too.
        let ok = "type Point:\n    x: Int\n    y: Int\nfn a() -> Point:\n    Point(y: 2, x: 1)\nfn b() -> Point:\n    Point(3, 4)\n";
        assert!(check_str(ok).is_ok(), "{:?}", check_str(ok));
        // A missing field (no spread to supply it) is rejected.
        let miss = check_str("type Point:\n    x: Int\n    y: Int\nfn a() -> Point:\n    Point(x: 1)\n").unwrap_err();
        assert!(miss.contains("missing field `y`"), "{miss}");
        // An unknown field name is rejected.
        let unknown = check_str("type Point:\n    x: Int\nfn a(p: Point) -> Point:\n    Point(nope: 1, ..p)\n").unwrap_err();
        assert!(unknown.contains("no field `nope`"), "{unknown}");
        // A name that isn't a record type is rejected.
        let notrec = check_str("fn a() -> Int:\n    Nope(x: 1)\n").unwrap_err();
        assert!(notrec.contains("not a record type"), "{notrec}");
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
        [head, ..] -> (head + "!")
"#;
        assert!(check_str(src).is_err());
    }

    #[test]
    fn for_in_binds_element_type() {
        let src = r#"
fn main(console: Console):
    for n in [1, 2, 3]:
        print(console, __render(n))
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
    Ok(string.to_int(s))

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
        Click(x, _) -> __render(x)
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
fn bump(var n: Int):
    n = (n + 1)

fn main():
    let x = 1
    bump(x)
"#;
        let e = check_str(src).unwrap_err();
        assert!(e.contains("var"), "got: {e}");
    }

    #[test]
    fn accepts_inout_argument_that_is_var() {
        let src = r#"
fn bump(var n: Int):
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
fn take(own s: String) -> String:
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
fn take(own s: String) -> String:
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
