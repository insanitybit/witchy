//! Canonical, backend-neutral RFC-0161 async-program schema.
//!
//! This is deliberately constructed only from final typed frame layouts.  It
//! has no generated callable names and no legacy scalar-transition fallback.

use crate::typed_async_lowering::FinalCallableIndex;
use crate::typed_execution::{ExecutionShapeId, FrameLane};

pub const ASYNC_PROGRAM_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AsyncProgramStateId(pub u32);

/// Final backend binding inputs. Capture fields are ordered physical fields;
/// lane IDs are canonical semantic fields. Backends must reject layout drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalStateBinding {
    pub state: AsyncProgramStateId,
    pub callable: FinalCallableIndex,
    pub capture_to_lane: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsyncExecutionShape {
    pub id: ExecutionShapeId,
    pub closure_layout_fingerprint: [u8; 32],
    pub frame: Vec<FrameLane>,
    pub states: Vec<FinalStateBinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsyncProgram {
    pub schema_version: u32,
    pub semantic_fingerprint: [u8; 32],
    pub execution_shapes: Vec<AsyncExecutionShape>,
}

impl AsyncProgram {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ASYNC_PROGRAM_SCHEMA_VERSION {
            return Err("unsupported async-program schema version".into());
        }
        for (expected, shape) in self.execution_shapes.iter().enumerate() {
            if shape.id.0 != expected as u32 { return Err("async execution shape IDs must be dense".into()); }
            for (lane, field) in shape.frame.iter().enumerate() {
                if field.id.0 != lane as u32 { return Err("async frame lane IDs must be dense".into()); }
            }
            for (expected_state, binding) in shape.states.iter().enumerate() {
                if binding.state.0 != expected_state as u32 { return Err("async state IDs must be dense".into()); }
                for lane in &binding.capture_to_lane {
                    if *lane as usize >= shape.frame.len() { return Err("async capture binding references absent frame lane".into()); }
                }
            }
        }
        Ok(())
    }
}
