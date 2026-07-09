//! Shared compiled-backend layout facts.
//!
//! These constants are part of the ABI between WIR helper generation, lowering,
//! and the wasmtime host runtime. Keep one home here so checked-heap and
//! sanitizer instrumentation cannot silently drift across crates.

/// (RFC-0023) Trailing redzone size, in bytes, reserved after each checked
/// allocation. The guest allocator reserves exactly this many bytes and the
/// host poisons/sweeps exactly this many bytes at `[end, end + HEAP_REDZONE)`.
pub const HEAP_REDZONE: usize = 8;

/// The alloc-size header word (`ptr-4`, written by `$rc_alloc`) holds the
/// allocated size in its low 24 bits; the high 8 bits are reserved for the
/// debug type tag.
pub const RC_SIZE_MASK: i32 = 0x00FF_FFFF;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_tag_vectors_are_stable() {
        assert_eq!(type_tag_of("Point"), 181);
        assert_eq!(type_tag_of("packed:Point"), 118);
        assert_eq!(type_tag_of("main.Point"), 40);
        assert_eq!(type_tag_of("Option"), 77);
        assert_eq!(type_tag_of("Result"), 208);
        assert_eq!(type_tag_of(""), 2);
    }
}
