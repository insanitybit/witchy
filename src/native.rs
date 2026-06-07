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
        "crypto.ed25519_verify" => Some(crypto::ed25519_verify),
        "crypto.sign" => Some(crypto::sign),
        "crypto.public_key" => Some(crypto::public_key),
        "compiler.footprint" => Some(compiler::footprint),
        "compiler.diff" => Some(compiler::diff),
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

    /// Sign `message` with a `SigningKey` capability, returning the hex signature.
    pub fn sign(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::SigningKey(seed), Value::Str(msg)] = args else {
            return Err(type_error("crypto.sign expects (SigningKey, message)"));
        };
        use ed25519_dalek::{Signer, SigningKey};
        let sig = SigningKey::from_bytes(seed).sign(msg.as_bytes()).to_bytes();
        Ok(Value::Str(hex(&sig)))
    }

    /// The hex-encoded Ed25519 public key for a `SigningKey` capability — what a
    /// verifier checks signatures against (safe to publish).
    pub fn public_key(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::SigningKey(seed)] = args else {
            return Err(type_error("crypto.public_key expects a SigningKey"));
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
