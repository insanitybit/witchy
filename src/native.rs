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
}
