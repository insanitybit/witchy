//! The checked boundary between the front end and later compiler stages.
//!
//! Linking performs expansion and source name resolution, then invokes the
//! declaration-level checker before destructive source lowering. Complete body
//! checking remains on the linked runtime module during the migration.

use std::collections::HashSet;
use std::fmt;

use witchy_syntax::ast::Module;
use witchy_syntax::linker::{
    self, ComptimeExpander, LinkError, LinkMode, LinkedModule, SourceCheckError,
    SourceLinkError,
};
use witchy_syntax::origin::OriginTable;
use witchy_syntax::type_resolve::ResolvedDeclarations;

use crate::runtime_type::{
    AuthenticatedModuleOwners, RuntimeDeclarationCatalog, RuntimeTypeError,
};
use crate::typeck::{self, TypeError};

/// A linked module that has successfully passed the ordinary runtime checker.
///
/// The wrapped AST remains private so APIs that require checked input can use
/// this type as evidence instead of relying on caller discipline.
#[derive(Debug, PartialEq)]
pub struct CheckedModule {
    linked: LinkedModule,
    module_owners: Option<AuthenticatedModuleOwners>,
}

impl CheckedModule {
    /// Borrow the checked linked AST.
    ///
    /// This is not yet a capability-secure proof boundary because [`Module`]
    /// remains cloneable for compiler stages. Production sinks must continue
    /// to accept `CheckedModule` rather than a clone of this view.
    pub fn module(&self) -> &Module {
        &self.linked.module
    }

    /// Borrow generated-node origins retained by the link stage.
    pub fn origins(&self) -> &OriginTable {
        &self.linked.origins
    }

    /// Borrow the declaration provenance retained by linking.
    pub fn declarations(&self) -> &ResolvedDeclarations {
        &self.linked.declarations
    }

    /// Build a catalog from the loader ownership retained during checked link.
    /// Legacy checked-link APIs deliberately fail here rather than accepting
    /// package identity after the fact.
    pub fn runtime_declaration_catalog(
        &self,
    ) -> Result<RuntimeDeclarationCatalog, RuntimeTypeError> {
        let module_owners = self.module_owners.as_ref().ok_or(
            RuntimeTypeError::MissingAuthenticatedModuleOwners,
        )?;
        RuntimeDeclarationCatalog::from_resolved_declarations(
            &self.linked.declarations,
            module_owners,
        )
    }

}

/// The stage at which linking and checking failed.
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineError {
    Ownership(RuntimeTypeError),
    Link(LinkError),
    Source(SourceCheckError),
    Type(TypeError),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ownership(error) => fmt::Display::fmt(error, f),
            Self::Link(error) => fmt::Display::fmt(error, f),
            Self::Source(error) => fmt::Display::fmt(error, f),
            Self::Type(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for PipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Ownership(error) => Some(error),
            Self::Link(error) => Some(error),
            Self::Source(error) => Some(error),
            Self::Type(error) => Some(error),
        }
    }
}

impl From<LinkError> for PipelineError {
    fn from(error: LinkError) -> Self {
        Self::Link(error)
    }
}

impl From<RuntimeTypeError> for PipelineError {
    fn from(error: RuntimeTypeError) -> Self {
        Self::Ownership(error)
    }
}

impl From<TypeError> for PipelineError {
    fn from(error: TypeError) -> Self {
        Self::Type(error)
    }
}

impl From<SourceLinkError> for PipelineError {
    fn from(error: SourceLinkError) -> Self {
        match error {
            SourceLinkError::Link(error) => Self::Link(error),
            SourceLinkError::Source(error) => Self::Source(error),
        }
    }
}

/// Check a post-link compiler-synthesized runtime module.
///
/// This is the boundary for modules whose source provenance was intentionally
/// invalidated by compiler-owned AST construction, such as a generated test
/// driver. It reruns the ordinary runtime checker and deliberately carries no
/// source origins, declaration provenance, or authenticated module ownership.
pub fn check_synthetic_module(module: Module) -> Result<CheckedModule, TypeError> {
    typeck::check(&module)?;
    Ok(CheckedModule {
        linked: LinkedModule {
            module,
            origins: OriginTable::default(),
            module_names: Vec::new(),
            declarations: ResolvedDeclarations::default(),
        },
        module_owners: None,
    })
}

fn link_checked_with(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    mode: LinkMode,
    user_modules: Option<&HashSet<String>>,
    module_owners: Option<AuthenticatedModuleOwners>,
) -> Result<CheckedModule, PipelineError> {
    let linked = match user_modules {
        Some(user_modules) => {
            linker::link_with_user_modules_with_mode_and_origins_and_source_check(
                modules,
                entry,
                expand,
                user_modules,
                mode,
                typeck::check_linked_source_headers,
            )
        }
        None => linker::link_with_mode_and_origins_and_source_check(
            modules,
            entry,
            expand,
            mode,
            typeck::check_linked_source_headers,
        ),
    }?;
    if let Some(module_owners) = &module_owners {
        module_owners.validate_module_names(linked.module_names.iter().cloned())?;
    }
    typeck::check(&linked.module)?;
    Ok(CheckedModule { linked, module_owners })
}

/// Link with the injected compile-time expander, then type-check the result.
pub fn link_checked(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
) -> Result<CheckedModule, PipelineError> {
    link_checked_with(modules, entry, expand, LinkMode::Production, None, None)
}

/// Production-ready checked link with loader-authenticated package ownership.
///
/// `module_owners` must cover every linker module key. It is retained in the
/// returned proof object so descriptor catalogs cannot substitute an ad hoc
/// package map after linking or type checking.
pub fn link_checked_authenticated(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    module_owners: AuthenticatedModuleOwners,
) -> Result<CheckedModule, PipelineError> {
    link_checked_with(
        modules,
        entry,
        expand,
        LinkMode::Production,
        None,
        Some(module_owners),
    )
}

/// Link a test source graph under test syntax policy, then type-check it.
///
/// This is intentionally task-shaped rather than exposing `LinkMode` through
/// production front ends. Test-driver synthesis must recheck its transformed
/// clone through [`check_synthetic_module`].
pub fn link_checked_test_with_user_modules(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
) -> Result<CheckedModule, PipelineError> {
    link_checked_with(
        modules,
        entry,
        expand,
        LinkMode::Test,
        Some(user_modules),
        None,
    )
}

/// Production-ready checked link with both exact source provenance and
/// loader-authenticated package ownership.
pub fn link_checked_authenticated_with_user_modules(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
    module_owners: AuthenticatedModuleOwners,
) -> Result<CheckedModule, PipelineError> {
    link_checked_with(
        modules,
        entry,
        expand,
        LinkMode::Production,
        Some(user_modules),
        Some(module_owners),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::runtime_type::{
        DeclarationKind, ModuleLoadIdentity, PackageCoordinate, PackageSource,
        RuntimeTypeError,
    };

    static EXPANSIONS: AtomicUsize = AtomicUsize::new(0);

    fn no_expand(
        _name: &str,
        _module: &mut Module,
        _siblings: &[(String, Module)],
    ) -> Result<OriginTable, String> {
        Ok(OriginTable::default())
    }

    fn count_expand(
        _name: &str,
        _module: &mut Module,
        _siblings: &[(String, Module)],
    ) -> Result<OriginTable, String> {
        EXPANSIONS.fetch_add(1, Ordering::SeqCst);
        Ok(OriginTable::default())
    }

    fn parse(source: &str) -> Module {
        witchy_syntax::parser::parse_module(source).expect("parse test module")
    }

    #[test]
    fn checked_link_is_the_existing_link_then_check_sequence() {
        let modules = vec![("main".to_string(), parse("fn main() -> Int:\n  1\n"))];
        let old = linker::link_with_mode_and_origins(
            modules.clone(),
            "main",
            count_expand,
            LinkMode::Production,
        )
        .expect("legacy link");
        typeck::check(&old.module).expect("legacy check");

        let before = EXPANSIONS.load(Ordering::SeqCst);
        let checked = link_checked(modules, "main", count_expand).expect("checked link");

        assert_eq!(checked.module(), &old.module);
        assert_eq!(checked.origins(), &old.origins);
        assert_eq!(EXPANSIONS.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn checked_proof_has_no_consuming_ast_accessors() {
        let source = include_str!("pipeline.rs");
        let into_module = ["pub fn into_", "module(self) -> Module"].concat();
        let into_linked = ["pub fn into_", "linked(self) -> LinkedModule"].concat();
        assert!(!source.contains(&into_module));
        assert!(!source.contains(&into_linked));
    }

    #[test]
    fn checked_link_preserves_link_errors() {
        let modules = vec![("main".to_string(), parse("fn main() -> Int:\n  1\n"))];
        let old = linker::link(modules.clone(), "missing", no_expand)
            .expect_err("legacy link must reject a missing entry");
        let new = link_checked(modules, "missing", no_expand)
            .expect_err("checked link must reject a missing entry");

        assert_eq!(new, PipelineError::Link(old.clone()));
        assert_eq!(new.to_string(), old.to_string());
    }

    #[test]
    fn checked_link_preserves_type_errors() {
        let modules = vec![(
            "main".to_string(),
            parse("fn main() -> Int:\n  \"not an int\"\n"),
        )];
        let linked = linker::link(modules.clone(), "main", no_expand).expect("legacy link");
        let old = typeck::check(&linked).expect_err("legacy check must reject the body");
        let new = link_checked(modules, "main", no_expand)
            .expect_err("checked link must reject the body");

        assert_eq!(new, PipelineError::Type(old.clone()));
        assert_eq!(new.to_string(), old.to_string());
    }

    #[test]
    fn checked_link_rejects_generator_headers_at_the_source_boundary() {
        let modules = vec![
            (
                "main".to_string(),
                parse("import helper\n\nfn main() -> Int:\n  0\n"),
            ),
            (
                "helper".to_string(),
                parse(
                    "\ngen fn bad(value: Int, value: Int) -> Iter(Int):\n  yield value\n",
                ),
            ),
        ];
        let error = link_checked(modules, "main", no_expand)
            .expect_err("duplicate generator parameters must fail source checking");
        let PipelineError::Source(error) = error else {
            panic!("source header failure must remain a source error: {error}");
        };
        assert!(
            error.message.contains(
                "parameter `value` is declared more than once in function `bad`"
            ),
            "{}",
            error.message
        );
        assert_eq!(
            error.location,
            Some(witchy_syntax::source_check::SourceCheckLocation {
                module: Some("helper".to_string()),
                line: 2,
            }),
        );
    }

    #[test]
    fn authenticated_checked_link_retains_package_distinct_and_toolchain_owners() {
        let modules = vec![
            (
                "workspace_model".to_string(),
                parse("type User:\n  User(Int)\n"),
            ),
            (
                "registry_model".to_string(),
                parse("type User:\n  User(Int)\n"),
            ),
            (
                "std_model".to_string(),
                parse("type StdThing:\n  StdThing(Int)\n"),
            ),
            (
                "main".to_string(),
                parse(
                    "import workspace_model\nimport registry_model\nimport std_model\n\nfn main() -> Int:\n  1\n",
                ),
            ),
        ];
        let workspace_package = PackageCoordinate::new(
            PackageSource::Workspace,
            "acme/model",
            "1.0.0",
        )
        .expect("workspace coordinate");
        let registry_package = PackageCoordinate::new(
            PackageSource::Registry("https://packages.example".into()),
            "acme/model",
            "1.0.0",
        )
        .expect("registry coordinate");
        let toolchain_package = PackageCoordinate::new(
            PackageSource::Toolchain,
            "witchy/stdlib",
            "0.1.0",
        )
        .expect("toolchain coordinate");
        let workspace_owner = ModuleLoadIdentity::new(
            workspace_package,
            ["model"],
        )
        .expect("workspace owner");
        let registry_owner = ModuleLoadIdentity::new(
            registry_package,
            ["model"],
        )
        .expect("registry owner");
        let std_owner = ModuleLoadIdentity::new(
            toolchain_package.clone(),
            ["std", "model"],
        )
        .expect("toolchain std owner");
        let main_owner = ModuleLoadIdentity::new(
            PackageCoordinate::new(PackageSource::Workspace, "example/app", "0.1.0")
                .expect("application coordinate"),
            ["main"],
        )
        .expect("application owner");
        let mut assignments = vec![
            ("workspace_model".to_string(), workspace_owner.clone()),
            ("registry_model".to_string(), registry_owner.clone()),
            ("std_model".to_string(), std_owner.clone()),
            ("main".to_string(), main_owner),
        ];
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|module| {
            (
                (*module).to_string(),
                ModuleLoadIdentity::new(
                    toolchain_package.clone(),
                    ["std", *module],
                )
                .expect("toolchain std owner"),
            )
        }));
        let module_owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .expect("exact authenticated owner map");

        let checked = link_checked_authenticated(
            modules,
            "main",
            no_expand,
            module_owners,
        )
        .expect("authenticated checked link");
        let provenance = checked.declarations();
        assert!(provenance.declarations.iter().any(|declaration| {
            declaration.source_module == "workspace_model"
                && declaration.local_name == "User"
        }));
        assert!(provenance.declarations.iter().any(|declaration| {
            declaration.source_module == "registry_model"
                && declaration.local_name == "User"
        }));
        assert!(provenance.declarations.iter().any(|declaration| {
            declaration.source_module == "std_model"
                && declaration.local_name == "StdThing"
        }));

        let catalog = checked
            .runtime_declaration_catalog()
            .expect("catalog from retained owners");
        let workspace_user = catalog
            .resolve("workspace_model.User", DeclarationKind::Type)
            .expect("workspace User");
        let registry_user = catalog
            .resolve("registry_model.User", DeclarationKind::Type)
            .expect("registry User");
        let std_thing = catalog
            .resolve("std_model.StdThing", DeclarationKind::Type)
            .expect("toolchain std declaration");

        assert_ne!(workspace_user, registry_user);
        assert_eq!(workspace_user.package().source(), &PackageSource::Workspace);
        assert!(matches!(
            registry_user.package().source(),
            PackageSource::Registry(registry) if registry == "https://packages.example"
        ));
        assert_eq!(std_thing.package().source(), &PackageSource::Toolchain);
        assert_eq!(workspace_user, &workspace_owner.declaration(DeclarationKind::Type, "User").expect("workspace identity"));
        assert_eq!(registry_user, &registry_owner.declaration(DeclarationKind::Type, "User").expect("registry identity"));
        assert_eq!(std_thing, &std_owner.declaration(DeclarationKind::Type, "StdThing").expect("std identity"));
    }

    #[test]
    fn legacy_checked_link_cannot_add_ownership_at_catalog_use_site() {
        let modules = vec![(
            "main".to_string(),
            parse("type User:\n  User(Int)\n\nfn main() -> Int:\n  1\n"),
        )];
        let checked = link_checked(modules, "main", no_expand).expect("legacy checked link");
        let error = checked
            .runtime_declaration_catalog()
            .expect_err("legacy path has no authenticated loader ownership");
        assert_eq!(error, RuntimeTypeError::MissingAuthenticatedModuleOwners);
    }

    #[test]
    fn authenticated_link_requires_owners_for_function_only_pulled_modules() {
        let modules = vec![(
            "main".to_string(),
            parse("import math\n\nfn main() -> Int:\n  1\n"),
        )];
        let application = PackageCoordinate::new(
            PackageSource::Workspace,
            "example/app",
            "0.1.0",
        )
        .expect("application coordinate");
        let toolchain = PackageCoordinate::new(
            PackageSource::Toolchain,
            "witchy/stdlib",
            "0.1.0",
        )
        .expect("toolchain coordinate");
        let mut assignments = vec![(
            "main".to_string(),
            ModuleLoadIdentity::new(application, ["main"]).expect("main owner"),
        )];
        assignments.extend(
            witchy_syntax::linker::STD_MODULES
                .iter()
                .filter(|module| **module != "math")
                .map(|module| {
                    (
                        (*module).to_string(),
                        ModuleLoadIdentity::new(toolchain.clone(), ["std", *module])
                            .expect("toolchain owner"),
                    )
                }),
        );
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
            .expect("authenticated owners");

        let error = link_checked_authenticated(modules, "main", no_expand, owners)
            .expect_err("pulled math module has no authenticated owner");
        assert_eq!(
            error,
            PipelineError::Ownership(RuntimeTypeError::MissingModuleOwner {
                module: "math".into(),
            })
        );
    }
}
