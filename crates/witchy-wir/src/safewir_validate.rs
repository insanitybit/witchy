//! Deterministic, fail-closed validation for RFC-0162 SafeWIR candidates.
//!
//! This module validates the declarative SafeWIR operation graph. Mapping the
//! executable WIR tree onto this graph and granting encoder authority remain
//! separate integration boundaries; accepting a [`CandidateModule`] here does
//! not authorize encoding an ordinary `WirModule`.
//!
//! This report-mode slice is deliberately closed-world. Candidate source-entry, raw-memory, and
//! post-validation flags are attestations until the production WIR adapter is
//! implemented, so this module does not discharge SWIR-MOD-001,
//! SWIR-KERN-001, or SWIR-ENC-001. Canonical decoding proves that the supplied
//! layout bundle is internally well formed, not that it came from the checked
//! source signature. Structured branch joins and conservative loop fixpoints
//! are checked here. The report validator's fixed primitive/host catalog
//! authenticates names and contract shapes; binding those entries to final
//! production kernel and host definitions remains an adapter integration gate.
//! Symbolic parameter extents, proven-index facts, and typed fills remain
//! explicit unsupported forms rather than masquerading as rule violations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

use crate::layout::{
    FieldKind, HeaderLayout, LayoutBundle, LayoutId, LayoutInterner, LayoutKind, LayoutSize,
    ScalarKind,
};
use crate::safewir_call::{check_callable_contracts, CheckedCallableContracts};
use crate::safewir::{
    AccessMode, ByteRange, CallableMemoryContract, CallableMemoryResult, ContractExtent,
    ContractProjection, ContractProjectionStep, ContractSizeExpr, DefinitionId, Extent,
    ObjectIdentity, ObjectRef, Ownership,
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
pub enum MemoryWidth {
    I8,
    I32,
    I64,
    F64,
    V128,
}

impl MemoryWidth {
    const fn bytes(self) -> u128 {
        match self {
            Self::I8 => 1,
            Self::I32 => 4,
            Self::I64 | Self::F64 => 8,
            Self::V128 => 16,
        }
    }

    const fn matches_scalar(self, scalar: ScalarKind) -> bool {
        matches!(
            (self, scalar),
            (Self::I8, ScalarKind::Bool | ScalarKind::Tag8)
                | (Self::I32, ScalarKind::U32 | ScalarKind::Tag32)
                | (Self::I64, ScalarKind::Int | ScalarKind::Duration)
                | (Self::F64, ScalarKind::Float)
        )
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ControlId(u32);

impl ControlId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
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
        width: MemoryWidth,
    },
    Store {
        place: Place,
        width: MemoryWidth,
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
    /// A closed-world function-value call. The validator derives the complete
    /// compatible target set from checked final bodies; the candidate supplies
    /// no target-set or result-shape authority.
    IndirectCall {
        arguments: Vec<CallArgument>,
        result: Option<ObjectRef>,
    },
    /// A closure call whose captures are prepended to the ordinary arguments.
    /// The named body must exist, while authority comes from the conservative
    /// join of every closed-world callable with the combined arity.
    ClosureCall {
        callable: String,
        captures: Vec<CallArgument>,
        arguments: Vec<CallArgument>,
        result: Option<ObjectRef>,
    },
    /// Mint an alias value for a descriptor-derived projection. This never
    /// mints storage or unique authority.
    ProjectObject {
        source: ObjectIdentity,
        projection: ContractProjection,
        result: ObjectRef,
    },
    /// Materialize a conservative value that may alias any named source.
    /// Every source must have one compatible descriptor shape.
    MergeObjectAliases {
        sources: Vec<ObjectIdentity>,
        result: ObjectRef,
    },
    HostCall {
        import: String,
        arguments: Vec<HostArgument>,
        result: Option<ObjectRef>,
    },
    If {
        condition: DefinitionId,
        then_operations: Vec<Operation>,
        else_operations: Vec<Operation>,
    },
    Block {
        control: ControlId,
        body: Vec<Operation>,
    },
    Loop {
        control: ControlId,
        body: Vec<Operation>,
    },
    Branch {
        target: ControlId,
        condition: Option<DefinitionId>,
    },
    ReturnVoid,
    ReturnObject {
        object: ObjectIdentity,
    },
    ReferenceCastFromScalar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedForm {
    ControlFlowJoin,
    ProvenIndex,
    SymbolicParameterExtent,
    TypedFill,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObjectState {
    object: ObjectRef,
    ownership: Ownership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReturnedObject {
    object: ObjectRef,
    ownership: Ownership,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct State {
    scalars: BTreeMap<DefinitionId, i128>,
    objects: BTreeMap<ObjectIdentity, ObjectState>,
    aliases: BTreeMap<ObjectIdentity, BTreeSet<ObjectIdentity>>,
    definitions: BTreeSet<DefinitionId>,
    minted: BTreeSet<ObjectIdentity>,
    returned: Option<ReturnedObject>,
}

#[derive(Default)]
struct ControlFlows {
    branches: BTreeMap<ControlId, State>,
    returned: Option<State>,
}

impl ControlFlows {
    fn fact_count(&self) -> usize {
        self.branches
            .values()
            .map(State::fact_count)
            .chain(self.returned.iter().map(State::fact_count))
            .fold(0_usize, usize::saturating_add)
    }
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
    checked_callables: Option<CheckedCallableContracts>,
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
        checked_callables: None,
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
    fn checked_callables(&self) -> &CheckedCallableContracts {
        self.checked_callables
            .as_ref()
            .expect("call contracts are checked before function validation")
    }

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
                total.checked_add(operation_count(&function.operations)?)
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
        // Preserve explicit report-mode deferrals ahead of body-summary
        // comparison. A branch-local object return has no closed summary until
        // exit accumulators are represented in the candidate schema, so its
        // attached result cannot be meaningfully checked yet.
        for function in &self.module.functions {
            if has_branch_local_object_return(&function.operations) {
                return Err(self.function_unsupported(
                    UnsupportedForm::ControlFlowJoin,
                    function,
                    None,
                    "object-returning branch joins require explicit exit accumulators",
                ));
            }
        }
        self.checked_callables = Some(check_callable_contracts(
            self.module,
            &self.layouts,
            self.budget,
        )?);
        for function in &self.module.functions {
            self.validate_function(function)?;
        }
        Ok(())
    }

    fn validate_function(&mut self, function: &CandidateFunction) -> Result<(), ValidationError> {
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
        self.validate_definition_census(function)?;

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
                        || !contract_extent_matches(extent, &object.extent, &arguments, &state)
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
                    derived.provenance = if entry == ParameterMemoryEntry::Unique {
                        Provenance::Fresh
                    } else {
                        Provenance::Unknown
                    };
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
        self.validate_control_structure(function, &function.operations, &mut Vec::new(), 0)?;

        let mut flows = ControlFlows::default();
        let falls_through = self.validate_operations(
            function,
            &function.operations,
            &mut state,
            0,
            &mut Vec::new(),
            &mut flows,
        )?;
        if !flows.branches.is_empty() {
            return Err(self.function_error(
                RuleId::ConservativeJoin,
                function,
                None,
                "branch escaped its lexical control target",
            ));
        }
        if falls_through {
            self.validate_function_exit(function, &state)?;
        }
        if let Some(returned) = &flows.returned {
            self.validate_function_exit(function, returned)?;
        }
        Ok(())
    }

    fn validate_definition_census(
        &self,
        function: &CandidateFunction,
    ) -> Result<(), ValidationError> {
        let mut definitions = BTreeSet::new();
        for definition in function
            .parameters
            .iter()
            .map(|parameter| parameter.definition)
            .chain(definition_sites(&function.operations))
        {
            if !definitions.insert(definition) {
                return Err(self.function_error(
                    RuleId::CheckedCallableContract,
                    function,
                    None,
                    format!(
                        "DefinitionId {} is minted by more than one structural definition site",
                        definition.get()
                    ),
                ));
            }
        }
        let mut identities = BTreeSet::new();
        for identity in function
            .parameters
            .iter()
            .filter_map(|parameter| parameter.object.as_ref().map(|object| object.identity))
            .chain(identity_sites(&function.operations))
        {
            if !identities.insert(identity) {
                return Err(self.function_error(
                    RuleId::UniqueAllocation,
                    function,
                    None,
                    format!(
                        "object identity {}:{} is minted by more than one structural site",
                        identity.root.get(),
                        identity.generation.get()
                    ),
                ));
            }
        }
        let mut controls = BTreeSet::new();
        for control in control_sites(&function.operations) {
            if !controls.insert(control) {
                return Err(self.function_error(
                    RuleId::ConservativeJoin,
                    function,
                    None,
                    format!(
                        "ControlId {} is declared by more than one structural site",
                        control.get()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn validate_control_structure(
        &self,
        function: &CandidateFunction,
        operations: &[Operation],
        controls: &mut Vec<ControlId>,
        depth: usize,
    ) -> Result<(), ValidationError> {
        if depth > self.budget.expression_depth {
            return Err(self.function_error(
                RuleId::BudgetExhaustionFailsClosed,
                function,
                None,
                "structured control census exhausted its depth budget",
            ));
        }
        for operation in operations {
            match operation {
                Operation::If { then_operations, else_operations, .. } => {
                    let nested = depth.checked_add(1).ok_or_else(|| {
                        self.function_error(
                            RuleId::BudgetExhaustionFailsClosed,
                            function,
                            None,
                            "structured control census depth overflowed",
                        )
                    })?;
                    self.validate_control_structure(
                        function,
                        then_operations,
                        controls,
                        nested,
                    )?;
                    self.validate_control_structure(
                        function,
                        else_operations,
                        controls,
                        nested,
                    )?;
                }
                Operation::Block { control, body } | Operation::Loop { control, body } => {
                    let nested = depth.checked_add(1).ok_or_else(|| {
                        self.function_error(
                            RuleId::BudgetExhaustionFailsClosed,
                            function,
                            None,
                            "structured control census depth overflowed",
                        )
                    })?;
                    controls.push(*control);
                    self.validate_control_structure(function, body, controls, nested)?;
                    controls.pop();
                }
                Operation::Branch { target, .. } => {
                    if !controls.contains(target) {
                        return Err(self.function_error(
                            RuleId::ConservativeJoin,
                            function,
                            None,
                            format!(
                                "branch target {} is not a lexically enclosing control",
                                target.get()
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_operations(
        &mut self,
        function: &CandidateFunction,
        operations: &[Operation],
        state: &mut State,
        control_depth: usize,
        controls: &mut Vec<ControlId>,
        flows: &mut ControlFlows,
    ) -> Result<bool, ValidationError> {
        if control_depth > self.budget.expression_depth {
            return Err(self.function_error(
                RuleId::BudgetExhaustionFailsClosed,
                function,
                None,
                "structured control depth exhausted",
            ));
        }
        let mut falls_through = true;
        for operation in operations {
            if !falls_through {
                break;
            }
            self.operations = self.operations.checked_add(1).ok_or_else(|| {
                self.function_error(
                    RuleId::BudgetExhaustionFailsClosed,
                    function,
                    None,
                    "operation counter overflowed",
                )
            })?;
            let operation_index = self.operations - 1;
            if self.operations > self.budget.operations {
                return Err(self.function_error(
                    RuleId::BudgetExhaustionFailsClosed,
                    function,
                    Some(operation_index),
                    "operation budget exhausted",
                ));
            }
            falls_through = self.validate_operation(
                function,
                operation_index,
                operation,
                state,
                control_depth,
                controls,
                flows,
            )?;
            self.observe_facts(&state, function, Some(operation_index))?;
            self.observe_control_facts(
                flows,
                falls_through.then_some(&*state),
                function,
                Some(operation_index),
            )?;
        }
        Ok(falls_through)
    }

    fn observe_control_facts(
        &mut self,
        flows: &ControlFlows,
        fallthrough: Option<&State>,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(), ValidationError> {
        let live = flows.fact_count().saturating_add(
            fallthrough.map(State::fact_count).unwrap_or_default(),
        );
        self.peak_facts = self.peak_facts.max(live);
        if live > self.budget.facts {
            return Err(self.function_error(
                RuleId::BudgetExhaustionFailsClosed,
                function,
                at,
                "control-flow fact budget exhausted",
            ));
        }
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
                    && contract_extent_matches(extent, &returned.object.extent, &arguments, state) =>
            {}
            (CallableMemoryResult::Aliases { parameter, projection }, Some(returned)) => {
                let Some(CallArgument::Object(source)) = arguments.get(*parameter) else {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        None,
                        "function alias result names a non-object parameter",
                    ));
                };
                let source_object = state.objects.get(source).ok_or_else(|| {
                    self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        None,
                        "function alias result source is not live at exit",
                    )
                })?;
                if projection.steps().is_empty()
                    && returned.object.identity == *source
                    && returned.object.layout == source_object.object.layout
                    && returned.object.extent == source_object.object.extent
                {
                    return Ok(());
                }
                let (layout, extent, actual_projection) = self.derive_contract_projection(
                    &source_object.object,
                    projection,
                    state,
                    function,
                    None,
                )?;
                let (resolved_source, resolved_projection) = self.resolve_alias_projection(
                    state,
                    returned.object.identity,
                    function,
                )?;
                if returned.object.layout != layout
                    || returned.object.extent != extent
                    || resolved_source != *source
                    || !self.projections_match_by_value(
                        &resolved_projection,
                        &actual_projection,
                        state,
                    )
                {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        None,
                        "function return does not match its descriptor-derived alias contract",
                    ));
                }
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
                if !matches!(&returned.object.provenance, Provenance::MayAlias(actual) if actual == &expected)
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

    fn resolve_alias_projection(
        &self,
        state: &State,
        mut identity: ObjectIdentity,
        function: &CandidateFunction,
    ) -> Result<(ObjectIdentity, crate::safewir::Projection), ValidationError> {
        let mut projection = crate::safewir::Projection::root();
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(identity) {
                return Err(self.function_error(
                    RuleId::CheckedCallableContract,
                    function,
                    None,
                    "callable alias provenance contains a cycle",
                ));
            }
            let Some(object) = state.objects.get(&identity) else {
                return Ok((identity, projection));
            };
            let Provenance::Aliases {
                object: source,
                projection: step,
            } = &object.object.provenance
            else {
                return Ok((identity, projection));
            };
            projection = step.projected(&projection);
            identity = *source;
        }
    }

    fn observed_effects(
        &self,
        function: &CandidateFunction,
        identity: ObjectIdentity,
        state: &State,
    ) -> crate::safewir::ParameterMemoryEffects {
        use crate::safewir::ParameterMemoryEffects;
        let mut observed = ParameterMemoryEffects::none();
        for operation in operations(&function.operations) {
            macro_rules! add {
                ($effect:expr) => {
                    observed = observed.with($effect)
                };
            }
            match operation {
                Operation::Load { place, .. }
                    if identities_may_alias(state, place.object.identity, identity) =>
                {
                    add!(ParameterMemoryEffect::Read);
                }
                Operation::Store { place, .. }
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
                    let contract = self.checked_callables().ordinary(callable);
                    if let Some(contract) = contract {
                        for (argument, parameter) in arguments.iter().zip(&contract.parameters) {
                            if matches!(argument, CallArgument::Object(actual) if identities_may_alias(state, *actual, identity))
                            {
                                observed = observed.join(parameter.effects);
                            }
                        }
                    }
                }
                Operation::ClosureCall { callable, captures, arguments, .. } => {
                    let arity = captures.len() + arguments.len();
                    let contract = if self
                        .checked_callables()
                        .ordinary(callable)
                        .is_some_and(|contract| contract.parameters.len() == arity)
                    {
                        self.checked_callables().indirect(arity)
                    } else {
                        CallableMemoryContract::unknown(arity)
                    };
                    for (argument, parameter) in
                        captures.iter().chain(arguments).zip(&contract.parameters)
                    {
                        if matches!(argument, CallArgument::Object(actual) if identities_may_alias(state, *actual, identity))
                        {
                            observed = observed.join(parameter.effects);
                        }
                    }
                }
                Operation::IndirectCall { arguments, .. } => {
                    let contract = self.checked_callables().indirect(arguments.len());
                    for (argument, parameter) in arguments.iter().zip(&contract.parameters) {
                        if matches!(argument, CallArgument::Object(actual) if identities_may_alias(state, *actual, identity))
                        {
                            observed = observed.join(parameter.effects);
                        }
                    }
                }
                Operation::HostCall { import, arguments, .. } => {
                    if let Some(contract) = self.checked_callables().host(import) {
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
        control_depth: usize,
        controls: &mut Vec<ControlId>,
        flows: &mut ControlFlows,
    ) -> Result<bool, ValidationError> {
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
            Operation::Load { place, width } => {
                if place.access != AccessMode::Read {
                    return Err(self.function_error(
                        RuleId::ObjectRange,
                        function,
                        at,
                        "load requires a read place",
                    ));
                }
                self.validate_place(place, state, function, at)?;
                self.validate_memory_width(place, *width, state, function, at)?;
            }
            Operation::Store { place, width } => {
                if place.access != AccessMode::Write {
                    return Err(self.function_error(
                        RuleId::ObjectRange,
                        function,
                        at,
                        "store requires a write place",
                    ));
                }
                let object = self.validate_place(place, state, function, at)?;
                self.validate_memory_width(place, *width, state, function, at)?;
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
                let (_, source_length, source_scalar) =
                    self.derive_projection(source, state, function, at)?;
                let (_, destination_length, destination_scalar) =
                    self.derive_projection(destination, state, function, at)?;
                if source_length != destination_length
                    || source_scalar.is_none()
                    || source_scalar != destination_scalar
                {
                    return Err(self.function_error(
                        RuleId::ProjectedLayout,
                        function,
                        at,
                        "bulk copy requires identical scalar projection representations",
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
                return Err(self.function_unsupported(
                    UnsupportedForm::TypedFill,
                    function,
                    at,
                    "typed fill requires a descriptor-level valid-byte-pattern contract",
                ));
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
                // Generation changes do not sever parameter-effect ancestry.
                // The old generation remains consumed; this edge exists only
                // so summary recomputation attributes later escape/retain to
                // the transferred input authority.
                self.link_alias_group(state, *old, new.identity);
                state.minted.insert(new.identity);
            }
            Operation::Call { callable, arguments, result } => {
                let contract = self
                    .checked_callables()
                    .ordinary(callable)
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
            Operation::IndirectCall { arguments, result } => {
                let contract = self.checked_callables().indirect(arguments.len());
                self.validate_call(
                    function,
                    at,
                    "<indirect>",
                    &contract,
                    arguments,
                    result.as_ref(),
                    state,
                )?;
            }
            Operation::ClosureCall { callable, captures, arguments, result } => {
                let combined = captures.iter().chain(arguments).copied().collect::<Vec<_>>();
                let named = self
                    .checked_callables()
                    .ordinary(callable)
                    .ok_or_else(|| {
                        self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            at,
                            format!("closure `{callable}` has no checked final body"),
                        )
                    })?;
                if named.parameters.len() != combined.len() {
                    return Err(self.function_error(
                        RuleId::CheckedCallableContract,
                        function,
                        at,
                        format!(
                            "closure `{callable}` has arity {}, but captures and arguments supply {}",
                            named.parameters.len(),
                            combined.len(),
                        ),
                    ));
                }
                let contract = self.checked_callables().indirect(combined.len());
                self.validate_call(
                    function,
                    at,
                    callable,
                    &contract,
                    &combined,
                    result.as_ref(),
                    state,
                )?;
            }
            Operation::ProjectObject { source, projection, result } => {
                self.materialize_projection(
                    function,
                    at,
                    *source,
                    projection,
                    result,
                    state,
                )?;
            }
            Operation::MergeObjectAliases { sources, result } => {
                self.materialize_shared_aliases(function, at, sources, result, state)?;
            }
            Operation::HostCall { import, arguments, result } => {
                let Some(contract) = self.checked_callables().host(import) else {
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
            Operation::If { condition, then_operations, else_operations } => {
                self.require_scalar(state, *condition, function, at)?;
                let nested_depth =
                    self.nested_control_depth(function, at, control_depth)?;
                let mut then_state = state.clone();
                let then_falls_through = self.validate_operations(
                    function,
                    then_operations,
                    &mut then_state,
                    nested_depth,
                    controls,
                    flows,
                )?;
                let mut else_state = state.clone();
                let else_falls_through = self.validate_operations(
                    function,
                    else_operations,
                    &mut else_state,
                    nested_depth,
                    controls,
                    flows,
                )?;
                match (then_falls_through, else_falls_through) {
                    (true, true) => {
                        *state = self.join_states(function, at, then_state, else_state)?;
                    }
                    (true, false) => *state = then_state,
                    (false, true) => *state = else_state,
                    (false, false) => return Ok(false),
                }
            }
            Operation::Block { control, body } => {
                let nested_depth =
                    self.nested_control_depth(function, at, control_depth)?;
                controls.push(*control);
                let body_falls_through = self.validate_operations(
                    function,
                    body,
                    state,
                    nested_depth,
                    controls,
                    flows,
                )?;
                controls.pop();
                let branch_exit = flows.branches.remove(control);
                match (body_falls_through, branch_exit) {
                    (true, Some(exit)) => {
                        *state = self.join_states(function, at, state.clone(), exit)?;
                    }
                    (false, Some(exit)) => *state = exit,
                    (true, None) => {}
                    (false, None) => return Ok(false),
                }
            }
            Operation::Loop { control, body } => {
                let nested_depth =
                    self.nested_control_depth(function, at, control_depth)?;
                let entry = state.clone();
                let mut header = entry.clone();
                loop {
                    let mut iteration_state = header.clone();
                    let mut iteration_flows = ControlFlows::default();
                    controls.push(*control);
                    let body_falls_through = self.validate_operations(
                        function,
                        body,
                        &mut iteration_state,
                        nested_depth,
                        controls,
                        &mut iteration_flows,
                    )?;
                    controls.pop();
                    let back_edge = iteration_flows.branches.remove(control);
                    let next = match back_edge {
                        Some(back_edge) => {
                            self.join_states(function, at, entry.clone(), back_edge)?
                        }
                        None => header.clone(),
                    };
                    if next == header {
                        self.merge_control_flows(function, at, flows, iteration_flows)?;
                        if body_falls_through {
                            *state = iteration_state;
                        } else {
                            return Ok(false);
                        }
                        break;
                    }
                    header = next;
                }
            }
            Operation::Branch { target, condition } => {
                if !controls.contains(target) {
                    return Err(self.function_error(
                        RuleId::ConservativeJoin,
                        function,
                        at,
                        format!(
                            "branch target {} is not a lexically enclosing control",
                            target.get()
                        ),
                    ));
                }
                if let Some(condition) = condition {
                    self.require_scalar(state, *condition, function, at)?;
                }
                self.merge_branch_state(function, at, flows, *target, state.clone())?;
                if condition.is_none() {
                    return Ok(false);
                }
            }
            Operation::ReturnVoid => {
                self.merge_return_state(function, at, flows, state.clone())?;
                return Ok(false);
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
        Ok(true)
    }

    fn merge_branch_state(
        &mut self,
        function: &CandidateFunction,
        at: Option<usize>,
        flows: &mut ControlFlows,
        target: ControlId,
        state: State,
    ) -> Result<(), ValidationError> {
        match flows.branches.remove(&target) {
            Some(existing) => {
                let joined = self.join_states(function, at, existing, state)?;
                flows.branches.insert(target, joined);
            }
            None => {
                flows.branches.insert(target, state);
            }
        }
        Ok(())
    }

    fn merge_return_state(
        &mut self,
        function: &CandidateFunction,
        at: Option<usize>,
        flows: &mut ControlFlows,
        state: State,
    ) -> Result<(), ValidationError> {
        flows.returned = Some(match flows.returned.take() {
            Some(existing) => self.join_states(function, at, existing, state)?,
            None => state,
        });
        Ok(())
    }

    fn merge_control_flows(
        &mut self,
        function: &CandidateFunction,
        at: Option<usize>,
        destination: &mut ControlFlows,
        source: ControlFlows,
    ) -> Result<(), ValidationError> {
        for (target, state) in source.branches {
            self.merge_branch_state(function, at, destination, target, state)?;
        }
        if let Some(returned) = source.returned {
            self.merge_return_state(function, at, destination, returned)?;
        }
        Ok(())
    }

    fn nested_control_depth(
        &self,
        function: &CandidateFunction,
        at: Option<usize>,
        control_depth: usize,
    ) -> Result<usize, ValidationError> {
        let nested = control_depth.checked_add(1).ok_or_else(|| {
            self.function_error(
                RuleId::BudgetExhaustionFailsClosed,
                function,
                at,
                "structured control depth overflowed",
            )
        })?;
        if nested > self.budget.expression_depth {
            return Err(self.function_error(
                RuleId::BudgetExhaustionFailsClosed,
                function,
                at,
                "structured control depth exhausted",
            ));
        }
        Ok(nested)
    }

    fn join_states(
        &self,
        function: &CandidateFunction,
        at: Option<usize>,
        left: State,
        right: State,
    ) -> Result<State, ValidationError> {
        if left.returned.is_some() || right.returned.is_some() {
            return Err(self.function_unsupported(
                UnsupportedForm::ControlFlowJoin,
                function,
                at,
                "object-returning branch joins require explicit exit accumulators",
            ));
        }

        let definitions = left
            .definitions
            .intersection(&right.definitions)
            .copied()
            .collect();
        let scalars = left
            .scalars
            .iter()
            .filter_map(|(definition, value)| {
                (right.scalars.get(definition) == Some(value)).then_some((*definition, *value))
            })
            .collect();
        let mut objects = BTreeMap::new();
        for (identity, left_object) in &left.objects {
            let Some(right_object) = right.objects.get(identity) else { continue };
            if left_object.object.layout != right_object.object.layout
                || left_object.object.extent != right_object.object.extent
            {
                continue;
            }
            let mut object = left_object.object.clone();
            if object.provenance != right_object.object.provenance {
                object.provenance = Provenance::Unknown;
            }
            objects.insert(
                *identity,
                ObjectState {
                    object,
                    ownership: join_ownership(left_object.ownership, right_object.ownership),
                },
            );
        }

        let mut aliases = left.aliases;
        for (identity, right_group) in right.aliases {
            aliases.entry(identity).or_default().extend(right_group);
        }

        Ok(State {
            scalars,
            objects,
            aliases,
            definitions,
            minted: left.minted.intersection(&right.minted).copied().collect(),
            returned: None,
        })
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
                        let object = self.require_live(state, *identity, function, at)?;
                        let ownership = object.ownership;
                        if parameter.layout != Some(object.object.layout)
                            || parameter.extent.as_ref().is_none_or(|extent| {
                                !contract_extent_matches(
                                    extent,
                                    &object.object.extent,
                                    arguments,
                                    state,
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
                                ownership != Ownership::Consumed
                            }
                            ParameterMemoryEntry::BorrowedWrite => matches!(
                                ownership,
                                Ownership::Unique | Ownership::BorrowedWrite
                            ),
                            ParameterMemoryEntry::Shared => {
                                ownership != Ownership::Consumed
                            }
                            ParameterMemoryEntry::Unique => {
                                ownership == Ownership::Unique
                            }
                            ParameterMemoryEntry::NoMemory | ParameterMemoryEntry::Unknown => {
                                unreachable!()
                            }
                        };
                        if !entry_ok
                            || (parameter.effects.contains(ParameterMemoryEffect::Write)
                                && !matches!(
                                    ownership,
                                    Ownership::Unique | Ownership::BorrowedWrite
                                ))
                            || (consumes && ownership != Ownership::Unique)
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
                            self.require_live_mut(state, *identity, function, at)?.ownership =
                                Ownership::Shared;
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
                    || !contract_extent_matches(extent, &result.extent, arguments, state)
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
                self.materialize_projection(
                    function,
                    at,
                    *source,
                    projection,
                    result,
                    state,
                )?;
            }
            (CallableMemoryResult::Shared(parameters), Some(result)) => {
                let mut sources = BTreeSet::new();
                for parameter in parameters {
                    match arguments.get(*parameter) {
                        Some(CallArgument::Object(identity)) => {
                            sources.insert(*identity);
                        }
                        _ => return Err(self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            at,
                            format!("shared result of `{callable}` has a non-object source"),
                        )),
                    }
                }
                self.materialize_shared_aliases(
                    function,
                    at,
                    &sources.into_iter().collect::<Vec<_>>(),
                    result,
                    state,
                )?;
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

    fn materialize_projection(
        &self,
        function: &CandidateFunction,
        at: Option<usize>,
        source: ObjectIdentity,
        projection: &ContractProjection,
        result: &ObjectRef,
        state: &mut State,
    ) -> Result<(), ValidationError> {
        let source_state = self.require_live(state, source, function, at)?;
        let source_object = source_state.object.clone();
        let alias_ownership = match source_state.ownership {
            Ownership::Unique | Ownership::Shared => Ownership::Shared,
            Ownership::BorrowedRead | Ownership::BorrowedWrite => Ownership::BorrowedRead,
            Ownership::Consumed => unreachable!(),
        };
        let (layout, extent, actual_projection) =
            self.derive_contract_projection(&source_object, projection, state, function, at)?;
        if result.identity.root == source.root
            || result.identity.generation != source.generation
            || result.layout != layout
            || result.extent != extent
            || !matches!(
                &result.provenance,
                Provenance::Aliases { object, projection }
                    if *object == source
                        && self.projections_match_by_value(
                            projection,
                            &actual_projection,
                            state,
                        )
            )
        {
            return Err(self.function_error(
                RuleId::CheckedCallableContract,
                function,
                at,
                "projected alias result contradicts descriptor-derived identity",
            ));
        }
        self.validate_object_claim(result, function, at)?;
        self.define(state, result.identity.root, function, at)?;
        if state.objects.contains_key(&result.identity) {
            return Err(self.function_error(
                RuleId::CheckedCallableContract,
                function,
                at,
                "projected alias reuses an existing value definition",
            ));
        }
        self.require_live_mut(state, source, function, at)?.ownership = alias_ownership;
        self.link_alias_group(state, source, result.identity);
        state.objects.insert(
            result.identity,
            ObjectState { object: result.clone(), ownership: alias_ownership },
        );
        Ok(())
    }

    fn materialize_shared_aliases(
        &self,
        function: &CandidateFunction,
        at: Option<usize>,
        sources: &[ObjectIdentity],
        result: &ObjectRef,
        state: &mut State,
    ) -> Result<(), ValidationError> {
        let expected = sources.iter().copied().collect::<BTreeSet<_>>();
        if expected.is_empty() || expected.len() != sources.len() {
            return Err(self.function_error(
                RuleId::CheckedCallableContract,
                function,
                at,
                "shared result requires a nonempty set of distinct source aliases",
            ));
        }
        let mut shape = None;
        for source in &expected {
            let object = self.require_live(state, *source, function, at)?;
            let current = (object.object.layout, object.object.extent.clone());
            if shape.as_ref().is_some_and(|shape| shape != &current) {
                return Err(self.function_error(
                    RuleId::CheckedCallableContract,
                    function,
                    at,
                    "shared result sources have incompatible descriptor identities",
                ));
            }
            shape = Some(current);
        }
        let (layout, extent) = shape.expect("nonempty source set has one shape");
        if expected.contains(&result.identity)
            || result.layout != layout
            || result.extent != extent
            || !matches!(&result.provenance, Provenance::MayAlias(actual) if actual == &expected)
        {
            return Err(self.function_error(
                RuleId::CheckedCallableContract,
                function,
                at,
                "shared result contradicts its checked alias set or descriptor shape",
            ));
        }
        self.validate_object_claim(result, function, at)?;
        self.define(state, result.identity.root, function, at)?;
        if state.objects.contains_key(&result.identity) {
            return Err(self.function_error(
                RuleId::CheckedCallableContract,
                function,
                at,
                "shared result reuses an existing value definition",
            ));
        }
        state.objects.insert(
            result.identity,
            ObjectState { object: result.clone(), ownership: Ownership::Shared },
        );
        for source in expected {
            self.require_live_mut(state, source, function, at)?.ownership = Ownership::Shared;
            self.link_alias_group(state, source, result.identity);
        }
        Ok(())
    }

    fn derive_contract_projection(
        &self,
        source: &ObjectRef,
        projection: &ContractProjection,
        state: &State,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(LayoutId, Extent, crate::safewir::Projection), ValidationError> {
        let mut layout = source.layout;
        let mut extent = source.extent.clone();
        let mut actual = crate::safewir::Projection::root();
        for (step_index, step) in projection.steps().iter().enumerate() {
            let descriptor = self.layouts.get(layout).ok_or_else(|| {
                self.function_error(
                    RuleId::CanonicalLayout,
                    function,
                    at,
                    "call projection references an unknown descriptor",
                )
            })?;
            match step {
                ContractProjectionStep::Header { component } => {
                    let HeaderLayout::PackedList { length_offset: _, capacity_offset: _, .. } =
                        descriptor.header()
                    else {
                        return Err(self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            at,
                            "header result projection requires a packed-list descriptor",
                        ));
                    };
                    if *component > 1 {
                        return Err(self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            at,
                            "header result projection names an unknown component",
                        ));
                    }
                    if step_index + 1 != projection.steps().len() {
                        return Err(self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            at,
                            "a scalar header result cannot be projected further",
                        ));
                    }
                    layout = self.scalar_layout(ScalarKind::U32, function, at)?;
                    extent = Extent::Exact(SizeExpr::Constant(4));
                    actual = actual.project(ProjectionStep::Header { component: *component });
                }
                ContractProjectionStep::Field { index } => {
                    let field = descriptor.fields().get(*index as usize).ok_or_else(|| {
                        self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            at,
                            "field result projection is outside the descriptor",
                        )
                    })?;
                    actual = actual.project(ProjectionStep::Field { index: *index });
                    match field.kind() {
                        FieldKind::Scalar(kind) => {
                            if step_index + 1 != projection.steps().len() {
                                return Err(self.function_error(
                                    RuleId::CheckedCallableContract,
                                    function,
                                    at,
                                    "a scalar field result cannot be projected further",
                                ));
                            }
                            layout = self.scalar_layout(kind, function, at)?;
                            extent = Extent::Exact(SizeExpr::Constant(u64::from(kind.size())));
                        }
                        FieldKind::Inline(child) => {
                            layout = child;
                            extent = match self.layouts.get(child).map(|descriptor| descriptor.size()) {
                                Some(LayoutSize::Fixed(size)) => {
                                    Extent::Exact(SizeExpr::Constant(u64::from(size)))
                                }
                                _ => Extent::Unknown,
                            };
                        }
                    }
                }
                ContractProjectionStep::Element { index } => {
                    let LayoutKind::PackedList { element, .. } = descriptor.kind() else {
                        return Err(self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            at,
                            "element result projection requires a packed-list descriptor",
                        ));
                    };
                    let LayoutSize::Dynamic { stride, .. } = descriptor.size() else {
                        unreachable!("packed lists have dynamic descriptors")
                    };
                    let Extent::Counted { count, .. } = &extent else {
                        return Err(self.function_error(
                            RuleId::ExactRangeSource,
                            function,
                            at,
                            "element result projection requires an exact source element count",
                        ));
                    };
                    let source_count =
                        self.require_nonnegative_scalar(state, *count, function, at)?;
                    if u128::from(*index) >= source_count {
                        return Err(self.function_error(
                            RuleId::ExactRangeSource,
                            function,
                            at,
                            "element result projection is outside the source list",
                        ));
                    }
                    let definition = self.constant_definition(state, u64::from(*index), function, at)?;
                    actual = actual.project(ProjectionStep::Element { index: definition });
                    layout = *element;
                    extent = Extent::Exact(SizeExpr::Constant(u64::from(stride)));
                }
                ContractProjectionStep::Slice { start, length } => {
                    let LayoutKind::PackedList { element, .. } = descriptor.kind() else {
                        return Err(self.function_error(
                            RuleId::CheckedCallableContract,
                            function,
                            at,
                            "slice result projection requires a packed-list descriptor",
                        ));
                    };
                    let LayoutSize::Dynamic { stride, .. } = descriptor.size() else {
                        unreachable!("packed lists have dynamic descriptors")
                    };
                    let Extent::Counted { count, .. } = &extent else {
                        return Err(self.function_error(
                            RuleId::ExactRangeSource,
                            function,
                            at,
                            "slice result projection requires an exact source element count",
                        ));
                    };
                    let source_count =
                        self.require_nonnegative_scalar(state, *count, function, at)?;
                    let end = u128::from(*start)
                        .checked_add(u128::from(*length))
                        .ok_or_else(|| {
                            self.function_error(
                                RuleId::NonWrappingSize,
                                function,
                                at,
                                "slice result bounds overflowed",
                            )
                        })?;
                    if end > source_count {
                        return Err(self.function_error(
                            RuleId::ExactRangeSource,
                            function,
                            at,
                            "slice result projection is outside the source list",
                        ));
                    }
                    let start_definition =
                        self.constant_definition(state, u64::from(*start), function, at)?;
                    let length_definition =
                        self.constant_definition(state, u64::from(*length), function, at)?;
                    actual = actual.project(ProjectionStep::Slice {
                        start: start_definition,
                        length: length_definition,
                    });
                    layout = *element;
                    extent = Extent::Exact(SizeExpr::Constant(
                        u64::from(stride)
                            .checked_mul(u64::from(*length))
                            .ok_or_else(|| {
                                self.function_error(
                                    RuleId::NonWrappingSize,
                                    function,
                                    at,
                                    "slice result extent overflowed",
                                )
                            })?,
                    ));
                }
            }
        }
        Ok((layout, extent, actual))
    }

    fn projections_match_by_value(
        &self,
        left: &crate::safewir::Projection,
        right: &crate::safewir::Projection,
        state: &State,
    ) -> bool {
        left.steps().len() == right.steps().len()
            && left.steps().iter().zip(right.steps()).all(|(left, right)| {
                match (left, right) {
                    (
                        ProjectionStep::Header { component: left },
                        ProjectionStep::Header { component: right },
                    )
                    | (
                        ProjectionStep::Field { index: left },
                        ProjectionStep::Field { index: right },
                    ) => left == right,
                    (
                        ProjectionStep::Element { index: left },
                        ProjectionStep::Element { index: right },
                    ) => matches!(
                        (state.scalars.get(left), state.scalars.get(right)),
                        (Some(left), Some(right)) if left == right
                    ),
                    (
                        ProjectionStep::Slice {
                            start: left_start,
                            length: left_length,
                        },
                        ProjectionStep::Slice {
                            start: right_start,
                            length: right_length,
                        },
                    ) => matches!(
                        (
                            state.scalars.get(left_start),
                            state.scalars.get(right_start),
                            state.scalars.get(left_length),
                            state.scalars.get(right_length),
                        ),
                        (
                            Some(left_start),
                            Some(right_start),
                            Some(left_length),
                            Some(right_length),
                        ) if left_start == right_start && left_length == right_length
                    ),
                    _ => false,
                }
            })
    }

    fn scalar_layout(
        &self,
        kind: ScalarKind,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<LayoutId, ValidationError> {
        self.layouts
            .iter()
            .find_map(|(layout, descriptor)| {
                matches!(descriptor.kind(), LayoutKind::Scalar(actual) if *actual == kind)
                    .then_some(layout)
            })
            .ok_or_else(|| {
                self.function_error(
                    RuleId::CanonicalLayout,
                    function,
                    at,
                    format!("projection requires canonical {kind:?} layout identity"),
                )
            })
    }

    fn constant_definition(
        &self,
        state: &State,
        value: u64,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<DefinitionId, ValidationError> {
        let value = i128::from(value);
        state
            .scalars
            .iter()
            .find_map(|(definition, actual)| (*actual == value).then_some(*definition))
            .ok_or_else(|| {
                self.function_error(
                    RuleId::CheckedCallableContract,
                    function,
                    at,
                    "constant projection has no checked scalar definition",
                )
            })
    }

    fn validate_object_claim(
        &self,
        object: &ObjectRef,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(), ValidationError> {
        if self.layouts.get(object.layout).is_none() {
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
        let (derived_start, derived_length, _) =
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
    ) -> Result<(u128, u128, Option<ScalarKind>), ValidationError> {
        let mut layout = place.object.layout;
        let mut offset = 0_u128;
        let mut length = self.layout_extent(layout, &place.object.extent, state, function, at)?;
        let mut scalar = self.scalar_layout_kind(layout);
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
                    scalar = Some(ScalarKind::U32);
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
                            scalar = Some(kind);
                            length = u128::from(kind.size());
                        }
                        FieldKind::Inline(child) => {
                            layout = child;
                            scalar = self.scalar_layout_kind(child);
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
                    scalar = self.scalar_layout_kind(*element);
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
                    scalar = (count == 1)
                        .then(|| self.scalar_layout_kind(*element))
                        .flatten();
                }
            }
        }
        Ok((offset, length, scalar))
    }

    fn scalar_layout_kind(&self, layout: LayoutId) -> Option<ScalarKind> {
        match self.layouts.get(layout)?.kind() {
            LayoutKind::Scalar(kind) => Some(*kind),
            _ => None,
        }
    }

    fn validate_memory_width(
        &self,
        place: &Place,
        width: MemoryWidth,
        state: &State,
        function: &CandidateFunction,
        at: Option<usize>,
    ) -> Result<(), ValidationError> {
        let (_, length, scalar) = self.derive_projection(place, state, function, at)?;
        if length != width.bytes() || scalar.is_none_or(|kind| !width.matches_scalar(kind)) {
            return Err(self.function_error(
                RuleId::ProjectedLayout,
                function,
                at,
                format!(
                    "memory access {width:?} contradicts projected scalar {scalar:?} with {length} bytes"
                ),
            ));
        }
        Ok(())
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
    state: &State,
) -> bool {
    match (contract, actual) {
        (ContractExtent::Exact(contract), Extent::Exact(actual)) => {
            contract_size_matches(contract, actual, arguments, state)
        }
        (
            ContractExtent::Counted { header, count, stride },
            Extent::Counted {
                header: actual_header,
                count: actual_count,
                stride: actual_stride,
            },
        ) => {
            contract_size_matches(header, actual_header, arguments, state)
                && contract_size_matches(
                    count,
                    &SizeExpr::Definition(*actual_count),
                    arguments,
                    state,
                )
                && contract_size_matches(stride, actual_stride, arguments, state)
        }
        _ => false,
    }
}

fn contract_size_matches(
    contract: &ContractSizeExpr,
    actual: &SizeExpr,
    arguments: &[CallArgument],
    state: &State,
) -> bool {
    match (contract, actual) {
        (ContractSizeExpr::Constant(expected), SizeExpr::Constant(actual)) => expected == actual,
        (ContractSizeExpr::Constant(expected), SizeExpr::Definition(actual)) => state
            .scalars
            .get(actual)
            .and_then(|actual| u64::try_from(*actual).ok())
            == Some(*expected),
        (ContractSizeExpr::Parameter(parameter), SizeExpr::Definition(actual)) => {
            matches!(arguments.get(*parameter), Some(CallArgument::Scalar(expected)) if expected == actual)
        }
        (ContractSizeExpr::Add(a, b), SizeExpr::Add(x, y))
        | (ContractSizeExpr::Multiply(a, b), SizeExpr::Multiply(x, y)) => {
            contract_size_matches(a, x, arguments, state)
                && contract_size_matches(b, y, arguments, state)
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

fn operation_count(operations: &[Operation]) -> Option<usize> {
    let mut total = 0_usize;
    let mut pending = operations.iter().rev().collect::<Vec<_>>();
    while let Some(operation) = pending.pop() {
        total = total.checked_add(1)?;
        match operation {
            Operation::If { then_operations, else_operations, .. } => {
                pending.extend(else_operations.iter().rev());
                pending.extend(then_operations.iter().rev());
            }
            Operation::Block { body, .. } | Operation::Loop { body, .. } => {
                pending.extend(body.iter().rev());
            }
            _ => {}
        }
    }
    Some(total)
}

fn operations(input: &[Operation]) -> Vec<&Operation> {
    let mut output = Vec::new();
    let mut pending = input.iter().rev().collect::<Vec<_>>();
    while let Some(operation) = pending.pop() {
        output.push(operation);
        match operation {
            Operation::If { then_operations, else_operations, .. } => {
                pending.extend(else_operations.iter().rev());
                pending.extend(then_operations.iter().rev());
            }
            Operation::Block { body, .. } | Operation::Loop { body, .. } => {
                pending.extend(body.iter().rev());
            }
            _ => {}
        }
    }
    output
}

fn has_branch_local_object_return(input: &[Operation]) -> bool {
    fn contains_object_return(input: &[Operation]) -> bool {
        operations(input)
            .into_iter()
            .any(|operation| matches!(operation, Operation::ReturnObject { .. }))
    }

    input.iter().any(|operation| match operation {
        Operation::If { then_operations, else_operations, .. } => {
            contains_object_return(then_operations)
                || contains_object_return(else_operations)
                || has_branch_local_object_return(then_operations)
                || has_branch_local_object_return(else_operations)
        }
        Operation::Block { body, .. } | Operation::Loop { body, .. } => {
            has_branch_local_object_return(body)
        }
        _ => false,
    })
}

fn definition_sites(input: &[Operation]) -> Vec<DefinitionId> {
    let mut definitions = Vec::new();
    for operation in operations(input) {
        let definition = match operation {
            Operation::DefineScalar { definition, .. } => Some(*definition),
            Operation::Alloc { result } => Some(result.identity.root),
            Operation::Call { result, .. }
            | Operation::IndirectCall { result, .. }
            | Operation::ClosureCall { result, .. }
            | Operation::HostCall { result, .. } => {
                result.as_ref().map(|result| result.identity.root)
            }
            Operation::ProjectObject { result, .. }
            | Operation::MergeObjectAliases { result, .. } => Some(result.identity.root),
            _ => None,
        };
        definitions.extend(definition);
    }
    definitions
}

fn identity_sites(input: &[Operation]) -> Vec<ObjectIdentity> {
    let mut identities = Vec::new();
    for operation in operations(input) {
        let identity = match operation {
            Operation::Alloc { result }
            | Operation::Reuse { new: result, .. } => Some(result.identity),
            Operation::Call { result, .. }
            | Operation::IndirectCall { result, .. }
            | Operation::ClosureCall { result, .. }
            | Operation::HostCall { result, .. } => {
                result.as_ref().map(|result| result.identity)
            }
            Operation::ProjectObject { result, .. }
            | Operation::MergeObjectAliases { result, .. } => Some(result.identity),
            _ => None,
        };
        identities.extend(identity);
    }
    identities
}

fn control_sites(input: &[Operation]) -> Vec<ControlId> {
    operations(input)
        .into_iter()
        .filter_map(|operation| match operation {
            Operation::Block { control, .. } | Operation::Loop { control, .. } => {
                Some(*control)
            }
            _ => None,
        })
        .collect()
}

fn join_ownership(left: Ownership, right: Ownership) -> Ownership {
    use Ownership::{BorrowedRead, BorrowedWrite, Consumed, Shared, Unique};
    match (left, right) {
        (Consumed, _) | (_, Consumed) => Consumed,
        (Unique, Unique) => Unique,
        (BorrowedWrite, BorrowedWrite | Unique) | (Unique, BorrowedWrite) => BorrowedWrite,
        (BorrowedRead, _) | (_, BorrowedRead) => BorrowedRead,
        (Shared, Shared | Unique) | (Unique, Shared) => Shared,
        (BorrowedWrite, Shared) | (Shared, BorrowedWrite) => BorrowedRead,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{
        ClosedTypeResolver, RcHeader, ReferenceKind, ResolvedNamed, ScalarKind,
    };
    use crate::safewir::{
        ContractProjection, GenerationId, ParameterMemoryContract, ParameterMemoryEffects,
        Projection,
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
                "Float" => ResolvedNamed::Scalar(ScalarKind::Float),
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

    fn scalar_pair_layouts() -> (LayoutBundle, LayoutId, LayoutId) {
        let mut interner = LayoutInterner::new();
        let int = interner
            .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
            .unwrap();
        let float = interner
            .intern_type(&Type::Named("Float".into(), Vec::new()), &Resolver)
            .unwrap();
        (
            LayoutBundle::from_interner(&interner, [int, float]).unwrap(),
            int,
            float,
        )
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
                    Operation::Load {
                        place: place(allocated, AccessMode::Read, 0, 8),
                        width: MemoryWidth::I64,
                    },
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
                    Operation::Load { place: access, width: MemoryWidth::I64 },
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
                    Operation::Load { place: access, width: MemoryWidth::I64 },
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
        let Operation::Load { place, .. } = &mut forged.functions[0].operations[1] else {
            unreachable!()
        };
        place.range.start = SizeExpr::Constant(0);
        assert_rule(forged, RuleId::ProjectedLayout);
    }

    #[test]
    fn scalar_memory_width_and_representation_are_not_interchangeable() {
        let mut narrowed = base_module();
        let Operation::Load { width, .. } = &mut narrowed.functions[0].operations[3] else {
            unreachable!()
        };
        *width = MemoryWidth::I32;
        assert_rule(narrowed, RuleId::ProjectedLayout);

        let mut reinterpreted = base_module();
        let Operation::Load { width, .. } = &mut reinterpreted.functions[0].operations[3] else {
            unreachable!()
        };
        *width = MemoryWidth::F64;
        assert_rule(reinterpreted, RuleId::ProjectedLayout);
    }

    #[test]
    fn branch_join_never_resurrects_conditionally_consumed_authority() {
        let mut accepted = base_module();
        accepted.functions[0].operations.insert(
            3,
            Operation::If {
                condition: DefinitionId::new(1),
                then_operations: Vec::new(),
                else_operations: Vec::new(),
            },
        );
        validate_candidate(&accepted, ValidationBudget::default()).unwrap();

        let mut rejected = base_module();
        rejected.functions[0].operations.insert(
            3,
            Operation::If {
                condition: DefinitionId::new(1),
                then_operations: vec![Operation::Release {
                    object: identity(3, 1),
                }],
                else_operations: Vec::new(),
            },
        );
        assert_rule(rejected, RuleId::ConsumeOnce);

        let mut duplicated = base_module();
        let branch_allocation = object(4, 1, Provenance::Fresh);
        duplicated.functions[0].operations.insert(
            3,
            Operation::If {
                condition: DefinitionId::new(1),
                then_operations: vec![Operation::Alloc {
                    result: branch_allocation.clone(),
                }],
                else_operations: vec![Operation::Alloc {
                    result: branch_allocation,
                }],
            },
        );
        assert_rule(duplicated, RuleId::CheckedCallableContract);
    }

    #[test]
    fn branch_local_alias_effects_survive_the_join() {
        let mut module = base_module();
        module.imports.insert(
            "alias".into(),
            CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::Shared,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::Aliases {
                    parameter: 0,
                    projection: ContractProjection::from_steps([]),
                },
            },
        );
        let parameter = object(20, 1, Provenance::Unknown);
        let alias = ObjectRef {
            identity: identity(21, 1),
            layout: parameter.layout,
            extent: parameter.extent.clone(),
            provenance: Provenance::Aliases {
                object: parameter.identity,
                projection: Projection::root(),
            },
        };
        module.functions = vec![CandidateFunction {
            name: "conditional_alias".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::Shared,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::NoMemory,
            },
            parameters: vec![ParameterBinding {
                definition: parameter.identity.root,
                object: Some(parameter.clone()),
            }],
            operations: vec![
                Operation::DefineScalar {
                    definition: DefinitionId::new(1),
                    value: 1,
                },
                Operation::If {
                    condition: DefinitionId::new(1),
                    then_operations: vec![
                        Operation::Call {
                            callable: "alias".into(),
                            arguments: vec![CallArgument::Object(parameter.identity)],
                            result: Some(alias.clone()),
                        },
                        Operation::Retain {
                            object: alias.identity,
                        },
                    ],
                    else_operations: Vec::new(),
                },
            ],
        }];
        assert_rule(module, RuleId::CheckedCallableContract);
    }

    #[test]
    fn loop_fixpoint_accepts_stable_facts_and_rejects_consumed_back_edges() {
        let mut stable = base_module();
        let repeated_load = stable.functions[0].operations[3].clone();
        stable.functions[0].operations.push(Operation::Loop {
            control: ControlId::new(1),
            body: vec![
                repeated_load,
                Operation::Branch {
                    target: ControlId::new(1),
                    condition: None,
                },
            ],
        });
        let report = validate_candidate(&stable, ValidationBudget::default()).unwrap();
        assert_eq!(report.metrics().operations, 7);

        let mut consumed = base_module();
        consumed.functions[0].operations.push(Operation::Loop {
            control: ControlId::new(1),
            body: vec![
                Operation::Release { object: identity(3, 1) },
                Operation::Branch {
                    target: ControlId::new(1),
                    condition: None,
                },
            ],
        });
        assert_rule(consumed, RuleId::ConsumeOnce);

        let mut reused = base_module();
        reused.functions[0].operations.push(Operation::Loop {
            control: ControlId::new(1),
            body: vec![
                Operation::Reuse {
                    old: identity(3, 1),
                    new: object(3, 2, Provenance::Fresh),
                },
                Operation::Branch {
                    target: ControlId::new(1),
                    condition: None,
                },
            ],
        });
        assert_rule(reused, RuleId::ConsumeOnce);
    }

    #[test]
    fn loop_body_mints_do_not_escape_but_complete_lifetimes_are_accepted() {
        let temporary = object(4, 1, Provenance::Fresh);
        let temporary_place = place(temporary.clone(), AccessMode::Write, 0, 8);

        let mut complete = base_module();
        complete.functions[0].operations.push(Operation::Loop {
            control: ControlId::new(1),
            body: vec![
                Operation::Alloc { result: temporary.clone() },
                Operation::Store {
                    place: temporary_place.clone(),
                    width: MemoryWidth::I64,
                },
                Operation::Release { object: temporary.identity },
                Operation::Branch {
                    target: ControlId::new(1),
                    condition: None,
                },
            ],
        });
        validate_candidate(&complete, ValidationBudget::default()).unwrap();

        let mut branch_local = base_module();
        branch_local.functions[0].operations.push(Operation::Loop {
            control: ControlId::new(1),
            body: vec![
                Operation::If {
                    condition: DefinitionId::new(1),
                    then_operations: vec![Operation::Alloc { result: temporary.clone() }],
                    else_operations: Vec::new(),
                },
                Operation::Load {
                    place: place(temporary, AccessMode::Read, 0, 8),
                    width: MemoryWidth::I64,
                },
                Operation::Branch {
                    target: ControlId::new(1),
                    condition: None,
                },
            ],
        });
        assert_rule(branch_local, RuleId::LiveGeneration);
    }

    #[test]
    fn loop_escape_effect_removes_post_loop_write_authority() {
        let mut module = base_module();
        module.imports.insert(
            "escape".into(),
            CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::Shared,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::NoMemory,
            },
        );
        let allocated = object(3, 1, Provenance::Fresh);
        module.functions[0].operations.extend([
            Operation::Loop {
                control: ControlId::new(1),
                body: vec![
                    Operation::Call {
                        callable: "escape".into(),
                        arguments: vec![CallArgument::Object(allocated.identity)],
                        result: None,
                    },
                    Operation::Branch {
                        target: ControlId::new(1),
                        condition: Some(DefinitionId::new(1)),
                    },
                ],
            },
            Operation::Store {
                place: place(allocated, AccessMode::Write, 0, 8),
                width: MemoryWidth::I64,
            },
        ]);
        assert_rule(module, RuleId::ConsumeOnce);
    }

    #[test]
    fn loop_fixpoint_iterations_consume_the_operation_budget() {
        let mut module = base_module();
        module.functions[0].operations.push(Operation::Loop {
            control: ControlId::new(1),
            body: vec![
                Operation::Retain { object: identity(3, 1) },
                Operation::Branch {
                    target: ControlId::new(1),
                    condition: None,
                },
            ],
        });
        let error = validate_candidate(
            &module,
            ValidationBudget { operations: 8, ..ValidationBudget::default() },
        )
        .unwrap_err();
        assert_eq!(
            error.diagnostic,
            ValidationDiagnostic::Rule(RuleId::BudgetExhaustionFailsClosed)
        );
    }

    #[test]
    fn block_exit_accumulator_never_resurrects_consumed_authority() {
        let mut module = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        module.functions[0].operations.extend([
            Operation::Block {
                control: ControlId::new(1),
                body: vec![Operation::If {
                    condition: DefinitionId::new(1),
                    then_operations: vec![
                        Operation::Release { object: allocated.identity },
                        Operation::Branch {
                            target: ControlId::new(1),
                            condition: None,
                        },
                    ],
                    else_operations: vec![Operation::Branch {
                        target: ControlId::new(1),
                        condition: None,
                    }],
                }],
            },
            Operation::Store {
                place: place(allocated, AccessMode::Write, 0, 8),
                width: MemoryWidth::I64,
            },
        ]);
        assert_rule(module, RuleId::ConsumeOnce);
    }

    #[test]
    fn loop_outer_exit_merge_never_drops_a_consumption_effect() {
        let mut module = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        module.functions[0].operations.extend([
            Operation::Block {
                control: ControlId::new(1),
                body: vec![Operation::Loop {
                    control: ControlId::new(2),
                    body: vec![
                        Operation::Release { object: allocated.identity },
                        Operation::Branch {
                            target: ControlId::new(1),
                            condition: None,
                        },
                    ],
                }],
            },
            Operation::Load {
                place: place(allocated, AccessMode::Read, 0, 8),
                width: MemoryWidth::I64,
            },
        ]);
        assert_rule(module, RuleId::ConsumeOnce);
    }

    #[test]
    fn conditional_void_return_preserves_only_the_fallthrough_path() {
        let mut module = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        module.functions[0].operations.extend([
            Operation::If {
                condition: DefinitionId::new(1),
                then_operations: vec![Operation::ReturnVoid],
                else_operations: Vec::new(),
            },
            Operation::Store {
                place: place(allocated, AccessMode::Write, 0, 8),
                width: MemoryWidth::I64,
            },
        ]);
        validate_candidate(&module, ValidationBudget::default()).unwrap();
    }

    #[test]
    fn void_return_state_is_still_checked_against_parameter_effects() {
        let mut module = base_module();
        let parameter = object(1, 1, Provenance::Unknown);
        module.imports.insert(
            "escape".into(),
            CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::Shared,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                )],
                result: CallableMemoryResult::NoMemory,
            },
        );
        module.functions[0] = CandidateFunction {
            name: "main".into(),
            contract: CallableMemoryContract {
                parameters: vec![object_parameter(
                    ParameterMemoryEntry::Shared,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
                )],
                result: CallableMemoryResult::NoMemory,
            },
            parameters: vec![ParameterBinding {
                definition: parameter.identity.root,
                object: Some(parameter.clone()),
            }],
            operations: vec![
                Operation::Call {
                    callable: "escape".into(),
                    arguments: vec![CallArgument::Object(parameter.identity)],
                    result: None,
                },
                Operation::ReturnVoid,
            ],
        };
        assert_rule(module, RuleId::CheckedCallableContract);
    }

    #[test]
    fn control_census_rejects_duplicate_and_nonlexical_targets() {
        let mut duplicate = base_module();
        duplicate.functions[0].operations.extend([
            Operation::Block { control: ControlId::new(1), body: Vec::new() },
            Operation::Loop { control: ControlId::new(1), body: Vec::new() },
        ]);
        assert_rule(duplicate, RuleId::ConservativeJoin);

        let mut nonlexical = base_module();
        nonlexical.functions[0].operations.push(Operation::Branch {
            target: ControlId::new(77),
            condition: None,
        });
        assert_rule(nonlexical, RuleId::ConservativeJoin);
    }

    #[test]
    fn simultaneous_control_exits_count_against_the_fact_budget() {
        let mut module = base_module();
        module.functions[0].operations.push(Operation::Block {
            control: ControlId::new(1),
            body: vec![Operation::Block {
                control: ControlId::new(2),
                body: vec![
                    Operation::Branch {
                        target: ControlId::new(1),
                        condition: Some(DefinitionId::new(1)),
                    },
                    Operation::Branch {
                        target: ControlId::new(2),
                        condition: Some(DefinitionId::new(1)),
                    },
                ],
            }],
        });
        let error = validate_candidate(
            &module,
            ValidationBudget { facts: 14, ..ValidationBudget::default() },
        )
        .unwrap_err();
        assert_eq!(
            error.diagnostic,
            ValidationDiagnostic::Rule(RuleId::BudgetExhaustionFailsClosed)
        );
    }

    #[test]
    fn structured_control_depth_fails_closed_before_recursive_descent() {
        let mut module = base_module();
        module.functions[0].operations.push(Operation::If {
            condition: DefinitionId::new(1),
            then_operations: vec![Operation::If {
                condition: DefinitionId::new(1),
                then_operations: Vec::new(),
                else_operations: Vec::new(),
            }],
            else_operations: Vec::new(),
        });
        let error = validate_candidate(
            &module,
            ValidationBudget { expression_depth: 1, ..ValidationBudget::default() },
        )
        .unwrap_err();
        assert_eq!(
            error.diagnostic,
            ValidationDiagnostic::Rule(RuleId::BudgetExhaustionFailsClosed)
        );
    }

    #[test]
    fn default_control_depth_fits_a_two_megabyte_validator_stack() {
        let budget = ValidationBudget::default();
        let mut body = Vec::new();
        for control in 0..budget.expression_depth {
            body = vec![Operation::Loop {
                control: ControlId::new(u32::try_from(control).unwrap()),
                body,
            }];
        }
        let mut module = base_module();
        module.functions[0].operations.extend(body);
        std::thread::Builder::new()
            .name("safewir-depth-budget".into())
            .stack_size(2 << 20)
            .spawn(move || validate_candidate(&module, budget))
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
    }

    #[test]
    fn bulk_copy_cannot_reinterpret_same_width_scalar_layouts() {
        let (layouts, int_layout, float_layout) = scalar_pair_layouts();
        let source = ObjectRef {
            identity: identity(1, 1),
            layout: int_layout,
            extent: Extent::Exact(SizeExpr::Constant(8)),
            provenance: Provenance::Fresh,
        };
        let destination = ObjectRef {
            identity: identity(2, 1),
            layout: float_layout,
            extent: Extent::Exact(SizeExpr::Constant(8)),
            provenance: Provenance::Fresh,
        };
        let module = CandidateModule {
            checked_source_entries: 1,
            layouts,
            imports: BTreeMap::new(),
            host_imports: BTreeMap::new(),
            functions: vec![CandidateFunction {
                name: "copy".into(),
                contract: no_memory_contract(),
                parameters: Vec::new(),
                operations: vec![
                    Operation::Alloc { result: source.clone() },
                    Operation::Alloc { result: destination.clone() },
                    Operation::Copy {
                        source: place(source, AccessMode::Read, 0, 8),
                        destination: place(destination, AccessMode::Write, 0, 8),
                        kind: CopyKind::MayOverlap,
                    },
                ],
            }],
            contains_raw_memory: false,
            contains_raw_body: false,
            post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
        };
        assert_rule(module, RuleId::ProjectedLayout);
    }

    #[test]
    fn typed_fill_is_deferred_until_valid_byte_patterns_are_described() {
        let mut module = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        module.functions[0].operations[3] =
            Operation::Fill { destination: place(allocated, AccessMode::Write, 0, 8) };
        assert_unsupported(module, UnsupportedForm::TypedFill);
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
            width: MemoryWidth::I64,
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
        assert_rule(module, RuleId::CheckedCallableContract);
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
            width: MemoryWidth::I64,
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
            width: MemoryWidth::I64,
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
        let Operation::Load { place, .. } = &mut element.functions[0].operations[3] else {
            unreachable!()
        };
        let Extent::Counted { header, .. } = &place.object.extent else {
            unreachable!()
        };
        place.range.start = header.clone();
        validate_candidate(&element, ValidationBudget::default()).unwrap();

        let mut slice = element;
        let Operation::Load { place, .. } = &mut slice.functions[0].operations[3] else {
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
            let Operation::Load { place, .. } =
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
    fn non_root_alias_summary_is_checked_against_the_final_body() {
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
        assert_rule(module, RuleId::CheckedCallableContract);
    }

    #[test]
    fn shared_result_summary_is_checked_against_the_final_body() {
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
        assert_rule(module, RuleId::CheckedCallableContract);
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
                width: MemoryWidth::I64,
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
        let alias = add_identity_alias(&mut module);
        module.functions[0]
            .operations
            .push(Operation::Release { object: alias.identity });
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
            Operation::Load {
                place: place(stale, AccessMode::Read, 0, 8),
                width: MemoryWidth::I64,
            };
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
        module.functions[0].operations[3] =
            Operation::Load { place: forged, width: MemoryWidth::I64 };
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
            RuleId::ConservativeJoin,
            RuleId::LoopFixpoint,
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
        join.functions[0].operations.push(Operation::If {
            condition: DefinitionId::new(1),
            then_operations: vec![Operation::ReturnObject { object: identity(3, 1) }],
            else_operations: Vec::new(),
        });
        assert_unsupported(join, UnsupportedForm::ControlFlowJoin);

        let mut proven = base_module();
        let allocated = object(3, 1, Provenance::Fresh);
        proven.functions[0].operations[3] = Operation::ProvenIndex {
            object: allocated.identity,
            index: DefinitionId::new(1),
            length_source: DefinitionId::new(2),
            place: place(allocated, AccessMode::Read, 0, 8),
        };
        assert_unsupported(proven, UnsupportedForm::ProvenIndex);

    }

    #[test]
    fn recursive_no_memory_component_and_owned_parameter_transfer_are_checked() {
        let mut recursive = base_module();
        recursive.functions[0].operations.push(Operation::Call {
            callable: "main".into(),
            arguments: Vec::new(),
            result: None,
        });
        validate_candidate(&recursive, ValidationBudget::default()).unwrap();

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
                object: Some(parameter.clone()),
            }],
            operations: vec![Operation::Release { object: parameter.identity }],
        });
        validate_candidate(&owned, ValidationBudget::default()).unwrap();
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
