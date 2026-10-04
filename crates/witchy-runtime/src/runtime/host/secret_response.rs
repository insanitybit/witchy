//! `crypto.reveal` answered by an immutable response (RFC-0126 rule 3).
//!
//! The host reads the secret once. A host-sealed secret or the signing key is
//! the response's `Denied` status, not a trap; a forged or wrong-kind token
//! still traps.
use std::sync::Arc;

use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::{memory_of, VmState};
use super::secret::secret_material_ref;

/// `CapError::Denied`'s status: its variant ordinal plus one.
const DENIED: u8 = 1;

/// Private immutable reveal result, not a `Secret`. Neither an integer kind
/// nor bytes supplied by the guest can construct one. `status` is 0 with the
/// revealed bytes, or `DENIED` with a fixed refusal message.
#[derive(Clone)]
pub(in crate::runtime) struct SecretResponse {
    issuer: Arc<()>,
    status: u8,
    bytes: Arc<Vec<u8>>,
}

pub(in crate::runtime) fn link_canonical(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "secret_reveal_at", host_secret_reveal_at)?;
    linker.func_wrap("witchy", "secret_response_status", host_secret_response_status)?;
    linker.func_wrap("witchy", "secret_response_size", host_secret_response_size)?;
    linker.func_wrap("witchy", "secret_response_into", host_secret_response_into)?;
    Ok(())
}

fn length(bytes: usize) -> Result<i32> {
    i32::try_from(bytes).map_err(|_| Error::msg("Secret response exceeds the guest ABI size limit"))
}

/// The status and payload revealing `key` answers. The refusal messages are
/// guest-visible and fixed: they never carry key material or host detail.
fn reveal(caller: &Caller<'_, VmState>, key: Option<Rooted<ExternRef>>) -> Result<(u8, Vec<u8>)> {
    let secret = secret_material_ref(caller, key)?;
    // (RFC-0060) A sealed secret is consumable by opaque ref but never revealable.
    if secret.sealed {
        return Ok((DENIED, witchy_caps::capabilities::SEALED_SECRET_REVEAL_ERROR.as_bytes().to_vec()));
    }
    let signing = caller.data().caps.signing_key.as_ref().map(|key| key.as_slice());
    if witchy_caps::capabilities::secret_is_signing_key(signing, &secret.bytes) {
        return Ok((DENIED, b"the signing key is not revealable; use crypto.sign / crypto.public_key".to_vec()));
    }
    // The same lossy UTF-8 view the native registry gives a revealed String.
    Ok((0, String::from_utf8_lossy(&secret.bytes).into_owned().into_bytes()))
}

/// `secret_reveal_at(key) -> response`: read `key` once.
fn host_secret_reveal_at(
    mut caller: Caller<'_, VmState>,
    key: Option<Rooted<ExternRef>>,
) -> Result<Option<Rooted<ExternRef>>> {
    #[cfg(feature = "test-fixtures")]
    if caller.data().fixture_host.is_some() {
        let secret = super::fixture::fixture_handle(&caller, key, "Secret")?;
        let bytes = match super::fixture::invoke(
            &mut caller,
            witchy_test_host::HostRequest::SecretReveal { secret },
        )? {
            witchy_test_host::HostResponse::String(value) => value.into_bytes(),
            response => return Err(Error::msg(format!(
                "fixture crypto.reveal returned unexpected response {response:?}"
            ))),
        };
        length(bytes.len())?;
        let response = SecretResponse { issuer: caller.data().registry_issuer.clone(), status: 0, bytes: Arc::new(bytes) };
        return ExternRef::new(&mut caller, response).map(Some);
    }
    let (status, bytes) = reveal(&caller, key)?;
    length(bytes.len())?;
    let response = SecretResponse { issuer: caller.data().registry_issuer.clone(), status, bytes: Arc::new(bytes) };
    ExternRef::new(&mut caller, response).map(Some)
}

fn secret_response(caller: &Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<SecretResponse> {
    let token = token.ok_or_else(|| Error::msg("Secret response is null"))?;
    let data = token.data(caller)?.ok_or_else(|| Error::msg("Secret response has no host data"))?;
    let response = data.downcast_ref::<SecretResponse>()
        .ok_or_else(|| Error::msg("Secret response has wrong host kind"))?;
    if !Arc::ptr_eq(&caller.data().registry_issuer, &response.issuer) {
        return Err(Error::msg("Secret response belongs to another instance"));
    }
    Ok(response.clone())
}

fn host_secret_response_status(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    Ok(i32::from(secret_response(&caller, token)?.status))
}

fn host_secret_response_size(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    length(secret_response(&caller, token)?.bytes.len())
}

/// Copy the whole payload when `capacity` holds it and answer its length
/// either way; a short buffer is left untouched.
fn host_secret_response_into(
    mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, out_ptr: i32, capacity: i32,
) -> Result<i32> {
    let bytes = secret_response(&caller, token)?.bytes;
    let required = length(bytes.len())?;
    let start = usize::try_from(out_ptr).map_err(|_| Error::msg("negative Secret response pointer"))?;
    let room = usize::try_from(capacity).map_err(|_| Error::msg("negative Secret response capacity"))?;
    let memory = memory_of(&mut caller)?;
    let end = start.checked_add(room).filter(|end| *end <= memory.data_size(&caller))
        .ok_or_else(|| Error::msg("Secret response buffer lies outside guest memory"))?;
    if end - start >= bytes.len() {
        memory.write(&mut caller, start, bytes.as_slice())?;
    }
    Ok(required)
}
