//! Independent crypto input/status semantics; no runtime registry or header ABI.
use super::*;

impl Engine<'_> {
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

#[cfg(test)]
mod tests {
    use super::*;
    // RFC 8032 section 7.1, TEST 1. Data only, not another backend's answer.
    const KEY: &str = concat!("d75a980182b10ab7d54bfed3c964073a", "0ee172f3daa62325af021a68f707511a");
    const SIGNATURE: &str = concat!("e5564300c360ac729086e2cc806e828a", "84877f1eb8e5d974d873e06522490155", "5fb8821590a33bacc61e39701cf9b46b", "d25bf5f0595bbe24655141438e7a100b");

    #[test]
    fn crypto_ed25519_oracle_known_answer_and_input_statuses() {
        assert_eq!(verify_status(KEY, "", SIGNATURE), 1);
        assert_eq!(verify_status(&KEY.to_uppercase(), "", &SIGNATURE.to_uppercase()), 1);
        assert_eq!(verify_status(KEY, "tampered", SIGNATURE), 0);
        assert_eq!(verify_status("not hex", "", "also bad"), -1);
        assert_eq!(verify_status(&KEY[..62], "", SIGNATURE), -1);
        assert_eq!(verify_status(KEY, "", "not hex"), -3);
        assert_eq!(verify_status(KEY, "", &SIGNATURE[..126]), -3);
        assert_eq!(verify_status(&"00".repeat(32), "", SIGNATURE), 0);
        assert_eq!(verify_status(&"+0".repeat(32), "", SIGNATURE), 0);
    }

    #[test]
    fn crypto_ed25519_oracle_preserves_native_hex_byte_spelling() {
        assert_eq!(decode_hex::<3>("00aAFF"), Some([0, 170, 255]));
        assert_eq!(decode_hex::<1>("+f"), Some([15]));
        for malformed in ["", "f", "fff", " f", "f ", "-0", "gg", "\u{00e9}"] {
            assert_eq!(decode_hex::<1>(malformed), None, "{malformed:?}");
        }
    }
}
