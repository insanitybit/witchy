//! Bounded directory storage and immutable read responses.
use super::*;

/// An internal response value, not a source or launch capability. A `Dir` or
/// `File` read performs its filesystem operation once and answers with one of these:
/// a status (0 success, else the `CapError` variant ordinal plus one) and a
/// payload (the contents, or the failure message), each read back without
/// touching the filesystem again.
pub const DIR_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(22);
pub const DIR_STORAGE_IMPORT_NAMES: [&str; 16] = [
    "dir_create_new_at", "dir_replace_at", "dir_read_bytes_at",
    "dir_response_size", "dir_response_into", "dir_write_bytes_at",
    "dir_append_bytes_at", "dir_set_executable_at", "dir_rename_at",
    "dir_read_at", "dir_response_status",
    "file_read_at", "file_response_status", "file_response_size", "file_response_into",
    "dir_list_at",
];

impl CanonicalAbiCatalog {
    pub fn with_dir_storage_imports(
        mut self, signatures: [FunctionSignatureId; 16], byte_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let dir = HostParameterRole::Capability(DIR_CAPABILITY_KIND);
        let file = HostParameterRole::Capability(FILE_CAPABILITY_KIND);
        let response = HostParameterRole::Capability(DIR_RESPONSE_KIND);
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(byte_layout), access, extent: HostBufferExtent::Slice,
        };
        let imports = [
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[0],
                &[ExternRef, I32, I32, I32, I32], &[I32], signatures[0],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                vec![HostResultRole::Value(ValueType::Bool)], HostMutation::ReadOnly,
                &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[1],
                &[ExternRef, I32, I32, I32, I32], &[], signatures[1],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                Vec::new(), HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[2],
                &[ExternRef, I32, I32], &[ExternRef], signatures[2],
                vec![dir.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(DIR_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirRead])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[3],
                &[ExternRef], &[I32], signatures[3], vec![response.clone()],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::ReadOnly,
                &[AbiImportAuthority::DirRead])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[4],
                &[ExternRef, I32, I32], &[I32], signatures[4],
                vec![response.clone(), buffer(AccessMode::Write)],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers,
                &[AbiImportAuthority::DirRead])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[5],
                &[ExternRef, I32, I32, I32, I32], &[], signatures[5],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                Vec::new(), HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[6],
                &[ExternRef, I32, I32, I32, I32], &[], signatures[6],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                Vec::new(), HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[7],
                &[ExternRef, I32, I32], &[], signatures[7],
                vec![dir.clone(), buffer(AccessMode::Read)],
                Vec::new(), HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[8],
                &[ExternRef, I32, I32, I32, I32], &[], signatures[8],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                Vec::new(), HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[9],
                &[ExternRef, I32, I32], &[ExternRef], signatures[9],
                vec![dir.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(DIR_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirRead])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[10],
                &[ExternRef], &[I32], signatures[10], vec![response.clone()],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::ReadOnly,
                &[AbiImportAuthority::DirRead])?,
            // `file.read()`: the same response protocol under File authority.
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[11],
                &[ExternRef], &[ExternRef], signatures[11], vec![file],
                vec![HostResultRole::Capability(DIR_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::FileAuthority])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[12],
                &[ExternRef], &[I32], signatures[12], vec![response.clone()],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::ReadOnly,
                &[AbiImportAuthority::FileAuthority])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[13],
                &[ExternRef], &[I32], signatures[13], vec![response.clone()],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::ReadOnly,
                &[AbiImportAuthority::FileAuthority])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[14],
                &[ExternRef, I32, I32], &[I32], signatures[14],
                vec![response, buffer(AccessMode::Write)],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers,
                &[AbiImportAuthority::FileAuthority])?,
            // `dir.list()`: the admitted entry names, NUL-separated, as a response.
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[15],
                &[ExternRef], &[ExternRef], signatures[15], vec![dir],
                vec![HostResultRole::Capability(DIR_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirRead])?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}
