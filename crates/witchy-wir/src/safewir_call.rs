//! Closed-world callable-contract recomputation for SafeWIR.
//!
//! Candidate contracts are checked against final bodies. Local summaries are
//! solved together, including recursive components, before transfer validation
//! consumes them. Imports are accepted only through the fixed catalog below;
//! attaching a contract to an arbitrary import name grants no authority.

use std::collections::{BTreeMap, BTreeSet};

use crate::layout::{LayoutId, LayoutInterner, LayoutKind, ScalarKind};
use crate::safewir::{
    CallableMemoryContract, CallableMemoryResult, ContractExtent, ContractProjection,
    ContractSizeExpr, DefinitionId, Extent, ObjectIdentity, ParameterMemoryContract,
    ParameterMemoryEffect, ParameterMemoryEffects, ParameterMemoryEntry, RuleId, SizeExpr,
};
use crate::safewir_validate::{
    CallArgument, CandidateFunction, CandidateModule, HostArgument, Operation, ValidationBudget,
    ValidationDiagnostic, ValidationError,
};

#[derive(Debug, Clone)]
pub(crate) struct CheckedCallableContracts {
    local: BTreeMap<String, CallableMemoryContract>,
    imports: BTreeMap<String, CallableMemoryContract>,
    host_imports: BTreeMap<String, CallableMemoryContract>,
}

impl CheckedCallableContracts {
    pub(crate) fn ordinary(&self, name: &str) -> Option<&CallableMemoryContract> {
        self.local.get(name).or_else(|| self.imports.get(name))
    }

    pub(crate) fn host(&self, name: &str) -> Option<&CallableMemoryContract> {
        self.host_imports.get(name)
    }

    pub(crate) fn indirect(&self, arity: usize) -> CallableMemoryContract {
        let mut candidates = self
            .local
            .values()
            .chain(self.imports.values())
            .filter(|contract| contract.parameters.len() == arity);
        let Some(first) = candidates.next() else {
            return CallableMemoryContract::unknown(arity);
        };
        candidates.fold(first.clone(), |joined, contract| {
            join_contracts(joined, contract.clone())
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InferredResult {
    Bottom,
    Known(CallableMemoryResult),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InferredContract {
    effects: Vec<ParameterMemoryEffects>,
    result: InferredResult,
}

impl InferredContract {
    fn bottom(parameter_count: usize) -> Self {
        Self {
            effects: vec![ParameterMemoryEffects::none(); parameter_count],
            result: InferredResult::Bottom,
        }
    }

    fn with_entries(&self, declared: &CallableMemoryContract) -> CallableMemoryContract {
        CallableMemoryContract {
            parameters: declared
                .parameters
                .iter()
                .zip(&self.effects)
                .map(|(parameter, effects)| ParameterMemoryContract {
                    entry: parameter.entry,
                    layout: parameter.layout,
                    extent: parameter.extent.clone(),
                    effects: *effects,
                })
                .collect(),
            result: match &self.result {
                InferredResult::Bottom => CallableMemoryResult::Unknown,
                InferredResult::Known(result) => result.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    Bottom,
    Fresh {
        site: ObjectIdentity,
        layout: LayoutId,
        extent: ContractExtent,
    },
    Aliases(BTreeMap<usize, ContractProjection>),
    Unknown(BTreeSet<usize>),
}

impl Origin {
    fn source_parameters(&self) -> BTreeSet<usize> {
        match self {
            Self::Aliases(parameters) => parameters.keys().copied().collect(),
            Self::Unknown(parameters) => parameters.clone(),
            Self::Bottom | Self::Fresh { .. } => BTreeSet::new(),
        }
    }

    fn project(self, projection: &ContractProjection) -> Self {
        match self {
            Self::Aliases(parameters) => Self::Aliases(
                parameters
                    .into_iter()
                    .map(|(parameter, prior)| (parameter, prior.projected(projection)))
                    .collect(),
            ),
            Self::Bottom => Self::Bottom,
            Self::Fresh { .. } => Self::Unknown(BTreeSet::new()),
            Self::Unknown(parameters) => Self::Unknown(parameters),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct BodyFacts {
    objects: BTreeMap<ObjectIdentity, Origin>,
    scalar_parameters: BTreeMap<DefinitionId, usize>,
    scalar_constants: BTreeMap<DefinitionId, u64>,
    effects: Vec<ParameterMemoryEffects>,
    returned: InferredResult,
    saw_object_return: bool,
}

pub(crate) fn check_callable_contracts(
    module: &CandidateModule,
    layouts: &LayoutInterner,
    budget: ValidationBudget,
) -> Result<CheckedCallableContracts, ValidationError> {
    let imports = authenticate_catalog(module, layouts, false)?;
    let host_imports = authenticate_catalog(module, layouts, true)?;
    let mut inferred = module
        .functions
        .iter()
        .map(|function| {
            (
                function.name.clone(),
                InferredContract::bottom(function.parameters.len()),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let round_limit = module
        .functions
        .len()
        .checked_mul(8)
        .and_then(|rounds| rounds.checked_add(2))
        .ok_or_else(|| module_error(RuleId::BudgetExhaustionFailsClosed, "call fixpoint bound overflowed"))?;
    let mut work = 0_usize;
    let mut converged = false;
    for _ in 0..round_limit {
        let prior = inferred.clone();
        let mut changed = false;
        for function in &module.functions {
            let next = infer_function(
                function,
                &prior,
                &imports,
                &host_imports,
                budget,
                &mut work,
            )?;
            if prior.get(&function.name) != Some(&next) {
                inferred.insert(function.name.clone(), next);
                changed = true;
            }
        }
        if !changed {
            converged = true;
            break;
        }
    }
    if !converged {
        return Err(module_error(
            RuleId::BudgetExhaustionFailsClosed,
            "recursive callable components did not converge within their deterministic bound",
        ));
    }

    let mut local = BTreeMap::new();
    for function in &module.functions {
        let inferred = inferred
            .get(&function.name)
            .expect("every local callable has an inferred summary");
        check_declared_contract(function, inferred)?;
        let exact = inferred.with_entries(&function.contract);
        let effective = conservative_declared_contract(&function.contract, &exact);
        local.insert(function.name.clone(), effective);
    }
    Ok(CheckedCallableContracts { local, imports, host_imports })
}

fn authenticate_catalog(
    module: &CandidateModule,
    layouts: &LayoutInterner,
    host: bool,
) -> Result<BTreeMap<String, CallableMemoryContract>, ValidationError> {
    let attached = if host { &module.host_imports } else { &module.imports };
    let i64_layout = module.layouts.roots().iter().copied().find(|layout| {
        matches!(layouts.get(*layout).map(|descriptor| descriptor.kind()), Some(LayoutKind::Scalar(ScalarKind::Int)))
    });
    let mut checked = BTreeMap::new();
    for (name, candidate) in attached {
        let authenticated = catalog_contract(name, candidate, i64_layout, layouts, host).ok_or_else(|| {
            module_error(
                if host { RuleId::HostBufferAbi } else { RuleId::CheckedCallableContract },
                format!("import `{name}` has no matching authenticated catalog entry"),
            )
        })?;
        checked.insert(name.clone(), authenticated);
    }
    Ok(checked)
}

fn catalog_contract(
    name: &str,
    candidate: &CallableMemoryContract,
    i64_layout: Option<LayoutId>,
    layouts: &LayoutInterner,
    host: bool,
) -> Option<CallableMemoryContract> {
    let exact = |expected: CallableMemoryContract| (candidate == &expected).then_some(expected);
    if host {
        return match name {
            "safewir.host.no_memory" | "host" => exact(no_memory_contract()),
            "safewir.host.read_i64" | "read" => {
                exact(identity_shape_contract(i64_layout?, ParameterMemoryEffects::only(ParameterMemoryEffect::Read), CallableMemoryResult::NoMemory))
            }
            // A descriptor-polymorphic read-only ABI. Shape remains checked at
            // each call and cannot authorize Fresh, write, consume, or reuse.
            "read_list" if candidate.parameters.len() == 2
                && candidate.parameters[0].entry == ParameterMemoryEntry::NoMemory
                && candidate.parameters[1].entry == ParameterMemoryEntry::BorrowedRead
                && candidate.parameters[1].layout.is_some()
                && candidate.parameters[1].extent.is_some()
                && candidate.parameters[1].effects
                    == ParameterMemoryEffects::only(ParameterMemoryEffect::Read)
                && packed_list_contract_is_authenticated(&candidate.parameters[1], layouts)
                && candidate.result == CallableMemoryResult::NoMemory => Some(candidate.clone()),
            _ => None,
        };
    }
    match name {
        "safewir.primitive.identity_i64" | "alias"
            if candidate.parameters.len() == 1
                && matches!(
                    candidate.parameters[0].entry,
                    ParameterMemoryEntry::BorrowedRead | ParameterMemoryEntry::Shared
                )
                && candidate.parameters[0].layout == Some(i64_layout?)
                && candidate.parameters[0].extent == Some(exact_eight())
                && candidate.parameters[0].effects
                    == ParameterMemoryEffects::only(ParameterMemoryEffect::Escape)
                && candidate.result
                    == (CallableMemoryResult::Aliases {
                        parameter: 0,
                        projection: ContractProjection::default(),
                    }) => Some(candidate.clone()),
        "safewir.primitive.allocate_i64" => exact(CallableMemoryContract {
            parameters: Vec::new(),
            result: CallableMemoryResult::Fresh {
                layout: i64_layout?,
                extent: exact_eight(),
            },
        }),
        "safewir.primitive.escape_i64" | "escape" => exact(CallableMemoryContract {
            parameters: vec![ParameterMemoryContract {
                entry: ParameterMemoryEntry::Shared,
                layout: Some(i64_layout?),
                extent: Some(exact_eight()),
                effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
            }],
            result: CallableMemoryResult::NoMemory,
        }),
        "safewir.primitive.unknown" | "unknown" | "unknown_effect" => {
            exact(CallableMemoryContract::unknown(1))
        }
        _ => None,
    }
}

fn packed_list_contract_is_authenticated(
    parameter: &ParameterMemoryContract,
    layouts: &LayoutInterner,
) -> bool {
    let (Some(layout), Some(ContractExtent::Counted { header, count, stride })) =
        (parameter.layout, parameter.extent.as_ref())
    else {
        return false;
    };
    let Some(descriptor) = layouts.get(layout) else { return false };
    let LayoutKind::PackedList { element, .. } = descriptor.kind() else { return false };
    if !matches!(layouts.get(*element).map(|descriptor| descriptor.kind()), Some(LayoutKind::Scalar(ScalarKind::Int))) {
        return false;
    }
    let crate::layout::LayoutSize::Dynamic { base, stride: expected_stride } = descriptor.size()
    else {
        return false;
    };
    *header == ContractSizeExpr::Constant(u64::from(base))
        && *count == ContractSizeExpr::Parameter(0)
        && *stride == ContractSizeExpr::Constant(u64::from(expected_stride))
}

fn identity_shape_contract(
    layout: LayoutId,
    effects: ParameterMemoryEffects,
    result: CallableMemoryResult,
) -> CallableMemoryContract {
    CallableMemoryContract {
        parameters: vec![ParameterMemoryContract {
            entry: ParameterMemoryEntry::BorrowedRead,
            layout: Some(layout),
            extent: Some(exact_eight()),
            effects,
        }],
        result,
    }
}

fn no_memory_contract() -> CallableMemoryContract {
    CallableMemoryContract { parameters: Vec::new(), result: CallableMemoryResult::NoMemory }
}

fn exact_eight() -> ContractExtent {
    ContractExtent::Exact(ContractSizeExpr::Constant(8))
}

fn infer_function(
    function: &CandidateFunction,
    local: &BTreeMap<String, InferredContract>,
    imports: &BTreeMap<String, CallableMemoryContract>,
    host_imports: &BTreeMap<String, CallableMemoryContract>,
    budget: ValidationBudget,
    work: &mut usize,
) -> Result<InferredContract, ValidationError> {
    let mut facts = BodyFacts {
        objects: BTreeMap::new(),
        scalar_parameters: BTreeMap::new(),
        scalar_constants: BTreeMap::new(),
        effects: vec![ParameterMemoryEffects::none(); function.parameters.len()],
        returned: InferredResult::Bottom,
        saw_object_return: false,
    };
    for (index, binding) in function.parameters.iter().enumerate() {
        if let Some(object) = &binding.object {
            facts.objects.insert(
                object.identity,
                Origin::Aliases(BTreeMap::from([(index, ContractProjection::default())])),
            );
        } else {
            facts.scalar_parameters.insert(binding.definition, index);
        }
    }
    infer_operations(
        function,
        &function.operations,
        local,
        imports,
        host_imports,
        &mut facts,
        budget,
        work,
        0,
    )?;
    if matches!(facts.returned, InferredResult::Bottom) && !facts.saw_object_return {
        facts.returned = InferredResult::Known(CallableMemoryResult::NoMemory);
    }
    Ok(InferredContract { effects: facts.effects, result: facts.returned })
}

fn infer_operations(
    function: &CandidateFunction,
    operations: &[Operation],
    local: &BTreeMap<String, InferredContract>,
    imports: &BTreeMap<String, CallableMemoryContract>,
    host_imports: &BTreeMap<String, CallableMemoryContract>,
    facts: &mut BodyFacts,
    budget: ValidationBudget,
    work: &mut usize,
    depth: usize,
) -> Result<(), ValidationError> {
    if depth > budget.expression_depth {
        return Err(module_error(
            RuleId::BudgetExhaustionFailsClosed,
            "callable-contract summary exhausted the structured depth budget",
        ));
    }
    for operation in operations {
        *work = work.checked_add(1).ok_or_else(|| {
            module_error(RuleId::BudgetExhaustionFailsClosed, "call summary work overflowed")
        })?;
        if *work > budget.operations {
            return Err(module_error(
                RuleId::BudgetExhaustionFailsClosed,
                "callable-contract summary exhausted the operation budget",
            ));
        }
        match operation {
            Operation::DefineScalar { definition, value } => {
                if let Ok(value) = u64::try_from(*value) {
                    facts.scalar_constants.insert(*definition, value);
                }
            }
            Operation::Alloc { result } => {
                let origin = extent_to_contract(&result.extent, facts)
                    .map(|extent| Origin::Fresh {
                        site: result.identity,
                        layout: result.layout,
                        extent,
                    })
                    .unwrap_or_else(|| Origin::Unknown(BTreeSet::new()));
                facts.objects.insert(result.identity, origin);
            }
            Operation::ProjectObject { source, projection, result } => {
                let origin = facts.objects.get(source).cloned().unwrap_or_else(|| Origin::Unknown(BTreeSet::new()));
                facts.objects.insert(result.identity, origin.project(projection));
            }
            Operation::MergeObjectAliases { sources, result } => {
                let origin = sources.iter().fold(Origin::Bottom, |joined, source| {
                    join_origins(joined, facts.objects.get(source).cloned().unwrap_or_else(|| Origin::Unknown(BTreeSet::new())))
                });
                facts.objects.insert(result.identity, origin);
            }
            Operation::Reuse { old, new } => {
                let sources = facts.objects.get(old).map(Origin::source_parameters).unwrap_or_default();
                add_effect(facts, &sources, ParameterMemoryEffect::Consume);
                add_effect(facts, &sources, ParameterMemoryEffect::Write);
                facts.objects.insert(new.identity, Origin::Unknown(sources));
            }
            Operation::Load { place, .. } => add_object_effect(facts, place.object.identity, ParameterMemoryEffect::Read),
            Operation::Store { place, .. } | Operation::Fill { destination: place } => {
                add_object_effect(facts, place.object.identity, ParameterMemoryEffect::Write)
            }
            Operation::Copy { source, destination, .. } => {
                add_object_effect(facts, source.object.identity, ParameterMemoryEffect::Read);
                add_object_effect(facts, destination.object.identity, ParameterMemoryEffect::Write);
            }
            Operation::Retain { object } => add_object_effect(facts, *object, ParameterMemoryEffect::Retain),
            Operation::Release { object } | Operation::Move { object } => {
                add_object_effect(facts, *object, ParameterMemoryEffect::Consume)
            }
            Operation::Call { callable, arguments, result } => {
                let contract = local
                    .get(callable)
                    .map(|summary| summary.with_entries(&unknown_entries(summary.effects.len())))
                    .or_else(|| imports.get(callable).cloned())
                    .unwrap_or_else(|| CallableMemoryContract::unknown(arguments.len()));
                apply_call(&contract, arguments, result.as_ref(), facts);
            }
            Operation::ClosureCall { callable, captures, arguments, result } => {
                let combined = captures.iter().chain(arguments).copied().collect::<Vec<_>>();
                let named_arity = local
                    .get(callable)
                    .map(|summary| summary.effects.len())
                    .or_else(|| imports.get(callable).map(|contract| contract.parameters.len()));
                let contract = if named_arity == Some(combined.len()) {
                    local
                        .values()
                        .filter(|summary| summary.effects.len() == combined.len())
                        .map(|summary| {
                            summary.with_entries(&unknown_entries(summary.effects.len()))
                        })
                        .chain(
                            imports
                                .values()
                                .filter(|contract| contract.parameters.len() == combined.len())
                                .cloned(),
                        )
                        .reduce(join_contracts)
                        .unwrap_or_else(|| CallableMemoryContract::unknown(combined.len()))
                } else {
                    CallableMemoryContract::unknown(combined.len())
                };
                apply_call(&contract, &combined, result.as_ref(), facts);
            }
            Operation::IndirectCall { arguments, result } => {
                let contract = local
                    .values()
                    .filter(|summary| summary.effects.len() == arguments.len())
                    .map(|summary| summary.with_entries(&unknown_entries(summary.effects.len())))
                    .chain(imports.values().filter(|contract| contract.parameters.len() == arguments.len()).cloned())
                    .reduce(join_contracts)
                    .unwrap_or_else(|| CallableMemoryContract::unknown(arguments.len()));
                apply_call(&contract, arguments, result.as_ref(), facts);
            }
            Operation::HostCall { import, arguments, result } => {
                let call_arguments = arguments
                    .iter()
                    .map(|argument| match argument {
                        HostArgument::Scalar(definition) => CallArgument::Scalar(*definition),
                        HostArgument::Buffer(place) => CallArgument::Object(place.object.identity),
                    })
                    .collect::<Vec<_>>();
                let contract = host_imports
                    .get(import)
                    .cloned()
                    .unwrap_or_else(|| CallableMemoryContract::unknown(call_arguments.len()));
                apply_call(&contract, &call_arguments, result.as_ref(), facts);
            }
            Operation::If { then_operations, else_operations, .. } => {
                let nested = depth.checked_add(1).ok_or_else(|| {
                    module_error(
                        RuleId::BudgetExhaustionFailsClosed,
                        "call summary structured depth overflowed",
                    )
                })?;
                let mut then_facts = facts.clone();
                infer_operations(
                    function,
                    then_operations,
                    local,
                    imports,
                    host_imports,
                    &mut then_facts,
                    budget,
                    work,
                    nested,
                )?;
                let mut else_facts = facts.clone();
                infer_operations(
                    function,
                    else_operations,
                    local,
                    imports,
                    host_imports,
                    &mut else_facts,
                    budget,
                    work,
                    nested,
                )?;
                *facts = join_body_facts(then_facts, else_facts);
            }
            Operation::Block { body, .. } => {
                let nested = depth.checked_add(1).ok_or_else(|| {
                    module_error(
                        RuleId::BudgetExhaustionFailsClosed,
                        "call summary structured depth overflowed",
                    )
                })?;
                infer_operations(
                    function,
                    body,
                    local,
                    imports,
                    host_imports,
                    facts,
                    budget,
                    work,
                    nested,
                )?;
            }
            Operation::Loop { body, .. } => {
                let nested = depth.checked_add(1).ok_or_else(|| {
                    module_error(
                        RuleId::BudgetExhaustionFailsClosed,
                        "call summary structured depth overflowed",
                    )
                })?;
                let entry = facts.clone();
                let mut header = entry.clone();
                loop {
                    let mut iteration = header.clone();
                    infer_operations(
                        function,
                        body,
                        local,
                        imports,
                        host_imports,
                        &mut iteration,
                        budget,
                        work,
                        nested,
                    )?;
                    let next = join_body_facts(entry.clone(), iteration);
                    if next == header {
                        header = next;
                        break;
                    }
                    header = next;
                }
                *facts = header;
            }
            Operation::ReturnObject { object } => {
                facts.saw_object_return = true;
                let origin = facts.objects.get(object).cloned().unwrap_or_else(|| Origin::Unknown(BTreeSet::new()));
                let sources = origin.source_parameters();
                add_effect(facts, &sources, ParameterMemoryEffect::Escape);
                facts.returned = join_inferred_results(
                    facts.returned.clone(),
                    InferredResult::Known(result_from_origin(origin)),
                );
            }
            Operation::Branch { condition: None, .. } | Operation::ReturnVoid => return Ok(()),
            Operation::Project { .. }
            | Operation::CheckedIndex { .. }
            | Operation::ProvenIndex { .. }
            | Operation::Branch { condition: Some(_), .. }
            | Operation::ReferenceCastFromScalar => {}
        }
    }
    Ok(())
}

fn unknown_entries(parameter_count: usize) -> CallableMemoryContract {
    CallableMemoryContract::unknown(parameter_count)
}

fn apply_call(
    contract: &CallableMemoryContract,
    arguments: &[CallArgument],
    result: Option<&crate::safewir::ObjectRef>,
    facts: &mut BodyFacts,
) {
    for (argument, parameter) in arguments.iter().zip(&contract.parameters) {
        let CallArgument::Object(identity) = argument else { continue };
        let sources = facts.objects.get(identity).map(Origin::source_parameters).unwrap_or_default();
        for effect in [
            ParameterMemoryEffect::Read,
            ParameterMemoryEffect::Write,
            ParameterMemoryEffect::Consume,
            ParameterMemoryEffect::Retain,
            ParameterMemoryEffect::Escape,
        ] {
            if parameter.effects.contains(effect) {
                add_effect(facts, &sources, effect);
            }
        }
    }
    let Some(result) = result else { return };
    let origin = match &contract.result {
        CallableMemoryResult::Fresh { layout, extent } => {
            substitute_extent(extent, arguments, facts)
                .map(|extent| Origin::Fresh {
                    site: result.identity,
                    layout: *layout,
                    extent,
                })
                .unwrap_or_else(|| Origin::Unknown(argument_sources(arguments, facts)))
        }
        CallableMemoryResult::Aliases { parameter, projection } => arguments
            .get(*parameter)
            .and_then(|argument| match argument {
                CallArgument::Object(identity) => facts.objects.get(identity).cloned(),
                CallArgument::Scalar(_) => None,
            })
            .map(|origin| origin.project(projection))
            .unwrap_or_else(|| Origin::Unknown(argument_sources(arguments, facts))),
        CallableMemoryResult::Shared(parameters) => parameters.iter().fold(Origin::Bottom, |joined, parameter| {
            let origin = arguments
                .get(*parameter)
                .and_then(|argument| match argument {
                    CallArgument::Object(identity) => facts.objects.get(identity).cloned(),
                    CallArgument::Scalar(_) => None,
                })
                .unwrap_or_else(|| Origin::Unknown(argument_sources(arguments, facts)));
            join_origins(joined, origin)
        }),
        CallableMemoryResult::NoMemory | CallableMemoryResult::Unknown => {
            Origin::Unknown(argument_sources(arguments, facts))
        }
    };
    facts.objects.insert(result.identity, origin);
}

fn check_declared_contract(
    function: &CandidateFunction,
    inferred: &InferredContract,
) -> Result<(), ValidationError> {
    if function.contract.parameters.len() != inferred.effects.len()
        || !function.contract.result_effects_are_consistent()
    {
        return Err(function_error(function, "callable contract shape is inconsistent"));
    }
    for (index, (declared, actual)) in function
        .contract
        .parameters
        .iter()
        .zip(&inferred.effects)
        .enumerate()
    {
        if !declared.effects.is_unknown() && declared.effects != *actual {
            return Err(function_error(
                function,
                format!("parameter {index} has a stale effect summary: declared {:?}, final body {:?}", declared.effects, actual),
            ));
        }
    }
    let actual = match &inferred.result {
        InferredResult::Bottom => CallableMemoryResult::Unknown,
        InferredResult::Known(result) => result.clone(),
    };
    if !matches!(function.contract.result, CallableMemoryResult::Unknown)
        && function.contract.result != actual
    {
        return Err(function_error(
            function,
            format!("stale result summary: declared {:?}, final body {:?}", function.contract.result, actual),
        ));
    }
    Ok(())
}

fn conservative_declared_contract(
    declared: &CallableMemoryContract,
    inferred: &CallableMemoryContract,
) -> CallableMemoryContract {
    CallableMemoryContract {
        parameters: declared
            .parameters
            .iter()
            .zip(&inferred.parameters)
            .map(|(declared, inferred)| ParameterMemoryContract {
                entry: declared.entry,
                layout: declared.layout,
                extent: declared.extent.clone(),
                effects: if declared.effects.is_unknown() {
                    ParameterMemoryEffects::unknown()
                } else {
                    inferred.effects
                },
            })
            .collect(),
        result: if matches!(declared.result, CallableMemoryResult::Unknown) {
            CallableMemoryResult::Unknown
        } else {
            inferred.result.clone()
        },
    }
}

pub(crate) fn join_contracts(
    mut left: CallableMemoryContract,
    right: CallableMemoryContract,
) -> CallableMemoryContract {
    if left.parameters.len() != right.parameters.len() {
        return CallableMemoryContract::unknown(left.parameters.len().max(right.parameters.len()));
    }
    for (left, right) in left.parameters.iter_mut().zip(right.parameters) {
        if left.entry != right.entry || left.layout != right.layout || left.extent != right.extent {
            *left = ParameterMemoryContract::unknown();
        } else {
            left.effects = left.effects.join(right.effects);
        }
    }
    left.result = join_results(left.result, right.result);
    left
}

fn join_results(left: CallableMemoryResult, right: CallableMemoryResult) -> CallableMemoryResult {
    if left == right {
        return left;
    }
    match (left, right) {
        (CallableMemoryResult::Aliases { parameter: left, projection: left_projection },
         CallableMemoryResult::Aliases { parameter: right, projection: right_projection })
            if left_projection.steps().is_empty() && right_projection.steps().is_empty() =>
        {
            CallableMemoryResult::Shared(BTreeSet::from([left, right]))
        }
        (CallableMemoryResult::Shared(mut left), CallableMemoryResult::Shared(right)) => {
            left.extend(right);
            CallableMemoryResult::Shared(left)
        }
        (CallableMemoryResult::Shared(mut set), CallableMemoryResult::Aliases { parameter, projection })
        | (CallableMemoryResult::Aliases { parameter, projection }, CallableMemoryResult::Shared(mut set))
            if projection.steps().is_empty() =>
        {
            set.insert(parameter);
            CallableMemoryResult::Shared(set)
        }
        _ => CallableMemoryResult::Unknown,
    }
}

fn join_origins(left: Origin, right: Origin) -> Origin {
    if left == right {
        return left;
    }
    match (left, right) {
        (Origin::Bottom, other) | (other, Origin::Bottom) => other,
        (Origin::Aliases(mut left), Origin::Aliases(right)) => {
            for (parameter, projection) in right {
                if left.get(&parameter).is_some_and(|existing| existing != &projection) {
                    let mut sources = left.keys().copied().collect::<BTreeSet<_>>();
                    sources.insert(parameter);
                    return Origin::Unknown(sources);
                }
                left.insert(parameter, projection);
            }
            Origin::Aliases(left)
        }
        (left, right) => {
            let mut sources = left.source_parameters();
            sources.extend(right.source_parameters());
            Origin::Unknown(sources)
        }
    }
}

fn join_inferred_results(left: InferredResult, right: InferredResult) -> InferredResult {
    match (left, right) {
        (InferredResult::Bottom, other) | (other, InferredResult::Bottom) => other,
        (InferredResult::Known(left), InferredResult::Known(right)) => {
            InferredResult::Known(join_results(left, right))
        }
    }
}

fn result_from_origin(origin: Origin) -> CallableMemoryResult {
    match origin {
        Origin::Fresh { layout, extent, .. } => CallableMemoryResult::Fresh { layout, extent },
        Origin::Aliases(parameters) if parameters.len() == 1 => {
            let (parameter, projection) = parameters.into_iter().next().unwrap();
            CallableMemoryResult::Aliases { parameter, projection }
        }
        Origin::Aliases(parameters)
            if parameters.values().all(|projection| projection.steps().is_empty()) =>
        {
            CallableMemoryResult::Shared(parameters.into_keys().collect())
        }
        Origin::Bottom | Origin::Aliases(_) | Origin::Unknown(_) => CallableMemoryResult::Unknown,
    }
}

fn add_object_effect(facts: &mut BodyFacts, identity: ObjectIdentity, effect: ParameterMemoryEffect) {
    let sources = facts.objects.get(&identity).map(Origin::source_parameters).unwrap_or_default();
    add_effect(facts, &sources, effect);
}

fn add_effect(facts: &mut BodyFacts, sources: &BTreeSet<usize>, effect: ParameterMemoryEffect) {
    for source in sources {
        if let Some(effects) = facts.effects.get_mut(*source) {
            *effects = effects.with(effect);
        }
    }
}

fn argument_sources(arguments: &[CallArgument], facts: &BodyFacts) -> BTreeSet<usize> {
    arguments.iter().fold(BTreeSet::new(), |mut sources, argument| {
        if let CallArgument::Object(identity) = argument {
            if let Some(origin) = facts.objects.get(identity) {
                sources.extend(origin.source_parameters());
            }
        }
        sources
    })
}

fn extent_to_contract(extent: &Extent, facts: &BodyFacts) -> Option<ContractExtent> {
    match extent {
        Extent::Unknown => None,
        Extent::Exact(size) => Some(ContractExtent::Exact(size_to_contract(size, facts)?)),
        Extent::Counted { header, count, stride } => Some(ContractExtent::Counted {
            header: size_to_contract(header, facts)?,
            count: definition_to_contract(*count, facts)?,
            stride: size_to_contract(stride, facts)?,
        }),
    }
}

fn size_to_contract(size: &SizeExpr, facts: &BodyFacts) -> Option<ContractSizeExpr> {
    match size {
        SizeExpr::Constant(value) => Some(ContractSizeExpr::Constant(*value)),
        SizeExpr::Definition(definition) => definition_to_contract(*definition, facts),
        SizeExpr::Add(left, right) => Some(ContractSizeExpr::Add(
            Box::new(size_to_contract(left, facts)?),
            Box::new(size_to_contract(right, facts)?),
        )),
        SizeExpr::Multiply(left, right) => Some(ContractSizeExpr::Multiply(
            Box::new(size_to_contract(left, facts)?),
            Box::new(size_to_contract(right, facts)?),
        )),
        SizeExpr::NarrowU32(_) => None,
    }
}

fn definition_to_contract(definition: DefinitionId, facts: &BodyFacts) -> Option<ContractSizeExpr> {
    facts
        .scalar_parameters
        .get(&definition)
        .copied()
        .map(ContractSizeExpr::Parameter)
        .or_else(|| facts.scalar_constants.get(&definition).copied().map(ContractSizeExpr::Constant))
}

fn substitute_extent(
    extent: &ContractExtent,
    arguments: &[CallArgument],
    facts: &BodyFacts,
) -> Option<ContractExtent> {
    let substitute = |size| substitute_size(size, arguments, facts);
    match extent {
        ContractExtent::Exact(size) => Some(ContractExtent::Exact(substitute(size)?)),
        ContractExtent::Counted { header, count, stride } => Some(ContractExtent::Counted {
            header: substitute(header)?,
            count: substitute(count)?,
            stride: substitute(stride)?,
        }),
    }
}

fn substitute_size(
    size: &ContractSizeExpr,
    arguments: &[CallArgument],
    facts: &BodyFacts,
) -> Option<ContractSizeExpr> {
    match size {
        ContractSizeExpr::Constant(value) => Some(ContractSizeExpr::Constant(*value)),
        ContractSizeExpr::Parameter(parameter) => match arguments.get(*parameter)? {
            CallArgument::Scalar(definition) => definition_to_contract(*definition, facts),
            CallArgument::Object(_) => None,
        },
        ContractSizeExpr::Add(left, right) => Some(ContractSizeExpr::Add(
            Box::new(substitute_size(left, arguments, facts)?),
            Box::new(substitute_size(right, arguments, facts)?),
        )),
        ContractSizeExpr::Multiply(left, right) => Some(ContractSizeExpr::Multiply(
            Box::new(substitute_size(left, arguments, facts)?),
            Box::new(substitute_size(right, arguments, facts)?),
        )),
    }
}

fn join_body_facts(left: BodyFacts, right: BodyFacts) -> BodyFacts {
    let mut objects = BTreeMap::new();
    for identity in left.objects.keys().chain(right.objects.keys()).copied().collect::<BTreeSet<_>>() {
        let origin = match (left.objects.get(&identity), right.objects.get(&identity)) {
            (Some(left), Some(right)) => join_origins(left.clone(), right.clone()),
            (Some(origin), None) | (None, Some(origin)) => Origin::Unknown(origin.source_parameters()),
            (None, None) => unreachable!(),
        };
        objects.insert(identity, origin);
    }
    BodyFacts {
        objects,
        scalar_parameters: left
            .scalar_parameters
            .iter()
            .filter_map(|(definition, parameter)| {
                (right.scalar_parameters.get(definition) == Some(parameter)).then_some((*definition, *parameter))
            })
            .collect(),
        scalar_constants: left
            .scalar_constants
            .iter()
            .filter_map(|(definition, value)| {
                (right.scalar_constants.get(definition) == Some(value)).then_some((*definition, *value))
            })
            .collect(),
        effects: left.effects.iter().zip(right.effects).map(|(left, right)| left.join(right)).collect(),
        returned: join_inferred_results(left.returned, right.returned),
        saw_object_return: left.saw_object_return || right.saw_object_return,
    }
}

fn module_error(rule: RuleId, message: impl Into<String>) -> ValidationError {
    ValidationError {
        diagnostic: ValidationDiagnostic::Rule(rule),
        function: None,
        operation: None,
        message: message.into(),
    }
}

fn function_error(function: &CandidateFunction, message: impl Into<String>) -> ValidationError {
    ValidationError {
        diagnostic: ValidationDiagnostic::Rule(RuleId::CheckedCallableContract),
        function: Some(function.name.clone()),
        operation: None,
        message: message.into(),
    }
}
