//! Wiring that the stage crates deliberately don't do themselves.
//!
//! The linker takes a compile-time-expansion callback rather than calling
//! `comptime`/`tagged` directly (RFC-0018 dependency inversion), so the checked
//! production entries and deliberately feature-gated raw test entries live
//! here, where naming both the linker and the comptime evaluator is fine.

use witchy_syntax::ast::Module;
use witchy_syntax::linker::{LinkError, LinkMode};

pub use witchy_types::pipeline::{
    CheckedModule, CheckedStaticEvaluationRecipe, PipelineError, PipelineLocation, PipelineStage,
};
pub use witchy_types::runtime_type::AuthenticatedModuleOwners;

/// A linked, potentially type-incomplete module retained for diagnostics and
/// source navigation.
///
/// This phase is intentionally read-only and is not accepted by runtime or
/// lowering sinks. Tooling can inspect the expanded AST and generated-node
/// origins without receiving a consuming raw-`Module` accessor.
#[derive(Debug)]
pub struct ToolingLinkedModule {
    linked: witchy_syntax::linker::LinkedModule,
}

impl ToolingLinkedModule {
    /// Borrow the expanded AST for source-oriented inspection.
    pub fn module(&self) -> &Module {
        &self.linked.module
    }

    /// Borrow generated-node origin metadata.
    pub fn origins(&self) -> &witchy_syntax::origin::OriginTable {
        &self.linked.origins
    }
}

/// Link `modules` with the standard compile-time expander wired in (the common
/// case). Equivalent to the old two-argument `linker::link`.
///
/// Test-only, and pruned: standard-library functions and inherent methods the
/// program cannot name are dropped (witchy_types::std_pruning), the subset the
/// production route compiles. A profile of the example tests put 48% of their
/// time in type checking and 34% in mono over the WHOLE linked prelude, done
/// again by the interpreter preparation and the test backend, and 3% in
/// running the programs. A test that needs a pruned function fails loudly;
/// WITCHY_PRUNE_STD=off links everything.
#[cfg(any(test, feature = "raw-module-test-api"))]
pub fn link(modules: Vec<(String, Module)>, entry: &str) -> Result<Module, LinkError> {
    let std_impls = modules
        .iter()
        .filter(|(name, _)| witchy_syntax::linker::STD_MODULES.contains(&name.as_str()))
        .flat_map(|(_, module)| module.items.iter())
        .filter_map(|item| match item {
            witchy_syntax::ast::Item::Impl(implementation) if implementation.trait_name.is_none() => {
                Some((
                    implementation.type_name.clone(),
                    implementation.methods.iter().map(|method| method.name.clone()).collect(),
                ))
            }
            _ => None,
        })
        .collect();
    let linked = link_internal(modules, entry)?;
    Ok(witchy_types::std_pruning::prune_detached(&linked, &std_impls).unwrap_or(linked))
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
) -> Result<ToolingLinkedModule, LinkError> {
    witchy_syntax::linker::link_with_origins(
        modules,
        entry,
        crate::comptime::expand_compile_time,
    )
    .map(|linked| ToolingLinkedModule { linked })
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

/// Link a repository corpus entry and return its RFC-0161 async migration
/// report. As with the earlier migration census, callers receive findings even
/// when old source is rejected by the current checker.
pub fn async_migration_census(
    modules: Vec<(String, Module)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
) -> Result<witchy_types::async_migration::Census, LinkError> {
    let linked = witchy_syntax::linker::link_with_user_modules(
        modules,
        entry,
        crate::comptime::expand_compile_time,
        user_modules,
    )?;
    Ok(witchy_types::async_migration::census_linked(&linked, entry))
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

/// Parse and link exact source bytes while retaining authenticated package
/// owners, then mint the canonical checked-source witness used by SafeWIR.
pub fn link_checked_authenticated_from_source_bundle_with_user_modules(
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
    module_owners: AuthenticatedModuleOwners,
) -> Result<CheckedModule, PipelineError> {
    witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        sources,
        entry,
        crate::comptime::expand_compile_time,
        user_modules,
        module_owners,
    )
}

/// Build the compiler-owned static-evaluation recipe from an authenticated raw
/// source bundle.  Its fixed source rewrite happens before normal resolution,
/// checking, and fact sealing.
pub fn link_checked_static_evaluation_from_source_bundle(
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
    module_owners: AuthenticatedModuleOwners,
) -> Result<CheckedStaticEvaluationRecipe, PipelineError> {
    witchy_types::pipeline::link_checked_static_evaluation_from_source_bundle(
        sources,
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
    fn production_expander_retains_authenticated_loader_ownership() {
        let module = witchy_syntax::parser::parse_module(
            "import meta\n\
             \n\
             type User:\n\
             \x20 User(Int)\n\
             \n\
             pub comptime fn located(parts: List(String), holes: List(String), origin: String) -> meta.ExprSyntax:\n\
             \x20 meta.expr_string(origin)\n\
             \n\
             fn main() -> Int:\n\
             \x20 let value: String = located\"value\"\n\
             \x20 0\n",
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
        assert_eq!(
            catalog.resolve("main.located", DeclarationKind::Function),
            Some(
                &owner
                    .declaration(DeclarationKind::Function, "located")
                    .expect("tag function identity")
            )
        );
    }
}

/// Retain the exact source bundle and loader ownership for compiler-owned tests.
pub fn link_checked_test_suite_from_source_bundle(
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    user_modules: &std::collections::HashSet<String>,
    owners: AuthenticatedModuleOwners,
) -> Result<witchy_types::pipeline::CheckedTestSuite, PipelineError> {
    witchy_types::pipeline::link_checked_test_suite_from_source_bundle(
        sources, entry, crate::comptime::expand_compile_time, user_modules, owners,
    )
}

/// Select a declared bare test; the caller supplies neither a body nor witnesses.
pub fn check_bare_test_driver(
    suite: &witchy_types::pipeline::CheckedTestSuite,
    target: &str,
) -> Result<witchy_types::pipeline::CheckedTestDriverModule, PipelineError> {
    witchy_types::pipeline::check_bare_test_driver(
        suite, target, crate::comptime::expand_compile_time,
    )
}

/// Source-backed Console recipe; inert provider data cannot choose its entry.
pub fn check_console_fixture_test_driver(
    suite: &witchy_types::pipeline::CheckedTestSuite, target: &str,
) -> Result<witchy_types::pipeline::CheckedTestDriverModule, PipelineError> {
    witchy_types::pipeline::check_console_fixture_test_driver(
        suite, target, crate::comptime::expand_compile_time,
    )
}

/// Real-capability test recipe. Runtime grants and fixture providers cannot
/// supply source or entry authority; the target comes from retained entry bytes.
pub fn check_integration_test_driver(
    suite: &witchy_types::pipeline::CheckedTestSuite, target: &str,
) -> Result<witchy_types::pipeline::CheckedTestDriverModule, PipelineError> {
    witchy_types::pipeline::check_integration_test_driver(
        suite, target, crate::comptime::expand_compile_time,
    )
}
