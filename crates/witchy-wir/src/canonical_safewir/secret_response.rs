//! `crypto.reveal` as one host read answered by an immutable response.
use super::*;

pub const SECRET_RESPONSE_IMPORT_NAMES: [&str; 1] = ["secret_reveal_at"];

impl CanonicalAbiCatalog {
    pub fn with_secret_response_imports(
        mut self, signatures: [FunctionSignatureId; 1],
    ) -> Result<Self, String> {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy::*};
        let import = authenticated_capability_import(SECRET_RESPONSE_IMPORT_NAMES[0],
            &[ExternRef], &[ExternRef], signatures[0],
            vec![HostParameterRole::Capability(SECRET_CAPABILITY_KIND)],
            vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly,
            &[AbiImportAuthority::Secret])?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
}
