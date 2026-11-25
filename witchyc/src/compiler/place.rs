//! A lowered place uses concrete roots and physical/flat projections.
use super::ir::{Expr, Ty};

#[derive(Debug)]
pub(super) struct Place {
    pub root: u32,
    pub root_ty: Ty,
    pub ty: Ty,
    pub pointer: bool,
    pub projections: Vec<Projection>,
}
#[derive(Debug)]
pub(super) enum Projection {
    Field { slot: u32, byte_offset: u32 },
    Index { value: Expr, length: u32, slots: u32, stride: u32 },
}
impl Place {
    pub fn static_offsets(&self) -> Option<(u32, u32)> {
        let (mut slot, mut offset) = (0, 0);
        for projection in &self.projections {
            match projection {
                Projection::Field { slot: s, byte_offset: b } => { slot += *s; offset += *b; }
                Projection::Index { .. } => return None,
            }
        }
        Some((slot, offset))
    }
}
