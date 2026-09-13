//! Confined build reads return immutable, instance-authenticated responses.
use super::*;

pub const BUILD_READ_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(12);
pub const BUILD_READ_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(13);
pub const BUILD_ENV_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(14);
pub const BUILD_READ_IMPORT_NAMES: [&str; 3] = [
    "build_read_at", "build_read_response_size", "build_read_response_into",
];
/// Env shares the immutable response protocol; only the first two imports are new.
pub const BUILD_ENV_IMPORT_NAMES: [&str; 4] = [
    "build_env_at", "build_response_is_some", "build_read_response_size", "build_read_response_into",
];

impl CanonicalAbiCatalog {
    pub fn with_build_read_imports(
        mut self, signatures: [FunctionSignatureId; 3], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access, extent: HostBufferExtent::Slice,
        };
        let read = authenticated_capability_import(
            BUILD_READ_IMPORT_NAMES[0], &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
            &[WasmTy::ExternRef], signatures[0],
            vec![HostParameterRole::Capability(BUILD_READ_CAPABILITY_KIND), buffer(AccessMode::Read)],
            vec![HostResultRole::Capability(BUILD_READ_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::BuildRead],
        )?;
        self.imports.insert(read.id, read);
        self.with_build_response_imports([signatures[1], signatures[2]], string_layout)
    }

    pub fn with_build_env_imports(
        mut self, signatures: [FunctionSignatureId; 4], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};
        let read = authenticated_capability_import(
            BUILD_ENV_IMPORT_NAMES[0], &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
            &[WasmTy::ExternRef], signatures[0],
            vec![HostParameterRole::Capability(BUILD_ENV_CAPABILITY_KIND), HostParameterRole::Buffer {
                value_type: ValueType::Ref(string_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
            }],
            vec![HostResultRole::Capability(BUILD_READ_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::BuildEnv],
        )?;
        let present = authenticated_internal_service_import(
            BUILD_ENV_IMPORT_NAMES[1], &[WasmTy::ExternRef], &[WasmTy::I32], signatures[1],
            vec![HostParameterRole::Capability(BUILD_READ_RESPONSE_KIND)],
            vec![HostResultRole::Value(ValueType::Bool)], HostMutation::ReadOnly,
        )?;
        for import in [read, present] { self.imports.insert(import.id, import); }
        self.with_build_response_imports([signatures[2], signatures[3]], string_layout)
    }

    fn with_build_response_imports(
        mut self, signatures: [FunctionSignatureId; 2], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy;
        let size = authenticated_internal_service_import(
            BUILD_READ_IMPORT_NAMES[1], &[WasmTy::ExternRef], &[WasmTy::I32], signatures[0],
            vec![HostParameterRole::Capability(BUILD_READ_RESPONSE_KIND)],
            vec![HostResultRole::Value(ValueType::Size)], HostMutation::ReadOnly,
        )?;
        let copy = authenticated_internal_service_import(
            BUILD_READ_IMPORT_NAMES[2], &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
            &[WasmTy::I32], signatures[1],
            vec![HostParameterRole::Capability(BUILD_READ_RESPONSE_KIND), HostParameterRole::Buffer {
                value_type: ValueType::Ref(string_layout), access: AccessMode::Write, extent: HostBufferExtent::Slice,
            }],
            vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers,
        )?;
        for import in [size, copy] { self.imports.insert(import.id, import); }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_env_catalog_authenticates_root_and_keeps_presence_internal() {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};
        let layout = LayoutId::from_bytes([32; 32]);
        let signatures = [0, 1, 2, 3].map(FunctionSignatureId::new);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_build_env_imports(signatures, layout).unwrap();
        let imports: Vec<_> = BUILD_ENV_IMPORT_NAMES.iter().map(|name| {
            catalog.imports.values().find(|import| import.name == *name).unwrap()
        }).collect();
        assert_eq!(catalog.imports.len(), 4);
        for (index, import) in imports.iter().enumerate() {
            assert_eq!(import.signature, signatures[index]);
            assert_eq!(import.reentrancy, HostReentrancy::Forbidden);
            assert!(matches!(import.abi_identity, ImportContractIdentity::AuthenticatedAbi(_)));
        }
        assert_eq!(imports[0].parameter_roles, vec![
            HostParameterRole::Capability(BUILD_ENV_CAPABILITY_KIND),
            HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice },
        ]);
        assert_eq!(imports[0].result_roles, vec![HostResultRole::Capability(BUILD_READ_RESPONSE_KIND)]);
        assert_eq!(imports[1].parameter_roles, vec![HostParameterRole::Capability(BUILD_READ_RESPONSE_KIND)]);
        assert_eq!(imports[1].result_roles, vec![HostResultRole::Value(ValueType::Bool)]);
        assert_eq!(imports[1].mutation, HostMutation::ReadOnly);
        assert!(authenticated_internal_service_import(
            BUILD_ENV_IMPORT_NAMES[0], &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32], &[WasmTy::ExternRef],
            signatures[0], imports[0].parameter_roles.clone(), imports[0].result_roles.clone(), HostMutation::ReadOnly,
        ).is_err());
        assert!(authenticated_capability_import(
            BUILD_ENV_IMPORT_NAMES[1], &[WasmTy::ExternRef], &[WasmTy::I32], signatures[1],
            imports[1].parameter_roles.clone(), imports[1].result_roles.clone(), HostMutation::ReadOnly,
            &[AbiImportAuthority::BuildEnv],
        ).is_err());
    }

    #[test]
    fn build_read_catalog_separates_root_authority_from_bounded_responses() {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};
        let layout = LayoutId::from_bytes([31; 32]);
        let signatures = [FunctionSignatureId::new(0), FunctionSignatureId::new(1), FunctionSignatureId::new(2)];
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_build_read_imports(signatures, layout).unwrap();
        assert_eq!(catalog.imports.len(), 3);
        let imports: Vec<_> = BUILD_READ_IMPORT_NAMES.iter().map(|name| {
            catalog.imports.values().find(|import| import.name == *name).unwrap()
        }).collect();
        for (index, import) in imports.iter().enumerate() {
            assert_eq!(import.signature, signatures[index]);
            assert_eq!(import.reentrancy, HostReentrancy::Forbidden);
            assert!(matches!(import.abi_identity, ImportContractIdentity::AuthenticatedAbi(_)));
        }
        assert_eq!(imports[0].parameter_roles, vec![
            HostParameterRole::Capability(BUILD_READ_CAPABILITY_KIND),
            HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice },
        ]);
        assert_eq!(imports[0].result_roles, vec![HostResultRole::Capability(BUILD_READ_RESPONSE_KIND)]);
        assert_eq!(imports[0].mutation, HostMutation::ReadOnly);
        assert_eq!(imports[1].parameter_roles, vec![HostParameterRole::Capability(BUILD_READ_RESPONSE_KIND)]);
        assert_eq!(imports[1].result_roles, vec![HostResultRole::Value(ValueType::Size)]);
        assert_eq!(imports[1].mutation, HostMutation::ReadOnly);
        assert_eq!(imports[2].parameter_roles, vec![
            HostParameterRole::Capability(BUILD_READ_RESPONSE_KIND),
            HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::Slice },
        ]);
        assert_eq!(imports[2].result_roles, vec![HostResultRole::Value(ValueType::Size)]);
        assert_eq!(imports[2].mutation, HostMutation::WriteBuffers);
        assert!(authenticated_internal_service_import(
            BUILD_READ_IMPORT_NAMES[0], &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32], &[WasmTy::ExternRef],
            signatures[0], imports[0].parameter_roles.clone(), imports[0].result_roles.clone(), HostMutation::ReadOnly,
        ).is_err(), "root reads must not acquire authority from a service classification");
        assert!(authenticated_capability_import(
            BUILD_READ_IMPORT_NAMES[1], &[WasmTy::ExternRef], &[WasmTy::I32], signatures[1],
            imports[1].parameter_roles.clone(), imports[1].result_roles.clone(), HostMutation::ReadOnly, &[AbiImportAuthority::BuildRead],
        ).is_err(), "a response must not acquire root read authority");
    }
}
