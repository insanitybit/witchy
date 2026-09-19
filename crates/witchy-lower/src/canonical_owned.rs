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

type Values = BTreeSet<ValueId>;
use witchy_wir::canonical_safewir::closure_owner_contract::{OwnerFamily, OwnerOperation};

#[derive(Debug)]
pub(super) enum ReleasePlacementError {
    MissingBlock(BlockId),
    MissingSignature(FunctionSignatureId),
    MissingFunction(FunctionId),
    MissingToken(ValueId),
    IncompatibleOwnedEdge(BlockId),
    UnresolvedResult(ValueId),
    HostMemoryResult(ValueId),
    TailOwnedTokenNeedsTransfer(ValueId),
    EdgeArity(BlockId),
    BlockIdOverflow,
}

#[cfg(test)]
mod nullable_tests {
    use super::*;
    use witchy_wir::canonical_safewir::{AccessMode, Place, Projection};
    use witchy_wir::layout::LayoutId;

    #[test]
    fn nullable_owned_release_placement_tracks_storage_use_and_loaded_token() {
        let parent = ValueId::new(1);
        let child = ValueId::new(2);
        let reference = ValueType::Ref(LayoutId::from_bytes([0; 32]));
        let function = Function {
            id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: BlockId::new(0),
            value_types: BTreeMap::from([(parent, reference), (child, reference)]), blocks: vec![],
        };
        let init = Operation::InitNullableOwned {
            place: Place::projected(parent, Projection::Field { index: 0 }, reference, AccessMode::Write),
        };
        assert!(definitions(&init).is_empty());
        assert_eq!(uses(&init), vec![parent]);
        let load = Operation::Load {
            destination: child,
            place: Place::projected(parent, Projection::Field { index: 0 }, reference, AccessMode::Read),
            transfer: LoadTransfer::ClonePresent,
        };
        let mut owned = Values::from([parent]);
        operation_flow(&function, &init, &mut owned, &Values::new(), &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert_eq!(owned, Values::from([parent]));
        operation_flow(&function, &load, &mut owned, &Values::new(), &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert_eq!(owned, Values::from([parent, child]));
        let mut drops = vec![];
        release_dead(&mut owned, &Values::new(), &mut drops);
        assert_eq!(drops, vec![Operation::DropRef { object: parent }, Operation::DropRef { object: child }]);
        assert!(owned.is_empty());
    }
}

fn argument_value(argument: &CallArgument) -> ValueId {
    match argument {
        CallArgument::Value(value) | CallArgument::Move(value) => *value,
    }
}

fn definitions(operation: &Operation) -> Vec<ValueId> {
    match operation {
        Operation::ClosureOwner(owner) => owner.destinations(),
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
        | Operation::HostCall { destinations, .. } => destinations.clone(),
        Operation::Guard { .. } | Operation::InitNullableOwned { .. } | Operation::Store { .. } | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. } | Operation::CloneRange { .. } | Operation::UpdateCount { .. }
        | Operation::Retain { .. } | Operation::Release { .. } | Operation::DropRef { .. }
        | Operation::Reclaim { .. } => Vec::new(),
    }
}

fn uses(operation: &Operation) -> Vec<ValueId> {
    match operation {
        Operation::ClosureOwner(owner) => owner.operands(),
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
        Operation::Call { arguments, .. } | Operation::VarCall { arguments, .. } => {
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

fn edge_needs(
    function: &Function,
    target: BlockId,
    arguments: &[ValueId],
    live_in: &BTreeMap<BlockId, Values>,
) -> Values {
    let mut needed = live_in.get(&target).cloned().unwrap_or_default();
    needed.extend(arguments.iter().copied().filter(|value| is_reference(function, *value)));
    needed
}

fn live_tail(function: &Function, block: &BasicBlock, live_in: &BTreeMap<BlockId, Values>) -> Values {
    let mut live: Values = terminal_uses(&block.terminator)
        .into_iter()
        .filter(|value| is_reference(function, *value))
        .collect();
    for (target, arguments) in edges(&block.terminator) {
        live.extend(edge_needs(function, target, arguments, live_in));
    }
    live
}

fn liveness(function: &Function) -> BTreeMap<BlockId, Values> {
    let mut live_in: BTreeMap<_, Values> = function.blocks.iter().map(|block| (block.id, Values::new())).collect();
    // Monotone finite-set iteration: no guessed loop trip count or range fact.
    loop {
        let mut changed = false;
        for block in function.blocks.iter().rev() {
            let mut live = live_tail(function, block, &live_in);
            for operation in block.operations.iter().rev() {
                for value in definitions(operation) {
                    if is_reference(function, value) { live.remove(&value); }
                }
                live.extend(uses(operation).into_iter().filter(|value| is_reference(function, *value)));
            }
            for parameter in &block.parameters { live.remove(&parameter.value); }
            if live_in.get(&block.id) != Some(&live) {
                live_in.insert(block.id, live);
                changed = true;
            }
        }
        if !changed { return live_in; }
    }
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
            ResultContract::Value(ValueType::Gc(_)) => { owned.insert(*destination); }
            ResultContract::Shared { .. } | ResultContract::Unique { .. }
            | ResultContract::Fresh { class: AllocationClass::ReferenceCounted, .. } => { owned.insert(*destination); }
            ResultContract::BodyDerived(_) | ResultContract::CheckedValue { .. }
            | ResultContract::AliasesParameter { .. } | ResultContract::ReallocatesParameter { .. }
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
                OwnerOperation::DropList { list, .. } => consume(owned, *list)?,
                OwnerOperation::ListPush { list, value, .. }
                | OwnerOperation::ListSet { list, value, .. } => {
                    consume(owned, *list)?;
                    consume(owned, *value)?;
                }
                OwnerOperation::Construct { .. }
                | OwnerOperation::ReadCapability { .. }
                | OwnerOperation::ReadOwner { .. }
                | OwnerOperation::ReadSharedPlace { .. }
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
        Operation::Alloc { destination, class: AllocationClass::ReferenceCounted, .. }
        | Operation::Load { destination, transfer: LoadTransfer::CloneShared | LoadTransfer::ClonePresent | LoadTransfer::MoveUnique | LoadTransfer::TakeLast, .. } => {
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
        | Operation::Alloc { .. }
        | Operation::StaticData { .. }
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
    signatures: &[FunctionSignature],
    source_functions: &HashMap<String, (FunctionId, FunctionSignatureId)>,
) -> Result<(), ReleasePlacementError> {
    let signatures = signatures.iter().map(|signature| (signature.id, signature)).collect();
    let callees = source_functions.values().copied().collect();
    let blocks: BTreeMap<_, _> = function.blocks.iter().map(|block| (block.id, block)).collect();
    let entry = blocks.get(&function.entry).ok_or(ReleasePlacementError::MissingBlock(function.entry))?;
    let owned = entry.parameters.iter().zip(&signature.contract.parameters).filter_map(|(parameter, contract)| {
        (is_reference(function, parameter.value)
            && matches!(contract.ownership, ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack | ParameterOwnership::Shared | ParameterOwnership::WriteBack))
            .then_some(parameter.value)
    }).collect::<Values>();
    let live_in = liveness(function);
    let immortal = static_data_values(function);
    let mut incoming = BTreeMap::from([(function.entry, owned)]);
    let mut pending = VecDeque::from([function.entry]);
    let mut rewritten = BTreeMap::new();
    let mut edge_blocks = Vec::new();
    let mut next_block = function.blocks.iter().map(|block| block.id.get()).max().unwrap_or(0);

    while let Some(id) = pending.pop_front() {
        let block = blocks.get(&id).ok_or(ReleasePlacementError::MissingBlock(id))?;
        let mut owned = incoming[&id].clone();
        let mut live = live_tail(function, block, &live_in);
        let mut dead_after = Vec::with_capacity(block.operations.len());
        for operation in block.operations.iter().rev() {
            let definitions = definitions(operation);
            let uses = uses(operation);
            let dead = definitions
                .iter()
                .chain(&uses)
                .copied()
                .filter(|value| is_reference(function, *value) && !live.contains(value))
                .collect();
            dead_after.push(dead);
            for value in definitions {
                if is_reference(function, value) { live.remove(&value); }
            }
            live.extend(uses.into_iter().filter(|value| is_reference(function, *value)));
        }
        dead_after.reverse();
        let mut output = Vec::new();
        release_dead(&mut owned, &live, &mut output);
        for (operation, dead) in block.operations.iter().zip(dead_after) {
            operation_flow(function, operation, &mut owned, &immortal, &signatures, &callees)?;
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
                &edge_needs(function, target, arguments, &live_in),
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
    signatures: &[FunctionSignature],
    source_functions: &HashMap<String, (FunctionId, FunctionSignatureId)>,
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
    insert_releases(function, signature, signatures, source_functions)?;
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

#[cfg(test)]
mod opaque_tests {
    use super::*;
    use witchy_wir::canonical_safewir::{BlockParameter, CallableContract, GcTypeId, Ownership, ParameterContract};

    #[test]
    fn opaque_owner_release_placement_preserves_return_and_drops_original() {
        let ty = GcTypeId::new(0xc1000000);
        let family = OwnerFamily {
            source: witchy_wir::canonical_safewir::closure_owner_contract::OwnerFamilySource::Closure,
            owner_type: ty, list_type: GcTypeId::new(0xc1000001),
            signature: FunctionSignatureId::new(0), declaration_signature: FunctionSignatureId::new(1),
            environment: witchy_wir::layout::LayoutId::from_bytes([0; 32]), constructors: vec![],
        };
        let original = ValueId::new(0);
        let cloned = ValueId::new(1);
        let signature = FunctionSignature {
            id: family.signature, parameters: vec![ValueType::Gc(ty)], results: vec![ValueType::Gc(ty)],
            contract: CallableContract {
                parameters: vec![ParameterContract { value_type: ValueType::Gc(ty), ownership: ParameterOwnership::Shared }],
                results: vec![ResultContract::Value(ValueType::Gc(ty))], parameter_effects: BTreeMap::new(),
            },
        };
        let mut function = Function {
            id: FunctionId::new(0), signature: signature.id, entry: BlockId::new(0),
            value_types: BTreeMap::from([(original, ValueType::Gc(ty)), (cloned, ValueType::Gc(ty))]),
            blocks: vec![BasicBlock {
                id: BlockId::new(0), parameters: vec![BlockParameter { value: original, value_type: ValueType::Gc(ty), ownership: Some(Ownership::Shared) }],
                operations: vec![Operation::ClosureOwner(OwnerOperation::Clone { destination: cloned, owner: original, family: family.signature })],
                terminator: Terminator::Return { values: vec![cloned] },
            }],
        };
        insert_releases_with_owner_families(&mut function, &signature, &[signature.clone()], &HashMap::new(), &[family.clone()]).unwrap();
        assert_eq!(function.blocks[0].operations, vec![
            Operation::ClosureOwner(OwnerOperation::Clone { destination: cloned, owner: original, family: family.signature }),
            Operation::ClosureOwner(OwnerOperation::Drop { owner: original, family: family.signature }),
        ]);
        assert_eq!(function.blocks[0].terminator, Terminator::Return { values: vec![cloned] });
    }
}
