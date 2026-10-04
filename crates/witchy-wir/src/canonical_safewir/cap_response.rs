//! The one immutable response every fallible capability operation answers
//! (RFC-0126 rule 3).
use super::*;

/// An internal response value, not a source or launch capability. A
/// fallible `Dir`, `File`, `Env`, `Net` or `Secret` operation consults its
/// grant and the host once and answers with one of these: a status (0
/// success, else the operation's failure code), a payload (the contents, or
/// the failure message), a predicate answer, and on success the capability it
/// minted. The getters read that snapshot back and exercise no authority: the
/// operation that issued the response carries it.
pub const CAP_RESPONSE_KIND: CapabilityKindId = CapabilityKindId::new(22);
pub const CAP_RESPONSE_IMPORT_NAMES: [&str; 9] = [
    "cap_response_status", "cap_response_size", "cap_response_into", "cap_response_flag",
    "cap_response_take_dir", "cap_response_take_file", "cap_response_take_net",
    "cap_response_take_socket", "cap_response_take_listener",
];

/// The capability kinds `cap_response_take_*` mint, in import order from
/// `CAP_RESPONSE_IMPORT_NAMES[4]`.
pub const CAP_RESPONSE_TAKE_KINDS: [CapabilityKindId; 5] = [
    DIR_CAPABILITY_KIND, FILE_CAPABILITY_KIND, NET_CAPABILITY_KIND,
    SOCKET_CAPABILITY_KIND, LISTENER_CAPABILITY_KIND,
];

impl CanonicalAbiCatalog {
    pub fn with_cap_response_imports(
        mut self, signatures: [FunctionSignatureId; 9], byte_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy::*;
        let response = || vec![HostParameterRole::Capability(CAP_RESPONSE_KIND)];
        let size = || vec![HostResultRole::Value(ValueType::Size)];
        let into = vec![HostParameterRole::Capability(CAP_RESPONSE_KIND), HostParameterRole::Buffer {
            value_type: ValueType::Ref(byte_layout), access: AccessMode::Write, extent: HostBufferExtent::Slice,
        }];
        let mut imports = vec![
            authenticated_internal_service_import(CAP_RESPONSE_IMPORT_NAMES[0],
                &[ExternRef], &[I32], signatures[0], response(), size(), HostMutation::ReadOnly)?,
            authenticated_internal_service_import(CAP_RESPONSE_IMPORT_NAMES[1],
                &[ExternRef], &[I32], signatures[1], response(), size(), HostMutation::ReadOnly)?,
            authenticated_internal_service_import(CAP_RESPONSE_IMPORT_NAMES[2],
                &[ExternRef, I32, I32], &[I32], signatures[2], into, size(), HostMutation::WriteBuffers)?,
            authenticated_internal_service_import(CAP_RESPONSE_IMPORT_NAMES[3],
                &[ExternRef], &[I32], signatures[3], response(), size(), HostMutation::ReadOnly)?,
        ];
        for (offset, kind) in CAP_RESPONSE_TAKE_KINDS.into_iter().enumerate() {
            imports.push(authenticated_internal_service_import(CAP_RESPONSE_IMPORT_NAMES[4 + offset],
                &[ExternRef], &[ExternRef], signatures[4 + offset], response(),
                vec![HostResultRole::Capability(kind)], HostMutation::ReadOnly)?);
        }
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }
}
