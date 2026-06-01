//! WebAssembly code generation for witchy.
//!
//! Compiles witchy functions to a WAT module. Two value representations, both
//! `i32` at the WASM level:
//!   * integers (and capability placeholders) are plain `i32`;
//!   * strings are an `i32` pointer to a length-prefixed record in linear
//!     memory: `[len: i32][utf8 bytes...]`.
//!
//! Capabilities remain host imports: `print` (string output) and `print_int`
//! are linked by the runtime only when granted, so an ungranted compiled module
//! cannot instantiate — the same security model as the spike, now on compiled
//! witchy. `main`'s integer result is auto-printed via `print_int`; otherwise
//! `main` performs its own output through `print`.
//!
//! Not yet compiled: floats, lists, `int_to_string`, ADT constructors, `match`,
//! actors — each produces a clear error.

use std::fmt;

use crate::ast::{BinOp, Block, Expr, Function, Item, Module, Stmt, Type, UnOp};

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

/// Static data starts at offset 8 (leaving a little scratch space at the base).
const DATA_BASE: u32 = 8;

struct Codegen {
    /// Interned string literals: (text, offset of its length-prefixed record).
    strings: Vec<(String, u32)>,
    next_offset: u32,
    uses_print: bool,
    uses_print_int: bool,
    uses_concat: bool,
}

impl Codegen {
    fn new() -> Self {
        Self {
            strings: Vec::new(),
            next_offset: DATA_BASE,
            uses_print: false,
            uses_print_int: false,
            uses_concat: false,
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
                Stmt::Let { name, value, .. } | Stmt::Assign { name, value } => {
                    out.push_str(&self.compile_expr(value)?);
                    out.push_str(&format!("    local.set ${name}\n"));
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
            Expr::Var(name) => Ok(format!("    local.get ${name}\n")),
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
            // print(console, message): the console argument is a capability
            // placeholder; the authority is the `print` host import itself.
            ("print", 2) => {
                self.uses_print = true;
                let msg = self.compile_expr(&args[1])?;
                // call $print_str, then leave Nil (0) as the expression's value.
                Ok(format!("{msg}    call $print_str\n    i32.const 0\n"))
            }
            ("int_to_string", _) => {
                cerr("int_to_string is not compiled to WASM yet (interpreter only)")
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

/// Compile a module to WAT. Requires a `main` returning Int or Nil; `main` may
/// take a single capability parameter (e.g. `Console`).
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
            Item::Actor(_) => return cerr("actors are not compiled to WASM yet"),
        }
    }
    if !has_main {
        return cerr("no `main` function to compile");
    }
    if main_returns_int {
        cg.uses_print_int = true;
    }

    let need_heap = cg.uses_concat || !cg.strings.is_empty();

    let mut wat = String::from("(module\n");
    if cg.uses_print {
        wat.push_str("  (import \"witchy\" \"print\" (func $print (param i32 i32)))\n");
    }
    if cg.uses_print_int {
        wat.push_str("  (import \"witchy\" \"print_int\" (func $print_int (param i32)))\n");
    }
    wat.push_str("  (memory (export \"memory\") 1)\n");

    for (s, off) in &cg.strings {
        wat.push_str(&data_segment(*off, s));
    }
    if need_heap {
        wat.push_str(&format!(
            "  (global $heap (mut i32) (i32.const {}))\n",
            cg.next_offset
        ));
        wat.push_str(CONCAT_WAT);
    }
    if cg.uses_print {
        wat.push_str(PRINT_STR_WAT);
    }

    wat.push_str(&func_wat);

    // Entry point. Supply capability placeholders for main's parameter(s), call
    // main, then either print its Int result or discard it.
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

/// A length-prefixed string record emitted as a `\HH`-escaped data segment.
fn data_segment(off: u32, s: &str) -> String {
    let mut bytes = (s.len() as u32).to_le_bytes().to_vec();
    bytes.extend_from_slice(s.as_bytes());
    let escaped: String = bytes.iter().map(|b| format!("\\{b:02x}")).collect();
    format!("  (data (i32.const {off}) \"{escaped}\")\n")
}

/// Concatenate two string records into a freshly bump-allocated record.
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

/// Print a string record via the `print` host capability (ptr, len).
const PRINT_STR_WAT: &str = r#"  (func $print_str (param $s i32)
    local.get $s i32.const 4 i32.add
    local.get $s i32.load
    call $print)
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
    use crate::parser::parse_module;
    use std::sync::{Arc, Mutex};
    use wasmtime::{Caller, Engine, Linker, Module as WtModule, Store};

    /// Run an integer program with a capturing `print_int`.
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

    /// Run a program that prints strings, capturing them from linear memory.
    fn run_str(src: &str) -> Vec<String> {
        let module = parse_module(src).expect("parse");
        let wat = compile_module(&module).expect("compile");
        let engine = Engine::default();
        let wt = WtModule::new(&engine, &wat).expect("valid wat");
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
                    sink.lock().unwrap().push(String::from_utf8_lossy(bytes).into_owned());
                },
            )
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
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
    fn compiles_if_and_recursion() {
        let src = r#"
            fn fib(n: Int) -> Int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
            fn main() -> Int { fib(10) }
        "#;
        assert_eq!(run_int(src), 55);
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
    fn compiles_string_literal_print() {
        let src = r#"fn main(console: Console) { print(console, "hello") }"#;
        assert_eq!(run_str(src), vec!["hello"]);
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
    fn nested_concatenation() {
        let src = r#"
            fn main(console: Console) {
              print(console, "a" <> "b" <> "c" <> "d")
            }
        "#;
        assert_eq!(run_str(src), vec!["abcd"]);
    }

    #[test]
    fn rejects_unsupported_floats() {
        let src = r#"fn main() -> Int { let x = 1.5 0 }"#;
        assert!(compile_module(&parse_module(src).unwrap()).is_err());
    }
}
