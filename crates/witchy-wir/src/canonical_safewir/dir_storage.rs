//! Bounded directory storage and immutable read responses.
use super::*;

pub const DIR_STORAGE_IMPORT_NAMES: [&str; 16] = [
    "dir_create_new_at", "dir_replace_at", "dir_read_bytes_at",
    "dir_write_bytes_at", "dir_append_bytes_at", "dir_set_executable_at",
    "dir_rename_at", "dir_read_at", "file_read_at", "dir_list_at",
    "dir_subtree_open_at", "dir_read_file_at", "dir_write_file_at",
    "dir_append_at", "file_write_at", "dir_only_at",
];

impl CanonicalAbiCatalog {
    pub fn with_dir_storage_imports(
        mut self, signatures: [FunctionSignatureId; 16], byte_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let dir = HostParameterRole::Capability(DIR_CAPABILITY_KIND);
        let file = HostParameterRole::Capability(FILE_CAPABILITY_KIND);
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(byte_layout), access, extent: HostBufferExtent::Slice,
        };
        let imports = [
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[0],
                &[ExternRef, I32, I32, I32, I32], &[ExternRef], signatures[0],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly,
                &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[1],
                &[ExternRef, I32, I32, I32, I32], &[ExternRef], signatures[1],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[2],
                &[ExternRef, I32, I32], &[ExternRef], signatures[2],
                vec![dir.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirRead])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[3],
                &[ExternRef, I32, I32, I32, I32], &[ExternRef], signatures[3],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[4],
                &[ExternRef, I32, I32, I32, I32], &[ExternRef], signatures[4],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[5],
                &[ExternRef, I32, I32], &[ExternRef], signatures[5],
                vec![dir.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[6],
                &[ExternRef, I32, I32, I32, I32], &[ExternRef], signatures[6],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[7],
                &[ExternRef, I32, I32], &[ExternRef], signatures[7],
                vec![dir.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirRead])?,
            // `file.read()`: the same response protocol under File authority.
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[8],
                &[ExternRef], &[ExternRef], signatures[8], vec![file],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::FileAuthority])?,
            // `dir.list()`: the admitted entry names, NUL-separated, as a response.
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[9],
                &[ExternRef], &[ExternRef], signatures[9], vec![dir.clone()],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirRead])?,
            // Navigation answers a response carrying the minted capability.
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[10],
                &[ExternRef, I32, I32], &[ExternRef], signatures[10],
                vec![dir.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirRead])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[11],
                &[ExternRef, I32, I32], &[ExternRef], signatures[11],
                vec![dir.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirRead])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[12],
                &[ExternRef, I32, I32], &[ExternRef], signatures[12],
                vec![dir.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            // The write family's new members answer responses like the rest.
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[13],
                &[ExternRef, I32, I32, I32, I32], &[ExternRef], signatures[13],
                vec![dir.clone(), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[14],
                &[ExternRef, I32, I32], &[ExternRef], signatures[14],
                vec![HostParameterRole::Capability(FILE_CAPABILITY_KIND), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(CAP_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::FileAuthority])?,
            // `dir.only(policy)`: narrowing never widens and never fails, so
            // any Dir may narrow itself and the result is the Dir itself.
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[15],
                &[ExternRef, I32, I32], &[ExternRef], signatures[15],
                vec![dir.clone(), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(DIR_CAPABILITY_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::DirGrant])?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}
