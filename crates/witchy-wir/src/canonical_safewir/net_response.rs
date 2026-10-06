//! Net refinement and its immutable host responses (RFC-0126 rule 3).
use super::*;

pub const NET_LISTEN_TLS_IMPORT: &str = "net_listen_tls_at";
pub const NET_RESPONSE_IMPORT_NAMES: [&str; 2] = ["net_only_at", "net_deny_at"];

impl CanonicalAbiCatalog {
    pub fn with_net_response_imports(
        mut self, signatures: [FunctionSignatureId; 2], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy::*};
        let net = HostParameterRole::Capability(NET_CAPABILITY_KIND);
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access, extent: HostBufferExtent::Slice,
        };
        // Narrowing and subtraction are verb-neutral: either needs only the
        // Net grant itself, never Connect or Listen.
        let grant = &[AbiImportAuthority::NetGrant];
        let imports = [
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[0],
                &[ExternRef, I32, I32], &[ExternRef], signatures[0],
                vec![net.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(NET_RESPONSE_IMPORT_NAMES[1],
                &[ExternRef, I32, I32], &[ExternRef], signatures[1],
                vec![net, buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(NET_CAPABILITY_KIND)], HostMutation::ReadOnly, grant)?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }

    /// `net_listen_tls_at(net, addr, cert_pem, key) -> response`: the key is
    /// an opaque `Secret` (sealed ones included), so its bytes never enter
    /// guest memory. Binding needs both the Listen right and the Secret.
    pub fn with_net_listen_tls_import(
        mut self, signature: FunctionSignatureId, string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy::*};
        let read = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        };
        let import = authenticated_capability_import(NET_LISTEN_TLS_IMPORT,
            &[ExternRef, I32, I32, I32, I32, ExternRef], &[ExternRef], signature,
            vec![HostParameterRole::Capability(NET_CAPABILITY_KIND), read(), read(),
                HostParameterRole::Capability(SECRET_CAPABILITY_KIND)],
            vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly,
            &[AbiImportAuthority::NetListen, AbiImportAuthority::Secret])?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
}
