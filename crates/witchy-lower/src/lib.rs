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
pub mod canonical_lower;
#[doc(hidden)]
pub mod canonical_owner_field_checks;
mod canonical_layouts;
#[doc(hidden)]
pub use canonical_layouts::self_checks as canonical_layout_self_checks;
mod canonical_pipeline;
#[doc(hidden)]
#[path = "canonical_pipeline_checks.rs"]
pub mod canonical_pipeline_checks;
pub mod codegen;
pub mod escape;
