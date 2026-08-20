//! Non-destructive checks over source-only syntax.
//!
//! Destructive source lowerings consume successive opaque typestates so their
//! call sites cannot erase source syntax before the rules that depend on those
//! nodes have run, or skip a stage in the lowering sequence.

use crate::ast::Module;

/// Source location retained while source-only syntax is still intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCheckLocation {
    pub module: Option<String>,
    pub line: u32,
}

/// A semantic diagnostic produced before destructive source lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCheckError {
    pub message: String,
    pub location: Option<SourceCheckLocation>,
}

impl SourceCheckError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            location: None,
        }
    }

    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        if let Some(location) = &mut self.location {
            location.module = Some(module.into());
        }
        self
    }

    pub fn with_location(mut self, module: impl Into<String>, line: u32) -> Self {
        self.location = Some(SourceCheckLocation {
            module: Some(module.into()),
            line,
        });
        self
    }

    pub fn link_location(&self) -> Option<crate::linker::LinkLocation> {
        let location = self.location.as_ref()?;
        Some(crate::linker::LinkLocation {
            module: location.module.clone()?,
            line: location.line,
        })
    }

    fn from_validation(module: &Module, error: SourceValidationError) -> Self {
        let line = module
            .item_lines
            .get(error.item_index)
            .copied()
            .filter(|line| *line != 0);
        Self {
            message: error.message,
            location: line.map(|line| SourceCheckLocation {
                module: None,
                line,
            }),
        }
    }
}

impl std::fmt::Display for SourceCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SourceCheckError {}

pub(crate) struct SourceValidationError {
    item_index: usize,
    message: String,
}

impl SourceValidationError {
    pub(crate) fn new(item_index: usize, message: String) -> Self {
        Self { item_index, message }
    }
}

/// A complete expanded link set whose imports, aliases, type identities, trait
/// identities, and method owners were resolved while all source-only nodes were
/// still present. Its fields are private so destructive lowering can require the
/// proof instead of accepting an arbitrary module vector.
#[derive(Debug)]
pub struct ResolvedSource {
    modules: Vec<(String, Module)>,
    declarations: crate::type_resolve::ResolvedDeclarations,
    method_owners: crate::type_resolve::MethodOwnerCandidates,
    entry: String,
    mode: crate::linker::LinkMode,
    user_modules: std::collections::HashSet<String>,
}

/// A resolved expanded source set whose injected semantic checker completed
/// successfully while generator, async, record, trait, and impl nodes were
/// still intact. Destructive lowering accepts this proof rather than bare
/// [`ResolvedSource`].
#[derive(Debug)]
pub(crate) struct SemanticallyCheckedSource {
    resolved: ResolvedSource,
}

impl ResolvedSource {
    pub fn modules(&self) -> &[(String, Module)] {
        &self.modules
    }

    pub fn method_owners(&self) -> &crate::type_resolve::MethodOwnerCandidates {
        &self.method_owners
    }

    /// Build the ordinary runtime projection on a clone while retaining this
    /// resolved source as the authoritative pre-lowering proof input. The
    /// legacy linker path deliberately installs no source callback, so this
    /// reuses the production name, trait, and lowering semantics without
    /// recursively constructing another semantic proof.
    pub fn runtime_projection(&self) -> Result<Module, crate::linker::LinkError> {
        fn no_expand(
            _: &str,
            _: &mut Module,
            _: &[(String, Module)],
        ) -> Result<crate::origin::OriginTable, String> {
            Ok(crate::origin::OriginTable::default())
        }

        fn restore_local_name(name: &mut String, prefix: &str) {
            if let Some(local) = name.strip_prefix(prefix).map(str::to_string) {
                *name = local;
            }
        }

        let mut modules = self.modules.clone();
        for (name, module) in &mut modules {
            let prefix = format!("{name}.");
            for item in &mut module.items {
                match item {
                    crate::ast::Item::Type(definition) => {
                        restore_local_name(&mut definition.name, &prefix);
                        for variant in &mut definition.variants {
                            restore_local_name(&mut variant.name, &prefix);
                        }
                    }
                    crate::ast::Item::Trait(definition) => {
                        restore_local_name(&mut definition.name, &prefix);
                    }
                    crate::ast::Item::TypeAlias { name, .. } => {
                        restore_local_name(name, &prefix);
                    }
                    crate::ast::Item::Impl(definition) => {
                        restore_local_name(&mut definition.type_name, &prefix);
                        if let Some(trait_name) = &mut definition.trait_name {
                            restore_local_name(trait_name, &prefix);
                        }
                    }
                    crate::ast::Item::Function(_)
                    | crate::ast::Item::Const { .. }
                    | crate::ast::Item::Comptime(_) => {}
                }
            }
            if !module.imports.contains(name) {
                module.imports.push(name.clone());
                module.import_lines.push(0);
            }
        }
        crate::linker::link_with_user_modules_with_mode(
            modules,
            &self.entry,
            no_expand,
            &self.user_modules,
            self.mode,
        )
    }

    pub(crate) fn after_semantic_check(self) -> SemanticallyCheckedSource {
        SemanticallyCheckedSource { resolved: self }
    }
}

impl SemanticallyCheckedSource {
    pub(crate) fn modules(&self) -> &[(String, Module)] {
        self.resolved.modules()
    }

    pub(crate) fn into_checked_modules(
        self,
    ) -> (Vec<(String, SourceCheckedModule)>, crate::type_resolve::ResolvedDeclarations) {
        let modules = self
            .resolved
            .modules
            .into_iter()
            .map(|(name, module)| (name, SourceCheckedModule { module }))
            .collect();
        (modules, self.resolved.declarations)
    }
}

/// An owned module whose source-only generator and async rules have been
/// checked without rewriting its AST.
#[derive(Debug)]
pub struct SourceCheckedModule {
    module: Module,
}

/// A source-checked module after generator lowering, but before async lowering.
#[derive(Debug)]
pub struct GeneratorsLoweredModule {
    module: Module,
}

/// A source-checked module after generator and async lowering, but before
/// record lowering completes the destructive source-lowering sequence.
#[derive(Debug)]
pub struct AsyncLoweredModule {
    module: Module,
}

/// A source-checked module after the destructive source-lowering sequence has
/// completed. Only this terminal stage exposes the runtime AST to downstream
/// crates.
#[derive(Debug)]
pub struct SourceLoweredModule {
    module: Module,
}

impl SourceCheckedModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub(crate) fn module_mut(&mut self) -> &mut Module {
        &mut self.module
    }

    pub(crate) fn into_module(self) -> Module {
        self.module
    }
}

impl GeneratorsLoweredModule {
    pub(crate) fn preserve(module: Module) -> Self {
        Self { module }
    }

    pub fn module(&self) -> &Module {
        &self.module
    }

    pub(crate) fn module_mut(&mut self) -> &mut Module {
        &mut self.module
    }

    pub(crate) fn into_module(self) -> Module {
        self.module
    }
}

impl AsyncLoweredModule {
    pub(crate) fn preserve(module: Module) -> Self {
        Self { module }
    }

    pub(crate) fn into_module(self) -> Module {
        self.module
    }
}

impl SourceLoweredModule {
    pub(crate) fn preserve(module: Module) -> Self {
        Self { module }
    }

    /// Finish the source-lowering sequence and recover the runtime AST.
    pub fn into_module(self) -> Module {
        self.module
    }
}

/// Check source-only semantics before any pass can erase their syntax.
pub fn check(module: Module) -> Result<SourceCheckedModule, SourceCheckError> {
    validate_source(&module)?;
    Ok(SourceCheckedModule { module })
}

/// Construct the linked-source proof after expansion and std discovery but
/// before any source module is destructively lowered.
pub(crate) fn resolve_linked_source(
    mut modules: Vec<(String, Module)>,
    user_modules: &std::collections::HashSet<String>,
    entry: &str,
    mode: crate::linker::LinkMode,
) -> Result<ResolvedSource, crate::linker::LinkError> {
    for (name, module) in &modules {
        if let Some(constant) = crate::consts::find_cycle(module) {
            return Err(crate::linker::LinkError {
                message: format!(
                    "module `{name}`: constant `{constant}` is defined cyclically"
                ),
                location: None,
            });
        }
        if let Some(alias) = crate::aliases::find_cycle(module) {
            return Err(crate::linker::LinkError {
                message: format!(
                    "module `{name}`: type alias `{alias}` is defined cyclically"
                ),
                location: None,
            });
        }
    }
    let namespace = crate::type_resolve::resolve_source_namespace(&mut modules, user_modules)?;
    for (name, module) in &modules {
        validate_source(module).map_err(|error| {
            let error = error.with_module(name);
            crate::linker::LinkError {
                message: format!("module `{name}`: expanded source: {}", error.message),
                location: error.link_location(),
            }
        })?;
    }
    Ok(ResolvedSource {
        modules,
        declarations: namespace.declarations,
        method_owners: namespace.method_owners,
        entry: entry.to_string(),
        mode,
        user_modules: user_modules.clone(),
    })
}

fn validate_source(module: &Module) -> Result<(), SourceCheckError> {
    crate::generators::validate_source(module)
        .map_err(|error| SourceCheckError::from_validation(module, error))?;
    crate::async_lower::validate_source(module)
        .map_err(|error| SourceCheckError::from_validation(module, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Module {
        crate::parser::parse_module(source).expect("parse source-check fixture")
    }

    #[test]
    fn generator_region_error_precedes_destructive_lowering() {
        let module = parse(
            "gen fn bad() -> Iter(Int):\n  region:\n    yield 1\n    0\n",
        );
        let error = check(module).expect_err("source check must reject yield in region");
        assert!(error.message.contains("cannot `yield` inside `region:`"), "{error}");
    }

    #[test]
    fn generator_return_contract_precedes_destructive_lowering() {
        let error = check(parse("gen fn bad() -> Int:\n  yield 1\n"))
            .expect_err("source check must reject a non-Iter generator result");
        assert_eq!(
            error.message,
            "generator `bad` must declare exactly one element type as `-> Iter(a)`"
        );
    }

    #[test]
    fn linked_source_rechecks_generated_generator_return_contract() {
        let error = resolve_linked_source(
            vec![("main".into(), parse("gen fn emitted() -> Int:\n  yield 1\n"))],
            &std::collections::HashSet::new(),
            "main",
            crate::linker::LinkMode::Production,
        )
        .expect_err("expanded source must re-enter the generator contract");
        assert_eq!(
            error.message,
            "module `main`: expanded source: generator `emitted` must declare exactly one element type as `-> Iter(a)`"
        );
        assert_eq!(
            error.location,
            Some(crate::linker::LinkLocation {
                module: "main".into(),
                line: 1,
            })
        );
    }

    #[test]
    fn async_tail_region_error_precedes_destructive_lowering() {
        let module = parse(
            "async fn bad() -> Int:\n  if true:\n    region:\n      1\n",
        );
        let error = check(module).expect_err("source check must reject async tail region");
        assert!(error.message.contains("`region:` in an async tail"), "{error}");
    }

    #[test]
    fn destructive_source_lowerers_require_the_proof_wrapper() {
        let source_check = include_str!("source_check.rs");
        let generators = include_str!("generators.rs");
        let async_lower = include_str!("async_lower.rs");
        let records = include_str!("records.rs");
        let linker = include_str!("linker.rs");
        assert!(generators.contains("pub fn lower(mut checked: SourceCheckedModule)"));
        assert!(generators.contains("Result<GeneratorsLoweredModule, String>"));
        assert!(async_lower.contains("pub fn lower(checked: GeneratorsLoweredModule)"));
        assert!(async_lower.contains("mut checked: GeneratorsLoweredModule"));
        assert!(async_lower.contains("Result<AsyncLoweredModule, String>"));
        assert!(records.contains("pub fn lower(module: AsyncLoweredModule)"));
        assert!(records.contains("pub fn lower_lenient(module: AsyncLoweredModule)"));
        assert!(linker.contains(
            "fn lower_expanded_source(\n    checked: crate::source_check::SemanticallyCheckedSource"
        ));
        assert!(linker.contains("let checked = resolved.after_semantic_check();"));
        assert!(source_check.contains("pub(crate) fn into_module(self) -> Module"));
        assert_eq!(
            source_check
                .matches("\n    pub fn into_module(self) -> Module")
                .count(),
            1
        );
    }

    #[test]
    fn linked_source_proof_retains_and_resolves_source_nodes() {
        let module = parse(
            "type Alias = Widget\n\n\
             trait Base:\n    fn base(self) -> Alias\n\n\
             trait Child: Base:\n    fn child(self, other: Alias) -> Alias\n\n\
             type Widget:\n    Widget(Int)\n\n\
             impl Base for Widget:\n    fn base(self) -> Alias:\n        self\n\n\
             impl Child for Widget:\n    fn child(self, other: Alias) -> Alias:\n        other\n\n\
             impl Widget:\n    pub fn inherent(self) -> Alias:\n        self\n\n\
             gen fn values(value: Alias) -> Iter(Alias):\n    yield value\n\n\
             async fn later(value: Alias) -> Alias:\n    value\n\n\
             fn bounded(value: a) -> a where a: Child:\n    value\n",
        );
        let source_item_count = module.items.len();
        let proof = resolve_linked_source(
            vec![("main".into(), module)],
            &std::collections::HashSet::new(),
            "main",
            crate::linker::LinkMode::Production,
        )
        .expect("linked source resolves");
        let resolved = &proof.modules()[0].1;

        assert_eq!(resolved.items.len(), source_item_count, "proof must retain every item");
        assert!(resolved.items.iter().any(|item| matches!(
            item,
            crate::ast::Item::TypeAlias { name, ty: crate::ast::Type::Named(target, _), .. }
                if name == "main.Alias" && target == "main.Widget"
        )));
        assert!(resolved.items.iter().any(|item| matches!(
            item,
            crate::ast::Item::Type(definition)
                if definition.name == "main.Widget"
                    && definition.variants[0].name == "main.Widget"
        )));
        assert!(resolved.items.iter().any(|item| matches!(
            item,
            crate::ast::Item::Function(function) if function.is_gen && function.name == "values"
        )));
        assert!(resolved.items.iter().any(|item| matches!(
            item,
            crate::ast::Item::Function(function) if function.is_async && function.name == "later"
        )));
        let child_trait = resolved.items.iter().find_map(|item| match item {
            crate::ast::Item::Trait(definition) if definition.name == "main.Child" => {
                Some(definition)
            }
            _ => None,
        }).expect("resolved Child trait");
        assert_eq!(child_trait.supertraits, ["main.Base"]);
        assert_eq!(
            child_trait.methods[0].params[1].ty,
            Some(crate::ast::Type::Named("main.Alias".into(), Vec::new()))
        );
        assert_eq!(
            child_trait.methods[0].ret,
            Some(crate::ast::Type::Named("main.Alias".into(), Vec::new()))
        );
        assert!(resolved.items.iter().any(|item| matches!(
            item,
            crate::ast::Item::Impl(definition)
                if definition.trait_name.as_deref() == Some("main.Child")
                    && definition.type_name == "main.Widget"
        )));
        assert!(resolved.items.iter().any(|item| matches!(
            item,
            crate::ast::Item::Function(function)
                if function.name == "bounded" && function.bounds[0].1 == "main.Child"
        )));

        let child = proof.method_owners().for_method("child");
        assert_eq!(child.len(), 1);
        assert_eq!(child[0].owner, "main.Child");
        assert_eq!(child[0].kind, crate::type_resolve::MethodOwnerKind::Trait);
        let inherent = proof.method_owners().for_method("inherent");
        assert_eq!(inherent.len(), 1);
        assert_eq!(inherent[0].owner, "main.Widget");
        assert_eq!(inherent[0].kind, crate::type_resolve::MethodOwnerKind::Inherent);
    }
}
