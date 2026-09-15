//! Authority-free verification over authenticated initialized String slices.
use super::*;

pub const CRYPTO_SHA512_IMPORT: &str = "crypto_sha512_slices";
pub const CRYPTO_SHA3_256_IMPORT: &str = "crypto_sha3_256_slices";
pub const CRYPTO_HMAC_SHA256_IMPORT: &str = "crypto_hmac_sha256_slices";
pub const CRYPTO_SHAKE128_IMPORT: &str = "crypto_shake128_slices";
pub const CRYPTO_SHAKE256_IMPORT: &str = "crypto_shake256_slices";
pub const CRYPTO_ECDSA_P256_VERIFY_IMPORT: &str = "crypto_ecdsa_p256_verify_slices";
pub const CRYPTO_ECDSA_P256_VERIFY_HEX_IMPORT: &str = "crypto_ecdsa_p256_verify_hex_slices";
pub const CRYPTO_EXTENSION_IMPORT_NAMES: [&str; 7] = [
    CRYPTO_SHA512_IMPORT, CRYPTO_SHA3_256_IMPORT, CRYPTO_HMAC_SHA256_IMPORT,
    CRYPTO_SHAKE128_IMPORT, CRYPTO_SHAKE256_IMPORT,
    CRYPTO_ECDSA_P256_VERIFY_IMPORT, CRYPTO_ECDSA_P256_VERIFY_HEX_IMPORT,
];

impl CanonicalAbiCatalog {
    /// Fixed authenticated transports, never caller-selected algorithms. SHAKE
    /// takes a language Int length, not a pointer or inferred memory contract.
    pub fn with_crypto_extension_imports(mut self, signatures: [FunctionSignatureId; 4],
        bytes: LayoutId) -> Result<Self, String>
    {
        use crate::wir_prelude::WasmTy::{I32, I64};
        let read = HostParameterRole::Buffer { value_type: ValueType::Ref(bytes),
            access: AccessMode::Read, extent: HostBufferExtent::Slice };
        let write = HostParameterRole::Buffer { value_type: ValueType::Ref(bytes),
            access: AccessMode::Write, extent: HostBufferExtent::Slice };
        for name in CRYPTO_EXTENSION_IMPORT_NAMES {
            let (parameters, result, signature, roles, result_type, mutation) = match name {
                CRYPTO_ECDSA_P256_VERIFY_IMPORT | CRYPTO_ECDSA_P256_VERIFY_HEX_IMPORT =>
                    (vec![I32; 6], I64, signatures[0], vec![read.clone(); 3], ValueType::Int, HostMutation::ReadOnly),
                CRYPTO_SHA512_IMPORT | CRYPTO_SHA3_256_IMPORT =>
                    (vec![I32; 4], I32, signatures[1], vec![read.clone(), write.clone()], ValueType::Size, HostMutation::WriteBuffers),
                CRYPTO_HMAC_SHA256_IMPORT =>
                    (vec![I32; 6], I32, signatures[2], vec![read.clone(), read.clone(), write.clone()], ValueType::Size, HostMutation::WriteBuffers),
                _ => (vec![I32, I32, I64, I32, I32], I32, signatures[3],
                    vec![read.clone(), HostParameterRole::Value(ValueType::Int), write.clone()], ValueType::Size, HostMutation::WriteBuffers),
            };
            let import = authenticated_prelude_import(name, &parameters, &[result], signature,
                roles, vec![HostResultRole::Value(result_type)], mutation)?;
            self.imports.insert(import.id, import);
        }
        Ok(self)
    }
}

#[cfg(test)]
mod extension_tests {
    use super::*;
    #[test]
    fn crypto_extension_catalog_has_fixed_native_only_slice_and_scalar_roles() {
        let layout = LayoutId::from_bytes([56; 32]);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_crypto_extension_imports([FunctionSignatureId::new(0); 4], layout).unwrap();
        assert_eq!(catalog.imports().len(), 7);
        for import in catalog.imports() {
            let info = crate::wir_prelude::abi_import_info(&import.name).unwrap();
            assert_eq!(info.class, crate::wir_prelude::AbiImportClass::PureInfrastructure);
            assert!(info.authorities.is_empty());
            assert!(!info.browser);
            assert_eq!(import.reentrancy, HostReentrancy::Forbidden);
            for role in &import.parameter_roles {
                assert!(matches!(role, HostParameterRole::Buffer {
                    value_type: ValueType::Ref(id), extent: HostBufferExtent::Slice, ..
                } if *id == layout) || *role == HostParameterRole::Value(ValueType::Int));
            }
            if matches!(import.name.as_str(), CRYPTO_SHAKE128_IMPORT | CRYPTO_SHAKE256_IMPORT) {
                assert_eq!(import.parameter_roles[1], HostParameterRole::Value(ValueType::Int));
                assert_eq!(import.result_roles, vec![HostResultRole::Value(ValueType::Size)]);
            }
        }
        use crate::wir_prelude::WasmTy::I32;
        assert!(authenticated_prelude_import(CRYPTO_SHAKE128_IMPORT, &[I32; 5], &[I32],
            FunctionSignatureId::new(0), vec![], vec![], HostMutation::WriteBuffers).is_err());
    }
}

pub const CRYPTO_ED25519_VERIFY_IMPORT: &str = "crypto_ed25519_verify_slices";
pub const CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT: &str = "crypto_rsa_pkcs1_sha256_verify_slices";

impl CanonicalAbiCatalog {
    /// RSA verification receives three initialized read-only UTF-8 slices, not
    /// legacy header addresses. Its status result carries no memory authority.
    pub fn with_rsa_pkcs1_sha256_verify_import(mut self, signature: FunctionSignatureId, string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy::{I32, I64};
        let read = HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let import = authenticated_prelude_import(
            CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT, &[I32; 6], &[I64], signature,
            vec![read; 3], vec![HostResultRole::Value(ValueType::Int)], HostMutation::ReadOnly,
        )?;
        self.imports.insert(import.id, import);
        Ok(self)
    }

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
    fn crypto_rsa_catalog_requires_three_initialized_read_slices() {
        let layout = LayoutId::from_bytes([55; 32]);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_rsa_pkcs1_sha256_verify_import(FunctionSignatureId::new(0), layout).unwrap();
        let imports = catalog.imports();
        assert_eq!(imports.len(), 1);
        let import = &imports[0];
        assert_eq!(import.name, CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT);
        assert_eq!(import.parameter_roles, vec![HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        }; 3]);
        assert_eq!(import.result_roles, vec![HostResultRole::Value(ValueType::Int)]);
        assert_eq!(import.mutation, HostMutation::ReadOnly);
        assert_eq!(import.reentrancy, HostReentrancy::Forbidden);
        let info = abi_import_info(import.name.as_str()).unwrap();
        assert_eq!(info.class, AbiImportClass::PureInfrastructure);
        assert!(info.authorities.is_empty());
        assert!(!info.browser);
    }

    #[test]
    fn crypto_rsa_catalog_refuses_legacy_and_result_carrier_shapes() {
        for (name, parameters, results) in [
            (CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT, vec![I32; 3], vec![I64]),
            (CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT, vec![I32; 6], vec![I32]),
            (witchy_syntax::intrinsics::CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS, vec![I32; 6], vec![I64]),
        ] {
            assert!(authenticated_prelude_import(name, &parameters, &results,
                FunctionSignatureId::new(0), vec![], vec![], HostMutation::ReadOnly).is_err());
        }
    }

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
