//! `console.read_line()` as one host read answered by an immutable response.
use super::*;

pub const CONSOLE_READ_IMPORT_NAMES: [&str; 1] = ["console_read_line_at"];

impl CanonicalAbiCatalog {
    pub fn with_console_read_imports(mut self, signatures: [FunctionSignatureId; 1]) -> Result<Self, String> {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy::*};
        let import = authenticated_capability_import(CONSOLE_READ_IMPORT_NAMES[0],
            &[ExternRef], &[ExternRef], signatures[0],
            vec![HostParameterRole::Capability(CONSOLE_CAPABILITY_KIND)],
            vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
            HostMutation::ReadOnly, &[AbiImportAuthority::ConsoleRead])?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
}
