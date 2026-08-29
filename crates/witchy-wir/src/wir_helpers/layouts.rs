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

use crate::layout::{LayoutId, LayoutInterner, LayoutSize, ScalarKind};
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

/// A checked place for one payload byte of a helper-allocated byte buffer.
///
/// `count` must be a LOCAL already holding the buffer's length -- typically the
/// `len`/`slen`/`plen` the helper loaded before its loop. It cannot be the
/// length `Load` itself: a claim operand has to be a leaf the final validator
/// can bind to an exact reaching definition, and `validate_claim_operand`
/// rightly refuses anything else. Where a helper has no such local, the site is
/// not migratable yet and must wait for the def-use adapter rather than have a
/// local invented for it -- adding one would change emitted code and forfeit
/// the zero-cost property.
///
/// The base offset and stride come from the descriptor; the caller supplies
/// only the root, the index, and the count.
pub(crate) fn byte_buffer_element_place(
    root: WirExpr,
    index: WirExpr,
    count: WirExpr,
    access: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    let encoded = WirExpr::Binary {
        op: crate::wir::BinOp::Add,
        kind: crate::wir::Kind::I32,
        lhs: Box::new(root.clone()),
        rhs: Box::new(index.clone()),
    };
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.byte_buffer,
        SafeExactExtent::counted(count).expect("a length local is an ordinary claim operand"),
        root,
        access,
    )
    .expect("the helper byte-buffer layout is claimable")
    .element(index, SafeEncodedAddress::new(encoded, 4))
    .expect("a stride-one payload index reconciles against the descriptor")
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

/// A checked place for a contiguous payload slice starting at `start` with
/// length `len` within a helper-allocated byte buffer.
///
/// Used for bulk copy operations. Both operands must be leaves (locals or
/// constants) for the validator to bind them.
pub(crate) fn byte_buffer_payload_slice_place(
    root: WirExpr,
    start: WirExpr,
    len: WirExpr,
    count: WirExpr,
    mode: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    // The payload offset rides in the BASE with a zero wasm immediate, never in
    // the immediate. A raw `memory.copy` has no immediate operand, so
    // `erase_node` refuses a bulk-copy place that kept one ("a bulk-copy place
    // retained a nonzero immediate offset"). Emitting `root + payload + start`
    // is the only spelling that serves scalar and bulk operations alike, and
    // `validate_fixed_address` reconciles base and immediate by their total, so
    // it is accepted for a constant start as well as a dynamic one.
    let payload = payload_offset(layouts.byte_buffer);
    let encoded_base = add32(add32(root.clone(), WirExpr::ConstI32(payload)), start.clone());
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.byte_buffer,
        SafeExactExtent::counted(count).expect("a length local is an ordinary claim operand"),
        root,
        mode,
    )
    .expect("the helper byte-buffer layout is claimable")
    .slice(start, len, SafeEncodedAddress::new(encoded_base, 0))
    .expect("a contiguous payload slice reconciles against the descriptor")
}

/// A checked place for the complete byte buffer (header + payload) for bulk
/// operations like `memory.copy` and `memory.fill`.
///
/// This claims the entire buffer at `root` with no projection, spanning from
/// offset 0 through the full dynamic extent (header + `count` payload bytes).
/// Used for operations that transfer or initialize the whole object rather than
/// addressing individual elements.
///
/// The address is the root at immediate zero, so this shape already satisfied
/// the bulk-copy rule that `byte_buffer_payload_slice_place` above had to be
/// folded to meet.
pub(crate) fn byte_buffer_root_place(
    root: WirExpr,
    count: WirExpr,
    access: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.byte_buffer,
        SafeExactExtent::counted(count).expect("a helper length is an ordinary claim operand"),
        root.clone(),
        access,
    )
    .expect("the helper byte-buffer layout is claimable")
    .root(SafeEncodedAddress::new(root, 0))
    .expect("the root place at the buffer base is claimable")
}

fn add32(lhs: WirExpr, rhs: WirExpr) -> WirExpr {
    WirExpr::Binary {
        op: crate::wir::BinOp::Add,
        kind: crate::wir::Kind::I32,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

/// Where the payload starts, asked of the descriptor rather than assumed.
///
/// A runtime buffer is `[u32 length][payload]`, so this is four today. Reading
/// it from `LayoutSize::Dynamic` means a descriptor change moves the helpers
/// with it instead of silently addressing the wrong byte.
fn payload_offset(layout: LayoutId) -> i32 {
    let descriptor = helper_layouts()
        .interner
        .get(layout)
        .expect("the helper layout is interned in this module");
    match descriptor.size() {
        LayoutSize::Dynamic { base, .. } => {
            i32::try_from(base).expect("a runtime buffer header is a small offset")
        }
        LayoutSize::Fixed(_) => {
            unreachable!("a runtime buffer descriptor is dynamically sized")
        }
    }
}
