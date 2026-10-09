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
