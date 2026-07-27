//! Authority-free deterministic fixture data and transcripts.
//!
//! This crate deliberately has no host-provider, interpreter, compiler, or
//! filesystem dependency. It validates inert plans before a test adapter mints
//! any capability handle.

#![deny(unsafe_code)]

mod basic;
mod engine;
mod exec;
mod filesystem;
mod fetch;
mod hex;
mod json;
mod model;
mod secret;
mod validate;

pub use basic::FixtureHandle;
pub use engine::{FixtureCall, FixtureSession};
pub use json::{
    PlanDecodeError, canonical_plan_json, parse_fixture_plan, parse_unique_json,
};
pub use model::*;
pub use validate::PlanValidationError;
