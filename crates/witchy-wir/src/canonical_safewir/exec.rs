//! Exec observes exact host grants; immutable responses confer no root authority.
use super::*;

pub const EXEC_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(10);
pub const EXEC_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(11);
pub const EXEC_IMPORT_NAMES: [&str; 3] = ["exec_run_at", "exec_response_size", "exec_response_into"];

impl CanonicalAbiCatalog {
    pub fn with_exec_imports(mut self, signatures: [FunctionSignatureId; 3], string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::{WasmTy::*, AbiImportAuthority};
        let cap = HostParameterRole::Capability;
        let read = || HostParameterRole::Buffer { value_type: ValueType::Ref(string_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice };
        let write = HostParameterRole::Buffer { value_type: ValueType::Ref(string_layout), access: AccessMode::Write, extent: HostBufferExtent::Slice };
        let size = || vec![HostResultRole::Value(ValueType::Size)];
        let imports = [
            authenticated_capability_import(EXEC_IMPORT_NAMES[0], &[ExternRef, ExternRef, I32, I32, I32, I32, I32, I32], &[ExternRef], signatures[0],
                vec![cap(EXEC_CAPABILITY_KIND), cap(DIR_CAPABILITY_KIND), read(), read(), read()], vec![HostResultRole::Capability(EXEC_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::Exec, AbiImportAuthority::DirRead])?,
            authenticated_internal_service_import(EXEC_IMPORT_NAMES[1], &[ExternRef], &[I32], signatures[1], vec![cap(EXEC_RESPONSE_KIND)], size(), HostMutation::ReadOnly)?,
            authenticated_internal_service_import(EXEC_IMPORT_NAMES[2], &[ExternRef, I32, I32], &[I32], signatures[2], vec![cap(EXEC_RESPONSE_KIND), write], size(), HostMutation::WriteBuffers)?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}
