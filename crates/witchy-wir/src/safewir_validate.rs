//! Deterministic, fail-closed validation for RFC-0162 SafeWIR candidates.
//!
//! This module validates the declarative SafeWIR operation graph. Mapping the
//! executable WIR tree onto this graph and granting encoder authority remain
//! separate integration boundaries; accepting a [`CandidateModule`] here does
//! not authorize encoding an ordinary `WirModule`.
//!
//! This first report-mode slice is deliberately closed-world and
//! intraprocedural. Candidate source-entry, raw-memory, import-contract, and
//! post-validation flags are attestations until the production WIR adapter is
//! implemented, so this module does not discharge SWIR-MOD-001,
//! SWIR-KERN-001, or SWIR-ENC-001. Canonical decoding proves that the supplied
//! layout bundle is internally well formed, not that it came from the checked
//! source signature. CFG joins, loops, recursive call graphs, symbolic
//! parameter extents, proven-index facts, shared result sets, non-root alias
//! results, and owned parameter transfers fail as explicitly unsupported rather
//! than masquerading as a rule violation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

use crate::layout::{
    FieldKind, HeaderLayout, LayoutBundle, LayoutId, LayoutInterner, LayoutKind, LayoutSize,
};
use crate::safewir::{
    AccessMode, ByteRange, CallableMemoryContract, CallableMemoryResult, ContractExtent,
    ContractSizeExpr, DefinitionId, Extent, ObjectIdentity, ObjectRef, Ownership,
    ParameterMemoryEffect, ParameterMemoryEntry, Place, ProjectionStep, Provenance, RuleId,
    SizeExpr,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationBudget {
    pub functions: usize,
    pub operations: usize,
    pub facts: usize,
    pub expression_depth: usize,
}

impl Default for ValidationBudget {
    fn default() -> Self {
        Self {
            functions: 10_000,
            operations: 2_000_000,
            facts: 4_000_000,
            expression_depth: 128,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostValidationTransform {
    FixedTemplateExpansionAndErasure,
    Arbitrary,
}

#[derive(Debug, Clone)]
pub struct CandidateModule {
    pub checked_source_entries: usize,
    pub layouts: LayoutBundle,
    pub imports: BTreeMap<String, CallableMemoryContract>,
    pub host_imports: BTreeMap<String, CallableMemoryContract>,
    pub functions: Vec<CandidateFunction>,
    pub contains_raw_memory: bool,
    pub contains_raw_body: bool,
    pub post_validation_transform: PostValidationTransform,
}

#[derive(Debug, Clone)]
pub struct CandidateFunction {
    pub name: String,
    pub contract: CallableMemoryContract,
    pub parameters: Vec<ParameterBinding>,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone)]
pub struct ParameterBinding {
    pub definition: DefinitionId,
    pub object: Option<ObjectRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyKind {
    Disjoint,
    MayOverlap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallArgument {
    Scalar(DefinitionId),
    Object(ObjectIdentity),
}

#[derive(Debug, Clone)]
pub enum HostArgument {
    Scalar(DefinitionId),
    Buffer(Place),
}

#[derive(Debug, Clone)]
pub enum Operation {
    DefineScalar {
        definition: DefinitionId,
        value: i128,
    },
    Alloc {
        result: ObjectRef,
    },
    Project {
        object: ObjectIdentity,
        place: Place,
    },
    CheckedIndex {
        object: ObjectIdentity,
        index: DefinitionId,
        length_source: DefinitionId,
        place: Place,
    },
    ProvenIndex {
        object: ObjectIdentity,
        index: DefinitionId,
        length_source: DefinitionId,
        place: Place,
    },
    Load {
        place: Place,
    },
    Store {
        place: Place,
    },
    Copy {
        source: Place,
        destination: Place,
        kind: CopyKind,
    },
    Fill {
        destination: Place,
    },
    Retain {
        object: ObjectIdentity,
    },
    Release {
        object: ObjectIdentity,
    },
    Move {
        object: ObjectIdentity,
    },
    Reuse {
        old: ObjectIdentity,
        new: ObjectRef,
    },
    Call {
        callable: String,
        arguments: Vec<CallArgument>,
        result: Option<ObjectRef>,
    },
    HostCall {
        import: String,
        arguments: Vec<HostArgument>,
        result: Option<ObjectRef>,
    },
    Join {
        result: ObjectRef,
        incoming: Vec<ObjectIdentity>,
    },
    Loop {
        entry: Vec<ObjectIdentity>,
        back_edges: Vec<Vec<ObjectIdentity>>,
    },
    ReturnObject {
        object: ObjectIdentity,
    },
    ReferenceCastFromScalar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedForm {
    ControlFlowJoin,
    Loop,
    RecursiveCallGraph,
    ProvenIndex,
    SymbolicParameterExtent,
    NonRootAliasResult,
    SharedResult,
    OwnedParameterTransfer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationDiagnostic {
    Rule(RuleId),
    Unsupported(UnsupportedForm),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub diagnostic: ValidationDiagnostic,
    pub function: Option<String>,
    pub operation: Option<usize>,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.diagnostic {
            ValidationDiagnostic::Rule(rule) => write!(formatter, "{}", rule.code())?,
            ValidationDiagnostic::Unsupported(form) => write!(formatter, "unsupported {form:?}")?,
        }
        if let Some(function) = &self.function {
            write!(formatter, " in `{function}`")?;
        }
        if let Some(operation) = self.operation {
            write!(formatter, " at operation {operation}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationMetrics {
    pub functions: usize,
    pub operations: usize,
    pub peak_facts: usize,
}

/// Report-mode success for the declarative candidate graph. This type carries
/// no encoder authority.
#[derive(Debug)]
pub struct ValidationReport {
    metrics: ValidationMetrics,
    elapsed: Duration,
}

impl ValidationReport {
    pub const fn metrics(&self) -> ValidationMetrics {
        self.metrics
    }

    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

#[derive(Debug, Clone)]
struct ObjectState {
    object: ObjectRef,
    ownership: Ownership,
}

#[derive(Debug, Clone)]
struct ReturnedObject {
    object: ObjectRef,
    ownership: Ownership,
}

#[derive(Default)]
struct State {
    scalars: BTreeMap<DefinitionId, i128>,
    objects: BTreeMap<ObjectIdentity, ObjectState>,
    aliases: BTreeMap<ObjectIdentity, BTreeSet<ObjectIdentity>>,
    definitions: BTreeSet<DefinitionId>,
    minted: BTreeSet<ObjectIdentity>,
    returned: Option<ReturnedObject>,
}

impl State {
    fn fact_count(&self) -> usize {
        self.scalars.len()
            + self.objects.len()
            + self.aliases.values().map(BTreeSet::len).sum::<usize>()
            + self.definitions.len()
            + self.minted.len()
            + usize::from(self.returned.is_some())
    }
}

struct Validator<'a> {
    module: &'a CandidateModule,
    layouts: LayoutInterner,
    budget: ValidationBudget,
    operations: usize,
    peak_facts: usize,
}

pub fn validate_candidate(
    module: &CandidateModule,
    budget: ValidationBudget,
) -> Result<ValidationReport, ValidationError> {
    let started = Instant::now();
    let (_, layouts) = LayoutBundle::decode_canonical(&module.layouts.canonical_bytes()).map_err(
        |error| ValidationError {
            diagnostic: ValidationDiagnostic::Rule(RuleId::CanonicalLayout),
            function: None,
            operation: None,
            message: format!("invalid canonical layout bundle: {error}"),
        },
    )?;
    let mut validator = Validator {
        module,
        layouts,
        budget,
        operations: 0,
        peak_facts: 0,
    };
    validator.validate_module()?;
    Ok(ValidationReport {
        metrics: ValidationMetrics {
            functions: module.functions.len(),
            operations: validator.operations,
            peak_facts: validator.peak_facts,
        },
        elapsed: started.elapsed(),
    })
}

impl Validator<'_> {
    fn validate_module(&mut self) -> Result<(), ValidationError> {
        if self.module.checked_source_entries != 1 {
            return Err(self.module_error(
                RuleId::CheckedSourceEntry,
                "production candidate must have exactly one checked source entry",
            ));
        }
        if self.module.contains_raw_memory || self.module.contains_raw_body {
            return Err(self.module_error(
                RuleId::SealedRawMemory,
                "raw memory and raw function bodies are forbidden in candidate SafeWIR",
            ));
        }
        if self.module.post_validation_transform
            != PostValidationTransform::FixedTemplateExpansionAndErasure
        {
            return Err(self.module_error(
                RuleId::ValidatedEncoderAuthority,
                "only fixed template expansion and ghost-fact erasure may follow validation",
            ));
        }
        if self.module.functions.len() > self.budget.functions {
            return Err(self.module_error(
                RuleId::BudgetExhaustionFailsClosed,
                "function budget exhausted",
            ));
        }
        let declared_operations = self
            .module
            .functions
            .iter()
            .try_fold(0_usize, |total, function| {
                total.checked_add(function.operations.len())
            })
            .ok_or_else(|| {
                self.module_error(
                    RuleId::BudgetExhaustionFailsClosed,
                    "declared operation count overflowed",
                )
            })?;
        if declared_operations > self.budget.operations {
            return Err(self.module_error(
                RuleId::BudgetExhaustionFailsClosed,
                "operation budget exhausted before call-graph analysis",
            ));
        }

        let names: BTreeSet<_> = self.module.functions.iter().map(|f| f.name.as_str()).collect();
        if names.len() != self.module.functions.len() {
            return Err(self.module_error(
                RuleId::CheckedCallableContract,
                "function names must be unique",
            ));
        }
        if names.iter().any(|name| self.module.imports.contains_key(*name)) {
            return Err(self.module_error(
                RuleId::CheckedCallableContract,
                "a callable name cannot denote both a local function and an import",
            ));
        }
        if names
            .iter()
            .any(|name| self.module.host_imports.contains_key(*name))
            || self
                .module
                .imports
                .keys()
                .any(|name| self.module.host_imports.contains_key(name))
        {
            return Err(self.module_error(
                RuleId::CheckedCallableContract,
                "local, imported, and host-imported callable namespaces must be disjoint",
            ));
        }
        if self.has_recursive_call_graph() {
            return Err(self.module_unsupported(
                UnsupportedForm::RecursiveCallGraph,
                "recursive callable components require the deferred contract fixpoint",
            ));
        }
        for function in &self.module.functions {
            self.validate_function(function)?;
        }
        Ok(())
    }

    fn has_recursive_call_graph(&self) -> bool {
        let names: BTreeSet<_> = self.module.functions.iter().map(|f| f.name.as_str()).collect();
        let mut edges = BTreeMap::<&str, BTreeSet<&str>>::new();
        let mut incoming = names
            .iter()
            .copied()
            .map(|name| (name, 0_usize))
            .collect::<BTreeMap<_, _>>();
        for function in &self.module.functions {
            let callees = edges.entry(function.name.as_str()).or_default();
            for operation in &function.operations {
                let Operation::Call { callable, .. } = operation else { continue };
                if names.contains(callable.as_str()) && callees.insert(callable) {
                    *incoming.get_mut(callable.as_str()).expect("local name is indexed") += 1;
                }
            }
        }
        let mut ready = incoming
            .iter()
            .filter_map(|(name, count)| (*count == 0).then_some(*name))
            .collect::<Vec<_>>();
        let mut visited = 0_usize;
        while let Some(name) = ready.pop() {
            visited += 1;
            if let Some(callees) = edges.get(name) {
                for callee in callees {
                    let count = incoming.get_mut(callee).expect("local name is indexed");
                    *count -= 1;
                    if *count == 0 {
                        ready.push(callee);
                    }
                }
            }
        }
        visited != names.len()
    }

    fn validate_function(&mut self, function: &CandidateFunction) -> Result<(), ValidationError> {
        match &function.contract.result {
            CallableMemoryResult::Aliases { projection, .. } if !projection.steps().is_empty() => {
                return Err(self.function_unsupported(
                    UnsupportedForm::NonRootAliasResult,
                    function,
                    None,
                    "non-root callable aliases require descriptor-derived result shapes",
                ));
            }
            CallableMemoryResult::Shared(_) => {
                return Err(self.function_unsupported(
                    UnsupportedForm::SharedResult,
                    function,
                    None,
                    "shared result sets require a private may-alias value state",
                ));
            }
            _ => {}
        }
        if function.contract.parameters.iter().any(|parameter| {
            parameter.effects.contains(ParameterMemoryEffect::Consume)
                && !parameter.effects.is_unknown()
        }) {
            return Err(self.function_unsupported(
                UnsupportedForm::OwnedParameterTransfer,
                function,
                None,
                "owned parameter transfer requires private transferred provenance state",
            ));
        }
        if function.parameters.len() != function.contract.parameters.len()
            || !function.contract.result_effects_are_consistent()
        {
            return Err(self.function_error(
                RuleId::CheckedCallableContract,
                function,
                None,
                "parameter bindings or result effects contradict the callable contract",
            ));
        }

        let mut state = State::default();
        let arguments = function
            .parameters
            .iter()
            .map(|binding| match &binding.object {
                Some(object) => CallArgument::Object(object.identity),
                None => CallArgument::Scalar(binding.definition),
            })
            .collect::<Vec<_>>();
        for (index, (binding, contract)) in function
            .parameters
            .iter()
            .zip(&function.contract.parameters)
            .enumerate()
        {
            self.define(&mut state, binding.definition, function, None)?;
            match (contract.entry, contract.layout, &contract.extent, &binding.object) {
                (ParameterMemoryEntry::NoMemory | ParameterMemoryEntry::Unknown, None, None, None) => {}
                (ParameterMemoryEntry::NoMemory, _, _, Some(_)) => {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        None,
                        format!("parameter {index} declares NoMemory but carries an object"),
                    ));
                }
                (ParameterMemoryEntry::Unknown, _, _, Some(_)) => {
                    return Err(self.function_unsupported(
                        UnsupportedForm::SymbolicParameterExtent,
                        function,
                        None,
                        format!(
                            "parameter {index} has unknown object shape in report mode"
                        ),
                    ));
                }
                (entry, Some(layout), Some(extent), Some(object)) => {
                    self.validate_object_claim(object, function, None)?;
                    if object.identity.root != binding.definition
                        || object.layout != layout
                        || !contract_extent_matches(extent, &object.extent, &arguments)
                    {
                        return Err(self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            None,
                            format!(
                                "parameter {index} object identity, layout, or extent contradicts its checked signature"
                            ),
                        ));
                    }
                    if matches!(object.provenance, Provenance::Fresh) {
                        return Err(self.function_error(
                            RuleId::UniqueAllocation,
                            function,
                            None,
                            format!("parameter {index} cannot attest Fresh provenance"),
                        ));
                    }
                    let ownership = match entry {
                        ParameterMemoryEntry::BorrowedRead => Ownership::BorrowedRead,
                        ParameterMemoryEntry::BorrowedWrite => Ownership::BorrowedWrite,
                        ParameterMemoryEntry::Shared => Ownership::Shared,
                        ParameterMemoryEntry::Unique => Ownership::Unique,
                        ParameterMemoryEntry::NoMemory | ParameterMemoryEntry::Unknown => {
                            unreachable!()
                        }
                    };
                    let mut derived = object.clone();
                    derived.provenance = Provenance::Unknown;
                    if state
                        .objects
                        .insert(
                            object.identity,
                            ObjectState { object: derived, ownership },
                        )
                        .is_some()
                    {
                        return Err(self.function_error(
                            RuleId::UniqueAllocation,
                            function,
                            None,
                            "parameter bindings duplicate one object generation",
                        ));
                    }
                }
                (_, _, _, None) => {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        None,
                        format!("memory-bearing parameter {index} has no object fact"),
                    ));
                }
                _ => {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        None,
                        format!("parameter {index} has an inconsistent object shape"),
                    ));
                }
            }
        }
        for contract in &function.contract.parameters {
            if contract.extent.as_ref().is_some_and(|extent| !contract_extent_is_constant(extent)) {
                return Err(self.function_unsupported(
                    UnsupportedForm::SymbolicParameterExtent,
                    function,
                    None,
                    "symbolic parameter extents require the deferred range lattice",
                ));
            }
        }
        for object in state.objects.values().map(|object| &object.object) {
            self.validate_extent_against_layout(object, &state, function, None)?;
        }
        self.observe_facts(&state, function, None)?;

        for (operation_index, operation) in function.operations.iter().enumerate() {
            self.operations = self.operations.checked_add(1).ok_or_else(|| {
                self.function_error(
                    RuleId::BudgetExhaustionFailsClosed,
                    function,
                    Some(operation_index),
                    "operation counter overflowed",
                )
            })?;
            if self.operations > self.budget.operations {
                return Err(self.function_error(
                    RuleId::BudgetExhaustionFailsClosed,
                    function,
                    Some(operation_index),
                    "operation budget exhausted",
                ));
            }
            self.validate_operation(function, operation_index, operation, &mut state)?;
            self.observe_facts(&state, function, Some(operation_index))?;
        }
        self.validate_function_exit(function, &state)?;
        Ok(())
    }

    fn validate_function_exit(
        &self,
        function: &CandidateFunction,
        state: &State,
    ) -> Result<(), ValidationError> {
        const EFFECTS: [ParameterMemoryEffect; 5] = [
            ParameterMemoryEffect::Read,
            ParameterMemoryEffect::Write,
            ParameterMemoryEffect::Consume,
            ParameterMemoryEffect::Retain,
            ParameterMemoryEffect::Escape,
        ];
        for (index, (binding, contract)) in function
            .parameters
            .iter()
            .zip(&function.contract.parameters)
            .enumerate()
        {
            let Some(object) = &binding.object else { continue };
            let observed = self.observed_effects(function, object.identity, state);
            for effect in EFFECTS {
                if observed.contains(effect) && !contract.effects.contains(effect) {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        None,
                        format!(
                            "parameter {index} body performs undeclared {effect:?} effect"
                        ),
                    ));
                }
            }
            if !contract.effects.is_unknown() {
                for effect in [
                    ParameterMemoryEffect::Consume,
                    ParameterMemoryEffect::Retain,
                    ParameterMemoryEffect::Escape,
                ] {
                    if contract.effects.contains(effect) && !observed.contains(effect) {
                        return Err(self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            None,
                            format!(
                                "parameter {index} declares {effect:?} but no path discharges it"
                            ),
                        ));
                    }
                }
            }
        }

        let arguments: Vec<_> = function
            .parameters
            .iter()
            .map(|binding| match &binding.object {
                Some(object) => CallArgument::Object(object.identity),
                None => CallArgument::Scalar(binding.definition),
            })
            .collect();
        match (&function.contract.result, &state.returned) {
            (CallableMemoryResult::NoMemory, None) => {}
            (CallableMemoryResult::Unknown, _) => {}
            (CallableMemoryResult::Fresh { layout, extent }, Some(returned))
                if returned.object.layout == *layout
                    && returned.ownership == Ownership::Unique
                    && state.minted.contains(&returned.object.identity)
                    && contract_extent_matches(extent, &returned.object.extent, &arguments) =>
            {}
            (CallableMemoryResult::Aliases { parameter, projection }, Some(returned))
                if projection.steps().is_empty()
                    && matches!(
                        arguments.get(*parameter),
                        Some(CallArgument::Object(identity)) if *identity == returned.object.identity
                    ) =>
            {}
            (CallableMemoryResult::Aliases { .. }, Some(_)) => {
                return Err(self.function_error(
                    RuleId::CheckedCallableContract,
                    function,
                    None,
                    "function return does not alias the declared parameter",
                ));
            }
            (CallableMemoryResult::Shared(parameters), Some(returned))
                if matches!(
                    &returned.object.provenance,
                    Provenance::MayAlias(_)
                ) => {
                let expected: BTreeSet<_> = parameters
                    .iter()
                    .filter_map(|parameter| match arguments.get(*parameter) {
                        Some(CallArgument::Object(identity)) => Some(*identity),
                        _ => None,
                    })
                    .collect();
                if !expected.contains(&returned.object.identity)
                    || !matches!(&returned.object.provenance, Provenance::MayAlias(actual) if actual == &expected)
                {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        None,
                        "function return contradicts its Shared result contract",
                    ));
                }
            }
            _ => {
                return Err(self.function_error(
                    RuleId::CheckedCallableContract,
                    function,
                    None,
                    "function return contradicts its checked result contract",
                ));
            }
        }
        Ok(())
    }

    fn observed_effects(
        &self,
        function: &CandidateFunction,
        identity: ObjectIdentity,
        state: &State,
    ) -> crate::safewir::ParameterMemoryEffects {
        use crate::safewir::ParameterMemoryEffects;
        let mut observed = ParameterMemoryEffects::none();
        for operation in &function.operations {
            macro_rules! add {
                ($effect:expr) => {
                    observed = observed.with($effect)
                };
            }
            match operation {
                Operation::Load { place }
                    if identities_may_alias(state, place.object.identity, identity) =>
                {
                    add!(ParameterMemoryEffect::Read);
                }
                Operation::Store { place }
                    if identities_may_alias(state, place.object.identity, identity) =>
                {
                    add!(ParameterMemoryEffect::Write);
                }
                Operation::Copy { source, destination, .. } => {
                    if identities_may_alias(state, source.object.identity, identity) {
                        add!(ParameterMemoryEffect::Read);
                    }
                    if identities_may_alias(state, destination.object.identity, identity) {
                        add!(ParameterMemoryEffect::Write);
                    }
                }
                Operation::Fill { destination }
                    if identities_may_alias(state, destination.object.identity, identity) =>
                {
                    add!(ParameterMemoryEffect::Write);
                }
                Operation::Retain { object } if identities_may_alias(state, *object, identity) => {
                    add!(ParameterMemoryEffect::Retain);
                }
                Operation::Release { object } | Operation::Move { object }
                    if identities_may_alias(state, *object, identity) =>
                {
                    add!(ParameterMemoryEffect::Consume);
                }
                Operation::Reuse { old, .. } if identities_may_alias(state, *old, identity) => {
                    add!(ParameterMemoryEffect::Consume);
                    add!(ParameterMemoryEffect::Write);
                }
                Operation::ReturnObject { object }
                    if identities_may_alias(state, *object, identity) =>
                {
                    add!(ParameterMemoryEffect::Escape);
                }
                Operation::Call { callable, arguments, .. } => {
                    let contract = self
                        .module
                        .functions
                        .iter()
                        .find(|candidate| candidate.name == *callable)
                        .map(|candidate| &candidate.contract)
                        .or_else(|| self.module.imports.get(callable));
                    if let Some(contract) = contract {
                        for (argument, parameter) in arguments.iter().zip(&contract.parameters) {
                            if matches!(argument, CallArgument::Object(actual) if identities_may_alias(state, *actual, identity))
                            {
                                observed = observed.join(parameter.effects);
                            }
                        }
                    }
                }
                Operation::HostCall { import, arguments, .. } => {
                    if let Some(contract) = self.module.host_imports.get(import) {
                        for (argument, parameter) in arguments.iter().zip(&contract.parameters) {
                            if matches!(argument, HostArgument::Buffer(place) if identities_may_alias(state, place.object.identity, identity))
                            {
                                observed = observed.join(parameter.effects);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        observed
    }

    fn validate_operation(
        &mut self,
        function: &CandidateFunction,
        operation_index: usize,
        operation: &Operation,
        state: &mut State,
    ) -> Result<(), ValidationError> {
        let at = Some(operation_index);
        match operation {
            Operation::DefineScalar { definition, value } => {
                self.define(state, *definition, function, at)?;
                state.scalars.insert(*definition, *value);
            }
            Operation::Alloc { result } => {
                self.validate_object_claim(result, function, at)?;
                self.define(state, result.identity.root, function, at)?;
                if !matches!(result.provenance, Provenance::Fresh) {
                    return Err(self.function_error(
                        RuleId::UniqueAllocation,
                        function,
                        at,
                        "allocation result must claim Fresh provenance",
                    ));
                }
                self.validate_extent(&result.extent, state, function, at)?;
                self.validate_extent_against_layout(result, state, function, at)?;
                if state
                    .objects
                    .insert(
                        result.identity,
                        ObjectState { object: result.clone(), ownership: Ownership::Unique },
                    )
                    .is_some()
                {
                    return Err(self.function_error(
                        RuleId::UniqueAllocation,
                        function,
                        at,
                        "allocation duplicated an existing object generation",
                    ));
                }
                state.minted.insert(result.identity);
            }
            Operation::Project { object, place } => {
                self.require_live(state, *object, function, at)?;
                self.validate_place(place, state, function, at)?;
                if place.object.identity != *object {
                    return Err(self.function_error(
                        RuleId::ProjectedLayout,
                        function,
                        at,
                        "projection source and place object differ",
                    ));
                }
            }
            Operation::CheckedIndex { object, index, length_source, place } => {
                self.require_scalar(state, *index, function, at)?;
                self.require_scalar(state, *length_source, function, at)?;
                self.require_live(state, *object, function, at)?;
                self.validate_place(place, state, function, at)?;
                if place.object.identity != *object {
                    return Err(self.function_error(
                        RuleId::ExactRangeSource,
                        function,
                        at,
                        "checked index uses a different object generation",
                    ));
                }
            }
            Operation::ProvenIndex { object, index, length_source, place } => {
                self.require_scalar(state, *index, function, at)?;
                self.require_scalar(state, *length_source, function, at)?;
                self.require_live(state, *object, function, at)?;
                let _ = place;
                return Err(self.function_unsupported(
                    UnsupportedForm::ProvenIndex,
                    function,
                    at,
                    "unchecked ProvenIndex is unsupported until final CFG dominance facts exist",
                ));
            }
            Operation::Load { place } => {
                if place.access != AccessMode::Read {
                    return Err(self.function_error(
                        RuleId::ObjectRange,
                        function,
                        at,
                        "load requires a read place",
                    ));
                }
                self.validate_place(place, state, function, at)?;
            }
            Operation::Store { place } => {
                if place.access != AccessMode::Write {
                    return Err(self.function_error(
                        RuleId::ObjectRange,
                        function,
                        at,
                        "store requires a write place",
                    ));
                }
                let object = self.validate_place(place, state, function, at)?;
                if !matches!(object.ownership, Ownership::Unique | Ownership::BorrowedWrite) {
                    return Err(self.function_error(
                        RuleId::ConsumeOnce,
                        function,
                        at,
                        "store requires unique or exclusive borrowed authority",
                    ));
                }
            }
            Operation::Copy { source, destination, kind } => {
                let source_state = self.validate_place(source, state, function, at)?;
                let destination_state = self.validate_place(destination, state, function, at)?;
                if source.access != AccessMode::Read || destination.access != AccessMode::Write {
                    return Err(self.function_error(
                        RuleId::BulkRanges,
                        function,
                        at,
                        "copy requires read source and write destination places",
                    ));
                }
                if !matches!(destination_state.ownership, Ownership::Unique | Ownership::BorrowedWrite)
                {
                    return Err(self.function_error(
                        RuleId::BulkRanges,
                        function,
                        at,
                        "copy destination lacks exclusive write authority",
                    ));
                }
                if *kind == CopyKind::Disjoint
                    && source_state.object.identity == destination_state.object.identity
                    && ranges_overlap(&source.range, &destination.range, state, self.budget.expression_depth)
                        .unwrap_or(true)
                {
                    return Err(self.function_error(
                        RuleId::BulkRanges,
                        function,
                        at,
                        "disjoint copy ranges overlap or cannot be proved disjoint",
                    ));
                }
            }
            Operation::Fill { destination } => {
                let destination_state = self.validate_place(destination, state, function, at)?;
                if destination.access != AccessMode::Write
                    || !matches!(
                        destination_state.ownership,
                        Ownership::Unique | Ownership::BorrowedWrite
                    )
                {
                    return Err(self.function_error(
                        RuleId::BulkRanges,
                        function,
                        at,
                        "fill requires an exclusively writable destination",
                    ));
                }
            }
            Operation::Retain { object } => {
                let object_state = self.require_live_mut(state, *object, function, at)?;
                if matches!(
                    object_state.ownership,
                    Ownership::BorrowedRead | Ownership::BorrowedWrite
                ) {
                    return Err(self.function_error(
                        RuleId::ConsumeOnce,
                        function,
                        at,
                        "a borrowed token cannot be promoted to retained Shared authority",
                    ));
                }
                object_state.ownership = Ownership::Shared;
            }
            Operation::Release { object } | Operation::Move { object } => {
                let object_state = self.require_live(state, *object, function, at)?;
                if object_state.ownership != Ownership::Unique
                    || !matches!(object_state.object.provenance, Provenance::Fresh)
                {
                    return Err(self.function_error(
                        RuleId::DestructiveProvenance,
                        function,
                        at,
                        "destructive operation requires one fresh unique generation",
                    ));
                }
                self.consume_alias_group(state, *object);
            }
            Operation::Reuse { old, new } => {
                let old_state = self.require_live(state, *old, function, at)?;
                if old_state.ownership != Ownership::Unique
                    || !matches!(old_state.object.provenance, Provenance::Fresh)
                {
                    return Err(self.function_error(
                        RuleId::DestructiveProvenance,
                        function,
                        at,
                        "reuse requires one fresh unique input generation",
                    ));
                }
                if old.root != new.identity.root || old.generation == new.identity.generation {
                    return Err(self.function_error(
                        RuleId::LiveGeneration,
                        function,
                        at,
                        "reuse must retain the root and mint a distinct generation",
                    ));
                }
                self.consume_alias_group(state, *old);
                self.validate_object_claim(new, function, at)?;
                self.validate_extent(&new.extent, state, function, at)?;
                self.validate_extent_against_layout(new, state, function, at)?;
                if !matches!(new.provenance, Provenance::Fresh)
                    || state
                        .objects
                        .insert(
                            new.identity,
                            ObjectState { object: new.clone(), ownership: Ownership::Unique },
                        )
                        .is_some()
                {
                    return Err(self.function_error(
                        RuleId::UniqueAllocation,
                        function,
                        at,
                        "reuse result must be one new fresh generation",
                    ));
                }
                state.minted.insert(new.identity);
            }
            Operation::Call { callable, arguments, result } => {
                let contract = self
                    .module
                    .functions
                    .iter()
                    .find(|candidate| candidate.name == *callable)
                    .map(|candidate| &candidate.contract)
                    .or_else(|| self.module.imports.get(callable))
                    .ok_or_else(|| {
                        self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            at,
                            format!("call `{callable}` has no checked module contract"),
                        )
                    })?;
                self.validate_call(
                    function,
                    at,
                    callable,
                    contract,
                    arguments,
                    result.as_ref(),
                    state,
                )?;
            }
            Operation::HostCall { import, arguments, result } => {
                let Some(contract) = self.module.host_imports.get(import) else {
                    return Err(self.function_error(
                        RuleId::HostBufferAbi,
                        function,
                        at,
                        format!("host import `{import}` has no authenticated contract"),
                    ));
                };
                let mut call_arguments = Vec::with_capacity(arguments.len());
                for (index, argument) in arguments.iter().enumerate() {
                    match argument {
                        HostArgument::Scalar(definition) => {
                            self.require_scalar(state, *definition, function, at)?;
                            call_arguments.push(CallArgument::Scalar(*definition));
                        }
                        HostArgument::Buffer(place) => {
                            self.validate_place(place, state, function, at)?;
                            let Some(parameter) = contract.parameters.get(index) else {
                                return Err(self.function_error(
                                    RuleId::HostBufferAbi,
                                    function,
                                    at,
                                    format!("host import `{import}` has too many arguments"),
                                ));
                            };
                            let writes = parameter
                                .effects
                                .contains(ParameterMemoryEffect::Write);
                            if (writes && place.access != AccessMode::Write)
                                || (!writes && place.access != AccessMode::Read)
                            {
                                return Err(self.function_error(
                                    RuleId::HostBufferAbi,
                                    function,
                                    at,
                                    format!(
                                        "host import `{import}` buffer {index} access contradicts its contract"
                                    ),
                                ));
                            }
                            call_arguments.push(CallArgument::Object(place.object.identity));
                        }
                    }
                }
                self.validate_call(
                    function,
                    at,
                    import,
                    contract,
                    &call_arguments,
                    result.as_ref(),
                    state,
                )?;
            }
            Operation::Join { result, incoming } => {
                let _ = (result, incoming);
                return Err(self.function_unsupported(
                    UnsupportedForm::ControlFlowJoin,
                    function,
                    at,
                    "control-flow joins are unsupported until the final CFG fact graph exists",
                ));
            }
            Operation::Loop { entry, back_edges } => {
                let _ = (entry, back_edges);
                return Err(self.function_unsupported(
                    UnsupportedForm::Loop,
                    function,
                    at,
                    "loop fixpoints are unsupported until the final CFG fact graph exists",
                ));
            }
            Operation::ReturnObject { object } => {
                if state.returned.is_some() {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        at,
                        "straight-line report candidate has multiple object returns",
                    ));
                }
                let returned = {
                    let returned = self.require_live_mut(state, *object, function, at)?;
                    let returned_object = ReturnedObject {
                        object: returned.object.clone(),
                        ownership: returned.ownership,
                    };
                    returned.ownership = match returned.ownership {
                        Ownership::Unique => Ownership::Consumed,
                        Ownership::BorrowedRead
                        | Ownership::BorrowedWrite
                        | Ownership::Shared => Ownership::Shared,
                        Ownership::Consumed => unreachable!(),
                    };
                    returned_object
                };
                state.returned = Some(returned);
            }
            Operation::ReferenceCastFromScalar => {
                return Err(self.function_error(
                    RuleId::ReferenceSeparation,
                    function,
                    at,
                    "linear-memory scalar cannot become a Wasm reference",
                ));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_call(
        &self,
        function: &CandidateFunction,
        at: Option<usize>,
        callable: &str,
        contract: &CallableMemoryContract,
        arguments: &[CallArgument],
        result: Option<&ObjectRef>,
        state: &mut State,
    ) -> Result<(), ValidationError> {
        if arguments.len() != contract.parameters.len() || !contract.result_effects_are_consistent()
        {
            return Err(self.function_error(
                RuleId::CheckedCallableContract,
                function,
                at,
                format!("call `{callable}` arguments contradict its contract"),
            ));
        }
        match &contract.result {
            CallableMemoryResult::Aliases { projection, .. } if !projection.steps().is_empty() => {
                return Err(self.function_unsupported(
                    UnsupportedForm::NonRootAliasResult,
                    function,
                    at,
                    format!("call `{callable}` has a non-root alias result"),
                ));
            }
            CallableMemoryResult::Shared(_) => {
                return Err(self.function_unsupported(
                    UnsupportedForm::SharedResult,
                    function,
                    at,
                    format!("call `{callable}` has a shared result set"),
                ));
            }
            _ => {}
        }
        let mut occurrences = BTreeMap::<ObjectIdentity, Vec<usize>>::new();
        for (index, argument) in arguments.iter().enumerate() {
            if let CallArgument::Object(identity) = argument {
                occurrences.entry(*identity).or_default().push(index);
            }
        }
        for positions in occurrences.values().filter(|positions| positions.len() > 1) {
            if positions.iter().any(|index| {
                let parameter = &contract.parameters[*index];
                matches!(
                    parameter.entry,
                    ParameterMemoryEntry::Unique | ParameterMemoryEntry::BorrowedWrite
                ) || parameter.effects.contains(ParameterMemoryEffect::Write)
                    || parameter.effects.contains(ParameterMemoryEffect::Consume)
            }) {
                return Err(self.function_error(
                    RuleId::CheckedCallableContract,
                    function,
                    at,
                    format!("call `{callable}` duplicates an exclusively used object argument"),
                ));
            }
        }

        for (index, (argument, parameter)) in arguments.iter().zip(&contract.parameters).enumerate()
        {
            match (argument, parameter.entry) {
                (CallArgument::Scalar(definition), ParameterMemoryEntry::NoMemory) => {
                    self.require_scalar(state, *definition, function, at)?;
                }
                (CallArgument::Scalar(definition), ParameterMemoryEntry::Unknown) => {
                    self.require_scalar(state, *definition, function, at)?;
                }
                (CallArgument::Object(identity), ParameterMemoryEntry::Unknown) => {
                    self.require_live(state, *identity, function, at)?;
                    // Unknown is lattice top and may consume the argument. No
                    // later operation may retain unique or borrowed authority.
                    self.consume_alias_group(state, *identity);
                }
                (CallArgument::Object(_), ParameterMemoryEntry::NoMemory)
                | (CallArgument::Scalar(_), _) => {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        at,
                        format!("call `{callable}` parameter {index} has incompatible entry"),
                    ));
                }
                (CallArgument::Object(identity), entry) => {
                    let consumes = parameter.effects.contains(ParameterMemoryEffect::Consume);
                    {
                        let object = self.require_live_mut(state, *identity, function, at)?;
                        if parameter.layout != Some(object.object.layout)
                            || parameter.extent.as_ref().is_none_or(|extent| {
                                !contract_extent_matches(
                                    extent,
                                    &object.object.extent,
                                    arguments,
                                )
                            })
                        {
                            return Err(self.function_error(
                                RuleId::CheckedCallableContract,
                                function,
                                at,
                                format!(
                                    "call `{callable}` parameter {index} layout or extent contradicts its checked signature"
                                ),
                            ));
                        }
                        let entry_ok = match entry {
                            ParameterMemoryEntry::BorrowedRead => {
                                object.ownership != Ownership::Consumed
                            }
                            ParameterMemoryEntry::BorrowedWrite => matches!(
                                object.ownership,
                                Ownership::Unique | Ownership::BorrowedWrite
                            ),
                            ParameterMemoryEntry::Shared => {
                                object.ownership != Ownership::Consumed
                            }
                            ParameterMemoryEntry::Unique => {
                                object.ownership == Ownership::Unique
                            }
                            ParameterMemoryEntry::NoMemory | ParameterMemoryEntry::Unknown => {
                                unreachable!()
                            }
                        };
                        if !entry_ok
                            || (parameter.effects.contains(ParameterMemoryEffect::Write)
                                && !matches!(
                                    object.ownership,
                                    Ownership::Unique | Ownership::BorrowedWrite
                                ))
                            || (consumes && object.ownership != Ownership::Unique)
                        {
                            return Err(self.function_error(
                                RuleId::CheckedCallableContract,
                                function,
                                at,
                                format!(
                                    "call `{callable}` parameter {index} lacks required authority"
                                ),
                            ));
                        }
                        if !consumes
                            && (parameter.effects.contains(ParameterMemoryEffect::Retain)
                                || parameter.effects.contains(ParameterMemoryEffect::Escape))
                        {
                            object.ownership = Ownership::Shared;
                        }
                    }
                    if consumes {
                        self.consume_alias_group(state, *identity);
                    }
                }
            }
        }

        match (&contract.result, result) {
            (CallableMemoryResult::NoMemory, None) => {}
            (CallableMemoryResult::Unknown, None) => {}
            (CallableMemoryResult::Unknown, Some(_)) => {
                return Err(self.function_error(
                    RuleId::UnknownResultIsNotFresh,
                    function,
                    at,
                    format!(
                        "unknown result of `{callable}` is unsupported as an object fact"
                    ),
                ));
            }
            (CallableMemoryResult::Fresh { layout, extent }, Some(result)) => {
                if result.layout != *layout
                    || !contract_extent_matches(extent, &result.extent, arguments)
                {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        at,
                        format!("fresh result of `{callable}` contradicts layout or extent"),
                    ));
                }
                self.validate_object_claim(result, function, at)?;
                self.validate_extent(&result.extent, state, function, at)?;
                self.validate_extent_against_layout(result, state, function, at)?;
                self.define(state, result.identity.root, function, at)?;
                if !matches!(result.provenance, Provenance::Fresh)
                    || state.objects.contains_key(&result.identity)
                {
                    return Err(self.function_error(
                        RuleId::UniqueAllocation,
                        function,
                        at,
                        format!("fresh result of `{callable}` did not mint one generation"),
                    ));
                }
                state.objects.insert(
                    result.identity,
                    ObjectState { object: result.clone(), ownership: Ownership::Unique },
                );
                state.minted.insert(result.identity);
            }
            (CallableMemoryResult::Aliases { parameter, projection }, Some(result)) => {
                let Some(CallArgument::Object(source)) = arguments.get(*parameter) else {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        at,
                        format!("alias result of `{callable}` has no source parameter"),
                    ));
                };
                if !projection.steps().is_empty() {
                    return Err(self.function_unsupported(
                        UnsupportedForm::NonRootAliasResult,
                        function,
                        at,
                        format!("alias result of `{callable}` is not a root alias"),
                    ));
                }
                let source_state = self.require_live(state, *source, function, at)?;
                if !matches!(
                    &result.provenance,
                    Provenance::Aliases { object, projection: actual }
                        if object == source
                            && actual.steps().is_empty()
                ) || result.identity.root == source.root
                    || result.identity.generation != source.generation
                    || source_state.object.layout != result.layout
                    || source_state.object.extent != result.extent
                {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        at,
                        format!("alias result of `{callable}` contradicts provenance"),
                    ));
                }
                self.define(state, result.identity.root, function, at)?;
                if state.objects.contains_key(&result.identity) {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        at,
                        format!("alias result of `{callable}` reuses a value definition"),
                    ));
                }
                self.require_live_mut(state, *source, function, at)?.ownership = Ownership::Shared;
                self.link_alias_group(state, *source, result.identity);
                state.objects.insert(
                    result.identity,
                    ObjectState { object: result.clone(), ownership: Ownership::Shared },
                );
            }
            (CallableMemoryResult::Shared(parameters), Some(result)) => {
                let expected: BTreeSet<_> = parameters
                    .iter()
                    .filter_map(|parameter| match arguments.get(*parameter) {
                        Some(CallArgument::Object(identity)) => Some(*identity),
                        _ => None,
                    })
                    .collect();
                if !matches!(&result.provenance, Provenance::MayAlias(actual) if actual == &expected)
                    || !expected.contains(&result.identity)
                {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        at,
                        format!("shared result of `{callable}` contradicts alias set"),
                    ));
                }
                for identity in expected {
                    self.require_live_mut(state, identity, function, at)?.ownership =
                        Ownership::Shared;
                }
            }
            _ => {
                return Err(self.function_error(
                    RuleId::CheckedCallableContract,
                    function,
                    at,
                    format!("call `{callable}` result contradicts its contract"),
                ));
            }
        }
        Ok(())
    }

    fn validate_object_claim(
        &self,
        object: &ObjectRef,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(), ValidationError> {
        if !self.module.layouts.roots().contains(&object.layout)
            || self.layouts.get(object.layout).is_none()
        {
            return Err(self.function_error(
                RuleId::CanonicalLayout,
                function,
                at,
                format!("unknown canonical LayoutId {}", object.layout),
            ));
        }
        if !object.provenance.has_nonempty_alias_set() {
            return Err(self.function_error(
                RuleId::DestructiveProvenance,
                function,
                at,
                "empty MayAlias set is invalid",
            ));
        }
        Ok(())
    }

    fn validate_place<'a>(
        &self,
        place: &Place,
        state: &'a State,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<&'a ObjectState, ValidationError> {
        self.validate_object_claim(&place.object, function, at)?;
        if !place.range_matches_object_generation() {
            return Err(self.function_error(
                RuleId::LiveGeneration,
                function,
                at,
                "place range names a different object generation",
            ));
        }
        let object = self.require_live(state, place.object.identity, function, at)?;
        if object.object.layout != place.object.layout || object.object.extent != place.object.extent {
            return Err(self.function_error(
                RuleId::ProjectedLayout,
                function,
                at,
                "place layout or extent disagrees with the live object",
            ));
        }
        self.validate_extent_against_layout(&place.object, state, function, at)?;
        let (derived_start, derived_length) =
            self.derive_projection(place, state, function, at)?;
        let claimed_start = self.evaluate_size(&place.range.start, state, function, at, 0)?;
        let claimed_length = self.evaluate_size(&place.range.length, state, function, at, 0)?;
        if claimed_start != derived_start || claimed_length != derived_length {
            return Err(self.function_error(
                RuleId::ProjectedLayout,
                function,
                at,
                format!(
                    "candidate range {claimed_start}+{claimed_length} disagrees with descriptor-derived {derived_start}+{derived_length}"
                ),
            ));
        }
        self.validate_range(&place.range, &place.object.extent, state, function, at)?;
        Ok(object)
    }

    fn derive_projection(
        &self,
        place: &Place,
        state: &State,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(u128, u128), ValidationError> {
        let mut layout = place.object.layout;
        let mut offset = 0_u128;
        let mut length = self.layout_extent(layout, &place.object.extent, state, function, at)?;
        for (step_index, step) in place.projection.steps().iter().enumerate() {
            let descriptor = self.layouts.get(layout).ok_or_else(|| {
                self.function_error(
                    RuleId::CanonicalLayout,
                    function,
                    at,
                    format!("projection references unknown LayoutId {layout}"),
                )
            })?;
            match step {
                ProjectionStep::Header { component } => {
                    let HeaderLayout::PackedList {
                        length_offset,
                        capacity_offset,
                        ..
                    } = descriptor.header()
                    else {
                        return Err(self.function_error(
                            RuleId::ProjectedLayout,
                            function,
                            at,
                            "header projection requires a descriptor-defined header",
                        ));
                    };
                    let component_offset = match component {
                        0 => length_offset,
                        1 => capacity_offset,
                        _ => {
                            return Err(self.function_error(
                                RuleId::ProjectedLayout,
                                function,
                                at,
                                "unknown descriptor header component",
                            ));
                        }
                    };
                    if step_index + 1 != place.projection.steps().len() {
                        return Err(self.function_error(
                            RuleId::ProjectedLayout,
                            function,
                            at,
                            "a scalar header component cannot be projected further",
                        ));
                    }
                    offset = offset.checked_add(u128::from(component_offset)).ok_or_else(|| {
                        self.function_error(
                            RuleId::NonWrappingSize,
                            function,
                            at,
                            "header projection offset overflowed",
                        )
                    })?;
                    length = 4;
                }
                ProjectionStep::Field { index } => {
                    let field = descriptor.fields().get(*index as usize).ok_or_else(|| {
                        self.function_error(
                            RuleId::ProjectedLayout,
                            function,
                            at,
                            format!("field projection index {index} is outside the descriptor"),
                        )
                    })?;
                    offset = offset.checked_add(u128::from(field.offset())).ok_or_else(|| {
                        self.function_error(
                            RuleId::NonWrappingSize,
                            function,
                            at,
                            "field projection offset overflowed",
                        )
                    })?;
                    match field.kind() {
                        FieldKind::Scalar(kind) => {
                            if step_index + 1 != place.projection.steps().len() {
                                return Err(self.function_error(
                                    RuleId::ProjectedLayout,
                                    function,
                                    at,
                                    "a scalar field cannot be projected further",
                                ));
                            }
                            length = u128::from(kind.size());
                        }
                        FieldKind::Inline(child) => {
                            layout = child;
                            length = self.layout_extent(
                                child,
                                &Extent::Exact(SizeExpr::Constant(u64::MAX)),
                                state,
                                function,
                                at,
                            )?;
                        }
                    }
                }
                ProjectionStep::Element { index } => {
                    let LayoutKind::PackedList { element, .. } = descriptor.kind() else {
                        return Err(self.function_error(
                            RuleId::ProjectedLayout,
                            function,
                            at,
                            "element projection requires a packed-list descriptor",
                        ));
                    };
                    let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
                        unreachable!("packed lists have dynamic descriptors")
                    };
                    let index = self.require_nonnegative_scalar(state, *index, function, at)?;
                    offset = offset
                        .checked_add(u128::from(base))
                        .and_then(|base| base.checked_add(index.checked_mul(u128::from(stride))?))
                        .ok_or_else(|| {
                            self.function_error(
                                RuleId::NonWrappingSize,
                                function,
                                at,
                                "element projection arithmetic overflowed",
                            )
                        })?;
                    layout = *element;
                    length = u128::from(stride);
                }
                ProjectionStep::Slice {
                    start,
                    length: slice_length,
                } => {
                    let LayoutKind::PackedList { element, .. } = descriptor.kind() else {
                        return Err(self.function_error(
                            RuleId::ProjectedLayout,
                            function,
                            at,
                            "slice projection requires a packed-list descriptor",
                        ));
                    };
                    let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
                        unreachable!("packed lists have dynamic descriptors")
                    };
                    let start = self.require_nonnegative_scalar(state, *start, function, at)?;
                    let count =
                        self.require_nonnegative_scalar(state, *slice_length, function, at)?;
                    offset = offset
                        .checked_add(u128::from(base))
                        .and_then(|base| base.checked_add(start.checked_mul(u128::from(stride))?))
                        .ok_or_else(|| {
                            self.function_error(
                                RuleId::NonWrappingSize,
                                function,
                                at,
                                "slice projection start overflowed",
                            )
                        })?;
                    length = count.checked_mul(u128::from(stride)).ok_or_else(|| {
                        self.function_error(
                            RuleId::NonWrappingSize,
                            function,
                            at,
                            "slice projection length overflowed",
                        )
                    })?;
                    layout = *element;
                }
            }
        }
        let _ = layout;
        Ok((offset, length))
    }

    fn layout_extent(
        &self,
        layout: LayoutId,
        extent: &Extent,
        state: &State,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<u128, ValidationError> {
        let descriptor = self.layouts.get(layout).ok_or_else(|| {
            self.function_error(
                RuleId::CanonicalLayout,
                function,
                at,
                format!("unknown canonical LayoutId {layout}"),
            )
        })?;
        match descriptor.size() {
            LayoutSize::Fixed(size) => Ok(u128::from(size)),
            LayoutSize::Dynamic { .. } => match extent {
                Extent::Exact(size) => self.evaluate_size(size, state, function, at, 0),
                Extent::Counted { header, count, stride } => {
                    let header = self.evaluate_size(header, state, function, at, 0)?;
                    let count = self.require_nonnegative_scalar(state, *count, function, at)?;
                    let stride = self.evaluate_size(stride, state, function, at, 0)?;
                    header
                        .checked_add(count.checked_mul(stride).ok_or_else(|| {
                            self.function_error(
                                RuleId::NonWrappingSize,
                                function,
                                at,
                                "dynamic layout payload size overflowed",
                            )
                        })?)
                        .ok_or_else(|| {
                            self.function_error(
                                RuleId::NonWrappingSize,
                                function,
                                at,
                                "dynamic layout extent overflowed",
                            )
                        })
                }
                Extent::Unknown => Err(self.function_error(
                    RuleId::CanonicalLayout,
                    function,
                    at,
                    "dynamic layout requires an exact counted extent",
                )),
            },
        }
    }

    fn validate_extent_against_layout(
        &self,
        object: &ObjectRef,
        state: &State,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(), ValidationError> {
        let descriptor = self.layouts.get(object.layout).ok_or_else(|| {
            self.function_error(
                RuleId::CanonicalLayout,
                function,
                at,
                format!("unknown canonical LayoutId {}", object.layout),
            )
        })?;
        match (descriptor.size(), &object.extent) {
            (LayoutSize::Fixed(expected), Extent::Exact(actual))
                if self.evaluate_size(actual, state, function, at, 0)?
                    == u128::from(expected) =>
            {
                Ok(())
            }
            (
                LayoutSize::Dynamic { base, stride },
                Extent::Counted { header, count, stride: actual_stride },
            ) if self.evaluate_size(header, state, function, at, 0)? == u128::from(base)
                && self.evaluate_size(actual_stride, state, function, at, 0)?
                    == u128::from(stride)
                && self.require_nonnegative_scalar(state, *count, function, at).is_ok() =>
            {
                Ok(())
            }
            _ => Err(self.function_error(
                RuleId::CanonicalLayout,
                function,
                at,
                "object extent contradicts its canonical layout descriptor",
            )),
        }
    }

    fn validate_range(
        &self,
        range: &ByteRange,
        extent: &Extent,
        state: &State,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(), ValidationError> {
        let start = self.evaluate_size(&range.start, state, function, at, 0)?;
        let length = self.evaluate_size(&range.length, state, function, at, 0)?;
        let Some(end) = start.checked_add(length) else {
            return Err(self.function_error(
                RuleId::NonWrappingSize,
                function,
                at,
                "range start plus length overflowed mathematical validation domain",
            ));
        };
        let extent = match extent {
            Extent::Unknown => {
                return Err(self.function_error(
                    RuleId::ObjectRange,
                    function,
                    at,
                    "unknown extent cannot prove an access in bounds",
                ));
            }
            Extent::Exact(size) => self.evaluate_size(size, state, function, at, 0)?,
            Extent::Counted { header, count, stride } => {
                let header = self.evaluate_size(header, state, function, at, 0)?;
                let count = self.require_nonnegative_scalar(state, *count, function, at)?;
                let stride = self.evaluate_size(stride, state, function, at, 0)?;
                header
                    .checked_add(count.checked_mul(stride).ok_or_else(|| {
                        self.function_error(
                            RuleId::NonWrappingSize,
                            function,
                            at,
                            "count times stride overflowed mathematical validation domain",
                        )
                    })?)
                    .ok_or_else(|| {
                        self.function_error(
                            RuleId::NonWrappingSize,
                            function,
                            at,
                            "header plus payload overflowed mathematical validation domain",
                        )
                    })?
            }
        };
        if end > extent {
            return Err(self.function_error(
                RuleId::ObjectRange,
                function,
                at,
                format!("byte range {start}..{end} exceeds object extent {extent}"),
            ));
        }
        Ok(())
    }

    fn validate_extent(
        &self,
        extent: &Extent,
        state: &State,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(), ValidationError> {
        match extent {
            Extent::Unknown => Err(self.function_error(
                RuleId::NonWrappingSize,
                function,
                at,
                "allocation or fresh result cannot have unknown extent",
            )),
            Extent::Exact(size) => {
                self.evaluate_size(size, state, function, at, 0)?;
                Ok(())
            }
            Extent::Counted { header, count, stride } => {
                let header = self.evaluate_size(header, state, function, at, 0)?;
                let count = self.require_nonnegative_scalar(state, *count, function, at)?;
                let stride = self.evaluate_size(stride, state, function, at, 0)?;
                count
                    .checked_mul(stride)
                    .and_then(|payload| header.checked_add(payload))
                    .ok_or_else(|| {
                        self.function_error(
                            RuleId::NonWrappingSize,
                            function,
                            at,
                            "extent arithmetic overflowed mathematical validation domain",
                        )
                    })?;
                Ok(())
            }
        }
    }

    fn evaluate_size(
        &self,
        expression: &SizeExpr,
        state: &State,
        function: &CandidateFunction,
        at: Option<usize>,
        depth: usize,
    ) -> Result<u128, ValidationError> {
        if depth >= self.budget.expression_depth {
            return Err(self.function_error(
                RuleId::BudgetExhaustionFailsClosed,
                function,
                at,
                "size-expression depth budget exhausted",
            ));
        }
        let next = depth + 1;
        match expression {
            SizeExpr::Constant(value) => Ok(u128::from(*value)),
            SizeExpr::Definition(definition) => {
                self.require_nonnegative_scalar(state, *definition, function, at)
            }
            SizeExpr::Add(lhs, rhs) => self
                .evaluate_size(lhs, state, function, at, next)?
                .checked_add(self.evaluate_size(rhs, state, function, at, next)?)
                .ok_or_else(|| {
                    self.function_error(
                        RuleId::NonWrappingSize,
                        function,
                        at,
                        "size addition overflowed mathematical validation domain",
                    )
                }),
            SizeExpr::Multiply(lhs, rhs) => self
                .evaluate_size(lhs, state, function, at, next)?
                .checked_mul(self.evaluate_size(rhs, state, function, at, next)?)
                .ok_or_else(|| {
                    self.function_error(
                        RuleId::NonWrappingSize,
                        function,
                        at,
                        "size multiplication overflowed mathematical validation domain",
                    )
                }),
            SizeExpr::NarrowU32(value) => {
                let value = self.evaluate_size(value, state, function, at, next)?;
                if value > u128::from(u32::MAX) {
                    return Err(self.function_error(
                        RuleId::NonWrappingSize,
                        function,
                        at,
                        "checked narrowing exceeds the Wasm u32 address domain",
                    ));
                }
                Ok(value)
            }
        }
    }

    fn define(
        &self,
        state: &mut State,
        definition: DefinitionId,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(), ValidationError> {
        if !state.definitions.insert(definition) {
            return Err(self.function_error(
                RuleId::CheckedCallableContract,
                function,
                at,
                format!("DefinitionId {} is not unique", definition.get()),
            ));
        }
        Ok(())
    }

    fn require_scalar(
        &self,
        state: &State,
        definition: DefinitionId,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<i128, ValidationError> {
        state.scalars.get(&definition).copied().ok_or_else(|| {
            self.function_error(
                RuleId::ExactRangeSource,
                function,
                at,
                format!("DefinitionId {} has no exact scalar fact", definition.get()),
            )
        })
    }

    fn require_nonnegative_scalar(
        &self,
        state: &State,
        definition: DefinitionId,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<u128, ValidationError> {
        let value = self.require_scalar(state, definition, function, at)?;
        u128::try_from(value).map_err(|_| {
            self.function_error(
                RuleId::NonWrappingSize,
                function,
                at,
                format!("DefinitionId {} is negative", definition.get()),
            )
        })
    }

    fn require_live<'a>(
        &self,
        state: &'a State,
        identity: ObjectIdentity,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<&'a ObjectState, ValidationError> {
        let object = state.objects.get(&identity).ok_or_else(|| {
            self.function_error(
                RuleId::LiveGeneration,
                function,
                at,
                format!(
                    "unknown object generation {}:{}",
                    identity.root.get(),
                    identity.generation.get()
                ),
            )
        })?;
        if object.ownership == Ownership::Consumed {
            return Err(self.function_error(
                RuleId::ConsumeOnce,
                function,
                at,
                "consumed generation reached a later operation",
            ));
        }
        Ok(object)
    }

    fn require_live_mut<'a>(
        &self,
        state: &'a mut State,
        identity: ObjectIdentity,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<&'a mut ObjectState, ValidationError> {
        let object = state.objects.get_mut(&identity).ok_or_else(|| {
            self.function_error(
                RuleId::LiveGeneration,
                function,
                at,
                "unknown object generation",
            )
        })?;
        if object.ownership == Ownership::Consumed {
            return Err(self.function_error(
                RuleId::ConsumeOnce,
                function,
                at,
                "consumed generation reached a later operation",
            ));
        }
        Ok(object)
    }

    fn link_alias_group(
        &self,
        state: &mut State,
        left: ObjectIdentity,
        right: ObjectIdentity,
    ) {
        let mut group = BTreeSet::from([left, right]);
        let mut pending = vec![left, right];
        while let Some(identity) = pending.pop() {
            if let Some(neighbors) = state.aliases.get(&identity) {
                for neighbor in neighbors {
                    if group.insert(*neighbor) {
                        pending.push(*neighbor);
                    }
                }
            }
        }
        for identity in &group {
            let neighbors = state.aliases.entry(*identity).or_default();
            neighbors.extend(group.iter().copied().filter(|other| other != identity));
        }
    }

    fn consume_alias_group(&self, state: &mut State, identity: ObjectIdentity) {
        let mut group = BTreeSet::from([identity]);
        let mut pending = vec![identity];
        while let Some(current) = pending.pop() {
            if let Some(neighbors) = state.aliases.get(&current) {
                for neighbor in neighbors {
                    if group.insert(*neighbor) {
                        pending.push(*neighbor);
                    }
                }
            }
        }
        for member in group {
            if let Some(object) = state.objects.get_mut(&member) {
                object.ownership = Ownership::Consumed;
            }
        }
    }

    fn observe_facts(
        &mut self,
        state: &State,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(), ValidationError> {
        self.peak_facts = self.peak_facts.max(state.fact_count());
        if self.peak_facts > self.budget.facts {
            return Err(self.function_error(
                RuleId::BudgetExhaustionFailsClosed,
                function,
                at,
                "fact budget exhausted",
            ));
        }
        Ok(())
    }

    fn module_error(&self, rule: RuleId, message: impl Into<String>) -> ValidationError {
        ValidationError {
            diagnostic: ValidationDiagnostic::Rule(rule),
            function: None,
            operation: None,
            message: message.into(),
        }
    }

    fn module_unsupported(
        &self,
        form: UnsupportedForm,
        message: impl Into<String>,
    ) -> ValidationError {
        ValidationError {
            diagnostic: ValidationDiagnostic::Unsupported(form),
            function: None,
            operation: None,
            message: message.into(),
        }
    }

    fn function_error(
        &self,
        rule: RuleId,
        function: &CandidateFunction,
        operation: Option<usize>,
        message: impl Into<String>,
    ) -> ValidationError {
        ValidationError {
            diagnostic: ValidationDiagnostic::Rule(rule),
            function: Some(function.name.clone()),
            operation,
            message: message.into(),
        }
    }

    fn function_unsupported(
        &self,
        form: UnsupportedForm,
        function: &CandidateFunction,
        operation: Option<usize>,
        message: impl Into<String>,
    ) -> ValidationError {
        ValidationError {
            diagnostic: ValidationDiagnostic::Unsupported(form),
            function: Some(function.name.clone()),
            operation,
            message: message.into(),
        }
    }
}

fn contract_extent_is_constant(extent: &ContractExtent) -> bool {
    fn size_is_constant(size: &ContractSizeExpr) -> bool {
        match size {
            ContractSizeExpr::Constant(_) => true,
            ContractSizeExpr::Parameter(_) => false,
            ContractSizeExpr::Add(left, right) | ContractSizeExpr::Multiply(left, right) => {
                size_is_constant(left) && size_is_constant(right)
            }
        }
    }
    match extent {
        ContractExtent::Exact(size) => size_is_constant(size),
        ContractExtent::Counted { header, count, stride } => {
            size_is_constant(header) && size_is_constant(count) && size_is_constant(stride)
        }
    }
}

fn identities_may_alias(
    state: &State,
    left: ObjectIdentity,
    right: ObjectIdentity,
) -> bool {
    left == right
        || state
            .aliases
            .get(&left)
            .is_some_and(|neighbors| neighbors.contains(&right))
}

fn contract_extent_matches(
    contract: &ContractExtent,
    actual: &Extent,
    arguments: &[CallArgument],
) -> bool {
    match (contract, actual) {
        (ContractExtent::Exact(contract), Extent::Exact(actual)) => {
            contract_size_matches(contract, actual, arguments)
        }
        (
            ContractExtent::Counted { header, count, stride },
            Extent::Counted {
                header: actual_header,
                count: actual_count,
                stride: actual_stride,
            },
        ) => {
            contract_size_matches(header, actual_header, arguments)
                && contract_size_matches(
                    count,
                    &SizeExpr::Definition(*actual_count),
                    arguments,
                )
                && contract_size_matches(stride, actual_stride, arguments)
        }
        _ => false,
    }
}

fn contract_size_matches(
    contract: &ContractSizeExpr,
    actual: &SizeExpr,
    arguments: &[CallArgument],
) -> bool {
    match (contract, actual) {
        (ContractSizeExpr::Constant(expected), SizeExpr::Constant(actual)) => expected == actual,
        (ContractSizeExpr::Parameter(parameter), SizeExpr::Definition(actual)) => {
            matches!(arguments.get(*parameter), Some(CallArgument::Scalar(expected)) if expected == actual)
        }
        (ContractSizeExpr::Add(a, b), SizeExpr::Add(x, y))
        | (ContractSizeExpr::Multiply(a, b), SizeExpr::Multiply(x, y)) => {
            contract_size_matches(a, x, arguments)
                && contract_size_matches(b, y, arguments)
        }
        _ => false,
    }
}

fn ranges_overlap(
    left: &ByteRange,
    right: &ByteRange,
    state: &State,
    expression_depth: usize,
) -> Option<bool> {
    fn evaluate(expression: &SizeExpr, state: &State, remaining: usize) -> Option<u128> {
        if remaining == 0 {
            return None;
        }
        match expression {
            SizeExpr::Constant(value) => Some(u128::from(*value)),
            SizeExpr::Definition(definition) => {
                u128::try_from(*state.scalars.get(definition)?).ok()
            }
            SizeExpr::Add(lhs, rhs) => evaluate(lhs, state, remaining - 1)?
                .checked_add(evaluate(rhs, state, remaining - 1)?),
            SizeExpr::Multiply(lhs, rhs) => evaluate(lhs, state, remaining - 1)?
                .checked_mul(evaluate(rhs, state, remaining - 1)?),
            SizeExpr::NarrowU32(value) => {
                let value = evaluate(value, state, remaining - 1)?;
                (value <= u128::from(u32::MAX)).then_some(value)
            }
        }
    }
    let left_start = evaluate(&left.start, state, expression_depth)?;
    let right_start = evaluate(&right.start, state, expression_depth)?;
    let left_end = left_start.checked_add(evaluate(&left.length, state, expression_depth)?)?;
    let right_end = right_start.checked_add(evaluate(&right.length, state, expression_depth)?)?;
    Some(left_start < right_end && right_start < left_end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{
        ClosedTypeResolver, RcHeader, ReferenceKind, ResolvedNamed, ScalarKind,
    };
    use crate::safewir::{
        GenerationId, ParameterMemoryContract, ParameterMemoryEffects, Projection,
    };
    use witchy_syntax::ast::Type;

    struct Resolver;

    impl ClosedTypeResolver for Resolver {
        fn resolve_named<'a>(
            &'a self,
            name: &str,
            _arguments: &[Type],
        ) -> Option<ResolvedNamed<'a>> {
            Some(match name {
                "Int" => ResolvedNamed::Scalar(ScalarKind::Int),
                "List" => ResolvedNamed::PackedList { rc: RcHeader::Required },
                "String" => ResolvedNamed::Reference(ReferenceKind::Owning),
                _ => return None,
            })
        }
    }

    fn layouts() -> (LayoutBundle, LayoutId) {
        let mut interner = LayoutInterner::new();
        let id = interner
            .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
            .unwrap();
        (LayoutBundle::from_interner(&interner, [id]).unwrap(), id)
    }

    fn list_layouts() -> (LayoutBundle, LayoutId, u32, u32) {
        let mut interner = LayoutInterner::new();
        let id = interner
            .intern_type(
                &Type::Named(
                    "List".into(),
                    vec![Type::Named("Int".into(), Vec::new())],
                ),
                &Resolver,
            )
            .unwrap();
        let LayoutSize::Dynamic { base, stride } = interner.get(id).unwrap().size() else {
            panic!("list layout is dynamic")
        };
        (LayoutBundle::from_interner(&interner, [id]).unwrap(), id, base, stride)
    }

    fn tuple_layouts() -> (LayoutBundle, LayoutId) {
        let mut interner = LayoutInterner::new();
        let int = Type::Named("Int".into(), Vec::new());
        let id = interner
            .intern_type(&Type::Tuple(vec![int.clone(), int]), &Resolver)
            .unwrap();
        (LayoutBundle::from_interner(&interner, [id]).unwrap(), id)
    }

    fn layout() -> LayoutId {
        layouts().1
    }

    fn identity(root: u32, generation: u32) -> ObjectIdentity {
        ObjectIdentity {
            root: DefinitionId::new(root),
            generation: GenerationId::new(generation),
        }
    }

    fn object(root: u32, generation: u32, provenance: Provenance) -> ObjectRef {
        ObjectRef {
            identity: identity(root, generation),
            layout: layout(),
            extent: Extent::Exact(SizeExpr::Constant(8)),
            provenance,
        }
    }

    fn place(object: ObjectRef, access: AccessMode, start: u64, length: u64) -> Place {
        Place {
            range: ByteRange {
                object: object.identity,
                start: SizeExpr::Constant(start),
                length: SizeExpr::Constant(length),
            },
            object,
            projection: Projection::root(),
            access,
        }
    }

    fn no_memory_contract() -> CallableMemoryContract {
        CallableMemoryContract { parameters: Vec::new(), result: CallableMemoryResult::NoMemory }
    }

    fn object_parameter(
        entry: ParameterMemoryEntry,
        effects: ParameterMemoryEffects,
    ) -> ParameterMemoryContract {
        ParameterMemoryContract {
            entry,
            layout: Some(layout()),
            extent: Some(ContractExtent::Exact(ContractSizeExpr::Constant(8))),
            effects,
        }
    }

    fn identity_alias_function() -> CandidateFunction {
        let parameter = object(20, 1, Provenance::Unknown);
        CandidateFunction {
            name: "identity".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::BorrowedRead,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::Aliases {
                    parameter: 0,
                    projection: Default::default(),
                },
            },
            parameters: vec![ParameterBinding {
                definition: parameter.identity.root,
                object: Some(parameter.clone()),
            }],
            operations: vec![Operation::ReturnObject { object: parameter.identity }],
        }
    }

    fn add_identity_alias(module: &mut CandidateModule) -> ObjectRef {
        module.functions.push(identity_alias_function());
        let source = object(3, 1, Provenance::Fresh);
        let result = ObjectRef {
            identity: identity(11, source.identity.generation.get()),
            layout: source.layout,
            extent: source.extent.clone(),
            provenance: Provenance::Aliases {
                object: source.identity,
                projection: Projection::root(),
            },
        };
        module.functions[0].operations.push(Operation::Call {
            callable: "identity".into(),
            arguments: vec![CallArgument::Object(source.identity)],
            result: Some(result.clone()),
        });
        result
    }

    fn base_module() -> CandidateModule {
        let allocated = object(3, 1, Provenance::Fresh);
        let (layouts, _) = layouts();
        CandidateModule {
            checked_source_entries: 1,
            layouts,
            imports: BTreeMap::new(),
            host_imports: BTreeMap::new(),
            functions: vec![CandidateFunction {
                name: "main".into(),
                contract: no_memory_contract(),
                parameters: Vec::new(),
                operations: vec![
                    Operation::DefineScalar { definition: DefinitionId::new(1), value: 0 },
                    Operation::DefineScalar { definition: DefinitionId::new(2), value: 8 },
                    Operation::Alloc { result: allocated.clone() },
                    Operation::Load { place: place(allocated, AccessMode::Read, 0, 8) },
                ],
            }],
            contains_raw_memory: false,
            contains_raw_body: false,
            post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
        }
    }

    fn out_of_bounds_list_module() -> CandidateModule {
        let (layouts, layout, base, stride) = list_layouts();
        let object = ObjectRef {
            identity: identity(3, 1),
            layout,
            extent: Extent::Counted {
                header: SizeExpr::Constant(u64::from(base)),
                count: DefinitionId::new(2),
                stride: SizeExpr::Constant(u64::from(stride)),
            },
            provenance: Provenance::Fresh,
        };
        let mut access = place(
            object.clone(),
            AccessMode::Read,
            u64::from(base + 2 * stride),
            u64::from(stride),
        );
        access.projection =
            Projection::root().project(ProjectionStep::Element { index: DefinitionId::new(1) });
        CandidateModule {
            checked_source_entries: 1,
            layouts,
            imports: BTreeMap::new(),
            host_imports: BTreeMap::new(),
            functions: vec![CandidateFunction {
                name: "main".into(),
                contract: no_memory_contract(),
                parameters: Vec::new(),
                operations: vec![
                    Operation::DefineScalar { definition: DefinitionId::new(1), value: 2 },
                    Operation::DefineScalar { definition: DefinitionId::new(2), value: 1 },
                    Operation::Alloc { result: object },
                    Operation::Load { place: access },
                ],
            }],
            contains_raw_memory: false,
            contains_raw_body: false,
            post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
        }
    }

    fn projected_field_module() -> CandidateModule {
        let (layouts, layout) = tuple_layouts();
        let object = ObjectRef {
            identity: identity(3, 1),
            layout,
            extent: Extent::Exact(SizeExpr::Constant(16)),
            provenance: Provenance::Fresh,
        };
        let mut access = place(object.clone(), AccessMode::Read, 8, 8);
        access.projection = Projection::root().project(ProjectionStep::Field { index: 1 });
        CandidateModule {
            checked_source_entries: 1,
            layouts,
            imports: BTreeMap::new(),
            host_imports: BTreeMap::new(),
            functions: vec![CandidateFunction {
                name: "main".into(),
                contract: no_memory_contract(),
                parameters: Vec::new(),
                operations: vec![
                    Operation::Alloc { result: object },
                    Operation::Load { place: access },
                ],
            }],
            contains_raw_memory: false,
            contains_raw_body: false,
            post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
        }
    }

    fn assert_rule(module: CandidateModule, expected: RuleId) {
        let error = validate_candidate(&module, ValidationBudget::default()).unwrap_err();
        assert_eq!(
            error.diagnostic,
            ValidationDiagnostic::Rule(expected),
            "unexpected diagnostic: {error}"
        );
        assert!(error.to_string().starts_with(expected.code()));
    }

    fn assert_unsupported(module: CandidateModule, expected: UnsupportedForm) {
        let error = validate_candidate(&module, ValidationBudget::default()).unwrap_err();
        assert_eq!(
            error.diagnostic,
            ValidationDiagnostic::Unsupported(expected),
            "unexpected diagnostic: {error}"
        );
    }

    #[test]
    fn accepted_fixture_reports_deterministic_resource_counts() {
        let report = validate_candidate(&base_module(), ValidationBudget::default()).unwrap();
        let metrics = report.metrics();
        assert_eq!(metrics.functions, 1);
        assert_eq!(metrics.operations, 4);
        assert_eq!(metrics.peak_facts, 7);
    }

    #[test]
    fn canonical_descriptor_derives_field_offset_and_width() {
        validate_candidate(&projected_field_module(), ValidationBudget::default()).unwrap();
        let mut forged = projected_field_module();
        let Operation::Load { place } = &mut forged.functions[0].operations[1] else {
            unreachable!()
        };
        place.range.start = SizeExpr::Constant(0);
        assert_rule(forged, RuleId::ProjectedLayout);
    }

    #[test]
    fn direct_call_uses_the_checked_callee_contract_and_transfers_one_fresh_result() {
        let mut module = base_module();
        let produced = object(10, 1, Provenance::Fresh);
        module.functions.push(CandidateFunction {
            name: "allocate".into(),
            contract: CallableMemoryContract {
                parameters: Vec::new(),
                result: CallableMemoryResult::Fresh {
                    layout: layout(),
                    extent: ContractExtent::Exact(ContractSizeExpr::Constant(8)),
                },
            },
            parameters: Vec::new(),
            operations: vec![
                Operation::Alloc { result: produced.clone() },
                Operation::ReturnObject { object: produced.identity },
            ],
        });
        let call_result = object(11, 1, Provenance::Fresh);
        module.functions[0].operations.push(Operation::Call {
            callable: "allocate".into(),
            arguments: Vec::new(),
            result: Some(call_result.clone()),
        });
        module.functions[0].operations.push(Operation::Load {
            place: place(call_result, AccessMode::Read, 0, 8),
        });
        validate_candidate(&module, ValidationBudget::default()).unwrap();
    }

    #[test]
    fn borrowed_parameter_cannot_launder_into_a_fresh_result() {
        let mut module = base_module();
        let parameter = object(20, 1, Provenance::Fresh);
        module.functions.push(CandidateFunction {
            name: "launder".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::BorrowedRead,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::Fresh {
                    layout: layout(),
                    extent: ContractExtent::Exact(ContractSizeExpr::Constant(8)),
                },
            },
            parameters: vec![ParameterBinding {
                definition: parameter.identity.root,
                object: Some(parameter.clone()),
            }],
            operations: vec![Operation::ReturnObject { object: parameter.identity }],
        });
        assert_rule(module, RuleId::UniqueAllocation);
    }

    #[test]
    fn escaping_call_does_not_mint_a_second_unique_token() {
        let mut module = base_module();
        let result = add_identity_alias(&mut module);
        module.functions[0]
            .operations
            .push(Operation::Release { object: result.identity });
        module.functions.swap(0, 1);
        assert_rule(module, RuleId::DestructiveProvenance);
    }

    #[test]
    fn accepted_alias_round_trip_validates_the_callee_and_alias_load() {
        let mut module = base_module();
        let result = add_identity_alias(&mut module);
        module.functions[0].operations.push(Operation::Load {
            place: place(result, AccessMode::Read, 0, 8),
        });
        module.functions.swap(0, 1);
        validate_candidate(&module, ValidationBudget::default()).unwrap();
    }

    #[test]
    fn consuming_one_alias_invalidates_the_entire_group() {
        let mut module = base_module();
        let result = add_identity_alias(&mut module);
        module
            .imports
            .insert("unknown".into(), CallableMemoryContract::unknown(1));
        module.functions[0].operations.push(Operation::Call {
            callable: "unknown".into(),
            arguments: vec![CallArgument::Object(identity(3, 1))],
            result: None,
        });
        module.functions[0].operations.push(Operation::Load {
            place: place(result, AccessMode::Read, 0, 8),
        });
        module.functions.swap(0, 1);
        assert_rule(module, RuleId::ConsumeOnce);
    }

    #[test]
    fn callee_cannot_widen_a_parameter_extent() {
        let mut module = base_module();
        let mut parameter = object(20, 1, Provenance::Unknown);
        parameter.extent = Extent::Exact(SizeExpr::Constant(16));
        module.functions.push(CandidateFunction {
            name: "widen".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::BorrowedRead,
                    ParameterMemoryEffects::none(),
                )],
                result: CallableMemoryResult::NoMemory,
            },
            parameters: vec![ParameterBinding {
                definition: parameter.identity.root,
                object: Some(parameter),
            }],
            operations: Vec::new(),
        });
        assert_rule(module, RuleId::CheckedCallableContract);
    }

    #[test]
    fn unique_parameter_cannot_satisfy_fresh_result_without_body_minting() {
        let mut module = base_module();
        let parameter = object(20, 1, Provenance::Unknown);
        module.functions.push(CandidateFunction {
            name: "not_fresh".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::Unique,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::Fresh {
                    layout: layout(),
                    extent: ContractExtent::Exact(ContractSizeExpr::Constant(8)),
                },
            },
            parameters: vec![ParameterBinding {
                definition: parameter.identity.root,
                object: Some(parameter.clone()),
            }],
            operations: vec![Operation::ReturnObject { object: parameter.identity }],
        });
        assert_rule(module, RuleId::CheckedCallableContract);
    }

    #[test]
    fn retaining_a_body_allocation_cannot_satisfy_a_fresh_result() {
        let mut module = base_module();
        let produced = object(20, 1, Provenance::Fresh);
        module.functions.push(CandidateFunction {
            name: "retained".into(),
            contract: CallableMemoryContract {
                parameters: Vec::new(),
                result: CallableMemoryResult::Fresh {
                    layout: layout(),
                    extent: ContractExtent::Exact(ContractSizeExpr::Constant(8)),
                },
            },
            parameters: Vec::new(),
            operations: vec![
                Operation::Alloc { result: produced.clone() },
                Operation::Retain { object: produced.identity },
                Operation::ReturnObject { object: produced.identity },
            ],
        });
        assert_rule(module, RuleId::CheckedCallableContract);
    }

    #[test]
    fn accepted_list_element_and_slice_use_descriptor_derived_ranges() {
        let mut element = out_of_bounds_list_module();
        let Operation::DefineScalar { value: index, .. } =
            &mut element.functions[0].operations[0]
        else {
            unreachable!()
        };
        *index = 0;
        let Operation::Load { place } = &mut element.functions[0].operations[3] else {
            unreachable!()
        };
        let Extent::Counted { header, .. } = &place.object.extent else {
            unreachable!()
        };
        place.range.start = header.clone();
        validate_candidate(&element, ValidationBudget::default()).unwrap();

        let mut slice = element;
        let Operation::Load { place } = &mut slice.functions[0].operations[3] else {
            unreachable!()
        };
        place.projection = Projection::root().project(ProjectionStep::Slice {
            start: DefinitionId::new(1),
            length: DefinitionId::new(2),
        });
        validate_candidate(&slice, ValidationBudget::default()).unwrap();
    }

    #[test]
    fn ordinary_call_cannot_reach_a_host_import() {
        let mut module = base_module();
        module
            .host_imports
            .insert("host".into(), no_memory_contract());
        module.functions[0].operations.push(Operation::Call {
            callable: "host".into(),
            arguments: Vec::new(),
            result: None,
        });
        assert_rule(module, RuleId::CheckedCallableContract);
    }

    #[test]
    fn host_buffer_ranges_are_validated_before_the_contract() {
        let mut module = base_module();
        module.host_imports.insert(
            "read".into(),
            CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::BorrowedRead,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
                )],
                result: CallableMemoryResult::NoMemory,
            },
        );
        let allocated = object(3, 1, Provenance::Fresh);
        module.functions[0].operations.push(Operation::HostCall {
            import: "read".into(),
            arguments: vec![HostArgument::Buffer(place(
                allocated.clone(),
                AccessMode::Read,
                0,
                8,
            ))],
            result: None,
        });
        validate_candidate(&module, ValidationBudget::default()).unwrap();

        let Operation::HostCall { arguments, .. } =
            module.functions[0].operations.last_mut().unwrap()
        else {
            unreachable!()
        };
        let HostArgument::Buffer(place) = &mut arguments[0] else { unreachable!() };
        place.range.start = SizeExpr::Constant(1);
        place.range.length = SizeExpr::Constant(7);
        assert_rule(module, RuleId::ProjectedLayout);

        let mut out_of_bounds = out_of_bounds_list_module();
        let (base, stride, list_layout) = {
            let Operation::Load { place } =
                out_of_bounds.functions[0].operations.pop().unwrap()
            else {
                unreachable!()
            };
            let Extent::Counted { header, stride, .. } = &place.object.extent else {
                unreachable!()
            };
            let SizeExpr::Constant(base) = header else { unreachable!() };
            let SizeExpr::Constant(stride) = stride else { unreachable!() };
            (*base, *stride, place)
        };
        out_of_bounds.host_imports.insert(
            "read_list".into(),
            CallableMemoryContract {
                parameters: vec![
                    ParameterMemoryContract {
                        entry: ParameterMemoryEntry::NoMemory,
                        layout: None,
                        extent: None,
                        effects: ParameterMemoryEffects::none(),
                    },
                    ParameterMemoryContract {
                        entry: ParameterMemoryEntry::BorrowedRead,
                        layout: Some(list_layout.object.layout),
                        extent: Some(ContractExtent::Counted {
                            header: ContractSizeExpr::Constant(base),
                            count: ContractSizeExpr::Parameter(0),
                            stride: ContractSizeExpr::Constant(stride),
                        }),
                        effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
                    },
                ],
                result: CallableMemoryResult::NoMemory,
            },
        );
        out_of_bounds.functions[0].operations.push(Operation::HostCall {
            import: "read_list".into(),
            arguments: vec![
                HostArgument::Scalar(DefinitionId::new(2)),
                HostArgument::Buffer(list_layout),
            ],
            result: None,
        });
        assert_rule(out_of_bounds, RuleId::ObjectRange);
    }

    #[test]
    fn reuse_extent_is_rejected_at_the_reuse_operation() {
        let mut module = base_module();
        let mut replacement = object(3, 2, Provenance::Fresh);
        replacement.extent = Extent::Exact(SizeExpr::Constant(16));
        module.functions[0].operations.push(Operation::Reuse {
            old: identity(3, 1),
            new: replacement,
        });
        let error = validate_candidate(&module, ValidationBudget::default()).unwrap_err();
        assert_eq!(
            error.diagnostic,
            ValidationDiagnostic::Rule(RuleId::CanonicalLayout)
        );
        assert_eq!(error.operation, Some(4));
    }

    #[test]
    fn non_root_alias_results_are_explicitly_unsupported() {
        use crate::safewir::{ContractProjection, ContractProjectionStep};

        let mut module = base_module();
        module.functions.push(CandidateFunction {
            name: "project".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::BorrowedRead,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::Aliases {
                    parameter: 0,
                    projection: ContractProjection::from_steps([
                        ContractProjectionStep::Field { index: 0 },
                    ]),
                },
            },
            parameters: Vec::new(),
            operations: Vec::new(),
        });
        assert_unsupported(module, UnsupportedForm::NonRootAliasResult);
    }

    #[test]
    fn shared_result_sets_are_explicitly_unsupported() {
        let mut module = base_module();
        module.functions.push(CandidateFunction {
            name: "share".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::BorrowedRead,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::Shared(BTreeSet::from([0])),
            },
            parameters: Vec::new(),
            operations: Vec::new(),
        });
        assert_unsupported(module, UnsupportedForm::SharedResult);
    }

    #[test]
    fn unknown_call_effect_consumes_prior_unique_authority() {
        let mut module = base_module();
        module
            .imports
            .insert("unknown_effect".into(), CallableMemoryContract::unknown(1));
        let allocated = object(3, 1, Provenance::Fresh);
        module.functions[0].operations.push(Operation::Call {
            callable: "unknown_effect".into(),
            arguments: vec![CallArgument::Object(allocated.identity)],
            result: None,
        });
        module.functions[0].operations.push(Operation::Release {
            object: allocated.identity,
        });
        assert_rule(module, RuleId::ConsumeOnce);
    }

    #[test]
    fn function_body_cannot_hide_a_consume_effect() {
        let mut module = base_module();
        let parameter = object(20, 1, Provenance::Unknown);
        module.functions.push(CandidateFunction {
            name: "underdeclared".into(),
            contract: CallableMemoryContract {
                parameters: vec![ParameterMemoryContract {
                    entry: ParameterMemoryEntry::BorrowedWrite,
                    layout: Some(layout()),
                    extent: Some(ContractExtent::Exact(ContractSizeExpr::Constant(8))),
                    effects: ParameterMemoryEffects::none(),
                }],
                result: CallableMemoryResult::NoMemory,
            },
            parameters: vec![ParameterBinding {
                definition: parameter.identity.root,
                object: Some(parameter.clone()),
            }],
            operations: vec![Operation::Store {
                place: place(parameter, AccessMode::Write, 0, 8),
            }],
        });
        assert_rule(module, RuleId::CheckedCallableContract);
    }

    #[test]
    fn alias_routed_consume_is_attributed_to_the_parameter_contract() {
        let mut module = base_module();
        module.functions.push(identity_alias_function());
        module
            .imports
            .insert("unknown".into(), CallableMemoryContract::unknown(1));
        let parameter = object(30, 1, Provenance::Unknown);
        let alias = ObjectRef {
            identity: identity(31, 1),
            layout: parameter.layout,
            extent: parameter.extent.clone(),
            provenance: Provenance::Aliases {
                object: parameter.identity,
                projection: Projection::root(),
            },
        };
        module.functions.push(CandidateFunction {
            name: "alias_consume".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::BorrowedRead,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::NoMemory,
            },
            parameters: vec![ParameterBinding {
                definition: parameter.identity.root,
                object: Some(parameter.clone()),
            }],
            operations: vec![
                Operation::Call {
                    callable: "identity".into(),
                    arguments: vec![CallArgument::Object(parameter.identity)],
                    result: Some(alias.clone()),
                },
                Operation::Call {
                    callable: "unknown".into(),
                    arguments: vec![CallArgument::Object(alias.identity)],
                    result: None,
                },
            ],
        });
        assert_rule(module, RuleId::CheckedCallableContract);
    }

    #[test]
    fn report_mode_rule_diagnostics_have_forged_fixtures() {
        let mut module = base_module();
        module.checked_source_entries = 0;
        assert_rule(module, RuleId::CheckedSourceEntry);

        let mut module = base_module();
        if let Operation::Alloc { result } = &mut module.functions[0].operations[2] {
            result.layout = LayoutId::from_bytes([0x55; 32]);
        }
        assert_rule(module, RuleId::CanonicalLayout);

        let mut module = base_module();
        let borrowed = object(8, 1, Provenance::Unknown);
        module.functions[0].contract = CallableMemoryContract {
            parameters: vec![ParameterMemoryContract {
                entry: ParameterMemoryEntry::Unique,
                layout: Some(layout()),
                extent: Some(ContractExtent::Exact(ContractSizeExpr::Constant(8))),
                effects: ParameterMemoryEffects::none(),
            }],
            result: CallableMemoryResult::NoMemory,
        };
        module.functions[0].parameters = vec![ParameterBinding {
            definition: borrowed.identity.root,
            object: Some(borrowed.clone()),
        }];
        module.functions[0].operations = vec![Operation::Release { object: borrowed.identity }];
        assert_rule(module, RuleId::DestructiveProvenance);

        let mut module = base_module();
        if let Operation::Alloc { result } = &mut module.functions[0].operations[2] {
            result.provenance = Provenance::Unknown;
        }
        assert_rule(module, RuleId::UniqueAllocation);

        let mut module = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        module.functions[0].operations.insert(3, Operation::Release { object: allocated.identity });
        assert_rule(module, RuleId::ConsumeOnce);

        let mut module = base_module();
        let stale = object(3, 2, Provenance::Fresh);
        module.functions[0].operations[3] =
            Operation::Load { place: place(stale, AccessMode::Read, 0, 8) };
        assert_rule(module, RuleId::LiveGeneration);

        let mut module = base_module();
        if let Operation::Alloc { result } = &mut module.functions[0].operations[2] {
            result.extent = Extent::Exact(SizeExpr::narrow_u32(SizeExpr::Constant(
                u64::from(u32::MAX) + 1,
            )));
        }
        assert_rule(module, RuleId::NonWrappingSize);

        let mut module = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        let second = object(4, 1, Provenance::Fresh);
        module.functions[0]
            .operations
            .insert(3, Operation::Alloc { result: second.clone() });
        module.functions[0].operations[4] = Operation::CheckedIndex {
            index: DefinitionId::new(1),
            object: allocated.identity,
            length_source: DefinitionId::new(2),
            place: place(second, AccessMode::Read, 0, 8),
        };
        assert_rule(module, RuleId::ExactRangeSource);

        let mut module = base_module();
        module.functions[0].contract = CallableMemoryContract::unknown(1);
        assert_rule(module, RuleId::CheckedCallableContract);

        let mut module = base_module();
        module.functions.push(CandidateFunction {
            name: "unknown".into(),
            contract: CallableMemoryContract::unknown(0),
            parameters: Vec::new(),
            operations: Vec::new(),
        });
        module.functions[0].operations[3] = Operation::Call {
            callable: "unknown".into(),
            arguments: Vec::new(),
            result: Some(object(9, 1, Provenance::Fresh)),
        };
        assert_rule(module, RuleId::UnknownResultIsNotFresh);

        assert_rule(out_of_bounds_list_module(), RuleId::ObjectRange);

        let mut module = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        let mut forged = place(allocated, AccessMode::Read, 0, 8);
        forged.projection = Projection::root().project(ProjectionStep::Field { index: 0 });
        module.functions[0].operations[3] = Operation::Load { place: forged };
        assert_rule(module, RuleId::ProjectedLayout);

        let mut module = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        module.functions[0].operations[3] =
            Operation::Fill { destination: place(allocated, AccessMode::Read, 0, 8) };
        assert_rule(module, RuleId::BulkRanges);

        let mut module = base_module();
        module.functions[0].operations[3] = Operation::ReferenceCastFromScalar;
        assert_rule(module, RuleId::ReferenceSeparation);

        let mut module = base_module();
        module.functions[0].operations[3] = Operation::HostCall {
            import: "missing".into(),
            arguments: Vec::new(),
            result: None,
        };
        assert_rule(module, RuleId::HostBufferAbi);

        let mut module = base_module();
        module.contains_raw_memory = true;
        assert_rule(module, RuleId::SealedRawMemory);

        let mut module = base_module();
        module.post_validation_transform = PostValidationTransform::Arbitrary;
        assert_rule(module, RuleId::ValidatedEncoderAuthority);

        let module = base_module();
        let budget = ValidationBudget { operations: 0, ..ValidationBudget::default() };
        let error = validate_candidate(&module, budget).unwrap_err();
        assert_eq!(
            error.diagnostic,
            ValidationDiagnostic::Rule(RuleId::BudgetExhaustionFailsClosed)
        );
    }

    #[test]
    fn rule_evidence_partitions_the_catalog() {
        const REPORT_MODE_ENFORCED: &[RuleId] = &[
            RuleId::CanonicalLayout,
            RuleId::DestructiveProvenance,
            RuleId::UniqueAllocation,
            RuleId::ConsumeOnce,
            RuleId::LiveGeneration,
            RuleId::NonWrappingSize,
            RuleId::CheckedCallableContract,
            RuleId::UnknownResultIsNotFresh,
            RuleId::ObjectRange,
            RuleId::ProjectedLayout,
            RuleId::BulkRanges,
            RuleId::ReferenceSeparation,
            RuleId::BudgetExhaustionFailsClosed,
        ];
        const DEFERRED_FULL_RULES: &[RuleId] = &[
            RuleId::CheckedSourceEntry,
            RuleId::ExactRangeSource,
            RuleId::ConservativeJoin,
            RuleId::LoopFixpoint,
            RuleId::HostBufferAbi,
            RuleId::SealedRawMemory,
            RuleId::ValidatedEncoderAuthority,
        ];
        let enforced = REPORT_MODE_ENFORCED.iter().copied().collect::<BTreeSet<_>>();
        let deferred = DEFERRED_FULL_RULES.iter().copied().collect::<BTreeSet<_>>();
        assert!(enforced.is_disjoint(&deferred));
        assert_eq!(
            enforced.union(&deferred).copied().collect::<BTreeSet<_>>(),
            RuleId::ALL.iter().copied().collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn deferred_forms_report_unsupported_not_a_rule_violation() {
        let mut join = base_module();
        join.functions[0].operations.push(Operation::Join {
            result: object(3, 1, Provenance::Fresh),
            incoming: vec![identity(3, 1)],
        });
        assert_unsupported(join, UnsupportedForm::ControlFlowJoin);

        let mut loop_module = base_module();
        loop_module.functions[0].operations.push(Operation::Loop {
            entry: vec![identity(3, 1)],
            back_edges: vec![vec![identity(3, 1)]],
        });
        assert_unsupported(loop_module, UnsupportedForm::Loop);

        let mut proven = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        proven.functions[0].operations[3] = Operation::ProvenIndex {
            object: allocated.identity,
            index: DefinitionId::new(1),
            length_source: DefinitionId::new(2),
            place: place(allocated, AccessMode::Read, 0, 8),
        };
        assert_unsupported(proven, UnsupportedForm::ProvenIndex);

        let mut recursive = base_module();
        recursive.functions[0].operations.push(Operation::Call {
            callable: "main".into(),
            arguments: Vec::new(),
            result: None,
        });
        assert_unsupported(recursive, UnsupportedForm::RecursiveCallGraph);

        let mut owned = base_module();
        let parameter = object(20, 1, Provenance::Unknown);
        owned.functions.push(CandidateFunction {
            name: "consume".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::Unique,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Consume),
                )],
                result: CallableMemoryResult::NoMemory,
            },
            parameters: vec![ParameterBinding {
                definition: parameter.identity.root,
                object: Some(parameter),
            }],
            operations: Vec::new(),
        });
        assert_unsupported(owned, UnsupportedForm::OwnedParameterTransfer);
    }

    #[test]
    fn validation_is_deterministic_across_runs() {
        let first = validate_candidate(&base_module(), ValidationBudget::default())
            .unwrap()
            .metrics();
        let second = validate_candidate(&base_module(), ValidationBudget::default())
            .unwrap()
            .metrics();
        assert_eq!(first, second);
    }

    #[test]
    fn every_budget_fails_closed() {
        for budget in [
            ValidationBudget { functions: 0, ..ValidationBudget::default() },
            ValidationBudget { operations: 0, ..ValidationBudget::default() },
            ValidationBudget { facts: 0, ..ValidationBudget::default() },
            ValidationBudget { expression_depth: 0, ..ValidationBudget::default() },
        ] {
            let error = validate_candidate(&base_module(), budget).unwrap_err();
            assert_eq!(
                error.diagnostic,
                ValidationDiagnostic::Rule(RuleId::BudgetExhaustionFailsClosed)
            );
        }

    }
}
