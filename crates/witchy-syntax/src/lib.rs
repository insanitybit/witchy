//! `witchy-syntax` — source text → AST.
//!
//! The front of the pipeline: the lexer (off-side-rule tokenizer), the parser,
//! and the `ast` types they produce. The group is self-contained — `ast` and
//! `lexer` have no in-compiler dependencies and `parser` depends only on the
//! other two — so it extracts as a clean upstream crate (RFC-0018). The
//! canonical formatter (`format`) is *not* here yet: it sits in the typeck SCC
//! and joins `witchy-syntax` once that cycle is cut.

// Match the project-wide lint policy (root `src/lib.rs`).
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]

pub mod ast;
pub mod lexer;
pub mod parser;
