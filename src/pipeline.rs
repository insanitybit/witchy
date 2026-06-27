//! Wiring that the stage crates deliberately don't do themselves.
//!
//! The linker takes a compile-time-expansion callback rather than calling
//! `comptime`/`tagged` directly (RFC-0018 dependency inversion), so the default
//! `link` — the one almost every caller wants — lives here, where naming both
//! the linker and the comptime evaluator is fine.

use crate::ast::Module;
use crate::linker::LinkError;

/// Link `modules` with the standard compile-time expander wired in (the common
/// case). Equivalent to the old two-argument `linker::link`.
pub fn link(modules: Vec<(String, Module)>, entry: &str) -> Result<Module, LinkError> {
    crate::linker::link(modules, entry, crate::comptime::expand_compile_time)
}
