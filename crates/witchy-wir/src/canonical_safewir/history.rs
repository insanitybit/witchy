use std::fmt;

use sha2::{Digest, Sha256};

use crate::layout::LayoutBundle;

use super::{AccessMode, AllocationClass, ArithmeticOperator, BasicBlock, BlockId, BorrowMode,
    CallArgument, CallBorrow, CallableContract, CanonicalModule, ComparisonOperator, Extent,
    Function, FunctionId, FunctionSignature, FunctionSignatureId, HostBufferExtent,
    HostCallArgument, HostMutation, HostParameterRole, HostReentrancy, HostResultRole,
    GuardKind, HeaderField, ImportContract, ImportContractIdentity, InitialValid, LoadTransfer, MemoryEffect, ModuleStage,
    Operation, OptimizedValid, Ownership, ParameterContract, ParameterOwnership,
    Place, Projection, ProperTailSite, ResultContract, Revision, StoreTransfer, Terminator,
    TransformedCandidate, ValueId, ValueType};

use std::collections::BTreeMap;

const OBSERVATION_MODEL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleDigest([u8; 32]);

impl ModuleDigest {
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OptimizationPassId {
    UnreachableBlockPruning,
    ConstantFolding,
}

impl OptimizationPassId {
    pub const ALL: [Self; 2] = [Self::UnreachableBlockPruning, Self::ConstantFolding];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DischargedObligation {
    PrunedBlocks { function: FunctionId, blocks: Vec<u32> },
    FoldedScalarOperation {
        function: FunctionId,
        block: BlockId,
        operation_index: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefinementReceipt {
    pass: OptimizationPassId,
    before_digest: ModuleDigest,
    after_digest: ModuleDigest,
    before_revision: Revision,
    after_revision: Revision,
    before_tail_digest: ModuleDigest,
    after_tail_digest: ModuleDigest,
    observation_model_version: u32,
    discharged_obligations: Vec<DischargedObligation>,
}

impl RefinementReceipt {
    pub const fn pass(&self) -> OptimizationPassId {
        self.pass
    }

    pub const fn before_digest(&self) -> ModuleDigest {
        self.before_digest
    }

    pub const fn after_digest(&self) -> ModuleDigest {
        self.after_digest
    }

    pub const fn before_revision(&self) -> Revision {
        self.before_revision
    }

    pub const fn after_revision(&self) -> Revision {
        self.after_revision
    }

    pub(crate) const fn before_tail_digest(&self) -> ModuleDigest {
        self.before_tail_digest
    }

    pub(crate) const fn after_tail_digest(&self) -> ModuleDigest {
        self.after_tail_digest
    }

    pub fn discharged_obligations(&self) -> &[DischargedObligation] {
        &self.discharged_obligations
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationHistoryBody {
    checked_lowering_revision_digest: ModuleDigest,
    proper_tail_manifest_digest: ModuleDigest,
    ordered: Vec<RefinementReceipt>,
    final_safewir_revision_digest: ModuleDigest,
    optimizer_policy_and_route_digest: ModuleDigest,
}

impl OptimizationHistoryBody {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut writer = CanonicalWriter::new(HISTORY_SERIALIZATION_VERSION, HISTORY_DOMAIN);
        writer.module_digest(self.checked_lowering_revision_digest);
        writer.module_digest(self.proper_tail_manifest_digest);
        writer.len(self.ordered.len());
        for receipt in &self.ordered {
            writer.receipt(receipt);
        }
        writer.module_digest(self.final_safewir_revision_digest);
        writer.module_digest(self.optimizer_policy_and_route_digest);
        writer.finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationHistoryWitness {
    body: OptimizationHistoryBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryError {
    InitialDigestMismatch,
    BrokenChain { index: usize },
    InvalidObservationModel { index: usize },
    InvalidPass { index: usize },
    InvalidPolicyDigest,
    TailSetChanged { index: usize },
    TailManifestDigestMismatch,
    FinalDigestMismatch,
    ZeroPassRevisionMismatch,
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid canonical SafeWIR optimization history: {self:?}")
    }
}

impl std::error::Error for HistoryError {}

impl OptimizationHistoryWitness {
    pub fn ordered(&self) -> &[RefinementReceipt] {
        &self.body.ordered
    }

    pub const fn is_zero_pass(&self) -> bool {
        self.body.ordered.is_empty()
    }

    pub fn body(&self) -> &OptimizationHistoryBody {
        &self.body
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.body.canonical_bytes()
    }

    pub fn digest(&self) -> ModuleDigest {
        digest_bytes(HISTORY_DIGEST_DOMAIN, &self.canonical_bytes())
    }

    pub fn proper_tail_manifest_digest(&self) -> ModuleDigest {
        self.body.proper_tail_manifest_digest
    }

    pub(crate) fn identity_digest(&self) -> ModuleDigest {
        self.digest()
    }

    pub fn validate(
        &self,
        initial: &CanonicalModule<InitialValid>,
        final_module: &CanonicalModule<OptimizedValid>,
    ) -> Result<(), HistoryError> {
        if self.body.optimizer_policy_and_route_digest != policy_and_route_digest() {
            return Err(HistoryError::InvalidPolicyDigest);
        }
        let initial_digest = module_digest(initial);
        if self.body.checked_lowering_revision_digest != initial_digest {
            return Err(HistoryError::InitialDigestMismatch);
        }
        let initial_tail_digest = tail_digest(initial);
        if self.body.proper_tail_manifest_digest != initial_tail_digest
            || tail_digest(final_module) != initial_tail_digest
        {
            return Err(HistoryError::TailManifestDigestMismatch);
        }

        let mut expected_digest = initial_digest;
        let mut expected_revision = initial.revision();
        for (index, receipt) in self.body.ordered.iter().enumerate() {
            if !super::assurance::receipt_matches_registered(receipt) {
                return Err(HistoryError::InvalidPass { index });
            }
            if receipt.before_digest != expected_digest
                || receipt.before_revision != expected_revision
            {
                return Err(HistoryError::BrokenChain { index });
            }
            if receipt.observation_model_version != OBSERVATION_MODEL_VERSION {
                return Err(HistoryError::InvalidObservationModel { index });
            }
            if receipt.before_tail_digest != receipt.after_tail_digest {
                return Err(HistoryError::TailSetChanged { index });
            }
            expected_digest = receipt.after_digest;
            expected_revision = receipt.after_revision;
        }

        let final_digest = module_digest(final_module);
        if self.body.final_safewir_revision_digest != final_digest
            || expected_digest != final_digest
            || expected_revision != final_module.revision()
        {
            return Err(HistoryError::FinalDigestMismatch);
        }
        if self.is_zero_pass() && initial.revision() != final_module.revision() {
            return Err(HistoryError::ZeroPassRevisionMismatch);
        }
        Ok(())
    }

    pub(crate) fn from_receipts(
        initial: &CanonicalModule<InitialValid>,
        final_module: &CanonicalModule<OptimizedValid>,
        receipts: Vec<RefinementReceipt>,
    ) -> Result<Self, HistoryError> {
        let witness = Self {
            body: OptimizationHistoryBody {
                checked_lowering_revision_digest: module_digest(initial),
                proper_tail_manifest_digest: tail_digest(initial),
                ordered: receipts,
                final_safewir_revision_digest: module_digest(final_module),
                optimizer_policy_and_route_digest: policy_and_route_digest(),
            },
        };
        witness.validate(initial, final_module)?;
        Ok(witness)
    }

    pub(crate) fn unreachable_receipt<S: ModuleStage>(
        before: &CanonicalModule<S>,
        after: &CanonicalModule<TransformedCandidate>,
        obligations: Vec<DischargedObligation>,
    ) -> RefinementReceipt {
        RefinementReceipt {
            pass: OptimizationPassId::UnreachableBlockPruning,
            before_digest: module_digest(before),
            after_digest: module_digest(after),
            before_revision: before.revision(),
            after_revision: after.revision(),
            before_tail_digest: tail_digest(before),
            after_tail_digest: tail_digest(after),
            observation_model_version: OBSERVATION_MODEL_VERSION,
            discharged_obligations: obligations,
        }
    }

    pub(crate) fn constant_folding_receipt<S: ModuleStage>(
        before: &CanonicalModule<S>,
        after: &CanonicalModule<TransformedCandidate>,
        obligation: DischargedObligation,
    ) -> RefinementReceipt {
        RefinementReceipt {
            pass: OptimizationPassId::ConstantFolding,
            before_digest: module_digest(before),
            after_digest: module_digest(after),
            before_revision: before.revision(),
            after_revision: after.revision(),
            before_tail_digest: tail_digest(before),
            after_tail_digest: tail_digest(after),
            observation_model_version: OBSERVATION_MODEL_VERSION,
            discharged_obligations: vec![obligation],
        }
    }

    #[cfg(test)]
    pub(crate) fn tamper_first_after_digest(&mut self) {
        self.body.ordered[0].after_digest = ModuleDigest([0xa5; 32]);
    }
}

pub(crate) fn module_digest<S: ModuleStage>(module: &CanonicalModule<S>) -> ModuleDigest {
    let mut writer = CanonicalWriter::new(MODULE_SERIALIZATION_VERSION, MODULE_DOMAIN);
    writer.u64(module.revision().get());
    writer.fixed_bytes(&module.checked_source().digest());
    writer.bytes(&module.layouts().canonical_bytes());
    writer.static_data_segments(module.static_data_segments());
    writer.signatures(module.signatures());
    writer.imports(module.imports());
    writer.functions(module.functions());
    writer.tail_sites(module.proper_tail_sites());
    digest_bytes(MODULE_DIGEST_DOMAIN, &writer.finish())
}

pub(crate) fn tail_digest<S: ModuleStage>(module: &CanonicalModule<S>) -> ModuleDigest {
    let sites = module
        .proper_tail_sites()
        .iter()
        .map(|site| (site.id.digest(), site.function.get()))
        .collect::<Vec<_>>();
    ModuleDigest::from_bytes(
        witchy_types::typeck::canonical_proper_tail_manifest_digest(&sites),
    )
}

fn digest_bytes(domain: &[u8], bytes: &[u8]) -> ModuleDigest {
    let mut hasher = Sha256::new();
    hasher.update(b"witchy/canonical-safewir/digest\0");
    hasher.update(u64::try_from(domain.len()).expect("digest domain length fits u64").to_le_bytes());
    hasher.update(domain);
    hasher.update(u64::try_from(bytes.len()).expect("digest input length fits u64").to_le_bytes());
    hasher.update(bytes);
    ModuleDigest(hasher.finalize().into())
}

fn policy_and_route_digest() -> ModuleDigest {
    digest_bytes(POLICY_DIGEST_DOMAIN, &super::assurance::policy_digest_bytes())
}

const MODULE_SERIALIZATION_VERSION: u32 = 2;
const HISTORY_SERIALIZATION_VERSION: u32 = 1;
const MODULE_DOMAIN: &[u8] = b"witchy/canonical-safewir/module/v2";
const MODULE_DIGEST_DOMAIN: &[u8] = b"witchy/canonical-safewir/module-sha256/v2";
const HISTORY_DOMAIN: &[u8] = b"witchy/canonical-safewir/optimization-history/v1";
const HISTORY_DIGEST_DOMAIN: &[u8] =
    b"witchy/canonical-safewir/optimization-history-sha256/v1";
const POLICY_DIGEST_DOMAIN: &[u8] = b"witchy/canonical-safewir/optimization-policy-sha256/v1";

/// Deterministic, tagged serialization for the fields that define a
/// CanonicalModule's identity. This deliberately does not use `Debug`: a
/// formatting change must never silently change or omit security semantics.
struct CanonicalWriter {
    bytes: Vec<u8>,
}

impl CanonicalWriter {
    fn new(version: u32, domain: &[u8]) -> Self {
        let mut writer = Self { bytes: Vec::new() };
        writer.bytes(b"WITCHY-SAFEWIR\0");
        writer.u32(version);
        writer.bytes(domain);
        writer
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn fixed_bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn module_digest(&mut self, digest: ModuleDigest) {
        self.fixed_bytes(&digest.bytes());
    }

    fn receipt(&mut self, receipt: &RefinementReceipt) {
        self.u8(match receipt.pass {
            OptimizationPassId::UnreachableBlockPruning => 0,
            OptimizationPassId::ConstantFolding => 1,
        });
        self.module_digest(receipt.before_digest);
        self.module_digest(receipt.after_digest);
        self.u64(receipt.before_revision.get());
        self.u64(receipt.after_revision.get());
        self.module_digest(receipt.before_tail_digest);
        self.module_digest(receipt.after_tail_digest);
        self.u32(receipt.observation_model_version);
        self.len(receipt.discharged_obligations.len());
        for obligation in &receipt.discharged_obligations {
            self.obligation(obligation);
        }
    }

    fn obligation(&mut self, obligation: &DischargedObligation) {
        match obligation {
            DischargedObligation::PrunedBlocks { function, blocks } => {
                self.u8(0);
                self.u32(function.get());
                self.len(blocks.len());
                for block in blocks {
                    self.u32(*block);
                }
            }
            DischargedObligation::FoldedScalarOperation {
                function,
                block,
                operation_index,
            } => {
                self.u8(1);
                self.u32(function.get());
                self.u32(block.get());
                self.u64(u64::try_from(*operation_index).expect("operation index fits u64"));
            }
        }
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.u64(u64::try_from(bytes.len()).expect("canonical field length fits u64"));
        self.fixed_bytes(bytes);
    }

    fn len(&mut self, length: usize) {
        self.u64(u64::try_from(length).expect("canonical collection length fits u64"));
    }

    fn value_id(&mut self, value: ValueId) {
        self.u32(value.get());
    }

    fn layout_id(&mut self, layout: crate::layout::LayoutId) {
        self.fixed_bytes(layout.as_bytes());
    }

    fn value_type(&mut self, value_type: ValueType) {
        match value_type {
            ValueType::Int => self.u8(0),
            ValueType::Float => self.u8(1),
            ValueType::Bool => self.u8(2),
            ValueType::Unit => self.u8(3),
            ValueType::Size => self.u8(4),
            ValueType::V128 => self.u8(5),
            ValueType::Ref(layout) => {
                self.u8(6);
                self.layout_id(layout);
            }
            ValueType::Gc(id) => {
                self.u8(7);
                self.u32(id.get());
            }
            ValueType::Capability(kind) => {
                self.u8(8);
                self.u32(kind.get());
            }
            ValueType::Callable(signature) => {
                self.u8(9);
                self.u32(signature.get());
            }
        }
    }

    fn access_mode(&mut self, mode: AccessMode) {
        self.u8(match mode {
            AccessMode::Read => 0,
            AccessMode::Write => 1,
        });
    }

    fn ownership(&mut self, ownership: Ownership) {
        self.u8(match ownership {
            Ownership::Unique => 0,
            Ownership::Shared => 1,
            Ownership::Consumed => 2,
        });
    }

    fn allocation_class(&mut self, class: AllocationClass) {
        self.u8(match class {
            AllocationClass::Bump => 0,
            AllocationClass::ReferenceCounted => 1,
            AllocationClass::Scratch => 2,
        });
    }

    fn extent(&mut self, extent: &Extent) {
        match extent {
            Extent::FixedBytes(bytes) => {
                self.u8(0);
                self.u64(*bytes);
            }
            Extent::HeaderPrefix { minimum_bytes } => {
                self.u8(1);
                self.u32(*minimum_bytes);
            }
            Extent::Counted { header_bytes, count, capacity, stride_bytes } => {
                self.u8(2);
                self.u32(*header_bytes);
                self.value_id(*count);
                self.value_id(*capacity);
                self.u32(*stride_bytes);
            }
        }
    }

    fn projection(&mut self, projection: &Projection) {
        match projection {
            Projection::Header(field) => {
                self.u8(0);
                self.u8(match field {
                    HeaderField::Length => 0,
                    HeaderField::Capacity => 1,
                });
            }
            Projection::Field { index } => {
                self.u8(1);
                self.u32(*index);
            }
            Projection::Element { index } => {
                self.u8(2);
                self.value_id(*index);
            }
            Projection::Slice { start, length } => {
                self.u8(3);
                self.value_id(*start);
                self.value_id(*length);
            }
        }
    }

    fn place(&mut self, place: &Place) {
        self.value_id(place.object());
        self.projection(place.projection());
        self.value_type(place.value_type());
        self.access_mode(place.access());
    }

    fn guard_kind(&mut self, kind: GuardKind) {
        self.u8(match kind {
            GuardKind::Bounds => 0,
            GuardKind::Arithmetic => 1,
            GuardKind::NonNull => 2,
            GuardKind::DynamicType => 3,
        });
    }

    fn load_transfer(&mut self, transfer: LoadTransfer) {
        self.u8(match transfer {
            LoadTransfer::CopyScalar => 0,
            LoadTransfer::CloneShared => 1,
            LoadTransfer::MoveUnique => 2,
        });
    }

    fn store_transfer(&mut self, transfer: StoreTransfer) {
        self.u8(match transfer {
            StoreTransfer::CopyScalar => 0,
            StoreTransfer::CloneShared => 1,
            StoreTransfer::MoveUnique => 2,
        });
    }

    fn borrow_mode(&mut self, mode: BorrowMode) {
        self.u8(match mode {
            BorrowMode::Read => 0,
            BorrowMode::Write => 1,
        });
    }

    fn call_borrow(&mut self, borrow: CallBorrow) {
        self.value_id(borrow.source());
        self.borrow_mode(borrow.mode());
    }

    fn call_argument(&mut self, argument: &CallArgument) {
        match argument {
            CallArgument::Value(value) => {
                self.u8(0);
                self.value_id(*value);
            }
            CallArgument::Move(value) => {
                self.u8(1);
                self.value_id(*value);
            }
            CallArgument::Share(value) => {
                self.u8(2);
                self.value_id(*value);
            }
            CallArgument::Borrow(borrow) => {
                self.u8(3);
                self.call_borrow(*borrow);
            }
        }
    }

    fn arguments(&mut self, arguments: &[CallArgument]) {
        self.len(arguments.len());
        for argument in arguments {
            self.call_argument(argument);
        }
    }

    fn host_argument(&mut self, argument: &HostCallArgument) {
        match argument {
            HostCallArgument::Value(value) => { self.u8(0); self.value_id(*value); }
            HostCallArgument::Capability(value) => { self.u8(1); self.value_id(*value); }
            HostCallArgument::Buffer { place, extent } => {
                self.u8(2);
                self.place(place);
                self.host_buffer_extent(extent);
            }
        }
    }

    fn host_arguments(&mut self, arguments: &[HostCallArgument]) {
        self.len(arguments.len());
        for argument in arguments {
            self.host_argument(argument);
        }
    }

    fn parameter_ownership(&mut self, ownership: ParameterOwnership) {
        match ownership {
            ParameterOwnership::Value => self.u8(0),
            ParameterOwnership::Unique => self.u8(1),
            ParameterOwnership::Shared => self.u8(2),
            ParameterOwnership::CallBorrow { mode } => {
                self.u8(3);
                self.borrow_mode(mode);
            }
        }
    }

    fn parameter_contract(&mut self, parameter: &ParameterContract) {
        self.value_type(parameter.value_type);
        self.parameter_ownership(parameter.ownership);
        self.bool(parameter.non_escaping);
    }

    fn host_buffer_extent(&mut self, extent: &HostBufferExtent) {
        match extent {
            HostBufferExtent::Slice => self.u8(0),
            HostBufferExtent::Exact(bytes) => { self.u8(1); self.u64(*bytes); }
            HostBufferExtent::AtLeast(bytes) => { self.u8(2); self.u64(*bytes); }
            HostBufferExtent::Unknown => self.u8(3),
        }
    }

    fn host_parameter_role(&mut self, role: &HostParameterRole) {
        match role {
            HostParameterRole::Value(value_type) => { self.u8(0); self.value_type(*value_type); }
            HostParameterRole::Capability(kind) => { self.u8(1); self.u32(kind.get()); }
            HostParameterRole::Buffer { value_type, access, extent } => {
                self.u8(2);
                self.value_type(*value_type);
                self.access_mode(*access);
                self.host_buffer_extent(extent);
            }
        }
    }

    fn host_result_role(&mut self, role: &HostResultRole) {
        match role {
            HostResultRole::Value(value_type) => { self.u8(0); self.value_type(*value_type); }
            HostResultRole::Capability(kind) => { self.u8(1); self.u32(kind.get()); }
        }
    }

    fn host_mutation(&mut self, mutation: HostMutation) {
        self.u8(match mutation {
            HostMutation::ReadOnly => 0,
            HostMutation::WriteBuffers => 1,
            HostMutation::Unknown => 2,
        });
    }

    fn host_reentrancy(&mut self, reentrancy: HostReentrancy) {
        self.u8(match reentrancy {
            HostReentrancy::Forbidden => 0,
            HostReentrancy::Allowed => 1,
            HostReentrancy::Unknown => 2,
        });
    }

    fn result_contract(&mut self, result: &ResultContract) {
        match result {
            ResultContract::BodyDerived(value_type) => {
                self.u8(6);
                self.value_type(*value_type);
            }
            ResultContract::Value(value_type) => {
                self.u8(0);
                self.value_type(*value_type);
            }
            ResultContract::Fresh { layout, class } => {
                self.u8(1);
                self.layout_id(*layout);
                self.allocation_class(*class);
            }
            ResultContract::Shared { layout } => {
                self.u8(2);
                self.layout_id(*layout);
            }
            ResultContract::AliasesParameter { parameter, layout } => {
                self.u8(3);
                self.u32(*parameter);
                self.layout_id(*layout);
            }
            ResultContract::ReallocatesParameter { parameter, layout } => {
                self.u8(4);
                self.u32(*parameter);
                self.layout_id(*layout);
            }
            ResultContract::StaticData { layout } => {
                self.u8(5);
                self.layout_id(*layout);
            }
        }
    }

    fn memory_effect(&mut self, effect: MemoryEffect) {
        self.u8(match effect {
            MemoryEffect::Read => 0,
            MemoryEffect::Write => 1,
            MemoryEffect::Retain => 2,
            MemoryEffect::Release => 3,
            MemoryEffect::Reclaim => 4,
        });
    }

    fn contract(&mut self, contract: &CallableContract) {
        self.len(contract.parameters.len());
        for parameter in &contract.parameters {
            self.parameter_contract(parameter);
        }
        self.len(contract.results.len());
        for result in &contract.results {
            self.result_contract(result);
        }
        self.len(contract.parameter_effects.len());
        for (parameter, effects) in &contract.parameter_effects {
            self.u32(*parameter);
            self.len(effects.len());
            for effect in effects {
                self.memory_effect(*effect);
            }
        }
    }

    fn signature(&mut self, signature: &FunctionSignature) {
        self.u32(signature.id.get());
        self.len(signature.parameters.len());
        for parameter in &signature.parameters {
            self.value_type(*parameter);
        }
        self.len(signature.results.len());
        for result in &signature.results {
            self.value_type(*result);
        }
        self.contract(&signature.contract);
    }

    fn signatures(&mut self, signatures: &[FunctionSignature]) {
        self.len(signatures.len());
        for signature in signatures {
            self.signature(signature);
        }
    }

    fn import(&mut self, import: &ImportContract) {
        self.u32(import.id.get());
        self.u32(import.signature.get());
        match &import.abi_identity {
            ImportContractIdentity::AuthenticatedAbi(identity) => {
                self.u8(0);
                self.fixed_bytes(identity);
            }
            ImportContractIdentity::Unknown => self.u8(1),
        }
        self.bytes(import.module.as_bytes());
        self.bytes(import.name.as_bytes());
        self.len(import.parameter_roles.len());
        for role in &import.parameter_roles {
            self.host_parameter_role(role);
        }
        self.len(import.result_roles.len());
        for role in &import.result_roles {
            self.host_result_role(role);
        }
        self.host_mutation(import.mutation);
        self.host_reentrancy(import.reentrancy);
    }

    fn imports(&mut self, imports: &[ImportContract]) {
        self.len(imports.len());
        for import in imports {
            self.import(import);
        }
    }

    fn operation(&mut self, operation: &Operation) {
        match operation {
            Operation::ConstInt { destination, value } => {
                self.u8(0);
                self.value_id(*destination);
                self.i64(*value);
            }
            Operation::ConstFloat { destination, bits } => {
                self.u8(1);
                self.value_id(*destination);
                self.u64(*bits);
            }
            Operation::ConstBool { destination, value } => {
                self.u8(2);
                self.value_id(*destination);
                self.bool(*value);
            }
            Operation::ConstSize { destination, value } => {
                self.u8(3);
                self.value_id(*destination);
                self.u64(*value);
            }
            Operation::Arithmetic { destination, operator, left, right, overflow } => {
                self.u8(4);
                self.value_id(*destination);
                self.arithmetic_operator(*operator);
                self.value_id(*left);
                self.value_id(*right);
                self.overflow(*overflow);
            }
            Operation::Compare { destination, operator, left, right } => {
                self.u8(5);
                self.value_id(*destination);
                self.comparison_operator(*operator);
                self.value_id(*left);
                self.value_id(*right);
            }
            Operation::NarrowSize { destination, value } => {
                self.u8(6);
                self.value_id(*destination);
                self.value_id(*value);
            }
            Operation::Guard { condition, kind } => {
                self.u8(7);
                self.value_id(*condition);
                self.guard_kind(*kind);
            }
            Operation::Alloc { destination, layout, extent, class } => {
                self.u8(8);
                self.value_id(*destination);
                self.layout_id(*layout);
                self.extent(extent);
                self.allocation_class(*class);
            }
            Operation::StaticData { destination, layout, extent, data } => {
                self.u8(9);
                self.value_id(*destination);
                self.layout_id(*layout);
                self.extent(extent);
                self.u32(data.get());
            }
            Operation::Load { destination, place, transfer } => {
                self.u8(10);
                self.value_id(*destination);
                self.place(place);
                self.load_transfer(*transfer);
            }
            Operation::Store { place, value, transfer } => {
                self.u8(11);
                self.place(place);
                self.value_id(*value);
                self.store_transfer(*transfer);
            }
            Operation::CopyBytes { destination, source, length } => {
                self.u8(12);
                self.place(destination);
                self.place(source);
                self.value_id(*length);
            }
            Operation::MoveRange { destination, source, count } => {
                self.u8(13);
                self.place(destination);
                self.place(source);
                self.value_id(*count);
            }
            Operation::CloneRange { destination, source, count } => {
                self.u8(14);
                self.place(destination);
                self.place(source);
                self.value_id(*count);
            }
            Operation::UpdateCount { object, count } => {
                self.u8(15);
                self.value_id(*object);
                self.value_id(*count);
            }
            Operation::MoveValue { destination, source } => {
                self.u8(16);
                self.value_id(*destination);
                self.value_id(*source);
            }
            Operation::Share { destination, source } => {
                self.u8(17);
                self.value_id(*destination);
                self.value_id(*source);
            }
            Operation::Retain { object } => {
                self.u8(18);
                self.value_id(*object);
            }
            Operation::Release { object } => {
                self.u8(19);
                self.value_id(*object);
            }
            Operation::Reclaim { object } => {
                self.u8(20);
                self.value_id(*object);
            }
            Operation::FunctionValue { destination, function } => {
                self.u8(24);
                self.value_id(*destination);
                self.u32(function.get());
            }
            Operation::Call { destinations, callee, arguments } => {
                self.u8(21);
                self.value_ids(destinations);
                self.u32(callee.get());
                self.arguments(arguments);
            }
            Operation::VarCall { destinations, callee, signature, arguments } => {
                self.u8(22);
                self.value_ids(destinations);
                self.value_id(*callee);
                self.u32(signature.get());
                self.arguments(arguments);
            }
            Operation::HostCall { destinations, import, arguments } => {
                self.u8(23);
                self.value_ids(destinations);
                self.u32(import.get());
                self.host_arguments(arguments);
            }
        }
    }

    fn value_ids(&mut self, values: &[ValueId]) {
        self.len(values.len());
        for value in values {
            self.value_id(*value);
        }
    }

    fn arithmetic_operator(&mut self, operator: ArithmeticOperator) {
        self.u8(operator as u8);
    }

    fn comparison_operator(&mut self, operator: ComparisonOperator) {
        self.u8(operator as u8);
    }

    fn overflow(&mut self, overflow: super::OverflowBehavior) {
        self.u8(match overflow {
            super::OverflowBehavior::Wrap => 0,
            super::OverflowBehavior::Trap => 1,
        });
    }

    fn terminator(&mut self, terminator: &Terminator) {
        match terminator {
            Terminator::Branch { target, arguments } => {
                self.u8(0);
                self.u32(target.get());
                self.value_ids(arguments);
            }
            Terminator::BranchIf {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => {
                self.u8(1);
                self.value_id(*condition);
                self.u32(then_target.get());
                self.value_ids(then_arguments);
                self.u32(else_target.get());
                self.value_ids(else_arguments);
            }
            Terminator::Return { values } => {
                self.u8(2);
                self.value_ids(values);
            }
            Terminator::TailCall { site, callee, arguments } => {
                self.u8(3);
                self.fixed_bytes(&site.digest());
                self.u32(callee.get());
                self.arguments(arguments);
            }
            Terminator::VarTailCall { site, callee, signature, arguments } => {
                self.u8(4);
                self.fixed_bytes(&site.digest());
                self.value_id(*callee);
                self.u32(signature.get());
                self.arguments(arguments);
            }
            Terminator::Trap => self.u8(5),
            Terminator::Unreachable => self.u8(6),
        }
    }

    fn block_parameter(&mut self, parameter: &super::BlockParameter) {
        self.value_id(parameter.value);
        self.value_type(parameter.value_type);
        match parameter.ownership {
            None => self.u8(0),
            Some(ownership) => {
                self.u8(1);
                self.ownership(ownership);
            }
        }
    }

    fn block(&mut self, block: &BasicBlock) {
        self.u32(block.id.get());
        self.len(block.parameters.len());
        for parameter in &block.parameters {
            self.block_parameter(parameter);
        }
        self.len(block.operations.len());
        for operation in &block.operations {
            self.operation(operation);
        }
        self.terminator(&block.terminator);
    }

    fn function(&mut self, function: &Function) {
        self.u32(function.id.get());
        self.u32(function.signature.get());
        self.u32(function.entry.get());
        self.len(function.value_types.len());
        for (value, value_type) in &function.value_types {
            self.value_id(*value);
            self.value_type(*value_type);
        }
        self.len(function.blocks.len());
        for block in &function.blocks {
            self.block(block);
        }
    }

    fn functions(&mut self, functions: &[Function]) {
        self.len(functions.len());
        for function in functions {
            self.function(function);
        }
    }

    fn tail_site(&mut self, site: &ProperTailSite) {
        self.fixed_bytes(&site.id.digest());
        self.u32(site.function.get());
    }

    fn tail_sites(&mut self, sites: &[ProperTailSite]) {
        self.len(sites.len());
        for site in sites {
            self.tail_site(site);
        }
    }

    fn static_data_segments(&mut self, segments: &[super::StaticDataSegment]) {
        let mut ordered = segments.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|segment| segment.id());
        self.len(ordered.len());
        for segment in ordered {
            self.u32(segment.id().get());
            self.bytes(segment.bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_layouts() -> LayoutBundle {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WLAB");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        LayoutBundle::decode_canonical(&bytes).unwrap().0
    }

    fn candidate(constant: i64, reverse_effect_insertion: bool) -> CanonicalModule<super::super::LoweredCandidate> {
        let mut effects = BTreeMap::new();
        let entries = [(1, vec![MemoryEffect::Write]), (0, vec![MemoryEffect::Read])];
        if reverse_effect_insertion {
            for (parameter, values) in entries.into_iter().rev() {
                effects.insert(parameter, values);
            }
        } else {
            for (parameter, values) in entries {
                effects.insert(parameter, values);
            }
        }

        let signature = FunctionSignature {
            id: FunctionSignatureId::new(0),
            parameters: Vec::new(),
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: Vec::new(),
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: effects,
            },
        };
        let mut value_types = BTreeMap::new();
        value_types.insert(ValueId::new(0), ValueType::Int);
        let function = Function {
            id: FunctionId::new(0),
            signature: FunctionSignatureId::new(0),
            entry: super::super::BlockId::new(0),
            value_types,
            blocks: vec![BasicBlock {
                id: super::super::BlockId::new(0),
                parameters: Vec::new(),
                operations: vec![Operation::ConstInt { destination: ValueId::new(0), value: constant }],
                terminator: Terminator::Return { values: vec![ValueId::new(0)] },
            }],
        };
        CanonicalModule::from_lowering(super::super::LoweredModuleParts {
            revision: Revision::new(1),
            checked_source: super::super::CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![function],
            proper_tail_sites: Vec::new(),
        })
    }

    #[test]
    fn semantic_field_mutation_changes_sha256_identity() {
        assert_ne!(module_digest(&candidate(1, false)), module_digest(&candidate(2, false)));
    }

    #[test]
    fn function_value_changes_sha256_identity() {
        let first = candidate(1, false);
        let mut changed = candidate(1, false);
        changed.functions[0].blocks[0].operations = vec![Operation::FunctionValue {
            destination: ValueId::new(0),
            function: FunctionId::new(0),
        }];
        assert_ne!(module_digest(&first), module_digest(&changed));
    }

    #[test]
    fn map_construction_order_does_not_change_identity() {
        assert_eq!(
            module_digest(&candidate(1, false)),
            module_digest(&candidate(1, true))
        );
    }

    fn with_static_data(
        module: CanonicalModule<super::super::LoweredCandidate>,
        segments: Vec<super::super::StaticDataSegment>,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        CanonicalModule::from_lowering_with_static_data(super::super::LoweredModuleParts {
            revision: module.revision(),
            checked_source: module.checked_source(),
            layouts: module.layouts().clone(),
            signatures: module.signatures().to_vec(),
            imports: module.imports().to_vec(),
            functions: module.functions().to_vec(),
            proper_tail_sites: module.proper_tail_sites().to_vec(),
        }, segments)
    }

    #[test]
    fn module_digest_binds_static_segment_bytes_and_ids() {
        let first = with_static_data(
            candidate(1, false),
            vec![super::super::StaticDataSegment::new(super::super::StaticDataId::new(4), vec![1, 2, 3])],
        );
        let changed = with_static_data(
            candidate(1, false),
            vec![super::super::StaticDataSegment::new(super::super::StaticDataId::new(4), vec![1, 2, 4])],
        );
        let changed_id = with_static_data(
            candidate(1, false),
            vec![super::super::StaticDataSegment::new(super::super::StaticDataId::new(5), vec![1, 2, 3])],
        );
        assert_ne!(module_digest(&first), module_digest(&changed));
        assert_ne!(module_digest(&first), module_digest(&changed_id));
    }

    #[test]
    fn duplicate_and_unreferenced_static_segments_are_rejected() {
        let duplicate = with_static_data(
            candidate(1, false),
            vec![
                super::super::StaticDataSegment::new(super::super::StaticDataId::new(0), vec![1]),
                super::super::StaticDataSegment::new(super::super::StaticDataId::new(0), vec![1]),
            ],
        );
        assert!(matches!(super::super::check_initial(duplicate), Err(super::super::InitialCheckError::DuplicateStaticDataId(_))));

        let unreferenced = with_static_data(
            candidate(1, false),
            vec![super::super::StaticDataSegment::new(super::super::StaticDataId::new(0), vec![1])],
        );
        assert!(matches!(super::super::check_initial(unreferenced), Err(super::super::InitialCheckError::UnexpectedStaticDataId(_))));
    }

    #[test]
    fn history_identity_binds_the_initial_checked_module() {
        let first = OptimizationHistoryWitness {
            body: OptimizationHistoryBody {
                checked_lowering_revision_digest: ModuleDigest([1; 32]),
                proper_tail_manifest_digest: ModuleDigest([4; 32]),
                ordered: Vec::new(),
                final_safewir_revision_digest: ModuleDigest([2; 32]),
                optimizer_policy_and_route_digest: policy_and_route_digest(),
            },
        };
        let mut second = first.clone();
        second.body.checked_lowering_revision_digest = ModuleDigest([3; 32]);
        assert_ne!(first.canonical_bytes(), second.canonical_bytes());
        assert_ne!(first.identity_digest(), second.identity_digest());
    }
}
