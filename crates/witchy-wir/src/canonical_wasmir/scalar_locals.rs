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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_safewir::{AllocationClass, BlockId, FunctionSignatureId};

    fn function(values: &[(ValueId, ValueType)], operations: Vec<Operation>, results: Vec<ValueId>) -> Function {
        Function { id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: BlockId::new(0),
            value_types: values.iter().copied().collect(),
            blocks: vec![BasicBlock { id: BlockId::new(0), parameters: Vec::new(), operations, terminator: Terminator::Return { values: results } }],
        }
    }

    #[test]
    fn scalar_intervals_are_closed_and_terminator_values_are_pinned() {
        let [a, b, c] = [ValueId::new(0), ValueId::new(1), ValueId::new(2)];
        let function = function(&[(a, ValueType::Int), (b, ValueType::Int), (c, ValueType::Int)], vec![
            Operation::ConstInt { destination: a, value: 1 },
            Operation::MoveValue { destination: b, source: a },
            Operation::MoveValue { destination: c, source: b },
        ], vec![c]);
        let assigned = slots(&function);
        assert_eq!(verify(&function, &assigned), Ok(()));
        assert_ne!(assigned[&a], assigned[&b], "last use and definition in one instruction overlap");
        assert!(!assigned.contains_key(&c));
    }

    #[test]
    fn scalar_allocation_extent_operands_extend_lifetimes() {
        let [count, first, later, second, object] = [0, 1, 2, 3, 4].map(ValueId::new);
        let mut layouts = LayoutInterner::new();
        let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let list = layouts.intern_packed_list(int, crate::layout::RcHeader::Required).unwrap();
        let function = function(&[(count, ValueType::Size), (first, ValueType::Size), (later, ValueType::Size), (second, ValueType::Size), (object, ValueType::Ref(list))], vec![
            Operation::ConstSize { destination: count, value: 1 },
            Operation::MoveValue { destination: first, source: count },
            Operation::ConstSize { destination: later, value: 9 },
            Operation::MoveValue { destination: second, source: later },
            Operation::Alloc { destination: object, layout: list, extent: Extent::Counted { header_bytes: 8, count, capacity: count, stride_bytes: 8 }, class: AllocationClass::ReferenceCounted },
            Operation::DropRef { object },
        ], vec![first, second]);
        let assigned = slots(&function);
        assert_eq!(verify(&function, &assigned), Ok(()));
        assert_ne!(assigned[&count], assigned[&later], "Alloc still reads count/capacity after the earlier ordinary use");
        assert!(!assigned.contains_key(&object));
        assert!(verify(&function, &[(count, (0, 0)), (later, (0, 0))].into()).is_err(), "Counted metadata is a late runtime use");
    }

    #[test]
    fn scalar_cross_block_uses_are_not_recycled() {
        let [a, b] = [ValueId::new(0), ValueId::new(1)];
        let mut function = function(&[(a, ValueType::Int), (b, ValueType::Int)], vec![Operation::ConstInt { destination: a, value: 41 }], Vec::new());
        function.blocks[0].terminator = Terminator::Branch { target: BlockId::new(1), arguments: Vec::new() };
        function.blocks.push(BasicBlock { id: BlockId::new(1), parameters: Vec::new(), operations: vec![Operation::MoveValue { destination: b, source: a }], terminator: Terminator::Return { values: vec![b] } });
        assert!(slots(&function).is_empty());
        assert!(verify(&function, &[(a, (1, 0))].into()).is_err());
    }

    #[test]
    fn scalar_slot_verifier_rejects_forged_overlap_kind_and_terminator_maps() {
        let [a, b, c] = [0, 1, 2].map(ValueId::new);
        let function = function(&[(a, ValueType::Int), (b, ValueType::Int), (c, ValueType::Int)], vec![
            Operation::ConstInt { destination: a, value: 7 },
            Operation::MoveValue { destination: b, source: a },
            Operation::MoveValue { destination: c, source: b },
        ], vec![c]);
        assert_eq!(verify(&function, &[(a, (1, 0)), (b, (1, 1))].into()), Ok(()));
        assert!(verify(&function, &[(a, (1, 0)), (b, (1, 0))].into()).is_err());
        assert!(verify(&function, &[(a, (0, 0))].into()).is_err());
        assert!(verify(&function, &[(c, (1, 0))].into()).is_err());
        assert!(verify(&function, &[(ValueId::new(99), (1, 0))].into()).is_err());
    }

    #[test]
    fn scalar_slot_verifier_rejects_parameters_duplicate_and_ineligible_definitions() {
        let [a, b] = [0, 1].map(ValueId::new);
        let values = [(a, ValueType::Int), (b, ValueType::Int)];
        let assigned = [(a, (1, 0))].into();
        let mut parameter = function(&values, vec![Operation::MoveValue { destination: b, source: a }], vec![b]);
        parameter.blocks[0].parameters.push(crate::canonical_safewir::BlockParameter { value: a, value_type: ValueType::Int, ownership: None });
        assert!(verify(&parameter, &assigned).is_err());
        let duplicate = function(&values, vec![Operation::ConstInt { destination: a, value: 1 },
            Operation::ConstInt { destination: a, value: 2 }, Operation::MoveValue { destination: b, source: a }], vec![b]);
        assert!(verify(&duplicate, &assigned).is_err());
        let call = function(&values, vec![Operation::Call { destinations: vec![a], callee: FunctionId::new(1), arguments: Vec::new() },
            Operation::MoveValue { destination: b, source: a }], vec![b]);
        assert!(verify(&call, &assigned).is_err());
        let unused = function(&values, vec![Operation::ConstInt { destination: a, value: 1 }], Vec::new());
        assert!(verify(&unused, &assigned).is_err());
    }

    #[test]
    fn scalar_slot_verifier_rejects_deferred_aggregate_fields_and_pointer_kinds() {
        let [a, b, aggregate] = [0, 1, 2].map(ValueId::new);
        let mut layouts = LayoutInterner::new();
        let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let function = function(&[(a, ValueType::Int), (b, ValueType::Int), (aggregate, ValueType::InlineAggregate(int))], vec![
            Operation::ConstInt { destination: a, value: 42 },
            Operation::MakeInlineAggregate { destination: aggregate, layout: int, fields: vec![a] },
            Operation::MoveValue { destination: b, source: a },
        ], vec![b]);
        assert!(verify(&function, &[(a, (1, 0))].into()).is_err());
        // White-box forged storage inputs, not fabricated checked modules.
        let mut pointer = function;
        pointer.value_types.insert(a, ValueType::Ref(int));
        assert!(verify(&pointer, &[(a, (0, 0))].into()).is_err());
    }
}
