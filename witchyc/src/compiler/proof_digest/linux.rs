use sha2::{Digest, Sha256, Sha384};
use std::io;

pub(super) struct Hash(Sha256);

impl Hash {
    pub(super) fn new() -> io::Result<Self> {
        Ok(Self(Sha256::new()))
    }

    pub(super) fn update(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.update(bytes);
        Ok(())
    }

    pub(super) fn finish(self) -> io::Result<String> {
        Ok(super::hex(&self.0.finalize()))
    }
}

pub(super) fn sha384(bytes: &[u8]) -> io::Result<[u8; 48]> {
    Ok(Sha384::digest(bytes).into())
}
