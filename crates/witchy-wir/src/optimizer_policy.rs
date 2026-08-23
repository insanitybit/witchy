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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wir::{GlobalInit, Kind, WirGlobal, WirModule};

    #[test]
    fn optimizer_policy_payload_is_exact_and_versioned() {
        assert_eq!(decode(&PRESERVE_RAW_PAYLOAD), Ok(OptimizerPolicy::PreserveRaw));
        assert!(decode(&[2, PRESERVE_RAW]).unwrap_err().contains("unsupported schema"));
        assert!(decode(&[SCHEMA_VERSION, 9]).unwrap_err().contains("unsupported policy"));
        assert!(decode(&[SCHEMA_VERSION]).unwrap_err().contains("expected 2 bytes"));
    }

    #[test]
    fn encoder_translates_internal_marker_to_one_custom_section() {
        let module = WirModule {
            imports: vec![],
            funcs: vec![],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal {
                name: MARKER_GLOBAL.into(),
                kind: Kind::I32,
                mutable: false,
                init: GlobalInit::I32(1),
                export: None,
            }],
            table: None,
            exports: vec![],
        };
        let wasm = crate::wir_encode::encode(&module, &[]);
        let policies: Vec<_> = wasmparser::Parser::new(0)
            .parse_all(&wasm)
            .filter_map(|payload| match payload.expect("valid encoded module") {
                wasmparser::Payload::CustomSection(section) if section.name() == SECTION_NAME => {
                    Some(section.data().to_vec())
                }
                _ => None,
            })
            .collect();
        assert_eq!(policies, vec![PRESERVE_RAW_PAYLOAD.to_vec()]);
    }
}
