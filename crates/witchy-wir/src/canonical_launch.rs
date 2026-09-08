//! The one owned data input supported by the canonical entry ABI.
//!
//! This constructs catalog identities, not authority. Admission also requires
//! the checked source parameter to be `List(String)` and the validated ABI to
//! carry this exact layout. Launchers never infer a layout from guest bytes.

use crate::layout::{HeaderLayout, LayoutError, LayoutId, LayoutInterner, LayoutSize, PointerSlot, RcHeader, ScalarKind};

pub fn argument_layouts() -> Result<(LayoutId, LayoutId), LayoutError> {
    let mut layouts = LayoutInterner::new();
    let byte = layouts.intern_scalar(ScalarKind::RawByte)?;
    let string = layouts.intern_runtime_buffer(byte)?;
    let slot = layouts.intern_runtime_pointer_value_slot(string, PointerSlot::Owned)?;
    let list = layouts.intern_packed_list(slot, RcHeader::Required)?;
    // Entry wire version 2 has one deliberately fixed host materialization
    // contract. A later catalog change must revise that writer/protocol, not
    // silently reinterpret its lengths, pointer lanes, or byte offsets.
    if layouts.get(string).map(|layout| layout.size()) != Some(LayoutSize::Dynamic { base: 4, stride: 1 })
        || layouts.get(slot).map(|layout| layout.size()) != Some(LayoutSize::Fixed(8))
        || layouts.get(list).map(|layout| layout.size()) != Some(LayoutSize::Dynamic { base: 8, stride: 8 })
        || !matches!(layouts.get(list).map(|layout| layout.header()), Some(HeaderLayout::PackedList {
            rc: RcHeader::Required, length_offset: 0, capacity_offset: 4, data_offset: 8,
        }))
    {
        return Err(LayoutError::DescriptorInvariant("launch input layout differs from the versioned host encoding"));
    }
    Ok((list, string))
}
