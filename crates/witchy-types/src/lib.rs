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
pub mod compiler_value;
mod call_scoped;
mod task_provenance;
mod canonical_dictionary;
mod canonical_equality;
mod canonical_render;
mod canonical_messages;
mod source_bridge_obligations;
#[doc(hidden)]
pub use source_bridge_obligations::self_checks as source_bridge_obligation_self_checks;
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
pub mod typed_static_async_call;
mod static_async_factory;
pub mod checked_channel_send;
pub mod checked_channel_select;
pub mod checked_channel_receive;
mod async_local_type_hints;
mod checked_channel_contract;
mod checked_scalar_contract;
mod checked_scalar_region;
mod scalar_region_recipe;
mod scalar_region_emitter;
mod scalar_region_preparation;
mod closed_region_preparation;
mod checked_select_region;
mod checked_select_contract;
mod select_region_recipe;
mod select_region_emitter;
mod select_region_preparation;
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

mod packet_region_recipe;
mod packet_region_emitter;
mod checked_packet_region;
mod packet_region_preparation;
