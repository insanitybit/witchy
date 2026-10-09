//! Values a function proves static: references defined by `StaticData` and
//! the SSA copies made of them.
//!
//! The retain and release helpers compare every address with the heap base
//! and return early for static data, so a retain, release or shared store of
//! a value this set names is a call that does nothing. The encoder and its
//! independent plan both consult the set and emit the plain local copy (or
//! nothing) instead; the checker's judgment is unchanged, since it already
//! treats these operations on a static object as no-op token transfers.
//!
//! The set is SSA-local: a block parameter is never static, even when every
//! incoming argument is, because its lanes may be heap objects after a join
//! and the helpers must then run.

use std::collections::BTreeSet;

use crate::canonical_safewir::{Function, Operation, ValueId};

pub(super) fn static_values(function: &Function) -> BTreeSet<ValueId> {
    let mut values = BTreeSet::new();
    // A copy's source is defined in a dominating block, which may be visited
    // later in body order; iterate to a fixpoint. Copies of copies are rare,
    // so this is one or two passes in practice.
    loop {
        let before = values.len();
        for block in &function.blocks {
            for operation in &block.operations {
                match operation {
                    Operation::StaticData { destination, .. } => {
                        values.insert(*destination);
                    }
                    Operation::CloneRef { destination, source } | Operation::MoveValue { destination, source }
                        if values.contains(source) =>
                    {
                        values.insert(*destination);
                    }
                    _ => {}
                }
            }
        }
        if values.len() == before {
            return values;
        }
    }
}
