//! Fixed, authority-free crypto transports. No managed guest headers cross.
use crate::runtime::GuestAddress;
use super::*;

pub(super) fn link<P: GuestAddress>(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "crypto_sha512_slices", sha512::<P>)?;
    linker.func_wrap("witchy", "crypto_sha3_256_slices", sha3::<P>)?;
    linker.func_wrap("witchy", "crypto_hmac_sha256_slices", hmac::<P>)?;
    linker.func_wrap("witchy", "crypto_shake128_slices", shake128::<P>)?;
    linker.func_wrap("witchy", "crypto_shake256_slices", shake256::<P>)?;
    linker.func_wrap("witchy", "crypto_ecdsa_p256_verify_slices", ecdsa::<P>)?;
    linker.func_wrap("witchy", "crypto_ecdsa_p256_verify_hex_slices", ecdsa_hex::<P>)?;
    Ok(())
}

fn range<P: GuestAddress>(data: &[u8], pointer: P, length: i32) -> Result<std::ops::Range<usize>> {
    if length < 0 { bail!("crypto extension slice lies outside guest memory"); }
    let start = pointer.offset();
    start.checked_add(length as usize).filter(|end| *end <= data.len())
        .map(|end| start..end).ok_or_else(|| Error::msg("crypto extension slice lies outside guest memory"))
}

fn string(bytes: &[u8]) -> Result<Value> {
    std::str::from_utf8(bytes).map(|text| Value::Str(text.to_owned()))
        .map_err(|_| Error::msg("crypto extension input is not valid UTF-8"))
}

fn digest<P: GuestAddress>(mut caller: Caller<'_, VmState>, native: &str, inputs: &[(P, i32)],
    output: P, capacity: i32, requested: Option<i64>) -> Result<i32>
{
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    let inputs = inputs.iter().map(|&(pointer, length)| range(data, pointer, length))
        .collect::<Result<Vec<_>>>()?;
    let output = range(data, output, capacity)?;
    // Every complete extent precedes decoding, length validation, or provider
    // execution. Inputs become owned before a possibly overlapping write.
    let (args, required) = if let Some(requested) = requested {
        if !(0..=1_048_576).contains(&requested) { bail!("invalid SHAKE output length"); }
        (vec![Value::Bytes(data[inputs[0].clone()].to_vec()), Value::Int(requested)], requested as usize)
    } else {
        (inputs.into_iter().map(|input| string(&data[input])).collect::<Result<Vec<_>>>()?,
            if native == intrinsics::CRYPTO_SHA512 { 128 } else { 64 })
    };
    let provider = crate::native::lookup(native)
        .ok_or_else(|| Error::msg(format!("{native} is not registered")))?;
    let bytes = match provider(&args).map_err(|error| Error::msg(error.message))? {
        Value::Bytes(bytes) if requested.is_some() && bytes.len() == required => bytes,
        Value::Str(text) if requested.is_none() && text.len() == required => text.into_bytes(),
        _ => bail!("crypto extension provider returned an invalid digest shape"),
    };
    if required <= output.len() { memory.write(&mut caller, output.start, &bytes)?; }
    Ok(required as i32)
}

fn sha512<P: GuestAddress>(caller: Caller<'_, VmState>, p: P, n: i32, out: P, cap: i32) -> Result<i32> {
    digest(caller, intrinsics::CRYPTO_SHA512, &[(p, n)], out, cap, None)
}
fn sha3<P: GuestAddress>(caller: Caller<'_, VmState>, p: P, n: i32, out: P, cap: i32) -> Result<i32> {
    digest(caller, intrinsics::CRYPTO_SHA3_256, &[(p, n)], out, cap, None)
}
fn hmac<P: GuestAddress>(caller: Caller<'_, VmState>, k: P, kn: i32, p: P, n: i32, out: P, cap: i32) -> Result<i32> {
    digest(caller, intrinsics::CRYPTO_HMAC_SHA256, &[(k, kn), (p, n)], out, cap, None)
}
fn shake128<P: GuestAddress>(caller: Caller<'_, VmState>, p: P, n: i32, length: i64, out: P, cap: i32) -> Result<i32> {
    digest(caller, intrinsics::CRYPTO_SHAKE128, &[(p, n)], out, cap, Some(length))
}
fn shake256<P: GuestAddress>(caller: Caller<'_, VmState>, p: P, n: i32, length: i64, out: P, cap: i32) -> Result<i32> {
    digest(caller, intrinsics::CRYPTO_SHAKE256, &[(p, n)], out, cap, Some(length))
}

fn verify<P: GuestAddress>(mut caller: Caller<'_, VmState>, native: &str, inputs: [(P, i32); 3]) -> Result<i64> {
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    let ranges = inputs.into_iter().map(|(p, n)| range(data, p, n)).collect::<Result<Vec<_>>>()?;
    let args = ranges.into_iter().map(|range| string(&data[range])).collect::<Result<Vec<_>>>()?;
    let provider = crate::native::lookup(native)
        .ok_or_else(|| Error::msg(format!("{native} is not registered")))?;
    match provider(&args).map_err(|error| Error::msg(error.message))? {
        Value::Int(status) => Ok(status),
        _ => bail!("crypto verifier did not return an Int"),
    }
}
fn ecdsa<P: GuestAddress>(caller: Caller<'_, VmState>, k: P, kn: i32, p: P, n: i32, s: P, sn: i32) -> Result<i64> {
    verify(caller, intrinsics::CRYPTO_ECDSA_P256_VERIFY_STATUS, [(k, kn), (p, n), (s, sn)])
}
fn ecdsa_hex<P: GuestAddress>(caller: Caller<'_, VmState>, k: P, kn: i32, p: P, n: i32, s: P, sn: i32) -> Result<i64> {
    verify(caller, intrinsics::CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS, [(k, kn), (p, n), (s, sn)])
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "crypto_extensions_tests.rs"]
pub mod tests;
