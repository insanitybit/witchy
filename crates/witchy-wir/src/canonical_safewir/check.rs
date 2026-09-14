use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;

use super::{
    BasicBlock, BlockId, CallArgument, CanonicalAbiCatalog, CanonicalModule, Function, FunctionId,
    FunctionSignature, FunctionSignatureId, GuardKind, HostCallArgument, ImportContract, ImportId, InitialValid,
    LoweredCandidate, ModuleStage, Operation, OptimizationCandidate,
    ParameterOwnership, ProperTailLocation, ProperTailSite, TailSiteId, Terminator, ValueId,
    ValueType,
};

mod memory;
use memory::MemoryChecker;
mod contracts;
mod host;
mod owners;

#[cfg(test)]
#[path = "check/branch_width_tests.rs"]
mod branch_width_tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialCheckError {
    InvalidLayoutBundle(String),
    UnknownLayout(crate::layout::LayoutId),
    InvalidExtent { function: FunctionId, object: ValueId },
    InvalidProjection { function: FunctionId, object: ValueId },
    MissingBoundsGuard {
        function: FunctionId,
        index: ValueId,
        count: ValueId,
    },
    MissingCapacityGuard {
        function: FunctionId,
        count: ValueId,
        capacity: ValueId,
    },
    MissingAllocationSizeGuard {
        function: FunctionId,
        capacity: ValueId,
        maximum: u64,
    },
    InvalidGuard {
        function: FunctionId,
        condition: ValueId,
        kind: GuardKind,
    },
    InvalidOwnership { function: FunctionId, object: ValueId },
    UnconsumedReference { function: FunctionId, object: ValueId },
    InvalidAllocationClass { function: FunctionId, object: ValueId },
    UseAfterConsume { function: FunctionId, object: ValueId },
    IncompatibleMemoryJoin { function: FunctionId, block: BlockId, object: ValueId },
    MemoryDataflowNonConvergence { function: FunctionId },
    UnsupportedMemoryControlFlow { function: FunctionId },
    DuplicateSignature(FunctionSignatureId),
    DuplicateFunction(FunctionId),
    DuplicateBlock { function: FunctionId, block: BlockId },
    DuplicateValue { function: FunctionId, value: ValueId },
    MissingSignature(FunctionSignatureId),
    MissingFunction(FunctionId),
    MissingBlock { function: FunctionId, block: BlockId },
    MissingValueType { function: FunctionId, value: ValueId },
    UndeclaredValue { function: FunctionId, value: ValueId },
    ValueDoesNotDominateUse {
        function: FunctionId,
        value: ValueId,
        block: BlockId,
    },
    TypeMismatch {
        function: FunctionId,
        value: ValueId,
        expected: ValueType,
        actual: ValueType,
    },
    ArityMismatch { context: &'static str, expected: usize, actual: usize },
    DuplicateTailObligation(TailSiteId),
    TailManifestDigestMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
        actual_site_count: usize,
    },
    CheckedSourceWitnessMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
    MissingTailObligation(TailSiteId),
    MissingTailLocation(TailSiteId),
    UnexpectedTailLocation(TailSiteId),
    DuplicateTailLocation(TailSiteId),
    UnexpectedTailSite(TailSiteId),
    TailSiteFunctionMismatch {
        site: TailSiteId,
        expected: FunctionId,
        actual: FunctionId,
    },
    TailSiteBlockMismatch {
        site: TailSiteId,
        expected: BlockId,
        actual: BlockId,
    },
    UnsupportedMemoryOperation { function: FunctionId, operation: &'static str },
    DuplicateStaticDataId(crate::canonical_safewir::StaticDataId),
    MissingStaticDataId { function: FunctionId, data: crate::canonical_safewir::StaticDataId },
    UnexpectedStaticDataId(crate::canonical_safewir::StaticDataId),
    InconsistentStaticDataUse(crate::canonical_safewir::StaticDataId),
    StaticDataExtentMismatch { function: FunctionId, object: ValueId },
    InvalidStaticDataEncoding { function: FunctionId, object: ValueId },
    PointerBearingStaticData { function: FunctionId, object: ValueId },
    WritableStaticData { function: FunctionId, object: ValueId },
    UnsupportedHostCall { function: FunctionId },
    DuplicateImport(ImportId),
    MissingImportContract { function: FunctionId, import: ImportId },
    UnknownImportIdentity { import: ImportId },
    HostContractRoleMismatch { import: ImportId },
    HostParameterRoleMismatch { function: FunctionId, import: ImportId, parameter: usize },
    HostResultRoleMismatch {
        function: FunctionId,
        import: ImportId,
        result: usize,
        expected: ValueType,
        actual: ValueType,
    },
    HostBufferExtentMismatch { function: FunctionId, import: ImportId, parameter: usize },
    UnknownHostBufferExtent { import: ImportId },
    UnsupportedHostMutation { import: ImportId },
    UnsupportedHostReentrancy { import: ImportId },
    MissingCanonicalAbiCatalog,
    InvalidCanonicalAbiContract { import: ImportId },
    UnsupportedHostBuffer { function: FunctionId, import: ImportId, parameter: usize },
    UnsupportedBorrowArgument { function: FunctionId },
    UnsupportedIndirectCall { function: FunctionId },
    UnsupportedMemorySignature { signature: FunctionSignatureId },
    CallableContractMismatch { signature: FunctionSignatureId },
    UnknownCallableContract { function: FunctionId },
    UnsupportedInlineAggregateAbi { signature: FunctionSignatureId },
    UnsupportedInlineAggregateControlFlow { function: FunctionId },
    InvalidInlineAggregateLayout { function: FunctionId, layout: crate::layout::LayoutId },
    /// A callable lane was reached on an object whose layout the module's
    /// closure registry does not name, or with a value type other than the
    /// registered `Callable(sig)`.
    InvalidClosureSlot { function: FunctionId, object: ValueId },
    UnsupportedScalarOperation { function: FunctionId, operation: &'static str },
}

impl fmt::Display for InitialCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR initial validation failed: {self:?}")
    }
}

impl Error for InitialCheckError {}

/// Validate the currently implemented canonical v1 subset.
///
/// The memory slice is deliberately small and fail-closed: generic allocation,
/// counted collections, projected scalar access, explicit runtime guards, and
/// structural ownership are admitted; loans and pointer-bearing copies are not.
pub fn check_initial(
    mut candidate: CanonicalModule<LoweredCandidate>,
) -> Result<CanonicalModule<InitialValid>, InitialCheckError> {
    install_body_derived_result_contracts(&mut candidate)?;
    check_candidate(&candidate, candidate.abi_catalog())?;
    Ok(candidate.into_initial_valid())
}

#[cfg(test)]
pub(crate) fn check_initial_with_catalog(
    mut candidate: CanonicalModule<LoweredCandidate>,
    catalog: &CanonicalAbiCatalog,
) -> Result<CanonicalModule<InitialValid>, InitialCheckError> {
    install_body_derived_result_contracts(&mut candidate)?;
    check_candidate(&candidate, Some(catalog))?;
    Ok(candidate.into_initial_valid())
}

fn install_body_derived_result_contracts(
    candidate: &mut CanonicalModule<LoweredCandidate>,
) -> Result<(), InitialCheckError> {
    let signatures = index_signatures(candidate.signatures())?;
    for signature in signatures.values() {
        if signature.parameters.iter().chain(&signature.results).any(|value| matches!(value, ValueType::InlineAggregate(_))) {
            return Err(InitialCheckError::UnsupportedInlineAggregateAbi { signature: signature.id });
        }
    }
    let functions = index_functions(candidate.functions())?;
    let derived = contracts::derive_result_contracts(&functions, &signatures)?;
    for signature in &mut candidate.signatures {
        if let Some(results) = derived.get(&signature.id) {
            if signature.contract.results.len() != results.len() {
                return Err(InitialCheckError::CallableContractMismatch {
                    signature: signature.id,
                });
            }
            for ((declared, derived), value_type) in signature
                .contract
                .results
                .iter_mut()
                .zip(results)
                .zip(&signature.results)
            {
                match declared {
                    super::ResultContract::BodyDerived(declared_type)
                        if declared_type == value_type =>
                    {
                        declared.clone_from(derived);
                    }
                    super::ResultContract::BodyDerived(_) => {
                        return Err(InitialCheckError::CallableContractMismatch {
                            signature: signature.id,
                        });
                    }
                    // CheckedValue erases whether a returned reference owns
                    // a fresh/shared token or names immutable storage. That
                    // ambiguity can hide the caller's release obligation.
                    super::ResultContract::CheckedValue { .. } => {
                        return Err(InitialCheckError::CallableContractMismatch {
                            signature: signature.id,
                        });
                    }
                    explicit if explicit != derived => {
                        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                            eprintln!(
                                "canonical contract mismatch: signature {:?} declared {explicit:?} derived {derived:?}",
                                signature.id
                            );
                        }
                        return Err(InitialCheckError::CallableContractMismatch {
                            signature: signature.id,
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn recheck_final(
    candidate: &CanonicalModule<InitialValid>,
) -> Result<(), InitialCheckError> {
    check_candidate(candidate, candidate.abi_catalog())
}

/// Re-establish every canonical invariant over an optimizer-produced graph.
/// The candidate has no authority until this traversal succeeds.
pub(super) fn check_optimization_candidate(
    candidate: CanonicalModule<OptimizationCandidate>,
) -> Result<CanonicalModule<InitialValid>, InitialCheckError> {
    check_candidate(&candidate, candidate.abi_catalog())?;
    Ok(candidate.into_stage())
}

fn check_candidate<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    catalog: Option<&CanonicalAbiCatalog>,
) -> Result<(), InitialCheckError> {
    let (_, layouts) =
        crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes())
            .map_err(|error| InitialCheckError::InvalidLayoutBundle(error.to_string()))?;
    if has_unadmitted_nullable_owned_slot(&layouts) {
        return Err(InitialCheckError::InvalidLayoutBundle(
            "nullable owned slots require canonical presence and destruction support".into(),
        ));
    }
    check_static_data(candidate, &layouts)?;
    let signatures = index_signatures(candidate.signatures())?;
    let imports = index_imports(candidate.imports())?;
    let functions = index_functions(candidate.functions())?;
    contracts::check_all(&functions, &signatures)?;
    let owners = owners::Registry::new(candidate)?;
    for function in candidate.functions() {
        owners::check(function, &owners)?;
    }
    host::check_imports(&imports, &signatures, catalog)?;
    let tail_obligations = index_tail_obligations(
        candidate.proper_tail_sites(),
        candidate.proper_tail_locations(),
    )?;
    let mut observed_tail_sites = BTreeSet::new();
    let closure_layouts = candidate.closure_layouts();

    for function in candidate.functions() {
        check_function(
            function,
            &signatures,
            &imports,
            catalog,
            &functions,
            &tail_obligations,
            &mut observed_tail_sites,
            &layouts,
            closure_layouts,
        )?;
    }

    for site in tail_obligations.keys() {
        if !observed_tail_sites.contains(site) {
            return Err(InitialCheckError::MissingTailObligation(*site));
        }
    }

    Ok(())
}

fn has_unadmitted_nullable_owned_slot(layouts: &crate::layout::LayoutInterner) -> bool {
    use crate::layout::{FieldKind, LayoutKind, PointerSlot};
    // One pass collects who carries whom, so the per-slot questions below are
    // set lookups rather than rescans of the whole catalog.
    let mut fixed_fields = BTreeSet::new();
    let mut packed_elements = BTreeSet::new();
    for (_, parent) in layouts.iter() {
        match parent.kind() {
            LayoutKind::PackedRecord { fields } | LayoutKind::Tuple { fields } => fixed_fields.extend(fields.iter().copied()),
            _ => {}
        }
        if let Some(element) = parent.kind().packed_element_layout() {
            packed_elements.insert(element);
        }
    }
    layouts.iter().any(|(id, descriptor)| {
        match descriptor.kind() {
            LayoutKind::RuntimePointerBuffer { semantics: PointerSlot::OwnedNullable, .. } => true,
            LayoutKind::RuntimePointerValueSlot { semantics: PointerSlot::OwnedNullable, .. } => {
                // The first admitted carrier is a typed fixed field. A naked
                // nullable slot, or a nullable collection element, is not a
                // first-class reference or a supported range operation.
                !fixed_fields.contains(&id) || packed_elements.contains(&id)
            }
            LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. } => false,
            _ => descriptor.fields().iter().any(|field| matches!(field.kind(),
                FieldKind::Pointer { slot: PointerSlot::OwnedNullable, .. })),
        }
    })
}

fn check_static_data<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    layouts: &crate::layout::LayoutInterner,
) -> Result<(), InitialCheckError> {
    let mut segments = BTreeMap::new();
    for segment in candidate.static_data_segments() {
        if segments.insert(segment.id(), segment).is_some() {
            return Err(InitialCheckError::DuplicateStaticDataId(segment.id()));
        }
    }
    let mut referenced = BTreeSet::new();
    let mut bindings = BTreeMap::new();
    for function in candidate.functions() {
        let constants: BTreeMap<_, _> = function
            .blocks
            .iter()
            .flat_map(|block| block.operations.iter())
            .filter_map(|operation| match operation {
                Operation::ConstSize { destination, value } => Some((*destination, *value)),
                _ => None,
            })
            .collect();
        for block in &function.blocks {
            for operation in &block.operations {
                let Operation::StaticData { destination, layout, extent, data } = operation else {
                    continue;
                };
                let segment = segments.get(data).ok_or(InitialCheckError::MissingStaticDataId {
                    function: function.id,
                    data: *data,
                })?;
                referenced.insert(*data);
                let binding = normalized_static_data_use(*layout, extent, &constants, layouts)
                    .ok_or(InitialCheckError::StaticDataExtentMismatch {
                        function: function.id,
                        object: *destination,
                    })?;
                if bindings.insert(*data, binding.clone()).is_some_and(|previous| previous != binding) {
                    return Err(InitialCheckError::InconsistentStaticDataUse(*data));
                }
                if layout_contains_pointer(*layout, layouts, &mut Vec::new()) {
                    return Err(InitialCheckError::PointerBearingStaticData {
                        function: function.id,
                        object: *destination,
                    });
                }
                if !static_data_extent_matches(*layout, extent, segment.bytes(), &constants, layouts) {
                    return Err(InitialCheckError::StaticDataExtentMismatch {
                        function: function.id,
                        object: *destination,
                    });
                }
                if !static_data_payload_is_canonical(*layout, extent, segment.bytes(), &constants, layouts) {
                    return Err(InitialCheckError::InvalidStaticDataEncoding {
                        function: function.id,
                        object: *destination,
                    });
                }
            }
        }
    }
    if let Some(id) = segments.keys().find(|id| !referenced.contains(id)) {
        return Err(InitialCheckError::UnexpectedStaticDataId(*id));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedStaticDataUse {
    layout: crate::layout::LayoutId,
    extent: NormalizedStaticExtent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NormalizedStaticExtent {
    FixedBytes(u64),
    Counted { header_bytes: u32, count: u64, capacity: u64, stride_bytes: u32 },
}

fn normalized_static_data_use(
    layout: crate::layout::LayoutId,
    extent: &super::Extent,
    constants: &BTreeMap<ValueId, u64>,
    layouts: &crate::layout::LayoutInterner,
) -> Option<NormalizedStaticDataUse> {
    let layout = layouts.resolve_nominal(layout).ok()?;
    let extent = match extent {
        super::Extent::FixedBytes(bytes) => NormalizedStaticExtent::FixedBytes(*bytes),
        super::Extent::Counted { header_bytes, count, capacity, stride_bytes } => {
            NormalizedStaticExtent::Counted {
                header_bytes: *header_bytes,
                count: *constants.get(count)?,
                capacity: *constants.get(capacity)?,
                stride_bytes: *stride_bytes,
            }
        }
        super::Extent::HeaderPrefix { .. } => return None,
    };
    Some(NormalizedStaticDataUse { layout, extent })
}

fn layout_contains_pointer(
    layout: crate::layout::LayoutId,
    layouts: &crate::layout::LayoutInterner,
    visiting: &mut Vec<crate::layout::LayoutId>,
) -> bool {
    let Ok(layout) = layouts.resolve_nominal(layout) else { return true };
    if visiting.contains(&layout) {
        return true;
    }
    let Some(descriptor) = layouts.get(layout) else {
        return true;
    };
    visiting.push(layout);
    let result = descriptor.fields().iter().any(|field| match field.kind() {
        crate::layout::FieldKind::Scalar(_) => false,
        crate::layout::FieldKind::Pointer { .. } => true,
        crate::layout::FieldKind::Inline(child) => layout_contains_pointer(child, layouts, visiting),
    });
    visiting.pop();
    result
}

fn static_data_extent_matches(
    layout: crate::layout::LayoutId,
    extent: &super::Extent,
    bytes: &[u8],
    constants: &BTreeMap<ValueId, u64>,
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let Ok(layout) = layouts.resolve_nominal(layout) else { return false };
    let Some(descriptor) = layouts.get(layout) else {
        return false;
    };
    match (extent, descriptor.size()) {
        (super::Extent::FixedBytes(expected), crate::layout::LayoutSize::Fixed(actual)) => {
            *expected == u64::from(actual) && bytes.len() == actual as usize
        }
        (
            super::Extent::Counted { header_bytes, count, capacity, stride_bytes },
            crate::layout::LayoutSize::Dynamic { base, stride },
        ) => {
            if let crate::layout::LayoutKind::RuntimeBuffer { element } = descriptor.kind() {
                let Some((length_offset, data_offset)) = runtime_buffer_offsets(descriptor, *element) else { return false };
                let Ok(element) = layouts.resolve_nominal(*element) else { return false };
                let Some(element) = layouts.get(element) else { return false };
                let crate::layout::LayoutKind::Scalar(kind) = element.kind() else { return false };
                let Some(count) = constants.get(count) else { return false; };
                let Some(capacity) = constants.get(capacity) else { return false; };
                let Some(total) = usize::try_from(data_offset)
                    .ok()
                    .and_then(|base| capacity.checked_mul(u64::from(*stride_bytes)).and_then(|body| usize::try_from(body).ok()).and_then(|body| base.checked_add(body)))
                else {
                    return false;
                };
                let Some(length_end) = (length_offset as usize).checked_add(4) else { return false };
            return base == data_offset
                && *header_bytes == data_offset
                    && *stride_bytes == stride
                    && *stride_bytes == kind.size()
                    && *count == *capacity
                    && *count <= u64::from(u32::MAX)
                    && bytes.len() == total
                    && bytes.get(length_offset as usize..length_end).is_some_and(|header| header == (*count as u32).to_le_bytes().as_slice());
            }
            let crate::layout::HeaderLayout::PackedList { length_offset, capacity_offset, data_offset, .. } = descriptor.header() else {
                return false;
            };
            let (Some(count), Some(capacity)) = (constants.get(count), constants.get(capacity)) else {
                return false;
            };
            let Some(body_bytes) = capacity.checked_mul(u64::from(*stride_bytes)) else {
                return false;
            };
            let Some(end) = u64::from(*header_bytes).checked_add(body_bytes) else {
                return false;
            };
            let header_end = usize::try_from(*header_bytes).ok();
            let length_end = usize::try_from(length_offset).ok().and_then(|offset| offset.checked_add(4));
            let capacity_end = usize::try_from(capacity_offset).ok().and_then(|offset| offset.checked_add(4));
            *header_bytes == base
                && *stride_bytes == stride
                && *count <= *capacity
                && *count <= u64::from(u32::MAX)
                && *capacity <= u64::from(u32::MAX)
                && end == bytes.len() as u64
                && header_end == Some(data_offset as usize)
                && length_end.is_some_and(|end| end <= bytes.len())
                && capacity_end.is_some_and(|end| end <= bytes.len())
                && u32::from_le_bytes(bytes[length_offset as usize..length_offset as usize + 4].try_into().unwrap()) == *count as u32
                && u32::from_le_bytes(bytes[capacity_offset as usize..capacity_offset as usize + 4].try_into().unwrap()) == *capacity as u32
        }
        _ => false,
    }
}

fn runtime_buffer_offsets(
    descriptor: &crate::layout::LayoutDescriptor,
    element: crate::layout::LayoutId,
) -> Option<(u32, u32)> {
    let [length, payload] = descriptor.fields() else { return None };
    if length.offset() != 0
        || length.kind() != crate::layout::FieldKind::Scalar(crate::layout::ScalarKind::U32)
        || payload.kind() != crate::layout::FieldKind::Inline(element)
    {
        return None;
    }
    Some((length.offset(), payload.offset()))
}

fn static_data_payload_is_canonical(
    layout: crate::layout::LayoutId,
    extent: &super::Extent,
    bytes: &[u8],
    constants: &BTreeMap<ValueId, u64>,
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let Ok(layout) = layouts.resolve_nominal(layout) else { return false };
    let Some(descriptor) = layouts.get(layout) else { return false };
    match extent {
        super::Extent::FixedBytes(_) => canonical_fixed_layout_bytes(layout, bytes, layouts, &mut Vec::new()),
        super::Extent::Counted { header_bytes, count, stride_bytes, .. } => {
            let Some(count) = constants.get(count).copied() else { return false };
            let (element, data_offset) = match descriptor.kind() {
                crate::layout::LayoutKind::RuntimeBuffer { element } => {
                    let Some((_, data_offset)) = runtime_buffer_offsets(descriptor, *element) else { return false };
                    if data_offset != *header_bytes {
                        return false;
                    }
                    (*element, data_offset)
                }
                crate::layout::LayoutKind::PackedList { element, .. } => {
                    let crate::layout::HeaderLayout::PackedList { data_offset, .. } = descriptor.header() else { return false };
                    (*element, data_offset)
                }
                _ => return false,
            };
            let Ok(element) = layouts.resolve_nominal(element) else { return false };
            let Some(element_descriptor) = layouts.get(element) else { return false };
            let crate::layout::LayoutSize::Fixed(element_size) = element_descriptor.size() else { return false };
            if element_size != *stride_bytes {
                return false;
            }
            (0..count).all(|index| {
                let Some(start) = index
                    .checked_mul(u64::from(*stride_bytes))
                    .and_then(|offset| u64::from(data_offset).checked_add(offset))
                    .and_then(|offset| usize::try_from(offset).ok())
                else { return false };
                let Some(end) = start.checked_add(element_size as usize) else { return false };
                bytes.get(start..end).is_some_and(|element_bytes| {
                    canonical_fixed_layout_bytes(element, element_bytes, layouts, &mut Vec::new())
                })
            })
        }
        super::Extent::HeaderPrefix { .. } => false,
    }
}

fn canonical_fixed_layout_bytes(
    layout: crate::layout::LayoutId,
    bytes: &[u8],
    layouts: &crate::layout::LayoutInterner,
    visiting: &mut Vec<crate::layout::LayoutId>,
) -> bool {
    let Ok(layout) = layouts.resolve_nominal(layout) else { return false };
    if visiting.contains(&layout) {
        return false;
    }
    let Some(descriptor) = layouts.get(layout) else { return false };
    let crate::layout::LayoutSize::Fixed(size) = descriptor.size() else { return false };
    if bytes.len() != size as usize {
        return false;
    }
    if let crate::layout::LayoutKind::Scalar(kind) = descriptor.kind() {
        return canonical_scalar_bytes(*kind, bytes);
    }
    if !matches!(descriptor.kind(), crate::layout::LayoutKind::Tuple { .. } | crate::layout::LayoutKind::PackedRecord { .. }) {
        return false;
    }
    visiting.push(layout);
    let valid = descriptor.fields().iter().all(|field| {
        let start = field.offset() as usize;
        match field.kind() {
            crate::layout::FieldKind::Scalar(kind) => {
                let Some(end) = start.checked_add(kind.size() as usize) else { return false };
                bytes.get(start..end).is_some_and(|field_bytes| canonical_scalar_bytes(kind, field_bytes))
            }
            crate::layout::FieldKind::Inline(child) => {
                let Ok(child) = layouts.resolve_nominal(child) else { return false };
                let Some(crate::layout::LayoutSize::Fixed(child_size)) = layouts.get(child).map(|child| child.size()) else { return false };
                let Some(end) = start.checked_add(child_size as usize) else { return false };
                bytes.get(start..end).is_some_and(|field_bytes| canonical_fixed_layout_bytes(child, field_bytes, layouts, visiting))
            }
            crate::layout::FieldKind::Pointer { .. } => false,
        }
    });
    visiting.pop();
    valid
}

fn canonical_scalar_bytes(kind: crate::layout::ScalarKind, bytes: &[u8]) -> bool {
    if bytes.len() != kind.size() as usize {
        return false;
    }
    match kind {
        crate::layout::ScalarKind::Unit => bytes == [0; 4],
        crate::layout::ScalarKind::Bool => matches!(bytes, [0] | [1]),
        crate::layout::ScalarKind::BoolValueSlot => {
            matches!(u64::from_le_bytes(bytes.try_into().expect("checked scalar width")), 0 | 1)
        }
        crate::layout::ScalarKind::Callable => false,
        crate::layout::ScalarKind::Int
        | crate::layout::ScalarKind::Float
        | crate::layout::ScalarKind::Duration
        | crate::layout::ScalarKind::U32
        | crate::layout::ScalarKind::Tag8
        | crate::layout::ScalarKind::Tag16
        | crate::layout::ScalarKind::Tag32
        | crate::layout::ScalarKind::RawByte
        | crate::layout::ScalarKind::V128 => true,
    }
}

#[test]
fn nominal_static_data_checks_resolve_physical_storage() {
    use crate::layout::{LayoutInterner, PointerSlot, ScalarKind};
    let mut layouts = LayoutInterner::new();
    let recursive = layouts.intern_opaque_nominal([41; 32]).unwrap();
    let slot = layouts.intern_runtime_pointer_value_slot(recursive, PointerSlot::OwnedNullable).unwrap();
    let body = layouts.intern_packed_record(vec![slot]).unwrap();
    layouts.bind_nominal(recursive, body).unwrap();
    assert!(layout_contains_pointer(recursive, &layouts, &mut Vec::new()));
    assert!(!static_data_extent_matches(recursive, &super::Extent::FixedBytes(0), &[], &BTreeMap::new(), &layouts), "opaque identity size is not physical storage extent");

    let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let scalar_body = layouts.intern_packed_record(vec![int]).unwrap();
    let scalar_nominal = layouts.intern_opaque_nominal([42; 32]).unwrap();
    layouts.bind_nominal(scalar_nominal, scalar_body).unwrap();
    assert!(!layout_contains_pointer(scalar_nominal, &layouts, &mut Vec::new()));
    assert!(static_data_extent_matches(scalar_nominal, &super::Extent::FixedBytes(8), &[0; 8], &BTreeMap::new(), &layouts));
    assert!(!static_data_extent_matches(scalar_nominal, &super::Extent::FixedBytes(0), &[], &BTreeMap::new(), &layouts));

    let unbound = layouts.intern_opaque_nominal([43; 32]).unwrap();
    assert!(layout_contains_pointer(unbound, &layouts, &mut Vec::new()));
    assert!(!static_data_extent_matches(unbound, &super::Extent::FixedBytes(0), &[], &BTreeMap::new(), &layouts));
}

fn index_signatures(
    signatures: &[FunctionSignature],
) -> Result<BTreeMap<FunctionSignatureId, &FunctionSignature>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for signature in signatures {
        if result.insert(signature.id, signature).is_some() {
            return Err(InitialCheckError::DuplicateSignature(signature.id));
        }
    }
    Ok(result)
}

/// The candidate's functions by id, plus the same functions grouped by
/// signature so an indirect call site finds its possible targets without
/// scanning every function on every pass.
pub(super) struct FunctionIndex<'a> {
    by_id: BTreeMap<FunctionId, &'a Function>,
    by_signature: BTreeMap<FunctionSignatureId, Vec<FunctionId>>,
}

impl<'a> FunctionIndex<'a> {
    /// The functions declaring `signature`, in id order.
    pub(super) fn with_signature(&self, signature: FunctionSignatureId) -> impl Iterator<Item = &'a Function> + '_ {
        self.by_signature
            .get(&signature)
            .into_iter()
            .flatten()
            .filter_map(move |id| self.by_id.get(id).copied())
    }
}

impl<'a> FromIterator<(FunctionId, &'a Function)> for FunctionIndex<'a> {
    fn from_iter<I: IntoIterator<Item = (FunctionId, &'a Function)>>(iter: I) -> Self {
        let mut by_id = BTreeMap::new();
        let mut by_signature: BTreeMap<FunctionSignatureId, Vec<FunctionId>> = BTreeMap::new();
        for (id, function) in iter {
            if by_id.insert(id, function).is_none() {
                by_signature.entry(function.signature).or_default().push(id);
            }
        }
        Self { by_id, by_signature }
    }
}

impl<'a> std::ops::Deref for FunctionIndex<'a> {
    type Target = BTreeMap<FunctionId, &'a Function>;

    fn deref(&self) -> &Self::Target {
        &self.by_id
    }
}

fn index_functions(
    functions: &[Function],
) -> Result<FunctionIndex<'_>, InitialCheckError> {
    let mut by_id = BTreeMap::new();
    let mut by_signature: BTreeMap<FunctionSignatureId, Vec<FunctionId>> = BTreeMap::new();
    for function in functions {
        if by_id.insert(function.id, function).is_some() {
            return Err(InitialCheckError::DuplicateFunction(function.id));
        }
        by_signature.entry(function.signature).or_default().push(function.id);
    }
    Ok(FunctionIndex { by_id, by_signature })
}

fn index_imports(
    imports: &[ImportContract],
) -> Result<BTreeMap<ImportId, &ImportContract>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for import in imports {
        if result.insert(import.id, import).is_some() {
            return Err(InitialCheckError::DuplicateImport(import.id));
        }
    }
    Ok(result)
}

fn index_tail_obligations(
    obligations: &[ProperTailSite],
    locations: &[ProperTailLocation],
) -> Result<BTreeMap<TailSiteId, ProperTailLocation>, InitialCheckError> {
    let mut expected_functions = BTreeMap::new();
    for obligation in obligations {
        let discharger = obligation.discharged_by.unwrap_or(obligation.function);
        if expected_functions.insert(obligation.id, discharger).is_some() {
            return Err(InitialCheckError::DuplicateTailObligation(obligation.id));
        }
    }
    let mut result = BTreeMap::new();
    let mut occupied = BTreeSet::new();
    for location in locations {
        let site = location.site();
        let expected = expected_functions
            .get(&site)
            .copied()
            .ok_or(InitialCheckError::UnexpectedTailLocation(site))?;
        if expected != location.function() {
            return Err(InitialCheckError::TailSiteFunctionMismatch {
                site,
                expected,
                actual: location.function(),
            });
        }
        if result.insert(site, *location).is_some()
            || !occupied.insert((location.function(), location.block()))
        {
            return Err(InitialCheckError::DuplicateTailLocation(site));
        }
    }
    for site in expected_functions.keys() {
        if !result.contains_key(site) {
            return Err(InitialCheckError::MissingTailLocation(*site));
        }
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn check_function(
    function: &Function,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    imports: &BTreeMap<ImportId, &ImportContract>,
    catalog: Option<&CanonicalAbiCatalog>,
    functions: &FunctionIndex,
    tail_obligations: &BTreeMap<TailSiteId, ProperTailLocation>,
    observed_tail_sites: &mut BTreeSet<TailSiteId>,
    layouts: &crate::layout::LayoutInterner,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, FunctionSignatureId>,
) -> Result<(), InitialCheckError> {
    let signature = signatures
        .get(&function.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(function.signature))?;

    let blocks = index_blocks(function)?;
    let entry = blocks
        .get(&function.entry)
        .copied()
        .ok_or(InitialCheckError::MissingBlock {
            function: function.id,
            block: function.entry,
        })?;
    check_entry_parameters(function, entry, signature)?;
    check_value_definitions(function)?;
    for block in &function.blocks {
        if block.parameters.iter().any(|parameter| matches!(parameter.value_type, ValueType::InlineAggregate(_)))
            || terminator_uses(&block.terminator).iter().any(|value| matches!(function.value_types.get(value), Some(ValueType::InlineAggregate(_))))
        {
            return Err(InitialCheckError::UnsupportedInlineAggregateControlFlow { function: function.id });
        }
    }

    let predecessors = predecessor_map(function, &blocks)?;
    let dominators = dominators(function, &blocks, &predecessors)?;
    let definition_sites = definition_sites(function);
    let memory = MemoryChecker::new(function, layouts, signature, closure_layouts)?;
    let mut all_tail_sites = BTreeSet::new();
    for block in &function.blocks {
        match &block.terminator {
            Terminator::TailCall { site, .. } | Terminator::VarTailCall { site, .. } => {
                if !all_tail_sites.insert(*site) {
                    return Err(InitialCheckError::DuplicateTailObligation(*site));
                }
                observe_tail_site(
                    function,
                    block.id,
                    *site,
                    false,
                    tail_obligations,
                    observed_tail_sites,
                )?;
            }
            _ => {}
        }
    }
    check_memory_cfg(
        function,
        signature,
        signatures,
        imports,
        catalog,
        functions,
        &blocks,
        &definition_sites,
        &dominators,
        memory,
        tail_obligations,
        observed_tail_sites,
    )
}

#[allow(clippy::too_many_arguments)]
fn check_memory_cfg(
    function: &Function,
    signature: &FunctionSignature,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    imports: &BTreeMap<ImportId, &ImportContract>,
    catalog: Option<&CanonicalAbiCatalog>,
    functions: &FunctionIndex,
    blocks: &BTreeMap<BlockId, &BasicBlock>,
    definition_sites: &BTreeMap<ValueId, DefinitionSite>,
    dominators: &Dominators,
    mut memory: MemoryChecker<'_>,
    tail_obligations: &BTreeMap<TailSiteId, ProperTailLocation>,
    observed_tail_sites: &mut BTreeSet<TailSiteId>,
) -> Result<(), InitialCheckError> {
    let mut incoming = BTreeMap::new();
    incoming.insert(function.entry, memory.snapshot());
    let mut pending = VecDeque::from([function.entry]);
    let mut iterations = 0usize;
    let maximum_iterations = function
        .blocks
        .len()
        .saturating_mul(function.value_types.len().saturating_add(1))
        .saturating_mul(8)
        .max(64);

    // A block checked against a not-yet-converged incoming state (a loop
    // header seen from its entry edge only) may fail on facts the back edge
    // will establish; its failure is held and cleared when a later state
    // admits it. Only a failure that stands once every state has converged
    // is reported (the earliest block's).
    let mut failures: BTreeMap<BlockId, InitialCheckError> = BTreeMap::new();
    while let Some(block_id) = pending.pop_front() {
        iterations += 1;
        if iterations > maximum_iterations {
            return Err(InitialCheckError::MemoryDataflowNonConvergence {
                function: function.id,
            });
        }
        let block = blocks[&block_id];
        memory.restore(incoming[&block_id].clone());
        let checked = check_block_flow(
            function, block, signature, signatures, imports, catalog, functions, blocks,
            definition_sites, dominators, &mut memory, &incoming,
        );
        let updates = match checked {
            Ok(updates) => {
                failures.remove(&block_id);
                updates
            }
            Err(error) => {
                failures.insert(block_id, error);
                continue;
            }
        };
        for (target, next) in updates {
            if incoming.get(&target) != Some(&next) {
                incoming.insert(target, next);
                pending.push_back(target);
            }
        }
    }
    if let Some((_, error)) = failures.into_iter().next() {
        return Err(error);
    }
    // Only blocks reached by the converged memory dataflow can discharge a
    // checked-source proper-tail obligation. Scanning the raw block list
    // would let an unreachable decoy hide a reachable Call; Return lowering.
    for block_id in incoming.keys() {
        match &blocks[block_id].terminator {
            Terminator::TailCall { site, .. } | Terminator::VarTailCall { site, .. } => {
                observe_tail_site(
                    function,
                    *block_id,
                    *site,
                    true,
                    tail_obligations,
                    observed_tail_sites,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Check one block against its incoming state and compute the states its
/// edges carry to its successors.
#[allow(clippy::too_many_arguments)]
fn check_block_flow(
    function: &Function,
    block: &BasicBlock,
    signature: &FunctionSignature,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    imports: &BTreeMap<ImportId, &ImportContract>,
    catalog: Option<&CanonicalAbiCatalog>,
    functions: &FunctionIndex,
    blocks: &BTreeMap<BlockId, &BasicBlock>,
    definition_sites: &BTreeMap<ValueId, DefinitionSite>,
    dominators: &Dominators,
    memory: &mut MemoryChecker<'_>,
    incoming: &BTreeMap<BlockId, memory::MemoryState>,
) -> Result<Vec<(BlockId, memory::MemoryState)>, InitialCheckError> {
    for (operation_index, operation) in block.operations.iter().enumerate() {
        check_uses_dominate(
            function,
            block.id,
            operation_index,
            operation_uses(operation),
            definition_sites,
            dominators,
        )?;
        check_operation(function, operation, signatures, imports, catalog, functions, memory)?;
    }
    check_uses_dominate(
        function,
        block.id,
        block.operations.len(),
        terminator_uses(&block.terminator),
        definition_sites,
        dominators,
    )?;
    check_terminator(
        function,
        &block.terminator,
        signature,
        signatures,
        functions,
        blocks,
        memory,
    )?;
    let mut updates = Vec::new();
    for (target, arguments) in memory_successor_edges(&block.terminator, memory) {
        let target_block = blocks[&target];
        let transferred = memory.transfer_edge(target_block, arguments)?;
        let next = match incoming.get(&target) {
            Some(current) => memory.meet(target_block, current, &transferred)?,
            None => transferred,
        };
        updates.push((target, next));
    }
    Ok(updates)
}

fn memory_successor_edges<'a>(
    terminator: &'a Terminator,
    memory: &MemoryChecker<'_>,
) -> Vec<(BlockId, &'a [ValueId])> {
    match terminator {
        Terminator::Branch { target, arguments } => vec![(*target, arguments)],
        Terminator::BranchIf {
            condition,
            then_target,
            then_arguments,
            else_target,
            else_arguments,
        } => match memory.constant_condition(*condition) {
            Some(true) => vec![(*then_target, then_arguments)],
            Some(false) => vec![(*else_target, else_arguments)],
            None => vec![(*then_target, then_arguments), (*else_target, else_arguments)],
        },
        Terminator::Return { .. }
        | Terminator::TailCall { .. }
        | Terminator::VarTailCall { .. }
        | Terminator::Trap
        | Terminator::Unreachable => Vec::new(),
    }
}

fn index_blocks(function: &Function) -> Result<BTreeMap<BlockId, &BasicBlock>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for block in &function.blocks {
        if result.insert(block.id, block).is_some() {
            return Err(InitialCheckError::DuplicateBlock {
                function: function.id,
                block: block.id,
            });
        }
    }
    Ok(result)
}

fn check_entry_parameters(
    function: &Function,
    block: &BasicBlock,
    signature: &FunctionSignature,
) -> Result<(), InitialCheckError> {
    require_arity("entry parameters", signature.parameters.len(), block.parameters.len())?;
    for (parameter_index, (parameter, expected)) in
        block.parameters.iter().zip(&signature.parameters).enumerate()
    {
        require_type(function, parameter.value, *expected)?;
        if parameter.value_type != *expected {
            return Err(InitialCheckError::TypeMismatch {
                function: function.id,
                value: parameter.value,
                expected: *expected,
                actual: parameter.value_type,
            });
        }
        let contract = &signature.contract.parameters[parameter_index];
        match contract.ownership {
            ParameterOwnership::Value => {
                if parameter.ownership.is_some() {
                    return Err(InitialCheckError::UnsupportedMemorySignature {
                        signature: signature.id,
                    });
                }
            }
            ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack => {
                if parameter.ownership != Some(super::Ownership::Unique) {
                    return Err(InitialCheckError::CallableContractMismatch {
                        signature: signature.id,
                    });
                }
            }
            // A scalar `var` parameter is written back through a result lane
            // and carries no memory token.
            ParameterOwnership::WriteBack if !matches!(parameter.value_type, ValueType::Ref(_)) => {
                if parameter.ownership.is_some() {
                    return Err(InitialCheckError::UnsupportedMemorySignature {
                        signature: signature.id,
                    });
                }
            }
            ParameterOwnership::Shared | ParameterOwnership::WriteBack => {
                if parameter.ownership != Some(super::Ownership::Shared) {
                    return Err(InitialCheckError::CallableContractMismatch {
                        signature: signature.id,
                    });
                }
            }
        }
    }
    Ok(())
}

fn check_value_definitions(function: &Function) -> Result<(), InitialCheckError> {
    let mut definitions = BTreeSet::new();
    for block in &function.blocks {
        for parameter in &block.parameters {
            insert_definition(function.id, &mut definitions, parameter.value)?;
        }
        for operation in &block.operations {
            for destination in operation_destinations(operation) {
                insert_definition(function.id, &mut definitions, destination)?;
            }
        }
    }
    for value in &definitions {
        if !function.value_types.contains_key(value) {
            return Err(InitialCheckError::MissingValueType {
                function: function.id,
                value: *value,
            });
        }
    }
    for value in function.value_types.keys() {
        if !definitions.contains(value) {
            return Err(InitialCheckError::UndeclaredValue {
                function: function.id,
                value: *value,
            });
        }
    }
    Ok(())
}

fn insert_definition(
    function: FunctionId,
    definitions: &mut BTreeSet<ValueId>,
    value: ValueId,
) -> Result<(), InitialCheckError> {
    if !definitions.insert(value) {
        return Err(InitialCheckError::DuplicateValue { function, value });
    }
    Ok(())
}

fn operation_destinations(operation: &Operation) -> Vec<ValueId> {
    match operation {
        Operation::ClosureOwner(owner) => owner.destinations(),
        Operation::ConstInt { destination, .. }
        | Operation::ConstFloat { destination, .. }
        | Operation::ConstUnit { destination }
        | Operation::ConstBool { destination, .. }
        | Operation::ConstSize { destination, .. }
        | Operation::CapabilityNone { destination, .. }
        | Operation::CapabilitySome { destination, .. }
        | Operation::CapabilityIsPresent { destination, .. }
        | Operation::CapabilityUnwrap { destination, .. }
        | Operation::Arithmetic { destination, .. }
        | Operation::Compare { destination, .. }
        | Operation::NarrowSize { destination, .. }
        | Operation::WidenSize { destination, .. }
        | Operation::FloatUnary { destination, .. }
        | Operation::Alloc { destination, .. }
        | Operation::StaticData { destination, .. }
        | Operation::Load { destination, .. }
        | Operation::MoveValue { destination, .. }
        | Operation::MakeInlineAggregate { destination, .. }
        | Operation::ExtractInlineField { destination, .. }
        | Operation::CloneRef { destination, .. }
        | Operation::FunctionValue { destination, .. } => vec![*destination],
        Operation::Call { destinations, .. }
        | Operation::VarCall { destinations, .. }
        | Operation::HostCall { destinations, .. } => destinations.clone(),
        Operation::Guard { .. }
        | Operation::InitNullableOwned { .. }
        | Operation::Store { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::UpdateCount { .. }
        | Operation::Retain { .. }
        | Operation::Release { .. }
        | Operation::DropRef { .. }
        | Operation::Reclaim { .. } => Vec::new(),
    }
}

pub(crate) fn operation_uses(operation: &Operation) -> Vec<ValueId> {
    match operation {
        Operation::ClosureOwner(owner) => owner.operands(),
        Operation::ConstInt { .. }
        | Operation::ConstFloat { .. }
        | Operation::ConstUnit { .. } | Operation::ConstBool { .. }
        | Operation::ConstSize { .. }
        | Operation::CapabilityNone { .. }
        | Operation::Alloc { .. }
        | Operation::StaticData { .. }
        | Operation::FunctionValue { .. } => Vec::new(),
        Operation::Arithmetic { left, right, .. }
        | Operation::Compare { left, right, .. } => vec![*left, *right],
        Operation::NarrowSize { value, .. }
        | Operation::WidenSize { value, .. }
        | Operation::FloatUnary { value, .. }
        | Operation::Guard { condition: value, .. }
        | Operation::MoveValue { source: value, .. }
        | Operation::CloneRef { source: value, .. }
        | Operation::Retain { object: value }
        | Operation::Release { object: value }
        | Operation::DropRef { object: value }
        | Operation::Reclaim { object: value } => vec![*value],
        Operation::CapabilitySome { source: value, .. }
        | Operation::CapabilityIsPresent { source: value, .. }
        | Operation::CapabilityUnwrap { source: value, .. } => vec![*value],
        Operation::MakeInlineAggregate { fields, .. } => fields.clone(),
        Operation::ExtractInlineField { aggregate, .. } => vec![*aggregate],
        Operation::Load { place, .. } | Operation::InitNullableOwned { place } => place_uses(place),
        Operation::Store { place, value, .. } => {
            let mut result = place_uses(place);
            result.push(*value);
            result
        }
        Operation::CopyBytes { destination, source, length }
        | Operation::MoveRange { destination, source, count: length }
        | Operation::CloneRange { destination, source, count: length } => {
            let mut result = place_uses(destination);
            result.extend(place_uses(source));
            result.push(*length);
            result
        }
        Operation::UpdateCount { object, count } => vec![*object, *count],
        Operation::Call { arguments, .. } => argument_uses(arguments),
        Operation::HostCall { arguments, .. } => host_argument_uses(arguments),
        Operation::VarCall { callee, arguments, .. } => {
            let mut result = vec![*callee];
            result.extend(argument_uses(arguments));
            result
        }
    }
}

fn place_uses(place: &super::Place) -> Vec<ValueId> {
    let mut result = vec![place.object()];
    match place.projection() {
        super::Projection::Header(_) | super::Projection::Field { .. } => {}
        super::Projection::Element { index } => result.push(*index),
        super::Projection::Slice { start, length } => {
            result.push(*start);
            result.push(*length);
        }
    }
    result
}

fn argument_uses(arguments: &[CallArgument]) -> Vec<ValueId> {
    arguments
        .iter()
        .map(|argument| match argument {
            CallArgument::Value(value) | CallArgument::Move(value) => {
                *value
            }
        })
        .collect()
}

fn host_argument_uses(arguments: &[HostCallArgument]) -> Vec<ValueId> {
    let mut result = Vec::new();
    for argument in arguments {
        match argument {
            HostCallArgument::Value(value) | HostCallArgument::Capability(value) => result.push(*value),
            HostCallArgument::Buffer { place, .. } => result.extend(place_uses(place)),
        }
    }
    result
}

pub(crate) fn terminator_uses(terminator: &Terminator) -> Vec<ValueId> {
    match terminator {
        Terminator::Branch { arguments, .. } | Terminator::Return { values: arguments } => {
            arguments.clone()
        }
        Terminator::BranchIf {
            condition,
            then_arguments,
            else_arguments,
            ..
        } => {
            let mut result = vec![*condition];
            result.extend(then_arguments.iter().copied());
            result.extend(else_arguments.iter().copied());
            result
        }
        Terminator::TailCall { arguments, .. } => argument_uses(arguments),
        Terminator::VarTailCall { callee, arguments, .. } => {
            let mut result = vec![*callee];
            result.extend(argument_uses(arguments));
            result
        }
        Terminator::Trap | Terminator::Unreachable => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy)]
struct DefinitionSite {
    block: BlockId,
    operation_index: Option<usize>,
}

fn definition_sites(function: &Function) -> BTreeMap<ValueId, DefinitionSite> {
    let mut result = BTreeMap::new();
    for block in &function.blocks {
        for parameter in &block.parameters {
            result.insert(
                parameter.value,
                DefinitionSite {
                    block: block.id,
                    operation_index: None,
                },
            );
        }
        for (operation_index, operation) in block.operations.iter().enumerate() {
            for destination in operation_destinations(operation) {
                result.insert(
                    destination,
                    DefinitionSite {
                        block: block.id,
                        operation_index: Some(operation_index),
                    },
                );
            }
        }
    }
    result
}

fn check_uses_dominate(
    function: &Function,
    use_block: BlockId,
    use_operation_index: usize,
    uses: Vec<ValueId>,
    definitions: &BTreeMap<ValueId, DefinitionSite>,
    dominators: &Dominators,
) -> Result<(), InitialCheckError> {
    for value in uses {
        let definition = definitions
            .get(&value)
            .copied()
            .ok_or(InitialCheckError::UndeclaredValue {
                function: function.id,
                value,
            })?;
        let dominates = if definition.block == use_block {
            definition
                .operation_index
                .is_none_or(|definition_index| definition_index < use_operation_index)
        } else {
            dominators.dominates(definition.block, use_block)
        };
        if !dominates {
            return Err(InitialCheckError::ValueDoesNotDominateUse {
                function: function.id,
                value,
                block: use_block,
            });
        }
    }
    Ok(())
}

fn predecessor_map(
    function: &Function,
    blocks: &BTreeMap<BlockId, &BasicBlock>,
) -> Result<BTreeMap<BlockId, BTreeSet<BlockId>>, InitialCheckError> {
    let mut predecessors: BTreeMap<_, BTreeSet<_>> =
        blocks.keys().copied().map(|block| (block, BTreeSet::new())).collect();
    for block in &function.blocks {
        for successor in successors(&block.terminator) {
            let incoming = predecessors
                .get_mut(&successor)
                .ok_or(InitialCheckError::MissingBlock {
                    function: function.id,
                    block: successor,
                })?;
            incoming.insert(block.id);
        }
    }
    Ok(predecessors)
}

fn successors(terminator: &Terminator) -> Vec<BlockId> {
    match terminator {
        Terminator::Branch { target, .. } => vec![*target],
        Terminator::BranchIf { then_target, else_target, .. } => {
            vec![*then_target, *else_target]
        }
        Terminator::Return { .. }
        | Terminator::TailCall { .. }
        | Terminator::VarTailCall { .. }
        | Terminator::Trap
        | Terminator::Unreachable => Vec::new(),
    }
}

mod dominance;
use dominance::{Dominators, dominators};

fn check_operation(
    function: &Function,
    operation: &Operation,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    imports: &BTreeMap<ImportId, &ImportContract>,
    catalog: Option<&CanonicalAbiCatalog>,
    functions: &FunctionIndex,
    memory: &mut MemoryChecker<'_>,
) -> Result<(), InitialCheckError> {
    if memory.check(operation)? {
        return Ok(());
    }
    match operation {
        // Registry/type/token checks run independently over the complete CFG.
        // Source correspondence remains a separate authority judgment.
        Operation::ClosureOwner(_) => Ok(()),
        Operation::ConstInt { destination, .. } => {
            require_type(function, *destination, ValueType::Int)
        }
        Operation::ConstFloat { destination, .. } => {
            require_type(function, *destination, ValueType::Float)
        }
        Operation::ConstUnit { destination } => require_type(function, *destination, ValueType::Unit),
        Operation::ConstBool { destination, .. } => {
            require_type(function, *destination, ValueType::Bool)
        }
        Operation::ConstSize { destination, value } => {
            require_type(function, *destination, ValueType::Size)?;
            if u32::try_from(*value).is_err() {
                return unsupported_scalar(function, "ConstSize outside Wasm32");
            }
            Ok(())
        }
        Operation::CapabilityNone { destination, kind } => require_type(function, *destination, ValueType::NullableCapability(*kind)),
        Operation::CapabilitySome { destination, source } => {
            let actual = value_type(function, *source)?;
            let ValueType::Capability(kind) = actual else { return Err(type_mismatch(function, *source, ValueType::Capability(crate::canonical_safewir::CapabilityKindId::new(0)), actual)); };
            require_type(function, *destination, ValueType::NullableCapability(kind))
        }
        Operation::CapabilityIsPresent { destination, source } => {
            require_type(function, *destination, ValueType::Bool)?;
            let actual = value_type(function, *source)?;
            if !matches!(actual, ValueType::NullableCapability(_)) { return Err(type_mismatch(function, *source, ValueType::NullableCapability(crate::canonical_safewir::CapabilityKindId::new(0)), actual)); }
            Ok(())
        }
        Operation::CapabilityUnwrap { destination, source } => {
            let actual = value_type(function, *source)?;
            let ValueType::NullableCapability(kind) = actual else { return Err(type_mismatch(function, *source, ValueType::NullableCapability(crate::canonical_safewir::CapabilityKindId::new(0)), actual)); };
            require_type(function, *destination, ValueType::Capability(kind))
        }
        Operation::Arithmetic { destination, operator, left, right, overflow } => {
            let result = value_type(function, *destination)?;
            if !scalar_arithmetic_is_backend_total(result, *operator, *overflow) {
                return unsupported_scalar(function, "Arithmetic operator/type/overflow combination");
            }
            require_type(function, *left, result)?;
            require_type(function, *right, result)
        }
        Operation::Compare { destination, operator, left, right } => {
            require_type(function, *destination, ValueType::Bool)?;
            let left_type = value_type(function, *left)?;
            if !scalar_comparison_is_backend_total(left_type, *operator) {
                return unsupported_scalar(function, "Compare operator/type combination");
            }
            require_type(function, *right, left_type)
        }
        Operation::NarrowSize { destination, value } => {
            require_type(function, *destination, ValueType::Size)?;
            require_type(function, *value, ValueType::Int)
        }
        Operation::WidenSize { destination, value } => {
            require_type(function, *destination, ValueType::Int)?;
            require_type(function, *value, ValueType::Size)
        }
        Operation::FloatUnary { destination, operator, value } => {
            let (operand, result) = match operator {
                crate::canonical_safewir::FloatUnaryOperator::Sqrt => (ValueType::Float, ValueType::Float),
                crate::canonical_safewir::FloatUnaryOperator::TruncateToInt => (ValueType::Float, ValueType::Int),
                crate::canonical_safewir::FloatUnaryOperator::FromInt => (ValueType::Int, ValueType::Float),
            };
            require_type(function, *destination, result)?;
            require_type(function, *value, operand)
        }
        Operation::Guard { condition, .. } => require_type(function, *condition, ValueType::Bool),
        Operation::MoveValue { destination, source } => {
            let source_type = value_type(function, *source)?;
            if source_type.is_memory_bearing() || matches!(source_type, ValueType::InlineAggregate(_)) {
                return unsupported_memory(function, "MoveValue");
            }
            require_type(function, *destination, source_type)
        }
        Operation::MakeInlineAggregate { destination, layout, fields } => {
            require_type(function, *destination, ValueType::InlineAggregate(*layout))?;
            let descriptor = memory.layouts().get(*layout).ok_or(InitialCheckError::InvalidInlineAggregateLayout {
                function: function.id,
                layout: *layout,
            })?;
            let (crate::layout::LayoutKind::PackedRecord { fields: expected }
                | crate::layout::LayoutKind::Tuple { fields: expected }) = descriptor.kind()
            else {
                return Err(InitialCheckError::InvalidInlineAggregateLayout { function: function.id, layout: *layout });
            };
            if expected.len() != fields.len() {
                return Err(InitialCheckError::ArityMismatch { context: "MakeInlineAggregate fields", expected: expected.len(), actual: fields.len() });
            }
            for (field, expected) in fields.iter().zip(expected) {
                require_type(function, *field, inline_scalar_layout_type(memory.layouts(), *expected, function.id, *layout)?)?;
            }
            Ok(())
        }
        Operation::ExtractInlineField { destination, aggregate, index } => {
            let ValueType::InlineAggregate(layout) = value_type(function, *aggregate)? else {
                return Err(InitialCheckError::TypeMismatch { function: function.id, value: *aggregate, expected: ValueType::InlineAggregate(crate::layout::LayoutId::from_bytes([0; 32])), actual: value_type(function, *aggregate)? });
            };
            let descriptor = memory.layouts().get(layout).ok_or(InitialCheckError::InvalidInlineAggregateLayout { function: function.id, layout })?;
            let (crate::layout::LayoutKind::PackedRecord { fields }
                | crate::layout::LayoutKind::Tuple { fields }) = descriptor.kind()
            else { return Err(InitialCheckError::InvalidInlineAggregateLayout { function: function.id, layout }); };
            let field = fields.get(*index as usize).ok_or(InitialCheckError::InvalidInlineAggregateLayout { function: function.id, layout })?;
            require_type(function, *destination, inline_scalar_layout_type(memory.layouts(), *field, function.id, layout)?)
        }
        Operation::FunctionValue { destination, function: target } => {
            let target = functions
                .get(target)
                .copied()
                .ok_or(InitialCheckError::MissingFunction(*target))?;
            require_type(function, *destination, ValueType::Callable(target.signature))
        }
        Operation::Call { destinations, callee, arguments } => {
            let callee = functions
                .get(callee)
                .copied()
                .ok_or(InitialCheckError::MissingFunction(*callee))?;
            let signature = signatures
                .get(&callee.signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(callee.signature))?;
            check_call(function, destinations, arguments, signature, Some(memory))
        }
        Operation::VarCall { destinations, callee, signature, arguments } => {
            require_type(function, *callee, ValueType::Callable(*signature))?;
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            check_indirect_reference_arguments(function, arguments, signature)?;
            check_call(function, destinations, arguments, signature, Some(memory))
        }
        Operation::HostCall { destinations, import, arguments } => {
            host::check_call(function, *import, destinations, arguments, imports, signatures, catalog)?;
            let contract = imports
                .get(import)
                .copied()
                .ok_or(InitialCheckError::MissingImportContract {
                    function: function.id,
                    import: *import,
                })?;
            let mut write_buffer = None;
            for (parameter, (argument, role)) in arguments
                .iter()
                .zip(&contract.parameter_roles)
                .enumerate()
            {
                if let (
                    HostCallArgument::Buffer { place, extent },
                    super::HostParameterRole::Buffer { access, .. },
                ) = (argument, role)
                {
                    memory.check_host_buffer(place, extent, *import, parameter)?;
                    if *access == super::AccessMode::Write && write_buffer.replace(place).is_some() {
                        return unsupported_memory(function, "HostCall(multiple write buffers)");
                    }
                }
            }
            if contract.mutation == super::HostMutation::WriteBuffers {
                let mut written = contract.result_roles.iter().zip(destinations).filter_map(|(role, value)| {
                    matches!(role, super::HostResultRole::Value(ValueType::Size)).then_some(*value)
                });
                let published = written.next();
                if written.next().is_some() || write_buffer.is_none() || published.is_none() {
                    return unsupported_memory(function, "HostCall(write prefix contract)");
                }
                memory.record_host_written_prefix(write_buffer.expect("checked"), published.expect("checked"))?;
            }
            Ok(())
        }
        Operation::Alloc { .. } => unsupported_memory(function, "Alloc"),
        Operation::StaticData { .. } => unsupported_memory(function, "StaticData"),
        Operation::Load { .. } => unsupported_memory(function, "Load"),
        Operation::InitNullableOwned { .. } => unsupported_memory(function, "InitNullableOwned"),
        Operation::Store { .. } => unsupported_memory(function, "Store"),
        Operation::CopyBytes { .. } => unsupported_memory(function, "CopyBytes"),
        Operation::MoveRange { .. } => unsupported_memory(function, "MoveRange"),
        Operation::CloneRange { .. } => unsupported_memory(function, "CloneRange"),
        Operation::UpdateCount { .. } => unsupported_memory(function, "UpdateCount"),
        Operation::CloneRef { .. } => unsupported_memory(function, "CloneRef"),
        Operation::Retain { .. } => unsupported_memory(function, "Retain"),
        Operation::Release { .. } => unsupported_memory(function, "Release"),
        Operation::DropRef { .. } => unsupported_memory(function, "DropRef"),
        Operation::Reclaim { .. } => unsupported_memory(function, "Reclaim"),
    }
}

fn scalar_arithmetic_is_backend_total(
    value_type: ValueType,
    operator: super::ArithmeticOperator,
    overflow: super::OverflowBehavior,
) -> bool {
    use super::{ArithmeticOperator as A, OverflowBehavior as O};
    match (value_type, operator, overflow) {
        (
            ValueType::Int | ValueType::Size,
            A::Add
            | A::Subtract
            | A::Multiply
            | A::BitAnd
            | A::BitOr
            | A::BitXor
            | A::ShiftLeft
            | A::ShiftRightSigned
            | A::ShiftRightUnsigned,
            O::Wrap,
        ) => true,
        (
            ValueType::Int | ValueType::Size,
            A::DivideSigned | A::DivideUnsigned | A::RemainderSigned | A::RemainderUnsigned,
            O::Trap,
        ) => true,
        (ValueType::Size, A::Add, O::Trap) => true,
        (
            ValueType::Float,
            A::Add | A::Subtract | A::Multiply | A::DivideSigned | A::DivideUnsigned,
            O::Wrap,
        ) => true,
        _ => false,
    }
}

fn scalar_comparison_is_backend_total(
    value_type: ValueType,
    operator: super::ComparisonOperator,
) -> bool {
    use super::ComparisonOperator as C;
    match value_type {
        ValueType::Int | ValueType::Size | ValueType::Float => true,
        ValueType::Bool => matches!(operator, C::Equal | C::NotEqual),
        _ => false,
    }
}

fn unsupported_scalar<T>(
    function: &Function,
    operation: &'static str,
) -> Result<T, InitialCheckError> {
    Err(InitialCheckError::UnsupportedScalarOperation { function: function.id, operation })
}

fn inline_scalar_layout_type(
    layouts: &crate::layout::LayoutInterner,
    layout: crate::layout::LayoutId,
    function: FunctionId,
    parent: crate::layout::LayoutId,
) -> Result<ValueType, InitialCheckError> {
    let descriptor = layouts.get(layout).ok_or(InitialCheckError::InvalidInlineAggregateLayout { function, layout: parent })?;
    match descriptor.kind() {
        crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int | crate::layout::ScalarKind::Duration) => Ok(ValueType::Int),
        crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Float) => Ok(ValueType::Float),
        crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Bool) => Ok(ValueType::Bool),
        crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Unit) => Ok(ValueType::Unit),
        crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::V128) => Ok(ValueType::V128),
        _ => Err(InitialCheckError::InvalidInlineAggregateLayout { function, layout: parent }),
    }
}

fn check_call(
    function: &Function,
    destinations: &[ValueId],
    arguments: &[CallArgument],
    signature: &FunctionSignature,
    mut memory: Option<&mut MemoryChecker<'_>>,
) -> Result<(), InitialCheckError> {
    require_arity("call arguments", signature.parameters.len(), arguments.len())?;
    require_arity("call results", signature.results.len(), destinations.len())?;
    for (argument, expected) in arguments.iter().zip(&signature.contract.parameters) {
        let (value, ownership) = match argument {
            CallArgument::Value(value) => (*value, ParameterOwnership::Value),
            CallArgument::Move(value) => (*value, ParameterOwnership::Unique),
        };
        require_type(function, value, expected.value_type)?;
        let accepted = match (expected.ownership, ownership) {
            (ParameterOwnership::Value, ParameterOwnership::Value)
            | (ParameterOwnership::Unique, ParameterOwnership::Unique)
            | (ParameterOwnership::WriteBack, ParameterOwnership::Unique)
            | (ParameterOwnership::UniqueWriteBack, ParameterOwnership::Unique)
            // A scalar `var` parameter has no token: the argument is a value
            // and the write-back is a result lane.
            | (ParameterOwnership::WriteBack, ParameterOwnership::Value)
                if !matches!(expected.value_type, ValueType::Ref(_)) => true,
            (ParameterOwnership::Value, ParameterOwnership::Value)
            | (ParameterOwnership::Unique, ParameterOwnership::Unique)
            | (ParameterOwnership::WriteBack, ParameterOwnership::Unique)
            | (ParameterOwnership::UniqueWriteBack, ParameterOwnership::Unique)
            // Moving a unique token into a shared parameter is a safe
            // ownership weakening; the callee receives that token.
            | (ParameterOwnership::Shared, ParameterOwnership::Unique) => true,
            _ => false,
        };
        if !accepted {
            return Err(InitialCheckError::UnsupportedBorrowArgument {
                function: function.id,
            });
        }
    }
    for (destination, (expected, contract)) in destinations
        .iter()
        .zip(signature.results.iter().zip(&signature.contract.results))
    {
        require_type(function, *destination, *expected)?;
        let contract_type = match contract {
            super::ResultContract::BodyDerived(value_type) => *value_type,
            super::ResultContract::Value(value_type) => *value_type,
            super::ResultContract::CheckedValue { layout }
            | super::ResultContract::Fresh { layout, .. }
            | super::ResultContract::Shared { layout }
            | super::ResultContract::Unique { layout }
            | super::ResultContract::StaticData { layout }
            | super::ResultContract::AliasesParameter { layout, .. }
            | super::ResultContract::ReallocatesParameter { layout, .. } => {
                ValueType::Ref(*layout)
            }
        };
        if contract_type != *expected {
            return Err(InitialCheckError::CallableContractMismatch {
                signature: signature.id,
            });
        }
    }
    if let Some(memory) = memory.as_deref_mut() {
        memory.check_call(
            arguments,
            &signature.contract.parameters,
            destinations,
            &signature.contract.results,
        )?;
    }
    Ok(())
}

fn check_indirect_reference_arguments(
    function: &Function,
    arguments: &[CallArgument],
    signature: &FunctionSignature,
) -> Result<(), InitialCheckError> {
    if arguments.len() != signature.contract.parameters.len() {
        return Err(InitialCheckError::UnsupportedIndirectCall { function: function.id });
    }
    if arguments.iter().zip(&signature.contract.parameters).enumerate().any(|(index, (argument, parameter))| {
        let value = match argument {
            CallArgument::Value(value) | CallArgument::Move(value) => *value,
        };
        let value_type = value_type(function, value);
        if matches!(value_type.as_ref(), Ok(ValueType::Capability(_) | ValueType::NullableCapability(_)))
            || matches!(parameter.value_type, ValueType::Capability(_) | ValueType::NullableCapability(_))
        {
            // Opaque capabilities are value lanes, not movable linear-memory
            // references. Preserve the exact kind and nullability, and do not
            // let this case acquire any reference ownership or memory effects.
            return !(matches!(argument, CallArgument::Value(_))
                && value_type.as_ref().is_ok_and(|actual| *actual == parameter.value_type)
                && signature.parameters.get(index) == Some(&parameter.value_type)
                && parameter.ownership == ParameterOwnership::Value
                && signature.contract.parameter_effects.get(&(index as u32))
                    .is_none_or(|effects| effects.is_empty()));
        }
        let memory_bearing = value_type.as_ref()
            .map(|value_type| value_type.is_memory_bearing())
            .unwrap_or(true);
        memory_bearing && !matches!(
            (argument, value_type, parameter.value_type, parameter.ownership),
            (CallArgument::Move(_), Ok(ValueType::Ref(_)), ValueType::Ref(_), ParameterOwnership::Shared | ParameterOwnership::WriteBack)
                | (CallArgument::Move(_), Ok(ValueType::Gc(_)), ValueType::Gc(_), ParameterOwnership::Shared)
        )
    }) {
        return Err(InitialCheckError::UnsupportedIndirectCall {
            function: function.id,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn check_terminator(
    function: &Function,
    terminator: &Terminator,
    current_signature: &FunctionSignature,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    functions: &FunctionIndex,
    blocks: &BTreeMap<BlockId, &BasicBlock>,
    memory: &mut MemoryChecker<'_>,
) -> Result<(), InitialCheckError> {
    match terminator {
        Terminator::Return { values } => {
            require_arity("return values", current_signature.results.len(), values.len())?;
            for (value, expected) in values.iter().zip(&current_signature.results) {
                require_type(function, *value, *expected)?;
            }
            memory.check_return(values, &current_signature.contract.results)?;
            memory.check_exit(values)?;
            Ok(())
        }
        Terminator::TailCall { site, callee, arguments } => {
            let _ = site;
            let callee = functions
                .get(callee)
                .copied()
                .ok_or(InitialCheckError::MissingFunction(*callee))?;
            let signature = signatures
                .get(&callee.signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(callee.signature))?;
            check_tail_call(function, arguments, signature, current_signature, Some(memory))?;
            memory.check_exit(&[])
        }
        Terminator::VarTailCall { site, callee, signature, arguments } => {
            let _ = site;
            require_type(function, *callee, ValueType::Callable(*signature))?;
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            check_indirect_reference_arguments(function, arguments, signature)?;
            check_tail_call(function, arguments, signature, current_signature, Some(memory))?;
            memory.check_exit(&[])
        }
        Terminator::Branch { target, arguments } => {
            check_branch(function, *target, arguments, blocks)
        }
        Terminator::BranchIf {
            condition,
            then_target,
            then_arguments,
            else_target,
            else_arguments,
        } => {
            require_type(function, *condition, ValueType::Bool)?;
            check_branch(function, *then_target, then_arguments, blocks)?;
            check_branch(function, *else_target, else_arguments, blocks)
        }
        Terminator::Trap | Terminator::Unreachable => Ok(()),
    }
}

fn check_branch(
    function: &Function,
    target: BlockId,
    arguments: &[ValueId],
    blocks: &BTreeMap<BlockId, &BasicBlock>,
) -> Result<(), InitialCheckError> {
    let target_block = blocks
        .get(&target)
        .copied()
        .ok_or(InitialCheckError::MissingBlock {
            function: function.id,
            block: target,
        })?;
    require_arity("branch arguments", target_block.parameters.len(), arguments.len())?;
    for (argument, parameter) in arguments.iter().zip(&target_block.parameters) {
        require_type(function, *argument, parameter.value_type)?;
    }
    Ok(())
}

fn check_tail_call(
    function: &Function,
    arguments: &[CallArgument],
    callee: &FunctionSignature,
    caller: &FunctionSignature,
    memory: Option<&mut MemoryChecker<'_>>,
) -> Result<(), InitialCheckError> {
    if callee.results != caller.results {
        return Err(InitialCheckError::ArityMismatch {
            context: "tail-call result signature",
            expected: caller.results.len(),
            actual: callee.results.len(),
        });
    }
    // A tail edge forwards results without executing the caller's Return
    // judgment. It must not turn a shared or opaque result into fresh storage.
    if callee.contract.results != caller.contract.results {
        return Err(InitialCheckError::CallableContractMismatch {
            signature: caller.id,
        });
    }
    check_call(function, &[], arguments, &FunctionSignature {
        id: callee.id,
        parameters: callee.parameters.clone(),
        results: Vec::new(),
        contract: callee.contract.clone(),
    }, memory)
}

fn observe_tail_site(
    function: &Function,
    block: BlockId,
    site: TailSiteId,
    reachable: bool,
    obligations: &BTreeMap<TailSiteId, ProperTailLocation>,
    observed: &mut BTreeSet<TailSiteId>,
) -> Result<(), InitialCheckError> {
    let expected = obligations
        .get(&site)
        .copied()
        .ok_or(InitialCheckError::UnexpectedTailSite(site))?;
    if expected.function() != function.id {
        return Err(InitialCheckError::TailSiteFunctionMismatch {
            site,
            expected: expected.function(),
            actual: function.id,
        });
    }
    if expected.block() != block {
        return Err(InitialCheckError::TailSiteBlockMismatch {
            site,
            expected: expected.block(),
            actual: block,
        });
    }
    if !reachable {
        return Ok(());
    }
    if !observed.insert(site) {
        return Err(InitialCheckError::DuplicateTailObligation(site));
    }
    Ok(())
}

fn value_type(function: &Function, value: ValueId) -> Result<ValueType, InitialCheckError> {
    function
        .value_types
        .get(&value)
        .copied()
        .ok_or(InitialCheckError::MissingValueType {
            function: function.id,
            value,
        })
}

fn require_type(
    function: &Function,
    value: ValueId,
    expected: ValueType,
) -> Result<(), InitialCheckError> {
    let actual = value_type(function, value)?;
    if actual != expected {
        return Err(type_mismatch(function, value, expected, actual));
    }
    Ok(())
}

fn type_mismatch(
    function: &Function,
    value: ValueId,
    expected: ValueType,
    actual: ValueType,
) -> InitialCheckError {
    InitialCheckError::TypeMismatch {
        function: function.id,
        value,
        expected,
        actual,
    }
}

fn require_arity(
    context: &'static str,
    expected: usize,
    actual: usize,
) -> Result<(), InitialCheckError> {
    if expected != actual {
        return Err(InitialCheckError::ArityMismatch {
            context,
            expected,
            actual,
        });
    }
    Ok(())
}

fn unsupported_memory<T>(
    function: &Function,
    operation: &'static str,
) -> Result<T, InitialCheckError> {
    Err(InitialCheckError::UnsupportedMemoryOperation {
        function: function.id,
        operation,
    })
}

impl ValueType {
    const fn is_linear_memory_ref(&self) -> bool {
        matches!(self, Self::Ref(_))
    }

    const fn is_memory_bearing(&self) -> bool {
        matches!(
            self,
            Self::Ref(_) | Self::Capability(_) | Self::Gc(_) | Self::Callable(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::{
        AccessMode, AllocationClass, ArithmeticOperator, BasicBlock, BlockId, BlockParameter, CallableContract,
        CanonicalAbiCatalog, CanonicalModule,
        CheckedSourceIdentity, ComparisonOperator, Extent, Function, FunctionId, FunctionSignature,
        FunctionSignatureId, GuardKind, HeaderField, HostBufferExtent, HostCallArgument,
        HostMutation, HostParameterRole, HostReentrancy, HostResultRole, ImportContract,
        ImportContractIdentity, ImportId, LoadTransfer, LoweredModuleParts, MemoryEffect, Operation, ParameterContract,
        ParameterOwnership, Place, Projection, ProperTailSite, ResultContract, Revision, TailSiteId,
        ProperTailLocation, StaticDataId, StaticDataSegment, Terminator, ValueId, ValueType,
    };
    use super::{InitialCheckError, check_initial};
    use super::super::{CallArgument, OverflowBehavior, StoreTransfer};
    use crate::layout::{LayoutBundle, LayoutId, LayoutInterner, PointerSlot, RcHeader, ScalarKind};

    // The closure-object subset: one `PackedRecord` whose slot 0 is the
    // callable lane and whose slot 1 is one Int capture. The driver builds
    // one, stores a function value into the lane, loads it back, and calls
    // through it with a shared clone of the object as the environment.
    const ENV_SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(2);
    const CAPTURE: ValueId = ValueId::new(0);
    const CLOSURE: ValueId = ValueId::new(1);
    const FUNCTION_VALUE: ValueId = ValueId::new(2);
    const LOADED_CALLABLE: ValueId = ValueId::new(3);
    const CLOSURE_CLONE: ValueId = ValueId::new(4);
    const CALL_RESULT: ValueId = ValueId::new(5);
    const ENVIRONMENT: ValueId = ValueId::new(6);
    const CAPTURE_READ: ValueId = ValueId::new(7);

    #[test]
    fn nullable_owned_slot_is_refused_at_the_initial_catalog_gate() {
        let mut layouts = LayoutInterner::new();
        let pointee = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let nullable = layouts
            .intern_runtime_pointer_value_slot(pointee, PointerSlot::OwnedNullable)
            .unwrap();
        assert!(super::has_unadmitted_nullable_owned_slot(&layouts));
        let bundle = LayoutBundle::from_interner(&layouts, [nullable]).unwrap();
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([9; 32]),
            layouts: bundle,
            signatures: Vec::new(),
            imports: Vec::new(),
            functions: Vec::new(),
            proper_tail_sites: Vec::new(),
        });
        assert!(matches!(
            check_initial(candidate),
            Err(InitialCheckError::InvalidLayoutBundle(message))
                if message == "nullable owned slots require canonical presence and destruction support"
        ));
    }

    #[test]
    fn nullable_owned_pointer_buffer_is_refused_at_the_initial_catalog_gate() {
        let mut layouts = LayoutInterner::new();
        let pointee = layouts.intern_scalar(ScalarKind::Int).unwrap();
        layouts
            .intern_runtime_pointer_buffer(pointee, PointerSlot::OwnedNullable)
            .unwrap();
        assert!(super::has_unadmitted_nullable_owned_slot(&layouts));
    }

    #[test]
    fn nullable_owned_collection_cannot_borrow_a_fixed_field_catalog_exception() {
        let mut layouts = LayoutInterner::new();
        let pointee = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let nullable = layouts.intern_runtime_pointer_value_slot(pointee, PointerSlot::OwnedNullable).unwrap();
        let record = layouts.intern_packed_record(vec![nullable]).unwrap();
        let list = layouts.intern_packed_list(nullable, crate::layout::RcHeader::Required).unwrap();
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([9; 32]),
            layouts: LayoutBundle::from_interner(&layouts, [record, list]).unwrap(),
            signatures: vec![], imports: vec![], functions: vec![], proper_tail_sites: vec![],
        });
        assert!(matches!(check_initial(candidate), Err(InitialCheckError::InvalidLayoutBundle(message))
            if message == "nullable owned slots require canonical presence and destruction support"));
    }

    fn closure_layout() -> (LayoutBundle, LayoutId) {
        let mut layouts = LayoutInterner::new();
        let callable = layouts
            .intern_scalar(ScalarKind::Callable)
            .expect("callable lane layout");
        let capture = layouts.intern_scalar(ScalarKind::Int).expect("int layout");
        let closure = layouts
            .intern_packed_record(vec![callable, capture])
            .expect("closure record layout");
        let bundle = LayoutBundle::from_interner(&layouts, [closure]).expect("layout bundle");
        (bundle, closure)
    }

    /// The closure module, with the driver's callable-lane places supplied by
    /// the caller so a refusal test can perturb exactly one of them.
    fn closure_module(
        stored_place: Place,
        stored_value: ValueId,
        loaded_place: Place,
        loaded_type: ValueType,
        register_layout: bool,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        let (layouts, closure) = closure_layout();
        let environment_signature = FunctionSignature {
            id: ENV_SIGNATURE,
            parameters: vec![ValueType::Ref(closure)],
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: vec![ParameterContract {
                    value_type: ValueType::Ref(closure),
                    ownership: ParameterOwnership::Shared,
                }],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: [(0, vec![MemoryEffect::Read, MemoryEffect::Release])]
                    .into_iter()
                    .collect(),
            },
        };
        let driver = Function {
            id: FUNCTION,
            signature: SIGNATURE,
            entry: ENTRY,
            value_types: [
                (CAPTURE, ValueType::Int),
                (CLOSURE, ValueType::Ref(closure)),
                (FUNCTION_VALUE, ValueType::Callable(ENV_SIGNATURE)),
                (LOADED_CALLABLE, loaded_type),
                (CLOSURE_CLONE, ValueType::Ref(closure)),
                (CALL_RESULT, ValueType::Int),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: ENTRY,
                parameters: Vec::new(),
                operations: vec![
                    Operation::Alloc {
                        destination: CLOSURE,
                        layout: closure,
                        extent: Extent::FixedBytes(16),
                        class: AllocationClass::ReferenceCounted,
                    },
                    Operation::ConstInt { destination: CAPTURE, value: 5 },
                    Operation::Store {
                        place: Place::projected(
                            CLOSURE,
                            Projection::Field { index: 1 },
                            ValueType::Int,
                            AccessMode::Write,
                        ),
                        value: CAPTURE,
                        transfer: StoreTransfer::CopyScalar,
                    },
                    Operation::FunctionValue {
                        destination: FUNCTION_VALUE,
                        function: CALLEE,
                    },
                    Operation::Store {
                        place: stored_place,
                        value: stored_value,
                        transfer: StoreTransfer::CopyScalar,
                    },
                    Operation::Load {
                        destination: LOADED_CALLABLE,
                        place: loaded_place,
                        transfer: LoadTransfer::CopyScalar,
                    },
                    Operation::CloneRef {
                        destination: CLOSURE_CLONE,
                        source: CLOSURE,
                    },
                    Operation::VarCall {
                        destinations: vec![CALL_RESULT],
                        callee: LOADED_CALLABLE,
                        signature: ENV_SIGNATURE,
                        arguments: vec![CallArgument::Move(CLOSURE_CLONE)],
                    },
                    Operation::DropRef { object: CLOSURE },
                ],
                terminator: Terminator::Return { values: vec![CALL_RESULT] },
            }],
        };
        let body = Function {
            id: CALLEE,
            signature: ENV_SIGNATURE,
            entry: ENTRY,
            value_types: [
                (ENVIRONMENT, ValueType::Ref(closure)),
                (CAPTURE_READ, ValueType::Int),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: ENTRY,
                parameters: vec![BlockParameter {
                    value: ENVIRONMENT,
                    value_type: ValueType::Ref(closure),
                    ownership: Some(super::super::Ownership::Shared),
                }],
                operations: vec![
                    Operation::Load {
                        destination: CAPTURE_READ,
                        place: Place::projected(
                            ENVIRONMENT,
                            Projection::Field { index: 1 },
                            ValueType::Int,
                            AccessMode::Read,
                        ),
                        transfer: LoadTransfer::CopyScalar,
                    },
                    Operation::DropRef { object: ENVIRONMENT },
                ],
                terminator: Terminator::Return { values: vec![CAPTURE_READ] },
            }],
        };
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: vec![ValueType::Int],
                    contract: contract(vec![ResultContract::Value(ValueType::Int)]),
                },
                environment_signature,
            ],
            imports: Vec::new(),
            functions: vec![driver, body],
            proper_tail_sites: Vec::new(),
        });
        if register_layout {
            module.with_closure_layouts([(closure, ENV_SIGNATURE)].into_iter().collect())
        } else {
            module
        }
    }

    fn callable_place(access: AccessMode, signature: FunctionSignatureId) -> Place {
        Place::projected(
            CLOSURE,
            Projection::Field { index: 0 },
            ValueType::Callable(signature),
            access,
        )
    }

    fn admitted_closure_module() -> CanonicalModule<super::super::LoweredCandidate> {
        closure_module(
            callable_place(AccessMode::Write, ENV_SIGNATURE),
            FUNCTION_VALUE,
            callable_place(AccessMode::Read, ENV_SIGNATURE),
            ValueType::Callable(ENV_SIGNATURE),
            true,
        )
    }

    #[test]
    fn closure_record_stores_loads_and_calls_its_callable_slot() {
        assert!(check_initial(admitted_closure_module()).is_ok());
    }

    #[test]
    fn closure_slot_load_before_store_is_refused() {
        // A fresh record's code slot holds nothing a call may dispatch
        // through: the store to the capture slot alone leaves the lane
        // uninitialized, and the load refuses.
        let module = closure_module(
            Place::projected(
                CLOSURE,
                Projection::Field { index: 1 },
                ValueType::Int,
                AccessMode::Write,
            ),
            CAPTURE,
            callable_place(AccessMode::Read, ENV_SIGNATURE),
            ValueType::Callable(ENV_SIGNATURE),
            true,
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidClosureSlot { function: FUNCTION, object: CLOSURE }
        );
    }

    #[test]
    fn closure_slot_rejects_an_integer_store() {
        // The lane is typed by the registry, so a place claiming `Int` over
        // it is a closure-slot refusal rather than an ordinary type mismatch.
        let module = closure_module(
            Place::projected(
                CLOSURE,
                Projection::Field { index: 0 },
                ValueType::Int,
                AccessMode::Write,
            ),
            CAPTURE,
            callable_place(AccessMode::Read, ENV_SIGNATURE),
            ValueType::Callable(ENV_SIGNATURE),
            true,
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidClosureSlot { function: FUNCTION, object: CLOSURE }
        );
    }

    #[test]
    fn closure_slot_load_rejects_a_different_signature() {
        let module = closure_module(
            callable_place(AccessMode::Write, ENV_SIGNATURE),
            FUNCTION_VALUE,
            callable_place(AccessMode::Read, SIGNATURE),
            ValueType::Callable(SIGNATURE),
            true,
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidClosureSlot { function: FUNCTION, object: CLOSURE }
        );
    }

    #[test]
    fn closure_record_outside_the_registry_has_no_callable_slot() {
        let module = closure_module(
            callable_place(AccessMode::Write, ENV_SIGNATURE),
            FUNCTION_VALUE,
            callable_place(AccessMode::Read, ENV_SIGNATURE),
            ValueType::Callable(ENV_SIGNATURE),
            false,
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidClosureSlot { function: FUNCTION, object: CLOSURE }
        );
    }

    const FUNCTION: FunctionId = FunctionId::new(0);
    const CALLEE: FunctionId = FunctionId::new(1);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
    const CALLEE_SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(1);
    const ENTRY: BlockId = BlockId::new(0);
    const VALUE: ValueId = ValueId::new(0);
    const TAIL_SITE: TailSiteId = TailSiteId::from_digest([11; 32]);

    fn call_contract(
        layout: LayoutId,
        ownership: ParameterOwnership,
        results: Vec<ResultContract>,
        parameter_effects: BTreeMap<u32, Vec<MemoryEffect>>,
    ) -> CallableContract {
        CallableContract {
            parameters: vec![ParameterContract {
                value_type: ValueType::Ref(layout),
                ownership,
            }],
            results,
            parameter_effects,
        }
    }

    fn call_module(
        layouts: LayoutBundle,
        caller_values: BTreeMap<ValueId, ValueType>,
        caller_operations: Vec<Operation>,
        caller_terminator: Terminator,
        callee_operations: Vec<Operation>,
        callee_values: BTreeMap<ValueId, ValueType>,
        callee_terminator: Terminator,
        callee_contract: CallableContract,
        callee_results: Vec<ValueType>,
        callee_parameters: Vec<BlockParameter>,
        tail_sites: Vec<ProperTailSite>,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: Vec::new(),
                    contract: CallableContract {
                        parameters: Vec::new(),
                        results: Vec::new(),
                        parameter_effects: BTreeMap::new(),
                    },
                },
                FunctionSignature {
                    id: CALLEE_SIGNATURE,
                    parameters: callee_parameters
                        .iter()
                        .map(|parameter| parameter.value_type)
                        .collect(),
                    results: callee_results
                        .iter()
                        .copied()
                        .collect(),
                    contract: callee_contract,
                },
            ],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FUNCTION,
                    signature: SIGNATURE,
                    entry: ENTRY,
                    value_types: caller_values,
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: caller_operations,
                        terminator: caller_terminator,
                    }],
                },
                Function {
                    id: CALLEE,
                    signature: CALLEE_SIGNATURE,
                    entry: ENTRY,
                    value_types: callee_values,
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: callee_parameters,
                        operations: callee_operations,
                        terminator: callee_terminator,
                    }],
                },
            ],
            proper_tail_sites: tail_sites,
        })
    }

    #[test]
    fn indirect_reference_value_argument_is_refused() {
        let layout = LayoutId::from_bytes([0; 32]);
        let function = Function {
            id: FUNCTION,
            signature: SIGNATURE,
            entry: ENTRY,
            value_types: [(VALUE, ValueType::Ref(layout))].into_iter().collect(),
            blocks: Vec::new(),
        };
        let signature = FunctionSignature {
            id: CALLEE_SIGNATURE,
            parameters: vec![ValueType::Ref(layout)],
            results: Vec::new(),
            contract: call_contract(layout, ParameterOwnership::Shared, Vec::new(), BTreeMap::new()),
        };

        assert!(matches!(
            super::check_indirect_reference_arguments(
                &function,
                &[super::super::CallArgument::Value(VALUE)],
                &signature,
            ),
            Err(InitialCheckError::UnsupportedIndirectCall { function }) if function == FUNCTION
        ));
    }

    #[test]
    fn indirect_capability_arguments_require_exact_effect_free_value_contracts() {
        let kind = super::super::CapabilityKindId::new(17);
        let other_kind = super::super::CapabilityKindId::new(0);
        for actual in [ValueType::Capability(kind), ValueType::NullableCapability(kind)] {
            let function = Function {
                id: FUNCTION, signature: SIGNATURE, entry: ENTRY,
                value_types: [(VALUE, actual)].into_iter().collect(),
                blocks: Vec::new(),
            };
            let valid = FunctionSignature {
                id: CALLEE_SIGNATURE,
                parameters: vec![actual], results: Vec::new(),
                contract: CallableContract {
                    parameters: vec![ParameterContract {
                        value_type: actual, ownership: ParameterOwnership::Value,
                    }],
                    results: Vec::new(), parameter_effects: BTreeMap::new(),
                },
            };
            let check = |signature: &FunctionSignature, argument| {
                super::check_indirect_reference_arguments(&function, &[argument], signature)
            };
            assert!(check(&valid, CallArgument::Value(VALUE)).is_ok());
            let mut empty_effects = valid.clone();
            empty_effects.contract.parameter_effects.insert(0, Vec::new());
            assert!(check(&empty_effects, CallArgument::Value(VALUE)).is_ok());
            assert!(check(&valid, CallArgument::Move(VALUE)).is_err());
            assert!(check(&valid, CallArgument::Value(ValueId::new(u32::MAX))).is_err());
            for wrong in [ValueType::Capability(other_kind), ValueType::NullableCapability(other_kind),
                ValueType::Capability(kind), ValueType::NullableCapability(kind), ValueType::Int]
            {
                if wrong == actual { continue; }
                let mut signature = valid.clone();
                signature.parameters[0] = wrong;
                signature.contract.parameters[0].value_type = wrong;
                assert!(check(&signature, CallArgument::Value(VALUE)).is_err());
            }
            let mut wrong_signature = valid.clone();
            wrong_signature.parameters[0] = ValueType::Int;
            assert!(check(&wrong_signature, CallArgument::Value(VALUE)).is_err());
            for ownership in [ParameterOwnership::Unique, ParameterOwnership::Shared,
                ParameterOwnership::WriteBack, ParameterOwnership::UniqueWriteBack]
            {
                let mut signature = valid.clone();
                signature.contract.parameters[0].ownership = ownership;
                assert!(check(&signature, CallArgument::Value(VALUE)).is_err());
            }
            for effect in [MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release,
                MemoryEffect::Write, MemoryEffect::Reclaim]
            {
                let mut signature = valid.clone();
                signature.contract.parameter_effects.insert(0, vec![effect]);
                assert!(check(&signature, CallArgument::Value(VALUE)).is_err());
            }
            let mut missing_contract = valid.clone();
            missing_contract.contract.parameters.clear();
            assert!(check(&missing_contract, CallArgument::Value(VALUE)).is_err());
        }
    }

    #[test]
    fn indirect_capability_support_preserves_ref_gc_move_restrictions() {
        let layout = LayoutId::from_bytes([0; 32]);
        for value_type in [ValueType::Ref(layout), ValueType::Gc(crate::canonical_safewir::GcTypeId::new(0))] {
            let function = Function {
                id: FUNCTION, signature: SIGNATURE, entry: ENTRY,
                value_types: [(VALUE, value_type)].into_iter().collect(), blocks: Vec::new(),
            };
            let mut signature = FunctionSignature {
                id: CALLEE_SIGNATURE, parameters: vec![value_type], results: Vec::new(),
                contract: CallableContract {
                    parameters: vec![ParameterContract { value_type, ownership: ParameterOwnership::Shared }],
                    results: Vec::new(), parameter_effects: BTreeMap::new(),
                },
            };
            assert!(super::check_indirect_reference_arguments(
                &function, &[CallArgument::Move(VALUE)], &signature,
            ).is_ok());
            assert!(super::check_indirect_reference_arguments(
                &function, &[CallArgument::Value(VALUE)], &signature,
            ).is_err());
            signature.contract.parameters[0].ownership = ParameterOwnership::Value;
            assert!(super::check_indirect_reference_arguments(
                &function, &[CallArgument::Value(VALUE)], &signature,
            ).is_err());
            assert!(super::check_indirect_reference_arguments(
                &function, &[CallArgument::Move(VALUE)], &signature,
            ).is_err());
        }
    }

    fn allocated_list_with_header(
        layout: LayoutId,
        header_bytes: u32,
    ) -> (BTreeMap<ValueId, ValueType>, Vec<Operation>) {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        (
            [
                (count, ValueType::Size),
                (capacity, ValueType::Size),
                (object, ValueType::Ref(layout)),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize { destination: count, value: 0 },
                Operation::ConstSize { destination: capacity, value: 1 },
                Operation::Alloc {
                    destination: object,
                    layout,
                    extent: Extent::Counted {
                        header_bytes,
                        count,
                        capacity,
                        stride_bytes: 4,
                    },
                    class: AllocationClass::ReferenceCounted,
                },
            ],
        )
    }

    fn allocated_list(layout: LayoutId) -> (BTreeMap<ValueId, ValueType>, Vec<Operation>) {
        allocated_list_with_header(layout, 4)
    }

    fn contract(results: Vec<ResultContract>) -> CallableContract {
        CallableContract {
            parameters: Vec::new(),
            results,
            parameter_effects: BTreeMap::new(),
        }
    }

    fn empty_layouts() -> LayoutBundle {
        LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("an empty canonical layout bundle is valid")
    }

    fn dynamic_u32_layout() -> (LayoutBundle, LayoutId) {
        let mut layouts = LayoutInterner::new();
        let element = layouts
            .intern_scalar(ScalarKind::U32)
            .expect("u32 layout is valid");
        let list = layouts
            .intern_runtime_buffer(element)
            .expect("u32 runtime buffer layout is valid");
        let bundle = LayoutBundle::from_interner(&layouts, [list])
            .expect("runtime buffer layout bundle is valid");
        (bundle, list)
    }

    fn dynamic_byte_layout() -> (LayoutBundle, LayoutId) {
        let mut layouts = LayoutInterner::new();
        let element = layouts
            .intern_scalar(ScalarKind::RawByte)
            .expect("raw byte layout is valid");
        let buffer = layouts
            .intern_runtime_buffer(element)
            .expect("byte runtime buffer layout is valid");
        let bundle = LayoutBundle::from_interner(&layouts, [buffer])
            .expect("byte runtime buffer layout bundle is valid");
        (bundle, buffer)
    }

    fn packed_u32_layout() -> (LayoutBundle, LayoutId) {
        let mut layouts = LayoutInterner::new();
        let element = layouts
            .intern_scalar(ScalarKind::U32)
            .expect("u32 layout is valid");
        let list = layouts
            .intern_packed_list(element, RcHeader::Required)
            .expect("u32 packed list layout is valid");
        let bundle = LayoutBundle::from_interner(&layouts, [list])
            .expect("packed list layout bundle is valid");
        (bundle, list)
    }

    fn lowered(
        value_types: BTreeMap<ValueId, ValueType>,
        operations: Vec<Operation>,
        terminator: Terminator,
        result_types: Vec<ValueType>,
        tail_sites: Vec<ProperTailSite>,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        let result_contracts = result_types
            .iter()
            .copied()
            .map(ResultContract::Value)
            .collect();
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: result_types,
                contract: contract(result_contracts),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types,
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations,
                    terminator,
                }],
            }],
            proper_tail_sites: tail_sites,
        })
    }

    fn host_module(
        function_parameter: ValueType,
        import_parameter: ValueType,
        argument: HostCallArgument,
        layouts: LayoutBundle,
        imports: Vec<ImportContract>,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        let function_signature = FunctionSignature {
            id: SIGNATURE,
            parameters: vec![function_parameter],
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: vec![ParameterContract {
                    value_type: function_parameter,
                    ownership: ParameterOwnership::Value,
                }],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let import_signature = FunctionSignature {
            id: FunctionSignatureId::new(1),
            parameters: vec![import_parameter],
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: vec![ParameterContract {
                    value_type: import_parameter,
                    ownership: ParameterOwnership::Value,
                }],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![function_signature, import_signature],
            imports,
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [(VALUE, function_parameter), (ValueId::new(1), ValueType::Int)]
                    .into_iter()
                    .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: vec![BlockParameter {
                        value: VALUE,
                        value_type: function_parameter,
                        ownership: None,
                    }],
                    operations: vec![Operation::HostCall {
                        destinations: vec![ValueId::new(1)],
                        import: ImportId::new(0),
                        arguments: vec![argument],
                    }],
                    terminator: Terminator::Return {
                        values: vec![ValueId::new(1)],
                    },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    fn allocated_host_buffer_module(
        raw_bytes: bool,
        start_value: u64,
        length_value: u64,
        count_value: u64,
        contract_extent: HostBufferExtent,
        supplied_extent: HostBufferExtent,
    ) -> (
        CanonicalModule<super::super::LoweredCandidate>,
        CanonicalAbiCatalog,
    ) {
        let (layouts, layout) = if raw_bytes {
            dynamic_byte_layout()
        } else {
            dynamic_u32_layout()
        };
        let start = ValueId::new(0);
        let length = ValueId::new(1);
        let count = ValueId::new(2);
        let capacity = ValueId::new(3);
        let capacity_ok = ValueId::new(4);
        let object = ValueId::new(5);
        let result = ValueId::new(6);
        let import_signature_id = FunctionSignatureId::new(1);
        let import = ImportContract {
            id: ImportId::new(0),
            signature: import_signature_id,
            abi_identity: ImportContractIdentity::AuthenticatedAbi([9; 32]),
            module: "test".to_string(),
            name: "buffer".to_string(),
            parameter_roles: vec![HostParameterRole::Buffer {
                value_type: ValueType::Ref(layout),
                access: AccessMode::Read,
                extent: contract_extent,
            }],
            result_roles: vec![HostResultRole::Value(ValueType::Int)],
            mutation: HostMutation::ReadOnly,
            reentrancy: HostReentrancy::Forbidden,
        };
        let mut value_types: BTreeMap<ValueId, ValueType> = [
            (start, ValueType::Size),
            (length, ValueType::Size),
            (count, ValueType::Size),
            (capacity, ValueType::Size),
            (capacity_ok, ValueType::Bool),
            (object, ValueType::Ref(layout)),
            (result, ValueType::Int),
        ]
        .into_iter()
        .collect();
        let mut operations = vec![
            Operation::ConstSize { destination: start, value: start_value },
            Operation::ConstSize { destination: length, value: length_value },
            Operation::ConstSize { destination: count, value: count_value },
            Operation::ConstSize { destination: capacity, value: count_value },
            Operation::Compare {
                destination: capacity_ok,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: count,
                right: capacity,
            },
            Operation::Guard { condition: capacity_ok, kind: GuardKind::Bounds },
            Operation::Alloc {
                destination: object,
                layout,
                extent: Extent::Counted {
                    header_bytes: 4,
                    count,
                    capacity,
                    stride_bytes: if raw_bytes { 1 } else { 4 },
                },
                class: AllocationClass::ReferenceCounted,
            },
        ];
        for index_value in 0..count_value {
            let index = ValueId::new(100 + index_value as u32 * 2);
            let stored = ValueId::new(101 + index_value as u32 * 2);
            let in_capacity = ValueId::new(200 + index_value as u32);
            value_types.insert(index, ValueType::Size);
            value_types.insert(stored, ValueType::Size);
            value_types.insert(in_capacity, ValueType::Bool);
            operations.push(Operation::ConstSize { destination: index, value: index_value });
            operations.push(Operation::ConstSize { destination: stored, value: 0 });
            operations.push(Operation::Compare {
                destination: in_capacity,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: capacity,
            });
            operations.push(Operation::Guard { condition: in_capacity, kind: GuardKind::Bounds });
            operations.push(Operation::Store {
                place: Place::projected(
                    object,
                    Projection::Element { index },
                    ValueType::Size,
                    AccessMode::Write,
                ),
                value: stored,
                transfer: StoreTransfer::CopyScalar,
            });
        }
        operations.extend([
            Operation::HostCall {
                destinations: vec![result],
                import: ImportId::new(0),
                arguments: vec![HostCallArgument::Buffer {
                    place: Place::projected(
                        object,
                        Projection::Slice { start, length },
                        ValueType::Ref(layout),
                        AccessMode::Read,
                    ),
                    extent: supplied_extent,
                }],
            },
            Operation::DropRef { object },
        ]);
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: vec![ValueType::Int],
                    contract: contract(vec![ResultContract::Value(ValueType::Int)]),
                },
                FunctionSignature {
                    id: import_signature_id,
                    parameters: vec![ValueType::Ref(layout)],
                    results: vec![ValueType::Int],
                    contract: CallableContract {
                        parameters: vec![ParameterContract {
                            value_type: ValueType::Ref(layout),
                            ownership: ParameterOwnership::Value,
                        }],
                        results: vec![ResultContract::Value(ValueType::Int)],
                        parameter_effects: BTreeMap::new(),
                    },
                },
            ],
            imports: vec![import.clone()],
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types,
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations,
                    terminator: Terminator::Return { values: vec![result] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        (module, catalog)
    }

    #[test]
    fn canonical_abort_requires_both_bounded_read_slices_not_integer_pointers() {
        for case in 0..5 {
            let (mut module, _) = allocated_host_buffer_module(
                true, 0, 2, 1, HostBufferExtent::Slice, HostBufferExtent::Slice,
            );
            let text = module.functions[0].value_types[&ValueId::new(5)];
            let ValueType::Ref(layout) = text else { unreachable!() };
            let id = FunctionSignatureId::new(1);
            let parameters = vec![text, text, ValueType::Size];
            module.signatures[1] = FunctionSignature {
                id, parameters: parameters.clone(), results: Vec::new(),
                contract: CallableContract {
                    parameters: parameters.into_iter().map(|value_type| ParameterContract {
                        value_type, ownership: ParameterOwnership::Value,
                    }).collect(),
                    results: Vec::new(),
                    parameter_effects: BTreeMap::from([(0, vec![MemoryEffect::Read]), (1, vec![MemoryEffect::Read])]),
                },
            };
            let catalog = CanonicalAbiCatalog::from_imports(Vec::new()).with_abort_import(id, layout).unwrap();
            let import = catalog.imports().pop().unwrap();
            module.imports = vec![import.clone()];
            let buffer = |length| HostCallArgument::Buffer {
                place: Place::projected(ValueId::new(5), Projection::Slice {
                    start: ValueId::new(0), length: ValueId::new(length),
                }, text, AccessMode::Read), extent: HostBufferExtent::Slice,
            };
            // Count=1 is the valid extent; length=2 exceeds the same allocation.
            let mut arguments = vec![buffer(2), buffer(2), HostCallArgument::Value(ValueId::new(2))];
            if case == 1 || case == 2 { arguments[case - 1] = buffer(1); }
            if case == 3 || case == 4 {
                arguments[case - 3] = HostCallArgument::Value(ValueId::new(5));
            }
            let function = &mut module.functions[0];
            function.value_types.remove(&ValueId::new(6));
            let block = &mut function.blocks[0];
            block.operations.retain(|operation| !matches!(operation, Operation::HostCall { .. } | Operation::DropRef { .. }));
            block.operations.push(Operation::HostCall { destinations: Vec::new(), import: import.id, arguments });
            block.terminator = Terminator::Trap;
            let result = check_initial(module.with_abi_catalog(catalog));
            if case == 0 {
                result.expect("both authenticated read slices are bounded");
            } else {
                assert!(result.is_err(), "case {case}: reject an unbounded slice or a raw reference carrier");
            }
        }
    }

    #[test]
    fn canonical_list_bounds_diagnostic_grants_no_slice_access_or_liveness() {
        for case in 0..6 {
            let (mut module, _) = allocated_host_buffer_module(true, 0, 2, 1,
                HostBufferExtent::Slice, HostBufferExtent::Slice);
            let text = module.functions[0].value_types[&ValueId::new(5)];
            let ValueType::Ref(layout) = text else { unreachable!() };
            let id = FunctionSignatureId::new(1);
            let parameters = vec![ValueType::Int, ValueType::Size, text, ValueType::Size];
            module.signatures[1] = FunctionSignature {
                id, parameters: parameters.clone(), results: Vec::new(),
                contract: CallableContract {
                    parameters: parameters.into_iter().map(|value_type| ParameterContract { value_type, ownership: ParameterOwnership::Value }).collect(),
                    results: Vec::new(), parameter_effects: BTreeMap::from([(2, vec![MemoryEffect::Read])]),
                },
            };
            let catalog = CanonicalAbiCatalog::from_imports(Vec::new()).with_list_bounds_import(id, layout).unwrap();
            let import = catalog.imports().pop().unwrap();
            module.imports = vec![import.clone()];
            let object = ValueId::new(5);
            let index = ValueId::new(6);
            let mut label = HostCallArgument::Buffer {
                place: Place::projected(object, Projection::Slice {
                    start: ValueId::new(0), length: ValueId::new(if case == 1 { 1 } else { 2 }),
                }, text, AccessMode::Read), extent: HostBufferExtent::Slice,
            };
            if case == 2 { label = HostCallArgument::Value(object); }
            let function = &mut module.functions[0];
            let block = &mut function.blocks[0];
            block.operations.retain(|operation| !matches!(operation, Operation::HostCall { .. } | Operation::DropRef { .. }));
            block.operations.push(Operation::WidenSize { destination: index, value: ValueId::new(0) });
            if case == 3 { block.operations.push(Operation::DropRef { object }); }
            block.operations.push(Operation::HostCall {
                destinations: Vec::new(), import: import.id,
                arguments: vec![HostCallArgument::Value(index), HostCallArgument::Value(ValueId::new(2)), label, HostCallArgument::Value(ValueId::new(2))],
            });
            if case == 4 || case == 5 {
                let result = ValueId::new(7);
                function.value_types.insert(result, ValueType::Size);
                // A returning call cannot authorize an unguarded element read.
                // Nor may a released label be reused after that call.
                if case == 5 { block.operations.push(Operation::DropRef { object }); }
                block.operations.push(Operation::Load { destination: result,
                    place: Place::projected(object, Projection::Element { index: ValueId::new(2) }, ValueType::Size, AccessMode::Read),
                    transfer: LoadTransfer::CopyScalar,
                });
            }
            block.terminator = Terminator::Trap;
            let result = check_initial(module.with_abi_catalog(catalog));
            if case == 0 { result.expect("bounded live diagnostic label"); }
            else { assert!(result.is_err(), "case {case} must not gain slice, bounds, or lifetime authority"); }
        }
    }

    /// A `float_to_str`-shaped host call: the guest hands the host a unique
    /// raw-byte buffer to fill, then publishes the returned byte count under
    /// its own capacity guard.
    fn write_buffer_host_module(
        slice_length: u64,
        mutation: HostMutation,
        share_object: bool,
    ) -> (
        CanonicalModule<super::super::LoweredCandidate>,
        CanonicalAbiCatalog,
    ) {
        let (layouts, layout) = dynamic_byte_layout();
        let start = ValueId::new(0);
        let length = ValueId::new(1);
        let count = ValueId::new(2);
        let capacity = ValueId::new(3);
        let capacity_ok = ValueId::new(4);
        let object = ValueId::new(5);
        let rendered = ValueId::new(6);
        let written = ValueId::new(7);
        let written_ok = ValueId::new(8);
        let alias = ValueId::new(9);
        let import_signature_id = FunctionSignatureId::new(1);
        let import = ImportContract {
            id: ImportId::new(0),
            signature: import_signature_id,
            abi_identity: ImportContractIdentity::AuthenticatedAbi([9; 32]),
            module: "test".to_string(),
            name: "float_to_str".to_string(),
            parameter_roles: vec![
                HostParameterRole::Value(ValueType::Float),
                HostParameterRole::Buffer {
                    value_type: ValueType::Ref(layout),
                    access: AccessMode::Write,
                    extent: HostBufferExtent::AtLeast(32),
                },
            ],
            result_roles: vec![HostResultRole::Value(ValueType::Size)],
            mutation,
            reentrancy: HostReentrancy::Forbidden,
        };
        let mut value_types: BTreeMap<ValueId, ValueType> = [
            (start, ValueType::Size),
            (length, ValueType::Size),
            (count, ValueType::Size),
            (capacity, ValueType::Size),
            (capacity_ok, ValueType::Bool),
            (object, ValueType::Ref(layout)),
            (rendered, ValueType::Float),
            (written, ValueType::Size),
            (written_ok, ValueType::Bool),
        ]
        .into_iter()
        .collect();
        let mut operations = vec![
            Operation::ConstSize { destination: start, value: 0 },
            Operation::ConstSize { destination: length, value: slice_length },
            Operation::ConstSize { destination: count, value: 32 },
            Operation::ConstSize { destination: capacity, value: 32 },
            Operation::ConstFloat { destination: rendered, bits: 0 },
            Operation::Compare {
                destination: capacity_ok,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: count,
                right: capacity,
            },
            Operation::Guard { condition: capacity_ok, kind: GuardKind::Bounds },
            Operation::Alloc {
                destination: object,
                layout,
                extent: Extent::Counted {
                    header_bytes: 4,
                    count,
                    capacity,
                    stride_bytes: 1,
                },
                class: AllocationClass::ReferenceCounted,
            },
            Operation::UpdateCount { object, count },
        ];
        if share_object {
            // A second live reference weakens the buffer to shared storage.
            value_types.insert(alias, ValueType::Ref(layout));
            operations.push(Operation::CloneRef { destination: alias, source: object });
        }
        operations.extend([
            Operation::HostCall {
                destinations: vec![written],
                import: ImportId::new(0),
                arguments: vec![
                    HostCallArgument::Value(rendered),
                    HostCallArgument::Buffer {
                        place: Place::projected(
                            object,
                            Projection::Slice { start, length },
                            ValueType::Ref(layout),
                            AccessMode::Write,
                        ),
                        extent: HostBufferExtent::AtLeast(32),
                    },
                ],
            },
            // The guest publishes the host-written length under its own guard.
            Operation::Compare {
                destination: written_ok,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: written,
                right: capacity,
            },
            Operation::Guard { condition: written_ok, kind: GuardKind::Bounds },
            Operation::UpdateCount { object, count: written },
            // Callee-drops: the buffer is released before exit.
            Operation::DropRef { object },
        ]);
        if share_object {
            operations.push(Operation::DropRef { object: alias });
        }
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: vec![ValueType::Size],
                    contract: contract(vec![ResultContract::Value(ValueType::Size)]),
                },
                FunctionSignature {
                    id: import_signature_id,
                    parameters: vec![ValueType::Float, ValueType::Ref(layout)],
                    results: vec![ValueType::Size],
                    contract: CallableContract {
                        parameters: vec![
                            ParameterContract {
                                value_type: ValueType::Float,
                                ownership: ParameterOwnership::Value,
                            },
                            ParameterContract {
                                value_type: ValueType::Ref(layout),
                                ownership: ParameterOwnership::Value,
                            },
                        ],
                        results: vec![ResultContract::Value(ValueType::Size)],
                        parameter_effects: BTreeMap::new(),
                    },
                },
            ],
            imports: vec![import.clone()],
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types,
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations,
                    terminator: Terminator::Return { values: vec![written] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        (module, catalog)
    }

    /// How the guest states the length lane of a checked WRITE slice.
    #[derive(Debug, Clone, Copy)]
    enum WriteSliceLength {
        /// The buffer's own capacity value: the whole writable body.
        Capacity,
        /// A constant length the checker must prove against the capacity.
        Constant(u64),
    }

    /// A `dir_read_into`-shaped host call: the guest hands the host a checked
    /// WRITE slice — (ptr, len) with `len` the buffer's capacity — and
    /// publishes `min(returned, capacity)` itself afterwards.
    fn write_slice_host_module(
        length: WriteSliceLength,
    ) -> (
        CanonicalModule<super::super::LoweredCandidate>,
        CanonicalAbiCatalog,
    ) {
        let (layouts, layout) = dynamic_byte_layout();
        let start = ValueId::new(0);
        let count = ValueId::new(1);
        let capacity = ValueId::new(2);
        let capacity_ok = ValueId::new(3);
        let object = ValueId::new(4);
        let written = ValueId::new(5);
        let written_ok = ValueId::new(6);
        let constant_length = ValueId::new(7);
        let import_signature_id = FunctionSignatureId::new(1);
        let import = ImportContract {
            id: ImportId::new(0),
            signature: import_signature_id,
            abi_identity: ImportContractIdentity::AuthenticatedAbi([9; 32]),
            module: "test".to_string(),
            name: "dir_read_into".to_string(),
            parameter_roles: vec![HostParameterRole::Buffer {
                value_type: ValueType::Ref(layout),
                access: AccessMode::Write,
                extent: HostBufferExtent::Slice,
            }],
            result_roles: vec![HostResultRole::Value(ValueType::Size)],
            mutation: HostMutation::WriteBuffers,
            reentrancy: HostReentrancy::Forbidden,
        };
        let mut value_types: BTreeMap<ValueId, ValueType> = [
            (start, ValueType::Size),
            (count, ValueType::Size),
            (capacity, ValueType::Size),
            (capacity_ok, ValueType::Bool),
            (object, ValueType::Ref(layout)),
            (written, ValueType::Size),
            (written_ok, ValueType::Bool),
        ]
        .into_iter()
        .collect();
        let mut operations = vec![
            Operation::ConstSize { destination: start, value: 0 },
            Operation::ConstSize { destination: count, value: 32 },
            Operation::ConstSize { destination: capacity, value: 32 },
            Operation::Compare {
                destination: capacity_ok,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: count,
                right: capacity,
            },
            Operation::Guard { condition: capacity_ok, kind: GuardKind::Bounds },
            Operation::Alloc {
                destination: object,
                layout,
                extent: Extent::Counted {
                    header_bytes: 4,
                    count,
                    capacity,
                    stride_bytes: 1,
                },
                class: AllocationClass::ReferenceCounted,
            },
            Operation::UpdateCount { object, count },
        ];
        let slice_length = match length {
            WriteSliceLength::Capacity => capacity,
            WriteSliceLength::Constant(value) => {
                value_types.insert(constant_length, ValueType::Size);
                operations.push(Operation::ConstSize { destination: constant_length, value });
                constant_length
            }
        };
        operations.extend([
            Operation::HostCall {
                destinations: vec![written],
                import: ImportId::new(0),
                arguments: vec![HostCallArgument::Buffer {
                    place: Place::projected(
                        object,
                        Projection::Slice { start, length: slice_length },
                        ValueType::Ref(layout),
                        AccessMode::Write,
                    ),
                    extent: HostBufferExtent::Slice,
                }],
            },
            // The guest publishes the host-written length under its own guard.
            Operation::Compare {
                destination: written_ok,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: written,
                right: capacity,
            },
            Operation::Guard { condition: written_ok, kind: GuardKind::Bounds },
            Operation::UpdateCount { object, count: written },
            Operation::DropRef { object },
        ]);
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: vec![ValueType::Size],
                    contract: contract(vec![ResultContract::Value(ValueType::Size)]),
                },
                FunctionSignature {
                    id: import_signature_id,
                    parameters: vec![ValueType::Ref(layout)],
                    results: vec![ValueType::Size],
                    contract: CallableContract {
                        parameters: vec![ParameterContract {
                            value_type: ValueType::Ref(layout),
                            ownership: ParameterOwnership::Value,
                        }],
                        results: vec![ResultContract::Value(ValueType::Size)],
                        parameter_effects: BTreeMap::new(),
                    },
                },
            ],
            imports: vec![import.clone()],
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types,
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations,
                    terminator: Terminator::Return { values: vec![written] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        (module, catalog)
    }

    /// The new backend form: a WRITE slice of the buffer's whole capacity on
    /// a unique, non-static raw-byte buffer.
    #[test]
    fn write_slice_host_call_is_admitted_at_capacity() {
        let (module, catalog) = write_slice_host_module(WriteSliceLength::Capacity);
        let result = super::check_initial_with_catalog(module, &catalog);
        assert!(result.is_ok(), "{result:?}");
    }

    /// A shorter lane than the capacity is admitted only because constants
    /// prove it lies inside the capacity.
    #[test]
    fn write_slice_host_call_admits_a_constant_range_inside_capacity() {
        let (module, catalog) = write_slice_host_module(WriteSliceLength::Constant(16));
        let result = super::check_initial_with_catalog(module, &catalog);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn host_write_does_not_initialize_unspecified_capacity() {
        let (mut module, catalog) = write_slice_host_module(WriteSliceLength::Constant(16));
        let index = ValueId::new(8);
        let loaded = ValueId::new(9);
        let in_bounds = ValueId::new(10);
        let function = &mut module.functions[0];
        function.value_types.insert(index, ValueType::Size);
        function.value_types.insert(loaded, ValueType::Size);
        function.value_types.insert(in_bounds, ValueType::Bool);
        let operations = &mut function.blocks[0].operations;
        let host_call = operations.iter()
            .position(|operation| matches!(operation, Operation::HostCall { .. }))
            .expect("host call");
        operations.splice(host_call + 1..host_call + 1, [
            Operation::ConstSize { destination: index, value: 31 },
            Operation::Compare {
                destination: in_bounds,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: ValueId::new(1),
            },
            Operation::Guard { condition: in_bounds, kind: GuardKind::Bounds },
            Operation::Load {
                destination: loaded,
                place: Place::projected(
                    ValueId::new(4),
                    Projection::Element { index },
                    ValueType::Size,
                    AccessMode::Read,
                ),
                transfer: LoadTransfer::CopyScalar,
            },
        ]);
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::UnsupportedMemoryOperation {
                operation: "Load(uninitialized scalar)", ..
            })
        ));
    }

    #[test]
    fn write_slice_host_call_refuses_a_lane_beyond_capacity() {
        let (module, catalog) = write_slice_host_module(WriteSliceLength::Constant(64));
        let result = super::check_initial_with_catalog(module, &catalog);
        assert!(
            matches!(result, Err(InitialCheckError::MissingBoundsGuard { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn write_buffer_host_call_is_admitted_within_capacity() {
        let (module, catalog) = write_buffer_host_module(32, HostMutation::WriteBuffers, false);
        let result = super::check_initial_with_catalog(module, &catalog);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn write_buffer_host_call_refuses_a_slice_beyond_capacity() {
        let (module, catalog) = write_buffer_host_module(64, HostMutation::WriteBuffers, false);
        let result = super::check_initial_with_catalog(module, &catalog);
        assert!(result.is_err(), "a 64-byte slice of a 32-byte buffer must be refused");
        println!("slice beyond capacity: {:?}", result.err());
    }

    #[test]
    fn write_buffer_host_call_refuses_a_shared_object() {
        let (module, catalog) = write_buffer_host_module(32, HostMutation::WriteBuffers, true);
        let result = super::check_initial_with_catalog(module, &catalog);
        assert!(result.is_err(), "a shared buffer must not be handed to a host write");
        println!("shared object: {:?}", result.err());
    }

    #[test]
    fn write_buffer_host_call_refuses_a_read_only_contract() {
        let (module, catalog) = write_buffer_host_module(32, HostMutation::ReadOnly, false);
        let result = super::check_initial_with_catalog(module, &catalog);
        assert!(
            matches!(result, Err(InitialCheckError::UnsupportedHostMutation { .. })),
            "{result:?}"
        );
    }

    fn byte_copy_module(raw_bytes: bool) -> CanonicalModule<super::super::LoweredCandidate> {
        let (layouts, layout) = if raw_bytes {
            dynamic_byte_layout()
        } else {
            dynamic_u32_layout()
        };
        let start = ValueId::new(0);
        let length = ValueId::new(1);
        let capacity = ValueId::new(2);
        let capacity_ok = ValueId::new(3);
        let source = ValueId::new(4);
        let destination = ValueId::new(5);
        let index_1 = ValueId::new(6);
        let index_2 = ValueId::new(7);
        let index_3 = ValueId::new(8);
        let stored = ValueId::new(9);
        let check_0 = ValueId::new(10);
        let check_1 = ValueId::new(11);
        let check_2 = ValueId::new(12);
        let check_3 = ValueId::new(13);
        let stride_bytes = if raw_bytes { 1 } else { 4 };
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: contract(Vec::new()),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (start, ValueType::Size),
                    (length, ValueType::Size),
                    (capacity, ValueType::Size),
                    (capacity_ok, ValueType::Bool),
                    (source, ValueType::Ref(layout)),
                    (destination, ValueType::Ref(layout)),
                    (index_1, ValueType::Size),
                    (index_2, ValueType::Size),
                    (index_3, ValueType::Size),
                    (stored, ValueType::Size),
                    (check_0, ValueType::Bool),
                    (check_1, ValueType::Bool),
                    (check_2, ValueType::Bool),
                    (check_3, ValueType::Bool),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: start, value: 0 },
                        Operation::ConstSize { destination: length, value: 4 },
                        Operation::ConstSize { destination: capacity, value: 4 },
                        Operation::Compare {
                            destination: capacity_ok,
                            operator: ComparisonOperator::LessEqualUnsigned,
                            left: length,
                            right: capacity,
                        },
                        Operation::Guard { condition: capacity_ok, kind: GuardKind::Bounds },
                        Operation::Alloc {
                            destination: source,
                            layout,
                            extent: Extent::Counted {
                                header_bytes: 4,
                                count: length,
                                capacity,
                                stride_bytes,
                            },
                            class: AllocationClass::ReferenceCounted,
                        },
                        Operation::Alloc {
                            destination,
                            layout,
                            extent: Extent::Counted {
                                header_bytes: 4,
                                count: length,
                                capacity,
                                stride_bytes,
                            },
                            class: AllocationClass::ReferenceCounted,
                        },
                        Operation::ConstSize { destination: index_1, value: 1 },
                        Operation::ConstSize { destination: index_2, value: 2 },
                        Operation::ConstSize { destination: index_3, value: 3 },
                        Operation::ConstSize { destination: stored, value: 0 },
                        Operation::Compare { destination: check_0, operator: ComparisonOperator::LessUnsigned, left: start, right: capacity },
                        Operation::Guard { condition: check_0, kind: GuardKind::Bounds },
                        Operation::Compare { destination: check_1, operator: ComparisonOperator::LessUnsigned, left: index_1, right: capacity },
                        Operation::Guard { condition: check_1, kind: GuardKind::Bounds },
                        Operation::Compare { destination: check_2, operator: ComparisonOperator::LessUnsigned, left: index_2, right: capacity },
                        Operation::Guard { condition: check_2, kind: GuardKind::Bounds },
                        Operation::Compare { destination: check_3, operator: ComparisonOperator::LessUnsigned, left: index_3, right: capacity },
                        Operation::Guard { condition: check_3, kind: GuardKind::Bounds },
                        Operation::Store {
                            place: Place::projected(source, Projection::Element { index: start }, ValueType::Size, AccessMode::Write),
                            value: stored,
                            transfer: StoreTransfer::CopyScalar,
                        },
                        Operation::Store {
                            place: Place::projected(source, Projection::Element { index: index_1 }, ValueType::Size, AccessMode::Write),
                            value: stored,
                            transfer: StoreTransfer::CopyScalar,
                        },
                        Operation::Store {
                            place: Place::projected(source, Projection::Element { index: index_2 }, ValueType::Size, AccessMode::Write),
                            value: stored,
                            transfer: StoreTransfer::CopyScalar,
                        },
                        Operation::Store {
                            place: Place::projected(source, Projection::Element { index: index_3 }, ValueType::Size, AccessMode::Write),
                            value: stored,
                            transfer: StoreTransfer::CopyScalar,
                        },
                        Operation::CopyBytes {
                            destination: Place::projected(
                                destination,
                                Projection::Slice { start, length },
                                ValueType::Ref(layout),
                                AccessMode::Write,
                            ),
                            source: Place::projected(
                                source,
                                Projection::Slice { start, length },
                                ValueType::Ref(layout),
                                AccessMode::Read,
                            ),
                            length,
                        },
                        // Callee-drops: both buffers are released before exit.
                        Operation::DropRef { object: source },
                        Operation::DropRef { object: destination },
                    ],
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    fn host_import(
        parameter_role: HostParameterRole,
        result_role: HostResultRole,
        mutation: HostMutation,
        reentrancy: HostReentrancy,
    ) -> ImportContract {
        ImportContract {
            id: ImportId::new(0),
            signature: FunctionSignatureId::new(1),
            abi_identity: ImportContractIdentity::AuthenticatedAbi([9; 32]),
            module: "test".to_string(),
            name: "host".to_string(),
            parameter_roles: vec![parameter_role],
            result_roles: vec![result_role],
            mutation,
            reentrancy,
        }
    }

    #[test]
    fn typed_capability_host_call_is_admitted() {
        let kind = super::super::CapabilityKindId::new(4);
        let import = host_import(
            HostParameterRole::Capability(kind),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let module = host_module(
            ValueType::Capability(kind),
            ValueType::Capability(kind),
            HostCallArgument::Capability(VALUE),
            empty_layouts(),
            vec![import.clone()],
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        assert!(super::check_initial_with_catalog(module, &catalog).is_ok());
    }

    #[test]
    fn integer_cannot_fill_capability_host_role() {
        let kind = super::super::CapabilityKindId::new(4);
        let import = host_import(
            HostParameterRole::Capability(kind),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let module = host_module(
            ValueType::Int,
            ValueType::Capability(kind),
            HostCallArgument::Value(VALUE),
            empty_layouts(),
            vec![import.clone()],
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn integer_cannot_be_wrapped_as_capability_host_argument() {
        let kind = super::super::CapabilityKindId::new(4);
        let import = host_import(
            HostParameterRole::Capability(kind),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let module = host_module(
            ValueType::Int,
            ValueType::Capability(kind),
            HostCallArgument::Capability(VALUE),
            empty_layouts(),
            vec![import.clone()],
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::TypeMismatch {
                expected: ValueType::Capability(expected),
                actual: ValueType::Int,
                ..
            }) if expected == kind
        ));
    }

    #[test]
    fn wrong_capability_kind_is_rejected() {
        let expected = super::super::CapabilityKindId::new(4);
        let actual = super::super::CapabilityKindId::new(5);
        let import = host_import(
            HostParameterRole::Capability(expected),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let module = host_module(
            ValueType::Capability(actual),
            ValueType::Capability(expected),
            HostCallArgument::Capability(VALUE),
            empty_layouts(),
            vec![import.clone()],
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn unknown_import_contract_is_rejected() {
        let module = lowered(
            [(ValueId::new(1), ValueType::Int)].into_iter().collect(),
            vec![Operation::HostCall {
                destinations: vec![ValueId::new(1)],
                import: ImportId::new(99),
                arguments: vec![],
            }],
            Terminator::Return {
                values: vec![ValueId::new(1)],
            },
            vec![ValueType::Int],
            Vec::new(),
        );
        let result = check_initial(module);
        assert!(
            matches!(result.as_ref(), Err(InitialCheckError::MissingImportContract { .. })),
            "unexpected result: {result:?}"
        );
    }

    #[test]
    fn candidate_abi_bytes_do_not_authenticate_an_import() {
        let kind = super::super::CapabilityKindId::new(4);
        let expected = host_import(
            HostParameterRole::Capability(kind),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![expected]);
        let mut forged = host_import(
            HostParameterRole::Capability(kind),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        forged.abi_identity = ImportContractIdentity::AuthenticatedAbi([8; 32]);
        let module = host_module(
            ValueType::Capability(kind),
            ValueType::Capability(kind),
            HostCallArgument::Capability(VALUE),
            empty_layouts(),
            vec![forged],
        );
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::InvalidCanonicalAbiContract { .. })
        ));
    }

    #[test]
    fn host_buffer_extent_must_satisfy_contract() {
        let (layouts, layout) = dynamic_u32_layout();
        let place = Place::projected(
            VALUE,
            Projection::Field { index: 0 },
            ValueType::Ref(layout),
            AccessMode::Read,
        );
        let role = HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout),
            access: AccessMode::Read,
            extent: HostBufferExtent::Exact(8),
        };
        let import = host_import(
            role.clone(),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let module = host_module(
            ValueType::Ref(layout),
            ValueType::Ref(layout),
            HostCallArgument::Buffer {
                place,
                extent: HostBufferExtent::Exact(4),
            },
            layouts,
            vec![import.clone()],
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::HostBufferExtentMismatch { .. })
        ));
    }

    #[test]
    fn authenticated_dynamic_host_slice_is_accepted() {
        let (module, catalog) = allocated_host_buffer_module(
            true,
            0,
            4,
            4,
            HostBufferExtent::Slice,
            HostBufferExtent::Slice,
        );
        let result = super::check_initial_with_catalog(module, &catalog);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn authenticated_host_read_refuses_uninitialized_guest_bytes() {
        let (mut module, catalog) = allocated_host_buffer_module(
            true, 0, 4, 4, HostBufferExtent::Slice, HostBufferExtent::Slice,
        );
        module.functions[0].blocks[0].operations
            .retain(|operation| !matches!(operation, Operation::Store { .. }));
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::UnsupportedMemoryOperation {
                operation: "HostCall(read uninitialized scalar range)", ..
            })
        ));
    }

    #[test]
    fn host_slice_outside_logical_length_is_rejected() {
        let (module, catalog) = allocated_host_buffer_module(
            true,
            1,
            4,
            4,
            HostBufferExtent::Slice,
            HostBufferExtent::Slice,
        );
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::MissingBoundsGuard { .. })
        ));
    }

    #[test]
    fn pointer_or_typed_element_buffers_cannot_be_exposed_as_host_bytes() {
        let (module, catalog) = allocated_host_buffer_module(
            false,
            0,
            4,
            4,
            HostBufferExtent::Slice,
            HostBufferExtent::Slice,
        );
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::UnsupportedHostBuffer { .. })
        ));
    }

    #[test]
    fn raw_byte_slice_copy_is_accepted() {
        let result = check_initial(byte_copy_module(true));
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn typed_element_slice_cannot_use_copy_bytes() {
        assert!(matches!(
            check_initial(byte_copy_module(false)),
            Err(InitialCheckError::UnsupportedMemoryOperation {
                operation: "CopyBytes(non-byte layout)",
                ..
            })
        ));
    }

    #[test]
    fn forged_field_and_element_buffer_projections_are_rejected() {
        let (layouts, layout) = dynamic_u32_layout();
        let role = HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout),
            access: AccessMode::Read,
            extent: HostBufferExtent::Exact(8),
        };
        for projection in [
            Projection::Field { index: 99 },
            Projection::Element { index: VALUE },
        ] {
            let import = host_import(
                role.clone(),
                HostResultRole::Value(ValueType::Int),
                HostMutation::ReadOnly,
                HostReentrancy::Forbidden,
            );
            let catalog = CanonicalAbiCatalog::from_imports(vec![import.clone()]);
            let module = host_module(
                ValueType::Ref(layout),
                ValueType::Ref(layout),
                HostCallArgument::Buffer {
                    place: Place::projected(
                        VALUE,
                        projection,
                        ValueType::Ref(layout),
                        AccessMode::Read,
                    ),
                    extent: HostBufferExtent::Exact(8),
                },
                layouts.clone(),
                vec![import],
            );
            assert!(matches!(
                super::check_initial_with_catalog(module, &catalog),
                Err(InitialCheckError::UnsupportedHostBuffer { .. })
            ));
        }
    }

    #[test]
    fn unsupported_host_mutation_and_reentrancy_fail_closed() {
        let kind = super::super::CapabilityKindId::new(4);
        for (mutation, reentrancy, expected) in [
            (HostMutation::Unknown, HostReentrancy::Forbidden, true),
            (HostMutation::ReadOnly, HostReentrancy::Allowed, true),
        ] {
            let import = host_import(
                HostParameterRole::Capability(kind),
                HostResultRole::Value(ValueType::Int),
                mutation,
                reentrancy,
            );
            let import_for_catalog = import.clone();
            let result = super::check_initial_with_catalog(host_module(
                ValueType::Capability(kind),
                ValueType::Capability(kind),
                HostCallArgument::Capability(VALUE),
                empty_layouts(),
                vec![import],
            ), &CanonicalAbiCatalog::from_imports(vec![import_for_catalog]));
            assert_eq!(result.is_err(), expected);
        }
    }

    #[test]
    fn host_mutation_must_match_explicit_buffer_access() {
        let (layouts, layout) = dynamic_u32_layout();
        for (access, mutation) in [
            (AccessMode::Write, HostMutation::ReadOnly),
            (AccessMode::Read, HostMutation::WriteBuffers),
        ] {
            let role = HostParameterRole::Buffer {
                value_type: ValueType::Ref(layout),
                access,
                extent: HostBufferExtent::Exact(8),
            };
            let import = host_import(
                role,
                HostResultRole::Value(ValueType::Int),
                mutation,
                HostReentrancy::Forbidden,
            );
            let catalog = CanonicalAbiCatalog::from_imports(vec![import.clone()]);
            let result = super::check_initial_with_catalog(
                host_module(
                    ValueType::Ref(layout),
                    ValueType::Ref(layout),
                    HostCallArgument::Buffer {
                        place: Place::projected(
                            VALUE,
                            Projection::Field { index: 0 },
                            ValueType::Ref(layout),
                            access,
                        ),
                        extent: HostBufferExtent::Exact(8),
                    },
                    layouts.clone(),
                    vec![import],
                ),
                &catalog,
            );
            assert!(matches!(
                result,
                Err(InitialCheckError::UnsupportedHostMutation { .. })
            ));
        }
    }

    #[test]
    fn scalar_module_mints_initial_valid() {
        let module = lowered(
            [(VALUE, ValueType::Int)].into_iter().collect(),
            vec![Operation::ConstInt {
                destination: VALUE,
                value: 42,
            }],
            Terminator::Return {
                values: vec![VALUE],
            },
            vec![ValueType::Int],
            Vec::new(),
        );

        assert!(check_initial(module).is_ok());
    }

    fn static_candidate(
        layouts: LayoutBundle,
        value_types: BTreeMap<ValueId, ValueType>,
        operations: Vec<Operation>,
        segments: Vec<StaticDataSegment>,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        CanonicalModule::from_lowering_with_static_data(
            LoweredModuleParts {
                revision: Revision::new(0),
                checked_source: CheckedSourceIdentity::from_digest([41; 32]),
                layouts,
                signatures: vec![FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: Vec::new(),
                    contract: contract(Vec::new()),
                }],
                imports: Vec::new(),
                functions: vec![Function {
                    id: FUNCTION,
                    signature: SIGNATURE,
                    entry: ENTRY,
                    value_types,
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations,
                        terminator: Terminator::Return { values: Vec::new() },
                    }],
                }],
                proper_tail_sites: Vec::new(),
            },
            segments,
        )
    }

    #[test]
    fn static_data_id_cannot_change_normalized_extent() {
        let (layouts, layout) = dynamic_byte_layout();
        let count_a = ValueId::new(40);
        let capacity_a = ValueId::new(41);
        let count_b = ValueId::new(42);
        let capacity_b = ValueId::new(43);
        let first = ValueId::new(44);
        let second = ValueId::new(45);
        let data = StaticDataId::new(0);
        let candidate = static_candidate(
            layouts,
            [
                (count_a, ValueType::Size), (capacity_a, ValueType::Size),
                (count_b, ValueType::Size), (capacity_b, ValueType::Size),
                (first, ValueType::Ref(layout)), (second, ValueType::Ref(layout)),
            ].into_iter().collect(),
            vec![
                Operation::ConstSize { destination: count_a, value: 1 },
                Operation::ConstSize { destination: capacity_a, value: 1 },
                Operation::ConstSize { destination: count_b, value: 0 },
                Operation::ConstSize { destination: capacity_b, value: 0 },
                Operation::StaticData { destination: first, layout, extent: Extent::Counted { header_bytes: 4, count: count_a, capacity: capacity_a, stride_bytes: 1 }, data },
                Operation::StaticData { destination: second, layout, extent: Extent::Counted { header_bytes: 4, count: count_b, capacity: capacity_b, stride_bytes: 1 }, data },
            ],
            vec![StaticDataSegment::new(data, vec![1, 0, 0, 0, 7])],
        );
        assert!(matches!(check_initial(candidate), Err(InitialCheckError::InconsistentStaticDataUse(id)) if id == data));
    }

    #[test]
    fn nested_static_bool_requires_canonical_zero_or_one() {
        let mut interner = LayoutInterner::new();
        let boolean = interner.intern_scalar(ScalarKind::Bool).unwrap();
        let inner = interner.intern_packed_record(vec![boolean]).unwrap();
        let outer = interner.intern_packed_record(vec![inner]).unwrap();
        let descriptor = interner.get(outer).unwrap();
        let crate::layout::LayoutSize::Fixed(size) = descriptor.size() else { panic!("fixed record") };
        let inner_offset = descriptor.fields()[0].offset() as usize;
        let bool_offset = interner.get(inner).unwrap().fields()[0].offset() as usize;
        let mut bytes = vec![0; size as usize];
        bytes[inner_offset + bool_offset] = 2;
        let object = ValueId::new(40);
        let data = StaticDataId::new(0);
        let candidate = static_candidate(
            LayoutBundle::from_interner(&interner, [outer]).unwrap(),
            [(object, ValueType::Ref(outer))].into_iter().collect(),
            vec![Operation::StaticData { destination: object, layout: outer, extent: Extent::FixedBytes(u64::from(size)), data }],
            vec![StaticDataSegment::new(data, bytes)],
        );
        assert!(matches!(check_initial(candidate), Err(InitialCheckError::InvalidStaticDataEncoding { object: invalid, .. }) if invalid == object));
    }

    #[test]
    fn static_runtime_buffer_header_must_match_declared_count() {
        let (layouts, layout) = dynamic_byte_layout();
        let count = ValueId::new(40);
        let capacity = ValueId::new(41);
        let object = ValueId::new(42);
        let data = StaticDataId::new(0);
        let candidate = static_candidate(
            layouts,
            [(count, ValueType::Size), (capacity, ValueType::Size), (object, ValueType::Ref(layout))].into_iter().collect(),
            vec![
                Operation::ConstSize { destination: count, value: 1 },
                Operation::ConstSize { destination: capacity, value: 1 },
                Operation::StaticData { destination: object, layout, extent: Extent::Counted { header_bytes: 4, count, capacity, stride_bytes: 1 }, data },
            ],
            vec![StaticDataSegment::new(data, vec![0, 0, 0, 0, 7])],
        );
        assert!(matches!(check_initial(candidate), Err(InitialCheckError::StaticDataExtentMismatch { object: invalid, .. }) if invalid == object));
    }

    fn scalar_binary_candidate(
        value_type: ValueType,
        operation: Operation,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        lowered(
            [(ValueId::new(40), value_type), (ValueId::new(41), value_type), (ValueId::new(42), value_type)]
                .into_iter().collect(),
            operation_constants(value_type).into_iter().chain([operation]).collect(),
            Terminator::Return { values: vec![ValueId::new(42)] },
            vec![value_type],
            Vec::new(),
        )
    }

    fn operation_constants(value_type: ValueType) -> Vec<Operation> {
        match value_type {
            ValueType::Int => vec![Operation::ConstInt { destination: ValueId::new(40), value: 4 }, Operation::ConstInt { destination: ValueId::new(41), value: 2 }],
            ValueType::Size => vec![Operation::ConstSize { destination: ValueId::new(40), value: 4 }, Operation::ConstSize { destination: ValueId::new(41), value: 2 }],
            ValueType::Float => vec![Operation::ConstFloat { destination: ValueId::new(40), bits: 4.0_f64.to_bits() }, Operation::ConstFloat { destination: ValueId::new(41), bits: 2.0_f64.to_bits() }],
            ValueType::Bool => vec![Operation::ConstBool { destination: ValueId::new(40), value: true }, Operation::ConstBool { destination: ValueId::new(41), value: false }],
            _ => vec![Operation::ConstUnit { destination: ValueId::new(40) }, Operation::ConstUnit { destination: ValueId::new(41) }],
        }
    }

    #[test]
    fn scalar_checker_refuses_non_total_backend_combinations() {
        let arithmetic = |value_type, operator, overflow| scalar_binary_candidate(
            value_type,
            Operation::Arithmetic { destination: ValueId::new(42), operator, left: ValueId::new(40), right: ValueId::new(41), overflow },
        );
        for candidate in [
            arithmetic(ValueType::Int, ArithmeticOperator::Add, OverflowBehavior::Trap),
            arithmetic(ValueType::Size, ArithmeticOperator::Subtract, OverflowBehavior::Trap),
            arithmetic(ValueType::Float, ArithmeticOperator::RemainderSigned, OverflowBehavior::Wrap),
            arithmetic(ValueType::Float, ArithmeticOperator::Add, OverflowBehavior::Trap),
            scalar_binary_candidate(ValueType::Bool, Operation::Compare { destination: ValueId::new(42), operator: ComparisonOperator::LessSigned, left: ValueId::new(40), right: ValueId::new(41) }),
            lowered(
                [(ValueId::new(40), ValueType::Unit), (ValueId::new(41), ValueType::Unit), (ValueId::new(42), ValueType::Bool)].into_iter().collect(),
                operation_constants(ValueType::Unit).into_iter().chain([Operation::Compare { destination: ValueId::new(42), operator: ComparisonOperator::Equal, left: ValueId::new(40), right: ValueId::new(41) }]).collect(),
                Terminator::Return { values: vec![ValueId::new(42)] },
                vec![ValueType::Bool],
                Vec::new(),
            ),
        ] {
            assert!(matches!(check_initial(candidate), Err(InitialCheckError::UnsupportedScalarOperation { .. })));
        }
    }

    fn int_arithmetic_candidate(
        left_value: i64,
        right_value: i64,
        operator: ArithmeticOperator,
        overflow: OverflowBehavior,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        lowered(
            [
                (ValueId::new(40), ValueType::Int),
                (ValueId::new(41), ValueType::Int),
                (ValueId::new(42), ValueType::Int),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstInt { destination: ValueId::new(40), value: left_value },
                Operation::ConstInt { destination: ValueId::new(41), value: right_value },
                Operation::Arithmetic {
                    destination: ValueId::new(42),
                    operator,
                    left: ValueId::new(40),
                    right: ValueId::new(41),
                    overflow,
                },
            ],
            Terminator::Return { values: vec![ValueId::new(42)] },
            vec![ValueType::Int],
            Vec::new(),
        )
    }

    #[test]
    fn wrapping_division_and_remainder_are_refused_before_runtime_traps() {
        for (operator, left, right) in [
            (ArithmeticOperator::DivideSigned, i64::MIN, -1),
            (ArithmeticOperator::RemainderSigned, i64::MIN, -1),
            (ArithmeticOperator::DivideSigned, 1, 0),
            (ArithmeticOperator::DivideUnsigned, 1, 0),
            (ArithmeticOperator::RemainderSigned, 1, 0),
            (ArithmeticOperator::RemainderUnsigned, 1, 0),
        ] {
            assert!(matches!(
                check_initial(int_arithmetic_candidate(left, right, operator, OverflowBehavior::Wrap)),
                Err(InitialCheckError::UnsupportedScalarOperation { .. })
            ));
        }
    }

    #[test]
    fn size_constant_must_fit_wasm32() {
        let module = lowered(
            [(VALUE, ValueType::Size)].into_iter().collect(),
            vec![Operation::ConstSize { destination: VALUE, value: u64::from(u32::MAX) + 1 }],
            Terminator::Return { values: vec![VALUE] },
            vec![ValueType::Size],
            Vec::new(),
        );
        assert!(matches!(check_initial(module), Err(InitialCheckError::UnsupportedScalarOperation { operation: "ConstSize outside Wasm32", .. })));
    }

    #[test]
    fn scalar_checker_accepts_exact_backend_matrix_edges() {
        let arithmetic = |value_type, operator, overflow| scalar_binary_candidate(
            value_type,
            Operation::Arithmetic { destination: ValueId::new(42), operator, left: ValueId::new(40), right: ValueId::new(41), overflow },
        );
        for candidate in [
            arithmetic(ValueType::Int, ArithmeticOperator::BitXor, OverflowBehavior::Wrap),
            arithmetic(ValueType::Int, ArithmeticOperator::DivideSigned, OverflowBehavior::Trap),
            arithmetic(ValueType::Size, ArithmeticOperator::Add, OverflowBehavior::Trap),
            arithmetic(ValueType::Float, ArithmeticOperator::DivideUnsigned, OverflowBehavior::Wrap),
            scalar_binary_candidate(ValueType::Bool, Operation::Compare { destination: ValueId::new(42), operator: ComparisonOperator::NotEqual, left: ValueId::new(40), right: ValueId::new(41) }),
        ] {
            assert!(check_initial(candidate).is_ok());
        }
    }

    #[test]
    fn nullable_capability_operations_are_checked() {
        let absent = ValueId::new(40);
        let present = ValueId::new(41);
        let module = lowered(
            [(absent, ValueType::NullableCapability(super::super::CapabilityKindId::new(1))), (present, ValueType::Bool)]
                .into_iter()
                .collect(),
            vec![
                Operation::CapabilityNone { destination: absent, kind: super::super::CapabilityKindId::new(1) },
                Operation::CapabilityIsPresent { destination: present, source: absent },
            ],
            Terminator::Return { values: vec![present] },
            vec![ValueType::Bool],
            Vec::new(),
        );
        assert!(check_initial(module).is_ok());

        let scalar = ValueId::new(42);
        let invalid = lowered(
            [(scalar, ValueType::Int), (absent, ValueType::NullableCapability(super::super::CapabilityKindId::new(1)))]
                .into_iter()
                .collect(),
            vec![
                Operation::ConstInt { destination: scalar, value: 0 },
                Operation::CapabilitySome { destination: absent, source: scalar },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            Vec::new(),
        );
        assert!(matches!(check_initial(invalid), Err(InitialCheckError::TypeMismatch { .. })));
    }

    #[test]
    fn function_value_authenticates_checked_target_signature() {
        let destination = ValueId::new(1);
        let module = lowered(
            [(destination, ValueType::Callable(SIGNATURE))].into_iter().collect(),
            vec![Operation::FunctionValue {
                destination,
                function: FUNCTION,
            }],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            Vec::new(),
        );

        assert!(check_initial(module).is_ok());
    }

    #[test]
    fn function_value_rejects_destination_signature_mismatch() {
        let destination = ValueId::new(1);
        let module = lowered(
            [(destination, ValueType::Int)].into_iter().collect(),
            vec![Operation::FunctionValue {
                destination,
                function: FUNCTION,
            }],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::TypeMismatch {
                function: FUNCTION,
                value: destination,
                expected: ValueType::Callable(SIGNATURE),
                actual: ValueType::Int,
            }
        );
    }

    #[test]
    fn function_value_rejects_unknown_function() {
        let destination = ValueId::new(1);
        let unknown = FunctionId::new(99);
        let module = lowered(
            [(destination, ValueType::Callable(SIGNATURE))].into_iter().collect(),
            vec![Operation::FunctionValue {
                destination,
                function: unknown,
            }],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::MissingFunction(unknown)
        );
    }

    #[test]
    fn proper_tail_site_is_matched_exactly_once() {
        let module = lowered(
            BTreeMap::new(),
            Vec::new(),
            Terminator::TailCall {
                site: TAIL_SITE,
                callee: FUNCTION,
                arguments: Vec::new(),
            },
            Vec::new(),
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION, discharged_by: None,
            }],
        ).with_proper_tail_locations(vec![ProperTailLocation::new(TAIL_SITE, FUNCTION, ENTRY)]);

        assert!(check_initial(module).is_ok());
    }

    #[test]
    fn duplicate_tail_terminators_cannot_discharge_one_obligation() {
        let mut module = lowered(
            BTreeMap::new(),
            Vec::new(),
            Terminator::TailCall {
                site: TAIL_SITE,
                callee: FUNCTION,
                arguments: Vec::new(),
            },
            Vec::new(),
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION, discharged_by: None,
            }],
        ).with_proper_tail_locations(vec![ProperTailLocation::new(
            TAIL_SITE,
            FUNCTION,
            BlockId::new(1),
        )]);
        let condition = ValueId::new(99);
        module.functions[0].value_types.insert(condition, ValueType::Bool);
        module.functions[0].blocks[0].operations = vec![Operation::ConstBool {
            destination: condition,
            value: true,
        }];
        module.functions[0].blocks[0].terminator = Terminator::BranchIf {
            condition,
            then_target: BlockId::new(1),
            then_arguments: Vec::new(),
            else_target: BlockId::new(2),
            else_arguments: Vec::new(),
        };
        for id in [BlockId::new(1), BlockId::new(2)] {
            module.functions[0].blocks.push(BasicBlock {
                id,
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: Terminator::TailCall {
                    site: TAIL_SITE,
                    callee: FUNCTION,
                    arguments: Vec::new(),
                },
            });
        }

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::DuplicateTailObligation(TAIL_SITE)
        );
    }

    #[test]
    fn unreachable_tail_decoy_cannot_discharge_an_obligation() {
        let mut module = lowered(
            BTreeMap::new(),
            Vec::new(),
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION, discharged_by: None,
            }],
        ).with_proper_tail_locations(vec![ProperTailLocation::new(
            TAIL_SITE,
            FUNCTION,
            BlockId::new(1),
        )]);
        module.functions[0].blocks.push(BasicBlock {
            id: BlockId::new(1),
            parameters: Vec::new(),
            operations: Vec::new(),
            terminator: Terminator::TailCall {
                site: TAIL_SITE,
                callee: FUNCTION,
                arguments: Vec::new(),
            },
        });
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::MissingTailObligation(TAIL_SITE)
        );
    }

    #[test]
    fn ordinary_return_cannot_discharge_a_tail_obligation() {
        let module = lowered(
            BTreeMap::new(),
            Vec::new(),
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION, discharged_by: None,
            }],
        ).with_proper_tail_locations(vec![ProperTailLocation::new(TAIL_SITE, FUNCTION, ENTRY)]);

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::MissingTailObligation(TAIL_SITE)
        );
    }

    #[test]
    fn unreachable_tail_at_the_bound_location_cannot_discharge_an_obligation() {
        let mut module = lowered(
            BTreeMap::new(),
            Vec::new(),
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION,
                discharged_by: None,
            }],
        )
        .with_proper_tail_locations(vec![ProperTailLocation::new(
            TAIL_SITE,
            FUNCTION,
            BlockId::new(1),
        )]);
        module.functions[0].blocks.push(BasicBlock {
            id: BlockId::new(1),
            parameters: Vec::new(),
            operations: Vec::new(),
            terminator: Terminator::TailCall {
                site: TAIL_SITE,
                callee: FUNCTION,
                arguments: Vec::new(),
            },
        });

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::MissingTailObligation(TAIL_SITE)
        );
    }

    #[test]
    fn tail_site_moved_to_another_reachable_branch_is_refused() {
        let mut module = lowered(
            [(VALUE, ValueType::Bool)].into_iter().collect(),
            vec![Operation::ConstBool {
                destination: VALUE,
                value: true,
            }],
            Terminator::BranchIf {
                condition: VALUE,
                then_target: BlockId::new(1),
                then_arguments: Vec::new(),
                else_target: BlockId::new(2),
                else_arguments: Vec::new(),
            },
            Vec::new(),
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION,
                discharged_by: None,
            }],
        )
        .with_proper_tail_locations(vec![ProperTailLocation::new(
            TAIL_SITE,
            FUNCTION,
            BlockId::new(1),
        )]);
        module.functions[0].blocks.extend([
            BasicBlock {
                id: BlockId::new(1),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: Terminator::Return { values: Vec::new() },
            },
            BasicBlock {
                id: BlockId::new(2),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: Terminator::TailCall {
                    site: TAIL_SITE,
                    callee: FUNCTION,
                    arguments: Vec::new(),
                },
            },
        ]);

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::TailSiteBlockMismatch {
                site: TAIL_SITE,
                expected: BlockId::new(1),
                actual: BlockId::new(2),
            }
        );
    }

    #[test]
    fn reclaim_without_unique_object_authority_fails_closed() {
        let module = lowered(
            [(VALUE, ValueType::Int)].into_iter().collect(),
            vec![
                Operation::ConstInt {
                    destination: VALUE,
                    value: 0,
                },
                Operation::Reclaim { object: VALUE },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidOwnership {
                function: FUNCTION,
                object: VALUE,
            }
        );
    }

    #[test]
    fn block_arguments_are_typed_ssa_definitions() {
        let next = BlockId::new(1);
        let parameter = ValueId::new(1);
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: vec![ValueType::Int],
                contract: contract(vec![ResultContract::Value(ValueType::Int)]),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [(VALUE, ValueType::Int), (parameter, ValueType::Int)]
                    .into_iter()
                    .collect(),
                blocks: vec![
                    BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt {
                            destination: VALUE,
                            value: 42,
                        }],
                        terminator: Terminator::Branch {
                            target: next,
                            arguments: vec![VALUE],
                        },
                    },
                    BasicBlock {
                        id: next,
                        parameters: vec![super::super::BlockParameter {
                            value: parameter,
                            value_type: ValueType::Int,
                            ownership: None,
                        }],
                        operations: Vec::new(),
                        terminator: Terminator::Return {
                            values: vec![parameter],
                        },
                    },
                ],
            }],
            proper_tail_sites: Vec::new(),
        });

        assert!(check_initial(module).is_ok());
    }

    #[test]
    fn definition_on_one_branch_does_not_dominate_join() {
        let condition = ValueId::new(1);
        let then_block = BlockId::new(1);
        let else_block = BlockId::new(2);
        let join_block = BlockId::new(3);
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: vec![ValueType::Int],
                contract: contract(vec![ResultContract::Value(ValueType::Int)]),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [(VALUE, ValueType::Int), (condition, ValueType::Bool)]
                    .into_iter()
                    .collect(),
                blocks: vec![
                    BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstBool {
                            destination: condition,
                            value: true,
                        }],
                        terminator: Terminator::BranchIf {
                            condition,
                            then_target: then_block,
                            then_arguments: Vec::new(),
                            else_target: else_block,
                            else_arguments: Vec::new(),
                        },
                    },
                    BasicBlock {
                        id: then_block,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt {
                            destination: VALUE,
                            value: 1,
                        }],
                        terminator: Terminator::Branch {
                            target: join_block,
                            arguments: Vec::new(),
                        },
                    },
                    BasicBlock {
                        id: else_block,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Branch {
                            target: join_block,
                            arguments: Vec::new(),
                        },
                    },
                    BasicBlock {
                        id: join_block,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Return {
                            values: vec![VALUE],
                        },
                    },
                ],
            }],
            proper_tail_sites: Vec::new(),
        });

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::ValueDoesNotDominateUse {
                function: FUNCTION,
                value: VALUE,
                block: join_block,
            }
        );
    }

    fn dynamic_load_module(
        with_guard: bool,
        capacity_value: u64,
        initialize: bool,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let index = ValueId::new(2);
        let condition = ValueId::new(3);
        let object = ValueId::new(4);
        let loaded = ValueId::new(5);
        let stored = ValueId::new(6);
        let (layouts, layout) = dynamic_u32_layout();
        let mut operations = vec![
            Operation::ConstSize {
                destination: count,
                value: 1,
            },
            Operation::ConstSize {
                destination: capacity,
                value: capacity_value,
            },
            Operation::ConstSize {
                destination: index,
                value: 0,
            },
            Operation::Compare {
                destination: condition,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: count,
            },
        ];
        if with_guard {
            operations.push(Operation::Guard {
                condition,
                kind: GuardKind::Bounds,
            });
        }
        operations.push(Operation::ConstSize { destination: stored, value: 7 });
        operations.push(Operation::Alloc {
                destination: object,
                layout,
                extent: Extent::Counted {
                    header_bytes: 4,
                    count,
                    capacity,
                    stride_bytes: 4,
                },
                class: AllocationClass::ReferenceCounted,
        });
        if initialize {
            operations.push(
                Operation::Store {
                    place: Place::projected(
                        object,
                        Projection::Element { index },
                        ValueType::Size,
                        AccessMode::Write,
                    ),
                    value: stored,
                    transfer: StoreTransfer::CopyScalar,
                },
            );
        }
        operations.extend([
            Operation::Load {
                destination: loaded,
                place: Place::projected(
                    object,
                    Projection::Element { index },
                    ValueType::Size,
                    AccessMode::Read,
                ),
                transfer: LoadTransfer::CopyScalar,
            },
            Operation::Reclaim { object },
        ]);

        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: contract(Vec::new()),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (index, ValueType::Size),
                    (condition, ValueType::Bool),
                    (object, ValueType::Ref(layout)),
                    (loaded, ValueType::Size),
                    (stored, ValueType::Size),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations,
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    #[test]
    fn guarded_dynamic_element_load_is_admitted() {
        assert!(check_initial(dynamic_load_module(true, 1, true)).is_ok());
    }

    #[test]
    fn uninitialized_scalar_load_is_refused() {
        assert!(matches!(
            check_initial(dynamic_load_module(true, 1, false)),
            Err(InitialCheckError::UnsupportedMemoryOperation {
                operation: "Load(uninitialized scalar)", ..
            })
        ));
    }

    #[test]
    fn dynamic_element_load_without_guard_is_refused() {
        assert_eq!(
            check_initial(dynamic_load_module(false, 1, false)).unwrap_err(),
            InitialCheckError::MissingBoundsGuard {
                function: FUNCTION,
                index: ValueId::new(2),
                count: ValueId::new(0),
            }
        );
    }

    #[test]
    fn dynamic_allocation_extent_must_fit_wasm_address_space() {
        assert_eq!(
            check_initial(dynamic_load_module(true, u64::MAX, true)).unwrap_err(),
            InitialCheckError::UnsupportedScalarOperation {
                function: FUNCTION,
                operation: "ConstSize outside Wasm32",
            }
        );
    }

    #[test]
    fn declared_callable_result_must_match_checked_signature() {
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: vec![ValueType::Int],
                contract: contract(vec![ResultContract::Value(ValueType::Float)]),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [(VALUE, ValueType::Int)].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations: vec![Operation::ConstInt {
                        destination: VALUE,
                        value: 42,
                    }],
                    terminator: Terminator::Return {
                        values: vec![VALUE],
                    },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::CallableContractMismatch {
                signature: SIGNATURE,
            }
        );
    }

    #[test]
    fn matching_callable_result_contract_is_accepted() {
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: vec![ValueType::Int],
                contract: contract(vec![ResultContract::Value(ValueType::Int)]),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [(VALUE, ValueType::Int)].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations: vec![Operation::ConstInt {
                        destination: VALUE,
                        value: 42,
                    }],
                    terminator: Terminator::Return {
                        values: vec![VALUE],
                    },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });

        assert!(check_initial(module).is_ok());
    }

    #[test]
    fn valid_unreachable_block_is_available_to_pruning() {
        let dead = BlockId::new(1);
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: contract(Vec::new()),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: BTreeMap::new(),
                blocks: vec![
                    BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Return { values: Vec::new() },
                    },
                    BasicBlock {
                        id: dead,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Return { values: Vec::new() },
                    },
                ],
            }],
            proper_tail_sites: Vec::new(),
        });

        assert!(check_initial(module).is_ok());
    }

    fn counted_access_module(
        count_value: u64,
        capacity_value: u64,
        index_value: u64,
        guard_right: ValueId,
        with_guard: bool,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let index = ValueId::new(2);
        let condition = ValueId::new(3);
        let object = ValueId::new(4);
        let loaded = ValueId::new(5);
        let (layouts, layout) = dynamic_u32_layout();
        let mut operations = vec![
            Operation::ConstSize { destination: count, value: count_value },
            Operation::ConstSize { destination: capacity, value: capacity_value },
            Operation::ConstSize { destination: index, value: index_value },
            Operation::Compare {
                destination: condition,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: guard_right,
            },
        ];
        if with_guard {
            operations.push(Operation::Guard { condition, kind: GuardKind::Bounds });
        }
        operations.extend([
            Operation::Alloc {
                destination: object,
                layout,
                extent: Extent::Counted {
                    header_bytes: 4,
                    count,
                    capacity,
                    stride_bytes: 4,
                },
                class: AllocationClass::ReferenceCounted,
            },
            Operation::Load {
                destination: loaded,
                place: Place::projected(
                    object,
                    Projection::Element { index },
                    ValueType::Size,
                    AccessMode::Read,
                ),
                transfer: LoadTransfer::CopyScalar,
            },
            Operation::Reclaim { object },
        ]);
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: contract(Vec::new()),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (index, ValueType::Size),
                    (condition, ValueType::Bool),
                    (object, ValueType::Ref(layout)),
                    (loaded, ValueType::Size),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations,
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    #[test]
    fn logical_length_guard_is_not_satisfied_by_capacity_guard() {
        let capacity = ValueId::new(1);
        assert_eq!(
            check_initial(counted_access_module(1, 2, 1, capacity, true)).unwrap_err(),
            InitialCheckError::MissingBoundsGuard {
                function: FUNCTION,
                index: ValueId::new(2),
                count: ValueId::new(0),
            }
        );
    }

    #[test]
    fn update_count_grows_logical_length_only_after_capacity_guard() {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let new_count = ValueId::new(2);
        let index = ValueId::new(3);
        let capacity_condition = ValueId::new(4);
        let index_condition = ValueId::new(5);
        let object = ValueId::new(6);
        let loaded = ValueId::new(7);
        let zero = ValueId::new(8);
        let stored = ValueId::new(9);
        let zero_in_capacity = ValueId::new(10);
        let index_in_capacity = ValueId::new(11);
        let (layouts, layout) = dynamic_u32_layout();
        let operations = vec![
            Operation::ConstSize { destination: count, value: 1 },
            Operation::ConstSize { destination: capacity, value: 2 },
            Operation::ConstSize { destination: new_count, value: 2 },
            Operation::ConstSize { destination: index, value: 1 },
            Operation::Compare {
                destination: capacity_condition,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: new_count,
                right: capacity,
            },
            Operation::Guard { condition: capacity_condition, kind: GuardKind::Bounds },
            Operation::Alloc {
                destination: object,
                layout,
                extent: Extent::Counted {
                    header_bytes: 4,
                    count,
                    capacity,
                    stride_bytes: 4,
                },
                class: AllocationClass::ReferenceCounted,
            },
            Operation::ConstSize { destination: zero, value: 0 },
            Operation::ConstSize { destination: stored, value: 7 },
            Operation::Compare { destination: zero_in_capacity, operator: ComparisonOperator::LessUnsigned, left: zero, right: capacity },
            Operation::Guard { condition: zero_in_capacity, kind: GuardKind::Bounds },
            Operation::Compare { destination: index_in_capacity, operator: ComparisonOperator::LessUnsigned, left: index, right: capacity },
            Operation::Guard { condition: index_in_capacity, kind: GuardKind::Bounds },
            Operation::Store {
                place: Place::projected(object, Projection::Element { index: zero }, ValueType::Size, AccessMode::Write),
                value: stored,
                transfer: StoreTransfer::CopyScalar,
            },
            Operation::Store {
                place: Place::projected(object, Projection::Element { index }, ValueType::Size, AccessMode::Write),
                value: stored,
                transfer: StoreTransfer::CopyScalar,
            },
            Operation::UpdateCount { object, count: new_count },
            Operation::Compare {
                destination: index_condition,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: new_count,
            },
            Operation::Guard { condition: index_condition, kind: GuardKind::Bounds },
            Operation::Load {
                destination: loaded,
                place: Place::projected(
                    object,
                    Projection::Element { index },
                    ValueType::Size,
                    AccessMode::Read,
                ),
                transfer: LoadTransfer::CopyScalar,
            },
            Operation::DropRef { object },
        ];
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: contract(Vec::new()),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (new_count, ValueType::Size),
                    (index, ValueType::Size),
                    (capacity_condition, ValueType::Bool),
                    (index_condition, ValueType::Bool),
                    (object, ValueType::Ref(layout)),
                    (loaded, ValueType::Size),
                    (zero, ValueType::Size),
                    (stored, ValueType::Size),
                    (zero_in_capacity, ValueType::Bool),
                    (index_in_capacity, ValueType::Bool),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations,
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        let result = check_initial(module);
        assert!(result.is_ok(), "UpdateCount fixture rejected: {result:?}");
    }

    fn header_load_module(length_type: ValueType) -> CanonicalModule<super::super::LoweredCandidate> {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        let loaded_length = ValueId::new(3);
        let loaded_capacity = ValueId::new(4);
        let (layouts, layout) = packed_u32_layout();
        let header = |field, destination, value_type| Operation::Load {
            destination,
            place: Place::projected(
                object,
                Projection::Header(field),
                value_type,
                AccessMode::Read,
            ),
            transfer: LoadTransfer::CopyScalar,
        };
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: contract(Vec::new()),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (object, ValueType::Ref(layout)),
                    (loaded_length, length_type),
                    (loaded_capacity, ValueType::Size),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 1 },
                        Operation::ConstSize { destination: capacity, value: 2 },
                        Operation::Alloc {
                            destination: object,
                            layout,
                            extent: Extent::Counted {
                                header_bytes: 8,
                                count,
                                capacity,
                                stride_bytes: 4,
                            },
                            class: AllocationClass::ReferenceCounted,
                        },
                        header(HeaderField::Length, loaded_length, length_type),
                        header(HeaderField::Capacity, loaded_capacity, ValueType::Size),
                        Operation::Reclaim { object },
                    ],
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    #[test]
    fn counted_header_projections_load_typed_scalars() {
        assert!(check_initial(header_load_module(ValueType::Size)).is_ok());
    }

    #[test]
    fn header_projection_cannot_claim_non_scalar_type() {
        assert!(matches!(
            check_initial(header_load_module(ValueType::Bool)),
            Err(InitialCheckError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn bounds_guard_without_a_comparison_is_refused() {
        let condition = ValueId::new(1);
        let module = lowered(
            [(condition, ValueType::Bool)].into_iter().collect(),
            vec![
                Operation::ConstBool { destination: condition, value: true },
                Operation::Guard { condition, kind: GuardKind::Bounds },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidGuard {
                function: FUNCTION,
                condition,
                kind: GuardKind::Bounds,
            }
        );
    }

    #[test]
    fn bounds_guard_requires_unsigned_size_operands() {
        let left = ValueId::new(1);
        let right = ValueId::new(2);
        let condition = ValueId::new(3);
        let module = lowered(
            [
                (left, ValueType::Int),
                (right, ValueType::Int),
                (condition, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstInt { destination: left, value: 0 },
                Operation::ConstInt { destination: right, value: 1 },
                Operation::Compare {
                    destination: condition,
                    operator: ComparisonOperator::LessUnsigned,
                    left,
                    right,
                },
                Operation::Guard { condition, kind: GuardKind::Bounds },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidGuard {
                function: FUNCTION,
                condition,
                kind: GuardKind::Bounds,
            }
        );
    }

    #[test]
    fn checked_value_memory_result_is_refused() {
        let (layouts, layout) = dynamic_u32_layout();
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: vec![ValueType::Ref(layout)],
                contract: contract(vec![ResultContract::CheckedValue { layout }]),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (object, ValueType::Ref(layout)),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 0 },
                        Operation::ConstSize { destination: capacity, value: 0 },
                        Operation::Alloc {
                            destination: object,
                            layout,
                            extent: Extent::Counted {
                                header_bytes: 4,
                                count,
                                capacity,
                                stride_bytes: 4,
                            },
                            class: AllocationClass::ReferenceCounted,
                        },
                    ],
                    terminator: Terminator::Return { values: vec![object] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::CallableContractMismatch { signature: SIGNATURE }
        );
    }

    #[test]
    fn checked_value_memory_result_is_refused_in_auxiliary_signature() {
        let (layouts, layout) = dynamic_u32_layout();
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE, parameters: Vec::new(), results: Vec::new(),
                    contract: contract(Vec::new()),
                },
                FunctionSignature {
                    id: CALLEE_SIGNATURE, parameters: Vec::new(), results: vec![ValueType::Ref(layout)],
                    contract: contract(vec![ResultContract::CheckedValue { layout }]),
                },
            ],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION, signature: SIGNATURE, entry: ENTRY,
                value_types: BTreeMap::new(),
                blocks: vec![BasicBlock { id: ENTRY, parameters: Vec::new(), operations: Vec::new(),
                    terminator: Terminator::Return { values: Vec::new() } }],
            }],
            proper_tail_sites: Vec::new(),
        });
        assert_eq!(check_initial(module).unwrap_err(),
            InitialCheckError::CallableContractMismatch { signature: CALLEE_SIGNATURE });
    }

    #[test]
    fn shared_ownership_cannot_be_reclaimed_as_unique() {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        let shared = ValueId::new(3);
        let (layouts, layout) = dynamic_u32_layout();
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: contract(Vec::new()),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (object, ValueType::Ref(layout)),
                    (shared, ValueType::Ref(layout)),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 0 },
                        Operation::ConstSize { destination: capacity, value: 1 },
                        Operation::Alloc {
                            destination: object,
                            layout,
                            extent: Extent::Counted {
                                header_bytes: 4,
                                count,
                                capacity,
                                stride_bytes: 4,
                            },
                            class: AllocationClass::ReferenceCounted,
                        },
                        Operation::CloneRef { destination: shared, source: object },
                        Operation::Reclaim { object: shared },
                    ],
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidOwnership {
                function: FUNCTION,
                object: shared,
            }
        );
    }

    #[test]
    fn call_move_consumes_unique_and_later_use_is_rejected() {
        let (layouts, layout) = dynamic_u32_layout();
        let (values, mut operations) = allocated_list(layout);
        let object = ValueId::new(2);
        operations.extend([
            Operation::Call {
                destinations: Vec::new(),
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Move(object)],
            },
            Operation::Reclaim { object },
        ]);
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Unique,
                Vec::new(),
                BTreeMap::new(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Unique),
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::UseAfterConsume {
                function: FUNCTION,
                object,
            }
        );
    }

    #[test]
    fn shared_reference_cannot_be_moved_twice_without_clone() {
        let (layouts, layout) = dynamic_u32_layout();
        let (mut values, mut operations) = allocated_list(layout);
        let object = ValueId::new(2);
        let clone = ValueId::new(13);
        values.insert(clone, ValueType::Ref(layout));
        operations.extend([
            Operation::CloneRef { destination: clone, source: object },
            Operation::Call {
                destinations: Vec::new(),
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Move(clone)],
            },
            Operation::Call {
                destinations: Vec::new(),
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Move(clone)],
            },
        ]);
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            vec![Operation::DropRef { object: ValueId::new(10) }],
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Shared,
                Vec::new(),
                [(0, vec![MemoryEffect::Release])].into_iter().collect(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Shared),
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::UseAfterConsume {
                function: FUNCTION,
                object: clone,
            }
        );
    }

    #[test]
    fn memory_value_call_is_fail_closed() {
        let (layouts, layout) = dynamic_u32_layout();
        let (values, mut operations) = allocated_list(layout);
        let object = ValueId::new(2);
        operations.push(Operation::Call {
            destinations: Vec::new(),
            callee: CALLEE,
            arguments: vec![super::super::CallArgument::Value(object)],
        });
        let value_module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Value,
                Vec::new(),
                BTreeMap::new(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: None,
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(value_module).unwrap_err(),
            InitialCheckError::UnsupportedMemoryOperation {
                function: FUNCTION,
                operation: "Call(Value memory)",
            }
        );
    }

    #[test]
    fn tail_call_after_release_is_rejected() {
        let (layouts, layout) = dynamic_u32_layout();
        let (values, mut operations) = allocated_list(layout);
        let object = ValueId::new(2);
        operations.push(Operation::Reclaim { object });
        let moved_module = call_module(
            layouts.clone(),
            values.clone(),
            operations,
            Terminator::TailCall {
                site: TAIL_SITE,
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Move(object)],
            },
            vec![Operation::DropRef { object: ValueId::new(10) }],
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Shared,
                Vec::new(),
                [(0, vec![MemoryEffect::Release])].into_iter().collect(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Shared),
            }],
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION, discharged_by: None,
            }],
        ).with_proper_tail_locations(vec![ProperTailLocation::new(TAIL_SITE, FUNCTION, ENTRY)]);
        assert_eq!(
            check_initial(moved_module).unwrap_err(),
            InitialCheckError::UseAfterConsume {
                function: FUNCTION,
                object,
            }
        );

    }

    #[test]
    fn owned_parameter_must_be_discharged_at_exit() {
        let (layouts, layout) = dynamic_u32_layout();
        let (values, mut operations) = allocated_list(layout);
        let object = ValueId::new(2);
        operations.push(Operation::DropRef { object });
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            [(ValueId::new(10), ValueType::Ref(layout))].into_iter().collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(layout, ParameterOwnership::Shared, Vec::new(), BTreeMap::new()),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Shared),
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::UnconsumedReference {
                function: CALLEE,
                object: ValueId::new(10),
            }
        );
    }

    #[test]
    fn owned_parameter_read_then_drop_is_accepted() {
        let (layouts, layout) = packed_u32_layout();
        let object = ValueId::new(2);
        let destination = ValueId::new(12);
        let (mut values, mut operations) = allocated_list_with_header(layout, 8);
        values.insert(destination, ValueType::Size);
        operations.push(Operation::Call {
            destinations: vec![destination],
            callee: CALLEE,
            arguments: vec![super::super::CallArgument::Move(object)],
        });
        let parameter = ValueId::new(10);
        let length = ValueId::new(11);
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            vec![
                Operation::Load {
                    destination: length,
                    place: Place::projected(
                        parameter,
                        Projection::Header(HeaderField::Length),
                        ValueType::Size,
                        super::super::AccessMode::Read,
                    ),
                    transfer: LoadTransfer::CopyScalar,
                },
                Operation::DropRef { object: parameter },
            ],
            [(parameter, ValueType::Ref(layout)), (length, ValueType::Size)]
                .into_iter()
                .collect(),
            Terminator::Return { values: vec![length] },
            call_contract(
                layout,
                ParameterOwnership::Shared,
                vec![ResultContract::Value(ValueType::Size)],
                [(0, vec![MemoryEffect::Read, MemoryEffect::Release])]
                    .into_iter()
                    .collect(),
            ),
            vec![ValueType::Size],
            vec![BlockParameter {
                value: parameter,
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Shared),
            }],
            Vec::new(),
        );
        assert!(check_initial(module).is_ok());
    }

    #[test]
    fn clone_then_move_requires_caller_source_cleanup() {
        let (layouts, layout) = dynamic_u32_layout();
        let (mut values, mut operations) = allocated_list(layout);
        let source = ValueId::new(2);
        let clone = ValueId::new(3);
        values.insert(clone, ValueType::Ref(layout));
        operations.extend([
            Operation::CloneRef { destination: clone, source },
            Operation::Call {
                destinations: Vec::new(),
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Move(clone)],
            },
        ]);
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            vec![Operation::DropRef { object: ValueId::new(10) }],
            [(ValueId::new(10), ValueType::Ref(layout))].into_iter().collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Shared,
                Vec::new(),
                [(0, vec![MemoryEffect::Release])].into_iter().collect(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Shared),
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::UnconsumedReference {
                function: FUNCTION,
                object: source,
            }
        );
    }

    #[test]
    fn tail_shared_move_transfers_without_reclaim() {
        let (layouts, layout) = dynamic_u32_layout();
        let (values, operations) = allocated_list(layout);
        let object = ValueId::new(2);
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::TailCall {
                site: TAIL_SITE,
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Move(object)],
            },
            vec![Operation::DropRef { object: ValueId::new(10) }],
            [(ValueId::new(10), ValueType::Ref(layout))].into_iter().collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Shared,
                Vec::new(),
                [(0, vec![MemoryEffect::Release])].into_iter().collect(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Shared),
            }],
            vec![ProperTailSite { id: TAIL_SITE, function: FUNCTION, discharged_by: None }],
        ).with_proper_tail_locations(vec![ProperTailLocation::new(TAIL_SITE, FUNCTION, ENTRY)]);
        assert!(check_initial(module).is_ok());
    }

    #[test]
    fn shared_move_into_unique_parameter_is_rejected() {
        let (layouts, layout) = dynamic_u32_layout();
        let (mut values, mut operations) = allocated_list(layout);
        let source = ValueId::new(2);
        let shared = ValueId::new(3);
        values.insert(shared, ValueType::Ref(layout));
        operations.extend([
            Operation::CloneRef { destination: shared, source },
            Operation::Call {
                destinations: Vec::new(),
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Move(shared)],
            },
            Operation::DropRef { object: source },
        ]);
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            vec![Operation::DropRef { object: ValueId::new(10) }],
            [(ValueId::new(10), ValueType::Ref(layout))].into_iter().collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Unique,
                Vec::new(),
                [(0, vec![MemoryEffect::Release])].into_iter().collect(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Unique),
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidOwnership {
                function: FUNCTION,
                object: shared,
            }
        );
    }
}
