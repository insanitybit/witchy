//! Read-only directory metadata, authenticated independently of its lowering.
use super::*;

impl CanonicalAbiCatalog {
    pub fn with_dir_is_dir_import(mut self, signature: FunctionSignatureId, string_layout: LayoutId)
        -> Result<Self, String>
    {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};
        let import = authenticated_capability_import("dir_is_dir_at",
            &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32], &[WasmTy::I32], signature,
            vec![HostParameterRole::Capability(DIR_CAPABILITY_KIND),
                HostParameterRole::Buffer { value_type: ValueType::Ref(string_layout),
                    access: AccessMode::Read, extent: HostBufferExtent::Slice }],
            vec![HostResultRole::Value(ValueType::Bool)], HostMutation::ReadOnly,
            &[AbiImportAuthority::DirRead])?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dir_metadata_catalog_requires_dir_read_initialized_slice_and_bool() {
        let layout = LayoutId::from_bytes([71; 32]);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_dir_is_dir_import(FunctionSignatureId::new(0), layout).unwrap();
        let imports = catalog.imports();
        assert_eq!(imports.len(), 1);
        let import = &imports[0];
        assert_eq!(import.parameter_roles, vec![HostParameterRole::Capability(DIR_CAPABILITY_KIND),
            HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice }]);
        assert_eq!(import.result_roles, vec![HostResultRole::Value(ValueType::Bool)]);
        assert_eq!(import.mutation, HostMutation::ReadOnly);
        assert_eq!(import.reentrancy, HostReentrancy::Forbidden);
    }
    #[test]
    fn dir_metadata_catalog_refuses_pure_or_legacy_header_authority() {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};
        assert!(authenticated_prelude_import("dir_is_dir_at", &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
            &[WasmTy::I32], FunctionSignatureId::new(0), vec![], vec![], HostMutation::ReadOnly).is_err());
        assert!(authenticated_capability_import("dir_is_dir_at", &[WasmTy::ExternRef, WasmTy::I32],
            &[WasmTy::I32], FunctionSignatureId::new(0), vec![], vec![], HostMutation::ReadOnly,
            &[AbiImportAuthority::DirRead]).is_err());
    }
}
