//! Wiring that the stage crates deliberately don't do themselves.
//!
//! The linker takes a compile-time-expansion callback rather than calling
//! `comptime`/`tagged` directly (RFC-0018 dependency inversion), so the checked
//! production entries and deliberately feature-gated raw test entries live
//! here, where naming both the linker and the comptime evaluator is fine.

use witchy_syntax::ast::Module;
use witchy_syntax::linker::{LinkError, LinkMode};

pub use witchy_types::pipeline::{CheckedModule, PipelineError};
pub use witchy_types::runtime_type::AuthenticatedModuleOwners;

/// Link `modules` with the standard compile-time expander wired in (the common
/// case). Equivalent to the old two-argument `linker::link`.
#[cfg(any(test, feature = "raw-module-test-api"))]
pub fn link(modules: Vec<(String, Module)>, entry: &str) -> Result<Module, LinkError> {
    link_internal(modules, entry)
}

pub(crate) fn link_internal(
    modules: Vec<(String, Module)>,
    entry: &str,
) -> Result<Module, LinkError> {
    link_with_mode(modules, entry, LinkMode::Production)
}

/// Link once and retain RFC-0080 generated-node origins for tooling. Runtime
/// callers use the checked entries below.
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

pub(crate) fn link_with_mode(
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

/// Link with the standard compile-time expander, then type-check the linked
/// runtime module. This runs the standard link followed by
/// `witchy_types::typeck::check`.
pub fn link_checked(
    modules: Vec<(String, Module)>,
    entry: &str,
) -> Result<CheckedModule, PipelineError> {
    witchy_types::pipeline::link_checked(
        modules,
        entry,
        crate::comptime::expand_compile_time,
    )
}

/// Link and type-check while retaining loader-authenticated package ownership.
pub fn link_checked_authenticated(
    modules: Vec<(String, Module)>,
    entry: &str,
    module_owners: AuthenticatedModuleOwners,
) -> Result<CheckedModule, PipelineError> {
    witchy_types::pipeline::link_checked_authenticated(
        modules,
        entry,
        crate::comptime::expand_compile_time,
        module_owners,
    )
}

/// Link a test source graph under test syntax policy, then type-check it.
pub fn link_checked_test_with_user_modules(
    modules: Vec<(String, Module)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
) -> Result<CheckedModule, PipelineError> {
    witchy_types::pipeline::link_checked_test_with_user_modules(
        modules,
        entry,
        crate::comptime::expand_compile_time,
        user_modules,
    )
}

/// Link with origin hints for modules loaded from user files. This keeps the
/// common in-memory path simple while enforcing reserved std module ownership.
#[cfg(any(test, feature = "raw-module-test-api"))]
pub fn link_with_user_modules(
    modules: Vec<(String, Module)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
) -> Result<Module, LinkError> {
    link_with_user_modules_with_mode(modules, entry, user_modules, LinkMode::Production)
}

#[cfg(any(test, feature = "raw-module-test-api"))]
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

/// Link a repository corpus entry and return its RFC-0087 migration report.
///
/// The census intentionally records type-check failures instead of rejecting
/// the whole corpus, so it cannot use [`CheckedModule`]. Keeping the unchecked
/// AST inside this task-shaped boundary avoids exposing a production raw-link
/// escape solely for the historical migration verifier.
pub fn migration_census(
    modules: Vec<(String, Module)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
) -> Result<witchy_types::migration::Census, LinkError> {
    let linked = witchy_syntax::linker::link_with_user_modules(
        modules,
        entry,
        crate::comptime::expand_compile_time,
        user_modules,
    )?;
    Ok(witchy_types::migration::census_linked(&linked, entry))
}

/// Link with exact source provenance and retain authenticated package owners.
pub fn link_checked_authenticated_with_user_modules(
    modules: Vec<(String, Module)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
    module_owners: AuthenticatedModuleOwners,
) -> Result<CheckedModule, PipelineError> {
    witchy_types::pipeline::link_checked_authenticated_with_user_modules(
        modules,
        entry,
        crate::comptime::expand_compile_time,
        user_modules,
        module_owners,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_types::runtime_type::{
        DeclarationKind, ModuleLoadIdentity, PackageCoordinate, PackageSource,
    };

    #[test]
    fn production_expander_wrapper_matches_legacy_link_then_check() {
        let module = witchy_syntax::parser::parse_module("fn main() -> Int:\n  1\n")
            .expect("parse test module");
        let modules = vec![("main".to_string(), module)];

        let old = link(modules.clone(), "main").expect("legacy link");
        witchy_types::typeck::check(&old).expect("legacy check");
        let checked = link_checked(modules, "main").expect("checked link");

        assert_eq!(checked.module(), &old);
    }

    #[test]
    fn production_expander_retains_authenticated_loader_ownership() {
        let module = witchy_syntax::parser::parse_module(
            "type User:\n  User(Int)\n\nfn main() -> Int:\n  1\n",
        )
        .expect("parse test module");
        let modules = vec![("main".to_string(), module)];
        let owner = ModuleLoadIdentity::new(
            PackageCoordinate::new(PackageSource::Workspace, "example/app", "0.1.0")
                .expect("workspace coordinate"),
            ["main"],
        )
        .expect("main owner");
        let toolchain = PackageCoordinate::new(
            PackageSource::Toolchain,
            "witchy/stdlib",
            "0.1.0",
        )
        .expect("toolchain coordinate");
        let mut assignments = vec![("main".to_string(), owner.clone())];
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|module| {
            (
                (*module).to_string(),
                ModuleLoadIdentity::new(toolchain.clone(), ["std", *module])
                    .expect("toolchain std owner"),
            )
        }));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .expect("authenticated owners");

        let checked = link_checked_authenticated(modules, "main", owners)
            .expect("authenticated production link");
        let catalog = checked
            .runtime_declaration_catalog()
            .expect("catalog from retained owner");
        assert_eq!(
            catalog.resolve("main.User", DeclarationKind::Type),
            Some(&owner.declaration(DeclarationKind::Type, "User").expect("identity"))
        );
    }
}
