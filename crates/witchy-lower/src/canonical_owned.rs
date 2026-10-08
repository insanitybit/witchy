//! Placement of releases for the owned-reference source ABI.
//!
//! This is untrusted lowering, not a source of checker authority. Reference
//! liveness is computed from the explicit CFG. RC tokens are conserved through
//! operations and SSA edges; dead tokens are released on the appropriate edge.
//! The resulting function still has to pass the independent memory checker.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use witchy_wir::canonical_safewir::{
    AllocationClass, BasicBlock, BlockId, CallArgument, Function, FunctionId,
    FunctionSignature, FunctionSignatureId, HostCallArgument, LoadTransfer,
    Operation, ParameterOwnership, ResultContract, StoreTransfer, Terminator,
    ValueId, ValueType,
};

pub(super) type Values = BTreeSet<ValueId>;
use witchy_wir::canonical_safewir::closure_owner_contract::{CaptureTransfer, OwnerFamily, OwnerOperation};
use witchy_wir::canonical_safewir::dictionary::{DictionaryOperation, DictionaryRehashTransfer};

/// The module's signature and callee tables. Built once per module: rebuilding
/// them for every lowered function made lowering quadratic in the number of
/// functions.
pub(super) struct CallTables<'a> {
    signatures: BTreeMap<FunctionSignatureId, &'a FunctionSignature>,
    callees: BTreeMap<FunctionId, FunctionSignatureId>,
}

impl<'a> CallTables<'a> {
    pub(super) fn new(
        signatures: &'a [FunctionSignature],
        source_functions: &HashMap<String, (FunctionId, FunctionSignatureId)>,
    ) -> Self {
        Self {
            signatures: signatures.iter().map(|signature| (signature.id, signature)).collect(),
            callees: source_functions.values().copied().collect(),
        }
    }

    pub(super) fn callee_signature(&self, callee: FunctionId) -> Option<FunctionSignatureId> {
        self.callees.get(&callee).copied()
    }
}

#[derive(Debug)]
pub(super) enum ReleasePlacementError {
    MissingBlock(BlockId),
    MissingSignature(FunctionSignatureId),
    MissingFunction(FunctionId),
    MissingToken(ValueId),
    IncompatibleOwnedEdge(BlockId),
    UnresolvedResult(ValueId),
    HostMemoryResult(ValueId),
    ExternalManagedValue(ValueId),
    TailOwnedTokenNeedsTransfer(ValueId),
    EdgeArity(BlockId),
    BlockIdOverflow,
}

fn argument_value(argument: &CallArgument) -> ValueId {
    match argument {
        CallArgument::Value(value) | CallArgument::Move(value) => *value,
    }
}

pub(super) fn definitions(operation: &Operation) -> Vec<ValueId> {
    match operation {
        Operation::ClosureOwner(owner) => owner.destinations(),
        Operation::Dictionary(dictionary) => dictionary.destinations(),
        Operation::ConstUnit { destination } | Operation::ConstInt { destination, .. } | Operation::ConstFloat { destination, .. }
        | Operation::ConstBool { destination, .. } | Operation::ConstSize { destination, .. }
        | Operation::CapabilityNone { destination, .. } | Operation::CapabilitySome { destination, .. }
        | Operation::CapabilityIsPresent { destination, .. } | Operation::CapabilityUnwrap { destination, .. }
        | Operation::Arithmetic { destination, .. } | Operation::Compare { destination, .. }
        | Operation::NarrowSize { destination, .. } | Operation::WidenSize { destination, .. }
        | Operation::FloatUnary { destination, .. }
        | Operation::Alloc { destination, .. } | Operation::StaticData { destination, .. }
        | Operation::Load { destination, .. } | Operation::MoveValue { destination, .. }
        | Operation::MakeInlineAggregate { destination, .. }
        | Operation::ExtractInlineField { destination, .. }
        | Operation::CloneRef { destination, .. } | Operation::FunctionValue { destination, .. } => vec![*destination],
        Operation::Call { destinations, .. } | Operation::VarCall { destinations, .. }
        | Operation::ExternalCall { destinations, .. }
        | Operation::HostCall { destinations, .. } => destinations.clone(),
        Operation::Guard { .. } | Operation::InitNullableOwned { .. } | Operation::Store { .. } | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. } | Operation::CloneRange { .. } | Operation::UpdateCount { .. }
        | Operation::Retain { .. } | Operation::Release { .. } | Operation::DropRef { .. }
        | Operation::Reclaim { .. } | Operation::ReauthenticateRawHeaderExtent { .. } => Vec::new(),
    }
}

pub(super) fn uses(operation: &Operation) -> Vec<ValueId> {
    match operation {
        Operation::ClosureOwner(owner) => owner.operands(),
        Operation::Dictionary(dictionary) => dictionary.operands(),
        Operation::Arithmetic { left, right, .. } | Operation::Compare { left, right, .. } => vec![*left, *right],
        Operation::NarrowSize { value, .. } | Operation::WidenSize { value, .. } | Operation::FloatUnary { value, .. } => vec![*value],
        Operation::Guard { condition, .. } => vec![*condition],
        Operation::Load { place, .. } | Operation::InitNullableOwned { place } => vec![place.object()],
        Operation::Store { place, value, .. } => vec![place.object(), *value],
        Operation::CopyBytes { destination, source, .. }
        | Operation::MoveRange { destination, source, .. }
        | Operation::CloneRange { destination, source, .. } => vec![destination.object(), source.object()],
        Operation::UpdateCount { object, .. } | Operation::Retain { object }
        | Operation::Release { object } | Operation::DropRef { object } | Operation::Reclaim { object } => vec![*object],
        Operation::MoveValue { source, .. } | Operation::CloneRef { source, .. } => vec![*source],
        Operation::CapabilitySome { source, .. } | Operation::CapabilityIsPresent { source, .. }
        | Operation::CapabilityUnwrap { source, .. } => vec![*source],
        Operation::MakeInlineAggregate { fields, .. } => fields.clone(),
        Operation::ExtractInlineField { aggregate, .. } => vec![*aggregate],
        Operation::Call { arguments, .. } | Operation::VarCall { arguments, .. }
        | Operation::ExternalCall { arguments, .. } => {
            arguments.iter().map(argument_value).collect()
        }
        Operation::HostCall { arguments, .. } => arguments.iter().filter_map(|argument| {
            // Authenticated host roles permit a linear reference only in a
            // buffer place. Scalar/capability lanes cannot own linear storage.
            match argument { HostCallArgument::Buffer { place, .. } => Some(place.object()), _ => None }
        }).collect(),
        Operation::ConstUnit { .. } | Operation::ConstInt { .. } | Operation::ConstFloat { .. } | Operation::ConstBool { .. }
        | Operation::ConstSize { .. } | Operation::Alloc { .. } | Operation::StaticData { .. }
        | Operation::CapabilityNone { .. }
        | Operation::FunctionValue { .. } => Vec::new(),
        Operation::ReauthenticateRawHeaderExtent { object, header } => vec![*object, *header],
    }
}

fn edges(terminator: &Terminator) -> Vec<(BlockId, &[ValueId])> {
    match terminator {
        Terminator::Branch { target, arguments } => vec![(*target, arguments)],
        Terminator::BranchIf { then_target, then_arguments, else_target, else_arguments, .. } => {
            vec![(*then_target, then_arguments), (*else_target, else_arguments)]
        }
        _ => Vec::new(),
    }
}

fn terminal_uses(terminator: &Terminator) -> Vec<ValueId> {
    match terminator {
        Terminator::Return { values } => values.clone(),
        Terminator::TailCall { arguments, .. } | Terminator::VarTailCall { arguments, .. } => {
            arguments.iter().map(argument_value).collect()
        }
        _ => edges(terminator).into_iter().flat_map(|(_, arguments)| arguments.iter().copied()).collect(),
    }
}

fn is_reference(function: &Function, value: ValueId) -> bool {
    matches!(function.value_types.get(&value), Some(ValueType::Ref(_) | ValueType::Gc(_)))
}

/// `is_reference` answered once per function: the liveness fixpoint and the
/// per-operation scans below ask it for every value of every operation, and
/// each answer was an ordered-map search of the function's value types.
pub(super) struct References(foldhash::HashSet<ValueId>);

impl References {
    pub(super) fn of(function: &Function) -> Self {
        Self(function.value_types.iter()
            .filter(|(_, ty)| matches!(ty, ValueType::Ref(_) | ValueType::Gc(_)))
            .map(|(value, _)| *value)
            .collect())
    }

    pub(super) fn contains(&self, value: ValueId) -> bool {
        self.0.contains(&value)
    }
}

fn edge_needs(
    references: &References,
    target: BlockId,
    arguments: &[ValueId],
    live_in: &BTreeMap<BlockId, Values>,
) -> Values {
    let mut needed = live_in.get(&target).cloned().unwrap_or_default();
    needed.extend(arguments.iter().copied().filter(|value| references.contains(*value)));
    needed
}

pub(super) fn live_tail(references: &References, block: &BasicBlock, live_in: &BTreeMap<BlockId, Values>) -> Values {
    let mut live: Values = terminal_uses(&block.terminator)
        .into_iter()
        .filter(|value| references.contains(*value))
        .collect();
    for (target, arguments) in edges(&block.terminator) {
        live.extend(edge_needs(references, target, arguments, live_in));
    }
    live
}

pub(super) fn liveness(function: &Function, references: &References) -> BTreeMap<BlockId, Values> {
    let mut live_in: BTreeMap<_, Values> = function.blocks.iter().map(|block| (block.id, Values::new())).collect();
    // The same monotone finite-set equations as a full reverse sweep, but
    // reschedule only predecessors of a block whose live-in set changed.
    let blocks: BTreeMap<_, _> = function.blocks.iter().map(|block| (block.id, block)).collect();
    let mut predecessors: BTreeMap<BlockId, Vec<BlockId>> =
        function.blocks.iter().map(|block| (block.id, Vec::new())).collect();
    for block in &function.blocks {
        for (target, _) in edges(&block.terminator) {
            if let Some(sources) = predecessors.get_mut(&target) {
                sources.push(block.id);
            }
        }
    }
    let mut pending: VecDeque<_> = function.blocks.iter().rev().map(|block| block.id).collect();
    let mut queued: BTreeSet<_> = pending.iter().copied().collect();
    while let Some(id) = pending.pop_front() {
        queued.remove(&id);
        let block = blocks[&id];
        let mut live = live_tail(references, block, &live_in);
        for operation in block.operations.iter().rev() {
            for value in definitions(operation) {
                if references.contains(value) { live.remove(&value); }
            }
            live.extend(uses(operation).into_iter().filter(|value| references.contains(*value)));
        }
        for parameter in &block.parameters { live.remove(&parameter.value); }
        if live_in.get(&id) != Some(&live) {
            live_in.insert(id, live);
            if let Some(sources) = predecessors.get(&id) {
                for source in sources {
                    if queued.insert(*source) {
                        pending.push_back(*source);
                    }
                }
            }
        }
    }
    live_in
}

fn release_dead(owned: &mut Values, live: &Values, output: &mut Vec<Operation>) {
    let dead: Vec<_> = owned.difference(live).copied().collect();
    for object in dead {
        output.push(Operation::DropRef { object });
        owned.remove(&object);
    }
}

fn release_values(owned: &mut Values, dead: &Values, output: &mut Vec<Operation>) {
    for object in dead {
        if owned.remove(object) {
            output.push(Operation::DropRef { object: *object });
        }
    }
}

fn consume(owned: &mut Values, value: ValueId) -> Result<(), ReleasePlacementError> {
    if owned.remove(&value) { Ok(()) } else { Err(ReleasePlacementError::MissingToken(value)) }
}

/// A returned parameter alias is not a fresh reference.  It may carry the one
/// consumed call token into its result lane only when the signature names that
/// exact moved managed lane.  Every incomplete or mismatched relation remains
/// unresolved: release placement is not permitted to guess an alias.
fn aliases_parameter_rebinds_moved_token(
    function: &Function,
    arguments: &[CallArgument],
    destination: ValueId,
    signature: &FunctionSignature,
    parameter: u32,
    layout: witchy_wir::layout::LayoutId,
) -> bool {
    let Ok(parameter) = usize::try_from(parameter) else {
        return false;
    };
    let Some(CallArgument::Move(source)) = arguments.get(parameter) else {
        return false;
    };
    let (
        Some(ValueType::Ref(source_layout)),
        Some(ValueType::Ref(destination_layout)),
        Some(ValueType::Ref(parameter_layout)),
        Some(parameter_contract),
    ) = (
        function.value_types.get(source),
        function.value_types.get(&destination),
        signature.parameters.get(parameter),
        signature.contract.parameters.get(parameter),
    ) else {
        return false;
    };
    *source_layout == layout
        && *destination_layout == layout
        && *parameter_layout == layout
        && parameter_contract.value_type == ValueType::Ref(layout)
        && parameter_contract.ownership == ParameterOwnership::Value
}

fn call_flow(
    function: &Function,
    owned: &mut Values,
    arguments: &[CallArgument],
    destinations: &[ValueId],
    signature: &FunctionSignature,
) -> Result<(), ReleasePlacementError> {
    for argument in arguments {
        if let CallArgument::Move(value) = argument {
            if is_reference(function, *value) { consume(owned, *value)?; }
        }
    }
    let write_back_count = signature.contract.parameters.iter()
        .filter(|parameter| matches!(parameter.ownership, ParameterOwnership::WriteBack | ParameterOwnership::UniqueWriteBack))
        .count();
    if destinations.len() < write_back_count {
        return Err(ReleasePlacementError::UnresolvedResult(
            *destinations.first().unwrap_or(&ValueId::new(0)),
        ));
    }
    let ordinary_results = destinations.len() - write_back_count;
    for (index, (destination, contract)) in destinations.iter().zip(&signature.contract.results).enumerate() {
        if index >= ordinary_results {
            // A reference write-back lane is the replacement token; a scalar
            // `var` lane is a plain value with nothing to release.
            if is_reference(function, *destination) {
                owned.insert(*destination);
            }
            continue;
        }
        match contract {
            ResultContract::Value(ValueType::Gc(_)) | ResultContract::OwnedGc { .. } => { owned.insert(*destination); }
            ResultContract::Shared { .. } | ResultContract::Unique { .. }
            | ResultContract::AliasesWriteBackParameter { .. }
            | ResultContract::AliasesOneOfWriteBackParameters { .. }
            | ResultContract::Fresh { class: AllocationClass::ReferenceCounted, .. } => { owned.insert(*destination); }
            // Like a direct StaticData operation, an authenticated static
            // result has a destructor-neutral token. Carry it through joins
            // so it can meet a releasable result in the same phi lane.
            ResultContract::StaticData { .. } if is_reference(function, *destination) => {
                owned.insert(*destination);
            }
            ResultContract::DisjunctiveWriteBackSelector { .. } => {}
            ResultContract::AliasesParameter { parameter, layout }
                if is_reference(function, *destination) => {
                    if !aliases_parameter_rebinds_moved_token(
                        function,
                        arguments,
                        *destination,
                        signature,
                        *parameter,
                        *layout,
                    ) {
                        return Err(ReleasePlacementError::UnresolvedResult(*destination));
                    }
                    owned.insert(*destination);
                }
            ResultContract::BodyDerived(_) | ResultContract::CheckedValue { .. }
            | ResultContract::ReallocatesParameter { .. }
                if is_reference(function, *destination) => {
                    return Err(ReleasePlacementError::UnresolvedResult(*destination));
                }
            _ => {}
        }
    }
    Ok(())
}

/// The values a function binds to static data segments. Such a value is
/// immortal: it carries no releasable token, and a clone of it needs none.
fn static_data_values(function: &Function) -> Values {
    function
        .blocks
        .iter()
        .flat_map(|block| block.operations.iter())
        .filter_map(|operation| match operation {
            Operation::StaticData { destination, .. } => Some(*destination),
            _ => None,
        })
        .collect()
}

fn operation_flow(
    function: &Function,
    operation: &Operation,
    owned: &mut Values,
    immortal: &Values,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    callees: &BTreeMap<FunctionId, FunctionSignatureId>,
    families: &[OwnerFamily],
) -> Result<(), ReleasePlacementError> {
    match operation {
        Operation::ClosureOwner(owner) => {
            for value in owner.operands() {
                if is_reference(function, value) && !owned.contains(&value) && !immortal.contains(&value) {
                    return Err(ReleasePlacementError::MissingToken(value));
                }
            }
            match owner {
                OwnerOperation::Drop { owner, .. } => consume(owned, *owner)?,
                OwnerOperation::Deconstruct { owner, .. } => consume(owned, *owner)?,
                OwnerOperation::DropList { list, .. } => consume(owned, *list)?,
                OwnerOperation::Construct { destination, constructor, captures } => {
                    let Some(ValueType::Gc(owner_type)) = function.value_types.get(destination) else {
                        return Err(ReleasePlacementError::UnresolvedResult(*destination));
                    };
                    let contract = families.iter().find(|family| family.owner_type == *owner_type)
                        .and_then(|family| family.constructors.iter().find(|entry| entry.id == *constructor))
                        .ok_or(ReleasePlacementError::UnresolvedResult(*destination))?;
                    if contract.captures.len() != captures.len() {
                        return Err(ReleasePlacementError::UnresolvedResult(*destination));
                    }
                    for (value, field) in captures.iter().zip(&contract.captures) {
                        if matches!(field.transfer, CaptureTransfer::MoveManaged) {
                            consume(owned, *value)?;
                        }
                    }
                }
                OwnerOperation::ListPush { list, value, .. }
                | OwnerOperation::ListSet { list, value, .. } => {
                    consume(owned, *list)?;
                    consume(owned, *value)?;
                }
                OwnerOperation::ReadCapability { .. }
                | OwnerOperation::ReadOwner { .. }
                | OwnerOperation::ReadEnvironmentField { .. }
                | OwnerOperation::ReadSharedPlace { .. }
                | OwnerOperation::ReadSharedSlice { .. }
                | OwnerOperation::ReadCallable { .. }
                | OwnerOperation::ReadTag { .. }
                | OwnerOperation::Clone { .. }
                | OwnerOperation::CloneEnvironment { .. }
                | OwnerOperation::NewList { .. }
                | OwnerOperation::ListGet { .. }
                | OwnerOperation::ListLength { .. }
                | OwnerOperation::CloneList { .. } => {}
            }
            for value in owner.destinations() {
                if is_reference(function, value) { owned.insert(value); }
            }
        }
        Operation::Dictionary(dictionary) => {
            for value in dictionary.operands() {
                if is_reference(function, value) && !owned.contains(&value) && !immortal.contains(&value) {
                    return Err(ReleasePlacementError::MissingToken(value));
                }
            }
            match dictionary {
                DictionaryOperation::InsertSlot { key, value, key_transfer, value_transfer, .. } => {
                    if *key_transfer == StoreTransfer::MoveUnique && is_reference(function, *key) {
                        consume(owned, *key)?;
                    }
                    if *value_transfer == StoreTransfer::MoveUnique && is_reference(function, *value) {
                        consume(owned, *value)?;
                    }
                }
                DictionaryOperation::ReplaceValue { value, transfer, .. } => {
                    if *transfer == StoreTransfer::MoveUnique && is_reference(function, *value) {
                        consume(owned, *value)?;
                    }
                }
                DictionaryOperation::Rehash { source, transfer: DictionaryRehashTransfer::MoveUnique, .. } => {
                    consume(owned, *source)?;
                }
                _ => {}
            }
            for value in dictionary.destinations() {
                if matches!(dictionary, DictionaryOperation::ReadSlot {
                    destination, transfer: LoadTransfer::BorrowAffine, ..
                } if *destination == value) {
                    continue;
                }
                if is_reference(function, value) { owned.insert(value); }
            }
        }
        Operation::Alloc { destination, class: AllocationClass::ReferenceCounted, .. }
        // A static reference has no physical RC count to release, but it is a
        // destructor-neutral token at a CFG join. Track it exactly like a
        // releasable token so a phi that receives static data on one edge and
        // an owned reference on another has one uniform target token. The
        // independent checker authenticates StaticData and admits its
        // DropRef as a no-op; this pass must not make ownership depend on
        // which predecessor happened to reach the join.
        | Operation::StaticData { destination, .. }
        | Operation::Load { destination, transfer: LoadTransfer::CloneShared | LoadTransfer::ClonePresent | LoadTransfer::MoveUnique | LoadTransfer::TakeLast | LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField, .. } => {
            if is_reference(function, *destination) { owned.insert(*destination); }
        }
        Operation::CloneRef { destination, source } => {
            // StaticData has no releasable source token. A clone still creates
            // an explicit reference value for ordinary call/store accounting;
            // the independent checker authenticates the static segment.
            if !owned.contains(source) && !immortal.contains(source) { return Err(ReleasePlacementError::MissingToken(*source)); }
            owned.insert(*destination);
        }
        Operation::MoveValue { destination, source } if is_reference(function, *source) => {
            consume(owned, *source)?;
            owned.insert(*destination);
        }
        Operation::Store { value, transfer: StoreTransfer::MoveUnique, .. } if is_reference(function, *value) => {
            consume(owned, *value)?;
        }
        Operation::DropRef { object } | Operation::Release { object } => consume(owned, *object)?,
        Operation::Reclaim { object } => { owned.remove(object); }
        Operation::Call { destinations, arguments, callee } => {
            let signature = callees.get(callee).ok_or(ReleasePlacementError::MissingFunction(*callee))?;
            let signature = signatures.get(signature).ok_or(ReleasePlacementError::MissingSignature(*signature))?;
            call_flow(function, owned, arguments, destinations, signature)?;
        }
        Operation::VarCall { destinations, arguments, signature, .. } => {
            let signature = signatures.get(signature).ok_or(ReleasePlacementError::MissingSignature(*signature))?;
            call_flow(function, owned, arguments, destinations, signature)?;
        }
        Operation::ExternalCall { destinations, arguments, .. } => {
            // The SafeWIR checker admits this edge only for scalar Value lanes.
            // Preserve that boundary here instead of treating an external
            // import like a local or host call: no ownership token can cross
            // into a separately-instantiated Wasm memory.
            if let Some(value) = arguments.iter().map(argument_value)
                .chain(destinations.iter().copied())
                .find(|value| is_reference(function, *value))
            {
                return Err(ReleasePlacementError::ExternalManagedValue(value));
            }
        }
        Operation::HostCall { destinations, .. } => {
            if let Some(value) = destinations.iter().find(|value| is_reference(function, **value)) {
                return Err(ReleasePlacementError::HostMemoryResult(*value));
            }
        }
        Operation::ConstInt { .. }
        | Operation::ConstUnit { .. }
        | Operation::ConstFloat { .. }
        | Operation::ConstBool { .. }
        | Operation::ConstSize { .. }
        | Operation::CapabilityNone { .. }
        | Operation::CapabilitySome { .. }
        | Operation::CapabilityIsPresent { .. }
        | Operation::CapabilityUnwrap { .. }
        | Operation::Arithmetic { .. }
        | Operation::Compare { .. }
        | Operation::NarrowSize { .. }
        | Operation::WidenSize { .. }
        | Operation::FloatUnary { .. }
        | Operation::Guard { .. }
        | Operation::ReauthenticateRawHeaderExtent { .. }
        | Operation::Alloc { .. }
        | Operation::Load { .. }
        | Operation::InitNullableOwned { .. }
        | Operation::Store { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::UpdateCount { .. }
        | Operation::MoveValue { .. }
        | Operation::MakeInlineAggregate { .. }
        | Operation::ExtractInlineField { .. }
        | Operation::Retain { .. }
        | Operation::FunctionValue { .. } => {}
    }
    Ok(())
}

pub(super) fn insert_releases(
    function: &mut Function,
    signature: &FunctionSignature,
    tables: &CallTables<'_>,
    families: &[OwnerFamily],
) -> Result<(), ReleasePlacementError> {
    let blocks: BTreeMap<_, _> = function.blocks.iter().map(|block| (block.id, block)).collect();
    let entry = blocks.get(&function.entry).ok_or(ReleasePlacementError::MissingBlock(function.entry))?;
    let owned = entry.parameters.iter().zip(&signature.contract.parameters).filter_map(|(parameter, contract)| {
        (is_reference(function, parameter.value)
            && matches!(contract.ownership, ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack | ParameterOwnership::Shared | ParameterOwnership::WriteBack))
            .then_some(parameter.value)
    }).collect::<Values>();
    let references = References::of(function);
    let live_in = liveness(function, &references);
    let mut immortal = static_data_values(function);
    immortal.extend(entry.parameters.iter().zip(&signature.contract.parameters).filter_map(|(parameter, contract)| {
        (is_reference(function, parameter.value) && contract.ownership == ParameterOwnership::Value)
            .then_some(parameter.value)
    }));
    let mut incoming = BTreeMap::from([(function.entry, owned)]);
    let mut pending = VecDeque::from([function.entry]);
    let mut rewritten = BTreeMap::new();
    let mut edge_blocks = Vec::new();
    let mut next_block = function.blocks.iter().map(|block| block.id.get()).max().unwrap_or(0);

    while let Some(id) = pending.pop_front() {
        let block = blocks.get(&id).ok_or(ReleasePlacementError::MissingBlock(id))?;
        let mut owned = incoming[&id].clone();
        let mut live = live_tail(&references, block, &live_in);
        let mut dead_after = Vec::with_capacity(block.operations.len());
        for operation in block.operations.iter().rev() {
            let definitions = definitions(operation);
            let uses = uses(operation);
            let dead = definitions
                .iter()
                .chain(&uses)
                .copied()
                .filter(|value| references.contains(*value) && !live.contains(value))
                .collect();
            dead_after.push(dead);
            for value in definitions {
                if references.contains(value) { live.remove(&value); }
            }
            live.extend(uses.into_iter().filter(|value| references.contains(*value)));
        }
        dead_after.reverse();
        let mut output = Vec::new();
        release_dead(&mut owned, &live, &mut output);
        for (operation, dead) in block.operations.iter().zip(dead_after) {
            operation_flow(function, operation, &mut owned, &immortal, &tables.signatures, &tables.callees, families)?;
            output.push(operation.clone());
            release_values(&mut owned, &dead, &mut output);
        }

        let mut terminator = block.terminator.clone();
        match &mut terminator {
            Terminator::Return { values } => {
                release_dead(&mut owned, &values.iter().copied().collect(), &mut output);
            }
            Terminator::TailCall { arguments, .. } | Terminator::VarTailCall { arguments, .. } => {
                let used = arguments.iter().map(argument_value).collect();
                release_dead(&mut owned, &used, &mut output);
                for argument in arguments {
                    if let CallArgument::Move(value) = argument {
                        if is_reference(function, *value) { consume(&mut owned, *value)?; }
                    }
                }
                if let Some(value) = owned.first() {
                    return Err(ReleasePlacementError::TailOwnedTokenNeedsTransfer(*value));
                }
            }
            Terminator::Trap | Terminator::Unreachable => {
                release_dead(&mut owned, &Values::new(), &mut output);
            }
            _ => {}
        }
        let mut replacements = Vec::new();
        for (target, arguments) in edges(&terminator) {
            let destination = blocks.get(&target).ok_or(ReleasePlacementError::MissingBlock(target))?;
            if destination.parameters.len() != arguments.len() { return Err(ReleasePlacementError::EdgeArity(target)); }
            let mut edge_owned = owned.clone();
            let mut drops = Vec::new();
            release_dead(
                &mut edge_owned,
                &edge_needs(&references, target, arguments, &live_in),
                &mut drops,
            );
            // Edge forwarding is simultaneous. One source token cannot provide
            // two destination lanes; that requires an explicit CloneRef.
            let mut moved = Values::new();
            let mut renamed = edge_owned.clone();
            let mut transfers = Vec::new();
            for (argument, parameter) in arguments.iter().zip(&destination.parameters) {
                if edge_owned.contains(argument) {
                    if !moved.insert(*argument) { return Err(ReleasePlacementError::MissingToken(*argument)); }
                    transfers.push((*argument, parameter.value));
                }
            }
            // Remove every source before inserting any destination. Applying
            // renames one at a time loses a token for permutations such as
            // `(left, right) -> (right, left)` on a loop backedge.
            for (source, _) in &transfers { renamed.remove(source); }
            for (_, destination) in transfers { renamed.insert(destination); }
            if let Some(previous) = incoming.get(&target) {
                if previous != &renamed {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical owned edge mismatch at {target:?}: first {previous:?}, now {renamed:?} (from {:?})", block.id);
                    }
                    return Err(ReleasePlacementError::IncompatibleOwnedEdge(target));
                }
            } else {
                incoming.insert(target, renamed);
                pending.push_back(target);
            }
            let replacement = if drops.is_empty() { target } else {
                next_block = next_block.checked_add(1).ok_or(ReleasePlacementError::BlockIdOverflow)?;
                let id = BlockId::new(next_block);
                edge_blocks.push(BasicBlock {
                    id, parameters: Vec::new(), operations: drops,
                    terminator: Terminator::Branch { target, arguments: arguments.to_vec() },
                });
                id
            };
            replacements.push(replacement);
        }
        match &mut terminator {
            Terminator::Branch { target, arguments } => {
                if *target != replacements[0] { arguments.clear(); }
                *target = replacements[0];
            }
            Terminator::BranchIf { then_target, then_arguments, else_target, else_arguments, .. } => {
                if *then_target != replacements[0] { then_arguments.clear(); }
                if *else_target != replacements[1] { else_arguments.clear(); }
                *then_target = replacements[0];
                *else_target = replacements[1];
            }
            _ => {}
        }
        rewritten.insert(id, BasicBlock { id, parameters: block.parameters.clone(), operations: output, terminator });
    }
    function.blocks = function.blocks.iter().map(|block| rewritten.remove(&block.id).unwrap_or_else(|| block.clone())).collect();
    function.blocks.extend(edge_blocks);
    Ok(())
}

/// Use the same CFG/token placement as linear references, then select the
/// destructor from the explicit candidate registry. This registry is not
/// authority; the independent checker authenticates its source correspondence.
pub(super) fn insert_releases_with_owner_families(
    function: &mut Function,
    signature: &FunctionSignature,
    tables: &CallTables<'_>,
    families: &[OwnerFamily],
) -> Result<(), ReleasePlacementError> {
    let mut drops = BTreeMap::new();
    for family in families {
        if drops.insert(family.owner_type, (family.signature, false)).is_some()
            || drops.insert(family.list_type, (family.signature, true)).is_some()
        {
            return Err(ReleasePlacementError::MissingSignature(family.signature));
        }
    }
    for (value, ty) in &function.value_types {
        if let ValueType::Gc(id) = ty {
            if !drops.contains_key(id) { return Err(ReleasePlacementError::UnresolvedResult(*value)); }
        }
    }
    insert_releases(function, signature, tables, families)?;
    for block in &mut function.blocks {
        for operation in &mut block.operations {
            let Operation::DropRef { object } = operation else { continue };
            let Some(ValueType::Gc(id)) = function.value_types.get(object) else { continue };
            let (family, list) = drops[id];
            *operation = Operation::ClosureOwner(if list {
                OwnerOperation::DropList { list: *object, family }
            } else {
                OwnerOperation::Drop { owner: *object, family }
            });
        }
    }
    Ok(())
}
