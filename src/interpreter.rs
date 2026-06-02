//! Tree-walking evaluator for witchy.
//!
//! This is a semantics prototype: it runs witchy programs directly in the host
//! so we can iterate on language behaviour. Compiling to WASM actors on the
//! proven runtime is a later phase.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};

use crate::ast::*;
use crate::parser::parse_module;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    List(Vec<Value>),
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

fn err<T>(message: impl Into<String>) -> Result<T, RuntimeError> {
    Err(RuntimeError {
        message: message.into(),
    })
}

/// Lexically scoped variable bindings. Functions are not closures: a call
/// starts a fresh `Env` so a function body sees only its parameters and the
/// global function table.
enum Assign {
    Done,
    Immutable,
    Unbound,
}

#[derive(Default)]
struct Env {
    /// Each binding carries whether it is mutable (`var`/`inout`/`sink`) or not
    /// (`let`). Mutable value semantics: bindings hold independent values.
    scopes: Vec<HashMap<String, (Value, bool)>>,
}

impl Env {
    fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    fn define(&mut self, name: String, value: Value, mutable: bool) {
        self.scopes.last_mut().unwrap().insert(name, (value, mutable));
    }
    fn get(&self, name: &str) -> Option<&Value> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .map(|(v, _)| v)
    }
    /// Reassign an existing binding in place; rejects immutable (`let`) bindings.
    fn assign(&mut self, name: &str, value: Value) -> Assign {
        for scope in self.scopes.iter_mut().rev() {
            if let Some((slot, mutable)) = scope.get_mut(name) {
                if *mutable {
                    *slot = value;
                    return Assign::Done;
                }
                return Assign::Immutable;
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
    functions: HashMap<String, Function>,
    actor_defs: HashMap<String, ActorDef>,
    actors: Vec<ActorInstance>,
    queue: VecDeque<(usize, Value)>,
    /// Host directory the root `Dir` capability is rooted at.
    root: PathBuf,
    /// Allow-list backing the root `Net` capability.
    net_allow: Vec<String>,
    /// Open sockets, indexed by `Value::Socket` handle.
    sockets: Vec<BufReader<TcpStream>>,
    pub output: Vec<String>,
}

impl Interpreter {
    pub fn new(module: Module) -> Self {
        let mut functions = HashMap::new();
        let mut actor_defs = HashMap::new();
        for item in module.items {
            match item {
                Item::Function(f) => {
                    functions.insert(f.name.clone(), f);
                }
                Item::Actor(a) => {
                    actor_defs.insert(a.name.clone(), a);
                }
                // Type declarations are erased at runtime; the type checker
                // uses them.
                Item::Type(_) => {}
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
        self.eval_block(&func.body, &mut env)
    }

    /// Evaluate a function call expression, honoring parameter conventions:
    /// `inout` arguments must be mutable variables and are written back after
    /// the call returns (Hylo-style move-in / move-out).
    fn eval_call(&mut self, name: &str, args: &[Expr], env: &mut Env) -> Result<Value, RuntimeError> {
        let argvals = args
            .iter()
            .map(|a| self.eval(a, env))
            .collect::<Result<Vec<_>, _>>()?;
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
        let result = self.eval_block(&func.body, &mut fenv)?;
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
                    self.output.push(msg.to_string());
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
                Some(init) => self.eval(init, &mut Env::new())?,
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
        self.eval_block(&handler.body, &mut env)?;
        // Persist any state the handler mutated.
        let field_names: Vec<String> = self.actors[id].state.keys().cloned().collect();
        for k in field_names {
            if let Some(v) = env.get(&k) {
                self.actors[id].state.insert(k, v.clone());
            }
        }
        Ok(())
    }

    fn eval_block(&mut self, block: &Block, env: &mut Env) -> Result<Value, RuntimeError> {
        env.push();
        let mut result = Value::Nil;
        for stmt in &block.stmts {
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
                Stmt::Expr(e) => {
                    result = self.eval(e, env)?;
                }
            }
        }
        env.pop();
        Ok(result)
    }

    fn eval(&mut self, expr: &Expr, env: &mut Env) -> Result<Value, RuntimeError> {
        match expr {
            Expr::Int(n) => Ok(Value::Int(*n)),
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
            Expr::Var(name) => match env.get(name) {
                Some(v) => Ok(v.clone()),
                None => err(format!("unbound variable `{name}`")),
            },
            Expr::Call { name, args } => self.eval_call(name, args, env),
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
                    (UnOp::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
                    (UnOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
                    (UnOp::Neg, other) => err(format!("cannot negate `{other}`")),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Not, other) => err(format!("cannot apply `!` to `{other}`")),
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
                eval_binary(*op, l, r)
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
            Expr::Block(block) => self.eval_block(block, env),
            Expr::While { cond, body } => {
                loop {
                    match self.eval(cond, env)? {
                        Value::Bool(true) => {
                            self.eval_block(body, env)?;
                        }
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
                self.spawn_actor(actor, argvals)
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
        _ => false,
    }
}

fn eval_binary(op: BinOp, l: Value, r: Value) -> Result<Value, RuntimeError> {
    use BinOp::*;
    use Value::{Float, Int, Str};
    match op {
        Add | Sub | Mul | Div => match (op, l, r) {
            (Add, Int(a), Int(b)) => Ok(Int(a + b)),
            (Sub, Int(a), Int(b)) => Ok(Int(a - b)),
            (Mul, Int(a), Int(b)) => Ok(Int(a * b)),
            (Div, Int(_), Int(0)) => err("division by zero"),
            (Div, Int(a), Int(b)) => Ok(Int(a / b)),
            (Add, Float(a), Float(b)) => Ok(Float(a + b)),
            (Sub, Float(a), Float(b)) => Ok(Float(a - b)),
            (Mul, Float(a), Float(b)) => Ok(Float(a * b)),
            (Div, Float(a), Float(b)) => Ok(Float(a / b)),
            (_, a, b) => err(format!("cannot apply arithmetic to `{a}` and `{b}`")),
        },
        Mod => match (l, r) {
            (Int(_), Int(0)) => err("modulo by zero"),
            (Int(a), Int(b)) => Ok(Int(a % b)),
            (a, b) => err(format!("`%` expects two Ints, got `{a}` and `{b}`")),
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
    let mut interp = Interpreter::new(module);
    interp.root = root.as_ref().to_path_buf();
    interp.net_allow = net_allow;
    let root_args = match interp.functions.get("main").cloned() {
        Some(f) => f
            .params
            .iter()
            .map(|p| interp.root_cap_for(&p.ty))
            .collect::<Result<Vec<_>, _>>()?,
        None => vec![],
    };
    interp.call("main", root_args)?;
    interp.run_to_completion()?;
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
        let out = run("fn main(console: Console) { print(console, int_to_string(1 + 2 * 3)) }")
            .unwrap();
        assert_eq!(out, vec!["7"]);
    }

    #[test]
    fn calls_user_functions_and_concats_strings() {
        let src = r#"
            fn double(n: Int) -> Int { n * 2 }
            fn main(console: Console) {
              print(console, "doubled: " <> int_to_string(double(21)))
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["doubled: 42"]);
    }

    #[test]
    fn pipelines_thread_left_to_right() {
        let src = r#"
            fn double(n: Int) -> Int { n * 2 }
            fn main(console: Console) {
              let result = 4 |> double() |> int_to_string()
              print(console, result)
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["8"]);
    }

    #[test]
    fn match_with_constructors_and_guards() {
        let src = r#"
            fn describe(e: Event) -> String {
              match e {
                Click(x, _) if x > 0 -> "right click"
                Click(_, _) -> "other click"
                Closed -> "closed"
                _ -> "unknown"
              }
            }
            fn main(console: Console) {
              print(console, describe(Click(5, 9)))
              print(console, describe(Click(-1, 0)))
              print(console, describe(Closed))
            }
        "#;
        assert_eq!(
            run(src).unwrap(),
            vec!["right click", "other click", "closed"]
        );
    }

    #[test]
    fn if_else_and_let_bindings() {
        let src = r#"
            fn sign(n: Int) -> String {
              let label = if n > 0 { "positive" } else { "non-positive" }
              label
            }
            fn main(console: Console) {
              print(console, sign(3))
              print(console, sign(-2))
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["positive", "non-positive"]);
    }

    #[test]
    fn recursion_works() {
        let src = r#"
            fn fact(n: Int) -> Int {
              match n {
                0 -> 1
                _ -> n * fact(n - 1)
              }
            }
            fn main(console: Console) { print(console, int_to_string(fact(5))) }
        "#;
        assert_eq!(run(src).unwrap(), vec!["120"]);
    }

    #[test]
    fn reports_unknown_function() {
        let e = run("fn main() { nope() }").unwrap_err();
        assert!(e.message.contains("unknown function"));
    }

    /// The capability thesis at the language level: a function that was never
    /// handed the Console capability cannot print, even though `print` exists.
    #[test]
    fn function_without_capability_cannot_print() {
        let src = r#"
            fn leak(secret: String) -> Nil { print(secret) }
            fn main(console: Console) { leak("password") }
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
            fn announce(console: Console, who: String) -> Nil {
              print(console, "hello, " <> who)
            }
            fn main(console: Console) { announce(console, "witchy") }
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
            fn main(console: Console, root: Dir) {
              let d = subdir(root, "sub")
              print(console, read(d, "hi.txt"))
            }
        "#;
        assert_eq!(run_in(ok, &root).unwrap(), vec!["hi!"]);

        // Confinement: `..` cannot escape the granted subtree.
        let escape = r#"
            fn main(console: Console, root: Dir) {
              print(console, read(root, "../secret"))
            }
        "#;
        assert!(run_in(escape, &root).is_err());

        // A function with no Dir cannot read (no way to obtain the capability).
        let no_cap = r#"
            fn sneaky() -> String { read(root, "sub/hi.txt") }
            fn main(console: Console, root: Dir) { print(console, sneaky()) }
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
                fn main(console: Console, root: Dir) {
                  let d = subdir(root, "sub")
                  print(console, read(d, "escape"))
                }
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
            fn main(console: Console, net: Net) {{
              let only = restrict(net, "{addr}")
              let s = connect(only, "{addr}")
              send_line(s, "ping")
              print(console, recv_line(s))
            }}
        "#
        );
        assert_eq!(run_with(&ok, ".", vec![addr.clone()]).unwrap(), vec!["ping"]);
        server.join().ok();

        // Denied: connecting to an address not in the allow-list.
        let denied = format!(
            r#"
            fn main(console: Console, net: Net) {{
              let s = connect(net, "10.255.255.1:80")
              send_line(s, "x")
            }}
        "#
        );
        assert!(run_with(&denied, ".", vec![addr.clone()]).is_err());

        // Denied: cannot attenuate to an address not already held.
        let bad_restrict = r#"
            fn main(console: Console, net: Net) {
              let bad = restrict(net, "10.255.255.1:80")
              print(console, "unreachable")
            }
        "#;
        assert!(run_with(bad_restrict, ".", vec![addr]).is_err());
    }

    #[test]
    fn modules_qualified_calls() {
        let strutil = r#"fn shout(name: String) -> String { "HELLO, " <> name }"#;
        let app = r#"
            import strutil
            fn main(console: Console) { print(console, strutil.shout("witchy")) }
        "#;
        assert_eq!(
            run_program(&[("strutil", strutil), ("app", app)], "app").unwrap(),
            vec!["HELLO, witchy"]
        );
    }

    #[test]
    fn library_uses_only_passed_capabilities() {
        // The app chooses to hand the logger its Console.
        let logger = r#"fn log(console: Console, msg: String) { print(console, "[log] " <> msg) }"#;
        let app = r#"
            import logger
            fn main(console: Console) { logger.log(console, "hi") }
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
        let evil = r#"fn steal(secret: String) -> String { print(console, secret) }"#;
        let app = r#"
            import evil
            fn main(console: Console) { print(console, evil.steal("data")) }
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
        let app = r#"fn main(console: Console) { print(console, other.foo()) }"#;
        assert!(run_program(&[("app", app)], "app").is_err());
    }

    #[test]
    fn float_arithmetic() {
        let src = r#"
            fn half(x: Float) -> Float { x / 2.0 }
            fn main(console: Console) { print(console, to_string(half(7.0))) }
        "#;
        assert_eq!(run(src).unwrap(), vec!["3.5"]);
    }

    #[test]
    fn boolean_operators() {
        let src = r#"
            fn classify(n: Int) -> String {
              if n > 0 && n < 10 { "small positive" }
              else if n <= 0 || n >= 100 { "out of range" }
              else { "other" }
            }
            fn main(console: Console) {
              print(console, classify(5))
              print(console, classify(-1))
              print(console, classify(50))
            }
        "#;
        assert_eq!(
            run(src).unwrap(),
            vec!["small positive", "out of range", "other"]
        );
    }

    #[test]
    fn string_stdlib() {
        let src = r#"
            fn main(console: Console) {
              print(console, to_upper("witchy"))
              print(console, int_to_string(string_length("hello")))
              print(console, trim("  hi  "))
              if starts_with("witchy", "wit") { print(console, "yes") } else { print(console, "no") }
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["WITCHY", "5", "hi", "yes"]);
    }

    #[test]
    fn while_loop_and_modulo() {
        let src = r#"
            fn main(console: Console) {
              var i = 1
              var total = 0
              while i <= 5 {
                total = total + i
                i = i + 1
              }
              print(console, int_to_string(total))
              print(console, int_to_string(10 % 3))
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["15", "1"]);
    }

    #[test]
    fn boolean_not_and_short_circuit() {
        let src = r#"
            fn is_zero(n: Int) -> Bool { n == 0 }
            fn main(console: Console) {
              if !is_zero(5) { print(console, "nonzero") } else { print(console, "zero") }
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["nonzero"]);
    }

    #[test]
    fn lists_length_and_index() {
        let src = r#"
            fn main(console: Console) {
              let xs = [10, 20, 30]
              print(console, int_to_string(length(xs)))
              print(console, int_to_string(at(xs, 1)))
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["3", "20"]);
    }

    /// An actor with mutable state and a granted capability handles messages in
    /// order, run-to-completion.
    #[test]
    fn actor_handles_messages_with_state_and_capability() {
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
              send(logger, Log("first"))
              send(logger, Log("second"))
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["[1] first", "[2] second"]);
    }

    /// The capability thesis through the actor boundary: an actor that was
    /// never granted a Console cannot print.
    #[test]
    fn actor_without_capability_cannot_print() {
        let src = r#"
            actor Sneaky {
              on Go(msg: String) { print(msg) }
            }
            fn main(console: Console) {
              let s = spawn Sneaky()
              send(s, Go("exfiltrate"))
            }
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
            actor Printer {
              console: Console
              on Say(msg: String) { print(console, msg) }
            }
            actor Forwarder {
              target: Subject
              on Relay(msg: String) { send(target, Say(msg)) }
            }
            fn main(console: Console) {
              let printer = spawn Printer(console)
              let fwd = spawn Forwarder(printer)
              send(fwd, Relay("relayed hello"))
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["relayed hello"]);
    }

    #[test]
    fn let_bindings_are_immutable() {
        let src = r#"
            fn main(console: Console) {
              let x = 1
              x = 2
            }
        "#;
        let e = run(src).unwrap_err();
        assert!(e.message.contains("immutable"), "got: {}", e.message);
    }

    #[test]
    fn var_bindings_are_mutable() {
        let src = r#"
            fn main(console: Console) {
              var x = 1
              x = x + 41
              print(console, int_to_string(x))
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["42"]);
    }

    /// Hylo-style mutable value semantics: an `inout` parameter mutates the
    /// caller's variable in place — easy mutability, no pointers.
    #[test]
    fn inout_parameter_writes_back_to_caller() {
        let src = r#"
            fn bump(inout n: Int) { n = n + 1 }
            fn main(console: Console) {
              var x = 41
              bump(x)
              print(console, int_to_string(x))
            }
        "#;
        assert_eq!(run(src).unwrap(), vec!["42"]);
    }

    #[test]
    fn inout_requires_a_mutable_variable() {
        let src = r#"
            fn bump(inout n: Int) { n = n + 1 }
            fn main(console: Console) {
              let x = 41
              bump(x)
            }
        "#;
        let e = run(src).unwrap_err();
        assert!(
            e.message.contains("var") || e.message.contains("immutable"),
            "got: {}",
            e.message
        );
    }
}
