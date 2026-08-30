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
    /// `[u32 count][16-byte rows...]` -- the DENSE entry array a `Dict` handle
    /// exposes, which is `Dynamic { base: 4, stride: 16 }` over a row of
    /// `{ key@0, value@8 }`. The two eight-byte lanes are why a row needs its own
    /// descriptor: a runtime buffer's element is one scalar, and no single-step
    /// projection can name "row `i`'s value".
    ///
    /// This is NOT the Swiss carrier the same helpers also address. The carrier
    /// is `RuntimeDictionaryIndex`, `Dynamic { base: 20, stride: 25 }`, and it is
    /// section-major (all control bytes, then all indices, then all keys...), so
    /// a section's base is a function of the slot count rather than a descriptor
    /// constant. Its extent is expressible; its projections are not, and this
    /// module deliberately offers no carrier accessor.
    dict_body: LayoutId,
    /// The Swiss control/index carrier: `[u32 slots][ctrl: slots + 16]
    /// [index: slots * 4][keys: slots * 8][values: slots * 8][order: slots * 4]`,
    /// which is `RuntimeDictionaryIndex`, `Dynamic { base: 20, stride: 25 }`.
    ///
    /// SECTION-MAJOR: it stores all control bytes, then all entry indices, then
    /// all key slots, so a section's base scales with the slot count and no
    /// element/field chain can name a slot. Its accessor projects through the
    /// descriptor's section table instead.
    carrier: LayoutId,
}

/// Field indices into a dictionary ROW, in descriptor order.
pub(crate) const DICT_ROW_KEY: u32 = 0;
pub(crate) const DICT_ROW_VALUE: u32 = 1;

/// The dense entry array's own count word, field zero of the body descriptor.
const DICT_BODY_COUNT: u32 = 0;

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
        // A dense row's key and value lanes are raw tagged slots, which is the
        // same `Int` scalar the slot buffer is built over.
        let dict_body = interner
            .intern_runtime_dictionary_body(slot, slot)
            .expect("a dictionary body over exact scalar slots is always internable");
        let carrier = interner
            .intern_runtime_dictionary_index(slot, slot)
            .expect("a dictionary carrier over exact scalar slots is always internable");
        HelperLayouts { interner, byte_buffer, slot_buffer, bool_list, dict_body, carrier }
    })
}

/// A checked place for the dense entry array's count word.
///
/// `count` is the array's row CAPACITY -- the `cap` the `*_extract` helpers
/// already carry -- not the entry count being stored. The stored value is data.
pub(crate) fn dict_body_count_place(
    root: WirExpr,
    count: WirExpr,
    access: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.dict_body,
        SafeExactExtent::counted(count).expect("a capacity local is an ordinary claim operand"),
        root.clone(),
        access,
    )
    .expect("the helper dictionary-body layout is claimable")
    .fixed_field(DICT_BODY_COUNT, SafeEncodedAddress::new(root, 0))
    .expect("the count word is field zero at offset zero")
}

/// A checked place for one lane of dense entry `index` -- its key or its value.
///
/// The row's stride and the lane's offset within it both come from the
/// descriptor: `4 + index * 16` for a key and `12 + index * 16` for a value,
/// which is what the helpers already emit, so the wasm immediate absorbs the
/// whole constant part and the migration spends no instruction.
///
/// Both `index` and `count` must be leaves. In practice that means this serves
/// the helpers that step the dense array by a bound `count`/`found` local while
/// holding `cap`; a helper reaching a row through a loaded bucket index (as
/// `dict/read.rs` does) has neither, and is not migratable here.
pub(crate) fn dict_entry_lane_place(
    root: WirExpr,
    index: WirExpr,
    lane: u32,
    count: WirExpr,
    access: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    let payload = payload_offset(layouts.dict_body);
    let stride = element_stride(layouts.dict_body);
    let within = row_lane_offset(layouts.dict_body, lane);
    let (encoded, immediate) = scaled_index_address(root.clone(), &index, payload + within, stride);
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.dict_body,
        SafeExactExtent::counted(count).expect("a capacity local is an ordinary claim operand"),
        root,
        access,
    )
    .expect("the helper dictionary-body layout is claimable")
    .element_field(index, lane, SafeEncodedAddress::new(encoded, immediate))
    .expect("a descriptor-strided row lane reconciles against the descriptor")
}

/// A row lane's offset WITHIN its row, asked of the row descriptor.
fn row_lane_offset(layout: LayoutId, lane: u32) -> i32 {
    let layouts = helper_layouts();
    let row = layouts
        .interner
        .get(layout)
        .expect("the helper layout is interned in this module")
        .kind()
        .packed_element_layout()
        .expect("a dictionary body describes a row element");
    let offset = layouts
        .interner
        .get(row)
        .expect("the row layout is interned alongside its body")
        .fields()
        .get(lane as usize)
        .expect("a dictionary row defines a key lane and a value lane")
        .offset();
    i32::try_from(offset).expect("a row lane offset is a small width")
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

#[cfg(test)]
mod final_validation {
    use super::helper_layouts;
    use crate::layout::{LayoutBundle, LayoutId};
    use crate::safewir::{
        CallableMemoryContract, CallableMemoryResult, ContractExtent, ContractSizeExpr,
        ParameterMemoryContract, ParameterMemoryEffect, ParameterMemoryEffects,
        ParameterMemoryEntry,
    };
    use crate::safewir_adapter::{validate_final_wir_report, FinalWirValidationInputs};
    use crate::safewir_validate::ValidationBudget;
    use std::collections::BTreeMap;

    /// How far a fully migrated helper is from the VALIDATED encoder.
    ///
    /// This is the first end-to-end measurement of that distance, and it exists
    /// because the raw-constructor census does not measure it. The production
    /// encoder runs `ErasureMode::TransitionalBridge`
    /// (`witchy-lower/src/codegen/assembly.rs`), which erases claims WITHOUT
    /// checking them, so a helper can be fully migrated and still compile
    /// unchecked. Migration is the precondition; passing this is the goal.
    ///
    /// The two subjects are chosen to separate the gates. `ascii_case` is
    /// completely migrated -- header read, both SIMD lane accesses, and the
    /// scalar tail -- and returns a String. `dict_reindex` is migrated on its
    /// dense side and returns nothing. The difference matters: the FIRST refusal
    /// is not about memory at all.
    ///
    /// Each expectation pins the message the validator gives TODAY, so that
    /// closing one of these gates fails this test and says which. A test that
    /// merely asserted "not yet validated" would keep passing while the reason
    /// silently changed.
    ///
    /// The measured roadmap, in the order a helper meets it:
    ///
    ///   1. value-returning callables require the deferred return-value def-use
    ///      adapter                     -- blocks nearly every helper, and is
    ///                                     reached before any claim is examined
    ///   2. symbolic counted parameter extents require the deferred def-use
    ///      adapter                     -- blocks any runtime-sized memory
    ///                                     parameter
    ///   3. mutable local definitions require the deferred reaching-definition
    ///      graph                       -- blocks every loop
    ///
    /// Gate 3 is only visible once gate 2 is cleared with a constant extent,
    /// which is why the same helper appears twice with different contracts.
    ///
    /// All three name one component: the reaching-definition graph. None names a
    /// missing projection, accessor, or layout -- so the accessor work this
    /// migration has been doing is the precondition and not the blocker. Note
    /// also what gate 3 implies: a mutable local written in a loop has more than
    /// one definition reaching its use, so binding it needs a JOIN over merge
    /// points, not merely a lookup. The representation for that already exists
    /// (`SizeExpr::Definition`, and `CheckedIndex`'s `length_source`); what is
    /// concrete today is the EVALUATION -- `layout_extent` and
    /// `derive_projection` return `u128`.
    #[test]
    fn migrated_helpers_report_their_distance_from_validated_encoding() {
        // A value-returning callable is refused before any claim is examined, so
        // this gate is in front of every helper that returns anything -- which is
        // nearly all of them.
        assert_distance(
            crate::wir_helpers::strings::ascii_case_helper(),
            vec![scalar_parameter(), scalar_parameter()],
            "value-returning callables require the deferred return-value def-use adapter",
        );
        // With that gate cleared by a void signature, the next refusal is the one
        // the leaf rules were built for: binding a loop-carried operand to an
        // exact reaching definition.
        assert_distance(
            crate::wir_helpers::dict_reindex_helper(),
            vec![
                counted_parameter(helper_layouts().dict_body, 1, 16, true),
                scalar_parameter(),
                scalar_parameter(),
            ],
            "symbolic counted parameter extents require the deferred def-use adapter",
        );
        // The same helper with a CONSTANT extent clears the contract-level gate,
        // which isolates what the OPERATIONS themselves stop at -- this helper's
        // dense side uses the two-step row projection, so it exercises more than
        // scalar access.
        assert_distance(
            crate::wir_helpers::dict_reindex_helper(),
            vec![
                ParameterMemoryContract {
                    entry: ParameterMemoryEntry::BorrowedWrite,
                    layout: Some(helper_layouts().dict_body),
                    extent: Some(ContractExtent::Exact(ContractSizeExpr::Constant(4 + 16 * 8))),
                    effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Read)
                        .with(ParameterMemoryEffect::Write),
                },
                scalar_parameter(),
                scalar_parameter(),
            ],
            "mutable local definitions require the deferred reaching-definition graph",
        );
    }

    /// A scalar parameter that carries no memory authority.
    fn scalar_parameter() -> ParameterMemoryContract {
        ParameterMemoryContract {
            entry: ParameterMemoryEntry::NoMemory,
            layout: None,
            extent: None,
            effects: ParameterMemoryEffects::none(),
        }
    }

    /// A memory parameter whose extent is a function of another parameter.
    ///
    /// This is what a helper's real contract looks like: `dict_reindex(d, cap, ..)`
    /// borrows a dense array of `4 + cap * 16` bytes, so the extent names
    /// parameter one rather than a constant. `ContractSizeExpr::Parameter`
    /// exists precisely for that, which is a good sign the contract vocabulary
    /// anticipated runtime-sized helper arguments.
    fn counted_parameter(
        layout: LayoutId,
        count: usize,
        stride: u64,
        write: bool,
    ) -> ParameterMemoryContract {
        ParameterMemoryContract {
            entry: if write {
                ParameterMemoryEntry::BorrowedWrite
            } else {
                ParameterMemoryEntry::BorrowedRead
            },
            layout: Some(layout),
            extent: Some(ContractExtent::Counted {
                header: ContractSizeExpr::Constant(4),
                count: ContractSizeExpr::Parameter(count),
                stride: ContractSizeExpr::Constant(stride),
            }),
            effects: if write {
                ParameterMemoryEffects::only(ParameterMemoryEffect::Read)
                    .with(ParameterMemoryEffect::Write)
            } else {
                ParameterMemoryEffects::only(ParameterMemoryEffect::Read)
            },
        }
    }

    /// Run one helper through report-mode final validation and pin the outcome.
    fn assert_distance(
        function: crate::wir::WirFunc,
        parameters: Vec<ParameterMemoryContract>,
        expected: &str,
    ) {
        let layouts = helper_layouts();
        let bundle = LayoutBundle::from_interner(
            &layouts.interner,
            [layouts.byte_buffer, layouts.dict_body],
        )
        .expect("the helper layouts bundle");
        let name = function.name.clone();
        assert_eq!(
            parameters.len(),
            function.params.len(),
            "`{name}` declares {} parameters; the contract lists {}",
            function.params.len(),
            parameters.len(),
        );
        let module = crate::wir::WirModule {
            imports: Vec::new(),
            funcs: vec![function],
            memory_pages: 1,
            data: Vec::new(),
            globals: Vec::new(),
            table: None,
            exports: Vec::new(),
        };
        let inputs = FinalWirValidationInputs {
            layouts: bundle,
            functions: BTreeMap::from([(
                name.clone(),
                CallableMemoryContract { parameters, result: CallableMemoryResult::NoMemory },
            )]),
            imports: BTreeMap::new(),
            host_imports: BTreeMap::new(),
        };
        match validate_final_wir_report(&module, &[], &[], inputs, ValidationBudget::default()) {
            Ok(report) => panic!(
                "`{name}` now passes final validation ({} operations). That is the \
                 milestone: wire the production encoder to the validated path.",
                report.metrics().operations,
            ),
            Err(error) => {
                let reported = error.to_string();
                assert!(
                    reported.contains(expected),
                    "`{name}` is expected to stop at `{expected}`; it now reports \
                     something else, which is the new roadmap and belongs in this \
                     expectation: {reported}",
                );
            }
        }
    }
}

/// A checked place for slot `index` of carrier section `section`.
///
/// `count` is the carrier's slot count -- the `slots` every dict helper already
/// carries. The section's base and element width come from the descriptor's
/// section table, so this builds the address the helpers spelled by hand
/// (`idx + 4 + h` for a control byte, `idx + 20 + slots * 5 + h * 8` for a key)
/// without repeating any of those constants here.
pub(crate) fn carrier_slot_place(
    root: WirExpr,
    section: u32,
    index: WirExpr,
    count: WirExpr,
    access: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    let spec = carrier_section(section);
    let encoded = section_address(&root, spec, &count, &index);
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.carrier,
        SafeExactExtent::counted(count).expect("a slot-count local is an ordinary claim operand"),
        root,
        access,
    )
    .expect("the helper carrier layout is claimable")
    .section_element(section, index, SafeEncodedAddress::new(encoded, 0))
    .expect("a descriptor-derived section address reconciles against the section table")
}

/// The descriptor's spec for one carrier section, asked rather than assumed.
fn carrier_section(section: u32) -> crate::layout::SectionSpec {
    let layouts = helper_layouts();
    *layouts
        .interner
        .get(layouts.carrier)
        .expect("the carrier layout is interned in this module")
        .kind()
        .sections()
        .expect("the carrier is section-major")
        .get(section as usize)
        .expect("the caller names a section the table defines")
}

/// `root + constant_base + count * count_multiple + index * element_width`,
/// with a multiple of zero contributing no term and a width or multiple of one
/// riding as a bare leaf -- the same spelling the helpers used.
fn section_address(
    root: &WirExpr,
    spec: crate::layout::SectionSpec,
    count: &WirExpr,
    index: &WirExpr,
) -> WirExpr {
    let mut address = add32(
        root.clone(),
        WirExpr::ConstI32(
            i32::try_from(spec.constant_base).expect("a section base is a small offset"),
        ),
    );
    if spec.count_multiple != 0 {
        address = add32(address, scale(count, spec.count_multiple));
    }
    // A constant index folds into the address constant rather than emitting
    // `0 * width`, which is not an affine term at all (both operands constant).
    if let WirExpr::ConstI32(value) = index {
        let folded = value
            .checked_mul(i32::try_from(spec.element_width).expect("a small width"))
            .expect("a constant section offset does not overflow");
        if folded == 0 {
            return address;
        }
        return add32(address, WirExpr::ConstI32(folded));
    }
    add32(address, scale(index, spec.element_width))
}

/// `operand * scale`, or the bare operand when the scale is one.
fn scale(operand: &WirExpr, scale: u32) -> WirExpr {
    if scale == 1 {
        return operand.clone();
    }
    WirExpr::Binary {
        op: crate::wir::BinOp::Mul,
        kind: crate::wir::Kind::I32,
        lhs: Box::new(operand.clone()),
        rhs: Box::new(WirExpr::ConstI32(
            i32::try_from(scale).expect("a section element width is a small width"),
        )),
    }
}


/// A checked place spanning a contiguous run inside one carrier section.
///
/// `start` is a slot index within the section and `length` is the byte count the
/// bulk op writes. The section's base still comes from the descriptor, so a fill
/// over a whole section names the section rather than recomputing its base.
pub(crate) fn carrier_section_slice_place(
    root: WirExpr,
    section: u32,
    start: WirExpr,
    length: WirExpr,
    count: WirExpr,
    access: AccessMode,
) -> Box<SafePlaceExpr> {
    let layouts = helper_layouts();
    let spec = carrier_section(section);
    let encoded = section_address(&root, spec, &count, &start);
    SafePlaceBuilder::new(
        &layouts.interner,
        layouts.carrier,
        SafeExactExtent::counted(count).expect("a slot-count local is an ordinary claim operand"),
        root,
        access,
    )
    .expect("the helper carrier layout is claimable")
    .section_slice(section, start, length, SafeEncodedAddress::new(encoded, 0))
    .expect("a descriptor-derived section range reconciles against the section table")
}
