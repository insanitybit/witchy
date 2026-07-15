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
#![deny(unsafe_code)]

// RFC-0018: AST→WIR lowering (codegen) + its uniqueness analysis live in the
// `witchy-lower` crate.
pub use witchy_lower::{analysis, codegen, escape};
// RFC-0018: the front-end + AST-level base layer lives in the `witchy-syntax`
// crate; re-export so the rest of the compiler keeps using
// `crate::{ast,lexer,parser,aliases,consts,fmt,…}::…` paths unchanged.
pub use witchy_syntax::{
    aliases, ast, async_lower, build_entry, consts, derive, doc, fmt, format, generators,
    lambda_scan, lexer, linker, opt, optimize, parser, records, reflect,
};
// RFC-0030: deterministic optimization counters (`witchy stats`) — native-only
// (needs the wasmtime sandbox to run a program and read its counters).
#[cfg(feature = "native")]
pub mod stats;
// RFC-0018: footprint analysis + grant docs live in the `witchy-caps` crate.
pub use witchy_caps::capabilities;
pub mod artifact;
#[cfg(test)]
mod capabilities_tests;
// RFC-0018: runtime values + the capability host live in `witchy-runtime`
// (wasm-safe); the wasmtime sandbox `runtime` is native-only.
pub use witchy_runtime::{confine, native, net, value};
#[cfg(feature = "native")]
pub use witchy_runtime::runtime;
/// RFC-0013 capability grant documents (TOML); native-only — re-exported from
/// the `witchy-caps` crate.
#[cfg(feature = "native")]
pub use witchy_caps::grants;
// RFC-0018: the reference interpreter (parity oracle) + compile-time evaluation
// live in the `witchy-interp` crate.
pub use witchy_interp::{comptime, interpreter, pipeline, tagged};
// RFC-0018: type checking + trait resolution live in the `witchy-types` crate.
pub use witchy_types::{traits, typeck};
// RFC-0018: the WIR group lives in the `witchy-wir` crate; re-export it so the
// rest of the compiler keeps using `crate::wir::…` paths unchanged.
pub use witchy_wir::{wir, wir_encode, wir_helpers, wir_opt, wir_prelude};

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
    pipeline::link(modules, "main").map_err(|e| e.to_string())
}

/// Whether a linked function originated in the entry file. The linker keeps the
/// entry module's `main` unqualified and qualifies its other functions with the
/// entry stem; linked-in modules carry a different prefix.
pub fn is_entry_function(name: &str, entry_stem: &str) -> bool {
    name == "main"
        || name.starts_with("main::<")
        || name.starts_with(&format!("{entry_stem}."))
}

/// Enforce the source file's declared performance mode after linking and type
/// checking. This lives in the wasm-safe library so CLI, LSP, and browser
/// compilation apply one contract.
pub fn enforce_performance_modes(linked: &ast::Module, entry_stem: &str) -> Result<(), String> {
    let source_modes: Vec<&str> = linked
        .modes
        .iter()
        .filter(|mode| !mode.starts_with('@'))
        .map(String::as_str)
        .collect();
    if source_modes.is_empty() {
        return Ok(());
    }

    let mode_names = source_modes.join(", ");
    let mut errors = Vec::new();

    for (func, cliff) in analysis::module_cliffs(linked) {
        if !is_entry_function(&func, entry_stem) {
            continue;
        }
        errors.push(format!(
            "error: in `{func}` (line {}): `{}` is rebuilt by copy on every \
             iteration of this loop — it is {} [mode {}]\n  keep `{}` on the \
             in-place path: certify helper calls with `let`/`own` so they do \
             not alias it out, and do not share it mid-loop",
            cliff.line, cliff.var, cliff.reason, mode_names, cliff.var,
        ));
    }

    for miss in analysis::module_no_copy_misses(linked) {
        if !is_entry_function(&miss.function, entry_stem) {
            continue;
        }
        let advice = if miss.reason.contains("first-class call ABI") {
            "call the function directly so its capacity token stays in the compiled ABI, or use normal mode for the copy-correct indirect call".to_string()
        } else {
            format!(
                "keep `{}` uniquely owned: initialize it from fresh storage or a `var` parameter typed `unique`/`local unique`, and do not alias or loan it before this call",
                miss.var
            )
        };
        errors.push(format!(
            "error: in `{}` (line {}): `{}` cannot satisfy the no-copy `var` \
             contract of `{}` — {} [mode {}]\n  {}",
            miss.function,
            miss.line,
            miss.var,
            miss.callee,
            miss.reason,
            mode_names,
            advice,
        ));
    }

    for item in &linked.items {
        let ast::Item::Function(function) = item else { continue };
        if !is_entry_function(&function.name, entry_stem) {
            continue;
        }
        for param in &function.params {
            if param.convention == ast::Convention::Let && ownership_relevant(&param.ty) {
                errors.push(format!(
                    "error: in `{}`: parameter `{}` has no ownership convention — \
                     `mode {}` requires an explicit `let` (read-only borrow), `own` \
                     (consumed), or `var` (mutated in place)",
                    function.name, param.name, mode_names,
                ));
            }
        }
    }

    if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
}

/// Whether an ownership convention changes generated code for this parameter.
/// Qualifiers refine the contract but do not hide the underlying heap type.
pub fn ownership_relevant(ty: &Option<ast::Type>) -> bool {
    ty.as_ref().is_some_and(ownership_relevant_type)
}

fn ownership_relevant_type(ty: &ast::Type) -> bool {
    match ty {
        ast::Type::Named(name, _) => {
            !matches!(name.as_str(), "Int" | "Float" | "Bool" | "Duration")
                && !capabilities::is_capability_type_name(name)
        }
        ast::Type::Tuple(_) => true,
        ast::Type::Fn(_, _, _) => false,
        ast::Type::Qualified(_, inner) => ownership_relevant_type(inner),
    }
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
    enforce_performance_modes(&linked, "main")?;
    // Compile through the WIR → wasm-binary pipeline (`wasmparser`/`wasm-encoder`,
    // pure Rust, so this runs on EVERY target including the browser playground).
    // A program that doesn't fully lower returns `Ok(None)` → the hard "cannot
    // compile" error below; there is no WAT fallback.
    let bytes = codegen::compile_module_binary(&linked)
        .map_err(|e| format!("cannot compile to WASM: {e}"))?
        .ok_or_else(|| {
            "cannot compile to WASM: the program reached a construct the compiled backend \
             does not support (an interpreter-only feature?)"
                .to_string()
        })?;
    Ok(artifact::embed_launch_contract(bytes, &linked))
}

#[cfg(test)]
mod performance_mode_tests {
    #[test]
    fn browser_compile_enforces_no_copy_contracts() {
        let error = super::compile_source(
            r#"mode opt

import dict

fn main(console: Console):
    var d = dict.new()
    let snapshot = d
    d.insert("a", 2)
    console.print("${snapshot.length()}")
"#,
        )
        .expect_err("mode opt must reject an aliased no-copy receiver");

        assert!(error.contains("no-copy `var` contract of `dict.insert`"), "{error}");
        assert!(error.contains("bound to a new name"), "{error}");
    }

    #[test]
    fn browser_compile_enforces_place_assignment_no_copy_contract() {
        let error = super::compile_source(
            r#"mode opt

import dict

fn main(console: Console):
    var d = dict.new()
    let snapshot = d
    d["a"] = 2
    console.print("${snapshot.length()}")
"#,
        )
        .expect_err("place assignment must retain the discarded Dict contract");

        assert!(error.contains("no-copy `var` contract of `dict.insert`"), "{error}");
        assert!(error.contains("bound to a new name"), "{error}");
    }

    #[test]
    fn performance_gate_includes_entry_lambdas() {
        let linked = super::resolve_std_only(
            r#"mode opt

fn take(var xs: unique List(Int)) -> Nil:
    return

fn main() -> Int:
    let work = fn() -> Nil:
        var xs = [1]
        let snapshot = xs
        take(xs)
        let _ = snapshot
        return
    work()
    0
"#,
        )
        .expect("resolve lambda program");
        super::typeck::check(&linked).expect("type-check lambda program");
        let error = super::enforce_performance_modes(&linked, "main")
            .expect_err("an entry lambda is part of the entry module contract");
        assert!(error.contains("main::<lambda>"), "{error}");
        assert!(error.contains("bound to a new name"), "{error}");
    }
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

/// `encoding.*` via the shared native registry (hex/base64), selected by the
/// same op table the native WASM runtime uses. Text ops read `input` lossily as
/// UTF-8; byte ops preserve the raw slice and return raw flat-buffer bytes.
/// The playground host shim delegates here.
pub fn encoding(op: i32, input: &[u8]) -> Result<Vec<u8>, String> {
    use crate::value::NativeValue;
    enum Input {
        String,
        Bytes,
        LossyString,
    }
    let (name, input_kind) = match op {
        0 => ("encoding.hex_encode", Input::String),
        1 => ("encoding.hex_decode_lossy", Input::String),
        2 => ("encoding.base64_encode", Input::String),
        3 => ("encoding.base64_decode_lossy", Input::String),
        4 => ("encoding.hex_to_base64url_lossy", Input::String),
        5 => ("encoding.base64url_decode_lossy", Input::String),
        6 => ("encoding.base64url_to_hex_lossy", Input::String),
        7 => ("encoding.utf8_lossy", Input::LossyString),
        8 => ("encoding.hex_encode_bytes", Input::Bytes),
        9 => ("encoding.base64_encode_bytes", Input::Bytes),
        10 => ("encoding.base64url_encode_bytes", Input::Bytes),
        11 => ("encoding.hex_decode_bytes_raw", Input::String),
        12 => ("encoding.base64_decode_bytes_raw", Input::String),
        13 => ("encoding.base64url_decode_bytes_raw", Input::String),
        _ => return Err(format!("unknown encoding op {op}")),
    };
    let arg = match input_kind {
        Input::String => NativeValue::Str(String::from_utf8_lossy(input).into_owned()),
        Input::Bytes => NativeValue::Bytes(input.to_vec()),
        Input::LossyString => NativeValue::Str(String::from_utf8_lossy(input).into_owned()),
    };
    let f = native::lookup(name).ok_or_else(|| format!("{name} is not registered"))?;
    match f(&[arg]).map_err(|e| e.message)? {
        NativeValue::Str(s) => Ok(s.into_bytes()),
        NativeValue::Bytes(bytes) => Ok(bytes),
        _ => Err(format!("{name} did not return a flat buffer")),
    }
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
        // The browser host calls this through the C-style wasm export, whose ABI
        // can only return bytes. Prefix host-visible errors with an impossible
        // span byte; web/witchy-host.js turns it back into a thrown error.
        Err(e) => format!("\x1fregex-error:{e}"),
        _ => "\x1fregex-error:regex.match_spans did not return a String".into(),
    }
}

/// Signature verification status (op 0 ed25519, 1 ecdsa_p256, 2 ecdsa_p256_hex);
/// private std wrappers map the status into `Result(Bool, String)`.
pub fn crypto_verify_status(op: i32, pk: &str, msg: &str, sig: &str) -> i64 {
    let name = match op {
        0 => "crypto.__ed25519_verify_status",
        1 => "crypto.__ecdsa_p256_verify_status",
        2 => "crypto.__ecdsa_p256_verify_hex_status",
        _ => return -4,
    };
    match native_call(name, &[pk, msg, sig]) {
        Ok(crate::value::NativeValue::Int(n)) => n,
        _ => -4,
    }
}

/// Back-compat for older playground hosts that still ask only for a bool.
pub fn crypto_verify(op: i32, pk: &str, msg: &str, sig: &str) -> bool {
    crypto_verify_status(op, pk, msg, sig) == 1
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

/// Owned browser-ABI buffers. JavaScript sees only the address of each boxed
/// slice; Rust retains ownership and validates the exact base/length before a
/// read or free. That turns the foreign pointer contract into safe, testable
/// map lookups instead of relying on `from_raw_parts`/manual deallocation.
#[cfg(any(target_arch = "wasm32", test))]
mod abi_buffers {
    use std::collections::BTreeMap;
    #[cfg(target_arch = "wasm32")]
    use std::cell::RefCell;

    #[derive(Default)]
    struct BufferStore {
        live: BTreeMap<usize, Box<[u8]>>,
    }

    impl BufferStore {
        fn store(&mut self, bytes: Vec<u8>) -> *mut u8 {
            if bytes.is_empty() {
                return std::ptr::null_mut();
            }
            let mut bytes = bytes.into_boxed_slice();
            let ptr = bytes.as_mut_ptr();
            let old = self.live.insert(ptr as usize, bytes);
            debug_assert!(old.is_none(), "two live boxes cannot have the same base address");
            ptr
        }

        fn allocate(&mut self, len: usize) -> *mut u8 {
            if len == 0 {
                std::ptr::null_mut()
            } else {
                self.store(vec![0; len])
            }
        }

        fn read(&self, ptr: *const u8, len: usize) -> Option<Vec<u8>> {
            if ptr.is_null() {
                return (len == 0).then(Vec::new);
            }
            self.live
                .get(&(ptr as usize))
                .and_then(|bytes| bytes.get(..len))
                .map(<[u8]>::to_vec)
        }

        fn free(&mut self, ptr: *mut u8, len: usize) -> bool {
            if ptr.is_null() {
                return len == 0;
            }
            let key = ptr as usize;
            if !matches!(self.live.get(&key), Some(bytes) if bytes.len() == len) {
                return false;
            }
            let removed = self.live.remove(&key);
            debug_assert_eq!(removed.as_deref().map(<[u8]>::len), Some(len));
            true
        }
    }

    #[cfg(target_arch = "wasm32")]
    thread_local! {
        static BUFFERS: RefCell<BufferStore> = RefCell::new(BufferStore::default());
    }

    #[cfg(target_arch = "wasm32")]
    pub fn allocate(len: usize) -> *mut u8 {
        BUFFERS.with(|buffers| buffers.borrow_mut().allocate(len))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn store(bytes: Vec<u8>) -> *mut u8 {
        BUFFERS.with(|buffers| buffers.borrow_mut().store(bytes))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn read(ptr: *const u8, len: usize) -> Option<Vec<u8>> {
        BUFFERS.with(|buffers| buffers.borrow().read(ptr, len))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn free(ptr: *mut u8, len: usize) -> bool {
        BUFFERS.with(|buffers| buffers.borrow_mut().free(ptr, len))
    }

    #[cfg(test)]
    mod tests {
        use super::BufferStore;

        #[test]
        fn wasm_export_lint_exception_contains_no_unsafe_operations() {
            // `wasm_abi` must allow Rust 2024's `#[unsafe(no_mangle)]`
            // attributes, so guard that narrow lint exception against growing
            // actual unsafe functions, impls, extern blocks, or operations.
            let unsafe_operation =
                regex::Regex::new(r"\bunsafe\s*(?:\{|fn\b|impl\b|extern\b|trait\b)").unwrap();
            assert!(
                !unsafe_operation.is_match(include_str!("lib.rs")),
                "the browser ABI must use safe Rust despite its export-attribute lint exception"
            );
        }

        #[test]
        fn reads_only_live_base_bounded_ranges() {
            let mut buffers = BufferStore::default();
            let ptr = buffers.store(b"witchy".to_vec());
            assert_eq!(buffers.read(ptr, 6).as_deref(), Some(b"witchy".as_slice()));
            assert_eq!(buffers.read(ptr, 3).as_deref(), Some(b"wit".as_slice()));
            assert!(buffers.read(ptr, 7).is_none(), "an oversized range must be rejected");
            assert!(
                buffers.read(std::ptr::dangling(), 1).is_none(),
                "a forged base must be rejected"
            );
            assert_eq!(buffers.read(std::ptr::null(), 0), Some(Vec::new()));
            assert!(buffers.read(std::ptr::null(), 1).is_none());
        }

        #[test]
        fn free_requires_exact_live_allocation() {
            let mut buffers = BufferStore::default();
            let ptr = buffers.allocate(8);
            assert!(!buffers.free(ptr, 7), "a mismatched layout must not free the box");
            assert!(buffers.read(ptr, 8).is_some(), "a rejected free must leave the allocation live");
            assert!(buffers.free(ptr, 8));
            assert!(!buffers.free(ptr, 8), "a double free must be rejected");
            assert!(buffers.free(std::ptr::null_mut(), 0));
            assert!(!buffers.free(std::ptr::null_mut(), 1));
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[allow(
    unsafe_code,
    reason = "Rust 2024 classifies exported no_mangle symbols as unsafe attributes; a source-level test forbids unsafe operations in this module"
)]
mod wasm_abi {
    use super::abi_buffers;

    /// Allocate `len` bytes of guest memory for JS to write into.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_alloc(len: usize) -> *mut u8 {
        abi_buffers::allocate(len)
    }

    /// Free a buffer previously returned by `witchy_alloc` / `witchy_compile` /
    /// the helper exports.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_free(ptr: *mut u8, len: usize) {
        // Invalid, mismatched, forged, and already-freed pairs fail closed. The
        // host has no authority to make Rust deallocate an unowned address.
        let _ = abi_buffers::free(ptr, len);
    }

    /// `[u32 status][u32 len][payload]` in a fresh buffer handed to JS.
    fn pack_tagged(status: u32, payload: &[u8]) -> *mut u8 {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&status.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        abi_buffers::store(out)
    }

    /// `[u32 len][payload]` in a fresh buffer handed to JS.
    fn pack(payload: &[u8]) -> *mut u8 {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        abi_buffers::store(out)
    }

    /// Compile the source at `ptr[..len]` to a wasm binary; status 0 → bytes,
    /// 1 → error message. Free with `witchy_free(p, 8 + len)`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_compile(ptr: *const u8, len: usize) -> *mut u8 {
        let Some(src) = abi_buffers::read(ptr, len) else {
            return pack_tagged(1, b"browser ABI rejected an invalid source buffer");
        };
        let src = String::from_utf8_lossy(&src).into_owned();
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

    /// `encoding(op, input[..in_len])` → `[u32 len][bytes]`. Text results use
    /// UTF-8; byte decoders preserve their raw output. Public witchy decoders
    /// validate before calling a raw op, so the pointer-only bridge has no error
    /// variant; an impossible host-table error becomes an empty result.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_encoding(op: i32, in_ptr: *const u8, in_len: usize) -> *mut u8 {
        let Some(input) = abi_buffers::read(in_ptr, in_len) else {
            return pack(&[]);
        };
        let out = super::encoding(op, &input).unwrap_or_default();
        pack(&out)
    }

    fn text(ptr: *const u8, len: usize) -> Option<String> {
        abi_buffers::read(ptr, len).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `crypto.sha256/sha512/sha3_256(op, input)` → `[u32 len][hex]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_crypto_hash(op: i32, in_ptr: *const u8, in_len: usize) -> *mut u8 {
        let Some(input) = text(in_ptr, in_len) else { return pack(&[]) };
        pack(super::crypto_hash(op, &input).as_bytes())
    }

    /// `crypto.hmac_sha256(key, msg)` → `[u32 len][hex]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_hmac_sha256(
        k_ptr: *const u8,
        k_len: usize,
        m_ptr: *const u8,
        m_len: usize,
    ) -> *mut u8 {
        let (Some(key), Some(message)) = (text(k_ptr, k_len), text(m_ptr, m_len)) else {
            return pack(&[]);
        };
        pack(super::hmac_sha256(&key, &message).as_bytes())
    }

    /// `regex.match_spans(pattern, text)` → `[u32 len][packed spans]`.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_regex(
        p_ptr: *const u8,
        p_len: usize,
        t_ptr: *const u8,
        t_len: usize,
    ) -> *mut u8 {
        let (Some(pattern), Some(text)) = (text(p_ptr, p_len), text(t_ptr, t_len)) else {
            return pack(&[]);
        };
        pack(super::regex_spans(&pattern, &text).as_bytes())
    }

    /// Signature verify status (op 0 ed25519, 1/2 ecdsa): 1 valid, 0 invalid
    /// signature, negative malformed input/unavailable.
    #[unsafe(no_mangle)]
    pub extern "C" fn witchy_verify_status(
        op: i32,
        pk_ptr: *const u8,
        pk_len: usize,
        m_ptr: *const u8,
        m_len: usize,
        s_ptr: *const u8,
        s_len: usize,
    ) -> i64 {
        let (Some(pk), Some(message), Some(signature)) = (
            text(pk_ptr, pk_len),
            text(m_ptr, m_len),
            text(s_ptr, s_len),
        ) else {
            return -4;
        };
        super::crypto_verify_status(op, &pk, &message, &signature)
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
        let (Some(pk), Some(message), Some(signature)) = (
            text(pk_ptr, pk_len),
            text(m_ptr, m_len),
            text(s_ptr, s_len),
        ) else {
            return 0;
        };
        (super::crypto_verify_status(op, &pk, &message, &signature) == 1) as i32
    }
}
