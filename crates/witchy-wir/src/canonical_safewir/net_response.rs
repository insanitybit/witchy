//! Net refinement and its immutable host responses (RFC-0126 rule 3).
use super::*;

/// An internal response value, not a source or launch capability. A fallible
/// `Net` operation consults the grant once and answers with one of these: a
/// status (0 success, else the `CapError` variant ordinal plus one), a payload
/// (received text, resolved addresses, or the failure message), and on success
/// the `Net`, `Socket` or `Listener` it minted.
pub const NET_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(24);
pub const NET_RESPONSE_IMPORT_NAMES: [&str; 8] = [
    "net_only_at", "net_deny_at", "net_response_status", "net_response_size",
    "net_response_into", "net_response_take_net", "net_response_take_socket",
    "net_response_take_listener",
];

impl CanonicalAbiCatalog {
    pub fn with_net_response_imports(
        mut self, signatures: [FunctionSignatureId; 8], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let net = HostParameterRole::Capability(NET_CAPABILITY_KIND);
        let response = HostParameterRole::Capability(NET_RESPONSE_KIND);
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access, extent: HostBufferExtent::Slice,
        };
        let size = || vec![HostResultRole::Value(ValueType::Size)];
        // Narrowing and subtraction are verb-neutral: either needs only the
        // Net grant itself, never Connect or Listen.
        let grant = &[AbiImportAuthority::NetGrant];
        let imports = [
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[0],
                &[ExternRef, I32, I32], &[ExternRef], signatures[0],
                vec![net.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(NET_RESPONSE_KIND)], HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[1],
                &[ExternRef, I32, I32], &[ExternRef], signatures[1],
                vec![net, buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(NET_CAPABILITY_KIND)], HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[2],
                &[ExternRef], &[I32], signatures[2], vec![response.clone()],
                size(), HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[3],
                &[ExternRef], &[I32], signatures[3], vec![response.clone()],
                size(), HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[4],
                &[ExternRef, I32, I32], &[I32], signatures[4],
                vec![response.clone(), buffer(AccessMode::Write)],
                size(), HostMutation::WriteBuffers, grant)?,
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[5],
                &[ExternRef], &[ExternRef], signatures[5], vec![response.clone()],
                vec![HostResultRole::Capability(NET_CAPABILITY_KIND)], HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[6],
                &[ExternRef], &[ExternRef], signatures[6], vec![response.clone()],
                vec![HostResultRole::Capability(SOCKET_CAPABILITY_KIND)], HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[7],
                &[ExternRef], &[ExternRef], signatures[7], vec![response],
                vec![HostResultRole::Capability(LISTENER_CAPABILITY_KIND)], HostMutation::ReadOnly, grant)?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}
