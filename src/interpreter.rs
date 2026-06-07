//! Tree-walking evaluator for witchy.
//!
//! This is a semantics prototype: it runs witchy programs directly in the host
//! so we can iterate on language behaviour. Compiling to WASM actors on the
//! proven runtime is a later phase.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use crate::ast::*;
use crate::parser::parse_module;

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
    /// A handle to an actor — the authority to send it messages.
    Subject(usize),
    /// An unforgeable capability to a directory subtree (cap-std `Dir` style).
    /// Carries the host path it is rooted at; can only be obtained from the root
    /// grant or by attenuation (`subdir`).
    Dir(PathBuf),
    /// A network capability: an allow-list of permitted `host:port` destinations
    /// (wasi:sockets / cap-std-net style). Attenuable via `restrict`.
    Net(Vec<String>),
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
    Nil,
}

/// Capabilities are unforgeable: no witchy expression can construct one. They
/// enter a program only at `main` (the root grant) and propagate solely by
/// being passed as arguments. This is the hybrid capability model — a function
/// that needs to perform an effect must be handed the capability for it, so a
/// library that was never granted one cannot perform that effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Console,
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) => write!(f, "{x}"),
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
            Value::Subject(id) => write!(f, "<actor #{id}>"),
            Value::Dir(_) => write!(f, "<dir>"),
            Value::Net(_) => write!(f, "<net>"),
            Value::Socket(id) => write!(f, "<socket #{id}>"),
            Value::Listener(id) => write!(f, "<listener #{id}>"),
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
    /// the binding is mutable (`var`/`inout`/`sink`) or not (`let`). Scopes are
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
}

/// A live actor: which definition it is, plus its current state (fields).
struct ActorInstance {
    def: String,
    state: HashMap<String, Value>,
}

pub struct Interpreter {
    // `Rc` so a call clones a pointer, not the whole function AST (this is the
    // hot path for recursion).
    functions: HashMap<String, Rc<Function>>,
    actor_defs: HashMap<String, ActorDef>,
    actors: Vec<ActorInstance>,
    queue: VecDeque<(usize, Value)>,
    /// Host directory the root `Dir` capability is rooted at.
    root: PathBuf,
    /// Allow-list backing the root `Net` capability.
    net_allow: Vec<String>,
    /// Open sockets, indexed by `Value::Socket` handle.
    sockets: Vec<BufReader<TcpStream>>,
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
const DEFAULT_STEP_LIMIT: u64 = 100_000_000;

impl Interpreter {
    pub fn new(module: Module) -> Self {
        let mut functions = HashMap::new();
        let mut actor_defs = HashMap::new();
        let mut record_fields = HashMap::new();
        for item in module.items {
            match item {
                Item::Function(f) => {
                    functions.insert(f.name.clone(), Rc::new(f));
                }
                Item::Actor(a) => {
                    actor_defs.insert(a.name.clone(), a);
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
                // constants are inlined by `crate::consts`.
                Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } => {}
            }
        }
        Self {
            functions,
            actor_defs,
            actors: Vec::new(),
            queue: VecDeque::new(),
            root: PathBuf::from("."),
            net_allow: Vec::new(),
            sockets: Vec::new(),
            listeners: Vec::new(),
            record_fields,
            steps: 0,
            step_limit: DEFAULT_STEP_LIMIT,
            cur_line: 0,
            cur_fn: String::new(),
            depth: 0,
            depth_limit: DEFAULT_DEPTH_LIMIT,
            output: Vec::new(),
        }
    }

    /// Mint the root capability for a `main` parameter of the given type. This
    /// is where authority enters the program — `main` is the root actor.
    fn root_cap_for(&self, ty: &Option<Type>) -> Result<Value, RuntimeError> {
        match ty {
            Some(Type::Named(n, _)) if n == "Console" => Ok(Value::Cap(Capability::Console)),
            Some(Type::Named(n, _)) if n == "Dir" => Ok(Value::Dir(self.root.clone())),
            Some(Type::Named(n, _)) if n == "Net" => Ok(Value::Net(self.net_allow.clone())),
            other => err(format!(
                "`main` may only declare capability parameters (Console, Dir, Net); got `{other:?}`"
            )),
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
                !matches!(param.convention, Convention::Let),
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
            cenv.define(p.name.clone(), v, !matches!(p.convention, Convention::Let));
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
    /// `inout` arguments must be mutable variables and are written back after
    /// the call returns (Hylo-style move-in / move-out).
    fn eval_call(&mut self, name: &str, args: &[Expr], env: &mut Env) -> Result<Value, Flow> {
        let argvals = args
            .iter()
            .map(|a| self.eval(a, env))
            .collect::<Result<Vec<_>, _>>()?;
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
                !matches!(param.convention, Convention::Let),
            );
            if matches!(param.convention, Convention::Inout) {
                match &args[i] {
                    Expr::Var(caller) => writebacks.push((caller.clone(), param.name.clone())),
                    _ => {
                        return err(format!(
                            "`inout` argument to `{name}` must be a mutable variable"
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
                        "`inout` argument `{caller}` must be a `var` (it is immutable)"
                    ))
                }
                Assign::Unbound => {
                    return err(format!("`inout` argument `{caller}` must be a local variable"))
                }
            }
        }
        self.cur_fn = prev;
        Ok(result)
    }

    fn call_builtin(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, RuntimeError> {
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
                    // printed string ends in `\n` (e.g. `s <> "\n"`).
                    self.output.push(msg.to_string().trim_end_matches('\n').to_string());
                    Ok(Some(Value::Nil))
                }
                [_, _] => err("print requires a Console capability as its first argument"),
                _ => err("print expects a Console capability and a message: print(console, msg)"),
            },
            // Deliver a message to an actor. Holding the Subject IS the
            // authority to send to it.
            "send" => match args {
                [Value::Subject(id), msg] => {
                    self.queue.push_back((*id, msg.clone()));
                    Ok(Some(Value::Nil))
                }
                _ => err("send expects an actor subject and a message: send(actor, Msg(..))"),
            },
            // Pure builtins need no capability.
            "to_string" => Ok(Some(Value::Str(one(args)?.to_string()))),
            "int_to_string" => match one(args)? {
                Value::Int(n) => Ok(Some(Value::Str(n.to_string()))),
                other => err(format!("int_to_string expects an Int, got `{other}`")),
            },
            // String stdlib.
            "string_length" => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Int(s.len() as i64))),
                other => err(format!("string_length expects a String, got `{other}`")),
            },
            // The number of Unicode scalars — the character count, as opposed to
            // `string_length`'s byte count (they agree for ASCII).
            "char_count" => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Int(s.chars().count() as i64))),
                other => err(format!("char_count expects a String, got `{other}`")),
            },
            "to_upper" => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Str(s.to_uppercase()))),
                other => err(format!("to_upper expects a String, got `{other}`")),
            },
            "to_lower" => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Str(s.to_lowercase()))),
                other => err(format!("to_lower expects a String, got `{other}`")),
            },
            "trim" => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Str(s.trim().to_string()))),
                other => err(format!("trim expects a String, got `{other}`")),
            },
            "starts_with" => match args {
                [Value::Str(s), Value::Str(prefix)] => {
                    Ok(Some(Value::Bool(s.starts_with(prefix.as_str()))))
                }
                _ => err("starts_with expects two Strings"),
            },
            "contains" => match args {
                [Value::Str(s), Value::Str(sub)] => {
                    Ok(Some(Value::Bool(s.contains(sub.as_str()))))
                }
                _ => err("contains expects two Strings"),
            },
            // Split on a separator into a list of pieces (the separator itself is
            // dropped); the empty separator yields the whole string unchanged.
            "split" => match args {
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
            "replace" => match args {
                [Value::Str(s), Value::Str(from), Value::Str(to)] => {
                    Ok(Some(Value::Str(s.replace(from.as_str(), to.as_str()))))
                }
                _ => err("replace expects three Strings"),
            },
            "ends_with" => match args {
                [Value::Str(s), Value::Str(suffix)] => {
                    Ok(Some(Value::Bool(s.ends_with(suffix.as_str()))))
                }
                _ => err("ends_with expects two Strings"),
            },
            // Char index of the first occurrence of `sub`, or -1 if absent.
            "index_of" => match args {
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
            "substring" => match args {
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
            "int_to_float" => match one(args)? {
                Value::Int(n) => Ok(Some(Value::Float(n as f64))),
                other => err(format!("int_to_float expects an Int, got `{other}`")),
            },
            "float_to_int" => match one(args)? {
                Value::Float(x) => Ok(Some(Value::Int(x as i64))),
                other => err(format!("float_to_int expects a Float, got `{other}`")),
            },
            // Duration <-> Int(ms): a Duration is an Int(ms) at runtime, so both
            // directions are the identity.
            "int_to_duration" | "duration_to_int" => match one(args)? {
                Value::Int(n) => Ok(Some(Value::Int(n))),
                other => err(format!("{name} expects an Int/Duration, got `{other}`")),
            },
            "sqrt" => match one(args)? {
                Value::Float(x) => Ok(Some(Value::Float(x.sqrt()))),
                other => err(format!("sqrt expects a Float, got `{other}`")),
            },
            "string_to_int" => match one(args)? {
                Value::Str(s) => match s.trim().parse::<i64>() {
                    Ok(n) => Ok(Some(Value::Int(n))),
                    Err(_) => err(format!("cannot parse `{s}` as an Int")),
                },
                other => err(format!("string_to_int expects a String, got `{other}`")),
            },
            "length" => match args {
                [Value::List(items)] => Ok(Some(Value::Int(items.len() as i64))),
                _ => err("length expects a list"),
            },
            "at" => match args {
                [Value::List(items), Value::Int(i)] => match items.get(*i as usize) {
                    Some(v) => Ok(Some(v.clone())),
                    None => err(format!("list index {i} out of bounds (length {})", items.len())),
                },
                _ => err("at expects a list and an Int index"),
            },
            // Return a new list with `x` appended (lists are values, so this does
            // not mutate the original).
            "push" => match args {
                [Value::List(items), x] => {
                    let mut out = items.clone();
                    out.push(x.clone());
                    Ok(Some(Value::List(out)))
                }
                _ => err("push expects a list and a value"),
            },
            // Return a new list that is the two given lists joined.
            "concat" => match args {
                [Value::List(a), Value::List(b)] => {
                    let mut out = a.clone();
                    out.extend(b.clone());
                    Ok(Some(Value::List(out)))
                }
                _ => err("concat expects two lists"),
            },
            // --- Dict: an immutable association map ---
            "dict_new" => match args {
                [] => Ok(Some(Value::Dict(Vec::new()))),
                _ => err("dict_new takes no arguments"),
            },
            // Return a new dict with `k` set to `v` (replacing any existing entry).
            "insert" => match args {
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
            "get_or" => match args {
                [Value::Dict(entries), k, default] => {
                    let found = entries.iter().find(|(ek, _)| ek == k);
                    Ok(Some(found.map(|(_, v)| v.clone()).unwrap_or_else(|| default.clone())))
                }
                _ => err("get_or expects a Dict, a key, and a default value"),
            },
            "has" => match args {
                [Value::Dict(entries), k] => {
                    Ok(Some(Value::Bool(entries.iter().any(|(ek, _)| ek == k))))
                }
                _ => err("has expects a Dict and a key"),
            },
            // A new dict with `k` (and its value) removed; unchanged if absent.
            "remove" => match args {
                [Value::Dict(entries), k] => {
                    let out: Vec<(Value, Value)> =
                        entries.iter().filter(|(ek, _)| ek != k).cloned().collect();
                    Ok(Some(Value::Dict(out)))
                }
                _ => err("remove expects a Dict and a key"),
            },
            "keys" => match args {
                [Value::Dict(entries)] => {
                    Ok(Some(Value::List(entries.iter().map(|(k, _)| k.clone()).collect())))
                }
                _ => err("keys expects a Dict"),
            },
            "values" => match args {
                [Value::Dict(entries)] => {
                    Ok(Some(Value::List(entries.iter().map(|(_, v)| v.clone()).collect())))
                }
                _ => err("values expects a Dict"),
            },
            // Each entry as a `(key, value)` tuple, in insertion order.
            "pairs" => match args {
                [Value::Dict(entries)] => Ok(Some(Value::List(
                    entries
                        .iter()
                        .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                        .collect(),
                ))),
                _ => err("pairs expects a Dict"),
            },
            "size" => match args {
                [Value::Dict(entries)] => Ok(Some(Value::Int(entries.len() as i64))),
                _ => err("size expects a Dict"),
            },
            // Filesystem capability (cap-std style): attenuate to a subdirectory.
            "subdir" => match args {
                [Value::Dir(base), Value::Str(name)] => {
                    Ok(Some(Value::Dir(resolve(base, name)?)))
                }
                _ => err("subdir expects a Dir and a name"),
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
                _ => err("read expects a Dir and a relative path"),
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
                _ => err("write expects a Dir, a relative path, and contents"),
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
            // Network capability: attenuate a Net to a held address.
            "restrict" => match args {
                [Value::Net(allow), Value::Str(addr)] => {
                    if !allow.iter().any(|a| a == addr) {
                        return err(format!("restrict: `{addr}` is not in this Net capability"));
                    }
                    Ok(Some(Value::Net(vec![addr.clone()])))
                }
                _ => err("restrict expects a Net and an address"),
            },
            // Connect only to an address the Net capability permits.
            "connect" => match args {
                [Value::Net(allow), Value::Str(addr)] => {
                    if !allow.iter().any(|a| a == addr) {
                        return err(format!("connect: `{addr}` is not permitted by this Net capability"));
                    }
                    match TcpStream::connect(addr) {
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
                    if !allow.iter().any(|a| a == addr) {
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
                            self.sockets.push(BufReader::new(stream));
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
                        let _ = sock.get_mut().shutdown(std::net::Shutdown::Both);
                    }
                    Ok(Some(Value::Nil))
                }
                _ => err("close expects a Socket"),
            },
            _ => Ok(None),
        }
    }

    /// Spawn an actor: build its initial state from field initializers and the
    /// positional spawn arguments (which supply the non-defaulted fields, e.g.
    /// capabilities). Returns a Subject handle.
    fn spawn_actor(&mut self, name: &str, args: Vec<Value>) -> Result<Value, RuntimeError> {
        let Some(def) = self.actor_defs.get(name).cloned() else {
            return err(format!("cannot spawn unknown actor `{name}`"));
        };
        let mut state = HashMap::new();
        let mut supplied = args.into_iter();
        for field in &def.fields {
            let value = match &field.init {
                Some(init) => finish(self.eval(init, &mut Env::new()))?,
                None => match supplied.next() {
                    Some(v) => v,
                    None => {
                        return err(format!(
                            "spawn {name}: missing value for field `{}`",
                            field.name
                        ))
                    }
                },
            };
            state.insert(field.name.clone(), value);
        }
        let id = self.actors.len();
        self.actors.push(ActorInstance {
            def: name.to_string(),
            state,
        });
        Ok(Value::Subject(id))
    }

    /// Process queued messages until the system is quiescent. Each handler runs
    /// to completion before the next message is dispatched (BEAM-style).
    pub fn run_to_completion(&mut self) -> Result<(), RuntimeError> {
        let mut steps = 0u64;
        while let Some((id, msg)) = self.queue.pop_front() {
            steps += 1;
            if steps > 1_000_000 {
                return err("actor scheduler exceeded its step budget");
            }
            self.handle_message(id, msg)?;
        }
        Ok(())
    }

    fn handle_message(&mut self, id: usize, msg: Value) -> Result<(), RuntimeError> {
        let Value::Ctor { name: msg_name, fields } = msg else {
            return err("a message must be a constructor value, e.g. Log(\"hi\")");
        };
        let def_name = self.actors[id].def.clone();
        let def = self.actor_defs.get(&def_name).cloned().unwrap();
        let Some(handler) = def.handlers.iter().find(|h| h.message == msg_name) else {
            return err(format!(
                "actor `{def_name}` has no handler for message `{msg_name}`"
            ));
        };
        if handler.params.len() != fields.len() {
            return err(format!(
                "message `{msg_name}` carries {} value(s) but the handler expects {}",
                fields.len(),
                handler.params.len()
            ));
        }
        // State is the base scope; handler parameters layer on top. `var` fields
        // are mutable; capability/immutable fields are not.
        let field_mut: HashMap<&str, bool> =
            def.fields.iter().map(|f| (f.name.as_str(), f.mutable)).collect();
        let mut env = Env::new();
        for (k, v) in &self.actors[id].state {
            let mutable = field_mut.get(k.as_str()).copied().unwrap_or(false);
            env.define(k.clone(), v.clone(), mutable);
        }
        env.push();
        for (param, value) in handler.params.iter().zip(fields) {
            env.define(
                param.name.clone(),
                value,
                !matches!(param.convention, Convention::Let),
            );
        }
        finish(self.eval_block(&handler.body, &mut env))?;
        // Persist any state the handler mutated.
        let field_names: Vec<String> = self.actors[id].state.keys().cloned().collect();
        for k in field_names {
            if let Some(v) = env.get(&k) {
                self.actors[id].state.insert(k, v.clone());
            }
        }
        Ok(())
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
                Stmt::Let { name, mutable, value } => {
                    let v = self.eval(value, env)?;
                    env.define(name.clone(), v, *mutable);
                    result = Value::Nil;
                }
                Stmt::Assign { name, value } => {
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
                Stmt::Expr(e) => {
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
            Expr::Int(n) | Expr::Duration(n) => Ok(Value::Int(*n)),
            Expr::Float(x) => Ok(Value::Float(*x)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
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
                    (UnOp::Neg, Value::Int(n)) => {
                        n.checked_neg().map(Value::Int).ok_or_else(over("-")).map_err(Flow::Err)
                    }
                    (UnOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
                    (UnOp::Neg, other) => err(format!("cannot negate `{other}`")),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Not, other) => err(format!("cannot apply `!` to `{other}`")),
                    (UnOp::BitNot, Value::Int(n)) => Ok(Value::Int(!n)),
                    (UnOp::BitNot, other) => err(format!("cannot apply `~` to `{other}`")),
                }
            }
            Expr::Lambda { params, body } => Ok(Value::Closure {
                params: params.clone(),
                body: body.clone(),
                env: Box::new(env.clone()),
            }),
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
            Expr::Spawn { actor, args } => {
                let argvals = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(self.spawn_actor(actor, argvals)?)
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

fn eval_binary(op: BinOp, l: Value, r: Value) -> Result<Value, RuntimeError> {
    use BinOp::*;
    use Value::{Float, Int, Str};
    match op {
        // Int arithmetic is checked: overflow yields a runtime error rather than
        // panicking the host (a program must never crash the interpreter).
        Add | Sub | Mul | Div => match (op, l, r) {
            (Add, Int(a), Int(b)) => a.checked_add(b).map(Int).ok_or_else(over("+")),
            (Sub, Int(a), Int(b)) => a.checked_sub(b).map(Int).ok_or_else(over("-")),
            (Mul, Int(a), Int(b)) => a.checked_mul(b).map(Int).ok_or_else(over("*")),
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
            (Int(a), Int(b)) => a.checked_rem(b).map(Int).ok_or_else(over("%")),
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
fn resolve(base: &Path, rel: &str) -> Result<PathBuf, RuntimeError> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return err("absolute paths are not allowed (a Dir capability is a subtree)");
    }
    for comp in p.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return err("`..` escapes the Dir capability"),
            _ => return err("invalid path component in a Dir-relative path"),
        }
    }
    let joined = base.join(rel);
    let real = std::fs::canonicalize(&joined).map_err(|e| RuntimeError {
        message: format!("cannot access `{}`: {e}", joined.display()),
    })?;
    let real_base = std::fs::canonicalize(base).map_err(|e| RuntimeError {
        message: format!("invalid Dir base `{}`: {e}", base.display()),
    })?;
    if !real.starts_with(&real_base) {
        return err("path escapes the Dir capability (via symlink)");
    }
    Ok(real)
}

/// Like `resolve`, but for writing: the target file need not exist, so
/// confinement is checked against its parent directory (which must exist and lie
/// within the capability's subtree). The lexical `..`/absolute checks still apply.
fn resolve_write(base: &Path, rel: &str) -> Result<PathBuf, RuntimeError> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return err("absolute paths are not allowed (a Dir capability is a subtree)");
    }
    for comp in p.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return err("`..` escapes the Dir capability"),
            _ => return err("invalid path component in a Dir-relative path"),
        }
    }
    let joined = base.join(rel);
    let parent = joined.parent().unwrap_or(base);
    let real_parent = std::fs::canonicalize(parent).map_err(|e| RuntimeError {
        message: format!("cannot access `{}`: {e}", parent.display()),
    })?;
    let real_base = std::fs::canonicalize(base).map_err(|e| RuntimeError {
        message: format!("invalid Dir base `{}`: {e}", base.display()),
    })?;
    if !real_parent.starts_with(&real_base) {
        return err("path escapes the Dir capability (via symlink)");
    }
    // The parent is confined, but the final component itself could be a
    // pre-existing symlink pointing outside the subtree (unlike `read`, we can't
    // canonicalize a not-yet-existing target). Refuse to write *through* a
    // symlink leaf. Same canonicalize-then-use TOCTOU caveat as `resolve` — the
    // race-free fix is the planned `openat2`/WASI-preopen substrate.
    if let Ok(meta) = std::fs::symlink_metadata(&joined) {
        if meta.file_type().is_symlink() {
            return err("path escapes the Dir capability (the target is a symlink)");
        }
    }
    Ok(joined)
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
/// `main` is the root actor: it receives the capabilities it declares (the only
/// place authority is minted) and hands attenuated ones to the actors it spawns.
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
    // Desugar traits/impls to ordinary functions (no-op for trait-free modules)
    // so the interpreter, like codegen, only ever sees plain functions.
    let module = crate::traits::lower(module);
    // Run the tree-walker on a thread with a large stack. The interpreter
    // recurses in Rust for nested calls, so deep (but bounded) recursion would
    // otherwise overflow the default stack and *abort the host*. The big stack
    // accommodates legitimate depth; `depth_limit` is the graceful guard against
    // runaway recursion well before this stack is exhausted.
    let root = root.as_ref().to_path_buf();
    let handle = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024 * 1024)
        .spawn(move || run_module_inner(module, root, net_allow))
        .map_err(|e| RuntimeError {
            message: format!("could not start the interpreter thread: {e}"),
        })?;
    // `join` contains any panic from the evaluator thread, so even an unforeseen
    // panic becomes a graceful error here instead of taking down the host.
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(RuntimeError {
            message: "internal error: the interpreter thread panicked".into(),
        }),
    }
}

fn run_module_inner(
    module: Module,
    root: PathBuf,
    net_allow: Vec<String>,
) -> Result<Vec<String>, RuntimeError> {
    let mut interp = Interpreter::new(module);
    interp.root = root;
    interp.net_allow = net_allow;
    let root_args = match interp.functions.get("main").cloned() {
        Some(f) => f
            .params
            .iter()
            .map(|p| interp.root_cap_for(&p.ty))
            .collect::<Result<Vec<_>, _>>()?,
        None => vec![],
    };
    let ret = interp
        .call("main", root_args)
        .map_err(|e| rt_at_line(e, interp.cur_line, &interp.cur_fn))?;
    interp
        .run_to_completion()
        .map_err(|e| rt_at_line(e, interp.cur_line, &interp.cur_fn))?;
    // A program that computes a value but prints nothing (e.g. `main` returns an
    // Int) still has something to show: surface its result.
    if interp.output.is_empty() && !matches!(ret, Value::Nil) {
        interp.output.push(format!("{ret}"));
    }
    Ok(interp.output)
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
    let linked = crate::linker::link(modules, entry)
        .map_err(|e| RuntimeError { message: e.message })?;
    run_module(linked, ".", Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_arithmetic_and_precedence() {
        let out = run(r#"
fn main(console: Console):
    print(console, int_to_string((1 + (2 * 3))))
"#)
            .unwrap();
        assert_eq!(out, vec!["7"]);
    }

    #[test]
    fn dir_capability_rejects_path_traversal() {
        // A Dir capability is confined to its subtree. `resolve` must reject any
        // path that would escape it, so a holder (e.g. an untrusted library
        // handed a narrow Dir) can read within the subtree but never above it.
        use std::path::Path;
        let base = Path::new(".");
        // Positive control: a path inside the subtree resolves (Cargo.toml is at
        // the crate root, the CWD for tests).
        assert!(resolve(base, "Cargo.toml").is_ok());
        // `..` is rejected lexically, before any filesystem access.
        assert!(resolve(base, "../secret").is_err());
        assert!(resolve(base, "src/../../etc/passwd").is_err());
        // Absolute paths are rejected: the capability is a subtree, not root.
        assert!(resolve(base, "/etc/passwd").is_err());
    }

    #[test]
    fn calls_user_functions_and_concats_strings() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main(console: Console):
    print(console, ("doubled: " <> int_to_string(double(21))))
"#;
        assert_eq!(run(src).unwrap(), vec!["doubled: 42"]);
    }

    #[test]
    fn pipelines_thread_left_to_right() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn main(console: Console):
    let result = int_to_string(double(4))
    print(console, result)
"#;
        assert_eq!(run(src).unwrap(), vec!["8"]);
    }

    #[test]
    fn match_with_constructors_and_guards() {
        let src = r#"
fn describe(e: Event) -> String:
    match e:
        Click(x, _) if (x > 0) -> "right click"
        Click(_, _) -> "other click"
        Closed -> "closed"
        _ -> "unknown"

fn main(console: Console):
    print(console, describe(Click(5, 9)))
    print(console, describe(Click((-1), 0)))
    print(console, describe(Closed))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["right click", "other click", "closed"]
        );
    }

    #[test]
    fn if_else_and_let_bindings() {
        let src = r#"
fn sign(n: Int) -> String:
    let label = if (n > 0): "positive" else: "non-positive"
    label

fn main(console: Console):
    print(console, sign(3))
    print(console, sign((-2)))
"#;
        assert_eq!(run(src).unwrap(), vec!["positive", "non-positive"]);
    }

    #[test]
    fn recursion_works() {
        let src = r#"
fn fact(n: Int) -> Int:
    match n:
        0 -> 1
        _ -> (n * fact((n - 1)))

fn main(console: Console):
    print(console, int_to_string(fact(5)))
"#;
        assert_eq!(run(src).unwrap(), vec!["120"]);
    }

    #[test]
    fn reports_unknown_function() {
        let e = run(r#"
fn main():
    nope()
"#).unwrap_err();
        assert!(e.message.contains("unknown function"));
    }

    /// The capability thesis at the language level: a function that was never
    /// handed the Console capability cannot print, even though `print` exists.
    #[test]
    fn function_without_capability_cannot_print() {
        let src = r#"
fn leak(secret: String) -> Nil:
    print(secret)

fn main(console: Console):
    leak("password")
"#;
        let e = run(src).unwrap_err();
        assert!(
            e.message.contains("Console capability"),
            "expected a capability error, got: {}",
            e.message
        );
    }

    /// Holding the capability, the same effect succeeds — capabilities
    /// propagate by being passed explicitly.
    #[test]
    fn capability_can_be_threaded_to_a_helper() {
        let src = r#"
fn announce(console: Console, who: String) -> Nil:
    print(console, ("hello, " <> who))

fn main(console: Console):
    announce(console, "witchy")
"#;
        assert_eq!(run(src).unwrap(), vec!["hello, witchy"]);
    }

    #[test]
    fn dir_capability_reads_attenuates_and_confines() {
        let root = std::env::temp_dir().join(format!("witchy_fs_{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/hi.txt"), "hi!").unwrap();

        // Attenuate to a subdir and read a file within it.
        let ok = r#"
fn main(console: Console, root: Dir):
    let d = subdir(root, "sub")
    print(console, read(d, "hi.txt"))
"#;
        assert_eq!(run_in(ok, &root).unwrap(), vec!["hi!"]);

        // Confinement: `..` cannot escape the granted subtree.
        let escape = r#"
fn main(console: Console, root: Dir):
    print(console, read(root, "../secret"))
"#;
        assert!(run_in(escape, &root).is_err());

        // A function with no Dir cannot read (no way to obtain the capability).
        let no_cap = r#"
fn sneaky() -> String:
    read(root, "sub/hi.txt")

fn main(console: Console, root: Dir):
    print(console, sneaky())
"#;
        assert!(run_in(no_cap, &root).is_err());

        // Confinement holds against symlinks: a link inside the subtree pointing
        // outside it must not be followable.
        #[cfg(unix)]
        {
            let outside = std::env::temp_dir().join(format!("witchy_outside_{}", std::process::id()));
            std::fs::write(&outside, "secret").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("sub/escape")).ok();
            let via_symlink = r#"
fn main(console: Console, root: Dir):
    let d = subdir(root, "sub")
    print(console, read(d, "escape"))
"#;
            assert!(run_in(via_symlink, &root).is_err());
            std::fs::remove_file(&outside).ok();
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn net_capability_connects_attenuates_and_denies() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        // One-shot loopback echo server.
        let server = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut r = BufReader::new(stream);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let _ = r.get_mut().write_all(line.as_bytes());
            }
        });

        // Attenuate to the one held address, connect, send, receive the echo.
        let ok = format!(
            r#"
fn main(console: Console, net: Net):
    let only = restrict(net, "{addr}")
    let s = connect(only, "{addr}")
    send_line(s, "ping")
    print(console, recv_line(s))
"#
        );
        assert_eq!(run_with(&ok, ".", vec![addr.clone()]).unwrap(), vec!["ping"]);
        server.join().ok();

        // Denied: connecting to an address not in the allow-list.
        let denied = format!(
            r#"
fn main(console: Console, net: Net):
    let s = connect(net, "10.255.255.1:80")
    send_line(s, "x")
"#
        );
        assert!(run_with(&denied, ".", vec![addr.clone()]).is_err());

        // Denied: cannot attenuate to an address not already held.
        let bad_restrict = r#"
fn main(console: Console, net: Net):
    let bad = restrict(net, "10.255.255.1:80")
    print(console, "unreachable")
"#;
        assert!(run_with(bad_restrict, ".", vec![addr]).is_err());
    }

    #[test]
    fn net_server_listen_accept_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        // A free port to hand the witchy server (bind+drop to discover it).
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
fn main(console: Console, net: Net):
    let server = listen(net, "{addr}")
    let sock = accept(server)
    let line = recv_line(sock)
    print(console, line)
    send_bytes(sock, "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello witchy")
    close(sock)
"#
        );
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_with(&src, ".", allow));

        // Connect once the server has bound (retry through the bind race).
        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect to witchy server");
        stream.write_all(b"GET /hi HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK"), "resp: {resp}");
        assert!(resp.ends_with("hello witchy"), "resp: {resp}");

        let out = server.join().unwrap().unwrap();
        assert_eq!(out, vec!["GET /hi HTTP/1.1\r"]);
    }

    #[test]
    fn serve_loopback_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
fn main(console: Console, net: Net):
    let app = server.router()
        .get("/", fn(req: Request): server.text(200, "home"))
        .get("/users/:id", fn(req: Request): server.text(200, "user " <> server.param(req, "id")))
        .post("/echo", fn(req: Request): server.text(201, server.request_body(req)))
    server.serve_n(net, "{addr}", app, 3)
"#
        );
        // Link in the bundled std (http + its deps), then run on a thread.
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let r1 = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r1.contains("200 OK") && r1.ends_with("home"), "r1: {r1}");
        let r2 = request("GET /users/42 HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.ends_with("user 42"), "r2: {r2}");
        let r3 = request("POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello");
        assert!(r3.contains("201 ") && r3.ends_with("hello"), "r3: {r3}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn http_client_builder_loopback() {
        // The reqwest-style client builder (get_request |> with_header |> send)
        // against a raw TCP server: it sends the method/path/header and parses
        // the response status and body.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let srv = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nhello!!")
                .unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });

        let src = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let req = http.get_request("http://{addr}/path")
        .with_header("X-Test", "abc")
        .with_query("q", "hi")
    match http.send(req, net):
        Ok(resp) ->
            print(console, int_to_string(http.status(resp)))
            print(console, http.body(resp))
            print(console, to_string(http.is_success(resp)))
        Err(e) -> print(console, "err: " <> e)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let out = run_module(linked, ".", vec![addr.clone()]).expect("run");
        assert_eq!(out, vec!["200", "hello!!", "true"]);
        let req = srv.join().unwrap();
        assert!(req.contains("GET /path?q=hi HTTP/1.1"), "req: {req}");
        assert!(req.contains("X-Test: abc"), "req: {req}");
    }

    #[test]
    fn serve_status_constructors_roundtrip() {
        // The status-named response constructors (created/bad_request/
        // unauthorized/no_content) render the right status line and reason.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
fn main(console: Console, net: Net):
    let app = server.router()
        |> server.post("/make", fn(req: Request): server.created("made"))
        |> server.get("/bad", fn(req: Request): server.bad_request("nope"))
        |> server.get("/secret", fn(req: Request): server.unauthorized("auth"))
        |> server.delete("/item", fn(req: Request): server.no_content())
    server.serve_n(net, "{addr}", app, 4)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let r1 = request("POST /make HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
        assert!(r1.contains("201 Created") && r1.ends_with("made"), "r1: {r1}");
        let r2 = request("GET /bad HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.contains("400 Bad Request") && r2.ends_with("nope"), "r2: {r2}");
        let r3 = request("GET /secret HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r3.contains("401 Unauthorized") && r3.ends_with("auth"), "r3: {r3}");
        let r4 = request("DELETE /item HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r4.contains("204 No Content"), "r4: {r4}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_method_not_allowed_vs_not_found() {
        // A known path with the wrong method is a 405; an unknown path is a 404.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
fn main(console: Console, net: Net):
    let app = server.router()
        |> server.post("/items", fn(req: Request): server.created("ok"))
    server.serve_n(net, "{addr}", app, 3)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let ok = request("POST /items HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
        assert!(ok.contains("201 Created"), "ok: {ok}");
        let wrong = request("GET /items HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(wrong.contains("405 Method Not Allowed"), "wrong: {wrong}");
        let missing = request("GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(missing.contains("404 Not Found"), "missing: {missing}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_var_receiver_method_resolution() {
        // Method calls on a variable receiver (`var app = router(); app = app.get(...)`)
        // resolve the overloaded `get`/`post` by the tracked variable type (Router),
        // even though http/server/json all export `get`.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
import json
fn main(console: Console, net: Net):
    var app = server.router()
    app = app.get("/", fn(req: Request): server.ok("home"))
    app = app.post("/items", fn(req: Request): server.created("made"))
    server.serve_n(net, "{addr}", app, 2)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let g = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(g.contains("200 OK") && g.ends_with("home"), "g: {g}");
        let p = request("POST /items HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
        assert!(p.contains("201 Created") && p.ends_with("made"), "p: {p}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_any_method_route_roundtrip() {
        // An `any` route answers every verb (the `*` wildcard method).
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
fn main(console: Console, net: Net):
    let app = server.router()
        |> server.any("/ping", fn(req: Request): server.ok(server.method(req)))
    server.serve_n(net, "{addr}", app, 2)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let g = request("GET /ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(g.contains("200 OK") && g.ends_with("GET"), "g: {g}");
        let d = request("DELETE /ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(d.contains("200 OK") && d.ends_with("DELETE"), "d: {d}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_middleware_nest_and_notfound_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server

// A tower-style Layer that tags every response with a header.
fn tagger(next: fn(Request) -> Response) -> fn(Request) -> Response:
    fn(req: Request): tag(next(req))

fn tag(resp: Response) -> Response:
    server.with_header(resp, "x-by", "witchy")

fn main(console: Console, net: Net):
    let api = server.router()
        |> server.get("/ping", fn(req: Request): server.text(200, "pong"))
    let app = server.router()
        |> server.get("/", fn(req: Request): server.text(200, "root"))
        |> server.nest("/api", api)
        |> server.layer(tagger)
    server.serve_n(net, "{addr}", app, 3)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        // Middleware tagged the response; root handler ran.
        let r1 = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r1.contains("x-by: witchy") && r1.ends_with("root"), "r1: {r1}");
        // Nested route under /api.
        let r2 = request("GET /api/ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.ends_with("pong"), "r2: {r2}");
        // Unknown path -> 404 (still tagged by the layer).
        let r3 = request("GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r3.contains("404 ") && r3.contains("x-by: witchy"), "r3: {r3}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_json_handler_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
import json
fn greet(req: Request) -> Response:
    server.json_value(200, JsonObject([("hello", JsonString(server.param(req, "name")))]))
fn main(console: Console, net: Net):
    let app = server.router() |> server.get("/hello/:name", greet)
    server.serve_n(net, "{addr}", app, 1)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect");
        stream.write_all(b"GET /hello/witchy HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("application/json"), "resp: {resp}");
        assert!(resp.contains("\"hello\"") && resp.contains("\"witchy\""), "resp: {resp}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_json_body_decode_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
import json
import option
fn name_of(doc: Json) -> String:
    match json.get(doc, "name"):
        Some(v) -> option.unwrap_or(json.as_string(v), "?")
        None -> "?"
fn echo_name(req: Request) -> Response:
    match server.json_body(req):
        Ok(doc) -> server.text(200, name_of(doc))
        Err(e) -> server.text(400, e)
fn main(console: Console, net: Net):
    let app = server.router() |> server.post("/", echo_name)
    server.serve_n(net, "{addr}", app, 1)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect");
        let body = "{\"name\":\"witchy\"}";
        let req = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK") && resp.ends_with("witchy"), "resp: {resp}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_form_field_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let src = format!(
            r#"
import http
import server
fn main(console: Console, net: Net):
    let app = server.router() |> server.post("/", fn(req: Request): server.text(200, server.form_field(req, "name")))
    server.serve_n(net, "{addr}", app, 1)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect(&addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect");
        let body = "name=witchy&lang=rust";
        let req = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK") && resp.ends_with("witchy"), "resp: {resp}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn serve_static_files_roundtrip() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        // The handler captures a Dir rooted at examples/data and serves from it.
        let src = format!(
            r#"
import http
import server
fn file_server(dir: Dir) -> fn(Request) -> Response:
    fn(req: Request): serve_file(dir, server.param(req, "path"))
fn serve_file(dir: Dir, p: String) -> Response:
    if exists(dir, p):
        server.text(200, read(dir, p))
    else:
        server.not_found()
fn main(console: Console, net: Net, root: Dir):
    let examples = subdir(root, "examples")
    let data = subdir(examples, "data")
    let app = server.router() |> server.get("/files/*path", file_server(data))
    server.serve_n(net, "{addr}", app, 2)
"#
        );
        let parsed = crate::parser::parse_module(&src).expect("parse");
        let linked =
            crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        let allow = vec![addr.clone()];
        let server = std::thread::spawn(move || run_module(linked, ".", allow));

        let request = |raw: &str| -> String {
            for _ in 0..100 {
                if let Ok(mut s) = TcpStream::connect(&addr) {
                    s.write_all(raw.as_bytes()).unwrap();
                    let mut resp = String::new();
                    s.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("could not connect to server");
        };

        let r1 = request("GET /files/greeting.txt HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r1.contains("200 OK") && r1.contains("sandboxed Dir"), "r1: {r1}");
        let r2 = request("GET /files/nope.txt HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r2.contains("404 "), "r2: {r2}");

        server.join().unwrap().unwrap();
    }

    #[test]
    fn handlers_cannot_reach_the_network() {
        // The capability guarantee: a pure handler has no Net, so even trying to
        // open a socket is a compile-time (type) error — it can't be written.
        let src = r#"
import server
fn evil(req: Request) -> Response:
    let s = connect(net, "10.0.0.1:80")
    server.text(200, "leaked")
fn main(console: Console, net: Net):
    let app = server.router() |> server.get("/", evil)
    server.serve_n(net, "127.0.0.1:0", app, 0)
"#;
        let parsed = crate::parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".to_string(), parsed)], "main").expect("link");
        // Type-check the linked program: `connect` needs a Net the handler lacks.
        assert!(crate::typeck::check(&linked).is_err());
    }

    #[test]
    fn modules_qualified_calls() {
        let strutil = r#"
fn shout(name: String) -> String:
    ("HELLO, " <> name)
"#;
        let app = r#"
import strutil

fn main(console: Console):
    print(console, strutil.shout("witchy"))
"#;
        assert_eq!(
            run_program(&[("strutil", strutil), ("app", app)], "app").unwrap(),
            vec!["HELLO, witchy"]
        );
    }

    #[test]
    fn library_uses_only_passed_capabilities() {
        // The app chooses to hand the logger its Console.
        let logger = r#"
fn log(console: Console, msg: String):
    print(console, ("[log] " <> msg))
"#;
        let app = r#"
import logger

fn main(console: Console):
    logger.log(console, "hi")
"#;
        assert_eq!(
            run_program(&[("logger", logger), ("app", app)], "app").unwrap(),
            vec!["[log] hi"]
        );
    }

    #[test]
    fn library_cannot_fabricate_a_capability() {
        // `steal` references `console` it was never given — caught at compile
        // time as an unbound variable (no ambient authority to grab).
        let evil = r#"
fn steal(secret: String) -> String:
    print(console, secret)
"#;
        let app = r#"
import evil

fn main(console: Console):
    print(console, evil.steal("data"))
"#;
        let linked = crate::linker::link(
            vec![
                ("evil".into(), parse_module(evil).unwrap()),
                ("app".into(), parse_module(app).unwrap()),
            ],
            "app",
        )
        .unwrap();
        assert!(crate::typeck::check(&linked).is_err());
    }

    #[test]
    fn calling_unimported_module_is_a_link_error() {
        let app = r#"
fn main(console: Console):
    print(console, other.foo())
"#;
        assert!(run_program(&[("app", app)], "app").is_err());
    }

    #[test]
    fn float_arithmetic() {
        let src = r#"
fn half(x: Float) -> Float:
    (x / 2.0)

fn main(console: Console):
    print(console, to_string(half(7.0)))
"#;
        assert_eq!(run(src).unwrap(), vec!["3.5"]);
    }

    #[test]
    fn boolean_operators() {
        let src = r#"
fn classify(n: Int) -> String:
    if ((n > 0) && (n < 10)):
        "small positive"
    else if ((n <= 0) || (n >= 100)):
        "out of range"
    else:
        "other"

fn main(console: Console):
    print(console, classify(5))
    print(console, classify((-1)))
    print(console, classify(50))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["small positive", "out of range", "other"]
        );
    }

    #[test]
    fn tuples_destructure_and_match() {
        let src = r#"
fn divmod(a: Int, b: Int) -> (Int, Int):
    ((a / b), (a % b))

fn main(console: Console):
    let (q, r) = divmod(17, 5)
    print(console, int_to_string(q))
    print(console, int_to_string(r))
    let pair = (1, "one")
    match pair:
        (n, name) -> print(console, ((int_to_string(n) <> "=") <> name))
"#;
        assert_eq!(run(src).unwrap(), vec!["3", "2", "1=one"]);
    }

    #[test]
    fn generic_identity_runs() {
        let src = r#"
fn id(x: a) -> a:
    x

fn main(console: Console):
    print(console, id("hi"))
    print(console, int_to_string(id(5)))
"#;
        assert_eq!(run(src).unwrap(), vec!["hi", "5"]);
    }

    #[test]
    fn generic_adt_runs() {
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn show(r: Result(Int, String)) -> String:
    match r:
        Ok(n) -> ("ok " <> int_to_string(n))
        Err(msg) -> ("err " <> msg)

fn main(console: Console):
    print(console, show(Ok(7)))
    print(console, show(Err("boom")))
"#;
        assert_eq!(run(src).unwrap(), vec!["ok 7", "err boom"]);
    }

    /// Run a no-parameter `main` with a small step ceiling, so an infinite loop
    /// is caught quickly instead of hanging the test.
    fn run_capped(src: &str, limit: u64) -> Result<Vec<String>, RuntimeError> {
        let module = parse_module(src).map_err(|e| RuntimeError { message: e.to_string() })?;
        let mut interp = Interpreter::new(module);
        interp.step_limit = limit;
        interp.call("main", vec![])?;
        interp.run_to_completion()?;
        Ok(interp.output)
    }

    #[test]
    fn early_return_exits_function_and_loop() {
        let src = r#"
fn first_even(xs: List(Int)) -> Int:
    for x in xs:
        if ((x % 2) == 0):
            return x
    (0 - 1)

fn main(console: Console):
    print(console, int_to_string(first_even([1, 3, 8, 5])))
    print(console, int_to_string(first_even([1, 3, 5])))
"#;
        assert_eq!(run(src).unwrap(), vec!["8", "-1"]);
    }

    #[test]
    fn negative_int_patterns_match() {
        let src = r#"
fn classify(n: Int) -> String:
    match n:
        -1 -> "neg one"
        0 -> "zero"
        _ -> "other"

fn main(console: Console):
    print(console, classify((-1)))
    print(console, classify(0))
    print(console, classify(3))
"#;
        assert_eq!(run(src).unwrap(), vec!["neg one", "zero", "other"]);
    }

    #[test]
    fn deep_recursion_is_a_graceful_error_not_a_crash() {
        // Runaway recursion must hit the depth limit and return an error rather
        // than overflowing the stack and aborting the host.
        let src = r#"
fn rec(n: Int) -> Int:
    if (n == 0):
        0
    else:
        rec((n - 1))

fn main(console: Console):
    print(console, int_to_string(rec(5000000)))
"#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("too deep"), "got: {}", e.message);
    }

    #[test]
    fn moderate_recursion_succeeds() {
        // Recursion well within the limit still works.
        let src = r#"
fn rec(n: Int) -> Int:
    if (n == 0):
        0
    else:
        rec((n - 1))

fn main(console: Console):
    print(console, int_to_string(rec(10000)))
"#;
        assert_eq!(run(src).unwrap(), vec!["0"]);
    }

    #[test]
    fn integer_overflow_is_a_graceful_error_not_a_panic() {
        // Multiplication that overflows i64 must return an error, never panic.
        let src = r#"
fn main(console: Console):
    let big = 9999999999
    print(console, int_to_string((big * big)))
"#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("overflow"), "got: {}", e.message);
    }

    #[test]
    fn negating_int_min_does_not_panic() {
        // -(i64::MIN) overflows via unary negation; it must be a graceful error.
        let src = r#"
fn main(console: Console):
    let lo = ((0 - 9223372036854775807) - 1)
    print(console, int_to_string((-lo)))
"#;
        assert!(run(src).is_err());
    }

    #[test]
    fn runtime_errors_report_their_source_line() {
        // Division by zero happens on the third line.
        let src = "fn main(console: Console):\n    let a = 1\n    print(console, int_to_string(a / 0))\n";
        let e = run(src).unwrap_err();
        assert!(e.message.contains("line 3"), "got: {}", e.message);
    }

    #[test]
    fn runtime_errors_name_the_innermost_function() {
        // The error must be attributed to `risky`, not the caller `main`.
        let src = r#"
fn risky(n: Int) -> Int:
    (n / 0)

fn main(console: Console):
    print(console, int_to_string(risky(5)))
"#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("risky"), "got: {}", e.message);
    }

    #[test]
    fn runaway_loop_is_bounded_not_hung() {
        let src = r#"
fn main() -> Int:
    var i = 0
    while true:
        i = (i + 1)
    i
"#;
        let e = run_capped(src, 100_000).unwrap_err();
        assert!(e.message.contains("step budget"), "got: {}", e.message);
    }

    #[test]
    fn normal_program_runs_within_budget() {
        // A finite loop well under the ceiling completes normally.
        let src = r#"
fn main() -> Int:
    var sum = 0
    var i = 0
    while (i < 1000):
        sum = (sum + i)
        i = (i + 1)
    sum
"#;
        assert!(run_capped(src, 100_000).is_ok());
    }

    #[test]
    fn dict_values_and_pairs_iterate() {
        let src = r#"
fn main(console: Console):
    var d = dict_new()
    d = insert(d, "a", 10)
    d = insert(d, "b", 20)
    var sum = 0
    for v in values(d):
        sum = (sum + v)
    print(console, int_to_string(sum))
    var report = ""
    for e in pairs(d):
        let (k, v) = e
        report = ((((report <> k) <> "=") <> int_to_string(v)) <> ";")
    print(console, report)
"#;
        assert_eq!(run(src).unwrap(), vec!["30", "a=10;b=20;"]);
    }

    #[test]
    fn dict_insert_get_has_keys_and_immutability() {
        let src = r#"
fn main(console: Console):
    let a = insert(dict_new(), "x", 1)
    let b = insert(a, "y", 2)
    let c = insert(b, "x", 9)
    print(console, int_to_string(get_or(c, "x", 0)))
    print(console, int_to_string(get_or(c, "y", 0)))
    print(console, int_to_string(get_or(c, "z", 0)))
    print(console, int_to_string(size(c)))
    print(console, int_to_string(get_or(a, "x", 0)))
    print(console, to_string(has(c, "y")))
    print(console, int_to_string(length(keys(c))))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["9", "2", "0", "2", "1", "true", "2"]
        );
    }

    #[test]
    fn sqrt_builtin_computes() {
        let src = r#"
fn main(console: Console):
    print(console, to_string(sqrt(2.0)))
    print(console, to_string(float_to_int(sqrt(144.0))))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["1.4142135623730951", "12"]
        );
    }

    #[test]
    fn string_slicing_and_search() {
        let src = r#"
fn main(console: Console):
    let s = "abcdef"
    print(console, substring(s, 1, 4))
    print(console, substring(s, 4, 100))
    print(console, substring(s, 3, 1))
    print(console, int_to_string(index_of(s, "cd")))
    print(console, int_to_string(index_of(s, "z")))
    print(console, to_string(ends_with(s, "ef")))
"#;
        assert_eq!(
            run(src).unwrap(),
            vec!["bcd", "ef", "", "2", "-1", "true"]
        );
    }

    #[test]
    fn substring_is_char_based_not_byte_based() {
        // A multi-byte char must count as one position, not its byte length.
        let src = r#"
fn main(console: Console):
    let s = "héllo"
    print(console, substring(s, 0, 2))
"#;
        assert_eq!(run(src).unwrap(), vec!["hé"]);
    }

    #[test]
    fn string_split_contains_replace() {
        let src = r#"
fn main(console: Console):
    let parts = split("a,b,c", ",")
    print(console, int_to_string(length(parts)))
    print(console, at(parts, 1))
    print(console, replace("a,b,c", ",", "-"))
    print(console, to_string(contains("hello", "ell")))
"#;
        assert_eq!(run(src).unwrap(), vec!["3", "b", "a-b-c", "true"]);
    }

    #[test]
    fn push_is_immutable_and_concat_joins() {
        let src = r#"
fn main(console: Console):
    let a = [1, 2]
    let b = push(a, 3)
    print(console, int_to_string(length(a)))
    print(console, int_to_string(length(b)))
    let c = concat(a, [9, 9])
    print(console, int_to_string(at(c, 3)))
"#;
        assert_eq!(run(src).unwrap(), vec!["2", "3", "9"]);
    }

    #[test]
    fn closure_captures_environment() {
        let src = r#"
fn adder(n: Int) -> fn(Int) -> Int:
    fn(x: Int): (x + n)

fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let inc = adder(1)
    let plus100 = adder(100)
    print(console, int_to_string(apply(inc, 5)))
    print(console, int_to_string(apply(plus100, 5)))
"#;
        assert_eq!(run(src).unwrap(), vec!["6", "105"]);
    }

    #[test]
    fn try_inside_lambda_returns_from_lambda() {
        // `?` inside a lambda short-circuits the lambda, not the outer function.
        let src = r#"
type Option:
    Some(a)
    None
fn run(f: fn(Option(Int)) -> Option(Int), o: Option(Int)) -> Option(Int):
    f(o)
fn render(o: Option(Int)) -> String:
    match o:
        Some(n) -> int_to_string(n)
        None -> "none"
fn main(console: Console):
    let g = fn(o: Option(Int)):
        let n = o?
        Some(n + 1)
    print(console, render(run(g, Some(7))))
    print(console, render(run(g, None)))
"#;
        assert_eq!(run(src).unwrap(), vec!["8", "none"]);
    }

    #[test]
    fn record_update_does_not_mutate_the_original() {
        let src = r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(1, 2)
    let q = update p: x = 10 y = ((p).y + 1)
    print(console, (int_to_string((p).x) <> int_to_string((p).y)))
    print(console, (int_to_string((q).x) <> int_to_string((q).y)))
"#;
        assert_eq!(run(src).unwrap(), vec!["12", "103"]);
    }

    #[test]
    fn record_field_access_runs() {
        let src = r#"
type Person:
    name: String
    age: Int

fn main(console: Console):
    let p = Person("witchy", 7)
    print(console, (((p).name <> " is ") <> int_to_string((p).age)))
"#;
        assert_eq!(run(src).unwrap(), vec!["witchy is 7"]);
    }

    #[test]
    fn list_pattern_head_tail() {
        let src = r#"
fn len(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [_, ..tail] -> (1 + len(tail))

fn main(console: Console):
    print(console, int_to_string(len([5, 6, 7, 8])))
"#;
        assert_eq!(run(src).unwrap(), vec!["4"]);
    }

    #[test]
    fn for_in_accumulates() {
        let src = r#"
fn main(console: Console):
    var total = 0
    for n in [10, 20, 30]:
        total = (total + n)
    print(console, int_to_string(total))
"#;
        assert_eq!(run(src).unwrap(), vec!["60"]);
    }

    #[test]
    fn try_option_short_circuits() {
        // `?` on `None` returns `None` from `first_word`; on `Some` it unwraps.
        let src = r#"
type Option:
    Some(a)
    None

fn head(o: Option(Int)) -> Option(Int):
    let n = (o)?
    Some((n + 100))

fn render(o: Option(Int)) -> String:
    match o:
        Some(n) -> int_to_string(n)
        None -> "none"

fn main(console: Console):
    print(console, render(head(Some(1))))
    print(console, render(head(None)))
"#;
        assert_eq!(run(src).unwrap(), vec!["101", "none"]);
    }

    #[test]
    fn conversions() {
        let src = r#"
fn main(console: Console):
    print(console, to_string(int_to_float(7)))
    print(console, int_to_string(float_to_int(3.9)))
    print(console, int_to_string(string_to_int("42")))
"#;
        assert_eq!(run(src).unwrap(), vec!["7", "3", "42"]);
    }

    #[test]
    fn string_stdlib() {
        let src = r#"
fn main(console: Console):
    print(console, to_upper("witchy"))
    print(console, int_to_string(string_length("hello")))
    print(console, trim("  hi  "))
    if starts_with("witchy", "wit"):
        print(console, "yes")
    else:
        print(console, "no")
"#;
        assert_eq!(run(src).unwrap(), vec!["WITCHY", "5", "hi", "yes"]);
    }

    #[test]
    fn while_loop_and_modulo() {
        let src = r#"
fn main(console: Console):
    var i = 1
    var total = 0
    while (i <= 5):
        total = (total + i)
        i = (i + 1)
    print(console, int_to_string(total))
    print(console, int_to_string((10 % 3)))
"#;
        assert_eq!(run(src).unwrap(), vec!["15", "1"]);
    }

    #[test]
    fn boolean_not_and_short_circuit() {
        let src = r#"
fn is_zero(n: Int) -> Bool:
    (n == 0)

fn main(console: Console):
    if (!is_zero(5)):
        print(console, "nonzero")
    else:
        print(console, "zero")
"#;
        assert_eq!(run(src).unwrap(), vec!["nonzero"]);
    }

    #[test]
    fn lists_length_and_index() {
        let src = r#"
fn main(console: Console):
    let xs = [10, 20, 30]
    print(console, int_to_string(length(xs)))
    print(console, int_to_string(at(xs, 1)))
"#;
        assert_eq!(run(src).unwrap(), vec!["3", "20"]);
    }

    /// An actor with mutable state and a granted capability handles messages in
    /// order, run-to-completion.
    #[test]
    fn actor_handles_messages_with_state_and_capability() {
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
    send(logger, Log("first"))
    send(logger, Log("second"))
"#;
        assert_eq!(run(src).unwrap(), vec!["[1] first", "[2] second"]);
    }

    /// The capability thesis through the actor boundary: an actor that was
    /// never granted a Console cannot print.
    #[test]
    fn actor_without_capability_cannot_print() {
        let src = r#"
actor Sneaky:

impl Sneaky:
    on Go(msg: String):
        print(msg)

fn main(console: Console):
    let s = spawn Sneaky()
    send(s, Go("exfiltrate"))
"#;
        let e = run(src).unwrap_err();
        assert!(
            e.message.contains("Console capability"),
            "expected a capability error, got: {}",
            e.message
        );
    }

    /// Actors can spawn and message other actors; the scheduler keeps draining
    /// until quiescent.
    #[test]
    fn actors_can_message_other_actors() {
        let src = r#"
actor Printer:
    console: Console

impl Printer:
    on Say(msg: String):
        print(console, msg)

actor Forwarder:
    target: Subject

impl Forwarder:
    on Relay(msg: String):
        send(target, Say(msg))

fn main(console: Console):
    let printer = spawn Printer(console)
    let fwd = spawn Forwarder(printer)
    send(fwd, Relay("relayed hello"))
"#;
        assert_eq!(run(src).unwrap(), vec!["relayed hello"]);
    }

    #[test]
    fn let_bindings_are_immutable() {
        let src = r#"
fn main(console: Console):
    let x = 1
    x = 2
"#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("immutable"), "got: {}", e.message);
    }

    #[test]
    fn var_bindings_are_mutable() {
        let src = r#"
fn main(console: Console):
    var x = 1
    x = (x + 41)
    print(console, int_to_string(x))
"#;
        assert_eq!(run(src).unwrap(), vec!["42"]);
    }

    /// Hylo-style mutable value semantics: an `inout` parameter mutates the
    /// caller's variable in place — easy mutability, no pointers.
    #[test]
    fn inout_parameter_writes_back_to_caller() {
        let src = r#"
fn bump(inout n: Int):
    n = (n + 1)

fn main(console: Console):
    var x = 41
    bump(x)
    print(console, int_to_string(x))
"#;
        assert_eq!(run(src).unwrap(), vec!["42"]);
    }

    #[test]
    fn inout_requires_a_mutable_variable() {
        let src = r#"
fn bump(inout n: Int):
    n = (n + 1)

fn main(console: Console):
    let x = 41
    bump(x)
"#;
        let e = run(src).unwrap_err();
        assert!(
            e.message.contains("var") || e.message.contains("immutable"),
            "got: {}",
            e.message
        );
    }
}
