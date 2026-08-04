//! (RFC-0106) Native-only SHAKE128/256 extendable-output functions (XOF).
//!
//! `aws-lc-rs`'s safe digest API does not expose SHAKE, so this module owns the
//! only direct `aws-lc-sys` FFI in the runtime and confines every `unsafe` block
//! and raw AWS-LC pointer to itself. The rest of the runtime sees a narrow safe
//! interface returning owned `Vec<u8>` or a typed error — no context, pointer, or
//! generated binding type escapes. Compiled only for non-`wasm32` targets; the
//! browser build has no AWS-LC and omits SHAKE by omission (RFC-0007).
#![allow(unsafe_code)]

use aws_lc_sys::{
    EVP_DigestFinalXOF, EVP_DigestInit_ex, EVP_DigestUpdate, EVP_MD_CTX_free, EVP_MD_CTX_new,
    EVP_shake128, EVP_shake256, EVP_MD,
};

/// A SHAKE failure the caller can surface as a typed error. Invalid output
/// length is rejected before any allocation or FFI; an AWS-LC failure is a
/// distinct variant so a genuine crypto fault is never confused with bad input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShakeError {
    /// The requested output length is negative or larger than we will allocate.
    InvalidLength(i64),
    /// AWS-LC reported a failure during init/update/finalize.
    Backend(&'static str),
}

impl ShakeError {
    pub fn message(&self) -> String {
        match self {
            ShakeError::InvalidLength(n) => {
                format!("SHAKE output length {n} is invalid (must be 0..={MAX_OUTPUT})")
            }
            ShakeError::Backend(op) => format!("SHAKE backend failure in {op}"),
        }
    }
}

/// Refuse absurd output requests before allocation. A megabyte of squeezed
/// output is already far past any realistic key/nonce/commitment use; a caller
/// that needs a stream should call repeatedly with a domain-separated input.
const MAX_OUTPUT: usize = 1 << 20;

/// SHAKE128 of `input`, squeezed to exactly `output_len` bytes.
pub fn shake128(input: &[u8], output_len: i64) -> Result<Vec<u8>, ShakeError> {
    // SAFETY: `EVP_shake128()` returns a static `EVP_MD` descriptor; the digest
    // helper confines the context lifetime.
    xof(input, output_len, unsafe { EVP_shake128() }, "shake128")
}

/// SHAKE256 of `input`, squeezed to exactly `output_len` bytes.
pub fn shake256(input: &[u8], output_len: i64) -> Result<Vec<u8>, ShakeError> {
    // SAFETY: `EVP_shake256()` returns a static `EVP_MD` descriptor.
    xof(input, output_len, unsafe { EVP_shake256() }, "shake256")
}

/// Absorb `input`, then squeeze exactly `output_len` bytes through `md`.
///
/// The `EVP_MD_CTX` is allocated, used, and freed within this function on every
/// path (including errors) via the RAII `Ctx` guard, so no AWS-LC allocation
/// leaks and no raw pointer is returned to the caller.
fn xof(
    input: &[u8],
    output_len: i64,
    md: *const EVP_MD,
    op: &'static str,
) -> Result<Vec<u8>, ShakeError> {
    let len = usize::try_from(output_len)
        .ok()
        .filter(|&n| n <= MAX_OUTPUT)
        .ok_or(ShakeError::InvalidLength(output_len))?;

    // RAII guard: frees the context in Drop, covering every early return below.
    struct Ctx(*mut aws_lc_sys::EVP_MD_CTX);
    impl Drop for Ctx {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` was produced by `EVP_MD_CTX_new` and is freed
                // exactly once (Drop runs once; the pointer is never copied out).
                unsafe { EVP_MD_CTX_free(self.0) };
            }
        }
    }

    // SAFETY: allocate a fresh digest context; a null return is an allocation
    // failure we surface rather than dereference.
    let ctx = Ctx(unsafe { EVP_MD_CTX_new() });
    if ctx.0.is_null() {
        return Err(ShakeError::Backend(op));
    }

    // SAFETY: `ctx.0` is a valid, freshly-allocated context and `md` is a static
    // descriptor; AWS-LC returns 1 on success. `input` is a valid slice; an empty
    // input yields a null base pointer with len 0, which AWS-LC accepts.
    let ok = unsafe {
        EVP_DigestInit_ex(ctx.0, md, std::ptr::null_mut()) == 1
            && EVP_DigestUpdate(ctx.0, input.as_ptr().cast(), input.len()) == 1
    };
    if !ok {
        return Err(ShakeError::Backend(op));
    }

    let mut out = vec![0u8; len];
    // SAFETY: `out` has capacity `len`; `EVP_DigestFinalXOF` writes exactly `len`
    // bytes into it and returns 1 on success. A zero-length squeeze is valid.
    let finalized = unsafe { EVP_DigestFinalXOF(ctx.0, out.as_mut_ptr(), len) == 1 };
    if !finalized {
        return Err(ShakeError::Backend(op));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // FIPS 202 / NIST test-vector prefixes for the empty message. SHAKE128("")
    // and SHAKE256("") first bytes are stable, well-known values.
    #[test]
    fn empty_message_matches_known_prefixes() {
        let s128 = shake128(b"", 16).expect("shake128");
        assert_eq!(
            s128,
            [
                0x7f, 0x9c, 0x2b, 0xa4, 0xe8, 0x8f, 0x82, 0x7d, 0x61, 0x60, 0x45, 0x50, 0x76, 0x05,
                0x85, 0x3e
            ]
        );
        let s256 = shake256(b"", 16).expect("shake256");
        assert_eq!(
            s256,
            [
                0x46, 0xb9, 0xdd, 0x2b, 0x0b, 0xa8, 0x8d, 0x13, 0x23, 0x3b, 0x3f, 0xeb, 0x74, 0x3e,
                0xeb, 0x24
            ]
        );
    }

    #[test]
    fn length_is_exact_and_prefix_stable() {
        // XOF prefix property: a shorter squeeze is a prefix of a longer one.
        let short = shake256(b"witchy", 8).expect("short");
        let long = shake256(b"witchy", 32).expect("long");
        assert_eq!(short.len(), 8);
        assert_eq!(long.len(), 32);
        assert_eq!(&long[..8], &short[..]);
    }

    #[test]
    fn invalid_lengths_are_rejected() {
        assert_eq!(shake128(b"x", -1), Err(ShakeError::InvalidLength(-1)));
        assert_eq!(
            shake256(b"x", (MAX_OUTPUT as i64) + 1),
            Err(ShakeError::InvalidLength((MAX_OUTPUT as i64) + 1))
        );
        // A zero-length squeeze is valid and empty.
        assert_eq!(shake128(b"x", 0), Ok(Vec::new()));
    }
}
