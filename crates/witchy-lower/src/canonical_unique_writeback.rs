use super::*;

pub(super) fn specialize(signature: &mut FunctionSignature, source: &Type, environment: usize) {
    let Type::Fn(parameters, _, conventions, _) = source.unqualified() else { return };
    let writebacks = conventions.iter().filter(|convention| **convention == Convention::Var).count();
    let mut result = signature.results.len() - writebacks;
    for (index, parameter) in parameters.iter().enumerate() {
        if conventions.get(index) != Some(&Convention::Var) { continue; }
        if witchy_canonical_source::unique_writeback::scalar_list(parameter)
            && let ValueType::Ref(layout) = signature.parameters[index + environment]
        {
            signature.contract.parameters[index + environment].ownership = ParameterOwnership::UniqueWriteBack;
            signature.contract.results[result] = ResultContract::Unique { layout };
            let effects = signature.contract.parameter_effects.entry((index + environment) as u32).or_default();
            if !effects.contains(&MemoryEffect::Write) { effects.push(MemoryEffect::Write); }
        }
        result += 1;
    }
}

pub(super) fn specialize_closure(
    signatures: &mut Vec<FunctionSignature>,
    declaration: FunctionSignatureId,
    body: FunctionSignatureId,
    source: &Type,
) -> Result<FunctionSignatureId, CanonicalLoweringError> {
    // Type-only signatures authorize targetless Shared calls, so actual closure families need separate physical declarations.
    let mut specialized = signatures[declaration.get() as usize].clone();
    specialize(&mut specialized, source, 0);
    let declaration = if specialized == signatures[declaration.get() as usize] {
        declaration
    } else {
        specialized.id = FunctionSignatureId::new(u32::try_from(signatures.len())
            .map_err(|_| CanonicalLoweringError::IdOverflow)?);
        let declaration = specialized.id;
        signatures.push(specialized.clone());
        declaration
    };
    align_closure_signature(&mut signatures[body.get() as usize], &specialized)?;
    Ok(declaration)
}
