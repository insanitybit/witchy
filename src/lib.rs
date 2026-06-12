//! The witchy library: the wasm-safe interpreter front-end (lexer, parser, type
//! checker, linker, and the tree-walking interpreter) plus the pure codegen/
//! format/doc passes. The wasmtime sandbox, the package manager, and the LSP
//! live only in the binary (`main.rs`), so this crate compiles to
//! `wasm32-unknown-unknown` — which is what powers the in-browser playground.
//!
//! Build the browser module with:
//!   cargo build --release --lib --no-default-features \
//!       --target wasm32-unknown-unknown

// Mirror the binary crate's lint posture (these collapse-suggestions hurt the
// readability of the nested capability/pattern checks).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]

pub mod analysis;
pub mod aliases;
pub mod ast;
pub mod capabilities;
pub mod codegen;
pub mod consts;
pub mod derive;
pub mod doc;
pub mod format;
pub mod generators;
pub mod interpreter;
pub mod lexer;
pub mod linker;
pub mod native;
pub mod parser;
pub mod records;
pub mod traits;
pub mod typeck;

/// Resolve a single-source program against the BUNDLED standard library only
/// (no filesystem — the browser has none): parse the entry, then breadth-first
/// load each `import`ed std module from the embedded sources and link them.
pub fn resolve_std_only(src: &str) -> Result<ast::Module, String> {
    use std::collections::{HashSet, VecDeque};
    let entry = parser::parse_module(src).map_err(|e| e.to_string())?;
    let mut modules: Vec<(String, ast::Module)> = vec![("main".to_string(), entry.clone())];
    let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
    let mut queue: VecDeque<ast::Module> = VecDeque::from([entry]);
    while let Some(module) = queue.pop_front() {
        for name in module.imports.clone() {
            if !loaded.insert(name.clone()) {
                continue;
            }
            let source = linker::std_source(&name).ok_or_else(|| {
                let hint = linker::closest_std_module(&name)
                    .map(|m| format!(" — did you mean `import {m}`?"))
                    .unwrap_or_default();
                format!("unknown module `{name}`{hint} (the browser playground has only the bundled std)")
            })?;
            let parsed = parser::parse_module(source).map_err(|e| e.to_string())?;
            queue.push_back(parsed.clone());
            modules.push((name, parsed));
        }
    }
    linker::link(modules, "main").map_err(|e| e.to_string())
}

/// Run a Console-only witchy program and return its printed lines, or a
/// type/parse/runtime error message. This is the function the playground calls;
/// it never touches the filesystem, the clock, or the network (a program that
/// asks for those capabilities type-checks but errors when it tries to use them
/// — the browser build grants none).
pub fn run_source(src: &str) -> Result<Vec<String>, String> {
    let linked = resolve_std_only(src)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    interpreter::run_module(linked, std::path::Path::new("."), Vec::new())
        .map_err(|e| e.message)
}

// --- the browser ABI (no wasm-bindgen; hand-marshaled UTF-8) -----------------
//
// JS writes the source into memory it got from `witchy_alloc`, calls
// `witchy_run(ptr, len)`, and reads a `[u32 little-endian length][utf-8 bytes]`
// result at the returned pointer. The result is tagged: the first line is
// `ok` or `error`, the rest is the output / message.

#[cfg(target_arch = "wasm32")]
mod wasm_abi {
    use std::alloc::{alloc, dealloc, Layout};

    /// Allocate `len` bytes of guest memory for JS to write into.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_alloc(len: usize) -> *mut u8 {
        if len == 0 {
            return std::ptr::null_mut();
        }
        // SAFETY: non-zero len; the matching free is `witchy_free`.
        unsafe { alloc(Layout::from_size_align(len, 1).unwrap()) }
    }

    /// Free a buffer previously returned by `witchy_alloc` / `witchy_run`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_free(ptr: *mut u8, len: usize) {
        if !ptr.is_null() && len != 0 {
            // SAFETY: ptr/len pair came from one of our allocations.
            unsafe { dealloc(ptr, Layout::from_size_align(len, 1).unwrap()) }
        }
    }

    /// Run the source at `ptr[..len]`; return a pointer to a length-prefixed,
    /// tagged UTF-8 result. The caller reads the u32 length, then the bytes,
    /// then frees the whole block (`4 + length` bytes) with `witchy_free`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_run(ptr: *const u8, len: usize) -> *mut u8 {
        // SAFETY: JS hands back the exact ptr/len it wrote via witchy_alloc.
        let src = unsafe { std::slice::from_raw_parts(ptr, len) };
        let src = String::from_utf8_lossy(src).into_owned();
        let body = match super::run_source(&src) {
            Ok(lines) => format!("ok\n{}", lines.join("\n")),
            Err(message) => format!("error\n{message}"),
        };
        let bytes = body.into_bytes();
        let mut out = Vec::with_capacity(4 + bytes.len());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytes);
        let ptr = out.as_mut_ptr();
        std::mem::forget(out); // handed to JS; freed via witchy_free
        ptr
    }
}
