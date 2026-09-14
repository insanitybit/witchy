//! Confined build reads return immutable, instance-authenticated responses.
use super::*;

pub const BUILD_READ_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(12);
pub const BUILD_READ_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(13);
pub const BUILD_ENV_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(14);
pub const BUILD_EXEC_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(15);
pub const BUILD_NET_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(16);
pub const BUILD_READ_IMPORT_NAMES: [&str; 3] = [
    "build_read_at", "build_read_response_size", "build_read_response_into",
];
/// Env shares the immutable response protocol; only the first two imports are new.
pub const BUILD_ENV_IMPORT_NAMES: [&str; 4] = [
    "build_env_at", "build_response_is_some", "build_read_response_size", "build_read_response_into",
];
pub const BUILD_EXEC_IMPORT_NAMES: [&str; 3] = [
    "build_exec_run", "build_read_response_size", "build_read_response_into",
];
pub const BUILD_NET_IMPORT_NAMES: [&str; 3] = [
    "build_net_fetch", "build_read_response_size", "build_read_response_into",
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

    pub fn with_build_exec_imports(
        mut self, signatures: [FunctionSignatureId; 3], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};
        let buffer = HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        };
        let run = authenticated_capability_import(
            BUILD_EXEC_IMPORT_NAMES[0],
            &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
            &[WasmTy::ExternRef], signatures[0],
            vec![HostParameterRole::Capability(BUILD_EXEC_CAPABILITY_KIND), buffer.clone(), buffer],
            vec![HostResultRole::Capability(BUILD_READ_RESPONSE_KIND)],
            HostMutation::ReadOnly, &[AbiImportAuthority::BuildExec],
        )?;
        self.imports.insert(run.id, run);
        self.with_build_response_imports([signatures[1], signatures[2]], string_layout)
    }

    pub fn with_build_net_imports(
        mut self, signatures: [FunctionSignatureId; 3], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};
        let buffer = HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let fetch = authenticated_capability_import(
            BUILD_NET_IMPORT_NAMES[0],
            &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
            &[WasmTy::ExternRef], signatures[0],
            vec![HostParameterRole::Capability(BUILD_NET_CAPABILITY_KIND), buffer.clone(), buffer],
            vec![HostResultRole::Capability(BUILD_READ_RESPONSE_KIND)],
            HostMutation::ReadOnly, &[AbiImportAuthority::BuildFetch],
        )?;
        self.imports.insert(fetch.id, fetch);
        self.with_build_response_imports([signatures[1], signatures[2]], string_layout)
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
    fn build_net_catalog_authenticates_root_and_both_read_slices() {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};
        let layout = LayoutId::from_bytes([34; 32]);
        let signatures = [0, 1, 2].map(FunctionSignatureId::new);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_build_net_imports(signatures, layout).unwrap();
        assert_eq!(catalog.imports.len(), 3);
        let fetch = catalog.imports.values().find(|import| import.name == "build_net_fetch").unwrap();
        let buffer = HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        };
        assert_eq!(fetch.signature, signatures[0]);
        assert_eq!(fetch.parameter_roles, vec![HostParameterRole::Capability(BUILD_NET_CAPABILITY_KIND), buffer.clone(), buffer]);
        assert_eq!(fetch.result_roles, vec![HostResultRole::Capability(BUILD_READ_RESPONSE_KIND)]);
        assert_eq!(fetch.reentrancy, HostReentrancy::Forbidden);
        assert_eq!(fetch.mutation, HostMutation::ReadOnly);
        assert!(matches!(fetch.abi_identity, ImportContractIdentity::AuthenticatedAbi(_)));
        let params = [WasmTy::ExternRef, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32];
        assert!(authenticated_internal_service_import(
            "build_net_fetch", &params, &[WasmTy::ExternRef], signatures[0],
            fetch.parameter_roles.clone(), fetch.result_roles.clone(), HostMutation::ReadOnly,
        ).is_err(), "a response service must not acquire network authority");
        assert!(authenticated_capability_import(
            "build_net_fetch", &params, &[WasmTy::ExternRef], signatures[0],
            fetch.parameter_roles.clone(), fetch.result_roles.clone(), HostMutation::ReadOnly,
            &[AbiImportAuthority::BuildExec],
        ).is_err(), "a tool grant is not a BuildNet grant");
        assert!(authenticated_capability_import(
            "build_net_fetch", &[WasmTy::I32, WasmTy::I32], &[WasmTy::I32], signatures[0],
            fetch.parameter_roles.clone(), fetch.result_roles.clone(), HostMutation::ReadOnly,
            &[AbiImportAuthority::BuildFetch],
        ).is_err(), "legacy pointer-only transport must not authenticate");
    }

    #[test]
    fn build_exec_catalog_requires_token_and_two_complete_read_slices() {
        use crate::wir_prelude::WasmTy;
        let layout = LayoutId::from_bytes([33; 32]);
        let signatures = [0, 1, 2].map(FunctionSignatureId::new);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_build_exec_imports(signatures, layout).unwrap();
        assert_eq!(catalog.imports.len(), 3);
        let run = catalog.imports.values().find(|import| import.name == "build_exec_run").unwrap();
        assert_eq!(run.signature, signatures[0]);
        let buffer = HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        };
        assert_eq!(run.parameter_roles, vec![HostParameterRole::Capability(BUILD_EXEC_CAPABILITY_KIND), buffer.clone(), buffer]);
        assert_eq!(run.result_roles, vec![HostResultRole::Capability(BUILD_READ_RESPONSE_KIND)]);
        assert_eq!(run.mutation, HostMutation::ReadOnly);
        assert_eq!(run.reentrancy, HostReentrancy::Forbidden);
        assert!(matches!(run.abi_identity, ImportContractIdentity::AuthenticatedAbi(_)));
        assert!(authenticated_internal_service_import(
            "build_exec_run", &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
            &[WasmTy::ExternRef], signatures[0], run.parameter_roles.clone(), run.result_roles.clone(), HostMutation::ReadOnly,
        ).is_err(), "a service classification must not grant tool execution");
        assert!(authenticated_capability_import(
            "build_exec_run", &[WasmTy::I32, WasmTy::I32], &[WasmTy::I32], signatures[0],
            run.parameter_roles.clone(), run.result_roles.clone(), HostMutation::ReadOnly,
            &[crate::wir_prelude::AbiImportAuthority::BuildExec],
        ).is_err(), "the old pointer-only ABI must not authenticate");
    }

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
