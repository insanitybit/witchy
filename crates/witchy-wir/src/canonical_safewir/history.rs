use sha2::{Digest, Sha256};

use super::{AccessMode, AllocationClass, ArithmeticOperator, BasicBlock,
    CallArgument, CallableContract, CanonicalModule, ComparisonOperator, Extent,
    Function, FunctionSignature, HostBufferExtent,
    HostCallArgument, HostMutation, HostParameterRole, HostReentrancy, HostResultRole,
    GuardKind, HeaderField, ImportContract, ImportContractIdentity, LoadTransfer, MemoryEffect, ModuleStage,
    Operation, Ownership, ParameterContract, ParameterOwnership,
    Place, Projection, ProperTailSite, ResultContract, StoreTransfer, Terminator,
    ValueId, ValueType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ModuleDigest([u8; 32]);

impl ModuleDigest {
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

pub(crate) fn module_digest<S: ModuleStage>(module: &CanonicalModule<S>) -> ModuleDigest {
    let mut writer = CanonicalWriter::new(MODULE_SERIALIZATION_VERSION, MODULE_DOMAIN);
    writer.u64(module.revision().get());
    writer.fixed_bytes(&module.checked_source().digest());
    writer.bytes(module.launch_contract_payload());
    writer.bytes(&module.layouts().canonical_bytes());
    writer.static_data_segments(module.static_data_segments());
    writer.signatures(module.signatures());
    writer.imports(module.imports());
    writer.functions(module.functions());
    writer.source_abi_bindings(module.source_abi_bindings());
    writer.closure_bindings(module.closure_bindings());
    writer.adapter_bindings(module.adapter_bindings());
    writer.closure_layouts(module.closure_layouts());
    writer.owner_families(module.owner_families());
    writer.u64(module.vm_services().len() as u64);
    for service in module.vm_services() {
        writer.u32(service.id());
        writer.u32(service.caller().get());
        writer.u32(service.callback().get());
    }
    writer.tail_sites(module.proper_tail_sites());
    writer.tail_locations(module.proper_tail_locations());
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

const MODULE_SERIALIZATION_VERSION: u32 = 6;
const MODULE_DOMAIN: &[u8] = b"witchy/canonical-safewir/module/v6";
const MODULE_DIGEST_DOMAIN: &[u8] = b"witchy/canonical-safewir/module-sha256/v6";

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
            ValueType::NullableCapability(kind) => {
                self.u8(11);
                self.u32(kind.get());
            }
            ValueType::Callable(signature) => {
                self.u8(9);
                self.u32(signature.get());
            }
            ValueType::InlineAggregate(layout) => {
                self.u8(10);
                self.layout_id(layout);
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
            LoadTransfer::ClonePresent => 3,
            LoadTransfer::TakeLast => 4,
        });
    }

    fn store_transfer(&mut self, transfer: StoreTransfer) {
        self.u8(match transfer {
            StoreTransfer::CopyScalar => 0,
            StoreTransfer::CloneShared => 1,
            StoreTransfer::MoveUnique => 2,
        });
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
            ParameterOwnership::WriteBack => self.u8(3),
            ParameterOwnership::UniqueWriteBack => self.u8(4),
        }
    }

    fn parameter_contract(&mut self, parameter: &ParameterContract) {
        self.value_type(parameter.value_type);
        self.parameter_ownership(parameter.ownership);
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
            HostResultRole::NullableCapability(kind) => { self.u8(2); self.u32(kind.get()); }
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
            ResultContract::Unique { layout } => { self.u8(8); self.layout_id(*layout); }
            ResultContract::BodyDerived(value_type) => {
                self.u8(6);
                self.value_type(*value_type);
            }
            ResultContract::Value(value_type) => {
                self.u8(0);
                self.value_type(*value_type);
            }
            ResultContract::CheckedValue { layout } => {
                self.u8(7);
                self.layout_id(*layout);
            }
            ResultContract::Fresh { layout, class, extent } => {
                self.u8(1);
                self.layout_id(*layout);
                self.allocation_class(*class);
                match extent {
                    super::ReturnedExtent::FixedBytes(bytes) => {
                        self.u8(0);
                        self.u64(*bytes);
                    }
                    super::ReturnedExtent::Counted { header_bytes, stride_bytes } => {
                        self.u8(1);
                        self.u32(*header_bytes);
                        self.u32(*stride_bytes);
                    }
                }
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
            Operation::ClosureOwner(operation) => {
                self.u8(40);
                self.owner_operation(operation);
            }
            Operation::ConstUnit { destination } => {
                self.u8(41);
                self.value_id(*destination);
            }
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
            Operation::CapabilityNone { destination, kind } => { self.u8(30); self.value_id(*destination); self.u32(kind.get()); }
            Operation::CapabilitySome { destination, source } => { self.u8(31); self.value_id(*destination); self.value_id(*source); }
            Operation::CapabilityIsPresent { destination, source } => { self.u8(32); self.value_id(*destination); self.value_id(*source); }
            Operation::CapabilityUnwrap { destination, source } => { self.u8(33); self.value_id(*destination); self.value_id(*source); }
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
            Operation::WidenSize { destination, value } => {
                self.u8(25);
                self.value_id(*destination);
                self.value_id(*value);
            }
            Operation::FloatUnary { destination, operator, value } => {
                self.u8(28);
                self.value_id(*destination);
                self.u8(match operator {
                    crate::canonical_safewir::FloatUnaryOperator::Sqrt => 0,
                    crate::canonical_safewir::FloatUnaryOperator::TruncateToInt => 1,
                    crate::canonical_safewir::FloatUnaryOperator::FromInt => 2,
                });
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
            Operation::InitNullableOwned { place } => { self.u8(29); self.place(place); }
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
            Operation::MakeInlineAggregate { destination, layout, fields } => {
                self.u8(26);
                self.value_id(*destination);
                self.layout_id(*layout);
                self.value_ids(fields);
            }
            Operation::ExtractInlineField { destination, aggregate, index } => {
                self.u8(27);
                self.value_id(*destination);
                self.value_id(*aggregate);
                self.u32(*index);
            }
            Operation::CloneRef { destination, source } => {
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
            Operation::DropRef { object } => {
                self.u8(20);
                self.value_id(*object);
            }
            Operation::Reclaim { object } => {
                self.u8(21);
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

    fn owner_operation(&mut self, operation: &super::closure_owner_contract::OwnerOperation) {
        use super::closure_owner_contract::OwnerOperation as O;
        let (tag, constructor, capture, family) = match operation {
            O::Construct { constructor, .. } => (0, Some(*constructor), None, None),
            O::ReadCapability { constructor, capture, .. } => (1, Some(*constructor), Some(*capture), None),
            // Preserve established tags: ReadOwner was added after the original
            // twelve operations and receives the next unused value.
            O::ReadOwner { constructor, capture, .. } => (12, Some(*constructor), Some(*capture), None),
            O::ReadTag { family, .. } => (13, None, None, Some(*family)),
            O::ReadCallable { family, .. } => (2, None, None, Some(*family)),
            O::Clone { family, .. } => (3, None, None, Some(*family)),
            O::Drop { family, .. } => (4, None, None, Some(*family)),
            O::CloneEnvironment { constructor, .. } => (5, Some(*constructor), None, None),
            O::NewList { family, .. } => (6, None, None, Some(*family)),
            O::ListGet { family, .. } => (7, None, None, Some(*family)),
            O::ListLength { family, .. } => (8, None, None, Some(*family)),
            O::CloneList { family, .. } => (9, None, None, Some(*family)),
            O::ListPush { family, .. } => (10, None, None, Some(*family)),
            O::DropList { family, .. } => (11, None, None, Some(*family)),
            O::ListSet { family, .. } => (14, None, None, Some(*family)),
        };
        self.u8(tag);
        self.value_ids(&operation.destinations());
        self.value_ids(&operation.operands());
        if let Some(constructor) = constructor { self.u32(constructor.0); }
        if let Some(capture) = capture { self.u32(capture.0); }
        if let Some(family) = family { self.u32(family.get()); }
    }

    fn owner_families(&mut self, families: &[super::closure_owner_contract::OwnerFamily]) {
        use super::closure_owner_contract::{CaptureStorage, CaptureTransfer, OwnerFamilySource};
        self.len(families.len());
        for family in families {
            match &family.source {
                OwnerFamilySource::Closure => self.u8(0),
                OwnerFamilySource::Aggregate(source) => {
                    self.u8(1);
                    self.source_type(source);
                }
            }
            self.u32(family.owner_type.get());
            self.u32(family.list_type.get());
            self.u32(family.signature.get());
            self.u32(family.declaration_signature.get());
            self.layout_id(family.environment);
            self.len(family.constructors.len());
            for constructor in &family.constructors {
                self.u32(constructor.id.0);
                self.u32(constructor.function.get());
                self.bool(constructor.witness.is_some());
                if let Some(witness) = constructor.witness { self.u32(witness); }
                self.u64(constructor.environment_tag as u64);
                self.len(constructor.captures.len());
                for capture in &constructor.captures {
                    self.value_type(capture.value_type);
                    match capture.storage {
                        CaptureStorage::EnvironmentField(field) => { self.u8(0); self.u32(field); }
                        CaptureStorage::OwnerField(field) => { self.u8(1); self.u32(field); }
                    }
                    match capture.transfer {
                        CaptureTransfer::CopyScalar => self.u8(0),
                        CaptureTransfer::CopyCapability(kind) => { self.u8(1); self.u32(kind.get()); }
                        CaptureTransfer::CloneManaged => self.u8(2),
                    }
                }
            }
        }
    }

    fn source_type(&mut self, source: &witchy_syntax::ast::Type) {
        use witchy_syntax::ast::{Convention, Type, TypeQual};
        match source {
            Type::Named(name, arguments) => {
                self.u8(0); self.bytes(name.as_bytes()); self.len(arguments.len());
                for argument in arguments { self.source_type(argument); }
            }
            Type::Tuple(fields) => {
                self.u8(1); self.len(fields.len());
                for field in fields { self.source_type(field); }
            }
            Type::Slice(element) => { self.u8(2); self.source_type(element); }
            Type::RecordCompose { base, fields } => {
                self.u8(3); self.source_type(base); self.len(fields.len());
                for (name, field) in fields { self.bytes(name.as_bytes()); self.source_type(field); }
            }
            Type::Fn(parameters, result, conventions, qualifiers) => {
                self.u8(4); self.len(parameters.len());
                for parameter in parameters { self.source_type(parameter); }
                self.source_type(result); self.len(conventions.len());
                for convention in conventions {
                    self.u8(match convention { Convention::Let => 0, Convention::Borrow => 1, Convention::Var => 2, Convention::Own => 3 });
                }
                self.bool(qualifiers.pure); self.bool(qualifiers.once);
            }
            Type::Qualified(qualifier, inner) => {
                self.u8(5);
                match qualifier {
                    TypeQual::Frozen => self.u8(0),
                    TypeQual::Unique => self.u8(1),
                    TypeQual::LocalUnique => self.u8(2),
                    TypeQual::Borrow(lifetime) => { self.u8(3); self.bytes(lifetime.as_bytes()); }
                    TypeQual::LegacyBorrow(lifetime) => { self.u8(4); self.bytes(lifetime.as_bytes()); }
                    TypeQual::BorrowMut(lifetime) => { self.u8(5); self.bytes(lifetime.as_bytes()); }
                }
                self.source_type(inner);
            }
            Type::Dyn(name, arguments) => {
                self.u8(6); self.bytes(name.as_bytes()); self.len(arguments.len());
                for argument in arguments { self.source_type(argument); }
            }
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

    fn source_abi_bindings(&mut self, bindings: &super::CandidateAbiBindings) {
        self.len(bindings.entries().len());
        for binding in bindings.entries() {
            self.u32(binding.function().get());
            self.u32(binding.source_declaration_ordinal());
        }
    }

    fn closure_bindings(&mut self, bindings: &super::CandidateClosureBindings) {
        self.len(bindings.entries().len());
        for binding in bindings.entries() {
            self.u32(binding.function().get());
            self.u32(binding.closure());
        }
    }

    fn adapter_bindings(&mut self, bindings: &super::CandidateAdapterBindings) {
        self.len(bindings.entries().len());
        for binding in bindings.entries() {
            self.u32(binding.adapter().get());
            self.u32(binding.target().get());
        }
    }

    fn closure_layouts(
        &mut self,
        layouts: &std::collections::BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
    ) {
        self.len(layouts.len());
        for (layout, signature) in layouts {
            self.layout_id(*layout);
            self.u32(signature.get());
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

    fn tail_locations(&mut self, locations: &[super::ProperTailLocation]) {
        let mut ordered = locations.to_vec();
        ordered.sort();
        self.len(ordered.len());
        for location in ordered {
            self.fixed_bytes(&location.site().digest());
            self.u32(location.function().get());
            self.u32(location.block().get());
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
    use std::collections::BTreeMap;

    use crate::layout::LayoutBundle;

    use super::*;
    use super::super::{FunctionId, FunctionSignatureId, Revision};

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

}
