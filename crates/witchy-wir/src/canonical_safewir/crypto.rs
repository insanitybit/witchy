//! Authority-free verification over authenticated initialized String slices.
use super::*;

pub const CRYPTO_ED25519_VERIFY_IMPORT: &str = "crypto_ed25519_verify_slices";

impl CanonicalAbiCatalog {
    pub fn with_ed25519_verify_import(mut self, signature: FunctionSignatureId, string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy::{I32, I64};
        let read = HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let import = authenticated_prelude_import(
            CRYPTO_ED25519_VERIFY_IMPORT, &[I32; 6], &[I64], signature,
            vec![read; 3], vec![HostResultRole::Value(ValueType::Int)], HostMutation::ReadOnly,
        )?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wir_prelude::{abi_import_info, AbiImportClass, WasmTy::{I32, I64}};

    #[test]
    fn crypto_ed25519_catalog_requires_three_initialized_read_slices() {
        let layout = LayoutId::from_bytes([53; 32]);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_ed25519_verify_import(FunctionSignatureId::new(0), layout).unwrap();
        let imports = catalog.imports();
        assert_eq!(imports.len(), 1);
        let import = &imports[0];
        assert_eq!(import.name, CRYPTO_ED25519_VERIFY_IMPORT);
        assert_eq!(import.parameter_roles, vec![HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        }; 3]);
        assert_eq!(import.result_roles, vec![HostResultRole::Value(ValueType::Int)]);
        assert_eq!(import.mutation, HostMutation::ReadOnly);
        assert_eq!(import.reentrancy, HostReentrancy::Forbidden);
        let info = abi_import_info(CRYPTO_ED25519_VERIFY_IMPORT).unwrap();
        assert_eq!(info.class, AbiImportClass::PureInfrastructure);
        assert!(info.authorities.is_empty());
        assert!(!info.browser);
    }

    #[test]
    fn crypto_ed25519_catalog_does_not_accept_the_legacy_header_shape() {
        assert!(authenticated_prelude_import(
            CRYPTO_ED25519_VERIFY_IMPORT, &[I32; 3], &[I64], FunctionSignatureId::new(0),
            vec![], vec![], HostMutation::ReadOnly,
        ).is_err());
        assert!(authenticated_prelude_import(
            witchy_syntax::intrinsics::CRYPTO_ED25519_VERIFY_STATUS, &[I32; 6], &[I64], FunctionSignatureId::new(0),
            vec![], vec![], HostMutation::ReadOnly,
        ).is_err());
    }
}

pub const CRYPTO_SHA256_IMPORT: &str = "crypto_sha256_slices";
pub const CRYPTO_RUNE_HASH_IMPORT: &str = "crypto_rune_hash_slices";

impl CanonicalAbiCatalog {
    /// Stateless hashing: an initialized byte slice and a fully checked output
    /// capacity. The RuneHash input is a wire frame, never a managed list.
    pub fn with_crypto_hash_imports(
        mut self,
        signatures: [FunctionSignatureId; 2],
        byte_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy::I32;
        for (name, signature) in [CRYPTO_SHA256_IMPORT, CRYPTO_RUNE_HASH_IMPORT]
            .into_iter().zip(signatures)
        {
            let roles = [AccessMode::Read, AccessMode::Write].into_iter().map(|access|
                HostParameterRole::Buffer { value_type: ValueType::Ref(byte_layout),
                    access, extent: HostBufferExtent::Slice }).collect();
            let import = authenticated_prelude_import(name, &[I32; 4], &[I32], signature,
                roles, vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers)?;
            self.imports.insert(import.id, import);
        }
        Ok(self)
    }
}

#[cfg(test)]
mod hash_tests {
    use super::*;
    #[test]
    fn crypto_hash_catalog_authenticates_raw_bytes_and_full_output_capacity() {
        let layout = LayoutId::from_bytes([54; 32]);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }.with_crypto_hash_imports(
            [FunctionSignatureId::new(0), FunctionSignatureId::new(1)], layout).unwrap();
        for import in catalog.imports() {
            assert_eq!(import.parameter_roles, vec![
                HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice },
                HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::Slice }]);
            assert_eq!(import.mutation, HostMutation::WriteBuffers);
            assert_eq!(import.result_roles, vec![HostResultRole::Value(ValueType::Size)]);
        }
    }
}
