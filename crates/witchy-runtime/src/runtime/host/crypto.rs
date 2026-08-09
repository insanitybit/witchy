use wasmtime::{bail, Caller, Error, ExternRef, Linker, Result, Rooted};
use witchy_syntax::intrinsics;

use super::super::{memory_of, read_wbytes, read_wstr, read_wstr_list, VmState};
use super::secret::secret_material_ref;
use crate::value::NativeValue as Value;

/// Register crypto operations that consume opaque host-side secret material.
pub(in crate::runtime) fn link_keyed(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", intrinsics::CRYPTO_SIGN, host_crypto_sign)?;
    linker.func_wrap(
        "witchy",
        intrinsics::CRYPTO_PUBLIC_KEY,
        host_crypto_public_key,
    )?;
    Ok(())
}

/// Register authority-free crypto operations.
pub(in crate::runtime) fn link_pure(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap(
        "witchy",
        intrinsics::CRYPTO_ED25519_VERIFY_STATUS,
        host_ed25519_verify_status,
    )?;
    linker.func_wrap("witchy", intrinsics::CRYPTO_SHA256, host_sha256)?;
    linker.func_wrap(
        "witchy",
        intrinsics::CRYPTO_ECDSA_P256_VERIFY_STATUS,
        host_ecdsa_p256_verify_status,
    )?;
    linker.func_wrap(
        "witchy",
        intrinsics::CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS,
        host_ecdsa_p256_verify_hex_status,
    )?;
    linker.func_wrap(
        "witchy",
        intrinsics::CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS,
        host_rsa_pkcs1_sha256_verify_status,
    )?;
    linker.func_wrap("witchy", intrinsics::CRYPTO_SHA512, host_sha512)?;
    linker.func_wrap("witchy", intrinsics::CRYPTO_SHA3_256, host_sha3_256)?;
    linker.func_wrap("witchy", intrinsics::CRYPTO_HMAC_SHA256, host_hmac_sha256)?;
    linker.func_wrap(
        "witchy",
        intrinsics::CRYPTO_RUNE_HASH,
        host_crypto_rune_hash,
    )?;
    // (RFC-0106) SHAKE XOFs — native-only. The browser host deliberately does
    // NOT register these, so a compiled module reaching them cannot instantiate.
    linker.func_wrap("witchy", intrinsics::CRYPTO_SHAKE128, host_shake128)?;
    linker.func_wrap("witchy", intrinsics::CRYPTO_SHAKE256, host_shake256)?;
    Ok(())
}

/// Register the checked reveal bridge after secret-store lookup. Revealing a
/// value needs a non-null Secret ref and applies its own sealed/signing-key
/// checks, so the import itself carries no authority.
pub(in crate::runtime) fn link_reveal(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "crypto_reveal_len", host_crypto_reveal_len)?;
    Ok(())
}

/// `crypto.__ed25519_verify_status(pk, msg, sig) -> Int`, bridged to the shared native
/// registry (the same implementation the interpreter uses). Each argument is a
/// pointer to a witchy string header (`[i32 len][bytes]`).
fn host_ed25519_verify_status(
    mut caller: Caller<'_, VmState>,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i64> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let args = [
        Value::Str(read_wstr(data, pk)?),
        Value::Str(read_wstr(data, msg)?),
        Value::Str(read_wstr(data, sig)?),
    ];
    let f = crate::native::lookup(intrinsics::CRYPTO_ED25519_VERIFY_STATUS).ok_or_else(|| {
        Error::msg(format!(
            "{} is not registered",
            intrinsics::CRYPTO_ED25519_VERIFY_STATUS
        ))
    })?;
    match f(&args).map_err(|e| Error::msg(e.message))? {
        Value::Int(n) => Ok(n),
        _ => Err(Error::msg(
            "crypto.__ed25519_verify_status did not return an Int",
        )),
    }
}

/// `crypto.sha256(in_header_ptr, out_data_ptr)`: read the input string, compute
/// its SHA-256 (via the shared native registry), and write the 64 hex bytes into
/// guest memory at `out_data_ptr` (the guest pre-allocated the result string).
fn host_sha256(mut caller: Caller<'_, VmState>, in_ptr: i32, out_ptr: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let input = read_wstr(mem.data(&caller), in_ptr)?;
    let f = crate::native::lookup(intrinsics::CRYPTO_SHA256)
        .ok_or_else(|| Error::msg(format!("{} is not registered", intrinsics::CRYPTO_SHA256)))?;
    let hex = match f(&[Value::Str(input)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.sha256 did not return a String")),
    };
    if hex.len() != 64 {
        return Err(Error::msg("crypto.sha256 hex digest is not 64 bytes"));
    }
    mem.write(&mut caller, out_ptr as usize, hex.as_bytes())
        .map_err(|e| Error::msg(format!("writing sha256 result into guest memory: {e}")))
}

/// Shared bridge for private three-string crypto verifier status intrinsics:
/// read three string headers, dispatch through the shared native registry (the
/// SAME aws-lc-rs impl the interpreter uses), return an Int status.
fn host_crypto_verify_status3(
    mut caller: Caller<'_, VmState>,
    native: &str,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i64> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let args = [
        Value::Str(read_wstr(data, pk)?),
        Value::Str(read_wstr(data, msg)?),
        Value::Str(read_wstr(data, sig)?),
    ];
    let f = crate::native::lookup(native)
        .ok_or_else(|| Error::msg(format!("{native} is not registered")))?;
    match f(&args).map_err(|e| Error::msg(e.message))? {
        Value::Int(n) => Ok(n),
        _ => Err(Error::msg(format!("{native} did not return an Int"))),
    }
}

/// Shared bridge for the fixed-width hex digests (`sha512`, `sha3_256`,
/// `hmac_sha256`): read one or two input string headers, compute via the shared
/// native registry, and write the `expect`-length hex digest into the guest's
/// pre-allocated buffer at `out_ptr`.
fn host_crypto_digest(
    mut caller: Caller<'_, VmState>,
    native: &str,
    inputs: &[i32],
    out_ptr: i32,
    expect: usize,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let args: Vec<Value> = inputs
        .iter()
        .map(|&p| read_wstr(data, p).map(Value::Str))
        .collect::<Result<_>>()?;
    let f = crate::native::lookup(native)
        .ok_or_else(|| Error::msg(format!("{native} is not registered")))?;
    let hex = match f(&args).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg(format!("{native} did not return a String"))),
    };
    if hex.len() != expect {
        return Err(Error::msg(format!(
            "{native} hex digest is not {expect} bytes"
        )));
    }
    mem.write(&mut caller, out_ptr as usize, hex.as_bytes())
        .map_err(|e| Error::msg(format!("writing {native} result into guest memory: {e}")))
}

fn host_ecdsa_p256_verify_status(
    caller: Caller<'_, VmState>,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i64> {
    host_crypto_verify_status3(
        caller,
        intrinsics::CRYPTO_ECDSA_P256_VERIFY_STATUS,
        pk,
        msg,
        sig,
    )
}

fn host_ecdsa_p256_verify_hex_status(
    caller: Caller<'_, VmState>,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i64> {
    host_crypto_verify_status3(
        caller,
        intrinsics::CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS,
        pk,
        msg,
        sig,
    )
}

fn host_rsa_pkcs1_sha256_verify_status(
    caller: Caller<'_, VmState>,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i64> {
    host_crypto_verify_status3(
        caller,
        intrinsics::CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS,
        pk,
        msg,
        sig,
    )
}

fn host_sha512(caller: Caller<'_, VmState>, in_ptr: i32, out_ptr: i32) -> Result<()> {
    host_crypto_digest(caller, intrinsics::CRYPTO_SHA512, &[in_ptr], out_ptr, 128)
}

fn host_sha3_256(caller: Caller<'_, VmState>, in_ptr: i32, out_ptr: i32) -> Result<()> {
    host_crypto_digest(caller, intrinsics::CRYPTO_SHA3_256, &[in_ptr], out_ptr, 64)
}

/// (RFC-0106) Shared SHAKE XOF bridge: read the guest `Bytes` input at `in_ptr`
/// (`[len][payload]`), squeeze exactly `out_len` bytes through the shared native
/// adapter, and write them into the pre-allocated guest buffer at `out_ptr`. The
/// guest helper already validated `out_len` to `0..=1048576` and reserved the
/// buffer; this is a direct output-pointer write.
fn host_shake(
    mut caller: Caller<'_, VmState>,
    which: &str,
    in_ptr: i32,
    out_ptr: i32,
    out_len: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let input = read_wbytes(mem.data(&caller), in_ptr)?;
    let len = i64::from(out_len);
    let squeezed = match which {
        intrinsics::CRYPTO_SHAKE128 => crate::shake::shake128(&input, len),
        _ => crate::shake::shake256(&input, len),
    }
    .map_err(|e| Error::msg(e.message()))?;
    mem.write(&mut caller, out_ptr as usize, &squeezed)
        .map_err(|e| Error::msg(format!("writing {which} result into guest memory: {e}")))
}

fn host_shake128(caller: Caller<'_, VmState>, in_ptr: i32, out_ptr: i32, out_len: i32) -> Result<()> {
    host_shake(caller, intrinsics::CRYPTO_SHAKE128, in_ptr, out_ptr, out_len)
}

fn host_shake256(caller: Caller<'_, VmState>, in_ptr: i32, out_ptr: i32, out_len: i32) -> Result<()> {
    host_shake(caller, intrinsics::CRYPTO_SHAKE256, in_ptr, out_ptr, out_len)
}

fn host_hmac_sha256(
    caller: Caller<'_, VmState>,
    key_ptr: i32,
    msg_ptr: i32,
    out_ptr: i32,
) -> Result<()> {
    host_crypto_digest(
        caller,
        intrinsics::CRYPTO_HMAC_SHA256,
        &[key_ptr, msg_ptr],
        out_ptr,
        64,
    )
}

/// `crypto.rune_hash(paths_ptr, contents_ptr, out_data_ptr)`: read both guest
/// string lists, compute the store's content hash through the shared native
/// registry, and write the fixed 71-byte `sha256:<hex>` result into the
/// guest-allocated string.
fn host_crypto_rune_hash(
    mut caller: Caller<'_, VmState>,
    paths_ptr: i32,
    contents_ptr: i32,
    out_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let paths = read_wstr_list(mem.data(&caller), paths_ptr)?;
    let contents = read_wstr_list(mem.data(&caller), contents_ptr)?;
    let to_list = |xs: Vec<String>| Value::List(xs.into_iter().map(Value::Str).collect());
    let f = crate::native::lookup(intrinsics::CRYPTO_RUNE_HASH).ok_or_else(|| {
        Error::msg(format!(
            "{} is not registered",
            intrinsics::CRYPTO_RUNE_HASH
        ))
    })?;
    let hash = match f(&[to_list(paths), to_list(contents)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.rune_hash did not return a String")),
    };
    if hash.len() != 71 {
        return Err(Error::msg("crypto.rune_hash result is not 71 bytes"));
    }
    mem.write(&mut caller, out_ptr as usize, hash.as_bytes())
        .map_err(|e| Error::msg(format!("writing rune_hash result into guest memory: {e}")))
}

/// `crypto.sign(key, msg_ptr, out_data_ptr)`: sign the message with the opaque
/// Secret externref (host-side bytes) and write the 128 hex signature bytes into
/// the guest's pre-allocated string — through the same native registry the
/// interpreter uses, so the output is byte-identical.
fn host_crypto_sign(
    mut caller: Caller<'_, VmState>,
    key: Option<Rooted<ExternRef>>,
    msg_ptr: i32,
    out_ptr: i32,
) -> Result<()> {
    #[cfg(feature = "test-fixtures")]
    if caller.data().fixture_host.is_some() {
        let secret = super::fixture::fixture_handle(&caller, key, "Secret")?;
        let mem = memory_of(&mut caller)?;
        let message = read_wstr(mem.data(&caller), msg_ptr)?;
        let signature = match super::fixture::invoke(
            &mut caller,
            witchy_test_host::HostRequest::SecretSign { secret, message },
        )? {
            witchy_test_host::HostResponse::String(signature) => signature,
            response => {
                return Err(Error::msg(format!(
                    "fixture crypto.sign returned unexpected response {response:?}"
                )));
            }
        };
        return mem
            .write(&mut caller, out_ptr as usize, signature.as_bytes())
            .map_err(|error| Error::msg(format!("writing fixture signature: {error}")));
    }
    let bytes = secret_material_ref(&caller, key)?.bytes;
    let mem = memory_of(&mut caller)?;
    let msg = read_wstr(mem.data(&caller), msg_ptr)?;
    let f = crate::native::lookup(intrinsics::CRYPTO_SIGN)
        .ok_or_else(|| Error::msg(format!("{} is not registered", intrinsics::CRYPTO_SIGN)))?;
    let sig =
        match f(&[Value::Secret(bytes), Value::Str(msg)]).map_err(|e| Error::msg(e.message))? {
            Value::Str(s) => s,
            _ => return Err(Error::msg("crypto.sign did not return a String")),
        };
    mem.write(&mut caller, out_ptr as usize, sig.as_bytes())
        .map_err(|e| Error::msg(format!("writing signature into guest memory: {e}")))
}

/// `crypto.public_key(key, out_data_ptr)`: write the 64 hex public-key bytes of
/// the opaque Secret externref (safe to publish) into the guest's pre-allocated
/// string.
fn host_crypto_public_key(
    mut caller: Caller<'_, VmState>,
    key: Option<Rooted<ExternRef>>,
    out_ptr: i32,
) -> Result<()> {
    #[cfg(feature = "test-fixtures")]
    if caller.data().fixture_host.is_some() {
        let secret = super::fixture::fixture_handle(&caller, key, "Secret")?;
        let public_key = match super::fixture::invoke(
            &mut caller,
            witchy_test_host::HostRequest::SecretPublicKey { secret },
        )? {
            witchy_test_host::HostResponse::String(public_key) => public_key,
            response => {
                return Err(Error::msg(format!(
                    "fixture crypto.public_key returned unexpected response {response:?}"
                )));
            }
        };
        let mem = memory_of(&mut caller)?;
        return mem
            .write(&mut caller, out_ptr as usize, public_key.as_bytes())
            .map_err(|error| Error::msg(format!("writing fixture public key: {error}")));
    }
    let bytes = secret_material_ref(&caller, key)?.bytes;
    let f = crate::native::lookup(intrinsics::CRYPTO_PUBLIC_KEY).ok_or_else(|| {
        Error::msg(format!(
            "{} is not registered",
            intrinsics::CRYPTO_PUBLIC_KEY
        ))
    })?;
    let pk = match f(&[Value::Secret(bytes)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.public_key did not return a String")),
    };
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, out_ptr as usize, pk.as_bytes())
        .map_err(|e| Error::msg(format!("writing public key into guest memory: {e}")))
}

/// `crypto_reveal_len(key) -> i32`: reveal the opaque Secret externref as a
/// string (its raw bytes, lossy UTF-8 — through the same native registry the
/// interpreter uses), stage it for `fill_pending`, and report its byte length.
/// For value secrets handed to an external sink; the bytes only cross into the
/// guest here.
fn host_crypto_reveal_len(
    mut caller: Caller<'_, VmState>,
    key: Option<Rooted<ExternRef>>,
) -> Result<i32> {
    #[cfg(feature = "test-fixtures")]
    if caller.data().fixture_host.is_some() {
        let secret = super::fixture::fixture_handle(&caller, key, "Secret")?;
        return match super::fixture::invoke(
            &mut caller,
            witchy_test_host::HostRequest::SecretReveal { secret },
        )? {
            witchy_test_host::HostResponse::String(value) => {
                let length = i32::try_from(value.len()).map_err(|_| {
                    Error::msg("fixture Secret reveal exceeds the guest ABI size limit")
                })?;
                caller.data_mut().pending = Some(value.into_bytes());
                Ok(length)
            }
            response => Err(Error::msg(format!(
                "fixture crypto.reveal returned unexpected response {response:?}"
            ))),
        };
    }
    let secret = secret_material_ref(&caller, key)?;
    // (RFC-0060) A sealed secret is consumable by opaque ref but never revealable.
    if secret.sealed {
        bail!(
            "{}",
            witchy_caps::capabilities::SEALED_SECRET_REVEAL_ERROR
        );
    }
    if witchy_caps::capabilities::secret_is_signing_key(
        caller
            .data()
            .caps
            .signing_key
            .as_ref()
            .map(|s| s.as_slice()),
        &secret.bytes,
    ) {
        bail!("the signing key is not revealable; use crypto.sign / crypto.public_key");
    }
    let f = crate::native::lookup(intrinsics::CRYPTO_REVEAL)
        .ok_or_else(|| Error::msg(format!("{} is not registered", intrinsics::CRYPTO_REVEAL)))?;
    let s = match f(&[Value::Secret(secret.bytes)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.reveal did not return a String")),
    };
    let len = s.len() as i32;
    caller.data_mut().pending = Some(s.into_bytes());
    Ok(len)
}
