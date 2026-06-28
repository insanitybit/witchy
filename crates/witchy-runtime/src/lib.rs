//! `witchy-runtime` — runtime values, the capability host, and the wasmtime
//! sandbox (the trusted computing base).
//!
//! `value` is the interpreter's runtime value; `native` is the native-function
//! registry (string/crypto/encoding primitives both backends share); `net` and
//! `confine` are the address/path confinement checks. These are wasm-safe and
//! always built. `runtime` is the wasmtime sandbox that executes an emitted
//! module under enforced capabilities — native-only, behind the `native` feature.

// Match the project-wide lint policy (root `src/lib.rs`).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]

pub mod confine;
pub mod native;
pub mod net;
pub mod rand;
pub mod value;

/// The wasmtime sandbox (the TCB) — native-only.
#[cfg(feature = "native")]
pub mod runtime;
