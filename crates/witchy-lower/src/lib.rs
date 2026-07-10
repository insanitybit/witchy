//! `witchy-lower` — the lowering: checked AST → WIR.
//!
//! `codegen` lowers a type-checked module to WIR (which `witchy-wir` then encodes
//! to wasm), including the in-place / inline fast paths and the per-shape
//! equality/format/capability emission. `analysis` is the uniqueness / cap-token
//! pass those fast paths depend on. This is the compiled backend's front half;
//! it depends on the syntax, type, and WIR crates and nothing downstream.

// Match the project-wide lint policy (root `src/lib.rs`).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

pub mod analysis;
pub mod codegen;
pub mod escape;
