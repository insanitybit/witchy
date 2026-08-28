//! Exhaustive differential-fuzz campaigns, intentionally outside the ordinary test budget.
//!
//! These tests are selected explicitly by `.github/workflows/nightly-fuzz.yml`. The default
//! nextest profile excludes this named target, rather than relying on `#[ignore]`: campaigns
//! remain visible, runnable tests with their original seed counts and failure diagnostics.

#[path = "differential_fuzz.rs"]
mod differential_fuzz;

#[test]
fn async_state_machine_backend_parity_campaign() {
    differential_fuzz::run_async_state_machine_campaign();
}

#[test]
fn scalar_backend_parity_campaign() {
    differential_fuzz::run_scalar_differential_campaign();
}

#[test]
fn capability_aggregate_backend_parity_campaign() {
    differential_fuzz::run_capability_aggregate_campaign();
}

#[test]
fn dead_allocation_metamorphic_campaign() {
    differential_fuzz::run_dead_alloc_metamorphic_campaign();
}

#[test]
fn property_law_campaign() {
    differential_fuzz::run_property_law_campaign();
}

#[test]
fn uaf_sanitizer_campaign() {
    differential_fuzz::run_uaf_sanitizer_campaign();
}

#[test]
fn rc_assertion_campaign() {
    differential_fuzz::run_rc_assertion_campaign();
}
