//! Opaque linear token descriptor sharing the concrete semantic type identity.
use super::ir::Ty;

#[derive(Debug)]
pub(super) struct Buffer {
    pub element: Ty,
    pub element_name: String,
    pub stride: u64,
    pub alignment: u64,
}
