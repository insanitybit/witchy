//! `witchy-interp` — the reference interpreter (the parity oracle) and the
//! compile-time evaluator.
//!
//! `interpreter` is the tree-walking evaluator the differential tests run
//! alongside the compiled path — keeping it a separate implementation is what
//! lets the parity diff catch lowering bugs. `comptime` + `tagged` evaluate
//! `comptime:` blocks and `tag"…"` literals at compile time (on the interpreter),
//! and `pipeline` wires the compile-time expander into the linker (RFC-0018
//! dependency inversion). These four are mutually recursive — one crate, one
//! intra-crate cycle. Wasm-safe: this is the playground's run path, not the
//! native wasmtime sandbox.

// Match the project-wide lint policy (root `src/lib.rs`).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

pub mod comptime;
pub mod interpreter;
pub mod pipeline;
pub mod reachability;
pub mod tagged;
