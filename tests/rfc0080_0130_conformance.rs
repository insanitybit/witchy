//! Consolidated cold-heavy RFC conformance driver.
//!
//! These tests all link the same root compiler and native runtime graph. One
//! integration binary preserves every assertion while paying that link and
//! nextest-discovery cost once instead of seven times.

#[path = "support/authenticated.rs"]
mod authenticated;
#[path = "rfc0082/dynamic_methods.rs"]
mod dynamic_methods;
#[path = "rfc0130_generator/liveness.rs"]
mod liveness;
#[path = "rfc0130_generator/match_binding.rs"]
mod match_binding;
#[path = "rfc0130_generator/match_cfg.rs"]
mod match_cfg;
#[path = "rfc0130_generator/nested_cfg.rs"]
mod nested_cfg;
#[path = "rfc0130_generator/owned_frame.rs"]
mod owned_frame;
#[path = "rfc0080_0130_conformance/rfc0080.rs"]
mod rfc0080;
#[path = "rfc0080_0130_conformance/rfc0081_existential_frontend.rs"]
mod rfc0081_existential_frontend;
#[path = "rfc0080_0130_conformance/rfc0090.rs"]
mod rfc0090;
#[path = "rfc0130_generator/scaling.rs"]
mod scaling;
#[path = "rfc0130_generator/while_let_cfg.rs"]
mod while_let_cfg;
