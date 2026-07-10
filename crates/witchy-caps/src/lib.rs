//! `witchy-caps` — capability footprint analysis and grant documents.
//!
//! `capabilities` computes a program's authority footprint from its AST (what
//! `witchy caps`/`caps-diff` report, and the widening gate the package manager
//! enforces); `grants` (native-only) reads RFC-0013 TOML grant documents. Both
//! are downstream only of the syntax front-end, so this is a clean leaf crate.

// Match the project-wide lint policy (root `src/lib.rs`).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

pub mod capabilities;
/// RFC-0013 capability grant documents (TOML); native-only (uses `serde`/`toml`).
#[cfg(feature = "native")]
pub mod grants;
