use super::*;

pub(super) fn validate_external_imports(
    candidate: &CanonicalModule<LoweredCandidate>,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<(), InitialWitnessError> {
    let expected = source.precompiled_exports().iter().map(|export| {
        (
            format!("witchy.precompiled.{}", export.library_alias),
            export.export.clone(),
            super::super::ExternalWasmIdentity {
                library_binding_digest: export.library_binding_digest,
                wasm_sha256: export.wasm_sha256,
                sidecar_sha256: export.sidecar_sha256,
            },
        )
    }).collect::<HashSet<_>>();
    let actual = candidate.imports().iter().filter_map(|import| {
        let super::super::ImportContractIdentity::AuthenticatedExternal(identity) = &import.abi_identity else {
            return None;
        };
        Some((import.module.clone(), import.name.clone(), *identity))
    }).collect::<HashSet<_>>();
    (actual == expected).then_some(()).ok_or(InitialWitnessError::ExternalImportMismatch)
}

pub(super) fn validate_candidate_abi_bindings<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<(), CandidateAbiBindingError> {
    use witchy_canonical_source::abi::CanonicalTargetPolicy;
    super::super::vm_service::validate(candidate, source).map_err(CandidateAbiBindingError::VmService)?;
    super::super::owner_authority::validate(candidate, source)?;

    let source_entries = source.callable_abi_manifest().entries().iter().filter_map(|entry| {
        let CanonicalTargetPolicy::ClosedLocal(target) = entry.target_policy else { return None };
        Some((target.function_ordinal, entry.key.as_bytes()))
    }).collect::<BTreeMap<_, _>>();
    let records = source.type_definitions();
    // Signature correspondence reads a `&String` field as its String token;
    // computed once, used only by the two layout-correspondence matches.
    let byte_buffer_view_records = erase_record_byte_buffer_views(&records);
    let access_facts = source.access_facts()
        .ok_or(CandidateAbiBindingError::MissingCanonicalLayouts)?;
    // A direct exclusive ABI has no syntax-only fallback: a stale or absent
    // checked loan graph must not be silently treated as an empty one.
    let loan_facts_available = source.loans_checked();
    // One decode of the candidate's layout bundle serves every signature
    // comparison and the closure catalog below.
    let (_bundle, layouts) = match crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes()) {
        Ok(value) => value,
        Err(_) => return Err(CandidateAbiBindingError::MissingCanonicalLayouts),
    };
    let mut bindings = BTreeMap::new();
    let mut bound_functions = BTreeMap::new();
    for binding in candidate.source_abi_bindings().entries() {
        if bindings.insert(binding.source_declaration_ordinal(), binding).is_some() {
            return Err(CandidateAbiBindingError::DuplicateSourceDeclaration {
                source_declaration: binding.source_declaration_ordinal(),
            });
        }
        if bound_functions.insert(binding.function(), binding).is_some() {
            return Err(CandidateAbiBindingError::DuplicateCandidateFunction { function: binding.function() });
        }
    }
    for source_declaration in source_entries.keys() {
        let Some(binding) = bindings.get(source_declaration) else {
            return Err(CandidateAbiBindingError::MissingBinding { source_declaration: *source_declaration });
        };
        let _ = binding;
    }
    for source_declaration in bindings.keys() {
        if !source_entries.contains_key(source_declaration) {
            return Err(CandidateAbiBindingError::UnexpectedBinding { source_declaration: *source_declaration });
        }
    }
    let mut functions = BTreeMap::new();
    for function in candidate.functions() {
        if functions.insert(function.id, function).is_some() {
            return Err(CandidateAbiBindingError::DuplicateCandidateFunction { function: function.id });
        }
    }
    for binding in candidate.source_abi_bindings().entries() {
        let Some(function) = functions.get(&binding.function()) else {
            return Err(CandidateAbiBindingError::MissingCandidateFunction { function: binding.function() });
        };
        let Some(signature) = candidate.signatures().iter().find(|signature| signature.id == function.signature) else {
            return Err(CandidateAbiBindingError::MissingCandidateSignature { function: binding.function() });
        };
        let Some(source_signature) = source.closed_callable_signature(binding.source_declaration_ordinal()) else {
            return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: binding.source_declaration_ordinal() });
        };
        if !record_root_plans_in_signature(source, source_signature) {
            return Err(CandidateAbiBindingError::SourceAbiMismatch {
                source_declaration: binding.source_declaration_ordinal(),
            });
        }
        let source_declaration = source.closed_callable_declaration(binding.source_declaration_ordinal());
        // The checked callable ABI erases unsupported legacy loan spelling.
        // Check every original annotation before comparing a candidate to it.
        if source_declaration.is_some_and(|declaration| {
            declaration.ret.as_ref().is_some_and(|result|
                original_type_has_unchecked_loan(result, &records))
                || declaration.params.iter().any(|parameter| parameter.ty.as_ref()
                    .is_some_and(|ty| original_type_has_unchecked_loan(ty, &records)))
        })
        {
            return Err(CandidateAbiBindingError::MissingCheckedReferenceRelation {
                source_declaration: binding.source_declaration_ordinal(),
            });
        }
        if source_declaration.is_some_and(|declaration| {
            generic_shared_relay_template_candidate(source, &declaration.name)
                && !exact_generic_shared_relay_binding(source, source_signature, declaration, signature)
        }) {
            return Err(CandidateAbiBindingError::MissingCheckedReferenceRelation {
                source_declaration: binding.source_declaration_ordinal(),
            });
        }
        let projected_scalar_relay = source_declaration.is_some_and(|declaration| {
            authenticated_projected_scalar_relay_relation_is_checked(
                declaration,
                source.module(),
                access_facts,
                &records,
            )
        });
        if !direct_reference_binding_is_checked(
            &source_signature,
            source_declaration,
            source_declaration.and_then(|declaration| access_facts.declaration(&declaration.name)),
            loan_facts_available,
            &records,
        ) && !projected_scalar_relay {
            return Err(CandidateAbiBindingError::MissingCheckedReferenceRelation {
                source_declaration: binding.source_declaration_ordinal(),
            });
        }
        let mut source_signature = restore_source_abi_qualifiers(
            source_signature,
            source_declaration,
            &records,
        );
        restore_authenticated_projected_scalar_relay_abi(
            &mut source_signature,
            source_declaration,
            source.module(),
            access_facts,
            &records,
        );
        let source_signature = erase_scalar_value_views(&erase_nested_byte_buffer_views(&source_signature));
        let source_matches = candidate_signature_matches_source_with_layouts(
            signature,
            &source_signature,
            candidate,
            &byte_buffer_view_records,
            &layouts,
        );
        let fresh_list_push_matches = !source_matches
            && source_declaration.is_some_and(|declaration|
                declaration.name.starts_with("list.push__")
                    || declaration.name.starts_with("list.__push__"))
            && {
                // The compiler-owned list.push helper always allocates its
                // replacement. Compare its physical ABI against source after
                // erasing only that stronger result fact; the safety checker
                // must still prove the original Unique contract from the body.
                let mut source_shaped = (*signature).clone();
                for result in &mut source_shaped.contract.results {
                    if let super::super::ResultContract::Unique { layout } = result {
                        *result = super::super::ResultContract::Shared { layout: *layout };
                    }
                }
                candidate_signature_matches_source_with_layouts(
                    &source_shaped,
                    &source_signature,
                    candidate,
                    &byte_buffer_view_records,
                    &layouts,
                )
            };
        let projected_contract_matches = source_declaration.is_none_or(|declaration| {
            projected_exclusive_result_contract_matches(signature, declaration, &records)
        });
        if (!source_matches || !projected_contract_matches) && !fresh_list_push_matches {
            return Err(CandidateAbiBindingError::CandidateSignatureMismatch { function: binding.function() });
        }
    }
    // A function synthesized for a lambda is bound to that lambda's closure
    // witness instead of a source declaration.  The uniform closure record
    // owns a tagged environment record; captures are its checked scalar or
    // managed-reference payloads, never capabilities or an erased value.
    let closures = source.closure_witness().entries();
    // A malformed checked signature cannot be omitted from a normalized
    // family merely because its structural key exceeds the identity budget.
    let closure_family_keys = closures.iter().map(|witness| {
        witness.signature().map(|signature| {
            source_closure_family_key(signature, &records)
                .ok_or(CandidateAbiBindingError::MissingCanonicalLayouts)
        }).transpose()
    }).collect::<Result<Vec<_>, _>>()?;
    let closure_layout_catalog = &layouts;
    let mut closure_bound = BTreeMap::new();
    for binding in candidate.closure_bindings().entries() {
        if bound_functions.contains_key(&binding.function())
            || closure_bound.insert(binding.function(), binding).is_some()
        {
            return Err(CandidateAbiBindingError::DuplicateCandidateFunction { function: binding.function() });
        }
        let Some(witness) = closures.get(binding.closure() as usize) else {
            return Err(CandidateAbiBindingError::MissingClosureWitness { closure: binding.closure() });
        };
        let Some(function) = functions.get(&binding.function()) else {
            return Err(CandidateAbiBindingError::MissingCandidateFunction { function: binding.function() });
        };
        let Some(signature) = candidate.signatures().iter().find(|signature| signature.id == function.signature) else {
            return Err(CandidateAbiBindingError::MissingCandidateSignature { function: binding.function() });
        };
        let source_signature = witness.declared_signature().clone();
        if let Some(super::super::ValueType::Gc(owner)) = signature.parameters.first() {
            if !candidate.owner_families().iter().any(|family| matches!(family.source, super::super::closure_owner_contract::OwnerFamilySource::Closure)
                && family.owner_type == *owner && family.signature == signature.id
                && family.constructors.iter().any(|constructor| constructor.function == function.id && constructor.witness == Some(binding.closure())))
            {
                return Err(CandidateAbiBindingError::CapturingClosureBinding { closure: binding.closure() });
            }
            let mut declaration = signature.clone();
            declaration.parameters.remove(0);
            if declaration.contract.parameters.is_empty() {
                return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
            }
            declaration.contract.parameters.remove(0);
            declaration.contract.parameter_effects = declaration.contract.parameter_effects.into_iter()
                .filter_map(|(index, effects)| index.checked_sub(1).map(|index| (index, effects))).collect();
            declaration.contract.results = strip_closure_result_contracts(&declaration.contract.results)
                .ok_or(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() })?;
            if !candidate_signature_matches_source_with_layouts(&declaration, &source_signature, candidate, &records, &layouts) {
                return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
            }
            continue;
        }
        let family_key = closure_family_keys.get(binding.closure() as usize)
            .and_then(Option::as_ref)
            .ok_or(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() })?;
        let family = closures.iter().zip(&closure_family_keys)
            .filter_map(|(entry, key)| (key.as_ref() == Some(family_key)).then_some(entry))
            .collect::<Vec<_>>();
        let Some(tag) = family.iter().position(|entry| entry.id() == witness.id()) else {
            return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
        };
        let environment = match signature.parameters.first() {
            Some(super::super::ValueType::Ref(layout)) => closure_layout_catalog
                .get(*layout)
                .and_then(|descriptor| match descriptor.kind() {
                    crate::layout::LayoutKind::PackedRecord { fields } if fields.len() == 2 => {
                        let crate::layout::LayoutKind::RuntimePointerValueSlot {
                            pointee, semantics: crate::layout::PointerSlot::Owned,
                        } = closure_layout_catalog.get(fields[1])?.kind() else { return None };
                        let physical = closure_layout_catalog.resolve_nominal(*pointee).ok()?;
                        closure_layout_catalog.get(physical).and_then(|environment| match environment.kind() {
                            crate::layout::LayoutKind::PackedRecord { fields } => Some((*pointee, fields.clone())),
                            _ => None,
                        })
                    }
                    _ => None,
                })
                .ok_or(CandidateAbiBindingError::CapturingClosureBinding { closure: binding.closure() })?,
            _ => return Err(CandidateAbiBindingError::CapturingClosureBinding { closure: binding.closure() }),
        };
        let (environment_layout, environment_fields) = environment;
        let expected = family.iter().flat_map(|entry| entry.captures()).collect::<Vec<_>>();
        let own_offset = family[..tag].iter().map(|entry| entry.captures().len()).sum::<usize>();
        let tag_is_int = environment_fields.first().is_some_and(|field| matches!(
            closure_layout_catalog.get(*field).map(|field| field.kind()),
            Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int))
        ));
        let mut correspondence = CorrespondenceContext::with_owners(candidate);
        let expected_field_types = expected.iter().enumerate().map(|(index, capture)| {
                let field = *environment_fields.get(index + 1)?;
                let actual = match closure_layout_catalog.get(field).map(|field| field.kind()) {
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int | crate::layout::ScalarKind::Duration)) => super::super::ValueType::Int,
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Float)) => super::super::ValueType::Float,
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Bool)) => super::super::ValueType::Bool,
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Unit)) => super::super::ValueType::Unit,
                    Some(crate::layout::LayoutKind::RuntimePointerValueSlot { pointee, semantics: crate::layout::PointerSlot::OwnedNullable }) => super::super::ValueType::Ref(*pointee),
                    _ => return None,
                };
                let transfer_matches = matches!(
                    (actual, capture.transfer()),
                    (super::super::ValueType::Ref(_), witchy_canonical_source::closure::ClosureCaptureTransfer::ManagedReferenceByValue)
                        | (super::super::ValueType::Int | super::super::ValueType::Float | super::super::ValueType::Bool | super::super::ValueType::Unit,
                           witchy_canonical_source::closure::ClosureCaptureTransfer::CopyScalar)
                ) || (
                    // A whole-root view (`&String`, a record or sum, a scalar
                    // List) is its retained object token, so its capture is a
                    // managed reference slot. The exact tuple-root family has no
                    // separately checked closure-capture witness yet.
                    matches!(actual, super::super::ValueType::Ref(_))
                        && matches!(capture.transfer(), witchy_canonical_source::closure::ClosureCaptureTransfer::RootedViewByValue)
                        && is_direct_rooted_view_whole_root(capture.checked_type(), &records)
                        && !matches!(capture.checked_type(),
                            witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                                if matches!(witchy_canonical_source::carrier::CarrierElement::retained_structural_root(inner, &records),
                                    Some(witchy_canonical_source::carrier::CarrierElement::Tuple(_))))
                        && !matches!(capture.checked_type(),
                            witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                                if witchy_canonical_source::aggregates::is_inline_scalar_view_payload(inner))
                ) || (
                    // A `&Float`/`&Bool`/`&Duration` view is its value
                    // (`ScalarValue`), so its capture is a copied scalar slot.
                    matches!(actual, super::super::ValueType::Int | super::super::ValueType::Float | super::super::ValueType::Bool)
                        && matches!(capture.transfer(), witchy_canonical_source::closure::ClosureCaptureTransfer::RootedViewByValue)
                        && matches!(capture.checked_type(),
                            witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                                if witchy_canonical_source::aggregates::is_inline_scalar_view_payload(inner))
                );
                (transfer_matches && value_type_matches_source(
                    actual, capture.checked_type(), closure_layout_catalog, candidate.signatures(), &mut correspondence,
                    &records, candidate.closure_layouts(),
                )).then_some(actual)
            }).collect::<Vec<_>>();
        let fields_match = environment_fields.len() == expected.len() + 1
            && expected_field_types.iter().all(Option::is_some);
        // Tag zero is reserved for the adapter environment.  The binding's
        // source-witness ordinal therefore selects its exact nonzero variant.
        if !tag_is_int
            || !fields_match
            || !closure_environment_prefix_matches(
                function,
                environment_layout,
                i64::try_from(tag + 1).ok(),
                u32::try_from(own_offset + 1).ok(),
                &expected_field_types[own_offset..own_offset + witness.captures().len()],
            )
        {
            return Err(CandidateAbiBindingError::CapturingClosureBinding { closure: binding.closure() });
        }
        // The synthesized function takes the closure object first; only the
        // remaining lanes correspond to the source lambda's own signature.
        let Some(source_lanes) = strip_closure_environment(signature, candidate) else {
            return Err(CandidateAbiBindingError::MissingClosureEnvironmentParameter { function: binding.function() });
        };
        if !candidate_signature_matches_source_with_layouts(&source_lanes, &source_signature, candidate, &records, &layouts) {
            return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
        }
    }
    // A function synthesized for a named function used as a value forwards to
    // its target under the environment-taking ABI and does nothing else.
    let mut adapter_bound = BTreeMap::new();
    for binding in candidate.adapter_bindings().entries() {
        if bound_functions.contains_key(&binding.adapter())
            || closure_bound.contains_key(&binding.adapter())
            || adapter_bound.insert(binding.adapter(), binding).is_some()
        {
            return Err(CandidateAbiBindingError::DuplicateCandidateFunction { function: binding.adapter() });
        }
        let Some(adapter) = functions.get(&binding.adapter()) else {
            return Err(CandidateAbiBindingError::MissingCandidateFunction { function: binding.adapter() });
        };
        let Some(target) = functions.get(&binding.target()) else {
            return Err(CandidateAbiBindingError::MissingCandidateFunction { function: binding.target() });
        };
        let Some(adapter_signature) = candidate.signatures().iter().find(|signature| signature.id == adapter.signature) else {
            return Err(CandidateAbiBindingError::MissingCandidateSignature { function: binding.adapter() });
        };
        let Some(target_signature) = candidate.signatures().iter().find(|signature| signature.id == target.signature) else {
            return Err(CandidateAbiBindingError::MissingCandidateSignature { function: binding.target() });
        };
        if strip_closure_environment(adapter_signature, candidate)
            .is_none_or(|lanes| !signature_lanes_match(&lanes, target_signature))
        {
            return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: binding.adapter() });
        }
        let owner_family = candidate.owner_families().iter().find(|family| {
            matches!(family.source, super::super::closure_owner_contract::OwnerFamilySource::Closure)
                && family.signature == adapter.signature
                && adapter_signature.parameters.first() == Some(&super::super::ValueType::Gc(family.owner_type))
        });
        if let Some(family) = owner_family {
            let Some(constructor) = family.constructors.iter().find(|constructor| {
                constructor.function == adapter.id && constructor.witness.is_none()
            }) else {
                return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: binding.adapter() });
            };
            if constructor.environment_tag != 0
                || !constructor.captures.is_empty()
                || target.signature != family.declaration_signature
            {
                return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: binding.adapter() });
            }
        }
        if !adapter_body_forwards(adapter, target.id, target_signature, owner_family.map(|family| family.signature)) {
            return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: binding.adapter() });
        }
    }
    if functions.len() != bound_functions.len() + closure_bound.len() + adapter_bound.len() {
        let function = functions.keys()
            .find(|function| !bound_functions.contains_key(function)
                && !closure_bound.contains_key(function)
                && !adapter_bound.contains_key(function))
            .copied().unwrap_or_else(|| *bound_functions.keys().next().expect("nonempty mismatch"));
        return Err(CandidateAbiBindingError::MissingCandidateFunction { function });
    }
    Ok(())
}

/// The signature that remains when the leading closure-environment parameter
/// is removed. `None` when the first parameter is not a shared reference to a
/// layout the module registered as carrying this very signature -- so a
/// candidate cannot smuggle an arbitrary extra parameter past the comparison.
pub(super) fn strip_closure_environment<S: ModuleStage>(
    signature: &super::super::FunctionSignature,
    candidate: &CanonicalModule<S>,
) -> Option<super::super::FunctionSignature> {
    match signature.parameters.first()? {
        super::super::ValueType::Ref(layout) => {
            if candidate.closure_layouts().get(layout) != Some(&signature.id) {
                return None;
            }
        }
        super::super::ValueType::Gc(owner) => {
            if !candidate.owner_families().iter().any(|family| {
                matches!(family.source, super::super::closure_owner_contract::OwnerFamilySource::Closure)
                    && family.owner_type == *owner && family.signature == signature.id
            }) {
                return None;
            }
        }
        _ => return None,
    }
    let environment = signature.contract.parameters.first()?;
    if environment.value_type != signature.parameters[0]
        || environment.ownership != super::super::ParameterOwnership::Shared
    {
        return None;
    }
    Some(super::super::FunctionSignature {
        id: signature.id,
        parameters: signature.parameters[1..].to_vec(),
        results: signature.results.clone(),
        contract: super::super::CallableContract {
            parameters: signature.contract.parameters[1..].to_vec(),
            results: strip_closure_result_contracts(&signature.contract.results)?,
            parameter_effects: signature
                .contract
                .parameter_effects
                .iter()
                .filter_map(|(parameter, effects)| {
                    parameter.checked_sub(1).map(|shifted| (shifted, effects.clone()))
                })
                .collect(),
        },
    })
}

pub(super) fn strip_closure_environment_with(
    signature: &super::super::FunctionSignature,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::super::FunctionSignatureId>,
) -> Option<super::super::FunctionSignature> {
    let super::super::ValueType::Ref(layout) = *signature.parameters.first()? else { return None };
    if closure_layouts.get(&layout) != Some(&signature.id) {
        return None;
    }
    let environment = signature.contract.parameters.first()?;
    if environment.value_type != super::super::ValueType::Ref(layout)
        || environment.ownership != super::super::ParameterOwnership::Shared
    {
        return None;
    }
    Some(super::super::FunctionSignature {
        id: signature.id,
        parameters: signature.parameters[1..].to_vec(),
        results: signature.results.clone(),
        contract: super::super::CallableContract {
            parameters: signature.contract.parameters[1..].to_vec(),
            results: strip_closure_result_contracts(&signature.contract.results)?,
            parameter_effects: signature
                .contract
                .parameter_effects
                .iter()
                .filter_map(|(parameter, effects)| {
                    parameter.checked_sub(1).map(|shifted| (shifted, effects.clone()))
                })
                .collect(),
        },
    })
}

/// Closure ABI parameter zero is the environment record.  Remove it from
/// result-alias targets together with the parameter lane; a bare physical
/// layout must never make an alias point at that hidden owner.
pub(super) fn strip_closure_result_contracts(
    results: &[super::super::ResultContract],
) -> Option<Vec<super::super::ResultContract>> {
    results
        .iter()
        .cloned()
        .map(|result| match result {
            super::super::ResultContract::AliasesParameter { parameter, layout } => {
                Some(super::super::ResultContract::AliasesParameter {
                    parameter: parameter.checked_sub(1)?,
                    layout,
                })
            }
            super::super::ResultContract::AliasesWriteBackParameter { parameter, layout, projection } => {
                Some(super::super::ResultContract::AliasesWriteBackParameter {
                    parameter: parameter.checked_sub(1)?,
                    layout,
                    projection,
                })
            }
            super::super::ResultContract::ReallocatesParameter { parameter, layout } => {
                Some(super::super::ResultContract::ReallocatesParameter {
                    parameter: parameter.checked_sub(1)?,
                    layout,
                })
            }
            result => Some(result),
        })
        .collect()
}

/// Two signatures carry the same lanes: the identity differs (an adapter has
/// its own signature id) but every parameter, result, and contract does not.
pub(super) fn signature_lanes_match(
    actual: &super::super::FunctionSignature,
    expected: &super::super::FunctionSignature,
) -> bool {
    actual.parameters == expected.parameters
        && actual.results == expected.results
        && actual.contract.parameters == expected.contract.parameters
        && actual.contract.results == expected.contract.results
        && actual.contract.parameter_effects == expected.contract.parameter_effects
}

/// The adapter's body is exactly one forwarding call: one block, one `Call` to
/// the target passing the adapter's parameters 1.. in order, optionally one
/// release of the environment token it was handed, and a `Return` of the
/// call's destinations. Anything else -- a second call, arithmetic, a
/// reordered argument, a different terminator -- refuses.
pub(super) fn adapter_body_forwards(
    adapter: &super::super::Function,
    target: super::super::FunctionId,
    target_signature: &super::super::FunctionSignature,
    owner_family: Option<super::super::FunctionSignatureId>,
) -> bool {
    let [block] = adapter.blocks.as_slice() else { return false };
    if block.id != adapter.entry {
        return false;
    }
    let parameters = block.parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
    let Some((environment, forwarded)) = parameters.split_first() else { return false };
    let (call, tail) = match block.operations.split_first() {
        Some((call, tail)) => (call, tail),
        None => return false,
    };
    let super::super::Operation::Call { destinations, callee, arguments } = call else { return false };
    if *callee != target || arguments.len() != forwarded.len() {
        return false;
    }
    // A reference lane is forwarded by moving the token; a scalar lane is
    // passed by value. The convention is the target's, not the adapter's.
    if !arguments.iter().zip(forwarded).enumerate().all(|(index, (argument, parameter))| {
        match (argument, target_signature.parameters.get(index)) {
            (super::super::CallArgument::Move(value), Some(super::super::ValueType::Ref(_) | super::super::ValueType::Gc(_))) => value == parameter,
            (super::super::CallArgument::Value(value), Some(value_type)) => {
                !matches!(value_type, super::super::ValueType::Ref(_)) && value == parameter
            }
            _ => false,
        }
    }) {
        return false;
    }
    // Exactly one release of the environment token the adapter was handed:
    // every call site clones the record for the callee, so an adapter that
    // kept its token would leak one reference per call.
    match tail {
        [super::super::Operation::Release { object } | super::super::Operation::DropRef { object }]
            if owner_family.is_none() && object == environment => {}
        [super::super::Operation::ClosureOwner(super::super::closure_owner_contract::OwnerOperation::Drop { owner, family })]
            if owner_family == Some(*family) && owner == environment => {}
        _ => return false,
    }
    matches!(&block.terminator, super::super::Terminator::Return { values } if values == destinations)
}

/// The candidate itself is not trusted to pair a lambda's code with its
/// environment.  The source witness ordinal fixes the nonzero tag and the
/// lambda must check that tag before it can load any capture payload.
pub(super) fn closure_environment_prefix_matches(
    function: &super::super::Function,
    environment_layout: crate::layout::LayoutId,
    expected_tag: Option<i64>,
    first_capture_field: Option<u32>,
    fields: &[Option<super::super::ValueType>],
) -> bool {
    let Some(expected_tag) = expected_tag else { return false };
    let Some(first_capture_field) = first_capture_field else { return false };
    let Some(block) = function.blocks.iter().find(|block| block.id == function.entry) else { return false };
    let Some(closure) = block.parameters.first().map(|parameter| parameter.value) else { return false };
    let Some((environment_load, tail)) = block.operations.split_first() else { return false };
    let super::super::Operation::Load { destination: environment, place, transfer } = environment_load else { return false };
    if place.object() != closure
        || !matches!(place.projection(), super::super::Projection::Field { index: 1 })
        || place.value_type() != super::super::ValueType::Ref(environment_layout)
        || place.access() != super::super::AccessMode::Read
        || *transfer != super::super::LoadTransfer::CloneShared
    {
        return false;
    }
    let Some((tag_load, tail)) = tail.split_first() else { return false };
    let super::super::Operation::Load { destination: tag, place, transfer } = tag_load else { return false };
    if place.object() != *environment
        || !matches!(place.projection(), super::super::Projection::Field { index: 0 })
        || place.value_type() != super::super::ValueType::Int
        || place.access() != super::super::AccessMode::Read
        || *transfer != super::super::LoadTransfer::CopyScalar
    {
        return false;
    }
    // Only a capture-free lambda can release the cloned environment before
    // its guard: no subsequent operation can load an environment payload.
    let tail = if fields.is_empty() {
        match tail.split_first() {
            Some((super::super::Operation::Release { object } | super::super::Operation::DropRef { object }, rest)) if *object == *environment => rest,
            _ => tail,
        }
    } else {
        tail
    };
    let Some((constant, tail)) = tail.split_first() else { return false };
    let super::super::Operation::ConstInt { destination: expected, value } = constant else { return false };
    if *value != expected_tag { return false; }
    let Some((comparison, tail)) = tail.split_first() else { return false };
    let super::super::Operation::Compare { destination: condition, operator, left, right } = comparison else { return false };
    if *operator != super::super::ComparisonOperator::Equal
        || !((*left == *tag && *right == *expected) || (*left == *expected && *right == *tag))
    {
        return false;
    }
    let Some((guard, capture_loads)) = tail.split_first() else { return false };
    if !matches!(guard, super::super::Operation::Guard { condition: guard_condition, kind: super::super::GuardKind::DynamicType } if *guard_condition == *condition) {
        return false;
    }
    let capture_start = if fields.is_empty() && matches!(block.operations.get(2), Some(super::super::Operation::Release { object } | super::super::Operation::DropRef { object }) if *object == *environment) { 6 } else { 5 };
    let captures_match = fields.iter().enumerate().all(|(offset, field)| {
        let Some(value_type) = field else { return false };
        let Some(super::super::Operation::Load { place, transfer, .. }) = capture_loads.get(offset) else { return false };
        place.object() == *environment
            && matches!(place.projection(), super::super::Projection::Field { index } if *index == first_capture_field + offset as u32)
            && place.value_type() == *value_type
            && place.access() == super::super::AccessMode::Read
            && match value_type {
                super::super::ValueType::Ref(_) => *transfer == super::super::LoadTransfer::ClonePresent,
                _ => *transfer == super::super::LoadTransfer::CopyScalar,
            }
    });
    captures_match
        && hidden_environment_uses_are_closed(function, closure, *environment, capture_start, fields.len())
}

/// The synthesized closure and environment parameters are compiler-private.
/// After the authenticated prefix, only their affine cleanup may mention them;
/// no alias, reload, call argument, branch argument, or later place can expose
/// an inactive family payload.
pub(super) fn hidden_environment_uses_are_closed(
    function: &super::super::Function,
    closure: super::super::ValueId,
    environment: super::super::ValueId,
    capture_start: usize,
    capture_count: usize,
) -> bool {
    for block in &function.blocks {
        if crate::canonical_safewir::check::terminator_uses(&block.terminator)
            .iter().any(|value| *value == closure || *value == environment)
        {
            return false;
        }
        for (index, operation) in block.operations.iter().enumerate() {
            let mentions_hidden = crate::canonical_safewir::check::operation_uses(operation)
                .iter().any(|value| *value == closure || *value == environment);
            if !mentions_hidden { continue; }
            let prefix_use = block.id == function.entry
                && (index == 0 || index == 1 || (capture_start..capture_start + capture_count).contains(&index));
            let cleanup = matches!(operation,
                super::super::Operation::Release { object } | super::super::Operation::DropRef { object }
                if *object == closure || *object == environment
            );
            if !prefix_use && !cleanup { return false; }
        }
    }
    true
}

/// The checked module's type definitions, keyed by bare and entry-qualified
/// names. A nominal scalar aggregate (a record, a sum type, a scalar
/// `Option`/`Result`) binds to a candidate reference whose layout carries
/// exactly the slots `witchy_canonical_source::aggregates` derives for it.
pub(super) type ScalarRecordFields = BTreeMap<String, witchy_syntax::ast::TypeDef>;

/// The only source form permitted to carry an exclusive result through a
/// record is a final `&mut parameter.named_string_field`.  This descriptor is
/// deliberately source-derived: it selects neither a candidate layout nor a
/// candidate contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProjectedExclusiveReturn {
    pub(super) parameter: usize,
    field: u32,
}

pub(super) fn projected_exclusive_return_source(
    declaration: &witchy_syntax::ast::Function,
    records: &ScalarRecordFields,
) -> Option<ProjectedExclusiveReturn> {
    use witchy_syntax::ast::{Expr, Stmt, Type, TypeQual, UnOp};

    let result = declaration.ret.as_ref()?;
    let (lifetime, result_referent) = direct_exclusive_reference_parts(result)?;
    if !is_projected_exclusive_leaf(result_referent) {
        return None;
    }
    let Some(Stmt::Expr(Expr::Unary { op: UnOp::BorrowMut, expr })) = declaration.body.stmts.last() else {
        return None;
    };
    let Expr::Field { base, field } = expr.as_ref() else { return None };
    let Expr::Var(owner_name) = base.as_ref() else { return None };
    let owners = declaration.params.iter().enumerate().filter_map(|(index, parameter)| {
        let Type::Qualified(TypeQual::BorrowMut(candidate_lifetime), owner) = parameter.ty.as_ref()? else {
            return None;
        };
        (parameter.convention == witchy_syntax::ast::Convention::Let
            && candidate_lifetime == lifetime
            && parameter.name == *owner_name
            && matches!(owner.unqualified(), Type::Named(_, arguments) if arguments.is_empty()))
            .then_some((index, owner.as_ref()))
    }).collect::<Vec<_>>();
    let [(parameter, owner)] = owners.as_slice() else { return None };
    let Type::Named(name, arguments) = owner.unqualified() else { return None };
    if !arguments.is_empty() { return None; }
    let definition = records.get(name).or_else(|| name.rsplit('.').next().and_then(|bare| records.get(bare)))?;
    if definition.packed || definition.variants.len() != 1 { return None; }
    let variant = &definition.variants[0];
    let field_index = variant.field_names.iter().position(|candidate| candidate == field)?;
    if !variant.fields.get(field_index).is_some_and(is_projected_exclusive_leaf) {
        return None;
    }
    Some(ProjectedExclusiveReturn {
        parameter: *parameter,
        field: u32::try_from(field_index).ok()?,
    })
}

pub(super) fn projected_exclusive_result_contract_matches(
    signature: &super::super::FunctionSignature,
    declaration: &witchy_syntax::ast::Function,
    records: &ScalarRecordFields,
) -> bool {
    let Some(expected) = projected_exclusive_return_source(declaration, records) else { return true };
    matches!(signature.contract.results.first(),
        Some(super::super::ResultContract::AliasesWriteBackParameter {
            parameter,
            projection: Some(field),
            ..
        }) if usize::try_from(*parameter).ok() == Some(expected.parameter) && *field == expected.field)
}
