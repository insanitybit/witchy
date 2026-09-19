//! `witchy-interp` — frozen source-tree evaluator and temporary compiler
//! services.
//!
//! RFC-0167 freezes `interpreter`: it is not a production runtime, semantic
//! oracle, or destination for new language features. Executable acceptance is
//! independent expectation -> canonical SafeWIR interpreter -> admitted Wasm.
//! `comptime` + `tagged` temporarily continue to evaluate `comptime:` blocks
//! and `tag"…"` literals here, while `pipeline` still wires expansion into the
//! linker. Those services must migrate to canonical execution or a
//! runtime-neutral crate. The playground must migrate from this tree walker to
//! compiled admitted Wasm before the crate can be removed.

// Match the project-wide lint policy (root `src/lib.rs`).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

pub mod compiler_natives;
pub mod comptime;
pub mod interpreter;
pub mod pipeline;
pub mod typed_async;
pub mod typed_host;
pub mod typed_queue;
pub mod typed_scheduler;
pub(crate) mod reachability;
pub(crate) mod tagged;
