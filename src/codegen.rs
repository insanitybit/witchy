//! WebAssembly code generation for witchy.
//!
//! This is the first slice of the codegen path: it compiles integer-valued
//! functions (literals, arithmetic, comparisons, `if`/`else`, `let`, calls, and
//! recursion) to a WAT module. `main` is compiled like any other function and
//! its result is printed through the imported `witchy.print_int` capability.
//!
//! The point is unification with the runtime: capability grants stop being a
//! property of hand-written WAT and become a property of *compiled witchy*. A
//! module that imports `print_int` cannot run unless the runtime links that
//! host function in — exactly the security model proven in the spike.
//!
//! Strings, ADTs, and actors are not compiled yet and produce a clear error.

use std::fmt;

use crate::ast::{BinOp, Block, Expr, Function, Item, Module, Stmt, UnOp};

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

/// Compile a module to WAT text. Requires a zero-argument `main` returning Int.
pub fn compile_module(module: &Module) -> Result<String, CodegenError> {
    let mut funcs = Vec::new();
    let mut has_main = false;
    for item in &module.items {
        match item {
            Item::Function(f) => {
                if f.name == "main" {
                    has_main = true;
                    if !f.params.is_empty() {
                        return cerr("codegen `main` must take no arguments");
                    }
                }
                funcs.push(compile_function(f)?);
            }
            Item::Type(_) => {} // erased
            Item::Actor(_) => return cerr("actors are not compiled to WASM yet"),
        }
    }
    if !has_main {
        return cerr("no `main` function to compile");
    }

    let mut wat = String::from("(module\n");
    wat.push_str("  (import \"witchy\" \"print_int\" (func $print_int (param i32)))\n");
    wat.push_str("  (memory (export \"memory\") 1)\n");
    for f in funcs {
        wat.push_str(&f);
    }
    // The capability call: print main's result, then return.
    wat.push_str("  (func (export \"run\")\n    call $main\n    call $print_int)\n");
    wat.push_str(")\n");
    Ok(wat)
}

fn compile_function(f: &Function) -> Result<String, CodegenError> {
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

    let body = compile_block(&f.body)?;
    Ok(format!("{header}{body}  )\n"))
}

fn compile_block(block: &Block) -> Result<String, CodegenError> {
    let mut out = String::new();
    let last = block.stmts.len().saturating_sub(1);
    let mut tail_is_value = false;
    for (i, stmt) in block.stmts.iter().enumerate() {
        match stmt {
            Stmt::Let { name, value, .. } => {
                out.push_str(&compile_expr(value)?);
                out.push_str(&format!("    local.set ${name}\n"));
                tail_is_value = false;
            }
            Stmt::Assign { name, value } => {
                out.push_str(&compile_expr(value)?);
                out.push_str(&format!("    local.set ${name}\n"));
                tail_is_value = false;
            }
            Stmt::Expr(e) => {
                out.push_str(&compile_expr(e)?);
                if i == last {
                    tail_is_value = true;
                } else {
                    out.push_str("    drop\n");
                }
            }
        }
    }
    // A function must leave an i32 on the stack; if the block has no tail value
    // (empty, or ends in a binding), yield 0.
    if !tail_is_value {
        out.push_str("    i32.const 0\n");
    }
    Ok(out)
}

fn compile_expr(expr: &Expr) -> Result<String, CodegenError> {
    match expr {
        Expr::Int(n) => Ok(format!("    i32.const {n}\n")),
        Expr::Bool(b) => Ok(format!("    i32.const {}\n", if *b { 1 } else { 0 })),
        Expr::Var(name) => Ok(format!("    local.get ${name}\n")),
        Expr::Unary { op: UnOp::Neg, expr } => {
            Ok(format!("    i32.const 0\n{}    i32.sub\n", compile_expr(expr)?))
        }
        Expr::Binary { op, lhs, rhs } => {
            let l = compile_expr(lhs)?;
            let r = compile_expr(rhs)?;
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
                BinOp::Concat => {
                    return cerr("string concatenation is not compiled to WASM yet")
                }
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
                compile_expr(cond)?,
                compile_block(then_block)?,
                compile_block(else_block)?
            ))
        }
        Expr::Block(b) => compile_block(b),
        Expr::Call { name, args } => {
            let mut out = String::new();
            for arg in args {
                out.push_str(&compile_expr(arg)?);
            }
            out.push_str(&format!("    call ${name}\n"));
            Ok(out)
        }
        Expr::Str(_) => cerr("string values are not compiled to WASM yet"),
        Expr::Float(_) => cerr("float values are not compiled to WASM yet"),
        Expr::List(_) => cerr("list values are not compiled to WASM yet"),
        Expr::Ctor { name, .. } => cerr(format!("constructor `{name}` is not compiled to WASM yet")),
        Expr::Match { .. } => cerr("`match` is not compiled to WASM yet"),
        Expr::Spawn { .. } => cerr("`spawn` is not compiled to WASM yet"),
    }
}

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
    use wasmtime::{Engine, Linker, Module as WtModule, Store};

    /// Compile a program, run it on a bare wasmtime instance with a capturing
    /// `print_int`, and return what `main` printed.
    fn run_compiled(src: &str) -> i32 {
        let module = parse_module(src).expect("parse");
        let wat = compile_module(&module).expect("compile");

        let engine = Engine::default();
        let wt_module = WtModule::new(&engine, &wat).expect("valid wat");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_int", move |n: i32| {
                *sink.lock().unwrap() = Some(n);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt_module).unwrap();
        let run = instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap();
        run.call(&mut store, ()).unwrap();
        let value = captured.lock().unwrap().take();
        value.expect("main should have printed a value")
    }

    #[test]
    fn compiles_arithmetic() {
        assert_eq!(run_compiled("fn main() -> Int { 1 + 2 * 3 }"), 7);
    }

    #[test]
    fn compiles_calls_and_let() {
        let src = r#"
            fn double(n: Int) -> Int { n * 2 }
            fn main() -> Int {
              let a = double(21)
              a
            }
        "#;
        assert_eq!(run_compiled(src), 42);
    }

    #[test]
    fn compiles_if_and_recursion() {
        let src = r#"
            fn fib(n: Int) -> Int {
              if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
            }
            fn main() -> Int { fib(10) }
        "#;
        assert_eq!(run_compiled(src), 55);
    }

    #[test]
    fn full_program_value() {
        let src = r#"
            fn double(n: Int) -> Int { n * 2 }
            fn fib(n: Int) -> Int {
              if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
            }
            fn main() -> Int {
              let a = double(21)
              let b = fib(10)
              a + b
            }
        "#;
        assert_eq!(run_compiled(src), 97);
    }

    #[test]
    fn rejects_unsupported_strings() {
        let src = r#"fn main() -> Int { "nope" }"#;
        assert!(compile_module(&parse_module(src).unwrap()).is_err());
    }
}
