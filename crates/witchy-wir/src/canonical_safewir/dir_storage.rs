//! Bounded directory storage and immutable binary-read snapshots.
use super::*;

/// An internal response value, not a source or launch capability.
pub const DIR_BYTES_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(22);
pub const DIR_STORAGE_IMPORT_NAMES: [&str; 9] = [
    "dir_create_new_at", "dir_replace_at", "dir_read_bytes_at",
    "dir_bytes_response_size", "dir_bytes_response_into", "dir_write_bytes_at",
    "dir_append_bytes_at", "dir_set_executable_at", "dir_rename_at",
];

impl CanonicalAbiCatalog {
    pub fn with_dir_storage_imports(
        mut self, signatures: [FunctionSignatureId; 9], byte_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let dir = HostParameterRole::Capability(DIR_CAPABILITY_KIND);
        let response = HostParameterRole::Capability(DIR_BYTES_RESPONSE_KIND);
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
                vec![HostResultRole::Capability(DIR_BYTES_RESPONSE_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::DirRead])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[3],
                &[ExternRef], &[I32], signatures[3], vec![response.clone()],
                vec![HostResultRole::Value(ValueType::Size)], HostMutation::ReadOnly,
                &[AbiImportAuthority::DirRead])?,
            authenticated_capability_import(DIR_STORAGE_IMPORT_NAMES[4],
                &[ExternRef, I32, I32], &[I32], signatures[4],
                vec![response, buffer(AccessMode::Write)],
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
                vec![dir, buffer(AccessMode::Read), buffer(AccessMode::Read)],
                Vec::new(), HostMutation::ReadOnly, &[AbiImportAuthority::DirWrite])?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_storage_catalog_preserves_atomic_bool_and_snapshot_roles() {
        let layout = LayoutId::from_bytes([83; 32]);
        let catalog = CanonicalAbiCatalog::from_imports(Vec::new())
            .with_dir_storage_imports([0, 1, 2, 3, 4, 5, 6, 7, 8].map(FunctionSignatureId::new), layout).unwrap();
        let imports = catalog.imports();
        assert_eq!(imports.len(), DIR_STORAGE_IMPORT_NAMES.len());
        let at = |index: usize| imports.iter().find(|import| import.name == DIR_STORAGE_IMPORT_NAMES[index]).unwrap();
        assert_eq!(at(0).result_roles, [HostResultRole::Value(ValueType::Bool)]);
        assert!(at(1).result_roles.is_empty());
        assert_eq!(at(2).result_roles, [HostResultRole::Capability(DIR_BYTES_RESPONSE_KIND)]);
        for index in [3, 4] {
            assert_eq!(at(index).parameter_roles[0], HostParameterRole::Capability(DIR_BYTES_RESPONSE_KIND));
            assert_eq!(at(index).result_roles, [HostResultRole::Value(ValueType::Size)]);
        }
        assert_eq!(at(4).parameter_roles[1], HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::Slice,
        });
        for index in [6, 7, 8] {
            assert!(at(index).result_roles.is_empty());
            assert_eq!(at(index).parameter_roles[0], HostParameterRole::Capability(DIR_CAPABILITY_KIND));
            assert_eq!(at(index).parameter_roles.len(), if index == 7 { 2 } else { 3 });
        }
        for (index, import) in (0..DIR_STORAGE_IMPORT_NAMES.len()).map(|index| (index, at(index))) {
            assert_eq!(import.reentrancy, HostReentrancy::Forbidden);
            assert_eq!(import.mutation, if index == 4 { HostMutation::WriteBuffers } else { HostMutation::ReadOnly });
        }
        assert_ne!(DIR_BYTES_RESPONSE_KIND, DIR_CAPABILITY_KIND);
    }
}
