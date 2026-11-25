//! Ephemeral P-384 ECDH using the provider's DRBG and validation.

use aws_lc_rs::agreement::{
    ECDH_P384, EphemeralPrivateKey, ParsedPublicKey, UnparsedPublicKey, agree_ephemeral,
};
use aws_lc_rs::rand::SystemRandom;
use wasmtime::{Caller, Linker};

use super::{State, guest_range, reject};

const POINT_LEN: usize = 97;
const SECRET_LEN: usize = 48;

pub(super) fn register(linker: &mut Linker<State>, namespace: &str) -> wasmtime::Result<()> {
    linker.func_wrap(
        namespace,
        "rand_p384_agree",
        |mut caller: Caller<'_, State>,
         handle: i64,
         peer: i64,
         public: i64,
         secret: i64|
         -> wasmtime::Result<i64> {
            if !caller.data().active || handle != 5 {
                return Err(reject("invalid randomness capability"));
            }

            let (memory, peer_at, peer_len) = guest_range(&mut caller, peer)?;
            let (_, public_at, public_len) = guest_range(&mut caller, public)?;
            let (_, secret_at, secret_len) = guest_range(&mut caller, secret)?;
            if peer_len != POINT_LEN || public_len != POINT_LEN || secret_len != SECRET_LEN {
                return Ok(2);
            }
            let public_end = public_at + POINT_LEN;
            let secret_end = secret_at + SECRET_LEN;
            if public_at < secret_end && secret_at < public_end {
                return Err(reject("overlapping agreement outputs"));
            }
            let peer_bytes = &memory.data(&caller)[peer_at..peer_at + POINT_LEN];
            if peer_bytes[0] != 4 {
                return Ok(1);
            }
            if super::sha::provider().is_err() {
                return Ok(4);
            }
            let peer = match parse_peer(peer_bytes) {
                Ok(peer) => peer,
                Err(code) => return Ok(code),
            };
            let Ok(private) = EphemeralPrivateKey::generate(&ECDH_P384, &SystemRandom::new())
            else {
                return Ok(4);
            };
            let Ok(public) = private.compute_public_key() else {
                return Ok(4);
            };
            let public = public.as_ref();
            if public.len() != POINT_LEN || public[0] != 4 {
                return Ok(4);
            }
            Ok(agree_ephemeral(private, peer, 4, |shared| {
                if shared.len() != SECRET_LEN {
                    return Err(4);
                }
                // Publish both outputs only after peer validation and agreement.
                let data = memory.data_mut(&mut caller);
                data[public_at..public_end].copy_from_slice(public);
                data[secret_at..secret_end].copy_from_slice(shared);
                Ok(0)
            })
            .unwrap_or_else(|code| code))
        },
    )?;
    register_pending(linker, namespace)
}

/// Provider keys held between ClientHello and ServerHello. IDs are never
/// recycled within an instance. Abandoning the instance drops all its keys.
#[derive(Default)]
pub(super) struct Pending {
    next: i64,
    keys: std::collections::BTreeMap<i64, EphemeralPrivateKey>,
}

const MAX_PENDING: usize = 16;
const FIRST_KEY: i64 = 256;

impl Pending {
    fn start(&mut self) -> i64 {
        if self.keys.len() == MAX_PENDING || super::sha::provider().is_err() {
            return 0;
        }
        let Some(handle) = FIRST_KEY.checked_add(self.next) else { return 0 };
        let Ok(key) = EphemeralPrivateKey::generate(&ECDH_P384, &SystemRandom::new()) else {
            return 0;
        };
        self.next += 1;
        self.keys.insert(handle, key);
        handle
    }

    fn public(&self, handle: i64) -> Result<[u8; POINT_LEN], i64> {
        let key = self.keys.get(&handle).ok_or(4)?;
        let point = key.compute_public_key().map_err(|_| 4)?;
        let bytes: [u8; POINT_LEN] = point.as_ref().try_into().map_err(|_| 4)?;
        if bytes[0] != 4 { return Err(4); }
        Ok(bytes)
    }

    fn take(&mut self, handle: i64) -> wasmtime::Result<EphemeralPrivateKey> {
        self.keys.remove(&handle).ok_or_else(|| reject("invalid agreement capability"))
    }
}

fn parse_peer(point: &[u8]) -> Result<ParsedPublicKey, i64> {
    if point.len() != POINT_LEN || point[0] != 4 {
        return Err(1);
    }
    ParsedPublicKey::try_from(UnparsedPublicKey::new(&ECDH_P384, point)).map_err(|_| 1)
}

fn register_pending(linker: &mut Linker<State>, namespace: &str) -> wasmtime::Result<()> {
    linker.func_wrap(namespace, "rand_p384_start", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
        if !caller.data().active || handle != 5 {
            return Err(reject("invalid randomness capability"));
        }
        Ok(caller.data_mut().agreements.start())
    })?;
    linker.func_wrap(namespace, "rand_p384_public", |mut caller: Caller<'_, State>, handle: i64, output: i64| -> wasmtime::Result<i64> {
        if !caller.data().active { return Err(reject("invalid agreement capability")); }
        let (memory, at, len) = guest_range(&mut caller, output)?;
        if len != POINT_LEN { return Ok(2); }
        let point = match caller.data().agreements.public(handle) {
            Ok(point) => point,
            Err(code) => return Ok(code),
        };
        memory.data_mut(&mut caller)[at..at + len].copy_from_slice(&point);
        Ok(0)
    })?;
    linker.func_wrap(namespace, "rand_p384_finish", |mut caller: Caller<'_, State>, handle: i64, peer: i64, shared: i64| -> wasmtime::Result<i64> {
        if !caller.data().active { return Err(reject("invalid agreement capability")); }
        let private = caller.data_mut().agreements.take(handle)?;
        let (memory, peer_at, peer_len) = guest_range(&mut caller, peer)?;
        let (_, shared_at, shared_len) = guest_range(&mut caller, shared)?;
        if peer_len != POINT_LEN || shared_len != SECRET_LEN { return Ok(2); }
        let peer_bytes = &memory.data(&caller)[peer_at..peer_at + peer_len];
        let peer = match parse_peer(peer_bytes) {
            Ok(peer) => peer,
            Err(code) => return Ok(code),
        };
        Ok(agree_ephemeral(private, peer, 4, |secret| {
            if secret.len() != SECRET_LEN { return Err(4); }
            memory.data_mut(&mut caller)[shared_at..shared_at + shared_len].copy_from_slice(secret);
            Ok(0)
        }).unwrap_or_else(|code| code))
    })?;
    linker.func_wrap(namespace, "rand_p384_close", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
        if !caller.data().active { return Err(reject("invalid agreement capability")); }
        // A start that failed returns zero and holds no key.
        if handle != 0 { caller.data_mut().agreements.take(handle)?; }
        Ok(0)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfinished_keys_are_bounded_and_consumed_handles_are_stale() {
        let mut pending = Pending::default();
        let first = pending.start();
        let second = pending.start();
        assert!(first >= FIRST_KEY && second > first);
        let first_share = pending.public(first).unwrap();
        let second_share = pending.public(second).unwrap();
        assert_ne!(first_share, second_share);
        let one = pending.take(first).unwrap();
        let two = pending.take(second).unwrap();
        assert!(pending.take(first).is_err());
        assert!(pending.public(second).is_err());
        let a = agree_ephemeral(one, parse_peer(&second_share).unwrap(), 4, |shared| Ok(shared.to_vec())).unwrap();
        let b = agree_ephemeral(two, parse_peer(&first_share).unwrap(), 4, |shared| Ok(shared.to_vec())).unwrap();
        assert_eq!(a.len(), SECRET_LEN);
        assert_eq!(a, b);

        for _ in 0..MAX_PENDING {
            assert!(pending.start() > second);
        }
        assert_eq!(pending.start(), 0);
        let release = *pending.keys.keys().next().unwrap();
        pending.take(release).unwrap();
        assert!(pending.start() > release);
        assert!(pending.take(release).is_err());
        let mut off_curve = [0; POINT_LEN];
        off_curve[0] = 4;
        assert!(parse_peer(&off_curve).is_err());
    }
}
