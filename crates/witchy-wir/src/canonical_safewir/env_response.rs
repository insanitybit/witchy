//! `env.get_env` as one host lookup answered by an immutable response.
use super::*;

/// An internal response value, not a source or launch capability. An `Env`
/// lookup consults the grant and the process environment once and answers
/// with one of these: a status (0 set, 1 unset, 2 denied) and a payload (the
/// value, or the denial message), each read back without another lookup.
pub const ENV_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(23);
pub const ENV_IMPORT_NAMES: [&str; 4] = [
    "env_get_at", "env_response_status", "env_response_size", "env_response_into",
];

impl CanonicalAbiCatalog {
    pub fn with_env_imports(
        mut self, signatures: [FunctionSignatureId; 4], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let env = HostParameterRole::Capability(ENV_CAPABILITY_KIND);
        let response = HostParameterRole::Capability(ENV_RESPONSE_KIND);
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access, extent: HostBufferExtent::Slice,
        };
        let size = || vec![HostResultRole::Value(ValueType::Size)];
        let imports = [
            authenticated_capability_import(ENV_IMPORT_NAMES[0],
                &[ExternRef, I32, I32], &[ExternRef], signatures[0],
                vec![env, buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(ENV_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::Env])?,
            authenticated_capability_import(ENV_IMPORT_NAMES[1],
                &[ExternRef], &[I32], signatures[1], vec![response.clone()],
                size(), HostMutation::ReadOnly, &[AbiImportAuthority::Env])?,
            authenticated_capability_import(ENV_IMPORT_NAMES[2],
                &[ExternRef], &[I32], signatures[2], vec![response.clone()],
                size(), HostMutation::ReadOnly, &[AbiImportAuthority::Env])?,
            authenticated_capability_import(ENV_IMPORT_NAMES[3],
                &[ExternRef, I32, I32], &[I32], signatures[3],
                vec![response, buffer(AccessMode::Write)],
                size(), HostMutation::WriteBuffers, &[AbiImportAuthority::Env])?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}
