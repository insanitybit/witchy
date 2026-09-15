//! Independent transport, encoding, and status semantics. Only cryptographic
//! primitives are delegated; this oracle never calls the runtime registry.
use super::*;
use crate::canonical_safewir::{HostBufferExtent, CRYPTO_SHA512_IMPORT, CRYPTO_SHA3_256_IMPORT,
    CRYPTO_HMAC_SHA256_IMPORT, CRYPTO_SHAKE128_IMPORT, CRYPTO_SHAKE256_IMPORT,
    CRYPTO_ECDSA_P256_VERIFY_IMPORT, CRYPTO_ECDSA_P256_VERIFY_HEX_IMPORT};
use super::crypto::{rsa_hex, rsa_integer, rsa_tlv};
use sha2::{Digest, Sha512};
use tiny_keccak::{Hasher, Sha3, Shake};

fn text(bytes: &[u8]) -> Result<&str, InterpretError> {
    std::str::from_utf8(bytes).map_err(|_| InterpretError::TypeMismatch("crypto extension UTF-8"))
}
fn hex(bytes: &[u8]) -> Vec<u8> {
    let digits = b"0123456789abcdef";
    bytes.iter().flat_map(|byte| [digits[(byte >> 4) as usize], digits[(byte & 15) as usize]]).collect()
}
fn shake(name: &str, input: &[u8], length: i64) -> Result<Vec<u8>, InterpretError> {
    let length = usize::try_from(length).ok().filter(|length| *length <= 1_048_576)
        .ok_or(InterpretError::TypeMismatch("invalid SHAKE output length"))?;
    let mut output = vec![0; length];
    let mut state = if name == CRYPTO_SHAKE128_IMPORT { Shake::v128() } else { Shake::v256() };
    state.update(input);
    state.finalize(&mut output);
    Ok(output)
}

fn ecdsa_status(key: &str, message: &str, signature: &str, hex_message: bool) -> Result<i64, InterpretError> {
    let Some(key) = rsa_hex(key).filter(|bytes| bytes.len() == 65 && bytes[0] == 4) else { return Ok(-1); };
    let message = if hex_message {
        let Some(bytes) = rsa_hex(message) else { return Ok(-2); };
        bytes
    } else { message.as_bytes().to_vec() };
    let Some(signature) = rsa_hex(signature) else { return Ok(-3); };
    let Some((sequence, [])) = rsa_tlv(&signature, 0x30) else { return Ok(-3); };
    let Some((r, remaining)) = rsa_tlv(sequence, 2) else { return Ok(-3); };
    let Some((s, [])) = rsa_tlv(remaining, 2) else { return Ok(-3); };
    if !rsa_integer(r) || !rsa_integer(s) { return Ok(-3); }
    ecdsa_primitive(&key, &message, &signature).map(i64::from)
}

#[cfg(not(target_arch = "wasm32"))]
fn ecdsa_primitive(key: &[u8], message: &[u8], signature: &[u8]) -> Result<bool, InterpretError> {
    use aws_lc_rs::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
    Ok(UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key).verify(message, signature).is_ok())
}
#[cfg(target_arch = "wasm32")]
fn ecdsa_primitive(_: &[u8], _: &[u8], _: &[u8]) -> Result<bool, InterpretError> {
    Err(InterpretError::UnsupportedOperation("ECDSA P256 oracle primitive on wasm32"))
}
#[cfg(not(target_arch = "wasm32"))]
fn hmac(key: &[u8], message: &[u8]) -> Result<Vec<u8>, InterpretError> {
    use aws_lc_rs::hmac;
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    Ok(hex(hmac::sign(&key, message).as_ref()))
}
#[cfg(target_arch = "wasm32")]
fn hmac(_: &[u8], _: &[u8]) -> Result<Vec<u8>, InterpretError> {
    Err(InterpretError::UnsupportedOperation("HMAC SHA256 oracle primitive on wasm32"))
}

impl Engine<'_> {
    pub(super) fn crypto_extension_host_call(&mut self, name: &str, destinations: &[ValueId],
        arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError>
    {
        let [destination] = destinations else { return Err(InterpretError::TypeMismatch("crypto extension results")); };
        if matches!(name, CRYPTO_ECDSA_P256_VERIFY_IMPORT | CRYPTO_ECDSA_P256_VERIFY_HEX_IMPORT) {
            let [HostCallArgument::Buffer { place: key, extent: HostBufferExtent::Slice },
                HostCallArgument::Buffer { place: message, extent: HostBufferExtent::Slice },
                HostCallArgument::Buffer { place: signature, extent: HostBufferExtent::Slice }] = arguments
                else { return Err(InterpretError::TypeMismatch("ECDSA slice arguments")); };
            // Read all initialized slices before any text/hex/status decisions.
            let key = self.host_bytes(key, values)?;
            let message = self.host_bytes(message, values)?;
            let signature = self.host_bytes(signature, values)?;
            let (key, message, signature) = (text(&key)?, text(&message)?, text(&signature)?);
            let status = ecdsa_status(key, message, signature, name == CRYPTO_ECDSA_P256_VERIFY_HEX_IMPORT)?;
            values.insert(*destination, InterpreterValue::Int(status));
            return Ok(());
        }
        let slice = |argument: &HostCallArgument| match argument {
            HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice } => Ok(place.clone()),
            _ => Err(InterpretError::TypeMismatch("crypto extension slice argument")),
        };
        let (input, second, output) = match (name, arguments) {
            (CRYPTO_SHA512_IMPORT | CRYPTO_SHA3_256_IMPORT, [input, output]) => (input, None, output),
            (CRYPTO_HMAC_SHA256_IMPORT | CRYPTO_SHAKE128_IMPORT | CRYPTO_SHAKE256_IMPORT, [input, second, output]) => (input, Some(second), output),
            _ => return Err(InterpretError::TypeMismatch("crypto extension arguments")),
        };
        let input = self.host_bytes(&slice(input)?, values)?;
        let message = if name == CRYPTO_HMAC_SHA256_IMPORT {
            Some(self.host_bytes(&slice(second.expect("HMAC input"))?, values)?)
        } else { None };
        let (object, start, capacity) = self.slice_place(&slice(output)?, values)?;
        self.check_range(object, start, capacity, true)?;
        let encoded = match name {
            CRYPTO_SHA512_IMPORT => hex(&Sha512::digest(text(&input)?.as_bytes())),
            CRYPTO_SHA3_256_IMPORT => {
                let mut state = Sha3::v256();
                state.update(text(&input)?.as_bytes());
                let mut digest = [0; 32];
                state.finalize(&mut digest);
                hex(&digest)
            }
            CRYPTO_HMAC_SHA256_IMPORT => {
                let key = text(&input)?;
                let message = text(message.as_ref().expect("HMAC message"))?;
                let key = rsa_hex(key).ok_or(InterpretError::TypeMismatch("crypto.hmac_sha256: key is not valid hex"))?;
                hmac(&key, message.as_bytes())?
            }
            _ => {
                let Some(HostCallArgument::Value(length)) = second else { return Err(InterpretError::TypeMismatch("SHAKE Int length")); };
                shake(name, &input, self.int_value(*length, values)?)?
            }
        };
        let required = encoded.len() as u64;
        if required <= capacity {
            for (offset, byte) in encoded.into_iter().enumerate() {
                let index = start.checked_add(offset as u64).ok_or(InterpretError::ArithmeticTrap)?;
                self.object_mut(object)?.cells.insert(CellKey::Element(index), InterpreterValue::Size(u64::from(byte)));
            }
        }
        values.insert(*destination, InterpreterValue::Size(required));
        Ok(())
    }
}

#[cfg(test)]
#[path = "crypto_extensions_tests.rs"]
mod tests;
