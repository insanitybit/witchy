//! Shared compiled-backend layout facts.
//!
//! These constants are part of the ABI between WIR helper generation, lowering,
//! and the wasmtime host runtime. Keep one home here so checked-heap and
//! sanitizer instrumentation cannot silently drift across crates.

mod specialized;
mod transport;

pub use specialized::*;
pub use transport::*;

/// First guest-data byte. The compiled backend leaves the low address range
/// reserved for null/sentinel values and starts static data here.
pub const DATA_BASE: u32 = 8;

/// (RFC-0023) Trailing redzone size, in bytes, reserved after each checked
/// allocation. The guest allocator reserves exactly this many bytes and the
/// host poisons/sweeps exactly this many bytes at `[end, end + HEAP_REDZONE)`.
pub const HEAP_REDZONE: usize = 8;

const FNV1A_OFFSET: u32 = 2_166_136_261;
const FNV1A_PRIME: u32 = 16_777_619;

/// (RFC-0037 §3) A stable, stateless 8-bit type id for the type-confusion
/// sanitizer. The same type name always maps to the same non-zero tag; 0 means
/// "untagged". Collisions only miss a confusion, never false-trap.
pub fn type_tag_of(name: &str) -> u8 {
    let mut h: u32 = FNV1A_OFFSET;
    for byte in name.bytes() {
        h ^= byte as u32;
        h = h.wrapping_mul(FNV1A_PRIME);
    }
    (h % 255) as u8 + 1
}
