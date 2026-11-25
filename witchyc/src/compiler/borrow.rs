//! Storable referents; shared Console access grants no permission to copy its owner.
use super::{ir::Ty, scalar::Scalar};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Pointee {
    Unsigned(Scalar),
    I64,
    F64,
    Bool,
    Console,
    Input,
    Allocator,
    Host(super::host::Host),
    Buffer(u32),
    Boxed,
    Aggregate(u32),
    Sum(u32),
    Slice(u32),
}
impl Pointee {
    pub fn from_ty(ty: Ty) -> Option<Self> {
        match ty {
            Ty::Unsigned(s) => Some(Self::Unsigned(s)),
            Ty::I64 => Some(Self::I64),
            Ty::F64 => Some(Self::F64),
            Ty::Bool => Some(Self::Bool),
            Ty::Console => Some(Self::Console),
            Ty::Input => Some(Self::Input),
            Ty::Allocator => Some(Self::Allocator),
            Ty::Host(h) => Some(Self::Host(h)),
            Ty::Boxed => Some(Self::Boxed),
            Ty::Buffer(id) => Some(Self::Buffer(id)),
            Ty::Aggregate(index) => Some(Self::Aggregate(index)),
            Ty::Sum(index) => Some(Self::Sum(index)),
            Ty::Slice(index) => Some(Self::Slice(index)),
            _ => None,
        }
    }
}
