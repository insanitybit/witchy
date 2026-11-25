//! An initialized scalar leaf, derived only by traversal of the common layout.
use super::ir::Ty;

#[derive(Clone, Copy, Debug)]
pub(super) struct Cell {
    pub slot: u32,
    pub ty: Ty,
    pub byte_offset: u64,
}
