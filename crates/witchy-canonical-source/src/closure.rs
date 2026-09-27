//! Closure capture vocabulary: how a checked binding reaches a closure and how
//! its value crosses into the closure environment, and the per-lambda
//! witnesses whose digest binds them into the checked-source witness. The
//! checker derives the witnesses (`witchy_types::canonical_closure`).

use std::collections::BTreeMap;

use witchy_syntax::ast::{Convention, Type, TypeDef, TypeQual};

use crate::proper_tail::{ProperTailCallableId, ProperTailPathStep};
use crate::storage::StoragePlan;
use crate::source::{SourceExecutionMode, TaskCallableScope, TaskExecutionProof};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClosureBindingPathStep {
    Parameter(u32),
    Statement(u32),
    Pattern(u32),
    LoopVariable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClosureCaptureTransfer {
    CopyScalar,
    CopyCapability,
    ManagedReferenceByValue,
    /// An already-authenticated immutable root-plus-projection carrier.
    ///
    /// This is deliberately not the generic managed-reference case: lowering
    /// may retain the carrier, but must never recover a root or projection
    /// from its physical layout and synthesize a new view.
    RootedViewByValue,
}

impl ClosureCaptureTransfer {
    /// The source-authenticated capture lane. The checker mints this in the
    /// closure witness; the SafeWIR owner path derives it again before mapping
    /// it to a physical transfer. Unproved and must-consume storage has no
    /// copy fallback.
    pub fn classify(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> Option<Self> {
        use crate::capabilities::CapabilityRepresentation;
        // Frozen changes the source contract, not its physical storage. Keep
        // the original qualified type for the recursive ownership check.
        let storage_type = match ty {
            Type::Qualified(TypeQual::Frozen, inner) => inner.as_ref(),
            _ => ty,
        };
        crate::storage::replay_capture_eligibility(ty, definitions).ok()?;
        if let Some(representation) = crate::capabilities::representation(ty, definitions) {
            return matches!(representation, CapabilityRepresentation::Direct(_))
                .then_some(Self::CopyCapability);
        }
        let plan = StoragePlan::classify(storage_type, definitions, |_| Some(0));
        match plan {
            StoragePlan::RootedView(_) if matches!(ty, Type::Qualified(TypeQual::View, _)) => {
                Some(Self::RootedViewByValue)
            }
            // An outer frozen qualifier keeps the view's physical managed
            // representation, but does not satisfy the direct-view authority
            // checks for the distinct rooted-view transfer tag.
            StoragePlan::RootedView(_) => Some(Self::ManagedReferenceByValue),
            StoragePlan::InlineScalar(_) => Some(Self::CopyScalar),
            StoragePlan::InlineAggregate(_) => Some(Self::ManagedReferenceByValue),
            StoragePlan::Owned(_)
            | StoragePlan::RetainedCallable(_)
            | StoragePlan::RetainedOwnerList(_)
            | StoragePlan::RetainedGraphList(_)
            | StoragePlan::RetainedCallableList(_)
            | StoragePlan::RetainedOpaqueAggregateList(_) => Some(Self::ManagedReferenceByValue),
            StoragePlan::AggregateView(_, graph) if graph.cloneable() => Some(Self::ManagedReferenceByValue),
            StoragePlan::AuthenticatedPlace(plan) if plan.cloneable() => Some(Self::ManagedReferenceByValue),
            StoragePlan::MoveOnlyList(_) => None,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClosureBindingId {
    owner: ProperTailCallableId,
    path: Vec<ClosureBindingPathStep>,
    name: String,
}

impl ClosureBindingId {
    pub fn new(owner: ProperTailCallableId, path: Vec<ClosureBindingPathStep>, name: String) -> Self {
        Self { owner, path, name }
    }

    pub fn owner(&self) -> &ProperTailCallableId { &self.owner }
    pub fn path(&self) -> &[ClosureBindingPathStep] { &self.path }
    pub fn name(&self) -> &str { &self.name }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureCaptureWitness {
    binding: ClosureBindingId,
    checked_type: Type,
    checked_type_encoding: Vec<u8>,
    transfer: ClosureCaptureTransfer,
}

impl ClosureCaptureWitness {
    pub fn new(
        binding: ClosureBindingId,
        checked_type: Type,
        checked_type_encoding: Vec<u8>,
        transfer: ClosureCaptureTransfer,
    ) -> Self {
        Self { binding, checked_type, checked_type_encoding, transfer }
    }

    pub fn binding(&self) -> &ClosureBindingId { &self.binding }
    pub fn checked_type(&self) -> &Type { &self.checked_type }
    pub fn checked_type_encoding(&self) -> &[u8] { &self.checked_type_encoding }
    pub fn transfer(&self) -> ClosureCaptureTransfer { self.transfer }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureWitness {
    id: ProperTailCallableId,
    /// The runtime type-table view used for physical closure-family layout,
    /// in source form; `None` when the checked type has no source spelling.
    signature: Option<Type>,
    /// The checked source declaration used to authenticate the callable ABI.
    /// `Ty` intentionally omits qualifier-only state, so a compiler-generated
    /// eta lambda must retain its declared parameter contract separately.
    declared_signature: Type,
    signature_encoding: Vec<u8>,
    captures: Vec<ClosureCaptureWitness>,
    execution_contract: Option<ClosureExecutionContract>,
}

/// Symbolic source behavior at an exact final checked lambda. Capture types
/// and transfers remain in the containing witness's ordered capture records.
/// This describes a source promise; a candidate body must prove correspondence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureExecutionContract {
    id: ProperTailCallableId,
    source_mode: SourceExecutionMode,
    captures: Vec<ClosureBindingId>,
    conventions: Vec<Convention>,
    formal_scope: TaskCallableScope,
    result: TaskExecutionProof,
    required_opt_inputs: Vec<TaskExecutionProof>,
}

impl ClosureExecutionContract {
    #[doc(hidden)]
    pub fn from_checked_frontend(id: ProperTailCallableId, source_mode: SourceExecutionMode,
        captures: Vec<ClosureBindingId>, conventions: Vec<Convention>, formal_scope: TaskCallableScope,
        result: TaskExecutionProof, required_opt_inputs: Vec<TaskExecutionProof>) -> Self {
        Self { id, source_mode, captures, conventions, formal_scope, result, required_opt_inputs }
    }
    pub fn id(&self) -> &ProperTailCallableId { &self.id }
    pub fn source_mode(&self) -> SourceExecutionMode { self.source_mode }
    pub fn captures(&self) -> &[ClosureBindingId] { &self.captures }
    pub fn conventions(&self) -> &[Convention] { &self.conventions }
    pub fn formal_scope(&self) -> &TaskCallableScope { &self.formal_scope }
    pub fn result(&self) -> &TaskExecutionProof { &self.result }
    pub fn required_opt_inputs(&self) -> &[TaskExecutionProof] { &self.required_opt_inputs }
}

impl ClosureWitness {
    pub fn new(
        id: ProperTailCallableId,
        signature: Option<Type>,
        declared_signature: Type,
        signature_encoding: Vec<u8>,
        captures: Vec<ClosureCaptureWitness>,
    ) -> Self {
        Self { id, signature, declared_signature, signature_encoding, captures, execution_contract: None }
    }

    pub fn id(&self) -> &ProperTailCallableId { &self.id }
    pub fn signature(&self) -> Option<&Type> { self.signature.as_ref() }
    pub fn declared_signature(&self) -> &Type { &self.declared_signature }
    pub fn captures(&self) -> &[ClosureCaptureWitness] { &self.captures }
    pub fn execution_contract(&self) -> Option<&ClosureExecutionContract> { self.execution_contract.as_ref() }

    #[doc(hidden)]
    pub fn bind_checked_execution_contract(&mut self, contract: ClosureExecutionContract) -> bool {
        if contract.id != self.id || contract.captures.len() != self.captures.len()
            || !contract.captures.iter().zip(&self.captures).all(|(id, capture)| id == capture.binding()) {
            return false;
        }
        self.execution_contract = Some(contract);
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClosureWitnessManifest {
    entries: Vec<ClosureWitness>,
    source_bundle_digest: [u8; 32],
}

impl ClosureWitnessManifest {
    /// Witness entries, sorted and deduplicated by the checker, not yet bound
    /// to a source bundle.
    pub fn new(entries: Vec<ClosureWitness>) -> Self {
        Self { entries, source_bundle_digest: [0; 32] }
    }

    pub fn entries(&self) -> &[ClosureWitness] { &self.entries }

    #[doc(hidden)]
    pub fn bind_checked_execution_contracts(&mut self, contracts: BTreeMap<ProperTailCallableId, ClosureExecutionContract>) -> bool {
        if contracts.keys().any(|id| !self.entries.iter().any(|entry| entry.id() == id)) { return false; }
        for entry in &mut self.entries {
            if let Some(contract) = contracts.get(entry.id())
                && !entry.bind_checked_execution_contract(contract.clone()) { return false; }
        }
        true
    }

    pub fn digest(&self) -> [u8; 32] {
        let mut bytes = b"witchy.canonical-closure-witness.v2".to_vec();
        bytes.extend_from_slice(&self.source_bundle_digest);
        put_u32(&mut bytes, self.entries.len());
        for entry in &self.entries {
            encode_callable(&entry.id, &mut bytes);
            put_bytes(&mut bytes, &entry.signature_encoding);
            put_u32(&mut bytes, entry.captures.len());
            for capture in &entry.captures {
                encode_binding(&capture.binding, &mut bytes);
                put_bytes(&mut bytes, &capture.checked_type_encoding);
                bytes.push(capture.transfer as u8);
            }
            bytes.push(u8::from(entry.execution_contract.is_some()));
            if let Some(contract) = &entry.execution_contract {
                encode_callable(&contract.id, &mut bytes);
                bytes.push(u8::from(contract.source_mode.is_opt()));
                put_u32(&mut bytes, contract.captures.len());
                for capture in &contract.captures { encode_binding(capture, &mut bytes); }
                put_u32(&mut bytes, contract.conventions.len());
                for convention in &contract.conventions { bytes.push(*convention as u8); }
                contract.formal_scope.encode(&mut bytes);
                contract.result.encode(&mut bytes);
                put_u32(&mut bytes, contract.required_opt_inputs.len());
                for requirement in &contract.required_opt_inputs { requirement.encode(&mut bytes); }
            }
        }
        *blake3::hash(&bytes).as_bytes()
    }

    pub fn bind_source_bundle_digest(mut self, digest: [u8; 32]) -> Self {
        self.source_bundle_digest = digest;
        self
    }
}

/// Private representation checks; no checked authority escapes this helper.
#[doc(hidden)]
pub mod self_checks {
    use super::*;

    pub fn execution_digest_and_binding_are_exact() {
        let id = ProperTailCallableId { item: 3, method: None, lambda_path: Vec::new() };
        let other = ProperTailCallableId { item: 3, method: Some(0), lambda_path: Vec::new() };
        let scope = TaskCallableScope::checked_closure(id.clone());
        let signature = Type::Fn(Vec::new(), Box::new(Type::Named("Int".into(), Vec::new())), Vec::new(), Default::default());
        let witness = ClosureWitness::new(id.clone(), Some(signature.clone()), signature, vec![1], Vec::new());
        let empty = ClosureWitnessManifest::new(vec![witness.clone()]).digest();
        let mut digests = Vec::new();
        for (mode, result) in [
            (SourceExecutionMode::Normal, TaskExecutionProof::Unknown),
            (SourceExecutionMode::Opt, TaskExecutionProof::Unknown),
            (SourceExecutionMode::Opt, TaskExecutionProof::Task(SourceExecutionMode::Opt, Box::new(TaskExecutionProof::Unknown))),
        ] {
            let mut witness = witness.clone();
            let contract = ClosureExecutionContract { id: id.clone(), source_mode: mode,
                captures: Vec::new(), conventions: Vec::new(), formal_scope: scope.clone(),
                result, required_opt_inputs: Vec::new() };
            assert!(witness.bind_checked_execution_contract(contract));
            let manifest = ClosureWitnessManifest::new(vec![witness]);
            assert_eq!(manifest.digest(), manifest.clone().digest());
            let digest = manifest.digest();
            assert_ne!(digest, empty, "absent behavior cannot share the populated contract key");
            assert!(!digests.contains(&digest), "execution mode/result changes must invalidate the witness key");
            digests.push(digest);
        }
        let mut wrong = witness;
        assert!(!wrong.bind_checked_execution_contract(ClosureExecutionContract {
            id: other, source_mode: SourceExecutionMode::Opt, captures: Vec::new(),
            conventions: Vec::new(), formal_scope: scope, result: TaskExecutionProof::Unknown,
            required_opt_inputs: Vec::new(),
        }));
    }
}

fn put_u32(bytes: &mut Vec<u8>, value: usize) {
    bytes.extend_from_slice(&u32::try_from(value).expect("validated closure manifest size").to_le_bytes());
}

#[doc(hidden)]
pub fn put_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    put_u32(bytes, value.len());
    bytes.extend_from_slice(value);
}

fn put_str(bytes: &mut Vec<u8>, value: &str) {
    put_bytes(bytes, value.as_bytes());
}

pub(crate) fn encode_callable(callable: &ProperTailCallableId, bytes: &mut Vec<u8>) {
    put_u32(bytes, callable.item as usize);
    match callable.method { Some(method) => { bytes.push(1); put_u32(bytes, method as usize); }, None => bytes.push(0) }
    put_path(&callable.lambda_path, bytes);
}

fn encode_binding(binding: &ClosureBindingId, bytes: &mut Vec<u8>) {
    encode_callable(&binding.owner, bytes);
    put_u32(bytes, binding.path.len());
    for step in &binding.path {
        match step {
            ClosureBindingPathStep::Parameter(index) => { bytes.push(0); put_u32(bytes, *index as usize); }
            ClosureBindingPathStep::Statement(index) => { bytes.push(1); put_u32(bytes, *index as usize); }
            ClosureBindingPathStep::Pattern(index) => { bytes.push(2); put_u32(bytes, *index as usize); }
            ClosureBindingPathStep::LoopVariable => bytes.push(3),
        }
    }
    put_str(bytes, &binding.name);
}

fn put_path(path: &[ProperTailPathStep], bytes: &mut Vec<u8>) {
    put_u32(bytes, path.len());
    for step in path {
        match step {
            ProperTailPathStep::Statement(v) => { bytes.push(0); put_u32(bytes, *v as usize); }
            ProperTailPathStep::Value => bytes.push(1),
            ProperTailPathStep::ReturnValue => bytes.push(2),
            ProperTailPathStep::YieldValue => bytes.push(3),
            ProperTailPathStep::Node(v) => { bytes.push(4); bytes.push(*v as u8); }
            ProperTailPathStep::Child(v) => { bytes.push(5); put_u32(bytes, *v as usize); }
            ProperTailPathStep::Arm(v) => { bytes.push(6); put_u32(bytes, *v as usize); }
            ProperTailPathStep::Guard => bytes.push(7),
        }
    }
}
