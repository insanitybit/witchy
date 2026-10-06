//! Origin-scoped Fetch and immutable responses, separate from build authority.
use super::*;

pub const FETCH_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(17);
pub const FETCH_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(18);
pub const FETCH_IMPORT_NAMES: [&str; 5] = [
    "net_fetch_at", "fetch_send_at", "fetch_response_size", "fetch_response_into",
    "fetch_only_at",
];

impl CanonicalAbiCatalog {
    pub fn with_fetch_imports(mut self, signatures: [FunctionSignatureId; 5], string_layout: LayoutId) -> Result<Self, String> {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy::*};
        let cap = HostParameterRole::Capability;
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access, extent: HostBufferExtent::Slice,
        };
        let imports = [
            authenticated_capability_import(FETCH_IMPORT_NAMES[0], &[ExternRef, I32, I32], &[ExternRef], signatures[0],
                vec![cap(NET_CAPABILITY_KIND), buffer(AccessMode::Read)], vec![HostResultRole::Capability(FETCH_CAPABILITY_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::NetConnect])?,
            authenticated_capability_import(FETCH_IMPORT_NAMES[1], &[ExternRef, I32, I32, I32, I32, I32, I32, I32, I32], &[ExternRef], signatures[1],
                vec![cap(FETCH_CAPABILITY_KIND), buffer(AccessMode::Read), buffer(AccessMode::Read), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(FETCH_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::Fetch])?,
            authenticated_internal_service_import(FETCH_IMPORT_NAMES[2], &[ExternRef], &[I32], signatures[2],
                vec![cap(FETCH_RESPONSE_KIND)], vec![HostResultRole::Value(ValueType::Size)], HostMutation::ReadOnly)?,
            authenticated_internal_service_import(FETCH_IMPORT_NAMES[3], &[ExternRef, I32, I32], &[I32], signatures[3],
                vec![cap(FETCH_RESPONSE_KIND), buffer(AccessMode::Write)], vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers)?,
            authenticated_capability_import(FETCH_IMPORT_NAMES[4], &[ExternRef, I32, I32], &[ExternRef], signatures[4],
                vec![cap(FETCH_CAPABILITY_KIND), buffer(AccessMode::Read)], vec![HostResultRole::Capability(FETCH_CAPABILITY_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::Fetch])?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}
