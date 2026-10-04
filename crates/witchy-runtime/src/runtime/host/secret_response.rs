//! `crypto.reveal` answered by an immutable response (RFC-0126 rule 3).
//!
//! The host reads the secret once. A host-sealed secret or the signing key is
//! the response's `Denied` status, not a trap; a forged or wrong-kind token
//! still traps.
use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::VmState;
use super::cap_response::respond;
use super::secret::secret_material_ref;

/// `CapError::Denied`'s status: its variant ordinal plus one.
const DENIED: u8 = 1;

pub(in crate::runtime) fn link_canonical(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "secret_reveal_at", host_secret_reveal_at)?;
    Ok(())
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
        return respond(&mut caller, 0, bytes, None);
    }
    let (status, bytes) = reveal(&caller, key)?;
    respond(&mut caller, status, bytes, None)
}
