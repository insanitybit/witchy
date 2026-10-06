//! Registry host contracts: opaque roots and initialized bounded byte slices.
use super::*;

pub const CLOCK_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(19);
pub const SECRET_STORE_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(20);
pub const SECRET_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(21);
pub const REGISTRY_HOST_IMPORT_NAMES: [&str; 6] = [
    "clock_now", "dir_make_dir_at", "secretstore_lookup_at", "crypto_sign_into", "crypto_public_key_into", "clock_now_monotonic",
];

impl CanonicalAbiCatalog {
    pub fn from_authenticated_clock(signature: FunctionSignatureId) -> Result<Self, String> {
        Self::from_imports(Vec::new()).with_clock_import(signature)
    }

    pub fn from_authenticated_monotonic_clock(signature: FunctionSignatureId) -> Result<Self, String> {
        Self::from_imports(Vec::new()).with_clock_monotonic_import(signature)
    }

    pub fn with_clock_import(mut self, signature: FunctionSignatureId) -> Result<Self, String> {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy::*};
        let import = authenticated_capability_import(REGISTRY_HOST_IMPORT_NAMES[0], &[ExternRef], &[I64], signature,
            vec![HostParameterRole::Capability(CLOCK_CAPABILITY_KIND)], vec![HostResultRole::Value(ValueType::Int)],
            HostMutation::ReadOnly, &[AbiImportAuthority::Clock])?;
        self.imports.insert(import.id, import);
        Ok(self)
    }

    pub fn with_clock_monotonic_import(mut self, signature: FunctionSignatureId) -> Result<Self, String> {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy::*};
        let import = authenticated_capability_import("clock_now_monotonic", &[ExternRef], &[I64], signature,
            vec![HostParameterRole::Capability(CLOCK_CAPABILITY_KIND)], vec![HostResultRole::Value(ValueType::Int)],
            HostMutation::ReadOnly, &[AbiImportAuthority::Clock])?;
        self.imports.insert(import.id, import);
        Ok(self)
    }

    pub fn with_registry_host_imports(
        mut self, signatures: [FunctionSignatureId; 5], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy::*};
        let cap = HostParameterRole::Capability;
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access, extent: HostBufferExtent::Slice,
        };
        self = self.with_clock_import(signatures[0])?;
        let imports = [
            authenticated_capability_import(REGISTRY_HOST_IMPORT_NAMES[1], &[ExternRef, I32, I32], &[ExternRef], signatures[1],
                vec![cap(DIR_CAPABILITY_KIND), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
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
