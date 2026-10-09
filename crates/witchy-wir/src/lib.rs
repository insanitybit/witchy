//! `witchy-wir` — the compiled backend: canonical SafeWIR, its checker, the
//! layout catalog, and admission of checked SafeWIR to Wasm.
//!
//! The `native` feature pulls in the test/validation helpers that run an emitted
//! module under wasmtime; it is off in the wasm-playground build.

// Match the project-wide lint policy (root `src/lib.rs`).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

pub mod layout;
pub mod canonical_safewir;
mod canonical_wasmir;
mod parallel;
mod canonical_entry;
pub mod canonical_launch;
pub use canonical_entry::{CANONICAL_ENTRY_SECTION, canonical_entry_from_wasm};
pub use canonical_wasmir::{
    AdmittedOwnerBridge, OwnerBridgeRecipe,
    AdmittedVmService, AdmittedParMap, AdmittedExternalWasmImport, AdmittedCanonicalArtifact, CanonicalAdmissionError, CanonicalEntry, CapabilityKind,
    CanonicalEntryResult, HEAP_BASE_EXPORT, HEAP_FREE_EXPORT, HEAP_TOP_EXPORT, admit_canonical,
    memory_op_counters::{COUNT_ALLOCS_EXPORT, COUNT_RELEASES_EXPORT, COUNT_RETAINS_EXPORT},
};
pub use canonical_wasmir::admit_canonical_with_observer;
pub use witchy_canonical_source::capabilities::HeapMode;
pub mod abi_catalog;

// In-crate checks that need private internals. The lib is `test = false`
// (one compile, not a second cfg(test) copy); `tests/unit.rs` drives these
// panicking functions, each through a one-line #[test] wrapper.
#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod self_checks;
