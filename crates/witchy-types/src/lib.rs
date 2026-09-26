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
pub mod canonical_abi;
pub mod canonical_closure;
mod call_scoped;
mod task_provenance;
mod canonical_dictionary;
mod canonical_equality;
mod canonical_render;
mod canonical_messages;
pub mod async_program;
pub mod async_readiness;
pub mod async_migration;
pub mod async_control_flow;
pub mod loans;
pub mod existential;
pub mod migration;
pub mod opt_callable_export;
pub mod optimization_readiness;
pub mod precompiled_declarations;
mod record_projection;
pub mod pipeline;
pub mod runtime_type;
pub mod storage;
pub mod suspension_carrier;
pub mod traits;
pub mod typed_execution;
pub mod typed_async_lowering;
pub mod std_pruning;
/// Generated census of std names the compiler spells; public only so the
/// integration test `std_pruning::tests::pruning_roots_cover_every_compiler_std_literal`
/// can compare it against the sources.
#[doc(hidden)]
pub mod std_pruning_roots;
pub mod typeck;
pub mod try_context;
pub mod witness;
pub mod views;
