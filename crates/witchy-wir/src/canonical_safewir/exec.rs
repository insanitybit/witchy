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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_catalog_authenticates_capabilities_and_bounded_buffer_roles() {
        let layout = LayoutId::from_bytes([31; 32]);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_exec_imports([0, 1, 2].map(FunctionSignatureId::new), layout).unwrap();
        let imports = catalog.imports();
        let run = imports.iter().find(|import| import.name == "exec_run_at").unwrap();
        assert_eq!(run.parameter_roles[0], HostParameterRole::Capability(EXEC_CAPABILITY_KIND));
        assert_eq!(run.parameter_roles[1], HostParameterRole::Capability(DIR_CAPABILITY_KIND));
        assert_eq!(run.result_roles, vec![HostResultRole::Capability(EXEC_RESPONSE_KIND)]);
        assert_eq!(run.mutation, HostMutation::ReadOnly);
        for role in &run.parameter_roles[2..] {
            assert_eq!(*role, HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice });
        }
        let copy = imports.iter().find(|import| import.name == "exec_response_into").unwrap();
        assert_eq!(copy.parameter_roles[0], HostParameterRole::Capability(EXEC_RESPONSE_KIND));
        assert_eq!(copy.parameter_roles[1], HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::Slice });
        assert_eq!(copy.mutation, HostMutation::WriteBuffers);
        assert!(imports.iter().all(|import| import.reentrancy == HostReentrancy::Forbidden));
    }
}
