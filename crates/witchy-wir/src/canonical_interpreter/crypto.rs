//! Independent crypto input/status semantics; no runtime registry or header ABI.
use super::*;

impl Engine<'_> {
    pub(super) fn rsa_verify_host_call(&mut self, destinations: &[ValueId], arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        use crate::canonical_safewir::HostBufferExtent;
        let ([destination], [
            HostCallArgument::Buffer { place: key, extent: HostBufferExtent::Slice },
            HostCallArgument::Buffer { place: message, extent: HostBufferExtent::Slice },
            HostCallArgument::Buffer { place: signature, extent: HostBufferExtent::Slice },
        ]) = (destinations, arguments) else {
            return Err(InterpretError::TypeMismatch("RSA slice arguments"));
        };
        // Transport is checked completely before interpreting key/signature
        // syntax. A malformed slice or UTF-8 string is not a verifier status.
        let key = self.host_string(key, values)?;
        let message = self.host_string(message, values)?;
        let signature = self.host_string(signature, values)?;
        let status = rsa_verify_status(&key, &message, &signature)?;
        values.insert(*destination, InterpreterValue::Int(status));
        Ok(())
    }

    pub(super) fn ed25519_verify_host_call(&mut self, destinations: &[ValueId], arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        let ([destination], [HostCallArgument::Buffer { place: key, .. }, HostCallArgument::Buffer { place: message, .. }, HostCallArgument::Buffer { place: signature, .. }]) = (destinations, arguments) else {
            return Err(InterpretError::TypeMismatch("Ed25519 slice arguments"));
        };
        // Validate all three bounded UTF-8 strings before assigning a verifier
        // status. Bad memory/UTF-8 is not a well-formed false signature.
        let key = self.host_string(key, values)?;
        let message = self.host_string(message, values)?;
        let signature = self.host_string(signature, values)?;
        values.insert(*destination, InterpreterValue::Int(verify_status(&key, &message, &signature)));
        Ok(())
    }
}

fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != N * 2 { return None; }
    let nibble = |byte| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    };
    let mut decoded = [0; N];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        // Preserve existing native two-character radix parsing, including its
        // leading-plus spelling. Do not silently tighten accepted source input.
        let high = if pair[0] == b'+' { 0 } else { nibble(pair[0])? };
        decoded[index] = high * 16 + nibble(pair[1])?;
    }
    Some(decoded)
}

fn verify_status(key: &str, message: &str, signature: &str) -> i64 {
    let Some(key) = decode_hex::<32>(key) else { return -1 };
    let Some(signature) = decode_hex::<64>(signature) else { return -3 };
    if verify_primitive(&key, message.as_bytes(), &signature) { 1 } else { 0 }
}

// RSA hex is variable-length. Decode ASCII pairs independently of the native
// parser, retaining its accepted leading-plus pair spelling ("+f" -> 15).
pub(super) fn rsa_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) { return None; }
    let digit = |byte| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    };
    text.as_bytes().chunks_exact(2).map(|pair| {
        let high = if pair[0] == b'+' { 0 } else { digit(pair[0])? };
        Some(high * 16 + digit(pair[1])?)
    }).collect()
}

// Consume one definite-length TLV using shrinking slices rather than the
// native parser's mutable cursor. Preserve its exact length acceptance: up to
// four length octets, long form >=128, including redundant leading zero octets.
pub(super) fn rsa_tlv(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (&actual, input) = input.split_first()?;
    if actual != tag { return None; }
    let (&first, input) = input.split_first()?;
    let (length, body) = if first < 128 {
        (usize::from(first), input)
    } else {
        let octets = usize::from(first & 127);
        if !(1..=4).contains(&octets) { return None; }
        let encoded = input.get(..octets)?;
        let length = encoded.iter().try_fold(0usize, |length, byte| {
            length.checked_mul(256)?.checked_add(usize::from(*byte))
        })?;
        if length < 128 { return None; }
        (length, &input[octets..])
    };
    Some((body.get(..length)?, body.get(length..) ?))
}

pub(super) fn rsa_integer(bytes: &[u8]) -> bool {
    match bytes {
        [] => false,
        [first, rest @ ..] => first & 128 == 0
            && !(*first == 0 && rest.first().is_some_and(|next| next & 128 == 0)),
    }
}

fn rsa_key_encoding(key: &[u8]) -> bool {
    let Some((sequence, [])) = rsa_tlv(key, 0x30) else { return false; };
    let Some((modulus, rest)) = rsa_tlv(sequence, 0x02) else { return false; };
    let Some((exponent, [])) = rsa_tlv(rest, 0x02) else { return false; };
    if !rsa_integer(modulus) || !rsa_integer(exponent) || exponent.len() > 8 {
        return false;
    }
    let modulus = if modulus[0] == 0 { &modulus[1..] } else { modulus };
    if !(256..=1024).contains(&modulus.len()) { return false; }
    let mut encoded = [0u8; 8];
    encoded[8 - exponent.len()..].copy_from_slice(exponent);
    let exponent = u64::from_be_bytes(encoded);
    exponent > 1 && exponent % 2 == 1
}

fn rsa_verify_status(key: &str, message: &str, signature: &str) -> Result<i64, InterpretError> {
    let Some(key) = rsa_hex(key).filter(|key| rsa_key_encoding(key)) else { return Ok(-1); };
    let Some(signature) = rsa_hex(signature).filter(|signature| !signature.is_empty()) else { return Ok(-3); };
    rsa_verify_primitive(&key, message.as_bytes(), &signature).map(i64::from)
}

#[cfg(not(target_arch = "wasm32"))]
fn rsa_verify_primitive(key: &[u8], message: &[u8], signature: &[u8]) -> Result<bool, InterpretError> {
    use aws_lc_rs::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
    Ok(UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key).verify(message, signature).is_ok())
}

#[cfg(target_arch = "wasm32")]
fn rsa_verify_primitive(_key: &[u8], _message: &[u8], _signature: &[u8]) -> Result<bool, InterpretError> {
    Err(InterpretError::UnsupportedOperation("RSA PKCS1 SHA256 oracle primitive on wasm32"))
}

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
#[path = "crypto_rsa_tests.rs"]
pub mod rsa_self_checks;

#[cfg(not(target_arch = "wasm32"))]
fn verify_primitive(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    use aws_lc_rs::signature::{UnparsedPublicKey, ED25519};
    UnparsedPublicKey::new(&ED25519, key).verify(message, signature).is_ok()
}

#[cfg(target_arch = "wasm32")]
fn verify_primitive(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    VerifyingKey::from_bytes(key).is_ok_and(|key| key.verify(message, &Signature::from_bytes(signature)).is_ok())
}
