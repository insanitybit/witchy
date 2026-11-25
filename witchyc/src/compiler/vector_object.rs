//! Standalone native object lowering for pure vector functions.
use crate::ast::{Declaration, Expr, Expression, Program, Statement, Type};
use std::{
    collections::HashSet,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn expression(expr: &Expr, params: &[(String, Type)], width: &str) -> Result<String, String> {
    match &expr.kind {
        Expression::Name(name) => params
            .iter()
            .position(|(p, _)| p == name)
            .map(|i| format!("p{i}"))
            .ok_or_else(|| format!("unknown vector {name}")),
        Expression::Call(method, args) => {
            if let Expression::Field(lhs, op) = &method.kind
                && let [rhs] = args.as_slice()
            {
                let a = expression(lhs, params, width)?;
                let b = expression(rhs, params, width)?;
                return Ok(match op.as_str() {
                    "bit_and" | "bit_or" | "bit_xor" => format!("{op}({a}, {b})"),
                    _ => {
                        let function = match op.as_str() {
                            "wrapping_add" => "add",
                            "wrapping_sub" => "sub",
                            "wrapping_mul" => "multiply",
                            "cmp_gt" => "greater",
                            "cmp_eq" => "equal",
                            _ => {
                                return Err(format!(
                                    "unsupported vector operation at {:?}",
                                    expr.span
                                ));
                            }
                        };
                        format!("{function}({width}, {a}, {b})")
                    }
                });
            }
            Err(format!("unsupported vector operation at {:?}", expr.span))
        }
        _ => Err(format!("expected a vector expression at {:?}", expr.span)),
    }
}

fn source(program: &Program) -> Result<String, String> {
    let mut code = String::from(include_str!("vector_object.h"));
    let mut names = HashSet::new();
    for (index, decl) in program.declarations.iter().enumerate() {
        if matches!(decl, Declaration::Trait {name,..} if program.language.contains(name)) {
            continue;
        }
        let Declaration::Function(f) = decl else {
            return Err("expected a vector function".into());
        };
        let Type::Named(name, generics) = &f.result else {
            return Err("expected vector result".into());
        };
        let width = match name.as_str() {
            "U8x16" => "BYTE",
            "U16x8" => "HALF",
            "U32x4" => "WORD",
            "U64x2" => "DOUBLE",
            _ => return Err("expected vector result".into()),
        };
        if !generics.is_empty()
            || !f.generics.is_empty()
            || !f.requires.is_empty()
            || !f.ensures.is_empty()
            || f.suspends
            || f.nopanic.is_some()
            || !f.borrows.is_empty()
            || f.defaults.iter().any(Option::is_some)
            || !f.parameters.iter().all(|(_, ty)| ty == &f.result)
        {
            return Err(format!(
                "{} requires a single vector shape throughout its signature",
                f.name
            ));
        }
        if !f
            .name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || !names.insert(&f.name)
        {
            return Err(format!("invalid or duplicate native symbol {}", f.name));
        }
        let mut params = HashSet::new();
        if !f.parameters.iter().all(|(name, _)| params.insert(name)) {
            return Err(format!("duplicate parameter in {}", f.name));
        }
        let [Statement::Return(Some(value))] = f.body.as_slice() else {
            return Err(format!("{} requires one return expression", f.name));
        };
        let signature = (0..f.parameters.len())
            .map(|i| format!("V p{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let value = expression(value, &f.parameters, width)?;
        let prefix = if cfg!(target_vendor = "apple") {
            "_"
        } else {
            ""
        };
        code.push_str(&format!("V fn{index}({signature}) __asm__(\"{prefix}{}\");\nV fn{index}({signature}) {{ return {value}; }}\n", f.name));
    }
    if names.is_empty() {
        return Err("expected a vector function".into());
    }
    Ok(code)
}

pub fn build_object(input: &Path, path: &Path) -> Result<(), String> {
    if !cfg!(all(
        target_endian = "little",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )) {
        return Err("native SIMD object emission requires AArch64 or x86-64".into());
    }
    if path.canonicalize().ok() == Some(input.canonicalize().map_err(|e| e.to_string())?) {
        return Err("source and output must be different files".into());
    }
    let text = std::fs::read_to_string(input).map_err(|e| e.to_string())?;
    let program = crate::parser::parse(&text).map_err(|e| format!("{}: {}", e.code, e.message))?;
    let code = source(&program)?;
    let mut compiler = if cfg!(target_os = "macos") {
        let mut command = Command::new("xcrun");
        command.arg("clang");
        command
    } else {
        Command::new("clang")
    };
    let mut child = compiler
        .args(["-O2", "-x", "c", "-c", "-", "-o", "-"])
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let write = child.stdin.take().unwrap().write_all(code.as_bytes());
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    write.map_err(|e| e.to_string())?;
    if output.status.success() {
        super::files::publish(path, &output.stdout).map_err(|e| e.to_string())
    } else {
        Err(format!("native object emission failed ({})", output.status))
    }
}
