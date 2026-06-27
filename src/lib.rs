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
pub mod confine;
pub mod async_lower;
pub mod consts;
pub mod comptime;
pub mod derive;
pub mod doc;
pub mod fmt;
pub mod format;
pub mod generators;
/// RFC-0013 capability grant documents (TOML); native-only (uses `serde`/`toml`).
#[cfg(feature = "native")]
pub mod grants;
pub mod interpreter;
pub mod lexer;
pub mod linker;
pub mod native;
pub mod net;
pub mod optimize;
pub mod parser;
pub mod records;
pub mod tagged;
pub mod traits;
pub mod typeck;
pub mod value;
// RFC-0018: the WIR group lives in the `witchy-wir` crate; re-export it so the
// rest of the compiler keeps using `crate::wir::…` paths unchanged.
pub use witchy_wir::{wir, wir_encode, wir_opt, wir_prelude};

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

/// Compile a witchy program to a WebAssembly **binary** the browser's own engine
/// can instantiate: resolve against the bundled std, type-check, codegen to WAT,
/// then assemble with the pure-Rust `wat` crate. This is the codegen path that
/// replaces the in-browser interpreter — the page runs the SAME module `witchy
/// sandbox` would, so the playground is now dev == deploy too. The produced module
/// imports `witchy.print` plus, only if used, the pure helpers below; the page
/// supplies them (capabilities are granted as trapping stubs — the browser has
/// none).
pub fn compile_source(src: &str) -> Result<Vec<u8>, String> {
    let linked = resolve_std_only(src)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    // Compile through the WIR → wasm-binary pipeline (`wasmparser`/`wasm-encoder`,
    // pure Rust, so this runs on EVERY target including the browser playground).
    // A program that doesn't fully lower returns `Ok(None)` → the hard "cannot
    // compile" error below; there is no WAT fallback.
    codegen::compile_module_binary(&linked)
        .map_err(|e| format!("cannot compile to WASM: {e}"))?
        .ok_or_else(|| {
            "cannot compile to WASM: the program reached a construct the compiled backend \
             does not support (an interpreter-only feature?)"
                .to_string()
        })
}

/// The exact float formatting both backends share. The playground's host shim
/// delegates `float_to_str` here so it never reimplements (and so never diverges
/// from) Rust's float `Display`.
pub fn render_float(x: f64) -> String {
    crate::fmt::render_float(x)
}

/// `string.from_code` via the shared native registry — the same `char::from_u32`
/// both backends use. Out-of-range/surrogate becomes U+FFFD.
pub fn string_from_code(cp: i64) -> String {
    native_str("string.from_code", crate::value::NativeValue::Int(cp))
        .unwrap_or_else(|_| "\u{FFFD}".to_string())
}

/// `encoding.*` via the shared native registry (hex/base64), selected by op code
/// (0 hex_encode, 1 hex_decode, 2 base64_encode, 3 base64_decode, 4
/// base64url_of_hex). The playground host shim delegates here.
pub fn encoding(op: i32, input: &str) -> Result<String, String> {
    let name = match op {
        0 => "encoding.hex_encode",
        1 => "encoding.hex_decode",
        2 => "encoding.base64_encode",
        3 => "encoding.base64_decode",
        4 => "encoding.base64url_of_hex",
        _ => return Err(format!("unknown encoding op {op}")),
    };
    native_str(name, crate::value::NativeValue::Str(input.to_string()))
}

fn native_str(name: &str, arg: crate::value::NativeValue) -> Result<String, String> {
    let f = native::lookup(name).ok_or_else(|| format!("{name} is not registered"))?;
    match f(&[arg]).map_err(|e| e.message)? {
        crate::value::NativeValue::Str(s) => Ok(s),
        _ => Err(format!("{name} did not return a String")),
    }
}

fn native_call(name: &str, args: &[&str]) -> Result<crate::value::NativeValue, String> {
    let f = native::lookup(name).ok_or_else(|| format!("{name} is not registered"))?;
    let vals: Vec<crate::value::NativeValue> =
        args.iter().map(|s| crate::value::NativeValue::Str(s.to_string())).collect();
    f(&vals).map_err(|e| e.message)
}

/// SHA-256 / SHA-512 / SHA3-256 of `input` as lowercase hex (op 0/1/2), via the
/// shared native registry. The playground host shim delegates here so a pasted
/// hashing program runs in the browser instead of trapping — the bundled `regex`/
/// `crypto` std modules are native-backed, and the browser has no filesystem
/// sibling to fall back to.
pub fn crypto_hash(op: i32, input: &str) -> String {
    let name = match op {
        0 => "crypto.sha256",
        1 => "crypto.sha512",
        2 => "crypto.sha3_256",
        _ => return String::new(),
    };
    match native_call(name, &[input]) {
        Ok(crate::value::NativeValue::Str(s)) => s,
        _ => String::new(),
    }
}

/// HMAC-SHA256(key, message) as hex.
pub fn hmac_sha256(key: &str, msg: &str) -> String {
    match native_call("crypto.hmac_sha256", &[key, msg]) {
        Ok(crate::value::NativeValue::Str(s)) => s,
        _ => String::new(),
    }
}

/// `regex.match_spans(pattern, text)` — the packed match-span string both backends
/// share; the host shim stages it through `fill_pending` like the native runtime.
pub fn regex_spans(pattern: &str, text: &str) -> String {
    match native_call("regex.match_spans", &[pattern, text]) {
        Ok(crate::value::NativeValue::Str(s)) => s,
        _ => String::new(),
    }
}

/// Signature verification (op 0 ed25519, 1 ecdsa_p256, 2 ecdsa_p256_hex); all are
/// pure (hex inputs, no Secret), so they run in the browser.
pub fn crypto_verify(op: i32, pk: &str, msg: &str, sig: &str) -> bool {
    let name = match op {
        0 => "crypto.ed25519_verify",
        1 => "crypto.ecdsa_p256_verify",
        2 => "crypto.ecdsa_p256_verify_hex",
        _ => return false,
    };
    matches!(native_call(name, &[pk, msg, sig]), Ok(crate::value::NativeValue::Bool(true)))
}

// --- the browser ABI (no wasm-bindgen; hand-marshaled UTF-8) -----------------
//
// JS writes the source into memory it got from `witchy_alloc`, calls
// `witchy_compile(ptr, len)` to get the program's wasm binary, then instantiates
// THAT module on the browser's own WebAssembly engine — the interpreter never
// runs a user program in the browser. During the program's run its `witchy.*`
// host imports call back into the page, which delegates the pure ones
// (`float_to_str`, `string_from_code`, `encoding`) to the exports below so they
// match the native backend byte-for-byte.
//
// Result framing: `witchy_compile` returns `[u32 status][u32 len][payload]`
// (status 0 → wasm bytes, 1 → utf-8 error). The helper exports return
// `[u32 len][payload]` (always succeed; bad input already folds to a sentinel).
// The caller frees each block (`8 + len` or `4 + len`) with `witchy_free`.

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

    /// Free a buffer previously returned by `witchy_alloc` / `witchy_compile` /
    /// the helper exports.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_free(ptr: *mut u8, len: usize) {
        if !ptr.is_null() && len != 0 {
            // SAFETY: ptr/len pair came from one of our allocations.
            unsafe { dealloc(ptr, Layout::from_size_align(len, 1).unwrap()) }
        }
    }

    /// `[u32 status][u32 len][payload]` in a fresh buffer handed to JS.
    fn pack_tagged(status: u32, payload: &[u8]) -> *mut u8 {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&status.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        let ptr = out.as_mut_ptr();
        std::mem::forget(out); // handed to JS; freed via witchy_free
        ptr
    }

    /// `[u32 len][payload]` in a fresh buffer handed to JS.
    fn pack(payload: &[u8]) -> *mut u8 {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        let ptr = out.as_mut_ptr();
        std::mem::forget(out);
        ptr
    }

    /// Compile the source at `ptr[..len]` to a wasm binary; status 0 → bytes,
    /// 1 → error message. Free with `witchy_free(p, 8 + len)`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_compile(ptr: *const u8, len: usize) -> *mut u8 {
        // SAFETY: JS hands back the exact ptr/len it wrote via witchy_alloc.
        let src = unsafe { std::slice::from_raw_parts(ptr, len) };
        let src = String::from_utf8_lossy(src).into_owned();
        match super::compile_source(&src) {
            Ok(binary) => pack_tagged(0, &binary),
            Err(message) => pack_tagged(1, message.as_bytes()),
        }
    }

    /// `float_to_str(x)` → `[u32 len][utf-8]`. Free with `witchy_free(p, 4 + len)`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_render_float(x: f64) -> *mut u8 {
        pack(super::render_float(x).as_bytes())
    }

    /// `string_from_code(cp)` → `[u32 len][utf-8]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_string_from_code(cp: i64) -> *mut u8 {
        pack(super::string_from_code(cp).as_bytes())
    }

    /// `encoding(op, input[..in_len])` → `[u32 len][utf-8]` (a decode error folds
    /// to the empty string, matching how the program would observe a failed op).
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_encoding(op: i32, in_ptr: *const u8, in_len: usize) -> *mut u8 {
        let input = unsafe { std::slice::from_raw_parts(in_ptr, in_len) };
        let input = String::from_utf8_lossy(input).into_owned();
        let out = super::encoding(op, &input).unwrap_or_default();
        pack(out.as_bytes())
    }

    // SAFETY for the next four: each `*_ptr`/`*_len` pair is a slice JS wrote into
    // a `witchy_alloc` buffer just before the call.
    fn raw<'a>(ptr: *const u8, len: usize) -> std::borrow::Cow<'a, str> {
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
    }

    /// `crypto.sha256/sha512/sha3_256(op, input)` → `[u32 len][hex]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_crypto_hash(op: i32, in_ptr: *const u8, in_len: usize) -> *mut u8 {
        pack(super::crypto_hash(op, &raw(in_ptr, in_len)).as_bytes())
    }

    /// `crypto.hmac_sha256(key, msg)` → `[u32 len][hex]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_hmac_sha256(
        k_ptr: *const u8,
        k_len: usize,
        m_ptr: *const u8,
        m_len: usize,
    ) -> *mut u8 {
        pack(super::hmac_sha256(&raw(k_ptr, k_len), &raw(m_ptr, m_len)).as_bytes())
    }

    /// `regex.match_spans(pattern, text)` → `[u32 len][packed spans]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_regex(
        p_ptr: *const u8,
        p_len: usize,
        t_ptr: *const u8,
        t_len: usize,
    ) -> *mut u8 {
        pack(super::regex_spans(&raw(p_ptr, p_len), &raw(t_ptr, t_len)).as_bytes())
    }

    /// Signature verify (op 0 ed25519, 1/2 ecdsa) → 1 if valid, else 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_verify(
        op: i32,
        pk_ptr: *const u8,
        pk_len: usize,
        m_ptr: *const u8,
        m_len: usize,
        s_ptr: *const u8,
        s_len: usize,
    ) -> i32 {
        super::crypto_verify(op, &raw(pk_ptr, pk_len), &raw(m_ptr, m_len), &raw(s_ptr, s_len)) as i32
    }
}
