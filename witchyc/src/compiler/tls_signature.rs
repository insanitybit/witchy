//! The TLS 1.3 server CertificateVerify message signed by both runtimes.

pub(crate) const HASH_LEN: usize = 48;
pub(crate) const DER_CAPACITY: usize = 104;
const CONTEXT: &[u8] = b"TLS 1.3, server CertificateVerify";
const MESSAGE_LEN: usize = 64 + CONTEXT.len() + 1 + HASH_LEN;

pub(crate) fn server_message(transcript: &[u8; HASH_LEN]) -> [u8; MESSAGE_LEN] {
    let mut message = [0; MESSAGE_LEN];
    message[..64].fill(32);
    message[64..64 + CONTEXT.len()].copy_from_slice(CONTEXT);
    message[65 + CONTEXT.len()..].copy_from_slice(transcript);
    message
}
