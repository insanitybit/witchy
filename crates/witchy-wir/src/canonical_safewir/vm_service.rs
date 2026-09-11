//! Authenticated internal service resources and exact checked source sites.
use super::*;

pub const VM_SERVICE_KIND: CapabilityKindId = CapabilityKindId::new(7);
pub const VM_SERVICE_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(8);
pub const VM_SERVICE_IMPORT_NAMES: [&str; 5] = ["vm_service_start", "vm_service_step",
    "vm_service_response_size", "vm_service_response_into", "vm_service_close"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateVmService { id: u32, caller: FunctionId, callback: FunctionId }
impl CandidateVmService {
    pub fn new(id: u32, caller: FunctionId, callback: FunctionId) -> Self { Self { id, caller, callback } }
    pub fn id(&self) -> u32 { self.id }
    pub fn caller(&self) -> FunctionId { self.caller }
    pub fn callback(&self) -> FunctionId { self.callback }
}
impl CanonicalAbiCatalog {
    pub fn with_vm_service_imports(mut self, signatures: [FunctionSignatureId; 5], byte_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy::*;
        let cap = HostParameterRole::Capability;
        let read = || HostParameterRole::Buffer { value_type: ValueType::Ref(byte_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice };
        let write = || HostParameterRole::Buffer { value_type: ValueType::Ref(byte_layout), access: AccessMode::Write, extent: HostBufferExtent::Slice };
        let size = || vec![HostResultRole::Value(ValueType::Size)];
        let declarations = [
            (vec![I64, I32, I32], vec![ExternRef], vec![HostParameterRole::Value(ValueType::Int), read()], vec![HostResultRole::Capability(VM_SERVICE_KIND)], HostMutation::ReadOnly),
            (vec![ExternRef, I32, I32], vec![ExternRef], vec![cap(VM_SERVICE_KIND), read()], vec![HostResultRole::Capability(VM_SERVICE_RESPONSE_KIND)], HostMutation::ReadOnly),
            (vec![ExternRef], vec![I32], vec![cap(VM_SERVICE_RESPONSE_KIND)], size(), HostMutation::ReadOnly),
            (vec![ExternRef, I32, I32], vec![I32], vec![cap(VM_SERVICE_RESPONSE_KIND), write()], size(), HostMutation::WriteBuffers),
            (vec![ExternRef], vec![], vec![cap(VM_SERVICE_KIND)], vec![], HostMutation::ReadOnly),
        ];
        for ((name, signature), (params, results, roles, result_roles, mutation)) in VM_SERVICE_IMPORT_NAMES.into_iter().zip(signatures).zip(declarations) {
            let import = authenticated_internal_service_import(name, &params, &results, signature, roles, result_roles, mutation)?;
            self.imports.insert(import.id, import);
        }
        Ok(self)
    }
}

pub(super) fn validate<S: ModuleStage>(candidate: &CanonicalModule<S>, source: &witchy_types::pipeline::CanonicalCheckedSource) -> Result<(), String> {
    use std::collections::{BTreeMap, BTreeSet};
    let sites = witchy_types::canonical_worker::service_sites(source)?;
    if candidate.vm_services().len() != sites.len() { return Err("missing/extra service descriptors".into()); }
    let bindings: BTreeMap<_, _> = candidate.source_abi_bindings().entries().iter()
        .map(|binding| (binding.source_declaration_ordinal(), binding.function())).collect();
    for (site, descriptor) in sites.iter().zip(candidate.vm_services()) {
        if descriptor.id() != site.id()
            || Some(&descriptor.caller()) != bindings.get(&site.caller_ordinal())
            || Some(&descriptor.callback()) != bindings.get(&site.callback_ordinal())
        { return Err("service descriptor does not match exact checked caller/callback site".into()); }
        let callback = candidate.functions().iter().find(|f| f.id == descriptor.callback()).ok_or("missing service callback")?;
        let signature = candidate.signatures().iter().find(|s| s.id == callback.signature).ok_or("missing service callback signature")?;
        if !matches!((signature.parameters.as_slice(), signature.results.as_slice()),
            ([ValueType::Ref(a), ValueType::Ref(b)], [ValueType::Ref(c)]) if a == b && b == c)
        { return Err("service callback ABI is not two owned byte buffers to one byte buffer".into()); }
    }
    let starts: BTreeSet<_> = candidate.imports().iter()
        .filter(|i| i.module == "witchy" && i.name == VM_SERVICE_IMPORT_NAMES[0]).map(|i| i.id).collect();
    let mut used = BTreeSet::new();
    for function in candidate.functions() {
        for block in &function.blocks {
            for (position, operation) in block.operations.iter().enumerate() {
                let Operation::HostCall { import, arguments, .. } = operation else { continue };
                if !starts.contains(import) { continue; }
                let Some(HostCallArgument::Value(value)) = arguments.first() else { return Err("service start requires a literal source id".into()) };
                let id = block.operations[..position].iter().find_map(|op| match op {
                    Operation::ConstInt { destination, value: literal } if destination == value => u32::try_from(*literal).ok(),
                    _ => None,
                }).ok_or("service start id is not a source literal")?;
                let descriptor = candidate.vm_services().get(id as usize).filter(|d| d.id() == id).ok_or("unknown service site")?;
                if descriptor.caller() != function.id || !used.insert(id) { return Err("wrong caller or duplicate service start site".into()); }
            }
        }
    }
    if used.len() != sites.len() { return Err("service site has no exact start operation".into()); }
    Ok(())
}
