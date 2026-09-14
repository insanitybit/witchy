//! Origin-scoped Fetch and immutable responses, separate from build authority.
use super::*;

pub const FETCH_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(17);
pub const FETCH_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(18);
pub const FETCH_IMPORT_NAMES: [&str; 5] = [
    "net_fetch_at", "fetch_send_at", "fetch_response_size", "fetch_response_into",
    "fetch_only_at",
];

impl CanonicalAbiCatalog {
    pub fn with_fetch_imports(mut self, signatures: [FunctionSignatureId; 5], string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy::*};
        let cap = HostParameterRole::Capability;
        let buffer = |access| HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access, extent: HostBufferExtent::Slice,
        };
        let imports = [
            authenticated_capability_import(FETCH_IMPORT_NAMES[0], &[ExternRef, I32, I32], &[ExternRef], signatures[0],
                vec![cap(NET_CAPABILITY_KIND), buffer(AccessMode::Read)], vec![HostResultRole::Capability(FETCH_CAPABILITY_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::NetConnect])?,
            authenticated_capability_import(FETCH_IMPORT_NAMES[1], &[ExternRef, I32, I32, I32, I32, I32, I32, I32, I32], &[ExternRef], signatures[1],
                vec![cap(FETCH_CAPABILITY_KIND), buffer(AccessMode::Read), buffer(AccessMode::Read), buffer(AccessMode::Read), buffer(AccessMode::Read)],
                vec![HostResultRole::Capability(FETCH_RESPONSE_KIND)], HostMutation::ReadOnly, &[AbiImportAuthority::Fetch])?,
            authenticated_internal_service_import(FETCH_IMPORT_NAMES[2], &[ExternRef], &[I32], signatures[2],
                vec![cap(FETCH_RESPONSE_KIND)], vec![HostResultRole::Value(ValueType::Size)], HostMutation::ReadOnly)?,
            authenticated_internal_service_import(FETCH_IMPORT_NAMES[3], &[ExternRef, I32, I32], &[I32], signatures[3],
                vec![cap(FETCH_RESPONSE_KIND), buffer(AccessMode::Write)], vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers)?,
            authenticated_capability_import(FETCH_IMPORT_NAMES[4], &[ExternRef, I32, I32], &[ExternRef], signatures[4],
                vec![cap(FETCH_CAPABILITY_KIND), buffer(AccessMode::Read)], vec![HostResultRole::Capability(FETCH_CAPABILITY_KIND)],
                HostMutation::ReadOnly, &[AbiImportAuthority::Fetch])?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_catalog_keeps_roots_responses_and_full_slices_distinct() {
        let layout = LayoutId::from_bytes([29; 32]);
        let catalog = CanonicalAbiCatalog { imports: BTreeMap::new() }
            .with_fetch_imports([0, 1, 2, 3, 4].map(FunctionSignatureId::new), layout).unwrap();
        let imports = catalog.imports();
        let derive = imports.iter().find(|import| import.name == FETCH_IMPORT_NAMES[0]).unwrap();
        assert_eq!(derive.parameter_roles[0], HostParameterRole::Capability(NET_CAPABILITY_KIND));
        assert_eq!(derive.result_roles, [HostResultRole::Capability(FETCH_CAPABILITY_KIND)]);
        let send = imports.iter().find(|import| import.name == FETCH_IMPORT_NAMES[1]).unwrap();
        assert_eq!(send.parameter_roles[0], HostParameterRole::Capability(FETCH_CAPABILITY_KIND));
        assert_eq!(send.result_roles, [HostResultRole::Capability(FETCH_RESPONSE_KIND)]);
        assert_eq!(send.parameter_roles.len(), 5);
        for role in &send.parameter_roles[1..] {
            assert_eq!(*role, HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice });
        }
        let copy = imports.iter().find(|import| import.name == FETCH_IMPORT_NAMES[3]).unwrap();
        assert_eq!(copy.parameter_roles, [HostParameterRole::Capability(FETCH_RESPONSE_KIND), HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::Slice,
        }]);
        assert_eq!(copy.mutation, HostMutation::WriteBuffers);
        let only = imports.iter().find(|import| import.name == FETCH_IMPORT_NAMES[4]).unwrap();
        assert_eq!(only.parameter_roles, [HostParameterRole::Capability(FETCH_CAPABILITY_KIND), HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        }]);
        assert_eq!(only.result_roles, [HostResultRole::Capability(FETCH_CAPABILITY_KIND)]);
        assert_eq!(only.mutation, HostMutation::ReadOnly);
        assert_ne!(FETCH_RESPONSE_KIND, BUILD_READ_RESPONSE_KIND);
        assert!(imports.iter().all(|import| import.reentrancy == HostReentrancy::Forbidden));
    }
}
