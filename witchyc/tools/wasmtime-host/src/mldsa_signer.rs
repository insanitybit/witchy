//! Descriptor-backed signing for application domains and TUF targets.

use aws_lc_rs::signature::{KeyPair, PqdsaKeyPair};
use wasmtime::{Caller, Linker};

use super::{SecretEntry, State, guest_range, reject, sha};

pub(super) const TUF_USE: &str = "sign/tuf/targets-ml-dsa-87";
const TARGETS_PREFIX: &[u8] = b"{\"_type\":\"targets\",";
const APPLICATION_USE: &str = "sign/ml-dsa-87/";

pub(super) fn use_domain(use_: &str) -> Option<String> {
    if use_ == TUF_USE { return Some(String::new()); }
    let encoded = use_.strip_prefix(APPLICATION_USE)?;
    if encoded.contains(' ') || encoded.len() > 64 * 3 { return None; }
    let domain = encoded.replace("%20", " ");
    (domain.len() <= 64 && domain.split(' ').all(super::plain_role)).then_some(domain)
}

pub(super) struct Signer {
    key: PqdsaKeyPair,
    domain: Vec<u8>,
}

impl Signer {
    pub(super) fn new(key: PqdsaKeyPair, role: &str) -> Self {
        let domain = if role.is_empty() { TARGETS_PREFIX.to_vec() }
            else { [role.as_bytes(), b"\0"].concat() };
        Self { key, domain }
    }

    fn sign(&self, message: &[u8]) -> Result<[u8; sha::MLDSA87_SIGNATURE], i64> {
        let tuf = self.domain == TARGETS_PREFIX;
        if message.len() > if tuf { 2 * super::MAX_SIGNED } else { super::MAX_SIGNED } { return Err(3); }
        if !message.starts_with(&self.domain) { return Err(1); }
        sha::provider().map_err(|_| 4i64)?;
        // TAP 21 version 1 signs a tagged SHA-512 digest of the targets bytes.
        let mut payload = [0; 68];
        let message = if tuf {
            payload[..4].copy_from_slice(b"tuf\x01");
            payload[4..].copy_from_slice(&sha::sha512(message));
            &payload
        } else { message };
        let mut signature = [0; sha::MLDSA87_SIGNATURE];
        match self.key.sign(message, &mut signature) {
            Ok(sha::MLDSA87_SIGNATURE) => Ok(signature),
            _ => Err(4),
        }
    }
}

pub(super) fn register(linker: &mut Linker<State>, namespace: &str) -> wasmtime::Result<()> {
    linker.func_wrap(namespace, "mldsa87_signer_public_key", |mut caller: Caller<'_, State>, handle: i64, output: i64| -> wasmtime::Result<i64> {
        if !caller.data().active { return Err(reject("invalid secret capability")); }
        let (memory, at, capacity) = guest_range(&mut caller, output)?;
        let (data, state) = memory.data_and_store_mut(&mut caller);
        let SecretEntry::ApplicationSigner(signer) = state.secrets.entry(handle)?
            else { return Err(reject("invalid secret capability")); };
        if capacity != sha::MLDSA87_PUBLIC_KEY { return Ok(2); }
        data[at..at + capacity].copy_from_slice(signer.key.public_key().as_ref());
        Ok(0)
    })?;
    linker.func_wrap(namespace, "mldsa87_signer_sign", |mut caller: Caller<'_, State>, handle: i64, message: i64, output: i64| -> wasmtime::Result<i64> {
        if !caller.data().active { return Err(reject("invalid secret capability")); }
        let (memory, at, length) = guest_range(&mut caller, message)?;
        let (_, out, capacity) = guest_range(&mut caller, output)?;
        let (data, state) = memory.data_and_store_mut(&mut caller);
        let SecretEntry::ApplicationSigner(signer) = state.secrets.entry(handle)?
            else { return Err(reject("invalid secret capability")); };
        if capacity != sha::MLDSA87_SIGNATURE { return Ok(2); }
        match signer.sign(&data[at..at + length]) {
            Ok(signature) => { data[out..out + capacity].copy_from_slice(&signature); Ok(0) }
            Err(code) => Ok(code),
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_and_tuf_signatures_use_distinct_transcripts() {
        sha::provider().unwrap();
        let make_key = || sha::mldsa87_from_seed(&[42; 32]).unwrap();
        let domain = use_domain("sign/ml-dsa-87/example%20application").unwrap();
        let application = Signer::new(make_key(), &domain);
        let message = b"example application\0request";
        let signature = application.sign(message).unwrap();
        let public = application.key.public_key();
        assert!(sha::mldsa87_verify(public.as_ref(), message, &signature));
        assert_eq!(application.sign(b"other application\0request"), Err(1));
        assert_eq!(application.sign(b"example application request"), Err(1));
        assert!(use_domain("sign/ml-dsa-87/").is_none());
        assert!(use_domain("sign/ml-dsa-87/example%20%20application").is_none());

        let tuf = Signer::new(make_key(), &use_domain(TUF_USE).unwrap());
        assert_eq!(tuf.sign(message), Err(1));
        assert_eq!(tuf.sign(br#"{"_type":"root","version":1}"#), Err(1));
        let targets = br#"{"_type":"targets","version":1}"#;
        let signature = tuf.sign(targets).unwrap();
        assert!(!sha::mldsa87_verify(public.as_ref(), targets, &signature));
        let payload = [b"tuf\x01".as_slice(), &sha::sha512(targets)].concat();
        assert!(sha::mldsa87_verify(public.as_ref(), &payload, &signature));
    }
}
