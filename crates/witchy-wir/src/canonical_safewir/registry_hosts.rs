//! Registry host contracts: opaque roots and initialized bounded byte slices.
use super::*;

pub const CLOCK_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(19);
pub const SECRET_STORE_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(20);
pub const SECRET_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(21);
pub const REGISTRY_HOST_IMPORT_NAMES: [&str; 5] = [
    "clock_now", "dir_make_dir_at", "secretstore_lookup_at", "crypto_sign_into", "crypto_public_key_into",
];

impl CanonicalAbiCatalog {
    pub fn from_authenticated_clock(signature: FunctionSignatureId) -> Result<Self, String> {
        Self::from_imports(Vec::new()).with_clock_import(signature)
    }

    pub fn with_clock_import(mut self, signature: FunctionSignatureId) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let import = authenticated_capability_import(REGISTRY_HOST_IMPORT_NAMES[0], &[ExternRef], &[I64], signature,
            vec![HostParameterRole::Capability(CLOCK_CAPABILITY_KIND)], vec![HostResultRole::Value(ValueType::Int)],
            HostMutation::ReadOnly, &[AbiImportAuthority::Clock])?;
        self.imports.insert(import.id, import);
        Ok(self)
    }

    pub fn with_registry_host_imports(
        mut self, signatures: [FunctionSignatureId; 5], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let cap = HostParameterRole::Capability;
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access, extent: HostBufferExtent::Slice,
        };
        self = self.with_clock_import(signatures[0])?;
        let imports = [
            authenticated_capability_import(REGISTRY_HOST_IMPORT_NAMES[1], &[ExternRef, I32, I32], &[], signatures[1],
                vec![cap(DIR_CAPABILITY_KIND), buffer(AccessMode::Read)], Vec::new(),
                HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(REGISTRY_HOST_IMPORT_NAMES[2], &[ExternRef, I32, I32], &[ExternRef], signatures[2],
                vec![cap(SECRET_STORE_CAPABILITY_KIND), buffer(AccessMode::Read)],
                vec![HostResultRole::NullableCapability(SECRET_CAPABILITY_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::SecretStore])?,
            authenticated_capability_import(REGISTRY_HOST_IMPORT_NAMES[3], &[ExternRef, I32, I32, I32, I32], &[I32], signatures[3],
                vec![cap(SECRET_CAPABILITY_KIND), buffer(AccessMode::Read), buffer(AccessMode::Write)],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers, &[AbiImportAuthority::Secret])?,
            authenticated_capability_import(REGISTRY_HOST_IMPORT_NAMES[4], &[ExternRef, I32, I32], &[I32], signatures[4],
                vec![cap(SECRET_CAPABILITY_KIND), buffer(AccessMode::Write)],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers, &[AbiImportAuthority::Secret])?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_hosts_catalog_binds_kinds_presence_and_complete_slices() {
        let clock = CanonicalAbiCatalog::from_authenticated_clock(FunctionSignatureId::new(0)).unwrap();
        assert_eq!(clock.imports().len(), 1);
        assert_eq!(clock.imports()[0].name, "clock_now");
        assert_eq!(clock.imports()[0].parameter_roles, [HostParameterRole::Capability(CLOCK_CAPABILITY_KIND)]);
        let layout = LayoutId::from_bytes([71; 32]);
        let catalog = CanonicalAbiCatalog::from_imports(Vec::new())
            .with_registry_host_imports([0, 1, 2, 3, 4].map(FunctionSignatureId::new), layout).unwrap();
        let imports = catalog.imports();
        let at = |index: usize| imports.iter().find(|import| import.name == REGISTRY_HOST_IMPORT_NAMES[index]).unwrap();
        let read = HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice };
        let write = HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::Slice };
        assert_eq!(at(0).parameter_roles, [HostParameterRole::Capability(CLOCK_CAPABILITY_KIND)]);
        assert_eq!(at(0).result_roles, [HostResultRole::Value(ValueType::Int)]);
        assert_eq!(at(1).parameter_roles, [HostParameterRole::Capability(DIR_CAPABILITY_KIND), read.clone()]);
        assert!(at(1).result_roles.is_empty());
        assert_eq!(at(2).parameter_roles, [HostParameterRole::Capability(SECRET_STORE_CAPABILITY_KIND), read.clone()]);
        assert_eq!(at(2).result_roles, [HostResultRole::NullableCapability(SECRET_CAPABILITY_KIND)]);
        assert_eq!(at(3).parameter_roles, [HostParameterRole::Capability(SECRET_CAPABILITY_KIND), read, write.clone()]);
        assert_eq!(at(4).parameter_roles, [HostParameterRole::Capability(SECRET_CAPABILITY_KIND), write]);
        for index in [3, 4] {
            assert_eq!(at(index).result_roles, [HostResultRole::Value(ValueType::Size)]);
            assert_eq!(at(index).mutation, HostMutation::WriteBuffers);
        }
        for index in [0, 1, 2] { assert_eq!(at(index).mutation, HostMutation::ReadOnly); }
        assert!(imports.iter().all(|import| import.reentrancy == HostReentrancy::Forbidden));
        assert_ne!(SECRET_STORE_CAPABILITY_KIND, SECRET_CAPABILITY_KIND);
        assert_ne!(SECRET_CAPABILITY_KIND, FETCH_RESPONSE_KIND);
    }

    #[test]
    fn registry_hosts_catalog_rejects_forged_physical_or_authority_projection() {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let project = |params: &[crate::wir_prelude::WasmTy], authorities: &[AbiImportAuthority]| {
            authenticated_capability_import("clock_now", params, &[I64], FunctionSignatureId::new(0),
                vec![HostParameterRole::Capability(CLOCK_CAPABILITY_KIND)],
                vec![HostResultRole::Value(ValueType::Int)], HostMutation::ReadOnly, authorities)
        };
        assert!(project(&[ExternRef], &[AbiImportAuthority::Clock]).is_ok());
        assert!(project(&[I64], &[AbiImportAuthority::Clock]).is_err());
        assert!(project(&[ExternRef], &[AbiImportAuthority::Secret]).is_err());
        assert!(project(&[ExternRef], &[]).is_err());
    }
}
