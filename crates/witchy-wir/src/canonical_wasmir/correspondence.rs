use super::*;

pub(super) const STATIC_DATA_BASE: u32 = 16;
pub(super) const WASM_PAGE_BYTES: u64 = 65_536;

pub(super) fn initial_memory_pages(heap_start: u32) -> u64 {
    (u64::from(heap_start).saturating_add(WASM_PAGE_BYTES - 1) / WASM_PAGE_BYTES).max(1)
}

pub(super) fn static_data_plan(
    segments: &[crate::canonical_safewir::StaticDataSegment],
) -> Result<(Vec<WasmStaticData>, u32), LoweringError> {
    let mut ordered = segments.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|segment| segment.id());
    let mut offset = STATIC_DATA_BASE;
    let mut plan = Vec::with_capacity(ordered.len());
    for segment in ordered {
        let bytes = u32::try_from(segment.bytes().len()).map_err(|_| LoweringError::StaticDataAddressOverflow)?;
        let end = offset.checked_add(bytes).ok_or(LoweringError::StaticDataAddressOverflow)?;
        plan.push(WasmStaticData { id: segment.id(), offset, bytes: segment.bytes().to_vec() });
        offset = end;
    }
    let heap_start = offset.checked_add(7).map(|value| value & !7).ok_or(LoweringError::StaticDataAddressOverflow)?;
    Ok((plan, heap_start))
}

/// The reviewed SafeWIR-to-WasmIR mapping, keyed by canonical rule: every
/// rule is either mapped under one correspondence rule or refused by the
/// backend (`None`). `canonical_safewir::manifest` cites this table row by
/// row, so a new rule cannot reach the backend without a manifest entry.
pub(crate) fn rule_backend(rule: crate::canonical_safewir::CanonicalRuleId) -> RuleBackend {
    use crate::canonical_safewir::CanonicalRuleId as Rule;
    match rule {
        Rule::ConstUnit | Rule::ConstInt | Rule::ConstFloat | Rule::ConstBool | Rule::ConstSize | Rule::FunctionValue => RuleBackend::Receipt(CorrespondenceRule::ScalarConstant),
        Rule::CapabilityNone | Rule::CapabilitySome | Rule::CapabilityIsPresent | Rule::CapabilityUnwrap => RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        Rule::Arithmetic => RuleBackend::Receipt(CorrespondenceRule::ScalarArithmetic),
        Rule::Compare => RuleBackend::Receipt(CorrespondenceRule::ScalarComparison),
        Rule::NarrowSize | Rule::WidenSize | Rule::FloatUnary => RuleBackend::Receipt(CorrespondenceRule::ScalarConversion),
        Rule::Guard => RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        Rule::ReauthenticateRawHeaderExtent => RuleBackend::Receipt(CorrespondenceRule::ScalarGuard),
        Rule::MoveValue | Rule::MakeInlineAggregate | Rule::ExtractInlineField => RuleBackend::Receipt(CorrespondenceRule::ScalarMove),
        Rule::Alloc => RuleBackend::Receipt(CorrespondenceRule::MemoryAlloc),
        Rule::StaticData => RuleBackend::Receipt(CorrespondenceRule::MemoryStaticData),
        Rule::InitNullableOwned => RuleBackend::Receipt(CorrespondenceRule::MemoryStore),
        Rule::Load => RuleBackend::Receipt(CorrespondenceRule::MemoryLoad),
        Rule::ClosureSlot => RuleBackend::Subjudgment(&[Rule::Load, Rule::Store]),
        Rule::Store => RuleBackend::Receipt(CorrespondenceRule::MemoryStore),
        Rule::CopyBytes | Rule::CloneRange => RuleBackend::Receipt(CorrespondenceRule::MemoryCopy),
        Rule::MoveRange | Rule::CloneRef | Rule::Release | Rule::DropRef | Rule::Reclaim => RuleBackend::Receipt(CorrespondenceRule::MemoryOwnership),
        Rule::UpdateCount => RuleBackend::Receipt(CorrespondenceRule::MemoryUpdateCount),
        Rule::Call | Rule::VarCall => RuleBackend::Receipt(CorrespondenceRule::ScalarCall),
        Rule::ExternalCall => RuleBackend::Receipt(CorrespondenceRule::ExternalCall),
        Rule::HostCall => RuleBackend::Receipt(CorrespondenceRule::HostCall),
        // v1 has no standalone retain: a second owner comes from `CloneRef`.
        Rule::Retain => RuleBackend::Refused,
        Rule::ClosureOwner => RuleBackend::Receipt(CorrespondenceRule::ClosureOwner),
        Rule::Branch | Rule::BranchIf | Rule::Return | Rule::TailCall | Rule::VarTailCall | Rule::Trap | Rule::Unreachable => {
            RuleBackend::Receipt(CorrespondenceRule::Control)
        }
    }
}

pub(super) fn operation_rule(operation: &Operation) -> Option<CorrespondenceRule> {
    match rule_backend(operation.rule_id()) {
        RuleBackend::Receipt(rule) => Some(rule),
        RuleBackend::Subjudgment(_) | RuleBackend::Refused => None,
    }
}

pub(super) fn matches_operation(operation: &Operation, target: &ScalarInstruction) -> bool {
    match (operation, target) {
        (Operation::ReauthenticateRawHeaderExtent { .. }, ScalarInstruction::ProofNoop) => true,
        (Operation::ClosureOwner(source), ScalarInstruction::ClosureOwner(target)) => source == target,
        (Operation::ConstUnit { destination }, ScalarInstruction::UnitConst { destination: actual }) => destination == actual,
        (Operation::ConstInt { destination, value }, ScalarInstruction::IntConst { destination: actual, value: actual_value }) => destination == actual && value == actual_value,
        (Operation::ConstFloat { destination, bits }, ScalarInstruction::FloatConst { destination: actual, bits: actual_bits }) => destination == actual && bits == actual_bits,
        (Operation::ConstBool { destination, value }, ScalarInstruction::BoolConst { destination: actual, value: actual_value }) => destination == actual && value == actual_value,
        (Operation::ConstSize { destination, value }, ScalarInstruction::SizeConst { destination: actual, value: actual_value }) => destination == actual && value == actual_value,
        (Operation::CapabilityNone { destination, .. }, ScalarInstruction::CapabilityNone { destination: actual }) => destination == actual,
        (Operation::CapabilitySome { destination, source }, ScalarInstruction::CapabilitySome { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::CapabilityIsPresent { destination, source }, ScalarInstruction::CapabilityIsPresent { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::CapabilityUnwrap { destination, source }, ScalarInstruction::CapabilityUnwrap { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::Arithmetic { destination, operator, left, right, overflow }, ScalarInstruction::Arithmetic { destination: actual_destination, operator: actual_operator, left: actual_left, right: actual_right, overflow: actual_overflow }) => destination == actual_destination && operator == actual_operator && left == actual_left && right == actual_right && overflow == actual_overflow,
        (Operation::Compare { destination, operator, left, right }, ScalarInstruction::Compare { destination: actual_destination, operator: actual_operator, left: actual_left, right: actual_right }) => destination == actual_destination && operator == actual_operator && left == actual_left && right == actual_right,
        (Operation::NarrowSize { destination, value }, ScalarInstruction::NarrowSize { destination: actual_destination, value: actual_value }) => destination == actual_destination && value == actual_value,
        (Operation::WidenSize { destination, value }, ScalarInstruction::WidenSize { destination: actual_destination, value: actual_value }) => destination == actual_destination && value == actual_value,
        (Operation::FloatUnary { destination, operator, value }, ScalarInstruction::FloatUnary { destination: actual_destination, operator: actual_operator, value: actual_value }) => destination == actual_destination && operator == actual_operator && value == actual_value,
        (Operation::Guard { condition, kind }, ScalarInstruction::Guard { condition: actual_condition, kind: actual_kind }) => condition == actual_condition && kind == actual_kind,
        (Operation::MoveValue { destination, source }, ScalarInstruction::MoveValue { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::MakeInlineAggregate { destination, layout, fields }, ScalarInstruction::MakeInlineAggregate { destination: actual_destination, layout: actual_layout, fields: actual_fields }) => destination == actual_destination && layout == actual_layout && fields == actual_fields,
        (Operation::ExtractInlineField { destination, aggregate, index }, ScalarInstruction::ExtractInlineField { destination: actual_destination, aggregate: actual_aggregate, index: actual_index, .. }) => destination == actual_destination && aggregate == actual_aggregate && index == actual_index,
        (Operation::FunctionValue { destination, function }, ScalarInstruction::FunctionValue { destination: actual_destination, function: actual_function, .. }) => destination == actual_destination && function == actual_function,
        (Operation::Alloc { destination, layout, extent, class }, ScalarInstruction::Alloc { destination: actual_destination, layout: actual_layout, extent: actual_extent, class: actual_class }) => destination == actual_destination && layout == actual_layout && extent == actual_extent && class == actual_class,
        (Operation::StaticData { destination, data, .. }, ScalarInstruction::StaticData { destination: actual_destination, data: actual_data }) => destination == actual_destination && data == actual_data,
        (Operation::InitNullableOwned { place }, ScalarInstruction::InitNullableOwned { place: actual_place }) => place == actual_place,
        (Operation::Load { destination, place, transfer }, ScalarInstruction::Load { destination: actual_destination, place: actual_place, transfer: actual_transfer }) => destination == actual_destination && place == actual_place && transfer == actual_transfer,
        (Operation::Store { place, value, transfer }, ScalarInstruction::Store { place: actual_place, value: actual_value, transfer: actual_transfer }) => place == actual_place && value == actual_value && transfer == actual_transfer,
        (Operation::CopyBytes { destination, source, length }, ScalarInstruction::CopyBytes { destination: actual_destination, source: actual_source, length: actual_length }) => destination == actual_destination && source == actual_source && length == actual_length,
        (Operation::MoveRange { destination, source, count }, ScalarInstruction::MoveRange { destination: actual_destination, source: actual_source, count: actual_count }) => destination == actual_destination && source == actual_source && count == actual_count,
        (Operation::CloneRange { destination, source, count }, ScalarInstruction::CloneRange { destination: actual_destination, source: actual_source, count: actual_count }) => destination == actual_destination && source == actual_source && count == actual_count,
        (Operation::UpdateCount { object, count }, ScalarInstruction::UpdateCount { object: actual_object, count: actual_count }) => object == actual_object && count == actual_count,
        (Operation::CloneRef { destination, source }, ScalarInstruction::CloneRef { destination: actual_destination, source: actual_source }) => destination == actual_destination && source == actual_source,
        (Operation::Release { object }, ScalarInstruction::Release { object: actual_object })
        | (Operation::DropRef { object }, ScalarInstruction::Release { object: actual_object })
        | (Operation::Reclaim { object }, ScalarInstruction::Reclaim { object: actual_object }) => object == actual_object,
        (Operation::Call { destinations, callee, arguments }, ScalarInstruction::Call { destinations: actual_destinations, callee: actual_callee, arguments: actual_arguments }) => *callee == *actual_callee && destinations == actual_destinations && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        (Operation::ExternalCall { destinations, import, arguments }, ScalarInstruction::ExternalCall { destinations: actual_destinations, import: actual_import, arguments: actual_arguments }) => import == actual_import && destinations == actual_destinations && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        (Operation::HostCall { destinations, import, arguments }, ScalarInstruction::HostCall { destinations: actual_destinations, import: actual_import, arguments: actual_arguments }) => import == actual_import && destinations == actual_destinations && arguments == actual_arguments,
        (Operation::VarCall { destinations, callee, signature, arguments }, ScalarInstruction::VarCall { destinations: actual_destinations, callee: actual_callee, signature: actual_signature, arguments: actual_arguments }) => *callee == *actual_callee && *signature == *actual_signature && destinations == actual_destinations && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        _ => false,
    }
}

pub(super) fn scalar_argument_values(arguments: &[CallArgument]) -> Option<Vec<ValueId>> {
    arguments.iter().map(|argument| match argument {
        CallArgument::Value(value) | CallArgument::Move(value) => Some(*value),
    }).collect()
}

pub(super) fn matches_terminator(terminator: &Terminator, target: &ScalarTerminator) -> bool {
    match (terminator, target) {
        (Terminator::Branch { target, arguments }, ScalarTerminator::Branch { target: actual_target, arguments: actual_arguments }) => target == actual_target && arguments == actual_arguments,
        (Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments }, ScalarTerminator::BranchIf { condition: actual_condition, then_target: actual_then_target, then_arguments: actual_then_arguments, else_target: actual_else_target, else_arguments: actual_else_arguments }) => condition == actual_condition && then_target == actual_then_target && then_arguments == actual_then_arguments && else_target == actual_else_target && else_arguments == actual_else_arguments,
        (Terminator::Return { values }, ScalarTerminator::Return { values: actual_values }) => values == actual_values,
        (Terminator::TailCall { site, callee, arguments }, ScalarTerminator::TailCall { site: actual_site, callee: actual_callee, arguments: actual_arguments }) => site == actual_site && callee == actual_callee && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        (Terminator::VarTailCall { site, callee, signature, arguments }, ScalarTerminator::VarTailCall { site: actual_site, callee: actual_callee, signature: actual_signature, arguments: actual_arguments }) => site == actual_site && callee == actual_callee && signature == actual_signature && scalar_argument_values(arguments) == Some(actual_arguments.clone()),
        (Terminator::Trap, ScalarTerminator::Trap) | (Terminator::Unreachable, ScalarTerminator::Unreachable) => true,
        _ => false,
    }
}

pub(super) fn expected_source_items<S: ModuleStage>(module: &CanonicalModule<S>) -> BTreeSet<SourceItemId> {
    module
        .functions()
        .iter()
        .flat_map(|function| function.blocks.iter().flat_map(move |block| {
            block
                .operations
                .iter()
                .enumerate()
                .map(move |(index, _)| SourceItemId::Operation { function: function.id, block: block.id, index })
                .chain(std::iter::once(SourceItemId::Terminator { function: function.id, block: block.id }))
        }))
        .collect()
}

/// Independently check a target against source structure and mint its receipt.
/// This matcher does not call the lowering functions above: it validates the
/// source claim, position, semantic payload, and ownership rule directly.
pub(super) fn check_correspondence(
    source: &CanonicalModule<InitialValid>,
    target: &WasmIr,
) -> Result<CorrespondenceReceipt, CorrespondenceError> {
    if owner_bridges::build(source).map_err(|_| CorrespondenceError::ModuleIdentityMismatch)? != target.owner_bridges {
        return Err(CorrespondenceError::ModuleIdentityMismatch);
    }
    if source.revision() != target.revision || source.checked_source() != target.checked_source {
        return Err(CorrespondenceError::ModuleIdentityMismatch);
    }

    let source_functions: BTreeMap<_, _> = source.functions().iter().map(|function| (function.id, function)).collect();
    let target_functions: BTreeMap<_, _> = target.functions.iter().map(|function| (function.id, function)).collect();
    for id in source_functions.keys() {
        if !target_functions.contains_key(id) {
            return Err(CorrespondenceError::MissingFunction(*id));
        }
    }
    for id in target_functions.keys() {
        if !source_functions.contains_key(id) {
            return Err(CorrespondenceError::UnexpectedFunction(*id));
        }
    }

    let expected = expected_source_items(source);
    let mut claimed = BTreeSet::new();
    for function in &target.functions {
        let source_function = source_functions[&function.id];
        if function.signature != source_function.signature || function.entry != source_function.entry || function.blocks.len() != source_function.blocks.len() {
            return Err(CorrespondenceError::FunctionShapeMismatch(function.id));
        }
        let source_blocks: BTreeMap<_, _> = source_function.blocks.iter().map(|block| (block.id, block)).collect();
        let target_blocks: BTreeMap<_, _> = function.blocks.iter().map(|block| (block.id, block)).collect();
        for id in source_blocks.keys() {
            if !target_blocks.contains_key(id) {
                return Err(CorrespondenceError::MissingBlock { function: function.id, block: *id });
            }
        }
        for id in target_blocks.keys() {
            if !source_blocks.contains_key(id) {
                return Err(CorrespondenceError::UnexpectedBlock { function: function.id, block: *id });
            }
        }
        for block in &function.blocks {
            let source_block = source_blocks[&block.id];
            for instruction in &block.instructions {
                if !claimed.insert(instruction.source) {
                    return Err(CorrespondenceError::DuplicateSourceItem(instruction.source));
                }
            }
            if !claimed.insert(block.terminator.source) {
                return Err(CorrespondenceError::DuplicateSourceItem(block.terminator.source));
            }
            if block.instructions.len() != source_block.operations.len() {
                continue;
            }
        }
    }
    if let Some(item) = expected.difference(&claimed).next() {
        return Err(CorrespondenceError::MissingSourceItem(*item));
    }
    if let Some(item) = claimed.difference(&expected).next() {
        return Err(CorrespondenceError::UnexpectedSourceItem(*item));
    }

    let mut mappings = Vec::new();
    for function in &target.functions {
        let source_function = source_functions[&function.id];
        let source_blocks: BTreeMap<_, _> = source_function.blocks.iter().map(|block| (block.id, block)).collect();
        for block in &function.blocks {
            let source_block = source_blocks[&block.id];
            if block.instructions.len() != source_block.operations.len() {
                return Err(CorrespondenceError::FunctionShapeMismatch(function.id));
            }
            for (index, instruction) in block.instructions.iter().enumerate() {
                let expected_source = SourceItemId::Operation { function: function.id, block: block.id, index };
                if instruction.source != expected_source {
                    return Err(CorrespondenceError::WrongSourcePosition { expected: expected_source, actual: instruction.source });
                }
                if !matches_operation(&source_block.operations[index], &instruction.operation) {
                    return Err(CorrespondenceError::InstructionMismatch(expected_source));
                }
                let Some(rule) = operation_rule(&source_block.operations[index]) else {
                    return Err(CorrespondenceError::UnsupportedSourceItem(expected_source));
                };
                mappings.push(CorrespondenceMapping { source: expected_source, target: TargetItemId::Instruction { function: function.id, block: block.id, index }, rule });
            }
            let expected_source = SourceItemId::Terminator { function: function.id, block: block.id };
            if block.terminator.source != expected_source {
                return Err(CorrespondenceError::WrongSourcePosition { expected: expected_source, actual: block.terminator.source });
            }
            if !matches_terminator(&source_block.terminator, &block.terminator.operation) {
                return Err(CorrespondenceError::TerminatorMismatch(expected_source));
            }
            mappings.push(CorrespondenceMapping { source: expected_source, target: TargetItemId::Terminator { function: function.id, block: block.id }, rule: CorrespondenceRule::Control });
        }
    }
    Ok(CorrespondenceReceipt { revision: source.revision(), checked_source: source.checked_source(), mappings })
}
