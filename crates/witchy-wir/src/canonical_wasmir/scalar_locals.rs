//! Wasm storage assignment for nonescaping scalar SSA temporaries. Source
//! values, operations and checked facts are unchanged. Intervals are closed:
//! an instruction's inputs cannot share storage with its result, even when
//! that instruction is the input's last use (kernels can read inputs late).
use super::*;

type UseSite = Option<(crate::canonical_safewir::BlockId, usize)>;

fn note_use(uses: &mut BTreeMap<ValueId, UseSite>, value: ValueId,
    block: crate::canonical_safewir::BlockId, position: usize, pinned: bool) {
    use std::collections::btree_map::Entry;
    match uses.entry(value) {
        Entry::Vacant(entry) => { entry.insert((!pinned).then_some((block, position))); }
        Entry::Occupied(mut entry) => {
            if let Some((previous, last)) = *entry.get() {
                entry.insert((!pinned && previous == block).then_some((block, last.max(position))));
            }
        }
    }
}

fn scalar_destination(operation: &Operation) -> Option<ValueId> {
    // Only these single-result operations participate. In particular, owned,
    // capability, callable, aggregate and multi-result ABI lanes never do.
    match operation {
        Operation::ConstInt { destination, .. }
        | Operation::ConstFloat { destination, .. }
        | Operation::ConstBool { destination, .. }
        | Operation::ConstUnit { destination }
        | Operation::ConstSize { destination, .. }
        | Operation::Arithmetic { destination, .. }
        | Operation::Compare { destination, .. }
        | Operation::NarrowSize { destination, .. }
        | Operation::WidenSize { destination, .. }
        | Operation::FloatUnary { destination, .. }
        | Operation::Load { destination, .. }
        | Operation::MoveValue { destination, .. } => Some(*destination),
        _ => None,
    }
}

fn scalar_kind(value: ValueType) -> Option<u8> {
    match value {
        ValueType::Bool | ValueType::Unit | ValueType::Size => Some(0),
        ValueType::Int => Some(1),
        ValueType::Float => Some(2),
        _ => None,
    }
}

fn runtime_uses(operation: &Operation) -> Vec<ValueId> {
    let mut operands = crate::canonical_safewir::operation_uses(operation);
    // Backend read inventory beyond operation_uses:
    // - Alloc reads Counted capacity for sizing and count/capacity for headers.
    // - ExtractInlineField reads the original MakeInlineAggregate field local,
    //   not just its aggregate operand. All constructor fields are pinned below.
    // - Load CloneShared/ClonePresent rereads its own result; those results are
    //   references and cannot enter a scalar pool. TakeLast's index is already
    //   a place operand. Store CloneShared rereads its enumerated value.
    // - Owner kernels enumerate captures/index/value through owner.operands();
    //   synthetic ENV/TEMP and cleanup worklists use reserved scratch locals.
    // Place indices, slice starts/lengths, range counts, call/host operands and
    // branch/tail operands are covered by the ordinary visitors. Static data,
    // fixed layout offsets, callable table indices and host minimum byte counts
    // are constants, not hidden source ValueId reads.
    if let Operation::Alloc { extent: Extent::Counted { count, capacity, .. }, .. } = operation {
        operands.extend([*count, *capacity]);
    }
    operands
}

pub(super) fn slots(function: &Function) -> BTreeMap<ValueId, (u8, usize)> {
    let mut uses = BTreeMap::new();
    for block in &function.blocks {
        for (position, operation) in block.operations.iter().enumerate() {
            let operands = runtime_uses(operation);
            // Inline aggregates are deferred aliases, not copies into Wasm
            // storage. Their fields must survive later extraction/call use.
            let pinned = matches!(operation, Operation::MakeInlineAggregate { .. });
            for value in operands { note_use(&mut uses, value, block.id, position, pinned); }
        }
        for value in crate::canonical_safewir::terminator_uses(&block.terminator) {
            note_use(&mut uses, value, block.id, block.operations.len(), true);
        }
    }
    let mut result = BTreeMap::new();
    let mut pool_sizes = [0; 3];
    for block in &function.blocks {
        let mut intervals: [Vec<(usize, usize, ValueId)>; 3] = Default::default();
        for (position, operation) in block.operations.iter().enumerate() {
            let Some(value) = scalar_destination(operation) else { continue; };
            let Some(kind) = function.value_types.get(&value).copied().and_then(scalar_kind) else { continue; };
            let Some(Some((use_block, last))) = uses.get(&value) else { continue; };
            if *use_block == block.id && *last >= position {
                intervals[kind as usize].push((position, *last, value));
            }
        }
        for (kind, intervals) in intervals.into_iter().enumerate() {
            let mut free: BTreeSet<usize> = (0..pool_sizes[kind]).collect();
            let mut active: BTreeMap<(usize, ValueId), usize> = BTreeMap::new();
            for (begin, end, value) in intervals {
                while active.first_key_value().is_some_and(|((last, _), _)| *last < begin) {
                    let (_, slot) = active.pop_first().expect("active interval exists");
                    free.insert(slot);
                }
                let slot = free.pop_first().unwrap_or_else(|| {
                    let slot = pool_sizes[kind];
                    pool_sizes[kind] += 1;
                    slot
                });
                active.insert((end, value), slot);
                result.insert(value, (kind as u8, slot));
            }
        }
    }
    result
}

// Enumerate every definition, including definitions the allocator cannot pool.
// A forged map must not hide a second definition behind an ineligible opcode.
fn definitions(operation: &Operation) -> Vec<ValueId> {
    match operation {
        Operation::ClosureOwner(owner) => owner.destinations(),
        Operation::Dictionary(operation) => operation.destinations(),
        Operation::ConstInt { destination, .. } | Operation::ConstFloat { destination, .. }
        | Operation::ConstBool { destination, .. } | Operation::ConstUnit { destination }
        | Operation::ConstSize { destination, .. } | Operation::CapabilityNone { destination, .. }
        | Operation::CapabilitySome { destination, .. } | Operation::CapabilityIsPresent { destination, .. }
        | Operation::CapabilityUnwrap { destination, .. } | Operation::Arithmetic { destination, .. }
        | Operation::Compare { destination, .. } | Operation::NarrowSize { destination, .. }
        | Operation::WidenSize { destination, .. } | Operation::FloatUnary { destination, .. }
        | Operation::Alloc { destination, .. } | Operation::StaticData { destination, .. }
        | Operation::Load { destination, .. } | Operation::MoveValue { destination, .. }
        | Operation::MakeInlineAggregate { destination, .. } | Operation::ExtractInlineField { destination, .. }
        | Operation::FunctionValue { destination, .. } | Operation::CloneRef { destination, .. } => vec![*destination],
        Operation::Call { destinations, .. } | Operation::VarCall { destinations, .. }
        | Operation::ExternalCall { destinations, .. }
        | Operation::HostCall { destinations, .. } => destinations.clone(),
        Operation::Guard { .. } | Operation::InitNullableOwned { .. } | Operation::Store { .. }
        | Operation::CopyBytes { .. } | Operation::MoveRange { .. } | Operation::CloneRange { .. }
        | Operation::UpdateCount { .. } | Operation::Retain { .. } | Operation::Release { .. }
        | Operation::DropRef { .. } | Operation::Reclaim { .. } | Operation::ReauthenticateRawHeaderExtent { .. } => Vec::new(),
    }
}

/// Verify a candidate storage map, without calling the allocator, its use-site
/// accumulator, or its free/active interval machinery. Both encoder and plan
/// must pass this check: agreeing on an invalid allocation is not evidence.
pub(super) fn verify(function: &Function, assigned: &BTreeMap<ValueId, (u8, usize)>) -> Result<(), ()> {
    for (value, (kind, _)) in assigned {
        let physical = match function.value_types.get(value) {
            Some(ValueType::Bool | ValueType::Unit | ValueType::Size) => 0,
            Some(ValueType::Int) => 1,
            Some(ValueType::Float) => 2,
            _ => return Err(()),
        };
        if *kind != physical { return Err(()); }
    }
    let mut sites = BTreeMap::new();
    let mut blocks = BTreeSet::new();
    for block in &function.blocks {
        if !blocks.insert(block.id) { return Err(()); }
        if block.parameters.iter().any(|parameter| assigned.contains_key(&parameter.value)) { return Err(()); }
        for (position, operation) in block.operations.iter().enumerate() {
            for value in definitions(operation) {
                if !assigned.contains_key(&value) { continue; }
                if scalar_destination(operation) != Some(value)
                    || sites.insert(value, (block.id, position)).is_some() { return Err(()); }
            }
        }
    }
    if sites.len() != assigned.len() { return Err(()); }
    let mut ends = BTreeMap::new();
    for block in &function.blocks {
        for (position, operation) in block.operations.iter().enumerate() {
            for value in runtime_uses(operation) {
                let Some((definition_block, begin)) = sites.get(&value) else { continue; };
                if *definition_block != block.id || position <= *begin
                    || matches!(operation, Operation::MakeInlineAggregate { .. }) { return Err(()); }
                ends.insert(value, position);
            }
        }
        if crate::canonical_safewir::terminator_uses(&block.terminator).iter()
            .any(|value| assigned.contains_key(value)) { return Err(()); }
    }
    let mut by_slot: BTreeMap<_, Vec<(usize, usize)>> = BTreeMap::new();
    for (value, (kind, slot)) in assigned {
        let (block, begin) = sites[value];
        let end = *ends.get(value).ok_or(())?; // Unused scalars remain pinned.
        by_slot.entry((block, *kind, *slot)).or_default().push((begin, end));
    }
    for intervals in by_slot.values_mut() {
        intervals.sort_unstable();
        // Closed intervals: a result must not overwrite any input used by
        // the same expanded operation, even if that was the input's last use.
        if intervals.windows(2).any(|pair| pair[0].1 >= pair[1].0) { return Err(()); }
    }
    Ok(())
}
