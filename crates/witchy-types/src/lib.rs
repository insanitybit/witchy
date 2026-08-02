//! `witchy-types` — the type system: annotation + Hindley-Milner checking
//! (`typeck`) and trait desugaring/resolution + monomorphization (`traits`).
//!
//! `typeck` and `traits` are mutually recursive (type checking resolves traits;
//! trait lowering needs types), so they share this one crate — the intra-crate
//! cycle is fine. Everything they depend on is upstream in `witchy-syntax`.

// Match the project-wide lint policy (root `src/lib.rs`).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

pub mod access;
pub mod loans;
pub mod existential;
pub mod migration;
mod record_projection;
pub mod pipeline;
pub mod runtime_type;
pub mod storage;
pub mod traits;
pub mod typeck;
pub mod witness;

#[cfg(test)]
mod access_tests;
