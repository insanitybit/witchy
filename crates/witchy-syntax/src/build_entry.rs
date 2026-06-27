//! Recognizing a rune's *build entrypoint* from its AST.
//!
//! A build entrypoint is a top-level `fn build` whose first parameter is one of
//! the build-capability types. Both checks are pure functions of the syntax
//! tree, shared by every consumer (the type checker validates it, the footprint
//! analyzer reads it, the interpreter runs it), so they live here rather than in
//! any one of those stages.

use crate::ast::{Function, Item, Module, Type};

/// True if `t` is one of the build-capability types a `fn build` receives
/// (`BuildOut` and friends) — the marker that distinguishes a build entrypoint
/// from an ordinary function named `build`.
pub fn is_build_capability_type(t: &Type) -> bool {
    matches!(t, Type::Named(n, _)
        if matches!(n.as_str(), "BuildOut" | "BuildRead" | "BuildEnv" | "BuildNet" | "BuildExec"))
}

/// The rune's build entrypoint, if any: a top-level `fn build` whose first
/// parameter is a `BuildOut`. Returns `None` for a `build` function that isn't
/// shaped like an entrypoint (so it's just an ordinary function).
pub fn build_entrypoint(module: &Module) -> Option<&Function> {
    module.items.iter().find_map(|it| match it {
        // The linker qualifies non-`main` functions as `mod.name`, so match on the
        // unqualified tail.
        Item::Function(f)
            if f.name.rsplit('.').next() == Some("build")
                && matches!(f.params.first(), Some(p) if matches!(&p.ty, Some(t) if is_build_capability_type(t))) =>
        {
            Some(f)
        }
        _ => None,
    })
}
