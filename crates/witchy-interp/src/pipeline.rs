//! Wiring that the stage crates deliberately don't do themselves.
//!
//! The linker takes a compile-time-expansion callback rather than calling
//! `comptime`/`tagged` directly (RFC-0018 dependency inversion), so the default
//! `link` — the one almost every caller wants — lives here, where naming both
//! the linker and the comptime evaluator is fine.

use witchy_syntax::ast::Module;
use witchy_syntax::linker::LinkError;

/// Link `modules` with the standard compile-time expander wired in (the common
/// case). Equivalent to the old two-argument `linker::link`.
pub fn link(modules: Vec<(String, Module)>, entry: &str) -> Result<Module, LinkError> {
    witchy_syntax::linker::link(modules, entry, crate::comptime::expand_compile_time)
}

/// Link with origin hints for modules loaded from user files. This keeps the
/// common in-memory path simple while allowing CLI diagnostics to distinguish a
/// real local std-name shadow from the bundled std fallback.
pub fn link_with_user_modules(
    modules: Vec<(String, Module)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
) -> Result<Module, LinkError> {
    witchy_syntax::linker::link_with_user_modules(
        modules,
        entry,
        crate::comptime::expand_compile_time,
        user_modules,
    )
}
