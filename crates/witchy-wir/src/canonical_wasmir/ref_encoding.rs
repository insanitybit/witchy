//! How a stored 32-bit reference names its object's byte address in the 64-bit
//! linear memory (RFC-0179 R4).
//!
//! `Byte` references are byte addresses below 4 GiB: an access adds its
//! offset in i32 and widens the sum once. `Scaled8` references count 8-byte
//! units, reaching 32 GiB: an access widens and shifts the reference first,
//! then adds its widened i32 offset. Either way every address the engine sees
//! is a bounded sum (`uextend`, or `uextend << 3` plus `uextend`), so the
//! vendored range-based elision drops its bounds check, given a reservation
//! that covers the bound.
//!
//! A site that forms an address pushes the reference, calls `base`, pushes
//! each i32 offset followed by `add`, and ends with `finish`. Under `Byte` the
//! sequence is exactly the i32 sum and one widening it always was. The planner
//! derives the same tokens through the `plan_*` mirrors.
//!
//! No module selects `Scaled8` yet. The allocator, the drop worklist and the
//! dictionary, owner and closure kernels still form `Byte` addresses, and
//! several cache absolute interior addresses in i32 scratch locals, which
//! `Scaled8` must replace with object-relative offsets. The allocator's
//! globals are byte-valued too: the static-data boundary that retain and
//! clone compare a reference against (`global 2`) and the free-list head
//! that reclaim stores into an object (`global 1`) must count the same units
//! as references. The selection that maps a large-heap program to `Scaled8`
//! lands with that conversion.
//!
//! A `Scaled8` interior offset kept in a range slot is read back after the
//! slot is written, so the object a range moves or clones must not share a
//! local with the range's slots.

use super::plan::PlanToken;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RefEncoding {
    #[default]
    Byte,
    Scaled8,
}

impl RefEncoding {
    /// The encoding a program's heap mode selects.
    pub(crate) fn for_heap(heap: witchy_canonical_source::capabilities::HeapMode) -> Self {
        match heap {
            witchy_canonical_source::capabilities::HeapMode::Standard => Self::Byte,
            witchy_canonical_source::capabilities::HeapMode::Large => Self::Scaled8,
        }
    }

    /// Bits a reference is shifted by to form its byte address.
    pub(crate) fn shift(self) -> u32 {
        match self {
            Self::Byte => 0,
            Self::Scaled8 => 3,
        }
    }

    /// The reference that names byte address `address`, which must be a
    /// multiple of the unit.
    pub(crate) fn reference(self, address: u32) -> u32 {
        address >> self.shift()
    }

    /// An allocation of `bytes` rounded to whole units, so the next object's
    /// header is addressable by a reference.
    pub(crate) fn round_bytes(self, bytes: u32) -> Option<u32> {
        let mask = (1u32 << self.shift()) - 1;
        bytes.checked_add(mask).map(|bytes| bytes & !mask)
    }

    /// The largest allocation size the allocator accepts: one that rounds
    /// to whole units without leaving 32 bits.
    pub(crate) fn max_alloc_bytes(self) -> u32 {
        u32::MAX - ((1u32 << self.shift()) - 1)
    }

    /// The RC prefix (count and size words) in reference units.
    pub(crate) fn prefix_units(self) -> i32 {
        8 >> self.shift()
    }

    /// The exclusive limit of an object's end byte address: every byte of the
    /// heap must be addressable from a 32-bit reference.
    pub(crate) fn max_end(self) -> i64 {
        i64::from(u32::MAX) << self.shift()
    }

    /// After an i32 byte count that is a whole number of units: convert it
    /// to units.
    pub(crate) fn units(self, body: &mut wasm_encoder::Function) {
        if self == Self::Scaled8 {
            body.instruction(&wasm_encoder::Instruction::I32Const(3));
            body.instruction(&wasm_encoder::Instruction::I32ShrU);
        }
    }

    /// Mirror of `units`.
    pub(super) fn plan_units(self, plan: &mut Vec<PlanToken>) {
        if self == Self::Scaled8 {
            plan.extend([PlanToken::I32Const(3), PlanToken::I32ShrU]);
        }
    }

    /// After an i64 byte count: round it up to whole units.
    pub(crate) fn round_i64(self, body: &mut wasm_encoder::Function) {
        if self == Self::Scaled8 {
            body.instruction(&wasm_encoder::Instruction::I64Const(7));
            body.instruction(&wasm_encoder::Instruction::I64Add);
            body.instruction(&wasm_encoder::Instruction::I64Const(-8));
            body.instruction(&wasm_encoder::Instruction::I64And);
        }
    }

    /// Mirror of `round_i64`.
    pub(super) fn plan_round_i64(self, plan: &mut Vec<PlanToken>) {
        if self == Self::Scaled8 {
            plan.extend([PlanToken::I64Const(7), PlanToken::I64Add, PlanToken::I64Const(-8), PlanToken::I64And]);
        }
    }

    /// After the reference: start its address.
    pub(super) fn base(self, body: &mut wasm_encoder::Function) {
        if self == Self::Scaled8 {
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
            body.instruction(&wasm_encoder::Instruction::I64Const(3));
            body.instruction(&wasm_encoder::Instruction::I64Shl);
        }
    }

    /// After an i32 byte offset: add it to the address.
    pub(super) fn add(self, body: &mut wasm_encoder::Function) {
        match self {
            Self::Byte => body.instruction(&wasm_encoder::Instruction::I32Add),
            Self::Scaled8 => {
                body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
                body.instruction(&wasm_encoder::Instruction::I64Add)
            }
        };
    }

    /// After an i32 byte offset: subtract it from the address.
    pub(super) fn sub(self, body: &mut wasm_encoder::Function) {
        match self {
            Self::Byte => body.instruction(&wasm_encoder::Instruction::I32Sub),
            Self::Scaled8 => {
                body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
                body.instruction(&wasm_encoder::Instruction::I64Sub)
            }
        };
    }

    /// End the address: the memory index is on the stack.
    pub(super) fn finish(self, body: &mut wasm_encoder::Function) {
        if self == Self::Byte {
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        }
    }

    /// The address of the reference on the stack, with no offset.
    pub(super) fn object(self, body: &mut wasm_encoder::Function) {
        self.base(body);
        self.finish(body);
    }

    /// Mirror of `base`.
    pub(super) fn plan_base(self, plan: &mut Vec<PlanToken>) {
        if self == Self::Scaled8 {
            plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Const(3), PlanToken::I64Shl]);
        }
    }

    /// Mirror of `add`.
    pub(super) fn plan_add(self, plan: &mut Vec<PlanToken>) {
        match self {
            Self::Byte => plan.push(PlanToken::I32Add),
            Self::Scaled8 => plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Add]),
        }
    }

    /// Mirror of `sub`.
    pub(super) fn plan_sub(self, plan: &mut Vec<PlanToken>) {
        match self {
            Self::Byte => plan.push(PlanToken::I32Sub),
            Self::Scaled8 => plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Sub]),
        }
    }

    /// Mirror of `finish`.
    pub(super) fn plan_finish(self, plan: &mut Vec<PlanToken>) {
        if self == Self::Byte {
            plan.push(PlanToken::I64ExtendI32U);
        }
    }

    /// Mirror of `object`.
    pub(super) fn plan_object(self, plan: &mut Vec<PlanToken>) {
        self.plan_base(plan);
        self.plan_finish(plan);
    }
}
