//! Native standard-library modules — functions implemented in Rust because they
//! cannot be expressed in witchy itself (cryptography today; encoding, etc.
//! later). This is the *trusted, compiled-in* native set:
//!
//! - reached only through their module (`crypto.sha256`), never as a global
//!   builtin;
//! - **pure and stateless** — capability-gated host I/O (`read`, `now`, …) is
//!   not here; it lives in the interpreter, which threads host state;
//! - interpreter-only (native Rust can't run in the WASM sandbox tier).
//!
//! This is the static half of "Rust modules": adding a native stdlib function is
//! a registration here plus a typed signature in the module's `.witchy` stub —
//! no editing of the interpreter's builtin match. A future, explicitly
//! capability-gated `Native` capability would add *dynamically*-loaded FFI on
//! top of this registry.

use crate::interpreter::{RuntimeError, Value};

/// A native module function: pure and stateless, `(args) -> value`.
pub type NativeFn = fn(&[Value]) -> Result<Value, RuntimeError>;

/// Resolve a native-module function by its qualified name (`crypto.sha256`), or
/// `None` if `qualified` is not a native-module function.
pub fn lookup(qualified: &str) -> Option<NativeFn> {
    match qualified {
        "crypto.sha256" => Some(crypto::sha256),
        "crypto.rune_hash" => Some(crypto::rune_hash),
        "crypto.ed25519_verify" => Some(crypto::ed25519_verify),
        "crypto.sign" => Some(crypto::sign),
        "crypto.public_key" => Some(crypto::public_key),
        "compiler.footprint" => Some(compiler::footprint),
        "compiler.diff" => Some(compiler::diff),
        "encoding.hex_encode" => Some(encoding::hex_encode),
        "encoding.hex_decode" => Some(encoding::hex_decode),
        "encoding.base64_encode" => Some(encoding::base64_encode),
        "encoding.base64_decode" => Some(encoding::base64_decode),
        _ => None,
    }
}

/// Whether `qualified` names a native-module function — used by the WASM backend
/// to reject these (they are interpreter-only).
pub fn is_native(qualified: &str) -> bool {
    lookup(qualified).is_some()
}

fn type_error(msg: impl Into<String>) -> RuntimeError {
    RuntimeError { message: msg.into() }
}

/// The `crypto` module: hashing and signatures (sha2 / ed25519-dalek).
mod crypto {
    use super::{type_error, Value};
    use crate::interpreter::RuntimeError;

    /// SHA-256 of a string's UTF-8 bytes, as 64 lowercase hex characters.
    pub fn sha256(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("crypto.sha256 expects a String"));
        };
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(s.as_bytes());
        let digest = h.finalize();
        let mut out = String::with_capacity(64);
        for b in digest.as_ref() as &[u8] {
            use std::fmt::Write;
            let _ = write!(out, "{b:02x}");
        }
        Ok(Value::Str(out))
    }

    /// The canonical content hash of a rune's source tree — the package manager's
    /// content address. `paths` and `contents` are parallel lists, one entry per
    /// file (`witchy.toml` plus each `src/**/*.witchy`). Entries are sorted by
    /// path and each content is LF-normalized, then SHA-256'd with a u64 length
    /// prefix on *every* field so no concatenation is ambiguous — byte-identical
    /// to the store's hashing (`src/pm/store.rs`). Returns `sha256:<hex>`.
    pub fn rune_hash(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::List(paths), Value::List(contents)] = args else {
            return Err(type_error(
                "crypto.rune_hash expects (List(String), List(String))",
            ));
        };
        if paths.len() != contents.len() {
            return Err(type_error(
                "crypto.rune_hash: paths and contents differ in length",
            ));
        }
        let mut files: Vec<(&str, Vec<u8>)> = Vec::with_capacity(paths.len());
        for (p, c) in paths.iter().zip(contents.iter()) {
            let (Value::Str(path), Value::Str(content)) = (p, c) else {
                return Err(type_error("crypto.rune_hash: entries must be strings"));
            };
            files.push((path.as_str(), normalize_lf(content.as_bytes())));
        }
        files.sort_by(|a, b| a.0.cmp(b.0));
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for (path, bytes) in &files {
            h.update((path.len() as u64).to_le_bytes());
            h.update(path.as_bytes());
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(bytes);
        }
        let digest = h.finalize();
        let mut out = String::with_capacity(7 + 64);
        out.push_str("sha256:");
        for b in digest.as_ref() as &[u8] {
            use std::fmt::Write;
            let _ = write!(out, "{b:02x}");
        }
        Ok(Value::Str(out))
    }

    /// LF-normalize so a CRLF checkout hashes identically (matches the store).
    fn normalize_lf(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                out.push(b'\n');
                i += 2;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    /// Verify an Ed25519 signature. `public_key`/`signature` are hex; `message`
    /// is the raw string. Total — malformed input or a bad signature is `false`.
    pub fn ed25519_verify(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pk_hex), Value::Str(msg), Value::Str(sig_hex)] = args else {
            return Err(type_error(
                "crypto.ed25519_verify expects (pubkey_hex, message, sig_hex) strings",
            ));
        };
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let decode = |s: &str| -> Option<Vec<u8>> {
            if !s.len().is_multiple_of(2) {
                return None;
            }
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
                .collect()
        };
        let ok = (|| {
            let pk: [u8; 32] = decode(pk_hex)?.try_into().ok()?;
            let vk = VerifyingKey::from_bytes(&pk).ok()?;
            let sig: [u8; 64] = decode(sig_hex)?.try_into().ok()?;
            Some(vk.verify(msg.as_bytes(), &Signature::from_bytes(&sig)).is_ok())
        })()
        .unwrap_or(false);
        Ok(Value::Bool(ok))
    }

    /// Sign `message` with a `Secret` capability, returning the hex signature.
    pub fn sign(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Secret(seed), Value::Str(msg)] = args else {
            return Err(type_error("crypto.sign expects (Secret, message)"));
        };
        use ed25519_dalek::{Signer, SigningKey};
        let sig = SigningKey::from_bytes(seed).sign(msg.as_bytes()).to_bytes();
        Ok(Value::Str(hex(&sig)))
    }

    /// The hex-encoded Ed25519 public key for a `Secret` capability — what a
    /// verifier checks signatures against (safe to publish).
    pub fn public_key(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Secret(seed)] = args else {
            return Err(type_error("crypto.public_key expects a Secret"));
        };
        use ed25519_dalek::SigningKey;
        Ok(Value::Str(hex(SigningKey::from_bytes(seed).verifying_key().as_bytes())))
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

/// The `compiler` module: witchy's own toolchain, exposed to witchy. This is what
/// lets a (self-hosted) package manager compute a rune's capability footprint —
/// the heart of the supply-chain story — from within witchy.
mod compiler {
    use super::{type_error, Value};
    use crate::interpreter::RuntimeError;

    /// Compute the capability footprint of witchy `source`, returned as JSON:
    /// `{"total":[..],"entries":[{"name":..,"capabilities":[..],"brands":[..]}]}`,
    /// or `{"error":".."}` if the source does not parse. Pairs with `std/json`.
    pub fn footprint(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(src)] = args else {
            return Err(type_error("compiler.footprint expects a String"));
        };
        let json = match crate::parser::parse_module(src) {
            Ok(module) => {
                let fp = crate::capabilities::analyze(&module);
                let total = arr(fp.total.iter().map(|(n, r)| crate::capabilities::show_cap(n, r)));
                let entries: Vec<String> = fp
                    .entries
                    .iter()
                    .map(|e| {
                        let caps =
                            arr(e.capabilities.iter().map(|(n, r)| crate::capabilities::show_cap(n, r)));
                        let brands = arr(e.brands.iter().cloned());
                        format!(
                            "{{\"name\":{},\"capabilities\":{},\"brands\":{}}}",
                            string(&e.name),
                            caps,
                            brands
                        )
                    })
                    .collect();
                format!("{{\"total\":{},\"entries\":[{}]}}", total, entries.join(","))
            }
            Err(e) => format!("{{\"error\":{}}}", string(&e.to_string())),
        };
        Ok(Value::Str(json))
    }

    /// Compare two witchy sources by capability footprint, as JSON:
    /// `{"widened":bool,"added":[..],"removed":[..]}` — the rights-precise
    /// block-on-widening gate (the package manager's core safety check), exposed
    /// to witchy. `{"error":".."}` if either source does not parse.
    pub fn diff(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(old_src), Value::Str(new_src)] = args else {
            return Err(type_error("compiler.diff expects (old_source, new_source) strings"));
        };
        let json = match (crate::parser::parse_module(old_src), crate::parser::parse_module(new_src)) {
            (Ok(old), Ok(new)) => {
                let old_fp = crate::capabilities::analyze(&old);
                let new_fp = crate::capabilities::analyze(&new);
                let d = crate::capabilities::diff(&old_fp, &new_fp);
                let added = arr(d.added.iter().map(|(n, r)| crate::capabilities::show_cap(n, r)));
                let removed = arr(d.removed.iter().map(|(n, r)| crate::capabilities::show_cap(n, r)));
                format!(
                    "{{\"widened\":{},\"added\":{},\"removed\":{}}}",
                    d.widened(),
                    added,
                    removed
                )
            }
            (Err(e), _) | (_, Err(e)) => format!("{{\"error\":{}}}", string(&e.to_string())),
        };
        Ok(Value::Str(json))
    }

    /// A JSON string literal (quoted, with `"` and `\` escaped).
    fn string(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// A JSON array of strings.
    fn arr(items: impl Iterator<Item = String>) -> String {
        let parts: Vec<String> = items.map(|s| string(&s)).collect();
        format!("[{}]", parts.join(","))
    }
}

/// The `encoding` module: hex and base64, over a string's UTF-8 bytes. These need
/// byte-level access witchy strings don't expose, so (like `crypto`) they are
/// native. Decoding is lenient — it returns the bytes it could decode as a UTF-8
/// string (lossy for non-text payloads), never an error.
mod encoding {
    use super::{type_error, Value};
    use crate::interpreter::RuntimeError;

    /// Lowercase hex of the input's UTF-8 bytes.
    pub fn hex_encode(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.hex_encode expects a String"));
        };
        use std::fmt::Write;
        let mut out = String::with_capacity(s.len() * 2);
        for b in s.as_bytes() {
            let _ = write!(out, "{b:02x}");
        }
        Ok(Value::Str(out))
    }

    /// Decode a hex string back to text (lossy UTF-8). Whitespace is skipped; an
    /// odd or non-hex tail is ignored.
    pub fn hex_decode(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.hex_decode expects a String"));
        };
        let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let mut bytes = Vec::with_capacity(digits.len() / 2);
        for pair in digits.chunks_exact(2) {
            let hi = (pair[0] as char).to_digit(16);
            let lo = (pair[1] as char).to_digit(16);
            match (hi, lo) {
                (Some(h), Some(l)) => bytes.push((h * 16 + l) as u8),
                _ => break,
            }
        }
        Ok(Value::Str(String::from_utf8_lossy(&bytes).into_owned()))
    }

    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    /// Standard base64 (with `=` padding) of the input's UTF-8 bytes.
    pub fn base64_encode(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64_encode expects a String"));
        };
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(B64[(n >> 18 & 63) as usize] as char);
            out.push(B64[(n >> 12 & 63) as usize] as char);
            out.push(if chunk.len() > 1 { B64[(n >> 6 & 63) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { B64[(n & 63) as usize] as char } else { '=' });
        }
        Ok(Value::Str(out))
    }

    /// Decode standard base64 back to text (lossy UTF-8). Padding and whitespace
    /// are tolerated; a non-alphabet byte stops decoding.
    pub fn base64_decode(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64_decode expects a String"));
        };
        let mut acc: u32 = 0;
        let mut nbits = 0;
        let mut bytes = Vec::new();
        for c in s.bytes() {
            if c == b'=' || c.is_ascii_whitespace() {
                continue;
            }
            let Some(v) = B64.iter().position(|&x| x == c) else {
                break;
            };
            acc = (acc << 6) | v as u32;
            nbits += 6;
            if nbits >= 8 {
                nbits -= 8;
                bytes.push((acc >> nbits & 0xff) as u8);
            }
        }
        Ok(Value::Str(String::from_utf8_lossy(&bytes).into_owned()))
    }
}
