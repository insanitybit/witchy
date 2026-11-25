//! The TLS 1.3 AEAD provider boundary. Record parsing and sequence state live
//! in the caller. Keys, nonces, and AAD are supplied explicitly.

use aws_lc_rs::aead::{
    AES_128_GCM, AES_256_GCM, Aad, Nonce, TlsProtocolId, TlsRecordOpeningKey, TlsRecordSealingKey,
};
use wasmtime::{Caller, Linker};
use zeroize::Zeroizing;

use super::{State, guest_range, reject, write_guest};

const TAG_LEN: usize = 16;
const MAX_CIPHERTEXT: usize = (1 << 14) + 256;

pub(super) fn register(linker: &mut Linker<State>, namespace: &str) -> wasmtime::Result<()> {
    for (name, algorithm, seal) in [
        ("crypto_tls13_aes128gcm_seal", &AES_128_GCM, true),
        ("crypto_tls13_aes128gcm_open", &AES_128_GCM, false),
        ("crypto_tls13_aes256gcm_seal", &AES_256_GCM, true),
        ("crypto_tls13_aes256gcm_open", &AES_256_GCM, false),
    ] {
        linker.func_wrap(
            namespace,
            name,
            move |mut caller: Caller<'_, State>,
                  key: i64,
                  nonce: i64,
                  aad: i64,
                  input: i64,
                  output: i64|
                  -> wasmtime::Result<i64> {
                if !caller.data().active {
                    return Err(reject("crypto outside the program"));
                }
                let (memory, key_at, key_len) = guest_range(&mut caller, key)?;
                let (_, nonce_at, nonce_len) = guest_range(&mut caller, nonce)?;
                let (_, aad_at, aad_len) = guest_range(&mut caller, aad)?;
                let (_, input_at, input_len) = guest_range(&mut caller, input)?;
                let (_, _, output_len) = guest_range(&mut caller, output)?;
                if input_len > MAX_CIPHERTEXT || output_len > MAX_CIPHERTEXT {
                    return Ok(3);
                }
                let correct_size = if seal {
                    input_len + TAG_LEN == output_len
                } else {
                    output_len + TAG_LEN == input_len
                };
                if key_len != algorithm.key_len()
                    || nonce_len != 12
                    || aad_len != 5
                    || !correct_size
                {
                    return Ok(2);
                }
                if super::sha::provider().is_err() {
                    return Ok(4);
                }

                let data = memory.data(&caller);
                let key_bytes = &data[key_at..key_at + key_len];
                let Ok(nonce) =
                    Nonce::try_assume_unique_for_key(&data[nonce_at..nonce_at + nonce_len])
                else {
                    return Ok(2);
                };
                let aad = Aad::from(&data[aad_at..aad_at + aad_len]);
                // The host must not let failed authentication change guest memory,
                // including when presented with aliasing ranges by a hostile module.
                let mut buffer = Zeroizing::new(Vec::with_capacity(input_len.max(output_len)));
                buffer.extend_from_slice(&data[input_at..input_at + input_len]);
                let result = if seal {
                    let Ok(mut key) =
                        TlsRecordSealingKey::new(algorithm, TlsProtocolId::TLS13, key_bytes)
                    else {
                        return Ok(4);
                    };
                    key.seal_in_place_append_tag(nonce, aad, &mut *buffer)
                } else {
                    let Ok(key) =
                        TlsRecordOpeningKey::new(algorithm, TlsProtocolId::TLS13, key_bytes)
                    else {
                        return Ok(4);
                    };
                    key.open_in_place(nonce, aad, &mut buffer).map(|_| ())
                };
                if result.is_err() {
                    return Ok(4);
                }
                write_guest(&mut caller, output, &buffer[..output_len])?;
                Ok(0)
            },
        )?;
    }
    Ok(())
}
