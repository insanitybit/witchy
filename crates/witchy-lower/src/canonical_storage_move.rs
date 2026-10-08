//! Storage that copies a dying buffer moves it instead.
//!
//! A string entering an owned slot is copied, because the source binding
//! may still be read (value semantics). Where CFG liveness shows the source
//! token dies at that very copy, nothing can observe the difference: the
//! fresh allocation, byte copy and count update become one `MoveValue`, and
//! release placement no longer drops the source. This is untrusted lowering;
//! the checker still has to accept the moved token as the slot's owner.

use std::collections::BTreeSet;
use witchy_wir::canonical_safewir::{Function, Operation, Ownership, ValueId};

use super::owned::{live_tail, liveness, uses, References};

/// Rewrite each recorded `(copy, source)` whose source is a fresh unique
/// token that dies at the copy.
pub(super) fn forward_dying_copies(function: &mut Function, copies: &[(ValueId, ValueId)]) {
    let unique = fresh_unique_values(function);
    let candidates = copies.iter().copied()
        .filter(|(_, source)| unique.contains(source))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return;
    }
    let references = References::of(function);
    let live_in = liveness(function, &references);
    for block in &mut function.blocks {
        for &(copy, source) in &candidates {
            let Some(at) = block.operations.iter().position(|operation| matches!(operation,
                Operation::CopyBytes { destination, source: from, .. }
                    if destination.object() == copy && from.object() == source))
            else { continue };
            // SSA: the source is live after the copy only through a later
            // use in this block or the block's live-out set.
            if block.operations[at + 1..].iter().any(|operation| uses(operation).contains(&source))
                || live_tail(&references, block, &live_in).contains(&source)
            {
                continue;
            }
            let Some(alloc) = block.operations[..at].iter().position(|operation|
                matches!(operation, Operation::Alloc { destination, .. } if *destination == copy))
            else { continue };
            let Some(count) = block.operations[at + 1..].iter().position(|operation|
                matches!(operation, Operation::UpdateCount { object, .. } if *object == copy))
                .map(|offset| at + 1 + offset)
            else { continue };
            // Only the copy may touch the fresh buffer before its count.
            if block.operations[alloc + 1..count].iter().enumerate()
                .any(|(offset, operation)| alloc + 1 + offset != at && uses(operation).contains(&copy))
            {
                continue;
            }
            block.operations[at] = Operation::MoveValue { destination: copy, source };
            block.operations.remove(count);
            block.operations.remove(alloc);
        }
    }
}

/// Reference values this function creates and never aliases: allocations,
/// call results, and unique block parameters other than the entry's. A
/// cloned, loaded or static value is excluded. A call result may still be a
/// shared token; a copy is recorded only from a sole token (`sole_token`),
/// which excludes a call result whose contract is not unique.
fn fresh_unique_values(function: &Function) -> BTreeSet<ValueId> {
    let mut fresh = BTreeSet::new();
    let mut aliased = BTreeSet::new();
    for block in &function.blocks {
        if block.id != function.entry {
            fresh.extend(block.parameters.iter()
                .filter(|parameter| parameter.ownership == Some(Ownership::Unique))
                .map(|parameter| parameter.value));
        }
        for operation in &block.operations {
            match operation {
                Operation::Alloc { destination, .. } => { fresh.insert(*destination); }
                Operation::Call { destinations, .. } | Operation::VarCall { destinations, .. } => {
                    fresh.extend(destinations.iter().copied());
                }
                Operation::CloneRef { source, .. } | Operation::MoveValue { source, .. } => {
                    aliased.insert(*source);
                }
                _ => {}
            }
        }
    }
    fresh.retain(|value| !aliased.contains(value));
    fresh
}
