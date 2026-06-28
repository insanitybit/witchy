//! Tree-walking evaluator for witchy — the **reference oracle**.
//!
//! User programs run on the compiled (WASM) backend: `witchy run`, `witchy
//! sandbox`, build steps, and the browser playground all go through `codegen`.
//! This evaluator's job is to *define* the semantics the compiled backend is
//! checked against — it is the independent implementation `witchy parity` and the
//! differential test suite diff the compiler against. It is reached at runtime
//! only as: the parity oracle and the differential test runner, the `comptime`
//! evaluator (compile-time blocks, zero capabilities), the capability-sound
//! executor for *effectful* build steps (BuildExec/BuildNet/BuildEnv, whose
//! host-side I/O the WASM boundary can't sandbox — the grant allow-list is the
//! confinement), and the `witchy demo` showcase. Its `Dir`/`Net` path-confinement
//! logic is also reused by the sandbox, so it stays even as the evaluator role
//! shrinks. See `rfcs/oracle-only-migration.md`.

// `Result<Value, Flow>` threads control flow (early `return`, `break`,
// `continue`) — not just errors — through evaluation, so the `Flow::Return(Value)`
// "Err" variant deliberately carries a whole Value. Boxing it to shrink the
// Result (what `result_large_err` asks for) would put a heap allocation on every
// `return`/`?` in the oracle's hot path; the larger Result is the right trade.
#![allow(clippy::result_large_err)]

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

use witchy_runtime::net::Stream;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use witchy_syntax::ast::*;
use witchy_syntax::parser::parse_module;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    List(Vec<Value>),
    Tuple(Vec<Value>),
    Ctor { name: String, fields: Vec<Value> },
    Cap(Capability),
    /// An unforgeable capability to a directory subtree (cap-std `Dir` style).
    /// Carries the host path it is rooted at; can only be obtained from the root
    /// grant or by attenuation (`subdir`).
    Dir(PathBuf),
    /// A file capability (RFC-0012): authority to one file (the leaf of the
    /// Dir/File hierarchy). Carries the confined host path; obtained by navigating
    /// a `Dir` (`dir.open`/`dir.create`) or as a `main` grant. Rights are checked
    /// at compile time, so the value carries only the path.
    File(PathBuf),
    /// A network capability: an allow-list of permitted `host:port` destinations
    /// (wasi:sockets / cap-std-net style). Attenuable via `restrict`.
    Net(Vec<String>),
    /// A single secret's raw bytes (a signing seed, or a value secret like a token).
    /// Unforgeable — minted only by the host or fetched from a `SecretStore`. The
    /// ability to use it *is* authority; `.sign`/`.public_key` read it as a hex
    /// Ed25519 seed, `.reveal` returns it verbatim.
    Secret(Vec<u8>),
    /// The host-granted store of NAMED secrets (from `--secret`/`--secret-file`/
    /// `--signing-key`). `secret_store.get(name)` yields a `Secret`.
    SecretStore(std::collections::BTreeMap<String, Vec<u8>>),
    /// A connected socket — a handle into the interpreter's socket table.
    Socket(usize),
    /// A listening server socket — a handle into the interpreter's listener
    /// table. Obtained from `listen(net, addr)`; `accept` blocks for a `Socket`.
    Listener(usize),
    /// A first-class function (closure): its parameters, body, and the
    /// environment captured where it was defined.
    Closure {
        params: Vec<Param>,
        body: Block,
        env: Box<Env>,
    },
    /// An immutable associative map, kept as insertion-ordered key/value pairs
    /// (keys compared by value equality). `Dict(K, V)` in the type system.
    Dict(Vec<(Value, Value)>),
    /// A build-time capability, minted only for a rune's `build` entrypoint and
    /// carrying its confined grant (an output/read directory, or an allow-list).
    /// The build sandbox is where these enter — never `main`.
    Build(BuildCap),
    Nil,
}

/// A build-time capability instance, carrying the attenuated grant the build
/// driver minted it with. Kind-only in the type system; the specifics live here.
#[derive(Debug, Clone, PartialEq)]
pub enum BuildCap {
    /// Write generated source into this confined output directory.
    Out(PathBuf),
    /// Read project files confined to one of these directory subtrees. A relative
    /// path resolves against the first granted root that contains it.
    Read(Vec<PathBuf>),
    /// Read environment variables, restricted to this allow-list of names.
    Env(Vec<String>),
    /// Fetch from this allow-list of hosts.
    Net(Vec<String>),
    /// Invoke external tools, restricted to this allow-list.
    Exec(Vec<String>),
}

/// Capabilities are unforgeable: no witchy expression can construct one. They
/// enter a program only at `main` (the root grant) and propagate solely by
/// being passed as arguments. This is the hybrid capability model — a function
/// that needs to perform an effect must be handed the capability for it, so a
/// library that was never granted one cannot perform that effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Console,
    Clock,
    Env,
    /// The right to spawn a native subprocess (`exec`). Right-less and payload-
    /// free: the executable is named + confined by a `Dir[Read]` argument.
    Exec,
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) => write!(f, "{}", witchy_syntax::fmt::render_float(*x)),
            Value::Str(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Nil => write!(f, "Nil"),
            Value::List(items) => {
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Value::Tuple(items) => {
                write!(f, "(")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, ")")
            }
            Value::Ctor { name, fields } => {
                write!(f, "{name}")?;
                if !fields.is_empty() {
                    write!(f, "(")?;
                    for (i, v) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{v}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Value::Cap(c) => write!(f, "<capability {c:?}>"),
            Value::Dir(_) => write!(f, "<dir>"),
            Value::File(_) => write!(f, "<file>"),
            Value::Net(_) => write!(f, "<net>"),
            Value::Secret(_) => write!(f, "<secret>"),
            Value::SecretStore(_) => write!(f, "<secret store>"),
            Value::Socket(id) => write!(f, "<socket #{id}>"),
            Value::Listener(id) => write!(f, "<listener #{id}>"),
            Value::Build(_) => write!(f, "<build capability>"),
            Value::Closure { params, .. } => write!(f, "<function/{}>", params.len()),
            Value::Dict(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeError {
    pub message: String,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime error: {}", self.message)
    }
}

impl std::error::Error for RuntimeError {}

/// The control-flow channel threaded through expression evaluation: either a
/// real error, or an early `return` carrying a value (produced by `?` when it
/// short-circuits on `Err`/`None`). `Return` is caught at the function boundary
/// and turned back into that function's result.
enum Flow {
    Err(RuntimeError),
    Return(Value),
    /// `break` — caught by the innermost loop, which stops.
    Break,
    /// `continue` — caught by the innermost loop, which proceeds to the next
    /// iteration.
    Continue,
}

impl From<RuntimeError> for Flow {
    fn from(e: RuntimeError) -> Self {
        Flow::Err(e)
    }
}

fn err<T, E: From<RuntimeError>>(message: impl Into<String>) -> Result<T, E> {
    Err(E::from(RuntimeError {
        message: message.into(),
    }))
}

/// Narrow a `Net`'s allowlist to a `NetPolicy`'s pattern set (`\n`-joined for a union from
/// `confine.union`). Each pattern must already be admitted by the current allowlist
/// (monotone — refinement only shrinks). Returns the narrowed `Net`.
fn net_narrow_to(allow: &[String], patterns: &str) -> Result<Value, RuntimeError> {
    match witchy_caps::capabilities::net_only(allow, patterns) {
        Ok(narrowed) => Ok(Value::Net(narrowed)),
        Err(p) => Err(RuntimeError {
            message: format!("`{p}` is not in this Net capability"),
        }),
    }
}

/// Prefix a runtime error with where it occurred — the executing function (after
/// linking, `module.func`, which also names the file) and source line. `line ==
/// 0` means no line is available; an empty `func` omits the name.
fn rt_at_line(e: RuntimeError, line: u32, func: &str) -> RuntimeError {
    if line == 0 {
        return e;
    }
    let where_ = if func.is_empty() {
        format!("line {line}")
    } else {
        format!("`{func}`, line {line}")
    };
    RuntimeError {
        message: format!("{where_}: {}", e.message),
    }
}

/// Collapse a function body's `Flow` result into a plain value: a `?`-driven
/// early `return` becomes the body's value; a real error propagates.
fn finish(r: Result<Value, Flow>) -> Result<Value, RuntimeError> {
    match r {
        Ok(v) => Ok(v),
        Err(Flow::Return(v)) => Ok(v),
        Err(Flow::Err(e)) => Err(e),
        Err(Flow::Break | Flow::Continue) => {
            Err(RuntimeError { message: "`break`/`continue` outside a loop".into() })
        }
    }
}

/// Lexically scoped variable bindings. Functions are not closures: a call
/// starts a fresh `Env` so a function body sees only its parameters and the
/// global function table.
enum Assign {
    Done,
    Immutable,
    Unbound,
}

#[derive(Default, Debug, Clone, PartialEq)]
pub struct Env {
    /// A stack of scopes; each scope is a small list of bindings carrying whether
    /// the binding is mutable (`var`/`own`) or not (`let`). Scopes are
    /// usually tiny (a couple of params/locals), so a linear scan beats a
    /// `HashMap`'s allocation and hashing on the hot call path. Lookups scan most
    /// recent first, so a later `let` shadows an earlier one.
    scopes: Vec<Vec<(String, Value, bool)>>,
}

impl Env {
    fn new() -> Self {
        Self {
            scopes: vec![Vec::new()],
        }
    }
    fn push(&mut self) {
        self.scopes.push(Vec::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    fn define(&mut self, name: String, value: Value, mutable: bool) {
        self.scopes.last_mut().unwrap().push((name, value, mutable));
    }
    fn get(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            for (n, v, _) in scope.iter().rev() {
                if n == name {
                    return Some(v);
                }
            }
        }
        None
    }
    /// Reassign an existing binding in place; rejects immutable (`let`) bindings.
    fn assign(&mut self, name: &str, value: Value) -> Assign {
        for scope in self.scopes.iter_mut().rev() {
            for (n, slot, mutable) in scope.iter_mut().rev() {
                if n == name {
                    if *mutable {
                        *slot = value;
                        return Assign::Done;
                    }
                    return Assign::Immutable;
                }
            }
        }
        Assign::Unbound
    }

    /// A pruned snapshot for closure capture: only bindings whose names appear
    /// in the closure body (`mentioned`), innermost occurrence winning — the
    /// same resolution `get`'s reverse scan produces. Observationally identical
    /// to cloning the whole environment (a name the body never mentions can
    /// never be looked up), without the O(everything) copy per closure created
    /// or applied.
    fn capture(&self, mentioned: &HashSet<String>) -> Env {
        let mut scope: Vec<(String, Value, bool)> = Vec::new();
        for s in &self.scopes {
            for (n, v, m) in s {
                if mentioned.contains(n.as_str()) {
                    match scope.iter_mut().find(|(en, _, _)| en == n) {
                        Some(slot) => *slot = (n.clone(), v.clone(), *m),
                        None => scope.push((n.clone(), v.clone(), *m)),
                    }
                }
            }
        }
        Env { scopes: vec![scope] }
    }

    /// Mutable access to a binding's slot plus its mutability, innermost first
    /// (the same binding `assign` would write).
    fn slot_mut(&mut self, name: &str) -> Option<(&mut Value, bool)> {
        for scope in self.scopes.iter_mut().rev() {
            for (n, slot, mutable) in scope.iter_mut().rev() {
                if n == name {
                    return Some((slot, *mutable));
                }
            }
        }
        None
    }
}

/// Walk every identifier an expression can possibly resolve through the
/// environment: variable reads, call names (a closure in a variable), method
/// names, assignment targets. Binders (params, patterns, loop variables) are
/// deliberately included-by-omission — we never report them, but we DO walk
/// the scopes they govern, so the scan over-approximates. Over-approximation
/// is safe for both users (closure capture keeps an extra binding; the
/// in-place fast path stands down).
fn idents_in_expr(e: &Expr, f: &mut dyn FnMut(&str)) {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) => {}
        // No identifier children (its holes are unparsed source); gone before this runs.
        Expr::TaggedLit { .. } => {}
        Expr::Var(n) => f(n),
        Expr::List(items) | Expr::Tuple(items) => {
            for it in items {
                idents_in_expr(it, f);
            }
        }
        Expr::Call { name, args } => {
            f(name);
            for a in args {
                idents_in_expr(a, f);
            }
        }
        Expr::MethodCall { receiver, method, args } => {
            f(method);
            idents_in_expr(receiver, f);
            for a in args {
                idents_in_expr(a, f);
            }
        }
        Expr::Apply { func, args } => {
            idents_in_expr(func, f);
            for a in args {
                idents_in_expr(a, f);
            }
        }
        Expr::Ctor { args, .. } => {
            for a in args {
                idents_in_expr(a, f);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
            idents_in_expr(expr, f)
        }
        Expr::Field { base, .. } => idents_in_expr(base, f),
        Expr::Lambda { body, .. } => idents_in_block(body, f),
        Expr::RecordUpdate { base, fields } => {
            idents_in_expr(base, f);
            for (_, fe) in fields {
                idents_in_expr(fe, f);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, fe) in fields {
                idents_in_expr(fe, f);
            }
            if let Some(s) = spread {
                idents_in_expr(s, f);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            idents_in_expr(lhs, f);
            idents_in_expr(rhs, f);
        }
        Expr::If { cond, then_block, else_block } => {
            idents_in_expr(cond, f);
            idents_in_block(then_block, f);
            if let Some(b) = else_block {
                idents_in_block(b, f);
            }
        }
        Expr::Match { scrutinee, arms } => {
            idents_in_expr(scrutinee, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    idents_in_expr(g, f);
                }
                idents_in_expr(&arm.body, f);
            }
        }
        Expr::Block(b) => idents_in_block(b, f),
        Expr::While { cond, body } => {
            idents_in_expr(cond, f);
            idents_in_block(body, f);
        }
        Expr::For { iter, body, .. } => {
            idents_in_expr(iter, f);
            idents_in_block(body, f);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            idents_in_expr(scrutinee, f);
            idents_in_block(body, f);
        }
        Expr::Range { lo, hi, .. } => {
            idents_in_expr(lo, f);
            idents_in_expr(hi, f);
        }
        Expr::Index { base, index } => {
            idents_in_expr(base, f);
            idents_in_expr(index, f);
        }
    }
}

fn idents_in_block(b: &Block, f: &mut dyn FnMut(&str)) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::LetTuple { value, .. } => idents_in_expr(value, f),
            Stmt::Assign { name, value } => {
                f(name);
                idents_in_expr(value, f);
            }
            Stmt::Return(opt) => {
                if let Some(e) = opt {
                    idents_in_expr(e, f);
                }
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::Expr(e) | Stmt::Yield(e) => idents_in_expr(e, f),
        }
    }
}

/// Does the expression mention `name` anywhere it could resolve through the
/// environment? Conservative (over-approximates); used to guard the in-place
/// accumulation fast path.
fn expr_mentions(e: &Expr, name: &str) -> bool {
    let mut found = false;
    idents_in_expr(e, &mut |n| {
        if n == name {
            found = true;
        }
    });
    found
}

/// If `e` is a `<>` chain whose leftmost operand is exactly `Var(name)`
/// (`name + a + b` parses left-associated), return the right operands in
/// evaluation order; otherwise None.
fn concat_spine<'a>(mut e: &'a Expr, name: &str) -> Option<Vec<&'a Expr>> {
    let mut rights = Vec::new();
    loop {
        match e {
            Expr::Binary { op: BinOp::Add, lhs, rhs } => {
                rights.push(&**rhs);
                e = lhs;
            }
            Expr::Var(v) if v == name => {
                rights.reverse();
                return Some(rights);
            }
            _ => return None,
        }
    }
}

pub struct Interpreter {
    // `Rc` so a call clones a pointer, not the whole function AST (this is the
    // hot path for recursion).
    functions: HashMap<String, Rc<Function>>,
    /// Host directory the root `Dir` capability is rooted at (handle 0 / the
    /// first `Dir` parameter of `main`).
    root: PathBuf,
    /// Additional `Dir` grants for a `main` taking several `Dir` params: the
    /// i-th `Dir` param (i>=1) is rooted at `dir_roots[i-1]`, falling back to
    /// `root`. Empty for the common single-`Dir` case. Mirrors the compiled
    /// backend's `Capabilities::dir_roots`. See rfcs/0004-self-hosted-cli.md.
    dir_roots: Vec<PathBuf>,
    /// Direct `File` grants (RFC-0012): the i-th `File` param of `main` is the
    /// i-th path here. Read/write is the param's compile-time right, so these are
    /// plain paths. Mirrors the compiled backend's `Capabilities::file_grants`.
    file_grants: Vec<PathBuf>,
    /// Allow-list backing the root `Net` capability.
    net_allow: Vec<String>,
    /// Ed25519 seed backing the root `Secret` capability, if the host granted
    /// one. A `main` that declares a `Secret` parameter requires this.
    signing_key: Option<[u8; 32]>,
    /// Named secrets backing the `SecretStore` capability (from
    /// `--secret`/`--secret-file`/`--signing-key`). `secret_store.get(name)`.
    secrets: std::collections::BTreeMap<String, Vec<u8>>,
    /// Open sockets, indexed by `Value::Socket` handle. Each is a plain or TLS byte
    /// stream behind one `dyn Stream` (RFC-0009 terminates `tls:` host-side, so
    /// `send_line`/`recv_line` operate on either without knowing which).
    sockets: Vec<BufReader<Box<dyn Stream>>>,
    /// Listening server sockets, indexed by `Value::Listener` handle.
    listeners: Vec<TcpListener>,
    /// Record constructor name -> ordered field names, for `value.field` access.
    record_fields: HashMap<String, Vec<String>>,
    /// Evaluation-step counter and ceiling. Unlike the runtime's epoch
    /// preemption, the tree-walker can't be interrupted, so a `while true {}`
    /// would hang the host — this bounds total work and errors out instead.
    steps: u64,
    step_limit: u64,
    /// Source line of the statement currently executing, attached to runtime
    /// errors for diagnostics. 0 means "no line known".
    cur_line: u32,
    /// The function currently executing (after linking, `module.func`), attached
    /// to runtime errors. Empty means "unknown".
    cur_fn: String,
    /// When user code calls into a `std/testing` assertion, the caller's
    /// (function, line). A FAILED assertion is reported here — the user's call
    /// site — instead of the `fail` line buried inside std/testing. Only
    /// assertion failures consult this; a genuine runtime error still names the
    /// innermost frame (a tested guarantee).
    assert_site: Option<(String, u32)>,
    /// Current call nesting and its ceiling. The tree-walker recurses in Rust,
    /// so unbounded recursion would overflow the (large) stack; this errors
    /// gracefully well before that.
    depth: u32,
    depth_limit: u32,
    pub output: Vec<String>,
}

/// Maximum call-nesting depth. Comfortably below what the 4 GiB interpreter
/// thread can hold (debug frames are large), but far deeper than any reasonable
/// program recurses.
const DEFAULT_DEPTH_LIMIT: u32 = 25_000;

/// Default ceiling on evaluation steps for one program run. High enough that no
/// realistic program reaches it, low enough that an infinite loop fails in
/// seconds rather than hanging forever.
// Programs run UNBUDGETED: the compiled backend has no step ceiling, so an
// interpreter ceiling would make `parity` diverge on legitimately large (but
// finite) workloads — an artificial difference, the exact thing parity
// exists to catch. An infinite loop hangs on both backends alike. The budget
// survives where termination is part of the CONTRACT: `comptime:` blocks
// (a compile must finish) and tests that pin the runaway-loop diagnostic.
const DEFAULT_STEP_LIMIT: u64 = u64::MAX;

/// The step budget for `comptime:` blocks — compile-time execution must
/// terminate, so the contract keeps a (generous) ceiling there.
pub const COMPTIME_STEP_LIMIT: u64 = 500_000_000;

impl Interpreter {
    pub fn new(module: Module) -> Self {
        let mut functions = HashMap::new();
        let mut record_fields = HashMap::new();
        for item in module.items {
            match item {
                Item::Function(f) => {
                    functions.insert(f.name.clone(), Rc::new(f));
                }
                // Types are erased at runtime, except a record's field names,
                // which map `value.field` to a position in the constructor.
                Item::Type(t) => {
                    for v in &t.variants {
                        if !v.field_names.is_empty() {
                            record_fields.insert(v.name.clone(), v.field_names.clone());
                        }
                    }
                }
                // Desugared to functions by `traits::lower` before this point;
                // constants are inlined by `witchy_syntax::consts`.
                Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
            }
        }
        Self {
            functions,
            root: PathBuf::from("."),
            dir_roots: Vec::new(),
            file_grants: Vec::new(),
            net_allow: Vec::new(),
            signing_key: None,
            secrets: std::collections::BTreeMap::new(),
            sockets: Vec::new(),
            listeners: Vec::new(),
            record_fields,
            steps: 0,
            step_limit: DEFAULT_STEP_LIMIT,
            cur_line: 0,
            cur_fn: String::new(),
            assert_site: None,
            depth: 0,
            depth_limit: DEFAULT_DEPTH_LIMIT,
            output: Vec::new(),
        }
    }

    /// Mint the root capability for a `main` parameter of the given type. This
    /// is where authority enters the program — `main` is the root entrypoint.
    fn root_cap_for(&self, ty: &Option<Type>) -> Result<Value, RuntimeError> {
        match ty {
            Some(Type::Named(n, _)) if n == "Console" => Ok(Value::Cap(Capability::Console)),
            Some(Type::Named(n, _)) if n == "Clock" => Ok(Value::Cap(Capability::Clock)),
            Some(Type::Named(n, _)) if n == "Env" => Ok(Value::Cap(Capability::Env)),
            Some(Type::Named(n, _)) if n == "Dir" => Ok(Value::Dir(self.root.clone())),
            Some(Type::Named(n, _)) if n == "Net" => Ok(Value::Net(self.net_allow.clone())),
            Some(Type::Named(n, _)) if n == "Exec" => Ok(Value::Cap(Capability::Exec)),
            Some(Type::Named(n, _)) if n == "Secret" => match self.signing_key {
                Some(seed) => Ok(Value::Secret(seed.to_vec())),
                None => err("`main` requires a `Secret`, but the host granted none (provide `--signing-key <hex-seed-file>`)"),
            },
            Some(Type::Named(n, _)) if n == "SecretStore" => Ok(Value::SecretStore(self.secrets.clone())),
            other => {
                let found = match other {
                    Some(t) => format!("`{}`", witchy_syntax::format::type_str(t)),
                    None => "no type annotation".to_string(),
                };
                err(format!(
                    "`main` parameters must be capabilities (Console, Clock, Env, Dir, Net, Exec, Secret) or `List(String)` for command-line args; got {found}"
                ))
            }
        }
    }

    /// Mint a build-time capability for a `build` parameter, from the confined
    /// grants the build driver was handed. This is where build-time authority
    /// enters — never `main`. A demanded cap with no matching grant is an error
    /// (safe by default: only `BuildOut` is supplied unconditionally).
    fn mint_build_cap(&self, ty: &Option<Type>, grants: &BuildGrants) -> Result<Value, RuntimeError> {
        match ty {
            Some(Type::Named(n, _)) if n == "BuildOut" => {
                Ok(Value::Build(BuildCap::Out(grants.out_dir.clone())))
            }
            Some(Type::Named(n, _)) if n == "BuildRead" => {
                if grants.read_roots.is_empty() {
                    return err("build step demands `BuildRead` but no read grant was provided");
                }
                Ok(Value::Build(BuildCap::Read(grants.read_roots.clone())))
            }
            Some(Type::Named(n, _)) if n == "BuildEnv" => {
                Ok(Value::Build(BuildCap::Env(grants.env_keys.clone())))
            }
            Some(Type::Named(n, _)) if n == "BuildNet" => {
                Ok(Value::Build(BuildCap::Net(grants.net_hosts.clone())))
            }
            Some(Type::Named(n, _)) if n == "BuildExec" => {
                Ok(Value::Build(BuildCap::Exec(grants.exec_tools.clone())))
            }
            other => {
                let found = match other {
                    Some(t) => format!("`{}`", witchy_syntax::format::type_str(t)),
                    None => "no type annotation".to_string(),
                };
                err(format!(
                    "`build` parameters must be build capabilities (BuildOut, BuildRead, BuildEnv, BuildNet, BuildExec); got {found}"
                ))
            }
        }
    }

    /// Call a top-level function by name with already-evaluated arguments.
    pub fn call(&mut self, name: &str, args: Vec<Value>) -> Result<Value, RuntimeError> {
        if let Some(v) = self.call_builtin(name, &args)? {
            return Ok(v);
        }
        let Some(func) = self.functions.get(name).cloned() else {
            return err(format!("call to unknown function `{name}`"));
        };
        if func.params.len() != args.len() {
            return err(format!(
                "`{name}` expects {} argument(s) but got {}",
                func.params.len(),
                args.len()
            ));
        }
        let mut env = Env::new();
        for (param, value) in func.params.iter().zip(args) {
            env.define(
                param.name.clone(),
                value,
                param.convention.binds_mutable(),
            );
        }
        let prev = std::mem::replace(&mut self.cur_fn, name.to_string());
        self.depth += 1;
        if self.depth > self.depth_limit {
            self.depth -= 1;
            return err("call stack too deep (possible infinite recursion)");
        }
        let result = finish(self.eval_block(&func.body, &mut env));
        self.depth -= 1;
        // On success, restore the caller's name; on error, keep this one so the
        // innermost failing function is reported.
        if result.is_ok() {
            self.cur_fn = prev;
        }
        result
    }

    /// Record the user→testing crossing: when a non-`testing` function calls a
    /// `testing.*` assertion, remember the caller's (function, line) so a failed
    /// assertion is reported at the user's call site rather than at the `fail`
    /// buried in std/testing. Crossings *within* testing don't overwrite it.
    fn note_assert_crossing(&mut self, callee: &str) {
        if callee.starts_with("testing.") && !self.cur_fn.starts_with("testing.") {
            self.assert_site = Some((self.cur_fn.clone(), self.cur_line));
        }
    }

    /// Apply a closure to already-evaluated arguments. The closure runs in its
    /// captured environment (plus a fresh scope for the parameters), and its body
    /// is a function boundary, so a `?` inside it returns from the closure.
    fn apply_closure(&mut self, clo: Value, argvals: Vec<Value>) -> Result<Value, Flow> {
        let Value::Closure { params, body, env } = clo else {
            return err("attempted to call a non-function value");
        };
        if params.len() != argvals.len() {
            return err(format!(
                "function expects {} argument(s) but got {}",
                params.len(),
                argvals.len()
            ));
        }
        let mut cenv = *env;
        cenv.push();
        for (p, v) in params.iter().zip(argvals) {
            cenv.define(p.name.clone(), v, p.convention.binds_mutable());
        }
        self.depth += 1;
        if self.depth > self.depth_limit {
            self.depth -= 1;
            return err("call stack too deep (possible infinite recursion)");
        }
        let result = self.eval_block(&body, &mut cenv);
        self.depth -= 1;
        match result {
            Ok(v) | Err(Flow::Return(v)) => Ok(v),
            Err(e @ Flow::Err(_)) => Err(e),
            Err(Flow::Break | Flow::Continue) => err("`break`/`continue` outside a loop"),
        }
    }

    /// Evaluate a function call expression, honoring parameter conventions:
    /// `var` arguments must be mutable variables and are written back after
    /// the call returns (Hylo-style move-in / move-out).
    fn eval_call(&mut self, name: &str, args: &[Expr], env: &mut Env) -> Result<Value, Flow> {
        // Record an assertion call SITE *before* evaluating arguments — nested
        // calls in the arguments move `cur_line`, so capturing it later (e.g. once
        // we're inside the callee) would report the wrong line.
        self.note_assert_crossing(name);
        let argvals = args
            .iter()
            .map(|a| self.eval(a, env))
            .collect::<Result<Vec<_>, _>>()?;
        // `dict.update(dict, key, default, f)`: a single-lookup upsert. Handled here
        // (not in the pure builtin table) because it applies the updater closure
        // `f` to the current value — or `default` when the key is absent — which
        // needs the interpreter. Arguments are evaluated exactly once (above).
        if name == "dict.update" && argvals.len() == 4 {
            let Value::Dict(entries) = &argvals[0] else {
                return err("update expects a Dict as its first argument");
            };
            let mut out = entries.clone();
            let key = &argvals[1];
            let current = out
                .iter()
                .find(|(ek, _)| ek == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| argvals[2].clone());
            let new_v = self.apply_closure(argvals[3].clone(), vec![current])?;
            match out.iter_mut().find(|(ek, _)| ek == key) {
                Some(slot) => slot.1 = new_v,
                None => out.push((argvals[1].clone(), new_v)),
            }
            return Ok(Value::Dict(out));
        }
        // A local variable holding a function value (a closure): apply it.
        if let Some(Value::Closure { .. }) = env.get(name) {
            let clo = env.get(name).unwrap().clone();
            return self.apply_closure(clo, argvals);
        }
        if let Some(v) = self.call_builtin(name, &argvals)? {
            return Ok(v);
        }
        let Some(func) = self.functions.get(name).cloned() else {
            return err(format!("call to unknown function `{name}`"));
        };
        if func.params.len() != argvals.len() {
            return err(format!(
                "`{name}` expects {} argument(s) but got {}",
                func.params.len(),
                argvals.len()
            ));
        }
        let mut fenv = Env::new();
        let mut writebacks: Vec<(String, String)> = Vec::new();
        for (i, param) in func.params.iter().enumerate() {
            fenv.define(
                param.name.clone(),
                argvals[i].clone(),
                param.convention.binds_mutable(),
            );
            if matches!(param.convention, Convention::Var) {
                match &args[i] {
                    Expr::Var(caller) => writebacks.push((caller.clone(), param.name.clone())),
                    _ => {
                        return err(format!(
                            "`var` argument to `{name}` must be a mutable variable"
                        ))
                    }
                }
            }
        }
        // The callee's own `?` early-return stops here; it becomes the call's
        // value rather than propagating into the caller.
        let prev = std::mem::replace(&mut self.cur_fn, name.to_string());
        self.depth += 1;
        if self.depth > self.depth_limit {
            self.depth -= 1;
            return err("call stack too deep (possible infinite recursion)");
        }
        let block_result = self.eval_block(&func.body, &mut fenv);
        self.depth -= 1;
        let result = match block_result {
            Ok(v) => v,
            Err(Flow::Return(v)) => v,
            // On error keep `cur_fn = name` so the innermost frame is reported.
            Err(e @ Flow::Err(_)) => return Err(e),
            Err(Flow::Break | Flow::Continue) => {
                return err("`break`/`continue` outside a loop")
            }
        };
        for (caller, param_name) in writebacks {
            let final_v = fenv.get(&param_name).cloned().unwrap();
            match env.assign(&caller, final_v) {
                Assign::Done => {}
                Assign::Immutable => {
                    return err(format!(
                        "`var` argument `{caller}` must be a mutable variable (it is immutable)"
                    ))
                }
                Assign::Unbound => {
                    return err(format!("`var` argument `{caller}` must be a local variable"))
                }
            }
        }
        self.cur_fn = prev;
        Ok(result)
    }

    fn call_builtin(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, RuntimeError> {
        // `secret_store.get(name)` — a named lookup into the granted store. Handled
        // here (not in `native`) because a `SecretStore` is not a `NativeValue`.
        if name == "secretstore.get" {
            return match args {
                [Value::SecretStore(map), Value::Str(key)] => Ok(Some(match map.get(key) {
                    Some(bytes) => Value::Ctor {
                        name: "Some".into(),
                        fields: vec![Value::Secret(bytes.clone())],
                    },
                    None => Value::Ctor { name: "None".into(), fields: Vec::new() },
                })),
                _ => err("secretstore.get expects (SecretStore, name)"),
            };
        }
        // `__try_ctx(value, msg)` — the `e ? "msg"` desugar. Turn the operand (an
        // `Option` or a `Result`) into a `Result(T, String)` carrying `msg`: `None`
        // -> `Err(msg)`, a `Result`'s `Err(e)` -> `Err("msg: e")` (e is a String),
        // and `Some(x)`/`Ok(x)` -> `Ok(x)`. The enclosing `?` then unwraps it.
        if name == "__try_ctx" {
            return match args {
                [val, Value::Str(msg)] => {
                    let out = match val {
                        Value::Ctor { name: c, fields } if c == "Some" || c == "Ok" => {
                            Value::Ctor { name: "Ok".into(), fields: fields.clone() }
                        }
                        Value::Ctor { name: c, .. } if c == "None" => {
                            Value::Ctor { name: "Err".into(), fields: vec![Value::Str(msg.clone())] }
                        }
                        Value::Ctor { name: c, fields } if c == "Err" => {
                            let inner = match fields.first() {
                                Some(Value::Str(e)) => e.clone(),
                                Some(other) => format!("{other}"),
                                None => String::new(),
                            };
                            Value::Ctor {
                                name: "Err".into(),
                                fields: vec![Value::Str(format!("{msg}: {inner}"))],
                            }
                        }
                        _ => return err("`? \"msg\"` applies to an Option or Result"),
                    };
                    Ok(Some(out))
                }
                _ => err("__try_ctx expects (value, message)"),
            };
        }
        // `secret_store.require(name)` — a required secret: the `Secret` directly,
        // or a loud error if absent (a configuration mistake, not an `Option`).
        if name == "secretstore.require" {
            return match args {
                [Value::SecretStore(map), Value::Str(key)] => match map.get(key) {
                    Some(bytes) => Ok(Some(Value::Secret(bytes.clone()))),
                    None => err(format!("required secret `{key}` was not granted")),
                },
                _ => err("secretstore.require expects (SecretStore, name)"),
            };
        }
        // `crypto.reveal` is gated: a `Secret` equal to the signing key (the bare
        // `Secret` / `require("signing")`) is sign-only and must not be revealed —
        // only named value-secrets are. Mirrors the WASM host (`host_crypto_reveal_len`)
        // through the one shared identity rule so the backends can't drift.
        if name == "crypto.reveal" {
            if let [Value::Secret(bytes)] = args {
                if witchy_caps::capabilities::secret_is_signing_key(
                    self.signing_key.as_ref().map(|s| s.as_slice()),
                    bytes,
                ) {
                    return err("the signing key is not revealable; use crypto.sign / crypto.public_key");
                }
            }
        }
        // Native stdlib modules (crypto, …): pure, stateless functions reached by
        // their qualified name (`crypto.sha256`). Dispatched through the registry
        // so adding one needs no change here — see `src/native.rs`.
        if let Some(f) = witchy_runtime::native::lookup(name) {
            // `native` speaks `NativeValue` (it doesn't depend on the interpreter);
            // bridge our `Value` across the call.
            let nargs = args
                .iter()
                .map(value_to_native)
                .collect::<Result<Vec<_>, RuntimeError>>()?;
            let nresult = f(&nargs).map_err(|e| RuntimeError { message: e.message })?;
            return Ok(Some(native_to_value(nresult)));
        }
        let one = |args: &[Value]| -> Result<Value, RuntimeError> {
            match args {
                [v] => Ok(v.clone()),
                _ => err(format!("`{name}` expects exactly one argument")),
            }
        };
        match name {
            // Effectful: requires the Console capability as its first argument.
            "print" => match args {
                [Value::Cap(Capability::Console), msg] => {
                    // Each print is one output line; the trailing newline is the
                    // line terminator. Strip it to match the WASM host
                    // (`host_print` in runtime.rs), so the backends agree when a
                    // printed string ends in `\n` (e.g. `s + "\n"`).
                    self.output.push(msg.to_string().trim_end_matches('\n').to_string());
                    Ok(Some(Value::Nil))
                }
                [_, _] => err("print requires a Console capability as its first argument"),
                _ => err("print expects a Console capability and a message: print(console, msg)"),
            },
            // Pure builtins need no capability.
            "__render" => Ok(Some(Value::Str(one(args)?.to_string()))),
            // String stdlib.
            "string.length" => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Int(s.len() as i64))),
                other => err(format!("string_length expects a String, got `{other}`")),
            },
            // The number of Unicode scalars — the character count, as opposed to
            // `string_length`'s byte count (they agree for ASCII).
            "string.char_count" => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Int(s.chars().count() as i64))),
                other => err(format!("char_count expects a String, got `{other}`")),
            },
            // ASCII case mapping (a-z <-> A-Z); non-ASCII bytes are unchanged.
            // Deliberately ASCII-only so the WASM backend can match it byte-for-
            // byte (full Unicode case folding would need large tables).
            "string.to_upper" => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Str(s.to_ascii_uppercase()))),
                other => err(format!("to_upper expects a String, got `{other}`")),
            },
            "string.to_lower" => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Str(s.to_ascii_lowercase()))),
                other => err(format!("to_lower expects a String, got `{other}`")),
            },
            // Abort with a message — the error-raising primitive behind
            // `std/testing`'s assertions (a deliberate, loud failure).
            "fail" => match one(args)? {
                Value::Str(msg) => {
                    // When this `fail` is the one behind a `std/testing` assertion
                    // that user code invoked, retarget the reported location to the
                    // user's call site (recorded at the crossing). A direct `fail`
                    // in user code, or any non-assertion runtime error, is left to
                    // the default innermost-frame reporting.
                    if self.cur_fn.starts_with("testing.") {
                        if let Some((func, line)) = self.assert_site.take() {
                            self.cur_fn = func;
                            self.cur_line = line;
                        }
                    }
                    Err(RuntimeError { message: msg.clone() })
                }
                other => err(format!("fail expects a String message, got `{other}`")),
            },
            "string.trim" => match one(args)? {
                // ASCII whitespace only — exactly the byte set the WASM `$is_ws`
                // helper strips (space, tab, LF, VT, FF, CR). Rust's `str::trim`
                // would additionally strip Unicode whitespace (NBSP, …), which the
                // compiled backend does not, so we pin both to this set to keep the
                // backends in agreement (consistent with ASCII `to_upper`/`to_lower`).
                Value::Str(s) => {
                    let trimmed =
                        s.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
                    Ok(Some(Value::Str(trimmed.to_string())))
                }
                other => err(format!("trim expects a String, got `{other}`")),
            },
            "string.starts_with" => match args {
                [Value::Str(s), Value::Str(prefix)] => {
                    Ok(Some(Value::Bool(s.starts_with(prefix.as_str()))))
                }
                _ => err("starts_with expects two Strings"),
            },
            "string.contains" => match args {
                [Value::Str(s), Value::Str(sub)] => {
                    Ok(Some(Value::Bool(s.contains(sub.as_str()))))
                }
                _ => err("contains expects two Strings"),
            },
            // Split on a separator into a list of pieces (the separator itself is
            // dropped); the empty separator yields the whole string unchanged.
            "string.split" => match args {
                [Value::Str(s), Value::Str(sep)] => {
                    let parts: Vec<Value> = if sep.is_empty() {
                        vec![Value::Str(s.clone())]
                    } else {
                        s.split(sep.as_str()).map(|p| Value::Str(p.to_string())).collect()
                    };
                    Ok(Some(Value::List(parts)))
                }
                _ => err("split expects two Strings"),
            },
            // The characters of a string, each as a single-char String (one pass).
            "string.chars" => match one(args)? {
                Value::Str(s) => {
                    Ok(Some(Value::List(s.chars().map(|c| Value::Str(c.to_string())).collect())))
                }
                _ => err("string_chars expects a String"),
            },
            "string.replace" => match args {
                [Value::Str(s), Value::Str(from), Value::Str(to)] => {
                    Ok(Some(Value::Str(s.replace(from.as_str(), to.as_str()))))
                }
                _ => err("replace expects three Strings"),
            },
            "string.ends_with" => match args {
                [Value::Str(s), Value::Str(suffix)] => {
                    Ok(Some(Value::Bool(s.ends_with(suffix.as_str()))))
                }
                _ => err("ends_with expects two Strings"),
            },
            // Char index of the first occurrence of `sub`, or -1 if absent.
            "string.index_of" => match args {
                [Value::Str(s), Value::Str(sub)] => {
                    let idx = s
                        .find(sub.as_str())
                        .map(|byte| s[..byte].chars().count() as i64)
                        .unwrap_or(-1);
                    Ok(Some(Value::Int(idx)))
                }
                _ => err("index_of expects two Strings"),
            },
            // Characters in the half-open range [start, end), clamped to bounds
            // (counted by Unicode scalar, so slicing never splits a character).
            "string.substring" => match args {
                [Value::Str(s), Value::Int(start), Value::Int(end)] => {
                    let chars: Vec<char> = s.chars().collect();
                    let lo = (*start).max(0) as usize;
                    let hi = (*end).max(0) as usize;
                    let lo = lo.min(chars.len());
                    let hi = hi.min(chars.len());
                    let out: String = if lo < hi {
                        chars[lo..hi].iter().collect()
                    } else {
                        String::new()
                    };
                    Ok(Some(Value::Str(out)))
                }
                _ => err("substring expects a String and two Int indices"),
            },
            // Conversions.
            "math.to_float" => match one(args)? {
                Value::Int(n) => Ok(Some(Value::Float(n as f64))),
                other => err(format!("int_to_float expects an Int, got `{other}`")),
            },
            "math.to_int" => match one(args)? {
                Value::Float(x) => Ok(Some(Value::Int(x as i64))),
                other => err(format!("float_to_int expects a Float, got `{other}`")),
            },
            // Duration <-> Int(ms): a Duration is an Int(ms) at runtime, so both
            // directions are the identity.
            "int_to_duration" | "duration_to_int" => match one(args)? {
                Value::Int(n) => Ok(Some(Value::Int(n))),
                other => err(format!("{name} expects an Int/Duration, got `{other}`")),
            },
            "math.sqrt" => match one(args)? {
                Value::Float(x) => Ok(Some(Value::Float(x.sqrt()))),
                other => err(format!("sqrt expects a Float, got `{other}`")),
            },
            "string.to_int" => match one(args)? {
                Value::Str(s) => match s.trim().parse::<i64>() {
                    Ok(n) => Ok(Some(Value::Int(n))),
                    Err(_) => err(format!("cannot parse `{s}` as an Int")),
                },
                other => err(format!("string_to_int expects a String, got `{other}`")),
            },
            "list.length" => match args {
                [Value::List(items)] => Ok(Some(Value::Int(items.len() as i64))),
                _ => err("length expects a list"),
            },
            "list.at" => match args {
                [Value::List(items), Value::Int(i)] => match items.get(*i as usize) {
                    Some(v) => Ok(Some(v.clone())),
                    None => err(format!("list index {i} out of bounds (length {})", items.len())),
                },
                _ => err("at expects a list and an Int index"),
            },
            // Return a new list with `x` appended (lists are values, so this does
            // not mutate the original).
            "list.push" => match args {
                [Value::List(items), x] => {
                    let mut out = items.clone();
                    out.push(x.clone());
                    Ok(Some(Value::List(out)))
                }
                _ => err("push expects a list and a value"),
            },
            // Return a new list that is the two given lists joined.
            "list.concat" => match args {
                [Value::List(a), Value::List(b)] => {
                    let mut out = a.clone();
                    out.extend(b.clone());
                    Ok(Some(Value::List(out)))
                }
                _ => err("concat expects two lists"),
            },
            // --- Dict: an immutable association map ---
            "dict.new" => match args {
                [] => Ok(Some(Value::Dict(Vec::new()))),
                _ => err("dict_new takes no arguments"),
            },
            // Return a new dict with `k` set to `v` (replacing any existing entry).
            "dict.insert" => match args {
                [Value::Dict(entries), k, v] => {
                    let mut out = entries.clone();
                    match out.iter_mut().find(|(ek, _)| ek == k) {
                        Some(slot) => slot.1 = v.clone(),
                        None => out.push((k.clone(), v.clone())),
                    }
                    Ok(Some(Value::Dict(out)))
                }
                _ => err("insert expects a Dict, a key, and a value"),
            },
            // Value for `k`, or `default` if absent.
            "dict.get_or" => match args {
                [Value::Dict(entries), k, default] => {
                    let found = entries.iter().find(|(ek, _)| ek == k);
                    Ok(Some(found.map(|(_, v)| v.clone()).unwrap_or_else(|| default.clone())))
                }
                _ => err("get_or expects a Dict, a key, and a default value"),
            },
            "dict.contains_key" => match args {
                [Value::Dict(entries), k] => {
                    Ok(Some(Value::Bool(entries.iter().any(|(ek, _)| ek == k))))
                }
                _ => err("has expects a Dict and a key"),
            },
            // A new dict with `k` (and its value) removed; unchanged if absent.
            "dict.remove" => match args {
                [Value::Dict(entries), k] => {
                    let out: Vec<(Value, Value)> =
                        entries.iter().filter(|(ek, _)| ek != k).cloned().collect();
                    Ok(Some(Value::Dict(out)))
                }
                _ => err("remove expects a Dict and a key"),
            },
            "dict.keys" => match args {
                [Value::Dict(entries)] => {
                    Ok(Some(Value::List(entries.iter().map(|(k, _)| k.clone()).collect())))
                }
                _ => err("keys expects a Dict"),
            },
            "dict.values" => match args {
                [Value::Dict(entries)] => {
                    Ok(Some(Value::List(entries.iter().map(|(_, v)| v.clone()).collect())))
                }
                _ => err("values expects a Dict"),
            },
            // Each entry as a `(key, value)` tuple, in insertion order.
            "dict.pairs" => match args {
                [Value::Dict(entries)] => Ok(Some(Value::List(
                    entries
                        .iter()
                        .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                        .collect(),
                ))),
                _ => err("pairs expects a Dict"),
            },
            "dict.length" => match args {
                [Value::Dict(entries)] => Ok(Some(Value::Int(entries.len() as i64))),
                _ => err("size expects a Dict"),
            },
            // Filesystem capability (cap-std style): attenuate to a subdirectory.
            "subtree" => match args {
                [Value::Dir(base), Value::Str(name)] => {
                    Ok(Some(Value::Dir(resolve(base, name)?)))
                }
                _ => err("subtree expects a Dir and a name"),
            },
            // RFC-0012 navigation: a `Dir` opens a confined `File`. `read_file`
            // requires the file to exist; `write_file` allows a not-yet-existing target.
            "read_file" => match args {
                [Value::Dir(base), Value::Str(rel)] => Ok(Some(Value::File(resolve(base, rel)?))),
                _ => err("read_file expects a Dir and a relative path"),
            },
            "write_file" => match args {
                [Value::Dir(base), Value::Str(rel)] => {
                    Ok(Some(Value::File(resolve_write(base, rel)?)))
                }
                _ => err("write_file expects a Dir and a relative path"),
            },
            // Spawn a native subprocess. `Exec` is the right to spawn; the
            // executable is named through (and confined to) the `Dir[Read]`, so you
            // can only run a file you can read. The low-level primitive takes argv
            // as a single `\0`-joined string and returns a payload string
            // `"<exit_code>\n<stdout><stderr>"`; the std `exec` module wraps this as
            // `(Int, String)` over a `List(String)`. (One staged-string result, so
            // the compiled backend mirrors `dir_read` exactly — see rfcs/0004.)
            "exec" => match args {
                [Value::Cap(Capability::Exec), Value::Dir(base), Value::Str(path), Value::Str(joined), Value::Str(stdin)] => {
                    let prog = resolve(base, path)?;
                    let argv: Vec<&str> =
                        if joined.is_empty() { Vec::new() } else { joined.split('\0').collect() };
                    use std::io::Write as _;
                    use std::process::{Command, Stdio};
                    let spawned = Command::new(&prog)
                        .args(&argv)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn();
                    let mut child = match spawned {
                        Ok(c) => c,
                        Err(e) => return err(format!("exec failed to spawn `{}`: {e}", prog.display())),
                    };
                    if let Some(mut sin) = child.stdin.take() {
                        if let Err(e) = sin.write_all(stdin.as_bytes()) {
                            return err(format!("exec failed writing stdin to `{}`: {e}", prog.display()));
                        }
                    }
                    let output = match child.wait_with_output() {
                        Ok(o) => o,
                        Err(e) => return err(format!("exec failed running `{}`: {e}", prog.display())),
                    };
                    let code = output.status.code().unwrap_or(-1);
                    let out = String::from_utf8_lossy(&output.stdout);
                    let serr = String::from_utf8_lossy(&output.stderr);
                    Ok(Some(Value::Str(format!("{code}\n{out}{serr}"))))
                }
                _ => err("exec expects (Exec, Dir, path, args, stdin)"),
            },
            // Read a file relative to a Dir capability (confined to its subtree).
            "read" => match args {
                [Value::Dir(base), Value::Str(rel)] => {
                    let path = resolve(base, rel)?;
                    match std::fs::read_to_string(&path) {
                        Ok(contents) => Ok(Some(Value::Str(contents))),
                        Err(e) => err(format!("read failed for `{}`: {e}", path.display())),
                    }
                }
                // A `File` is already a confined path; read it directly (RFC-0012).
                [Value::File(path)] => match std::fs::read_to_string(path) {
                    Ok(contents) => Ok(Some(Value::Str(contents))),
                    Err(e) => err(format!("read failed for `{}`: {e}", path.display())),
                },
                _ => err("read expects a Dir and a relative path, or a File"),
            },
            // Write a file relative to a Dir capability, confined to its subtree
            // (the target may not exist yet, so confinement is checked via its
            // parent directory).
            "write" => match args {
                [Value::Dir(base), Value::Str(rel), Value::Str(contents)] => {
                    let path = resolve_write(base, rel)?;
                    match std::fs::write(&path, contents) {
                        Ok(()) => Ok(Some(Value::Nil)),
                        Err(e) => err(format!("write failed for `{}`: {e}", path.display())),
                    }
                }
                // A `File` is already a confined path; write it directly (RFC-0012).
                [Value::File(path), Value::Str(contents)] => match std::fs::write(path, contents) {
                    Ok(()) => Ok(Some(Value::Nil)),
                    Err(e) => err(format!("write failed for `{}`: {e}", path.display())),
                },
                _ => err("write expects a Dir + path + contents, or a File + contents"),
            },
            // Append to a file (creating it if absent) — `write`'s confinement
            // and rights, without clobbering existing contents.
            "append" => match args {
                [Value::Dir(base), Value::Str(rel), Value::Str(contents)] => {
                    let path = resolve_write(base, rel)?;
                    use std::io::Write as _;
                    let res = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .and_then(|mut f| f.write_all(contents.as_bytes()));
                    match res {
                        Ok(()) => Ok(Some(Value::Nil)),
                        Err(e) => err(format!("append failed for `{}`: {e}", path.display())),
                    }
                }
                _ => err("append expects a Dir, a relative path, and contents"),
            },
            // Whether a file exists within the Dir capability's subtree — total
            // (never errors), so a path outside the subtree, or a missing file,
            // simply reads as `false`. Lets `read` callers avoid a crash.
            "exists" => match args {
                [Value::Dir(base), Value::Str(rel)] => {
                    let ok = resolve(base, rel).map(|p| p.exists()).unwrap_or(false);
                    Ok(Some(Value::Bool(ok)))
                }
                _ => err("exists expects a Dir and a relative path"),
            },
            // Whether a path within the Dir capability's subtree is a directory —
            // total (a path outside the subtree or a non-dir reads as `false`), so
            // a caller can walk `src/**` without tripping over a file.
            "is_dir" => match args {
                [Value::Dir(base), Value::Str(rel)] => {
                    let ok = resolve(base, rel).map(|p| p.is_dir()).unwrap_or(false);
                    Ok(Some(Value::Bool(ok)))
                }
                _ => err("is_dir expects a Dir and a relative path"),
            },
            // List the immediate entries of the Dir capability's own directory, as
            // sorted names (deterministic — `read_dir` order is OS-dependent).
            "list" => match args {
                [Value::Dir(base)] => {
                    let mut names: Vec<String> = match std::fs::read_dir(base) {
                        Ok(entries) => entries
                            .filter_map(|e| e.ok())
                            .filter_map(|e| e.file_name().into_string().ok())
                            .collect(),
                        Err(e) => return err(format!("list failed for `{}`: {e}", base.display())),
                    };
                    names.sort();
                    Ok(Some(Value::List(names.into_iter().map(Value::Str).collect())))
                }
                _ => err("list expects a Dir"),
            },
            // Create a subdirectory within the Dir capability's subtree, confined
            // like `write` (idempotent — succeeds if it already exists).
            "make_dir" => match args {
                [Value::Dir(base), Value::Str(name)] => {
                    let path = resolve_write(base, name)?;
                    match std::fs::create_dir_all(&path) {
                        Ok(()) => Ok(Some(Value::Nil)),
                        Err(e) => err(format!("make_dir failed for `{}`: {e}", path.display())),
                    }
                }
                _ => err("make_dir expects a Dir and a name"),
            },
            // Wall-clock time (milliseconds since the Unix epoch) — requires a
            // `Clock` capability, since reading the real clock is ambient
            // nondeterminism (a side channel), not a pure computation.
            "now" => match args {
                [Value::Cap(Capability::Clock)] => {
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    Ok(Some(Value::Int(ms)))
                }
                _ => err("now expects a Clock"),
            },
            // Read a named environment variable through an `Env` capability:
            // `get_env(env, name) -> Option(String)` (None when unset). Reading the
            // process environment is ambient authority, so it is capability-gated.
            "get_env" => match args {
                [Value::Cap(Capability::Env), Value::Str(name)] => Ok(Some(match std::env::var(name) {
                    Ok(v) => Value::Ctor { name: "Some".into(), fields: vec![Value::Str(v)] },
                    Err(_) => Value::Ctor { name: "None".into(), fields: Vec::new() },
                })),
                _ => err("get_env expects an Env and a variable name"),
            },
            // --- build-time host operations (only reachable from a `build` step) ---
            // Write generated source into the confined per-rune output sandbox.
            "write_out" => match args {
                [Value::Build(BuildCap::Out(base)), Value::Str(rel), Value::Str(contents)] => {
                    let path = resolve_write(base, rel)?;
                    match std::fs::write(&path, contents) {
                        Ok(()) => Ok(Some(Value::Nil)),
                        Err(e) => err(format!("write_out failed for `{}`: {e}", path.display())),
                    }
                }
                _ => err("write_out expects a BuildOut, a relative path, and contents"),
            },
            // Read a project file confined to the BuildRead grant's subtree(s).
            // Each granted root is tried in turn; the first that both confines the
            // path and holds the file wins. Confinement (no `..`, no absolute, no
            // symlink escape) is enforced per root, exactly like a runtime `Dir`.
            "read_build" => match args {
                [Value::Build(BuildCap::Read(roots)), Value::Str(rel)] => {
                    if roots.is_empty() {
                        return err("read_build: this BuildRead grant names no readable root");
                    }
                    let mut last_err = None;
                    for base in roots {
                        match resolve(base, rel) {
                            Ok(path) => match std::fs::read_to_string(&path) {
                                Ok(contents) => return Ok(Some(Value::Str(contents))),
                                Err(e) => last_err = Some(format!("`{}`: {e}", path.display())),
                            },
                            Err(e) => last_err = Some(e.message),
                        }
                    }
                    err(format!(
                        "read_build: `{rel}` not found in any granted read root ({})",
                        last_err.unwrap_or_default()
                    ))
                }
                _ => err("read_build expects a BuildRead and a relative path"),
            },
            // Read a named env var, but only one on the BuildEnv allow-list.
            "get_build_env" => match args {
                [Value::Build(BuildCap::Env(allow)), Value::Str(name)] => {
                    if !allow.iter().any(|k| k == name) {
                        return err(format!(
                            "get_build_env: `{name}` is not in this BuildEnv grant's allow-list"
                        ));
                    }
                    Ok(Some(match std::env::var(name) {
                        Ok(v) => Value::Ctor { name: "Some".into(), fields: vec![Value::Str(v)] },
                        Err(_) => Value::Ctor { name: "None".into(), fields: Vec::new() },
                    }))
                }
                _ => err("get_build_env expects a BuildEnv and a variable name"),
            },
            // Fetch over HTTP at build time — but only from a host on the BuildNet
            // grant's allow-list (`host:port` form, exact match — the same shape as
            // the runtime Net allow-list). Returns the response body. The fetched
            // bytes are data, not authority: anything the build step *generates*
            // from them is re-audited against the locked footprint, and
            // BuildNet/BuildExec use marks the build `pinned-only` for determinism.
            "fetch_build" => match args {
                [Value::Build(BuildCap::Net(allow)), Value::Str(host), Value::Str(path)] => {
                    if !allow.iter().any(|h| h == host) {
                        return err(format!(
                            "fetch_build: `{host}` is not in this BuildNet grant's allow-list"
                        ));
                    }
                    use std::io::{Read, Write};
                    let mut sock = std::net::TcpStream::connect(host.as_str()).map_err(|e| {
                        RuntimeError {
                            message: format!("fetch_build: cannot connect to `{host}`: {e}"),
                        }
                    })?;
                    let hostname = host.split(':').next().unwrap_or(host);
                    let req = format!(
                        "GET {path} HTTP/1.1\r\nHost: {hostname}\r\nConnection: close\r\n\r\n"
                    );
                    sock.write_all(req.as_bytes()).map_err(|e| RuntimeError {
                        message: format!("fetch_build: sending to `{host}`: {e}"),
                    })?;
                    let mut raw = Vec::new();
                    sock.read_to_end(&mut raw).map_err(|e| RuntimeError {
                        message: format!("fetch_build: reading from `{host}`: {e}"),
                    })?;
                    let text = String::from_utf8_lossy(&raw);
                    let body = match text.split_once("\r\n\r\n") {
                        Some((_, b)) => b.to_string(),
                        None => text.into_owned(),
                    };
                    Ok(Some(Value::Str(body)))
                }
                _ => err("fetch_build expects a BuildNet, a host, and a path"),
            },
            // Invoke an external tool — but only one named on the BuildExec grant's
            // allow-list. `input` is fed on stdin; stdout is returned. This is the
            // "native toolchain escape hatch" (§7.1): the allow-list is the
            // confinement, since the tool itself runs as a native process.
            "run_tool" => match args {
                [Value::Build(BuildCap::Exec(allow)), Value::Str(tool), Value::Str(input)] => {
                    if !allow.iter().any(|t| t == tool) {
                        return err(format!(
                            "run_tool: `{tool}` is not in this BuildExec grant's allow-list"
                        ));
                    }
                    use std::io::Write;
                    use std::process::{Command, Stdio};
                    let mut child = Command::new(tool)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .map_err(|e| RuntimeError {
                            message: format!("run_tool: cannot start `{tool}`: {e}"),
                        })?;
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin.write_all(input.as_bytes()).map_err(|e| RuntimeError {
                            message: format!("run_tool: writing to `{tool}` stdin: {e}"),
                        })?;
                    }
                    let out = child.wait_with_output().map_err(|e| RuntimeError {
                        message: format!("run_tool: `{tool}` failed: {e}"),
                    })?;
                    if !out.status.success() {
                        return err(format!("run_tool: `{tool}` exited with {}", out.status));
                    }
                    Ok(Some(Value::Str(String::from_utf8_lossy(&out.stdout).into_owned())))
                }
                _ => err("run_tool expects a BuildExec, a tool name, and input"),
            },
            // Network capability: attenuate a Net to a held address. `only` is
            // RFC-0011's method-form verb for the same allow-narrowing.
            "restrict" => match args {
                [Value::Net(allow), Value::Str(addr)] => Ok(Some(net_narrow_to(allow, addr)?)),
                _ => err("restrict expects a Net and an address"),
            },
            // RFC-0011 typed verbs: the argument is a `NetPolicy` carrying one or more address
            // patterns (a `confine.union` joins them, newline-separated). `only` narrows to the
            // set; `deny` subtracts it (a monotone exclusion recorded as `!`-prefixed entries
            // the shared `net_allows` honours).
            "only" => match args {
                [Value::Net(allow), Value::Ctor { fields, .. }] if fields.len() == 1 => {
                    let Value::Str(addr) = &fields[0] else {
                        return err("only expects a NetPolicy");
                    };
                    Ok(Some(net_narrow_to(allow, addr)?))
                }
                _ => err("only expects a Net and a NetPolicy"),
            },
            "deny" => match args {
                [Value::Net(allow), Value::Ctor { fields, .. }] if fields.len() == 1 => {
                    let Value::Str(addr) = &fields[0] else {
                        return err("deny expects a NetPolicy");
                    };
                    let mut next = allow.clone();
                    for p in addr.split('\n') {
                        next.push(format!("!{p}"));
                    }
                    Ok(Some(Value::Net(next)))
                }
                _ => err("deny expects a Net and a NetPolicy"),
            },
            // Connect only to an address the Net capability permits.
            "connect" => match args {
                [Value::Net(allow), Value::Str(addr)] => {
                    let (tls, host_port) = witchy_runtime::net::parse_scheme(addr);
                    let targets = match witchy_caps::capabilities::resolve_admitted(allow, host_port) {
                        Ok(t) => t,
                        Err(e) => return err(format!("connect: {e}")),
                    };
                    match witchy_runtime::net::dial(&targets, tls, host_port) {
                        Ok(stream) => {
                            let id = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            Ok(Some(Value::Socket(id)))
                        }
                        Err(e) => err(format!("connect to `{addr}` failed: {e}")),
                    }
                }
                _ => err("connect expects a Net and an address"),
            },
            "try_connect" => match args {
                [Value::Net(allow), Value::Str(addr)] => {
                    let (tls, host_port) = witchy_runtime::net::parse_scheme(addr);
                    let targets = match witchy_caps::capabilities::resolve_admitted(allow, host_port) {
                        Ok(t) => t,
                        Err(e) => return err(format!("try_connect: {e}")),
                    };
                    let v = match witchy_runtime::net::dial(&targets, tls, host_port) {
                        Ok(stream) => {
                            let id = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            Value::Ctor { name: "Some".into(), fields: vec![Value::Socket(id)] }
                        }
                        Err(_) => Value::Ctor { name: "None".into(), fields: Vec::new() },
                    };
                    Ok(Some(v))
                }
                _ => err("try_connect expects a Net and an address"),
            },
            "send_line" => match args {
                [Value::Socket(id), Value::Str(line)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    sock.get_mut()
                        .write_all(line.as_bytes())
                        .and_then(|_| sock.get_mut().write_all(b"\n"))
                        .map_err(|e| RuntimeError { message: format!("send failed: {e}") })?;
                    Ok(Some(Value::Nil))
                }
                _ => err("send_line expects a Socket and a String"),
            },
            "recv_line" => match args {
                [Value::Socket(id)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    let mut line = String::new();
                    sock.read_line(&mut line)
                        .map_err(|e| RuntimeError { message: format!("recv failed: {e}") })?;
                    Ok(Some(Value::Str(line.trim_end_matches('\n').to_string())))
                }
                _ => err("recv_line expects a Socket"),
            },
            // Write raw bytes to the socket with no trailing newline — for
            // sending an exact request (headers + body) where `send_line`'s
            // appended `\n` would corrupt the framing.
            "send_bytes" => match args {
                [Value::Socket(id), Value::Str(s)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    sock.get_mut()
                        .write_all(s.as_bytes())
                        .map_err(|e| RuntimeError { message: format!("send failed: {e}") })?;
                    Ok(Some(Value::Nil))
                }
                _ => err("send_bytes expects a Socket and a String"),
            },
            // Read the rest of the connection to EOF (the peer closing the
            // connection ends it) — e.g. an HTTP `Connection: close` response.
            "recv_all" => match args {
                [Value::Socket(id)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(sock, &mut buf)
                        .map_err(|e| RuntimeError { message: format!("recv failed: {e}") })?;
                    Ok(Some(Value::Str(String::from_utf8_lossy(&buf).into_owned())))
                }
                _ => err("recv_all expects a Socket"),
            },
            // Read exactly `n` bytes from the socket — for a request/response body
            // of a known `Content-Length`. Returns fewer bytes only if the peer
            // closes early.
            "recv_bytes" => match args {
                [Value::Socket(id), Value::Int(n)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    let want = (*n).max(0) as usize;
                    let mut buf = vec![0u8; want];
                    let mut read = 0;
                    while read < want {
                        match sock.read(&mut buf[read..]) {
                            Ok(0) => break,
                            Ok(k) => read += k,
                            Err(e) => return err(format!("recv failed: {e}")),
                        }
                    }
                    buf.truncate(read);
                    Ok(Some(Value::Str(String::from_utf8_lossy(&buf).into_owned())))
                }
                _ => err("recv_bytes expects a Socket and an Int"),
            },
            // Bind and listen on an address the Net capability permits — the
            // server side of the network capability. Returns a `Listener`.
            "listen" => match args {
                [Value::Net(allow), Value::Str(addr)] => {
                    if !witchy_caps::capabilities::net_allows(allow, addr) {
                        return err(format!("listen: `{addr}` is not permitted by this Net capability"));
                    }
                    match TcpListener::bind(addr) {
                        Ok(listener) => {
                            let id = self.listeners.len();
                            self.listeners.push(listener);
                            Ok(Some(Value::Listener(id)))
                        }
                        Err(e) => err(format!("listen on `{addr}` failed: {e}")),
                    }
                }
                _ => err("listen expects a Net and an address"),
            },
            // Block until a client connects, returning the connection `Socket`.
            "accept" => match args {
                [Value::Listener(id)] => {
                    let listener = self
                        .listeners
                        .get(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid listener".into() })?;
                    match listener.accept() {
                        Ok((stream, _peer)) => {
                            let sid = self.sockets.len();
                            self.sockets.push(BufReader::new(Box::new(stream) as Box<dyn Stream>));
                            Ok(Some(Value::Socket(sid)))
                        }
                        Err(e) => err(format!("accept failed: {e}")),
                    }
                }
                _ => err("accept expects a Listener"),
            },
            // Close a connected socket (e.g. after sending a `Connection: close`
            // response). Idempotent; an already-closed socket is not an error.
            "close" => match args {
                [Value::Socket(id)] => {
                    if let Some(sock) = self.sockets.get_mut(*id) {
                        sock.get_mut().shutdown();
                    }
                    Ok(Some(Value::Nil))
                }
                _ => err("close expects a Socket"),
            },
            _ => Ok(None),
        }
    }

    /// The interpreter-side linear-update fast path: a self-assignment of an
    /// accumulation shape — `xs = list.push(xs, e)`, `d = dict.insert(d, k, v)`,
    /// `d = dict.update(d, k, dflt, f)`, `s = s + p` (any left spine) — mutates the
    /// variable's slot in place instead of cloning the whole collection per
    /// step, turning accumulate-in-loop from O(n²) into O(n). Sound because
    /// values are fully owned (binding one clones it; no two bindings share
    /// storage), so the slot is the value's only home — and the path stands
    /// down unless the rest of the right-hand side provably never mentions the
    /// variable, so the early mutation is unobservable. Returns Ok(true) when
    /// handled; Ok(false) means take the general clone-and-assign path.
    fn try_inplace_assign(
        &mut self,
        name: &str,
        rhs: &Expr,
        env: &mut Env,
    ) -> Result<bool, Flow> {
        match rhs {
            Expr::Call { name: f, args }
                if f == "list.push" && args.len() == 2
                    && matches!(&args[0], Expr::Var(v) if v == name)
                    && !expr_mentions(&args[1], name)
                    && !matches!(env.get(f), Some(Value::Closure { .. })) =>
            {
                if !matches!(env.slot_mut(name), Some((Value::List(_), true))) {
                    return Ok(false);
                }
                let x = self.eval(&args[1], env)?;
                let Some((Value::List(items), true)) = env.slot_mut(name) else {
                    unreachable!("slot checked above; the argument cannot reach it");
                };
                items.push(x);
                Ok(true)
            }
            Expr::Call { name: f, args }
                if f == "dict.insert" && args.len() == 3
                    && matches!(&args[0], Expr::Var(v) if v == name)
                    && !expr_mentions(&args[1], name)
                    && !expr_mentions(&args[2], name)
                    && !matches!(env.get(f), Some(Value::Closure { .. })) =>
            {
                if !matches!(env.slot_mut(name), Some((Value::Dict(_), true))) {
                    return Ok(false);
                }
                let k = self.eval(&args[1], env)?;
                let v = self.eval(&args[2], env)?;
                let Some((Value::Dict(entries), true)) = env.slot_mut(name) else {
                    unreachable!("slot checked above; the arguments cannot reach it");
                };
                match entries.iter_mut().find(|(ek, _)| ek == &k) {
                    Some(slot) => slot.1 = v,
                    None => entries.push((k, v)),
                }
                Ok(true)
            }
            // `update` is matched before locals in `eval_call`, so no shadow check.
            Expr::Call { name: f, args }
                if f == "dict.update" && args.len() == 4
                    && matches!(&args[0], Expr::Var(v) if v == name)
                    && args[1..].iter().all(|a| !expr_mentions(a, name)) =>
            {
                if !matches!(env.slot_mut(name), Some((Value::Dict(_), true))) {
                    return Ok(false);
                }
                let k = self.eval(&args[1], env)?;
                let dflt = self.eval(&args[2], env)?;
                let updater = self.eval(&args[3], env)?;
                let current = {
                    let Some((Value::Dict(entries), true)) = env.slot_mut(name) else {
                        unreachable!("slot checked above; the arguments cannot reach it");
                    };
                    entries
                        .iter()
                        .find(|(ek, _)| ek == &k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(dflt)
                };
                let new_v = self.apply_closure(updater, vec![current])?;
                let Some((Value::Dict(entries), true)) = env.slot_mut(name) else {
                    unreachable!("slot checked above; the closure cannot reach it");
                };
                match entries.iter_mut().find(|(ek, _)| ek == &k) {
                    Some(slot) => slot.1 = new_v,
                    None => entries.push((k, new_v)),
                }
                Ok(true)
            }
            Expr::Binary { op: BinOp::Add, .. } => {
                let Some(rights) = concat_spine(rhs, name) else {
                    return Ok(false);
                };
                if rights.iter().any(|r| expr_mentions(r, name)) {
                    return Ok(false);
                }
                if !matches!(env.slot_mut(name), Some((Value::Str(_), true))) {
                    return Ok(false);
                }
                let Some((slot, true)) = env.slot_mut(name) else { unreachable!() };
                let mut acc = match std::mem::replace(slot, Value::Nil) {
                    Value::Str(s) => s,
                    _ => unreachable!("slot checked above"),
                };
                for r in rights {
                    let v = match self.eval(r, env) {
                        Ok(v) => v,
                        Err(flow) => {
                            // Put the accumulated string back before unwinding so
                            // the environment stays consistent.
                            if let Some((slot, _)) = env.slot_mut(name) {
                                *slot = Value::Str(acc);
                            }
                            return Err(flow);
                        }
                    };
                    match v {
                        Value::Str(b) => acc.push_str(&b),
                        other => {
                            // The same error the general `<>` evaluation reports,
                            // with the left side accumulated so far.
                            let a = Value::Str(acc);
                            return err(format!(
                                "`<>` expects two Strings, got `{a}` and `{other}`"
                            ));
                        }
                    }
                }
                let Some((slot, true)) = env.slot_mut(name) else { unreachable!() };
                *slot = Value::Str(acc);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn eval_block(&mut self, block: &Block, env: &mut Env) -> Result<Value, Flow> {
        // Only open a scope if the block actually introduces bindings. Most
        // function bodies and if-branches are binding-free (just an expression),
        // and skipping the push/pop avoids growing the scopes vector on the hot
        // call path.
        let needs_scope = block
            .stmts
            .iter()
            .any(|s| matches!(s, Stmt::Let { .. } | Stmt::LetTuple { .. }));
        if needs_scope {
            env.push();
        }
        let mut result = Value::Nil;
        for (i, stmt) in block.stmts.iter().enumerate() {
            if let Some(line) = block.lines.get(i) {
                self.cur_line = *line;
            }
            match stmt {
                Stmt::Let { name, ty: _, mutable, value } => {
                    let v = self.eval(value, env)?;
                    env.define(name.clone(), v, *mutable);
                    result = Value::Nil;
                }
                Stmt::Assign { name, value } => {
                    if !self.try_inplace_assign(name, value, env)? {
                        let v = self.eval(value, env)?;
                        match env.assign(name, v) {
                            Assign::Done => {}
                            Assign::Immutable => {
                                return err(format!(
                                    "cannot assign to `{name}`: it is immutable (declared with `let`)"
                                ))
                            }
                            Assign::Unbound => {
                                return err(format!("cannot assign to unbound variable `{name}`"))
                            }
                        }
                    }
                    result = Value::Nil;
                }
                Stmt::LetTuple { names, value } => {
                    let v = self.eval(value, env)?;
                    match v {
                        Value::Tuple(items) if items.len() == names.len() => {
                            for (n, item) in names.iter().zip(items) {
                                env.define(n.clone(), item, false);
                            }
                        }
                        other => {
                            // (`needs_scope` is always true here — there's a LetTuple.)
                            env.pop();
                            return err(format!(
                                "tuple destructure expected a {}-tuple, got `{other}`",
                                names.len()
                            ));
                        }
                    }
                    result = Value::Nil;
                }
                Stmt::Return(opt) => {
                    let v = match opt {
                        Some(e) => self.eval(e, env)?,
                        None => Value::Nil,
                    };
                    if needs_scope {
                        env.pop();
                    }
                    // Unwind to the enclosing function boundary, which turns this
                    // into the function's result (same channel `?` uses).
                    return Err(Flow::Return(v));
                }
                Stmt::Break => {
                    if needs_scope {
                        env.pop();
                    }
                    return Err(Flow::Break);
                }
                Stmt::Continue => {
                    if needs_scope {
                        env.pop();
                    }
                    return Err(Flow::Continue);
                }
                Stmt::Expr(e) | Stmt::Yield(e) => {
                    result = self.eval(e, env)?;
                }
            }
        }
        if needs_scope {
            env.pop();
        }
        Ok(result)
    }

    fn eval(&mut self, expr: &Expr, env: &mut Env) -> Result<Value, Flow> {
        self.steps += 1;
        if self.steps > self.step_limit {
            return err("evaluation step budget exceeded (possible infinite loop)");
        }
        match expr {
            // Expanded away by `crate::tagged` during linking, before evaluation.
            Expr::TaggedLit { tag, .. } => {
                unreachable!("unexpanded tagged literal `{tag}` reached the interpreter")
            }
            Expr::Int(n) | Expr::Duration(n) => Ok(Value::Int(*n)),
            Expr::Float(x) => Ok(Value::Float(*x)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            // A range lowers to a list-building block; evaluate that.
            Expr::Range { lo, hi, inclusive } => {
                let d = witchy_syntax::parser::desugar_range((**lo).clone(), (**hi).clone(), *inclusive);
                self.eval(&d, env)
            }
            // A subscript lowers to an `list.at(base, index)` call; evaluate that.
            Expr::Index { base, index } => {
                let d = witchy_syntax::parser::desugar_index((**base).clone(), (**index).clone());
                self.eval(&d, env)
            }
            // Trait lowering resolves every method call; one that reaches
            // evaluation is unresolvable (mirrors the type checker's error).
            Expr::MethodCall { method, .. } => err(format!(
                "cannot resolve the method call `.{method}(…)` — methods come from \
                 `impl` blocks; a plain function is called as `{method}(value, …)`"
            )),
            // Named-field record construction is lowered by `witchy_syntax::records`
            // before evaluation.
            Expr::Record { .. } => {
                unreachable!("Expr::Record is lowered by witchy_syntax::records before the interpreter")
            }
            // `while let` lowers to a `while true` over a match; evaluate that.
            Expr::WhileLet { pattern, scrutinee, body } => {
                let d = witchy_syntax::parser::desugar_while_let(
                    pattern.clone(),
                    (**scrutinee).clone(),
                    body.clone(),
                );
                self.eval(&d, env)
            }
            Expr::List(items) => {
                let vals = items
                    .iter()
                    .map(|e| self.eval(e, env))
                    .collect::<Result<_, _>>()?;
                Ok(Value::List(vals))
            }
            Expr::Tuple(items) => {
                let vals = items
                    .iter()
                    .map(|e| self.eval(e, env))
                    .collect::<Result<_, _>>()?;
                Ok(Value::Tuple(vals))
            }
            Expr::Var(name) => match env.get(name) {
                Some(v) => Ok(v.clone()),
                None => match self.functions.get(name).cloned() {
                    // A bare top-level function name is a first-class function
                    // value: wrap it as a closure over an empty environment
                    // (top-level functions are closed; nested calls resolve
                    // through the global function table at apply time).
                    Some(func) => Ok(Value::Closure {
                        params: func.params.clone(),
                        body: func.body.clone(),
                        env: Box::new(Env::new()),
                    }),
                    None => err(format!("unbound variable `{name}`")),
                },
            },
            Expr::Call { name, args } => self.eval_call(name, args, env),
            Expr::Apply { func, args } => {
                let clo = self.eval(func, env)?;
                let argvals = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<Result<Vec<_>, _>>()?;
                self.apply_closure(clo, argvals)
            }
            Expr::Ctor { name, args } => {
                let fields = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<Result<_, _>>()?;
                Ok(Value::Ctor {
                    name: name.clone(),
                    fields,
                })
            }
            Expr::Unary { op, expr } => {
                let v = self.eval(expr, env)?;
                match (op, v) {
                    // `move x` is value-neutral: it evaluates to its operand. The
                    // ownership transfer it denotes only matters for the native
                    // backend's clone elision; the interpreter just yields the value.
                    (UnOp::Move, v) => Ok(v),
                    // `await e` is value-neutral in Phase 1 (no executor yet): it
                    // yields its operand and runs sequentially, identical on both
                    // backends. Suspension semantics arrive with the executor.
                    (UnOp::Await, v) => Ok(v),
                    // Negation wraps (matching the WASM backend's `0 - x`): so
                    // `-INT_MIN` is `INT_MIN`, not a host panic / divergence.
                    (UnOp::Neg, Value::Int(n)) => Ok(Value::Int(n.wrapping_neg())),
                    (UnOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
                    (UnOp::Neg, other) => err(format!("cannot negate `{other}`")),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Not, other) => err(format!("cannot apply `!` to `{other}`")),
                    (UnOp::BitNot, Value::Int(n)) => Ok(Value::Int(!n)),
                    (UnOp::BitNot, other) => err(format!("cannot apply `~` to `{other}`")),
                }
            }
            Expr::Lambda { params, body, .. } => {
                let mut mentioned = HashSet::new();
                idents_in_block(body, &mut |n| {
                    if !mentioned.contains(n) {
                        mentioned.insert(n.to_string());
                    }
                });
                Ok(Value::Closure {
                    params: params.clone(),
                    body: body.clone(),
                    env: Box::new(env.capture(&mentioned)),
                })
            }
            Expr::RecordUpdate { base, fields } => {
                let v = self.eval(base, env)?;
                let Value::Ctor { name, fields: mut values } = v else {
                    return err(format!("`update` requires a record value, got `{v}`"));
                };
                for (fname, vexpr) in fields {
                    let idx = self
                        .record_fields
                        .get(&name)
                        .and_then(|names| names.iter().position(|n| n == fname));
                    let val = self.eval(vexpr, env)?;
                    match idx.filter(|i| *i < values.len()) {
                        Some(i) => values[i] = val,
                        None => return err(format!("`{name}` has no field `{fname}`")),
                    }
                }
                Ok(Value::Ctor { name, fields: values })
            }
            Expr::Field { base, field } => {
                let v = self.eval(base, env)?;
                if let Ok(i) = field.parse::<usize>() {
                    return match v {
                        Value::Tuple(items) => match items.get(i) {
                            Some(item) => Ok(item.clone()),
                            None => err(format!(
                                "tuple has no element `.{i}` (it has {})",
                                items.len()
                            )),
                        },
                        other => {
                            err(format!("element access `.{i}` on a non-tuple value `{other}`"))
                        }
                    };
                }
                match v {
                    Value::Ctor { name, fields } => {
                        let idx = self
                            .record_fields
                            .get(&name)
                            .and_then(|names| names.iter().position(|n| n == field));
                        match idx.and_then(|i| fields.get(i)) {
                            Some(v) => Ok(v.clone()),
                            None => err(format!("`{name}` has no field `{field}`")),
                        }
                    }
                    other => err(format!("field access `.{field}` on a non-record value `{other}`")),
                }
            }
            Expr::Try(inner) => {
                let v = self.eval(inner, env)?;
                match v {
                    Value::Ctor { name, mut fields }
                        if (name == "Ok" || name == "Some") && fields.len() == 1 =>
                    {
                        Ok(fields.remove(0))
                    }
                    Value::Ctor { name, fields } if name == "Err" || name == "None" => {
                        // Short-circuit: return the Err/None from the enclosing function.
                        Err(Flow::Return(Value::Ctor { name, fields }))
                    }
                    other => err(format!("`?` expects a Result/Option value, got `{other}`")),
                }
            }
            // `e as T` narrows a capability's rights — purely type-level, so at
            // runtime it is the identity on the underlying value.
            Expr::As { expr, .. } => self.eval(expr, env),
            // `&&`/`||` short-circuit, so the right side isn't always evaluated.
            Expr::Binary { op: BinOp::And, lhs, rhs } => match self.eval(lhs, env)? {
                Value::Bool(false) => Ok(Value::Bool(false)),
                Value::Bool(true) => match self.eval(rhs, env)? {
                    Value::Bool(b) => Ok(Value::Bool(b)),
                    other => err(format!("`&&` expects Bool operands, got `{other}`")),
                },
                other => err(format!("`&&` expects Bool operands, got `{other}`")),
            },
            Expr::Binary { op: BinOp::Or, lhs, rhs } => match self.eval(lhs, env)? {
                Value::Bool(true) => Ok(Value::Bool(true)),
                Value::Bool(false) => match self.eval(rhs, env)? {
                    Value::Bool(b) => Ok(Value::Bool(b)),
                    other => err(format!("`||` expects Bool operands, got `{other}`")),
                },
                // Non-Bool `||` is the truthy fallback: `a` when truthy, else `b`.
                // Falsy values are "" / None / [] (typeck restricts the operands to
                // Bool / String / Option / List).
                v if value_truthy(&v) => Ok(v),
                _ => self.eval(rhs, env),
            },
            Expr::Binary { op, lhs, rhs } => {
                let l = self.eval(lhs, env)?;
                let r = self.eval(rhs, env)?;
                Ok(eval_binary(*op, l, r)?)
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => match self.eval(cond, env)? {
                Value::Bool(true) => self.eval_block(then_block, env),
                Value::Bool(false) => match else_block {
                    Some(b) => self.eval_block(b, env),
                    None => Ok(Value::Nil),
                },
                other => err(format!("`if` condition must be a Bool, got `{other}`")),
            },
            Expr::For { var, iter, body } => {
                // A range iterator counts directly — no list is materialized, so
                // `for i in 0..n` is O(1) memory and O(n) time like the compiled
                // loop. `checked_add` stops cleanly at i64::MAX, matching the
                // compiled backend's inclusive-end guard (no overflow/wrap).
                if let Expr::Range { lo, hi, inclusive } = iter.as_ref() {
                    let (start, end) = match (self.eval(lo, env)?, self.eval(hi, env)?) {
                        (Value::Int(a), Value::Int(b)) => (a, b),
                        (a, b) => {
                            return err(format!("`for` range bounds must be Int, got `{a}`..`{b}`"))
                        }
                    };
                    let mut i = start;
                    while if *inclusive { i <= end } else { i < end } {
                        env.push();
                        env.define(var.clone(), Value::Int(i), false);
                        let r = self.eval_block(body, env);
                        env.pop();
                        match r {
                            Ok(_) | Err(Flow::Continue) => {}
                            Err(Flow::Break) => break,
                            Err(e) => return Err(e),
                        }
                        match i.checked_add(1) {
                            Some(n) => i = n,
                            None => break,
                        }
                    }
                    return Ok(Value::Nil);
                }
                let items = match self.eval(iter, env)? {
                    Value::List(items) => items,
                    other => return err(format!("`for` expects a List, got `{other}`")),
                };
                for item in items {
                    env.push();
                    env.define(var.clone(), item, false);
                    let r = self.eval_block(body, env);
                    env.pop();
                    match r {
                        Ok(_) | Err(Flow::Continue) => {}
                        Err(Flow::Break) => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(Value::Nil)
            }
            Expr::Block(block) => self.eval_block(block, env),
            Expr::While { cond, body } => {
                loop {
                    match self.eval(cond, env)? {
                        Value::Bool(true) => match self.eval_block(body, env) {
                            Ok(_) | Err(Flow::Continue) => {}
                            Err(Flow::Break) => break,
                            Err(e) => return Err(e),
                        },
                        Value::Bool(false) => break,
                        other => {
                            return err(format!("`while` condition must be Bool, got `{other}`"))
                        }
                    }
                }
                Ok(Value::Nil)
            }
            Expr::Match { scrutinee, arms } => {
                let value = self.eval(scrutinee, env)?;
                for arm in arms {
                    env.push();
                    if match_pattern(&arm.pattern, &value, env) {
                        let guard_ok = match &arm.guard {
                            Some(g) => matches!(self.eval(g, env)?, Value::Bool(true)),
                            None => true,
                        };
                        if guard_ok {
                            let result = self.eval(&arm.body, env);
                            env.pop();
                            return result;
                        }
                    }
                    env.pop();
                }
                err(format!("no match arm for value `{value}`"))
            }
        }
    }
}

fn match_pattern(pat: &Pattern, value: &Value, env: &mut Env) -> bool {
    match (pat, value) {
        (Pattern::Wildcard, _) => true,
        (Pattern::Var(name), v) => {
            env.define(name.clone(), v.clone(), false);
            true
        }
        (Pattern::Int(a), Value::Int(b)) => a == b,
        (Pattern::Str(a), Value::Str(b)) => a == b,
        (Pattern::Bool(a), Value::Bool(b)) => a == b,
        (Pattern::Ctor { name, args }, Value::Ctor { name: vname, fields }) => {
            name == vname
                && args.len() == fields.len()
                && args
                    .iter()
                    .zip(fields)
                    .all(|(p, v)| match_pattern(p, v, env))
        }
        (Pattern::Tuple(pats), Value::Tuple(items)) => {
            pats.len() == items.len()
                && pats
                    .iter()
                    .zip(items)
                    .all(|(p, v)| match_pattern(p, v, env))
        }
        (Pattern::List { elems, rest }, Value::List(items)) => {
            let len_ok = match rest {
                None => items.len() == elems.len(),
                Some(_) => items.len() >= elems.len(),
            };
            if !len_ok {
                return false;
            }
            if !elems
                .iter()
                .zip(items)
                .all(|(p, v)| match_pattern(p, v, env))
            {
                return false;
            }
            if let Some(Some(name)) = rest {
                let tail = items[elems.len()..].to_vec();
                env.define(name.clone(), Value::List(tail), false);
            }
            true
        }
        _ => false,
    }
}

/// Build an "integer overflow" error producer for the given operator.
fn over(op: &str) -> impl FnOnce() -> RuntimeError + '_ {
    move || RuntimeError {
        message: format!("integer overflow in `{op}`"),
    }
}

// Runtime truthiness for the non-Bool `||` fallback. Falsy values are the empty
// forms of the emptyable built-ins: "" / [] / None. Everything else is truthy.
fn value_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Str(s) => !s.is_empty(),
        Value::List(xs) => !xs.is_empty(),
        Value::Ctor { name, .. } => name != "None",
        _ => true,
    }
}

fn eval_binary(op: BinOp, l: Value, r: Value) -> Result<Value, RuntimeError> {
    use BinOp::*;
    use Value::{Float, Int, Str};
    match op {
        // Int arithmetic WRAPS on overflow — well-defined two's-complement i64,
        // identical to the WASM backend's `i64.add/sub/mul` (so the two backends
        // agree exactly). It never panics the host. Division still errors on the
        // two cases WASM's `i64.div_s` traps on: divide-by-zero and INT_MIN / -1.
        Add | Sub | Mul | Div => match (op, l, r) {
            // `+` on strings concatenates (typeck guarantees both sides are
            // strings; this arm makes the reference semantics value-exact).
            (Add, Str(a), Str(b)) => Ok(Str(format!("{a}{b}"))),
            (Add, Int(a), Int(b)) => Ok(Int(a.wrapping_add(b))),
            (Sub, Int(a), Int(b)) => Ok(Int(a.wrapping_sub(b))),
            (Mul, Int(a), Int(b)) => Ok(Int(a.wrapping_mul(b))),
            (Div, Int(_), Int(0)) => err("division by zero"),
            (Div, Int(a), Int(b)) => a.checked_div(b).map(Int).ok_or_else(over("/")),
            (Add, Float(a), Float(b)) => Ok(Float(a + b)),
            (Sub, Float(a), Float(b)) => Ok(Float(a - b)),
            (Mul, Float(a), Float(b)) => Ok(Float(a * b)),
            (Div, Float(a), Float(b)) => Ok(Float(a / b)),
            (_, a, b) => err(format!("cannot apply arithmetic to `{a}` and `{b}`")),
        },
        Mod => match (l, r) {
            (Int(_), Int(0)) => err("modulo by zero"),
            (Int(a), Int(b)) => Ok(Int(a.wrapping_rem(b))),
            (a, b) => err(format!("`%` expects two Ints, got `{a}` and `{b}`")),
        },
        BitAnd => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a & b)),
            (a, b) => err(format!("`&` expects two Ints, got `{a}` and `{b}`")),
        },
        BitOr => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a | b)),
            (a, b) => err(format!("`|` expects two Ints, got `{a}` and `{b}`")),
        },
        BitXor => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a ^ b)),
            (a, b) => err(format!("`^` expects two Ints, got `{a}` and `{b}`")),
        },
        Shl => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a.wrapping_shl(b as u32))),
            (a, b) => err(format!("`<<` expects two Ints, got `{a}` and `{b}`")),
        },
        Shr => match (l, r) {
            (Int(a), Int(b)) => Ok(Int(a.wrapping_shr(b as u32))),
            (a, b) => err(format!("`>>` expects two Ints, got `{a}` and `{b}`")),
        },
        Concat => match (l, r) {
            (Str(a), Str(b)) => Ok(Str(a + &b)),
            (a, b) => err(format!("`<>` expects two Strings, got `{a}` and `{b}`")),
        },
        Eq => Ok(Value::Bool(l == r)),
        NotEq => Ok(Value::Bool(l != r)),
        Lt | LtEq | Gt | GtEq => {
            let ord = compare(&l, &r)?;
            let result = match op {
                Lt => ord == std::cmp::Ordering::Less,
                LtEq => ord != std::cmp::Ordering::Greater,
                Gt => ord == std::cmp::Ordering::Greater,
                GtEq => ord != std::cmp::Ordering::Less,
                _ => unreachable!(),
            };
            Ok(Value::Bool(result))
        }
        And | Or => unreachable!("&&/|| are short-circuited in eval"),
    }
}

fn compare(l: &Value, r: &Value) -> Result<std::cmp::Ordering, RuntimeError> {
    use Value::*;
    match (l, r) {
        (Int(a), Int(b)) => Ok(a.cmp(b)),
        (Float(a), Float(b)) => a
            .partial_cmp(b)
            .ok_or_else(|| RuntimeError { message: "cannot compare NaN".into() }),
        (Str(a), Str(b)) => Ok(a.cmp(b)),
        _ => err(format!("cannot order `{l}` and `{r}`")),
    }
}

/// Parse and run a witchy program, returning everything it `print`ed. Expects a
/// `main` function with no parameters.
/// Resolve a path relative to a `Dir` capability, confining it to the subtree.
/// Beyond the lexical `..`/absolute checks, we canonicalize (resolving symlinks)
/// and verify the real target stays under the real base, so a symlink *inside*
/// the subtree can't point out of it.
///
/// Note: canonicalize-then-use is mildly TOCTOU; the race-free fix is
/// syscall-level confinement (openat2/O_NOFOLLOW, i.e. the cap-std crate), which
/// is what the planned WASI-preopen substrate gives us.
// Bridge between the interpreter's `Value` and the registry's `NativeValue` at
// the single native-dispatch site. Native functions are typed (their `.witchy`
// stubs), so they only ever receive the five shapes `NativeValue` carries; any
// other `Value` is a caller bug surfaced as a runtime error.
fn value_to_native(v: &Value) -> Result<witchy_runtime::value::NativeValue, RuntimeError> {
    use witchy_runtime::value::NativeValue as N;
    Ok(match v {
        Value::Int(i) => N::Int(*i),
        Value::Str(s) => N::Str(s.clone()),
        Value::Bool(b) => N::Bool(*b),
        Value::List(xs) => N::List(
            xs.iter().map(value_to_native).collect::<Result<Vec<_>, RuntimeError>>()?,
        ),
        Value::Secret(s) => N::Secret(s.clone()),
        other => {
            return Err(RuntimeError {
                message: format!("native function received an unsupported argument: {other}"),
            });
        }
    })
}

fn native_to_value(v: witchy_runtime::value::NativeValue) -> Value {
    use witchy_runtime::value::NativeValue as N;
    match v {
        N::Int(i) => Value::Int(i),
        N::Str(s) => Value::Str(s),
        N::Bool(b) => Value::Bool(b),
        N::List(xs) => Value::List(xs.into_iter().map(native_to_value).collect()),
        N::Secret(s) => Value::Secret(s),
    }
}

// The `Dir` confinement lives in `witchy_runtime::confine` — the single implementation the
// compiled sandbox shares (see that module). These thin wrappers adapt its
// `ConfineError` to the interpreter's `RuntimeError` so the eval call sites are
// unchanged.
pub(crate) fn resolve(base: &Path, rel: &str) -> Result<PathBuf, RuntimeError> {
    witchy_runtime::confine::resolve(base, rel).map_err(|e| RuntimeError { message: e.0 })
}

pub(crate) fn resolve_write(base: &Path, rel: &str) -> Result<PathBuf, RuntimeError> {
    witchy_runtime::confine::resolve_write(base, rel).map_err(|e| RuntimeError { message: e.0 })
}

pub fn run(src: &str) -> Result<Vec<String>, RuntimeError> {
    run_with(src, ".", Vec::new())
}

/// Run with a chosen root directory for the root `Dir` capability.
#[allow(dead_code)]
pub fn run_in(src: &str, root: impl AsRef<Path>) -> Result<Vec<String>, RuntimeError> {
    run_with(src, root, Vec::new())
}

/// Run with the host-provided root capabilities: `root` backs the root `Dir`,
/// and `net_allow` backs the root `Net` (the permitted `host:port` list).
/// `main` is the root entrypoint: it receives the capabilities it declares (the
/// only place authority is minted) and may pass attenuated ones to the functions
/// it calls.
pub fn run_with(
    src: &str,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
) -> Result<Vec<String>, RuntimeError> {
    let module = parse_module(src).map_err(|e| RuntimeError { message: e.to_string() })?;
    run_module(module, root, net_allow)
}

/// Run an already-built (e.g. linked) module.
pub fn run_module(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
) -> Result<Vec<String>, RuntimeError> {
    run_module_args(module, root, net_allow, Vec::new())
}

/// Like [`run_module`], but with an evaluation step ceiling — the `comptime:`
/// path, where termination is part of the contract.
pub fn run_module_budgeted(
    module: Module,
    root: impl AsRef<Path>,
    step_limit: u64,
) -> Result<Vec<String>, RuntimeError> {
    let module = witchy_syntax::records::lower(module).map_err(|message| RuntimeError { message })?;
    let module = witchy_types::traits::lower(module);
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited(module, root, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, step_limit)
    })
    .map(|(output, _)| output)
}

/// Run with direct `File` grants (RFC-0012): the i-th `File` parameter of `main`
/// maps to `file_grants[i]`. The differential-test oracle for `--file`.
pub fn run_module_files(
    module: Module,
    root: impl AsRef<Path>,
    file_grants: Vec<PathBuf>,
) -> Result<Vec<String>, RuntimeError> {
    let module = witchy_syntax::records::lower(module).map_err(|message| RuntimeError { message })?;
    let module = witchy_types::traits::lower(module);
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited(module, root, Vec::new(), file_grants, Vec::new(), Vec::new(), None, DEFAULT_STEP_LIMIT)
    })
    .map(|(output, _)| output)
}

/// Like [`run_module`], but also hands command-line `args` to a `main` that
/// declares a `List(String)` parameter to receive them (argv is input data, not
/// authority, so it is an ordinary value parameter — not a capability).
pub fn run_module_args(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
) -> Result<Vec<String>, RuntimeError> {
    run_module_signed(module, root, net_allow, args, None)
}

/// Like [`run_module_args`], but also grants the root `Secret` capability
/// from `signing_key` (an Ed25519 seed) to a `main` that declares one. Signing is
/// authority, so the key is host-provided, never constructed by the program.
pub fn run_module_signed(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<Vec<String>, RuntimeError> {
    run_module_exit(module, root, net_allow, args, signing_key).map(|(output, _)| output)
}

/// Like [`run_module_signed`], but also returns the process exit code (`main`'s
/// `Int` return, or 0). Used by the CLI to set the process status.
pub fn run_module_exit(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<(Vec<String>, i32), RuntimeError> {
    // Lower named-field record construction, then traits/impls — so the
    // interpreter only ever sees plain constructors and functions. (Both are
    // no-ops once the linker has done them, for the linked CLI path.)
    let module = witchy_syntax::records::lower(module).map_err(|message| RuntimeError { message })?;
    let module = witchy_types::traits::lower(module);
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || run_module_inner(module, root, Vec::new(), net_allow, args, signing_key))
}

/// Like [`run_module_exit`], but grants several `Dir` capabilities: `roots[0]`
/// backs the first `Dir` param of `main` (handle 0), `roots[1..]` the rest, in
/// order. For multi-directory programs — e.g. the witchy CLI holding both a
/// project `Dir` and a toolchain-`bin` `Dir`. See rfcs/0004-self-hosted-cli.md.
pub fn run_module_exit_dirs(
    module: Module,
    roots: Vec<PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<(Vec<String>, i32), RuntimeError> {
    let module = witchy_syntax::records::lower(module).map_err(|message| RuntimeError { message })?;
    let module = witchy_types::traits::lower(module);
    let mut roots = roots;
    let root = if roots.is_empty() { PathBuf::from(".") } else { roots.remove(0) };
    run_on_deep_stack(move || run_module_inner(module, root, roots, net_allow, args, signing_key))
}

/// Parse and run `src` with several `Dir` grants (the multi-`Dir` analog of
/// [`run_in`]); test/CLI helper for [`run_module_exit_dirs`].
#[allow(dead_code)]
pub fn run_in_dirs(src: &str, roots: &[PathBuf]) -> Result<Vec<String>, RuntimeError> {
    let module = parse_module(src).map_err(|e| RuntimeError { message: e.to_string() })?;
    run_module_exit_dirs(module, roots.to_vec(), Vec::new(), Vec::new(), None).map(|(out, _)| out)
}

/// Run the tree-walker on a thread with a large stack. The interpreter recurses
/// in Rust for nested calls, so deep (but bounded) recursion would otherwise
/// overflow the default stack and *abort the host*. The big stack accommodates
/// legitimate depth; `depth_limit` is the graceful guard against runaway
/// recursion well before this stack is exhausted. `join` contains any panic, so
/// even an unforeseen one becomes a graceful error rather than aborting the host.
#[cfg(not(target_arch = "wasm32"))]
fn run_on_deep_stack(
    f: impl FnOnce() -> Result<(Vec<String>, i32), RuntimeError> + Send + 'static,
) -> Result<(Vec<String>, i32), RuntimeError> {
    let handle = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024 * 1024)
        .spawn(f)
        .map_err(|e| RuntimeError {
            message: format!("could not start the interpreter thread: {e}"),
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(RuntimeError {
            message: "internal error: the interpreter thread panicked".into(),
        }),
    }
}

/// wasm32 (the browser playground) has neither threads nor a 4 GiB stack, so we
/// run the evaluator inline. The host stack is small, so very deep recursion can
/// trap the module — `depth_limit` still guards the common cases.
#[cfg(target_arch = "wasm32")]
fn run_on_deep_stack(
    f: impl FnOnce() -> Result<(Vec<String>, i32), RuntimeError>,
) -> Result<(Vec<String>, i32), RuntimeError> {
    f()
}

/// Whether a `main` parameter type is `List(String)` — the slot that receives the
/// command-line arguments.
fn is_args_param(ty: &Option<Type>) -> bool {
    matches!(
        ty,
        Some(Type::Named(n, targs))
            if n == "List"
                && matches!(targs.as_slice(), [Type::Named(e, ea)] if e == "String" && ea.is_empty())
    )
}

fn run_module_inner(
    module: Module,
    root: PathBuf,
    dir_roots: Vec<PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<(Vec<String>, i32), RuntimeError> {
    run_module_inner_limited(module, root, dir_roots, Vec::new(), net_allow, args, signing_key, DEFAULT_STEP_LIMIT)
}

#[allow(clippy::too_many_arguments)]
fn run_module_inner_limited(
    module: Module,
    root: PathBuf,
    dir_roots: Vec<PathBuf>,
    file_grants: Vec<PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    step_limit: u64,
) -> Result<(Vec<String>, i32), RuntimeError> {
    let mut interp = Interpreter::new(module);
    interp.step_limit = step_limit;
    interp.root = root;
    interp.dir_roots = dir_roots;
    interp.file_grants = file_grants;
    interp.net_allow = net_allow;
    interp.signing_key = signing_key;
    // The signing key is the `signing` secret in the store, so a program may take
    // either a `Secret` (the key directly) or a `SecretStore` and `get("signing")`.
    if let Some(seed) = signing_key {
        interp.secrets.insert("signing".to_string(), seed.to_vec());
    }
    let root_args = match interp.functions.get("main").cloned() {
        Some(f) => {
            // Each `Dir` param maps positionally to a grant: the first to `root`
            // (handle 0), the rest to `dir_roots` in order (mirrors the compiled
            // backend's handle assignment). Other caps mint normally.
            let mut vals = Vec::with_capacity(f.params.len());
            let mut dir_idx = 0usize;
            let mut file_idx = 0usize;
            for p in &f.params {
                if is_args_param(&p.ty) {
                    vals.push(Value::List(args.iter().cloned().map(Value::Str).collect()));
                } else if matches!(&p.ty, Some(Type::Named(n, _)) if n == "Dir") {
                    let r = if dir_idx == 0 {
                        interp.root.clone()
                    } else {
                        interp.dir_roots.get(dir_idx - 1).cloned().unwrap_or_else(|| interp.root.clone())
                    };
                    dir_idx += 1;
                    vals.push(Value::Dir(r));
                } else if matches!(&p.ty, Some(Type::Named(n, _)) if n == "File") {
                    // The i-th `File` param maps to the i-th `--file` grant (RFC-0012).
                    let path = interp.file_grants.get(file_idx).cloned().ok_or_else(|| RuntimeError {
                        message: "`main` requires a `File`, but the host granted none (provide `--file <path>`)".into(),
                    })?;
                    file_idx += 1;
                    vals.push(Value::File(path));
                } else {
                    vals.push(interp.root_cap_for(&p.ty)?);
                }
            }
            vals
        }
        None => vec![],
    };
    let ret = interp
        .call("main", root_args)
        .map_err(|e| rt_at_line(e, interp.cur_line, &interp.cur_fn))?;
    // `main` returning an `Int` sets the process exit code (the C/Go/Rust
    // convention) — it is *not* printed; a program shows output via `print`.
    let exit_code = match ret {
        Value::Int(n) => n as i32,
        _ => 0,
    };
    // A non-`Int`, non-`Nil` result (e.g. a `Float`) is still surfaced when the
    // program printed nothing — for a compiled `main -> Float` this is the only
    // way to show the value (the WASM backend has no float `to_string`).
    if interp.output.is_empty() && !matches!(ret, Value::Nil | Value::Int(_)) {
        interp.output.push(format!("{ret}"));
    }
    Ok((interp.output, exit_code))
}

/// The attenuated grants a build step runs under: a confined output directory
/// (always present — it is `BuildOut`), an optional confined read root, and
/// allow-lists for the env/net/exec caps. Safe by default — anything not granted
/// here cannot be minted, so a build step demanding it fails before running.
#[derive(Debug, Clone, Default)]
pub struct BuildGrants {
    pub out_dir: PathBuf,
    pub read_roots: Vec<PathBuf>,
    pub env_keys: Vec<String>,
    pub net_hosts: Vec<String>,
    pub exec_tools: Vec<String>,
}

/// Run a rune's `build` entrypoint under `grants`, on the interpreter, and return
/// the (sorted) names of the files it generated into `grants.out_dir`. The build
/// step's authority is exactly the build capabilities minted here: it cannot forge
/// a runtime capability (the type checker forbids a build step from taking one),
/// so even without the WASM sandbox it can only touch what these confined grants
/// permit. A module with no `build` entrypoint generates nothing.
pub fn run_build_step(module: Module, grants: BuildGrants) -> Result<Vec<String>, RuntimeError> {
    std::fs::create_dir_all(&grants.out_dir)
        .map_err(|e| RuntimeError { message: format!("build: cannot create output dir: {e}") })?;
    // Find the entrypoint before moving the module in — `build_entrypoint` is
    // robust to the linker's `mod.build` qualification.
    let Some(build) = witchy_syntax::build_entry::build_entrypoint(&module).cloned() else {
        return Ok(Vec::new());
    };
    let mut interp = Interpreter::new(module);
    let argv = build
        .params
        .iter()
        .map(|p| interp.mint_build_cap(&p.ty, &grants))
        .collect::<Result<Vec<_>, _>>()?;
    interp
        .call(&build.name, argv)
        .map_err(|e| rt_at_line(e, interp.cur_line, &interp.cur_fn))?;
    let mut generated: Vec<String> = std::fs::read_dir(&grants.out_dir)
        .map_err(|e| RuntimeError { message: format!("build: cannot read output dir: {e}") })?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    generated.sort();
    Ok(generated)
}

/// Parse and link a multi-module program, then run it. `entry` is the module
/// holding `main`. Importing a module grants no authority — only `main`'s root
/// capabilities (and what it passes on) flow in.
pub fn run_program(sources: &[(&str, &str)], entry: &str) -> Result<Vec<String>, RuntimeError> {
    let mut modules = Vec::new();
    for (name, src) in sources {
        let m = parse_module(src).map_err(|e| RuntimeError {
            message: format!("{name}: {e}"),
        })?;
        modules.push((name.to_string(), m));
    }
    let linked = crate::pipeline::link(modules, entry)
        .map_err(|e| RuntimeError { message: e.message })?;
    run_module(linked, ".", Vec::new())
}

#[cfg(test)]
#[path = "interpreter_tests.rs"]
mod tests;
