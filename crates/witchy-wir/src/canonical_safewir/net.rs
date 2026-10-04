//! Net import declarations. Authority remains in host-minted references.
//!
//! Every fallible operation answers with a Net response (RFC-0126 rule 3):
//! its status selects `Ok` or the `CapError` variant, and the response
//! getters in `net_response` read its payload or take what it minted.
use super::*;

pub const NET_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(4);
pub const LISTENER_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(5);
pub const SOCKET_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(6);

pub const NET_IMPORT_NAMES: [&str; 11] = [
    "net_listen_at", "net_accept_at", "net_send_at", "net_recv_line_at",
    "net_recv_bytes_at", "net_send_line_at", "net_close", "serve_pool", "net_recv_all_at",
    "net_connect_at", "net_resolve_at",
];

impl CanonicalAbiCatalog {
    pub fn with_net_imports(mut self, signatures: [FunctionSignatureId; 11], string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let cap = HostParameterRole::Capability;
        let read = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        };
        let response = || vec![HostResultRole::Capability(CAP_RESPONSE_KIND)];
        let socket = || cap(SOCKET_CAPABILITY_KIND);
        let declarations = [
            (vec![ExternRef, I32, I32], vec![ExternRef], vec![cap(NET_CAPABILITY_KIND), read()], response(), AbiImportAuthority::NetListen),
            (vec![ExternRef], vec![ExternRef], vec![cap(LISTENER_CAPABILITY_KIND)], response(), AbiImportAuthority::NetListen),
            (vec![ExternRef, I32, I32], vec![ExternRef], vec![socket(), read()], response(), AbiImportAuthority::NetConnect),
            (vec![ExternRef], vec![ExternRef], vec![socket()], response(), AbiImportAuthority::NetConnect),
            (vec![ExternRef, I64], vec![ExternRef], vec![socket(), HostParameterRole::Value(ValueType::Int)], response(), AbiImportAuthority::NetConnect),
            (vec![ExternRef, I32, I32], vec![ExternRef], vec![socket(), read()], response(), AbiImportAuthority::NetConnect),
            (vec![ExternRef], vec![], vec![socket()], vec![], AbiImportAuthority::NetConnect),
            (vec![ExternRef], vec![], vec![cap(LISTENER_CAPABILITY_KIND)], vec![], AbiImportAuthority::NetListen),
            (vec![ExternRef], vec![ExternRef], vec![socket()], response(), AbiImportAuthority::NetConnect),
            (vec![ExternRef, I32, I32], vec![ExternRef], vec![cap(NET_CAPABILITY_KIND), read()], response(), AbiImportAuthority::NetConnect),
            (vec![ExternRef, I32, I32], vec![ExternRef], vec![cap(NET_CAPABILITY_KIND), read()], response(), AbiImportAuthority::NetConnect),
        ];
        for ((name, signature), (params, results, roles, result_roles, authority)) in
            NET_IMPORT_NAMES.into_iter().zip(signatures).zip(declarations)
        {
            let import = authenticated_capability_import(name, &params, &results, signature, roles, result_roles, HostMutation::ReadOnly, &[authority])?;
            self.imports.insert(import.id, import);
        }
        Ok(self)
    }
}
