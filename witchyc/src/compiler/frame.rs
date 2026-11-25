//! Fixed disjoint frame plan; scalar sizes/alignment come from existing layouts.
use super::{ir::Ty, layout::Layouts};
use crate::diagnostic::Diagnostic;
use std::collections::BTreeMap;

pub(super) const HIGH: u32 = super::allocator::STACK_HIGH;
/// Frames grow down from `HIGH` to here; how far is the build's choice.
pub(super) fn low() -> u32 { super::allocator::heap().stack_low() }
pub(super) const ALIGNMENT: u32 = 8;
pub(super) const PAGES: u32 = 2;

#[derive(Default, Debug)]
pub(super) struct Frame {
    pub slots: BTreeMap<u32, u32>,
    pub roots: BTreeMap<u32, u32>,
    /// Each addressed root's type: its stored variants share bytes, so writes are by value.
    pub types: BTreeMap<u32, Ty>,
    pub bytes: u32,
}
impl Frame {
    pub fn plan(addressed: &BTreeMap<u32, Ty>, layouts: &Layouts) -> Result<Self, Diagnostic> {
        let mut result = Self::default();
        for (index, ty) in addressed {
            let (size, align) = layouts.storage(*ty)?;
            let align = u32::try_from(align).map_err(|_| overflow())?;
            let size = u32::try_from(size).map_err(|_| overflow())?;
            result.bytes = result.bytes.checked_add(align - 1).ok_or_else(overflow)? / align * align;
            let root_offset = result.bytes;
            result.roots.insert(*index, root_offset);
            result.types.insert(*index, *ty);
            for cell in layouts.cells(*ty)? {
                let offset = u32::try_from(cell.byte_offset).map_err(|_| overflow())?;
                result.slots.insert(index + cell.slot, root_offset.checked_add(offset).ok_or_else(overflow)?);
            }
            result.bytes = result.bytes.checked_add(size).ok_or_else(overflow)?;
        }
        result.bytes = result.bytes.checked_add(ALIGNMENT - 1).ok_or_else(overflow)? / ALIGNMENT * ALIGNMENT;
        Ok(result)
    }
}
fn overflow() -> Diagnostic { Diagnostic::new("W0403", 0..0, "fixed frame size overflow") }

pub(super) fn memory_receipt(heap: bool) -> String {
    let pages = if heap { super::allocator::heap().pages } else { PAGES };
    let low = low();
    format!("{{\"pages\":{pages},\"maximum_pages\":{pages},\"stack_low\":{low},\"stack_high\":{HIGH},\"stack_alignment\":{ALIGNMENT},\"allocator\":{heap},\"memory_exported\":true}}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_trailing_padding_is_reserved_and_never_a_cell() {
        let outer=crate::ast::Type::Named("Outer".into(),vec![]);
        let array=crate::ast::Type::Array{element:Box::new(outer.clone()),length:2};
        let (layouts,types)=super::super::concrete::test_layouts("type Inner:\n    wide: U64\n    small: U8\ntype Outer:\n    inner: Inner\n    tail: U8\n", &[outer,array]).unwrap();
        let ty=types[0];
        let frame = Frame::plan(&BTreeMap::from([(0, ty)]), &layouts).unwrap();
        assert_eq!(frame.bytes, 24);
        assert_eq!(frame.roots, BTreeMap::from([(0, 0)]));
        assert_eq!(frame.slots, BTreeMap::from([(0, 0), (1, 8), (2, 16)]));
        let cells = layouts.cells(ty).unwrap();
        assert_eq!(cells.iter().map(|cell| cell.byte_offset).collect::<Vec<_>>(), vec![0, 8, 16]);
        let array = types[1];
        assert_eq!(layouts.cells(array).unwrap().iter().map(|cell| cell.byte_offset).collect::<Vec<_>>(), vec![0, 8, 16, 24, 32, 40]);
    }
}
