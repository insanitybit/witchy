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

use crate::value::{NativeError as RuntimeError, NativeValue as Value};

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
        "crypto.reveal" => Some(crypto::reveal),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.ecdsa_p256_verify" => Some(crypto::ecdsa_p256_verify),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.ecdsa_p256_verify_hex" => Some(crypto::ecdsa_p256_verify_hex),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.rsa_pkcs1_sha256_verify" => Some(crypto::rsa_pkcs1_sha256_verify),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.sha512" => Some(crypto::sha512),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.sha3_256" => Some(crypto::sha3_256),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.hmac_sha256" => Some(crypto::hmac_sha256),
        "compiler.footprint" => Some(compiler::footprint),
        "compiler.diff" => Some(compiler::diff),
        "compiler.doc" => Some(compiler::doc),
        "encoding.hex_encode" => Some(encoding::hex_encode),
        "encoding.hex_decode" => Some(encoding::hex_decode),
        "encoding.base64_encode" => Some(encoding::base64_encode),
        "encoding.base64url_of_hex" => Some(encoding::base64url_of_hex),
        "encoding.base64_decode" => Some(encoding::base64_decode),
        "encoding.base64url_decode" => Some(encoding::base64url_decode),
        "encoding.base64url_to_hex" => Some(encoding::base64url_to_hex),
        "regex.match_spans" => Some(regexp::match_spans),
        "string.from_code" => Some(string::from_code),
        _ => None,
    }
}

fn type_error(msg: impl Into<String>) -> RuntimeError {
    RuntimeError { message: msg.into() }
}

/// The `crypto` module: hashing and signatures (sha2 / ed25519-dalek).
mod crypto {
    use super::{type_error, Value};
    use crate::value::NativeError as RuntimeError;

    /// SHA-256 of a string's UTF-8 bytes, as 64 lowercase hex characters.
    pub fn sha256(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("crypto.sha256 expects a String"));
        };
        Ok(Value::Str(hex(&sha256_digest(s.as_bytes()))))
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
        let mut buf = Vec::new();
        for (path, bytes) in &files {
            buf.extend_from_slice(&(path.len() as u64).to_le_bytes());
            buf.extend_from_slice(path.as_bytes());
            buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
        Ok(Value::Str(format!("sha256:{}", hex(&sha256_digest(&buf)))))
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
        let ok = (|| {
            let pk = hex_decode(pk_hex)?;
            let sig = hex_decode(sig_hex)?;
            Some(ed25519_verify_raw(&pk, msg.as_bytes(), &sig))
        })()
        .unwrap_or(false);
        Ok(Value::Bool(ok))
    }

    /// Normalize a secret's raw bytes to a 32-byte Ed25519 seed: accept the seed
    /// directly (32 bytes) or hex-encoded (64 chars). Anything else (e.g. a value
    /// secret like a token) is not a signing key.
    fn seed32(bytes: &[u8]) -> Result<[u8; 32], RuntimeError> {
        let raw = if bytes.len() == 32 {
            bytes.to_vec()
        } else if bytes.len() == 64 {
            hex_decode(&String::from_utf8_lossy(bytes))
                .ok_or_else(|| type_error("secret is not a valid hex Ed25519 seed"))?
        } else {
            return Err(type_error(
                "secret is not a signing key (need a 32-byte seed or 64 hex chars)",
            ));
        };
        raw.try_into()
            .map_err(|_| type_error("secret is not a 32-byte Ed25519 seed"))
    }

    /// Sign `message` with a `Secret`, returning the hex signature.
    pub fn sign(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Secret(bytes), Value::Str(msg)] = args else {
            return Err(type_error("crypto.sign expects (Secret, message)"));
        };
        Ok(Value::Str(hex(&ed25519_sign_raw(&seed32(bytes)?, msg.as_bytes()))))
    }

    /// The hex-encoded Ed25519 public key for a `Secret` — what a verifier checks
    /// signatures against (safe to publish).
    pub fn public_key(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Secret(bytes)] = args else {
            return Err(type_error("crypto.public_key expects a Secret"));
        };
        Ok(Value::Str(hex(&ed25519_public_raw(&seed32(bytes)?))))
    }

    /// Reveal a `Secret`'s raw bytes as a string — for value secrets (tokens,
    /// passwords) that must be handed to an external sink.
    pub fn reveal(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Secret(bytes)] = args else {
            return Err(type_error("crypto.reveal expects a Secret"));
        };
        Ok(Value::Str(String::from_utf8_lossy(bytes).into_owned()))
    }

    /// Verify an ECDSA P-256 / SHA-256 ("ES256", WebAuthn COSE alg -7) signature.
    /// `public_key`/`signature` are hex (a SEC1 uncompressed point `04||x||y`; an
    /// ASN.1-DER signature); `message` is the raw bytes the signature covers (the
    /// curve hashes it with SHA-256). Total — malformed input or a bad signature is
    /// `false`. Native-only: aws-lc-rs has no wasm32 build, and this is not bridged
    /// to the WASM backend (WebAuthn verification runs on the interpreter).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn ecdsa_p256_verify(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pk_hex), Value::Str(msg), Value::Str(sig_hex)] = args else {
            return Err(type_error(
                "crypto.ecdsa_p256_verify expects (pubkey_hex, message, sig_hex) strings",
            ));
        };
        let ok = (|| {
            let pk = hex_decode(pk_hex)?;
            let sig = hex_decode(sig_hex)?;
            use aws_lc_rs::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
            Some(
                UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, pk)
                    .verify(msg.as_bytes(), &sig)
                    .is_ok(),
            )
        })()
        .unwrap_or(false);
        Ok(Value::Bool(ok))
    }

    /// Verify an RSASSA-PKCS1-v1_5 / SHA-256 signature — JWT/OIDC "RS256". The
    /// `public_key` is the hex of a DER-encoded RSA public key (PKCS#1 `RSAPublicKey`);
    /// `signature` is hex; `message` is the raw signed bytes. Total: malformed input or
    /// a bad signature yields `false`, never an error. (Native/aws-lc only.)
    #[cfg(not(target_arch = "wasm32"))]
    pub fn rsa_pkcs1_sha256_verify(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pk_hex), Value::Str(msg), Value::Str(sig_hex)] = args else {
            return Err(type_error(
                "crypto.rsa_pkcs1_sha256_verify expects (pubkey_der_hex, message, sig_hex) strings",
            ));
        };
        let ok = (|| {
            let pk = hex_decode(pk_hex)?;
            let sig = hex_decode(sig_hex)?;
            use aws_lc_rs::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
            Some(
                UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, pk)
                    .verify(msg.as_bytes(), &sig)
                    .is_ok(),
            )
        })()
        .unwrap_or(false);
        Ok(Value::Bool(ok))
    }

    /// Like `ecdsa_p256_verify` but the message is also hex — for binary messages
    /// such as WebAuthn's `authenticatorData ‖ SHA256(clientDataJSON)`. Total.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn ecdsa_p256_verify_hex(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pk_hex), Value::Str(msg_hex), Value::Str(sig_hex)] = args else {
            return Err(type_error(
                "crypto.ecdsa_p256_verify_hex expects (pubkey_hex, message_hex, sig_hex) strings",
            ));
        };
        let ok = (|| {
            let pk = hex_decode(pk_hex)?;
            let msg = hex_decode(msg_hex)?;
            let sig = hex_decode(sig_hex)?;
            use aws_lc_rs::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
            Some(
                UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, pk)
                    .verify(&msg, &sig)
                    .is_ok(),
            )
        })()
        .unwrap_or(false);
        Ok(Value::Bool(ok))
    }

    /// SHA-512 (FIPS 180-4) of a string's UTF-8 bytes, as 128 lowercase hex chars.
    /// Native-only (aws-lc-rs; not WASM-bridged).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn sha512(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("crypto.sha512 expects a String"));
        };
        let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA512, s.as_bytes());
        Ok(Value::Str(hex(d.as_ref())))
    }

    /// SHA3-256 (FIPS 202) of a string's UTF-8 bytes, as 64 lowercase hex chars.
    /// Native-only (aws-lc-rs; not WASM-bridged).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn sha3_256(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("crypto.sha3_256 expects a String"));
        };
        let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA3_256, s.as_bytes());
        Ok(Value::Str(hex(d.as_ref())))
    }

    /// HMAC-SHA256 (FIPS 198-1). `key` is hex (so binary keys are representable),
    /// `message` is raw bytes; returns the 64-hex-char tag. Native-only.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn hmac_sha256(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(key_hex), Value::Str(msg)] = args else {
            return Err(type_error("crypto.hmac_sha256 expects (key_hex, message) strings"));
        };
        let key_bytes =
            hex_decode(key_hex).ok_or_else(|| type_error("crypto.hmac_sha256: key is not valid hex"))?;
        use aws_lc_rs::hmac;
        let key = hmac::Key::new(hmac::HMAC_SHA256, &key_bytes);
        let tag = hmac::sign(&key, msg.as_bytes());
        Ok(Value::Str(hex(tag.as_ref())))
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    fn hex_decode(s: &str) -> Option<Vec<u8>> {
        if !s.len().is_multiple_of(2) {
            return None;
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
            .collect()
    }

    // Crypto primitives. On native they are FIPS-approved algorithms via aws-lc-rs;
    // the wasm playground (which cannot build aws-lc-rs and runs no security-critical
    // crypto) keeps the pure-Rust path. Outputs are byte-identical — both implement
    // FIPS 180-4 SHA-256 and RFC 8032 Ed25519 deterministically.

    #[cfg(not(target_arch = "wasm32"))]
    fn sha256_digest(data: &[u8]) -> [u8; 32] {
        let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, data);
        let mut out = [0u8; 32];
        out.copy_from_slice(d.as_ref());
        out
    }
    #[cfg(target_arch = "wasm32")]
    fn sha256_digest(data: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        Sha256::digest(data).into()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ed25519_verify_raw(pk: &[u8], msg: &[u8], sig: &[u8]) -> bool {
        use aws_lc_rs::signature::{UnparsedPublicKey, ED25519};
        UnparsedPublicKey::new(&ED25519, pk).verify(msg, sig).is_ok()
    }
    #[cfg(target_arch = "wasm32")]
    fn ed25519_verify_raw(pk: &[u8], msg: &[u8], sig: &[u8]) -> bool {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        (|| {
            let pk: [u8; 32] = pk.try_into().ok()?;
            let vk = VerifyingKey::from_bytes(&pk).ok()?;
            let sig: [u8; 64] = sig.try_into().ok()?;
            Some(vk.verify(msg, &Signature::from_bytes(&sig)).is_ok())
        })()
        .unwrap_or(false)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ed25519_sign_raw(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
        use aws_lc_rs::signature::Ed25519KeyPair;
        let kp = Ed25519KeyPair::from_seed_unchecked(seed).expect("32-byte ed25519 seed");
        let mut out = [0u8; 64];
        out.copy_from_slice(kp.sign(msg).as_ref());
        out
    }
    #[cfg(target_arch = "wasm32")]
    fn ed25519_sign_raw(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::{Signer, SigningKey};
        SigningKey::from_bytes(seed).sign(msg).to_bytes()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ed25519_public_raw(seed: &[u8; 32]) -> [u8; 32] {
        use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
        let kp = Ed25519KeyPair::from_seed_unchecked(seed).expect("32-byte ed25519 seed");
        let mut out = [0u8; 32];
        out.copy_from_slice(kp.public_key().as_ref());
        out
    }
    #[cfg(target_arch = "wasm32")]
    fn ed25519_public_raw(seed: &[u8; 32]) -> [u8; 32] {
        use ed25519_dalek::SigningKey;
        *SigningKey::from_bytes(seed).verifying_key().as_bytes()
    }
}

/// The `compiler` module: witchy's own toolchain, exposed to witchy. This is what
/// lets a (self-hosted) package manager compute a rune's capability footprint —
/// the heart of the supply-chain story — from within witchy.
mod compiler {
    use super::{type_error, Value};
    use crate::value::NativeError as RuntimeError;

    /// Compute the capability footprint of witchy `source`, returned as JSON:
    /// `{"total":[..],"build":[..],"entries":[{"name":..,"capabilities":[..],"brands":[..]}]}`,
    /// or `{"error":".."}` if the source does not parse. `build` is the build-time
    /// footprint — the build capabilities (`BuildOut`/`BuildRead`/…) the rune's
    /// `build` entrypoint demands, which a build tool gates separately from the
    /// runtime `total`. Pairs with `std/json`.
    pub fn footprint(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(src)] = args else {
            return Err(type_error("compiler.footprint expects a String"));
        };
        let json = match witchy_syntax::parser::parse_module(src) {
            Ok(module) => {
                let fp = witchy_caps::capabilities::analyze(&module);
                let total = arr(fp.total.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                let build = arr(fp.build.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                let entries: Vec<String> = fp
                    .entries
                    .iter()
                    .map(|e| {
                        let caps =
                            arr(e.capabilities.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                        let brands = arr(e.brands.iter().cloned());
                        format!(
                            "{{\"name\":{},\"capabilities\":{},\"brands\":{}}}",
                            string(&e.name),
                            caps,
                            brands
                        )
                    })
                    .collect();
                format!(
                    "{{\"total\":{},\"build\":{},\"entries\":[{}]}}",
                    total,
                    build,
                    entries.join(",")
                )
            }
            Err(e) => format!("{{\"error\":{}}}", string(&e.to_string())),
        };
        Ok(Value::Str(json))
    }

    /// Render witchy `source` to Markdown API documentation — the same output as the
    /// `witchy doc` CLI: the module's public types and functions with their signatures
    /// and doc-comments. `name` titles the module heading. Lets a registry generate
    /// browsable docs from a rune's stored source, on either backend. `witchy doc` only
    /// *parses* the source (it never runs it), so this is safe on untrusted code; a parse
    /// error is returned as an HTML comment rather than trapping.
    pub fn doc(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(name), Value::Str(src)] = args else {
            return Err(type_error("compiler.doc expects (name, source) strings"));
        };
        let md = witchy_syntax::doc::render(name, src).unwrap_or_else(|e| format!("<!-- doc error: {e} -->"));
        Ok(Value::Str(md))
    }

    /// Compare two witchy sources by capability footprint, as JSON:
    /// `{"widened":bool,"added":[..],"removed":[..]}` — the rights-precise
    /// block-on-widening gate (the package manager's core safety check), exposed
    /// to witchy. `{"error":".."}` if either source does not parse.
    pub fn diff(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(old_src), Value::Str(new_src)] = args else {
            return Err(type_error("compiler.diff expects (old_source, new_source) strings"));
        };
        let json = match (witchy_syntax::parser::parse_module(old_src), witchy_syntax::parser::parse_module(new_src)) {
            (Ok(old), Ok(new)) => {
                let old_fp = witchy_caps::capabilities::analyze(&old);
                let new_fp = witchy_caps::capabilities::analyze(&new);
                let d = witchy_caps::capabilities::diff(&old_fp, &new_fp);
                let added = arr(d.added.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                let removed = arr(d.removed.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
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
    use crate::value::NativeError as RuntimeError;

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
    const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    fn hex_to_bytes(s: &str) -> Vec<u8> {
        let nib = |c: u8| -> Option<u8> {
            match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None,
            }
        };
        let cs: Vec<u8> = s.bytes().filter_map(nib).collect();
        cs.chunks(2).filter(|p| p.len() == 2).map(|p| p[0] * 16 + p[1]).collect()
    }

    /// base64url (no padding; `-`/`_`) of the bytes given as a HEX string. The hex
    /// indirection lets binary round-trip through witchy's UTF-8 strings — e.g. a
    /// WebAuthn `clientDataJSON.challenge` is base64url of the raw challenge bytes.
    pub fn base64url_of_hex(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(hexs)] = args else {
            return Err(type_error("encoding.base64url_of_hex expects a hex String"));
        };
        let bytes = hex_to_bytes(hexs);
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(B64URL[(n >> 18 & 63) as usize] as char);
            out.push(B64URL[(n >> 12 & 63) as usize] as char);
            if chunk.len() > 1 {
                out.push(B64URL[(n >> 6 & 63) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(B64URL[(n & 63) as usize] as char);
            }
        }
        Ok(Value::Str(out))
    }

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

    /// Decode base64url (URL-safe `-`/`_`, padding/whitespace tolerated) back to text
    /// (lossy UTF-8) — for the JSON header/payload segments of a JWT/OIDC token.
    pub fn base64url_decode(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64url_decode expects a String"));
        };
        Ok(Value::Str(String::from_utf8_lossy(&base64url_bytes(s)).into_owned()))
    }

    /// Decode base64url to a HEX string — for binary that must round-trip through a
    /// witchy String, e.g. a JWT's RS256 signature fed to `crypto.rsa_pkcs1_sha256_verify`.
    pub fn base64url_to_hex(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64url_to_hex expects a String"));
        };
        let hex: String = base64url_bytes(s).iter().map(|b| format!("{b:02x}")).collect();
        Ok(Value::Str(hex))
    }

    /// Shared base64url decoder (URL-safe alphabet; `=`/whitespace tolerated; a
    /// non-alphabet byte stops decoding).
    fn base64url_bytes(s: &str) -> Vec<u8> {
        let mut acc: u32 = 0;
        let mut nbits = 0;
        let mut bytes = Vec::new();
        for c in s.bytes() {
            if c == b'=' || c.is_ascii_whitespace() {
                continue;
            }
            let Some(v) = B64URL.iter().position(|&x| x == c) else {
                break;
            };
            acc = (acc << 6) | v as u32;
            nbits += 6;
            if nbits >= 8 {
                nbits -= 8;
                bytes.push((acc >> nbits & 0xff) as u8);
            }
        }
        bytes
    }
}

/// The `regex` module's engine: the Rust `regex` crate (RE2 semantics — linear
/// time, full alternation `|` and grouping `(...)`). This single native carries
/// the matching; the whole public `std/regex` API (matches/find/find_all/extract/
/// replace_all/split) is built in witchy on the spans it returns. Positions are
/// CHARACTER indices (witchy strings are char-indexed), converted from the
/// crate's byte offsets, so they feed `string.substring` directly. An invalid
/// pattern is a loud error on every backend, not a silent non-match.
mod regexp {
    use super::{type_error, Value};
    use crate::value::NativeError as RuntimeError;

    /// Character offset of `byte` within `text` (a byte index from the crate).
    fn char_off(text: &str, byte: usize) -> i64 {
        text[..byte].chars().count() as i64
    }

    /// Every non-overlapping match as char-index spans, encoded `"s,e;s,e;..."`
    /// (empty string when there is no match). The host-bridged form used by the
    /// WASM backend (`host_regex_spans`) calls straight through to this.
    pub fn match_spans(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pattern), Value::Str(text)] = args else {
            return Err(type_error("regex.match_spans expects (pattern, text) strings"));
        };
        let re = regex::Regex::new(pattern)
            .map_err(|e| type_error(format!("regex: invalid pattern `{pattern}`: {e}")))?;
        let mut out = String::new();
        for m in re.find_iter(text) {
            use std::fmt::Write;
            if !out.is_empty() {
                out.push(';');
            }
            let _ = write!(out, "{},{}", char_off(text, m.start()), char_off(text, m.end()));
        }
        Ok(Value::Str(out))
    }
}

mod string {
    use super::{type_error, Value};
    use crate::value::NativeError as RuntimeError;

    /// The single character for a Unicode scalar value, as a UTF-8 string. An
    /// out-of-range or surrogate code point yields U+FFFD (the replacement
    /// character) rather than an error — callers (e.g. the JSON `\u` decoder)
    /// range-check themselves, and this must never trap mid-parse.
    pub fn from_code(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Int(n)] = args else {
            return Err(type_error("string.from_code expects an Int"));
        };
        let ch = u32::try_from(*n).ok().and_then(char::from_u32).unwrap_or('\u{FFFD}');
        Ok(Value::Str(ch.to_string()))
    }
}
