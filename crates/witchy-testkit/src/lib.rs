//! Authority-free deterministic fixture data and transcripts.
//!
//! This crate deliberately has no host-provider, interpreter, compiler, or
//! filesystem dependency. It validates inert plans before a test adapter mints
//! any capability handle.

#![deny(unsafe_code)]

mod basic;
mod engine;
mod filesystem;
mod json;
mod model;
mod validate;

pub use basic::{FixtureHandle, ProviderResult};
pub use engine::{FixtureCall, FixtureSession};
pub use filesystem::FilesystemProviderResult;
pub use json::{PlanDecodeError, canonical_plan_json, parse_fixture_plan};
pub use model::*;
pub use validate::{PlanValidationError, PlanValidationLimits};
