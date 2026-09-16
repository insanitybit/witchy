//! Affine ownership of opaque GC values. These tokens have no linear-memory
//! address, extent, allocation class, or pointer-slot authority.
use super::*;
use crate::canonical_safewir::closure_owner_contract::{CaptureStorage, CaptureTransfer, ConstructorId, OwnerCapture, OwnerFamily, OwnerFamilySource, OwnerOperation, SharedReferenceConstructor};
use crate::canonical_safewir::{GcTypeId, Ownership, ResultContract};

type Live = BTreeSet<ValueId>;

pub(super) struct Registry<'a> {
    families: &'a [OwnerFamily],
}

impl<'a> Registry<'a> {
    pub(super) fn new<S: ModuleStage>(module: &'a CanonicalModule<S>) -> Result<Self, InitialCheckError> {
        let registry = Self { families: module.owner_families() };
        let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&module.layouts().canonical_bytes())
            .map_err(|_| InitialCheckError::UnsupportedMemoryOperation {
                function: FunctionId::new(u32::MAX),
                operation: "ClosureOwner invalid layout catalog",
            })?;
        let mut types = BTreeSet::new();
        let mut signatures = BTreeSet::new();
        let mut constructors = BTreeSet::new();
        for family in registry.families {
            let shared_reference = matches!(family.source, OwnerFamilySource::SharedReference(_));
            if !types.insert(family.owner_type)
                || !shared_reference && !types.insert(family.list_type)
                || !signatures.insert(family.signature)
            {
                return Err(InitialCheckError::UnsupportedMemorySignature { signature: family.signature });
            }
            if matches!(family.source, OwnerFamilySource::Closure) {
                let body = module.signatures().iter().find(|s| s.id == family.signature)
                    .ok_or(InitialCheckError::MissingSignature(family.signature))?;
                let declaration = module.signatures().iter().find(|s| s.id == family.declaration_signature)
                    .ok_or(InitialCheckError::MissingSignature(family.declaration_signature))?;
                if body.parameters.first() != Some(&ValueType::Gc(family.owner_type))
                    || body.parameters.get(1..) != Some(declaration.parameters.as_slice())
                    || body.results != declaration.results
                    || body.contract.parameters.first().map(|p| p.ownership) != Some(ParameterOwnership::Shared)
                    || body.contract.parameters.get(1..) != Some(declaration.contract.parameters.as_slice())
                    || body.contract.results != declaration.contract.results
                {
                    return Err(InitialCheckError::CallableContractMismatch { signature: family.signature });
                }
            }
            if family.constructors.is_empty() {
                return Err(InitialCheckError::CallableContractMismatch { signature: family.signature });
            }
            if let OwnerFamilySource::SharedReference(source_type) = &family.source {
                if !shared_int_reference(source_type) {
                    return Err(InitialCheckError::UnsupportedMemorySignature { signature: family.signature });
                }
                let mut kinds = BTreeSet::new();
                for constructor in &family.constructors {
                    let Some(kind) = constructor.shared_reference_constructor() else {
                        return Err(InitialCheckError::CallableContractMismatch { signature: family.signature });
                    };
                    let Some(OwnerCapture { value_type: ValueType::Ref(root), .. }) = constructor.captures.first() else {
                        return Err(InitialCheckError::CallableContractMismatch { signature: family.signature });
                    };
                    let physical = layouts.resolve_nominal(*root).map_err(|_| InitialCheckError::CallableContractMismatch { signature: family.signature })?;
                    let valid = match kind {
                        SharedReferenceConstructor::WholeIntCell { root } => root.0 == 0
                            && kinds.insert(0_u8)
                            && matches!(layouts.get(physical).map(|descriptor| descriptor.kind()),
                                Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int))),
                        SharedReferenceConstructor::ListIntElement { root, index, bounds: _ } => {
                            let element = match layouts.get(physical).map(|descriptor| descriptor.kind()) {
                                Some(crate::layout::LayoutKind::PackedList { element, .. }) => Some(*element),
                                _ => None,
                            };
                            root.0 == 0 && index.0 == 1 && kinds.insert(1_u8)
                                && element.is_some_and(|element| matches!(layouts.get(element).map(|descriptor| descriptor.kind()),
                                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int))))
                        }
                    };
                    if !valid {
                        return Err(InitialCheckError::CallableContractMismatch { signature: family.signature });
                    }
                }
            }
            for constructor in &family.constructors {
                if !constructors.insert(constructor.id.0)
                    || matches!(family.source, OwnerFamilySource::Closure)
                        && !module.functions().iter().any(|f| f.id == constructor.function && f.signature == family.signature)
                {
                    return Err(InitialCheckError::CallableContractMismatch { signature: family.signature });
                }
                let mut fields = BTreeSet::new();
                for capture in &constructor.captures {
                    let valid = match (capture.value_type, capture.storage, capture.transfer) {
                        (ValueType::Capability(actual), CaptureStorage::OwnerField(field), CaptureTransfer::CopyCapability(expected)) => {
                            actual == expected && field >= 2 && fields.insert((true, field))
                        }
                        (ValueType::Ref(_), CaptureStorage::EnvironmentField(field), CaptureTransfer::CloneManaged) => fields.insert((false, field)),
                        (ValueType::Gc(_), CaptureStorage::OwnerField(field), CaptureTransfer::CloneManaged) => field >= 2 && fields.insert((true, field)),
                        (ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit | ValueType::Size,
                            CaptureStorage::EnvironmentField(field), CaptureTransfer::CopyScalar) => fields.insert((false, field)),
                        _ => false,
                    };
                    if !valid {
                        return Err(InitialCheckError::CallableContractMismatch { signature: family.signature });
                    }
                }
            }
        }
        for signature in module.signatures() {
            for parameter in &signature.contract.parameters {
                if let ValueType::Gc(id) = parameter.value_type {
                    if !registry.registered(id) || parameter.ownership != ParameterOwnership::Shared {
                        return Err(InitialCheckError::UnsupportedMemorySignature { signature: signature.id });
                    }
                }
            }
            for (ty, result) in signature.results.iter().zip(&signature.contract.results) {
                if let ValueType::Gc(id) = ty {
                    if !registry.registered(*id) || result != &ResultContract::Value(*ty) {
                        return Err(InitialCheckError::UnsupportedMemorySignature { signature: signature.id });
                    }
                }
            }
        }
        Ok(registry)
    }

    fn registered(&self, id: GcTypeId) -> bool {
        self.families.iter().any(|family| family.owner_type == id
            || !matches!(family.source, OwnerFamilySource::SharedReference(_)) && family.list_type == id)
    }

    fn family(&self, id: FunctionSignatureId) -> Result<&OwnerFamily, InitialCheckError> {
        self.families.iter().find(|f| f.signature == id).ok_or(InitialCheckError::MissingSignature(id))
    }

    fn constructor(&self, function: &Function, id: ConstructorId) -> Result<(&OwnerFamily, &crate::canonical_safewir::closure_owner_contract::OwnerConstructor), InitialCheckError> {
        self.families.iter().find_map(|f| f.constructors.iter().find(|c| c.id == id).map(|c| (f, c)))
            .ok_or(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ClosureOwner unknown constructor" })
    }

    fn operation(&self, function: &Function, operation: &OwnerOperation) -> Result<(), InitialCheckError> {
        use OwnerOperation as O;
        match operation {
            O::Construct { destination, constructor, captures } => {
                let (family, constructor) = self.constructor(function, *constructor)?;
                require_type(function, *destination, ValueType::Gc(family.owner_type))?;
                require_arity("owner captures", constructor.captures.len(), captures.len())?;
                for (value, capture) in captures.iter().zip(&constructor.captures) {
                    require_type(function, *value, capture.value_type)?;
                }
            }
            O::ReadCapability { destination, owner, constructor, capture } => {
                let (family, constructor) = self.constructor(function, *constructor)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ReadCapability shared-reference family" });
                }
                require_type(function, *owner, ValueType::Gc(family.owner_type))?;
                let field = constructor.captures.get(capture.0 as usize).ok_or(InitialCheckError::InvalidOwnership { function: function.id, object: *owner })?;
                let CaptureTransfer::CopyCapability(kind) = field.transfer else {
                    return Err(InitialCheckError::InvalidOwnership { function: function.id, object: *owner });
                };
                require_type(function, *destination, ValueType::Capability(kind))?;
            }
            O::ReadOwner { destination, owner, constructor, capture } => {
                let (family, constructor) = self.constructor(function, *constructor)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ReadOwner shared-reference family" });
                }
                require_type(function, *owner, ValueType::Gc(family.owner_type))?;
                let field = constructor.captures.get(capture.0 as usize)
                    .ok_or(InitialCheckError::InvalidOwnership { function: function.id, object: *owner })?;
                let (ValueType::Gc(nested_type), CaptureStorage::OwnerField(_), CaptureTransfer::CloneManaged) =
                    (field.value_type, field.storage, field.transfer)
                else {
                    return Err(InitialCheckError::InvalidOwnership { function: function.id, object: *owner });
                };
                require_type(function, *destination, ValueType::Gc(nested_type))?;
            }
            O::ReadSharedPlace { destination, owner, family } => {
                let family = self.family(*family)?;
                if !matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation {
                        function: function.id,
                        operation: "ReadSharedPlace non-reference family",
                    });
                }
                require_type(function, *owner, ValueType::Gc(family.owner_type))?;
                require_type(function, *destination, ValueType::Int)?;
            }
            O::ReadTag { destination, owner, family } => {
                let family = self.family(*family)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ReadTag shared-reference family" });
                }
                require_type(function, *owner, ValueType::Gc(family.owner_type))?;
                require_type(function, *destination, ValueType::Int)?;
            }
            O::CloneEnvironment { destination, owner, constructor } => {
                let (family, _) = self.constructor(function, *constructor)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "CloneEnvironment shared-reference family" });
                }
                require_type(function, *owner, ValueType::Gc(family.owner_type))?;
                require_type(function, *destination, ValueType::Ref(family.environment))?;
            }
            O::ReadCallable { destination, owner, family } => {
                let family = self.family(*family)?;
                if !matches!(family.source, OwnerFamilySource::Closure) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ReadCallable aggregate family" });
                }
                require_type(function, *owner, ValueType::Gc(family.owner_type))?;
                require_type(function, *destination, ValueType::Callable(family.signature))?;
            }
            O::Clone { destination, owner, family } => {
                let ty = ValueType::Gc(self.family(*family)?.owner_type);
                require_type(function, *owner, ty)?;
                require_type(function, *destination, ty)?;
            }
            O::Drop { owner, family } => require_type(function, *owner, ValueType::Gc(self.family(*family)?.owner_type))?,
            O::NewList { destination, family, elements } => {
                let family = self.family(*family)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "NewList shared-reference family" });
                }
                require_type(function, *destination, ValueType::Gc(family.list_type))?;
                for element in elements { require_type(function, *element, ValueType::Gc(family.owner_type))?; }
            }
            O::ListGet { destination, list, index, family } => {
                let family = self.family(*family)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ListGet shared-reference family" });
                }
                require_type(function, *list, ValueType::Gc(family.list_type))?;
                require_type(function, *index, ValueType::Size)?;
                require_type(function, *destination, ValueType::Gc(family.owner_type))?;
            }
            O::ListLength { destination, list, family } => {
                let family = self.family(*family)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ListLength shared-reference family" });
                }
                require_type(function, *list, ValueType::Gc(family.list_type))?;
                require_type(function, *destination, ValueType::Size)?;
            }
            O::CloneList { destination, list, family } => {
                let family = self.family(*family)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "CloneList shared-reference family" });
                }
                let ty = ValueType::Gc(family.list_type);
                require_type(function, *list, ty)?;
                require_type(function, *destination, ty)?;
            }
            O::ListPush { destination, list, value, family } => {
                let family = self.family(*family)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ListPush shared-reference family" });
                }
                require_type(function, *list, ValueType::Gc(family.list_type))?;
                require_type(function, *destination, ValueType::Gc(family.list_type))?;
                require_type(function, *value, ValueType::Gc(family.owner_type))?;
            }
            O::ListSet { destination, list, index, value, family } => {
                let family = self.family(*family)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ListSet shared-reference family" });
                }
                require_type(function, *list, ValueType::Gc(family.list_type))?;
                require_type(function, *destination, ValueType::Gc(family.list_type))?;
                require_type(function, *index, ValueType::Size)?;
                require_type(function, *value, ValueType::Gc(family.owner_type))?;
            }
            O::DropList { list, family } => {
                let family = self.family(*family)?;
                if matches!(family.source, OwnerFamilySource::SharedReference(_)) {
                    return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "DropList shared-reference family" });
                }
                require_type(function, *list, ValueType::Gc(family.list_type))?;
            }
        }
        Ok(())
    }
}

fn is_gc(function: &Function, value: ValueId) -> bool {
    matches!(function.value_types.get(&value), Some(ValueType::Gc(_)))
}

fn live(function: &Function, state: &Live, value: ValueId) -> Result<(), InitialCheckError> {
    if state.contains(&value) { Ok(()) } else { Err(InitialCheckError::UseAfterConsume { function: function.id, object: value }) }
}

fn consume(function: &Function, state: &mut Live, value: ValueId) -> Result<(), InitialCheckError> {
    live(function, state, value)?;
    state.remove(&value);
    Ok(())
}

fn transfer_arguments(function: &Function, state: &mut Live, arguments: &[CallArgument]) -> Result<(), InitialCheckError> {
    for argument in arguments {
        let (CallArgument::Move(value) | CallArgument::Value(value)) = argument;
        if is_gc(function, *value) {
            if !matches!(argument, CallArgument::Move(_)) {
                return Err(InitialCheckError::InvalidOwnership { function: function.id, object: *value });
            }
            consume(function, state, *value)?;
        }
    }
    Ok(())
}

/// This judgment is additional to SSA dominance and the linear-memory checker.
/// It never authenticates source identity: authority admission does that later.
pub(super) fn check(function: &Function, registry: &Registry<'_>) -> Result<(), InitialCheckError> {
    for (value, ty) in &function.value_types {
        if let ValueType::Gc(id) = ty {
            if !registry.registered(*id) { return Err(InitialCheckError::InvalidOwnership { function: function.id, object: *value }); }
        }
    }
    for block in &function.blocks {
        for operation in &block.operations {
            if let Operation::ClosureOwner(operation) = operation {
                registry.operation(function, operation)?;
            } else if !matches!(operation, Operation::Call { .. } | Operation::VarCall { .. }) {
                if let Some(value) = operation_uses(operation).into_iter().chain(operation_destinations(operation)).find(|v| is_gc(function, *v)) {
                    return Err(InitialCheckError::InvalidOwnership { function: function.id, object: value });
                }
            }
        }
    }
    let blocks = index_blocks(function)?;
    let entry = blocks.get(&function.entry).ok_or(InitialCheckError::MissingBlock { function: function.id, block: function.entry })?;
    let mut initial = Live::new();
    for parameter in &entry.parameters {
        if is_gc(function, parameter.value) {
            if parameter.ownership != Some(Ownership::Shared) { return Err(InitialCheckError::InvalidOwnership { function: function.id, object: parameter.value }); }
            initial.insert(parameter.value);
        }
    }
    let mut incoming = BTreeMap::from([(function.entry, initial)]);
    let mut pending = VecDeque::from([function.entry]);
    while let Some(id) = pending.pop_front() {
        let block = blocks[&id];
        let mut state = incoming[&id].clone();
        for operation in &block.operations {
            for value in operation_uses(operation).into_iter().filter(|v| is_gc(function, *v)) { live(function, &state, value)?; }
            match operation {
                Operation::ClosureOwner(owner) => {
                    match owner {
                        OwnerOperation::Drop { owner, .. } => consume(function, &mut state, *owner)?,
                        OwnerOperation::DropList { list, .. } => consume(function, &mut state, *list)?,
                        OwnerOperation::ListPush { list, value, .. } => {
                            consume(function, &mut state, *list)?;
                            consume(function, &mut state, *value)?;
                        }
                        OwnerOperation::ListSet { list, value, .. } => {
                            consume(function, &mut state, *list)?;
                            consume(function, &mut state, *value)?;
                        }
                        OwnerOperation::Construct { .. }
                        | OwnerOperation::ReadCapability { .. }
                        | OwnerOperation::ReadOwner { .. }
                        | OwnerOperation::ReadSharedPlace { .. }
                        | OwnerOperation::ReadTag { .. }
                        | OwnerOperation::ReadCallable { .. }
                        | OwnerOperation::Clone { .. }
                        | OwnerOperation::CloneEnvironment { .. }
                        | OwnerOperation::NewList { .. }
                        | OwnerOperation::ListGet { .. }
                        | OwnerOperation::ListLength { .. }
                        | OwnerOperation::CloneList { .. } => {}
                    }
                }
                Operation::Call { arguments, .. } | Operation::VarCall { arguments, .. } => transfer_arguments(function, &mut state, arguments)?,
                Operation::ConstInt { .. }
                | Operation::ConstFloat { .. }
                | Operation::ConstUnit { .. } | Operation::ConstBool { .. }
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
                | Operation::CloneRef { .. }
                | Operation::Retain { .. }
                | Operation::Release { .. }
                | Operation::DropRef { .. }
                | Operation::Reclaim { .. }
                | Operation::FunctionValue { .. }
                | Operation::HostCall { .. } => {}
            }
            for destination in operation_destinations(operation).into_iter().filter(|v| is_gc(function, *v)) {
                if !state.insert(destination) { return Err(InitialCheckError::InvalidOwnership { function: function.id, object: destination }); }
            }
        }
        let edges = match &block.terminator {
            Terminator::Return { values } => {
                for value in values.iter().filter(|v| is_gc(function, **v)) { consume(function, &mut state, *value)?; }
                Vec::new()
            }
            Terminator::TailCall { arguments, .. } | Terminator::VarTailCall { arguments, .. } => {
                transfer_arguments(function, &mut state, arguments)?;
                Vec::new()
            }
            Terminator::Branch { target, arguments } => vec![(*target, arguments)],
            Terminator::BranchIf { then_target, then_arguments, else_target, else_arguments, .. } => vec![(*then_target, then_arguments), (*else_target, else_arguments)],
            Terminator::Trap | Terminator::Unreachable => continue,
        };
        if edges.is_empty() {
            if let Some(object) = state.first() { return Err(InitialCheckError::UnconsumedReference { function: function.id, object: *object }); }
        }
        for (target, arguments) in edges {
            let destination = blocks.get(&target).ok_or(InitialCheckError::MissingBlock { function: function.id, block: target })?;
            require_arity("owner edge", destination.parameters.len(), arguments.len())?;
            let mut next = state.clone();
            let mut renamed = Vec::new();
            for (argument, parameter) in arguments.iter().zip(&destination.parameters) {
                if is_gc(function, *argument) {
                    require_type(function, *argument, parameter.value_type)?;
                    if parameter.ownership != Some(Ownership::Shared) { return Err(InitialCheckError::InvalidOwnership { function: function.id, object: parameter.value }); }
                    consume(function, &mut next, *argument)?;
                    renamed.push(parameter.value);
                }
            }
            for value in renamed {
                if !next.insert(value) { return Err(InitialCheckError::InvalidOwnership { function: function.id, object: value }); }
            }
            if let Some(previous) = incoming.get(&target) {
                if previous != &next {
                    let object = previous.symmetric_difference(&next).next().copied().expect("unequal sets");
                    return Err(InitialCheckError::IncompatibleMemoryJoin { function: function.id, block: target, object });
                }
            } else {
                incoming.insert(target, next);
                pending.push_back(target);
            }
        }
    }
    Ok(())
}

fn shared_int_reference(source_type: &witchy_syntax::ast::Type) -> bool {
    matches!(source_type,
        witchy_syntax::ast::Type::Qualified(
            witchy_syntax::ast::TypeQual::Borrow(lifetime), inner,
        ) if !lifetime.is_empty()
            && matches!(inner.as_ref(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "Int" && arguments.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_safewir::{BlockParameter, CallableContract, ParameterContract};

    fn fixture(operations: Vec<OwnerOperation>, results: Vec<ValueId>) -> (Function, OwnerFamily) {
        let owner_type = GcTypeId::new(0xc1000000);
        let family = OwnerFamily {
            source: OwnerFamilySource::Closure,
            owner_type, list_type: GcTypeId::new(0xc1000001),
            signature: FunctionSignatureId::new(0), declaration_signature: FunctionSignatureId::new(1),
            environment: crate::layout::LayoutId::from_bytes([0; 32]), constructors: vec![],
        };
        let mut value_types = BTreeMap::from([(ValueId::new(0), ValueType::Gc(owner_type))]);
        for operation in &operations {
            for value in operation.destinations() {
                value_types.insert(value, match operation {
                    OwnerOperation::NewList { .. } | OwnerOperation::CloneList { .. } => ValueType::Gc(family.list_type),
                    OwnerOperation::ListLength { .. } => ValueType::Size,
                    _ => ValueType::Gc(owner_type),
                });
            }
        }
        let function = Function {
            id: FunctionId::new(0), signature: family.signature, entry: BlockId::new(0), value_types,
            blocks: vec![BasicBlock { id: BlockId::new(0),
                parameters: vec![BlockParameter { value: ValueId::new(0), value_type: ValueType::Gc(owner_type), ownership: Some(Ownership::Shared) }],
                operations: operations.into_iter().map(Operation::ClosureOwner).collect(),
                terminator: Terminator::Return { values: results },
            }],
        };
        (function, family)
    }

    #[test]
    fn opaque_owner_affine_clone_drop_and_return() {
        let signature = FunctionSignatureId::new(0);
        let source = ValueId::new(0);
        let cloned = ValueId::new(1);
        let (function, family) = fixture(vec![
            OwnerOperation::Clone { destination: cloned, owner: source, family: signature },
            OwnerOperation::Drop { owner: source, family: signature },
        ], vec![cloned]);
        check(&function, &Registry { families: &[family.clone()] }).unwrap();
        let mut duplicated = function.clone();
        duplicated.blocks[0].terminator = Terminator::Return { values: vec![cloned, cloned] };
        assert!(matches!(check(&duplicated, &Registry { families: &[family.clone()] }), Err(InitialCheckError::UseAfterConsume { .. })));
        let mut reused = function;
        reused.blocks[0].operations.push(Operation::ClosureOwner(OwnerOperation::Clone { destination: ValueId::new(2), owner: source, family: signature }));
        reused.value_types.insert(ValueId::new(2), ValueType::Gc(family.owner_type));
        assert!(matches!(check(&reused, &Registry { families: &[family] }), Err(InitialCheckError::UseAfterConsume { .. })));
    }

    #[test]
    fn opaque_owner_affine_dead_list_and_wrong_family_refuse() {
        let signature = FunctionSignatureId::new(0);
        let source = ValueId::new(0);
        let list = ValueId::new(1);
        let (function, family) = fixture(vec![
            OwnerOperation::NewList { destination: list, family: signature, elements: vec![source, source] },
            OwnerOperation::Drop { owner: source, family: signature },
            OwnerOperation::DropList { list, family: signature },
            OwnerOperation::ListLength { destination: ValueId::new(2), list, family: signature },
        ], vec![]);
        assert!(matches!(check(&function, &Registry { families: &[family.clone()] }), Err(InitialCheckError::UseAfterConsume { .. })));
        let mut wrong = function;
        wrong.blocks[0].operations[0] = Operation::ClosureOwner(OwnerOperation::NewList { destination: list, family: FunctionSignatureId::new(9), elements: vec![source] });
        assert!(matches!(check(&wrong, &Registry { families: &[family] }), Err(InitialCheckError::MissingSignature(_))));
    }

    #[test]
    fn opaque_owner_affine_call_requires_move_and_no_leak() {
        let (mut function, family) = fixture(vec![], vec![]);
        let source = ValueId::new(0);
        function.blocks[0].operations.push(Operation::Call { destinations: vec![], callee: FunctionId::new(1), arguments: vec![CallArgument::Value(source)] });
        assert!(matches!(check(&function, &Registry { families: &[family.clone()] }), Err(InitialCheckError::InvalidOwnership { .. })));
        function.blocks[0].operations[0] = Operation::Call { destinations: vec![], callee: FunctionId::new(1), arguments: vec![CallArgument::Move(source)] };
        check(&function, &Registry { families: &[family.clone()] }).unwrap();
        function.blocks[0].operations.clear();
        assert!(matches!(check(&function, &Registry { families: &[family] }), Err(InitialCheckError::UnconsumedReference { .. })));
    }

    #[test]
    fn nested_owner_read_requires_exact_family_and_owner_field_storage() {
        use crate::canonical_safewir::closure_owner_contract::{CaptureOrdinal, OwnerCapture, OwnerConstructor};

        let child_type = GcTypeId::new(0xc2000000);
        let child_signature = FunctionSignatureId::new(4);
        let child = OwnerFamily {
            source: OwnerFamilySource::Closure,
            owner_type: child_type,
            list_type: GcTypeId::new(0xc2000001),
            signature: child_signature,
            declaration_signature: FunctionSignatureId::new(5),
            environment: crate::layout::LayoutId::from_bytes([1; 32]),
            constructors: vec![],
        };
        let parent_type = GcTypeId::new(0xc3000000);
        let parent_signature = FunctionSignatureId::new(6);
        let constructor = ConstructorId(9);
        let parent = OwnerFamily {
            source: OwnerFamilySource::Closure,
            owner_type: parent_type,
            list_type: GcTypeId::new(0xc3000001),
            signature: parent_signature,
            declaration_signature: FunctionSignatureId::new(7),
            environment: crate::layout::LayoutId::from_bytes([2; 32]),
            constructors: vec![OwnerConstructor {
                id: constructor,
                function: FunctionId::new(8),
                witness: Some(9),
                environment_tag: 1,
                captures: vec![OwnerCapture {
                    value_type: ValueType::Gc(child_type),
                    storage: CaptureStorage::OwnerField(2),
                    transfer: CaptureTransfer::CloneManaged,
                }],
            }],
        };
        let owner = ValueId::new(0);
        let destination = ValueId::new(1);
        let mut function = Function {
            id: FunctionId::new(8),
            signature: parent_signature,
            entry: BlockId::new(0),
            value_types: BTreeMap::from([
                (owner, ValueType::Gc(parent_type)),
                (destination, ValueType::Gc(child_type)),
            ]),
            blocks: vec![],
        };
        let operation = OwnerOperation::ReadOwner {
            destination,
            owner,
            constructor,
            capture: CaptureOrdinal(0),
        };
        Registry { families: &[child.clone(), parent.clone()] }
            .operation(&function, &operation)
            .expect("exact nested owner field");

        function.value_types.insert(destination, ValueType::Gc(parent_type));
        assert!(matches!(
            Registry { families: &[child.clone(), parent.clone()] }.operation(&function, &operation),
            Err(InitialCheckError::TypeMismatch { .. })
        ));

        function.value_types.insert(destination, ValueType::Gc(child_type));
        let mut wrong_storage = parent;
        wrong_storage.constructors[0].captures[0].storage = CaptureStorage::EnvironmentField(1);
        assert!(matches!(
            Registry { families: &[child, wrong_storage] }.operation(&function, &operation),
            Err(InitialCheckError::InvalidOwnership { .. })
        ));
    }

    #[test]
    fn aggregate_owner_tag_is_typed_affine_and_never_callable() {
        let owner_type = GcTypeId::new(0xc4000000);
        let family_token = FunctionSignatureId::new(40);
        let family = OwnerFamily {
            source: OwnerFamilySource::Aggregate(witchy_syntax::ast::Type::Named("main.Message".into(), Vec::new())),
            owner_type,
            list_type: GcTypeId::new(0xc4000001),
            signature: family_token,
            declaration_signature: FunctionSignatureId::new(41),
            environment: crate::layout::LayoutId::from_bytes([4; 32]),
            constructors: vec![crate::canonical_safewir::closure_owner_contract::OwnerConstructor {
                id: ConstructorId(400),
                function: FunctionId::new(u32::MAX),
                witness: None,
                environment_tag: 7,
                captures: Vec::new(),
            }],
        };
        let owner = ValueId::new(0);
        let tag = ValueId::new(1);
        let cloned = ValueId::new(2);
        let mut function = Function {
            id: FunctionId::new(42),
            signature: FunctionSignatureId::new(999),
            entry: BlockId::new(0),
            value_types: BTreeMap::from([
                (owner, ValueType::Gc(owner_type)),
                (tag, ValueType::Int),
                (cloned, ValueType::Gc(owner_type)),
            ]),
            blocks: vec![BasicBlock {
                id: BlockId::new(0),
                parameters: vec![BlockParameter { value: owner, value_type: ValueType::Gc(owner_type), ownership: Some(Ownership::Shared) }],
                operations: vec![
                    Operation::ClosureOwner(OwnerOperation::ReadTag { destination: tag, owner, family: family_token }),
                    Operation::ClosureOwner(OwnerOperation::Clone { destination: cloned, owner, family: family_token }),
                    Operation::ClosureOwner(OwnerOperation::Drop { owner, family: family_token }),
                    Operation::ClosureOwner(OwnerOperation::Drop { owner: cloned, family: family_token }),
                ],
                terminator: Terminator::Return { values: Vec::new() },
            }],
        };
        check(&function, &Registry { families: std::slice::from_ref(&family) }).expect("aggregate owner affine flow");

        function.value_types.insert(tag, ValueType::Callable(family_token));
        let callable = OwnerOperation::ReadCallable { destination: tag, owner, family: family_token };
        assert!(matches!(
            Registry { families: std::slice::from_ref(&family) }.operation(&function, &callable),
            Err(InitialCheckError::UnsupportedMemoryOperation { operation: "ReadCallable aggregate family", .. })
        ));
    }

    #[test]
    fn shared_place_read_requires_exact_opaque_family_and_int_result() {
        use witchy_syntax::ast::{Type, TypeQual};

        let owner_type = GcTypeId::new(0xc5000000);
        let family_token = FunctionSignatureId::new(50);
        let family = OwnerFamily {
            source: OwnerFamilySource::SharedReference(Type::Qualified(
                TypeQual::Borrow("a".into()),
                Box::new(Type::Named("Int".into(), Vec::new())),
            )),
            owner_type,
            list_type: GcTypeId::new(0xc5000001),
            signature: family_token,
            declaration_signature: FunctionSignatureId::new(51),
            environment: crate::layout::LayoutId::from_bytes([5; 32]),
            constructors: Vec::new(),
        };
        let owner = ValueId::new(0);
        let destination = ValueId::new(1);
        let mut function = Function {
            id: FunctionId::new(52),
            signature: FunctionSignatureId::new(999),
            entry: BlockId::new(0),
            value_types: BTreeMap::from([
                (owner, ValueType::Gc(owner_type)),
                (destination, ValueType::Int),
            ]),
            blocks: Vec::new(),
        };
        let operation = OwnerOperation::ReadSharedPlace { destination, owner, family: family_token };
        Registry { families: std::slice::from_ref(&family) }.operation(&function, &operation).unwrap();

        function.value_types.insert(destination, ValueType::Size);
        assert!(matches!(Registry { families: std::slice::from_ref(&family) }.operation(&function, &operation),
            Err(InitialCheckError::TypeMismatch { .. })));

        function.value_types.insert(destination, ValueType::Int);
        let closure_family = OwnerFamily { source: OwnerFamilySource::Closure, ..family };
        assert!(matches!(Registry { families: std::slice::from_ref(&closure_family) }.operation(&function, &operation),
            Err(InitialCheckError::UnsupportedMemoryOperation { operation: "ReadSharedPlace non-reference family", .. })));
    }
}
