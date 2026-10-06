//! `env.get_env` as one host lookup answered by an immutable response.
use super::*;

pub const ENV_IMPORT_NAMES: [&str; 1] = ["env_get_at"];

impl CanonicalAbiCatalog {
    pub fn with_env_imports(
        mut self, signatures: [FunctionSignatureId; 1], string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy::*};
        let name = HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        };
        let import = authenticated_capability_import(ENV_IMPORT_NAMES[0],
            &[ExternRef, I32, I32], &[ExternRef], signatures[0],
            vec![HostParameterRole::Capability(ENV_CAPABILITY_KIND), name],
            vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
            HostMutation::ReadOnly, &[AbiImportAuthority::Env])?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
}
