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

use std::collections::{BTreeMap, HashMap, HashSet};
// foldhash (not SipHash) for the interpreter's OWN lookup tables: keys are
// program identifiers (function/ctor/binding names), never attacker-controlled
// hash-flood surface, and `functions.get(name)` sits on the call hot path.
// The comptime `compiler_*_syntax` tables stay std `HashMap`: they cross the
// comptime boundary as parameters and are not hot.
use foldhash::{HashMap as FxHashMap, HashMapExt as _, HashSet as FxHashSet};
use std::fmt;
use std::io::BufReader;
use std::net::TcpListener;

use witchy_runtime::net::Stream;
use std::path::PathBuf;
use std::rc::Rc;

use witchy_syntax::ast::*;
use witchy_syntax::diag::DiagTemplate;
use witchy_syntax::intrinsics;
use witchy_syntax::origin::SyntaxCategory;
use witchy_syntax::parser::parse_module;
use witchy_types::witness::WitnessPlan;
#[cfg(feature = "test-fixtures")]
use witchy_test_host::{FixtureHost, HostHandle, HostRequest, HostResponse};
#[cfg(feature = "test-fixtures")]
use witchy_testkit::{FixtureErrorCode, FixtureFailure, SourceLocation, U64Text};

mod environment;
use environment::{Assign, Env};
mod assignment_plan;
use assignment_plan::{
    desugared_assignment_plan, AssignmentProjection, CapturedPlace, PlaceProjection,
};
mod ast_walk;
use ast_walk::{concat_spine, expr_mentions, idents_in_block};
mod tail_analysis;
use tail_analysis::{direct_tail_envelope_is_forwarded, recursive_tail_edges};
mod value_ops;
use value_ops::{eval_binary, match_pattern, native_to_value, value_to_native};
mod runners;
pub use runners::*;
mod builtins;
mod calls;
mod places;
mod capability_values;
use capability_values::{
    dir_child_value, dir_file_value, net_narrow_to, read_file_value, write_file_value,
};
mod reflection;
use reflection::{
    compiler_binding_ident_name, compiler_block_syntax_value, compiler_ctor_tail,
    compiler_direct_hole_origins, compiler_expr_syntax_value, compiler_function_conventions,
    compiler_expr_leaf, compiler_ident_name, compiler_item_hole_origins, compiler_item_holes,
    compiler_match_arms, compiler_optional_expr_syntax_value,
    compiler_optional_type_syntax_value, compiler_params, compiler_pattern_holes,
    compiler_pattern_leaf, compiler_pattern_syntax_value, compiler_reflected_type,
    compiler_stmt_leaf, compiler_stmt_syntax_value,
    compiler_type_holes, compiler_type_syntax_value,
};

#[derive(Debug, Clone, PartialEq)]
pub enum DirValue {
    Fs(witchy_runtime::confine::ConfinedDir),
    #[cfg(feature = "test-fixtures")]
    Fixture(HostHandle),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FileValue {
    Fs(witchy_runtime::confine::ConfinedFile),
    #[cfg(feature = "test-fixtures")]
    Fixture(HostHandle),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(Rc<String>),
    Bytes(Vec<u8>),
    Bool(bool),
    List(Rc<Vec<Value>>),
    Tuple(Rc<Vec<Value>>),
    Ctor { name: Rc<str>, fields: Rc<Vec<Value>> },
    Cap(Capability),
    /// An unforgeable capability to an open directory subtree.
    /// Carries a host directory handle; can only be obtained from the root grant
    /// or by attenuation (`subdir`).
    // A confined directory handle + an entry policy (RFC-0011; `""` =
    // unrestricted). `dir.only(confine.ext(...))` narrows the policy; reads/writes
    // through the Dir are admitted only when the policy admits the entry name.
    Dir(DirValue, String),
    /// A file capability (RFC-0012): authority to one file (the leaf of the
    /// Dir/File hierarchy). Carries an anchored parent handle plus a fixed leaf;
    /// obtained by navigating a `Dir` (`dir.open`/`dir.create`) or as a `main`
    /// grant. Rights are checked at compile time.
    File(FileValue),
    /// A network capability: an allow-list of permitted `host:port` destinations
    /// (wasi:sockets / cap-std-net style). Attenuable via `restrict`.
    Net(Vec<String>),
    /// An origin-scoped HTTP(S) authority shared with the compiled runtime.
    Fetch(witchy_runtime::fetch::FetchPolicy),
    #[cfg(feature = "test-fixtures")]
    FixtureFetch(HostHandle),
    #[cfg(feature = "test-fixtures")]
    FixtureExec(HostHandle),
    #[cfg(feature = "test-fixtures")]
    FixtureSecret(HostHandle),
    #[cfg(feature = "test-fixtures")]
    FixtureSecretStore(HostHandle),
    /// A single secret's raw bytes (a signing seed, or a value secret like a token)
    /// plus its **use-only** flag (RFC-0060). Unforgeable — minted only by the host
    /// or fetched from a `SecretStore`. The ability to use it *is* authority;
    /// `.sign`/`.public_key` read it as a hex Ed25519 seed. `.reveal` returns it
    /// verbatim UNLESS `use_only` is set (or it is the signing key), in which case
    /// reveal errors — the key stays usable by handle only.
    Secret(Vec<u8>, bool),
    /// The host-granted store of NAMED secrets (from `--secret`/`--secret-file`/
    /// `--signing-key`). Each entry is `(bytes, use_only)`; `secret_store.get(name)`
    /// yields a `Secret` carrying both.
    SecretStore(std::collections::BTreeMap<String, (Vec<u8>, bool)>),
    /// A connected socket — a handle into the interpreter's socket table.
    Socket(usize),
    /// A listening server socket — a handle into the interpreter's listener
    /// table. Obtained from `net.listen(addr)`; `accept` blocks for a `Socket`.
    Listener(usize),
    /// A first-class function (closure): a synthetic `Function` (its `name` is
    /// the source owner, keeping a runtime error's function name paired with
    /// the body line that produced it) plus the environment captured where it
    /// was defined. The `Function` is `Rc`'d and built ONCE at closure
    /// creation — cloning a function value (every higher-order call site does)
    /// is a refcount bump, not an AST copy. It is normalized (`ret: None`,
    /// no bounds/flags) exactly like the wrapper `run_callable` used to build
    /// per application, so tail-ABI classification over `tail_function` sees
    /// byte-identical signatures.
    Closure {
        function: Rc<Function>,
        env: Box<Env>,
    },
    /// Compiler-owned RFC-0081 envelope. Source programs cannot observe the
    /// payload or witness; only a resolved existential call may use them.
    Existential {
        payload: Box<Value>,
        witness: u32,
    },
    /// An immutable associative map, kept as insertion-ordered key/value pairs
    /// (keys compared by value equality). `Dict(K, V)` in the type system.
    Dict(Rc<Vec<(Value, Value)>>),
    /// A build-time capability, minted only for a rune's `build` entrypoint and
    /// carrying its confined grant (an output/read directory, or an allow-list).
    /// The build sandbox is where these enter — never `main`.
    Build(BuildCap),
    Unit,
}

impl Value {
    /// Build a `Str` value from any string-ish source. `Value::Str` carries
    /// `Rc<String>` so CLONING a string value is a refcount bump (the deep
    /// `String` copy was the interpreter's single hottest allocation source),
    /// while `Rc::make_mut` keeps the in-place accumulation fast path: append
    /// mutates the buffer directly when the value is unshared and copies-on-
    /// write when it is not — observationally identical to value semantics.
    /// Containers carry `Rc<Vec<..>>`: cloning a list/tuple/record/dict value
    /// bumps a refcount instead of deep-copying elements (the interpreter's
    /// dominant allocation source). Mutation sites go through `Rc::make_mut`,
    /// which mutates in place when the value is unshared — preserving the
    /// in-place fast paths — and copies-on-write when it is not, which is
    /// exactly the eager copy value semantics always implied.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }
    pub fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(Rc::new(items))
    }
    pub fn ctor(name: impl Into<Rc<str>>, fields: Vec<Value>) -> Value {
        Value::Ctor { name: name.into(), fields: Rc::new(fields) }
    }
    pub fn dict(entries: Vec<(Value, Value)>) -> Value {
        Value::Dict(Rc::new(entries))
    }

    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(Rc::new(s.into()))
    }
}

/// Existentials are opaque at every runtime boundary. Keep this structural so
/// unchecked oracle input cannot regain equality by hiding one in a container.
fn contains_existential(value: &Value) -> bool {
    match value {
        Value::Existential { .. } => true,
        Value::List(items) | Value::Tuple(items) => items.iter().any(contains_existential),
        Value::Ctor { fields, .. } => fields.iter().any(contains_existential),
        Value::Dict(entries) => entries
            .iter()
            .any(|(key, value)| contains_existential(key) || contains_existential(value)),
        _ => false,
    }
}


const OWNED_ITEM_SYNTAX_CTOR: &str = "@owned_item_syntax";

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ComptimeItemEmission {
    ModuleSyntax {
        module: Box<Module>,
        definition_line: u32,
        hole_ancestry: Vec<ComptimeHoleOrigin>,
    },
    Syntax {
        item: Box<Item>,
        definition_line: u32,
        hole_ancestry: Vec<ComptimeHoleOrigin>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ComptimeHoleOrigin {
    pub category: SyntaxCategory,
    pub definition_line: u32,
    pub invocation_line: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct ComptimeSyntaxOrigin {
    definition_line: u32,
    hole_ancestry: Vec<ComptimeHoleOrigin>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ComptimeExprEmission {
    Syntax(Box<Expr>),
}

pub(crate) struct ComptimeOutputs {
    pub output: Vec<String>,
    pub items: Vec<PositionedComptimeItem>,
    pub exprs: Vec<ComptimeExprEmission>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PositionedComptimeItem {
    pub output_position: usize,
    pub emission: ComptimeItemEmission,
}

struct InterpreterOutcome {
    output: Vec<String>,
    exit_code: i32,
    comptime_items: Vec<PositionedComptimeItem>,
    comptime_exprs: Vec<ComptimeExprEmission>,
}

/// A build-time capability instance, carrying the attenuated grant the build
/// driver minted it with. Kind-only in the type system; the specifics live here.
#[derive(Debug, Clone, PartialEq)]
pub enum BuildCap {
    /// Write generated source into this confined output directory.
    Out(witchy_runtime::confine::ConfinedDir),
    /// Read project files confined to one of these directory subtrees. A relative
    /// path resolves against the first granted root that contains it.
    Read(Vec<witchy_runtime::confine::ConfinedDir>),
    /// Immutable host snapshot of the granted environment names and values.
    /// A missing map entry is ungranted; `None` is granted but unset.
    Env(BTreeMap<String, Option<String>>),
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    Console,
    Clock,
    Rand,
    Env(Option<std::collections::BTreeSet<String>>),
    /// The right to spawn a native subprocess (`exec`). Right-less and payload-
    /// free: the executable is named + confined by a `Dir[Read]` argument.
    Exec(Option<std::collections::BTreeSet<String>>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) => write!(f, "{}", witchy_syntax::fmt::render_float(*x)),
            Value::Str(s) => write!(f, "{s}"),
            Value::Bytes(b) => write!(f, "Bytes(len={})", b.len()),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Unit => write!(f, "()"),
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
                // (RFC-0042) Constructor names are canonical `module.Ctor`; render
                // the unqualified variant name a reader wrote (`Item`, not
                // `iter.Item`). Both backends strip identically (parity).
                let shown = if name.starts_with('.') {
                    &**name
                } else {
                    name.rsplit_once('.').map_or(&**name, |(_, c)| c)
                };
                write!(f, "{shown}")?;
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
            Value::Dir(..) => write!(f, "<dir>"),
            Value::File(_) => write!(f, "<file>"),
            Value::Net(_) => write!(f, "<net>"),
            Value::Fetch(_) => write!(f, "<fetch>"),
            #[cfg(feature = "test-fixtures")]
            Value::FixtureFetch(_) => write!(f, "<fetch>"),
            #[cfg(feature = "test-fixtures")]
            Value::FixtureExec(_) => write!(f, "<exec>"),
            #[cfg(feature = "test-fixtures")]
            Value::FixtureSecret(_) => write!(f, "<secret>"),
            #[cfg(feature = "test-fixtures")]
            Value::FixtureSecretStore(_) => write!(f, "<secret store>"),
            Value::Secret(_, _) => write!(f, "<secret>"),
            Value::SecretStore(_) => write!(f, "<secret store>"),
            Value::Socket(id) => write!(f, "<socket #{id}>"),
            Value::Listener(id) => write!(f, "<listener #{id}>"),
            Value::Build(_) => write!(f, "<build capability>"),
            Value::Closure { function, .. } => {
                write!(f, "<function/{}>", function.params.len())
            }
            Value::Existential { .. } => write!(f, "<existential>"),
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

fn is_generated_anon_name(name: &str) -> bool {
    name.strip_prefix("__anon")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
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
    /// A proper tail call. The enclosing callable boundary replaces its logical
    /// function/closure and parameter environment without growing the Rust stack.
    TailCall { callable: TailCallable, args: Vec<Value> },
    /// `break` — caught by the innermost loop, which stops.
    Break,
    /// `continue` — caught by the innermost loop, which proceeds to the next
    /// iteration.
    Continue,
}

#[derive(Clone)]
enum TailCallable {
    Function(Rc<Function>),
    Closure(Value),
}

struct CallableOutcome {
    value: Value,
    function: Rc<Function>,
    env: Env,
}

/// The normalized synthetic `Function` a closure value carries: the shape
/// `run_callable` historically rebuilt per application (`ret: None`, no
/// bounds/flags, `public: false`). Built once at closure CREATION so
/// application and tail-classification see the identical struct with zero
/// per-call AST copies.
fn closure_function(owner: String, params: Vec<Param>, body: Block) -> Rc<Function> {
    Rc::new(Function {
        public: false,
        comptime_only: false,
        attributes: Vec::new(),
        name: owner,
        params,
        ret: None,
        body,
        bounds: Vec::new(),
        is_gen: false,
        is_async: false,
    })
}

struct ClosureOutcome {
    value: Value,
    writebacks: Vec<(usize, Value)>,
}

/// Does `call_interpreter_special` or `call_builtin` handle this (surfaced) name?
/// A user function name satisfies NONE of these, so `eval_call` can skip both
/// dispatch helpers — each of which independently re-probes the intrinsic table
/// (`is_*_extract` ×3, `call_builtin`'s own `lookup`, `native::lookup`) — when this
/// returns false. That was ~33% of call-dense interpreter self-time (perf sweep).
///
/// Completeness is load-bearing: if a builtin name is NOT listed here, the fast
/// path would skip it and it would fall through to "unknown function". The three
/// sources are: the intrinsic table (list/dict/math/string/crypto/... ops), the
/// capability ops (`cap_ops::is_op_name` — bare surface names like `print`/`now`/
/// `read`/`write`, dispatched by `match` in `call_builtin`, NOT in the intrinsic
/// table), and the handful handled by neither table (`fail`, `duration_to_int`,
/// `int_to_duration`, `vm.par_map`). The test
/// `interpreter_builtin_names_are_covered`
/// (test) asserts every `call_builtin` dispatch arm is covered here.
fn is_interpreter_builtin(name: &str) -> bool {
    intrinsics::lookup(name).is_some()
        || witchy_syntax::cap_ops::is_op_name(name)
        || matches!(
            name,
            "fail"
                | "duration_to_int"
                | "int_to_duration"
                | "frozen"
                | "unique"
                | "vm.par_map"
        )
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

/// RFC-0038: `[user_caps]` grant values — `main` parameter name → (field name →
/// value). A grantable-cap `main` parameter mints a sealed record from its entry.
pub type UserCapGrants = std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>;

pub struct Interpreter {
    // `Rc` so a call clones a pointer, not the whole function AST (this is the
    // hot path for recursion).
    functions: FxHashMap<String, Rc<Function>>,
    /// Closed compiler-owned witness plan for the executable module.
    witnesses: WitnessPlan,
    /// Per-function parameter names as shared `Rc<str>`, built once at
    /// registration: binding call arguments clones a pointer per parameter
    /// instead of copying each name `String` on every call.
    /// Keyed by the `Rc<Function>` allocation address — pointer identity is
    /// authoritative for WHICH function object a call binds (names are not:
    /// comptime-emitted or closure-carried functions can share a name with a
    /// registered one while having different parameters). A miss falls back
    /// to allocating the names, so absent entries are always correct.
    param_names: FxHashMap<usize, Rc<[Rc<str>]>>,
    /// One shared `Rc<str>` per distinct binding name seen by `let`/loops —
    /// see `intern`.
    interned_names: FxHashMap<String, Rc<str>>,
    /// Memoized bare-function closure values (`Expr::Var` on a top-level
    /// function name). The function table is immutable for a run, so the
    /// wrapped value is too; re-evaluating the name clones Rcs instead of
    /// re-copying the function's AST.
    fn_values: FxHashMap<String, Value>,
    /// Host directory the root `Dir` capability is rooted at (handle 0 / the
    /// first `Dir` parameter of `main`).
    root: PathBuf,
    /// Additional `Dir` grants for a `main` taking several `Dir` params: the
    /// i-th `Dir` param (i>=1) is rooted at `dir_roots[i-1]`, falling back to
    /// `root`. Empty for the common single-`Dir` case. Mirrors the compiled
    /// backend's `Capabilities::dir_roots`. See rfcs/0004-self-hosted-cli.md.
    dir_roots: Vec<PathBuf>,
    /// Direct `File` grant inputs (RFC-0012): the i-th `File` param of `main` is
    /// admitted from the i-th path here, then carried as an anchored file
    /// authority. Mirrors the compiled backend's `Capabilities::file_grants`.
    file_grants: Vec<PathBuf>,
    /// Allow-list backing the root `Net` capability.
    net_allow: Vec<String>,
    /// Origin allow-list backing the root `Fetch` capability.
    fetch_origins: Vec<String>,
    /// Explicit Console input fixture. `None` reads native stdin; `Some` consumes
    /// the supplied lines and returns an empty string after exhaustion.
    console_input: Option<std::collections::VecDeque<String>>,
    /// (`Rand`) splitmix64 state for `rand_u64`. Seeded from `WITCHY_RAND_SEED` for
    /// deterministic parity/tests; `None` until first use, then clock-seeded (the
    /// interpreter is the oracle/playground, never the production CSPRNG path).
    rand_state: Option<u64>,
    /// Ed25519 seed backing the root `Secret` capability, if the host granted
    /// one. A `main` that declares a `Secret` parameter requires this.
    signing_key: Option<[u8; 32]>,
    /// Named secrets backing the `SecretStore` capability (from
    /// `--secret`/`--secret-file`/`--signing-key`), each `(bytes, use_only)`.
    /// `secret_store.get(name)` mints a `Secret` carrying both. (RFC-0060)
    secrets: std::collections::BTreeMap<String, (Vec<u8>, bool)>,
    /// RFC-0038: grant-document `[user_caps]` field values for a `main` that binds
    /// grantable capabilities. The grantable-cap param mints a sealed record from
    /// its entry here (empty unless launched with a `[user_caps]` grant).
    user_cap_grants: UserCapGrants,
    /// Open sockets, indexed by `Value::Socket` handle. Each is a plain or TLS byte
    /// stream behind one `dyn Stream` (RFC-0009 terminates `tls:` host-side, so
    /// `send_line`/`recv_line` operate on either without knowing which).
    sockets: Vec<BufReader<Box<dyn Stream>>>,
    /// Listening server sockets, indexed by `Value::Listener` handle, each with
    /// its server-TLS config (RFC-0060): `Some` for a `listen_tls` listener, whose
    /// accepts handshake host-side through the SAME shared module the compiled
    /// runtime uses (`witchy_runtime::net`); `None` for plain HTTP.
    listeners: Vec<(TcpListener, Option<witchy_runtime::net::ServerTlsConfig>)>,
    /// Record constructor name -> ordered field names, for `value.field` access.
    record_fields: FxHashMap<String, Vec<String>>,
    /// (RFC-0047) Constructor name -> its declaring type name, so value equality
    /// can find the type of a `Ctor` value and consult `custom_eq_types`.
    ctor_type_name: FxHashMap<String, String>,
    /// (RFC-0047) Type names with a CUSTOM (non-derived) `PartialEq` impl. `==`/`!=`
    /// honor these impls at EVERY depth: a container comparing elements of such a
    /// type calls its `PartialEq__T__eq` instead of recursing structurally, so a
    /// custom equality is respected inside `List`/`Option`/tuple/`Dict`/records.
    /// A derived (structural) impl is NOT here, so its containers keep the fast
    /// structural compare — behavior-identical to before.
    custom_eq_types: FxHashSet<String>,
    /// Deterministic namespace and sequence for RFC-0080 `meta.fresh`. They are
    /// present only in a compile-time evaluator; ordinary runtime interpreters
    /// reject the compiler-private hook instead of minting generated names.
    fresh_ident_scope: Option<String>,
    fresh_ident_counter: u64,
    compiler_syntax_instance_counter: u64,
    compiler_item_syntax: HashMap<String, Item>,
    compiler_item_modules: HashMap<String, Module>,
    compiler_expr_syntax: HashMap<String, Expr>,
    /// Module names that dynamic generated expressions may use as qualifiers.
    /// Populated only for tagged-literal compile-time evaluation.
    compiler_expr_qualifiers: Option<Vec<String>>,
    compiler_type_syntax: HashMap<String, Type>,
    compiler_pattern_syntax: HashMap<String, Pattern>,
    compiler_match_arm_syntax: HashMap<String, MatchArm>,
    compiler_param_syntax: HashMap<String, Param>,
    compiler_expr_origins: HashMap<String, ComptimeSyntaxOrigin>,
    compiler_type_origins: HashMap<String, ComptimeSyntaxOrigin>,
    compiler_pattern_origins: HashMap<String, ComptimeSyntaxOrigin>,
    compiler_stmt_syntax: HashMap<String, Stmt>,
    compiler_block_syntax: HashMap<String, Block>,
    comptime_item_output: Vec<PositionedComptimeItem>,
    comptime_expr_output: Vec<ComptimeExprEmission>,
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
    /// Current named function boundary, used to recognize explicit-return tail
    /// calls even when the `return` is nested in a non-tail control expression.
    tail_function: Option<Rc<Function>>,
    /// The active tail chain has crossed a function value. Later named edges may
    /// still close the dynamic cycle, so they trampoline without the named-only
    /// SCC prefilter until this callable boundary returns.
    tail_dynamic_chain: bool,
    /// Direct proper-call edges that belong to a recursive component. Acyclic
    /// tail calls retain their ordinary call boundary; recursive SCCs trampoline.
    proper_tail_edges: FxHashMap<String, FxHashSet<String>>,
    pub output: Vec<String>,
    #[cfg(feature = "test-fixtures")]
    fixture_host: Option<FixtureHost>,
    #[cfg(feature = "test-fixtures")]
    fixture_env_handles:
        BTreeMap<Option<std::collections::BTreeSet<String>>, HostHandle>,
}

/// Maximum call-nesting depth. Comfortably below what the 4 GiB interpreter
/// thread can hold (debug frames are large), but far deeper than any reasonable
/// program recurses.
const DEFAULT_DEPTH_LIMIT: u32 = 25_000;

fn encode_fresh_scope(scope: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(scope.len().saturating_mul(2));
    for byte in scope.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

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
    fn render_value(&self, value: &Value) -> String {
        match value {
            Value::List(items) => format!(
                "[{}]",
                items.iter().map(|v| self.render_value(v)).collect::<Vec<_>>().join(", ")
            ),
            Value::Tuple(items) => format!(
                "({})",
                items.iter().map(|v| self.render_value(v)).collect::<Vec<_>>().join(", ")
            ),
            Value::Ctor { name, fields } if is_generated_anon_name(name) => {
                let Some(names) = self.record_fields.get(&**name) else {
                    return value.to_string();
                };
                if names.len() != fields.len() {
                    return value.to_string();
                }
                let parts = names
                    .iter()
                    .zip(fields.iter())
                    .map(|(name, field)| format!("{name}: {}", self.render_value(field)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(".{{{parts}}}")
            }
            Value::Ctor { name, fields } => {
                let shown = if name.starts_with('.') {
                    &**name
                } else {
                    name.rsplit_once('.').map_or(&**name, |(_, c)| c)
                };
                if fields.is_empty() {
                    shown.to_string()
                } else {
                    format!(
                        "{shown}({})",
                        fields.iter().map(|v| self.render_value(v)).collect::<Vec<_>>().join(", ")
                    )
                }
            }
            Value::Dict(entries) => format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", self.render_value(k), self.render_value(v)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => value.to_string(),
        }
    }

    pub fn new(module: Module) -> Self {
        Self::new_with_witnesses(module, WitnessPlan::default())
    }

    fn new_with_witnesses(module: Module, witnesses: WitnessPlan) -> Self {
        let compiler_item_syntax = module
            .compiler_item_syntax
            .into_iter()
            .map(|syntax| (syntax.handle, syntax.item))
            .collect();
        let compiler_expr_syntax = module
            .compiler_expr_syntax
            .into_iter()
            .map(|syntax| (syntax.handle, syntax.expr))
            .collect();
        let compiler_type_syntax = module
            .compiler_type_syntax
            .into_iter()
            .map(|syntax| (syntax.handle, syntax.ty))
            .collect();
        let compiler_pattern_syntax = module
            .compiler_pattern_syntax
            .into_iter()
            .map(|syntax| (syntax.handle, syntax.pattern))
            .collect();
        let compiler_expr_origins = HashMap::new();
        let compiler_type_origins = HashMap::new();
        let compiler_pattern_origins = HashMap::new();
        let compiler_stmt_syntax = module
            .compiler_stmt_syntax
            .into_iter()
            .map(|syntax| (syntax.handle, syntax.stmt))
            .collect();
        let compiler_block_syntax = module
            .compiler_block_syntax
            .into_iter()
            .map(|syntax| (syntax.handle, syntax.block))
            .collect();
        let mut functions = FxHashMap::new();
        let mut record_fields = FxHashMap::new();
        let mut ctor_type_name: FxHashMap<String, String> = FxHashMap::new();
        // (RFC-0047) Type names that DERIVED PartialEq (their impl is structural)
        // vs. those declared. The `Item::Impl` was already desugared to a
        // `PartialEq__T__eq` function by `traits::lower`, so custom-eq is detected
        // post-lowering as "a declared type whose PartialEq impl exists but was NOT
        // derived" (see `custom_eq_types` assembly below).
        let mut declared_types: Vec<(String, bool)> = Vec::new();
        for item in module.items {
            match item {
                Item::Function(f) => {
                    functions.insert(f.name.clone(), Rc::new(f));
                }
                // Types are erased at runtime, except a record's field names,
                // which map `value.field` to a position in the constructor.
                Item::Type(t) => {
                    for v in &t.variants {
                        ctor_type_name.insert(v.name.clone(), t.name.clone());
                        if !v.field_names.is_empty() {
                            record_fields.insert(v.name.clone(), v.field_names.clone());
                        }
                    }
                    declared_types.push((t.name.clone(), t.partial_eq_derived));
                }
                // Desugared to functions by `traits::lower` before this point;
                // constants are inlined by `witchy_syntax::consts`.
                Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
            }
        }
        // (RFC-0047) A declared type has a CUSTOM (non-derived) PartialEq exactly
        // when the desugared `PartialEq__T__eq` function is present and the type did
        // NOT derive PartialEq/Eq. `==`/`!=` then honor that impl at every depth.
        // Built AFTER registration from the SURVIVING entries: `functions` is
        // last-wins on name collisions, and it pins every keyed `Rc<Function>`
        // allocation for the interpreter's lifetime — so a pointer key can
        // never outlive its function and be reused by a later allocation
        // (adversarial-review hardening; collisions are also rejected upstream
        // by typeck's unique-function check).
        let param_names: FxHashMap<usize, Rc<[Rc<str>]>> = functions
            .values()
            .map(|f| {
                (
                    Rc::as_ptr(f) as usize,
                    f.params.iter().map(|p| Rc::from(p.name.as_str())).collect(),
                )
            })
            .collect();
        let custom_eq_types: FxHashSet<String> = declared_types
            .into_iter()
            .filter(|(name, derived)| {
                !derived && functions.contains_key(&format!("PartialEq__{name}__eq"))
            })
            .map(|(name, _)| name)
            .collect();
        let proper_tail_edges = recursive_tail_edges(&functions);
        Self {
            functions,
            witnesses,
            root: PathBuf::from("."),
            dir_roots: Vec::new(),
            file_grants: Vec::new(),
            net_allow: Vec::new(),
            fetch_origins: Vec::new(),
            console_input: None,
            rand_state: witchy_runtime::rand::seed_from_env(),
            signing_key: None,
            secrets: std::collections::BTreeMap::new(),
            user_cap_grants: UserCapGrants::new(),
            sockets: Vec::new(),
            listeners: Vec::new(),
            record_fields,
            ctor_type_name,
            custom_eq_types,
            fresh_ident_scope: None,
            fresh_ident_counter: 0,
            compiler_syntax_instance_counter: 0,
            compiler_item_syntax,
            compiler_item_modules: HashMap::new(),
            compiler_expr_syntax,
            compiler_expr_qualifiers: None,
            compiler_type_syntax,
            compiler_pattern_syntax,
            compiler_match_arm_syntax: HashMap::new(),
            compiler_param_syntax: HashMap::new(),
            compiler_expr_origins,
            compiler_type_origins,
            compiler_pattern_origins,
            compiler_stmt_syntax,
            compiler_block_syntax,
            comptime_item_output: Vec::new(),
            comptime_expr_output: Vec::new(),
            steps: 0,
            step_limit: DEFAULT_STEP_LIMIT,
            cur_line: 0,
            cur_fn: String::new(),
            assert_site: None,
            depth: 0,
            depth_limit: DEFAULT_DEPTH_LIMIT,
            fn_values: FxHashMap::new(),
            param_names,
            interned_names: FxHashMap::new(),
            tail_function: None,
            tail_dynamic_chain: false,
            proper_tail_edges,
            output: Vec::new(),
            #[cfg(feature = "test-fixtures")]
            fixture_host: None,
            #[cfg(feature = "test-fixtures")]
            fixture_env_handles: BTreeMap::new(),
        }
    }

    #[cfg(feature = "test-fixtures")]
    fn fixture_source(&self) -> Option<SourceLocation> {
        if self.cur_line == 0 {
            return None;
        }
        let module = self
            .cur_fn
            .rsplit_once('.')
            .map_or(self.cur_fn.as_str(), |(module, _)| module);
        Some(SourceLocation {
            module: module.to_owned(),
            line: U64Text::new(u64::from(self.cur_line)),
            column: U64Text::new(1),
        })
    }

    #[cfg(feature = "test-fixtures")]
    fn invoke_fixture(
        &mut self,
        request: HostRequest,
    ) -> Result<HostResponse, RuntimeError> {
        self.invoke_fixture_raw(request)
            .map_err(Self::fixture_failure_error)
    }

    #[cfg(feature = "test-fixtures")]
    fn invoke_fixture_raw(
        &mut self,
        request: HostRequest,
    ) -> Result<HostResponse, FixtureFailure> {
        let source = self.fixture_source();
        let host = self.fixture_host.as_mut().ok_or_else(|| FixtureFailure {
            code: FixtureErrorCode::ProviderFailure,
            message: "internal error: fixture operation without a fixture host".into(),
        })?;
        host.invoke(request, source)
    }

    #[cfg(feature = "test-fixtures")]
    fn fixture_failure_error(failure: FixtureFailure) -> RuntimeError {
        RuntimeError {
            message: format!(
                "fixture {:?}: {}",
                failure.code, failure.message
            ),
        }
    }

    #[cfg(feature = "test-fixtures")]
    fn fixture_root_cap_for(
        &self,
        ty: &Option<Type>,
    ) -> Result<Value, RuntimeError> {
        let roots = self
            .fixture_host
            .as_ref()
            .expect("fixture root construction requires a fixture host")
            .roots();
        let missing = |name: &str| RuntimeError {
            message: format!(
                "`main` requires `{name}`, but the fixture plan declared no `{name}` provider"
            ),
        };
        match ty {
            Some(Type::Named(name, _)) if name == "Console" => roots
                .console
                .then_some(Value::Cap(Capability::Console))
                .ok_or_else(|| missing("Console")),
            Some(Type::Named(name, _)) if name == "Clock" => roots
                .clock
                .then_some(Value::Cap(Capability::Clock))
                .ok_or_else(|| missing("Clock")),
            Some(Type::Named(name, _)) if name == "Rand" => roots
                .rand
                .then_some(Value::Cap(Capability::Rand))
                .ok_or_else(|| missing("Rand")),
            Some(Type::Named(name, _)) if name == "Env" => roots
                .env
                .map(|_| Value::Cap(Capability::Env(None)))
                .ok_or_else(|| missing("Env")),
            Some(Type::Named(name, _)) if name == "Dir" => roots
                .filesystem
                .map(|handle| {
                    Value::Dir(DirValue::Fixture(handle), String::new())
                })
                .ok_or_else(|| missing("Dir")),
            Some(Type::Named(name, _)) if name == "Fetch" => roots
                .fetch
                .map(Value::FixtureFetch)
                .ok_or_else(|| missing("Fetch")),
            Some(Type::Named(name, _)) if name == "Exec" => roots
                .exec
                .map(Value::FixtureExec)
                .ok_or_else(|| missing("Exec")),
            Some(Type::Named(name, _)) if name == "SecretStore" => roots
                .secrets
                .map(Value::FixtureSecretStore)
                .ok_or_else(|| missing("SecretStore")),
            Some(Type::Named(name, _)) if name == "Net" => Err(RuntimeError {
                message:
                    "raw `Net` is integration-only and cannot be provided by deterministic fixtures"
                        .into(),
            }),
            Some(Type::Named(name, _))
                if matches!(
                    name.as_str(),
                    "File" | "Secret" | "Vm"
                ) =>
            {
                Err(RuntimeError {
                    message: format!(
                        "the deterministic interpreter adapter does not yet support `{name}`"
                    ),
                })
            }
            other => {
                let found = match other {
                    Some(found) => {
                        format!("`{}`", witchy_syntax::format::type_str(found))
                    }
                    None => "no type annotation".to_owned(),
                };
                Err(RuntimeError {
                    message: format!(
                        "fixture `main` parameters must be declared fixture roots or `List(String)` argv; got {found}"
                    ),
                })
            }
        }
    }

    /// Mint the root capability for a `main` parameter of the given type. This
    /// is where authority enters the program — `main` is the root entrypoint.
    fn root_cap_for(&self, ty: &Option<Type>) -> Result<Value, RuntimeError> {
        #[cfg(feature = "test-fixtures")]
        if self.fixture_host.is_some() {
            return self.fixture_root_cap_for(ty);
        }
        match ty {
            Some(Type::Named(n, _)) if n == "Console" => Ok(Value::Cap(Capability::Console)),
            Some(Type::Named(n, _)) if n == "Clock" => Ok(Value::Cap(Capability::Clock)),
            Some(Type::Named(n, _)) if n == "Rand" => Ok(Value::Cap(Capability::Rand)),
            Some(Type::Named(n, _)) if n == "Env" => Ok(Value::Cap(Capability::Env(None))),
            Some(Type::Named(n, _)) if n == "Dir" => {
                let root = witchy_runtime::confine::ConfinedDir::open_ambient(&self.root)
                    .map_err(|error| RuntimeError { message: error.0 })?;
                Ok(Value::Dir(DirValue::Fs(root), String::new()))
            }
            Some(Type::Named(n, _)) if n == "Net" => Ok(Value::Net(self.net_allow.clone())),
            Some(Type::Named(n, _)) if n == "Fetch" => {
                witchy_runtime::fetch::FetchPolicy::allow(self.fetch_origins.clone())
                    .map(Value::Fetch)
                    .map_err(|error| RuntimeError {
                        message: format!("invalid Fetch grant: {error}"),
                    })
            }
            Some(Type::Named(n, _)) if n == "Exec" => Ok(Value::Cap(Capability::Exec(None))),
            Some(Type::Named(n, _)) if n == "Secret" => match self.signing_key {
                // The bare `Secret` is the signing key: revealable=false is enforced by
                // the signing-key identity check, so its `use_only` flag stays false here.
                Some(seed) => Ok(Value::Secret(seed.to_vec(), false)),
                None => err("`main` requires a `Secret`, but the host granted none (provide `--signing-key <hex-seed-file>`)"),
            },
            Some(Type::Named(n, _)) if n == "SecretStore" => Ok(Value::SecretStore(self.secrets.clone())),
            other => {
                let found = match other {
                    Some(t) => format!("`{}`", witchy_syntax::format::type_str(t)),
                    None => "no type annotation".to_string(),
                };
                err(format!(
                    "`main` parameters must be capabilities (Console, Clock, Env, Dir, Net, Fetch, Exec, Secret) or `List(String)` for command-line args; got {found}"
                ))
            }
        }
    }

    /// RFC-0038: mint a bare grantable user capability for a `main` parameter — a
    /// sealed record built from the `[user_caps]` grant fields, in the type's field
    /// order. Typeck guarantees any record-typed `main` parameter is a bare
    /// grantable cap; mode-a minting supports policy fields of type `String`.
    fn mint_user_cap(&self, param: &str, ty: &str) -> Result<Value, RuntimeError> {
        let field_order = self.record_fields.get(ty).cloned().unwrap_or_default();
        let grant = self.user_cap_grants.get(param).ok_or_else(|| RuntimeError {
            message: format!(
                "`main` binds the grantable capability `{ty}` (parameter `{param}`), but the host \
                 granted none — add a `[user_caps]` entry (e.g. `{param} = {{ type = \"{ty}\", … }}`)"
            ),
        })?;
        let mut fields = Vec::with_capacity(field_order.len());
        for fname in &field_order {
            let v = grant.get(fname).ok_or_else(|| RuntimeError {
                message: format!(
                    "the `[user_caps]` grant for `{param}` is missing field `{fname}` required by `{ty}`"
                ),
            })?;
            fields.push(Value::str(v.as_str()));
        }
        Ok(Value::ctor(ty.to_string(), fields))
    }

    /// Mint a build-time capability for a `build` parameter, from the confined
    /// grants the build driver was handed. This is where build-time authority
    /// enters — never `main`. A demanded cap with no matching grant is an error
    /// (safe by default: only `BuildOut` is supplied unconditionally).
    fn mint_build_cap(&self, ty: &Option<Type>, grants: &BuildGrants) -> Result<Value, RuntimeError> {
        match ty {
            Some(Type::Named(n, _)) if n == "BuildOut" => {
                let out = witchy_runtime::confine::ConfinedDir::open_ambient(&grants.out_dir)
                    .map_err(|error| RuntimeError { message: error.0 })?;
                Ok(Value::Build(BuildCap::Out(out)))
            }
            Some(Type::Named(n, _)) if n == "BuildRead" => {
                if grants.read_roots.is_empty() {
                    return err("build step demands `BuildRead` but no read grant was provided");
                }
                let roots = grants
                    .read_roots
                    .iter()
                    .map(|root| witchy_runtime::confine::ConfinedDir::open_ambient(root))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| RuntimeError { message: error.0 })?;
                Ok(Value::Build(BuildCap::Read(roots)))
            }
            Some(Type::Named(n, _)) if n == "BuildEnv" => {
                Ok(Value::Build(BuildCap::Env(grants.env.clone())))
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
    /// (RFC-0047) Value equality that honors a custom `PartialEq` impl at every
    /// depth. Only invoked when the program has at least one custom-eq type. When a
    /// value's concrete type has a custom (non-derived) impl, dispatch to its
    /// `PartialEq__T__eq`; otherwise compare structurally, recursing into container
    /// elements/fields so a custom impl nested inside is still honored. Mirrors the
    /// compiled backend's per-shape eq helpers (which call the user impl mid-recursion).
    fn values_equal(&mut self, a: &Value, b: &Value) -> Result<bool, RuntimeError> {
        if matches!(a, Value::Existential { .. }) || matches!(b, Value::Existential { .. }) {
            return err("existential values do not support equality");
        }
        // A `Ctor` whose type has a custom impl: call it (this is the whole point).
        if let (Value::Ctor { name: an, .. }, Value::Ctor { name: bn, .. }) = (a, b) {
            if let Some(tyname) = self.ctor_type_name.get(&**an).cloned() {
                if self.custom_eq_types.contains(&tyname) {
                    // Only dispatch when both sides are the SAME custom-eq type;
                    // otherwise the impl's `other: T` parameter wouldn't type. (The
                    // checker guarantees `==` operands share a type, so this holds.)
                    let same_type = self.ctor_type_name.get(&**bn).map(|t| t == &tyname).unwrap_or(false);
                    if same_type {
                        let mangled = format!("PartialEq__{tyname}__eq");
                        let result = self.call(&mangled, vec![a.clone(), b.clone()])?;
                        return match result {
                            Value::Bool(v) => Ok(v),
                            other => err(format!(
                                "`{mangled}` returned `{other}`, expected a Bool"
                            )),
                        };
                    }
                }
            }
        }
        // Structural, recursing so a custom-eq type nested inside is honored.
        Ok(match (a, b) {
            (Value::List(xs), Value::List(ys)) => {
                xs.len() == ys.len()
                    && {
                        for (x, y) in xs.iter().zip(ys.iter()) {
                            if !self.values_equal(x, y)? {
                                return Ok(false);
                            }
                        }
                        true
                    }
            }
            (Value::Tuple(xs), Value::Tuple(ys)) => {
                xs.len() == ys.len()
                    && {
                        for (x, y) in xs.iter().zip(ys.iter()) {
                            if !self.values_equal(x, y)? {
                                return Ok(false);
                            }
                        }
                        true
                    }
            }
            (Value::Ctor { name: an, fields: af }, Value::Ctor { name: bn, fields: bf }) => {
                an == bn
                    && af.len() == bf.len()
                    && {
                        for (x, y) in af.iter().zip(bf.iter()) {
                            if !self.values_equal(x, y)? {
                                return Ok(false);
                            }
                        }
                        true
                    }
            }
            (Value::Dict(xs), Value::Dict(ys)) => {
                // Insertion-order-sensitive pairwise compare, as the structural
                // `==` and the compiled `$eq_dict_*` do.
                xs.len() == ys.len()
                    && {
                        for ((xk, xv), (yk, yv)) in xs.iter().zip(ys.iter()) {
                            if !self.values_equal(xk, yk)? || !self.values_equal(xv, yv)? {
                                return Ok(false);
                            }
                        }
                        true
                    }
            }
            // Scalars and everything else fall back to the derived structural `==`.
            _ => a == b,
        })
    }

    fn dict_key_position(
        &mut self,
        entries: &[(Value, Value)],
        key: &Value,
    ) -> Result<Option<usize>, RuntimeError> {
        for (index, (candidate, _)) in entries.iter().enumerate() {
            if self.values_equal(candidate, key)? {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

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
        match self.run_callable(TailCallable::Function(func), args) {
            Ok(outcome) => Ok(outcome.value),
            Err(Flow::Err(error)) => Err(error),
            Err(Flow::Return(_) | Flow::TailCall { .. } | Flow::Break | Flow::Continue) => {
                err("invalid control flow escaped a callable boundary")
            }
        }
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



    /// One `Rand` draw: advance splitmix64. Seeded from `WITCHY_RAND_SEED` (set in
    /// `new`) it is deterministic and matches the compiled backend; otherwise lazily
    /// clock-seed on first use so the oracle still varies per run.
    fn rand_next(&mut self) -> i64 {
        let mut s = self.rand_state.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
                ^ 0x9E37_79B9_7F4A_7C15
        });
        let v = witchy_runtime::rand::seeded_next(&mut s);
        self.rand_state = Some(s);
        v as i64
    }

    fn next_fresh_ident(&mut self, hint: &str) -> Result<String, RuntimeError> {
        let Some(scope) = self.fresh_ident_scope.as_deref() else {
            return err("meta.fresh is available only during compile-time expansion");
        };
        let ordinal = self.fresh_ident_counter;
        self.fresh_ident_counter = self
            .fresh_ident_counter
            .checked_add(1)
            .ok_or_else(|| RuntimeError { message: "meta.fresh identifier counter overflowed".into() })?;
        Ok(format!(
            "__witchy_fresh_{}_{ordinal}_{hint}",
            encode_fresh_scope(scope)
        ))
    }

    fn next_compiler_syntax_handle(&mut self, category: &str) -> Result<String, RuntimeError> {
        let ordinal = self.compiler_syntax_instance_counter;
        self.compiler_syntax_instance_counter = self
            .compiler_syntax_instance_counter
            .checked_add(1)
            .ok_or_else(|| RuntimeError {
                message: "compiler syntax instance counter overflowed".into(),
            })?;
        Ok(format!("\0compiler-syntax-instance\0{category}\0{ordinal}"))
    }

    fn store_compiler_stmt_syntax(
        &mut self,
        category: &str,
        stmt: Stmt,
    ) -> Result<Value, RuntimeError> {
        let source = witchy_syntax::format::stmt_str(&stmt);
        let handle = self.next_compiler_syntax_handle(category)?;
        self.compiler_stmt_syntax.insert(handle.clone(), stmt);
        Ok(Value::Ctor {
            name: "meta.CompilerStmtSyntax".into(),
            fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
        })
    }

    fn store_compiler_expr_syntax(
        &mut self,
        category: &str,
        expr: Expr,
        hole_ancestry: Vec<ComptimeHoleOrigin>,
    ) -> Result<Value, RuntimeError> {
        let source = witchy_syntax::format::expr_str(&expr);
        let handle = self.next_compiler_syntax_handle(category)?;
        self.compiler_expr_syntax.insert(handle.clone(), expr);
        self.compiler_expr_origins.insert(
            handle.clone(),
            ComptimeSyntaxOrigin {
                definition_line: self.cur_line,
                hole_ancestry,
            },
        );
        Ok(Value::Ctor {
            name: "meta.CompilerExprSyntax".into(),
            fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
        })
    }

    fn store_compiler_type_syntax(
        &mut self,
        category: &str,
        ty: Type,
        hole_ancestry: Vec<ComptimeHoleOrigin>,
    ) -> Result<Value, RuntimeError> {
        let source = witchy_syntax::format::type_str(&ty);
        let handle = self.next_compiler_syntax_handle(category)?;
        self.compiler_type_syntax.insert(handle.clone(), ty);
        self.compiler_type_origins.insert(
            handle.clone(),
            ComptimeSyntaxOrigin {
                definition_line: self.cur_line,
                hole_ancestry,
            },
        );
        Ok(Value::Ctor {
            name: "meta.CompilerTypeSyntax".into(),
            fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
        })
    }

    fn store_compiler_pattern_syntax(
        &mut self,
        category: &str,
        pattern: Pattern,
        hole_ancestry: Vec<ComptimeHoleOrigin>,
    ) -> Result<Value, RuntimeError> {
        let source = witchy_syntax::format::pattern_str(&pattern);
        let handle = self.next_compiler_syntax_handle(category)?;
        self.compiler_pattern_syntax.insert(handle.clone(), pattern);
        self.compiler_pattern_origins.insert(
            handle.clone(),
            ComptimeSyntaxOrigin {
                definition_line: self.cur_line,
                hole_ancestry,
            },
        );
        Ok(Value::Ctor {
            name: "meta.CompilerPatternSyntax".into(),
            fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
        })
    }


    /// The interpreter-side linear-update fast path: a self-assignment of an
    /// accumulation shape — `list.push(xs, e)`, `dict.insert(d, k, v)`,
    /// `dict.update(d, k, dflt, f)`, `s = s + p` (any left spine) — mutates the
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
                if matches!(f.as_str(), intrinsics::LIST_PUSH | intrinsics::GENERATED_LIST_PUSH)
                    && args.len() == 2
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
                Rc::make_mut(items).push(x);
                Ok(true)
            }
            Expr::Call { name: f, args }
                if f == intrinsics::DICT_INSERT && args.len() == 3
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
                let position = {
                    let Some((Value::Dict(entries), true)) = env.slot_mut(name) else {
                        unreachable!("slot checked above; the arguments cannot reach it");
                    };
                    self.dict_key_position(entries, &k)?
                };
                let Some((Value::Dict(entries), true)) = env.slot_mut(name) else {
                    unreachable!("slot checked above; the arguments cannot reach it");
                };
                let entries = Rc::make_mut(entries);
                match position {
                    Some(index) => entries[index].1 = v,
                    None => entries.push((k, v)),
                }
                Ok(true)
            }
            // `update` is matched before locals in `eval_call`, so no shadow check.
            Expr::Call { name: f, args }
                if f == intrinsics::DICT_UPDATE && args.len() == 4
                    && matches!(&args[0], Expr::Var(v) if v == name)
                    && args[1..].iter().all(|a| !expr_mentions(a, name)) =>
            {
                if !matches!(env.slot_mut(name), Some((Value::Dict(_), true))) {
                    return Ok(false);
                }
                let k = self.eval(&args[1], env)?;
                let dflt = self.eval(&args[2], env)?;
                let updater = self.eval(&args[3], env)?;
                let (position, current) = {
                    let Some((Value::Dict(entries), true)) = env.slot_mut(name) else {
                        unreachable!("slot checked above; the arguments cannot reach it");
                    };
                    let position = self.dict_key_position(entries, &k)?;
                    let current = position
                        .map(|index| entries[index].1.clone())
                        .unwrap_or(dflt);
                    (position, current)
                };
                let new_v = self.apply_closure(updater, vec![current])?;
                let Some((Value::Dict(entries), true)) = env.slot_mut(name) else {
                    unreachable!("slot checked above; the closure cannot reach it");
                };
                let entries = Rc::make_mut(entries);
                match position {
                    Some(index) => entries[index].1 = new_v,
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
                // Own the buffer while accumulating: unwrapping a unique Rc
                // keeps the in-place append fast path; a shared one is copied
                // once (copy-on-write — observationally identical).
                let mut acc = match std::mem::replace(slot, Value::Unit) {
                    Value::Str(s) => Rc::try_unwrap(s).unwrap_or_else(|rc| (*rc).clone()),
                    _ => unreachable!("slot checked above"),
                };
                for r in rights {
                    let v = match self.eval(r, env) {
                        Ok(v) => v,
                        Err(flow) => {
                            // Put the accumulated string back before unwinding so
                            // the environment stays consistent.
                            if let Some((slot, _)) = env.slot_mut(name) {
                                *slot = Value::Str(Rc::new(acc));
                            }
                            return Err(flow);
                        }
                    };
                    match v {
                        Value::Str(b) => acc.push_str(b.as_str()),
                        other => {
                            // The same error the general `<>` evaluation reports,
                            // with the left side accumulated so far.
                            let a = Value::Str(Rc::new(acc));
                            return err(format!(
                                "`<>` expects two Strings, got `{a}` and `{other}`"
                            ));
                        }
                    }
                }
                let Some((slot, true)) = env.slot_mut(name) else { unreachable!() };
                *slot = Value::Str(Rc::new(acc));
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Evaluate a function body with direct self-tail positions exposed to the
    /// function boundary. This is independent from the compiled WIR loop so the
    /// interpreter remains a genuine parity oracle.
    fn eval_function_block(
        &mut self,
        block: &Block,
        function: &Function,
        env: &mut Env,
    ) -> Result<Value, Flow> {
        let needs_scope = block
            .stmts
            .iter()
            .any(|s| matches!(s, Stmt::Let { .. } | Stmt::LetPattern { .. }));
        if needs_scope {
            env.push();
        }
        let last = block.stmts.len().saturating_sub(1);
        let mut result = Value::Unit;
        for (index, stmt) in block.stmts.iter().enumerate() {
            if let Some(line) = block.lines.get(index) {
                self.cur_line = *line;
            }
            let step = match stmt {
                Stmt::Let { name, mutable, value, .. } => {
                    let value = self.eval(value, env)?;
                    let name = self.intern(name);
                    env.define(name, value, *mutable);
                    Ok(Value::Unit)
                }
                Stmt::Assign { name, value } => {
                    if !self.try_inplace_assign(name, value, env)?
                        && !self.try_desugared_place_assign(name, value, env)?
                    {
                        let value = self.eval(value, env)?;
                        match env.assign(name, value) {
                            Assign::Done => {}
                            Assign::Immutable => {
                                return err(format!(
                                    "cannot assign to `{name}`: it is immutable (declared with `let`)"
                                ));
                            }
                            Assign::Unbound => {
                                return err(format!("cannot assign to unbound variable `{name}`"));
                            }
                        }
                    }
                    Ok(Value::Unit)
                }
                Stmt::LetPattern { pattern, value } => {
                    let value = self.eval(value, env)?;
                    if !match_pattern(pattern, &value, env) {
                        return err(format!(
                            "irrefutable `let` pattern did not match the value `{value}`"
                        ));
                    }
                    Ok(Value::Unit)
                }
                Stmt::Return(value) => {
                    let value = match value {
                        Some(expr) => self.eval_tail_expr(expr, function, env)?,
                        None => Value::Unit,
                    };
                    Err(Flow::Return(value))
                }
                Stmt::Break => Err(Flow::Break),
                Stmt::Continue => Err(Flow::Continue),
                Stmt::Expr(expr) | Stmt::Yield(expr) if index == last => {
                    self.eval_tail_expr(expr, function, env)
                }
                Stmt::Expr(expr) | Stmt::Yield(expr) => self.eval(expr, env),
            };
            match step {
                Ok(value) => result = value,
                Err(flow) => {
                    if needs_scope {
                        env.pop();
                    }
                    return Err(flow);
                }
            }
        }
        if needs_scope {
            env.pop();
        }
        Ok(result)
    }

    fn eval_tail_expr(
        &mut self,
        expr: &Expr,
        function: &Function,
        env: &mut Env,
    ) -> Result<Value, Flow> {
        let source_has_no_var = function
            .params
            .iter()
            .all(|param| param.convention != Convention::Var);
        let tail_target = match expr {
            Expr::Call { name, args }
                if (self.tail_dynamic_chain
                        || self
                            .proper_tail_edges
                            .get(&function.name)
                            .is_some_and(|targets| targets.contains(name)))
                    && !matches!(env.get(name), Some(Value::Closure { .. })) =>
            {
                self.functions.get(name).filter(|target| {
                    target.params.len() == args.len()
                        && direct_tail_envelope_is_forwarded(function, target, args)
                }).cloned()
            }
            _ => None,
        };
        let closure_tail = source_has_no_var
            && match expr {
                Expr::Apply { .. } => true,
                Expr::Call { name, .. } => {
                    matches!(env.get(name), Some(Value::Closure { .. }))
                }
                _ => false,
            };
        let handled_here = tail_target.is_some()
            || closure_tail
            || matches!(
                expr,
                Expr::If { .. }
                    | Expr::Match { .. }
                    | Expr::Block(_)
                    | Expr::Binary { op: BinOp::Coalesce, .. }
            );
        if handled_here {
            self.steps += 1;
            if self.steps > self.step_limit {
                return err("evaluation step budget exceeded (possible infinite loop)");
            }
        }
        match expr {
            Expr::Call { name, args } if tail_target.is_some() => {
                self.note_assert_crossing(name);
                let target = tail_target.expect("guarded tail-call target");
                let (values, places) = self.eval_call_args(args, &target.params, env)?;
                debug_assert!(
                    target.params.iter().zip(&places).all(|(param, place)| {
                        (param.convention == Convention::Var) == place.is_some()
                    })
                );
                // Builtin-over-user precedence: a name that is a builtin resolves
                // to it even here. Skip both probes for a plain user tail call
                // (the common case) — see `is_interpreter_builtin`.
                if is_interpreter_builtin(name) {
                    if let Some((value, var_values)) =
                        self.call_interpreter_special(name, &values)?
                    {
                        if !var_values.is_empty() {
                            return err(format!(
                                "internal: tail special `{name}` produced `var` write-backs without places"
                            ));
                        }
                        return Ok(value);
                    }
                    if let Some(value) = self.call_builtin(name, &values)? {
                        return Ok(value);
                    }
                }
                Err(Flow::TailCall {
                    callable: TailCallable::Function(target),
                    args: values,
                })
            }
            Expr::Apply { func, args } if source_has_no_var => {
                let closure = self.eval(func, env)?;
                let Value::Closure { function, .. } = &closure else {
                    return err("attempted to call a non-function value");
                };
                let function = function.clone();
                let (values, places) = self.eval_call_args(args, &function.params, env)?;
                if function.params.iter().all(|param| param.convention != Convention::Var) {
                    debug_assert!(places.iter().all(Option::is_none));
                    Err(Flow::TailCall {
                        callable: TailCallable::Closure(closure),
                        args: values,
                    })
                } else {
                    self.apply_closure_call(closure, values, places, env)
                }
            }
            Expr::Call { name, args } if source_has_no_var => {
                let Some(closure) = env.get(name).cloned() else {
                    return self.eval(expr, env);
                };
                let Value::Closure { function, .. } = &closure else {
                    return self.eval(expr, env);
                };
                let function = function.clone();
                let (values, places) = self.eval_call_args(args, &function.params, env)?;
                if function.params.iter().all(|param| param.convention != Convention::Var) {
                    debug_assert!(places.iter().all(Option::is_none));
                    Err(Flow::TailCall {
                        callable: TailCallable::Closure(closure),
                        args: values,
                    })
                } else {
                    self.apply_closure_call(closure, values, places, env)
                }
            }
            Expr::If { cond, then_block, else_block } => match self.eval(cond, env)? {
                Value::Bool(true) => self.eval_function_block(then_block, function, env),
                Value::Bool(false) => match else_block {
                    Some(block) => self.eval_function_block(block, function, env),
                    None => Ok(Value::Unit),
                },
                other => err(format!("`if` condition must be a Bool, got `{other}`")),
            },
            Expr::Match { scrutinee, arms } => {
                let value = self.eval(scrutinee, env)?;
                for arm in arms {
                    env.push();
                    if match_pattern(&arm.pattern, &value, env) {
                        let guard_ok = match &arm.guard {
                            Some(guard) => matches!(self.eval(guard, env)?, Value::Bool(true)),
                            None => true,
                        };
                        if guard_ok {
                            let result = self.eval_tail_expr(&arm.body, function, env);
                            env.pop();
                            return result;
                        }
                    }
                    env.pop();
                }
                err(format!("no match arm for value `{value}`"))
            }
            Expr::Block(block) => self.eval_function_block(block, function, env),
            Expr::Binary { op: BinOp::Coalesce, lhs, rhs } => match self.eval(lhs, env)? {
                Value::Ctor { name, mut fields }
                    if (&*name == "Some" || &*name == "Ok") && fields.len() == 1 =>
                {
                    Ok(Rc::make_mut(&mut fields).remove(0))
                }
                Value::Ctor { name, .. } if &*name == "None" || &*name == "Err" => {
                    self.eval_tail_expr(rhs, function, env)
                }
                other => err(format!(
                    "`??` expects an Option or Result on the left, got `{other}`"
                )),
            },
            _ => self.eval(expr, env),
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
            .any(|s| matches!(s, Stmt::Let { .. } | Stmt::LetPattern { .. }));
        if needs_scope {
            env.push();
        }
        let mut result = Value::Unit;
        for (i, stmt) in block.stmts.iter().enumerate() {
            if let Some(line) = block.lines.get(i) {
                self.cur_line = *line;
            }
            match stmt {
                Stmt::Let { name, ty: _, mutable, value } => {
                    let v = self.eval(value, env)?;
                    let name = self.intern(name);
                    env.define(name, v, *mutable);
                    result = Value::Unit;
                }
                Stmt::Assign { name, value } => {
                    if !self.try_inplace_assign(name, value, env)?
                        && !self.try_desugared_place_assign(name, value, env)?
                    {
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
                    result = Value::Unit;
                }
                Stmt::LetPattern { pattern, value } => {
                    let v = self.eval(value, env)?;
                    // The pattern is irrefutable (the refutability checker rejects a
                    // refutable pattern in `let` position at check time), so
                    // `match_pattern` always succeeds and binds every name. It handles
                    // tuples of any nesting, single-variant ctor/record patterns, and
                    // wildcards uniformly — one grammar, shared with `match` (parity).
                    if !match_pattern(pattern, &v, env) {
                        // Unreachable for a checked program; a loud guard beats a
                        // silent mis-bind if an unchecked path ever reaches here.
                        env.pop();
                        return err(format!(
                            "irrefutable `let` pattern did not match the value `{v}`"
                        ));
                    }
                    result = Value::Unit;
                }
                Stmt::Return(opt) => {
                    let v = match opt {
                        Some(e) => match self.tail_function.clone() {
                            Some(function) => self.eval_tail_expr(e, &function, env)?,
                            None => self.eval(e, env)?,
                        },
                        None => Value::Unit,
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

    /// One shared `Rc<str>` per distinct binding name: `let`s and loop
    /// variables inside hot loops re-bind the same name every iteration, and
    /// the interner turns each re-binding into a pointer clone instead of a
    /// fresh `String` allocation. Bounded by the program's distinct names.
    fn intern(&mut self, name: &str) -> Rc<str> {
        if let Some(interned) = self.interned_names.get(name) {
            return interned.clone();
        }
        let interned: Rc<str> = Rc::from(name);
        self.interned_names.insert(name.to_string(), interned.clone());
        interned
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
            // (RFC-0056) Resolved to a positional `Call`/`Block` by
            // `witchy_syntax::keyword_args` at the link layer, before evaluation.
            Expr::LabeledCall { name, .. } => {
                unreachable!("unresolved labeled call `{name}` reached the interpreter")
            }
            Expr::Int(n) | Expr::Duration(n) => Ok(Value::Int(*n)),
            Expr::Float(x) => Ok(Value::Float(*x)),
            Expr::Str(s) => Ok(Value::str(s.as_str())),
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
                Ok(Value::list(vals))
            }
            Expr::Tuple(items) => {
                let vals = items
                    .iter()
                    .map(|e| self.eval(e, env))
                    .collect::<Result<_, _>>()?;
                Ok(Value::tuple(vals))
            }
            Expr::Var(name) => match env.get(name) {
                Some(v) => Ok(v.clone()),
                None => match self.functions.get(name).cloned() {
                    // A bare top-level function name is a first-class function
                    // value: wrap it as a closure over an empty environment
                    // (top-level functions are closed; nested calls resolve
                    // through the global function table at apply time). The
                    // wrap is memoized per name — the function table is
                    // immutable for the run, and re-evaluating a function name
                    // (every higher-order loop transition does) must not
                    // re-copy its AST.
                    Some(func) => {
                        if let Some(value) = self.fn_values.get(name) {
                            return Ok(value.clone());
                        }
                        let value = Value::Closure {
                            function: closure_function(
                                name.clone(),
                                func.params.clone(),
                                func.body.clone(),
                            ),
                            env: Box::new(Env::new()),
                        };
                        self.fn_values.insert(name.clone(), value.clone());
                        Ok(value)
                    }
                    None => err(format!("unbound variable `{name}`")),
                },
            },
            Expr::Call { name, args } => self.eval_call(name, args, env),
            Expr::Apply { func, args } => {
                let clo = self.eval(func, env)?;
                let Value::Closure { function, .. } = &clo else {
                    return err("attempted to call a non-function value");
                };
                let function = function.clone();
                let (argvals, places) = self.eval_call_args(args, &function.params, env)?;
                self.apply_closure_call(clo, argvals, places, env)
            }
            Expr::Ctor { name, args } => {
                if name == "Nil" && args.is_empty() {
                    return Ok(Value::Unit);
                }
                let fields = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<Result<_, _>>()?;
                let name = self.intern(name);
                Ok(Value::ctor(name, fields))
            }
            Expr::AnonCtor { tag, args } => {
                let fields = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<Result<_, _>>()?;
                let name = self.intern(&format!(".{tag}"));
                Ok(Value::ctor(name, fields))
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
                    function: closure_function(
                        self.cur_fn.clone(),
                        params.clone(),
                        body.clone(),
                    ),
                    env: Box::new(env.capture(&mentioned)),
                })
            }
            Expr::RecordUpdate { name: _, base, fields } => {
                let v = self.eval(base, env)?;
                let Value::Ctor { name, fields: mut values } = v else {
                    return err(format!("`update` requires a record value, got `{v}`"));
                };
                for (fname, vexpr) in fields {
                    let idx = self
                        .record_fields
                        .get(&*name)
                        .and_then(|names| names.iter().position(|n| n == fname));
                    let val = self.eval(vexpr, env)?;
                    match idx.filter(|i| *i < values.len()) {
                        Some(i) => Rc::make_mut(&mut values)[i] = val,
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
                            .get(&*name)
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
                        if (&*name == "Ok" || &*name == "Some") && fields.len() == 1 =>
                    {
                        Ok(Rc::make_mut(&mut fields).remove(0))
                    }
                    Value::Ctor { name, fields } if &*name == "Err" || &*name == "None" => {
                        // Short-circuit: return the Err/None from the enclosing function.
                        Err(Flow::Return(Value::Ctor { name, fields }))
                    }
                    other => err(format!("`?` expects a Result/Option value, got `{other}`")),
                }
            }
            // `e as T` narrows a capability's rights — purely type-level, so at
            // runtime it is the identity on the underlying value.
            Expr::As { expr, .. } => self.eval(expr, env),
            Expr::ExistentialPack { expr, witness, .. } => Ok(Value::Existential {
                payload: Box::new(self.eval(expr, env)?),
                witness: *witness,
            }),
            Expr::ExistentialUpcast { expr, ty } => {
                let Value::Existential { payload, witness } = self.eval(expr, env)? else {
                    return err("internal: existential upcast received a non-existential value");
                };
                let target = self.witnesses.upcast(witness, ty).ok_or_else(|| {
                    Flow::from(RuntimeError {
                        message: format!(
                            "internal: no authenticated existential upcast from witness {witness} to {}",
                            witchy_syntax::format::type_str(ty)
                        ),
                    })
                })?;
                Ok(Value::Existential {
                    payload,
                    witness: target,
                })
            }
            Expr::ExistentialCall {
                receiver,
                args,
                owner_trait,
                method,
                slot,
                ..
            } => self.eval_existential_call(receiver, args, owner_trait, method, *slot, env),
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
                other => err(format!("`||` expects Bool operands, got `{other}`")),
            },
            // `a ?? b` (RFC-0048): unwrap `Some`/`Ok` to the payload, or evaluate
            // the fallback on `None`/`Err` (lazily; the error value is discarded).
            Expr::Binary { op: BinOp::Coalesce, lhs, rhs } => match self.eval(lhs, env)? {
                Value::Ctor { name, mut fields }
                    if (&*name == "Some" || &*name == "Ok") && fields.len() == 1 =>
                {
                    Ok(Rc::make_mut(&mut fields).remove(0))
                }
                Value::Ctor { name, .. } if &*name == "None" || &*name == "Err" => {
                    self.eval(rhs, env)
                }
                other => err(format!(
                    "`??` expects an Option or Result on the left, got `{other}`"
                )),
            },
            Expr::Binary { op, lhs, rhs } => {
                let l = self.eval(lhs, env)?;
                let r = self.eval(rhs, env)?;
                // The parser-level checker rejects existential equality. Preserve the
                // same opacity for unchecked oracle input, including containers that
                // hide an existential, before taking the normal fast equality path.
                if matches!(op, BinOp::Eq | BinOp::NotEq)
                    && (contains_existential(&l) || contains_existential(&r))
                {
                    return err("existential values do not support equality");
                }
                // (RFC-0047) `==`/`!=` desugar through PartialEq at every depth
                // only when the program declares a custom implementation.
                if matches!(op, BinOp::Eq | BinOp::NotEq) && !self.custom_eq_types.is_empty() {
                    let eq = self.values_equal(&l, &r)?;
                    return Ok(Value::Bool(if *op == BinOp::Eq { eq } else { !eq }));
                }
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
                    None => Ok(Value::Unit),
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
                    let var_name = self.intern(var);
                    while if *inclusive { i <= end } else { i < end } {
                        env.push();
                        env.define(var_name.clone(), Value::Int(i), false);
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
                    return Ok(Value::Unit);
                }
                let items = match self.eval(iter, env)? {
                    Value::List(items) => items,
                    other => return err(format!("`for` expects a List, got `{other}`")),
                };
                let var_name = self.intern(var);
                for item in items.iter().cloned() {
                    env.push();
                    env.define(var_name.clone(), item, false);
                    let r = self.eval_block(body, env);
                    env.pop();
                    match r {
                        Ok(_) | Err(Flow::Continue) => {}
                        Err(Flow::Break) => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(Value::Unit)
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
                Ok(Value::Unit)
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


#[cfg(test)]
#[path = "interpreter_tests.rs"]
mod tests;
