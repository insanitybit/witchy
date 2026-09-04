use std::collections::BTreeMap;

use super::super::{
    Function, FunctionSignature, FunctionSignatureId, HostBufferExtent,
    HostCallArgument, HostMutation, HostParameterRole, HostReentrancy, HostResultRole,
    ImportContract, ImportContractIdentity, ImportId, ValueId, ValueType,
};
use super::InitialCheckError;

pub(super) fn check_imports(
    imports: &BTreeMap<ImportId, &ImportContract>,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
) -> Result<(), InitialCheckError> {
    for import in imports.values() {
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
) -> Result<(), InitialCheckError> {
    let import = imports.get(&import_id).copied().ok_or(
        InitialCheckError::MissingImportContract {
            function: function.id,
            import: import_id,
        },
    )?;
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

    for (role, expected) in import.parameter_roles.iter().zip(&signature.parameters) {
        if role_type(role) != *expected {
            return Err(InitialCheckError::HostContractRoleMismatch { import: import.id });
        }
        if let HostParameterRole::Buffer { extent, .. } = role {
            if matches!(extent, HostBufferExtent::Unknown) {
                return Err(InitialCheckError::UnknownHostBufferExtent { import: import.id });
            }
        }
    }
    for (role, expected) in import.result_roles.iter().zip(&signature.results) {
        if result_type(role) != *expected {
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
                extent: expected_extent,
            },
            HostCallArgument::Buffer { place, extent },
        ) => {
            if place.value_type() != *value_type || place.access() != *access {
                return Err(InitialCheckError::HostParameterRoleMismatch {
                    function: function.id,
                    import,
                    parameter,
                });
            }
            if !extent_satisfies(expected_extent, extent) {
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
    matches!(value_type, ValueType::Ref(_) | ValueType::Gc(_) | ValueType::Callable(_))
}

fn extent_satisfies(expected: &HostBufferExtent, actual: &HostBufferExtent) -> bool {
    match (expected, actual) {
        (HostBufferExtent::Exact(expected), HostBufferExtent::Exact(actual)) => expected == actual,
        (HostBufferExtent::AtLeast(expected), HostBufferExtent::Exact(actual))
        | (HostBufferExtent::AtLeast(expected), HostBufferExtent::AtLeast(actual)) => actual >= expected,
        _ => false,
    }
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
