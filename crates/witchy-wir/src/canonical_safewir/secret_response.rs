//! `crypto.reveal` as one host read answered by an immutable response.
use super::*;

/// An internal response value, not a source or launch capability. Revealing
/// a `Secret` consults its seal once and answers with one of these: a status
/// (0 revealed, else the `CapError` variant ordinal plus one) and a payload
/// (the secret's bytes, or the refusal message), each read back without
/// another host read.
pub const SECRET_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(25);
pub const SECRET_RESPONSE_IMPORT_NAMES: [&str; 4] = [
    "secret_reveal_at", "secret_response_status", "secret_response_size", "secret_response_into",
];

impl CanonicalAbiCatalog {
    pub fn with_secret_response_imports(
        mut self, signatures: [FunctionSignatureId; 4], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let secret = HostParameterRole::Capability(SECRET_CAPABILITY_KIND);
        let response = HostParameterRole::Capability(SECRET_RESPONSE_KIND);
        let buffer = HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Write, extent: HostBufferExtent::Slice,
        };
        let size = || vec![HostResultRole::Value(ValueType::Size)];
        let grant = &[AbiImportAuthority::Secret];
        let imports = [
            authenticated_capability_import(SECRET_RESPONSE_IMPORT_NAMES[0],
                &[ExternRef], &[ExternRef], signatures[0], vec![secret],
                vec![HostResultRole::Capability(SECRET_RESPONSE_KIND)], HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(SECRET_RESPONSE_IMPORT_NAMES[1],
                &[ExternRef], &[I32], signatures[1], vec![response.clone()],
                size(), HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(SECRET_RESPONSE_IMPORT_NAMES[2],
                &[ExternRef], &[I32], signatures[2], vec![response.clone()],
                size(), HostMutation::ReadOnly, grant)?,
            authenticated_capability_import(SECRET_RESPONSE_IMPORT_NAMES[3],
                &[ExternRef, I32, I32], &[I32], signatures[3], vec![response, buffer],
                size(), HostMutation::WriteBuffers, grant)?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}
