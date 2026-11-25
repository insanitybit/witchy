//! Concrete logical alternatives with a private physical register representation.
use super::ir::Ty;

#[derive(Clone, Debug)]
pub(super) struct Sum {
    /// The type as written, for the layout receipt.
    pub name: String,
    pub arms: Vec<Ty>,
    /// Variant names, in tag order.
    pub names: Vec<String>,
    pub starts: Vec<u32>,
    pub counts: Vec<u32>,
    pub carriers: Vec<Ty>,
    /// Where each carrier sits when the value is stored in memory, and the stored
    /// size and alignment. `None` when a variant holds a reference: such a sum
    /// lives only in locals.
    pub stored: Option<(Vec<u64>, u64, u64)>,
    /// Where each variant's value starts in the stored form. The bytes from there are
    /// laid out as that value's own type, so the address is a reference to it.
    pub arm_offsets: Vec<u64>,
    /// Variant represented by zero when the other arm is one nonzero owner.
    pub zero_arm: Option<u8>,
}
pub(super) fn reserved(name: &str) -> bool { matches!(name, "Result" | "Ok" | "Err" | "discard" | "Buffer" | "PrefixBuffer" | "Boxed" | "Weak" | "Allocator") }
