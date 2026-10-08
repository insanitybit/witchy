//! Non-destructive checks over source-only syntax.
//!
//! Destructive source lowerings consume successive opaque typestates so their
//! call sites cannot erase source syntax before the rules that depend on those
//! nodes have run, or skip a stage in the lowering sequence.

use crate::ast::Module;

/// Internal marker retained on functions produced through typed `emit_item`.
/// Source cannot forge it because the parser accepts only the public
/// declaration attributes. The reserved-name walk uses it to preserve
/// compiler-owned identifiers such as `meta.fresh` bindings when a resolved
/// source set is recursively projected for complete semantic checking.
pub const GENERATED_ITEM_ATTRIBUTE: &str = "__compiler_generated_item";

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
            location: line.map(|line| SourceCheckLocation { module: None, line }),
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
        Self {
            item_index,
            message,
        }
    }
}

/// A complete expanded link set whose imports, aliases, type identities, trait
/// identities, and method owners were resolved while all source-only nodes were
/// still present. Its fields are private so destructive lowering can require the
/// proof instead of accepting an arbitrary module vector.
#[derive(Debug)]
pub struct ResolvedSource {
    async_local_type_hints: std::cell::RefCell<Vec<crate::async_lower::type_hints::AsyncLocalTypeHint>>,
    modules: Vec<(String, Module)>,
    declarations: crate::type_resolve::ResolvedDeclarations,
    method_owners: crate::type_resolve::MethodOwnerCandidates,
    definition_site_type_imports: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    entry: String,
    mode: crate::linker::LinkMode,
    allow_ast_fold: bool,
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
    pub fn record_async_local_type_hints(&self, hints: Vec<crate::async_lower::type_hints::AsyncLocalTypeHint>) {
        *self.async_local_type_hints.borrow_mut() = hints;
    }

    // Only compiler-owned type markers validated by the source namespace pass
    // contribute these imports. Projection imports are internal: the original
    // source still has to pass its ordinary explicit-import checks first.
    fn add_definition_site_type_imports(&self, modules: &mut [(String, Module)]) {
        for (name, module) in modules {
            if let Some(authorized) = self.definition_site_type_imports.get(name) {
                for imported in authorized {
                    if !module.imports.contains(imported) {
                        module.imports.push(imported.clone());
                        module.import_lines.push(0);
                    }
                }
            }
        }
    }

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

        fn restore_local_type_names(ty: &mut crate::ast::Type, prefix: &str) {
            match ty {
                crate::ast::Type::Named(name, arguments)
                | crate::ast::Type::Dyn(name, arguments) => {
                    restore_local_name(name, prefix);
                    for argument in arguments {
                        restore_local_type_names(argument, prefix);
                    }
                }
                crate::ast::Type::Tuple(items) => {
                    for item in items {
                        restore_local_type_names(item, prefix);
                    }
                }
                crate::ast::Type::RecordCompose { base, fields } => {
                    restore_local_type_names(base, prefix);
                    for (_, field) in fields {
                        restore_local_type_names(field, prefix);
                    }
                }
                crate::ast::Type::Fn(inputs, output, _, _) => {
                    for input in inputs {
                        restore_local_type_names(input, prefix);
                    }
                    restore_local_type_names(output, prefix);
                }
                crate::ast::Type::Qualified(_, inner) => {
                    restore_local_type_names(inner, prefix);
                }
                crate::ast::Type::Slice(inner) => {
                    restore_local_type_names(inner, prefix);
                }
            }
        }

        let mut modules = self.modules.clone();
        self.add_definition_site_type_imports(&mut modules);
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
                    crate::ast::Item::TypeAlias { name, ty, .. } => {
                        restore_local_name(name, &prefix);
                        // The pre-expansion record projection runs before the
                        // recursive link resolves names again. Keep a local
                        // alias declaration and references inside its body in
                        // the same namespace during that projection.
                        restore_local_type_names(ty, &prefix);
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
        crate::linker::link_with_user_modules_with_mode_and_origins_and_source_check_stage_fold_policy(
            modules,
            &self.entry,
            crate::linker::ComptimeExpander::new(no_expand),
            false,
            &self.user_modules,
            self.mode,
            |_| Ok(()),
            crate::linker::AsyncLoweringStage::Runtime,
            self.allow_ast_fold,
        )
        .map(|linked| linked.module)
        .map_err(|error| match error {
            crate::linker::SourceLinkError::Link(error) => error,
            crate::linker::SourceLinkError::Source(error) => crate::linker::LinkError {
                location: error.link_location(),
                message: error.message,
            },
        })
    }

    /// Build the compiler-only linked projection that preserves `async` source
    /// syntax. It shares expansion, name resolution, method resolution, labels,
    /// folding, and record normalization with the runtime link, but deliberately
    /// stops before CPS lowering. Consumers must keep its type table with this
    /// exact AST; it is not an executable module.
    pub fn typed_async_projection(&self) -> Result<crate::linker::LinkedModule, crate::linker::LinkError> {
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

        fn restore_local_type_names(ty: &mut crate::ast::Type, prefix: &str) {
            match ty {
                crate::ast::Type::Named(name, arguments)
                | crate::ast::Type::Dyn(name, arguments) => {
                    restore_local_name(name, prefix);
                    for argument in arguments {
                        restore_local_type_names(argument, prefix);
                    }
                }
                crate::ast::Type::Tuple(items) => {
                    for item in items {
                        restore_local_type_names(item, prefix);
                    }
                }
                crate::ast::Type::RecordCompose { base, fields } => {
                    restore_local_type_names(base, prefix);
                    for (_, field) in fields {
                        restore_local_type_names(field, prefix);
                    }
                }
                crate::ast::Type::Fn(inputs, output, _, _) => {
                    for input in inputs {
                        restore_local_type_names(input, prefix);
                    }
                    restore_local_type_names(output, prefix);
                }
                crate::ast::Type::Qualified(_, inner) | crate::ast::Type::Slice(inner) => {
                    restore_local_type_names(inner, prefix);
                }
            }
        }

        let mut modules = self.modules.clone();
        self.add_definition_site_type_imports(&mut modules);
        // The parser represents `for await value in receiver` with this
        // compiler-private marker until async lowering consumes it. The typed
        // projection intentionally stops before that lowering, so give its
        // otherwise-unexecutable marker an identity-only declaration. It exists
        // solely in this projection to let ordinary name/type resolution retain
        // the source loop and its receiver type; it is never emitted into the
        // runtime module and cannot make the marker valid in non-async source.
        let mut recv_stream = crate::parser::parse_module(
            "fn __recv_stream(receiver: Receiver(value)) -> List(value):\n    []\n",
        )
        .expect("compiler-owned receive-stream projection helper parses")
        .items
        .remove(0);
        if let crate::ast::Item::Function(function) = &mut recv_stream {
            function.public = true;
            function.attributes.push(GENERATED_ITEM_ATTRIBUTE.into());
        }
        for (name, module) in &mut modules {
            if name == "chan" && !module.items.iter().any(|item| {
                matches!(item, crate::ast::Item::Function(function) if function.name == "__recv_stream")
            }) {
                module.items.push(recv_stream.clone());
                module.item_lines.push(0);
            }
            if !module.imports.contains(name) {
                module.imports.push(name.clone());
                module.import_lines.push(0);
            }
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
                    crate::ast::Item::TypeAlias { name, ty, .. } => {
                        restore_local_name(name, &prefix);
                        restore_local_type_names(ty, &prefix);
                    }
                    crate::ast::Item::Impl(definition) => {
                        restore_local_name(&mut definition.type_name, &prefix);
                        if let Some(trait_name) = &mut definition.trait_name {
                            restore_local_name(trait_name, &prefix);
                        }
                    }
                    crate::ast::Item::Function(_) | crate::ast::Item::Const { .. }
                    | crate::ast::Item::Comptime(_) => {}
                }
            }
        }
        crate::linker::link_with_user_modules_with_mode_and_origins_and_source_check_stage_fold_policy(
            modules,
            &self.entry,
            crate::linker::ComptimeExpander::new(no_expand),
            false,
            &self.user_modules,
            self.mode,
            |_| Ok(()),
            crate::linker::AsyncLoweringStage::TypedSource,
            self.allow_ast_fold,
        )
        .map_err(|error| match error {
            crate::linker::SourceLinkError::Link(error) => error,
            crate::linker::SourceLinkError::Source(error) => crate::linker::LinkError {
                location: error.link_location(),
                message: error.message,
            },
        })
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
    ) -> (
        Vec<(String, SourceCheckedModule)>,
        crate::type_resolve::ResolvedDeclarations,
    ) {
        let mut resolved_modules = self.resolved.modules;
        let hints = self.resolved.async_local_type_hints.into_inner();
        for (name, module) in &mut resolved_modules {
            crate::async_lower::type_hints::apply(name, module, &hints);
        }
        for (name, module) in &mut resolved_modules {
            if let Some(authorized) = self.resolved.definition_site_type_imports.get(name) {
                for imported in authorized {
                    if !module.imports.contains(imported) {
                        module.imports.push(imported.clone());
                        module.import_lines.push(0);
                    }
                }
            }
        }
        let modules = resolved_modules
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
    allow_ast_fold: bool,
) -> Result<ResolvedSource, crate::linker::LinkError> {
    for (name, module) in &modules {
        if let Some(constant) = crate::consts::find_cycle(module) {
            return Err(crate::linker::LinkError {
                message: format!("module `{name}`: constant `{constant}` is defined cyclically"),
                location: None,
            });
        }
        if let Some(alias) = crate::aliases::find_cycle(module) {
            return Err(crate::linker::LinkError {
                message: format!("module `{name}`: type alias `{alias}` is defined cyclically"),
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
        async_local_type_hints: std::cell::RefCell::new(Vec::new()),
        modules,
        declarations: namespace.declarations,
        method_owners: namespace.method_owners,
        definition_site_type_imports: namespace.definition_site_type_imports,
        entry: entry.to_string(),
        mode,
        allow_ast_fold,
        user_modules: user_modules.clone(),
    })
}

fn validate_source(module: &Module) -> Result<(), SourceCheckError> {
    crate::generators::validate_source(module)
        .map_err(|error| SourceCheckError::from_validation(module, error))?;
    crate::async_lower::validate_source(module)
        .map_err(|error| SourceCheckError::from_validation(module, error))
}
