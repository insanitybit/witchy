//! An opaque TLS server signing key. The guest provides only a transcript
//! hash. The provider boundary enforces the TLS 1.3 server signature domain.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P384_SHA384_ASN1_SIGNING, EcdsaKeyPair};
use wasmtime::{Caller, Linker};

use super::{SecretEntry, State, guest_range, reject, write_guest};

#[path = "../../../src/compiler/tls_signature.rs"]
mod tls_signature;
use tls_signature::{DER_CAPACITY, HASH_LEN, server_message};

pub(super) fn from_pkcs8(bytes: &[u8]) -> Result<EcdsaKeyPair, &'static str> {
    super::sha::provider()?;
    EcdsaKeyPair::from_pkcs8(&ECDSA_P384_SHA384_ASN1_SIGNING, bytes)
        .map_err(|_| "expected a P-384 PKCS#8 DER private key")
}

pub(super) fn register(linker: &mut Linker<State>, namespace: &str) -> wasmtime::Result<()> {
    linker.func_wrap(
        namespace,
        "tls_signer_sign",
        |mut caller: Caller<'_, State>,
         handle: i64,
         transcript: i64,
         signature: i64|
         -> wasmtime::Result<i64> {
            if !caller.data().active {
                return Err(reject("invalid TLS signing capability"));
            }
            let (memory, from, length) = guest_range(&mut caller, transcript)?;
            let (_, _, capacity) = guest_range(&mut caller, signature)?;
            let message = (length == HASH_LEN && capacity >= DER_CAPACITY)
                .then(|| memory.data(&caller)[from..from + length].first_chunk::<HASH_LEN>())
                .flatten()
                .map(server_message);
            let SecretEntry::TlsSigner(key) = caller.data_mut().secrets.entry(handle)? else {
                return Err(reject("invalid TLS signing capability"));
            };
            let Some(message) = message else { return Ok(0) };
            let Ok(signed) = key.sign(&SystemRandom::new(), &message) else {
                return Ok(0);
            };
            let signed = signed.as_ref();
            if signed.is_empty() || signed.len() > DER_CAPACITY {
                return Ok(0);
            }
            write_guest(&mut caller, signature, signed)?;
            Ok(signed.len() as i64)
        },
    )?;
    Ok(())
}
