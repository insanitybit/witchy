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

#[cfg(test)]
mod test_support {
    use std::collections::BTreeMap;

    use super::{ConsoleFixture, FixtureOutcome, FixturePlan, FixtureStep, FixtureValue};

    pub(crate) fn step(
        operation: &str,
        target: Option<&str>,
        arguments: BTreeMap<String, FixtureValue>,
        effective_rights: Option<Vec<String>>,
        outcome: FixtureOutcome,
    ) -> FixtureStep {
        FixtureStep {
            operation: operation.into(),
            target: target.map(str::to_owned),
            arguments,
            effective_rights,
            outcome,
            required: true,
        }
    }

    pub(crate) fn returned(value: &str) -> FixtureOutcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value.into()),
        }
    }

    pub(crate) fn console_plan(script: Vec<FixtureStep>) -> FixturePlan {
        FixturePlan {
            console: Some(ConsoleFixture { script }),
            ..Default::default()
        }
    }
}
