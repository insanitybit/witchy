//! Authenticated scalar parallel-map worker sites and their copied-buffer ABI.
use super::*;

pub const PAR_MAP_IMPORT_NAMES: [&str; 2] = ["vm_par_map_admitted_run", "vm_par_map_admitted_write"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateParMap { id: u32, caller: FunctionId, callback: FunctionId }
impl CandidateParMap {
    pub fn new(id: u32, caller: FunctionId, callback: FunctionId) -> Self { Self { id, caller, callback } }
    pub fn id(&self) -> u32 { self.id }
    pub fn caller(&self) -> FunctionId { self.caller }
    pub fn callback(&self) -> FunctionId { self.callback }
}

impl CanonicalAbiCatalog {
    pub fn with_par_map_imports(mut self, signatures: [FunctionSignatureId; 2], byte_layout: LayoutId) -> Result<Self, String> {
        use crate::abi_catalog::WasmTy::*;
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(byte_layout), access, extent: HostBufferExtent::Slice,
        };
        let declarations = [
            (vec![I64, I32, I32], vec![I32], vec![HostParameterRole::Value(ValueType::Int), buffer(AccessMode::Read)],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::ReadOnly),
            (vec![I64, I32, I32], vec![I32], vec![HostParameterRole::Value(ValueType::Int), buffer(AccessMode::Write)],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers),
        ];
        for ((name, signature), (params, results, roles, result_roles, mutation)) in
            PAR_MAP_IMPORT_NAMES.into_iter().zip(signatures).zip(declarations) {
            let import = authenticated_internal_service_import(name, &params, &results, signature,
                roles, result_roles, mutation)?;
            self.imports.insert(import.id, import);
        }
        Ok(self)
    }
}

pub(super) fn validate<S: ModuleStage>(candidate: &CanonicalModule<S>, source: &witchy_canonical_source::source::CanonicalSourceView<'_>) -> Result<(), String> {
    use std::collections::{BTreeMap, BTreeSet};
    let sites = witchy_canonical_source::service::par_map_sites(source)?;
    if candidate.par_maps().len() != sites.len() { return Err("missing/extra scalar parallel-map descriptors".into()) }
    let bindings: BTreeMap<_, _> = candidate.source_abi_bindings().entries().iter()
        .map(|binding| (binding.source_declaration_ordinal(), binding.function())).collect();
    for (site, descriptor) in sites.iter().zip(candidate.par_maps()) {
        if descriptor.id() != site.id()
            || Some(&descriptor.caller()) != bindings.get(&site.caller_ordinal())
            || Some(&descriptor.callback()) != bindings.get(&site.callback_ordinal()) {
            return Err("parallel-map descriptor does not match checked caller/callback site".into());
        }
        let callback = candidate.functions().iter().find(|function| function.id == descriptor.callback())
            .ok_or("missing parallel-map callback")?;
        let signature = candidate.signatures().iter().find(|signature| signature.id == callback.signature)
            .ok_or("missing parallel-map callback signature")?;
        if !matches!((signature.parameters.as_slice(), signature.results.as_slice()),
            ([ValueType::Int], [ValueType::Int])) {
            return Err("scalar parallel-map callback ABI is not Int -> Int".into());
        }
    }
    let starts: BTreeSet<_> = candidate.imports().iter()
        .filter(|import| import.module == "witchy" && import.name == PAR_MAP_IMPORT_NAMES[0])
        .map(|import| import.id).collect();
    let writes: BTreeSet<_> = candidate.imports().iter()
        .filter(|import| import.module == "witchy" && import.name == PAR_MAP_IMPORT_NAMES[1])
        .map(|import| import.id).collect();
    let mut used = BTreeSet::new();
    let mut written = BTreeSet::new();
    for function in candidate.functions() {
        for block in &function.blocks {
            for (position, operation) in block.operations.iter().enumerate() {
                let Operation::HostCall { import, arguments, .. } = operation else { continue };
                let is_write = writes.contains(import);
                if !is_write && !starts.contains(import) { continue }
                let Some(HostCallArgument::Value(value)) = arguments.first() else {
                    return Err("parallel-map import requires a literal source site".into())
                };
                let id = block.operations[..position].iter().find_map(|operation| match operation {
                    Operation::ConstInt { destination, value: literal } if destination == value => u32::try_from(*literal).ok(),
                    _ => None,
                }).ok_or("parallel-map import site is not a source literal")?;
                let descriptor = candidate.par_maps().get(id as usize).filter(|descriptor| descriptor.id() == id)
                    .ok_or("unknown parallel-map site")?;
                let recorded = if is_write { &mut written } else { &mut used };
                if descriptor.caller() != function.id || !recorded.insert(id) {
                    return Err("wrong caller or duplicate parallel-map import site".into());
                }
            }
        }
    }
    if used.len() != sites.len() || written != used {
        return Err("parallel-map site lacks one run/write pair".into());
    }
    Ok(())
}
