//! Versioned compiler-to-runtime policy for the optional external Wasm optimizer.
//!
//! The policy is metadata, not executable state. Unknown or malformed schemas
//! are rejected by the native runtime rather than silently selecting a backend
//! whose transforms the compiler did not authorize.

pub const SECTION_NAME: &str = "witchy.optimizer-policy";
pub const MARKER_GLOBAL: &str = "__witchy_optimizer_policy_preserve_raw_v1";
pub const SCHEMA_VERSION: u8 = 1;
const PRESERVE_RAW: u8 = 1;
pub const PRESERVE_RAW_PAYLOAD: [u8; 2] = [SCHEMA_VERSION, PRESERVE_RAW];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptimizerPolicy {
    #[default]
    Default,
    PreserveRaw,
}

pub fn decode(payload: &[u8]) -> Result<OptimizerPolicy, String> {
    let [schema, policy] = payload else {
        return Err(format!("expected 2 bytes, found {}", payload.len()));
    };
    if *schema != SCHEMA_VERSION {
        return Err(format!("unsupported schema {schema}"));
    }
    match *policy {
        PRESERVE_RAW => Ok(OptimizerPolicy::PreserveRaw),
        other => Err(format!("unsupported policy {other}")),
    }
}
