//! Read-only directory metadata, authenticated independently of its lowering.
use super::*;

impl CanonicalAbiCatalog {
    pub fn with_dir_is_dir_import(mut self, signature: FunctionSignatureId, string_layout: LayoutId)
        -> Result<Self, String>
    {
        use crate::abi_catalog::{AbiImportAuthority, WasmTy};
        let import = authenticated_capability_import("dir_is_dir_at",
            &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32], &[WasmTy::ExternRef], signature,
            vec![HostParameterRole::Capability(DIR_CAPABILITY_KIND),
                HostParameterRole::Buffer { value_type: ValueType::Ref(string_layout),
                    access: AccessMode::Read, extent: HostBufferExtent::Slice }],
            vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly,
            &[AbiImportAuthority::DirRead])?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
}
