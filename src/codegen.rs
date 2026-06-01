//! WebAssembly code generation for witchy.
//!
//! Compiles witchy functions AND actors to WAT modules. Two value
//! representations, both `i32` at the WASM level:
//!   * integers (and capability placeholders) are plain `i32`;
//!   * strings are an `i32` pointer to a length-prefixed record in linear
//!     memory: `[len: i32][utf8 bytes...]`.
//!
//! Capabilities remain host imports (`print`, `print_int`) that the runtime
//! links only when granted, so an ungranted compiled module cannot instantiate.
//!
//! An actor compiles to its own module: each non-capability `Int` field becomes
//! a mutable WASM global (its state, persisting across messages), capability
//! fields are erased (their authority is the host import), and each `on` handler
//! becomes an exported function the host calls to deliver a message.
//!
//! Not yet compiled: floats, lists, ADT constructors, `match`, string/Subject
//! message parameters, and `send` between compiled actors — each errors clearly.

use std::collections::HashSet;
use std::fmt;

use crate::ast::{ActorDef, BinOp, Block, Expr, Function, Item, Module, Stmt, Type, UnOp};

#[derive(Debug, Clone, PartialEq)]
pub struct CodegenError {
    pub message: String,
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "codegen error: {}", self.message)
    }
}

impl std::error::Error for CodegenError {}

fn cerr<T>(message: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError {
        message: message.into(),
    })
}

const DATA_BASE: u32 = 8;

struct Codegen {
    strings: Vec<(String, u32)>,
    next_offset: u32,
    uses_print: bool,
    uses_print_int: bool,
    uses_concat: bool,
    uses_int_to_string: bool,
    /// Names that resolve to mutable WASM globals (actor state).
    globals: HashSet<String>,
    /// Capability field names (erased; referencing one yields a placeholder 0).
    cap_fields: HashSet<String>,
}

impl Codegen {
    fn new() -> Self {
        Self {
            strings: Vec::new(),
            next_offset: DATA_BASE,
            uses_print: false,
            uses_print_int: false,
            uses_concat: false,
            uses_int_to_string: false,
            globals: HashSet::new(),
            cap_fields: HashSet::new(),
        }
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some((_, off)) = self.strings.iter().find(|(t, _)| t == s) {
            return *off;
        }
        let off = self.next_offset;
        self.next_offset += 4 + s.len() as u32;
        self.strings.push((s.to_string(), off));
        off
    }

    fn need_heap(&self) -> bool {
        self.uses_concat || self.uses_int_to_string || !self.strings.is_empty()
    }

    fn emit_imports(&self) -> String {
        let mut s = String::new();
        if self.uses_print {
            s.push_str("  (import \"witchy\" \"print\" (func $print (param i32 i32)))\n");
        }
        if self.uses_print_int {
            s.push_str("  (import \"witchy\" \"print_int\" (func $print_int (param i32)))\n");
        }
        s
    }

    /// Data segments, the heap global, any extra (actor state) globals, and the
    /// runtime helper functions — emitted in valid module-field order (globals
    /// before functions).
    fn emit_data_globals_helpers(&self, extra_globals: &str) -> String {
        let mut s = String::new();
        for (text, off) in &self.strings {
            s.push_str(&data_segment(*off, text));
        }
        if self.need_heap() {
            s.push_str(&format!(
                "  (global $heap (mut i32) (i32.const {}))\n",
                self.next_offset
            ));
        }
        s.push_str(extra_globals);
        if self.need_heap() {
            s.push_str(CONCAT_WAT);
        }
        if self.uses_print {
            s.push_str(PRINT_STR_WAT);
        }
        if self.uses_int_to_string {
            s.push_str(INT_TO_STRING_WAT);
        }
        s
    }

    fn compile_function(&mut self, f: &Function) -> Result<String, CodegenError> {
        let mut header = format!("  (func ${} ", f.name);
        for p in &f.params {
            header.push_str(&format!("(param ${} i32) ", p.name));
        }
        header.push_str("(result i32)\n");

        let mut lets = Vec::new();
        collect_let_names(&f.body, &mut lets);
        for name in &lets {
            header.push_str(&format!("    (local ${name} i32)\n"));
        }

        let body = self.compile_block(&f.body)?;
        Ok(format!("{header}{body}  )\n"))
    }

    fn compile_block(&mut self, block: &Block) -> Result<String, CodegenError> {
        let mut out = String::new();
        let last = block.stmts.len().saturating_sub(1);
        let mut tail_is_value = false;
        for (i, stmt) in block.stmts.iter().enumerate() {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    out.push_str(&self.compile_expr(value)?);
                    out.push_str(&format!("    local.set ${name}\n"));
                    tail_is_value = false;
                }
                Stmt::Assign { name, value } => {
                    out.push_str(&self.compile_expr(value)?);
                    if self.globals.contains(name) {
                        out.push_str(&format!("    global.set ${name}\n"));
                    } else {
                        out.push_str(&format!("    local.set ${name}\n"));
                    }
                    tail_is_value = false;
                }
                Stmt::Expr(e) => {
                    out.push_str(&self.compile_expr(e)?);
                    if i == last {
                        tail_is_value = true;
                    } else {
                        out.push_str("    drop\n");
                    }
                }
            }
        }
        if !tail_is_value {
            out.push_str("    i32.const 0\n");
        }
        Ok(out)
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<String, CodegenError> {
        match expr {
            Expr::Int(n) => Ok(format!("    i32.const {n}\n")),
            Expr::Bool(b) => Ok(format!("    i32.const {}\n", if *b { 1 } else { 0 })),
            Expr::Str(s) => {
                let off = self.intern(s);
                Ok(format!("    i32.const {off}\n"))
            }
            Expr::Var(name) => {
                if self.cap_fields.contains(name) {
                    Ok("    i32.const 0\n".to_string())
                } else if self.globals.contains(name) {
                    Ok(format!("    global.get ${name}\n"))
                } else {
                    Ok(format!("    local.get ${name}\n"))
                }
            }
            Expr::Unary { op: UnOp::Neg, expr } => Ok(format!(
                "    i32.const 0\n{}    i32.sub\n",
                self.compile_expr(expr)?
            )),
            Expr::Binary { op, lhs, rhs } => {
                if *op == BinOp::Concat {
                    self.uses_concat = true;
                    let l = self.compile_expr(lhs)?;
                    let r = self.compile_expr(rhs)?;
                    return Ok(format!("{l}{r}    call $concat\n"));
                }
                let l = self.compile_expr(lhs)?;
                let r = self.compile_expr(rhs)?;
                let opcode = match op {
                    BinOp::Add => "i32.add",
                    BinOp::Sub => "i32.sub",
                    BinOp::Mul => "i32.mul",
                    BinOp::Div => "i32.div_s",
                    BinOp::Eq => "i32.eq",
                    BinOp::NotEq => "i32.ne",
                    BinOp::Lt => "i32.lt_s",
                    BinOp::LtEq => "i32.le_s",
                    BinOp::Gt => "i32.gt_s",
                    BinOp::GtEq => "i32.ge_s",
                    BinOp::Concat => unreachable!(),
                };
                Ok(format!("{l}{r}    {opcode}\n"))
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                let Some(else_block) = else_block else {
                    return cerr("an `if` used as a value needs an `else` branch");
                };
                Ok(format!(
                    "{}    if (result i32)\n{}    else\n{}    end\n",
                    self.compile_expr(cond)?,
                    self.compile_block(then_block)?,
                    self.compile_block(else_block)?
                ))
            }
            Expr::Block(b) => self.compile_block(b),
            Expr::Call { name, args } => self.compile_call(name, args),
            Expr::Float(_) => cerr("float values are not compiled to WASM yet"),
            Expr::List(_) => cerr("list values are not compiled to WASM yet"),
            Expr::Ctor { name, .. } => {
                cerr(format!("constructor `{name}` is not compiled to WASM yet"))
            }
            Expr::Match { .. } => cerr("`match` is not compiled to WASM yet"),
            Expr::Spawn { .. } => cerr("`spawn` is not compiled to WASM yet"),
        }
    }

    fn compile_call(&mut self, name: &str, args: &[Expr]) -> Result<String, CodegenError> {
        match (name, args.len()) {
            ("print", 2) => {
                self.uses_print = true;
                let msg = self.compile_expr(&args[1])?;
                Ok(format!("{msg}    call $print_str\n    i32.const 0\n"))
            }
            ("int_to_string", 1) => {
                self.uses_int_to_string = true;
                let arg = self.compile_expr(&args[0])?;
                Ok(format!("{arg}    call $int_to_string\n"))
            }
            ("send", _) | ("spawn", _) => {
                cerr(format!("`{name}` is not compiled to WASM yet"))
            }
            _ => {
                let mut out = String::new();
                for arg in args {
                    out.push_str(&self.compile_expr(arg)?);
                }
                out.push_str(&format!("    call ${name}\n"));
                Ok(out)
            }
        }
    }
}

/// Compile a module's functions to WAT. Requires a `main` returning Int or Nil;
/// `main` may take a single capability parameter.
pub fn compile_module(module: &Module) -> Result<String, CodegenError> {
    let mut cg = Codegen::new();
    let mut func_wat = String::new();
    let mut main_params = 0usize;
    let mut main_returns_int = false;
    let mut has_main = false;

    for item in &module.items {
        match item {
            Item::Function(f) => {
                if f.name == "main" {
                    has_main = true;
                    main_params = f.params.len();
                    main_returns_int = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Int");
                    if main_params > 1 {
                        return cerr("codegen `main` may take at most one (capability) argument");
                    }
                }
                func_wat.push_str(&cg.compile_function(f)?);
            }
            Item::Type(_) => {}
            Item::Actor(_) => return cerr("use compile_actor_module to compile an actor"),
        }
    }
    if !has_main {
        return cerr("no `main` function to compile");
    }
    if main_returns_int {
        cg.uses_print_int = true;
    }

    let mut wat = String::from("(module\n");
    wat.push_str(&cg.emit_imports());
    wat.push_str("  (memory (export \"memory\") 1)\n");
    wat.push_str(&cg.emit_data_globals_helpers(""));
    wat.push_str(&func_wat);

    wat.push_str("  (func (export \"run\")\n");
    for _ in 0..main_params {
        wat.push_str("    i32.const 0\n");
    }
    wat.push_str("    call $main\n");
    if main_returns_int {
        wat.push_str("    call $print_int)\n");
    } else {
        wat.push_str("    drop)\n");
    }
    wat.push_str(")\n");
    Ok(wat)
}

/// Compile a single actor to its own WASM module. Int fields with literal
/// initializers become mutable globals (state); capability fields are erased;
/// each handler becomes an exported function.
pub fn compile_actor_module(actor: &ActorDef) -> Result<String, CodegenError> {
    let mut cg = Codegen::new();

    let mut state_globals = String::new();
    for field in &actor.fields {
        let Type::Named(tname, _) = &field.ty;
        if tname == "Console" || tname == "Subject" {
            cg.cap_fields.insert(field.name.clone());
            continue;
        }
        if tname != "Int" {
            return cerr(format!(
                "actor field `{}`: only Int state and capability fields compile yet",
                field.name
            ));
        }
        let init = match &field.init {
            Some(Expr::Int(n)) => *n,
            Some(_) => return cerr(format!("field `{}`: initializer must be an Int literal", field.name)),
            None => {
                return cerr(format!(
                    "field `{}`: Int state needs an initializer in codegen",
                    field.name
                ))
            }
        };
        cg.globals.insert(field.name.clone());
        state_globals.push_str(&format!(
            "  (global ${} (mut i32) (i32.const {init}))\n",
            field.name
        ));
    }

    let mut handler_wat = String::new();
    for h in &actor.handlers {
        for p in &h.params {
            if !matches!(&p.ty, Some(Type::Named(t, _)) if t == "Int") {
                return cerr(format!(
                    "handler `{}` param `{}`: only Int message parameters compile yet",
                    h.message, p.name
                ));
            }
        }
        let mut header = format!("  (func (export \"{}\") ", h.message);
        for p in &h.params {
            header.push_str(&format!("(param ${} i32) ", p.name));
        }
        header.push('\n');
        let mut lets = Vec::new();
        collect_let_names(&h.body, &mut lets);
        for name in &lets {
            header.push_str(&format!("    (local ${name} i32)\n"));
        }
        let body = cg.compile_block(&h.body)?;
        // Handlers return nothing; discard the block's trailing value.
        handler_wat.push_str(&format!("{header}{body}    drop\n  )\n"));
    }

    let mut wat = String::from("(module\n");
    wat.push_str(&cg.emit_imports());
    wat.push_str("  (memory (export \"memory\") 1)\n");
    wat.push_str(&cg.emit_data_globals_helpers(&state_globals));
    wat.push_str(&handler_wat);
    wat.push_str(")\n");
    Ok(wat)
}

fn data_segment(off: u32, s: &str) -> String {
    let mut bytes = (s.len() as u32).to_le_bytes().to_vec();
    bytes.extend_from_slice(s.as_bytes());
    let escaped: String = bytes.iter().map(|b| format!("\\{b:02x}")).collect();
    format!("  (data (i32.const {off}) \"{escaped}\")\n")
}

const CONCAT_WAT: &str = r#"  (func $concat (param $a i32) (param $b i32) (result i32)
    (local $alen i32) (local $blen i32) (local $res i32)
    local.get $a i32.load local.set $alen
    local.get $b i32.load local.set $blen
    global.get $heap local.set $res
    local.get $res local.get $alen local.get $blen i32.add i32.store
    local.get $res i32.const 4 i32.add
    local.get $a i32.const 4 i32.add
    local.get $alen
    memory.copy
    local.get $res i32.const 4 i32.add local.get $alen i32.add
    local.get $b i32.const 4 i32.add
    local.get $blen
    memory.copy
    local.get $res i32.const 4 i32.add local.get $alen i32.add local.get $blen i32.add
    global.set $heap
    local.get $res)
"#;

const PRINT_STR_WAT: &str = r#"  (func $print_str (param $s i32)
    local.get $s i32.const 4 i32.add
    local.get $s i32.load
    call $print)
"#;

/// Non-negative integer to a string record. Negative inputs are out of scope.
const INT_TO_STRING_WAT: &str = r#"  (func $int_to_string (param $n i32) (result i32)
    (local $tmp i32) (local $ndigits i32) (local $res i32) (local $p i32)
    local.get $n
    i32.eqz
    if (result i32)
      global.get $heap local.set $res
      local.get $res i32.const 1 i32.store
      local.get $res i32.const 4 i32.add i32.const 48 i32.store8
      local.get $res i32.const 5 i32.add global.set $heap
      local.get $res
    else
      local.get $n local.set $tmp
      i32.const 0 local.set $ndigits
      block $b1
        loop $l1
          local.get $tmp i32.eqz br_if $b1
          local.get $ndigits i32.const 1 i32.add local.set $ndigits
          local.get $tmp i32.const 10 i32.div_s local.set $tmp
          br $l1
        end
      end
      global.get $heap local.set $res
      local.get $res local.get $ndigits i32.store
      local.get $res i32.const 4 i32.add local.get $ndigits i32.add i32.const 1 i32.sub local.set $p
      local.get $n local.set $tmp
      block $b2
        loop $l2
          local.get $tmp i32.eqz br_if $b2
          local.get $p
          local.get $tmp i32.const 10 i32.rem_s i32.const 48 i32.add
          i32.store8
          local.get $p i32.const 1 i32.sub local.set $p
          local.get $tmp i32.const 10 i32.div_s local.set $tmp
          br $l2
        end
      end
      local.get $res i32.const 4 i32.add local.get $ndigits i32.add global.set $heap
      local.get $res
    end)
"#;

fn collect_let_names(block: &Block, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                out.push(name.clone());
                collect_let_names_expr(value, out);
            }
            Stmt::Assign { value, .. } => collect_let_names_expr(value, out),
            Stmt::Expr(e) => collect_let_names_expr(e, out),
        }
    }
}

fn collect_let_names_expr(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::If {
            then_block,
            else_block,
            ..
        } => {
            collect_let_names(then_block, out);
            if let Some(b) = else_block {
                collect_let_names(b, out);
            }
        }
        Expr::Block(b) => collect_let_names(b, out),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Item;
    use crate::parser::parse_module;
    use std::sync::{Arc, Mutex};
    use wasmtime::{Caller, Engine, Linker, Module as WtModule, Store};

    fn run_int(src: &str) -> i32 {
        let module = parse_module(src).expect("parse");
        let wat = compile_module(&module).expect("compile");
        let engine = Engine::default();
        let wt = WtModule::new(&engine, &wat).expect("valid wat");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_int", move |n: i32| {
                *sink.lock().unwrap() = Some(n);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().take().expect("printed a value")
    }

    /// Build a wasmtime instance whose `print` captures strings from memory.
    fn instantiate_with_print(
        wat: &str,
    ) -> (Store<()>, wasmtime::Instance, Arc<Mutex<Vec<String>>>) {
        let engine = Engine::default();
        let wt = WtModule::new(&engine, wat).expect("valid wat");
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = Linker::new(&engine);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap(
                "witchy",
                "print",
                move |mut caller: Caller<'_, ()>, ptr: i32, len: i32| {
                    let mem = caller.get_export("memory").unwrap().into_memory().unwrap();
                    let data = mem.data(&caller);
                    let bytes = &data[ptr as usize..(ptr + len) as usize];
                    sink.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(bytes).into_owned());
                },
            )
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        (store, instance, captured)
    }

    fn run_str(src: &str) -> Vec<String> {
        let module = parse_module(src).expect("parse");
        let wat = compile_module(&module).expect("compile");
        let (mut store, instance, captured) = instantiate_with_print(&wat);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        let out = captured.lock().unwrap().clone();
        out
    }

    #[test]
    fn compiles_arithmetic() {
        assert_eq!(run_int("fn main() -> Int { 1 + 2 * 3 }"), 7);
    }

    #[test]
    fn full_int_program() {
        let src = r#"
            fn double(n: Int) -> Int { n * 2 }
            fn fib(n: Int) -> Int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
            fn main() -> Int { let a = double(21) let b = fib(10) a + b }
        "#;
        assert_eq!(run_int(src), 97);
    }

    #[test]
    fn compiles_string_concatenation() {
        let src = r#"
            fn shout(name: String) -> String { "hello, " <> name }
            fn main(console: Console) { print(console, shout("witchy")) }
        "#;
        assert_eq!(run_str(src), vec!["hello, witchy"]);
    }

    #[test]
    fn compiles_int_to_string() {
        let src = r#"fn main(console: Console) { print(console, int_to_string(12345)) }"#;
        assert_eq!(run_str(src), vec!["12345"]);
    }

    #[test]
    fn int_to_string_handles_zero() {
        let src = r#"fn main(console: Console) { print(console, int_to_string(0)) }"#;
        assert_eq!(run_str(src), vec!["0"]);
    }

    /// The headline: an actor compiled to its own WASM VM, with Int state in a
    /// global and a Console capability, handling messages run-to-completion.
    #[test]
    fn compiles_a_stateful_actor() {
        let src = r#"
            actor Counter {
              console: Console
              var count: Int = 0
              on Tick() {
                count = count + 1
                print(console, "count is " <> int_to_string(count))
              }
            }
        "#;
        let module = parse_module(src).unwrap();
        let Item::Actor(actor) = &module.items[0] else {
            panic!("expected actor");
        };
        let wat = compile_actor_module(actor).unwrap();
        let (mut store, instance, captured) = instantiate_with_print(&wat);
        let tick = instance.get_typed_func::<(), ()>(&mut store, "Tick").unwrap();
        tick.call(&mut store, ()).unwrap();
        tick.call(&mut store, ()).unwrap();
        tick.call(&mut store, ()).unwrap();
        assert_eq!(
            *captured.lock().unwrap(),
            vec!["count is 1", "count is 2", "count is 3"]
        );
    }
}
