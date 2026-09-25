//! Consolidated cold-heavy RFC conformance driver.
//!
//! These tests all link the same root compiler and native runtime graph. One
//! integration binary preserves every assertion while paying that link and
//! nextest-discovery cost once instead of four times.

#[path = "rfc0080_0090_conformance/rfc0080.rs"]
mod rfc0080;
#[path = "rfc0080_0090_conformance/rfc0081_existential_frontend.rs"]
mod rfc0081_existential_frontend;
#[path = "rfc0080_0090_conformance/rfc0090.rs"]
mod rfc0090;
