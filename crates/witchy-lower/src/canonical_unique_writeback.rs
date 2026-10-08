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
