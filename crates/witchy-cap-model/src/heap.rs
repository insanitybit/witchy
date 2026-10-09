//! The program's heap mode (RFC-0179 R4), declared by `@large_heap` on the
//! entry `main`.
//!
//! A standard program stores 32-bit byte addresses and its heap stops at
//! 4 GiB. A large program stores 32-bit references to 8-byte units and its heap
//! reaches 32 GiB. The mode is part of the launch contract, so every consumer
//! (encoder, admission planner, runtime engine, run limits, `witchy caps`)
//! reads one decision. Until the encoder selects the scaled references for
//! large programs, they keep byte references and trap at 4 GiB like any other.

/// Wasm pages are 64 KiB.
const PAGE_BYTES: u64 = 64 << 10;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum HeapMode {
    #[default]
    Standard,
    Large,
}

impl HeapMode {
    /// The mode `main`'s attributes declare.
    pub fn from_main_attributes<'a>(attributes: impl IntoIterator<Item = &'a str>) -> Self {
        if attributes.into_iter().any(|attribute| attribute == "large_heap") { Self::Large } else { Self::Standard }
    }

    /// The most pages the program's encoding can address.
    pub fn max_pages(self) -> u64 {
        match self {
            Self::Standard => 65_536,
            Self::Large => 524_288,
        }
    }

    /// `max_pages` in bytes.
    pub fn max_bytes(self) -> u64 {
        self.max_pages() * PAGE_BYTES
    }

    /// The launch contract version that carries this mode. A standard
    /// program's contract stays at version 1, byte for byte. A large program's
    /// is version 2: the same capability entries followed by one byte, `1`.
    pub fn launch_version(self) -> u8 {
        match self {
            Self::Standard => 1,
            Self::Large => 2,
        }
    }

    /// The trailing byte a launch contract of `launch_version` ends with, if any.
    pub fn launch_trailer(self) -> Option<u8> {
        (self == Self::Large).then_some(1)
    }

    /// The mode a launch contract payload declares. Only a well-formed
    /// version-2 capability contract ending in `1` is large; anything else,
    /// including compiler-session payloads, is standard.
    pub fn from_launch_contract(payload: &[u8]) -> Self {
        match payload {
            [2, count, .., 1] if payload.len() == 3 + usize::from(*count) * 2 => Self::Large,
            _ => Self::Standard,
        }
    }
}
