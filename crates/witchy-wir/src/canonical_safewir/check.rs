use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use super::{
    BasicBlock, BlockId, CallArgument, CanonicalModule, Function, FunctionId, FunctionSignature,
    FunctionSignatureId, InitialValid, LoweredCandidate, Operation, ParameterOwnership,
    ProperTailSite, TailSiteId, Terminator, ValueId, ValueType,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialCheckError {
    DuplicateSignature(FunctionSignatureId),
    DuplicateFunction(FunctionId),
    DuplicateBlock { function: FunctionId, block: BlockId },
    DuplicateValue { function: FunctionId, value: ValueId },
    MissingSignature(FunctionSignatureId),
    MissingFunction(FunctionId),
    MissingBlock { function: FunctionId, block: BlockId },
    MissingValueType { function: FunctionId, value: ValueId },
    UndeclaredValue { function: FunctionId, value: ValueId },
    TypeMismatch {
        function: FunctionId,
        value: ValueId,
        expected: ValueType,
        actual: ValueType,
    },
    ArityMismatch { context: &'static str, expected: usize, actual: usize },
    DuplicateTailObligation(TailSiteId),
    MissingTailObligation(TailSiteId),
    UnexpectedTailSite(TailSiteId),
    TailSiteFunctionMismatch {
        site: TailSiteId,
        expected: FunctionId,
        actual: FunctionId,
    },
    UnsupportedControlFlow { function: FunctionId },
    UnsupportedMemoryOperation { function: FunctionId, operation: &'static str },
    UnsupportedHostCall { function: FunctionId },
    UnsupportedBorrowArgument { function: FunctionId },
    UnsupportedMemorySignature { signature: FunctionSignatureId },
}

impl fmt::Display for InitialCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR initial validation failed: {self:?}")
    }
}

impl Error for InitialCheckError {}

/// Validate the currently implemented canonical v1 subset.
///
/// This first checker slice admits only single-block scalar functions. It is
/// intentionally useful for bringing up lowering and tail-site completeness,
/// while rejecting every memory-bearing operation until the corresponding
/// RFC-0165 judgment is implemented. Only success can mint `InitialValid`.
pub fn check_initial(
    candidate: CanonicalModule<LoweredCandidate>,
) -> Result<CanonicalModule<InitialValid>, InitialCheckError> {
    let signatures = index_signatures(candidate.signatures())?;
    let functions = index_functions(candidate.functions())?;
    let tail_obligations = index_tail_obligations(candidate.proper_tail_sites())?;
    let mut observed_tail_sites = BTreeSet::new();

    for function in candidate.functions() {
        check_function(
            function,
            &signatures,
            &functions,
            &tail_obligations,
            &mut observed_tail_sites,
        )?;
    }

    for site in tail_obligations.keys() {
        if !observed_tail_sites.contains(site) {
            return Err(InitialCheckError::MissingTailObligation(*site));
        }
    }

    Ok(candidate.into_initial_valid())
}

fn index_signatures(
    signatures: &[FunctionSignature],
) -> Result<BTreeMap<FunctionSignatureId, &FunctionSignature>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for signature in signatures {
        if result.insert(signature.id, signature).is_some() {
            return Err(InitialCheckError::DuplicateSignature(signature.id));
        }
        check_scalar_signature(signature)?;
    }
    Ok(result)
}

fn index_functions(
    functions: &[Function],
) -> Result<BTreeMap<FunctionId, &Function>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for function in functions {
        if result.insert(function.id, function).is_some() {
            return Err(InitialCheckError::DuplicateFunction(function.id));
        }
    }
    Ok(result)
}

fn index_tail_obligations(
    obligations: &[ProperTailSite],
) -> Result<BTreeMap<TailSiteId, FunctionId>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for obligation in obligations {
        if result.insert(obligation.id, obligation.function).is_some() {
            return Err(InitialCheckError::DuplicateTailObligation(obligation.id));
        }
    }
    Ok(result)
}

fn check_scalar_signature(signature: &FunctionSignature) -> Result<(), InitialCheckError> {
    if signature
        .parameters
        .iter()
        .chain(&signature.results)
        .any(ValueType::is_memory_bearing)
        || signature.contract.parameters.iter().any(|parameter| {
            parameter.value_type.is_memory_bearing()
                || parameter.ownership != ParameterOwnership::Value
                || parameter.non_escaping
        })
        || !signature.contract.parameter_effects.is_empty()
        || signature.contract.results.iter().any(|result| {
            !matches!(result, super::ResultContract::Value(value) if !value.is_memory_bearing())
        })
    {
        return Err(InitialCheckError::UnsupportedMemorySignature {
            signature: signature.id,
        });
    }
    Ok(())
}

fn check_function(
    function: &Function,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    functions: &BTreeMap<FunctionId, &Function>,
    tail_obligations: &BTreeMap<TailSiteId, FunctionId>,
    observed_tail_sites: &mut BTreeSet<TailSiteId>,
) -> Result<(), InitialCheckError> {
    let signature = signatures
        .get(&function.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(function.signature))?;

    if function.blocks.len() != 1 {
        return Err(InitialCheckError::UnsupportedControlFlow {
            function: function.id,
        });
    }
    let block = &function.blocks[0];
    if block.id != function.entry {
        return Err(InitialCheckError::MissingBlock {
            function: function.id,
            block: function.entry,
        });
    }

    check_entry_parameters(function, block, signature)?;
    check_value_definitions(function, block)?;

    for operation in &block.operations {
        check_operation(function, operation, signatures, functions)?;
    }
    check_terminator(
        function,
        &block.terminator,
        signature,
        signatures,
        functions,
        tail_obligations,
        observed_tail_sites,
    )
}

fn check_entry_parameters(
    function: &Function,
    block: &BasicBlock,
    signature: &FunctionSignature,
) -> Result<(), InitialCheckError> {
    require_arity("entry parameters", signature.parameters.len(), block.parameters.len())?;
    for (parameter, expected) in block.parameters.iter().zip(&signature.parameters) {
        require_type(function, parameter.value, *expected)?;
        if parameter.value_type != *expected {
            return Err(InitialCheckError::TypeMismatch {
                function: function.id,
                value: parameter.value,
                expected: *expected,
                actual: parameter.value_type,
            });
        }
        if parameter.ownership.is_some() {
            return Err(InitialCheckError::UnsupportedMemorySignature {
                signature: signature.id,
            });
        }
    }
    Ok(())
}

fn check_value_definitions(
    function: &Function,
    block: &BasicBlock,
) -> Result<(), InitialCheckError> {
    let mut definitions = BTreeSet::new();
    for parameter in &block.parameters {
        insert_definition(function.id, &mut definitions, parameter.value)?;
    }
    for operation in &block.operations {
        for destination in operation_destinations(operation) {
            insert_definition(function.id, &mut definitions, destination)?;
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
        Operation::ConstInt { destination, .. }
        | Operation::ConstFloat { destination, .. }
        | Operation::ConstBool { destination, .. }
        | Operation::ConstSize { destination, .. }
        | Operation::Arithmetic { destination, .. }
        | Operation::Compare { destination, .. }
        | Operation::NarrowSize { destination, .. }
        | Operation::Alloc { destination, .. }
        | Operation::StaticData { destination, .. }
        | Operation::Load { destination, .. }
        | Operation::MoveValue { destination, .. }
        | Operation::Share { destination, .. } => vec![*destination],
        Operation::Call { destinations, .. }
        | Operation::VarCall { destinations, .. }
        | Operation::HostCall { destinations, .. } => destinations.clone(),
        Operation::Guard { .. }
        | Operation::Store { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::UpdateCount { .. }
        | Operation::Retain { .. }
        | Operation::Release { .. }
        | Operation::Reclaim { .. } => Vec::new(),
    }
}

fn check_operation(
    function: &Function,
    operation: &Operation,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    functions: &BTreeMap<FunctionId, &Function>,
) -> Result<(), InitialCheckError> {
    match operation {
        Operation::ConstInt { destination, .. } => {
            require_type(function, *destination, ValueType::Int)
        }
        Operation::ConstFloat { destination, .. } => {
            require_type(function, *destination, ValueType::Float)
        }
        Operation::ConstBool { destination, .. } => {
            require_type(function, *destination, ValueType::Bool)
        }
        Operation::ConstSize { destination, .. } => {
            require_type(function, *destination, ValueType::Size)
        }
        Operation::Arithmetic { destination, left, right, .. } => {
            let result = value_type(function, *destination)?;
            if !matches!(result, ValueType::Int | ValueType::Size) {
                return Err(type_mismatch(function, *destination, ValueType::Int, result));
            }
            require_type(function, *left, result)?;
            require_type(function, *right, result)
        }
        Operation::Compare { destination, left, right, .. } => {
            require_type(function, *destination, ValueType::Bool)?;
            let left_type = value_type(function, *left)?;
            require_type(function, *right, left_type)
        }
        Operation::NarrowSize { destination, value } => {
            require_type(function, *destination, ValueType::Size)?;
            require_type(function, *value, ValueType::Int)
        }
        Operation::Guard { condition, .. } => require_type(function, *condition, ValueType::Bool),
        Operation::MoveValue { destination, source } => {
            let source_type = value_type(function, *source)?;
            if source_type.is_memory_bearing() {
                return unsupported_memory(function, "MoveValue");
            }
            require_type(function, *destination, source_type)
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
            check_call(function, destinations, arguments, signature)
        }
        Operation::VarCall { destinations, callee, signature, arguments } => {
            require_type(function, *callee, ValueType::Callable(*signature))?;
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            check_call(function, destinations, arguments, signature)
        }
        Operation::HostCall { .. } => Err(InitialCheckError::UnsupportedHostCall {
            function: function.id,
        }),
        Operation::Alloc { .. } => unsupported_memory(function, "Alloc"),
        Operation::StaticData { .. } => unsupported_memory(function, "StaticData"),
        Operation::Load { .. } => unsupported_memory(function, "Load"),
        Operation::Store { .. } => unsupported_memory(function, "Store"),
        Operation::CopyBytes { .. } => unsupported_memory(function, "CopyBytes"),
        Operation::MoveRange { .. } => unsupported_memory(function, "MoveRange"),
        Operation::CloneRange { .. } => unsupported_memory(function, "CloneRange"),
        Operation::UpdateCount { .. } => unsupported_memory(function, "UpdateCount"),
        Operation::Share { .. } => unsupported_memory(function, "Share"),
        Operation::Retain { .. } => unsupported_memory(function, "Retain"),
        Operation::Release { .. } => unsupported_memory(function, "Release"),
        Operation::Reclaim { .. } => unsupported_memory(function, "Reclaim"),
    }
}

fn check_call(
    function: &Function,
    destinations: &[ValueId],
    arguments: &[CallArgument],
    signature: &FunctionSignature,
) -> Result<(), InitialCheckError> {
    require_arity("call arguments", signature.parameters.len(), arguments.len())?;
    require_arity("call results", signature.results.len(), destinations.len())?;
    for (argument, expected) in arguments.iter().zip(&signature.parameters) {
        let value = match argument {
            CallArgument::Value(value) => *value,
            CallArgument::Move(_) | CallArgument::Share(_) | CallArgument::Borrow(_) => {
                return Err(InitialCheckError::UnsupportedBorrowArgument {
                    function: function.id,
                });
            }
        };
        require_type(function, value, *expected)?;
    }
    for (destination, expected) in destinations.iter().zip(&signature.results) {
        require_type(function, *destination, *expected)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn check_terminator(
    function: &Function,
    terminator: &Terminator,
    current_signature: &FunctionSignature,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    functions: &BTreeMap<FunctionId, &Function>,
    tail_obligations: &BTreeMap<TailSiteId, FunctionId>,
    observed_tail_sites: &mut BTreeSet<TailSiteId>,
) -> Result<(), InitialCheckError> {
    match terminator {
        Terminator::Return { values } => {
            require_arity("return values", current_signature.results.len(), values.len())?;
            for (value, expected) in values.iter().zip(&current_signature.results) {
                require_type(function, *value, *expected)?;
            }
            Ok(())
        }
        Terminator::TailCall { site, callee, arguments } => {
            observe_tail_site(function, *site, tail_obligations, observed_tail_sites)?;
            let callee = functions
                .get(callee)
                .copied()
                .ok_or(InitialCheckError::MissingFunction(*callee))?;
            let signature = signatures
                .get(&callee.signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(callee.signature))?;
            check_tail_call(function, arguments, signature, current_signature)
        }
        Terminator::VarTailCall { site, callee, signature, arguments } => {
            observe_tail_site(function, *site, tail_obligations, observed_tail_sites)?;
            require_type(function, *callee, ValueType::Callable(*signature))?;
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            check_tail_call(function, arguments, signature, current_signature)
        }
        Terminator::Trap | Terminator::Unreachable => Ok(()),
        Terminator::Branch { .. } | Terminator::BranchIf { .. } => {
            Err(InitialCheckError::UnsupportedControlFlow {
                function: function.id,
            })
        }
    }
}

fn check_tail_call(
    function: &Function,
    arguments: &[CallArgument],
    callee: &FunctionSignature,
    caller: &FunctionSignature,
) -> Result<(), InitialCheckError> {
    if callee.results != caller.results {
        return Err(InitialCheckError::ArityMismatch {
            context: "tail-call result signature",
            expected: caller.results.len(),
            actual: callee.results.len(),
        });
    }
    check_call(function, &[], arguments, &FunctionSignature {
        id: callee.id,
        parameters: callee.parameters.clone(),
        results: Vec::new(),
        contract: callee.contract.clone(),
    })
}

fn observe_tail_site(
    function: &Function,
    site: TailSiteId,
    obligations: &BTreeMap<TailSiteId, FunctionId>,
    observed: &mut BTreeSet<TailSiteId>,
) -> Result<(), InitialCheckError> {
    let expected = obligations
        .get(&site)
        .copied()
        .ok_or(InitialCheckError::UnexpectedTailSite(site))?;
    if expected != function.id {
        return Err(InitialCheckError::TailSiteFunctionMismatch {
            site,
            expected,
            actual: function.id,
        });
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
        BasicBlock, BlockId, CallableContract, CanonicalModule, CheckedSourceIdentity, Function,
        FunctionId, FunctionSignature, FunctionSignatureId, LoweredModuleParts, Operation,
        ProperTailSite, ResultContract, Revision, TailSiteId, Terminator, ValueId, ValueType,
    };
    use super::{InitialCheckError, check_initial};

    const FUNCTION: FunctionId = FunctionId::new(0);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
    const ENTRY: BlockId = BlockId::new(0);
    const VALUE: ValueId = ValueId::new(0);
    const TAIL_SITE: TailSiteId = TailSiteId::new(0);

    fn contract(results: Vec<ResultContract>) -> CallableContract {
        CallableContract {
            parameters: Vec::new(),
            results,
            parameter_effects: BTreeMap::new(),
        }
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
                function: FUNCTION,
            }],
        );

        assert!(check_initial(module).is_ok());
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
                function: FUNCTION,
            }],
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::MissingTailObligation(TAIL_SITE)
        );
    }

    #[test]
    fn unimplemented_memory_judgment_fails_closed() {
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
            InitialCheckError::UnsupportedMemoryOperation {
                function: FUNCTION,
                operation: "Reclaim",
            }
        );
    }
}
