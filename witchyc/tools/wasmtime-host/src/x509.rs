//! Wasmtime adapter for the shared certificate verification policy.

#[path = "../../../src/compiler/pkix.rs"]
mod pkix;

use wasmtime::{Caller, Linker};

use super::{State, guest_range, reject};
use pkix::verify;

pub(super) fn register(linker: &mut Linker<State>, namespace: &str) -> wasmtime::Result<()> {
    linker.func_wrap(namespace, "crypto_x509_p384_verify",
        |mut caller: Caller<'_, State>, roots: i64, chain: i64, host: i64, signed: i64, sig: i64| -> wasmtime::Result<i64> {
            if !caller.data().active { return Err(reject("crypto outside the program")); }
            let (memory, r, rl) = guest_range(&mut caller, roots)?;
            let (_, c, cl) = guest_range(&mut caller, chain)?;
            let (_, h, hl) = guest_range(&mut caller, host)?;
            let (_, s, sl) = guest_range(&mut caller, signed)?;
            let (_, b, bl) = guest_range(&mut caller, sig)?;
            if super::sha::provider().is_err() { return Ok(0); }
            let data = memory.data(&caller);
            Ok(i64::from(verify(&data[r..r + rl], &data[c..c + cl], &data[h..h + hl], &data[s..s + sl], &data[b..b + bl])))
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::verify;

    #[test]
    fn trust_anchor_identity_and_transcript_are_all_required() {
        super::super::sha::provider().unwrap();
        let root = include_bytes!("../../../tests/fixtures/crypto/pkix-kat/root.bundle");
        let leaf = include_bytes!("../../../tests/fixtures/crypto/pkix-kat/leaf.bundle");
        let signed = include_bytes!("../../../tests/fixtures/crypto/pkix-kat/signed.bin");
        let signature = include_bytes!("../../../tests/fixtures/crypto/pkix-kat/signature.der");
        assert!(verify(root, leaf, b"api.example.test", signed, signature));
        for bad_name in [b"other.example.test".as_slice(), b".example.test", b"api..example.test",
            b"api.example.test.", b"*.example.test", b"-api.example.test"] {
            assert!(!verify(root, leaf, bad_name, signed, signature));
        }
        assert!(!verify(&[], leaf, b"api.example.test", signed, signature));
        let mut trailing = signature.to_vec();
        trailing.push(0);
        assert!(!verify(root, leaf, b"api.example.test", signed, &trailing));
        let mut changed = *signed;
        changed[8] ^= 1;
        assert!(!verify(root, leaf, b"api.example.test", &changed, signature));
        changed = *signed;
        changed[..8].fill(0);
        assert!(!verify(root, leaf, b"api.example.test", &changed, signature));
    }
}
