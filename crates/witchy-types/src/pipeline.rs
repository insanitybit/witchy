//! The checked boundary between the front end and later compiler stages.
//!
//! This first RFC-0070 D6 seam deliberately preserves today's phase order:
//! linking (including injected compile-time expansion) completes before the
//! linked module is type-checked. Later slices can move work across this
//! boundary without making downstream callers reconstruct the sequence.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use witchy_syntax::ast::Module;
use witchy_syntax::linker::{self, ComptimeExpander, LinkError, LinkMode, LinkedModule};
use witchy_syntax::origin::OriginTable;
use witchy_syntax::type_resolve::ResolvedDeclarations;

use crate::runtime_type::{
    ModuleLoadIdentity, RuntimeDeclarationCatalog, RuntimeTypeError,
};
use crate::typeck::{self, TypeError};

/// A linked module that has successfully passed the ordinary runtime checker.
///
/// The wrapped AST remains private so APIs that require checked input can use
/// this type as evidence instead of relying on caller discipline.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedModule {
    linked: LinkedModule,
}

impl CheckedModule {
    /// Borrow the checked linked AST.
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

    /// Authenticate linked declarations against ownership supplied by the
    /// loader. This is the only checked-pipeline path to an RFC-0082 catalog;
    /// callers cannot recover package identity by parsing compiler names.
    pub fn runtime_declaration_catalog(
        &self,
        module_owners: &BTreeMap<String, ModuleLoadIdentity>,
    ) -> Result<RuntimeDeclarationCatalog, RuntimeTypeError> {
        RuntimeDeclarationCatalog::from_resolved_declarations(
            &self.linked.declarations,
            module_owners,
        )
    }

    /// Consume the proof wrapper and return the checked linked AST.
    pub fn into_module(self) -> Module {
        self.linked.module
    }

    /// Consume the proof wrapper while retaining generated-node origins.
    pub fn into_linked(self) -> LinkedModule {
        self.linked
    }
}

/// The stage at which linking and checking failed.
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineError {
    Link(LinkError),
    Type(TypeError),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Link(error) => fmt::Display::fmt(error, f),
            Self::Type(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for PipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Link(error) => Some(error),
            Self::Type(error) => Some(error),
        }
    }
}

impl From<LinkError> for PipelineError {
    fn from(error: LinkError) -> Self {
        Self::Link(error)
    }
}

impl From<TypeError> for PipelineError {
    fn from(error: TypeError) -> Self {
        Self::Type(error)
    }
}

fn check_linked(linked: LinkedModule) -> Result<CheckedModule, PipelineError> {
    typeck::check(&linked.module)?;
    Ok(CheckedModule { linked })
}

/// Link with the injected compile-time expander, then type-check the result.
pub fn link_checked(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
) -> Result<CheckedModule, PipelineError> {
    link_checked_with_mode(modules, entry, expand, LinkMode::Production)
}

/// Link in `mode` with the injected compile-time expander, then type-check.
pub fn link_checked_with_mode(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    mode: LinkMode,
) -> Result<CheckedModule, PipelineError> {
    let linked = linker::link_with_mode_and_origins(modules, entry, expand, mode)?;
    check_linked(linked)
}

/// Link with explicit user-module provenance, then type-check the result.
pub fn link_checked_with_user_modules(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
) -> Result<CheckedModule, PipelineError> {
    link_checked_with_user_modules_with_mode(
        modules,
        entry,
        expand,
        user_modules,
        LinkMode::Production,
    )
}

/// Link with explicit provenance and mode, then type-check the result.
pub fn link_checked_with_user_modules_with_mode(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
    mode: LinkMode,
) -> Result<CheckedModule, PipelineError> {
    let linked = linker::link_with_user_modules_with_mode_and_origins(
        modules,
        entry,
        expand,
        user_modules,
        mode,
    )?;
    check_linked(linked)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::runtime_type::{
        DeclarationKind, PackageCoordinate, PackageSource, RuntimeTypeError,
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
    fn checked_link_authenticates_its_retained_declarations() {
        let modules = vec![(
            "main".to_string(),
            parse("type User:\n  User(Int)\n\nfn main() -> Int:\n  1\n"),
        )];
        let checked = link_checked(modules, "main", no_expand).expect("checked link");
        let user_compiler_name = checked
            .declarations()
            .declarations
            .iter()
            .find(|declaration| {
                declaration.source_module == "main" && declaration.local_name == "User"
            })
            .expect("retained user declaration")
            .compiler_name
            .clone();

        let workspace = PackageCoordinate::new(
            PackageSource::Workspace,
            "example/app",
            "0.1.0",
        )
        .expect("package coordinate");
        let toolchain = PackageCoordinate::new(
            PackageSource::Toolchain,
            "witchy/stdlib",
            "0.1.0",
        )
        .expect("toolchain coordinate");
        let mut owners = BTreeMap::new();
        for declaration in &checked.declarations().declarations {
            let package = if declaration.source_module == "main" {
                workspace.clone()
            } else {
                toolchain.clone()
            };
            owners
                .entry(declaration.source_module.clone())
                .or_insert_with(|| {
                    ModuleLoadIdentity::new(
                        package,
                        [declaration.source_module.clone()],
                    )
                    .expect("module owner")
                });
        }
        let owner = owners.get("main").expect("main owner").clone();
        let catalog = checked
            .runtime_declaration_catalog(&owners)
            .expect("authenticated declarations");
        let expected = owner
            .declaration(DeclarationKind::Type, "User")
            .expect("declaration identity");

        assert_eq!(
            catalog.resolve(&user_compiler_name, DeclarationKind::Type),
            Some(&expected)
        );

        let mut missing_main = owners;
        missing_main.remove("main");
        let error = checked
            .runtime_declaration_catalog(&missing_main)
            .expect_err("missing loader ownership must fail closed");
        assert_eq!(
            error,
            RuntimeTypeError::MissingModuleOwner {
                module: "main".to_string(),
            }
        );
    }
}
