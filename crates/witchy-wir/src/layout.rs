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

/// Bytes in the tag/length header that starts every slot-backed aggregate:
/// records, tuples, lists, and enum payload blocks.
pub(crate) const SLOT_HEADER_SIZE: i32 = 4;

/// Bytes in one universal value slot. Scalars are stored as i64/f64-width
/// values, and pointers/bools are widened into the same slot at aggregate
/// boundaries.
pub(crate) const VALUE_SLOT_SIZE: i32 = 8;

/// (RFC-0023) Trailing redzone size, in bytes, reserved after each checked
/// allocation. The guest allocator reserves exactly this many bytes and the
/// host poisons/sweeps exactly this many bytes at `[end, end + HEAP_REDZONE)`.
pub const HEAP_REDZONE: usize = 8;

/// The alloc-size header word (`ptr-4`, written by `$rc_alloc`) holds the
/// allocated size in its low 24 bits; the high 8 bits are reserved for the
/// debug type tag.
pub(crate) const RC_SIZE_MASK: i32 = 0x00FF_FFFF;

/// Total byte size of a slot-backed aggregate with `slots` payload fields.
pub(crate) const fn slot_record_size(slots: usize) -> i32 {
    SLOT_HEADER_SIZE + VALUE_SLOT_SIZE * slots as i32
}

/// Byte offset of payload slot `index` inside a slot-backed aggregate.
pub(crate) const fn slot_offset(index: usize) -> i32 {
    SLOT_HEADER_SIZE + VALUE_SLOT_SIZE * index as i32
}

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

    #[test]
    fn slot_layout_vectors_are_stable() {
        assert_eq!(DATA_BASE, 8);
        assert_eq!(SLOT_HEADER_SIZE, 4);
        assert_eq!(VALUE_SLOT_SIZE, 8);
        assert_eq!(slot_record_size(0), 4);
        assert_eq!(slot_record_size(3), 28);
        assert_eq!(slot_offset(0), 4);
        assert_eq!(slot_offset(3), 28);
    }
}
