//! Canonical names for compiler/private intrinsics.
//!
//! These are not user-facing APIs. They are the private entry points generated
//! by the front-end or used by std modules to reach representation-level operations.
//! Keeping the names here makes linker privacy checks, typeck signatures,
//! interpreter dispatch, and lowering agree on one identity source.

pub const GENERATED_RENDER: &str = "@render";
pub const RETIRED_SOURCE_RENDER: &str = "__render";
pub const TRY_CONTEXT: &str = "__try_ctx";

pub const ERASE: &str = "__erase";
pub const UNERASE: &str = "__unerase";

pub const BYTES_FROM_STRING: &str = "__bytes_from_string";
pub const BYTES_FROM_LIST: &str = "__bytes_from_list";
pub const BYTES_TO_STRING: &str = "__bytes_to_string";
pub const BYTES_LENGTH: &str = "__bytes_length";
pub const BYTES_AT: &str = "__bytes_at";
pub const BYTES_CONCAT: &str = "__bytes_concat";
pub const BYTES_SLICE: &str = "__bytes_slice";

pub const CHANNEL_OPEN: &str = "__channel_open";
pub const CHANNEL_SEND: &str = "__channel_send";
pub const CHANNEL_RECV: &str = "__channel_recv";
pub const CHANNEL_SELECT: &str = "__channel_select";

pub const COMPILER_DOC_RESULT_JSON: &str = "compiler.__doc_result_json";

pub const ERASURE_BRIDGES: &[&str] = &[ERASE, UNERASE];
pub const BYTES_BRIDGES: &[&str] = &[
    BYTES_FROM_STRING,
    BYTES_FROM_LIST,
    BYTES_TO_STRING,
    BYTES_LENGTH,
    BYTES_AT,
    BYTES_CONCAT,
    BYTES_SLICE,
];

pub const CHANNEL_BRIDGES: &[&str] = &[
    CHANNEL_OPEN,
    CHANNEL_SEND,
    CHANNEL_RECV,
    CHANNEL_SELECT,
];

const MESSAGE_BRIDGE_CALLERS: &[&str] = &["chan", "task"];
const BYTES_BRIDGE_CALLERS: &[&str] = &["bytes"];

pub fn is_render(name: &str) -> bool {
    name == GENERATED_RENDER
}

pub fn is_erasure_bridge(name: &str) -> bool {
    ERASURE_BRIDGES.contains(&name)
}

pub fn is_bytes_bridge(name: &str) -> bool {
    BYTES_BRIDGES.contains(&name)
}

pub fn is_channel_bridge(name: &str) -> bool {
    CHANNEL_BRIDGES.contains(&name)
}

pub fn private_intrinsic_callers(bare_name: &str) -> Option<&'static [&'static str]> {
    if is_erasure_bridge(bare_name) || is_channel_bridge(bare_name) {
        Some(MESSAGE_BRIDGE_CALLERS)
    } else if is_bytes_bridge(bare_name) {
        Some(BYTES_BRIDGE_CALLERS)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_catalog_has_expected_privacy_owners() {
        for name in ERASURE_BRIDGES.iter().chain(CHANNEL_BRIDGES.iter()) {
            assert_eq!(private_intrinsic_callers(name), Some(MESSAGE_BRIDGE_CALLERS));
        }
        for name in BYTES_BRIDGES {
            assert_eq!(private_intrinsic_callers(name), Some(BYTES_BRIDGE_CALLERS));
        }
    }

    #[test]
    fn generated_frontend_intrinsics_are_not_std_bridges() {
        assert_eq!(private_intrinsic_callers(GENERATED_RENDER), None);
        assert_eq!(private_intrinsic_callers(RETIRED_SOURCE_RENDER), None);
        assert_eq!(private_intrinsic_callers(TRY_CONTEXT), None);
        assert_eq!(private_intrinsic_callers(COMPILER_DOC_RESULT_JSON), None);
    }
}
