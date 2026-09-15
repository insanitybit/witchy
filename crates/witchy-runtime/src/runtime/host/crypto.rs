use wasmtime::{bail, Caller, Error, ExternRef, Linker, Result, Rooted};
use witchy_syntax::intrinsics;

use super::super::{memory_of, read_wbytes, read_wstr, read_wstr_list, VmState};
use super::secret::secret_material_ref;
use crate::value::NativeValue as Value;

#[path = "crypto_extensions.rs"]
mod extensions;

pub(in crate::runtime) fn link_canonical_keyed(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "crypto_sign_into", host_crypto_sign_into)?;
    linker.func_wrap("witchy", "crypto_public_key_into", host_crypto_public_key_into)?;
    Ok(())
}

fn checked_keyed_output(data: &[u8], ptr: i32, capacity: i32) -> Result<std::ops::Range<usize>> {
    if ptr < 0 || capacity < 0 { bail!("negative keyed crypto output slice"); }
    let start = ptr as usize;
    let end = start.checked_add(capacity as usize)
        .filter(|end| *end <= data.len()).ok_or_else(|| Error::msg("keyed crypto output slice out of bounds"))?;
    Ok(start..end)
}

fn host_crypto_sign_into(
    mut caller: Caller<'_, VmState>, key: Option<Rooted<ExternRef>>,
    msg_ptr: i32, msg_len: i32, out_ptr: i32, out_cap: i32,
) -> Result<i32> {
    let material = super::secret::canonical_secret_material_ref(&caller, key)?;
    let memory = memory_of(&mut caller)?;
    let message = super::super::read_slice_str(memory.data(&caller), msg_ptr, msg_len)?;
    let output = checked_keyed_output(memory.data(&caller), out_ptr, out_cap)?;
    let provider = crate::native::lookup(intrinsics::CRYPTO_SIGN)
        .ok_or_else(|| Error::msg("crypto.sign is not registered"))?;
    let Value::Str(signature) = provider(&[Value::Secret(material.bytes), Value::Str(message)])
        .map_err(|e| Error::msg(e.message))? else { bail!("crypto.sign did not return a String"); };
    if signature.len() != 128 { bail!("crypto.sign returned an invalid signature length"); }
    if output.len() >= signature.len() {
        memory.write(&mut caller, output.start, signature.as_bytes())?;
    }
    Ok(128)
}

fn host_crypto_public_key_into(
    mut caller: Caller<'_, VmState>, key: Option<Rooted<ExternRef>>, out_ptr: i32, out_cap: i32,
) -> Result<i32> {
    let material = super::secret::canonical_secret_material_ref(&caller, key)?;
    let memory = memory_of(&mut caller)?;
    let output = checked_keyed_output(memory.data(&caller), out_ptr, out_cap)?;
    let provider = crate::native::lookup(intrinsics::CRYPTO_PUBLIC_KEY)
        .ok_or_else(|| Error::msg("crypto.public_key is not registered"))?;
    let Value::Str(public_key) = provider(&[Value::Secret(material.bytes)])
        .map_err(|e| Error::msg(e.message))? else { bail!("crypto.public_key did not return a String"); };
    if public_key.len() != 64 { bail!("crypto.public_key returned an invalid public key length"); }
    if output.len() >= public_key.len() {
        memory.write(&mut caller, output.start, public_key.as_bytes())?;
    }
    Ok(64)
}

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
    extensions::link(linker)?;
    linker.func_wrap("witchy", "crypto_ed25519_verify_slices", host_ed25519_verify_slices)?;
    linker.func_wrap("witchy", "crypto_rsa_pkcs1_sha256_verify_slices", host_rsa_pkcs1_sha256_verify_slices)?;
    linker.func_wrap("witchy", "crypto_sha256_slices", host_sha256_slices)?;
    linker.func_wrap("witchy", "crypto_rune_hash_slices", host_rune_hash_slices)?;
    linker.func_wrap(
        "witchy",
        intrinsics::CRYPTO_ED25519_VERIFY_STATUS,
        host_ed25519_verify_status,
    )?;
    linker.func_wrap("witchy", intrinsics::CRYPTO_SHA256, host_sha256)?;
    linker.func_wrap("witchy", intrinsics::CRYPTO_SHA256_BYTES, host_sha256_bytes)?;
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
    ed25519_status(&args)
}

/// Canonical authority-free verification: every input is a complete bounded
/// read slice, not a guest object header. No guest state is changed or retained.
fn host_ed25519_verify_slices(
    mut caller: Caller<'_, VmState>,
    pk_ptr: i32, pk_len: i32,
    msg_ptr: i32, msg_len: i32,
    sig_ptr: i32, sig_len: i32,
) -> Result<i64> {
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    // Preflight all extents before decoding or invoking the verifier, including
    // inputs the native verifier could otherwise skip after a malformed key.
    let ranges = [
        ed25519_slice_range(pk_ptr, pk_len, data.len())?,
        ed25519_slice_range(msg_ptr, msg_len, data.len())?,
        ed25519_slice_range(sig_ptr, sig_len, data.len())?,
    ];
    let args = ranges.into_iter().map(|range| {
        std::str::from_utf8(&data[range])
            .map(|text| Value::Str(text.to_owned()))
            .map_err(|_| Error::msg("Ed25519 input slice is not valid UTF-8"))
    }).collect::<Result<Vec<_>>>()?;
    ed25519_status(&args)
}

fn ed25519_slice_range(ptr: i32, len: i32, limit: usize) -> Result<std::ops::Range<usize>> {
    let start = ptr as u32 as usize;
    let end = start.checked_add(len as u32 as usize)
        .filter(|end| *end <= limit)
        .ok_or_else(|| Error::msg("Ed25519 input slice lies outside guest memory"))?;
    Ok(start..end)
}

// Both ABIs delegate status policy and cryptographic verification to the same
// native primitive used by the source interpreter.
fn ed25519_status(args: &[Value]) -> Result<i64> {
    let f = crate::native::lookup(intrinsics::CRYPTO_ED25519_VERIFY_STATUS).ok_or_else(|| {
        Error::msg(format!(
            "{} is not registered",
            intrinsics::CRYPTO_ED25519_VERIFY_STATUS
        ))
    })?;
    match f(args).map_err(|e| Error::msg(e.message))? {
        Value::Int(n) => Ok(n),
        _ => Err(Error::msg(
            "crypto.__ed25519_verify_status did not return an Int",
        )),
    }
}

#[cfg(test)]
#[path = "crypto_slices_tests.rs"]
mod slices_tests;

#[cfg(test)]
#[path = "crypto_hash_slices_tests.rs"]
mod hash_slices_tests;

#[cfg(test)]
#[path = "crypto_rsa_slices_tests.rs"]
mod rsa_slices_tests;

/// Authority-free RSA verification over three initialized read slices. Validate
/// every range before decoding, and every string before the native provider can
/// short-circuit on a malformed public key. No guest writes or retained state.
fn host_rsa_pkcs1_sha256_verify_slices(
    mut caller: Caller<'_, VmState>,
    pk_ptr: i32, pk_len: i32,
    msg_ptr: i32, msg_len: i32,
    sig_ptr: i32, sig_len: i32,
) -> Result<i64> {
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    let range = |ptr: i32, len: i32| {
        let start = ptr as u32 as usize;
        start.checked_add(len as u32 as usize)
            .filter(|end| *end <= data.len())
            .map(|end| start..end)
            .ok_or_else(|| Error::msg("RSA input slice lies outside guest memory"))
    };
    let ranges = [range(pk_ptr, pk_len)?, range(msg_ptr, msg_len)?, range(sig_ptr, sig_len)?];
    let args = ranges.into_iter().map(|range| {
        std::str::from_utf8(&data[range])
            .map(|text| Value::Str(text.to_owned()))
            .map_err(|_| Error::msg("RSA input slice is not valid UTF-8"))
    }).collect::<Result<Vec<_>>>()?;
    let native = intrinsics::CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS;
    let provider = crate::native::lookup(native)
        .ok_or_else(|| Error::msg(format!("{native} is not registered")))?;
    match provider(&args).map_err(|error| Error::msg(error.message))? {
        Value::Int(status) => Ok(status),
        _ => Err(Error::msg(format!("{native} did not return an Int"))),
    }
}

fn host_sha256_slices(caller: Caller<'_, VmState>, input: i32, len: i32, output: i32, capacity: i32) -> Result<i32> {
    hash_slices(caller, input, len, output, capacity, HashSlices::Sha256)
}

fn host_rune_hash_slices(caller: Caller<'_, VmState>, input: i32, len: i32, output: i32, capacity: i32) -> Result<i32> {
    hash_slices(caller, input, len, output, capacity, HashSlices::Rune)
}

enum HashSlices { Sha256, Rune }

fn hash_slices(
    mut caller: Caller<'_, VmState>, input: i32, len: i32, output: i32, capacity: i32,
    kind: HashSlices,
) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    let range = |ptr: i32, len: i32| {
        let start = ptr as u32 as usize;
        start.checked_add(len as u32 as usize).filter(|end| *end <= data.len())
            .map(|end| start..end)
            .ok_or_else(|| Error::msg("crypto hash slice lies outside guest memory"))
    };
    // Preflight both complete extents before allocating, decoding, or hashing.
    let input = range(input, len)?;
    let output = range(output, capacity)?;
    let (name, args, required) = match kind {
        HashSlices::Sha256 => (intrinsics::CRYPTO_SHA256_BYTES, vec![Value::Bytes(data[input].to_vec())], 64),
        HashSlices::Rune => (intrinsics::CRYPTO_RUNE_HASH, decode_rune_transport(&data[input])?, 71),
    };
    let primitive = crate::native::lookup(name)
        .ok_or_else(|| Error::msg(format!("{name} is not registered")))?;
    let digest = match primitive(&args).map_err(|error| Error::msg(error.message))? {
        Value::Str(digest) if digest.len() == required => digest,
        _ => return Err(Error::msg(format!("{name} returned an invalid digest shape"))),
    };
    // All inputs are owned host values before an overlapping output is written.
    // Short outputs still validate inputs, but never receive a partial digest.
    if output.len() >= required {
        memory.write(&mut caller, output.start, digest.as_bytes())
            .map_err(|error| Error::msg(format!("writing crypto hash result: {error}")))?;
    }
    Ok(required as i32)
}

fn rune_transport_bytes<'a>(data: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8]> {
    let end = cursor.checked_add(len).filter(|end| *end <= data.len())
        .ok_or_else(|| Error::msg("rune hash transport is truncated"))?;
    let bytes = &data[*cursor..end];
    *cursor = end;
    Ok(bytes)
}

fn rune_transport_word(data: &[u8], cursor: &mut usize) -> Result<usize> {
    let word = rune_transport_bytes(data, cursor, 4)?;
    Ok(u32::from_le_bytes(word.try_into().expect("four-byte transport word")) as usize)
}

fn walk_rune_array(
    data: &[u8], cursor: &mut usize, mut string: impl FnMut(&str) -> Result<()>,
) -> Result<usize> {
    let count = rune_transport_word(data, cursor)?;
    // Every remaining string needs at least its length word. Never allocate
    // or iterate from a cardinality unsupported by the bounded input itself.
    if count > (data.len() - *cursor) / 4 {
        return Err(Error::msg("rune hash string count exceeds its transport"));
    }
    for _ in 0..count {
        let len = rune_transport_word(data, cursor)?;
        let bytes = rune_transport_bytes(data, cursor, len)?;
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Error::msg("rune hash input string is not valid UTF-8"))?;
        string(text)?;
    }
    Ok(count)
}

fn decode_rune_transport(data: &[u8]) -> Result<Vec<Value>> {
    // Validate both arrays, all UTF-8 byte lengths, and the exact end before
    // reserving space for any native list or copying any string.
    let mut cursor = 0;
    let paths = walk_rune_array(data, &mut cursor, |_| Ok(()))?;
    let contents = walk_rune_array(data, &mut cursor, |_| Ok(()))?;
    if cursor != data.len() {
        return Err(Error::msg("rune hash transport has trailing bytes"));
    }
    cursor = 0;
    let mut args = Vec::new();
    for count in [paths, contents] {
        let mut values = Vec::new();
        values.try_reserve_exact(count).map_err(|error| Error::msg(error.to_string()))?;
        walk_rune_array(data, &mut cursor, |text| {
            let mut owned = String::new();
            owned.try_reserve_exact(text.len()).map_err(|error| Error::msg(error.to_string()))?;
            owned.push_str(text);
            values.push(Value::Str(owned));
            Ok(())
        })?;
        args.push(Value::List(values));
    }
    // This is transport, not the digest preimage. The native primitive retains
    // list-length errors, stable sorting, normalization, and hashing policy.
    Ok(args)
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

/// (RFC-0095) Read a raw `Bytes` header from guest memory, compute its SHA-256 via
/// the shared native registry, and write the 64 hex bytes into `out_ptr`. Mirrors
/// `host_sha256` but reads the input as bytes (not a UTF-8 string).
fn host_sha256_bytes(mut caller: Caller<'_, VmState>, in_ptr: i32, out_ptr: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let input = read_wbytes(mem.data(&caller), in_ptr)?;
    let f = crate::native::lookup(intrinsics::CRYPTO_SHA256_BYTES).ok_or_else(|| {
        Error::msg(format!("{} is not registered", intrinsics::CRYPTO_SHA256_BYTES))
    })?;
    let hex = match f(&[Value::Bytes(input)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.sha256_bytes did not return a String")),
    };
    if hex.len() != 64 {
        return Err(Error::msg("crypto.sha256_bytes hex digest is not 64 bytes"));
    }
    mem.write(&mut caller, out_ptr as usize, hex.as_bytes())
        .map_err(|e| Error::msg(format!("writing sha256_bytes result into guest memory: {e}")))
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
