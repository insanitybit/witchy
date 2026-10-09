use std::collections::{BTreeMap, BTreeSet};

use super::super::{
    AccessMode, AllocationClass, CallArgument, Function, FunctionId, FunctionSignature, FunctionSignatureId, MemoryEffect,
    Extent, LoadTransfer, Operation, ParameterContract, ParameterOwnership, Projection, ResultContract,
    ReturnedExtent, Terminator,
    ValueId, ValueType,
};
use super::{FunctionIndex, InitialCheckError};

const MAX_CONTRACT_ITERATIONS: usize = 32;

/// Borrowed syntax only, scoped to one contract derivation. Keep every candidate
/// in scan order, including duplicate destinations in malformed input. This is
/// not a cache of authentication answers or evolving body summaries.
struct CallableDefinitions<'a> {
    by_destination: BTreeMap<ValueId, Vec<&'a Operation>>,
}

impl<'a> CallableDefinitions<'a> {
    fn new(function: &'a Function) -> Self {
        let mut by_destination: BTreeMap<ValueId, Vec<&'a Operation>> = BTreeMap::new();
        for operation in function.blocks.iter().flat_map(|block| &block.operations) {
            let destination = match operation {
                Operation::Load { destination, .. }
                | Operation::MoveValue { destination, .. }
                | Operation::ClosureOwner(
                    crate::canonical_safewir::closure_owner_contract::OwnerOperation::ReadCallable {
                        destination, ..
                    },
                ) => *destination,
                _ => continue,
            };
            by_destination.entry(destination).or_default().push(operation);
        }
        Self { by_destination }
    }
}

/// A set of `MemoryEffect`s held as bits rather than a tree.
///
/// `MemoryEffect` has five variants, so a `BTreeSet` here was a heap-allocated
/// B-tree per handful of bits -- and contract derivation rebuilds every
/// function's summary on each of up to `MAX_CONTRACT_ITERATIONS` rounds, so
/// those allocations dominated the pass (19.2% of an async-compile profile on
/// 2026-09-20, almost all of it inside BTreeMap/BTreeSet code).
///
/// `iter` yields variants in declaration order, which is exactly the derived
/// `Ord` order the `BTreeSet` iterated in, so every consumer sees the same
/// sequence it saw before.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct EffectSet(u8);

impl EffectSet {
    const ALL: [MemoryEffect; 5] = [
        MemoryEffect::Read,
        MemoryEffect::Write,
        MemoryEffect::Retain,
        MemoryEffect::Release,
        MemoryEffect::Reclaim,
    ];

    const fn bit(effect: MemoryEffect) -> u8 { 1u8 << (effect as u8) }

    fn insert(&mut self, effect: MemoryEffect) { self.0 |= Self::bit(effect); }

    const fn contains(self, effect: MemoryEffect) -> bool { self.0 & Self::bit(effect) != 0 }

    fn iter(self) -> impl Iterator<Item = MemoryEffect> {
        Self::ALL.into_iter().filter(move |effect| self.contains(*effect))
    }

    /// Add every effect of `other`, the union `BTreeSet::extend` performed.
    fn union_with(&mut self, other: Self) { self.0 |= other.0; }

    const fn is_subset(self, other: Self) -> bool { self.0 & !other.0 == 0 }
}

impl Extend<MemoryEffect> for EffectSet {
    fn extend<T: IntoIterator<Item = MemoryEffect>>(&mut self, effects: T) {
        for effect in effects { self.insert(effect); }
    }
}

impl FromIterator<MemoryEffect> for EffectSet {
    fn from_iter<T: IntoIterator<Item = MemoryEffect>>(effects: T) -> Self {
        let mut set = Self::default();
        set.extend(effects);
        set
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BodySummary {
    needs: Vec<EffectSet>,
    results: Vec<DerivedResult>,
    effects: BTreeMap<u32, EffectSet>,
    escaped_parameters: BTreeSet<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DerivedResult {
    Unseen,
    Unknown,
    Value(ValueType),
    Fresh {
        layout: crate::layout::LayoutId,
        class: AllocationClass,
        extent: ReturnedExtent,
    },
    StaticData(crate::layout::LayoutId),
    Shared(crate::layout::LayoutId),
    Unique(crate::layout::LayoutId),
    AliasesParameter { parameter: u32, layout: crate::layout::LayoutId },
    /// The sole direct-exclusive result proof: an actual `CloneRef` of an
    /// entry write-back parameter plus the corresponding unmodified hidden
    /// write-back return lane.
    AliasesWriteBackParameter {
        parameter: u32,
        layout: crate::layout::LayoutId,
        projection: Option<u32>,
    },
    /// One branch of a checked direct-exclusive result.  Joining branches
    /// unions only these already-authenticated parameter indexes; it never
    /// turns an arbitrary same-layout reference into an alias candidate.
    AliasesOneOfWriteBackParameters {
        parameters: Vec<u32>,
        layout: crate::layout::LayoutId,
    },
    DisjunctiveWriteBackSelector {
        carrier_result: u32,
        parameters: Vec<u32>,
        selector_parameter: u32,
    },
    ReallocatesParameter { parameter: u32, layout: crate::layout::LayoutId },
}

impl DerivedResult {
    fn join(&mut self, other: Self) {
        if let (
            Self::AliasesOneOfWriteBackParameters { parameters, layout },
            Self::AliasesOneOfWriteBackParameters { parameters: other_parameters, layout: other_layout },
        ) = (&mut *self, &other)
            && *layout == *other_layout
        {
            parameters.extend(other_parameters.iter().copied());
            parameters.sort_unstable();
            parameters.dedup();
            return;
        }
        if matches!(self, Self::Unseen) {
            *self = other;
        } else if matches!(other, Self::Unseen) || *self == other {
            return;
        } else {
            *self = Self::Unknown;
        }
    }

    fn checked_contract(&self, function: FunctionId) -> Result<ResultContract, InitialCheckError> {
        match self {
            Self::Value(value_type) => Ok(ResultContract::Value(*value_type)),
            Self::Fresh { layout, class, extent } => Ok(ResultContract::Fresh {
                layout: *layout,
                class: *class,
                extent: extent.clone(),
            }),
            Self::StaticData(layout) => Ok(ResultContract::StaticData { layout: *layout }),
            Self::Shared(layout) => Ok(ResultContract::Shared { layout: *layout }),
            Self::Unique(layout) => Ok(ResultContract::Unique { layout: *layout }),
            Self::AliasesParameter { parameter, layout } => Ok(ResultContract::AliasesParameter {
                parameter: *parameter,
                layout: *layout,
            }),
            Self::AliasesWriteBackParameter { parameter, layout, projection } => {
                Ok(ResultContract::AliasesWriteBackParameter {
                    parameter: *parameter,
                    layout: *layout,
                    projection: *projection,
                })
            }
            Self::AliasesOneOfWriteBackParameters { parameters, layout } => {
                Ok(ResultContract::AliasesOneOfWriteBackParameters {
                    parameters: parameters.clone(),
                    layout: *layout,
                })
            }
            Self::DisjunctiveWriteBackSelector {
                carrier_result,
                parameters,
                selector_parameter,
            } => Ok(ResultContract::DisjunctiveWriteBackSelector {
                carrier_result: *carrier_result,
                parameters: parameters.clone(),
                selector_parameter: *selector_parameter,
            }),
            Self::ReallocatesParameter { parameter, layout } => {
                Ok(ResultContract::ReallocatesParameter {
                    parameter: *parameter,
                    layout: *layout,
                })
            }
            Self::Unseen | Self::Unknown => Err(InitialCheckError::UnknownCallableContract { function }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Unseen,
    Unknown,
    Parameter { parameter: u32, layout: Option<crate::layout::LayoutId> },
    OpaqueParameter { parameter: u32 },
    Fresh {
        layout: crate::layout::LayoutId,
        class: AllocationClass,
        extent: ReturnedExtent,
    },
    StaticData(crate::layout::LayoutId),
    Shared(crate::layout::LayoutId),
    /// A `CloneRef` directly sourced from an entry parameter. This is not an
    /// alias contract on its own: only the Return tuple proof may promote it
    /// to the dedicated write-back carrier.
    ClonedParameter { parameter: u32, layout: crate::layout::LayoutId },
    ProjectedParameter {
        parameter: u32,
        owner_layout: crate::layout::LayoutId,
        field: u32,
        layout: crate::layout::LayoutId,
    },
    ClonedProjectedParameter {
        parameter: u32,
        owner_layout: crate::layout::LayoutId,
        field: u32,
        layout: crate::layout::LayoutId,
    },
    /// One exact direct-call relay of a proven projected write-back result.
    /// `hidden` is the same call's declaration-derived write-back lane; it
    /// prevents a separately acquired root from completing the relation.
    ForwardedProjectedWriteBack {
        parameter: u32,
        layout: crate::layout::LayoutId,
        field: u32,
        hidden: ValueId,
    },
    ReallocatesParameter { parameter: u32, layout: crate::layout::LayoutId },
}

impl Origin {
    fn result(self, value_type: ValueType) -> DerivedResult {
        match (self, value_type) {
            (Origin::Unseen, _) => DerivedResult::Unseen,
            // A scalar carries no memory provenance: wherever it came from
            // (a constant, a parameter, arithmetic), the callable's result
            // contract is just its value type. Only references need an origin.
            (_, value_type) if is_non_memory_result(value_type) => DerivedResult::Value(value_type),
            (Origin::Fresh { layout, class, extent }, ValueType::Ref(actual)) if layout == actual => {
                DerivedResult::Fresh { layout, class, extent }
            }
            (Origin::StaticData(layout), ValueType::Ref(actual)) if layout == actual => DerivedResult::StaticData(layout),
            (Origin::Shared(layout), ValueType::Ref(actual)) if layout == actual => DerivedResult::Shared(layout),
            (Origin::ClonedParameter { layout, .. }, ValueType::Ref(actual)) if layout == actual => {
                DerivedResult::Shared(layout)
            }
            (Origin::ClonedProjectedParameter { layout, .. }, ValueType::Ref(actual)) if layout == actual => {
                DerivedResult::Shared(layout)
            }
            (Origin::ForwardedProjectedWriteBack { layout, .. }, ValueType::Ref(actual)) if layout == actual => {
                DerivedResult::Shared(layout)
            }
            (Origin::Parameter { parameter, layout: Some(layout) }, ValueType::Ref(actual)) if layout == actual => {
                DerivedResult::AliasesParameter { parameter, layout }
            }
            (Origin::ReallocatesParameter { parameter, layout }, ValueType::Ref(actual)) if layout == actual => {
                DerivedResult::ReallocatesParameter { parameter, layout }
            }
            _ => DerivedResult::Unknown,
        }
    }
}

/// The derived body summaries for every function in a module, as produced by
/// one `derive_all` fixpoint. Opaque to callers: they only ever hand it back.
pub(super) type Summaries = BTreeMap<FunctionId, BodySummary>;

pub(super) fn check_all(
    functions: &FunctionIndex,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    derived: Option<&Summaries>,
) -> Result<(), InitialCheckError> {
    for signature in signatures.values() {
        if signature.contract.results.iter().any(|result| matches!(result, ResultContract::CheckedValue { .. })) {
            return Err(contract_mismatch(signature.id, "CheckedValue memory result"));
        }
    }
    // A summary is a function of the bodies and the signatures' contracts.
    // When the caller has already derived them from inputs it knows are
    // unchanged, re-running the fixpoint reproduces them exactly.
    let owned;
    let summaries = match derived {
        Some(summaries) => summaries,
        None => {
            owned = derive_all(functions, signatures)?;
            &owned
        }
    };
    for function in functions.values() {
        let signature = signatures
            .get(&function.signature)
            .copied()
            .ok_or(InitialCheckError::MissingSignature(function.signature))?;
        check_declaration(signature, &summaries[&function.id])?;
    }
    Ok(())
}

pub(super) fn derive_result_contracts(
    functions: &FunctionIndex,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
) -> Result<(BTreeMap<FunctionSignatureId, Vec<ResultContract>>, Summaries), InitialCheckError> {
    let summaries = derive_all(functions, signatures)?;
    let mut contracts = BTreeMap::new();
    for function in functions.values() {
        let results = summaries[&function.id]
            .results
            .iter()
            .map(|result| result.checked_contract(function.id))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(previous) = contracts.insert(function.signature, results.clone()) {
            if previous != results {
                return Err(contract_mismatch(function.signature, "contracts.rs site 1"));
            }
        }
    }
    Ok((contracts, summaries))
}

/// The previous round's summaries, recording each callee a derivation reads.
/// Every read in the derivation goes through `get`, so the recorded set is
/// exactly what the derived summary depends on.
struct SummaryReads<'a> {
    summaries: &'a BTreeMap<FunctionId, BodySummary>,
    read: std::cell::RefCell<BTreeSet<FunctionId>>,
}

impl<'a> SummaryReads<'a> {
    fn new(summaries: &'a BTreeMap<FunctionId, BodySummary>) -> Self {
        Self { summaries, read: std::cell::RefCell::new(BTreeSet::new()) }
    }

    fn get(&self, function: &FunctionId) -> Option<&'a BodySummary> {
        self.read.borrow_mut().insert(*function);
        self.summaries.get(function)
    }

    fn into_reads(self) -> BTreeSet<FunctionId> {
        self.read.into_inner()
    }
}

fn derive_all(
    functions: &FunctionIndex,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
) -> Result<BTreeMap<FunctionId, BodySummary>, InitialCheckError> {
    let mut summaries = functions
        .iter()
        .map(|(id, function)| Ok((*id, initial_summary(function, signatures)?)))
        .collect::<Result<BTreeMap<_, _>, InitialCheckError>>()?;

    let definitions = functions
        .iter()
        .map(|(id, function)| (*id, CallableDefinitions::new(function)))
        .collect::<BTreeMap<_, _>>();
    // Each round derives every summary from the previous round's. A summary
    // depends only on its own function, the fixed indexes, and the previous
    // summaries it reads, so a function none of whose reads changed last
    // round derives exactly what it derived then; only the others recompute.
    let mut reads = BTreeMap::<FunctionId, BTreeSet<FunctionId>>::new();
    let mut changed: Option<BTreeSet<FunctionId>> = None;
    let mut converged = false;
    for _ in 0..MAX_CONTRACT_ITERATIONS {
        let mut next = BTreeMap::new();
        for (id, function) in functions.iter() {
            let stale = changed.as_ref().is_none_or(|changed| {
                reads.get(id).is_none_or(|read| !read.is_disjoint(changed))
            });
            if !stale {
                next.insert(*id, summaries[id].clone());
                continue;
            }
            let view = SummaryReads::new(&summaries);
            let summary = derive_summary(function, functions, signatures, &view, &definitions[id])?;
            reads.insert(*id, view.into_reads());
            next.insert(*id, summary);
        }
        let now_changed = next.iter()
            .filter(|(id, summary)| summaries.get(id) != Some(summary))
            .map(|(id, _)| *id)
            .collect::<BTreeSet<_>>();
        summaries = next;
        if now_changed.is_empty() {
            converged = true;
            break;
        }
        changed = Some(now_changed);
    }
    if !converged {
        return Err(InitialCheckError::UnknownCallableContract {
            function: functions.keys().next().copied().unwrap_or(FunctionId::new(0)),
        });
    }

    Ok(summaries)
}

fn initial_summary(
    function: &Function,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
) -> Result<BodySummary, InitialCheckError> {
    let signature = signatures
        .get(&function.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(function.signature))?;
    Ok(BodySummary {
        needs: vec![EffectSet::default(); signature.parameters.len()],
        results: signature
            .results
            .iter()
            .copied()
            .map(|value_type| {
                if is_non_memory_result(value_type) {
                    DerivedResult::Value(value_type)
                } else {
                    DerivedResult::Unseen
                }
            })
            .collect(),
        effects: BTreeMap::new(),
        escaped_parameters: BTreeSet::new(),
    })
}

fn derive_summary(
    function: &Function,
    functions: &FunctionIndex,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    summaries: &SummaryReads<'_>,
    definitions: &CallableDefinitions<'_>,
) -> Result<BodySummary, InitialCheckError> {
    let entry = function
        .blocks
        .iter()
        .find(|block| block.id == function.entry)
        .ok_or(InitialCheckError::MissingBlock { function: function.id, block: function.entry })?;
    let parameter_values = entry
        .parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| (parameter.value, index as u32))
        .collect::<BTreeMap<_, _>>();
    let entry_origins = parameter_values
        .iter()
        .map(|(value, parameter)| {
            if matches!(function.value_types.get(value), Some(ValueType::Gc(_))) {
                return (*value, Origin::OpaqueParameter { parameter: *parameter });
            }
            let layout = match function.value_types.get(value).copied() {
                Some(ValueType::Ref(layout)) => Some(layout),
                _ => None,
            };
            (*value, Origin::Parameter { parameter: *parameter, layout })
        })
        .collect::<BTreeMap<_, _>>();
    let mut origins = derive_origins(function, functions, signatures, summaries, &entry_origins, definitions)?;
    let mut summary = initial_summary(function, signatures)?;
    let signature = signatures
        .get(&function.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(function.signature))?;

    for block in &function.blocks {
        for operation in &block.operations {
            derive_operation(
                function,
                operation,
                functions,
                signatures,
                summaries,
                &mut origins,
                &mut summary,
                definitions,
            )?;
        }
        match &block.terminator {
            Terminator::Return { values } => {
                for (index, value) in values.iter().enumerate() {
                    let value_type = function.value_types.get(value).copied().unwrap_or(ValueType::Unit);
                    let result = write_back_reallocation_return_history(
                        signature,
                        &origins,
                        values,
                        index,
                    ).or_else(|| direct_write_back_alias_return_history(
                        function,
                        entry,
                        block,
                        signature,
                        &origins,
                        values,
                        index,
                    )).or_else(|| projected_write_back_alias_return_history(
                        function,
                        entry,
                        signature,
                        &origins,
                        values,
                        index,
                    )).or_else(|| projected_scalar_root_alias_return_history(
                        function,
                        entry,
                        signature,
                        &origins,
                        values,
                        index,
                    )).or_else(|| projected_scalar_forwarded_alias_return_history(
                        function,
                        entry,
                        signature,
                        &origins,
                        values,
                        index,
                    )).unwrap_or_else(|| {
                        origins.get(value).copied().unwrap_or(Origin::Unknown).result(value_type)
                    });
                    if let Some(slot) = summary.results.get_mut(index) {
                        slot.join(result);
                    }
                    mark_escape(&mut summary, origin_of(&origins, *value));
                }
            }
            Terminator::TailCall { callee, arguments, .. } => {
                derive_tail_results(
                    function,
                    *callee,
                    arguments,
                    functions,
                    signatures,
                    summaries,
                    &origins,
                    &mut summary,
                )?;
                derive_call_needs(*callee, arguments, functions, summaries, &origins, &mut summary)?;
            }
            Terminator::VarTailCall {
                callee,
                signature,
                arguments,
                ..
            } => {
                let signature = signatures
                    .get(signature)
                    .copied()
                    .ok_or(InitialCheckError::MissingSignature(*signature))?;
                let target = indirect_target_summary(function, *callee, signature, functions, summaries, definitions)?;
                if target.results.len() != summary.results.len()
                    || signature.results.len() != summary.results.len()
                    || target.results.iter().any(|result| !is_indirect_result(result))
                    || signature.contract.parameters.iter().enumerate().any(|(index, parameter)| {
                        matches!(parameter.value_type, ValueType::Ref(_))
                            && !supported_indirect_reference_parameter(index, parameter, signature)
                    })
                {
                    return Err(InitialCheckError::UnsupportedIndirectCall {
                        function: function.id,
                    });
                }
                for (slot, result) in summary.results.iter_mut().zip(&target.results) {
                    slot.join(result.clone());
                }
                if arguments.len() != signature.parameters.len() {
                    return Err(InitialCheckError::ArityMismatch {
                        context: "derived indirect tail-call arguments",
                        expected: signature.parameters.len(),
                        actual: arguments.len(),
                    });
                }
            }
            Terminator::Branch { .. } | Terminator::BranchIf { .. } | Terminator::Trap | Terminator::Unreachable => {}
        }
    }
    // An owned RC result is a local ABI invariant, not a whole-program origin
    // prediction. Each body's Return is checked independently by MemoryChecker
    // for layout, RC class, live ownership, and exactly-once token transfer.
    // This supports recursive calls and different allocation origins without
    // trusting an alias/freshness summary to grant memory authority. Preserve
    // effect/non-escape inference and the checks for stronger legacy results.
    for (index, declared) in signature.contract.results.iter().enumerate() {
        if let ResultContract::Shared { layout } | ResultContract::Unique { layout } = declared {
            if signature.results.get(index) != Some(&ValueType::Ref(*layout)) {
                return Err(contract_mismatch(signature.id, "contracts.rs site 2"));
            }
            let result = summary.results.get_mut(index).ok_or_else(|| {
                contract_mismatch(signature.id, "declared Shared result has no derived slot")
            })?;
            *result = if matches!(declared, ResultContract::Unique { .. }) { DerivedResult::Unique(*layout) } else { DerivedResult::Shared(*layout) };
        }
    }
    // A returned two-root handle joins through an ordinary SSA reference phi,
    // whose generic origin intentionally becomes unknown.  Reopen only this
    // declaration-authenticated case: the paired selector contract fixes the
    // exact ordered roots, while MemoryChecker proves each returned edge is
    // one of those entry identities and returns the exact selector input.
    for (index, declared) in signature.contract.results.iter().enumerate() {
        let ResultContract::AliasesOneOfWriteBackParameters { parameters, layout } = declared else {
            continue;
        };
        let selector_matches = signature.contract.results.iter().any(|result| {
            matches!(result, ResultContract::DisjunctiveWriteBackSelector {
                carrier_result,
                parameters: selector_parameters,
                ..
            } if *carrier_result as usize == index && selector_parameters == parameters)
        });
        if !selector_matches
            || parameters.len() != 2
            || !parameters.windows(2).all(|pair| pair[0] < pair[1])
            || signature.results.get(index) != Some(&ValueType::Ref(*layout))
            || parameters.iter().any(|parameter| !matches!(signature.contract.parameters.get(*parameter as usize), Some(ParameterContract {
                value_type: ValueType::Ref(parameter_layout),
                ownership: ParameterOwnership::WriteBack,
            }) if *parameter_layout == *layout))
        {
            return Err(contract_mismatch(signature.id, "contracts.rs disjunctive declaration"));
        }
        summary.results[index] = DerivedResult::AliasesOneOfWriteBackParameters {
            parameters: parameters.clone(),
            layout: *layout,
        };
    }
    // This scalar lane is not an ordinary Bool result.  It is the exact
    // source-selector companion of the leading disjunctive alias carrier;
    // return-memory checking additionally proves the returned SSA value is
    // the declared entry parameter, rather than an arbitrary Bool.
    for (index, declared) in signature.contract.results.iter().enumerate() {
        let ResultContract::DisjunctiveWriteBackSelector {
            carrier_result,
            parameters,
            selector_parameter,
        } = declared else { continue };
        let Some(DerivedResult::AliasesOneOfWriteBackParameters {
            parameters: carrier_parameters,
            ..
        }) = summary.results.get(*carrier_result as usize) else {
            return Err(contract_mismatch(signature.id, "contracts.rs disjunctive carrier"));
        };
        if signature.results.get(index) != Some(&ValueType::Bool)
            || !matches!(summary.results.get(index), Some(DerivedResult::Value(ValueType::Bool)))
            || parameters != carrier_parameters
            || parameters.len() != 2
            || !parameters.windows(2).all(|pair| pair[0] < pair[1])
            || !matches!(signature.contract.parameters.get(*selector_parameter as usize), Some(ParameterContract {
                value_type: ValueType::Bool,
                ownership: ParameterOwnership::Value,
            }))
        {
            return Err(contract_mismatch(signature.id, "contracts.rs disjunctive selector"));
        }
        summary.results[index] = DerivedResult::DisjunctiveWriteBackSelector {
            carrier_result: *carrier_result,
            parameters: parameters.clone(),
            selector_parameter: *selector_parameter,
        };
    }
    Ok(summary)
}

/// The scalar projected carrier keeps the aggregate root token as its
/// physical result.  It therefore cannot prove a scalar address by layout;
/// instead the body must perform one exact fixed-Int field read before
/// cloning that same root into the ordinary return lane.  The unmodified root
/// is returned on its declaration-derived hidden write-back lane.  The field
/// ordinal comes only from this history, never from the declared contract.
fn projected_scalar_root_alias_return_history(
    function: &Function,
    entry: &crate::canonical_safewir::BasicBlock,
    signature: &FunctionSignature,
    origins: &BTreeMap<ValueId, Origin>,
    values: &[ValueId],
    result_index: usize,
) -> Option<DerivedResult> {
    if result_index != 0 { return None; }
    let ordinary = *values.first()?;
    let Origin::ClonedParameter { parameter, layout } = origin_of(origins, ordinary) else {
        return None;
    };
    let parameter = usize::try_from(parameter).ok()?;
    let hidden_lane = write_back_hidden_lane_from_signature(signature, parameter, layout)?;
    let ParameterContract {
        value_type: ValueType::Ref(parameter_layout),
        ownership: ParameterOwnership::WriteBack,
    } = signature.contract.parameters.get(parameter)? else {
        return None;
    };
    if *parameter_layout != layout
        || values.len() != signature.results.len()
        || signature.results.first() != Some(&ValueType::Ref(layout))
        || signature.results.get(hidden_lane) != Some(&ValueType::Ref(layout))
    {
        return None;
    }
    let entry_value = entry.parameters.get(parameter)?.value;
    let hidden = *values.get(hidden_lane)?;
    if ordinary == hidden
        || hidden != entry_value
        || function.value_types.get(&ordinary) != Some(&ValueType::Ref(layout))
        || function.value_types.get(&hidden) != Some(&ValueType::Ref(layout))
        || function.blocks.len() != 1
    {
        return None;
    }
    let [Operation::Load { destination: observed, place, transfer: LoadTransfer::CopyScalar },
        Operation::CloneRef { destination, source }, suffix @ ..] = entry.operations.as_slice()
    else { return None; };
    let Projection::Field { index: field } = place.projection() else { return None; };
    if *destination != ordinary
        || *source != entry_value
        || place.object() != entry_value
        || place.value_type() != ValueType::Int
        || place.access() != AccessMode::Read
        || function.value_types.get(observed) != Some(&ValueType::Int)
        || !suffix.iter().all(|operation| matches!(operation, Operation::DropRef { object }
            if entry.parameters.iter().enumerate().any(|(index, parameter_value)| {
                index != parameter && parameter_value.value == *object
            })))
    {
        return None;
    }
    Some(DerivedResult::AliasesWriteBackParameter {
        parameter: u32::try_from(parameter).ok()?,
        layout,
        projection: Some(*field),
    })
}

/// A relay may preserve the scalar carrier only by forwarding the exact root
/// through one direct call whose callee independently derived the selected
/// projection.  This does not accept a matching result layout or a second
/// root: the tagged origin records the corresponding hidden destination from
/// that same call, and this history requires the moved entry parameter.
fn projected_scalar_forwarded_alias_return_history(
    function: &Function,
    entry: &crate::canonical_safewir::BasicBlock,
    signature: &FunctionSignature,
    origins: &BTreeMap<ValueId, Origin>,
    values: &[ValueId],
    result_index: usize,
) -> Option<DerivedResult> {
    if result_index != 0 { return None; }
    let ordinary = *values.first()?;
    let Origin::ForwardedProjectedWriteBack { parameter, layout, field, hidden: expected_hidden } = origin_of(origins, ordinary) else {
        return None;
    };
    let parameter = usize::try_from(parameter).ok()?;
    let hidden_lane = write_back_hidden_lane_from_signature(signature, parameter, layout)?;
    let ParameterContract {
        value_type: ValueType::Ref(parameter_layout),
        ownership: ParameterOwnership::WriteBack,
    } = signature.contract.parameters.get(parameter)? else {
        return None;
    };
    let entry_value = entry.parameters.get(parameter)?.value;
    let hidden = *values.get(hidden_lane)?;
    if *parameter_layout != layout
        || values.len() != signature.results.len()
        || signature.results.first() != Some(&ValueType::Ref(layout))
        || signature.results.get(hidden_lane) != Some(&ValueType::Ref(layout))
        || hidden != expected_hidden
        || ordinary == hidden
        || function.value_types.get(&ordinary) != Some(&ValueType::Ref(layout))
        || function.value_types.get(&hidden) != Some(&ValueType::Ref(layout))
        || function.blocks.len() != 1
    {
        return None;
    }
    let call_index = entry.operations.iter().position(|operation| matches!(operation, Operation::Call { .. }))?;
    let (prefix, rest) = entry.operations.split_at(call_index);
    let [Operation::Call { destinations, arguments, .. }] = &rest[..1] else { return None; };
    let suffix = &rest[1..];
    if destinations.first() != Some(&ordinary)
        || destinations.get(hidden_lane) != Some(&hidden)
        || !matches!(arguments.get(parameter), Some(crate::canonical_safewir::CallArgument::Move(value)) if *value == entry_value)
        || !prefix.iter().all(|operation| matches!(operation, Operation::CloneRef { source, .. }
            if entry.parameters.iter().enumerate().any(|(index, entry_parameter)| {
                index != parameter && entry_parameter.value == *source
            })))
        || !suffix.iter().all(|operation| matches!(operation, Operation::DropRef { object }
            if entry.parameters.iter().enumerate().any(|(index, entry_parameter)| {
                index != parameter && entry_parameter.value == *object
            })))
    {
        return None;
    }
    Some(DerivedResult::AliasesWriteBackParameter {
        parameter: u32::try_from(parameter).ok()?,
        layout,
        projection: Some(field),
    })
}

fn derive_origins(
    function: &Function,
    functions: &FunctionIndex,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    summaries: &SummaryReads<'_>,
    entry_origins: &BTreeMap<ValueId, Origin>,
    definitions: &CallableDefinitions<'_>,
) -> Result<BTreeMap<ValueId, Origin>, InitialCheckError> {
    let blocks = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect::<BTreeMap<_, _>>();
    let mut origins = function
        .blocks
        .iter()
        .flat_map(|block| block.parameters.iter().map(|parameter| (parameter.value, Origin::Unseen)))
        .collect::<BTreeMap<_, _>>();
    origins.extend(entry_origins.iter().map(|(value, origin)| (*value, *origin)));

    let iteration_limit = function.value_types.len().saturating_add(function.blocks.len()).saturating_add(1);
    for _ in 0..iteration_limit {
        let before = origins.clone();
        for block in &function.blocks {
            for operation in &block.operations {
                propagate_operation_origins(
                    function,
                    operation,
                    functions,
                    signatures,
                    summaries,
                    &mut origins,
                    definitions,
                )?;
            }
            match &block.terminator {
                Terminator::Branch { target, arguments } => {
                    propagate_block_arguments(function, &blocks, *target, arguments, &mut origins)?;
                }
                Terminator::BranchIf {
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                    ..
                } => {
                    propagate_block_arguments(function, &blocks, *then_target, then_arguments, &mut origins)?;
                    propagate_block_arguments(function, &blocks, *else_target, else_arguments, &mut origins)?;
                }
                Terminator::Return { .. }
                | Terminator::TailCall { .. }
                | Terminator::VarTailCall { .. }
                | Terminator::Trap
                | Terminator::Unreachable => {}
            }
        }
        if origins == before {
            return Ok(origins);
        }
    }
    Err(InitialCheckError::UnknownCallableContract { function: function.id })
}

fn propagate_operation_origins(
    function: &Function,
    operation: &Operation,
    functions: &FunctionIndex,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    summaries: &SummaryReads<'_>,
    origins: &mut BTreeMap<ValueId, Origin>,
    definitions: &CallableDefinitions<'_>,
) -> Result<(), InitialCheckError> {
    match operation {
        Operation::ClosureOwner(owner) => {
            use crate::canonical_safewir::closure_owner_contract::OwnerOperation as O;
            for destination in owner.destinations() {
                let origin = match owner {
                    O::ReadEnvironmentField { .. } => match function.value_types.get(&destination) {
                        Some(ValueType::Ref(layout)) => Origin::Shared(*layout),
                        _ => Origin::Unknown,
                    },
                    O::CloneEnvironment { .. } => {
                        match function.value_types.get(&destination) {
                            Some(ValueType::Ref(layout)) => Origin::Shared(*layout),
                            _ => return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "CloneEnvironment contract" }),
                        }
                    }
                    O::ReadSharedSlice {
                        field: crate::canonical_safewir::closure_owner_contract::SharedSliceField::Root, ..
                    } => match function.value_types.get(&destination) {
                        Some(ValueType::Ref(layout)) => Origin::Shared(*layout),
                        _ => return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ReadSharedSlice root contract" }),
                    },
                    O::Construct { .. } | O::Deconstruct { .. }
                    | O::ReadCapability { .. }
                    | O::ReadOwner { .. }
                    | O::ReadSharedPlace { .. }
                    | O::ReadSharedSlice { .. }
                    | O::ReadTag { .. }
                    | O::ReadCallable { .. }
                    | O::Clone { .. }
                    | O::Drop { .. }
                    | O::NewList { .. }
                    | O::ListGet { .. }
                    | O::ListLength { .. }
                    | O::CloneList { .. }
                    | O::ListPush { .. }
                    | O::ListSet { .. }
                    | O::DropList { .. } => Origin::Unknown,
                };
                join_origin(origins, destination, origin);
            }
        }
        Operation::Dictionary(dictionary) => {
            use crate::canonical_safewir::DictionaryOperation as D;
            for destination in dictionary.destinations() {
                let origin = match dictionary {
                    D::Initialize { layout, .. } => Origin::Fresh {
                        layout: *layout, class: AllocationClass::ReferenceCounted,
                        extent: ReturnedExtent::Counted { header_bytes: 36, stride_bytes: 33 },
                    },
                    D::Rehash { source, .. } | D::CloneExact { source, .. } => match function.value_types.get(source) {
                        Some(ValueType::Ref(layout)) => Origin::Fresh {
                            layout: *layout, class: AllocationClass::ReferenceCounted,
                            extent: ReturnedExtent::Counted { header_bytes: 36, stride_bytes: 33 },
                        },
                        _ => Origin::Unknown,
                    },
                    D::ReadSlot { field: crate::canonical_safewir::DictionarySlotField::Key
                        | crate::canonical_safewir::DictionarySlotField::Value, .. } =>
                        match function.value_types.get(&destination) {
                            Some(ValueType::Ref(layout)) => Origin::Shared(*layout),
                            _ => Origin::Unknown,
                        },
                    _ => Origin::Unknown,
                };
                join_origin(origins, destination, origin);
            }
        }
        Operation::Alloc { destination, layout, class, extent } => {
            join_origin(origins, *destination, Origin::Fresh {
                layout: *layout,
                class: *class,
                extent: returned_extent(extent),
            });
        }
        Operation::StaticData { destination, layout, .. } => {
            join_origin(origins, *destination, Origin::StaticData(*layout));
        }
        Operation::CloneRef { destination, source } => {
            let origin = match origin_of(origins, *source) {
                origin @ (Origin::Parameter { layout: Some(_), .. }
                | Origin::ReallocatesParameter { .. }) => origin,
                Origin::Fresh { layout, .. } | Origin::StaticData(layout) | Origin::Shared(layout) => {
                    Origin::Shared(layout)
                }
                Origin::Unseen => Origin::Unseen,
                _ => Origin::Unknown,
            };
            join_origin(origins, *destination, origin);
        }
        Operation::MoveValue { destination, source } => {
            let origin = origin_of(origins, *source);
            join_origin(origins, *destination, origin);
        }
        Operation::MakeInlineAggregate { destination, .. }
        | Operation::ExtractInlineField { destination, .. } => {
            join_origin(origins, *destination, Origin::Unknown);
        }
        Operation::CapabilityNone { destination, .. }
        | Operation::CapabilitySome { destination, .. }
        | Operation::CapabilityIsPresent { destination, .. }
        | Operation::CapabilityUnwrap { destination, .. } => {
            join_origin(origins, *destination, Origin::Unknown);
        }
        Operation::FunctionValue { .. } => {}
        Operation::Load {
            destination,
            place,
            transfer,
        } => join_origin(
            origins,
            *destination,
            load_result_origin(
                function,
                *destination,
                place,
                *transfer,
                origin_of(origins, place.object()),
            ),
        ),
        Operation::Call { destinations, callee, arguments } => {
            let summary = summaries.get(callee).ok_or(InitialCheckError::MissingFunction(*callee))?;
            if destinations.len() != summary.results.len() {
                return Err(InitialCheckError::ArityMismatch {
                    context: "derived call results",
                    expected: summary.results.len(),
                    actual: destinations.len(),
                });
            }
            for (destination, result) in destinations.iter().zip(&summary.results) {
                let origin = result_origin(result, arguments, origins, ValueType::Unit);
                join_origin(origins, *destination, origin);
            }
        }
        Operation::VarCall { destinations, callee, signature, arguments } => {
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            let target = indirect_target_summary(function, *callee, signature, functions, summaries, definitions)?;
            if target.results.len() != destinations.len()
                || target.results.iter().any(|result| !is_indirect_result(result))
            {
                return Err(InitialCheckError::UnsupportedIndirectCall { function: function.id });
            }
            for (destination, result) in destinations.iter().zip(&target.results) {
                let value_type = function.value_types.get(destination).copied().unwrap_or(ValueType::Unit);
                join_origin(
                    origins,
                    *destination,
                    result_origin(result, arguments, origins, value_type),
                );
            }
        }
        Operation::ConstInt { destination, .. }
        | Operation::ConstFloat { destination, .. }
        | Operation::ConstUnit { destination }
        | Operation::ConstBool { destination, .. }
        | Operation::ConstSize { destination, .. }
        | Operation::Arithmetic { destination, .. }
        | Operation::Compare { destination, .. }
        | Operation::NarrowSize { destination, .. }
        | Operation::WidenSize { destination, .. }
        | Operation::FloatUnary { destination, .. } => join_origin(origins, *destination, Origin::Unknown),
        Operation::Guard { .. }
        | Operation::InitNullableOwned { .. }
        | Operation::Store { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::UpdateCount { .. }
        | Operation::Retain { .. }
        | Operation::Release { .. }
        | Operation::DropRef { .. }
        | Operation::Reclaim { .. }
        | Operation::ExternalCall { .. }
        | Operation::HostCall { .. }
        | Operation::ReauthenticateRawHeaderExtent { .. } => {}
    }
    Ok(())
}

fn propagate_block_arguments(
    function: &Function,
    blocks: &BTreeMap<super::super::BlockId, &super::super::BasicBlock>,
    target: super::super::BlockId,
    arguments: &[ValueId],
    origins: &mut BTreeMap<ValueId, Origin>,
) -> Result<(), InitialCheckError> {
    let block = blocks
        .get(&target)
        .copied()
        .ok_or(InitialCheckError::MissingBlock { function: function.id, block: target })?;
    if block.parameters.len() != arguments.len() {
        return Err(InitialCheckError::ArityMismatch {
            context: "derived branch arguments",
            expected: block.parameters.len(),
            actual: arguments.len(),
        });
    }
    let incoming = arguments.iter().map(|argument| origin_of(origins, *argument)).collect::<Vec<_>>();
    for (parameter, origin) in block.parameters.iter().zip(incoming) {
        join_origin(origins, parameter.value, origin);
    }
    Ok(())
}

fn join_origin(origins: &mut BTreeMap<ValueId, Origin>, destination: ValueId, incoming: Origin) {
    let current = origins.get(&destination).copied().unwrap_or(Origin::Unseen);
    let joined = match (current, incoming) {
        (Origin::Unseen, value) | (value, Origin::Unseen) => value,
        (left, right) if left == right => left,
        _ => Origin::Unknown,
    };
    origins.insert(destination, joined);
}

#[allow(clippy::too_many_arguments)]
fn derive_operation(
    function: &Function,
    operation: &Operation,
    functions: &FunctionIndex,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    summaries: &SummaryReads<'_>,
    origins: &mut BTreeMap<ValueId, Origin>,
    summary: &mut BodySummary,
    definitions: &CallableDefinitions<'_>,
) -> Result<(), InitialCheckError> {
    match operation {
        Operation::Dictionary(dictionary) => {
            use crate::canonical_safewir::DictionaryOperation as D;
            let effect = match dictionary {
                D::Initialize { .. } => None,
                D::ReadHeader { dict, .. } | D::ReadSlot { dict, .. }
                | D::ProbeGroup { dict, .. } => Some((*dict, MemoryEffect::Read)),
                D::InsertSlot { dict, .. } | D::ReplaceValue { dict, .. }
                | D::RemoveSlot { dict, .. } => Some((*dict, MemoryEffect::Write)),
                D::Rehash { source, .. } | D::CloneExact { source, .. } => Some((*source, MemoryEffect::Read)),
            };
            if let Some((object, effect)) = effect { add_effect(summary, origin_of(origins, object), effect); }
            match dictionary {
                D::InsertSlot { key, value, .. } => {
                    mark_escape(summary, origin_of(origins, *key));
                    mark_escape(summary, origin_of(origins, *value));
                }
                D::ReplaceValue { value, .. } => mark_escape(summary, origin_of(origins, *value)),
                _ => {}
            }
            for destination in dictionary.destinations() {
                let origin = match dictionary {
                    D::Initialize { layout, .. } => Origin::Fresh {
                        layout: *layout, class: AllocationClass::ReferenceCounted,
                        extent: ReturnedExtent::Counted { header_bytes: 36, stride_bytes: 33 },
                    },
                    D::Rehash { source, .. } | D::CloneExact { source, .. } => match function.value_types.get(source) {
                        Some(ValueType::Ref(layout)) => Origin::Fresh {
                            layout: *layout, class: AllocationClass::ReferenceCounted,
                            extent: ReturnedExtent::Counted { header_bytes: 36, stride_bytes: 33 },
                        },
                        _ => Origin::Unknown,
                    },
                    D::ReadSlot { field: crate::canonical_safewir::DictionarySlotField::Key
                        | crate::canonical_safewir::DictionarySlotField::Value, .. } =>
                        match function.value_types.get(&destination) {
                            Some(ValueType::Ref(layout)) => Origin::Shared(*layout),
                            _ => Origin::Unknown,
                        },
                    _ => Origin::Unknown,
                };
                origins.insert(destination, origin);
            }
        }
        Operation::Alloc { destination, layout, class, extent } => {
            origins.insert(*destination, Origin::Fresh {
                layout: *layout,
                class: *class,
                extent: returned_extent(extent),
            });
        }
        Operation::StaticData { destination, layout, .. } => { origins.insert(*destination, Origin::StaticData(*layout)); }
        Operation::CloneRef { destination, source } => {
            let origin = match origin_of(origins, *source) {
                Origin::Parameter { parameter, layout: Some(layout) } => {
                    Origin::ClonedParameter { parameter, layout }
                }
                Origin::ProjectedParameter { parameter, owner_layout, field, layout } => {
                    Origin::ClonedProjectedParameter { parameter, owner_layout, field, layout }
                }
                Origin::Fresh { layout, .. }
                | Origin::StaticData(layout)
                | Origin::Shared(layout)
                | Origin::ReallocatesParameter { layout, .. } => Origin::Shared(layout),
                _ => Origin::Unknown,
            };
            origins.insert(*destination, origin);
            add_effect(summary, origin_of(origins, *source), MemoryEffect::Retain);
        }
        Operation::MoveValue { destination, source } => { origins.insert(*destination, origin_of(origins, *source)); }
        Operation::MakeInlineAggregate { destination, .. }
        | Operation::ExtractInlineField { destination, .. } => { origins.insert(*destination, Origin::Unknown); }
        Operation::FunctionValue { .. } => {}
        Operation::Load { destination, place, transfer } => {
            add_effect(summary, origin_of(origins, place.object()), MemoryEffect::Read);
            if matches!(transfer, LoadTransfer::TakeLast | LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField) {
                add_effect(summary, origin_of(origins, place.object()), MemoryEffect::Write);
            }
            origins.insert(
                *destination,
                load_result_origin(
                    function,
                    *destination,
                    place,
                    *transfer,
                    origin_of(origins, place.object()),
                ),
            );
        }
        Operation::InitNullableOwned { place } => { add_effect(summary, origin_of(origins, place.object()), MemoryEffect::Write); }
        Operation::Store { place, value, transfer } => {
            add_effect(summary, origin_of(origins, place.object()), MemoryEffect::Write);
            if transfer.replaces_owned() {
                add_effect(summary, origin_of(origins, place.object()), MemoryEffect::Read);
                add_effect(summary, origin_of(origins, place.object()), MemoryEffect::Release);
            }
            mark_escape(summary, origin_of(origins, *value));
        }
        Operation::UpdateCount { object, .. } => { add_effect(summary, origin_of(origins, *object), MemoryEffect::Write); }
        Operation::Reclaim { object } => { add_effect(summary, origin_of(origins, *object), MemoryEffect::Reclaim); }
        Operation::Retain { object } => { add_effect(summary, origin_of(origins, *object), MemoryEffect::Retain); }
        Operation::Release { object } => { add_effect(summary, origin_of(origins, *object), MemoryEffect::Release); }
        Operation::DropRef { object } => { add_effect(summary, origin_of(origins, *object), MemoryEffect::Release); }
        Operation::CopyBytes { destination, source, .. } => {
            add_effect(summary, origin_of(origins, source.object()), MemoryEffect::Read);
            add_effect(summary, origin_of(origins, destination.object()), MemoryEffect::Write);
        }
        Operation::MoveRange { destination, source, .. } => {
            add_effect(summary, origin_of(origins, source.object()), MemoryEffect::Write);
            add_effect(summary, origin_of(origins, destination.object()), MemoryEffect::Write);
        }
        Operation::CloneRange { destination, source, .. } => {
            add_effect(summary, origin_of(origins, source.object()), MemoryEffect::Read);
            add_effect(summary, origin_of(origins, destination.object()), MemoryEffect::Write);
        }
        Operation::Call { destinations, callee, arguments } => {
            let callee_summary = summaries.get(callee).ok_or(InitialCheckError::MissingFunction(*callee))?;
            if destinations.len() != callee_summary.results.len() {
                return Err(InitialCheckError::ArityMismatch { context: "derived call results", expected: callee_summary.results.len(), actual: destinations.len() });
            }
            let callee_signature = functions.get(callee)
                .and_then(|callee| signatures.get(&callee.signature).copied())
                .ok_or(InitialCheckError::MissingFunction(*callee))?;
            for (result_index, (destination, result)) in destinations.iter().zip(&callee_summary.results).enumerate() {
                let value_type = function.value_types.get(destination).copied().unwrap_or(ValueType::Unit);
                let forwarded = match result {
                    DerivedResult::AliasesWriteBackParameter { parameter, layout, projection: Some(field) }
                        if result_index == 0 =>
                    {
                        let parameter = usize::try_from(*parameter).ok();
                        let hidden_lane = parameter.and_then(|parameter| {
                            write_back_hidden_lane_from_signature(callee_signature, parameter, *layout)
                        });
                        match (parameter, hidden_lane) {
                            (Some(parameter), Some(hidden_lane)) => match (
                                arguments.get(parameter),
                                destinations.get(hidden_lane),
                            ) {
                                (Some(argument), Some(hidden)) => match origin_of(origins, argument_value(argument)) {
                                    Origin::Parameter { parameter: caller_parameter, layout: Some(caller_layout) }
                                        if caller_layout == *layout && usize::try_from(caller_parameter).ok() == Some(parameter) =>
                                    {
                                        Some(Origin::ForwardedProjectedWriteBack {
                                            parameter: caller_parameter,
                                            layout: *layout,
                                            field: *field,
                                            hidden: *hidden,
                                        })
                                    }
                                    _ => None,
                                },
                                _ => None,
                            },
                            _ => None,
                        }
                    }
                    _ => None,
                };
                origins.insert(*destination, forwarded.unwrap_or_else(|| result_origin(result, arguments, origins, value_type)));
            }
            derive_call_needs(*callee, arguments, functions, summaries, origins, summary)?;
        }
        Operation::VarCall { destinations, callee, signature, arguments } => {
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            let target = indirect_target_summary(function, *callee, signature, functions, summaries, definitions)?;
            if target.results.len() != destinations.len()
                || target.results.iter().any(|result| !is_indirect_result(result))
            {
                return Err(InitialCheckError::UnsupportedIndirectCall { function: function.id });
            }
            for (destination, result) in destinations.iter().zip(&target.results) {
                let value_type = function.value_types.get(destination).copied().unwrap_or(ValueType::Unit);
                origins.insert(
                    *destination,
                    result_origin(result, arguments, origins, value_type),
                );
            }
        }
        Operation::ClosureOwner(owner) => {
            use crate::canonical_safewir::closure_owner_contract::OwnerOperation as O;
            for value in owner.operands() {
                let effect = match owner {
                    O::Drop { .. } | O::DropList { .. } | O::Deconstruct { .. } => MemoryEffect::Release,
                    O::Clone { .. } | O::CloneList { .. } | O::Construct { .. } | O::NewList { .. } => MemoryEffect::Retain,
                    O::ReadCapability { .. }
                    | O::ReadEnvironmentField { .. }
                    | O::ReadOwner { .. }
                    | O::ReadSharedPlace { .. }
                    | O::ReadSharedSlice { .. }
                    | O::ReadTag { .. }
                    | O::ReadCallable { .. }
                    | O::CloneEnvironment { .. }
                    | O::ListGet { .. }
                    | O::ListLength { .. }
                    | O::ListPush { .. } => MemoryEffect::Read,
                    O::ListSet { .. } => MemoryEffect::Read,
                };
                add_effect(summary, origin_of(origins, value), effect);
                let escapes = match owner {
                    O::Construct { .. } | O::NewList { .. } | O::ListPush { .. } | O::ListSet { .. } => true,
                    O::ReadCapability { .. }
                    | O::ReadEnvironmentField { .. }
                    | O::ReadOwner { .. }
                    | O::ReadSharedPlace { .. }
                    | O::ReadSharedSlice { .. }
                    | O::ReadTag { .. }
                    | O::ReadCallable { .. }
                    | O::Clone { .. }
                    | O::Drop { .. }
                    | O::CloneEnvironment { .. }
                    | O::ListGet { .. }
                    | O::ListLength { .. }
                    | O::CloneList { .. }
                    | O::DropList { .. } => false,
                    O::Deconstruct { .. } => false,
                };
                if escapes {
                    mark_escape(summary, origin_of(origins, value));
                }
            }
            for destination in owner.destinations() {
                let origin = match owner {
                    O::ReadEnvironmentField { .. } => match function.value_types.get(&destination) {
                        Some(ValueType::Ref(layout)) => Origin::Shared(*layout),
                        _ => Origin::Unknown,
                    },
                    O::CloneEnvironment { .. } => match function.value_types.get(&destination) {
                        Some(ValueType::Ref(layout)) => Origin::Shared(*layout),
                        _ => return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "CloneEnvironment contract" }),
                    },
                    O::ReadSharedSlice {
                        field: crate::canonical_safewir::closure_owner_contract::SharedSliceField::Root, ..
                    } => match function.value_types.get(&destination) {
                        Some(ValueType::Ref(layout)) => Origin::Shared(*layout),
                        _ => return Err(InitialCheckError::UnsupportedMemoryOperation { function: function.id, operation: "ReadSharedSlice root contract" }),
                    },
                    O::Construct { .. } | O::Deconstruct { .. }
                    | O::ReadCapability { .. }
                    | O::ReadOwner { .. }
                    | O::ReadSharedPlace { .. }
                    | O::ReadSharedSlice { .. }
                    | O::ReadTag { .. }
                    | O::ReadCallable { .. }
                    | O::Clone { .. }
                    | O::Drop { .. }
                    | O::NewList { .. }
                    | O::ListGet { .. }
                    | O::ListLength { .. }
                    | O::CloneList { .. }
                    | O::ListPush { .. }
                    | O::ListSet { .. }
                    | O::DropList { .. } => Origin::Unknown,
                };
                origins.insert(destination, origin);
            }
        }
        Operation::ConstInt { .. } | Operation::ConstFloat { .. } | Operation::ConstUnit { .. } | Operation::ConstBool { .. } | Operation::ConstSize { .. } | Operation::CapabilityNone { .. } | Operation::CapabilitySome { .. } | Operation::CapabilityIsPresent { .. } | Operation::CapabilityUnwrap { .. } | Operation::Arithmetic { .. } | Operation::Compare { .. } | Operation::NarrowSize { .. } | Operation::WidenSize { .. } | Operation::FloatUnary { .. } | Operation::Guard { .. } | Operation::ReauthenticateRawHeaderExtent { .. } | Operation::ExternalCall { .. } | Operation::HostCall { .. } => {}
    }
    Ok(())
}

/// Resolve an indirect callable only through the canonical target set for its
/// checked signature. A callable signature with no lowered body is not an
/// authenticated target. When no body is present, only a callable loaded from
/// an exact checked closure lane may use the signature's ownership-neutral ABI.
/// Unknown, parameter-aliasing, and reallocating results remain fail-closed.
fn indirect_target_summary(
    function: &Function,
    callee: ValueId,
    signature: &FunctionSignature,
    functions: &FunctionIndex,
    summaries: &SummaryReads<'_>,
    definitions: &CallableDefinitions<'_>,
) -> Result<BodySummary, InitialCheckError> {
    let mut target = None;
    for candidate in functions.with_signature(signature.id) {
        let summary = summaries
            .get(&candidate.id)
            .ok_or(InitialCheckError::MissingFunction(candidate.id))?;
        if let Some(joined) = &mut target {
            join_body_summary(joined, summary);
        } else {
            target = Some(summary.clone());
        }
    }
    if let Some(target) = target {
        return Ok(target);
    }
    if authenticated_indirect_callee(definitions, callee, signature.id, &mut BTreeSet::new()) {
        return declared_shared_summary(signature)
            .ok_or(InitialCheckError::UnsupportedIndirectCall { function: function.id });
    }
    Err(InitialCheckError::UnsupportedIndirectCall { function: function.id })
}

/// A targetless indirect call receives ABI authority only when its callable
/// came from a checked closure lane. A raw Callable parameter remains
/// unsupported because its table index has no authenticated signature.
fn authenticated_indirect_callee(
    definitions: &CallableDefinitions<'_>,
    callee: ValueId,
    signature: FunctionSignatureId,
    visited: &mut BTreeSet<ValueId>,
) -> bool {
    if !visited.insert(callee) {
        return false;
    }
    definitions.by_destination.get(&callee).into_iter().flatten().copied().any(|operation| match operation {
        Operation::Load { destination, place, transfer }
            if *destination == callee
                && *transfer == LoadTransfer::CopyScalar
                && place.value_type() == ValueType::Callable(signature) => true,
        Operation::ClosureOwner(
            crate::canonical_safewir::closure_owner_contract::OwnerOperation::ReadCallable {
                destination,
                family,
                ..
            },
        ) if *destination == callee && *family == signature => true,
        Operation::MoveValue { destination, source } if *destination == callee => {
            authenticated_indirect_callee(definitions, *source, signature, visited)
        }
        _ => false,
    })
}

/// An indirect callable may write back a unique reference only when its
/// checked signature explicitly carries the unique lane and no effect beyond
/// the standard read/retain/release/write protocol. The caller's `Move` and
/// the returned `Unique` token are then independently checked by the CFG and
/// memory judgments; a targetless shared-summary fallback never reaches this
/// branch.
fn supported_indirect_reference_parameter(
    index: usize,
    parameter: &ParameterContract,
    signature: &FunctionSignature,
) -> bool {
    let effects = signature
        .contract
        .parameter_effects
        .get(&(index as u32));
    match parameter.ownership {
        ParameterOwnership::Shared | ParameterOwnership::WriteBack => effects
            .is_none_or(|effects| effects.iter().all(|effect| matches!(
                effect,
                MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release,
            ))),
        ParameterOwnership::Unique => effects.is_none_or(|effects| effects.iter().all(|effect| matches!(
            effect, MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release | MemoryEffect::Write,
        ))),
        ParameterOwnership::UniqueWriteBack => effects.is_some_and(|effects| {
            effects.iter().any(|effect| *effect == MemoryEffect::Write)
                && effects.iter().all(|effect| matches!(
                    effect,
                    MemoryEffect::Read
                        | MemoryEffect::Retain
                        | MemoryEffect::Release
                        | MemoryEffect::Write,
                ))
        }),
        ParameterOwnership::Value => exact_indirect_shared_alias_parameter(index, signature),
    }
}

/// A targetless closure call may carry the one checked shared alias lane only
/// past its owned environment parameter.  The exact two-lane contract prevents
/// a raw callable, additional reference, or different result relation from
/// acquiring this exception.
fn exact_indirect_shared_alias_parameter(index: usize, signature: &FunctionSignature) -> bool {
    let ([environment, ValueType::Ref(parameter_layout)], [ValueType::Ref(result_layout)]) =
        (signature.parameters.as_slice(), signature.results.as_slice())
    else { return false };
    let ([environment_contract, parameter], [result]) = (
        signature.contract.parameters.as_slice(),
        signature.contract.results.as_slice(),
    ) else { return false };
    index == 1
        && matches!(environment, ValueType::Ref(_) | ValueType::Gc(_))
        && environment_contract.value_type == *environment
        && environment_contract.ownership == ParameterOwnership::Shared
        && *parameter_layout == *result_layout
        && parameter.ownership == ParameterOwnership::Value
        && parameter.value_type == ValueType::Ref(*parameter_layout)
        && matches!(result, ResultContract::AliasesParameter { parameter: 1, layout }
            if *layout == *parameter_layout)
        && signature.contract.parameter_effects == BTreeMap::from([
            (0, vec![MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release]),
            (1, vec![MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release]),
        ])
}

/// Convert only the ownership-neutral portion of a checked signature into a
/// summary. This cannot mint fresh/static/alias/reallocation authority.
fn declared_shared_summary(signature: &FunctionSignature) -> Option<BodySummary> {
    if signature.contract.parameters.len() != signature.parameters.len()
        || signature.contract.results.len() != signature.results.len()
    {
        return None;
    }
    let mut needs = Vec::with_capacity(signature.parameters.len());
    for (index, (parameter, value_type)) in signature
        .contract
        .parameters
        .iter()
        .zip(&signature.parameters)
        .enumerate()
    {
        if parameter.value_type != *value_type
            || match value_type {
                ValueType::Ref(_) => !matches!(parameter.ownership,
                    ParameterOwnership::Shared | ParameterOwnership::WriteBack),
                ValueType::Gc(_) => parameter.ownership != ParameterOwnership::Shared,
                _ => !matches!(parameter.ownership,
                    ParameterOwnership::Value | ParameterOwnership::WriteBack),
            }
        {
            return None;
        }
        let effects = signature
            .contract
            .parameter_effects
            .get(&(index as u32))
            .cloned()
            .unwrap_or_default();
        if effects.iter().any(|effect| {
            !matches!(effect, MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release)
        }) {
            return None;
        }
        needs.push(effects.into_iter().collect());
    }
    let results = signature
        .contract
        .results
        .iter()
        .zip(&signature.results)
        .map(|(result, value_type)| match result {
            ResultContract::Value(actual) if actual == value_type && !matches!(actual, ValueType::Ref(_)) => {
                Some(DerivedResult::Value(*actual))
            }
            ResultContract::OwnedGc { family } if *value_type == ValueType::Gc(*family) => {
                Some(DerivedResult::Value(*value_type))
            }
            ResultContract::Shared { layout } if *value_type == ValueType::Ref(*layout) => {
                Some(DerivedResult::Shared(*layout))
            }
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(BodySummary {
        needs,
        results,
        effects: signature
            .contract
            .parameter_effects
            .iter()
            .map(|(parameter, effects)| (*parameter, effects.iter().copied().collect()))
            .collect(),
        escaped_parameters: BTreeSet::new(),
    })
}

fn join_body_summary(left: &mut BodySummary, right: &BodySummary) {
    for (left, right) in left.results.iter_mut().zip(&right.results) {
        left.join(right.clone());
    }
    for (left, right) in left.needs.iter_mut().zip(&right.needs) {
        left.union_with(*right);
    }
    for (parameter, effects) in &right.effects {
        left.effects.entry(*parameter).or_default().union_with(*effects);
    }
    left.escaped_parameters.extend(&right.escaped_parameters);
}

fn derive_tail_results(
    function: &Function,
    callee: FunctionId,
    arguments: &[CallArgument],
    functions: &FunctionIndex,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    summaries: &SummaryReads<'_>,
    origins: &BTreeMap<ValueId, Origin>,
    summary: &mut BodySummary,
) -> Result<(), InitialCheckError> {
    let callee_function = functions.get(&callee).ok_or(InitialCheckError::MissingFunction(callee))?;
    let callee_summary = summaries.get(&callee).ok_or(InitialCheckError::MissingFunction(callee))?;
    let callee_signature = signatures
        .get(&callee_function.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(callee_function.signature))?;
    let caller_signature = signatures
        .get(&function.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(function.signature))?;
    let expected_results = summary.results.len();
    for (index, slot) in summary.results.iter_mut().enumerate() {
        let result = callee_summary.results.get(index).ok_or(InitialCheckError::ArityMismatch { context: "derived tail-call results", expected: expected_results, actual: callee_summary.results.len() })?;
        let value_type = callee_signature.results.get(index).copied().ok_or(
            InitialCheckError::ArityMismatch {
                context: "tail-call signature results",
                expected: expected_results,
                actual: callee_signature.results.len(),
            },
        )?;
        let result = reallocated_tail_write_back_result(
            caller_signature,
            index,
            result,
            arguments,
            origins,
        ).unwrap_or_else(|| result_origin(result, arguments, origins, value_type).result(value_type));
        slot.join(result);
    }
    Ok(())
}

/// A tail call can return a reallocated hidden lane after the caller itself
/// has materialized the fresh replacement. Preserve that exact relation only
/// when the callee independently derives the same write-back fact and the
/// moved argument is a concrete fresh allocation; an entry, shared, joined,
/// or unknown argument intentionally falls through to ordinary origin logic.
fn reallocated_tail_write_back_result(
    caller: &FunctionSignature,
    result_index: usize,
    callee_result: &DerivedResult,
    arguments: &[CallArgument],
    origins: &BTreeMap<ValueId, Origin>,
) -> Option<DerivedResult> {
    let DerivedResult::ReallocatesParameter { parameter: callee_parameter, layout } = callee_result else {
        return None;
    };
    let Origin::Fresh { layout: argument_layout, .. } = origin_of(origins, argument_value(arguments.get(*callee_parameter as usize)?)) else {
        return None;
    };
    if argument_layout != *layout {
        return None;
    }
    let write_back_parameters = caller.contract.parameters.iter().enumerate()
        .filter_map(|(parameter, contract)| {
            (contract.ownership == ParameterOwnership::WriteBack).then_some(parameter)
        })
        .collect::<Vec<_>>();
    let ordinary_results = caller.results.len().checked_sub(write_back_parameters.len())?;
    let ordinal = result_index.checked_sub(ordinary_results)?;
    let parameter = *write_back_parameters.get(ordinal)?;
    matches!(caller.contract.parameters.get(parameter), Some(ParameterContract {
        value_type: ValueType::Ref(parameter_layout),
        ownership: ParameterOwnership::WriteBack,
    }) if *parameter_layout == *layout)
        .then_some(DerivedResult::ReallocatesParameter {
            parameter: u32::try_from(parameter).ok()?,
            layout: *layout,
        })
}

fn derive_call_needs(
    callee: FunctionId,
    arguments: &[CallArgument],
    functions: &FunctionIndex,
    summaries: &SummaryReads<'_>,
    origins: &BTreeMap<ValueId, Origin>,
    summary: &mut BodySummary,
) -> Result<(), InitialCheckError> {
    if !functions.contains_key(&callee) { return Err(InitialCheckError::MissingFunction(callee)); }
    let callee_summary = summaries.get(&callee).ok_or(InitialCheckError::MissingFunction(callee))?;
    for (index, argument) in arguments.iter().enumerate() {
        let origin = origin_of(origins, argument_value(argument));
        let parameter = match origin {
            Origin::Parameter { parameter, .. } | Origin::ReallocatesParameter { parameter, .. } => parameter,
            _ => continue,
        };
        let need = callee_summary
            .needs
            .get(index)
            .ok_or_else(|| contract_mismatch(functions[&callee].signature, "call argument beyond the callee's parameter needs"))?;
        let slot = summary
            .needs
            .get_mut(parameter as usize)
            .ok_or_else(|| contract_mismatch(functions[&callee].signature, "argument origin names a parameter beyond the caller's needs"))?;
        slot.union_with(*need);
    }
    Ok(())
}

fn origin_of(origins: &BTreeMap<ValueId, Origin>, value: ValueId) -> Origin {
    origins.get(&value).copied().unwrap_or(Origin::Unknown)
}

/// Prove the dedicated returned-exclusive carrier from its actual return
/// history. Parameter ownership and result arity determine the hidden lane;
/// the declared result contract never supplies provenance. The bounded slice
/// accepts only a pure direct clone, so mutation or replacement histories
/// fail closed until they acquire their own proof.
///
/// A hidden `var` write-back lane has no source result position to carry this
/// relation.  Recover it only from the lowered terminal value: every terminal
/// reaching this lane must carry an actual fresh allocation of the matching
/// reference layout.  The enclosing summary join turns a parameter/shared or
/// unknown path into `Unknown`, so callers can never receive a speculative
/// reallocation contract.
fn write_back_reallocation_return_history(
    signature: &FunctionSignature,
    origins: &BTreeMap<ValueId, Origin>,
    values: &[ValueId],
    result_index: usize,
) -> Option<DerivedResult> {
    let write_back_parameters = signature.contract.parameters.iter().enumerate()
        .filter_map(|(parameter, contract)| {
            (contract.ownership == ParameterOwnership::WriteBack).then_some(parameter)
        })
        .collect::<Vec<_>>();
    let ordinary_results = signature.results.len().checked_sub(write_back_parameters.len())?;
    let ordinal = result_index.checked_sub(ordinary_results)?;
    let parameter = *write_back_parameters.get(ordinal)?;
    let ParameterContract {
        value_type: ValueType::Ref(layout),
        ownership: ParameterOwnership::WriteBack,
    } = signature.contract.parameters.get(parameter)? else {
        return None;
    };
    if values.len() != signature.results.len()
        || signature.results.get(result_index) != Some(&ValueType::Ref(*layout))
    {
        return None;
    }
    let Origin::Fresh { layout: actual_layout, .. } = origin_of(origins, *values.get(result_index)?) else {
        return None;
    };
    (*layout == actual_layout).then_some(DerivedResult::ReallocatesParameter {
        parameter: u32::try_from(parameter).ok()?,
        layout: *layout,
    })
}

fn direct_write_back_alias_return_history(
    function: &Function,
    entry: &crate::canonical_safewir::BasicBlock,
    block: &crate::canonical_safewir::BasicBlock,
    signature: &FunctionSignature,
    origins: &BTreeMap<ValueId, Origin>,
    values: &[ValueId],
    result_index: usize,
) -> Option<DerivedResult> {
    if result_index != 0 {
        return None;
    }
    let ordinary = *values.first()?;
    let Origin::ClonedParameter { parameter, layout } = origin_of(origins, ordinary) else {
        return None;
    };
    let parameter = usize::try_from(parameter).ok()?;
    let hidden_lane = write_back_hidden_lane_from_signature(signature, parameter, layout)?;
    if values.len() != signature.results.len()
        || function.value_types.get(&ordinary) != Some(&ValueType::Ref(layout))
    {
        return None;
    }
    let ParameterContract {
        value_type: ValueType::Ref(parameter_layout),
        ownership: ParameterOwnership::WriteBack,
    } = signature.contract.parameters.get(parameter)? else {
        return None;
    };
    if *parameter_layout != layout {
        return None;
    }
    let entry_value = entry.parameters.get(parameter)?.value;
    let hidden = *values.get(hidden_lane)?;
    if ordinary == hidden
        || hidden != entry_value
        || function.value_types.get(&hidden) != Some(&ValueType::Ref(layout))
        || !is_read_only_direct_clone_history(&block.operations, ordinary, entry_value)
    {
        return None;
    }
    let parameter = u32::try_from(parameter).ok()?;
    match signature.contract.results.first() {
        Some(ResultContract::AliasesOneOfWriteBackParameters { parameters, layout: declared_layout })
            if *declared_layout == layout && parameters.binary_search(&parameter).is_ok() =>
        {
            Some(DerivedResult::AliasesOneOfWriteBackParameters {
                parameters: vec![parameter],
                layout,
            })
        }
        _ => Some(DerivedResult::AliasesWriteBackParameter {
            parameter,
            layout,
            projection: None,
        }),
    }
}

/// The lowering establishes fixed-layout/string extent before cloning its
/// returned handle. These scalar guards are observational only. Refuse every
/// operation that can write, transfer, retain/release, or create a replacement
/// token before the final clone.
fn is_read_only_direct_clone_history(
    operations: &[Operation],
    ordinary: ValueId,
    entry_value: ValueId,
) -> bool {
    let Some((last, prefix)) = operations.split_last() else { return false };
    matches!(last, Operation::CloneRef { destination, source }
        if *destination == ordinary && *source == entry_value)
        && prefix.iter().all(|operation| match operation {
            Operation::Load { transfer: LoadTransfer::CopyScalar, place, .. } => place.access == AccessMode::Read,
            Operation::Compare { .. } | Operation::Guard { .. } | Operation::ConstSize { .. } => true,
            _ => false,
        })
}

/// The projected sibling of the direct carrier. It proves the sole fixed
/// `Load CloneShared(entry.field)` followed immediately by the retained
/// ordinary clone. The hidden lane remains the unmodified aggregate token;
/// neither a matching result layout nor a declared contract can replace this
/// physical history.
fn projected_write_back_alias_return_history(
    function: &Function,
    entry: &crate::canonical_safewir::BasicBlock,
    signature: &FunctionSignature,
    origins: &BTreeMap<ValueId, Origin>,
    values: &[ValueId],
    result_index: usize,
) -> Option<DerivedResult> {
    if result_index != 0 {
        return None;
    }
    let ordinary = *values.first()?;
    let Origin::ClonedProjectedParameter { parameter, owner_layout, field, layout } = origin_of(origins, ordinary) else {
        return None;
    };
    let parameter = usize::try_from(parameter).ok()?;
    let hidden_lane = write_back_hidden_lane_from_signature(signature, parameter, layout)?;
    let ParameterContract {
        value_type: ValueType::Ref(parameter_layout),
        ownership: ParameterOwnership::WriteBack,
    } = signature.contract.parameters.get(parameter)? else {
        return None;
    };
    if *parameter_layout != owner_layout
        || values.len() != signature.results.len()
        || function.value_types.get(&ordinary) != Some(&ValueType::Ref(layout))
        || signature.results.first() != Some(&ValueType::Ref(layout))
        || signature.results.get(hidden_lane) != Some(&ValueType::Ref(owner_layout))
    {
        return None;
    }
    let entry_value = entry.parameters.get(parameter)?.value;
    let hidden = *values.get(hidden_lane)?;
    if ordinary == hidden
        || hidden != entry_value
        || function.value_types.get(&hidden) != Some(&ValueType::Ref(owner_layout))
        || function.blocks.len() != 1
        || !is_read_only_projected_clone_history(
            &entry.operations,
            ordinary,
            entry_value,
            field,
            layout,
        )
    {
        return None;
    }
    Some(DerivedResult::AliasesWriteBackParameter {
        parameter: u32::try_from(parameter).ok()?,
        layout,
        projection: Some(field),
    })
}

fn is_read_only_projected_clone_history(
    operations: &[Operation],
    ordinary: ValueId,
    entry_value: ValueId,
    field: u32,
    layout: crate::layout::LayoutId,
) -> bool {
    let [Operation::Load {
        destination: loaded,
        place,
        transfer: LoadTransfer::CloneShared,
    }, Operation::CloneRef { destination, source }, Operation::DropRef { object }, suffix @ ..] = operations else {
        return false;
    };
    *destination == ordinary
        && *source == *loaded
        && *object == *loaded
        && place.object() == entry_value
        && place.projection() == &Projection::Field { index: field }
        && place.value_type() == ValueType::Ref(layout)
        && place.access() == AccessMode::Read
        && is_read_only_string_extent_suffix(suffix, ordinary)
}

/// The returned projected handle may acquire its String extent after its
/// precise clone.  That authentication reads only the clone's own scalar
/// header fields and carries those scalars through bounds comparisons/guards;
/// it cannot touch another object or create a second reference token.
fn is_read_only_string_extent_suffix(operations: &[Operation], ordinary: ValueId) -> bool {
    let mut scalars = BTreeSet::new();
    let mut conditions = BTreeSet::new();
    operations.iter().all(|operation| match operation {
        Operation::Load {
            destination,
            place,
            transfer: LoadTransfer::CopyScalar,
        } if place.object() == ordinary
            && matches!(place.projection(), Projection::Header(
                crate::canonical_safewir::HeaderField::Length
                    | crate::canonical_safewir::HeaderField::Capacity
            ))
            && place.value_type() == ValueType::Size
            && place.access() == AccessMode::Read =>
        {
            scalars.insert(*destination)
        }
        Operation::ConstSize { destination, .. } => scalars.insert(*destination),
        Operation::Compare { destination, left, right, .. }
            if scalars.contains(left) && scalars.contains(right) =>
        {
            conditions.insert(*destination)
        }
        Operation::Guard { condition, kind: crate::canonical_safewir::GuardKind::Bounds }
            if conditions.contains(condition) => true,
        _ => false,
    })
}

/// Reconstruct a hidden write-back position from the authenticated parameter
/// ownership order and result ABI shape. It intentionally does not inspect a
/// result contract: contracts are checked against this derived history later.
fn write_back_hidden_lane_from_signature(
    signature: &FunctionSignature,
    parameter: usize,
    layout: crate::layout::LayoutId,
) -> Option<usize> {
    let ParameterContract {
        value_type: ValueType::Ref(parameter_layout),
        ownership: ParameterOwnership::WriteBack,
    } = signature.contract.parameters.get(parameter)? else {
        return None;
    };
    let write_back_parameters = signature.contract.parameters.iter()
        .enumerate()
        .filter(|(_, contract)| contract.ownership == ParameterOwnership::WriteBack)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let ordinal = write_back_parameters.iter().position(|index| *index == parameter)?;
    let ordinary_results = signature.results.len().checked_sub(write_back_parameters.len())?;
    let hidden_lane = ordinary_results.checked_add(ordinal)?;
    (ordinary_results > 0
        && hidden_lane != 0
        && signature.results.first() == Some(&ValueType::Ref(layout))
        && signature.results.get(hidden_lane) == Some(&ValueType::Ref(*parameter_layout)))
        .then_some(hidden_lane)
}

fn load_result_origin(
    function: &Function,
    destination: ValueId,
    place: &crate::canonical_safewir::Place,
    transfer: LoadTransfer,
    object_origin: Origin,
) -> Origin {
    match (transfer, place.value_type(), function.value_types.get(&destination).copied()) {
        (
            LoadTransfer::CloneShared,
            ValueType::Ref(layout),
            Some(ValueType::Ref(destination_layout)),
        ) if layout == destination_layout => match (object_origin, place.projection()) {
            (
                Origin::Parameter { parameter, layout: Some(owner_layout) },
                Projection::Field { index },
            ) => Origin::ProjectedParameter {
                parameter,
                owner_layout,
                field: *index,
                layout,
            },
            _ => Origin::Shared(layout),
        },
        (
            LoadTransfer::CloneShared | LoadTransfer::TakeLast | LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField,
            ValueType::Ref(place_layout),
            Some(ValueType::Ref(destination_layout)),
        ) if place_layout == destination_layout => Origin::Shared(place_layout),
        _ => Origin::Unknown,
    }
}

fn result_origin(result: &DerivedResult, arguments: &[CallArgument], origins: &BTreeMap<ValueId, Origin>, _value_type: ValueType) -> Origin {
    match result {
        DerivedResult::Fresh { layout, class, extent } => Origin::Fresh {
            layout: *layout,
            class: *class,
            extent: extent.clone(),
        },
        DerivedResult::StaticData(layout) => Origin::StaticData(*layout),
        DerivedResult::Shared(layout) | DerivedResult::Unique(layout) => Origin::Shared(*layout),
        DerivedResult::AliasesParameter { parameter, layout } => arguments.get(*parameter as usize).and_then(|argument| origins.get(&argument_value(argument)).copied()).map(|origin| match origin { Origin::Parameter { parameter, .. } => Origin::Parameter { parameter, layout: Some(*layout) }, other => other }).unwrap_or(Origin::Unknown),
        DerivedResult::AliasesWriteBackParameter { layout, .. }
        | DerivedResult::AliasesOneOfWriteBackParameters { layout, .. } => Origin::Shared(*layout),
        DerivedResult::ReallocatesParameter { parameter, layout } => arguments.get(*parameter as usize).and_then(|argument| origins.get(&argument_value(argument)).copied()).and_then(|origin| match origin { Origin::Parameter { parameter, .. } => Some(Origin::ReallocatesParameter { parameter, layout: *layout }), _ => None }).unwrap_or(Origin::Unknown),
        DerivedResult::Unseen => Origin::Unseen,
        DerivedResult::DisjunctiveWriteBackSelector { .. }
        | DerivedResult::Unknown | DerivedResult::Value(_) => Origin::Unknown,
    }
}

fn argument_value(argument: &CallArgument) -> ValueId {
    match argument { CallArgument::Value(value) | CallArgument::Move(value) => *value }
}

fn add_effect(summary: &mut BodySummary, origin: Origin, effect: MemoryEffect) {
    // Only a reference parameter has memory effects; a scalar parameter's
    // origin (`layout: None`) carries no memory.
    let (Origin::Parameter { parameter, layout: Some(_) } | Origin::OpaqueParameter { parameter } | Origin::ReallocatesParameter { parameter, .. }) = origin else { return; };
    summary.effects.entry(parameter).or_default().insert(effect);
    if let Some(slot) = summary.needs.get_mut(parameter as usize) {
        slot.insert(effect);
    }
}

fn mark_escape(summary: &mut BodySummary, origin: Origin) {
    // Returning or storing a scalar parameter is not an escape of memory: a
    // value with no layout has nothing to read, retain, or release.
    if let Origin::Parameter { parameter, layout: Some(_) } | Origin::OpaqueParameter { parameter } | Origin::ReallocatesParameter { parameter, .. } = origin {
        summary.escaped_parameters.insert(parameter);
        if let Some(need) = summary.needs.get_mut(parameter as usize) {
            need.insert(MemoryEffect::Read);
        }
    }
}

fn check_declaration(signature: &FunctionSignature, summary: &BodySummary) -> Result<(), InitialCheckError> {
    if signature.contract.parameters.len() != signature.parameters.len() || signature.contract.results.len() != signature.results.len() { return Err(contract_mismatch(signature.id, "contracts.rs site 6")); }
    for (parameter, value_type) in signature.contract.parameters.iter().zip(&signature.parameters) {
        if parameter.value_type != *value_type { return Err(contract_mismatch(signature.id, "contracts.rs site 7")); }
    }
    for (index, result) in signature.contract.results.iter().enumerate() {
        if !result_matches(result, &summary.results[index]) { return Err(contract_mismatch(signature.id, "contracts.rs site 8")); }
    }
    for (parameter, effects) in &signature.contract.parameter_effects {
        if *parameter as usize >= signature.parameters.len() || effects.iter().copied().collect::<BTreeSet<_>>().len() != effects.len() { return Err(contract_mismatch(signature.id, "contracts.rs site 9")); }
    }
    for (index, need) in summary.needs.iter().enumerate() {
        let parameter = &signature.contract.parameters[index];
        let declared = signature.contract.parameter_effects.get(&(index as u32)).map(|effects| effects.iter().copied().collect::<EffectSet>()).unwrap_or_default();
        let derived = summary.effects.get(&(index as u32)).copied().unwrap_or_default();
        if !derived.is_subset(declared) || !ownership_supports(parameter, *need) { return Err(contract_mismatch(signature.id, "contracts.rs site 10")); }
    }
    Ok(())
}

fn ownership_supports(parameter: &ParameterContract, needs: EffectSet) -> bool {
    needs.iter().all(|need| match (parameter.ownership, need) {
        (ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack | ParameterOwnership::WriteBack, _) => true,
        (ParameterOwnership::Shared, MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release) => true,
        (ParameterOwnership::Value, MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release)
            if parameter.value_type.is_linear_memory_ref() => true,
        _ => false,
    })
}

fn result_matches(declared: &ResultContract, derived: &DerivedResult) -> bool {
    match (declared, derived) {
        (ResultContract::Unique { layout: expected }, DerivedResult::Unique(actual)) => expected == actual,
        (ResultContract::OwnedGc { family }, DerivedResult::Value(ValueType::Gc(actual))) => family == actual,
        (ResultContract::Value(expected), DerivedResult::Value(actual)) => expected == actual,
        (ResultContract::CheckedValue { layout: expected }, DerivedResult::Fresh { layout: actual, .. })
        | (ResultContract::CheckedValue { layout: expected }, DerivedResult::StaticData(actual))
        | (ResultContract::CheckedValue { layout: expected }, DerivedResult::Shared(actual)) => {
            expected == actual
        }
        (
            ResultContract::Fresh { layout: expected_layout, class: expected_class, extent: expected_extent },
            DerivedResult::Fresh { layout: actual_layout, class: actual_class, extent: actual_extent },
        ) => expected_layout == actual_layout && expected_class == actual_class && expected_extent == actual_extent,
        (ResultContract::StaticData { layout: expected }, DerivedResult::StaticData(actual))
        | (ResultContract::Shared { layout: expected }, DerivedResult::Shared(actual)) => expected == actual,
        (ResultContract::AliasesParameter { parameter, layout },
            DerivedResult::AliasesParameter { parameter: actual_parameter, layout: actual_layout })
        | (ResultContract::ReallocatesParameter { parameter, layout },
            DerivedResult::ReallocatesParameter { parameter: actual_parameter, layout: actual_layout }) => {
            parameter == actual_parameter && layout == actual_layout
        }
        (
            ResultContract::AliasesWriteBackParameter { parameter, layout, projection },
            DerivedResult::AliasesWriteBackParameter {
                parameter: actual_parameter,
                layout: actual_layout,
                projection: actual_projection,
            },
        ) => parameter == actual_parameter && layout == actual_layout && projection == actual_projection,
        (
            ResultContract::AliasesOneOfWriteBackParameters { parameters, layout },
            DerivedResult::AliasesOneOfWriteBackParameters {
                parameters: actual_parameters,
                layout: actual_layout,
            },
        ) => parameters.len() >= 2
            && parameters.windows(2).all(|pair| pair[0] < pair[1])
            && parameters == actual_parameters
            && layout == actual_layout,
        (
            ResultContract::DisjunctiveWriteBackSelector {
                carrier_result,
                parameters,
                selector_parameter,
            },
            DerivedResult::DisjunctiveWriteBackSelector {
                carrier_result: actual_carrier,
                parameters: actual_parameters,
                selector_parameter: actual_selector,
            },
        ) => carrier_result == actual_carrier
            && parameters == actual_parameters
            && selector_parameter == actual_selector,
        _ => false,
    }
}


/// Every contract refusal in this module goes through here so a developer can
/// see which judgment fired (`WITCHY_CANONICAL_PLAN_DEBUG=1`); the refusal
/// itself is unchanged.
fn contract_mismatch(signature: FunctionSignatureId, site: &'static str) -> InitialCheckError {
    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
        eprintln!("canonical contract mismatch: signature {signature:?} at {site}");
    }
    InitialCheckError::CallableContractMismatch { signature }
}

fn is_non_memory_result(value_type: ValueType) -> bool {
    matches!(value_type, ValueType::Int | ValueType::Float | ValueType::Bool
        | ValueType::Unit | ValueType::Size | ValueType::V128
        | ValueType::Capability(_) | ValueType::NullableCapability(_) | ValueType::Gc(_))
}

fn is_indirect_result(result: &DerivedResult) -> bool {
    matches!(
        result,
        DerivedResult::Value(_)
            | DerivedResult::Fresh { .. }
            | DerivedResult::StaticData(_)
            | DerivedResult::Shared(_)
            | DerivedResult::Unique(_)
            | DerivedResult::AliasesParameter { .. }
            // A `var` parameter's write-back. The argument side already admits
            // `ParameterOwnership::WriteBack` and `result_origin` already maps
            // this to `Origin::ReallocatesParameter`; only this gate was
            // missing. It stays closed for a disagreeing call site, because
            // `indirect_target_summary` joins every function sharing the
            // signature and `DerivedResult::join` collapses unequal results to
            // `Unknown`, which is not admitted here.
            | DerivedResult::ReallocatesParameter { .. }
            | DerivedResult::Unseen
    )
}

fn returned_extent(extent: &Extent) -> ReturnedExtent {
    match extent {
        Extent::FixedBytes(bytes) => ReturnedExtent::FixedBytes(*bytes),
        Extent::Counted { header_bytes, stride_bytes, .. } => ReturnedExtent::Counted {
            header_bytes: *header_bytes,
            stride_bytes: *stride_bytes,
        },
        Extent::HeaderPrefix { minimum_bytes } => ReturnedExtent::FixedBytes(u64::from(*minimum_bytes)),
    }
}

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod self_checks {
    use super::*;


    pub fn indirect_writeback_summary_requires_owned_results_without_provenance_claims() {
        let reference = ValueType::Ref(layout());
        let mut signature = FunctionSignature {
            id: FunctionSignatureId::new(1),
            parameters: vec![reference, reference],
            results: vec![reference, reference],
            contract: super::super::super::CallableContract {
                parameters: vec![ParameterContract {
                    value_type: reference,
                    ownership: ParameterOwnership::WriteBack,
                }; 2],
                results: vec![ResultContract::Shared { layout: layout() }; 2],
                parameter_effects: BTreeMap::new(),
            },
        };
        let summary = declared_shared_summary(&signature).expect("owned replacement ABI");
        assert_eq!(summary.results, vec![DerivedResult::Shared(layout()); 2]);
        for unsupported in [
            ResultContract::BodyDerived(reference),
            ResultContract::AliasesParameter { parameter: 0, layout: layout() },
            ResultContract::ReallocatesParameter { parameter: 0, layout: layout() },
        ] {
            signature.contract.results[0] = unsupported;
            assert!(declared_shared_summary(&signature).is_none());
        }
        signature.contract.results[0] = ResultContract::Shared { layout: layout() };
        signature.contract.parameter_effects.insert(0, vec![MemoryEffect::Reclaim]);
        assert!(declared_shared_summary(&signature).is_none());
    }
    fn layout() -> crate::layout::LayoutId { crate::layout::LayoutId::from_bytes([1; 32]) }




}
