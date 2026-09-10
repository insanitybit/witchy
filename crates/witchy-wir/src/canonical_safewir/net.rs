//! Net import declarations. Authority remains in host-minted references.
use super::*;

pub const NET_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(4);
pub const LISTENER_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(5);
pub const SOCKET_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(6);

pub const NET_IMPORT_NAMES: [&str; 8] = [
    "net_listen_at", "net_accept", "net_send_slice", "net_recv_line_size",
    "net_recv_bytes_size", "net_take_received_into", "net_close", "serve_pool",
];

impl CanonicalAbiCatalog {
    pub fn with_net_imports(mut self, signatures: [FunctionSignatureId; 8], string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let cap = HostParameterRole::Capability;
        let read = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        };
        let write = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Write, extent: HostBufferExtent::Slice,
        };
        let size = || vec![HostResultRole::Value(ValueType::Size)];
        let declarations = [
            (vec![ExternRef, I32, I32], vec![ExternRef], vec![cap(NET_CAPABILITY_KIND), read()], vec![HostResultRole::Capability(LISTENER_CAPABILITY_KIND)], HostMutation::ReadOnly, AbiImportAuthority::NetListen),
            (vec![ExternRef], vec![ExternRef], vec![cap(LISTENER_CAPABILITY_KIND)], vec![HostResultRole::Capability(SOCKET_CAPABILITY_KIND)], HostMutation::ReadOnly, AbiImportAuthority::NetListen),
            (vec![ExternRef, I32, I32], vec![], vec![cap(SOCKET_CAPABILITY_KIND), read()], vec![], HostMutation::ReadOnly, AbiImportAuthority::NetConnect),
            (vec![ExternRef], vec![I32], vec![cap(SOCKET_CAPABILITY_KIND)], size(), HostMutation::ReadOnly, AbiImportAuthority::NetConnect),
            (vec![ExternRef, I64], vec![I32], vec![cap(SOCKET_CAPABILITY_KIND), HostParameterRole::Value(ValueType::Int)], size(), HostMutation::ReadOnly, AbiImportAuthority::NetConnect),
            (vec![ExternRef, I32, I32], vec![I32], vec![cap(SOCKET_CAPABILITY_KIND), write()], size(), HostMutation::WriteBuffers, AbiImportAuthority::NetConnect),
            (vec![ExternRef], vec![], vec![cap(SOCKET_CAPABILITY_KIND)], vec![], HostMutation::ReadOnly, AbiImportAuthority::NetConnect),
            (vec![ExternRef], vec![], vec![cap(LISTENER_CAPABILITY_KIND)], vec![], HostMutation::ReadOnly, AbiImportAuthority::NetListen),
        ];
        for ((name, signature), (params, results, roles, result_roles, mutation, authority)) in
            NET_IMPORT_NAMES.into_iter().zip(signatures).zip(declarations)
        {
            let import = authenticated_capability_import(name, &params, &results, signature, roles, result_roles, mutation, &[authority])?;
            self.imports.insert(import.id, import);
        }
        Ok(self)
    }
}
