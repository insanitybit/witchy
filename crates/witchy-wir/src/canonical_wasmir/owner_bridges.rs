//! Private, artifact-bound owner imports. Linkage IDs are only deterministic
//! encoder metadata; the runtime receives the full admitted structural recipe.
use super::*;
use crate::canonical_safewir::closure_owner_contract::{CaptureOrdinal, ConstructorId, OwnerFamily, OwnerOperation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerBridgeRecipe {
    Construct(ConstructorId),
    ReadCapability(ConstructorId, CaptureOrdinal),
    ReadCallable(crate::canonical_safewir::FunctionSignatureId),
    Clone(crate::canonical_safewir::FunctionSignatureId),
    Drop(crate::canonical_safewir::FunctionSignatureId),
    CloneEnvironment(ConstructorId),
    NewList(crate::canonical_safewir::FunctionSignatureId, u32),
    ListGet(crate::canonical_safewir::FunctionSignatureId),
    ListLength(crate::canonical_safewir::FunctionSignatureId),
    CloneList(crate::canonical_safewir::FunctionSignatureId),
    ListPush(crate::canonical_safewir::FunctionSignatureId),
    DropList(crate::canonical_safewir::FunctionSignatureId),
}

impl OwnerBridgeRecipe {
    pub(super) fn of(operation: &OwnerOperation) -> Self {
        use OwnerOperation as O;
        match operation {
            O::Construct { constructor, .. } => Self::Construct(*constructor),
            O::ReadCapability { constructor, capture, .. } => Self::ReadCapability(*constructor, *capture),
            O::ReadCallable { family, .. } => Self::ReadCallable(*family),
            O::Clone { family, .. } => Self::Clone(*family),
            O::Drop { family, .. } => Self::Drop(*family),
            O::CloneEnvironment { constructor, .. } => Self::CloneEnvironment(*constructor),
            O::NewList { family, elements, .. } => Self::NewList(*family, elements.len() as u32),
            O::ListGet { family, .. } => Self::ListGet(*family),
            O::ListLength { family, .. } => Self::ListLength(*family),
            O::CloneList { family, .. } => Self::CloneList(*family),
            O::ListPush { family, .. } => Self::ListPush(*family),
            O::DropList { family, .. } => Self::DropList(*family),
        }
    }

    pub(super) fn id(&self) -> ImportId {
        let (tag, first, second) = match self {
            Self::Construct(c) => (0, c.0, 0),
            Self::ReadCapability(c, capture) => (1, c.0, capture.0),
            Self::ReadCallable(f) => (2, f.get(), 0),
            Self::Clone(f) => (3, f.get(), 0), Self::Drop(f) => (4, f.get(), 0),
            Self::CloneEnvironment(c) => (5, c.0, 0), Self::NewList(f, count) => (6, f.get(), *count),
            Self::ListGet(f) => (7, f.get(), 0), Self::ListLength(f) => (8, f.get(), 0),
            Self::CloneList(f) => (9, f.get(), 0), Self::ListPush(f) => (10, f.get(), 0),
            Self::DropList(f) => (11, f.get(), 0),
        };
        let digest = digest_bytes(b"witchy/private-owner-import/v1", &[&[tag], &first.to_le_bytes(), &second.to_le_bytes()]);
        ImportId::new(u32::from_le_bytes(digest[..4].try_into().expect("digest prefix")) | 0x8000_0000)
    }
}

/// Only a successfully admitted artifact exposes these immutable recipes.
/// Neither a raw module nor an import name can construct this authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedOwnerBridge {
    recipe: OwnerBridgeRecipe,
    family: OwnerFamily,
    parameters: Vec<ValueType>,
    results: Vec<ValueType>,
    payload_result: bool,
    payload_type_index: u32,
    cleanup_type_index: u32,
    callable_indices: Vec<(ConstructorId, u32)>,
}

impl AdmittedOwnerBridge {
    pub fn recipe(&self) -> &OwnerBridgeRecipe { &self.recipe }
    pub fn family(&self) -> &OwnerFamily { &self.family }
    pub fn parameters(&self) -> &[ValueType] { &self.parameters }
    pub fn results(&self) -> &[ValueType] { &self.results }
    pub fn payload_result(&self) -> bool { self.payload_result }
    pub(super) fn payload_type_index(&self) -> u32 { self.payload_type_index }
    pub(super) fn cleanup_type_index(&self) -> u32 { self.cleanup_type_index }
    pub fn callable_indices(&self) -> &[(ConstructorId, u32)] { &self.callable_indices }
    pub fn name(&self) -> String { format!("b{}", self.recipe.id().get()) }
    pub fn module(&self) -> &'static str { "witchy.private.owner" }
    pub(super) fn id(&self) -> ImportId { self.recipe.id() }
}

pub(super) fn build<S: crate::canonical_safewir::ModuleStage>(source: &CanonicalModule<S>) -> Result<Vec<AdmittedOwnerBridge>, LoweringError> {
    let mut result: Vec<AdmittedOwnerBridge> = vec![];
    for function in source.functions() {
        for operation in function.blocks.iter().flat_map(|b| &b.operations) {
            let Operation::ClosureOwner(operation) = operation else { continue };
            let error = |operation| LoweringError::UnsupportedOperation { function: function.id, operation };
            let recipe = OwnerBridgeRecipe::of(operation);
            let family = match &recipe {
                OwnerBridgeRecipe::Construct(id) | OwnerBridgeRecipe::ReadCapability(id, _) | OwnerBridgeRecipe::CloneEnvironment(id) =>
                    source.owner_families().iter().find(|f| f.constructors.iter().any(|c| c.id == *id)),
                OwnerBridgeRecipe::ReadCallable(id) | OwnerBridgeRecipe::Clone(id) | OwnerBridgeRecipe::Drop(id)
                | OwnerBridgeRecipe::NewList(id, _) | OwnerBridgeRecipe::ListGet(id) | OwnerBridgeRecipe::ListLength(id)
                | OwnerBridgeRecipe::CloneList(id) | OwnerBridgeRecipe::ListPush(id) | OwnerBridgeRecipe::DropList(id) =>
                    source.owner_families().iter().find(|f| f.signature == *id),
            }.ok_or_else(|| error("unregistered owner bridge"))?;
            if family.constructors.iter().any(|c| c.captures.iter().any(|capture| !matches!(capture.value_type,
                ValueType::Capability(_) | ValueType::Ref(_) | ValueType::Int | ValueType::Float | ValueType::Bool))) {
                return Err(error("opaque owner capture storage kernel"));
            }
            let mut parameters = operation.operands().iter().map(|value| function.value_types.get(value).copied()
                .ok_or_else(|| error("owner operand type"))).collect::<Result<Vec<_>, _>>()?;
            if matches!(recipe, OwnerBridgeRecipe::Construct(_)) {
                parameters.retain(|ty| matches!(ty, ValueType::Capability(_)));
                parameters.insert(0, ValueType::Ref(family.environment));
            }
            let mut results = operation.destinations().iter().map(|value| function.value_types.get(value).copied()
                .ok_or_else(|| error("owner result type"))).collect::<Result<Vec<_>, _>>()?;
            let payload_result = matches!(recipe, OwnerBridgeRecipe::Drop(_) | OwnerBridgeRecipe::CloneEnvironment(_) | OwnerBridgeRecipe::DropList(_));
            if payload_result { results.clear(); }
            let callable_indices = family.constructors.iter().map(|constructor| {
                source.functions().iter().position(|f| f.id == constructor.function)
                    .map(|index| (constructor.id, index as u32)).ok_or_else(|| error("owner callable body"))
            }).collect::<Result<Vec<_>, _>>()?;
            let bridge = AdmittedOwnerBridge { recipe, family: family.clone(), parameters, results, callable_indices, payload_result, payload_type_index: 0, cleanup_type_index: 0 };
            if source.imports().iter().any(|import| import.id == bridge.id()) {
                return Err(error("private owner import collision"));
            }
            if let Some(previous) = result.iter().find(|previous| previous.id() == bridge.id()) {
                if previous != &bridge { return Err(error("private owner recipe collision")); }
            } else { result.push(bridge); }
        }
    }
    let first_payload = (source.signatures().len() + source.imports().len() + result.len()) as u32;
    let mut families = Vec::new();
    for bridge in &mut result {
        let index = if let Some(index) = families.iter().position(|id| *id == bridge.family.signature) { index }
            else { families.push(bridge.family.signature); families.len() - 1 };
        bridge.payload_type_index = first_payload + index as u32;
    }
    for bridge in &mut result { bridge.cleanup_type_index = first_payload + families.len() as u32; }
    Ok(result)
}

pub(super) fn append_types(types: &mut TypeSection, bridges: &[AdmittedOwnerBridge]) -> Result<(), EncodeError> {
    for bridge in bridges {
        let parameters = bridge.parameters.iter().enumerate().map(|(i, ty)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(i as u32), *ty)).collect::<Result<Vec<_>, _>>()?;
        let results = if bridge.payload_result { vec![ValType::Ref(RefType::ANYREF)] } else {
            bridge.results.iter().enumerate().map(|(i, ty)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(i as u32), *ty)).collect::<Result<Vec<_>, _>>()?
        };
        types.ty().function(parameters, results);
    }
    let mut emitted = BTreeSet::new();
    for bridge in bridges {
        if !emitted.insert(bridge.payload_type_index) { continue; }
        let slots = payload_slots(bridge.family());
        let fields = (0..slots).map(|field| wasm_encoder::FieldType {
            element_type: wasm_encoder::StorageType::Val(if field < 2 { ValType::I32 } else { ValType::EXTERNREF }), mutable: false,
        });
    types.ty().struct_(fields);
}
if bridges.iter().any(|b| matches!(b.recipe, OwnerBridgeRecipe::DropList(_))) {
    // Destruction work contains actual payload refs, never owner handles.
    types.ty().array(&wasm_encoder::StorageType::Val(ValType::Ref(RefType::ANYREF)), false);
}
Ok(())

}

pub fn payload_slots(family: &OwnerFamily) -> usize {
    family.constructors.iter().flat_map(|c| &c.captures).filter_map(|c| match c.storage {
        crate::canonical_safewir::closure_owner_contract::CaptureStorage::OwnerField(index) => Some(index as usize + 1),
        _ => None,
    }).max().unwrap_or(2)
}

pub(super) fn append_imports(imports: &mut ImportSection, bridges: &[AdmittedOwnerBridge], first_type: u32) {
    for (index, bridge) in bridges.iter().enumerate() {
        imports.import(bridge.module(), &bridge.name(), EntityType::Function(first_type + index as u32));
    }
}
