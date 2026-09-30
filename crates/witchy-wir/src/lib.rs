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
pub mod optimizer_policy;
pub mod runtime_layout_catalog;
pub mod canonical_safewir;
mod canonical_wasmir;
mod canonical_entry;
pub mod canonical_launch;
pub use canonical_entry::{CANONICAL_ENTRY_SECTION, canonical_entry_from_wasm};
pub use canonical_wasmir::{
    AdmittedOwnerBridge, OwnerBridgeRecipe,
    AdmittedVmService, AdmittedParMap, AdmittedExternalWasmImport, AdmittedCanonicalArtifact, CanonicalAdmissionError, CanonicalEntry, CanonicalEntryCapability,
    CanonicalEntryResult, HEAP_BASE_EXPORT, HEAP_FREE_EXPORT, HEAP_TOP_EXPORT, admit_canonical,
};
pub use canonical_wasmir::admit_canonical_with_observer;
pub mod safewir;
pub mod safewir_adapter;
mod safewir_call;
pub mod safewir_kernel;
pub mod safewir_reaching_defs;
pub mod safewir_surface;
pub mod safewir_validate;
pub mod wir_opt;
pub mod wir_prelude;

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod safewir_call_tests;
#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod rfc0162_mutation_evidence_tests;

// In-crate checks that need private internals. The lib is `test = false`
// (one compile, not a second cfg(test) copy); `tests/unit.rs` drives these
// panicking functions, each through a one-line #[test] wrapper.
#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod self_checks;
