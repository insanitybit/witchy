//! Canonical runtime layouts for the WIR helper library.
//!
//! The helpers build `WirFunc`s from scratch and have no layout context: they
//! allocate a `[u32 length][payload]` block through `rc_alloc` and address it
//! with hand-computed offsets. RFC-0162 needs those accesses to become checked
//! SafeWIR places, and a checked place needs a descriptor.
//!
//! Threading a `LayoutInterner` through every helper entry point would mean
//! changing ~25 signatures for what is a fixed, static vocabulary. It is also
//! unnecessary: a `LayoutId` is `sha256(canonical_bytes)`, so an interner built
//! here mints exactly the same ids as the compiler's own, and
//! `SafePlaceBuilder` accepts a descriptor from any interner as long as the id
//! is canonical. This module is that interner, built once.

use std::sync::OnceLock;

use crate::layout::{LayoutId, LayoutInterner, ScalarKind};
use crate::safewir::AccessMode;
use crate::safewir_surface::{
    SafeEncodedAddress, SafeExactExtent, SafePlaceBuilder, SafePlaceExpr,
};
use crate::wir::WirExpr;

/// The interner and the ids the helpers address through.
pub(crate) struct HelperLayouts {
    interner: LayoutInterner,
    /// `[u32 length][bytes...]` -- the shape `rc_alloc` hands back for strings,
    /// byte buffers, and every other payload the helpers treat as bytes.
    byte_buffer: LayoutId,
}

fn helper_layouts() -> &'static HelperLayouts {
    static LAYOUTS: OnceLock<HelperLayouts> = OnceLock::new();
    LAYOUTS.get_or_init(|| {
        let mut interner = LayoutInterner::new();
        let byte = interner
            .intern_scalar(ScalarKind::RawByte)
            .expect("the raw byte scalar is a primitive layout");
        let byte_buffer = interner
            .intern_runtime_buffer(byte)
            .expect("a runtime buffer over an exact scalar is always internable");
        HelperLayouts { interner, byte_buffer }
    })
}

/// A checked place for READING the length word of a helper-allocated byte
/// buffer.
///
/// The read is what establishes the count, so there is no count to claim yet.
/// It claims only the descriptor's header region, which is strictly less than
/// the write path claims.
pub(crate) fn byte_buffer_length_read_place(root: WirExpr) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.byte_buffer,
        SafeExactExtent::header_only(),
        root.clone(),
        AccessMode::Read,
    )
    .expect("the helper byte-buffer header is claimable")
    .runtime_buffer_length(SafeEncodedAddress::new(root, 0))
    .expect("the length word is at the descriptor's own header offset")
}

/// A checked place for the length word of a helper-allocated byte buffer.
///
/// `root` is the allocation pointer and `count` is the executable payload
/// length the buffer was sized for. The header offset and width come from the
/// descriptor; nothing here supplies them.
///
/// This cannot fail for a well-formed helper: the layout is interned above, the
/// root is a local, and the address is the root at offset zero. A failure means
/// the runtime buffer descriptor changed shape underneath the helper library,
/// which is a compiler bug rather than anything a program can provoke.
pub(crate) fn byte_buffer_length_place(root: WirExpr, count: WirExpr) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.byte_buffer,
        SafeExactExtent::counted(count).expect("a helper length is an ordinary claim operand"),
        root.clone(),
        AccessMode::Write,
    )
    .expect("the helper byte-buffer layout is claimable")
    .runtime_buffer_length(SafeEncodedAddress::new(root, 0))
    .expect("the length word is at the descriptor's own header offset")
}
