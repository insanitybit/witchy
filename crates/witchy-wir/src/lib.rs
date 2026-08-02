//! `witchy-wir` — the compiled backend's intermediate representation.
//!
//! WIR is a typed expression tree with named lexical `Block`/`Loop` labels (no
//! relooper), a peephole optimizer over it, the precompiled runtime-helper
//! prelude, and the encoder that emits a wasm binary via `wasm-encoder`. The
//! group is self-contained — it depends on nothing else in the compiler — so it
//! is the first crate split out of the monolith (RFC-0018).
//!
//! The `native` feature pulls in the test/validation helpers that run an emitted
//! module under wasmtime; it is off in the wasm-playground build.

// Match the project-wide lint policy (root `src/lib.rs`): collapsed-style `if`
// chains are intentional in the WIR builders.
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

pub mod wir;
pub mod wir_encode;
pub mod wir_helpers;
pub mod layout;
pub mod wir_opt;
pub mod wir_prelude;

#[cfg(test)]
mod layout_tests;
