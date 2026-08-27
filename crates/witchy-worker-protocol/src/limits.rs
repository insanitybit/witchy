use crate::ProtocolError;
use crate::io::{Input, put_u32, put_u64};

pub const MAX_SOURCE_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_DIAGNOSTIC_BYTES: u32 = 1024 * 1024;
pub const MAX_CANDIDATE_WIR_BYTES: u32 = 64 * 1024 * 1024;
pub const MAX_AUTHORITY_WIR_BYTES: u32 = 64 * 1024 * 1024;
pub const MAX_FINAL_BYTE_PLAN_BYTES: u32 = 64 * 1024 * 1024;

const MAX_CPU_TIME_MS: u64 = 30 * 60 * 1000;
const MAX_WALL_TIME_MS: u64 = 30 * 60 * 1000;
const MAX_MEMORY_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_SOURCE_FILES: u32 = 100_000;
const MAX_RECURSION_DEPTH: u32 = 4_096;
const MAX_PARSER_NODES: u64 = 100_000_000;
const MAX_TYPE_EXPANSIONS: u64 = 100_000_000;
const MAX_SPECIALIZATIONS: u64 = 10_000_000;

/// Requested OS/compiler resource ceilings. This value grants no resource; a
/// launcher must apply limits at least this strict before starting the worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerLimits {
    pub cpu_time_ms: u64,
    pub wall_time_ms: u64,
    pub memory_bytes: u64,
    pub max_source_bytes: u64,
    pub max_response_bytes: u64,
    pub max_source_files: u32,
    pub max_recursion_depth: u32,
    pub max_parser_nodes: u64,
    pub max_type_expansions: u64,
    pub max_specializations: u64,
    pub max_diagnostic_bytes: u32,
    pub max_candidate_wir_bytes: u32,
    pub max_authority_wir_bytes: u32,
    pub max_final_byte_plan_bytes: u32,
}

impl WorkerLimits {
    pub fn validate(self) -> Result<Self, ProtocolError> {
        bounded_u64(self.cpu_time_ms, MAX_CPU_TIME_MS, "cpu time")?;
        bounded_u64(self.wall_time_ms, MAX_WALL_TIME_MS, "wall time")?;
        bounded_u64(self.memory_bytes, MAX_MEMORY_BYTES, "memory bytes")?;
        bounded_u64(
            self.max_source_bytes,
            MAX_SOURCE_BUNDLE_BYTES,
            "source bytes",
        )?;
        bounded_u64(
            self.max_response_bytes,
            MAX_RESPONSE_BYTES,
            "response bytes",
        )?;
        bounded_u32(self.max_source_files, MAX_SOURCE_FILES, "source files")?;
        bounded_u32(
            self.max_recursion_depth,
            MAX_RECURSION_DEPTH,
            "recursion depth",
        )?;
        bounded_u64(self.max_parser_nodes, MAX_PARSER_NODES, "parser nodes")?;
        bounded_u64(
            self.max_type_expansions,
            MAX_TYPE_EXPANSIONS,
            "type expansions",
        )?;
        bounded_u64(
            self.max_specializations,
            MAX_SPECIALIZATIONS,
            "specializations",
        )?;
        bounded_u32(
            self.max_diagnostic_bytes,
            MAX_DIAGNOSTIC_BYTES,
            "diagnostic bytes",
        )?;
        bounded_u32(
            self.max_candidate_wir_bytes,
            MAX_CANDIDATE_WIR_BYTES,
            "candidate WIR bytes",
        )?;
        bounded_u32(
            self.max_authority_wir_bytes,
            MAX_AUTHORITY_WIR_BYTES,
            "AuthorityWIR bytes",
        )?;
        bounded_u32(
            self.max_final_byte_plan_bytes,
            MAX_FINAL_BYTE_PLAN_BYTES,
            "final-byte plan bytes",
        )?;
        if u64::from(self.max_diagnostic_bytes) > self.max_response_bytes
            || u64::from(self.max_candidate_wir_bytes) > self.max_response_bytes
            || u64::from(self.max_authority_wir_bytes) > self.max_response_bytes
            || u64::from(self.max_final_byte_plan_bytes) > self.max_response_bytes
        {
            return Err(ProtocolError::InvalidLimit(
                "per-field output exceeds aggregate response bytes",
            ));
        }
        Ok(self)
    }

    pub(crate) fn encode(self, output: &mut Vec<u8>) {
        put_u64(output, self.cpu_time_ms);
        put_u64(output, self.wall_time_ms);
        put_u64(output, self.memory_bytes);
        put_u64(output, self.max_source_bytes);
        put_u64(output, self.max_response_bytes);
        put_u32(output, self.max_source_files);
        put_u32(output, self.max_recursion_depth);
        put_u64(output, self.max_parser_nodes);
        put_u64(output, self.max_type_expansions);
        put_u64(output, self.max_specializations);
        put_u32(output, self.max_diagnostic_bytes);
        put_u32(output, self.max_candidate_wir_bytes);
        put_u32(output, self.max_authority_wir_bytes);
        put_u32(output, self.max_final_byte_plan_bytes);
    }

    pub(crate) fn decode(input: &mut Input<'_>) -> Result<Self, ProtocolError> {
        Self {
            cpu_time_ms: input.u64()?,
            wall_time_ms: input.u64()?,
            memory_bytes: input.u64()?,
            max_source_bytes: input.u64()?,
            max_response_bytes: input.u64()?,
            max_source_files: input.u32()?,
            max_recursion_depth: input.u32()?,
            max_parser_nodes: input.u64()?,
            max_type_expansions: input.u64()?,
            max_specializations: input.u64()?,
            max_diagnostic_bytes: input.u32()?,
            max_candidate_wir_bytes: input.u32()?,
            max_authority_wir_bytes: input.u32()?,
            max_final_byte_plan_bytes: input.u32()?,
        }
        .validate()
    }
}

fn bounded_u32(value: u32, maximum: u32, field: &'static str) -> Result<(), ProtocolError> {
    if value == 0 || value > maximum {
        Err(ProtocolError::InvalidLimit(field))
    } else {
        Ok(())
    }
}

fn bounded_u64(value: u64, maximum: u64, field: &'static str) -> Result<(), ProtocolError> {
    if value == 0 || value > maximum {
        Err(ProtocolError::InvalidLimit(field))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> WorkerLimits {
        WorkerLimits {
            cpu_time_ms: 30_000,
            wall_time_ms: 60_000,
            memory_bytes: 512 * 1024 * 1024,
            max_source_bytes: 1024 * 1024,
            max_response_bytes: 4 * 1024 * 1024,
            max_source_files: 128,
            max_recursion_depth: 256,
            max_parser_nodes: 1_000_000,
            max_type_expansions: 100_000,
            max_specializations: 50_000,
            max_diagnostic_bytes: 64 * 1024,
            max_candidate_wir_bytes: 2 * 1024 * 1024,
            max_authority_wir_bytes: 1024 * 1024,
            max_final_byte_plan_bytes: 1024 * 1024,
        }
    }

    macro_rules! reject_value {
        ($field:ident, $value:expr) => {{
            let mut limits = valid();
            limits.$field = $value;
            assert!(
                matches!(limits.validate(), Err(ProtocolError::InvalidLimit(_))),
                "{} accepted {}",
                stringify!($field),
                $value
            );
        }};
    }

    #[test]
    fn every_zero_resource_limit_rejects() {
        reject_value!(cpu_time_ms, 0);
        reject_value!(wall_time_ms, 0);
        reject_value!(memory_bytes, 0);
        reject_value!(max_source_bytes, 0);
        reject_value!(max_response_bytes, 0);
        reject_value!(max_source_files, 0);
        reject_value!(max_recursion_depth, 0);
        reject_value!(max_parser_nodes, 0);
        reject_value!(max_type_expansions, 0);
        reject_value!(max_specializations, 0);
        reject_value!(max_diagnostic_bytes, 0);
        reject_value!(max_candidate_wir_bytes, 0);
        reject_value!(max_authority_wir_bytes, 0);
        reject_value!(max_final_byte_plan_bytes, 0);
    }

    #[test]
    fn every_overflowing_resource_limit_rejects() {
        reject_value!(cpu_time_ms, u64::MAX);
        reject_value!(wall_time_ms, u64::MAX);
        reject_value!(memory_bytes, u64::MAX);
        reject_value!(max_source_bytes, u64::MAX);
        reject_value!(max_response_bytes, u64::MAX);
        reject_value!(max_source_files, u32::MAX);
        reject_value!(max_recursion_depth, u32::MAX);
        reject_value!(max_parser_nodes, u64::MAX);
        reject_value!(max_type_expansions, u64::MAX);
        reject_value!(max_specializations, u64::MAX);
        reject_value!(max_diagnostic_bytes, u32::MAX);
        reject_value!(max_candidate_wir_bytes, u32::MAX);
        reject_value!(max_authority_wir_bytes, u32::MAX);
        reject_value!(max_final_byte_plan_bytes, u32::MAX);
    }
}
