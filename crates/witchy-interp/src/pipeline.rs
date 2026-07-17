//! Wiring that the stage crates deliberately don't do themselves.
//!
//! The linker takes a compile-time-expansion callback rather than calling
//! `comptime`/`tagged` directly (RFC-0018 dependency inversion), so the default
//! `link` — the one almost every caller wants — lives here, where naming both
//! the linker and the comptime evaluator is fine.

use witchy_syntax::ast::Module;
use witchy_syntax::linker::{LinkError, LinkMode};

/// Link `modules` with the standard compile-time expander wired in (the common
/// case). Equivalent to the old two-argument `linker::link`.
pub fn link(modules: Vec<(String, Module)>, entry: &str) -> Result<Module, LinkError> {
    link_with_mode(modules, entry, LinkMode::Production)
}

/// Link once and retain RFC-0080 generated-node origins for tooling. Runtime
/// callers continue to use [`link`] and receive the identical expanded AST.
pub fn link_with_origins(
    modules: Vec<(String, Module)>,
    entry: &str,
) -> Result<witchy_syntax::linker::LinkedModule, LinkError> {
    witchy_syntax::linker::link_with_origins(
        modules,
        entry,
        crate::comptime::expand_compile_time,
    )
}

pub fn link_with_mode(
    modules: Vec<(String, Module)>,
    entry: &str,
    mode: LinkMode,
) -> Result<Module, LinkError> {
    witchy_syntax::linker::link_with_mode(
        modules,
        entry,
        crate::comptime::expand_compile_time,
        mode,
    )
}

/// Link with origin hints for modules loaded from user files. This keeps the
/// common in-memory path simple while enforcing reserved std module ownership.
pub fn link_with_user_modules(
    modules: Vec<(String, Module)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
) -> Result<Module, LinkError> {
    link_with_user_modules_with_mode(modules, entry, user_modules, LinkMode::Production)
}

pub fn link_with_user_modules_with_mode(
    modules: Vec<(String, Module)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
    mode: LinkMode,
) -> Result<Module, LinkError> {
    witchy_syntax::linker::link_with_user_modules_with_mode(
        modules,
        entry,
        crate::comptime::expand_compile_time,
        user_modules,
        mode,
    )
}
