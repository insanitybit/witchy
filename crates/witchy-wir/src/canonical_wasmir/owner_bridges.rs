//! Private, artifact-bound owner imports. Linkage IDs are only deterministic
//! encoder metadata; the runtime receives the full admitted structural recipe.
use super::*;
use crate::canonical_safewir::closure_owner_contract::{CaptureOrdinal, ConstructorId, OwnerFamily, OwnerFamilySource, OwnerOperation, SharedReferenceConstructor};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerBridgeRecipe {
    Construct(ConstructorId),
    Deconstruct(ConstructorId),
    ReadCapability(ConstructorId, CaptureOrdinal),
    ReadOwner(ConstructorId, CaptureOrdinal),
    ReadTag(crate::canonical_safewir::FunctionSignatureId),
    ReadCallable(crate::canonical_safewir::FunctionSignatureId),
    Clone(crate::canonical_safewir::FunctionSignatureId),
    Drop(crate::canonical_safewir::FunctionSignatureId),
    CloneEnvironment(ConstructorId),
    NewList(crate::canonical_safewir::FunctionSignatureId, u32),
    ListGet(crate::canonical_safewir::FunctionSignatureId),
    ListLength(crate::canonical_safewir::FunctionSignatureId),
    CloneList(crate::canonical_safewir::FunctionSignatureId),
    ListPush(crate::canonical_safewir::FunctionSignatureId),
    ListSet(crate::canonical_safewir::FunctionSignatureId),
    DropList(crate::canonical_safewir::FunctionSignatureId),
}

impl OwnerBridgeRecipe {
    pub(super) fn of(operation: &OwnerOperation) -> Self {
        use OwnerOperation as O;
        match operation {
            O::Construct { constructor, .. } => Self::Construct(*constructor),
            O::Deconstruct { constructor, .. } => Self::Deconstruct(*constructor),
            O::ReadCapability { constructor, capture, .. } => Self::ReadCapability(*constructor, *capture),
            O::ReadOwner { constructor, capture, .. } => Self::ReadOwner(*constructor, *capture),
            // The operation is implemented from this family-authenticated tag
            // bridge plus one admitted CloneEnvironment bridge per constructor.
            O::ReadSharedPlace { family, .. } => Self::ReadTag(*family),
            O::ReadSharedSlice { family, .. } => Self::ReadTag(*family),
            O::ReadTag { family, .. } => Self::ReadTag(*family),
            O::ReadCallable { family, .. } => Self::ReadCallable(*family),
            O::Clone { family, .. } => Self::Clone(*family),
            O::Drop { family, .. } => Self::Drop(*family),
            O::CloneEnvironment { constructor, .. } => Self::CloneEnvironment(*constructor),
            O::NewList { family, elements, .. } => Self::NewList(*family, elements.len() as u32),
            O::ListGet { family, .. } => Self::ListGet(*family),
            O::ListLength { family, .. } => Self::ListLength(*family),
            O::CloneList { family, .. } => Self::CloneList(*family),
            O::ListPush { family, .. } => Self::ListPush(*family),
            O::ListSet { family, .. } => Self::ListSet(*family),
            O::DropList { family, .. } => Self::DropList(*family),
        }
    }

    pub(super) fn id(&self) -> ImportId {
        let (tag, first, second) = match self {
            Self::Construct(c) => (0, c.0, 0),
            Self::Deconstruct(c) => (19, c.0, 0),
            Self::ReadCapability(c, capture) => (1, c.0, capture.0),
            Self::ReadOwner(c, capture) => (12, c.0, capture.0),
            Self::ReadTag(f) => (13, f.get(), 0),
            Self::ReadCallable(f) => (2, f.get(), 0),
            Self::Clone(f) => (3, f.get(), 0), Self::Drop(f) => (4, f.get(), 0),
            Self::CloneEnvironment(c) => (5, c.0, 0), Self::NewList(f, count) => (6, f.get(), *count),
            Self::ListGet(f) => (7, f.get(), 0), Self::ListLength(f) => (8, f.get(), 0),
            Self::CloneList(f) => (9, f.get(), 0), Self::ListPush(f) => (10, f.get(), 0),
            Self::DropList(f) => (11, f.get(), 0),
            Self::ListSet(f) => (14, f.get(), 0),
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
    cleanup_result: bool,
    payload_type_index: u32,
    cleanup_item_type_index: u32,
    cleanup_type_index: u32,
    cleanup_families: Vec<CleanupFamilyRecipe>,
    callable_indices: Vec<(ConstructorId, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CleanupFamilyRecipe {
    family: OwnerFamily,
    payload_type_index: u32,
}

impl CleanupFamilyRecipe {
    pub(super) fn family(&self) -> &OwnerFamily { &self.family }
    pub(super) fn payload_type_index(&self) -> u32 { self.payload_type_index }
}

impl AdmittedOwnerBridge {
    pub fn recipe(&self) -> &OwnerBridgeRecipe { &self.recipe }
    pub fn family(&self) -> &OwnerFamily { &self.family }
    pub fn parameters(&self) -> &[ValueType] { &self.parameters }
    pub fn results(&self) -> &[ValueType] { &self.results }
    pub fn payload_result(&self) -> bool { self.payload_result }
    pub fn cleanup_result(&self) -> bool { self.cleanup_result }
    pub(super) fn requires_cleanup_helper(&self) -> bool {
        recipe_requires_cleanup_helper(&self.recipe)
    }
    pub(super) fn payload_type_index(&self) -> u32 { self.payload_type_index }
    pub(super) fn cleanup_item_type_index(&self) -> u32 { self.cleanup_item_type_index }
    pub(super) fn cleanup_type_index(&self) -> u32 { self.cleanup_type_index }
    pub(super) fn cleanup_families(&self) -> &[CleanupFamilyRecipe] { &self.cleanup_families }
    pub fn callable_indices(&self) -> &[(ConstructorId, u32)] { &self.callable_indices }
    pub fn name(&self) -> String { format!("b{}", self.recipe.id().get()) }
    pub fn module(&self) -> &'static str { "witchy.private.owner" }
    pub(super) fn id(&self) -> ImportId { self.recipe.id() }
}

pub(super) fn build<S: crate::canonical_safewir::ModuleStage>(source: &CanonicalModule<S>) -> Result<Vec<AdmittedOwnerBridge>, LoweringError> {
    let mut result: Vec<AdmittedOwnerBridge> = vec![];
    // Recipe ids are digests; compute each once and keep the ones already
    // admitted in a map, so a module with thousands of owner operations does
    // not re-hash every earlier recipe on every new one.
    let import_ids = source.imports().iter().map(|import| import.id).collect::<BTreeSet<_>>();
    let mut admitted: BTreeMap<ImportId, usize> = BTreeMap::new();
    // Families by the constructor ids and signatures the recipes name.
    let mut family_by_constructor = BTreeMap::new();
    let mut family_by_signature = BTreeMap::new();
    for family in source.owner_families() {
        family_by_signature.entry(family.signature).or_insert(family);
        for constructor in &family.constructors {
            family_by_constructor.entry(constructor.id.0).or_insert(family);
        }
    }
    for function in source.functions() {
        for operation in function.blocks.iter().flat_map(|b| &b.operations) {
            let Operation::ClosureOwner(operation) = operation else { continue };
            let error = |operation| LoweringError::UnsupportedOperation { function: function.id, operation };
            let recipe = OwnerBridgeRecipe::of(operation);
            let family = match &recipe {
                OwnerBridgeRecipe::Construct(id) | OwnerBridgeRecipe::Deconstruct(id)
                | OwnerBridgeRecipe::ReadCapability(id, _)
                | OwnerBridgeRecipe::ReadOwner(id, _) | OwnerBridgeRecipe::CloneEnvironment(id) =>
                    family_by_constructor.get(&id.0).copied(),
                OwnerBridgeRecipe::ReadTag(id) | OwnerBridgeRecipe::ReadCallable(id) | OwnerBridgeRecipe::Clone(id) | OwnerBridgeRecipe::Drop(id)
                | OwnerBridgeRecipe::NewList(id, _) | OwnerBridgeRecipe::ListGet(id) | OwnerBridgeRecipe::ListLength(id)
                | OwnerBridgeRecipe::CloneList(id) | OwnerBridgeRecipe::ListPush(id) | OwnerBridgeRecipe::ListSet(id)
                | OwnerBridgeRecipe::DropList(id) =>
                    family_by_signature.get(id).copied(),
            }.ok_or_else(|| error("unregistered owner bridge"))?;
            if family.constructors.iter().any(|c| c.captures.iter().any(|capture| !matches!(
                (capture.value_type, capture.storage),
                (ValueType::Capability(_) | ValueType::Gc(_), crate::canonical_safewir::closure_owner_contract::CaptureStorage::OwnerField(_))
                | (ValueType::Ref(_) | ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit | ValueType::Size,
                    crate::canonical_safewir::closure_owner_contract::CaptureStorage::EnvironmentField(_))
            ))) {
                return Err(error("opaque owner capture storage kernel"));
            }
            let mut parameters = operation.operands().iter().map(|value| function.value_types.get(value).copied()
                .ok_or_else(|| error("owner operand type"))).collect::<Result<Vec<_>, _>>()?;
            if matches!(recipe, OwnerBridgeRecipe::Construct(_)) {
                parameters.retain(|ty| matches!(ty, ValueType::Capability(_) | ValueType::Gc(_)));
                parameters.insert(0, ValueType::Ref(family.environment));
            }
            let results = operation.destinations().iter().map(|value| function.value_types.get(value).copied()
                .ok_or_else(|| error("owner result type"))).collect::<Result<Vec<_>, _>>()?;
            let mut results = recipe_results(&recipe, results);
            if let OwnerBridgeRecipe::ReadOwner(constructor, capture) = &recipe {
                let captured_type = family.constructors.iter()
                    .find(|candidate| candidate.id == *constructor)
                    .and_then(|candidate| candidate.captures.get(capture.0 as usize))
                    .filter(|capture| matches!(capture.storage, crate::canonical_safewir::closure_owner_contract::CaptureStorage::OwnerField(_)))
                    .map(|capture| capture.value_type);
                if !matches!(captured_type, Some(ValueType::Gc(_))) || results.as_slice() != captured_type.as_slice() {
                    return Err(error("typed nested owner result"));
                }
            }
            if let OwnerBridgeRecipe::Deconstruct(constructor) = &recipe {
                let declared = family.constructors.iter().find(|candidate| candidate.id == *constructor)
                    .ok_or_else(|| error("deconstruct constructor"))?;
                if !matches!(family.source, OwnerFamilySource::Aggregate(_))
                    || !family.constructors.iter().flat_map(|candidate| &candidate.captures)
                        .any(|capture| capture.transfer == crate::canonical_safewir::closure_owner_contract::CaptureTransfer::MoveManaged)
                    || declared.captures.iter().any(|capture| matches!(capture.storage,
                        crate::canonical_safewir::closure_owner_contract::CaptureStorage::OwnerField(_))
                        && !matches!(capture.value_type, ValueType::Gc(_)))
                {
                    return Err(error("deconstruct owner family"));
                }
                let expected = std::iter::once(ValueType::Ref(family.environment)).chain(
                    declared.captures.iter().filter(|capture| matches!(capture.storage,
                        crate::canonical_safewir::closure_owner_contract::CaptureStorage::OwnerField(_)))
                        .map(|capture| capture.value_type)).collect::<Vec<_>>();
                if parameters != [ValueType::Gc(family.owner_type)] || results != expected {
                    return Err(error("deconstruct bridge lanes"));
                }
            }
            let payload_result = matches!(recipe, OwnerBridgeRecipe::Drop(_) | OwnerBridgeRecipe::CloneEnvironment(_) | OwnerBridgeRecipe::DropList(_));
            if payload_result { results.clear(); }
            let cleanup_result = recipe_has_cleanup_result(&recipe);
            let callable_indices = if matches!(family.source,
                crate::canonical_safewir::closure_owner_contract::OwnerFamilySource::Closure)
            {
                family.constructors.iter().map(|constructor| {
                    source.functions().iter().position(|f| f.id == constructor.function)
                        .map(|index| (constructor.id, index as u32)).ok_or_else(|| error("owner callable body"))
                }).collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            let bridge = AdmittedOwnerBridge {
                recipe,
                family: family.clone(),
                parameters,
                results,
                callable_indices,
                payload_result,
                cleanup_result,
                payload_type_index: 0,
                cleanup_item_type_index: 0,
                cleanup_type_index: 0,
                cleanup_families: Vec::new(),
            };
            let id = bridge.id();
            if import_ids.contains(&id) {
                return Err(error("private owner import collision"));
            }
            if let Some(&index) = admitted.get(&id) {
                if result[index] != bridge { return Err(error("private owner recipe collision")); }
            } else {
                admitted.insert(id, result.len());
                result.push(bridge);
            }

            if matches!(operation, OwnerOperation::ReadSharedPlace { .. } | OwnerOperation::ReadSharedSlice { .. }) {
                for constructor in &family.constructors {
                    if constructor.shared_reference_constructor().is_none() {
                        return Err(error("shared-place constructor schema"));
                    }
                    let recipe = OwnerBridgeRecipe::CloneEnvironment(constructor.id);
                    let dependency = AdmittedOwnerBridge {
                        recipe,
                        family: family.clone(),
                        parameters: vec![ValueType::Gc(family.owner_type)],
                        results: Vec::new(),
                        callable_indices: Vec::new(),
                        payload_result: true,
                        cleanup_result: false,
                        payload_type_index: 0,
                        cleanup_item_type_index: 0,
                        cleanup_type_index: 0,
                        cleanup_families: Vec::new(),
                    };
                    let dependency_id = dependency.id();
                    if import_ids.contains(&dependency_id) {
                        return Err(error("private owner import collision"));
                    }
                    if let Some(&index) = admitted.get(&dependency_id) {
                        if result[index] != dependency {
                            return Err(error("private owner recipe collision"));
                        }
                    } else {
                        admitted.insert(dependency_id, result.len());
                        result.push(dependency);
                    }
                    let Some(SharedReferenceConstructor::ListIntElementFromSlice { slice, .. }) =
                        constructor.shared_reference_constructor()
                    else { continue };
                    let nested_type = match constructor.captures.get(slice.0 as usize) {
                        Some(crate::canonical_safewir::closure_owner_contract::OwnerCapture {
                            value_type: ValueType::Gc(owner_type),
                            storage: crate::canonical_safewir::closure_owner_contract::CaptureStorage::OwnerField(_),
                            ..
                        }) => *owner_type,
                        _ => return Err(error("shared-slice nested owner capture")),
                    };
                    let nested_family = source.owner_families().iter().find(|candidate| {
                        candidate.owner_type == nested_type
                            && matches!(&candidate.source, OwnerFamilySource::SharedReference(source)
                                if matches!(source, witchy_syntax::ast::Type::Qualified(
                                    witchy_syntax::ast::TypeQual::View, inner,
                                ) if matches!(inner.as_ref(), witchy_syntax::ast::Type::Slice(element)
                                    if matches!(element.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                                        if name == "Int" && arguments.is_empty()))))
                    }).ok_or_else(|| error("shared-slice nested owner family"))?;
                    let recipe = OwnerBridgeRecipe::ReadOwner(constructor.id, slice);
                    let dependency = AdmittedOwnerBridge {
                        recipe,
                        family: family.clone(),
                        parameters: vec![ValueType::Gc(family.owner_type)],
                        results: vec![ValueType::Gc(nested_type)],
                        callable_indices: Vec::new(),
                        payload_result: false,
                        cleanup_result: false,
                        payload_type_index: 0,
                        cleanup_item_type_index: 0,
                        cleanup_type_index: 0,
                        cleanup_families: Vec::new(),
                    };
                    let dependency_id = dependency.id();
                    if import_ids.contains(&dependency_id) { return Err(error("private owner import collision")); }
                    if let Some(&index) = admitted.get(&dependency_id) {
                        if result[index] != dependency { return Err(error("private owner recipe collision")); }
                    } else {
                        admitted.insert(dependency_id, result.len());
                        result.push(dependency);
                    }
                    for nested_constructor in &nested_family.constructors {
                        if !matches!(nested_constructor.shared_reference_constructor(),
                            Some(SharedReferenceConstructor::ListSlice { .. }))
                        {
                            return Err(error("shared-slice nested constructor schema"));
                        }
                        let recipe = OwnerBridgeRecipe::CloneEnvironment(nested_constructor.id);
                        let dependency = AdmittedOwnerBridge {
                            recipe,
                            family: nested_family.clone(),
                            parameters: vec![ValueType::Gc(nested_family.owner_type)],
                            results: Vec::new(),
                            callable_indices: Vec::new(),
                            payload_result: true,
                            cleanup_result: false,
                            payload_type_index: 0,
                            cleanup_item_type_index: 0,
                            cleanup_type_index: 0,
                            cleanup_families: Vec::new(),
                        };
                        let dependency_id = dependency.id();
                        if import_ids.contains(&dependency_id) { return Err(error("private owner import collision")); }
                        if let Some(&index) = admitted.get(&dependency_id) {
                            if result[index] != dependency { return Err(error("private owner recipe collision")); }
                        } else {
                            admitted.insert(dependency_id, result.len());
                            result.push(dependency);
                        }
                    }
                    let recipe = OwnerBridgeRecipe::Drop(nested_family.signature);
                    let dependency = AdmittedOwnerBridge {
                        recipe,
                        family: nested_family.clone(),
                        parameters: vec![ValueType::Gc(nested_family.owner_type)],
                        results: Vec::new(),
                        callable_indices: Vec::new(),
                        payload_result: true,
                        cleanup_result: false,
                        payload_type_index: 0,
                        cleanup_item_type_index: 0,
                        cleanup_type_index: 0,
                        cleanup_families: Vec::new(),
                    };
                    let dependency_id = dependency.id();
                    if import_ids.contains(&dependency_id) { return Err(error("private owner import collision")); }
                    if let Some(&index) = admitted.get(&dependency_id) {
                        if result[index] != dependency { return Err(error("private owner recipe collision")); }
                    } else {
                        admitted.insert(dependency_id, result.len());
                        result.push(dependency);
                    }
                }
            }
        }
    }
    let first_payload = (source.signatures().len() + source.imports().len() + result.len()) as u32;
    let mut cleanup_families = Vec::new();
    let mut cleanup_tags = BTreeSet::new();
    for family in source.owner_families() {
        if !cleanup_tags.insert(family.signature) {
            return Err(LoweringError::UnsupportedOperation {
                function: FunctionId::new(u32::MAX),
                operation: "duplicate owner cleanup family",
            });
        }
        cleanup_families.push(CleanupFamilyRecipe {
            family: family.clone(),
            payload_type_index: first_payload + cleanup_families.len() as u32,
        });
    }
    let cleanup_item_type_index = first_payload + cleanup_families.len() as u32;
    for bridge in &mut result {
        bridge.payload_type_index = cleanup_families.iter()
            .find(|candidate| candidate.family.signature == bridge.family.signature)
            .ok_or(LoweringError::UnsupportedOperation { function: FunctionId::new(u32::MAX), operation: "owner cleanup family" })?
            .payload_type_index;
        bridge.cleanup_item_type_index = cleanup_item_type_index;
        bridge.cleanup_type_index = cleanup_item_type_index + 1;
        bridge.cleanup_families = cleanup_families.clone();
    }
    Ok(result)
}

pub(super) fn append_types(types: &mut TypeSection, bridges: &[AdmittedOwnerBridge]) -> Result<(), EncodeError> {
    for bridge in bridges {
        let parameters = bridge.parameters.iter().enumerate().map(|(i, ty)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(i as u32), *ty)).collect::<Result<Vec<_>, _>>()?;
        let mut results = if bridge.payload_result { vec![ValType::Ref(RefType::ANYREF)] } else {
            bridge.results.iter().enumerate().map(|(i, ty)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(i as u32), *ty)).collect::<Result<Vec<_>, _>>()?
        };
        if bridge.cleanup_result { results.push(ValType::Ref(RefType::ANYREF)); }
        types.ty().function(parameters, results);
    }
    if let Some(bridge) = bridges.first() {
    for family in bridge.cleanup_families() {
        let slots = payload_slots(family.family());
        let fields = (0..slots).map(|field| wasm_encoder::FieldType {
            element_type: wasm_encoder::StorageType::Val(if field < 2 { ValType::I32 } else { ValType::EXTERNREF }), mutable: false,
        });
    types.ty().struct_(fields);
}
}
if bridges.iter().any(AdmittedOwnerBridge::requires_cleanup_helper) {
    types.ty().struct_([
        wasm_encoder::FieldType {
            element_type: wasm_encoder::StorageType::Val(ValType::I32),
            mutable: false,
        },
        wasm_encoder::FieldType {
            element_type: wasm_encoder::StorageType::Val(ValType::Ref(RefType::ANYREF)),
            mutable: false,
        },
    ]);
    types.ty().array(&wasm_encoder::StorageType::Val(ValType::Ref(RefType::ANYREF)), false);
}

Ok(())

}

fn recipe_has_cleanup_result(recipe: &OwnerBridgeRecipe) -> bool {
    matches!(recipe, OwnerBridgeRecipe::ListPush(_) | OwnerBridgeRecipe::ListSet(_))
}

fn recipe_results(recipe: &OwnerBridgeRecipe, results: Vec<ValueType>) -> Vec<ValueType> {
    // ReadSharedPlace and ReadSharedSlice are implemented by composing
    // the same private family-tag bridge with constructor-specific environment
    // access. The bridge returns the intrinsic i64 tag, never the consuming
    // operation's public result lane (which is Size for a slice length).
    if matches!(recipe, OwnerBridgeRecipe::ReadTag(_)) {
        vec![ValueType::Int]
    } else {
        results
    }
}

fn recipe_requires_cleanup_helper(recipe: &OwnerBridgeRecipe) -> bool {
    recipe_has_cleanup_result(recipe)
        || matches!(recipe, OwnerBridgeRecipe::Drop(_) | OwnerBridgeRecipe::DropList(_))
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
