//! A type checker for witchy.
//!
//! Annotation-driven checking with light Hindley-Milner-style unification for
//! the bits that aren't annotated (let bindings, match arms). It is deliberately
//! lenient where it lacks information (e.g. an unknown constructor's arguments)
//! so it never rejects a valid program — it tightens as the type system grows.
//!
//! Capability safety is not a special case: `print` has type
//! `(Console, String) -> Nil`, and the only way to obtain a `Console` is to
//! receive one as a parameter — ultimately from `main`. So "this code may
//! perform output" is simply visible in its type, and code that never received
//! the capability cannot type-check a call that needs it.

use std::collections::{HashMap, HashSet};
use std::fmt;

use witchy_syntax::ast::{
    self, Block, Convention, Expr, Function, Item, MatchArm, Module, Pattern, Stmt, UnOp,
};
use witchy_syntax::build_entry::{build_entrypoint, is_build_capability_type};

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

/// The operations a `File` capability permits — the *leaf* of the same hierarchy
/// as `Dir` (authority to one file vs. one subtree, RFC-0012). Mirrors `DirRights`:
/// a `File` carries no path-scope to refine (it is already a leaf), so its only
/// refinement axis is its rights. (`Exec` — folding ambient `Exec` into
/// `File[Exec]` — is a later addition; today a `File` is read/write.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileRights {
    pub read: bool,
    pub write: bool,
}

impl FileRights {
    pub fn full() -> Self {
        FileRights { read: true, write: true }
    }
}

impl fmt::Display for FileRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.read, self.write) {
            (true, true) => write!(f, "File"),
            (true, false) => write!(f, "File[Read]"),
            (false, true) => write!(f, "File[Write]"),
            (false, false) => write!(f, "File[]"),
        }
    }
}

/// Interpret a `File`'s type arguments as its rights (bare `File` is the full set).
fn file_rights(args: &[ast::Type]) -> FileRights {
    if args.is_empty() {
        return FileRights::full();
    }
    let mut r = FileRights { read: false, write: false };
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
    Bytes,
    /// (RFC-0055) The erased message type of the concurrency executor. Opaque and
    /// representationally the universal slot (like a generic type variable, and
    /// like `Bytes` shares `String`'s layout): the `std/chan`/`std/task` executor
    /// buffers, `Step`, and `Slot` are monomorphic over it, while typed channel
    /// endpoints (`Sender(m)`/`Receiver(m)`) erase/unerase at the boundary via the
    /// non-inferable `__erase`/`__unerase` intrinsics, confined to `std/chan`. The
    /// spelling `__Msg` is reserved (double-underscore) so it never collides with a
    /// user `type Msg`.
    Msg,
    Bool,
    Nil,
    Console,
    Clock,
    /// The runtime authority to draw cryptographic randomness (`rand_u64`).
    Rand,
    Env,
    Secret,
    /// The runtime authority to spawn a native subprocess. Right-less (one op):
    /// the executable is named and confined through a `Dir[Read]` argument, so
    /// "you can only execute a file you can read". See rfcs/0004-self-hosted-cli.md.
    Exec,
    Dir(DirRights),
    File(FileRights),
    Net(NetRights),
    Socket,
    Listener,
    /// Build-time capabilities — a parallel set to the runtime caps, granted only
    /// to a rune's `build` entrypoint and enforced in a zero-ambient build
    /// sandbox. Kind-only (the specific tool/host/dir/var is the consumer's grant,
    /// not the type); see rfcs/build-time-execution-plan.md.
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
            Ty::Bytes => write!(f, "Bytes"),
            Ty::Msg => write!(f, "__Msg"),
            Ty::Bool => write!(f, "Bool"),
            Ty::Nil => write!(f, "Nil"),
            Ty::Console => write!(f, "Console"),
            Ty::Clock => write!(f, "Clock"),
            Ty::Rand => write!(f, "Rand"),
            Ty::Env => write!(f, "Env"),
            Ty::Secret => write!(f, "Secret"),
            Ty::Exec => write!(f, "Exec"),
            Ty::Dir(r) => write!(f, "{r}"),
            Ty::File(r) => write!(f, "{r}"),
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
fn at_loc(e: TypeError, line: u32, func: &str, home: &str) -> TypeError {
    if line == 0 {
        return e;
    }
    let where_ = if func.is_empty() {
        format!("line {line}")
    } else {
        format!("`{func}`, line {line}")
    };
    // The location prefix already names the home module, so render home-module
    // type/variant names bare in the body — the spelling the reader wrote — while
    // keeping cross-module qualifiers (BUG-292).
    TypeError {
        message: format!("{where_}: {}", strip_home_qualifiers(&e.message, home)),
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

/// (BUG-230) Reject duplicate top-level declarations in the type/constructor/method
/// namespaces — the same "defined more than once" quality of error the function
/// namespace already gets from [`check_unique_functions`]. Runs pre-lowering (while
/// `impl`/`type` items are still distinct) on the merged module, whose type and
/// constructor names are already module-qualified, so a genuine cross-module name
/// is distinct and only same-module duplicates (a typo or copy-paste) collide.
fn check_unique_declarations(module: &Module) -> Result<(), TypeError> {
    let bare = |n: &str| n.rsplit('.').next().unwrap_or(n).to_string();
    let check_type_params = |context: String, params: &[String]| -> Result<(), TypeError> {
        let mut seen: HashSet<&str> = HashSet::new();
        for param in params {
            if !seen.insert(param.as_str()) {
                return terr(format!(
                    "type parameter `{param}` is declared more than once in {context}; \
                     type parameter names must be unique"
                ));
            }
        }
        Ok(())
    };
    let mut types: HashMap<String, &witchy_syntax::ast::TypeDef> = HashMap::new();
    // Constructor name -> its owning type, so a cross-type duplicate names both.
    let mut ctors: HashMap<String, String> = HashMap::new();
    for item in &module.items {
        let Item::Type(t) = item else { continue };
        check_type_params(format!("type `{}`", bare(&t.name)), &t.params)?;
        if let Some(prev) = types.insert(t.name.clone(), t) {
            // A structurally-IDENTICAL re-declaration is a harmless shadow — a user
            // module may redefine a prelude-injected type (`Result`/`Option`) with
            // the same shape (examples/try teaches exactly this). Only a CONFLICTING
            // redefinition (a different shape under the same name) is an error; the
            // identical one is skipped so its constructors aren't double-counted.
            if prev.params == t.params && prev.variants == t.variants {
                continue;
            }
            return terr(format!(
                "type `{}` is defined more than once; top-level type names must be unique",
                bare(&t.name)
            ));
        }
        for v in &t.variants {
            if let Some(prev) = ctors.insert(v.name.clone(), t.name.clone()) {
                let (a, b) = (bare(&prev), bare(&t.name));
                let where_ = if a == b {
                    format!("in type `{a}`")
                } else {
                    format!("in types `{a}` and `{b}`")
                };
                return terr(format!(
                    "constructor `{}` is defined more than once ({where_}); \
                     constructor names must be unique",
                    bare(&v.name)
                ));
            }
        }
    }
    // Methods: no two methods with the same name in one `impl` block or `trait`,
    // and no duplicate inherent method (same receiver type, same name) across the
    // inherent `impl` blocks of a type.
    let mut inherent: HashSet<(String, String)> = HashSet::new();
    let mut trait_impls: HashSet<(String, String)> = HashSet::new();
    for item in &module.items {
        match item {
            Item::Impl(im) => {
                if let Some(trait_name) = &im.trait_name {
                    if !trait_impls.insert((trait_name.clone(), im.type_name.clone())) {
                        return terr(format!(
                            "impl `{}` for `{}` is defined more than once; \
                             trait impl heads must be unique",
                            bare(trait_name),
                            bare(&im.type_name)
                        ));
                    }
                }
                let mut here: HashSet<String> = HashSet::new();
                for m in &im.methods {
                    let name = bare(&m.name);
                    if !here.insert(name.clone()) {
                        return terr(format!(
                            "method `{name}` is defined more than once in `impl {}`; \
                             method names must be unique within an impl",
                            im.trait_name.as_deref().map_or_else(
                                || im.type_name.clone(),
                                |tr| format!("{tr} for {}", im.type_name)
                            )
                        ));
                    }
                    // Inherent (trait-free) methods share one namespace per type.
                    if im.trait_name.is_none() && !inherent.insert((im.type_name.clone(), name.clone())) {
                        return terr(format!(
                            "inherent method `{name}` is defined more than once on `{}`; \
                             method names must be unique per receiver type",
                            bare(&im.type_name)
                        ));
                    }
                }
            }
            Item::Trait(tr) => {
                check_type_params(format!("trait `{}`", bare(&tr.name)), &tr.typarams)?;
                let mut here: HashSet<&str> = HashSet::new();
                for m in &tr.methods {
                    if !here.insert(m.name.as_str()) {
                        return terr(format!(
                            "method `{}` is declared more than once in `trait {}`; \
                             trait method names must be unique",
                            m.name, tr.name
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Reject duplicate parameter names in every source and lowered callable. The
/// checker scopes parameters in a name map, so accepting duplicates would make a
/// later parameter silently hide an earlier one and would also make keyword labels
/// ambiguous at direct call sites.
fn check_unique_parameters(module: &Module) -> Result<(), TypeError> {
    fn bare(name: &str) -> &str {
        name.rsplit('.').next().unwrap_or(name)
    }

    fn check_params(context: String, params: &[ast::Param]) -> Result<(), TypeError> {
        let mut seen: HashSet<&str> = HashSet::new();
        for param in params {
            if !seen.insert(param.name.as_str()) {
                return terr(format!(
                    "parameter `{}` is declared more than once in {context}; \
                     parameter names must be unique",
                    param.name
                ));
            }
        }
        for param in params {
            if let Some(default) = &param.default {
                check_expr(default)?;
            }
        }
        Ok(())
    }

    fn check_function(context: &str, f: &Function) -> Result<(), TypeError> {
        check_params(format!("{context} `{}`", bare(&f.name)), &f.params)?;
        check_block(&f.body)
    }

    fn check_block(block: &Block) -> Result<(), TypeError> {
        for stmt in &block.stmts {
            check_stmt(stmt)?;
        }
        Ok(())
    }

    fn check_stmt(stmt: &Stmt) -> Result<(), TypeError> {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => check_expr(value),
            Stmt::Return(Some(value)) => check_expr(value),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => Ok(()),
        }
    }

    fn check_expr(expr: &Expr) -> Result<(), TypeError> {
        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_)
            | Expr::TaggedLit { .. } => Ok(()),
            Expr::List(values) | Expr::Tuple(values) => {
                for value in values {
                    check_expr(value)?;
                }
                Ok(())
            }
            Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
                for arg in args {
                    check_expr(arg)?;
                }
                Ok(())
            }
            Expr::LabeledCall { args, .. } => {
                for (_, arg) in args {
                    check_expr(arg)?;
                }
                Ok(())
            }
            Expr::MethodCall { receiver, args, .. } => {
                check_expr(receiver)?;
                for arg in args {
                    check_expr(arg)?;
                }
                Ok(())
            }
            Expr::Apply { func, args } => {
                check_expr(func)?;
                for arg in args {
                    check_expr(arg)?;
                }
                Ok(())
            }
            Expr::Unary { expr, .. }
            | Expr::Field { base: expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. } => check_expr(expr),
            Expr::Lambda { params, body, .. } => {
                check_params("lambda".to_string(), params)?;
                check_block(body)
            }
            Expr::RecordUpdate { base, fields } => {
                check_expr(base)?;
                for (_, value) in fields {
                    check_expr(value)?;
                }
                Ok(())
            }
            Expr::Record { fields, spread, .. } => {
                for (_, value) in fields {
                    check_expr(value)?;
                }
                if let Some(base) = spread {
                    check_expr(base)?;
                }
                Ok(())
            }
            Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } => {
                check_expr(lhs)?;
                check_expr(rhs)
            }
            Expr::If { cond, then_block, else_block } => {
                check_expr(cond)?;
                check_block(then_block)?;
                if let Some(block) = else_block {
                    check_block(block)?;
                }
                Ok(())
            }
            Expr::Match { scrutinee, arms } => {
                check_expr(scrutinee)?;
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        check_expr(guard)?;
                    }
                    check_expr(&arm.body)?;
                }
                Ok(())
            }
            Expr::Block(block) => check_block(block),
            Expr::While { cond, body } => {
                check_expr(cond)?;
                check_block(body)
            }
            Expr::For { iter, body, .. } => {
                check_expr(iter)?;
                check_block(body)
            }
            Expr::Index { base, index } => {
                check_expr(base)?;
                check_expr(index)
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                check_expr(scrutinee)?;
                check_block(body)
            }
        }
    }

    for item in &module.items {
        match item {
            Item::Function(f) => check_function("function", f)?,
            Item::Impl(im) => {
                for method in &im.methods {
                    check_function("method", method)?;
                }
            }
            Item::Trait(tr) => {
                for method in &tr.methods {
                    check_params(format!("trait method `{}`", method.name), &method.params)?;
                    if let Some(default) = &method.default {
                        check_block(default)?;
                    }
                }
            }
            Item::Const { value, .. } => check_expr(value)?,
            Item::Comptime(block) => check_block(block)?,
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
    Ok(())
}

/// (RFC-0064 Check 1) Enforce RFC-0043's row-3 rule: a function with any `var`
/// parameter must be EITHER a procedure channel (`is_var_procedure` — returns
/// `Nil`/nothing) OR a mutator receiver (`is_mutator` — first parameter, returning
/// that parameter's type). Every other `var` shape carries the abolished
/// *combined* write-back+return semantics and is a compile error:
///   (a) a `var` in a NON-first position with a self-typed return, and
///   (b) a `var` FIRST parameter with an UNRELATED (non-`Nil`, non-receiver)
///       return — the interpreter-only shape the WASM backend rejects, so this
///       also closes a parity divergence.
/// Runs before lowering for source-quality diagnostics on free functions, and
/// again after trait/impl lowering so method bodies cannot bypass the same
/// declaration-shape contract.
fn check_var_conventions(module: &Module) -> Result<(), TypeError> {
    for item in &module.items {
        if let Item::Function(f) = item {
            let has_var = f.params.iter().any(|p| p.convention == Convention::Var);
            if has_var && !f.is_mutator() && !f.is_var_procedure() {
                let bare = f.name.rsplit('.').next().unwrap_or(&f.name);
                return terr(format!(
                    "`{bare}`: a `var` parameter must be a write-back channel (return `Nil`) or a \
                     mutator receiver (first parameter, returning its type); split the function or \
                     return a tuple"
                ));
            }
        }
    }
    Ok(())
}

/// The type names the checker knows without a declaration: primitives, host
/// capabilities, and the built-in generics. Mirrors the named arms of
/// `to_ty_generic` plus the opaque generics the checker itself produces
/// (`Option`/`Result`/`Dict`). Any other named type must be declared (a `type`
/// or be a lowercase generic parameter.
const BUILTIN_TYPE_NAMES: &[&str] = &[
    "Int", "Float", "Duration", "String", "Bytes", "__Msg", "Bool", "Nil", "Console", "Clock", "Rand", "Env", "Secret",
    "SecretStore", "Dir", "File", "Net", "Exec", "Socket", "Listener", "List", "Option", "Result",
    "Dict", "BuildOut", "BuildRead", "BuildEnv", "BuildNet", "BuildExec",
];

const AMBIENT_STD_TYPE_NAMES: &[&str] = &["Ordering", "Set", "Iter"];
const AMBIENT_TRAIT_NAMES: &[&str] = &["PartialEq", "Eq", "PartialOrd", "Ord"];

fn builtin_type_arity(name: &str) -> Option<usize> {
    match name {
        "List" | "Option" | "Set" | "Iter" => Some(1),
        "Result" | "Dict" => Some(2),
        "Int" | "Float" | "Duration" | "String" | "Bytes" | "__Msg" | "Bool" | "Nil"
        | "Console" | "Clock" | "Rand" | "Env" | "Secret" | "SecretStore" | "Exec"
        | "Socket" | "Listener" | "BuildOut" | "BuildRead" | "BuildEnv" | "BuildNet"
        | "BuildExec" | "Ordering" => Some(0),
        _ => None,
    }
}

fn is_synthetic_type_name(name: &str) -> bool {
    name.strip_prefix("__anon").is_some_and(|n| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    }) || name.strip_prefix("Tuple").is_some_and(|n| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    })
}

fn tuple_synthetic_arity(name: &str) -> Option<usize> {
    name.strip_prefix("Tuple")
        .and_then(|n| (!n.is_empty()).then_some(n))
        .and_then(|n| n.parse::<usize>().ok())
}

/// Validate that every named type in `t` is known — a builtin, a declared type,
/// or a lowercase generic parameter — so a typo like `fn f(x: Flarb)` is a clear
/// "unknown type" error rather than an opaque type that mis-unifies later.
/// Whether a declared type carries a `frozen` qualifier at the top (`frozen T`,
/// possibly under other stacked qualifiers) — used to enforce that a `frozen`
/// (deeply immutable) value is never declared mutable (RFC-0025).
fn is_frozen_type(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(ast::TypeQual::Frozen, _) => true,
        ast::Type::Qualified(_, inner) => is_frozen_type(inner),
        _ => false,
    }
}

/// Whether a declared type carries a `local unique` qualifier at the top — used to
/// reject it in escaping positions (a return type), per RFC-0026.
fn is_local_unique_type(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(ast::TypeQual::LocalUnique, _) => true,
        ast::Type::Qualified(_, inner) => is_local_unique_type(inner),
        _ => false,
    }
}

/// The rights markers each capability kind admits inside `[...]`. A marker
/// outside this vocabulary is a typo (`Dir[Reed]`) or a rejected right
/// (`Net[Tls]`), and is rejected at check time rather than silently dropped —
/// keeping the declared authority shape faithful to the source (BUG-154). The
/// single source of truth the rights-interpreting functions
/// (`dir_rights`/`file_rights`/`net_rights`) match against.
fn cap_markers(cap: &str) -> &'static [&'static str] {
    match cap {
        "Dir" | "File" => &["Read", "Write"],
        "Net" => &["Connect", "Listen", "Tcp", "Udp", "Uds"],
        _ => &[],
    }
}

/// Reject any bracket marker on a `Dir`/`File`/`Net` capability that is not in its
/// [`cap_markers`] vocabulary. An empty list (`Dir[]`) is legal (no rights); each
/// marker must be a bare, argument-less name from the allowed set.
fn validate_cap_markers(cap: &str, args: &[ast::Type]) -> Result<(), TypeError> {
    let allowed = cap_markers(cap);
    for a in args {
        let ok = matches!(a, ast::Type::Named(m, margs)
            if margs.is_empty() && allowed.contains(&m.as_str()));
        if !ok {
            let found = match a {
                ast::Type::Named(m, _) => m.clone(),
                _ => format!("{a:?}"),
            };
            return terr(format!(
                "unknown `{cap}` right `{found}` — `{cap}` admits {}",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

fn type_param_names<'a>(fields: impl Iterator<Item = &'a ast::Type>) -> Vec<String> {
    let mut names = Vec::new();
    for field in fields {
        collect_type_params(field, &mut names);
    }
    names
}

fn type_def_arity(t: &ast::TypeDef) -> usize {
    if !t.params.is_empty() {
        t.params.len()
    } else {
        type_param_names(t.variants.iter().flat_map(|v| v.fields.iter())).len()
    }
}

fn validate_type(
    t: &ast::Type,
    known: &HashSet<&str>,
    arities: &HashMap<&str, usize>,
) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(_, inner) => validate_type(inner, known, arities),
        ast::Type::Tuple(ts) => ts.iter().try_for_each(|x| validate_type(x, known, arities)),
        ast::Type::Fn(params, ret) => {
            params.iter().try_for_each(|p| validate_type(p, known, arities))?;
            validate_type(ret, known, arities)
        }
        ast::Type::Named(n, args) => {
            // `Dir`/`File`/`Net` carry capability *rights* markers (`Dir[Read]`,
            // `Net[Connect]`) in their arguments, not types. Validate the marker
            // vocabulary here (BUG-154) so a typo (`Dir[Reed]`, `Net[Conect]`) or
            // a rejected right (`Net[Tls]` — TLS is an endpoint scheme, not a Net
            // right; RFC-0009) is a clear error instead of a silently-normalized
            // capability whose authority shape no longer matches the source.
            if n == "Dir" || n == "File" || n == "Net" {
                return validate_cap_markers(n, args);
            }
            if known.contains(n.as_str()) || is_synthetic_type_name(n) {
                if let Some(expected) = arities
                    .get(n.as_str())
                    .copied()
                    .or_else(|| tuple_synthetic_arity(n))
                {
                    let got = args.len();
                    if got != expected {
                        return terr(format!(
                            "type `{n}` expects {expected} type argument(s) but got {got}"
                        ));
                    }
                }
                args.iter().try_for_each(|a| validate_type(a, known, arities))
            } else if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) && !n.contains('.') {
                // A lowercase, argument-less name is a generic type parameter.
                Ok(())
            } else {
                terr(format!("unknown type `{n}`"))
            }
        }
    }
}

/// Reject references to undeclared types in function signatures. The
/// set of known names is the builtins plus every `type` declared in the
/// module; lowercase argument-less names are generic parameters.
fn check_type_names(module: &Module) -> Result<(), TypeError> {
    let mut known: HashSet<&str> = BUILTIN_TYPE_NAMES.iter().copied().collect();
    let mut arities: HashMap<&str, usize> = BUILTIN_TYPE_NAMES
        .iter()
        .chain(AMBIENT_STD_TYPE_NAMES.iter())
        .filter_map(|name| builtin_type_arity(name).map(|arity| (*name, arity)))
        .collect();
    let mut packed_names: HashSet<&str> = HashSet::new();
    // (RFC-0005) User `type`/`capability` declarations, so `carries_externref_cap`
    // can resolve whether a `Named` type transitively holds a migrated capability.
    let mut type_defs: HashMap<&str, &ast::TypeDef> = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            known.insert(t.name.as_str());
            arities.insert(t.name.as_str(), type_def_arity(t));
            type_defs.insert(t.name.as_str(), t);
            if t.packed {
                packed_names.insert(t.name.as_str());
            }
        }
    }
    for item in &module.items {
        let in_ctx = |e: TypeError, ctx: &str| TypeError {
            message: format!("in `{}`: {}", ctx.rsplit('.').next().unwrap_or(ctx), e.message),
        };
        fn validate_block_types(
            block: &Block,
            known: &HashSet<&str>,
            arities: &HashMap<&str, usize>,
            ctx: &str,
            in_ctx: &impl Fn(TypeError, &str) -> TypeError,
        ) -> Result<(), TypeError> {
            if let Some(region) = &block.region {
                if let Some(ty) = &region.ty {
                    validate_type(ty, known, arities).map_err(|e| in_ctx(e, ctx))?;
                }
            }
            for stmt in &block.stmts {
                validate_stmt_types(stmt, known, arities, ctx, in_ctx)?;
            }
            Ok(())
        }
        fn validate_stmt_types(
            stmt: &Stmt,
            known: &HashSet<&str>,
            arities: &HashMap<&str, usize>,
            ctx: &str,
            in_ctx: &impl Fn(TypeError, &str) -> TypeError,
        ) -> Result<(), TypeError> {
            match stmt {
                Stmt::Let { ty, value, .. } => {
                    if let Some(ty) = ty {
                        validate_type(ty, known, arities).map_err(|e| in_ctx(e, ctx))?;
                    }
                    validate_expr_types(value, known, arities, ctx, in_ctx)
                }
                Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Yield(value)
                | Stmt::Expr(value) => validate_expr_types(value, known, arities, ctx, in_ctx),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => Ok(()),
            }
        }
        fn validate_expr_types(
            expr: &Expr,
            known: &HashSet<&str>,
            arities: &HashMap<&str, usize>,
            ctx: &str,
            in_ctx: &impl Fn(TypeError, &str) -> TypeError,
        ) -> Result<(), TypeError> {
            match expr {
                Expr::Int(_)
                | Expr::Float(_)
                | Expr::Duration(_)
                | Expr::Str(_)
                | Expr::Bool(_)
                | Expr::Var(_)
                | Expr::TaggedLit { .. } => Ok(()),
                Expr::List(values) | Expr::Tuple(values) => {
                    for value in values {
                        validate_expr_types(value, known, arities, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
                    for arg in args {
                        validate_expr_types(arg, known, arities, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::LabeledCall { args, .. } => {
                    for (_, arg) in args {
                        validate_expr_types(arg, known, arities, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::MethodCall { receiver, args, .. } => {
                    validate_expr_types(receiver, known, arities, ctx, in_ctx)?;
                    for arg in args {
                        validate_expr_types(arg, known, arities, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Apply { func, args } => {
                    validate_expr_types(func, known, arities, ctx, in_ctx)?;
                    for arg in args {
                        validate_expr_types(arg, known, arities, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Unary { expr, .. }
                | Expr::Field { base: expr, .. }
                | Expr::Try(expr) => validate_expr_types(expr, known, arities, ctx, in_ctx),
                Expr::As { expr, ty } => {
                    validate_expr_types(expr, known, arities, ctx, in_ctx)?;
                    validate_type(ty, known, arities).map_err(|e| in_ctx(e, ctx))
                }
                Expr::Lambda { params, body, ret } => {
                    for param in params {
                        if let Some(ty) = &param.ty {
                            validate_type(ty, known, arities).map_err(|e| in_ctx(e, ctx))?;
                        }
                    }
                    if let Some(ret) = ret {
                        validate_type(ret, known, arities).map_err(|e| in_ctx(e, ctx))?;
                    }
                    validate_block_types(body, known, arities, ctx, in_ctx)
                }
                Expr::RecordUpdate { base, fields } => {
                    validate_expr_types(base, known, arities, ctx, in_ctx)?;
                    for (_, value) in fields {
                        validate_expr_types(value, known, arities, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Record { fields, spread, .. } => {
                    for (_, value) in fields {
                        validate_expr_types(value, known, arities, ctx, in_ctx)?;
                    }
                    if let Some(base) = spread {
                        validate_expr_types(base, known, arities, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } => {
                    validate_expr_types(lhs, known, arities, ctx, in_ctx)?;
                    validate_expr_types(rhs, known, arities, ctx, in_ctx)
                }
                Expr::If { cond, then_block, else_block } => {
                    validate_expr_types(cond, known, arities, ctx, in_ctx)?;
                    validate_block_types(then_block, known, arities, ctx, in_ctx)?;
                    if let Some(block) = else_block {
                        validate_block_types(block, known, arities, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Match { scrutinee, arms } => {
                    validate_expr_types(scrutinee, known, arities, ctx, in_ctx)?;
                    for arm in arms {
                        if let Some(guard) = &arm.guard {
                            validate_expr_types(guard, known, arities, ctx, in_ctx)?;
                        }
                        validate_expr_types(&arm.body, known, arities, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Block(block) => validate_block_types(block, known, arities, ctx, in_ctx),
                Expr::While { cond, body } => {
                    validate_expr_types(cond, known, arities, ctx, in_ctx)?;
                    validate_block_types(body, known, arities, ctx, in_ctx)
                }
                Expr::For { iter, body, .. } => {
                    validate_expr_types(iter, known, arities, ctx, in_ctx)?;
                    validate_block_types(body, known, arities, ctx, in_ctx)
                }
                Expr::Index { base, index } => {
                    validate_expr_types(base, known, arities, ctx, in_ctx)?;
                    validate_expr_types(index, known, arities, ctx, in_ctx)
                }
                Expr::WhileLet { scrutinee, body, .. } => {
                    validate_expr_types(scrutinee, known, arities, ctx, in_ctx)?;
                    validate_block_types(body, known, arities, ctx, in_ctx)
                }
            }
        }
        match item {
            Item::Function(f) => {
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        validate_type(t, &known, &arities).map_err(|e| in_ctx(e, &f.name))?;
                        reject_packed_list_boundary(t, &packed_names, &f.name, "a parameter")?;
                        reject_cap_slot_boundary(t, &type_defs, &f.name, "a parameter")?;
                    }
                }
                if let Some(t) = &f.ret {
                    validate_type(t, &known, &arities).map_err(|e| in_ctx(e, &f.name))?;
                    reject_packed_list_boundary(t, &packed_names, &f.name, "a return type")?;
                    reject_cap_slot_boundary(t, &type_defs, &f.name, "a return type")?;
                    // (RFC-0026) `local unique` is valid only WITHIN the call — it may
                    // not escape — so it cannot be a return type (a return escapes).
                    if is_local_unique_type(t) {
                        return Err(TypeError {
                            message: format!(
                                "`{}`: a `local unique` value cannot escape, so it cannot be a return type — use `unique` (returnable) or drop `local`",
                                f.name
                            ),
                        });
                    }
                }
                validate_block_types(&f.body, &known, &arities, &f.name, &in_ctx)?;
            }
            Item::Trait(tr) => {
                let mut trait_known = known.clone();
                let mut trait_arities = arities.clone();
                trait_known.insert("Self");
                trait_arities.insert("Self", 0);
                for method in &tr.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            validate_type(ty, &trait_known, &trait_arities).map_err(|e| in_ctx(e, &tr.name))?;
                        }
                    }
                    if let Some(ret) = &method.ret {
                        validate_type(ret, &trait_known, &trait_arities).map_err(|e| in_ctx(e, &tr.name))?;
                    }
                    if let Some(default) = &method.default {
                        validate_block_types(default, &trait_known, &trait_arities, &tr.name, &in_ctx)?;
                    }
                }
            }
            Item::Impl(im) => {
                let target = ast::Type::Named(im.type_name.clone(), im.target_args.clone());
                validate_type(&target, &known, &arities).map_err(|e| in_ctx(e, &im.type_name))?;
                for arg in &im.trait_args {
                    validate_type(arg, &known, &arities).map_err(|e| in_ctx(e, &im.type_name))?;
                }
                for (_, _, trait_args) in &im.bounds {
                    for arg in trait_args {
                        validate_type(arg, &known, &arities).map_err(|e| in_ctx(e, &im.type_name))?;
                    }
                }
                let mut method_known = known.clone();
                let mut method_arities = arities.clone();
                method_known.insert("Self");
                method_arities.insert("Self", 0);
                for method in &im.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            validate_type(ty, &method_known, &method_arities).map_err(|e| in_ctx(e, &method.name))?;
                        }
                    }
                    if let Some(ret) = &method.ret {
                        validate_type(ret, &method_known, &method_arities).map_err(|e| in_ctx(e, &method.name))?;
                    }
                    for (_, _, trait_args) in &method.bounds {
                        for arg in trait_args {
                            validate_type(arg, &method_known, &method_arities).map_err(|e| in_ctx(e, &method.name))?;
                        }
                    }
                    validate_block_types(&method.body, &method_known, &method_arities, &method.name, &in_ctx)?;
                }
            }
            // A type's variant field types must also be known. The type's own
            // name (and any sibling type) is already in `known`, so recursive and
            // mutually-recursive types check out; lowercase fields are its params.
            Item::Type(t) => {
                for variant in &t.variants {
                    for field in &variant.fields {
                        validate_type(field, &known, &arities).map_err(|e| in_ctx(e, &t.name))?;
                        reject_packed_list_boundary(field, &packed_names, &t.name, "a field")?;
                        reject_cap_slot_boundary(field, &type_defs, &t.name, "a field")?;
                    }
                }
                // (RFC-0027) A `packed` type's every field must be packable — a
                // scalar (Int/Float/Bool/Duration) or another `packed` type — so the
                // inline layout has a fixed, statically-known size. A variable-size
                // field (String, List, a non-packed record, …) is a check-time error
                // naming the offending field.
                if t.packed {
                    for variant in &t.variants {
                        for (i, field) in variant.fields.iter().enumerate() {
                            if !is_packable_type(field, &packed_names) {
                                let fname = variant
                                    .field_names
                                    .get(i)
                                    .map(String::as_str)
                                    .unwrap_or("(positional field)");
                                return Err(TypeError {
                                    message: format!(
                                        "`packed` type `{}` has a non-packable field `{}`: \
                                         a packed type's fields must be scalars \
                                         (Int/Float/Bool/Duration) or other `packed` types",
                                        t.name, fname
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            Item::Const { value, .. } => {
                validate_expr_types(value, &known, &arities, "<const>", &in_ctx)?;
            }
            Item::Comptime(block) => {
                validate_block_types(block, &known, &arities, "comptime", &in_ctx)?;
            }
            Item::TypeAlias { ty, name } => {
                validate_type(ty, &known, &arities).map_err(|e| in_ctx(e, name))?;
            }
        }
    }
    Ok(())
}

/// Reject trait names that do not resolve to a declared trait before trait/impl
/// lowering turns those names into dispatch strings. A misspelled trait in a
/// bound or impl head should not become an inert contract.
fn check_trait_names(module: &Module) -> Result<(), TypeError> {
    fn bare(name: &str) -> &str {
        name.rsplit('.').next().unwrap_or(name)
    }

    let mut known: HashSet<&str> = HashSet::new();
    known.extend(AMBIENT_TRAIT_NAMES.iter().copied());
    for item in &module.items {
        if let Item::Trait(tr) = item {
            known.insert(tr.name.as_str());
            known.insert(bare(&tr.name));
        }
    }

    let known_trait = |name: &str| known.contains(name) || known.contains(bare(name));
    let unknown = |trait_name: &str, context: String| -> Result<(), TypeError> {
        if known_trait(trait_name) {
            Ok(())
        } else {
            terr(format!("unknown trait `{}` in {context}", bare(trait_name)))
        }
    };

    for item in &module.items {
        match item {
            Item::Trait(tr) => {
                for supertrait in &tr.supertraits {
                    unknown(
                        supertrait,
                        format!("trait `{}` supertrait list", bare(&tr.name)),
                    )?;
                }
            }
            Item::Impl(im) => {
                if let Some(trait_name) = &im.trait_name {
                    unknown(
                        trait_name,
                        format!("impl head for `{}`", bare(&im.type_name)),
                    )?;
                }
                for (_, trait_name, _) in &im.bounds {
                    unknown(
                        trait_name,
                        format!("impl `{}` where clause", bare(&im.type_name)),
                    )?;
                }
                for method in &im.methods {
                    for (_, trait_name, _) in &method.bounds {
                        unknown(
                            trait_name,
                            format!("method `{}` where clause", bare(&method.name)),
                        )?;
                    }
                }
            }
            Item::Function(f) => {
                for (var, trait_name, _) in &f.bounds {
                    let bound_kind = if var.starts_with("impltrait_") {
                        "impl-trait parameter"
                    } else {
                        "where clause"
                    };
                    unknown(
                        trait_name,
                        format!("{bound_kind} of function `{}`", bare(&f.name)),
                    )?;
                }
            }
            Item::Type(_) | Item::Const { .. } | Item::Comptime(_) | Item::TypeAlias { .. } => {}
        }
    }
    Ok(())
}

/// (RFC-0027) Whether `t` is a packable field type for a `packed` layout: a
/// statically-fixed-size scalar (`Int`/`Float`/`Bool`/`Duration`) or another
/// `packed` type. Variable-size types (`String`, `List`, non-packed records, sum
/// types with differing payloads) are not packable.
fn is_packable_type(t: &ast::Type, packed_names: &HashSet<&str>) -> bool {
    matches!(t, ast::Type::Named(n, args)
        if args.is_empty()
            && (matches!(n.as_str(), "Int" | "Float" | "Bool" | "Duration")
                || packed_names.contains(n.as_str())))
}

/// (RFC-0027 declared `packed`) Reject `t` if it contains a `List(P)` where `P` is
/// declared `packed`, in a boundary position (parameter / return / field). A
/// `packed` type's flat inline layout is a CONFINED LOCAL buffer — it has no
/// cross-function or stored-field ABI (that is the deep host-layout change the
/// confined inference deliberately sidesteps), so a `List` of a packed type may
/// only be built and read within one function. This keeps the layout whole-program
/// consistent: every site that handles a packed list either packs it (a confined
/// local) or is a clean compile error here, never a silent boxed fall-back.
fn reject_packed_list_boundary(
    t: &ast::Type,
    packed_names: &HashSet<&str>,
    ctx: &str,
    position: &str,
) -> Result<(), TypeError> {
    if let Some(p) = packed_list_in_type(t, packed_names) {
        return Err(TypeError {
            message: format!(
                "`{ctx}`: a `List({p})` cannot appear in {position} — `{p}` is declared `packed`, so its \
                 list is a confined local buffer with no cross-function or stored layout. Build and read a \
                 `packed` list within one function, or drop `packed` from `{p}` to use the uniform boxed layout"
            ),
        });
    }
    Ok(())
}

/// The name of a declared-`packed` type `P` if `t` contains a `List(P)` anywhere —
/// directly, nested in another list/tuple/function type, or under an ownership
/// qualifier (`frozen`/`unique`). `None` if no packed-typed list appears.
fn packed_list_in_type(t: &ast::Type, packed_names: &HashSet<&str>) -> Option<String> {
    match t {
        ast::Type::Named(n, args) => {
            if n == "List" {
                if let Some(ast::Type::Named(en, eargs)) = args.first().map(ast::Type::unqualified) {
                    if eargs.is_empty() && packed_names.contains(en.as_str()) {
                        return Some(en.clone());
                    }
                }
            }
            args.iter().find_map(|a| packed_list_in_type(a, packed_names))
        }
        ast::Type::Tuple(items) => items.iter().find_map(|a| packed_list_in_type(a, packed_names)),
        ast::Type::Fn(args, ret) => args
            .iter()
            .find_map(|a| packed_list_in_type(a, packed_names))
            .or_else(|| packed_list_in_type(ret, packed_names)),
        ast::Type::Qualified(_, inner) => packed_list_in_type(inner, packed_names),
    }
}

/// (RFC-0005) Names of the capabilities represented as an unforgeable `externref`
/// on the compiled backend at the CURRENT migration stage — the caps with NO boxed
/// i64-slot representation. Stage 2 migrates `File` (the proving capability); the
/// remaining handle-bearing caps (`Dir`/`Net`/`Secret`/`SecretStore`/`Exec`/
/// `Socket`/`Listener`) keep their i32-handle representation until their own stage,
/// so they may still (for now) cross a slot — e.g. `std/secretstore.get` returns
/// `Option(Secret)`. `Console`/`Clock`/`Rand`/`Env` are zero-representation (no
/// runtime handle) and never migrate. Widen this set as each capability migrates.
fn is_externref_cap(name: &str) -> bool {
    name == "File"
}

/// (RFC-0005 §3) The `carries_cap` classification, scoped to the migrated
/// `externref` subset (`is_externref_cap`): the name of the first such capability
/// that `t` transitively carries, or `None`. Recurses through user `type`/
/// `capability` declarations (`defs`) with a cycle guard (`seen`), and through
/// tuples, function types, and type arguments. These are exactly the caps that
/// have no i64 bit-pattern, so they cannot round-trip the universal slot.
fn carries_externref_cap(
    t: &ast::Type,
    defs: &HashMap<&str, &ast::TypeDef>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    match t {
        ast::Type::Qualified(_, inner) => carries_externref_cap(inner, defs, seen),
        ast::Type::Tuple(items) => items.iter().find_map(|a| carries_externref_cap(a, defs, seen)),
        ast::Type::Fn(args, ret) => args
            .iter()
            .find_map(|a| carries_externref_cap(a, defs, seen))
            .or_else(|| carries_externref_cap(ret, defs, seen)),
        ast::Type::Named(n, args) => {
            if is_externref_cap(n) {
                return Some(n.clone());
            }
            if let Some(hit) = args.iter().find_map(|a| carries_externref_cap(a, defs, seen)) {
                return Some(hit);
            }
            // A user `type`/`capability`: scan its variants' field types. `seen`
            // guards recursive/mutually-recursive declarations and is kept
            // monotonic (a shared declaration is only worth scanning once).
            if let Some(def) = defs.get(n.as_str()) {
                if seen.insert(n.clone()) {
                    return def
                        .variants
                        .iter()
                        .flat_map(|v| v.fields.iter())
                        .find_map(|f| carries_externref_cap(f, defs, seen));
                }
            }
            None
        }
    }
}

/// (RFC-0005 §4.4/§7) Reject `t` when it would carry a migrated externref capability
/// through a representation the current lowering cannot preserve.
///
/// A bare capability parameter/return is fine: it stays an `externref`. Slot-boxed
/// containers (`Option`/`Result`/`List`/`Dict`) are impossible because an externref has
/// no i64 bit-pattern. Cap-carrying tuples/user records are also rejected until the
/// planned GC-struct aggregate stage lands; otherwise lowering would still send their
/// fields through `$mkN`/`ToSlot`.
fn reject_cap_slot_boundary(
    t: &ast::Type,
    defs: &HashMap<&str, &ast::TypeDef>,
    ctx: &str,
    position: &str,
) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(_, inner) => reject_cap_slot_boundary(inner, defs, ctx, position),
        ast::Type::Tuple(items) => {
            if let Some(cap) = items.iter().find_map(|a| carries_externref_cap(a, defs, &mut HashSet::new())) {
                return Err(TypeError {
                    message: format!(
                        "`{ctx}`: a `{cap}` capability cannot be held in a tuple in {position} until \
                         RFC-0005's GC-struct aggregate lowering lands — pass it directly"
                    ),
                });
            }
            items.iter().try_for_each(|a| reject_cap_slot_boundary(a, defs, ctx, position))
        }
        ast::Type::Fn(args, ret) => {
            args.iter().try_for_each(|a| reject_cap_slot_boundary(a, defs, ctx, position))?;
            reject_cap_slot_boundary(ret, defs, ctx, position)
        }
        ast::Type::Named(n, args) => {
            if matches!(n.as_str(), "Option" | "Result" | "List" | "Dict") {
                for a in args {
                    if let Some(cap) = carries_externref_cap(a, defs, &mut HashSet::new()) {
                        return Err(TypeError {
                            message: format!(
                                "`{ctx}`: a `{cap}` capability cannot be wrapped in `{n}` in {position} — \
                                 it is an unforgeable reference with no boxed representation; pass it directly"
                            ),
                        });
                    }
                }
            } else if !is_externref_cap(n) {
                if let Some(cap) = carries_externref_cap(t, defs, &mut HashSet::new()) {
                    return Err(TypeError {
                        message: format!(
                            "`{ctx}`: `{n}` carries a `{cap}` capability in {position}, but cap-carrying \
                             aggregates require RFC-0005's GC-struct lowering — pass the capability directly"
                        ),
                    });
                }
            }
            // Recurse into arguments for a nested container (`List(Option(File))`).
            args.iter().try_for_each(|a| reject_cap_slot_boundary(a, defs, ctx, position))
        }
    }
}

/// Whether `t` names a host capability that `main` may receive as a root
/// authority (the rights of `Dir`/`Net` don't matter here — any are grantable).
pub(crate) fn is_capability_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, _)
        if matches!(n.as_str(), "Console" | "Clock" | "Rand" | "Env" | "Dir" | "File" | "Net" | "Exec" | "Secret" | "SecretStore"))
}

/// (RFC-0047) The kind of an un-comparable component of a type: `==`/`!=` reject
/// function and capability types at every depth (top level or nested inside a
/// container). Returns the FIRST such component found (searching the shared type
/// of an equality's operands), or `None` if the whole type is comparable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Uncomparable {
    Function,
    Capability,
}

// `uncomparable_kind` is a `Checker` method (it must consult the record/enum type
// tables to reach fn/capability FIELDS, not just generic arguments — BUG-302).

/// (RFC-0047) Which key/member position a `Float` occupies — used for the teaching
/// error suggesting the right escape.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FloatKeyKind {
    DictKey,
    SetMember,
}

/// Whether a resolved [`Ty`] has a concrete `Float` in a `Dict` KEY position or a
/// `Set` MEMBER position, at any depth (a `Dict(k, Dict(Float, v))` counts). Value
/// positions and list elements are fine — only the hashed/compared key set needs
/// `Eq`. `None` when there is no such Float.
fn float_key_position(t: &Ty) -> Option<FloatKeyKind> {
    match t {
        Ty::Named(n, args) if n == "Dict" => {
            // args[0] = key, args[1] = value. A Float directly as the key is the
            // reject; otherwise recurse into BOTH (a nested dict/set anywhere).
            if matches!(args.first(), Some(Ty::Float)) {
                return Some(FloatKeyKind::DictKey);
            }
            args.iter().find_map(float_key_position)
        }
        Ty::Named(n, args) if n == "Set" => {
            if matches!(args.first(), Some(Ty::Float)) {
                return Some(FloatKeyKind::SetMember);
            }
            args.iter().find_map(float_key_position)
        }
        Ty::Named(_, args) => args.iter().find_map(float_key_position),
        Ty::List(e) => float_key_position(e),
        Ty::Tuple(ts) => ts.iter().find_map(float_key_position),
        Ty::Fn(ps, r) => ps.iter().chain(std::iter::once(r.as_ref())).find_map(float_key_position),
        _ => None,
    }
}

/// (BUG-395) The argument index of the KEY of a generic `Dict` key operation (the
/// ones that hash/compare the key), or `None` for a non-key-op. Key operations
/// require an `Eq` key; the position lets the checker read the key's type after
/// argument unification.
fn dict_key_op_index(name: &str) -> Option<usize> {
    match name {
        "dict.insert" | "dict.get_or" | "dict.update" | "dict.contains_key" | "dict.remove" => {
            Some(1)
        }
        _ => None,
    }
}

/// The first type variable appearing anywhere in `t`, if any.
fn first_type_var(t: &Ty) -> Option<u32> {
    match t {
        Ty::Var(v) => Some(*v),
        Ty::List(e) => first_type_var(e),
        Ty::Tuple(ts) => ts.iter().find_map(first_type_var),
        Ty::Named(_, args) => args.iter().find_map(first_type_var),
        Ty::Fn(ps, r) => ps.iter().chain(std::iter::once(r.as_ref())).find_map(first_type_var),
        _ => None,
    }
}

fn float_key_reject_message(kind: FloatKeyKind) -> String {
    match kind {
        FloatKeyKind::DictKey =>
            "`Float` is not a valid `Dict` key — keys require `Eq`, but `Float` is \
             only `PartialEq` (`NaN != NaN`, so a Float key can be unretrievable and \
             `0.1 + 0.2` is a precision trap). Use an `Int` key (scale to a fixed \
             precision) or a `String` rendering of the value"
                .to_string(),
        FloatKeyKind::SetMember =>
            "`Float` is not a valid `Set` member — members require `Eq`, but `Float` \
             is only `PartialEq` (`NaN != NaN`). Use an `Int` (scaled) or a `String` \
             rendering"
                .to_string(),
    }
}

/// The teaching error for an `==`/`!=` on an un-comparable type. `ty` is the whole
/// operand type so the message can point at the container when the offender nests.
fn equality_reject_message(kind: Uncomparable, ty: &Ty) -> String {
    // The offender is nested when the whole operand type is a container (a List,
    // Tuple, or a generic like `Option`/`Result`) rather than the fn/capability
    // itself. `SecretStore` is the one capability spelled as a `Named` type.
    let nested = matches!(ty, Ty::List(_) | Ty::Tuple(_))
        || matches!(ty, Ty::Named(n, _) if n != "SecretStore");
    let where_ = if nested { format!(" (found nested in `{ty}`)") } else { String::new() };
    match kind {
        Uncomparable::Function => format!(
            "`==` is not defined on function types{where_} — there is no meaningful \
             equality for functions (identity is not stable across compilation). \
             Compare the values functions *produce*, not the functions"
        ),
        Uncomparable::Capability => format!(
            "`==` is not defined on capability types{where_} — capabilities are \
             authority, not data; there is no meaningful equality between two \
             authorities"
        ),
    }
}

/// Whether `t` is `List(String)` — the command-line-arguments parameter `main`
/// may declare.
pub fn is_args_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, args)
        if n == "List"
            && matches!(args.as_slice(), [ast::Type::Named(s, inner)] if s == "String" && inner.is_empty()))
}

/// Validate `main`'s signature: every parameter must be a host capability or the
/// `List(String)` args parameter, since `main` is the program's root entrypoint and
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
    // RFC-0038: a library-defined capability marked `grantable` may also enter at
    // the root (the host mints it from a `[user_caps]` grant). Bareness is enforced
    // separately by `check_grantable_caps`, so here we only recognize the name.
    let grantable: std::collections::HashSet<&str> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    let is_grantable_cap = |t: &ast::Type| {
        matches!(t, ast::Type::Named(n, _) if grantable.contains(n.as_str()))
    };
    for p in &main.params {
        if matches!(&p.ty, Some(t) if is_capability_type(t) || is_args_type(t) || is_grantable_cap(t)) {
            continue;
        }
        let found = match &p.ty {
            Some(t) => format!("has type `{}`", witchy_syntax::format::type_str(t)),
            None => "has no type annotation".to_string(),
        };
        return terr(format!(
            "`main` parameter `{}` {found}, but `main` may only take host capabilities \
             (Console, Clock, Env, Dir, Net, Exec, Secret, SecretStore), a `grantable` \
             library capability, or `List(String)` for command-line args",
            p.name
        ));
    }
    // The runtime's value sink surfaces a plain `main` return — an `Int` becomes
    // the process exit code, a `Float`/`String`/… is printed — but it does NOT
    // surface a `Result` or `Option`: their wrapper is dropped, so an `Err`/`None`
    // is silently swallowed (no message, exit stays 0). That makes the Rust
    // `fn main() -> Result(...)` habit a quiet-failure trap. Reject those two
    // specifically and point at handling the outcome in `main`.
    if let Some(ast::Type::Named(n, _)) = &main.ret {
        if n == "Result" || n == "Option" {
            return terr(format!(
                "`main` returns `{}`, but a `Result`/`Option` returned from `main` is \
                 silently discarded — its `Err`/`None` never surfaces and the exit code \
                 stays 0. Handle the outcome inside `main` and return an exit code (or \
                 print it), e.g. `match r: Ok(_) -> 0; Err(e) -> ... ; 1`.",
                witchy_syntax::format::type_str(main.ret.as_ref().unwrap())
            ));
        }
    }
    // (BUG-335, spec §16.3) `main` may return only `Nil` (the default) or `Int` (the
    // process exit code); `Float` is also surfaced (both backends print it). Any
    // OTHER return type — `String`/`Bool`/`List`/a record — is a parity trap: the
    // interpreter echoes it when the program printed nothing, but the compiled run
    // wrapper wires only `print_int`/`print_float` and silently drops the rest. Reject
    // it at check time so the backends agree by construction (fail loud, never a
    // silently different answer).
    if let Some(t) = &main.ret {
        let allowed = matches!(
            t.unqualified(),
            ast::Type::Named(n, _) if n == "Nil" || n == "Int" || n == "Float"
        );
        if !allowed {
            return terr(format!(
                "`main` returns `{}`, but `main` may return only `Nil` (the default) or \
                 `Int` (the process exit code) — a `String`/`Bool`/`List`/record result is \
                 printed by the interpreter but dropped by the compiled backend, so the two \
                 diverge. Print the value inside `main` and return `Nil` or an exit code.",
                witchy_syntax::format::type_str(t)
            ));
        }
    }
    Ok(())
}

/// RFC-0038: a `grantable capability` may be granted to a root entrypoint, so it
/// must be **bare** — carrying zero transitive built-in host authority. Otherwise
/// granting it would be an invisible `Net`/`Dir`/`Secret` grant (and a later
/// version adding a host-cap field would silently widen root authority with no
/// signature change). Reject any grantable cap that reaches a host capability
/// through its fields, directly or through nested user types.
fn check_grantable_caps(module: &Module) -> Result<(), TypeError> {
    let types: std::collections::HashMap<&str, &ast::TypeDef> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    for it in &module.items {
        let Item::Type(t) = it else { continue };
        if !t.grantable {
            continue;
        }
        let mut seen = std::collections::HashSet::new();
        if let Some(host) = grantable_host_taint(t, &types, &mut seen) {
            return terr(format!(
                "`grantable capability {}` carries host capability `{host}`; a \
                 root-grantable capability must be BARE (zero transitive host \
                 authority), so it cannot disguise a built-in capability. Construct \
                 it inside the program from an explicit `{host}` root instead.",
                t.name
            ));
        }
    }
    Ok(())
}

/// The first built-in host capability reachable from a type def's fields
/// (transitively through user-type fields), or `None` if the type is bare.
fn grantable_host_taint<'a>(
    t: &'a ast::TypeDef,
    types: &std::collections::HashMap<&'a str, &'a ast::TypeDef>,
    seen: &mut std::collections::HashSet<&'a str>,
) -> Option<String> {
    if !seen.insert(t.name.as_str()) {
        return None; // cycle guard
    }
    for v in &t.variants {
        for fty in &v.fields {
            if let Some(h) = type_host_taint(fty, types, seen) {
                return Some(h);
            }
        }
    }
    None
}

fn type_host_taint<'a>(
    ty: &'a ast::Type,
    types: &std::collections::HashMap<&'a str, &'a ast::TypeDef>,
    seen: &mut std::collections::HashSet<&'a str>,
) -> Option<String> {
    match ty {
        ast::Type::Named(n, args) => {
            if is_capability_type(ty) {
                return Some(n.clone());
            }
            if let Some(inner) = types.get(n.as_str()) {
                if let Some(h) = grantable_host_taint(inner, types, seen) {
                    return Some(h);
                }
            }
            args.iter().find_map(|a| type_host_taint(a, types, seen))
        }
        ast::Type::Qualified(_, inner) => type_host_taint(inner, types, seen),
        ast::Type::Tuple(ts) => ts.iter().find_map(|t| type_host_taint(t, types, seen)),
        ast::Type::Fn(params, ret) => params
            .iter()
            .chain(std::iter::once(ret.as_ref()))
            .find_map(|t| type_host_taint(t, types, seen)),
    }
}

/// (RFC-0040) A cap-gated string export — `pub fn export_*(cap, String) -> String`,
/// a browser app root — must lead with a BARE grantable capability, since the host
/// mints it at that entry (like `main`). Guard the intended-but-wrong shape (a
/// 2-param `[Named, String] -> String` export whose leading type isn't grantable)
/// with a clear error instead of silently not exporting the function.
fn check_export_signatures(module: &Module) -> Result<(), TypeError> {
    let grantable: std::collections::HashSet<&str> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    let is_string = |t: &Option<ast::Type>| {
        matches!(t, Some(ast::Type::Named(n, a)) if n == "String" && a.is_empty())
    };
    for it in &module.items {
        let Item::Function(f) = it else { continue };
        if !f.public {
            continue;
        }
        let unqual = f.name.rsplit('.').next().unwrap_or(&f.name);
        if !unqual.starts_with("export_") {
            continue;
        }
        if let [p0, p1] = f.params.as_slice() {
            let ret_string = matches!(&f.ret, Some(ast::Type::Named(n, _)) if n == "String");
            if is_string(&p1.ty) && ret_string {
                if let Some(ast::Type::Named(n, _)) = &p0.ty {
                    if n != "String" && !grantable.contains(n.as_str()) {
                        return terr(format!(
                            "the leading parameter of the exported entrypoint `{unqual}` is `{n}`, but a \
                             cap-gated export may lead only with a bare `grantable capability` (RFC-0040) — \
                             `{n}` is not grantable"
                        ));
                    }
                }
            }
        }
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
            Some(t) => format!("has type `{}`", witchy_syntax::format::type_str(t)),
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
                | Stmt::LetPattern { value, .. }
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
        ast::Type::Qualified(_, inner) => collect_type_params(inner, acc),
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
            if args.is_empty() && name.chars().next().is_some_and(|c| c.is_lowercase()) && !name.contains('.') {
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
    /// Sealed record capabilities (`capability X:` with named fields). Their
    /// fields are opaque: `.field` access is rejected so the only way to reach a
    /// carried capability is `match`, which the linker confines to the home
    /// module — otherwise an alias would leak the underlying authority.
    sealed_types: HashSet<String>,
    adt_variants: HashMap<String, Vec<String>>,
    fn_conventions: HashMap<String, Vec<Convention>>,
    /// (RFC-0043) Functions that are mutators — `var` first param + a return of
    /// that param's type. A mutator's *receiver* (arg 0) is exempt from the
    /// `var`-argument mutability demand: its expression form is a pure value call
    /// (any argument accepted), and its statement form's write-back is delivered
    /// by the `xs = f(xs, …)` rewrite. Only a Nil-returning `var` procedure keeps
    /// the mutable-`var`-argument obligation on that parameter.
    fn_mutators: HashSet<String>,
    /// Per-function type parameters (name, var id), from lowercase type names in
    /// signatures. Generalized: instantiated fresh at each call site.
    fn_typarams: HashMap<String, Vec<(String, u32)>>,
    /// (BUG-308) The type parameters (name -> var id) of the function whose body is
    /// currently being checked, so a body `let`/`var` ascription's lowercase name
    /// (`let out: List(a) = …`) resolves to the SAME type-parameter var as the
    /// signature rather than a fresh concrete `Named("a")` — which would pin the
    /// generic parameter and trip the "isn't generic" soundness check.
    current_typarams: HashMap<String, u32>,
    subst: HashMap<u32, Ty>,
    next_var: u32,
    /// Each binding carries its type and whether it is mutable.
    scopes: Vec<HashMap<String, (Ty, bool)>>,
    /// Bindings that have been consumed (moved out via an `own` parameter) and
    /// may not be used again until reassigned. Flow-sensitive within a body.
    consumed: HashSet<String>,
    /// One entry per ACTIVE `region:` block, holding the names declared
    /// inside it — an assignment to a name outside the innermost region must
    /// be scalar (a region's only pointer-escape is its value).
    region_locals: Vec<HashSet<String>>,
    /// The declared return type of the function currently being checked, so `?`
    /// can require the enclosing function to return a matching Result/Option.
    current_ret: Option<Ty>,
    /// (BUG-395 / RFC-0047) The key types of the generic `Dict` key operations
    /// (`dict.insert`/`get_or`/`update`/`contains_key`/`remove`) invoked in the
    /// body currently being checked, with the source line. A dict key is hashed and
    /// compared, so it must be `Eq`; validated once the body is fully inferred so
    /// an unbounded generic key (a type parameter with no `where k: Eq` bound)
    /// is rejected rather than silently accepted.
    dict_key_ops: Vec<(Ty, u32)>,
    /// Source line of the statement currently being checked, attached to errors
    /// so diagnostics point at a location. 0 means "no line known".
    cur_line: u32,
    /// The module (file stem) of the function currently being checked, so a
    /// diagnostic can render a home-module type/variant with its bare name — the
    /// spelling the reader wrote — while keeping cross-module qualifiers that
    /// disambiguate (RFC-0042; BUG-292). Empty means "unknown".
    cur_module: String,
    /// The entry module's name (the home of the unqualified `main`), used as the
    /// home for a bare function whose name carries no `module.` prefix (BUG-292).
    entry_module: String,
}

/// Render a canonical `module.Name` for a diagnostic: strip the qualifier when it
/// names `home` (the reader wrote the bare name and the error location already
/// names the module), but keep a cross-module qualifier (it disambiguates a
/// same-named type from another module).
fn dequalify_home(name: &str, home: &str) -> String {
    match name.split_once('.') {
        Some((module, bare)) if module == home => bare.to_string(),
        _ => name.to_string(),
    }
}

/// Strip the `home.` qualifier from every canonical type/constructor name embedded
/// in a diagnostic message, leaving cross-module qualifiers intact (RFC-0042;
/// BUG-292). A canonical qualifier is `home.` at a word boundary followed by an
/// uppercase letter (`t_file.Point`) — never an incidental substring, and never a
/// lowercase-suffixed name like the `module.fn` location prefix.
fn strip_home_qualifiers(message: &str, home: &str) -> String {
    if home.is_empty() {
        return message.to_string();
    }
    let needle = format!("{home}.");
    let bytes = message.as_bytes();
    let mut out = String::with_capacity(message.len());
    let mut i = 0;
    while i < message.len() {
        let boundary = i == 0 || {
            let prev = bytes[i - 1];
            !(prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'.')
        };
        if boundary
            && message[i..].starts_with(&needle)
            && message[i + needle.len()..].chars().next().is_some_and(|c| c.is_ascii_uppercase())
        {
            i += needle.len(); // drop the `home.` qualifier, keep the bare name
            continue;
        }
        let ch = message[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
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
            // (RFC-0025/0026) Ownership/immutability qualifiers are compile-time
            // contracts with no runtime type — lower to the inner type.
            ast::Type::Qualified(_, inner) => return self.to_ty(inner),
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
            "Bytes" => Ty::Bytes,
            "__Msg" => Ty::Msg,
            "Bool" => Ty::Bool,
            "Nil" => Ty::Nil,
            "Console" => Ty::Console,
            "Clock" => Ty::Clock,
            "Rand" => Ty::Rand,
            "Env" => Ty::Env,
            "Secret" => Ty::Secret,
            "Exec" => Ty::Exec,
            "Dir" => Ty::Dir(dir_rights(args)),
            "File" => Ty::File(file_rights(args)),
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
            // (BUG-308) A lowercase, argument-less name that names one of the
            // enclosing function's type parameters is that parameter's var — so a
            // body ascription refines the generic parameter instead of pinning it
            // to a distinct concrete `Named`. (Matches the signature-only rule in
            // `to_ty_generic`; outside a generic fn `current_typarams` is empty, so
            // top-level `let` and non-parameter names are unaffected.)
            other
                if args.is_empty()
                    && other.chars().next().is_some_and(|c| c.is_lowercase())
                    && !other.contains('.')
                    && self.current_typarams.contains_key(other) =>
            {
                Ty::Var(self.current_typarams[other])
            }
            _ => Ty::Named(name.clone(), args.iter().map(|a| self.to_ty(a)).collect()),
        }
    }

    /// Like `to_ty`, but a lowercase, argument-less type name becomes a type
    /// *variable* (a parameter), shared within one signature via `vars`.
    #[allow(clippy::wrong_self_convention)]
    fn to_ty_generic(&mut self, t: &ast::Type, vars: &mut HashMap<String, Ty>) -> Ty {
        match t {
            ast::Type::Qualified(_, inner) => self.to_ty_generic(inner, vars),
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
            "Bytes" => Ty::Bytes,
            "__Msg" => Ty::Msg,
                "Bool" => Ty::Bool,
                "Nil" => Ty::Nil,
                "Console" => Ty::Console,
            "Clock" => Ty::Clock,
            "Rand" => Ty::Rand,
            "Env" => Ty::Env,
            "Secret" => Ty::Secret,
                "Exec" => Ty::Exec,
                "Dir" => Ty::Dir(dir_rights(args)),
                "File" => Ty::File(file_rights(args)),
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
                        && other.chars().next().is_some_and(|c| c.is_lowercase())
                        && !other.contains('.') =>
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

    /// The first migrated externref capability carried by `t`, if any. Kept in
    /// lockstep with `is_externref_cap`; today only `File` has moved off the i32
    /// handle path. This is used for expression-level shapes (not just annotated
    /// `ast::Type`s), such as closure captures inferred from local bindings.
    fn ty_carries_externref_cap(&self, t: &Ty) -> Option<&'static str> {
        fn go(c: &Checker, t: &Ty, seen: &mut HashSet<String>) -> Option<&'static str> {
            match c.resolve(t) {
                Ty::File(_) => Some("File"),
                Ty::List(inner) => go(c, &inner, seen),
                Ty::Tuple(items) => items.iter().find_map(|i| go(c, i, seen)),
                Ty::Fn(params, ret) => params
                    .iter()
                    .find_map(|p| go(c, p, seen))
                    .or_else(|| go(c, &ret, seen)),
                Ty::Named(n, args) => {
                    if args.iter().any(|a| go(c, a, seen).is_some()) {
                        return Some("File");
                    }
                    if !seen.insert(n.clone()) {
                        return None;
                    }
                    let fields = c.record_fields.get(&n).map(|(_, fields)| fields.clone());
                    let hit = fields
                        .into_iter()
                        .flatten()
                        .find_map(|(_, field)| go(c, &field, seen));
                    seen.remove(&n);
                    hit
                }
                _ => None,
            }
        }
        go(self, t, &mut HashSet::new())
    }

    fn reject_externref_cap_aggregate_ty(&self, t: &Ty, ctx: &str) -> Result<(), TypeError> {
        match self.resolve(t) {
            Ty::List(inner) => {
                if let Some(cap) = self.ty_carries_externref_cap(&inner) {
                    return terr(format!(
                        "`{ctx}` builds a `List` containing a `{cap}` capability; \
                         cap-carrying collections require RFC-0005's GC-struct aggregate lowering — \
                         pass the capability directly"
                    ));
                }
                Ok(())
            }
            Ty::Tuple(items) => {
                if let Some(cap) = items.iter().find_map(|i| self.ty_carries_externref_cap(i)) {
                    return terr(format!(
                        "`{ctx}` builds a tuple containing a `{cap}` capability; \
                         cap-carrying tuples require RFC-0005's GC-struct aggregate lowering — \
                         pass the capability directly"
                    ));
                }
                Ok(())
            }
            Ty::Named(n, args) if matches!(n.as_str(), "Option" | "Result" | "Dict") => {
                if let Some(cap) = args.iter().find_map(|a| self.ty_carries_externref_cap(a)) {
                    return terr(format!(
                        "`{ctx}` wraps a `{cap}` capability in `{n}`; \
                         an externref capability has no boxed i64-slot representation — \
                         pass it directly"
                    ));
                }
                Ok(())
            }
            Ty::Named(n, args) if !is_externref_cap(&n) => {
                if let Some(cap) = self.ty_carries_externref_cap(&Ty::Named(n.clone(), args)) {
                    return terr(format!(
                        "`{ctx}` builds `{n}` carrying a `{cap}` capability; \
                         cap-carrying aggregates require RFC-0005's GC-struct lowering — \
                         pass the capability directly"
                    ));
                }
                Ok(())
            }
            _ => Ok(()),
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
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    /// The (name, type) bindings introduced in the innermost scope frame — used to
    /// compare what each or-pattern alternative binds (RFC-0052 binding-consistency).
    fn scope_bindings(&self) -> Vec<(String, Ty)> {
        self.scopes
            .last()
            .map(|f| f.iter().map(|(n, (t, _))| (n.clone(), t.clone())).collect())
            .unwrap_or_default()
    }
    fn define(&mut self, name: String, ty: Ty, mutable: bool) {
        if let Some(r) = self.region_locals.last_mut() {
            r.insert(name.clone());
        }
        self.scopes.last_mut().unwrap().insert(name, (ty, mutable));
    }
    /// Walk frames inner→outer, returning the binding at the first frame that
    /// defines the name (inner bindings shadow outer ones).
    fn resolve_binding(&self, name: &str) -> Option<&(Ty, bool)> {
        for vars in self.scopes.iter().rev() {
            if let Some(b) = vars.get(name) {
                return Some(b);
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

    fn call_sig(&mut self, name: &str) -> Option<(Vec<Ty>, Ty)> {
        match name {
            "print" => Some((vec![Ty::Console, Ty::String], Ty::Nil)),
            "now" => Some((vec![Ty::Clock], Ty::Int)),
            // Monotonic elapsed nanoseconds (a steady clock, immune to wall-clock
            // jumps) — for measuring durations. Like `now`, the Clock arg is the
            // authority; reading it is ambient nondeterminism.
            "now_monotonic" => Some((vec![Ty::Clock], Ty::Int)),
            "rand_u64" => Some((vec![Ty::Rand], Ty::Int)),
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
            // (Bytes) Primitive intrinsics behind the `std/bytes` surface. `Bytes` and
            // `String` share the flat `[len][bytes]` layout, so the representation-level
            // ops are identity/reuse on the compiled backend.
            "__bytes_from_string" => Some((vec![Ty::String], Ty::Bytes)),
            "__bytes_to_string" => Some((vec![Ty::Bytes], Ty::String)),
            // (RFC-0055) The channel-endpoint erasure bridge. `__erase` casts any
            // typed message to the executor's opaque `__Msg`; `__unerase` recovers
            // it at the endpoint's type. Representationally the identity on both
            // backends (a message already rides the universal slot); the pairing of
            // a `Sender(m)`/`Receiver(m)` to one channel id is what makes every
            // `__unerase` see a value erased at the same `m`. Deliberately NOT
            // inferable end-to-end — `__unerase`'s result type `m` is a fresh var
            // fixed only by its use site — and confined to `std/chan`/`std/task`.
            "__erase" => {
                let m = self.fresh();
                Some((vec![m], Ty::Msg))
            }
            "__unerase" => {
                let m = self.fresh();
                Some((vec![Ty::Msg], m))
            }
            "__bytes_length" => Some((vec![Ty::Bytes], Ty::Int)),
            "__bytes_at" => Some((vec![Ty::Bytes, Ty::Int], Ty::Int)),
            "__bytes_concat" => Some((vec![Ty::Bytes, Ty::Bytes], Ty::Bytes)),
            "__bytes_slice" => Some((vec![Ty::Bytes, Ty::Int, Ty::Int], Ty::Bytes)),
            "string.to_upper" | "string.to_lower" | "string.trim" => Some((vec![Ty::String], Ty::String)),
            // Abort with a message (the primitive behind std/testing).
            "fail" => Some((vec![Ty::String], Ty::Nil)),
            "string.starts_with" | "string.contains" | "string.ends_with" => {
                Some((vec![Ty::String, Ty::String], Ty::Bool))
            }
            "string.find" => Some((vec![Ty::String, Ty::String], Ty::Int)),
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
            "dict.contains_key" => {
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
            "dict.length" => {
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
            // (RFC-0032) Spin up the `server.serve` worker pool over a bound listener.
            "serve_pool" => Some((vec![Ty::Listener], Ty::Nil)),
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

    /// Resolve a call's first argument as a `File` capability and yield its rights.
    /// An unconstrained variable defaults to the full right-set (bare `File`).
    fn file_cap_rights(&mut self, name: &str, arg: &Expr) -> Result<FileRights, TypeError> {
        let cap = self.infer(arg)?;
        match self.resolve(&cap) {
            Ty::File(r) => Ok(r),
            Ty::Var(_) => {
                self.unify(&cap, &Ty::File(FileRights::full()))?;
                Ok(FileRights::full())
            }
            other => terr(format!(
                "`{name}` expects a `File` capability but got `{other}`"
            )),
        }
    }

    /// Type-check a file-capability op (RFC-0012). A `File` is a leaf, so its ops
    /// take no path: `read(f: File[Read]) -> String` (arity 1) and
    /// `write(f: File[Write], data) -> Nil` (arity 2). Returns `Ok(None)` when the
    /// name/arity isn't a File op, so the Dir forms (`read(dir, path)` etc.) fall
    /// through — `read`/`write` are disambiguated from `Dir` by arity.
    fn check_file_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        let arity = match name {
            "read" => 1,
            "write" => 2,
            _ => return Ok(None),
        };
        if args.len() != arity {
            return Ok(None);
        }
        let rights = self.file_cap_rights(name, &args[0])?;
        for arg in &args[1..] {
            let at = self.infer(arg)?;
            self.unify(&Ty::String, &at).map_err(|e| TypeError {
                message: format!("in call to `{name}`: {}", e.message),
            })?;
        }
        let ret = match name {
            "read" => {
                if !rights.read {
                    return terr(format!("`read` needs `Read` but the file is `{rights}`"));
                }
                Ty::String
            }
            "write" => {
                if !rights.write {
                    return terr(format!("`write` needs `Write` but the file is `{rights}`"));
                }
                Ty::Nil
            }
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

    /// Type-check a directory-capability op, enforcing that the `Dir`'s rights
    /// permit the verb: `read`/`exists`/`subdir`/`list` need `Read`; `write`/
    /// `append`/`make_dir` need `Write`. (Narrowing is done with the `as`
    /// ascription, not per-op builtins.) Returns `Ok(None)` when `name` is not
    /// a Dir op.
    /// `__try_ctx(value, msg)` — the `e ? "msg"` desugar. Generic over the operand:
    /// `Option(T)` or `Result(T, String)`, both yielding `Result(T, String)` so the
    /// enclosing `?` unwraps `T` and propagates an `Err(String)`. The message is a
    /// `String`; a `Result`'s error must already be `String` (the message is
    /// prepended to it, so it stays `String`).
    fn check_try_ctx(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        if name != "__try_ctx" {
            return Ok(None);
        }
        if args.len() != 2 {
            return terr(format!("`? \"msg\"` expects (value, message) but got {}", args.len()));
        }
        let mty = self.infer(&args[1])?;
        self.unify(&Ty::String, &mty).map_err(|e| TypeError {
            message: format!("a `? \"msg\"` context message must be a String: {}", e.message),
        })?;
        let oty = self.infer(&args[0])?;
        let elem = self.fresh();
        let resolved = self.resolve(&oty);
        match &resolved {
            Ty::Named(n, _) if n == "Option" => {
                self.unify(&Ty::Named("Option".into(), vec![elem.clone()]), &oty).map_err(|e| {
                    TypeError { message: format!("in `? \"msg\"`: {}", e.message) }
                })?;
            }
            Ty::Named(n, _) if n == "Result" => {
                self.unify(
                    &Ty::Named("Result".into(), vec![elem.clone(), Ty::String]),
                    &oty,
                )
                .map_err(|_| TypeError {
                    message: "`? \"msg\"` prepends to a String error, so the `Result`'s error type must be `String`".to_string(),
                })?;
            }
            other => {
                return terr(format!(
                    "`? \"msg\"` applies to an `Option` or `Result`, not `{other}`"
                ));
            }
        }
        Ok(Some(Ty::Named("Result".into(), vec![elem, Ty::String])))
    }

    fn check_dir_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        // RFC-0011: `only` is polymorphic — `dir.only(DirPolicy)` narrows a Dir's
        // entry policy (handled here); a `Net` receiver defers to `check_net_op`.
        if name == "only" {
            if args.len() != 2 {
                return terr(format!("`only` expects 2 argument(s) but got {}", args.len()));
            }
            let recv = self.infer(&args[0])?;
            let Ty::Dir(rights) = self.resolve(&recv) else {
                return Ok(None); // not a Dir.only — let check_net_op handle Net.only
            };
            if !rights.read {
                return terr(format!("`only` needs `Read` but the capability is `{rights}`"));
            }
            let pt = self.infer(&args[1])?;
            self.unify(&Ty::Named("DirPolicy".into(), Vec::new()), &pt)
                .map_err(|e| TypeError { message: format!("in call to `only`: {}", e.message) })?;
            return Ok(Some(Ty::Dir(rights)));
        }
        let arity = match name {
            "list" => 1,
            "read" | "exists" | "is_dir" | "subtree" | "make_dir" | "read_file" | "write_file" => 2,
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
            "subtree" => {
                if !rights.read {
                    return terr(format!(
                        "`{name}` needs `Read` but the capability is `{rights}`"
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
            // RFC-0012 navigation: a `Dir` opens a confined `File` (the leaf). The
            // name states the conferred right: `read_file` needs `Read` and yields
            // `File[Read]`; `write_file` needs `Write` and yields `File[Write]`.
            "read_file" => {
                if !rights.read {
                    return terr(format!(
                        "`read_file` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::File(FileRights { read: true, write: false })
            }
            "write_file" => {
                if !rights.write {
                    return terr(format!(
                        "`write_file` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::File(FileRights { read: false, write: true })
            }
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

    /// Type-check the low-level `exec` op:
    /// `exec(exec, dir, path, args, stdin) -> String`.
    /// `Exec` is the right to spawn a subprocess; the executable is named through a
    /// `Dir[Read]` (the same confinement as `read`), so you can only run a file you
    /// can read. `args` is a single `\0`-joined argv string and the result is a
    /// `"<exit_code>\n<output>"` payload — the std `exec` module wraps this as
    /// `(Int, String)` over a `List(String)`. Returns `Ok(None)` when `name` is not
    /// `exec`.
    fn check_exec_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        if name != "exec" {
            return Ok(None);
        }
        if args.len() != 5 {
            return terr(format!(
                "`exec` expects (exec, dir, path, args, stdin) — 5 arguments but got {}",
                args.len()
            ));
        }
        let e = self.infer(&args[0])?;
        self.unify(&Ty::Exec, &e).map_err(|err| TypeError {
            message: format!("`exec`'s first argument must be an `Exec` capability: {}", err.message),
        })?;
        let rights = self.dir_cap_rights("exec", &args[1])?;
        if !rights.read {
            return terr(format!(
                "`exec` needs a `Dir` with `Read` to locate the executable, but the capability is `{rights}`"
            ));
        }
        for (i, what) in [(2usize, "path"), (3, "args"), (4, "stdin")] {
            let at = self.infer(&args[i])?;
            self.unify(&Ty::String, &at).map_err(|err| TypeError {
                message: format!("in call to `exec`: {what} must be a String: {}", err.message),
            })?;
        }
        Ok(Some(Ty::String))
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
            "connect" | "try_connect" | "listen" | "only" | "deny" | "resolve" => 2,
            // (RFC-0020) pinned dial: (net, ip, host, port, secure).
            "connect_pinned" | "try_connect_pinned" => 5,
            // (RFC-0060) HTTPS listen: (net, addr, cert_pem, key).
            "listen_tls" => 4,
            _ => return Ok(None),
        };
        if args.len() != arity {
            return terr(format!(
                "`{name}` expects {arity} argument(s) but got {}",
                args.len()
            ));
        }
        let rights = self.net_cap_rights(name, &args[0])?;
        if name == "connect_pinned" || name == "try_connect_pinned" {
            // (RFC-0020) mixed trailing args: ip:String, host:String, port:Int, secure:Bool.
            for (arg, expected) in args[1..].iter().zip([Ty::String, Ty::String, Ty::Int, Ty::Bool]) {
                let at = self.infer(arg)?;
                self.unify(&expected, &at).map_err(|e| TypeError {
                    message: format!("in call to `{name}`: {}", e.message),
                })?;
            }
        } else if name == "listen_tls" {
            // (RFC-0060) mixed trailing args: addr:String, cert_pem:String, key:Secret.
            // The key is a `Secret` — never a String path or raw bytes — so the
            // private key stays host-side, consumed by handle.
            for (arg, expected) in args[1..].iter().zip([Ty::String, Ty::String, Ty::Secret]) {
                let at = self.infer(arg)?;
                self.unify(&expected, &at).map_err(|e| TypeError {
                    message: format!("in call to `{name}`: {}", e.message),
                })?;
            }
        } else {
            // The trailing argument: a typed `NetPolicy` for the policy verbs (`only`/`deny`,
            // RFC-0011), a `host:port` string for the address verbs (`connect`/`listen`/`restrict`),
            // a bare host string for `resolve`.
            let expected = if name == "only" || name == "deny" {
                Ty::Named("NetPolicy".into(), Vec::new())
            } else {
                Ty::String
            };
            for arg in &args[1..] {
                let at = self.infer(arg)?;
                self.unify(&expected, &at).map_err(|e| TypeError {
                    message: format!("in call to `{name}`: {}", e.message),
                })?;
            }
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
            // (RFC-0060) HTTPS listen — the same rights as `listen` (the TLS layer
            // adds no network authority; the key's authority is the Secret itself).
            "listen_tls" => {
                if !rights.listen {
                    return terr(format!(
                        "`listen_tls` needs `Listen` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`listen_tls` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Listener
            }
            // (RFC-0020) Resolve a name to its IP literals. Gated on `Connect` alone (it
            // adds no authority — `connect_pinned` re-checks the chosen IP); no transport
            // requirement, since resolution is not itself a dial.
            "resolve" => {
                if !rights.connect {
                    return terr(format!(
                        "`resolve` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                Ty::List(Box::new(Ty::String))
            }
            // Pinned dials — same rights as `connect` (a literal-IP TCP dial), the hostname
            // carried only for SNI/Host. `try_` is total (`Option(Socket)`).
            "connect_pinned" => {
                if !rights.connect {
                    return terr(format!(
                        "`connect_pinned` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`connect_pinned` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Socket
            }
            "try_connect_pinned" => {
                if !rights.connect {
                    return terr(format!(
                        "`try_connect_pinned` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`try_connect_pinned` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Named("Option".into(), vec![Ty::Socket])
            }
            // Attenuating the address set leaves the rights (verbs + transports) intact.
            // `only` narrows a `Net` to a `NetPolicy`'s address set; `deny` subtracts an
            // address pattern (a monotone exclusion). Both preserve the rights set.
            "only" | "deny" => Ty::Net(rights),
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
            (Ty::File(s), Ty::File(t)) => (!t.read || s.read) && (!t.write || s.write),
            (Ty::Net(s), Ty::Net(t)) => {
                (!t.connect || s.connect)
                    && (!t.listen || s.listen)
                    && (!t.tcp || s.tcp)
                    && (!t.udp || s.udp)
                    && (!t.uds || s.uds)
            }
            (Ty::Console, Ty::Console) => true,
            (Ty::Exec, Ty::Exec) => true,
            // An unconstrained source: pin it to the ascribed capability.
            (Ty::Var(_), Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::Console | Ty::Exec) => {
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
            (Ty::File(want), Ty::File(has)) => (!want.read || has.read) && (!want.write || has.write),
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

    /// (RFC-0047 / BUG-302) Whether `t` (a resolved [`Ty`]) contains a function or
    /// capability type at any depth — the two kinds `==`/`!=` refuse. Containers
    /// (List/Tuple/Dict/Result/Option) are transparent through their generic
    /// arguments; a record or enum is transparent through its DECLARED FIELD /
    /// variant-payload types too (a `type H: run: fn(Int) -> Int` is `Named("H",
    /// [])` with no generic argument, so walking only arguments — the old bug —
    /// let it escape the net, then the compiled backend rejected it while the
    /// interpreter compared by closure identity: a parity divergence). `seen`
    /// guards against recursive types. A bare type variable is comparable here (a
    /// bounded generic resolves after monomorphization; an unbounded one is caught
    /// elsewhere).
    fn uncomparable_kind(&self, t: &Ty, seen: &mut HashSet<String>) -> Option<Uncomparable> {
        match t {
            Ty::Fn(_, _) => Some(Uncomparable::Function),
            Ty::Console | Ty::Clock | Ty::Rand | Ty::Env | Ty::Secret | Ty::Exec | Ty::Socket
            | Ty::Listener | Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::BuildOut | Ty::BuildRead
            | Ty::BuildEnv | Ty::BuildNet | Ty::BuildExec => Some(Uncomparable::Capability),
            Ty::List(e) => self.uncomparable_kind(e, seen),
            Ty::Tuple(ts) => ts.iter().find_map(|x| self.uncomparable_kind(x, seen)),
            Ty::Named(n, args) => {
                // `SecretStore` is a capability the type checker models as a Named type.
                if n == "SecretStore" {
                    return Some(Uncomparable::Capability);
                }
                // Generic arguments (`List(fn)`, `Option(File)`, `H(fn)`).
                if let Some(k) = args.iter().find_map(|a| self.uncomparable_kind(a, seen)) {
                    return Some(k);
                }
                // Declared record fields / enum variant payloads. Guard against a
                // recursive type revisiting itself (which cannot make it
                // uncomparable — the offending fn/cap would be found on the first
                // visit).
                if !seen.insert(n.clone()) {
                    return None;
                }
                let found = self.named_field_uncomparable(n, seen);
                seen.remove(n);
                found
            }
            _ => None,
        }
    }

    /// The un-comparable kind carried by any DECLARED field of record/enum `n`
    /// (payload types are walked with the type's own parameters left as vars — a
    /// concrete fn/cap field triggers; a generic field is caught via the generic
    /// argument walk in [`Self::uncomparable_kind`]).
    fn named_field_uncomparable(&self, n: &str, seen: &mut HashSet<String>) -> Option<Uncomparable> {
        // A record: its named fields.
        if let Some((_, fields)) = self.record_fields.get(n) {
            if let Some(k) = fields.iter().find_map(|(_, ty)| self.uncomparable_kind(ty, seen)) {
                return Some(k);
            }
        }
        // An enum (possibly the same record type, which is a one-variant enum):
        // every variant's payload types.
        if let Some(variants) = self.adt_variants.get(n) {
            for v in variants {
                if let Some((payloads, _)) = self.ctor_sigs.get(v) {
                    if let Some(k) = payloads.iter().find_map(|ty| self.uncomparable_kind(ty, seen)) {
                        return Some(k);
                    }
                }
            }
        }
        None
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
                        // (RFC-0025) `frozen` asserts deep immutability, so a `frozen`
                        // binding cannot also be mutable — `var x: frozen T` is a
                        // contradiction the checker rejects (the contract has teeth).
                        if *mutable && is_frozen_type(decl) {
                            return Err(TypeError {
                                message: format!(
                                    "`{name}` is declared `frozen` (deeply immutable) but also `var` (mutable) — drop the `var` (use `let`), or drop `frozen`"
                                ),
                            });
                        }
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
                Stmt::LetPattern { pattern, value } => {
                    // (RFC-0052) `let PAT = e` — ONE pattern grammar for every
                    // binding position. A `let`/`for`/comprehension pattern must be
                    // IRREFUTABLE (proven to always match); a refutable one errors,
                    // pointing at `if let`.
                    if let Some(dup) = pattern_dup_binding(pattern) {
                        return terr(format!(
                            "pattern binds `{dup}` more than once — each binding in a \
                             pattern must have a distinct name"
                        ));
                    }
                    let vt = self.infer(value)?;
                    if let Some(reason) = self.pattern_refutable(pattern, &vt) {
                        return terr(reason);
                    }
                    self.check_pattern(pattern, &vt)?;
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
        // (RFC-0047) `Dict` keys and `Set` members require `Eq`; `Float` is
        // `PartialEq` but not `Eq` (NaN != NaN), so a `Float`-keyed dict has an
        // unretrievable NaN entry and a `Float` key is a precision trap. Reject
        // it at check time — once any expression's type concretely has a `Float`
        // in key position, the whole program is refused (a type-level rule, not a
        // per-NaN runtime trap). Only fires when the key type is concretely Float
        // (an unresolved key var never triggers).
        if let Some(kind) = float_key_position(&self.resolve(&t)) {
            return terr(float_key_reject_message(kind));
        }
        if let Some(rec) = &mut self.type_record {
            rec.insert(expr as *const Expr as usize, t.clone());
        }
        Ok(t)
    }

    /// Infer a TRANSIENT (desugar-temp) expression WITHOUT recording any of its
    /// sub-node types into the address-keyed `type_record`. A desugared subtree
    /// (a `range`/index/`while let` lowering, built locally and dropped as soon
    /// as inference returns) is never walked by a table consumer — they walk the
    /// original AST — so recording its node addresses is useless, and a soundness
    /// hazard: once the temp is freed, a later allocation reusing its address
    /// would read a stale, wrong type from the table (BUG-004). Suppressing the
    /// recording for these subtrees closes that hole at the source; the persistent
    /// node that desugared (the `Range`/`Index`/`WhileLet` itself) is still
    /// recorded by the enclosing `infer`, with the result type this returns.
    fn infer_transient(&mut self, e: &Expr) -> Result<Ty, TypeError> {
        let saved = self.type_record.take();
        let r = self.infer(e);
        self.type_record = saved;
        r
    }

    fn infer_inner(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        match expr {
            // Expanded away by `crate::tagged` during linking, before checking.
            Expr::TaggedLit { tag, .. } => {
                unreachable!("unexpanded tagged literal `{tag}` reached the type checker")
            }
            Expr::Int(_) => Ok(Ty::Int),
            Expr::Float(_) => Ok(Ty::Float),
            Expr::Duration(_) => Ok(Ty::Duration),
            Expr::Str(_) => Ok(Ty::String),
            Expr::Bool(_) => Ok(Ty::Bool),
            // A range lowers to a list-building block; type it as that block.
            Expr::Range { lo, hi, inclusive } => {
                let d = witchy_syntax::parser::desugar_range((**lo).clone(), (**hi).clone(), *inclusive);
                self.infer_transient(&d)
            }
            // A subscript lowers to an `list.at(base, index)` call; type it as that.
            Expr::Index { base, index } => {
                let d = witchy_syntax::parser::desugar_index((**base).clone(), (**index).clone());
                self.infer_transient(&d)
            }
            Expr::MethodCall { method, .. } => {
                // Trait lowering resolves every method call (impl, trait
                // bound, or static); one that survives is unresolvable.
                terr(format!(
                    "cannot resolve the method call `.{method}(…)` — methods come from \
                     `impl` blocks; a plain function is called as `{method}(value, …)`"
                ))
            }
            // Named-field record construction is lowered by `witchy_syntax::records`
            // before type-checking.
            Expr::Record { .. } => {
                unreachable!("Expr::Record is lowered by witchy_syntax::records before typeck")
            }
            // (RFC-0056) Labeled calls are resolved to positional `Call`s by
            // `witchy_syntax::keyword_args` at the link layer, before type-checking.
            Expr::LabeledCall { .. } => {
                unreachable!("Expr::LabeledCall is lowered by witchy_syntax::keyword_args before typeck")
            }
            // `while let` lowers to a `while true` over a match; type that.
            Expr::WhileLet { pattern, scrutinee, body } => {
                let d = witchy_syntax::parser::desugar_while_let(
                    pattern.clone(),
                    (**scrutinee).clone(),
                    body.clone(),
                );
                self.infer_transient(&d)
            }
            Expr::List(items) => {
                let elem = self.fresh();
                for it in items {
                    let t = self.infer(it)?;
                    self.unify(&elem, &t)?;
                }
                let ty = Ty::List(Box::new(elem));
                self.reject_externref_cap_aggregate_ty(&ty, "list literal")?;
                Ok(ty)
            }
            Expr::Tuple(items) => {
                let tys = items
                    .iter()
                    .map(|e| self.infer(e))
                    .collect::<Result<Vec<_>, _>>()?;
                let ty = Ty::Tuple(tys);
                self.reject_externref_cap_aggregate_ty(&ty, "tuple literal")?;
                Ok(ty)
            }
            Expr::Var(name) => {
                if self.consumed.contains(name) {
                    return terr(format!(
                        "use of `{name}` after it was moved (consumed by an `own` parameter)"
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
                terr(format!("unbound variable `{name}`"))
            }
            Expr::Lambda { params, body, ret } => {
                // Closures capture by value, so an assignment to a captured
                // (outer) variable cannot propagate out: the interpreter would
                // silently mutate a private copy while the compiled backends can't
                // express it at all. Reject it uniformly here (using the shared
                // AST capture/assignment scan) so every backend agrees.
                let scan = witchy_syntax::lambda_scan::scan_lambda(params, body);
                let outer = scan.assigns_outer();
                if !outer.is_empty() {
                    return terr(format!(
                        "a closure cannot assign to the captured variable `{}` (captures are by value, so the write would be lost) — return the new value or use a `var` parameter instead",
                        outer.join("`, `")
                    ));
                }
                for cap_name in scan.captures() {
                    let Some(ty) = self.lookup(&cap_name) else {
                        continue;
                    };
                    let Some(cap) = self.ty_carries_externref_cap(&ty) else {
                        continue;
                    };
                    return terr(format!(
                        "a closure cannot capture `{cap_name}` because it carries a `{cap}` capability; \
                         cap-carrying closure environments require RFC-0005's GC-struct aggregate lowering — \
                         pass the capability directly"
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
                // The closure is its OWN `?` boundary: a `?` in its body propagates
                // to the closure's return type, not the enclosing function's. Use
                // the declared return type if given (`fn(x) -> Result(..): ...`),
                // else a fresh var the body pins. Save/restore the outer return so
                // a `?` after the closure still targets the enclosing function.
                let declared = ret.as_ref().map(|t| self.to_ty(t));
                let lambda_ret = declared.clone().unwrap_or_else(|| self.fresh());
                let saved_ret = self.current_ret.replace(lambda_ret.clone());
                let body_ty = self.infer_block(body)?;
                self.unify(&lambda_ret, &body_ty).map_err(|e| TypeError {
                    message: format!("closure body type does not match its declared return type: {}", e.message),
                })?;
                self.current_ret = saved_ret;
                self.pop();
                Ok(Ty::Fn(param_tys, Box::new(lambda_ret)))
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
                if let Some(t) = self.check_file_op(name, args)? {
                    return Ok(t);
                }
                if let Some(t) = self.check_dir_op(name, args)? {
                    return Ok(t);
                }
                if let Some(t) = self.check_exec_op(name, args)? {
                    return Ok(t);
                }
                if let Some(t) = self.check_net_op(name, args)? {
                    return Ok(t);
                }
                if let Some(t) = self.check_try_ctx(name, args)? {
                    return Ok(t);
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
                    // `show` is the `Show` trait method, not a free function: a
                    // bare `show(x)` resolves only when x's concrete type is
                    // statically known here. Point at the renderers that always
                    // work, rather than the misleading `import set` near-miss
                    // (`set.show` is an unrelated same-named module function).
                    if name == "show" && args.len() == 1 {
                        return terr(
                            "could not resolve the `Show` method `show` on this \
                             value — a bare `show(x)` needs x's concrete type to be \
                             statically known. Render any value with `\"${x}\"` \
                             interpolation or `say(console, x)`, or bind x via a \
                             `for` loop or a typed parameter so dispatch resolves",
                        );
                    }
                    // A retired global builtin: name the module-qualified
                    // spelling that replaced it (the one-cut migration).
                    if let Some(moved) = witchy_syntax::aliases::moved_builtin(name) {
                        return terr(format!(
                            "`{name}` moved to `{moved}` — pure data operations are \
                             module-qualified now (no import needed; the core modules \
                             are always available)"
                        ));
                    }
                    // `<` `<=` `>` `>=` desugar to these trait-method calls; an
                    // unresolved one means the operand's type lacks `Ord`, so name
                    // the operator and the fix rather than leaking the desugar name
                    // (and suggesting an unrelated `list` function as a "typo").
                    if let Some(op) = match name.as_str() {
                        "less" => Some("<"),
                        "greater" => Some(">"),
                        "less_equal" => Some("<="),
                        "greater_equal" => Some(">="),
                        _ => None,
                    } {
                        return terr(format!(
                            "`{op}` is not defined for this type — it requires `Ord`; derive it \
                             with `derive(PartialEq, Eq, PartialOrd, Ord)` or implement it"
                        ));
                    }
                    // If the name is an unimported stdlib function, point the way;
                    // otherwise suggest a near-miss stdlib name (a likely typo).
                    let hint = match witchy_syntax::linker::std_modules_for_function(name).as_slice() {
                        [m] => format!(" — did you forget `import {m}`?"),
                        many if !many.is_empty() => {
                            format!(" — did you forget to import one of: {}?", many.join(", "))
                        }
                        _ => match witchy_syntax::linker::closest_std_function(name) {
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
                    // (BUG-305) `"${f}"` on a function value is rejected HERE, at
                    // check time, so BOTH backends refuse it identically — rather
                    // than the interpreter rendering `<function/N>` while the
                    // compiled backend rejected at codegen with a misleading
                    // "generic record such as `Set`" diagnostic. A function has no
                    // printable form; interpolation renders DATA. `__render` is the
                    // desugaring of `"${…}"`, so the message speaks in the user's
                    // terms and never names `Set`/records for a function operand.
                    if name == "__render" {
                        if let Ty::Fn(..) = self.resolve(&at) {
                            return terr(
                                "cannot render a function value with `\"${…}\"` — a \
                                 function has no printable form. Interpolation renders \
                                 data; call the function (e.g. `f(x)`) and interpolate \
                                 its result instead",
                            );
                        }
                    }
                    self.coerce_arg(param_ty, &at)
                        .map_err(|e| TypeError { message: format!("in call to `{name}`: {}", e.message) })?;
                }
                // (BUG-395) A generic `Dict` key operation's key must be `Eq` — record
                // the (post-unification) key type and its line; validated once the
                // whole body is inferred (so a key var pinned later is seen concrete).
                if let Some(i) = dict_key_op_index(name) {
                    if let Some(key_ty) = params.get(i) {
                        self.dict_key_ops.push((key_ty.clone(), self.cur_line));
                    }
                }
                // Enforce conventions: a `var` parameter needs a mutable variable;
                // `own` consumes its argument (use-after-move becomes an error).
                if let Some(convs) = self.fn_conventions.get(name).cloned() {
                    let is_mutator = self.fn_mutators.contains(name);
                    for (i, (arg, conv)) in args.iter().zip(&convs).enumerate() {
                        match conv {
                            // (RFC-0043) A mutator's receiver (arg 0) is a pure value
                            // argument in expression form: any expression is accepted.
                            Convention::Var if is_mutator && i == 0 => {}
                            Convention::Var => match arg {
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
                            Convention::Own => {
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
                self.reject_externref_cap_aggregate_ty(&ret, &format!("call to `{name}`"))?;
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
                if name == "Nil" {
                    if !args.is_empty() {
                        return terr(format!(
                            "constructor `{name}` takes 0 field(s) but got {}",
                            args.len()
                        ));
                    }
                    return Ok(Ty::Nil);
                }
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
                    self.reject_externref_cap_aggregate_ty(&result, &format!("constructor `{name}`"))?;
                    Ok(result)
                } else {
                    if self.adt_variants.contains_key(name)
                        || BUILTIN_TYPE_NAMES.contains(&name.as_str())
                        || AMBIENT_STD_TYPE_NAMES.contains(&name.as_str())
                        || is_synthetic_type_name(name)
                    {
                        return terr(format!(
                            "type `{name}` is not a value; use it in a type annotation or call a real constructor/function"
                        ));
                    }
                    // Unknown constructor: still check its arguments, but don't
                    // constrain the result type.
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
                        // (RFC-0052) `-1s` negates a Duration to a Duration — a
                        // Duration is an exact i64 of milliseconds, so unary minus
                        // is well-defined and both backends negate the i64. This
                        // is why `let d: Duration = -1s` types (the sign folds into
                        // the literal's meaning, not into an Int).
                        Ty::Duration => {
                            self.unify(&t, &Ty::Duration)?;
                            Ok(Ty::Duration)
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
                if self.sealed_types.contains(tyname) {
                    return terr(format!(
                        "`{tyname}` is a sealed capability — its fields are private; \
                         destructure it with `match` inside its own module"
                    ));
                }
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
                if self.sealed_types.contains(&tyname) {
                    return terr(format!(
                        "`{tyname}` is a sealed capability and cannot be `update`d — \
                         only its own module may construct it"
                    ));
                }
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
                // (RFC-0047) `==`/`!=` desugar through `PartialEq` at every depth.
                // Function and capability types have no meaningful equality — a
                // function's identity is an implementation accident (monomorphization
                // /inlining), and a capability is authority, not data — so comparing
                // them (directly or nested in a container) is a compile-time error,
                // not a backend-dependent answer. This deletes the confirmed
                // `f == f` parity divergence by construction.
                let resolved = self.resolve(&lt);
                if let Some(kind) = self.uncomparable_kind(&resolved, &mut HashSet::new()) {
                    return terr(equality_reject_message(kind, &resolved));
                }
                Ok(Ty::Bool)
            }
            Lt | LtEq | Gt | GtEq => {
                self.unify(&lt, &rt)?;
                // Ordering is defined only for the totally-ordered primitives.
                // Without a type-class mechanism, allowing it on arbitrary types
                // would type-check but crash at runtime, so reject it here.
                match self.resolve(&lt) {
                    Ty::Int | Ty::Float | Ty::String | Ty::Duration => Ok(Ty::Bool),
                    // A `Ty::Var` operand is an unbounded generic type parameter
                    // (it renders as `?`): `<` on it needs an `Ord` bound to
                    // dispatch through the trait, so point at that instead of a
                    // bare "found `?`".
                    other @ Ty::Var(_) => terr(format!(
                        "ordering comparison on `{other}` — if this is a generic type \
                         parameter, bound it with `where T: Ord` so `<` dispatches through \
                         the Ord trait"
                    )),
                    other => terr(format!(
                        "ordering comparison requires Int, Float, String, Duration, or a type \
                         that derives `Ord` — found `{other}` (derive it with \
                         `derive(PartialEq, Eq, PartialOrd, Ord)`)"
                    )),
                }
            }
            And => {
                self.unify(&Ty::Bool, &lt)?;
                self.unify(&Ty::Bool, &rt)?;
                Ok(Ty::Bool)
            }
            Or => {
                // `a || b` is Bool-only logical-or (RFC-0048). A non-Bool operand
                // gets a teaching error pointing at `??`, the fallback operator
                // that took over the old truthy/unwrap meanings.
                for t in [self.resolve(&lt), self.resolve(&rt)] {
                    if !matches!(t, Ty::Bool | Ty::Var(_)) {
                        return terr(format!(
                            "`||` is logical-or on Bool, found `{t}`. For a fallback \
                             value use `??`: `name ?? \"anon\"` (Option), \
                             `parse(s) ?? 0` (Result)"
                        ));
                    }
                }
                self.unify(&Ty::Bool, &lt)?;
                self.unify(&Ty::Bool, &rt)?;
                Ok(Ty::Bool)
            }
            Coalesce => {
                // `a ?? b` (RFC-0048): the fallback operator. The left side must
                // be an Option(T) or a Result(T, e); the right side is a T
                // (evaluated only on None/Err); the expression is a T. Nothing
                // else is admissible — no truthiness, no same-typed fallback —
                // so the result type is never ambiguous.
                match self.resolve(&lt) {
                    Ty::Named(ref n, ref args) if n == "Option" && args.len() == 1 => {
                        let t = args[0].clone();
                        self.unify(&t, &rt).map_err(|e| TypeError {
                            message: format!(
                                "`??` fallback: the right side must have the Option's \
                                 payload type: {}",
                                e.message
                            ),
                        })?;
                        Ok(t)
                    }
                    Ty::Named(ref n, ref args) if n == "Result" && args.len() == 2 => {
                        let t = args[0].clone();
                        self.unify(&t, &rt).map_err(|e| TypeError {
                            message: format!(
                                "`??` fallback: the right side must have the Result's \
                                 Ok type: {}",
                                e.message
                            ),
                        })?;
                        Ok(t)
                    }
                    Ty::Var(_) => terr(
                        "the left side of `??` must be an Option or a Result — \
                         annotate it (`let x: Option(Int) = …`) so `??` knows what \
                         to unwrap",
                    ),
                    other => terr(format!(
                        "`??` unwraps an Option or a Result (`opt ?? default`), \
                         found `{other}` on the left"
                    )),
                }
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
            if let Some(dup) = pattern_dup_binding(&arm.pattern) {
                return terr(format!(
                    "pattern binds `{dup}` more than once — each binding in a pattern \
                     must have a distinct name (witchy has no equality patterns)"
                ));
            }
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
        // Coverage analysis (exhaustiveness + unreachability) reasons per
        // alternative, so flatten a top-level `Pattern::Or` arm into one synthetic
        // arm per alternative (sharing the guard) — exactly what the old
        // parse-time or-expansion produced, so these checks are unchanged.
        let flat = flatten_or_arms(arms);
        self.check_unreachable(&flat)?;
        self.check_exhaustive(&st, &flat)?;
        Ok(result)
    }

    /// (RFC-0052) Whether a pattern is REFUTABLE in an irrefutable context
    /// (`let`/`for`/comprehension) — returns `Some(teaching-error)` if so, `None`
    /// if it provably always matches. Irrefutable: `_`, a variable, a tuple of
    /// irrefutable patterns (any nesting), and a constructor/record pattern for a
    /// SINGLE-variant type whose fields are all irrefutable. Everything else
    /// (literals, ranges, or-patterns, list patterns, multi-variant constructors)
    /// is refutable — the message points at `if let`.
    fn pattern_refutable(&self, pat: &Pattern, ty: &Ty) -> Option<String> {
        match pat {
            Pattern::Wildcard | Pattern::Var(_) => None,
            Pattern::Tuple(ps) => {
                // Recover each slot type if the expected type is a concrete tuple;
                // otherwise a placeholder (`Ty::Nil`) — only used to type sub-tuple
                // slots, and refutability never depends on a slot's exact type.
                let slots = match self.resolve(ty) {
                    Ty::Tuple(ts) if ts.len() == ps.len() => Some(ts),
                    _ => None,
                };
                for (i, sub) in ps.iter().enumerate() {
                    let sub_ty = slots.as_ref().map(|s| s[i].clone()).unwrap_or(Ty::Nil);
                    if let Some(r) = self.pattern_refutable(sub, &sub_ty) {
                        return Some(r);
                    }
                }
                None
            }
            Pattern::Ctor { name, args } => {
                // The variant's ADT and how many variants it has.
                let adt = self
                    .ctor_sigs
                    .get(name)
                    .and_then(|(_, res)| match res {
                        Ty::Named(adt, _) => Some(adt.clone()),
                        _ => None,
                    });
                let variant_count = adt
                    .as_ref()
                    .and_then(|a| self.adt_variants.get(a))
                    .map(|v| v.len());
                match variant_count {
                    // A multi-variant type's constructor pattern can fail.
                    Some(n) if n > 1 => Some(format!(
                        "`let {} = …` — `{name}` is one of {n} variants of `{}`, so this \
                         pattern can fail. Use `if let {} = …:` (with an else), or `match`.",
                        describe_pattern(pat),
                        adt.as_deref().unwrap_or("?"),
                        describe_pattern(pat),
                    )),
                    // A single-variant record/wrapper is irrefutable IF its fields
                    // are; recurse (field types recovered via check_pattern's own
                    // machinery would need instantiation, so be permissive: use
                    // fresh vars, which never themselves report refutable).
                    _ => {
                        for sub in args {
                            if let Some(r) = self.pattern_refutable(sub, &Ty::Nil) {
                                return Some(r);
                            }
                        }
                        None
                    }
                }
            }
            // Everything below is refutable in an irrefutable context.
            Pattern::Int(_) | Pattern::Str(_) | Pattern::Bool(_) | Pattern::Duration(_) => {
                Some(format!(
                    "`let {} = …` — a literal pattern can fail to match. Use `if let` \
                     (with an else) or `match`.",
                    describe_pattern(pat)
                ))
            }
            Pattern::IntRange { .. } => Some(
                "a range pattern can fail to match — use `if let` (with an else) or `match`"
                    .to_string(),
            ),
            Pattern::Or(_) => Some(
                "an or-pattern can fail to match — use `if let` (with an else) or `match`"
                    .to_string(),
            ),
            Pattern::List { .. } => Some(
                "a list pattern can fail to match (a list's length is never statically \
                 known) — use `if let` (with an else) or `match`"
                    .to_string(),
            ),
        }
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
            // (RFC-0052) A duration literal pattern matches a `Duration` scrutinee.
            Pattern::Duration(_) => self.unify(expected, &Ty::Duration),
            // (RFC-0052) An integer range pattern matches an `Int` scrutinee.
            Pattern::IntRange { lo, hi, inclusive } => {
                self.unify(expected, &Ty::Int)?;
                // A backwards range never matches anything — almost always a typo.
                let empty = if *inclusive { lo > hi } else { lo >= hi };
                if empty {
                    return terr(format!(
                        "range pattern `{lo}..{}{hi}` is empty (matches nothing) — \
                         its low bound is not below its high bound",
                        if *inclusive { "=" } else { "" }
                    ));
                }
                Ok(())
            }
            // (RFC-0052) An or-pattern: every alternative checks against the same
            // expected type AND must bind the SAME names at the SAME types
            // (binding-consistency). We check the first alternative in the real
            // scope (defining its bindings), then verify each further alternative
            // binds an identical name→type set in a throwaway scope.
            Pattern::Or(alts) => {
                let Some((first, rest)) = alts.split_first() else {
                    return terr("empty or-pattern");
                };
                // Snapshot the bindings the first alternative introduces.
                self.push();
                self.check_pattern(first, expected)?;
                let mut first_binds = self.scope_bindings();
                self.pop();
                first_binds.sort_by(|a, b| a.0.cmp(&b.0));
                // Define the first alternative's bindings for real (the arm body /
                // let sees them).
                self.check_pattern(first, expected)?;
                for alt in rest {
                    self.push();
                    self.check_pattern(alt, expected)?;
                    let mut alt_binds = self.scope_bindings();
                    self.pop();
                    alt_binds.sort_by(|a, b| a.0.cmp(&b.0));
                    let names_a: Vec<&String> = first_binds.iter().map(|(n, _)| n).collect();
                    let names_b: Vec<&String> = alt_binds.iter().map(|(n, _)| n).collect();
                    if names_a != names_b {
                        return terr(format!(
                            "or-pattern alternatives must bind the same names — `{}` \
                             binds {{{}}} but another alternative binds {{{}}}",
                            describe_pattern(alt),
                            names_a.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                            names_b.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                        ));
                    }
                    for ((n, ta), (_, tb)) in first_binds.iter().zip(&alt_binds) {
                        self.unify(ta, tb).map_err(|e| TypeError {
                            message: format!(
                                "or-pattern binding `{n}` has inconsistent types across \
                                 alternatives: {}",
                                e.message
                            ),
                        })?;
                    }
                }
                Ok(())
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
        // (BUG-294) Duration literal patterns are ordinary `i64`-of-milliseconds
        // literals (RFC-0052), so a duplicate `1s` arm is dead code just like a
        // duplicate `1` — track them the same way Int/Str/Bool are tracked.
        let mut durations: HashSet<i64> = HashSet::new();
        for (i, arm) in arms.iter().enumerate() {
            let already = saturated
                || match &arm.pattern {
                    Pattern::Ctor { name, .. } => ctors.contains(name.as_str()),
                    Pattern::Int(n) => ints.contains(n),
                    Pattern::Str(s) => strs.contains(s.as_str()),
                    Pattern::Bool(b) => bools.contains(b),
                    Pattern::Duration(ms) => durations.contains(ms),
                    _ => false,
                };
            // (BUG-295, spec §6: `if let`/`while let` accept ANY pattern) A trailing
            // bare `_` arm is idiomatic AND is exactly the synthesized else-arm an
            // irrefutable `if let x = e:` / `while let x = e:` desugars to
            // (`match e: <irrefutable> -> …; _ -> …`). So a redundant FINAL wildcard is
            // not an error — otherwise `if let x = 3` rejects while the equally-
            // irrefutable `if let (a, b) = p` passes (an inconsistent split). A
            // non-final or non-wildcard duplicate (real dead code, e.g. `1s` then `1s`)
            // is still flagged.
            let is_trailing_catchall =
                i + 1 == arms.len() && arm.guard.is_none() && matches!(arm.pattern, Pattern::Wildcard);
            if already && !is_trailing_catchall {
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
                    Pattern::Duration(ms) => {
                        durations.insert(*ms);
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
        // Infinite/large scalar domains can only be matched exhaustively with a
        // catch-all (an unguarded `_`/variable) — which we'd have accepted above.
        // Reaching here means there is none, so a literal-only or guard-only match
        // is non-exhaustive and would trap at runtime on an unlisted value.
        let scalar = match resolved {
            Ty::Int => Some("Int"),
            Ty::Float => Some("Float"),
            Ty::Duration => Some("Duration"),
            Ty::String => Some("String"),
            _ => None,
        };
        if let Some(kind) = scalar {
            return terr(format!(
                "non-exhaustive match on `{kind}`: it has no finite set of cases, \
                 so add a catch-all `_` arm (a guard does not make a match exhaustive)"
            ));
        }
        // (BUG-293) Tuple/list scrutinees are compound, not `Ty::Named`, so the ADT
        // path below never saw them and a non-exhaustive tuple/list match passed
        // `check` then TRAPPED at runtime. Cover them with the general
        // constructor-matrix algorithm (`rows_exhaustive`): a tuple is a
        // single-constructor product; a list is `[]` (nil) + `[head, ..tail]`
        // (cons). Only unguarded arms count (a guard can fail).
        if matches!(resolved, Ty::Tuple(_) | Ty::List(_)) {
            let rows: Vec<Vec<Pattern>> = arms
                .iter()
                .filter(|a| a.guard.is_none())
                .map(|a| vec![a.pattern.clone()])
                .collect();
            if self.rows_exhaustive(std::slice::from_ref(&resolved), &rows) {
                return Ok(());
            }
            return match resolved {
                Ty::List(_) => terr(
                    "non-exhaustive match on list: cover both the empty list `[]` and a \
                     non-empty list (`[head, ..tail]`), or add a catch-all `_` arm",
                ),
                _ => terr(
                    "non-exhaustive match on tuple: the arms don't cover every combination \
                     of component cases — add the missing case(s) or a catch-all `_` arm",
                ),
            };
        }
        let Ty::Named(adt, _) = resolved else {
            return Ok(());
        };
        let Some(variants) = self.adt_variants.get(&adt) else {
            return Ok(());
        };
        // Top-level: every variant of the sum type must appear in an unguarded arm.
        let covered: HashSet<&str> = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .filter_map(|a| match &a.pattern {
                Pattern::Ctor { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<&String> = variants.iter().filter(|v| !covered.contains(v.as_str())).collect();
        if !missing.is_empty() {
            // Render home-module names bare (`Blue`, not `t_file.Blue`) and backtick
            // each missing variant (BUG-292).
            let adt_disp = dequalify_home(&adt, &self.cur_module);
            let names = missing
                .iter()
                .map(|s| format!("`{}`", dequalify_home(s, &self.cur_module)))
                .collect::<Vec<_>>()
                .join(", ");
            return terr(format!("non-exhaustive match on `{adt_disp}`: missing {names}"));
        }
        // ...and each present variant must ALSO cover its fields, so a *nested*
        // non-exhaustive match (`Circle(Red)` without `Circle(Blue)`) is caught at
        // check time instead of trapping at runtime. `patterns_cover` reads each
        // nested type from the sub-patterns' own constructors and is permissive on
        // shapes it can't analyze, so it only ever rejects a provably-incomplete
        // match — never a valid one (which is what the earlier shortcut got wrong).
        for v in variants {
            let v_arms: Vec<&[Pattern]> = arms
                .iter()
                .filter(|a| a.guard.is_none())
                .filter_map(|a| match &a.pattern {
                    Pattern::Ctor { name, args } if name == v => Some(args.as_slice()),
                    _ => None,
                })
                .collect();
            if !v_arms.is_empty() && !self.variant_fields_covered(&v_arms) {
                let adt_disp = dequalify_home(&adt, &self.cur_module);
                let v_disp = dequalify_home(v, &self.cur_module);
                return terr(format!(
                    "non-exhaustive match on `{adt_disp}`: `{v_disp}` is matched but its fields \
                     don't cover every case — add a wholesale `{v_disp}(_)` arm or a `_`"
                ));
            }
        }
        Ok(())
    }

    /// Whether one variant's arms jointly cover all its field values. 0 fields →
    /// yes; 1 field → recurse on that column (the common `Some(_)`/`Circle(c)`
    /// case, checked precisely); ≥2 fields → permissive (a full product matrix is
    /// rare and error-prone, so we don't risk rejecting a valid enumeration).
    fn variant_fields_covered(&self, arms: &[&[Pattern]]) -> bool {
        match arms.first().map(|a| a.len()).unwrap_or(0) {
            0 => true,
            1 => {
                let col: Vec<&Pattern> = arms.iter().map(|a| &a[0]).collect();
                self.patterns_cover(&col)
            }
            _ => true,
        }
    }

    /// (BUG-293) The constructors a value of `ty` can take, each with the types of
    /// its fields — the finite signature the exhaustiveness matrix enumerates.
    /// `None` for an *open* type (Int/Float/String/Duration, a type variable, a
    /// capability/function, or an unknown ADT): such a column can only be covered
    /// by a wildcard, so the algorithm stays SOUND (it never rejects a valid match,
    /// only one with a provable hole).
    fn column_ctors(&self, ty: &Ty) -> Option<Vec<ColCtor>> {
        match ty {
            Ty::Bool => Some(vec![
                ColCtor { key: "true".into(), args: Vec::new() },
                ColCtor { key: "false".into(), args: Vec::new() },
            ]),
            Ty::Tuple(comps) => {
                Some(vec![ColCtor { key: TUPLE_CTOR.into(), args: comps.clone() }])
            }
            Ty::List(elem) => Some(vec![
                ColCtor { key: LIST_NIL.into(), args: Vec::new() },
                ColCtor { key: LIST_CONS.into(), args: vec![(**elem).clone(), ty.clone()] },
            ]),
            Ty::Named(adt, args) => {
                let variants = self.adt_variants.get(adt)?;
                Some(
                    variants
                        .iter()
                        .map(|v| ColCtor { key: v.clone(), args: self.variant_arg_types(v, args) })
                        .collect(),
                )
            }
            _ => None,
        }
    }

    /// The field types of variant `v` of a `Named(adt, actual_args)` type, with the
    /// ADT's formal type parameters substituted by `actual_args` — so a
    /// `Pair(a): P(a, a)` matched at `Pair(Bool)` yields fields `[Bool, Bool]`, not
    /// the un-substituted parameter vars (which would look opaque). Any leftover
    /// var stays a var (→ opaque → needs a wildcard), keeping the check sound.
    fn variant_arg_types(&self, v: &str, actual_args: &[Ty]) -> Vec<Ty> {
        let Some((fields, result)) = self.ctor_sigs.get(v) else {
            return Vec::new();
        };
        // The variant's result type carries its ADT's formal parameter vars in
        // order (`Named(adt, [Var(p0), Var(p1), …])`), so zip them to the actual
        // arguments to build the substitution.
        let map: HashMap<u32, Ty> = match result {
            Ty::Named(_, formals) => formals
                .iter()
                .zip(actual_args)
                .filter_map(|(f, a)| match f {
                    Ty::Var(id) => Some((*id, a.clone())),
                    _ => None,
                })
                .collect(),
            _ => HashMap::new(),
        };
        fields.iter().map(|f| self.subst_vars(f, &map)).collect()
    }

    /// (BUG-293) Type-directed exhaustiveness: whether the `rows` (each a pattern
    /// per column) provably cover every value of `types`. The classic
    /// constructor-matrix algorithm — for a column whose type has a finite ctor
    /// signature that the rows mention completely, split per constructor; otherwise
    /// only wildcard rows can cover the rest, so recurse on the default matrix. It
    /// terminates on recursive types (List) because an all-wildcard column takes
    /// the default branch instead of expanding `cons` forever.
    fn rows_exhaustive(&self, types: &[Ty], rows: &[Vec<Pattern>]) -> bool {
        // Expand a head-position or-pattern into one row per alternative, so
        // coverage reasons per alternative (a nested `(1 | 2, x)` contributes both
        // `1` and `2`). Deeper ors surface as column heads after specialization and
        // are expanded by the same step on the recursive call.
        let rows: Vec<Vec<Pattern>> = rows.iter().flat_map(|r| expand_head_or(r)).collect();
        let Some((col_ty, rest)) = types.split_first() else {
            // No columns left: the (empty) value is covered iff a row remains.
            return !rows.is_empty();
        };
        let col_ty = self.resolve(col_ty);
        let full = self.column_ctors(&col_ty);
        let present: HashSet<&str> =
            rows.iter().filter_map(|r| pattern_ctor_key(&r[0])).collect();
        let complete = matches!(&full, Some(cs) if cs.iter().all(|c| present.contains(c.key.as_str())));
        if complete {
            // Every constructor is mentioned — the match is exhaustive iff each
            // constructor's specialized sub-matrix is exhaustive.
            for c in full.expect("complete implies Some") {
                let sub_rows: Vec<Vec<Pattern>> =
                    rows.iter().filter_map(|r| specialize_row(r, &c)).collect();
                let mut sub_types = c.args.clone();
                sub_types.extend_from_slice(rest);
                if !self.rows_exhaustive(&sub_types, &sub_rows) {
                    return false;
                }
            }
            true
        } else {
            // Open or incomplete column: only wildcard rows can cover the missing
            // constructors. Drop this column from them and recurse on the rest.
            let default: Vec<Vec<Pattern>> = rows
                .iter()
                .filter(|r| pattern_ctor_key(&r[0]).is_none())
                .map(|r| r[1..].to_vec())
                .collect();
            self.rows_exhaustive(rest, &default)
        }
    }

    /// Whether a set of patterns at one position covers every value of its type.
    /// The type is read from the patterns themselves — a constructor names its
    /// ADT, a `Bool`/`Int`/`Str` literal names a scalar. Returns `true` whenever
    /// coverage cannot be DISPROVEN (lists, tuples, foreign constructors, mixed or
    /// empty rows), so it never rejects a valid match — only one it can prove is
    /// incomplete (a missing variant, or a scalar with no catch-all).
    fn patterns_cover(&self, pats: &[&Pattern]) -> bool {
        if pats.iter().any(|p| matches!(p, Pattern::Wildcard | Pattern::Var(_))) {
            return true;
        }
        if pats.iter().any(|p| matches!(p, Pattern::Bool(_))) {
            let has = |b: bool| pats.iter().any(|p| matches!(p, Pattern::Bool(x) if *x == b));
            return has(true) && has(false);
        }
        // A scalar literal column with no catch-all is an infinite domain — it can
        // never be exhaustively enumerated.
        if pats.iter().any(|p| matches!(p, Pattern::Int(_) | Pattern::Str(_))) {
            return false;
        }
        if !pats.is_empty() && pats.iter().all(|p| matches!(p, Pattern::Ctor { .. })) {
            let Pattern::Ctor { name: first, .. } = pats[0] else {
                return true;
            };
            let Some((_, result)) = self.ctor_sigs.get(first) else {
                return true;
            };
            let Ty::Named(adt, _) = result else {
                return true;
            };
            let Some(variants) = self.adt_variants.get(adt) else {
                return true;
            };
            for v in variants {
                let v_arms: Vec<&[Pattern]> = pats
                    .iter()
                    .filter_map(|p| match p {
                        Pattern::Ctor { name, args } if name == v => Some(args.as_slice()),
                        _ => None,
                    })
                    .collect();
                if v_arms.is_empty() || !self.variant_fields_covered(&v_arms) {
                    return false;
                }
            }
            return true;
        }
        true
    }

    /// The source name of the type parameter appearing in a (rejected) `Dict` key
    /// type, for the "add a `where <k>: Eq`" hint — falls back to `k`.
    fn key_var_name(&self, ty: &Ty) -> String {
        if let Some(id) = first_type_var(ty) {
            for (name, v) in &self.current_typarams {
                if *v == id {
                    return name.clone();
                }
            }
        }
        "k".to_string()
    }

    fn check_function(&mut self, func: &Function) -> Result<(), TypeError> {
        borrow_escape_check(func)?;
        let (params, ret) = self.fn_sigs.get(&func.name).cloned().unwrap();
        self.scopes = vec![HashMap::new()];
        self.consumed.clear();
        self.current_ret = Some(ret.clone());
        // (BUG-308) Make this function's type parameters visible to `to_ty` so body
        // ascriptions resolve `a` to the signature's parameter var.
        self.current_typarams = self
            .fn_typarams
            .get(&func.name)
            .map(|ps| ps.iter().map(|(n, v)| (n.clone(), *v)).collect())
            .unwrap_or_default();
        self.dict_key_ops.clear();
        self.cur_line = 0;
        // `func.name` is the canonical `module.fn`; remember the module so home
        // types render bare in this function's diagnostics (BUG-292). The entry
        // module's `main` is left unqualified by the linker, so fall back to the
        // detected entry module for it.
        self.cur_module = func
            .name
            .rsplit_once('.')
            .map_or_else(|| self.entry_module.clone(), |(m, _)| m.to_string());
        for (param, ty) in func.params.iter().zip(&params) {
            // (RFC-0025) A `frozen` parameter is deeply immutable, so a mutable
            // convention (`var`/`own`, which exist to mutate/consume the argument)
            // contradicts it.
            if param.convention.binds_mutable() {
                if let Some(decl) = &param.ty {
                    if is_frozen_type(decl) {
                        return Err(TypeError {
                            message: format!(
                                "parameter `{}` of `{}` is `frozen` (deeply immutable) but its convention is mutable (`var`/`own`) — a frozen value cannot be mutated; use a plain (read-only) parameter",
                                param.name, func.name
                            ),
                        });
                    }
                }
            }
            self.define(param.name.clone(), ty.clone(), param.convention.binds_mutable());
        }
        let body = self.infer_block(&func.body)?;
        // A broader capability may be returned where a narrower one is declared
        // (`-> Net[Connect]` returning a full `Net`), mirroring call-argument
        // narrowing; `coerce_arg` falls back to unification for everything else.
        self.coerce_arg(&ret, &body).map_err(|e| TypeError {
            message: format!("function `{}` body: {}", func.name, e.message),
        })?;
        // (RFC-0064 Checks 1+2) A `var`-param function with an ELIDED return. An
        // elided return is INFERRED from the body tail — it does NOT imply `Nil`,
        // so `is_var_procedure` (which optimistically reads an absent return as a
        // Nil procedure) is only correct when the inferred tail really is `Nil`.
        // Now that the body is inferred, classify the function by that tail:
        //   - tail is `Nil`  -> a genuine procedure channel (today's semantics);
        //   - tail == the `var` FIRST parameter's type -> AMBIGUOUS (Check 2): an
        //     elided mutator (`-> T`, statement form writes back) and a procedure
        //     (`-> Nil`) are indistinguishable by inference, and this is the one
        //     signature property whose inferred value changes call-site semantics,
        //     so the author must annotate the intent (an EXPLICIT return already
        //     declares it, so `func.ret.is_some()` is exempt — never reaches here);
        //   - any other non-`Nil` tail -> row 3 (Check 1): a `var` parameter on a
        //     value-returning function is the abolished combined write-back+return
        //     shape — including the `var`-first/unrelated-return case that ran on
        //     the interpreter but was rejected only by the WASM backend (an
        //     accidental interpreter-only shape), now rejected here for BOTH.
        // A still-unresolved (generic) tail is left alone; a later specialization
        // re-checks it.
        if func.ret.is_none() && func.params.iter().any(|p| p.convention == Convention::Var) {
            let body_ty = self.resolve(&body);
            if !matches!(body_ty, Ty::Nil | Ty::Var(_)) {
                let bare = func.name.rsplit('.').next().unwrap_or(&func.name);
                let first_is_var =
                    func.params.first().is_some_and(|p| p.convention == Convention::Var);
                if first_is_var && self.resolve(&params[0]) == body_ty {
                    return terr(format!(
                        "`{bare}` has a `var` receiver and its body's tail is the receiver's type — \
                         annotate the intent: `-> {body_ty}` declares a mutator (statement form \
                         writes back); `-> Nil` (or add `return`) declares a procedure"
                    ));
                }
                return terr(format!(
                    "`{bare}`: a `var` parameter must be a write-back channel (return `Nil`) or a \
                     mutator receiver (first parameter, returning its type); split the function or \
                     return a tuple"
                ));
            }
        }
        // (BUG-395 / RFC-0047) A generic `Dict` key operation performed in this
        // (unbounded) body must have an `Eq` key. Now that the body is fully
        // inferred, a key type that still carries a type variable is an UNBOUNDED
        // generic key — the function needs a `where <k>: Eq` bound (a bounded
        // function is checked through monomorphization instead, where the key is
        // concrete). Concrete keys (Int/String/records that derive Eq) are already
        // fine; `Float` is caught separately by `float_key_position`.
        // The std `dict` module's own compositional helpers (`map_values`, `filter`,
        // `merge`, …) key their output dicts with keys drawn from an INPUT `Dict`
        // parameter — whose existence already guarantees the key is `Eq` (a dict
        // can only be built through the bounded insert wrappers). Exempting the
        // trusted std API layer keeps those helpers unbounded (bounding them makes
        // them templates that regress result inference), while USER code that does a
        // generic key op is still enforced.
        let is_std = func
            .name
            .rsplit_once('.')
            .is_some_and(|(m, _)| witchy_syntax::linker::STD_MODULES.contains(&m));
        for (key_ty, line) in std::mem::take(&mut self.dict_key_ops) {
            if is_std {
                continue;
            }
            let resolved = self.resolve(&key_ty);
            if ty_has_var(&resolved) {
                self.cur_line = line;
                return terr(format!(
                    "`{}`: a generic `Dict` key must be `Eq` — the key type is used to hash \
                     and compare entries, so an unbounded type parameter can't be a key. Add a \
                     `where {}: Eq` bound (or use a concrete key type)",
                    func.name.rsplit('.').next().unwrap_or(&func.name),
                    self.key_var_name(&resolved)
                ));
            }
        }
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

    // (BUG-230) Duplicate type / constructor / method declarations get the same
    // "defined more than once" error the function namespace already gets. Runs
    // pre-lowering, while `impl`/`type` items are still present and distinct.
    check_unique_declarations(module)?;

    // (BUG-444) Parameter names are binding labels, not an overloadable surface:
    // duplicates silently shadow in the checker scope and make keyword labels
    // incoherent. Validate before lowering for source-quality diagnostics.
    check_unique_parameters(module)?;

    // (RFC-0064 Check 1) A `var` parameter must be a procedure channel or a
    // mutator receiver — every other shape is the abolished combined write-back
    // (rejected before either backend lowers, so parity holds by construction).
    check_var_conventions(module)?;

    // Lower named-field record construction (a no-op once the linker has done so,
    // but covers single-module paths like `check_str`).
    let recs = witchy_syntax::records::lower(module.clone()).map_err(|message| TypeError { message })?;
    check_type_names(&recs)?;
    check_trait_names(&recs)?;

    // Trait/impl declarations are desugared to ordinary functions first, so the
    // checker only ever sees plain functions (a no-op for trait-free modules).
    // The checked flavor surfaces unsatisfiable dispatch ("`Float` does not
    // implement `Show`") instead of a post-lowering unknown-function error.
    match crate::traits::lower_checked(recs.clone()) {
        Ok(lowered) => {
            check_unique_parameters(&lowered)?;
            check_var_conventions(&lowered)?;
            run_check(&lowered, false).map(|_| ())
        }
        Err(message) => {
            // (BUG-307) Mono's "cannot infer the result type" fallback fires when
            // `annotate` returned an EMPTY TypeTable — which is itself a symptom of
            // a REAL checker error elsewhere in the module (a body type error breaks
            // annotate, so every result-position bounded call loses its inferred
            // type and mono then misdiagnoses it). Run the checker on the plainly
            // lowered module first: its genuine error takes priority. The inference
            // fallback only stands when the module otherwise type-checks. Other
            // (genuine) dispatch errors are surfaced unchanged, preserving their
            // teaching message.
            if message.contains("cannot infer the result type") {
                if let Err(real) = run_check(&crate::traits::lower(recs), false) {
                    // Plain lowering can't resolve the un-inferable bounded call and
                    // leaves it as an unknown function — that artifact IS the same
                    // problem the mono message already describes (and better), so
                    // ignore it and keep the mono message. Any OTHER checker error is
                    // the real one mono masked (a body type error, etc.) — surface it.
                    if !real.message.contains("call to unknown function") {
                        return Err(real);
                    }
                }
            }
            Err(TypeError { message })
        }
    }
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
}

/// Convert a resolved checker type to the surface `ast::Type` shape the
/// backends' type-directed machinery (eq/to_string shapes, valtypes)
/// consumes. None where no surface form exists (functions, free variables).
pub fn ty_to_ast(t: &Ty) -> Option<witchy_syntax::ast::Type> {
    use witchy_syntax::ast::Type as T;
    Some(match t {
        Ty::Int => T::Named("Int".into(), Vec::new()),
        Ty::Float => T::Named("Float".into(), Vec::new()),
        Ty::Duration => T::Named("Duration".into(), Vec::new()),
        Ty::String => T::Named("String".into(), Vec::new()),
        Ty::Bytes => T::Named("Bytes".into(), Vec::new()),
        Ty::Msg => T::Named("__Msg".into(), Vec::new()),
        Ty::Bool => T::Named("Bool".into(), Vec::new()),
        Ty::Nil => T::Named("Nil".into(), Vec::new()),
        Ty::Console => T::Named("Console".into(), Vec::new()),
        Ty::Clock => T::Named("Clock".into(), Vec::new()),
        Ty::Rand => T::Named("Rand".into(), Vec::new()),
        Ty::Env => T::Named("Env".into(), Vec::new()),
        Ty::Secret => T::Named("Secret".into(), Vec::new()),
        Ty::Exec => T::Named("Exec".into(), Vec::new()),
        Ty::Dir(_) => T::Named("Dir".into(), Vec::new()),
        Ty::File(_) => T::Named("File".into(), Vec::new()),
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
/// keep walking): the typed-lowering keystone (rfcs/language-evolution.md
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

/// The entry module — the home of the unqualified `main` — for home-module
/// de-qualification in `main`'s own diagnostics (BUG-292). The linker emits the
/// entry module's items FIRST and qualifies every declaration except the bare
/// `main`, so the first `module.`-qualified item appearing BEFORE `main` belongs
/// to the entry module. An entry that declares only `main` (no qualified item
/// before it) yields "" — there are no home types to strip, and `main`'s
/// references to imported types must keep their qualifiers. When there is no bare
/// `main` at all (a library/comptime unit), fall back to the first qualified item.
fn detect_entry_module(module: &Module) -> String {
    let prefix = |name: &str| name.rsplit_once('.').map(|(m, _)| m.to_string());
    let mut before: Option<String> = None;
    for item in &module.items {
        match item {
            Item::Function(f) if f.name == "main" => return before.unwrap_or_default(),
            Item::Type(t) => before = before.or_else(|| prefix(&t.name)),
            Item::Function(f) => before = before.or_else(|| prefix(&f.name)),
            _ => {}
        }
    }
    before.unwrap_or_default()
}

fn run_check(module: &Module, record: bool) -> Result<Option<TypeTable>, TypeError> {
    let module = &module;
    let mut c = Checker {
        type_record: if record { Some(HashMap::new()) } else { None },
        fn_sigs: HashMap::new(),
        fn_conventions: HashMap::new(),
        fn_mutators: HashSet::new(),
        ctor_sigs: HashMap::new(),
        ctor_typarams: HashMap::new(),
        record_fields: HashMap::new(),
        sealed_types: HashSet::new(),
        adt_variants: HashMap::new(),
        fn_typarams: HashMap::new(),
        current_typarams: HashMap::new(),
        subst: HashMap::new(),
        next_var: 0,
        scopes: vec![HashMap::new()],
        consumed: HashSet::new(),
        region_locals: Vec::new(),
        current_ret: None,
        dict_key_ops: Vec::new(),
        cur_line: 0,
        cur_module: String::new(),
        entry_module: detect_entry_module(module),
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
                if f.is_mutator() {
                    c.fn_mutators.insert(f.name.clone());
                }
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
                        // Field privacy is a CAPABILITY property (its carried
                        // authority is secret), not a general sealing one. A
                        // `sealed type` (RFC-0065) seals only CONSTRUCTION; its
                        // fields stay readable/matchable (DoD item 4), so only a
                        // `capability` goes in `sealed_types`.
                        if t.sealed && t.is_capability {
                            c.sealed_types.insert(t.name.clone());
                        }
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
            // `witchy_syntax::consts` before this point.
            Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }

    // Reject typo'd / undeclared type names in signatures before they become
    // opaque types that mis-unify with a confusing message later.
    check_type_names(module)?;

    // `main` is the root entrypoint: its parameters are where the host's authority
    // enters, so they must be capabilities (or the args list) — validate before
    // diving into bodies so a malformed entry point is reported up front.
    check_main_signature(module)?;
    // RFC-0038: a `grantable` capability must be BARE (no transitive host authority),
    // checked module-wide (a grantable cap is invalid regardless of `main`).
    check_grantable_caps(module)?;
    // RFC-0040: a cap-gated string export must lead with a bare grantable capability.
    check_export_signatures(module)?;
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
            Item::Function(f) => c
                .check_function(f)
                .map_err(|e| at_loc(e, c.cur_line, &f.name, &c.cur_module))?,
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

/// The first name a pattern binds twice, if any. A pattern like `P(x, x)` must be
/// a compile error: witchy has no non-linear patterns (binding the same name in
/// two positions would silently shadow rather than constrain the two to be
/// equal), so we reject it rather than pick a winner.
fn pattern_dup_binding(p: &Pattern) -> Option<String> {
    fn walk(p: &Pattern, seen: &mut HashSet<String>, dup: &mut Option<String>) {
        if dup.is_some() {
            return;
        }
        match p {
            Pattern::Var(n) => {
                if !seen.insert(n.clone()) {
                    *dup = Some(n.clone());
                }
            }
            Pattern::Tuple(ps) => ps.iter().for_each(|q| walk(q, seen, dup)),
            Pattern::Ctor { args, .. } => args.iter().for_each(|q| walk(q, seen, dup)),
            Pattern::List { elems, rest } => {
                elems.iter().for_each(|q| walk(q, seen, dup));
                if let Some(Some(name)) = rest {
                    if !seen.insert(name.clone()) {
                        *dup = Some(name.clone());
                    }
                }
            }
            // (RFC-0052) Each or-pattern alternative is an INDEPENDENT binding set
            // (they bind the same names by the consistency rule), so a name reused
            // ACROSS alternatives is not a duplicate — check each alternative with
            // its own `seen`, catching only a within-alternative repeat.
            Pattern::Or(alts) => {
                for alt in alts {
                    let mut alt_seen = HashSet::new();
                    walk(alt, &mut alt_seen, dup);
                    if dup.is_some() {
                        return;
                    }
                }
            }
            _ => {}
        }
    }
    let mut seen = HashSet::new();
    let mut dup = None;
    walk(p, &mut seen, &mut dup);
    dup
}

/// The module-qualified NATIVE INTRINSICS: declared in std as self-recursive
/// placeholders (signatures for the checker), intercepted by name on both
/// backends, never templated by monomorphization, never compiled as bodies.
pub fn intrinsic(name: &str) -> bool {
    matches!(
        name,
        "list.push" | "list.at" | "list.length" | "list.concat"
            | "dict.new" | "dict.insert" | "dict.get_or" | "dict.contains_key" | "dict.remove"
            | "dict.update" | "dict.keys" | "dict.values" | "dict.pairs" | "dict.length"
            | "string.split" | "string.trim" | "string.contains" | "string.starts_with"
            | "string.ends_with" | "string.replace" | "string.find" | "string.substring"
            | "string.length" | "string.char_count" | "string.chars" | "string.to_upper"
            | "string.to_lower" | "string.to_int"
            | "math.to_float" | "math.to_int" | "math.sqrt"
    )
}


/// Convenience: parse then type-check.
pub fn check_str(src: &str) -> Result<(), String> {
    let module = witchy_syntax::parser::parse_module(src).map_err(|e| e.to_string())?;
    check(&module).map_err(|e| e.to_string())
}

/// A short, human-readable rendering of a pattern for diagnostics.
/// Expand every top-level `Pattern::Or` arm into one arm per alternative
/// (sharing the guard), so the coverage checks — which reason per alternative —
/// see the same shape the old parse-time or-expansion produced. Nested
/// or-patterns are left intact (the checks are permissive on shapes they can't
/// analyze). The body is irrelevant to coverage, so a `Wildcard` placeholder is
/// reused. (RFC-0052)
fn flatten_or_arms(arms: &[MatchArm]) -> Vec<MatchArm> {
    let mut out = Vec::with_capacity(arms.len());
    for arm in arms {
        match &arm.pattern {
            Pattern::Or(alts) => {
                for alt in alts {
                    out.push(MatchArm {
                        pattern: alt.clone(),
                        guard: arm.guard.clone(),
                        body: Expr::Bool(false),
                    });
                }
            }
            _ => out.push(arm.clone()),
        }
    }
    out
}

/// (BUG-293) One entry of a type's constructor signature for the exhaustiveness
/// matrix: a constructor key plus the field types it introduces as sub-columns.
struct ColCtor {
    key: String,
    args: Vec<Ty>,
}

/// Synthetic constructor keys for the structural (non-ADT) constructors.
const TUPLE_CTOR: &str = "(tuple)";
const LIST_NIL: &str = "(nil)";
const LIST_CONS: &str = "(cons)";

/// The constructor key a pattern tests at its column, or `None` if it matches ANY
/// value of that column (a wildcard, a variable, or an open list tail `[..r]`).
/// Literal patterns on open types (Int/String/Duration/range) return a key so they
/// are NOT mistaken for wildcards — they never complete a signature (their types
/// are open), and excluding them from the default matrix keeps a literal-only
/// match non-exhaustive, as it should be.
fn pattern_ctor_key(p: &Pattern) -> Option<&str> {
    match p {
        Pattern::Wildcard | Pattern::Var(_) => None,
        Pattern::Bool(true) => Some("true"),
        Pattern::Bool(false) => Some("false"),
        Pattern::Ctor { name, .. } => Some(name),
        Pattern::Tuple(_) => Some(TUPLE_CTOR),
        Pattern::List { elems, rest } => {
            if !elems.is_empty() {
                Some(LIST_CONS)
            } else if rest.is_none() {
                Some(LIST_NIL)
            } else {
                None // `[..r]` matches any list — wildcard-like for this column
            }
        }
        Pattern::Int(_) | Pattern::Str(_) | Pattern::Duration(_) | Pattern::IntRange { .. } => {
            Some("(literal)")
        }
        // Expanded away at each column head by `expand_head_or` before this runs.
        Pattern::Or(_) => None,
    }
}

/// Specialize one matrix row by constructor `c`: if the row's head pattern can
/// match `c`, return the row with the head replaced by `c`'s field sub-patterns
/// (so `c`'s arguments become new leading columns); otherwise `None` (the row is
/// dropped, as it can never match `c`).
fn specialize_row(row: &[Pattern], c: &ColCtor) -> Option<Vec<Pattern>> {
    let (head, tail) = row.split_first()?;
    let mut new_row = specialize_head(head, c)?;
    new_row.extend_from_slice(tail);
    Some(new_row)
}

fn specialize_head(p: &Pattern, c: &ColCtor) -> Option<Vec<Pattern>> {
    let arity = c.args.len();
    match p {
        Pattern::Wildcard | Pattern::Var(_) => Some(vec![Pattern::Wildcard; arity]),
        Pattern::Bool(b) => (c.key == if *b { "true" } else { "false" }).then(Vec::new),
        Pattern::Ctor { name, args } => (c.key == *name).then(|| args.clone()),
        Pattern::Tuple(ps) => (c.key == TUPLE_CTOR).then(|| ps.clone()),
        Pattern::List { elems, rest } => specialize_list(elems, rest, c),
        // A literal on an open type never reaches a synthetic ctor; drop the row.
        Pattern::Int(_) | Pattern::Str(_) | Pattern::Duration(_) | Pattern::IntRange { .. } => None,
        Pattern::Or(_) => None, // expanded away before specialization
    }
}

/// Specialize a list pattern under the `nil`/`cons` constructors of `List(elem)`.
fn specialize_list(
    elems: &[Pattern],
    rest: &Option<Option<String>>,
    c: &ColCtor,
) -> Option<Vec<Pattern>> {
    match c.key.as_str() {
        // `[]` / `[..r]` match the empty list; a fixed-length prefix does not.
        LIST_NIL => elems.is_empty().then(Vec::new),
        LIST_CONS => {
            if let Some((first, more)) = elems.split_first() {
                // `[e0, e1.., ..r]` = cons(e0, `[e1.., ..r]`)
                Some(vec![first.clone(), Pattern::List { elems: more.to_vec(), rest: rest.clone() }])
            } else if rest.is_some() {
                // `[..r]` matches a non-empty list too: head `_`, tail `_`.
                Some(vec![Pattern::Wildcard, Pattern::Wildcard])
            } else {
                None // `[]` is not a cons
            }
        }
        _ => None,
    }
}

/// Expand a head-position or-pattern into one row per alternative (recursively, so
/// a nested `1 | 2 | 3` fully flattens). Non-or heads pass through unchanged.
fn expand_head_or(row: &[Pattern]) -> Vec<Vec<Pattern>> {
    match row.first() {
        Some(Pattern::Or(alts)) => alts
            .iter()
            .flat_map(|alt| {
                let mut r = vec![alt.clone()];
                r.extend_from_slice(&row[1..]);
                expand_head_or(&r)
            })
            .collect(),
        _ => vec![row.to_vec()],
    }
}

fn describe_pattern(p: &Pattern) -> String {
    match p {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Var(n) => n.clone(),
        Pattern::Int(n) => n.to_string(),
        Pattern::Str(s) => format!("\"{s}\""),
        Pattern::Bool(b) => b.to_string(),
        Pattern::Ctor { name, args } if args.is_empty() => name.clone(),
        Pattern::Ctor { name, args } => {
            format!("{name}({})", args.iter().map(describe_pattern).collect::<Vec<_>>().join(", "))
        }
        Pattern::Tuple(ps) => {
            format!("({})", ps.iter().map(describe_pattern).collect::<Vec<_>>().join(", "))
        }
        Pattern::List { .. } => "list pattern".to_string(),
        Pattern::Duration(ms) => format!("{ms}ms"),
        Pattern::IntRange { lo, hi, inclusive } => {
            format!("{lo}{}{hi}", if *inclusive { "..=" } else { ".." })
        }
        Pattern::Or(alts) => alts.iter().map(describe_pattern).collect::<Vec<_>>().join(" | "),
    }
}

#[cfg(test)]
#[path = "typeck_tests.rs"]
mod tests;
