//! Independent checked-source binding for opaque closure constructors.
//! Candidate IDs and field maps are claims, never capability authority.
use super::authority::*;
use super::closure_owner_contract::{CaptureStorage, CaptureTransfer, OwnerConstructor, OwnerFamily, OwnerFamilySource, OwnerOperation, SharedReferenceConstructor, SourceConstructor, rooted_slice_carrier, carrier_element_layout_matches};
use super::{CanonicalModule, ModuleStage, Operation, ValueId, ValueType};
use std::collections::{BTreeMap, BTreeSet};
use witchy_canonical_source::closure::ClosureBindingPathStep;

pub(super) fn validate<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<(), CandidateAbiBindingError> {
    let records = source.type_definitions();
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes())
        .map_err(|_| CandidateAbiBindingError::MissingCanonicalLayouts)?;
    let witnesses = source.closure_witness().entries();
    let opaque_evidence = source.opaque_owner_evidence()
        .ok_or(CandidateAbiBindingError::MissingCanonicalLayouts)?;
    // Validate the entire checked witness set before comparing constructor
    // families. An unencodable signature must not disappear from a family.
    let witness_family_keys = witnesses.iter().map(|witness| {
        witness.signature().map(|signature| {
            source_closure_family_key(signature, &records)
                .ok_or(CandidateAbiBindingError::MissingCanonicalLayouts)
        }).transpose()
    }).collect::<Result<Vec<_>, _>>()?;
    let mut seen = BTreeSet::new();
    let mut family_keys = std::collections::HashSet::new();
    let mut aggregate_types = std::collections::HashSet::new();
    let mut constructor_ids = BTreeSet::new();
    for constructor in candidate.owner_families().iter().flat_map(|family| &family.constructors) {
        if !constructor_ids.insert(constructor.id.0) {
            return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
        }
    }
    for family in candidate.owner_families() {
        if family.slice_carrier.is_some()
            && !matches!(&family.source, OwnerFamilySource::SharedReference(_)) {
            return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
        }
        match &family.source {
            OwnerFamilySource::Aggregate(source_type) => {
                if !opaque_evidence.requires_owner(source_type) {
                    return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
                }
                if !aggregate_types.insert(source_type.clone()) {
                    return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
                }
                validate_aggregate_family(candidate, family, source_type, &records, &layouts)?;
                continue;
            }
            OwnerFamilySource::SharedReference(source_type) => {
                let checked = match source.storage_plan(source_type) {
                    witchy_canonical_source::storage::StoragePlan::RootedView(
                        witchy_canonical_source::rooted_view::RootedViewStorage::RetainedProjection(element)
                    ) => Some(element),
                    _ => None,
                };
                if family.slice_carrier != checked
                    || checked.as_ref().is_some_and(|element| !source.admits_slice_carrier(element)) {
                    return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
                }
                validate_shared_reference_family(candidate.owner_families(), family, source_type, &layouts)?;
                continue;
            }
            OwnerFamilySource::Closure => {}
        }
        let mut source_key = None;
        let mut observed = BTreeSet::new();
        for constructor in &family.constructors {
            let Some(closure) = constructor.witness else {
                validate_adapter_constructor(candidate, family, constructor)?;
                continue;
            };
            let refuse = || CandidateAbiBindingError::CapturingClosureBinding { closure };
            if constructor.id.0 != closure || !seen.insert(closure) { return Err(refuse()); }
            let checked = SourceConstructor::from_checked(source, constructor.id).map_err(|_| refuse())?;
            let witness = checked.witness();
            let signature = witness.signature().cloned().ok_or_else(refuse)?;
            let key = witness_family_keys.get(closure as usize)
                .and_then(Option::as_ref).ok_or_else(refuse)?;
            if source_key.as_ref().is_some_and(|previous| previous != key) { return Err(refuse()); }
            source_key = Some(key.to_owned());
            observed.insert(closure);
            if !candidate.closure_bindings().entries().iter().any(|b| b.function() == constructor.function && b.closure() == closure) {
                return Err(refuse());
            }
            let declaration = candidate.signatures().iter().find(|s| s.id == family.declaration_signature).ok_or_else(refuse)?;
            if !candidate_signature_matches_source_with_layouts(declaration, &signature, candidate, &records, &layouts)
                || constructor.captures.len() != witness.captures().len()
            { return Err(refuse()); }
            let related = witness_family_keys.iter().enumerate()
                .filter(|(_, family)| family.as_ref() == Some(key))
                .collect::<Vec<_>>();
            let position = related.iter().position(|(ordinal, _)| *ordinal == closure as usize).ok_or_else(refuse)?;
            if constructor.environment_tag != i64::try_from(position + 1).map_err(|_| refuse())? { return Err(refuse()); }
            let mut environment_field = 1 + related[..position].iter().map(|(ordinal, _)| {
                family.constructors.iter().find(|constructor| constructor.witness == Some(*ordinal as u32))
                    .map(|constructor| constructor.captures.iter().filter(|capture| {
                        matches!(capture.storage, CaptureStorage::EnvironmentField(_))
                    }).count())
                    .unwrap_or(0)
            }).sum::<usize>();
            let mut owner_field = 2;
            for ((capture, original), transfer) in constructor.captures.iter().zip(witness.captures()).zip(checked.transfers()) {
                // Rooted views are source-authenticated opaque carriers.  The
                // manifest's exact lexical binding and checked type encoding
                // are the authority; matching a pointer-shaped slot alone is
                // never enough to claim a view capture.
                let rooted_view = original.transfer()
                    == witchy_canonical_source::closure::ClosureCaptureTransfer::RootedViewByValue;
                if rooted_view && (!matches!(original.checked_type(), witchy_syntax::ast::Type::Qualified(
                    witchy_syntax::ast::TypeQual::View, _
                )) || original.checked_type_encoding().is_empty()) {
                    return Err(refuse());
                }
                let type_matches = value_type_matches_source(capture.value_type, original.checked_type(), &layouts,
                    candidate.signatures(), &mut CorrespondenceContext::with_owners(candidate), &records, candidate.closure_layouts());
                let expected_storage = if matches!(transfer, CaptureTransfer::CopyCapability(_))
                    || matches!((transfer, capture.value_type), (CaptureTransfer::CloneManaged, ValueType::Gc(_)))
                {
                    let field = owner_field;
                    owner_field += 1;
                    CaptureStorage::OwnerField(field)
                } else {
                    let field = u32::try_from(environment_field).map_err(|_| refuse())?;
                    environment_field += 1;
                    CaptureStorage::EnvironmentField(field)
                };
                if capture.transfer != *transfer || capture.storage != expected_storage || !type_matches
                    // A view capture takes its carrier's own representation:
                    // the `&Int` opaque owner is retained into an owner field,
                    // and a token-carried whole-root view (`&String`, a record,
                    // a scalar List) is retained into a managed environment
                    // field. The token arm is bound to exactly those whole-root
                    // payloads, as the plain closure check binds it. The exact
                    // tuple-root family has no admitted capture fact yet.
                    // Legacy Borrow/BorrowMut types never reach this arm
                    // because the source witness is required to be View.
                    || rooted_view && !(matches!((capture.value_type, capture.storage, capture.transfer),
                        (ValueType::Gc(_), CaptureStorage::OwnerField(_), CaptureTransfer::CloneManaged))
                        || matches!((capture.value_type, capture.storage, capture.transfer),
                            (ValueType::Ref(_), CaptureStorage::EnvironmentField(_), CaptureTransfer::CloneManaged))
                            && super::authority::is_direct_rooted_view_whole_root(original.checked_type(), &records)
                            && !matches!(original.checked_type(),
                                witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                                    if matches!(witchy_canonical_source::carrier::CarrierElement::retained_structural_root(inner, &records),
                                        Some(witchy_canonical_source::carrier::CarrierElement::Tuple(_))))
                            && !matches!(original.checked_type(),
                                witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                                    if witchy_canonical_source::aggregates::is_inline_scalar_view_payload(inner))
                        // A `&Float`/`&Bool`/`&Duration` view is its value
                        // (`ScalarValue`), so its capture is a copied scalar
                        // environment field, as in the plain closure check.
                        || matches!((capture.value_type, capture.storage, capture.transfer),
                            (ValueType::Int | ValueType::Float | ValueType::Bool,
                             CaptureStorage::EnvironmentField(_), CaptureTransfer::CopyScalar))
                            && matches!(original.checked_type(),
                                witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                                    if witchy_canonical_source::aggregates::is_inline_scalar_view_payload(inner)))
                { return Err(refuse()); }
            }
        }
        let key = source_key.ok_or(CandidateAbiBindingError::MissingCanonicalLayouts)?;
        if !family_keys.insert(key.clone()) { return Err(CandidateAbiBindingError::MissingCanonicalLayouts); }
        let expected = witness_family_keys.iter().enumerate()
            .filter_map(|(index, family)| (family.as_ref() == Some(&key)).then_some(index as u32))
            .collect::<BTreeSet<_>>();
        // Opaque storage is required for managed owner captures as well as
        // capabilities. The loop above has already authenticated every
        // capture's source type, transfer, and storage class, so requiring an
        // actual owner field admits only a source-derived opaque family.
        let requires_owner_storage = family.constructors.iter().flat_map(|constructor| &constructor.captures)
            .any(|capture| matches!(capture.storage, CaptureStorage::OwnerField(_)));
        if expected != observed || !requires_owner_storage {
            return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
        }
        let physical = layouts.resolve_nominal(family.environment).map_err(|_| CandidateAbiBindingError::MissingCanonicalLayouts)?;
        let fields = match layouts.get(physical).map(|d| d.kind()) {
            Some(crate::layout::LayoutKind::PackedRecord { fields }) => fields,
            _ => return Err(CandidateAbiBindingError::MissingCanonicalLayouts),
        };
        if !fields.first().is_some_and(|id| matches!(layouts.get(*id).map(|d| d.kind()), Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int)))) {
            return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
        }
        let captures = family.constructors.iter().flat_map(|c| &c.captures).filter_map(|capture| {
            if let CaptureStorage::EnvironmentField(index) = capture.storage { Some((index, capture.value_type)) } else { None }
        }).collect::<BTreeMap<_, _>>();
        if fields.len() != captures.len() + 1 { return Err(CandidateAbiBindingError::MissingCanonicalLayouts); }
        for (index, ty) in captures {
            let descriptor = fields.get(index as usize).and_then(|id| layouts.get(*id)).ok_or(CandidateAbiBindingError::MissingCanonicalLayouts)?;
            let matches = match (ty, descriptor.kind()) {
                (ValueType::Int, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int | crate::layout::ScalarKind::Duration))
                | (ValueType::Float, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Float))
                | (ValueType::Bool, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Bool))
                | (ValueType::Unit, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Unit)) => true,
                (ValueType::Ref(layout), crate::layout::LayoutKind::RuntimePointerValueSlot { pointee, semantics: crate::layout::PointerSlot::OwnedNullable }) => layout == *pointee,
                _ => false,
            };
            if !matches { return Err(CandidateAbiBindingError::MissingCanonicalLayouts); }
        }
    }
    for function in candidate.functions() {
        for block in &function.blocks {
            for operation in &block.operations {
                match operation {
                    Operation::ClosureOwner(OwnerOperation::Construct { constructor, captures, .. }) => {
                        let family = candidate.owner_families().iter().find(|family|
                            family.constructors.iter().any(|entry| entry.id == *constructor))
                            .ok_or(CandidateAbiBindingError::MissingClosureWitness { closure: constructor.0 })?;
                        if matches!(family.source, OwnerFamilySource::Aggregate(_) | OwnerFamilySource::SharedReference(_)) {
                            continue;
                        }
                        let declared = family.constructors.iter().find(|entry| entry.id == *constructor)
                            .expect("family selected by constructor");
                        if declared.witness.is_none() {
                            // The family pass binds this constructor to a
                            // source-declaration adapter, not a lambda ordinal.
                            // Its call has no lexical captures to authenticate.
                            if !captures.is_empty() {
                                return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: declared.function });
                            }
                            continue;
                        }
                        let checked = SourceConstructor::from_checked(source, *constructor)
                            .map_err(|_| CandidateAbiBindingError::MissingClosureWitness { closure: constructor.0 })?;
                        let refuse = || CandidateAbiBindingError::CapturingClosureBinding { closure: constructor.0 };
                        if captures.len() != checked.witness().captures().len() { return Err(refuse()); }
                        let current = checked.witness().id();
                        let enclosing = witnesses.iter().filter(|witness| {
                            let id = witness.id();
                            id.item == current.item
                                && id.method == current.method
                                && id.lambda_path.len() < current.lambda_path.len()
                                && current.lambda_path.starts_with(&id.lambda_path)
                        }).max_by_key(|witness| witness.id().lambda_path.len())
                            .map(|witness| witness.id().clone())
                            .unwrap_or_else(|| witchy_canonical_source::proper_tail::ProperTailCallableId {
                                item: current.item,
                                method: current.method,
                                lambda_path: Vec::new(),
                            });
                        let (expected_function, offset) = if enclosing.lambda_path.is_empty() && enclosing.method.is_none() {
                            let item = usize::try_from(enclosing.item).map_err(|_| refuse())?;
                            let Some(witchy_syntax::ast::Item::Function(owner)) =
                                source.module().items.get(item)
                            else { return Err(refuse()); };
                            let source_abi = source.closed_callable_abi(&owner.name).ok_or_else(refuse)?;
                            let witchy_canonical_source::abi::CanonicalTargetPolicy::ClosedLocal(target) = source_abi.target_policy
                            else { return Err(refuse()); };
                            (candidate.source_abi_bindings().entries().iter()
                                .find(|binding| binding.source_declaration_ordinal() == target.function_ordinal)
                                .map(|binding| binding.function()).ok_or_else(refuse)?, 0)
                        } else {
                            let ordinal = witnesses.iter().position(|witness| witness.id() == &enclosing).ok_or_else(refuse)?;
                            (candidate.closure_bindings().entries().iter().find(|binding| binding.closure() == ordinal as u32)
                                .map(|binding| binding.function()).ok_or_else(refuse)?, 1)
                        };
                        if function.id != expected_function { return Err(refuse()); }
                        for (value, capture) in captures.iter().zip(checked.witness().captures()) {
                            // Lexical identity is checked independently, not
                            // inferred from an SSA name or matching type.
                            let binding = capture.binding();
                            if binding.owner().item != current.item
                                || binding.owner().method != current.method
                                || !current.lambda_path.starts_with(&binding.owner().lambda_path)
                                || binding.path().is_empty()
                            {
                                return Err(refuse());
                            }
                            if binding.owner() == &enclosing
                                && let [ClosureBindingPathStep::Parameter(index)] = binding.path()
                            {
                                let parameter = function.blocks.iter().find(|b| b.id == function.entry)
                                    .and_then(|b| b.parameters.get(*index as usize + offset)).ok_or_else(refuse)?;
                                if !same_parameter(function, *value, parameter.value, &mut BTreeSet::new()) {
                                    return Err(refuse());
                                }
                            }
                        }
                    }
                    Operation::ClosureOwner(OwnerOperation::ReadCapability { owner, constructor, .. }
                        | OwnerOperation::CloneEnvironment { owner, constructor, .. }) => {
                        let family = candidate.owner_families().iter().find(|family|
                            family.constructors.iter().any(|entry| entry.id == *constructor))
                            .ok_or(CandidateAbiBindingError::MissingClosureWitness { closure: constructor.0 })?;
                        if matches!(family.source, OwnerFamilySource::Aggregate(_)) {
                            continue;
                        }
                        let declared = family.constructors.iter().find(|entry| entry.id == *constructor)
                            .expect("family selected by constructor");
                        let parameter = function.blocks.iter().find(|b| b.id == function.entry).and_then(|b| b.parameters.first());
                        if function.id != declared.function || parameter.is_none_or(|p| p.value != *owner) {
                            return Err(CandidateAbiBindingError::CapturingClosureBinding { closure: constructor.0 });
                        }
                    }
                    Operation::ClosureOwner(OwnerOperation::ReadOwner { owner, constructor, capture, .. }) => {
                        let refuse = || CandidateAbiBindingError::CapturingClosureBinding { closure: constructor.0 };
                        let family = candidate.owner_families().iter().find(|family|
                            family.constructors.iter().any(|entry| entry.id == *constructor)).ok_or_else(refuse)?;
                        if matches!(family.source, OwnerFamilySource::Aggregate(_)) {
                            continue;
                        }
                        let declared = family.constructors.iter().find(|entry| entry.id == *constructor)
                            .expect("family selected by constructor");
                        let parameter = function.blocks.iter().find(|b| b.id == function.entry)
                            .and_then(|b| b.parameters.first());
                        let source_constructor = SourceConstructor::from_checked(source, *constructor).map_err(|_| refuse())?;
                        let Some(source_capture) = source_constructor.witness().captures().get(capture.0 as usize) else {
                            return Err(refuse());
                        };
                        let Some(declared_capture) = declared.captures.get(capture.0 as usize) else {
                            return Err(refuse());
                        };
                        let source_transfer_is_authorized = matches!(source_capture.transfer(),
                            witchy_canonical_source::closure::ClosureCaptureTransfer::ManagedReferenceByValue
                        ) || (source_capture.transfer()
                            == witchy_canonical_source::closure::ClosureCaptureTransfer::RootedViewByValue
                            && matches!(source_capture.checked_type(), witchy_syntax::ast::Type::Qualified(
                                witchy_syntax::ast::TypeQual::View, _
                            ))
                            && !source_capture.checked_type_encoding().is_empty());
                        if function.id != declared.function
                            || parameter.is_none_or(|p| p.value != *owner)
                            || !source_transfer_is_authorized
                            || !matches!((declared_capture.value_type, declared_capture.storage, declared_capture.transfer),
                                (ValueType::Gc(_), CaptureStorage::OwnerField(_), CaptureTransfer::CloneManaged))
                            || !value_type_matches_source(declared_capture.value_type, source_capture.checked_type(), &layouts,
                                candidate.signatures(), &mut CorrespondenceContext::with_owners(candidate), &records,
                                candidate.closure_layouts())
                        {
                            return Err(refuse());
                        }
                    }
                    Operation::ClosureOwner(OwnerOperation::ReadCallable { family, .. }) => {
                        if candidate.owner_families().iter().find(|entry| entry.signature == *family)
                            .is_some_and(|entry| matches!(entry.source, OwnerFamilySource::Aggregate(_)))
                        {
                            return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
                        }
                    }
                    Operation::ClosureOwner(OwnerOperation::ReadSharedPlace { family, .. }) => {
                        if candidate.owner_families().iter().find(|entry| entry.signature == *family)
                            .is_none_or(|entry| !matches!(entry.source, OwnerFamilySource::SharedReference(_)))
                        {
                            return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
                        }
                    }
                    Operation::ClosureOwner(OwnerOperation::ReadSharedSlice { family, .. }) => {
                        if candidate.owner_families().iter().find(|entry| entry.signature == *family)
                            .is_none_or(|entry| !matches!(&entry.source,
                                OwnerFamilySource::SharedReference(_) if entry.slice_carrier.is_some()))
                        {
                            return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn is_shared_scalar_cell_reference(source_type: &witchy_syntax::ast::Type) -> bool {
    matches!(witchy_canonical_source::storage::StoragePlan::classify(
        source_type, &BTreeMap::new(), |_| None,
    ), witchy_canonical_source::storage::StoragePlan::RootedView(
        witchy_canonical_source::rooted_view::RootedViewStorage::ScalarCell(_)))
}

fn is_rooted_int_slice_reference(source_type: &witchy_syntax::ast::Type) -> bool {
    rooted_slice_carrier(source_type) == Some(witchy_canonical_source::carrier::CarrierElement::Scalar(
        witchy_canonical_source::abi::CanonicalScalarKind::Int))
}

fn insert_compatible_environment_field(
    expected: &mut BTreeMap<u32, ValueType>,
    field: u32,
    value_type: ValueType,
) -> Result<(), CandidateAbiBindingError> {
    match expected.get(&field) {
        Some(existing) if *existing != value_type => Err(CandidateAbiBindingError::MissingCanonicalLayouts),
        Some(_) => Ok(()),
        None => {
            expected.insert(field, value_type);
            Ok(())
        }
    }
}

fn validate_shared_reference_family(
    families: &[OwnerFamily],
    family: &OwnerFamily,
    source_type: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
) -> Result<(), CandidateAbiBindingError> {
    let refuse = || CandidateAbiBindingError::MissingCanonicalLayouts;
    if !(is_shared_scalar_cell_reference(source_type) || family.slice_carrier.is_some())
        || family.constructors.is_empty()
    {
        return Err(refuse());
    }
    let mut kinds = BTreeSet::new();
    let mut expected_environment = BTreeMap::new();
    for constructor in &family.constructors {
        if constructor.witness.is_some() {
            return Err(refuse());
        }
        let kind = constructor.shared_reference_constructor().ok_or_else(refuse)?;
        if let SharedReferenceConstructor::ListIntElementFromSlice { slice, index, bounds: _ } = kind {
            let slice_capture = constructor.captures.get(slice.0 as usize).ok_or_else(refuse)?;
            let index_capture = constructor.captures.get(index.0 as usize).ok_or_else(refuse)?;
            let (ValueType::Gc(slice_type), CaptureStorage::OwnerField(slice_field), CaptureTransfer::CloneManaged) =
                (slice_capture.value_type, slice_capture.storage, slice_capture.transfer)
            else { return Err(refuse()) };
            let CaptureStorage::EnvironmentField(index_field) = index_capture.storage else { return Err(refuse()) };
            if index_capture.value_type != ValueType::Size
                || index_capture.transfer != CaptureTransfer::CopyScalar
                || !is_shared_scalar_cell_reference(source_type)
                || slice.0 != 0 || index.0 != 1 || slice_field != 2 || !kinds.insert(3_u8)
                || !families.iter().any(|candidate| {
                    candidate.owner_type == slice_type
                        && matches!(&candidate.source, OwnerFamilySource::SharedReference(source)
                            if is_rooted_int_slice_reference(source))
                })
            {
                return Err(refuse());
            }
            insert_compatible_environment_field(&mut expected_environment, index_field, ValueType::Size)?;
            continue;
        }
        let root = constructor.captures.first().ok_or_else(refuse)?;
        let ValueType::Ref(root_layout) = root.value_type else { return Err(refuse()) };
        let CaptureStorage::EnvironmentField(root_field) = root.storage else { return Err(refuse()) };
        insert_compatible_environment_field(&mut expected_environment, root_field, root.value_type)?;
        if matches!(layouts.get(root_layout).map(|descriptor| descriptor.kind()),
            Some(crate::layout::LayoutKind::OpaqueNominal { .. })) { return Err(refuse()); }
        let physical = layouts.resolve_nominal(root_layout).map_err(|_| refuse())?;
        match kind {
            SharedReferenceConstructor::WholeIntCell { root } => {
                if !is_shared_scalar_cell_reference(source_type) || root.0 != 0 || !kinds.insert(0_u8)
                    || !matches!(layouts.get(physical).map(|descriptor| descriptor.kind()),
                        Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int)))
                {
                    return Err(refuse());
                }
            }
            SharedReferenceConstructor::ListIntElement { root, index, bounds: _ } => {
                let index_capture = constructor.captures.get(index.0 as usize).ok_or_else(refuse)?;
                let CaptureStorage::EnvironmentField(index_field) = index_capture.storage else { return Err(refuse()) };
                insert_compatible_environment_field(&mut expected_environment, index_field, ValueType::Size)?;
                let Some(crate::layout::LayoutKind::PackedList { element, rc: crate::layout::RcHeader::Required }) =
                    layouts.get(physical).map(|descriptor| descriptor.kind())
                else { return Err(refuse()) };
                if !is_shared_scalar_cell_reference(source_type) || root.0 != 0 || index.0 != 1 || !kinds.insert(1_u8)
                    || !matches!(layouts.get(*element).map(|descriptor| descriptor.kind()),
                        Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int)))
                {
                    return Err(refuse());
                }
            }
            SharedReferenceConstructor::ListIntElementFromSlice { .. } => {
                // This nested-carrier schema is handled before the linear
                // root-layout path above; reaching it here is fail-closed.
                return Err(refuse());
            }
            SharedReferenceConstructor::ListSlice { root, start, length } => {
                let start_capture = constructor.captures.get(start.0 as usize).ok_or_else(refuse)?;
                let length_capture = constructor.captures.get(length.0 as usize).ok_or_else(refuse)?;
                let (CaptureStorage::EnvironmentField(start_field), CaptureStorage::EnvironmentField(length_field)) =
                    (start_capture.storage, length_capture.storage)
                else { return Err(refuse()) };
                insert_compatible_environment_field(&mut expected_environment, start_field, ValueType::Size)?;
                insert_compatible_environment_field(&mut expected_environment, length_field, ValueType::Size)?;
                let Some(crate::layout::LayoutKind::PackedList { element, rc: crate::layout::RcHeader::Required }) =
                    layouts.get(physical).map(|descriptor| descriptor.kind())
                else { return Err(refuse()) };
                // The backing list's element representation must be exactly
                // the family's checked `&[T]` element.
                if !family.slice_carrier.as_ref()
                        .is_some_and(|carrier| carrier_element_layout_matches(layouts, *element, carrier))
                    || root.0 != 0 || start.0 != 1 || length.0 != 2 || !kinds.insert(2_u8)
                {
                    return Err(refuse());
                }
            }
        }
    }
    validate_environment_layout(family.environment, &expected_environment, layouts, true)
}

fn validate_adapter_constructor<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    family: &OwnerFamily,
    constructor: &OwnerConstructor,
) -> Result<(), CandidateAbiBindingError> {
    let refuse = || CandidateAbiBindingError::AdapterShapeMismatch { adapter: constructor.function };
    let binding = candidate.adapter_bindings().entries().iter()
        .find(|binding| binding.adapter() == constructor.function).ok_or_else(refuse)?;
    let adapter = candidate.functions().iter().find(|function| function.id == binding.adapter()).ok_or_else(refuse)?;
    let target = candidate.functions().iter().find(|function| function.id == binding.target()).ok_or_else(refuse)?;
    // The enclosing authority validation independently binds this target to
    // checked source and checks the adapter's forwarding body. Candidate names
    // and constructor ordinals cannot substitute for either binding.
    if constructor.witness.is_some() || constructor.environment_tag != 0 || !constructor.captures.is_empty()
        || adapter.signature != family.signature || target.signature != family.declaration_signature
        || !candidate.source_abi_bindings().entries().iter().any(|entry| entry.function() == target.id)
    {
        return Err(refuse());
    }
    Ok(())
}

fn validate_aggregate_family<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    family: &OwnerFamily,
    claimed: &witchy_syntax::ast::Type,
    records: &BTreeMap<String, witchy_syntax::ast::TypeDef>,
    layouts: &crate::layout::LayoutInterner,
) -> Result<(), CandidateAbiBindingError> {
    let refuse = || CandidateAbiBindingError::MissingCanonicalLayouts;
    let normalized = witchy_canonical_source::aggregates::canonical_owner_aggregate_identity(claimed, records)
        .ok_or_else(refuse)?;
    if &normalized != claimed { return Err(refuse()); }
    let shape = witchy_canonical_source::aggregates::scalar_aggregate_shape(&normalized, records)
        .filter(|shape| !shape.packed).ok_or_else(refuse)?;
    if shape.variants.len() != family.constructors.len()
        || family.constructors.is_empty()
    {
        return Err(refuse());
    }
    let mut environment_field = 1_u32;
    let mut owner_field = 2_u32;
    let mut expected_environment = BTreeMap::new();
    for (variant_index, (variant, constructor)) in shape.variants.iter().zip(&family.constructors).enumerate() {
        if constructor.witness.is_some()
            || constructor.environment_tag != i64::try_from(variant_index).map_err(|_| refuse())?
            || constructor.captures.len() != variant.arity
        {
            return Err(refuse());
        }
        for (declared, source_field) in constructor.captures.iter().zip(
            &shape.fields[variant.first_field..variant.first_field + variant.arity]) {
            if !value_type_matches_source(
                declared.value_type,
                source_field,
                layouts,
                candidate.signatures(),
                &mut CorrespondenceContext::with_owners(candidate),
                records,
                candidate.closure_layouts(),
            ) {
                return Err(refuse());
            }
            let (storage, transfer) = match declared.value_type {
                ValueType::Capability(kind) => {
                    let field = owner_field;
                    owner_field = owner_field.checked_add(1).ok_or_else(refuse)?;
                    (CaptureStorage::OwnerField(field), CaptureTransfer::CopyCapability(kind))
                }
                ValueType::Gc(_) => {
                    let field = owner_field;
                    owner_field = owner_field.checked_add(1).ok_or_else(refuse)?;
                    (CaptureStorage::OwnerField(field), CaptureTransfer::CloneManaged)
                }
                ValueType::Ref(_) => {
                    let field = environment_field;
                    environment_field = environment_field.checked_add(1).ok_or_else(refuse)?;
                    expected_environment.insert(field, declared.value_type);
                    (CaptureStorage::EnvironmentField(field), CaptureTransfer::CloneManaged)
                }
                ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit | ValueType::Size => {
                    let field = environment_field;
                    environment_field = environment_field.checked_add(1).ok_or_else(refuse)?;
                    expected_environment.insert(field, declared.value_type);
                    (CaptureStorage::EnvironmentField(field), CaptureTransfer::CopyScalar)
                }
                ValueType::V128 | ValueType::Callable(_) | ValueType::NullableCapability(_)
                | ValueType::InlineAggregate(_) => return Err(refuse()),
            };
            if declared.storage != storage || declared.transfer != transfer { return Err(refuse()); }
        }
    }
    validate_environment_layout(family.environment, &expected_environment, layouts, shape.tagged)
}

fn validate_environment_layout(
    environment: crate::layout::LayoutId,
    expected: &BTreeMap<u32, ValueType>,
    layouts: &crate::layout::LayoutInterner,
    tagged: bool,
) -> Result<(), CandidateAbiBindingError> {
    let refuse = || CandidateAbiBindingError::MissingCanonicalLayouts;
    let physical = layouts.resolve_nominal(environment).map_err(|_| refuse())?;
    let fields = match layouts.get(physical).map(|descriptor| descriptor.kind()) {
        Some(crate::layout::LayoutKind::PackedRecord { fields }) => fields,
        _ => return Err(refuse()),
    };
    if fields.len() != expected.len() + 1
        || !fields.first().is_some_and(|id| matches!(layouts.get(*id).map(|descriptor| descriptor.kind()),
            Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int))))
    {
        return Err(refuse());
    }
    for (index, value_type) in expected {
        let kind = fields.get(*index as usize).and_then(|id| layouts.get(*id)).map(|descriptor| descriptor.kind()).ok_or_else(refuse)?;
        let matched = match (value_type, kind) {
            (ValueType::Int, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int | crate::layout::ScalarKind::Duration))
            | (ValueType::Float, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Float))
            | (ValueType::Bool, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Bool))
            | (ValueType::Unit, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Unit))
            | (ValueType::Size, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::U32)) => true,
            (ValueType::Ref(layout), crate::layout::LayoutKind::RuntimePointerValueSlot { pointee, semantics }) => {
                layout == pointee && *semantics == if tagged {
                    crate::layout::PointerSlot::OwnedNullable
                } else { crate::layout::PointerSlot::Owned }
            }
            _ => false,
        };
        if !matched { return Err(refuse()); }
    }
    Ok(())
}

fn same_parameter(function: &super::Function, value: ValueId, parameter: ValueId, active: &mut BTreeSet<ValueId>) -> bool {
    if value == parameter { return true; }
    if !active.insert(value) { return false; }
    let matches = function.blocks.iter().flat_map(|b| &b.operations).find_map(|operation| match operation {
        Operation::CloneRef { destination, source } | Operation::MoveValue { destination, source } if *destination == value => {
            Some(same_parameter(function, *source, parameter, active))
        }
        _ => None,
    }).unwrap_or(false);
    active.remove(&value);
    matches
}

// Owner-family cases that need a real checked source (RFC-0170 P4).
#[cfg(feature = "checked-source-test-api")]
#[doc(hidden)]
pub mod checked_source_cases;
