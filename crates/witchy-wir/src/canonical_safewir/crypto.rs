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
        use crate::abi_catalog::WasmTy::{I32, I64};
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


pub const CRYPTO_ED25519_VERIFY_IMPORT: &str = "crypto_ed25519_verify_slices";
pub const CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT: &str = "crypto_rsa_pkcs1_sha256_verify_slices";

impl CanonicalAbiCatalog {
    /// RSA verification receives three initialized read-only UTF-8 slices, not
    /// legacy header addresses. Its status result carries no memory authority.
    pub fn with_rsa_pkcs1_sha256_verify_import(mut self, signature: FunctionSignatureId, string_layout: LayoutId) -> Result<Self, String> {
        use crate::abi_catalog::WasmTy::{I32, I64};
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
        use crate::abi_catalog::WasmTy::{I32, I64};
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
        use crate::abi_catalog::WasmTy::I32;
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
