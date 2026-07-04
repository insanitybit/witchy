//! `witchy-syntax` — the compiler front-end and shared base layer.
//!
//! Source text → AST (lexer, parser, `ast`) plus the AST-level passes and
//! helpers that sit upstream of type checking: `aliases` (name migration),
//! `async_lower`/`generators` (desugaring), `optimize` (AST peephole), `fmt`
//! (shared formatting primitives), `reflect` (TypeInfo construction), `consts`,
//! and the `lambda_scan`/`build_entry` analyses. Self-contained — it depends on
//! nothing downstream of itself.

// Match the project-wide lint policy (root `src/lib.rs`).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]

pub mod aliases;
pub mod ast;
pub mod async_lower;
pub mod build_entry;
pub mod consts;
pub mod derive;
pub mod diag;
pub mod doc;
pub mod fmt;
pub mod format;
pub mod generators;
pub mod lambda_scan;
pub mod lexer;
pub mod linker;
pub mod opt;
pub mod optimize;
pub mod parser;
pub mod records;
pub mod reflect;
