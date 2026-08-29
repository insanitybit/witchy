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

use crate::layout::{LayoutId, LayoutInterner, LayoutSize, RcHeader, ScalarKind};
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
    /// `[u32 length][8-byte slots...]` -- the shape a List has. Physically the
    /// same descriptor family as `byte_buffer`, differing only in stride, which
    /// the descriptor supplies: a runtime buffer over `Int` is
    /// `Dynamic { base: 4, stride: 8 }`. Sharing the family is what lets one set
    /// of accessors serve both without a per-caller branch.
    slot_buffer: LayoutId,
    /// `[u32 length][u32 capacity][1-byte elements...]` -- a source packed list
    /// over a byte-wide element, which is `Dynamic { base: 8, stride: 1 }` with a
    /// real `HeaderLayout::PackedList`. Distinct from the two runtime buffers
    /// above: it carries a CAPACITY word, so its payload starts at eight.
    bool_list: LayoutId,
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
        let slot = interner
            .intern_scalar(ScalarKind::Int)
            .expect("the Int scalar is a primitive layout");
        let slot_buffer = interner
            .intern_runtime_buffer(slot)
            .expect("a runtime buffer over an exact scalar is always internable");
        let bool_element = interner
            .intern_scalar(ScalarKind::Bool)
            .expect("the Bool scalar is a primitive layout");
        let bool_list = interner
            .intern_packed_list(bool_element, RcHeader::Required)
            .expect("a packed list over an exact fixed element is always internable");
        HelperLayouts { interner, byte_buffer, slot_buffer, bool_list }
    })
}

/// A checked place for the length word of a helper-allocated SLOT list.
///
/// `count` is the buffer's element CAPACITY, not the length being written. The
/// stored value is data and may be any expression; the count is a claim operand
/// and must be a leaf, so this is migratable exactly where the helper already
/// holds a capacity local -- which the `*_cap` family does, by construction.
pub(crate) fn slot_buffer_length_place(root: WirExpr, count: WirExpr) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.slot_buffer,
        SafeExactExtent::counted(count).expect("a capacity local is an ordinary claim operand"),
        root.clone(),
        AccessMode::Write,
    )
    .expect("the helper slot-buffer layout is claimable")
    .runtime_buffer_length(SafeEncodedAddress::new(root, 0))
    .expect("the length word is at the descriptor's own header offset")
}

/// A checked place for READING the length word of a slot list.
///
/// As with the byte-buffer read, the read is what establishes the count, so
/// there is no count to claim yet and this claims only the descriptor's header.
pub(crate) fn slot_buffer_length_read_place(root: WirExpr) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.slot_buffer,
        SafeExactExtent::header_only(),
        root.clone(),
        AccessMode::Read,
    )
    .expect("the helper slot-buffer header is claimable")
    .runtime_buffer_length(SafeEncodedAddress::new(root, 0))
    .expect("the length word is at the descriptor's own header offset")
}

/// A checked place for one slot of a helper-allocated slot list.
///
/// The stride is eight and comes from the descriptor, never from here.
pub(crate) fn slot_buffer_element_place(
    root: WirExpr,
    index: WirExpr,
    count: WirExpr,
    access: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    let payload = payload_offset(layouts.slot_buffer);
    let stride = element_stride(layouts.slot_buffer);
    // A scalar access carries a wasm immediate, so the payload offset rides
    // THERE and costs nothing. Folding it into the base instead would emit an
    // extra `i32.const`/`i32.add` per site -- measured as +2 instructions each,
    // which the migration may not spend. Only a bulk copy, which has no
    // immediate operand, has to fold (see the slice accessor below).
    let (encoded, immediate) = scaled_index_address(root.clone(), &index, payload, stride);
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.slot_buffer,
        SafeExactExtent::counted(count).expect("a capacity local is an ordinary claim operand"),
        root,
        access,
    )
    .expect("the helper slot-buffer layout is claimable")
    .element(index, SafeEncodedAddress::new(encoded, immediate))
    .expect("a descriptor-strided slot index reconciles against the descriptor")
}

/// A checked place for a contiguous run of slots, for bulk copies.
///
/// `start` and `len` are in ELEMENTS; the descriptor's stride turns them into
/// bytes. As with the byte-buffer slice, the payload offset rides in the base
/// with a zero immediate so a `memory.copy` -- which has no immediate operand --
/// can carry it.
pub(crate) fn slot_buffer_payload_slice_place(
    root: WirExpr,
    start: WirExpr,
    len: WirExpr,
    count: WirExpr,
    access: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    let payload = payload_offset(layouts.slot_buffer);
    let stride = element_stride(layouts.slot_buffer);
    let encoded = strided_element_address(root.clone(), &start, payload, stride);
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.slot_buffer,
        SafeExactExtent::counted(count).expect("a capacity local is an ordinary claim operand"),
        root,
        access,
    )
    .expect("the helper slot-buffer layout is claimable")
    .slice(start, len, SafeEncodedAddress::new(encoded, 0))
    .expect("a contiguous slot slice reconciles against the descriptor")
}

/// A checked place for one header component of a byte-wide packed list.
///
/// Component zero is the length and component one is the capacity; both offsets
/// come from the descriptor's own `HeaderLayout::PackedList`.
pub(crate) fn bool_list_header_place(
    root: WirExpr,
    component: u32,
    count: WirExpr,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.bool_list,
        SafeExactExtent::counted(count).expect("a capacity local is an ordinary claim operand"),
        root.clone(),
        AccessMode::Write,
    )
    .expect("the helper packed-list layout is claimable")
    .header(component, SafeEncodedAddress::new(root, header_offset(layouts.bool_list, component)))
    .expect("a header component sits at the descriptor's own offset")
}

/// A checked place for a contiguous run of a byte-wide packed list's payload.
pub(crate) fn bool_list_payload_slice_place(
    root: WirExpr,
    start: WirExpr,
    len: WirExpr,
    count: WirExpr,
    access: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    let payload = payload_offset(layouts.bool_list);
    let stride = element_stride(layouts.bool_list);
    let encoded = strided_element_address(root.clone(), &start, payload, stride);
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.bool_list,
        SafeExactExtent::counted(count).expect("a capacity local is an ordinary claim operand"),
        root,
        access,
    )
    .expect("the helper packed-list layout is claimable")
    .slice(start, len, SafeEncodedAddress::new(encoded, 0))
    .expect("a contiguous payload slice reconciles against the descriptor")
}

/// A header component's byte offset, asked of the descriptor.
fn header_offset(layout: LayoutId, component: u32) -> u32 {
    let descriptor = helper_layouts()
        .interner
        .get(layout)
        .expect("the helper layout is interned in this module");
    match descriptor.header() {
        crate::layout::HeaderLayout::PackedList { length_offset, capacity_offset, .. } => {
            match component {
                0 => length_offset,
                1 => capacity_offset,
                _ => unreachable!("a packed list defines two header components"),
            }
        }
        crate::layout::HeaderLayout::None => {
            unreachable!("this layout defines a packed-list header")
        }
    }
}

/// The encoded address of element `index`, with the payload offset left in the
/// wasm IMMEDIATE so a scalar access spends no instruction on it.
///
/// A constant index folds into that immediate too, because a constant
/// projection may not carry an index term -- `validate_fixed_address` refuses a
/// scaled term, correctly, since such an address denotes a projection the claim
/// did not make.
fn scaled_index_address(
    root: WirExpr,
    index: &WirExpr,
    payload: i32,
    stride: i32,
) -> (WirExpr, u32) {
    if let WirExpr::ConstI32(n) = index {
        let immediate = u32::try_from(payload + n * stride)
            .expect("a constant slot offset is a nonnegative address");
        return (root, immediate);
    }
    let scaled = WirExpr::Binary {
        op: crate::wir::BinOp::Mul,
        kind: crate::wir::Kind::I32,
        lhs: Box::new(index.clone()),
        rhs: Box::new(WirExpr::ConstI32(stride)),
    };
    let immediate = u32::try_from(payload).expect("a payload offset is nonnegative");
    (add32(root, scaled), immediate)
}

/// The encoded address of element `index` in a strided runtime buffer.
///
/// A CONSTANT index is folded into the payload constant rather than emitted as
/// `index * stride`. Both spell the same address, but a constant projection may
/// not carry an index term -- `validate_fixed_address` refuses a scaled term,
/// correctly, because that address would denote a projection the claim did not
/// make. Folding keeps the two spellings in agreement with the grammar.
fn strided_element_address(root: WirExpr, index: &WirExpr, payload: i32, stride: i32) -> WirExpr {
    if let WirExpr::ConstI32(n) = index {
        let offset = payload + n * stride;
        return add32(root, WirExpr::ConstI32(offset));
    }
    // Stride one needs no multiply: the index IS the byte offset. Emitting
    // `index * 1` cost two instructions per site, measured as +8 across three
    // byte-buffer copies, which the migration may not spend. The builder's
    // address grammar already treats a bare leaf as a stride-one index.
    if stride == 1 {
        return add32(add32(root, WirExpr::ConstI32(payload)), index.clone());
    }
    add32(
        add32(root, WirExpr::ConstI32(payload)),
        WirExpr::Binary {
            op: crate::wir::BinOp::Mul,
            kind: crate::wir::Kind::I32,
            lhs: Box::new(index.clone()),
            rhs: Box::new(WirExpr::ConstI32(stride)),
        },
    )
}

/// The descriptor's element stride, asked rather than assumed.
fn element_stride(layout: LayoutId) -> i32 {
    let descriptor = helper_layouts()
        .interner
        .get(layout)
        .expect("the helper layout is interned in this module");
    match descriptor.size() {
        LayoutSize::Dynamic { stride, .. } => {
            i32::try_from(stride).expect("a runtime buffer stride is a small width")
        }
        LayoutSize::Fixed(_) => unreachable!("a runtime buffer descriptor is dynamically sized"),
    }
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
    // Through the folding helper, so a constant start becomes part of the
    // constant rather than an emitted `+ 0`: this accessor built the address
    // unconditionally, which cost two instructions at every constant-start copy.
    let encoded_base = strided_element_address(
        root.clone(),
        &start,
        payload,
        element_stride(layouts.byte_buffer),
    );
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
