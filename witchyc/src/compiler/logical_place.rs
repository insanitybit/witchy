//! One typed place retains root identity separately from physical/flat projections.
use super::logical_body::{Expr, Ty};

#[derive(Clone, Debug)]
pub(super) struct Place {
    pub root: u32,
    pub identity: u32,
    pub root_ty: Ty,
    pub ty: Ty,
    pub pointer: bool,
    pub mutable: bool,
    pub projections: Vec<Projection>,
}
#[derive(Clone, Debug)]
pub(super) enum Projection {
    Field { ordinal: u32 },
    Index { value: Expr, length: u32 },
}
