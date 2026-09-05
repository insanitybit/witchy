use std::collections::BTreeMap;

use super::super::{
    CanonicalAbiCatalog, Function, FunctionSignature, FunctionSignatureId, HostBufferExtent,
    HostCallArgument, HostMutation, HostParameterRole, HostReentrancy, HostResultRole,
    ImportContract, ImportContractIdentity, ImportId, ValueId, ValueType,
};
use super::InitialCheckError;

pub(super) fn check_imports(
    imports: &BTreeMap<ImportId, &ImportContract>,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    catalog: Option<&CanonicalAbiCatalog>,
) -> Result<(), InitialCheckError> {
    if !imports.is_empty() && catalog.is_none() {
        return Err(InitialCheckError::MissingCanonicalAbiCatalog);
    }
    for import in imports.values() {
        let Some(catalog) = catalog else { continue };
        let expected = catalog.imports.get(&import.id).ok_or(
            InitialCheckError::InvalidCanonicalAbiContract { import: import.id },
        )?;
        if **import != *expected {
            return Err(InitialCheckError::InvalidCanonicalAbiContract { import: import.id });
        }
        check_import(import, signatures)?;
    }
    Ok(())
}

pub(super) fn check_call(
    function: &Function,
    import_id: ImportId,
    destinations: &[ValueId],
    arguments: &[HostCallArgument],
    imports: &BTreeMap<ImportId, &ImportContract>,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    catalog: Option<&CanonicalAbiCatalog>,
) -> Result<(), InitialCheckError> {
    let import = imports.get(&import_id).copied().ok_or(
        InitialCheckError::MissingImportContract {
            function: function.id,
            import: import_id,
        },
    )?;
    let Some(catalog) = catalog else {
        return Err(InitialCheckError::MissingCanonicalAbiCatalog);
    };
    let expected = catalog.imports.get(&import.id).ok_or(
        InitialCheckError::InvalidCanonicalAbiContract { import: import.id },
    )?;
    if *import != *expected {
        return Err(InitialCheckError::InvalidCanonicalAbiContract { import: import.id });
    }
    check_import(import, signatures)?;
    require_arity(
        "host call arguments",
        import.parameter_roles.len(),
        arguments.len(),
    )?;
    require_arity(
        "host call results",
        import.result_roles.len(),
        destinations.len(),
    )?;

    for (index, (role, argument)) in import
        .parameter_roles
        .iter()
        .zip(arguments)
        .enumerate()
    {
        check_argument(function, import_id, index, role, argument)?;
    }
    for (index, (role, destination)) in import
        .result_roles
        .iter()
        .zip(destinations)
        .enumerate()
    {
        let expected = result_type(role);
        let actual = function.value_types.get(destination).copied().ok_or(
            InitialCheckError::MissingValueType {
                function: function.id,
                value: *destination,
            },
        )?;
        if actual != expected {
            return Err(InitialCheckError::HostResultRoleMismatch {
                function: function.id,
                import: import_id,
                result: index,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn check_import(
    import: &ImportContract,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
) -> Result<(), InitialCheckError> {
    if matches!(import.abi_identity, ImportContractIdentity::Unknown) {
        return Err(InitialCheckError::UnknownImportIdentity { import: import.id });
    }
    let signature = signatures
        .get(&import.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(import.signature))?;
    require_arity(
        "host contract parameters",
        signature.parameters.len(),
        import.parameter_roles.len(),
    )?;
    require_arity(
        "host contract results",
        signature.results.len(),
        import.result_roles.len(),
    )?;
    if !matches!(import.reentrancy, HostReentrancy::Forbidden) {
        return Err(InitialCheckError::UnsupportedHostReentrancy { import: import.id });
    }
    if matches!(import.mutation, HostMutation::Unknown) {
        return Err(InitialCheckError::UnsupportedHostMutation { import: import.id });
    }
    let has_write_buffer = import.parameter_roles.iter().any(|role| {
        matches!(
            role,
            HostParameterRole::Buffer {
                access: super::super::AccessMode::Write,
                ..
            }
        )
    });
    match import.mutation {
        HostMutation::ReadOnly if has_write_buffer => {
            return Err(InitialCheckError::UnsupportedHostMutation { import: import.id });
        }
        HostMutation::WriteBuffers if !has_write_buffer => {
            return Err(InitialCheckError::UnsupportedHostMutation { import: import.id });
        }
        HostMutation::ReadOnly | HostMutation::WriteBuffers | HostMutation::Unknown => {}
    }

    for (role, expected) in import.parameter_roles.iter().zip(&signature.parameters) {
        if role_type(role) != *expected
            || matches!(role, HostParameterRole::Value(value_type) if is_reference_like(*value_type))
        {
            return Err(InitialCheckError::HostContractRoleMismatch { import: import.id });
        }
        if let HostParameterRole::Buffer { extent, .. } = role {
            if !matches!(role_type(role), ValueType::Ref(_)) {
                return Err(InitialCheckError::HostContractRoleMismatch { import: import.id });
            }
            if matches!(extent, HostBufferExtent::Unknown) {
                return Err(InitialCheckError::UnknownHostBufferExtent { import: import.id });
            }
        }
    }
    for (role, expected) in import.result_roles.iter().zip(&signature.results) {
        if result_type(role) != *expected
            || matches!(role, HostResultRole::Value(value_type) if is_reference_like(*value_type))
        {
            return Err(InitialCheckError::HostContractRoleMismatch { import: import.id });
        }
    }
    Ok(())
}

fn check_argument(
    function: &Function,
    import: ImportId,
    parameter: usize,
    role: &HostParameterRole,
    argument: &HostCallArgument,
) -> Result<(), InitialCheckError> {
    match (role, argument) {
        (HostParameterRole::Value(expected), HostCallArgument::Value(value)) => {
            let actual = value_type(function, *value)?;
            if actual != *expected || is_reference_like(actual) {
                return Err(InitialCheckError::HostParameterRoleMismatch {
                    function: function.id,
                    import,
                    parameter,
                });
            }
        }
        (HostParameterRole::Capability(kind), HostCallArgument::Capability(value)) => {
            require_type(function, *value, ValueType::Capability(*kind))?;
        }
        (
            HostParameterRole::Buffer {
                value_type,
                access,
                extent,
            },
            HostCallArgument::Buffer {
                place,
                extent: supplied_extent,
            },
        ) => {
            if place.value_type() != *value_type
                || place.access() != *access
                || supplied_extent != extent
            {
                return Err(InitialCheckError::HostBufferExtentMismatch {
                    function: function.id,
                    import,
                    parameter,
                });
            }
        }
        _ => {
            let actual = match argument {
                HostCallArgument::Value(value) | HostCallArgument::Capability(value) => {
                    value_type(function, *value)?
                }
                HostCallArgument::Buffer { place, .. } => place.value_type(),
            };
            let expected = role_type(role);
            let value = match argument {
                HostCallArgument::Value(value) | HostCallArgument::Capability(value) => *value,
                HostCallArgument::Buffer { place, .. } => place.object(),
            };
            return Err(InitialCheckError::TypeMismatch {
                function: function.id,
                value,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn role_type(role: &HostParameterRole) -> ValueType {
    match role {
        HostParameterRole::Value(value_type) => *value_type,
        HostParameterRole::Capability(kind) => ValueType::Capability(*kind),
        HostParameterRole::Buffer { value_type, .. } => *value_type,
    }
}

fn result_type(role: &HostResultRole) -> ValueType {
    match role {
        HostResultRole::Value(value_type) => *value_type,
        HostResultRole::Capability(kind) => ValueType::Capability(*kind),
    }
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
        return Err(InitialCheckError::TypeMismatch {
            function: function.id,
            value,
            expected,
            actual,
        });
    }
    Ok(())
}

fn is_reference_like(value_type: ValueType) -> bool {
    matches!(value_type, ValueType::Ref(_) | ValueType::Gc(_) | ValueType::Callable(_) | ValueType::Capability(_))
}

fn require_arity(
    context: &'static str,
    expected: usize,
    actual: usize,
) -> Result<(), InitialCheckError> {
    if expected != actual {
        return Err(InitialCheckError::ArityMismatch { context, expected, actual });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::super::{
        AccessMode, BasicBlock, BlockId, CallableContract, Function, FunctionId,
        FunctionSignature, FunctionSignatureId, GcTypeId, HostBufferExtent, HostCallArgument,
        HostMutation, HostParameterRole, HostReentrancy, ImportContract, ImportContractIdentity,
        ImportId, LayoutId, Place, Projection, ValueId, ValueType, CapabilityKindId,
    };
    use super::{check_call, check_imports};

    const IMPORT: ImportId = ImportId::new(7);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(11);
    const VALUE: ValueId = ValueId::new(3);

    fn function(value_type: ValueType) -> Function {
        Function {
            id: FunctionId::new(1),
            signature: SIGNATURE,
            entry: BlockId::new(0),
            value_types: [(VALUE, value_type)].into_iter().collect(),
            blocks: Vec::<BasicBlock>::new(),
        }
    }

    fn import(role: HostParameterRole) -> ImportContract {
        ImportContract {
            id: IMPORT,
            signature: SIGNATURE,
            abi_identity: ImportContractIdentity::AuthenticatedAbi([4; 32]),
            module: "test".to_string(),
            name: "host".to_string(),
            parameter_roles: vec![role],
            result_roles: Vec::new(),
            mutation: HostMutation::ReadOnly,
            reentrancy: HostReentrancy::Forbidden,
        }
    }

    fn signature(role: &HostParameterRole) -> FunctionSignature {
        FunctionSignature {
            id: SIGNATURE,
            parameters: vec![role_type(role)],
            results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(),
                results: Vec::new(),
                parameter_effects: BTreeMap::new(),
            },
        }
    }

    fn role_type(role: &HostParameterRole) -> ValueType {
        match role {
            HostParameterRole::Value(value_type) => *value_type,
            HostParameterRole::Capability(kind) => ValueType::Capability(*kind),
            HostParameterRole::Buffer { value_type, .. } => *value_type,
        }
    }

    fn check(
        value_type: ValueType,
        role: HostParameterRole,
        argument: HostCallArgument,
    ) -> Result<(), super::InitialCheckError> {
        let import = import(role.clone());
        let signature = signature(&role);
        let catalog = super::super::super::CanonicalAbiCatalog::from_imports(vec![import.clone()]);
        let imports = [(IMPORT, &import)].into_iter().collect();
        let signatures = [(SIGNATURE, &signature)].into_iter().collect();
        check_call(
            &function(value_type),
            IMPORT,
            &[],
            &[argument],
            &imports,
            &signatures,
            Some(&catalog),
        )
    }

    #[test]
    fn unknown_import_id_is_rejected() {
        let function = function(ValueType::Int);
        let catalog = super::super::super::CanonicalAbiCatalog::from_imports(Vec::new());
        let imports = BTreeMap::new();
        let signatures = BTreeMap::new();
        assert!(matches!(
            check_call(
                &function,
                ImportId::new(99),
                &[],
                &[],
                &imports,
                &signatures,
                Some(&catalog),
            ),
            Err(super::InitialCheckError::MissingImportContract { .. })
        ));
    }

    #[test]
    fn forged_import_contract_is_rejected_by_catalog() {
        let role = HostParameterRole::Value(ValueType::Int);
        let mut forged = import(role.clone());
        forged.abi_identity = ImportContractIdentity::AuthenticatedAbi([9; 32]);
        let catalog = super::super::super::CanonicalAbiCatalog::from_imports(vec![import(role.clone())]);
        let signature = signature(&role);
        let imports = [(IMPORT, &forged)].into_iter().collect();
        let signatures = [(SIGNATURE, &signature)].into_iter().collect();
        assert!(matches!(
            check_imports(&imports, &signatures, Some(&catalog)),
            Err(super::InitialCheckError::InvalidCanonicalAbiContract { .. })
        ));
    }

    #[test]
    fn host_argument_role_mismatch_is_rejected() {
        assert!(matches!(
            check(
                ValueType::Capability(CapabilityKindId::new(1)),
                HostParameterRole::Value(ValueType::Int),
                HostCallArgument::Capability(VALUE),
            ),
            Err(super::InitialCheckError::TypeMismatch { .. })
                | Err(super::InitialCheckError::HostParameterRoleMismatch { .. })
        ));
    }

    #[test]
    fn integer_cannot_impersonate_a_capability_argument() {
        let kind = CapabilityKindId::new(1);
        assert!(matches!(
            check(
                ValueType::Int,
                HostParameterRole::Capability(kind),
                HostCallArgument::Capability(VALUE),
            ),
            Err(super::InitialCheckError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn linear_memory_reference_cannot_impersonate_a_capability_argument() {
        let kind = CapabilityKindId::new(1);
        assert!(matches!(
            check(
                ValueType::Ref(LayoutId::from_bytes([3; 32])),
                HostParameterRole::Capability(kind),
                HostCallArgument::Capability(VALUE),
            ),
            Err(super::InitialCheckError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn host_buffer_extent_mismatch_is_rejected() {
        let layout = LayoutId::from_bytes([2; 32]);
        let role = HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout),
            access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let place = Place::projected(
            VALUE,
            Projection::Slice { start: VALUE, length: VALUE },
            ValueType::Ref(layout),
            AccessMode::Read,
        );
        assert!(matches!(
            check(
                ValueType::Ref(layout),
                role,
                HostCallArgument::Buffer { place, extent: HostBufferExtent::Exact(4) },
            ),
            Err(super::InitialCheckError::HostBufferExtentMismatch { .. })
        ));
    }

    #[test]
    fn non_linear_memory_buffer_cannot_use_raw_slice_authority() {
        let role = HostParameterRole::Buffer {
            value_type: ValueType::Gc(GcTypeId::new(1)),
            access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let place = Place::projected(
            VALUE,
            Projection::Slice { start: VALUE, length: VALUE },
            ValueType::Gc(GcTypeId::new(1)),
            AccessMode::Read,
        );
        assert!(matches!(
            check(ValueType::Gc(GcTypeId::new(1)), role, HostCallArgument::Buffer {
                place,
                extent: HostBufferExtent::Slice,
            }),
            Err(super::InitialCheckError::HostContractRoleMismatch { .. })
        ));
    }

    #[test]
    fn int_or_ref_cannot_fill_capability_role() {
        let kind = super::super::super::CapabilityKindId::new(4);
        let role = HostParameterRole::Capability(kind);
        for actual in [ValueType::Int, ValueType::Ref(LayoutId::from_bytes([3; 32]))] {
            assert!(matches!(
                check(actual, role.clone(), HostCallArgument::Value(VALUE)),
                Err(super::InitialCheckError::TypeMismatch { .. })
            ));
        }
    }

    #[test]
    fn capability_cannot_be_declared_as_a_generic_host_value() {
        let capability = ValueType::Capability(CapabilityKindId::new(4));
        assert!(matches!(
            check(capability, HostParameterRole::Value(capability), HostCallArgument::Value(VALUE)),
            Err(super::InitialCheckError::HostContractRoleMismatch { .. })
        ));
        check(capability, HostParameterRole::Capability(CapabilityKindId::new(4)), HostCallArgument::Capability(VALUE))
            .expect("the dedicated capability role remains supported");
    }

    #[test]
    fn reference_results_cannot_be_declared_as_generic_host_values() {
        for value_type in [
            ValueType::Capability(CapabilityKindId::new(4)),
            ValueType::Ref(LayoutId::from_bytes([3; 32])),
            ValueType::Gc(GcTypeId::new(1)),
            ValueType::Callable(SIGNATURE),
        ] {
            let role = HostParameterRole::Value(ValueType::Int);
            let mut import = import(role.clone());
            import.result_roles = vec![super::super::super::HostResultRole::Value(value_type)];
            let mut signature = signature(&role);
            signature.results = vec![value_type];
            let catalog = super::super::super::CanonicalAbiCatalog::from_imports(vec![import.clone()]);
            let imports = [(IMPORT, &import)].into_iter().collect();
            let signatures = [(SIGNATURE, &signature)].into_iter().collect();
            assert!(matches!(
                check_imports(&imports, &signatures, Some(&catalog)),
                Err(super::InitialCheckError::HostContractRoleMismatch { .. })
            ));
        }
    }
}
