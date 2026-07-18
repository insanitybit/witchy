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
use foldhash::{HashMap as FxHashMap, HashMapExt as _, HashSet as FxHashSet, HashSetExt as _};
use std::fmt;
use std::io::{BufReader, Read, Write};
use std::net::TcpListener;

use witchy_runtime::net::Stream;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use witchy_syntax::ast::*;
use witchy_syntax::diag::DiagTemplate;
use witchy_syntax::intrinsics;
use witchy_syntax::origin::SyntaxCategory;
use witchy_syntax::parser::parse_module;

#[derive(Debug, Clone, PartialEq)]
pub enum DirValue {
    Fs(PathBuf),
    Mock {
        root: String,
        files: Rc<BTreeMap<String, String>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum FileValue {
    Fs(PathBuf),
    Mock {
        path: String,
        files: Rc<BTreeMap<String, String>>,
    },
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
    /// An unforgeable capability to a directory subtree (cap-std `Dir` style).
    /// Carries the host path it is rooted at; can only be obtained from the root
    /// grant or by attenuation (`subdir`).
    // A confined directory: its root path + an entry policy (RFC-0011; `""` =
    // unrestricted). `dir.only(confine.ext(...))` narrows the policy; reads/writes
    // through the Dir are admitted only when the policy admits the entry name.
    Dir(DirValue, String),
    /// A file capability (RFC-0012): authority to one file (the leaf of the
    /// Dir/File hierarchy). Carries the confined host path; obtained by navigating
    /// a `Dir` (`dir.open`/`dir.create`) or as a `main` grant. Rights are checked
    /// at compile time, so the value carries only the path.
    File(FileValue),
    /// A network capability: an allow-list of permitted `host:port` destinations
    /// (wasi:sockets / cap-std-net style). Attenuable via `restrict`.
    Net(Vec<String>),
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


const OWNED_ITEM_SYNTAX_CTOR: &str = "@owned_item_syntax";

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ComptimeItemEmission {
    Source(String),
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
    Source(String),
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
    Out(PathBuf),
    /// Read project files confined to one of these directory subtrees. A relative
    /// path resolves against the first granted root that contains it.
    Read(Vec<PathBuf>),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Console,
    Clock,
    Rand,
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
            Value::Secret(_, _) => write!(f, "<secret>"),
            Value::SecretStore(_) => write!(f, "<secret store>"),
            Value::Socket(id) => write!(f, "<socket #{id}>"),
            Value::Listener(id) => write!(f, "<listener #{id}>"),
            Value::Build(_) => write!(f, "<build capability>"),
            Value::Closure { function, .. } => {
                write!(f, "<function/{}>", function.params.len())
            }
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

#[derive(Clone)]
enum PlaceProjection {
    Field(String),
    Index(Value),
}

#[derive(Clone)]
struct CapturedPlace {
    root: String,
    projections: Vec<PlaceProjection>,
}

enum AssignmentProjection<'a> {
    Field(&'a str),
    Index { access: &'static str, expression: &'a Expr },
}

struct AssignmentPlan<'a> {
    projections: Vec<AssignmentProjection<'a>>,
    replacement: &'a Expr,
}

// Surface place assignments are desugared before either backend sees them:
// `root[i].field = value` becomes a root assignment built from private
// set-at/record-update expressions. Recover only that structural spine so the
// interpreter can mirror compiled lowering: capture coordinates, evaluate the
// replacement, then apply it to the current root.
fn expression_reads_assignment_place(
    expression: &Expr,
    root: &str,
    projections: &[AssignmentProjection<'_>],
) -> bool {
    let Some((projection, prefix)) = projections.split_last() else {
        return matches!(expression, Expr::Var(name) if name == root);
    };
    match (projection, expression) {
        (AssignmentProjection::Field(expected), Expr::Field { base, field }) => {
            field == expected
                && expression_reads_assignment_place(base, root, prefix)
        }
        (
            AssignmentProjection::Index { access, expression: expected },
            Expr::Call { name, args },
        ) => {
            name == access
                && args.len() == 2
                && args[1] == **expected
                && expression_reads_assignment_place(&args[0], root, prefix)
        }
        _ => false,
    }
}

fn desugared_assignment_plan<'a>(
    root: &str,
    expression: &'a Expr,
) -> Option<AssignmentPlan<'a>> {
    fn decode<'a>(
        root: &str,
        expression: &'a Expr,
        projections: &mut Vec<AssignmentProjection<'a>>,
    ) -> Option<&'a Expr> {
        match expression {
            Expr::Call { name, args }
                if args.len() == 3
                    && matches!(
                        name.as_str(),
                        intrinsics::LIST_SET_AT | intrinsics::DICT_INSERT
                    )
                    && expression_reads_assignment_place(
                        &args[0],
                        root,
                        projections,
                    ) =>
            {
                let access = if name == intrinsics::LIST_SET_AT {
                    intrinsics::LIST_AT
                } else {
                    intrinsics::DICT_AT
                };
                projections.push(AssignmentProjection::Index {
                    access,
                    expression: &args[1],
                });
                decode(root, &args[2], projections).or(Some(&args[2]))
            }
            Expr::RecordUpdate { name: None, base, fields }
                if fields.len() == 1
                    && expression_reads_assignment_place(
                        base,
                        root,
                        projections,
                    ) =>
            {
                let (field, value) = &fields[0];
                projections.push(AssignmentProjection::Field(field));
                decode(root, value, projections).or(Some(value))
            }
            _ => None,
        }
    }

    let mut projections = Vec::new();
    let replacement = decode(root, expression, &mut projections)?;
    Some(AssignmentPlan { projections, replacement })
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
/// `int_to_duration`, `secretstore.get`/`require`, `vm.par_map`). The test
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
                | "secretstore.get"
                | "secretstore.require"
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

/// Decode one `meta.ExprSyntax` value for compiler-owned structural builders.
/// A compatibility payload is parsed in isolation; an owned payload transfers
/// its AST directly so definition-site and call-site markers cannot be erased
/// by an intermediate source projection.
fn compiler_expr_syntax_value(
    value: &Value,
    compiler_expr_syntax: &HashMap<String, Expr>,
) -> Result<Expr, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.expr_call expected ExprSyntax values");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerExprSyntax", [Value::Str(handle), Value::Str(_source)]) => compiler_expr_syntax
            .get(handle.as_str())
            .cloned()
            .ok_or_else(|| RuntimeError {
                message: "CompilerExprSyntax carried an invalid syntax handle".into(),
            }),
        ("ExprSyntax", [Value::Str(source)]) => {
            witchy_syntax::syntax_holes::parse_expr_payload(source).map_err(|message| RuntimeError { message })
        }
        ("CompilerExprSyntax", _) => err("CompilerExprSyntax carried an invalid payload"),
        ("ExprSyntax", _) => err("ExprSyntax carried an invalid source payload"),
        (other, _) => err(format!("meta.expr_call expected ExprSyntax, got `{other}`")),
    }
}

fn compiler_type_syntax_value(
    value: &Value,
    compiler_type_syntax: &HashMap<String, Type>,
) -> Result<Type, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.stmt_let expected TypeSyntax");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerTypeSyntax", [Value::Str(handle), Value::Str(_source)]) => {
            compiler_type_syntax.get(handle.as_str()).cloned().ok_or_else(|| RuntimeError {
                message: "CompilerTypeSyntax carried an invalid syntax handle".into(),
            })
        }
        ("TypeSyntax", [Value::Str(source)]) => {
            witchy_syntax::syntax_holes::parse_type_payload(source)
                .map_err(|message| RuntimeError { message })
        }
        ("CompilerTypeSyntax", _) => err("CompilerTypeSyntax carried an invalid payload"),
        ("TypeSyntax", _) => err("TypeSyntax carried an invalid source payload"),
        (other, _) => err(format!("meta.stmt_let expected TypeSyntax, got `{other}`")),
    }
}

fn compiler_optional_type_syntax_value(
    value: &Value,
    compiler_type_syntax: &HashMap<String, Type>,
) -> Result<Option<Type>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    match value {
        Value::Ctor { name, fields } if tail(name) == "Some" && fields.len() == 1 => {
            Ok(Some(compiler_type_syntax_value(
                &fields[0],
                compiler_type_syntax,
            )?))
        }
        Value::Ctor { name, fields } if tail(name) == "None" && fields.is_empty() => Ok(None),
        _ => err("meta.stmt_let expected Option(TypeSyntax) annotation"),
    }
}

fn compiler_stmt_syntax_value(
    value: &Value,
    compiler_stmt_syntax: &HashMap<String, Stmt>,
) -> Result<Stmt, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.block expected StmtSyntax values");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerStmtSyntax", [Value::Str(handle), Value::Str(_source)]) => {
            compiler_stmt_syntax.get(handle.as_str()).cloned().ok_or_else(|| RuntimeError {
                message: "CompilerStmtSyntax carried an invalid syntax handle".into(),
            })
        }
        ("StmtSyntax", [Value::Str(source)]) => {
            let body = source.replace('\n', "\n    ");
            let module = parse_module(&format!(
                "fn __witchy_meta_stmt_payload():\n    {body}\n"
            ))
            .map_err(|error| RuntimeError {
                message: format!("invalid StmtSyntax payload: {error}"),
            })?;
            let [Item::Function(function)] = module.items.as_slice() else {
                return err("invalid StmtSyntax payload: expected one function wrapper");
            };
            let [stmt] = function.body.stmts.as_slice() else {
                return err("invalid StmtSyntax payload: expected exactly one statement");
            };
            Ok(stmt.clone())
        }
        ("CompilerStmtSyntax", _) => err("CompilerStmtSyntax carried an invalid payload"),
        ("StmtSyntax", _) => err("StmtSyntax carried an invalid source payload"),
        (other, _) => err(format!("meta.block expected StmtSyntax, got `{other}`")),
    }
}

fn compiler_optional_expr_syntax_value(
    value: &Value,
    compiler_expr_syntax: &HashMap<String, Expr>,
) -> Result<Option<Expr>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    match value {
        Value::Ctor { name, fields } if tail(name) == "Some" && fields.len() == 1 => {
            Ok(Some(compiler_expr_syntax_value(
                &fields[0],
                compiler_expr_syntax,
            )?))
        }
        Value::Ctor { name, fields } if tail(name) == "None" && fields.is_empty() => Ok(None),
        _ => err("meta.block expected Option(ExprSyntax) tail"),
    }
}

fn compiler_ident_name(value: &Value, operation: &str) -> Result<String, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    match value {
        Value::Ctor { name, fields }
            if matches!(tail(name), "Ident" | "CallSiteIdent") && matches!(fields.as_slice(), [Value::Str(_)]) =>
        {
            let Value::Str(name) = &fields[0] else { unreachable!() };
            Ok(name.to_string())
        }
        _ => err(format!("{operation} expected an Ident field name")),
    }
}

fn compiler_binding_ident_name(value: &Value, operation: &str) -> Result<String, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    match value {
        Value::Ctor { name, fields }
            if tail(name) == "Ident" && matches!(fields.as_slice(), [Value::Str(_)]) =>
        {
            let Value::Str(name) = &fields[0] else { unreachable!() };
            Ok(name.to_string())
        }
        Value::Ctor { name, .. } if tail(name) == "CallSiteIdent" => err(format!(
            "{operation} requires a binding identifier; meta.call_site is reference-only"
        )),
        _ => err(format!("{operation} expected an Ident binding name")),
    }
}

fn compiler_pattern_syntax_value(
    value: &Value,
    compiler_pattern_syntax: &HashMap<String, Pattern>,
) -> Result<Pattern, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.match_arm expected PatternSyntax");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerPatternSyntax", [Value::Str(handle), Value::Str(_source)]) => {
            compiler_pattern_syntax.get(handle.as_str()).cloned().ok_or_else(|| RuntimeError {
                message: "CompilerPatternSyntax carried an invalid syntax handle".into(),
            })
        }
        ("PatternSyntax", [Value::Str(source)]) => {
            witchy_syntax::syntax_holes::parse_pattern_payload(source)
                .map_err(|message| RuntimeError { message })
        }
        ("CompilerPatternSyntax", _) => err("CompilerPatternSyntax carried an invalid payload"),
        ("PatternSyntax", _) => err("PatternSyntax carried an invalid source payload"),
        (other, _) => err(format!("meta.match_arm expected PatternSyntax, got `{other}`")),
    }
}

fn compiler_match_arms(
    value: &Value,
    compiler_match_arm_syntax: &HashMap<String, MatchArm>,
) -> Result<Vec<MatchArm>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::List(arms) = value else {
        return err("meta.expr_match expected List(MatchArmSyntax) arms");
    };
    arms.iter()
        .map(|arm| match arm {
            Value::Ctor { name, fields }
                if tail(name) == "CompilerMatchArmSyntax"
                    && matches!(fields.as_slice(), [Value::Str(_), Value::Str(_)]) =>
            {
                let Value::Str(handle) = &fields[0] else { unreachable!() };
                compiler_match_arm_syntax.get(handle.as_str()).cloned().ok_or_else(|| {
                    RuntimeError {
                        message: "CompilerMatchArmSyntax carried an invalid syntax handle".into(),
                    }
                })
            }
            Value::Ctor { name, fields }
                if tail(name) == "MatchArmSyntax" && matches!(fields.as_slice(), [Value::Str(_)]) =>
            {
                let Value::Str(source) = &fields[0] else { unreachable!() };
                let source = source.replace('\n', "\n    ");
                let expr = witchy_syntax::syntax_holes::parse_expr_payload(&format!(
                    "match 0:\n    {source}"
                ))
                .map_err(|message| RuntimeError { message })?;
                let Expr::Match { arms, .. } = expr else {
                    return err("meta.expr_match failed to parse a compatibility arm");
                };
                let [arm] = arms.as_slice() else {
                    return err("meta.expr_match expected exactly one compatibility arm");
                };
                Ok(arm.clone())
            }
            _ => err("meta.expr_match expected MatchArmSyntax arms"),
        })
        .collect()
}

fn compiler_params(
    value: &Value,
    compiler_param_syntax: &HashMap<String, Param>,
) -> Result<Vec<Param>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::List(params) = value else {
        return err("meta.function_block expected List(ParamSyntax)");
    };
    params
        .iter()
        .map(|param| match param {
            Value::Ctor { name, fields }
                if tail(name) == "CompilerParamSyntax"
                    && matches!(fields.as_slice(), [Value::Str(_), Value::Str(_)]) =>
            {
                let Value::Str(handle) = &fields[0] else { unreachable!() };
                compiler_param_syntax.get(handle.as_str()).cloned().ok_or_else(|| {
                    RuntimeError {
                        message: "CompilerParamSyntax carried an invalid syntax handle".into(),
                    }
                })
            }
            Value::Ctor { name, fields }
                if tail(name) == "ParamSyntax" && matches!(fields.as_slice(), [Value::Str(_)]) =>
            {
                let Value::Str(source) = &fields[0] else { unreachable!() };
                let module = parse_module(&format!(
                    "fn __witchy_meta_param_payload({source}):\n    ()\n"
                ))
                .map_err(|error| RuntimeError {
                    message: format!("invalid ParamSyntax payload: {error}"),
                })?;
                let [Item::Function(function)] = module.items.as_slice() else {
                    return err("invalid ParamSyntax payload: expected one function wrapper");
                };
                let [param] = function.params.as_slice() else {
                    return err("invalid ParamSyntax payload: expected exactly one parameter");
                };
                Ok(param.clone())
            }
            _ => err("meta.function_block expected ParamSyntax values"),
        })
        .collect()
}

fn compiler_block_syntax_value(
    value: &Value,
    compiler_block_syntax: &HashMap<String, Block>,
) -> Result<Block, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let Value::Ctor { name, fields } = value else {
        return err("meta.function_block expected a BlockSyntax body");
    };
    match (tail(name), fields.as_slice()) {
        ("CompilerBlockSyntax", [Value::Str(handle), Value::Str(_source)]) => {
            compiler_block_syntax.get(handle.as_str()).cloned().ok_or_else(|| RuntimeError {
                message: "CompilerBlockSyntax carried an invalid syntax handle".into(),
            })
        }
        ("BlockSyntax", [Value::Str(source)]) => {
            let body = source.replace('\n', "\n    ");
            let module = parse_module(&format!(
                "fn __witchy_meta_block_payload():\n    {body}\n"
            ))
            .map_err(|error| RuntimeError {
                message: format!("invalid BlockSyntax payload: {error}"),
            })?;
            let [Item::Function(function)] = module.items.as_slice() else {
                return err("invalid BlockSyntax payload: expected one function wrapper");
            };
            Ok(function.body.clone())
        }
        ("CompilerBlockSyntax", _) => err("CompilerBlockSyntax carried an invalid payload"),
        ("BlockSyntax", _) => err("BlockSyntax carried an invalid source payload"),
        (other, _) => err(format!(
            "meta.function_block expected BlockSyntax, got `{other}`"
        )),
    }
}

fn compiler_item_holes(
    values: &[Value],
    compiler_expr_syntax: &HashMap<String, Expr>,
    compiler_type_syntax: &HashMap<String, Type>,
    compiler_pattern_syntax: &HashMap<String, Pattern>,
) -> Result<Vec<witchy_syntax::syntax_holes::ItemSyntaxHole>, RuntimeError> {
    use witchy_syntax::syntax_holes::{
        ItemSyntaxHole, parse_expr_payload, parse_pattern_payload, parse_type_payload,
    };

    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    fn source<'a>(value: &'a Value, expected: &str) -> Result<&'a str, RuntimeError> {
        let Value::Ctor { name, fields } = value else {
            return err(format!("{expected} hole carried a non-syntax value"));
        };
        if tail(name) != expected {
            return err(format!("{expected} hole carried `{}`", tail(name)));
        }
        let [Value::Str(source)] = fields.as_slice() else {
            return err(format!("{expected} carried an invalid source payload"));
        };
        Ok(source)
    }

    values
        .iter()
        .map(|value| {
            let Value::Ctor { name, fields } = value else {
                return err("compiler-owned item hole was not a meta.SyntaxHole");
            };
            let [syntax] = fields.as_slice() else {
                return err("compiler-owned item hole carried an invalid payload");
            };
            match tail(name) {
                "ExprHole" => match syntax {
                    Value::Ctor { name, fields } if tail(name) == "CompilerExprSyntax" => {
                        let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                            return err("CompilerExprSyntax carried an invalid payload");
                        };
                        compiler_expr_syntax
                            .get(handle.as_str())
                            .cloned()
                            .map(ItemSyntaxHole::Expr)
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned expression referenced an invalid syntax handle"
                                    .into(),
                            })
                    }
                    _ => parse_expr_payload(source(syntax, "ExprSyntax")?)
                        .map(ItemSyntaxHole::Expr)
                        .map_err(|message| RuntimeError { message }),
                },
                "TypeHole" => match syntax {
                    Value::Ctor { name, fields } if tail(name) == "CompilerTypeSyntax" => {
                        let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                            return err("CompilerTypeSyntax carried an invalid payload");
                        };
                        compiler_type_syntax
                            .get(handle.as_str())
                            .cloned()
                            .map(ItemSyntaxHole::Type)
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned type referenced an invalid syntax handle"
                                    .into(),
                            })
                    }
                    _ => parse_type_payload(source(syntax, "TypeSyntax")?)
                        .map(ItemSyntaxHole::Type)
                        .map_err(|message| RuntimeError { message }),
                },
                "PatternHole" => match syntax {
                    Value::Ctor { name, fields } if tail(name) == "CompilerPatternSyntax" => {
                        let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                            return err("CompilerPatternSyntax carried an invalid payload");
                        };
                        compiler_pattern_syntax
                            .get(handle.as_str())
                            .cloned()
                            .map(ItemSyntaxHole::Pattern)
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned pattern referenced an invalid syntax handle"
                                    .into(),
                            })
                    }
                    _ => parse_pattern_payload(source(syntax, "PatternSyntax")?)
                        .map(ItemSyntaxHole::Pattern)
                        .map_err(|message| RuntimeError { message }),
                },
                other => err(format!("compiler-owned item hole had unknown category `{other}`")),
            }
        })
        .collect()
}

fn compiler_item_hole_origins(
    values: &[Value],
    expr_origins: &HashMap<String, ComptimeSyntaxOrigin>,
    type_origins: &HashMap<String, ComptimeSyntaxOrigin>,
    pattern_origins: &HashMap<String, ComptimeSyntaxOrigin>,
    invocation_line: u32,
) -> Vec<ComptimeHoleOrigin> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    values
        .iter()
        .flat_map(|value| {
            let Value::Ctor { name, fields } = value else {
                return Vec::new();
            };
            let [syntax] = fields.as_slice() else {
                return Vec::new();
            };
            let (category, compiler_ctor, origins) = match tail(name) {
                "ExprHole" => (SyntaxCategory::Expr, "CompilerExprSyntax", expr_origins),
                "TypeHole" => (SyntaxCategory::Type, "CompilerTypeSyntax", type_origins),
                "PatternHole" => (
                    SyntaxCategory::Pattern,
                    "CompilerPatternSyntax",
                    pattern_origins,
                ),
                _ => return Vec::new(),
            };
            syntax_hole_origin(category, syntax, compiler_ctor, origins, invocation_line)
        })
        .collect()
}

fn compiler_direct_hole_origins(
    values: &[Value],
    category: SyntaxCategory,
    compiler_ctor: &str,
    origins: &HashMap<String, ComptimeSyntaxOrigin>,
    invocation_line: u32,
) -> Vec<ComptimeHoleOrigin> {
    values
        .iter()
        .flat_map(|syntax| {
            syntax_hole_origin(category, syntax, compiler_ctor, origins, invocation_line)
        })
        .collect()
}

fn syntax_hole_origin(
    category: SyntaxCategory,
    syntax: &Value,
    compiler_ctor: &str,
    origins: &HashMap<String, ComptimeSyntaxOrigin>,
    invocation_line: u32,
) -> Vec<ComptimeHoleOrigin> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    let syntax_origin = match syntax {
        Value::Ctor { name, fields } if tail(name) == compiler_ctor => {
            let [Value::Str(handle), ..] = fields.as_slice() else {
                return Vec::new();
            };
            origins.get(handle.as_str())
        }
        _ => None,
    };
    let mut ancestry = vec![ComptimeHoleOrigin {
        category,
        definition_line: syntax_origin.map_or(0, |origin| origin.definition_line),
        invocation_line,
    }];
    if let Some(origin) = syntax_origin {
        ancestry.extend(origin.hole_ancestry.iter().cloned());
    }
    ancestry
}

fn compiler_type_holes(
    values: &[Value],
    compiler_type_syntax: &HashMap<String, Type>,
) -> Result<Vec<Type>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    values
        .iter()
        .map(|value| {
            let Value::Ctor { name, fields } = value else {
                return err("compiler-owned type hole was not meta.TypeSyntax");
            };
            match tail(name) {
                "CompilerTypeSyntax" => {
                    let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                        return err("CompilerTypeSyntax carried an invalid payload");
                    };
                    compiler_type_syntax
                        .get(handle.as_str())
                        .cloned()
                        .ok_or_else(|| RuntimeError {
                            message: "compiler-owned type referenced an invalid syntax handle"
                                .into(),
                        })
                }
                "TypeSyntax" => {
                    let [Value::Str(source)] = fields.as_slice() else {
                        return err("TypeSyntax carried an invalid source payload");
                    };
                    witchy_syntax::syntax_holes::parse_type_payload(source)
                        .map_err(|message| RuntimeError { message })
                }
                other => err(format!(
                    "compiler-owned type hole carried `{other}`, expected TypeSyntax"
                )),
            }
        })
        .collect()
}

fn compiler_pattern_holes(
    values: &[Value],
    compiler_pattern_syntax: &HashMap<String, Pattern>,
) -> Result<Vec<Pattern>, RuntimeError> {
    fn tail(name: &str) -> &str {
        name.rsplit_once('.').map_or(name, |(_, tail)| tail)
    }

    values
        .iter()
        .map(|value| {
            let Value::Ctor { name, fields } = value else {
                return err("compiler-owned pattern hole was not meta.PatternSyntax");
            };
            match tail(name) {
                "CompilerPatternSyntax" => {
                    let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                        return err("CompilerPatternSyntax carried an invalid payload");
                    };
                    compiler_pattern_syntax
                        .get(handle.as_str())
                        .cloned()
                        .ok_or_else(|| RuntimeError {
                            message: "compiler-owned pattern referenced an invalid syntax handle"
                                .into(),
                        })
                }
                "PatternSyntax" => {
                    let [Value::Str(source)] = fields.as_slice() else {
                        return err("PatternSyntax carried an invalid source payload");
                    };
                    witchy_syntax::syntax_holes::parse_pattern_payload(source)
                        .map_err(|message| RuntimeError { message })
                }
                other => err(format!(
                    "compiler-owned pattern hole carried `{other}`, expected PatternSyntax"
                )),
            }
        })
        .collect()
}

fn mock_normalize(rel: &str) -> Result<String, RuntimeError> {
    let path = Path::new(rel);
    if path.is_absolute() {
        return err(format!("mock Dir path `{rel}` must be relative"));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return err(format!("mock Dir path `{rel}` is not valid UTF-8"));
                };
                parts.push(part.to_string());
            }
            std::path::Component::ParentDir => {
                return err(format!("mock Dir path `{rel}` may not contain `..`"));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return err(format!("mock Dir path `{rel}` must be relative"));
            }
        }
    }
    Ok(parts.join("/"))
}

fn mock_join(root: &str, rel: &str) -> Result<String, RuntimeError> {
    let rel = mock_normalize(rel)?;
    Ok(match (root.is_empty(), rel.is_empty()) {
        (_, true) => root.to_string(),
        (true, false) => rel,
        (false, false) => format!("{root}/{rel}"),
    })
}

fn mock_is_dir(files: &BTreeMap<String, String>, path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    let prefix = format!("{path}/");
    files.keys().any(|entry| entry.starts_with(&prefix))
}

fn mock_exists(files: &BTreeMap<String, String>, path: &str) -> bool {
    files.contains_key(path) || mock_is_dir(files, path)
}

fn mock_list(files: &BTreeMap<String, String>, root: &str) -> Result<Vec<String>, RuntimeError> {
    if !mock_is_dir(files, root) {
        return err(format!("list failed for mock Dir `{root}`: not a directory"));
    }
    let mut names = std::collections::BTreeSet::new();
    let prefix = if root.is_empty() {
        String::new()
    } else {
        format!("{root}/")
    };
    for path in files.keys() {
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let name = rest.split('/').next().unwrap_or(rest);
        names.insert(name.to_string());
    }
    Ok(names.into_iter().collect())
}

fn dir_child_value(dir: &DirValue, name: &str) -> Result<DirValue, RuntimeError> {
    match dir {
        DirValue::Fs(base) => Ok(DirValue::Fs(resolve(base, name)?)),
        DirValue::Mock { root, files } => {
            Ok(DirValue::Mock { root: mock_join(root, name)?, files: files.clone() })
        }
    }
}

fn dir_file_value(dir: &DirValue, rel: &str, write: bool) -> Result<FileValue, RuntimeError> {
    match dir {
        DirValue::Fs(base) => {
            let path = if write {
                resolve_write(base, rel)?
            } else {
                resolve(base, rel)?
            };
            Ok(FileValue::Fs(path))
        }
        DirValue::Mock { root, files } => {
            Ok(FileValue::Mock { path: mock_join(root, rel)?, files: files.clone() })
        }
    }
}

fn read_file_value(file: &FileValue) -> Result<String, RuntimeError> {
    match file {
        FileValue::Fs(path) => match std::fs::read_to_string(path) {
            Ok(contents) => Ok(contents),
            Err(e) => err(format!("read failed for `{}`: {e}", path.display())),
        },
        FileValue::Mock { path, files } => files
            .get(path)
            .cloned()
            .ok_or_else(|| RuntimeError {
                message: format!("read failed for mock Dir `{path}`: no such file"),
            }),
    }
}

fn write_file_value(file: &FileValue, contents: &str) -> Result<(), RuntimeError> {
    match file {
        FileValue::Fs(path) => match std::fs::write(path, contents) {
            Ok(()) => Ok(()),
            Err(e) => err(format!("write failed for `{}`: {e}", path.display())),
        },
        FileValue::Mock { path, .. } => err(format!(
            "write failed for mock Dir `{path}`: mock directories are read-only"
        )),
    }
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

/// Lexically scoped variable bindings. Functions are not closures: a call
/// starts a fresh `Env` so a function body sees only its parameters and the
/// global function table.
enum Assign {
    Done,
    Immutable,
    Unbound,
}

#[derive(Default, Debug)]
pub struct Env {
    /// A stack of scopes; each scope is a small list of bindings carrying whether
    /// the binding is mutable (`var`/`own`) or not (`let`). Scopes are
    /// usually tiny (a couple of params/locals), so a linear scan beats a
    /// `HashMap`'s allocation and hashing on the hot call path. Lookups scan most
    /// recent first, so a later `let` shadows an earlier one.
    ///
    /// Names are `Rc<str>`: bindings are created far more often than distinct
    /// names exist (every call re-binds its params; every loop iteration
    /// re-binds its variable), so defining clones a pointer instead of copying
    /// a `String` (the interner / per-function name cache own the one real
    /// allocation per distinct name).
    scopes: Vec<Vec<(Rc<str>, Value, bool)>>,
    /// Cleared scope vecs kept for reuse: loops push/pop a scope per
    /// iteration, and recycling the allocation removes a malloc/free pair
    /// from every iteration. Capacity is not a semantic: excluded from
    /// `Clone`/`PartialEq` (manual impls below), so a cloned env (a closure
    /// capture) or an env comparison behaves exactly as before.
    spare: Vec<Vec<(Rc<str>, Value, bool)>>,
}

/// `spare` is a reuse pool, not state: clones start with an empty pool.
impl Clone for Env {
    fn clone(&self) -> Self {
        Self { scopes: self.scopes.clone(), spare: Vec::new() }
    }
}

/// `spare` is a reuse pool, not state: equality is over bindings only.
impl PartialEq for Env {
    fn eq(&self, other: &Self) -> bool {
        self.scopes == other.scopes
    }
}

impl Env {
    fn new() -> Self {
        Self {
            scopes: vec![Vec::new()],
            spare: Vec::new(),
        }
    }
    fn push(&mut self) {
        self.scopes.push(self.spare.pop().unwrap_or_default());
    }
    fn pop(&mut self) {
        if let Some(mut scope) = self.scopes.pop() {
            scope.clear();
            if self.spare.len() < 16 {
                self.spare.push(scope);
            }
        }
    }
    fn define(&mut self, name: Rc<str>, value: Value, mutable: bool) {
        self.scopes.last_mut().unwrap().push((name, value, mutable));
    }
    fn get(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            for (n, v, _) in scope.iter().rev() {
                if &**n == name {
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
                if &**n == name {
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
        let mut scope: Vec<(Rc<str>, Value, bool)> = Vec::new();
        for s in &self.scopes {
            for (n, v, m) in s {
                if mentioned.contains(&**n) {
                    match scope.iter_mut().find(|(en, _, _)| en == n) {
                        Some(slot) => *slot = (n.clone(), v.clone(), *m),
                        None => scope.push((n.clone(), v.clone(), *m)),
                    }
                }
            }
        }
        Env { scopes: vec![scope], spare: Vec::new() }
    }

    /// Mutable access to a binding's slot plus its mutability, innermost first
    /// (the same binding `assign` would write).
    fn slot_mut(&mut self, name: &str) -> Option<(&mut Value, bool)> {
        for scope in self.scopes.iter_mut().rev() {
            for (n, slot, mutable) in scope.iter_mut().rev() {
                if &**n == name {
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
        // (RFC-0056) Lowered before evaluation; recurse defensively (this scan
        // over-approximates, so a stray traversal here is harmless).
        Expr::LabeledCall { name, args } => {
            f(name);
            for (_, a) in args {
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
        Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for a in args {
                idents_in_expr(a, f);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. } => {
            idents_in_expr(expr, f)
        }
        Expr::Field { base, .. } => idents_in_expr(base, f),
        Expr::Lambda { body, .. } => idents_in_block(body, f),
        Expr::RecordUpdate { name: _, base, fields } => {
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
            Stmt::Let { value, .. } | Stmt::LetPattern { value, .. } => idents_in_expr(value, f),
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

/// RFC-0038: `[user_caps]` grant values — `main` parameter name → (field name →
/// value). A grantable-cap `main` parameter mints a sealed record from its entry.
pub type UserCapGrants = std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>;

pub struct Interpreter {
    // `Rc` so a call clones a pointer, not the whole function AST (this is the
    // hot path for recursion).
    functions: FxHashMap<String, Rc<Function>>,
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
    /// Direct `File` grants (RFC-0012): the i-th `File` param of `main` is the
    /// i-th path here. Read/write is the param's compile-time right, so these are
    /// plain paths. Mirrors the compiled backend's `Capabilities::file_grants`.
    file_grants: Vec<PathBuf>,
    /// Allow-list backing the root `Net` capability.
    net_allow: Vec<String>,
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
    compiler_expr_syntax: HashMap<String, Expr>,
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

fn recursive_tail_edges(
    functions: &FxHashMap<String, Rc<Function>>,
) -> FxHashMap<String, FxHashSet<String>> {
    let graph: FxHashMap<_, Vec<_>> = functions
        .values()
        .map(|function| {
            let mut targets = FxHashSet::new();
            collect_tail_callees_block(&function.body, &mut targets);
            targets.retain(|target| {
                functions.get(target).is_some_and(|target| {
                    direct_tail_abis_are_compatible(function, target)
                })
            });
            (function.name.clone(), targets.into_iter().collect())
        })
        .collect();

    let mut recursive: FxHashMap<String, FxHashSet<String>> = FxHashMap::new();
    for (source, targets) in &graph {
        for target in targets {
            let mut pending = vec![target.as_str()];
            let mut seen = FxHashSet::new();
            while let Some(next) = pending.pop() {
                if next == source {
                    recursive.entry(source.clone()).or_default().insert(target.clone());
                    break;
                }
                if seen.insert(next) && let Some(successors) = graph.get(next) {
                    pending.extend(successors.iter().map(String::as_str));
                }
            }
        }
    }
    recursive
}

fn direct_tail_abis_are_compatible(source: &Function, target: &Function) -> bool {
    let source_has_var = source.params.iter().any(|param| param.convention == Convention::Var);
    if !source_has_var {
        return target.params.iter().all(|param| param.convention != Convention::Var);
    }
    source.ret == target.ret
        && source.params.len() == target.params.len()
        && source.params.iter().zip(&target.params).all(|(source, target)| {
            source.convention == target.convention && source.ty == target.ty
        })
}

fn direct_tail_envelope_is_forwarded(
    source: &Function,
    target: &Function,
    args: &[Expr],
) -> bool {
    let source_has_var = source.params.iter().any(|param| param.convention == Convention::Var);
    if !source_has_var {
        return target.params.iter().all(|param| param.convention != Convention::Var);
    }
    direct_tail_abis_are_compatible(source, target)
        && source.params.len() == args.len()
        && source.params.iter().zip(&target.params).zip(args).all(
            |((source_param, target_param), arg)| {
                source_param.convention == target_param.convention
                    && (source_param.convention != Convention::Var
                        || matches!(arg, Expr::Var(name) if name == &source_param.name))
            },
        )
}

fn collect_tail_callees_block(block: &Block, out: &mut FxHashSet<String>) {
    for stmt in &block.stmts {
        collect_nested_returns_stmt(stmt, out);
    }
    if let Some(Stmt::Expr(expr) | Stmt::Yield(expr)) = block.stmts.last() {
        collect_tail_callees_expr(expr, out);
    }
}

fn collect_tail_callees_expr(expr: &Expr, out: &mut FxHashSet<String>) {
    match expr {
        Expr::Call { name, .. } => {
            out.insert(name.clone());
        }
        Expr::If { then_block, else_block, .. } => {
            collect_tail_callees_block(then_block, out);
            if let Some(block) = else_block {
                collect_tail_callees_block(block, out);
            }
        }
        Expr::Match { arms, .. } => {
            for arm in arms {
                collect_tail_callees_expr(&arm.body, out);
            }
        }
        Expr::Block(block) => collect_tail_callees_block(block, out),
        Expr::Binary { op: BinOp::Coalesce, rhs, .. } => {
            collect_tail_callees_expr(rhs, out);
        }
        _ => collect_nested_returns_expr(expr, out),
    }
}

fn collect_nested_returns_stmt(stmt: &Stmt, out: &mut FxHashSet<String>) {
    match stmt {
        Stmt::Return(Some(expr)) => collect_tail_callees_expr(expr, out),
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Expr(value)
        | Stmt::Yield(value) => collect_nested_returns_expr(value, out),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
    }
}

fn collect_nested_returns_block(block: &Block, out: &mut FxHashSet<String>) {
    for stmt in &block.stmts {
        collect_nested_returns_stmt(stmt, out);
    }
}

fn collect_nested_returns_expr(expr: &Expr, out: &mut FxHashSet<String>) {
    match expr {
        Expr::List(items) | Expr::Tuple(items) | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                collect_nested_returns_expr(item, out);
            }
        }
        Expr::Call { args, .. } => {
            for arg in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, arg) in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_nested_returns_expr(receiver, out);
            for arg in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_nested_returns_expr(func, out);
            for arg in args {
                collect_nested_returns_expr(arg, out);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Field { base: expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. } => collect_nested_returns_expr(expr, out),
        Expr::RecordUpdate { base, fields, .. } => {
            collect_nested_returns_expr(base, out);
            for (_, value) in fields {
                collect_nested_returns_expr(value, out);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                collect_nested_returns_expr(value, out);
            }
            if let Some(spread) = spread {
                collect_nested_returns_expr(spread, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. }
        | Expr::Index { base: lhs, index: rhs } => {
            collect_nested_returns_expr(lhs, out);
            collect_nested_returns_expr(rhs, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_nested_returns_expr(cond, out);
            collect_nested_returns_block(then_block, out);
            if let Some(block) = else_block {
                collect_nested_returns_block(block, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_nested_returns_expr(scrutinee, out);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_nested_returns_expr(guard, out);
                }
                collect_nested_returns_expr(&arm.body, out);
            }
        }
        Expr::Block(block) => collect_nested_returns_block(block, out),
        Expr::While { cond, body } => {
            collect_nested_returns_expr(cond, out);
            collect_nested_returns_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_nested_returns_expr(iter, out);
            collect_nested_returns_block(body, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_nested_returns_expr(scrutinee, out);
            collect_nested_returns_block(body, out);
        }
        Expr::Lambda { .. } | Expr::Int(_) | Expr::Float(_) | Expr::Duration(_)
        | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}

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
            root: PathBuf::from("."),
            dir_roots: Vec::new(),
            file_grants: Vec::new(),
            net_allow: Vec::new(),
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
            compiler_expr_syntax,
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
        }
    }

    /// Mint the root capability for a `main` parameter of the given type. This
    /// is where authority enters the program — `main` is the root entrypoint.
    fn root_cap_for(&self, ty: &Option<Type>) -> Result<Value, RuntimeError> {
        match ty {
            Some(Type::Named(n, _)) if n == "Console" => Ok(Value::Cap(Capability::Console)),
            Some(Type::Named(n, _)) if n == "Clock" => Ok(Value::Cap(Capability::Clock)),
            Some(Type::Named(n, _)) if n == "Rand" => Ok(Value::Cap(Capability::Rand)),
            Some(Type::Named(n, _)) if n == "Env" => Ok(Value::Cap(Capability::Env)),
            Some(Type::Named(n, _)) if n == "Dir" => Ok(Value::Dir(DirValue::Fs(self.root.clone()), String::new())),
            Some(Type::Named(n, _)) if n == "Net" => Ok(Value::Net(self.net_allow.clone())),
            Some(Type::Named(n, _)) if n == "Exec" => Ok(Value::Cap(Capability::Exec)),
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
                    "`main` parameters must be capabilities (Console, Clock, Env, Dir, Net, Exec, Secret) or `List(String)` for command-line args; got {found}"
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
                Ok(Value::Build(BuildCap::Out(grants.out_dir.clone())))
            }
            Some(Type::Named(n, _)) if n == "BuildRead" => {
                if grants.read_roots.is_empty() {
                    return err("build step demands `BuildRead` but no read grant was provided");
                }
                Ok(Value::Build(BuildCap::Read(grants.read_roots.clone())))
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

    fn capture_place(&mut self, expr: &Expr, env: &mut Env) -> Result<CapturedPlace, Flow> {
        fn capture(
            interpreter: &mut Interpreter,
            expr: &Expr,
            env: &mut Env,
            projections: &mut Vec<PlaceProjection>,
        ) -> Result<String, Flow> {
            match expr {
                Expr::Var(root) => Ok(root.clone()),
                Expr::Field { base, field } => {
                    let root = capture(interpreter, base, env, projections)?;
                    projections.push(PlaceProjection::Field(field.clone()));
                    Ok(root)
                }
                Expr::Index { base, index } => {
                    let root = capture(interpreter, base, env, projections)?;
                    let index = interpreter.eval(index, env)?;
                    projections.push(PlaceProjection::Index(index));
                    Ok(root)
                }
                Expr::Call { name, args }
                    if matches!(name.as_str(), intrinsics::LIST_AT | intrinsics::DICT_AT)
                        && args.len() == 2 =>
                {
                    let root = capture(interpreter, &args[0], env, projections)?;
                    let index = interpreter.eval(&args[1], env)?;
                    projections.push(PlaceProjection::Index(index));
                    Ok(root)
                }
                _ => err("a `var` argument must be a mutable place"),
            }
        }

        let mut projections = Vec::new();
        let root = capture(self, expr, env, &mut projections)?;
        Ok(CapturedPlace { root, projections })
    }

    fn place_field_index(&self, value: &Value, field: &str) -> Result<usize, Flow> {
        if let Ok(index) = field.parse::<usize>() {
            return match value {
                Value::Tuple(items) if index < items.len() => Ok(index),
                Value::Tuple(items) => err(format!(
                    "tuple has no element `.{index}` (it has {})",
                    items.len()
                )),
                other => err(format!("element access `.{index}` on a non-tuple value `{other}`")),
            };
        }
        let Value::Ctor { name, fields } = value else {
            return err(format!("field access `.{field}` on a non-record value `{value}`"));
        };
        self.record_fields
            .get(&**name)
            .and_then(|names| names.iter().position(|candidate| candidate == field))
            .filter(|index| *index < fields.len())
            .ok_or_else(|| Flow::from(RuntimeError { message: format!("`{name}` has no field `{field}`") }))
    }

    fn read_place_value(&self, place: &CapturedPlace, env: &Env) -> Result<Value, Flow> {
        let mut value = env
            .get(&place.root)
            .cloned()
            .ok_or_else(|| Flow::from(RuntimeError {
                message: format!(
                    "`var` argument root `{}` must be a local variable",
                    place.root
                ),
            }))?;
        for projection in &place.projections {
            value = match projection {
                PlaceProjection::Field(field) => {
                    let index = self.place_field_index(&value, field)?;
                    match &value {
                        Value::Tuple(items) => items[index].clone(),
                        Value::Ctor { fields, .. } => fields[index].clone(),
                        _ => unreachable!("place_field_index checked the aggregate"),
                    }
                }
                PlaceProjection::Index(index) => match (&value, index) {
                    (Value::List(items), Value::Int(index))
                    | (Value::Tuple(items), Value::Int(index))
                        if *index >= 0 && (*index as usize) < items.len() =>
                    {
                        items[*index as usize].clone()
                    }
                    (Value::Dict(entries), key) => entries
                        .iter()
                        .find(|(candidate, _)| candidate == key)
                        .map(|(_, value)| value.clone())
                        .ok_or_else(|| Flow::from(RuntimeError {
                            message: "dictionary key is absent".into(),
                        }))?,
                    (Value::List(items), Value::Int(index))
                    | (Value::Tuple(items), Value::Int(index)) => {
                        return err(format!(
                            "index {index} is out of bounds for length {}",
                            items.len()
                        ));
                    }
                    (_, other) => return err(format!("invalid place index `{other}`")),
                },
            };
        }
        Ok(value)
    }

    fn store_place_value(
        &mut self,
        current: &mut Value,
        projections: &[PlaceProjection],
        replacement: Value,
    ) -> Result<(), Flow> {
        self.store_place_value_inner(current, projections, replacement, false)
    }

    fn store_assignment_place_value(
        &mut self,
        current: &mut Value,
        projections: &[PlaceProjection],
        replacement: Value,
    ) -> Result<(), Flow> {
        self.store_place_value_inner(current, projections, replacement, true)
    }

    fn store_place_value_inner(
        &mut self,
        current: &mut Value,
        projections: &[PlaceProjection],
        replacement: Value,
        insert_missing_dict_leaf: bool,
    ) -> Result<(), Flow> {
        let Some((projection, rest)) = projections.split_first() else {
            *current = replacement;
            return Ok(());
        };
        match projection {
            PlaceProjection::Field(field) => {
                let index = self.place_field_index(current, field)?;
                match current {
                    Value::Tuple(items) => self.store_place_value_inner(
                        &mut Rc::make_mut(items)[index],
                        rest,
                        replacement,
                        insert_missing_dict_leaf,
                    ),
                    Value::Ctor { fields, .. } => self.store_place_value_inner(
                        &mut Rc::make_mut(fields)[index],
                        rest,
                        replacement,
                        insert_missing_dict_leaf,
                    ),
                    _ => unreachable!("place_field_index checked the aggregate"),
                }
            }
            PlaceProjection::Index(index) => match (current, index) {
                (Value::List(items), Value::Int(index))
                    if *index >= 0 && (*index as usize) < items.len() =>
                {
                    self.store_place_value_inner(
                        &mut Rc::make_mut(items)[*index as usize],
                        rest,
                        replacement,
                        insert_missing_dict_leaf,
                    )
                }
                (Value::Tuple(items), Value::Int(index))
                    if *index >= 0 && (*index as usize) < items.len() =>
                {
                    self.store_place_value_inner(
                        &mut Rc::make_mut(items)[*index as usize],
                        rest,
                        replacement,
                        insert_missing_dict_leaf,
                    )
                }
                (Value::Dict(entries), key) => {
                    let position = self.dict_key_position(entries, key)?;
                    let entries = Rc::make_mut(entries);
                    if let Some(index) = position {
                        self.store_place_value_inner(
                            &mut entries[index].1,
                            rest,
                            replacement,
                            insert_missing_dict_leaf,
                        )
                    } else if insert_missing_dict_leaf && rest.is_empty() {
                        entries.push((key.clone(), replacement));
                        Ok(())
                    } else {
                        err("dictionary key is absent")
                    }
                }
                (Value::List(items), Value::Int(index)) => err(
                    DiagTemplate::ListIndexOob.render(
                        *index,
                        items.len() as i64,
                        "",
                    ),
                ),
                (Value::Tuple(items), Value::Int(index)) => err(format!(
                    "index {index} is out of bounds for length {}",
                    items.len()
                )),
                (_, other) => err(format!("invalid place index `{other}`")),
            },
        }
    }

    fn try_desugared_place_assign(
        &mut self,
        name: &str,
        expression: &Expr,
        env: &mut Env,
    ) -> Result<bool, Flow> {
        let Some(plan) = desugared_assignment_plan(name, expression) else {
            return Ok(false);
        };
        let mut projections = Vec::with_capacity(plan.projections.len());
        for projection in plan.projections {
            projections.push(match projection {
                AssignmentProjection::Field(field) => {
                    PlaceProjection::Field(field.to_string())
                }
                AssignmentProjection::Index { expression, .. } => {
                    PlaceProjection::Index(self.eval(expression, env)?)
                }
            });
        }
        let replacement = self.eval(plan.replacement, env)?;
        let mut current = env.get(name).cloned().ok_or_else(|| {
            Flow::from(RuntimeError {
                message: format!("cannot assign to unbound variable `{name}`"),
            })
        })?;
        self.store_assignment_place_value(
            &mut current,
            &projections,
            replacement,
        )?;
        match env.assign(name, current) {
            Assign::Done => Ok(true),
            Assign::Immutable => err(format!(
                "cannot assign to `{name}`: it is immutable (declared with `let`)"
            )),
            Assign::Unbound => {
                err(format!("cannot assign to unbound variable `{name}`"))
            }
        }
    }

    fn commit_writebacks(
        &mut self,
        writebacks: Vec<(CapturedPlace, Value)>,
        env: &mut Env,
    ) -> Result<(), Flow> {
        let mut roots: Vec<(String, Value)> = Vec::new();
        for (place, value) in writebacks {
            let root = if let Some((_, root)) = roots.iter_mut().find(|(name, _)| *name == place.root) {
                root
            } else {
                let current = env.get(&place.root).cloned().ok_or_else(|| {
                    Flow::from(RuntimeError {
                        message: format!(
                            "`var` argument root `{}` must be a local variable",
                            place.root
                        ),
                    })
                })?;
                roots.push((place.root.clone(), current));
                &mut roots.last_mut().expect("just pushed root").1
            };
            self.store_place_value(root, &place.projections, value)?;
        }
        for (name, value) in roots {
            match env.assign(&name, value) {
                Assign::Done => {}
                Assign::Immutable => {
                    return err(format!("`var` argument root `{name}` must be a mutable `var`"));
                }
                Assign::Unbound => {
                    return err(format!("`var` argument root `{name}` must be a local variable"));
                }
            }
        }
        Ok(())
    }

    fn run_callable(
        &mut self,
        mut callable: TailCallable,
        mut argvals: Vec<Value>,
    ) -> Result<CallableOutcome, Flow> {
        self.depth += 1;
        if self.depth > self.depth_limit {
            self.depth -= 1;
            return err("call stack too deep (possible infinite recursion)");
        }
        let prev_fn = self.cur_fn.clone();
        let prev_line = self.cur_line;
        let prev_tail_function = self.tail_function.take();
        let prev_tail_dynamic_chain = self.tail_dynamic_chain;
        let mut dynamic_chain = matches!(callable, TailCallable::Closure(_));
        let result = loop {
            let current_args = std::mem::take(&mut argvals);
            let (function, mut env, is_closure) = match callable {
                TailCallable::Function(function) => {
                    if function.params.len() != current_args.len() {
                        break err(format!(
                            "`{}` expects {} argument(s) but got {}",
                            function.name,
                            function.params.len(),
                            current_args.len()
                        ));
                    }
                    let mut env = Env::new();
                    let cached_names = self.param_names.get(&(Rc::as_ptr(&function) as usize)).cloned();
                    for (index, (param, value)) in
                        function.params.iter().zip(current_args).enumerate()
                    {
                        let name = match &cached_names {
                            Some(names) => names[index].clone(),
                            None => Rc::from(param.name.as_str()),
                        };
                        env.define(name, value, param.convention.binds_mutable());
                    }
                    (function, env, false)
                }
                TailCallable::Closure(Value::Closure { function, env }) => {
                    if function.params.len() != current_args.len() {
                        break err(format!(
                            "function expects {} argument(s) but got {}",
                            function.params.len(),
                            current_args.len()
                        ));
                    }
                    let mut env = *env;
                    env.push();
                    let cached_names = self.param_names.get(&(Rc::as_ptr(&function) as usize)).cloned();
                    for (index, (param, value)) in
                        function.params.iter().zip(current_args).enumerate()
                    {
                        let name = match &cached_names {
                            Some(names) => names[index].clone(),
                            None => Rc::from(param.name.as_str()),
                        };
                        env.define(name, value, param.convention.binds_mutable());
                    }
                    (function, env, true)
                }
                TailCallable::Closure(_) => break err("attempted to call a non-function value"),
            };
            self.cur_fn = function.name.clone();
            self.tail_function = Some(function.clone());
            dynamic_chain |= is_closure;
            self.tail_dynamic_chain = dynamic_chain;
            match self.eval_function_block(&function.body, &function, &mut env) {
                Err(Flow::TailCall { callable: next, args: next_args }) => {
                    callable = next;
                    argvals = next_args;
                }
                Ok(value) | Err(Flow::Return(value)) => {
                    break Ok(CallableOutcome { value, function, env });
                }
                Err(error @ Flow::Err(_)) => break Err(error),
                Err(Flow::Break | Flow::Continue) => {
                    break err("`break`/`continue` outside a loop");
                }
            }
        };
        self.tail_function = prev_tail_function;
        self.tail_dynamic_chain = prev_tail_dynamic_chain;
        self.depth -= 1;
        if result.is_ok() {
            self.cur_fn = prev_fn;
            self.cur_line = prev_line;
        }
        result
    }

    /// Apply a closure to already-evaluated arguments. The closure runs in its
    /// captured environment (plus a fresh scope for the parameters), and its body
    /// is a function boundary, so a `?` inside it returns from the closure.
    fn run_closure(&mut self, clo: Value, argvals: Vec<Value>) -> Result<ClosureOutcome, Flow> {
        let outcome = self.run_callable(TailCallable::Closure(clo), argvals)?;
        let writebacks = outcome
            .function
            .params
            .iter()
            .enumerate()
            .filter(|(_, param)| param.convention == Convention::Var)
            .map(|(index, param)| {
                let value = outcome
                    .env
                    .get(&param.name)
                    .cloned()
                    .expect("closure parameter is bound");
                (index, value)
            })
            .collect();
        Ok(ClosureOutcome { value: outcome.value, writebacks })
    }

    fn apply_closure(&mut self, clo: Value, argvals: Vec<Value>) -> Result<Value, Flow> {
        let outcome = self.run_closure(clo, argvals)?;
        if !outcome.writebacks.is_empty() {
            return err("a `var` function value requires a mutable caller place");
        }
        Ok(outcome.value)
    }

    fn apply_closure_call(
        &mut self,
        clo: Value,
        argvals: Vec<Value>,
        places: Vec<Option<CapturedPlace>>,
        env: &mut Env,
    ) -> Result<Value, Flow> {
        let outcome = self.run_closure(clo, argvals)?;
        let writebacks = outcome
            .writebacks
            .into_iter()
            .map(|(index, value)| {
                let place = places
                    .get(index)
                    .and_then(Clone::clone)
                    .ok_or_else(|| Flow::from(RuntimeError {
                        message: "a `var` function-value argument must be a mutable place".into(),
                    }))?;
                Ok((place, value))
            })
            .collect::<Result<Vec<_>, Flow>>()?;
        self.commit_writebacks(writebacks, env)?;
        Ok(outcome.value)
    }

    /// Evaluate a function call expression, honoring parameter conventions:
    /// `var` arguments must be mutable variables and are written back after
    /// the call returns (Hylo-style move-in / move-out).
    fn eval_call_args(
        &mut self,
        args: &[Expr],
        params: &[Param],
        env: &mut Env,
    ) -> Result<(Vec<Value>, Vec<Option<CapturedPlace>>), Flow> {
        let mut values = Vec::with_capacity(args.len());
        // The overwhelmingly common call has no `var` parameter; leave `places`
        // unallocated then (`Vec::new` doesn't allocate, and every consumer
        // reads it through `.get(i)`, where absent == None).
        let any_var = params
            .iter()
            .take(args.len())
            .any(|param| param.convention == Convention::Var);
        let mut places = if any_var { Vec::with_capacity(args.len()) } else { Vec::new() };
        for (index, arg) in args.iter().enumerate() {
            if params.get(index).map(|param| param.convention) == Some(Convention::Var) {
                let place = self.capture_place(arg, env)?;
                let value = self.read_place_value(&place, env)?;
                values.push(value);
                places.push(Some(place));
            } else {
                values.push(self.eval(arg, env)?);
                if any_var {
                    places.push(None);
                }
            }
        }
        Ok((values, places))
    }

    fn call_interpreter_special(
        &mut self,
        name: &str,
        argvals: &[Value],
    ) -> Result<Option<(Value, Vec<Value>)>, Flow> {
        // NOTE: any name for which this OR `call_builtin` produces a result MUST be
        // covered by `is_interpreter_builtin` (below) — the fast path in `eval_call`
        // skips both when that predicate is false. `interpreter_builtin_names_are_covered`
        // (test) enforces it so a new dispatch arm can't silently regress the fast path.
        // Native `var` operations have two independent result channels: the
        // ordinary source value and each final `var` value. Keep that split here
        // instead of encoding write-back into a tuple that source code must unpack.
        if intrinsics::is_list_pop_extract(name) && argvals.len() == 1
        {
            let Value::List(items) = &argvals[0] else {
                return err("pop expects a list");
            };
            let mut out = (**items).clone();
            let old = match out.pop() {
                Some(value) => Value::ctor("Some", vec![value]),
                None => Value::ctor("None", Vec::new()),
            };
            return Ok(Some((old, vec![Value::list(out)])));
        }
        if intrinsics::is_dict_insert_extract(name) && argvals.len() == 3
        {
            let Value::Dict(entries) = &argvals[0] else {
                return err("insert expects a Dict, a key, and a value");
            };
            let mut out = (**entries).clone();
            let previous = match self.dict_key_position(&out, &argvals[1])? {
                Some(index) => {
                    let old = std::mem::replace(&mut out[index].1, argvals[2].clone());
                    Value::ctor("Some", vec![old])
                }
                None => {
                    out.push((argvals[1].clone(), argvals[2].clone()));
                    Value::ctor("None", Vec::new())
                }
            };
            return Ok(Some((previous, vec![Value::dict(out)])));
        }
        if intrinsics::is_dict_remove_extract(name) && argvals.len() == 2
        {
            let Value::Dict(entries) = &argvals[0] else {
                return err("remove expects a Dict and a key");
            };
            let mut out = (**entries).clone();
            let previous = match self.dict_key_position(&out, &argvals[1])? {
                Some(index) => Value::Ctor {
                    name: "Some".into(),
                    fields: Rc::new(vec![out.remove(index).1]),
                },
                None => Value::ctor("None", Vec::new()),
            };
            return Ok(Some((previous, vec![Value::dict(out)])));
        }
        // These two operations need the interpreter to apply a function value,
        // so they cannot live in the pure builtin table.
        if name == intrinsics::DICT_UPDATE && argvals.len() == 4 {
            let Value::Dict(entries) = &argvals[0] else {
                return err("update expects a Dict as its first argument");
            };
            let mut out = (**entries).clone();
            let key = &argvals[1];
            let position = self.dict_key_position(&out, key)?;
            let current = position
                .map(|index| out[index].1.clone())
                .unwrap_or_else(|| argvals[2].clone());
            let new_v = self.apply_closure(argvals[3].clone(), vec![current])?;
            match position {
                Some(index) => out[index].1 = new_v,
                None => out.push((argvals[1].clone(), new_v)),
            }
            return Ok(Some((Value::dict(out), Vec::new())));
        }
        if name == "vm.par_map" && argvals.len() == 2 {
            let Value::List(items) = &argvals[0] else {
                return err("par_map expects a list as its first argument");
            };
            let items = items.clone();
            let f = argvals[1].clone();
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter().cloned() {
                out.push(self.apply_closure(f.clone(), vec![item])?);
            }
            return Ok(Some((Value::list(out), Vec::new())));
        }
        Ok(None)
    }

    fn eval_call(&mut self, name: &str, args: &[Expr], env: &mut Env) -> Result<Value, Flow> {
        // Record an assertion call SITE *before* evaluating arguments — nested
        // calls in the arguments move `cur_line`, so capturing it later (e.g. once
        // we're inside the callee) would report the wrong line.
        self.note_assert_crossing(name);
        let name = witchy_syntax::cap_ops::surface_name(name);
        let local_closure = matches!(env.get(name), Some(Value::Closure { .. }))
            .then(|| env.get(name).expect("closure just matched").clone());
        // ONE table lookup for the whole call (an Rc clone): it feeds the
        // parameter-convention slice here and is the callee at the end —
        // no per-call Vec<Convention> collect, no second lookup.
        let callee = self.functions.get(name).cloned();
        let closure_fn = local_closure.as_ref().and_then(|value| match value {
            Value::Closure { function, .. } => Some(function.clone()),
            _ => None,
        });
        let params: &[Param] = closure_fn
            .as_ref()
            .or(callee.as_ref())
            .map(|function| function.params.as_slice())
            .unwrap_or(&[]);
        let (argvals, places) = self.eval_call_args(args, params, env)?;
        // Fast path: skip both builtin-dispatch probes for a name that is not a
        // builtin (a plain user function / closure). Each probe otherwise re-scans
        // the intrinsic table (see is_interpreter_builtin) — ~33% of call-dense
        // interpreter self-time. Ordering within the builtin case is UNCHANGED:
        // special before the closure check, call_builtin after.
        let maybe_builtin = is_interpreter_builtin(name);
        if maybe_builtin {
            if let Some((value, var_values)) = self.call_interpreter_special(name, &argvals)? {
                let var_places: Vec<CapturedPlace> = params
                    .iter()
                    .enumerate()
                    .filter_map(|(index, param)| {
                        (param.convention == Convention::Var)
                            .then(|| places.get(index).and_then(Clone::clone))
                            .flatten()
                    })
                    .collect();
                if var_places.len() != var_values.len() {
                    return err(format!(
                        "internal: native `{name}` returned {} `var` value(s), expected {}",
                        var_values.len(),
                        var_places.len()
                    ));
                }
                self.commit_writebacks(var_places.into_iter().zip(var_values).collect(), env)?;
                return Ok(value);
            }
        }
        // A local variable holding a function value (a closure): apply it.
        if let Some(clo) = local_closure {
            return self.apply_closure_call(clo, argvals, places, env);
        }
        if maybe_builtin {
            if let Some(v) = self.call_builtin(name, &argvals)? {
                let var_indices: Vec<usize> = params
                    .iter()
                    .enumerate()
                    .filter_map(|(index, param)| {
                        (param.convention == Convention::Var).then_some(index)
                    })
                    .collect();
                if let [index] = var_indices.as_slice() {
                    let place = places
                        .get(*index)
                        .and_then(Clone::clone)
                        .ok_or_else(|| Flow::from(RuntimeError {
                            message: format!("`var` argument to `{name}` must be a mutable place"),
                        }))?;
                    // Current native collection primitives return the updated receiver.
                    // The stdlib migration will split auxiliary results from this
                    // write-back channel without changing the place machinery.
                    self.commit_writebacks(vec![(place, v.clone())], env)?;
                }
                return Ok(v);
            }
        }
        let Some(func) = callee else {
            return err(format!("call to unknown function `{name}`"));
        };
        if func.params.len() != argvals.len() {
            return err(format!(
                "`{name}` expects {} argument(s) but got {}",
                func.params.len(),
                argvals.len()
            ));
        }
        let mut writebacks: Vec<CapturedPlace> = Vec::new();
        for (i, param) in func.params.iter().enumerate() {
            if matches!(param.convention, Convention::Var) {
                let place = places
                    .get(i)
                    .and_then(Clone::clone)
                    .ok_or_else(|| Flow::from(RuntimeError {
                        message: format!("`var` argument to `{name}` must be a mutable place"),
                    }))?;
                writebacks.push(place);
            }
        }
        // The callee's own `?` early-return stops at this callable boundary; it
        // becomes the call's value rather than propagating into the caller.
        let outcome = self.run_callable(TailCallable::Function(func), argvals)?;
        let result = outcome.value;
        let fenv = outcome.env;
        let var_values: Vec<_> = outcome
            .function
            .params
            .iter()
            .filter(|param| param.convention == Convention::Var)
            .map(|param| {
                fenv.get(&param.name)
                    .cloned()
                    .expect("terminal var parameter is bound")
            })
            .collect();
        if writebacks.len() != var_values.len() {
            return err("internal: a tail call changed the var write-back envelope");
        }
        self.commit_writebacks(writebacks.into_iter().zip(var_values).collect(), env)?;
        Ok(result)
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

    fn call_builtin(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, RuntimeError> {
        let catalog = intrinsics::lookup(name);
        if let Some(spec) = catalog {
            if args.len() != spec.arity {
                return err(intrinsics::arity_diagnostic(spec, args.len()));
            }
        }
        // `secret_store.get(name)` — a named lookup into the granted store. Handled
        // here (not in `native`) because a `SecretStore` is not a `NativeValue`.
        if name == "secretstore.get" {
            return match args {
                [Value::SecretStore(map), Value::Str(key)] => Ok(Some(match map.get(key.as_str()) {
                    Some((bytes, use_only)) => Value::Ctor {
                        name: "Some".into(),
                        fields: Rc::new(vec![Value::Secret(bytes.clone(), *use_only)]),
                    },
                    None => Value::ctor("None", Vec::new()),
                })),
                _ => err("secretstore.get expects (SecretStore, name)"),
            };
        }
        // `__try_ctx(value, msg)` — the `e ? "msg"` desugar. Turn the operand (an
        // `Option` or a `Result`) into a `Result(T, String)` carrying `msg`: `None`
        // -> `Err(msg)`, a `Result`'s `Err(e)` -> `Err("msg: e")` (e is a String),
        // and `Some(x)`/`Ok(x)` -> `Ok(x)`. The enclosing `?` then unwraps it.
        if name == intrinsics::TRY_CONTEXT {
            return match args {
                [val, Value::Str(msg)] => {
                    let out = match val {
                        Value::Ctor { name: c, fields } if &**c == "Some" || &**c == "Ok" => {
                            Value::Ctor { name: "Ok".into(), fields: fields.clone() }
                        }
                        Value::Ctor { name: c, .. } if &**c == "None" => {
                            Value::ctor("Err", vec![Value::Str(msg.clone())])
                        }
                        Value::Ctor { name: c, fields } if &**c == "Err" => {
                            let inner = match fields.first() {
                                Some(Value::Str(e)) => (**e).clone(),
                                Some(other) => format!("{other}"),
                                None => String::new(),
                            };
                            Value::Ctor {
                                name: "Err".into(),
                                fields: Rc::new(vec![Value::str(format!("{msg}: {inner}"))]),
                            }
                        }
                        _ => return err("`? \"msg\"` applies to an Option or Result"),
                    };
                    Ok(Some(out))
                }
                _ => err(format!("{} expects (value, message)", intrinsics::TRY_CONTEXT)),
            };
        }
        // `secret_store.require(name)` — a required secret: the `Secret` directly,
        // or a loud error if absent (a configuration mistake, not an `Option`).
        if name == "secretstore.require" {
            return match args {
                [Value::SecretStore(map), Value::Str(key)] => match map.get(key.as_str()) {
                    Some((bytes, use_only)) => Ok(Some(Value::Secret(bytes.clone(), *use_only))),
                    None => err(format!("required secret `{key}` was not granted")),
                },
                _ => err("secretstore.require expects (SecretStore, name)"),
            };
        }
        // `crypto.reveal` is gated: a `Secret` equal to the signing key (the bare
        // `Secret` / `require("signing")`) is sign-only and must not be revealed —
        // only named value-secrets are. Mirrors the WASM host (`host_crypto_reveal_len`)
        // through the one shared identity rule so the backends can't drift.
        if name == intrinsics::CRYPTO_REVEAL {
            if let [Value::Secret(bytes, use_only)] = args {
                // (RFC-0060) A use-only secret is consumable by handle but never revealable.
                if *use_only {
                    return err(witchy_caps::capabilities::USE_ONLY_SECRET_REVEAL_ERROR);
                }
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
        if catalog.is_some_and(|spec| spec.runtime == intrinsics::IntrinsicRuntime::Native) {
            return err(format!("internal error: cataloged native operation `{name}` has no runtime hook"));
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
                    Ok(Some(Value::Unit))
                }
                [_, _] => err("print requires a Console capability as its first argument"),
                _ => err("print expects a Console capability and a message: console.print(msg)"),
            },
            name if intrinsics::is_meta_fresh_ident(name) => match one(args)? {
                Value::Str(hint) => Ok(Some(Value::str(self.next_fresh_ident(hint.as_str())?))),
                other => err(format!("meta.fresh expects a String hint, got `{other}`")),
            },
            name if intrinsics::is_meta_call_site_expr(name) => match one(args)? {
                Value::Str(name) => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.call_site is available only during compile-time expansion",
                        );
                    }
                    let expr = witchy_syntax::linker::call_site_expr(name.as_str());
                    let handle = self.next_compiler_syntax_handle("call-site-expression")?;
                    self.compiler_expr_syntax.insert(handle.clone(), expr);
                    self.compiler_expr_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin {
                            definition_line: self.cur_line,
                            hole_ancestry: Vec::new(),
                        },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerExprSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::Str(name)]),
                    }))
                }
                other => err(format!("meta.call_site expects a String name, got `{other}`")),
            },
            name if intrinsics::is_meta_call_site_type(name) => match args {
                [Value::Str(name), Value::List(args)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.call_site is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        args,
                        SyntaxCategory::Type,
                        "CompilerTypeSyntax",
                        &self.compiler_type_origins,
                        self.cur_line,
                    );
                    let args = compiler_type_holes(args, &self.compiler_type_syntax)?;
                    let source = witchy_syntax::linker::call_site_type_source(name, &args);
                    let ty = witchy_syntax::linker::call_site_type(name, args);
                    let handle = self.next_compiler_syntax_handle("call-site-type")?;
                    self.compiler_type_syntax.insert(handle.clone(), ty);
                    self.compiler_type_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin {
                            definition_line: self.cur_line,
                            hole_ancestry,
                        },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerTypeSyntax".into(),
                        fields: Rc::new(vec![
                            Value::str(handle),
                            Value::str(source),
                        ]),
                    }))
                }
                _ => err("meta.call_site type construction expects a name and type arguments"),
            },
            name if intrinsics::is_meta_call_site_pattern(name) => match args {
                [Value::Str(name), Value::List(args)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.call_site is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        args,
                        SyntaxCategory::Pattern,
                        "CompilerPatternSyntax",
                        &self.compiler_pattern_origins,
                        self.cur_line,
                    );
                    let args =
                        compiler_pattern_holes(args, &self.compiler_pattern_syntax)?;
                    let source =
                        witchy_syntax::linker::call_site_pattern_source(name, &args);
                    let pattern = witchy_syntax::linker::call_site_pattern(name, args);
                    let handle = self.next_compiler_syntax_handle("call-site-pattern")?;
                    self.compiler_pattern_syntax.insert(handle.clone(), pattern);
                    self.compiler_pattern_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin {
                            definition_line: self.cur_line,
                            hole_ancestry,
                        },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerPatternSyntax".into(),
                        fields: Rc::new(vec![
                            Value::str(handle),
                            Value::str(source),
                        ]),
                    }))
                }
                _ => {
                    err("meta.call_site pattern construction expects a name and pattern arguments")
                }
            },
            name if intrinsics::is_meta_expr_call(name) => match args {
                [callee, Value::List(args)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.expr_call is available only during compile-time expansion",
                        );
                    }
                    let mut inputs = Vec::with_capacity(args.len() + 1);
                    inputs.push(callee.clone());
                    inputs.extend(args.iter().cloned());
                    let hole_ancestry = compiler_direct_hole_origins(
                        &inputs,
                        SyntaxCategory::Expr,
                        "CompilerExprSyntax",
                        &self.compiler_expr_origins,
                        self.cur_line,
                    );
                    let callee = compiler_expr_syntax_value(callee, &self.compiler_expr_syntax)?;
                    let args = args
                        .iter()
                        .map(|arg| compiler_expr_syntax_value(arg, &self.compiler_expr_syntax))
                        .collect::<Result<Vec<_>, _>>()?;
                    let expr = Expr::Apply { func: Box::new(callee), args };
                    let source = witchy_syntax::format::expr_str(&expr);
                    let handle = self.next_compiler_syntax_handle("expression-call")?;
                    self.compiler_expr_syntax.insert(handle.clone(), expr);
                    self.compiler_expr_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin { definition_line: self.cur_line, hole_ancestry },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerExprSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.expr_call expects an ExprSyntax callee and List(ExprSyntax) arguments"),
            },
            name if intrinsics::is_meta_expr_field(name) => match args {
                [base, field] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.expr_field is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        std::slice::from_ref(base),
                        SyntaxCategory::Expr,
                        "CompilerExprSyntax",
                        &self.compiler_expr_origins,
                        self.cur_line,
                    );
                    let base = compiler_expr_syntax_value(base, &self.compiler_expr_syntax)?;
                    let field = compiler_ident_name(field, "meta.expr_field")?;
                    let expr = Expr::Field { base: Box::new(base), field };
                    let source = witchy_syntax::format::expr_str(&expr);
                    let handle = self.next_compiler_syntax_handle("expression-field")?;
                    self.compiler_expr_syntax.insert(handle.clone(), expr);
                    self.compiler_expr_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin { definition_line: self.cur_line, hole_ancestry },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerExprSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.expr_field expects an ExprSyntax base and Ident field"),
            },
            name if intrinsics::is_meta_match_arm(name) => match args {
                [pattern, body] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.match_arm is available only during compile-time expansion",
                        );
                    }
                    let pattern = compiler_pattern_syntax_value(
                        pattern,
                        &self.compiler_pattern_syntax,
                    )?;
                    let body = compiler_expr_syntax_value(body, &self.compiler_expr_syntax)?;
                    let source = format!(
                        "{} -> {}",
                        witchy_syntax::format::pattern_str(&pattern),
                        witchy_syntax::format::expr_str(&body),
                    );
                    let arm = MatchArm {
                        line: self.cur_line,
                        pattern,
                        guard: None,
                        body,
                    };
                    let handle = self.next_compiler_syntax_handle("match-arm")?;
                    self.compiler_match_arm_syntax.insert(handle.clone(), arm);
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerMatchArmSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.match_arm expects PatternSyntax and ExprSyntax"),
            },
            name if intrinsics::is_meta_expr_match(name) => match args {
                [scrutinee, arms] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.expr_match is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        std::slice::from_ref(scrutinee),
                        SyntaxCategory::Expr,
                        "CompilerExprSyntax",
                        &self.compiler_expr_origins,
                        self.cur_line,
                    );
                    let scrutinee =
                        compiler_expr_syntax_value(scrutinee, &self.compiler_expr_syntax)?;
                    let arms = compiler_match_arms(arms, &self.compiler_match_arm_syntax)?;
                    let expr = Expr::Match { scrutinee: Box::new(scrutinee), arms };
                    let canonical_source = witchy_syntax::format::expr_str(&expr);
                    let handle = self.next_compiler_syntax_handle("expression-match")?;
                    self.compiler_expr_syntax.insert(handle.clone(), expr);
                    self.compiler_expr_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin { definition_line: self.cur_line, hole_ancestry },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerExprSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(canonical_source)]),
                    }))
                }
                _ => err("meta.expr_match expects an ExprSyntax scrutinee and List(MatchArmSyntax) arms"),
            },
            name if intrinsics::is_meta_stmt_expr(name) => match args {
                [expr] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.stmt_expr is available only during compile-time expansion",
                        );
                    }
                    let expr =
                        compiler_expr_syntax_value(expr, &self.compiler_expr_syntax)?;
                    Ok(Some(self.store_compiler_stmt_syntax(
                        "expression-statement",
                        Stmt::Expr(expr),
                    )?))
                }
                _ => err("meta.stmt_expr expects ExprSyntax"),
            },
            name if intrinsics::is_meta_stmt_return(name) => match args {
                [expr] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.stmt_return is available only during compile-time expansion",
                        );
                    }
                    let expr =
                        compiler_expr_syntax_value(expr, &self.compiler_expr_syntax)?;
                    Ok(Some(self.store_compiler_stmt_syntax(
                        "return-statement",
                        Stmt::Return(Some(expr)),
                    )?))
                }
                _ => err("meta.stmt_return expects ExprSyntax"),
            },
            name if intrinsics::is_meta_stmt_let(name) => match args {
                [Value::Bool(mutable), binding, ty, value] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.stmt_let is available only during compile-time expansion",
                        );
                    }
                    let name = compiler_binding_ident_name(binding, "meta.stmt_let")?;
                    let ty = compiler_optional_type_syntax_value(
                        ty,
                        &self.compiler_type_syntax,
                    )?;
                    let value =
                        compiler_expr_syntax_value(value, &self.compiler_expr_syntax)?;
                    Ok(Some(self.store_compiler_stmt_syntax(
                        "let-statement",
                        Stmt::Let { name, ty, mutable: *mutable, value },
                    )?))
                }
                _ => err(
                    "meta.stmt_let expects Bool, Ident, Option(TypeSyntax), and ExprSyntax",
                ),
            },
            name if intrinsics::is_meta_block(name) => match args {
                [Value::List(stmts), tail] => {
                    if self.fresh_ident_scope.is_none() {
                        return err("meta.block is available only during compile-time expansion");
                    }
                    let mut stmts = stmts
                        .iter()
                        .map(|stmt| {
                            compiler_stmt_syntax_value(stmt, &self.compiler_stmt_syntax)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Some(tail) =
                        compiler_optional_expr_syntax_value(tail, &self.compiler_expr_syntax)?
                    {
                        stmts.push(Stmt::Expr(tail));
                    }
                    if stmts.is_empty() {
                        return err(
                            "meta.block body must contain at least one statement or tail expression",
                        );
                    }
                    let block = Block {
                        lines: vec![self.cur_line; stmts.len()],
                        stmts,
                        region: None,
                    };
                    let source = witchy_syntax::format::block_str(&block);
                    let handle = self.next_compiler_syntax_handle("block-builder")?;
                    self.compiler_block_syntax.insert(handle.clone(), block);
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerBlockSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.block expects List(StmtSyntax) and Option(ExprSyntax)"),
            },
            name if intrinsics::is_meta_param(name) => match args {
                [binding, ty] => {
                    if self.fresh_ident_scope.is_none() {
                        return err("meta.param is available only during compile-time expansion");
                    }
                    let name = compiler_binding_ident_name(binding, "meta.param")?;
                    let ty = compiler_type_syntax_value(ty, &self.compiler_type_syntax)?;
                    let source = format!(
                        "{name}: {}",
                        witchy_syntax::format::type_str(&ty),
                    );
                    let param = Param {
                        name,
                        ty: Some(ty),
                        convention: Convention::Let,
                        default: None,
                    };
                    let handle = self.next_compiler_syntax_handle("parameter")?;
                    self.compiler_param_syntax.insert(handle.clone(), param);
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerParamSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.param expects Ident and TypeSyntax"),
            },
            name if intrinsics::is_meta_function_block(name) => match args {
                [Value::Bool(public), name, params, ret, body] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.function_block is available only during compile-time expansion",
                        );
                    }
                    let name = compiler_binding_ident_name(name, "meta.function_block")?;
                    let params = compiler_params(params, &self.compiler_param_syntax)?;
                    let ret = compiler_optional_type_syntax_value(
                        ret,
                        &self.compiler_type_syntax,
                    )?;
                    let body = compiler_block_syntax_value(body, &self.compiler_block_syntax)?;
                    let module = parse_module("fn __witchy_meta_generated():\n    ()\n")
                        .map_err(|error| RuntimeError {
                            message: format!(
                                "meta.function_block failed to build a function skeleton: {error}"
                            ),
                        })?;
                    let [Item::Function(parsed)] = module.items.as_slice() else {
                        return err("meta.function_block failed to build one function skeleton");
                    };
                    let mut function = parsed.clone();
                    function.public = *public;
                    function.name = name;
                    function.params = params;
                    function.ret = ret;
                    function.body = body;
                    let item = Item::Function(function);
                    let handle = self.next_compiler_syntax_handle("function-item")?;
                    self.compiler_item_syntax.insert(handle.clone(), item);
                    Ok(Some(Value::Ctor {
                        name: OWNED_ITEM_SYNTAX_CTOR.into(),
                        fields: Rc::new(vec![
                            Value::str(handle),
                            Value::Int(i64::from(self.cur_line)),
                        ]),
                    }))
                }
                _ => err(
                    "meta.function_block expects Bool, Ident, List(ParamSyntax), Option(TypeSyntax), and BlockSyntax",
                ),
            },
            name if name == intrinsics::COMPILER_QUOTE_EXPR => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned expression quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_expr_syntax.contains_key(handle.as_str()) =>
                    {
                        let expr = self.compiler_expr_syntax[handle.as_str()].clone();
                        let instance_handle = self.next_compiler_syntax_handle("expression")?;
                        self.compiler_expr_syntax.insert(instance_handle.clone(), expr);
                        self.compiler_expr_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin {
                                definition_line: self.cur_line,
                                hole_ancestry: Vec::new(),
                            },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerExprSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::Str(source.clone()),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned expression quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned expression quotation expects an expression handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_EXPR_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned expression quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_expr_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned expression quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let definition_line = self.cur_line;
                        let hole_ancestry = compiler_item_hole_origins(
                            holes,
                            &self.compiler_expr_origins,
                            &self.compiler_type_origins,
                            &self.compiler_pattern_origins,
                            self.cur_line,
                        );
                        let holes = compiler_item_holes(
                            holes,
                            &self.compiler_expr_syntax,
                            &self.compiler_type_syntax,
                            &self.compiler_pattern_syntax,
                        )?;
                        let expr =
                            witchy_syntax::syntax_holes::instantiate_expr_mixed(&template, holes)
                                .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::expr_str(&expr);
                        let instance_handle = self.next_compiler_syntax_handle("expression")?;
                        if let Some(existing) = self.compiler_expr_syntax.get(&instance_handle) {
                            if existing != &expr {
                                return err(
                                    "compiler-owned expression instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_expr_syntax.insert(instance_handle.clone(), expr);
                        }
                        self.compiler_expr_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin { definition_line, hole_ancestry },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerExprSyntax".into(),
                            fields: Rc::new(vec![Value::str(instance_handle), Value::str(source)]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned expression quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned expression quotation expects an expression handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_TYPE => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned type quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_type_syntax.contains_key(handle.as_str()) =>
                    {
                        let ty = self.compiler_type_syntax[handle.as_str()].clone();
                        let instance_handle = self.next_compiler_syntax_handle("type")?;
                        self.compiler_type_syntax.insert(instance_handle.clone(), ty);
                        self.compiler_type_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin {
                                definition_line: self.cur_line,
                                hole_ancestry: Vec::new(),
                            },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerTypeSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::Str(source.clone()),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned type quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned type quotation expects a type handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_TYPE_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned type quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_type_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned type quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let definition_line = self.cur_line;
                        let hole_ancestry = compiler_direct_hole_origins(
                            holes,
                            SyntaxCategory::Type,
                            "CompilerTypeSyntax",
                            &self.compiler_type_origins,
                            self.cur_line,
                        );
                        let holes = compiler_type_holes(holes, &self.compiler_type_syntax)?;
                        let ty = witchy_syntax::syntax_holes::instantiate_type(&template, holes)
                            .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::type_str(&ty);
                        let instance_handle = self.next_compiler_syntax_handle("type")?;
                        if let Some(existing) = self.compiler_type_syntax.get(&instance_handle) {
                            if existing != &ty {
                                return err(
                                    "compiler-owned type instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_type_syntax.insert(instance_handle.clone(), ty);
                        }
                        self.compiler_type_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin { definition_line, hole_ancestry },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerTypeSyntax".into(),
                            fields: Rc::new(vec![Value::str(instance_handle), Value::str(source)]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned type quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned type quotation expects a type handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_PATTERN => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned pattern quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_pattern_syntax.contains_key(handle.as_str()) =>
                    {
                        let pattern = self.compiler_pattern_syntax[handle.as_str()].clone();
                        let instance_handle = self.next_compiler_syntax_handle("pattern")?;
                        self.compiler_pattern_syntax.insert(instance_handle.clone(), pattern);
                        self.compiler_pattern_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin {
                                definition_line: self.cur_line,
                                hole_ancestry: Vec::new(),
                            },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerPatternSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::Str(source.clone()),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned pattern quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned pattern quotation expects a pattern handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_PATTERN_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned pattern quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_pattern_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned pattern quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let definition_line = self.cur_line;
                        let hole_ancestry = compiler_direct_hole_origins(
                            holes,
                            SyntaxCategory::Pattern,
                            "CompilerPatternSyntax",
                            &self.compiler_pattern_origins,
                            self.cur_line,
                        );
                        let holes =
                            compiler_pattern_holes(holes, &self.compiler_pattern_syntax)?;
                        let pattern =
                            witchy_syntax::syntax_holes::instantiate_pattern(&template, holes)
                                .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::pattern_str(&pattern);
                        let instance_handle = self.next_compiler_syntax_handle("pattern")?;
                        if let Some(existing) = self.compiler_pattern_syntax.get(&instance_handle) {
                            if existing != &pattern {
                                return err(
                                    "compiler-owned pattern instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_pattern_syntax
                                .insert(instance_handle.clone(), pattern);
                        }
                        self.compiler_pattern_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin { definition_line, hole_ancestry },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerPatternSyntax".into(),
                            fields: Rc::new(vec![Value::str(instance_handle), Value::str(source)]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned pattern quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned pattern quotation expects a pattern handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_STMT => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned statement quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_stmt_syntax.contains_key(handle.as_str()) =>
                    {
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerStmtSyntax".into(),
                            fields: Rc::new(vec![Value::Str(handle.clone()), Value::Str(source.clone())]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned statement quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned statement quotation expects a statement handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_STMT_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned statement quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_stmt_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned statement quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let holes = compiler_item_holes(
                            holes,
                            &self.compiler_expr_syntax,
                            &self.compiler_type_syntax,
                            &self.compiler_pattern_syntax,
                        )?;
                        let stmt = witchy_syntax::syntax_holes::instantiate_stmt(&template, holes)
                            .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::stmt_str(&stmt);
                        let instance_handle =
                            format!("{handle}\0compiler-owned-statement-instance\0{source}");
                        if let Some(existing) = self.compiler_stmt_syntax.get(&instance_handle) {
                            if existing != &stmt {
                                return err(
                                    "compiler-owned statement instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_stmt_syntax.insert(instance_handle.clone(), stmt);
                        }
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerStmtSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::str(source),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned statement quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned statement quotation expects a statement handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_BLOCK => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned block quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_block_syntax.contains_key(handle.as_str()) =>
                    {
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerBlockSyntax".into(),
                            fields: Rc::new(vec![Value::Str(handle.clone()), Value::Str(source.clone())]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned block quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned block quotation expects a block handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_BLOCK_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned block quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_block_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned block quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let holes = compiler_item_holes(
                            holes,
                            &self.compiler_expr_syntax,
                            &self.compiler_type_syntax,
                            &self.compiler_pattern_syntax,
                        )?;
                        let block =
                            witchy_syntax::syntax_holes::instantiate_block(&template, holes)
                                .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::block_str(&block);
                        let instance_handle =
                            format!("{handle}\0compiler-owned-block-instance\0{source}");
                        if let Some(existing) = self.compiler_block_syntax.get(&instance_handle) {
                            if existing != &block {
                                return err(
                                    "compiler-owned block instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_block_syntax.insert(instance_handle.clone(), block);
                        }
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerBlockSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::str(source),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned block quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned block quotation expects a block handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_ITEM => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned item quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(_source)]
                        if self.compiler_item_syntax.contains_key(handle.as_str()) =>
                    {
                        Ok(Some(Value::Ctor {
                            name: OWNED_ITEM_SYNTAX_CTOR.into(),
                            fields: Rc::new(vec![
                                Value::Str(handle.clone()),
                                Value::Int(i64::from(self.cur_line)),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned item quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned item quotation expects an item handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_ITEM_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned item quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if self.compiler_item_syntax.contains_key(handle.as_str())
                            && parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        Ok(Some(Value::Ctor {
                            name: OWNED_ITEM_SYNTAX_CTOR.into(),
                            fields: Rc::new(vec![
                                Value::Str(handle.clone()),
                                Value::List(holes.clone()),
                                Value::Int(i64::from(self.cur_line)),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => {
                        err("compiler-owned item quotation referenced an invalid syntax handle or hole plan")
                    }
                    _ => err("compiler-owned item quotation expects an item handle and typed holes"),
                }
            }
            name if name == intrinsics::COMPILER_EMIT_ITEM => {
                if self.fresh_ident_scope.is_none() {
                    return err("item emission is available only during compile-time expansion");
                }
                let emission = match one(args)? {
                    Value::Ctor { name, fields }
                        if &*name == OWNED_ITEM_SYNTAX_CTOR
                            && matches!(fields.as_slice(), [Value::Str(_), Value::Int(_)]) =>
                    {
                        let [Value::Str(handle), Value::Int(definition_line)] = fields.as_slice()
                        else {
                            unreachable!()
                        };
                        let item = self
                            .compiler_item_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned item emission referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        ComptimeItemEmission::Syntax {
                            item: Box::new(item),
                            definition_line: u32::try_from(*definition_line).unwrap_or(0),
                            hole_ancestry: Vec::new(),
                        }
                    }
                    Value::Ctor { name, fields }
                        if &*name == OWNED_ITEM_SYNTAX_CTOR
                            && matches!(
                                fields.as_slice(),
                                [Value::Str(_), Value::List(_), Value::Int(_)]
                            ) =>
                    {
                        let [Value::Str(handle), Value::List(holes), Value::Int(invocation_line)] =
                            fields.as_slice()
                        else {
                            unreachable!()
                        };
                        let template = self
                            .compiler_item_syntax
                            .get(handle.as_str())
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned item emission referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let hole_ancestry = compiler_item_hole_origins(
                            holes,
                            &self.compiler_expr_origins,
                            &self.compiler_type_origins,
                            &self.compiler_pattern_origins,
                            u32::try_from(*invocation_line).unwrap_or(0),
                        );
                        let holes = compiler_item_holes(
                            holes,
                            &self.compiler_expr_syntax,
                            &self.compiler_type_syntax,
                            &self.compiler_pattern_syntax,
                        )?;
                        let item = witchy_syntax::syntax_holes::instantiate_item(template, holes)
                            .map_err(|message| RuntimeError { message })?;
                        ComptimeItemEmission::Syntax {
                            item: Box::new(item),
                            definition_line: u32::try_from(*invocation_line).unwrap_or(0),
                            hole_ancestry,
                        }
                    }
                    Value::Ctor { name, fields }
                        if name.rsplit_once('.').map_or(&*name, |(_, tail)| tail)
                            == "ItemSyntax" =>
                    {
                        let [Value::Str(source)] = fields.as_slice() else {
                            return err("ItemSyntax carried an invalid source payload");
                        };
                        ComptimeItemEmission::Source((**source).clone())
                    }
                    _ => return err("emit_item expects meta.ItemSyntax"),
                };
                self.comptime_item_output.push(PositionedComptimeItem {
                    output_position: self.output.len(),
                    emission,
                });
                Ok(Some(Value::Unit))
            }
            name if name == intrinsics::COMPILER_EMIT_EXPR => {
                if self.fresh_ident_scope.is_none() {
                    return err("expression emission is available only during compile-time expansion");
                }
                let emission = match one(args)? {
                    Value::Ctor { name, fields }
                        if name.rsplit_once('.').map_or(&*name, |(_, tail)| tail)
                            == "CompilerExprSyntax" =>
                    {
                        let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                            return err("CompilerExprSyntax carried an invalid payload");
                        };
                        let expr = self
                            .compiler_expr_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned expression emission referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        ComptimeExprEmission::Syntax(Box::new(expr))
                    }
                    Value::Ctor { name, fields }
                        if name.rsplit_once('.').map_or(&*name, |(_, tail)| tail)
                            == "ExprSyntax" =>
                    {
                        let [Value::Str(source)] = fields.as_slice() else {
                            return err("ExprSyntax carried an invalid source payload");
                        };
                        ComptimeExprEmission::Source((**source).clone())
                    }
                    _ => return err("expression emission expects meta.ExprSyntax"),
                };
                self.comptime_expr_output.push(emission);
                Ok(Some(Value::Unit))
            }
            // Pure builtins need no capability.
            name if is_render_intrinsic(name) => Ok(Some(Value::str(self.render_value(&one(args)?)))),
            // (RFC-0055) Channel message erasure. `Value` is uniform, so erasing a
            // typed message to the executor's opaque `__Msg` and recovering the
            // endpoint's type are both the identity — the value passes through
            // unchanged, exactly as the executor's former generic `m` did.
            intrinsics::ERASE | intrinsics::UNERASE => Ok(Some(one(args)?)),
            // String stdlib.
            intrinsics::STRING_LENGTH => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Int(s.len() as i64))),
                other => err(format!("string_length expects a String, got `{other}`")),
            },
            // (Bytes) Primitive intrinsics behind `std/bytes`. A `Bytes` is raw bytes
            // with no UTF-8 contract; `to_string` decodes lossily (a strict decoder can
            // live in std). `Str <-> Bytes` are real conversions in the tree-walker.
            intrinsics::BYTES_FROM_STRING => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Bytes(
                    Rc::try_unwrap(s).unwrap_or_else(|rc| (*rc).clone()).into_bytes(),
                ))),
                other => err(format!("bytes.from_string expects a String, got `{other}`")),
            },
            intrinsics::BYTES_FROM_LIST => match one(args)? {
                Value::List(xs) => {
                    let mut out = Vec::with_capacity(xs.len());
                    for x in xs.iter().cloned() {
                        let Value::Int(n) = x else {
                            return err("bytes.from_list expects a List(Int)");
                        };
                        if !(0..=255).contains(&n) {
                            return err(format!("bytes.from_list: value {n} is outside 0..=255"));
                        }
                        out.push(n as u8);
                    }
                    Ok(Some(Value::Bytes(out)))
                }
                other => err(format!("bytes.from_list expects a List(Int), got `{other}`")),
            },
            intrinsics::BYTES_TO_STRING => match one(args)? {
                Value::Bytes(b) => Ok(Some(Value::str(String::from_utf8_lossy(&b).into_owned()))),
                other => err(format!("bytes.to_string expects Bytes, got `{other}`")),
            },
            intrinsics::BYTES_LENGTH => match one(args)? {
                Value::Bytes(b) => Ok(Some(Value::Int(b.len() as i64))),
                other => err(format!("bytes.length expects Bytes, got `{other}`")),
            },
            intrinsics::BYTES_AT | "bytes.at" => match args {
                [Value::Bytes(b), Value::Int(i)] => match b.get(*i as usize) {
                    Some(byte) => Ok(Some(Value::Int(*byte as i64))),
                    None => err(DiagTemplate::BytesIndexOob.render(*i, b.len() as i64, "")),
                },
                _ => err("bytes.at expects Bytes and an Int index"),
            },
            intrinsics::BYTES_CONCAT => match args {
                [Value::Bytes(a), Value::Bytes(b)] => {
                    let mut out = a.clone();
                    out.extend_from_slice(b);
                    Ok(Some(Value::Bytes(out)))
                }
                _ => err("bytes.concat expects two Bytes"),
            },
            intrinsics::BYTES_SLICE => match args {
                [Value::Bytes(b), Value::Int(start), Value::Int(end)] => {
                    let lo = (*start).max(0) as usize;
                    let hi = (*end).max(0).min(b.len() as i64) as usize;
                    let hi = hi.max(lo);
                    Ok(Some(Value::Bytes(b.get(lo..hi).unwrap_or(&[]).to_vec())))
                }
                _ => err("bytes.slice expects Bytes and two Int indices"),
            },
            // The number of Unicode scalars — the character count, as opposed to
            // `string_length`'s byte count (they agree for ASCII).
            intrinsics::STRING_CHAR_COUNT => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Int(s.chars().count() as i64))),
                other => err(format!("char_count expects a String, got `{other}`")),
            },
            // ASCII case mapping (a-z <-> A-Z); non-ASCII bytes are unchanged.
            // Deliberately ASCII-only so the WASM backend can match it byte-for-
            // byte (full Unicode case folding would need large tables).
            intrinsics::STRING_TO_UPPER => match one(args)? {
                Value::Str(s) => Ok(Some(Value::str(s.to_ascii_uppercase()))),
                other => err(format!("to_upper expects a String, got `{other}`")),
            },
            intrinsics::STRING_TO_LOWER => match one(args)? {
                Value::Str(s) => Ok(Some(Value::str(s.to_ascii_lowercase()))),
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
                    Err(RuntimeError { message: (*msg).clone() })
                }
                other => err(format!("fail expects a String message, got `{other}`")),
            },
            intrinsics::STRING_TRIM => match one(args)? {
                // ASCII whitespace only — exactly the byte set the WASM `$is_ws`
                // helper strips (space, tab, LF, VT, FF, CR). Rust's `str::trim`
                // would additionally strip Unicode whitespace (NBSP, …), which the
                // compiled backend does not, so we pin both to this set to keep the
                // backends in agreement (consistent with ASCII `to_upper`/`to_lower`).
                Value::Str(s) => {
                    let trimmed =
                        s.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
                    Ok(Some(Value::str(trimmed)))
                }
                other => err(format!("trim expects a String, got `{other}`")),
            },
            intrinsics::STRING_STARTS_WITH => match args {
                [Value::Str(s), Value::Str(prefix)] => {
                    Ok(Some(Value::Bool(s.starts_with(prefix.as_str()))))
                }
                _ => err("starts_with expects two Strings"),
            },
            intrinsics::STRING_CONTAINS => match args {
                [Value::Str(s), Value::Str(sub)] => {
                    Ok(Some(Value::Bool(s.contains(sub.as_str()))))
                }
                _ => err("contains expects two Strings"),
            },
            // Split on a separator into a list of pieces (the separator itself is
            // dropped); the empty separator yields the whole string unchanged.
            intrinsics::STRING_SPLIT => match args {
                [Value::Str(s), Value::Str(sep)] => {
                    let parts: Vec<Value> = if sep.is_empty() {
                        vec![Value::Str(s.clone())]
                    } else {
                        s.split(sep.as_str()).map(Value::str).collect()
                    };
                    Ok(Some(Value::list(parts)))
                }
                _ => err("split expects two Strings"),
            },
            // The characters of a string, each as a single-char String (one pass).
            intrinsics::STRING_CHARS => match one(args)? {
                Value::Str(s) => {
                    Ok(Some(Value::list(s.chars().map(|c| Value::str(c.to_string())).collect())))
                }
                _ => err("string_chars expects a String"),
            },
            intrinsics::STRING_REPLACE => match args {
                [Value::Str(s), Value::Str(from), Value::Str(to)] => {
                    Ok(Some(Value::str(s.replace(from.as_str(), to.as_str()))))
                }
                _ => err("replace expects three Strings"),
            },
            intrinsics::STRING_ENDS_WITH => match args {
                [Value::Str(s), Value::Str(suffix)] => {
                    Ok(Some(Value::Bool(s.ends_with(suffix.as_str()))))
                }
                _ => err("ends_with expects two Strings"),
            },
            // Char index of the first occurrence of `sub`, or -1 if absent.
            intrinsics::STRING_FIND => match args {
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
            intrinsics::STRING_SUBSTRING => match args {
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
                    Ok(Some(Value::str(out)))
                }
                _ => err("substring expects a String and two Int indices"),
            },
            // Conversions.
            intrinsics::MATH_TO_FLOAT => match one(args)? {
                Value::Int(n) => Ok(Some(Value::Float(n as f64))),
                other => err(format!("int_to_float expects an Int, got `{other}`")),
            },
            intrinsics::MATH_TO_INT => match one(args)? {
                Value::Float(x) if x.is_nan() => err(DiagTemplate::NanToInt.render(0, 0, "")),
                Value::Float(x) => Ok(Some(Value::Int(x as i64))),
                other => err(format!("float_to_int expects a Float, got `{other}`")),
            },
            // Duration <-> Int(ms): a Duration is an Int(ms) at runtime, so both
            // directions are the identity.
            "int_to_duration" | "duration_to_int" => match one(args)? {
                Value::Int(n) => Ok(Some(Value::Int(n))),
                other => err(format!("{name} expects an Int/Duration, got `{other}`")),
            },
            intrinsics::MATH_SQRT => match one(args)? {
                Value::Float(x) => Ok(Some(Value::Float(x.sqrt()))),
                other => err(format!("sqrt expects a Float, got `{other}`")),
            },
            intrinsics::STRING_TO_INT => match one(args)? {
                Value::Str(s) => match s.trim().parse::<i64>() {
                    Ok(n) => Ok(Some(Value::Int(n))),
                    Err(_) => err(DiagTemplate::ParseInt.render(0, 0, &s)),
                },
                other => err(format!("string_to_int expects a String, got `{other}`")),
            },
            intrinsics::LIST_LENGTH => match args {
                [Value::List(items)] => Ok(Some(Value::Int(items.len() as i64))),
                _ => err("length expects a list"),
            },
            intrinsics::LIST_AT => match args {
                [Value::List(items), Value::Int(i)] => match items.get(*i as usize) {
                    Some(v) => Ok(Some(v.clone())),
                    None => err(DiagTemplate::ListIndexOob.render(*i, items.len() as i64, "")),
                },
                _ => err("at expects a list and an Int index"),
            },
            // Return a new list with `x` appended (lists are values, so this does
            // not mutate the original).
            intrinsics::LIST_PUSH | intrinsics::GENERATED_LIST_PUSH => match args {
                [Value::List(items), x] => {
                    let mut out = (**items).clone();
                    out.push(x.clone());
                    Ok(Some(Value::list(out)))
                }
                _ => err("push expects a list and a value"),
            },
            name if intrinsics::is_list_pop_extract(name) => match args {
                [Value::List(items)] => {
                    let mut out = (**items).clone();
                    let old = match out.pop() {
                        Some(value) => Value::Ctor {
                            name: "Some".into(),
                            fields: Rc::new(vec![value]),
                        },
                        None => Value::ctor("None", Vec::new()),
                    };
                    Ok(Some(Value::tuple(vec![Value::list(out), old])))
                }
                _ => err("pop expects a list"),
            },
            intrinsics::LIST_SET_AT => match args {
                [Value::List(items), Value::Int(index), value] => {
                    let i = *index as usize;
                    if i >= items.len() {
                        return err(DiagTemplate::ListIndexOob.render(
                            *index,
                            items.len() as i64,
                            "",
                        ));
                    }
                    let mut out = (**items).clone();
                    out[i] = value.clone();
                    Ok(Some(Value::list(out)))
                }
                _ => err("set_at expects a list, an Int index, and a value"),
            },
            // Return a new list that is the two given lists joined.
            intrinsics::LIST_CONCAT => match args {
                [Value::List(a), Value::List(b)] => {
                    let mut out = (**a).clone();
                    out.extend(b.iter().cloned());
                    Ok(Some(Value::list(out)))
                }
                _ => err("concat expects two lists"),
            },
            // --- Dict: an immutable association map ---
            intrinsics::DICT_NEW => match args {
                [] => Ok(Some(Value::dict(Vec::new()))),
                _ => err("dict_new takes no arguments"),
            },
            // Return a new dict with `k` set to `v` (replacing any existing entry).
            intrinsics::DICT_INSERT => match args {
                [Value::Dict(entries), k, v] => {
                    let mut out = (**entries).clone();
                    match self.dict_key_position(&out, k)? {
                        Some(index) => out[index].1 = v.clone(),
                        None => out.push((k.clone(), v.clone())),
                    }
                    Ok(Some(Value::dict(out)))
                }
                _ => err("insert expects a Dict, a key, and a value"),
            },
            name if intrinsics::is_dict_insert_extract(name) => match args {
                [Value::Dict(entries), k, v] => {
                    let mut out = (**entries).clone();
                    let previous = match self.dict_key_position(&out, k)? {
                        Some(index) => {
                            let old = std::mem::replace(&mut out[index].1, v.clone());
                            Value::ctor("Some", vec![old])
                        }
                        None => {
                            out.push((k.clone(), v.clone()));
                            Value::ctor("None", Vec::new())
                        }
                    };
                    Ok(Some(Value::tuple(vec![Value::dict(out), previous])))
                }
                _ => err("insert expects a Dict, a key, and a value"),
            },
            // Value for `k`, or `default` if absent.
            intrinsics::DICT_GET_OR => match args {
                [Value::Dict(entries), k, default] => {
                    let found = self.dict_key_position(entries, k)?;
                    Ok(Some(found.map(|index| entries[index].1.clone()).unwrap_or_else(|| default.clone())))
                }
                _ => err("get_or expects a Dict, a key, and a default value"),
            },
            intrinsics::DICT_AT => match args {
                [Value::Dict(entries), k] => match self.dict_key_position(entries, k)? {
                    Some(index) => Ok(Some(entries[index].1.clone())),
                    None => err(DiagTemplate::DictMissing.render(0, 0, "")),
                },
                _ => err("at expects a Dict and a key"),
            },
            intrinsics::DICT_CONTAINS_KEY => match args {
                [Value::Dict(entries), k] => {
                    Ok(Some(Value::Bool(self.dict_key_position(entries, k)?.is_some())))
                }
                _ => err("has expects a Dict and a key"),
            },
            // A new dict with `k` (and its value) removed; unchanged if absent.
            intrinsics::DICT_REMOVE => match args {
                [Value::Dict(entries), k] => {
                    let mut out = (**entries).clone();
                    if let Some(index) = self.dict_key_position(&out, k)? {
                        out.remove(index);
                    }
                    Ok(Some(Value::dict(out)))
                }
                _ => err("remove expects a Dict and a key"),
            },
            name if intrinsics::is_dict_remove_extract(name) => match args {
                [Value::Dict(entries), k] => {
                    let mut out = (**entries).clone();
                    let previous = match self.dict_key_position(&out, k)? {
                        Some(index) => Value::Ctor {
                            name: "Some".into(),
                            fields: Rc::new(vec![out.remove(index).1]),
                        },
                        None => Value::ctor("None", Vec::new()),
                    };
                    Ok(Some(Value::tuple(vec![Value::dict(out), previous])))
                }
                _ => err("remove expects a Dict and a key"),
            },
            intrinsics::DICT_KEYS => match args {
                [Value::Dict(entries)] => {
                    Ok(Some(Value::list(entries.iter().map(|(k, _)| k.clone()).collect())))
                }
                _ => err("keys expects a Dict"),
            },
            intrinsics::DICT_VALUES => match args {
                [Value::Dict(entries)] => {
                    Ok(Some(Value::list(entries.iter().map(|(_, v)| v.clone()).collect())))
                }
                _ => err("values expects a Dict"),
            },
            // Each entry as a `(key, value)` tuple, in insertion order.
            intrinsics::DICT_PAIRS => match args {
                [Value::Dict(entries)] => Ok(Some(Value::list(
                    entries
                        .iter()
                        .map(|(k, v)| Value::tuple(vec![k.clone(), v.clone()]))
                        .collect(),
                ))),
                _ => err("pairs expects a Dict"),
            },
            intrinsics::DICT_LENGTH => match args {
                [Value::Dict(entries)] => Ok(Some(Value::Int(entries.len() as i64))),
                _ => err("size expects a Dict"),
            },
            intrinsics::TESTING_MOCK_DIR => match args {
                [Value::List(entries)] => {
                    let mut files = BTreeMap::new();
                    for entry in entries.iter() {
                        let Value::Tuple(fields) = entry else {
                            return err("mock_dir entries must be `(String, String)` pairs");
                        };
                        let [Value::Str(path), Value::Str(contents)] = fields.as_slice() else {
                            return err("mock_dir entries must be `(String, String)` pairs");
                        };
                        let path = mock_normalize(path)?;
                        if path.is_empty() {
                            return err("mock Dir entry path must name a file");
                        }
                        files.insert(path, (**contents).clone());
                    }
                    Ok(Some(Value::Dir(
                        DirValue::Mock {
                            root: String::new(),
                            files: Rc::new(files),
                        },
                        String::new(),
                    )))
                }
                _ => err("mock_dir expects a list of `(path, contents)` pairs"),
            },
            // Filesystem capability (cap-std style): attenuate to a subdirectory.
            "subtree" => match args {
                // A subtree inherits the parent's entry policy (refinement is monotone).
                // Opening a sub-directory is a directory traversal (RFC-0011 `kind`): a
                // `files()` policy forbids it, an `ext`/empty policy does not.
                [Value::Dir(base, pol), Value::Str(name)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, name, true) {
                        return err(format!("`{name}` is not permitted by this Dir capability's entry policy"));
                    }
                    Ok(Some(Value::Dir(dir_child_value(base, name)?, pol.clone())))
                }
                _ => err("subtree expects a Dir and a name"),
            },
            // RFC-0012 navigation: a `Dir` opens a confined `File`. `read_file`
            // requires the file to exist; `write_file` allows a not-yet-existing target.
            "read_file" => match args {
                [Value::Dir(base, pol), Value::Str(rel)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    Ok(Some(Value::File(dir_file_value(base, rel, false)?)))
                }
                _ => err("read_file expects a Dir and a relative path"),
            },
            "write_file" => match args {
                [Value::Dir(base, pol), Value::Str(rel)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    Ok(Some(Value::File(dir_file_value(base, rel, true)?)))
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
                [Value::Cap(Capability::Exec), Value::Dir(base, pol), Value::Str(path), Value::Str(joined), Value::Str(stdin)] => {
                    // (RFC-0011) exec is the sharpest right, so it takes the SAME entry-policy
                    // gate as read/write: a `Dir[...].only(...)` may only run a file it admits.
                    if !witchy_caps::capabilities::dir_admits(pol, path, false) {
                        return err(format!("`{path}` is not permitted by this Dir capability's entry policy"));
                    }
                    let DirValue::Fs(base) = base else {
                        return err("exec cannot run programs from an in-memory mock Dir");
                    };
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
                    Ok(Some(Value::str(format!("{code}\n{out}{serr}"))))
                }
                _ => err("exec expects (Exec, Dir, path, args, stdin)"),
            },
            // Read a file relative to a Dir capability (confined to its subtree).
            "read" => match args {
                [Value::Dir(base, pol), Value::Str(rel)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    Ok(Some(Value::str(read_file_value(&dir_file_value(base, rel, false)?)?)))
                }
                // A `File` is already a confined path; read it directly (RFC-0012).
                [Value::File(file)] => Ok(Some(Value::str(read_file_value(file)?))),
                _ => err("read expects a Dir and a relative path, or a File"),
            },
            // Write a file relative to a Dir capability, confined to its subtree
            // (the target may not exist yet, so confinement is checked via its
            // parent directory).
            "write" => match args {
                [Value::Dir(base, pol), Value::Str(rel), Value::Str(contents)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    write_file_value(&dir_file_value(base, rel, true)?, contents)?;
                    Ok(Some(Value::Unit))
                }
                // A `File` is already a confined path; write it directly (RFC-0012).
                [Value::File(file), Value::Str(contents)] => {
                    write_file_value(file, contents)?;
                    Ok(Some(Value::Unit))
                }
                _ => err("write expects a Dir + path + contents, or a File + contents"),
            },
            // Append to a file (creating it if absent) — `write`'s confinement
            // and rights, without clobbering existing contents.
            "append" => match args {
                [Value::Dir(base, pol), Value::Str(rel), Value::Str(contents)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    match dir_file_value(base, rel, true)? {
                        FileValue::Fs(path) => {
                            use std::io::Write as _;
                            let res = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&path)
                                .and_then(|mut f| f.write_all(contents.as_bytes()));
                            match res {
                                Ok(()) => Ok(Some(Value::Unit)),
                                Err(e) => err(format!("append failed for `{}`: {e}", path.display())),
                            }
                        }
                        FileValue::Mock { path, .. } => err(format!(
                            "append failed for mock Dir `{path}`: mock directories are read-only"
                        )),
                    }
                }
                _ => err("append expects a Dir, a relative path, and contents"),
            },
            // Whether a file exists within the Dir capability's subtree — total
            // (never errors), so a path outside the subtree, or a missing file,
            // simply reads as `false`. Lets `read` callers avoid a crash.
            "exists" => match args {
                [Value::Dir(base, _), Value::Str(rel)] => {
                    let ok = match base {
                        DirValue::Fs(base) => resolve(base, rel).map(|p| p.exists()).unwrap_or(false),
                        DirValue::Mock { root, files } => {
                            mock_join(root, rel).map(|path| mock_exists(files, &path)).unwrap_or(false)
                        }
                    };
                    Ok(Some(Value::Bool(ok)))
                }
                _ => err("exists expects a Dir and a relative path"),
            },
            // Whether a path within the Dir capability's subtree is a directory —
            // total (a path outside the subtree or a non-dir reads as `false`), so
            // a caller can walk `src/**` without tripping over a file.
            "is_dir" => match args {
                [Value::Dir(base, _), Value::Str(rel)] => {
                    let ok = match base {
                        DirValue::Fs(base) => resolve(base, rel).map(|p| p.is_dir()).unwrap_or(false),
                        DirValue::Mock { root, files } => {
                            mock_join(root, rel).map(|path| mock_is_dir(files, &path)).unwrap_or(false)
                        }
                    };
                    Ok(Some(Value::Bool(ok)))
                }
                _ => err("is_dir expects a Dir and a relative path"),
            },
            // List the immediate entries of the Dir capability's own directory, as
            // sorted names (deterministic — `read_dir` order is OS-dependent).
            "list" => match args {
                [Value::Dir(base, _)] => {
                    let names: Vec<String> = match base {
                        DirValue::Fs(base) => {
                            let mut names = Vec::new();
                            let entries = std::fs::read_dir(base).map_err(|e| RuntimeError {
                                message: format!("list failed for `{}`: {e}", base.display()),
                            })?;
                            for entry in entries {
                                let entry = match entry {
                                    Ok(entry) => entry,
                                    Err(e) => {
                                        return err(format!("list failed for `{}`: {e}", base.display()));
                                    }
                                };
                                let name = match entry.file_name().into_string() {
                                    Ok(name) => name,
                                    Err(_) => {
                                        return err(format!(
                                            "list failed for `{}`: directory entry name is not valid UTF-8",
                                            base.display()
                                        ));
                                    }
                                };
                                names.push(name);
                            }
                            names.sort();
                            names
                        }
                        DirValue::Mock { root, files } => mock_list(files, root)?,
                    };
                    Ok(Some(Value::list(names.into_iter().map(Value::str).collect())))
                }
                _ => err("list expects a Dir"),
            },
            // Create a subdirectory within the Dir capability's subtree, confined
            // like `write` (idempotent — succeeds if it already exists). Creating a
            // directory is a directory op (RFC-0011 `kind`): a `files()` policy forbids it.
            "make_dir" => match args {
                [Value::Dir(base, pol), Value::Str(name)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, name, true) {
                        return err(format!("`{name}` is not permitted by this Dir capability's entry policy"));
                    }
                    match base {
                        DirValue::Fs(base) => {
                            let path = resolve_write(base, name)?;
                            match std::fs::create_dir_all(&path) {
                                Ok(()) => Ok(Some(Value::Unit)),
                                Err(e) => err(format!("make_dir failed for `{}`: {e}", path.display())),
                            }
                        }
                        DirValue::Mock { root, .. } => {
                            let path = mock_join(root, name)?;
                            err(format!("make_dir failed for mock Dir `{path}`: mock directories are read-only"))
                        }
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
            // Monotonic elapsed nanoseconds since first use — a steady clock for
            // measuring durations (unaffected by wall-clock adjustments). The
            // process-start reference is lazily set on the first call, so a
            // start/stop bracket around a computation yields its elapsed time.
            "now_monotonic" => match args {
                [Value::Cap(Capability::Clock)] => {
                    static START: std::sync::LazyLock<std::time::Instant> =
                        std::sync::LazyLock::new(std::time::Instant::now);
                    Ok(Some(Value::Int(START.elapsed().as_nanos() as i64)))
                }
                _ => err("now_monotonic expects a Clock"),
            },
            // A fresh draw of the `Rand` capability. Seeded (WITCHY_RAND_SEED) it is
            // deterministic and matches the compiled backend bit-for-bit (parity);
            // unseeded the oracle clock-seeds splitmix (the production CSPRNG is the
            // compiled host's getrandom, not this tree-walker).
            "rand_u64" => match args {
                [Value::Cap(Capability::Rand)] => Ok(Some(Value::Int(self.rand_next()))),
                _ => err("rand_u64 expects a Rand"),
            },
            // Read a named environment variable through an `Env` capability:
            // `env.get_env(name) -> Option(String)` (None when unset). Reading the
            // process environment is ambient authority, so it is capability-gated.
            "get_env" => match args {
                [Value::Cap(Capability::Env), Value::Str(name)] => Ok(Some(match std::env::var(name.as_str()) {
                    Ok(v) => Value::ctor("Some", vec![Value::str(v)]),
                    Err(_) => Value::ctor("None", Vec::new()),
                })),
                _ => err("get_env expects an Env and a variable name"),
            },
            // --- build-time host operations (only reachable from a `build` step) ---
            // Write generated source into the confined per-rune output sandbox.
            "write_out" => match args {
                [Value::Build(BuildCap::Out(base)), Value::Str(rel), Value::Str(contents)] => {
                    let path = resolve_write(base, rel)?;
                    match std::fs::write(&path, contents.as_bytes()) {
                        Ok(()) => Ok(Some(Value::Unit)),
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
                                Ok(contents) => return Ok(Some(Value::str(contents))),
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
                [Value::Build(BuildCap::Env(env)), Value::Str(name)] => {
                    let value = env.get(name.as_str()).ok_or_else(|| RuntimeError {
                        message: format!(
                            "get_build_env: `{name}` is not in this BuildEnv grant's allow-list"
                        ),
                    })?;
                    Ok(Some(match value {
                        Some(v) => Value::ctor("Some", vec![Value::str(v.as_str())]),
                        None => Value::ctor("None", Vec::new()),
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
                    if !allow.iter().any(|h| *h == **host) {
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
                    Ok(Some(Value::str(body)))
                }
                _ => err("fetch_build expects a BuildNet, a host, and a path"),
            },
            // Invoke an external tool — but only one named on the BuildExec grant's
            // allow-list. `input` is fed on stdin; stdout is returned. This is the
            // "native toolchain escape hatch" (§7.1): the allow-list is the
            // confinement, since the tool itself runs as a native process.
            "run_tool" => match args {
                [Value::Build(BuildCap::Exec(allow)), Value::Str(tool), Value::Str(input)] => {
                    if !allow.iter().any(|t| *t == **tool) {
                        return err(format!(
                            "run_tool: `{tool}` is not in this BuildExec grant's allow-list"
                        ));
                    }
                    use std::io::Write;
                    use std::process::{Command, Stdio};
                    let mut child = Command::new(tool.as_str())
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
                    Ok(Some(Value::str(String::from_utf8_lossy(&out.stdout).into_owned())))
                }
                _ => err("run_tool expects a BuildExec, a tool name, and input"),
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
                // RFC-0011: `dir.only(DirPolicy)` narrows the Dir's entry policy.
                [Value::Dir(base, pol), Value::Ctor { fields, .. }] if fields.len() == 1 => {
                    let Value::Str(refine) = &fields[0] else {
                        return err("only expects a DirPolicy");
                    };
                    Ok(Some(Value::Dir(
                        base.clone(),
                        witchy_caps::capabilities::dir_only(pol, refine),
                    )))
                }
                _ => err("only expects a Net and a NetPolicy, or a Dir and a DirPolicy"),
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
                            Value::ctor("Some", vec![Value::Socket(id)])
                        }
                        Err(_) => Value::ctor("None", Vec::new()),
                    };
                    Ok(Some(v))
                }
                _ => err("try_connect expects a Net and an address"),
            },
            // (RFC-0020) Resolve a hostname to its current IP literals. No allowlist
            // filtering — the program inspects the IPs and `connect_pinned` re-checks
            // the chosen one, so resolve adds no authority beyond `connect`. An empty
            // list signals a resolution failure (the std wrapper turns it into `Err`).
            "resolve" => match args {
                [Value::Net(_allow), Value::Str(host)] => {
                    let ips = witchy_runtime::net::resolve_ips(host);
                    Ok(Some(Value::list(ips.into_iter().map(Value::str).collect())))
                }
                _ => err("resolve expects a Net and a host"),
            },
            // (RFC-0020) Dial the EXACT `ip:port` — no DNS — while presenting `host` as
            // the TLS SNI / `Host`. The Net allowlist is still enforced on `ip` (a literal
            // IP resolves to itself), so a pin can never exceed the capability. This is
            // what closes the DNS-rebinding TOCTOU: the checked IP is the dialed IP.
            "connect_pinned" => match args {
                [Value::Net(allow), Value::Str(ip), Value::Str(host), Value::Int(port), Value::Bool(secure)] => {
                    let ip_port = witchy_runtime::net::authority(ip, *port);
                    let targets = match witchy_caps::capabilities::resolve_admitted(allow, &ip_port) {
                        Ok(t) => t,
                        Err(e) => return err(format!("connect_pinned: {e}")),
                    };
                    let host_port = witchy_runtime::net::authority(host, *port);
                    match witchy_runtime::net::dial(&targets, *secure, &host_port) {
                        Ok(stream) => {
                            let id = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            Ok(Some(Value::Socket(id)))
                        }
                        Err(e) => err(format!("connect_pinned to `{ip_port}` failed: {e}")),
                    }
                }
                _ => err("connect_pinned expects (Net, ip, host, port, secure)"),
            },
            "try_connect_pinned" => match args {
                [Value::Net(allow), Value::Str(ip), Value::Str(host), Value::Int(port), Value::Bool(secure)] => {
                    let ip_port = witchy_runtime::net::authority(ip, *port);
                    // A capability breach still traps; only a transient dial failure -> None.
                    let targets = match witchy_caps::capabilities::resolve_admitted(allow, &ip_port) {
                        Ok(t) => t,
                        Err(e) => return err(format!("try_connect_pinned: {e}")),
                    };
                    let host_port = witchy_runtime::net::authority(host, *port);
                    let v = match witchy_runtime::net::dial(&targets, *secure, &host_port) {
                        Ok(stream) => {
                            let id = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            Value::ctor("Some", vec![Value::Socket(id)])
                        }
                        Err(_) => Value::ctor("None", Vec::new()),
                    };
                    Ok(Some(v))
                }
                _ => err("try_connect_pinned expects (Net, ip, host, port, secure)"),
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
                    Ok(Some(Value::Unit))
                }
                _ => err("send_line expects a Socket and a String"),
            },
            "recv_line" => match args {
                [Value::Socket(id)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    // (SEC-035) Shared, bounded read so a peer that never sends a newline
                    // can't OOM the host — same cap + logic as the compiled backend.
                    let raw = witchy_runtime::net::read_line_capped(sock)
                        .map_err(|e| RuntimeError { message: e.to_string() })?;
                    let line = String::from_utf8_lossy(&raw);
                    Ok(Some(Value::str(line.trim_end_matches('\n'))))
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
                    Ok(Some(Value::Unit))
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
                    // (SEC-035) Cap the read so a peer streaming without EOF can't OOM the
                    // host — one byte past the cap detects overflow; same as the compiled side.
                    use std::io::Read;
                    let mut buf = Vec::new();
                    sock.by_ref()
                        .take(witchy_runtime::net::MAX_RECV_BYTES + 1)
                        .read_to_end(&mut buf)
                        .map_err(|e| RuntimeError { message: format!("recv failed: {e}") })?;
                    if buf.len() as u64 > witchy_runtime::net::MAX_RECV_BYTES {
                        return Err(RuntimeError {
                            message: format!(
                                "recv_all exceeded the {}-byte cap",
                                witchy_runtime::net::MAX_RECV_BYTES
                            ),
                        });
                    }
                    Ok(Some(Value::str(String::from_utf8_lossy(&buf).into_owned())))
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
                    // `want` is attacker-controlled (an HTTP Content-Length, up to i64::MAX);
                    // do NOT pre-allocate `vec![0u8; want]` — a peer that sends a huge count
                    // but few bytes would OOM the host before a single byte arrives. Read in
                    // bounded chunks so memory tracks bytes actually received, matching the
                    // compiled runtime (`host_net_recv_bytes_len`). (BUG-065)
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    while buf.len() < want {
                        let to_read = (want - buf.len()).min(chunk.len());
                        match sock.read(&mut chunk[..to_read]) {
                            Ok(0) => break,
                            Ok(k) => buf.extend_from_slice(&chunk[..k]),
                            Err(e) => return err(format!("recv failed: {e}")),
                        }
                    }
                    Ok(Some(Value::str(String::from_utf8_lossy(&buf).into_owned())))
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
                    match TcpListener::bind(addr.as_str()) {
                        Ok(listener) => {
                            let id = self.listeners.len();
                            self.listeners.push((listener, None));
                            Ok(Some(Value::Listener(id)))
                        }
                        Err(e) => err(format!("listen on `{addr}` failed: {e}")),
                    }
                }
                _ => err("listen expects a Net and an address"),
            },
            // (RFC-0060) Bind an HTTPS listener. The rustls config is built ONCE here
            // — through the SAME shared module the compiled runtime uses — from the
            // certificate PEM and the key `Secret`'s host-side bytes; malformed or
            // mismatched material is a loud listen-time error. Accepts handshake
            // host-side and yield ordinary `Socket`s.
            "listen_tls" => match args {
                [Value::Net(allow), Value::Str(addr), Value::Str(cert_pem), Value::Secret(key_bytes, _)] => {
                    if !witchy_caps::capabilities::net_allows(allow, addr) {
                        return err(format!("listen: `{addr}` is not permitted by this Net capability"));
                    }
                    let config = match witchy_runtime::net::server_tls_config(cert_pem, key_bytes) {
                        Ok(config) => config,
                        Err(message) => return err(message),
                    };
                    match TcpListener::bind(addr.as_str()) {
                        Ok(listener) => {
                            let id = self.listeners.len();
                            self.listeners.push((listener, Some(config)));
                            Ok(Some(Value::Listener(id)))
                        }
                        Err(e) => err(format!("listen on `{addr}` failed: {e}")),
                    }
                }
                _ => err("listen_tls expects a Net, an address, a certificate PEM, and a Secret key"),
            },
            // Block until a client connects, returning the connection `Socket`. On a
            // TLS listener the handshake completes host-side first; a failed handshake
            // (plaintext client, bad ClientHello) drops that connection and keeps
            // accepting — connection weather, not a program error (RFC-0060).
            "accept" => match args {
                [Value::Listener(id)] => loop {
                    let (listener, tls) = self
                        .listeners
                        .get(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid listener".into() })?;
                    match listener.accept() {
                        Ok((stream, _peer)) => {
                            let stream: Box<dyn Stream> = match tls {
                                None => Box::new(stream),
                                Some(config) => {
                                    match witchy_runtime::net::accept_tls(config.clone(), stream) {
                                        Ok(tls_stream) => tls_stream,
                                        Err(_) => continue,
                                    }
                                }
                            };
                            let sid = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            return Ok(Some(Value::Socket(sid)));
                        }
                        Err(e) => return err(format!("accept failed: {e}")),
                    }
                },
                _ => err("accept expects a Listener"),
            },
            // (RFC-0032) The compiled runtime's `serve_pool` spawns one worker VM per
            // core sharing the bound listener. The interpreter is a single VM (the
            // parity oracle), so the pool is the identity here: `serve`/`serve_tls`
            // fall through to their own accept loop, single-core — the same observable
            // request/response behavior, minus the scale-out.
            "serve_pool" => match args {
                [Value::Listener(_)] => Ok(Some(Value::Unit)),
                _ => err("serve_pool expects a Listener"),
            },
            // Close a connected socket (e.g. after sending a `Connection: close`
            // response). Idempotent; an already-closed socket is not an error.
            "close" => match args {
                [Value::Socket(id)] => {
                    if let Some(sock) = self.sockets.get_mut(*id) {
                        sock.get_mut().shutdown();
                    }
                    Ok(Some(Value::Unit))
                }
                _ => err("close expects a Socket"),
            },
            _ if catalog.is_some_and(|spec| {
                spec.runtime == intrinsics::IntrinsicRuntime::InterpreterBuiltin
            }) => err(format!(
                "internal error: cataloged interpreter builtin `{name}` has no dispatch arm"
            )),
            _ => Ok(None),
        }
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
            Expr::ExistentialPack { .. } => err(
                "internal: RFC-0081 existential node reached the interpreter before runtime witness lowering",
            ),
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
                // (RFC-0047) `==`/`!=` desugar through PartialEq at every depth: if
                // ANY type in the program has a custom (non-derived) `eq` impl, walk
                // the two values structurally and call that impl wherever a value of
                // such a type is reached. With no custom impls (the common case) the
                // walk never fires and equality is the plain structural `l == r`.
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

fn match_pattern(pat: &Pattern, value: &Value, env: &mut Env) -> bool {
    match (pat, value) {
        (Pattern::Wildcard, _) => true,
        (Pattern::Var(name), v) => {
            env.define(Rc::from(name.as_str()), v.clone(), false);
            true
        }
        (Pattern::Ctor { name, args }, Value::Unit) if name == "Nil" && args.is_empty() => true,
        (Pattern::Tuple(pats), Value::Unit) if pats.is_empty() => true,
        (Pattern::Int(a), Value::Int(b)) => a == b,
        (Pattern::Str(a), Value::Str(b)) => *a == **b,
        (Pattern::Bool(a), Value::Bool(b)) => a == b,
        // A Duration literal pattern is carried as whole milliseconds, and a
        // Duration value is an `Int` of milliseconds (Expr::Duration -> Value::Int),
        // so it is exact i64 equality — no float hazard.
        (Pattern::Duration(a), Value::Int(b)) => a == b,
        // `lo..hi` (half-open) / `lo..=hi` (inclusive) against an Int.
        (Pattern::IntRange { lo, hi, inclusive }, Value::Int(b)) => {
            *b >= *lo && (if *inclusive { *b <= *hi } else { *b < *hi })
        }
        // Every alternative binds the same names (checker-enforced), so binding
        // through the first that matches is well-defined.
        (Pattern::Or(alts), v) => alts.iter().any(|p| match_pattern(p, v, env)),
        (Pattern::Ctor { name, args }, Value::Ctor { name: vname, fields }) => {
            name.as_str() == &**vname
                && args.len() == fields.len()
                && args
                    .iter()
                    .zip(fields.iter())
                    .all(|(p, v)| match_pattern(p, v, env))
        }
        (Pattern::AnonCtor { tag, args }, Value::Ctor { name: vname, fields }) => {
            &**vname == format!(".{tag}").as_str()
                && args.len() == fields.len()
                && args
                    .iter()
                    .zip(fields.iter())
                    .all(|(p, v)| match_pattern(p, v, env))
        }
        (Pattern::Tuple(pats), Value::Tuple(items)) => {
            pats.len() == items.len()
                && pats
                    .iter()
                    .zip(items.iter())
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
                .zip(items.iter())
                .all(|(p, v)| match_pattern(p, v, env))
            {
                return false;
            }
            if let Some(Some(name)) = rest {
                let tail = items[elems.len()..].to_vec();
                env.define(Rc::from(name.as_str()), Value::list(tail), false);
            }
            true
        }
        _ => false,
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
            (Add, Str(a), Str(b)) => Ok(Value::str(format!("{a}{b}"))),
            (Add, Int(a), Int(b)) => Ok(Int(a.wrapping_add(b))),
            (Sub, Int(a), Int(b)) => Ok(Int(a.wrapping_sub(b))),
            (Mul, Int(a), Int(b)) => Ok(Int(a.wrapping_mul(b))),
            (Div, Int(_), Int(0)) => err(DiagTemplate::DivisionByZero.render(0, 0, "")),
            (Div, Int(a), Int(b)) => a.checked_div(b).map(Int).ok_or_else(|| RuntimeError {
                message: DiagTemplate::DivisionOverflow.render(0, 0, ""),
            }),
            (Add, Float(a), Float(b)) => Ok(Float(a + b)),
            (Sub, Float(a), Float(b)) => Ok(Float(a - b)),
            (Mul, Float(a), Float(b)) => Ok(Float(a * b)),
            (Div, Float(a), Float(b)) => Ok(Float(a / b)),
            (_, a, b) => err(format!("cannot apply arithmetic to `{a}` and `{b}`")),
        },
        Mod => match (l, r) {
            (Int(_), Int(0)) => err(DiagTemplate::ModuloByZero.render(0, 0, "")),
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
            (Str(a), Str(b)) => {
                // Reuse `a`'s buffer when this value is unshared (the string
                // accumulation fast path); copy-on-write otherwise.
                let mut out = Rc::try_unwrap(a).unwrap_or_else(|rc| (*rc).clone());
                out.push_str(&b);
                Ok(Str(Rc::new(out)))
            }
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
        And | Or | Coalesce => unreachable!("&&/||/?? are short-circuited in eval"),
    }
}

fn compare(l: &Value, r: &Value) -> Result<std::cmp::Ordering, RuntimeError> {
    use Value::*;
    match (l, r) {
        (Int(a), Int(b)) => Ok(a.cmp(b)),
        (Float(a), Float(b)) => a
            .partial_cmp(b)
            .ok_or_else(|| RuntimeError { message: DiagTemplate::NanOrder.render(0, 0, "") }),
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
// stubs), so they only ever receive the simple shapes `NativeValue` carries; any
// other `Value` is a caller bug surfaced as a runtime error.
fn value_to_native(v: &Value) -> Result<witchy_runtime::value::NativeValue, RuntimeError> {
    use witchy_runtime::value::NativeValue as N;
    Ok(match v {
        Value::Int(i) => N::Int(*i),
        Value::Str(s) => N::Str((**s).clone()),
        Value::Bytes(b) => N::Bytes(b.clone()),
        Value::Bool(b) => N::Bool(*b),
        Value::List(xs) => N::List(
            xs.iter().map(value_to_native).collect::<Result<Vec<_>, RuntimeError>>()?,
        ),
        // The native crypto op already passed the reveal gate (use-only is checked
        // before dispatch), so the raw bytes cross without the flag.
        Value::Secret(s, _) => N::Secret(s.clone()),
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
        N::Str(s) => Value::str(s),
        N::Bytes(b) => Value::Bytes(b),
        N::Bool(b) => Value::Bool(b),
        N::List(xs) => Value::list(xs.into_iter().map(native_to_value).collect()),
        // A secret produced by a native op (none do today) is revealable by default.
        N::Secret(s) => Value::Secret(s, false),
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
    run_module_budgeted_in_scope(module, root, step_limit, None)
}

pub(crate) fn run_module_budgeted_in_scope(
    module: Module,
    root: impl AsRef<Path>,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
) -> Result<Vec<String>, RuntimeError> {
    run_comptime_module_budgeted_in_scope(module, root, step_limit, fresh_ident_scope)
        .map(|(output, _)| output)
}

pub(crate) fn run_comptime_module_budgeted_in_scope(
    module: Module,
    root: impl AsRef<Path>,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
) -> Result<(Vec<String>, Vec<PositionedComptimeItem>), RuntimeError> {
    run_comptime_module_outputs_budgeted_in_scope(
        module,
        root,
        step_limit,
        fresh_ident_scope,
    )
    .and_then(|outputs| {
        if !outputs.exprs.is_empty() {
            return err("expression output is valid only during tagged-literal expansion");
        }
        Ok((outputs.output, outputs.items))
    })
}

pub(crate) fn run_comptime_module_outputs_budgeted_in_scope(
    module: Module,
    root: impl AsRef<Path>,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
) -> Result<ComptimeOutputs, RuntimeError> {
    let module = witchy_syntax::records::lower(module).map_err(|message| RuntimeError { message })?;
    let module = witchy_types::traits::lower(module);
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited(module, root, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, Vec::new(), UserCapGrants::new(), step_limit, fresh_ident_scope)
    })
    .map(|outcome| ComptimeOutputs {
        output: outcome.output,
        items: outcome.comptime_items,
        exprs: outcome.comptime_exprs,
    })
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
        run_module_inner_limited(module, root, Vec::new(), file_grants, Vec::new(), Vec::new(), None, Vec::new(), UserCapGrants::new(), DEFAULT_STEP_LIMIT, None)
    })
    .map(|outcome| outcome.output)
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
        .map(|outcome| (outcome.output, outcome.exit_code))
}

/// Like [`run_module_exit`], but also grants NAMED secrets to a `main` that binds
/// a `SecretStore` — each `(name, bytes, use_only)`; a use-only secret (RFC-0060)
/// is consumable by handle (`crypto.sign`, `server.serve_tls`) but `crypto.reveal`
/// on it errors. This is the interpreter twin of the compiled runtime's
/// `Capabilities.secrets`, for differential tests of secret-consuming servers.
pub fn run_module_exit_secrets(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<(String, Vec<u8>, bool)>,
) -> Result<(Vec<String>, i32), RuntimeError> {
    let module = witchy_syntax::records::lower(module).map_err(|message| RuntimeError { message })?;
    let module = witchy_types::traits::lower(module);
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited(
            module,
            root,
            Vec::new(),
            Vec::new(),
            net_allow,
            args,
            signing_key,
            named_secrets,
            UserCapGrants::new(),
            DEFAULT_STEP_LIMIT,
            None,
        )
    })
    .map(|outcome| (outcome.output, outcome.exit_code))
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
        .map(|outcome| (outcome.output, outcome.exit_code))
}

/// Parse and run `src` with several `Dir` grants (the multi-`Dir` analog of
/// [`run_in`]); test/CLI helper for [`run_module_exit_dirs`].
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
fn run_on_deep_stack<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, RuntimeError> + Send + 'static,
) -> Result<T, RuntimeError> {
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
fn run_on_deep_stack<T>(
    f: impl FnOnce() -> Result<T, RuntimeError>,
) -> Result<T, RuntimeError> {
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

/// Run a `main` that binds bare grantable capabilities (RFC-0038): each grantable
/// parameter mints a sealed record from `user_caps` (parameter name → field
/// values). The oracle for `witchy sandbox --grants` with a `[user_caps]` section.
pub fn run_module_user_caps(
    module: Module,
    root: impl AsRef<Path>,
    net_allow: Vec<String>,
    args: Vec<String>,
    file_grants: Vec<PathBuf>,
    user_caps: UserCapGrants,
) -> Result<Vec<String>, RuntimeError> {
    let module = witchy_syntax::records::lower(module).map_err(|message| RuntimeError { message })?;
    let module = witchy_types::traits::lower(module);
    let root = root.as_ref().to_path_buf();
    run_on_deep_stack(move || {
        run_module_inner_limited(module, root, Vec::new(), file_grants, net_allow, args, None, Vec::new(), user_caps, DEFAULT_STEP_LIMIT, None)
    })
    .map(|outcome| outcome.output)
}

fn run_module_inner(
    module: Module,
    root: PathBuf,
    dir_roots: Vec<PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<InterpreterOutcome, RuntimeError> {
    run_module_inner_limited(module, root, dir_roots, Vec::new(), net_allow, args, signing_key, Vec::new(), UserCapGrants::new(), DEFAULT_STEP_LIMIT, None)
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
    named_secrets: Vec<(String, Vec<u8>, bool)>,
    user_caps: UserCapGrants,
    step_limit: u64,
    fresh_ident_scope: Option<String>,
) -> Result<InterpreterOutcome, RuntimeError> {
    let mut interp = Interpreter::new(module);
    interp.step_limit = step_limit;
    interp.fresh_ident_scope = fresh_ident_scope;
    interp.root = root;
    interp.dir_roots = dir_roots;
    interp.file_grants = file_grants;
    interp.net_allow = net_allow;
    interp.signing_key = signing_key;
    interp.user_cap_grants = user_caps;
    // The signing key is the `signing` secret in the store, so a program may take
    // either a `Secret` (the key directly) or a `SecretStore` and `get("signing")`.
    if let Some(seed) = signing_key {
        // The signing key is never revealable, but its non-revealability is enforced
        // by the signing-key identity check, so it is stored use_only=false.
        interp.secrets.insert("signing".to_string(), (seed.to_vec(), false));
    }
    // Named `--secret`/`--secret-file` grants, each `(name, bytes, use_only)` —
    // a use-only secret (RFC-0060) is consumable by handle; `crypto.reveal` errors.
    for (name, bytes, use_only) in named_secrets {
        interp.secrets.insert(name, (bytes, use_only));
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
                    vals.push(Value::list(args.iter().map(Value::str).collect()));
                } else if matches!(&p.ty, Some(Type::Named(n, _)) if n == "Dir") {
                    let r = if dir_idx == 0 {
                        interp.root.clone()
                    } else {
                        interp.dir_roots.get(dir_idx - 1).cloned().unwrap_or_else(|| interp.root.clone())
                    };
                    dir_idx += 1;
                    vals.push(Value::Dir(DirValue::Fs(r), String::new()));
                } else if matches!(&p.ty, Some(Type::Named(n, _)) if n == "File") {
                    // The i-th `File` param maps to the i-th `--file` grant (RFC-0012).
                    let path = interp.file_grants.get(file_idx).cloned().ok_or_else(|| RuntimeError {
                        message: "`main` requires a `File`, but the host granted none (provide `--file <path>`)".into(),
                    })?;
                    file_idx += 1;
                    vals.push(Value::File(FileValue::Fs(path)));
                } else if let Some(Type::Named(tn, _)) = &p.ty {
                    // RFC-0038: a record-typed `main` param is a bare grantable cap
                    // (typeck guarantees it); mint the sealed record from the grant.
                    // Any other named type falls through to the host-cap minter.
                    if interp.record_fields.contains_key(tn) {
                        vals.push(interp.mint_user_cap(&p.name, tn)?);
                    } else {
                        vals.push(interp.root_cap_for(&p.ty)?);
                    }
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
    if interp.output.is_empty() && !matches!(ret, Value::Unit | Value::Int(_)) {
        interp.output.push(format!("{ret}"));
    }
    Ok(InterpreterOutcome {
        output: interp.output,
        exit_code,
        comptime_items: interp.comptime_item_output,
        comptime_exprs: interp.comptime_expr_output,
    })
}

/// The attenuated grants a build step runs under: a confined output directory
/// (always present — it is `BuildOut`), an optional confined read root, and
/// an immutable env snapshot, and allow-lists for the net/exec caps. Safe by
/// default — anything not granted here cannot be minted, so a build step
/// demanding it fails before running.
#[derive(Debug, Clone, Default)]
pub struct BuildGrants {
    pub out_dir: PathBuf,
    pub read_roots: Vec<PathBuf>,
    /// Granted environment values captured by the host before the build starts.
    /// Map membership is the allow-list; `None` represents an allowed but unset
    /// variable. The interpreter never consults mutable process-global env state.
    pub env: BTreeMap<String, Option<String>>,
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
    let module = witchy_types::traits::lower(module);
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
