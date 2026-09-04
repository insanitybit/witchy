//! The checked boundary between the front end and later compiler stages.
//!
//! Linking performs expansion and source name resolution, then invokes the
//! complete semantic checker over an internal runtime projection while the
//! authoritative expanded source remains intact. Only that proof can enter
//! destructive source lowering.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};

use std::fmt;

use witchy_syntax::ast::Module;
use witchy_syntax::linker::{
    self, ComptimeExpander, LinkError, LinkMode, LinkedModule, SourceCheckError,
    SourceLinkError,
};
use witchy_syntax::origin::OriginTable;
use witchy_syntax::type_resolve::ResolvedDeclarations;

use crate::runtime_type::{
    AuthenticatedModuleOwners, DeclarationKind, RuntimeDeclarationCatalog, RuntimeTypeError,
};
use crate::canonical_abi::{self, CanonicalCallableAbiManifest};
use crate::typeck::{self, ProperTailSiteManifest, TypeError};

/// The executable callable inventory produced by the canonical checked
/// frontend.  Generic templates deliberately have no entry: a direct call may
/// target only a concrete function whose body and signature were rechecked
/// after monomorphization.
#[derive(Debug, PartialEq)]
struct CanonicalCallableWitness {
    signatures: BTreeMap<String, witchy_syntax::ast::Type>,
    generic_instances: BTreeMap<String, crate::traits::LogicalSpecializationIdentity>,
    abi_manifest: CanonicalCallableAbiManifest,
}

/// Evidence that exact source bytes crossed the checked type-checking boundary.
///
/// The constructor is private so a caller cannot choose a digest and present
/// it as checked-source authority. The witness is minted only by the
/// source-bearing checked-link entry point below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedSourceWitness {
    source_bundle_digest: [u8; 32],
    proper_tail_manifest_digest: [u8; 32],
    callable_abi_manifest_digest: [u8; 32],
}

impl CheckedSourceWitness {
    pub fn source_bundle_digest(&self) -> [u8; 32] {
        self.source_bundle_digest
    }

    pub fn proper_tail_manifest_digest(&self) -> [u8; 32] {
        self.proper_tail_manifest_digest
    }

    pub fn callable_abi_manifest_digest(&self) -> [u8; 32] {
        self.callable_abi_manifest_digest
    }
}

/// The canonical source product consumed by SafeWIR lowering.
///
/// Its fields are private so the typed module, source witness, and proper-tail
/// manifest can only be paired by the canonical source-bearing frontend. The
/// AST-only checked-link APIs never mint this product.
#[derive(Debug, PartialEq)]
pub struct CanonicalCheckedSource {
    typed: typeck::TypedModule,
    source_witness: CheckedSourceWitness,
    proper_tail_manifest: ProperTailSiteManifest,
    callable_witness: CanonicalCallableWitness,
}

impl CanonicalCheckedSource {
    /// Borrow the exact typed module retained for canonical lowering.
    #[doc(hidden)]
    pub fn typed_module(&self) -> &typeck::TypedModule {
        &self.typed
    }

    /// Borrow the source identity minted for the same typed module.
    #[doc(hidden)]
    pub fn source_witness(&self) -> &CheckedSourceWitness {
        &self.source_witness
    }

    /// Borrow the proper-tail obligations derived from the same typed module.
    #[doc(hidden)]
    pub fn proper_tail_manifest(&self) -> &ProperTailSiteManifest {
        &self.proper_tail_manifest
    }

    /// Return the exact rechecked callable signature for a final executable
    /// declaration.  This is intentionally not a TypeTable name lookup: the
    /// map is sealed with the canonical typed module and excludes templates.
    #[doc(hidden)]
    pub fn callable_signature(&self, name: &str) -> Option<&witchy_syntax::ast::Type> {
        self.callable_witness.signatures.get(name)
    }

    /// Return the typechecker-owned specialization identity for a generated
    /// generic callable.  The identity is retained only as authority that the
    /// concrete body originated from monomorphization, never reconstructed by
    /// a backend from the callable spelling.
    #[doc(hidden)]
    pub fn generic_specialization(
        &self,
        name: &str,
    ) -> Option<&crate::traits::LogicalSpecializationIdentity> {
        self.callable_witness.generic_instances.get(name)
    }

    /// Borrow the source-derived structural callable ABI manifest.
    #[doc(hidden)]
    pub fn callable_abi_manifest(&self) -> &CanonicalCallableAbiManifest {
        &self.callable_witness.abi_manifest
    }
}

/// A linked module that has successfully passed the ordinary runtime checker.
///
/// The wrapped AST remains private so APIs that require checked input can use
/// this type as evidence instead of relying on caller discipline.
#[derive(Debug, PartialEq)]
pub struct CheckedModule {
    linked: LinkedModule,
    module_owners: Option<AuthenticatedModuleOwners>,
    typed_async_source: Option<typeck::TypedModule>,
    canonical_source: Option<CanonicalCheckedSource>,
}

/// A type-checked compiler rewrite that may be evaluated with the declaration
/// authority retained by its source module, but cannot enter production
/// lowering or code generation as a [`CheckedModule`].
#[derive(Debug)]
pub struct CheckedEvaluationModule {
    module: Module,
    runtime_catalog: RuntimeDeclarationCatalog,
}

/// A checked test executable derived from an already source-proved program by
/// replacing only its `main` function. The distinct proof prevents a generated
/// test driver from being passed to ordinary production sinks as a
/// [`CheckedModule`].
#[derive(Debug)]
pub struct CheckedTestDriverModule {
    module: Module,
    runtime_catalog: Option<RuntimeDeclarationCatalog>,
}

impl CheckedEvaluationModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn runtime_declaration_catalog(&self) -> &RuntimeDeclarationCatalog {
        &self.runtime_catalog
    }
}

impl CheckedTestDriverModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn runtime_declaration_catalog(&self) -> Option<&RuntimeDeclarationCatalog> {
        self.runtime_catalog.as_ref()
    }
}

impl CheckedModule {
    /// Borrow the opaque canonical source product, if this came from the
    /// exact-source frontend.
    #[doc(hidden)]
    pub fn canonical_source(&self) -> Option<&CanonicalCheckedSource> {
        self.canonical_source.as_ref()
    }

    /// Borrow the source witness minted by the exact-source checked pipeline.
    /// AST-only link APIs intentionally return `None` and cannot enter
    /// canonical production lowering.
    #[doc(hidden)]
    pub fn canonical_source_witness(&self) -> Option<&CheckedSourceWitness> {
        self.canonical_source
            .as_ref()
            .map(CanonicalCheckedSource::source_witness)
    }

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

    /// Borrow the lossless source async metadata retained by checked linking.
    ///
    /// It is deliberately not an executable plan: typed lowering must turn it
    /// into an RFC-0161 `AsyncProgram` with frame layouts, ownership
    /// transitions, and final backend bindings.
    #[doc(hidden)]
    pub fn async_surface(&self) -> &witchy_syntax::async_surface::LinkedAsyncSurface {
        self.linked.async_surface()
    }

    /// Build the structured source control-flow input for RFC-0161 typed
    /// lowering. This remains non-executable until final typed frame and
    /// closure-layout binding are available.
    #[doc(hidden)]
    pub fn source_async_control_flow(&self) -> crate::async_control_flow::SourceAsyncControlFlowInventory {
        crate::async_control_flow::SourceAsyncControlFlowInventory::from_surface(
            self.async_surface(),
        )
    }

    /// Borrow the fully linked, pre-CPS typed source projection for RFC-0161.
    /// Its type table belongs to this exact AST allocation; it is compiler-only
    /// evidence and cannot be evaluated or lowered as an executable module.
    #[doc(hidden)]
    pub fn typed_async_source(&self) -> Option<&typeck::TypedModule> {
        self.typed_async_source.as_ref()
    }

    /// Produce the compiler-owned, source-addressable input for RFC-0161
    /// executable lowering.  Keeping this operation on `CheckedModule`
    /// ensures that source operands and inferred types come from the same
    /// linked/type-checked allocation; callers cannot accidentally pair a
    /// detached AST with a stale typed table.  The returned value is still a
    /// lowering input, not an executable runtime program: frame lanes,
    /// ownership actions, and backend bindings are formed by the later
    /// executable-lowering stage.
    #[doc(hidden)]
    pub fn typed_async_lowering_input(
        &self,
    ) -> Result<crate::typed_async_lowering::TypedAsyncLoweringInput, String> {
        crate::typed_async_lowering::TypedAsyncLoweringInput::from_checked(self)
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
        let mut catalog = RuntimeDeclarationCatalog::from_resolved_declarations(
            &self.linked.declarations,
            module_owners,
        )?;
        for origin in self.linked.origins.tagged_literals() {
            let owner = module_owners
                .owner(&origin.definition.module)
                .ok_or_else(|| RuntimeTypeError::MissingModuleOwner {
                    module: origin.definition.module.clone(),
                })?;
            catalog.insert_resolved(
                &origin.tag,
                owner,
                &origin.name,
                DeclarationKind::Function,
            )?;
        }
        Ok(catalog)
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

/// Stable identity for the semantic stage that produced a pipeline failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineStage {
    Ownership,
    Link,
    Source,
    Type,
}

/// Source location retained by a semantic pipeline failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineLocation {
    pub module: Option<String>,
    pub line: u32,
}

impl PipelineError {
    pub const fn stage(&self) -> PipelineStage {
        match self {
            Self::Ownership(_) => PipelineStage::Ownership,
            Self::Link(_) => PipelineStage::Link,
            Self::Source(_) => PipelineStage::Source,
            Self::Type(_) => PipelineStage::Type,
        }
    }

    /// Return structured source provenance when the producing stage retained it.
    ///
    /// Type checking currently embeds its function/line context in the rendered
    /// message; migrating that final string-only location is tracked separately.
    pub fn location(&self) -> Option<PipelineLocation> {
        match self {
            Self::Link(error) => error.location.as_ref().map(|location| PipelineLocation {
                module: Some(location.module.clone()),
                line: location.line,
            }),
            Self::Source(error) => {
                error.location.as_ref().map(|location| PipelineLocation {
                    module: location.module.clone(),
                    line: location.line,
                })
            }
            Self::Ownership(_) | Self::Type(_) => None,
        }
    }
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

/// Check a compiler-generated test driver against its source-proved program.
///
/// Test execution replaces `main` after linking so each discovered test can be
/// called directly. Every other module field and item must remain identical to
/// the checked authority. The result is deliberately not a [`CheckedModule`],
/// so it can enter only the test-driver interpreter and codegen boundaries.
pub fn check_test_driver_module(
    authority: &CheckedModule,
    module: Module,
) -> Result<CheckedTestDriverModule, TypeError> {
    fn without_entry(mut module: Module) -> Module {
        module.items.retain(|item| {
            !matches!(item, witchy_syntax::ast::Item::Function(function) if function.name == "main")
        });
        // Linked modules normally have no parallel source-line vector. A test
        // driver is generated after linking and therefore owns no source line;
        // compare the semantic module shape rather than stale display metadata.
        module.item_lines.clear();
        module
    }

    let entries = module
        .items
        .iter()
        .filter_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) if function.name == "main" => {
                Some(function)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if entries.len() != 1
        || entries[0].is_gen
        || entries[0].is_async
        || entries[0].comptime_only
    {
        return Err(TypeError {
            message: "compiler test driver must define exactly one ordinary `main` function"
                .to_string(),
        });
    }
    if without_entry(module.clone()) != without_entry(authority.module().clone()) {
        return Err(TypeError {
            message: "compiler test driver may replace only the checked `main` function"
                .to_string(),
        });
    }
    typeck::check(&module)?;
    Ok(CheckedTestDriverModule {
        module,
        runtime_catalog: authority.runtime_declaration_catalog().ok(),
    })
}

/// Recheck a compiler-owned body rewrite for deterministic evaluation while
/// retaining the original module's authenticated runtime declaration catalog.
/// The distinct result type prevents the rewritten AST from reaching ordinary
/// production sinks.
pub fn check_compiler_evaluation_module(
    authority: &CheckedModule,
    module: Module,
) -> Result<CheckedEvaluationModule, PipelineError> {
    typeck::check(&module)?;
    Ok(CheckedEvaluationModule {
        module,
        runtime_catalog: authority.runtime_declaration_catalog()?,
    })
}

fn link_checked_with(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
    mode: LinkMode,
    user_modules: Option<&HashSet<String>>,
    capture_canonical_typed_source: bool,
    module_owners: Option<AuthenticatedModuleOwners>,
) -> Result<(CheckedModule, Option<typeck::TypedModule>), PipelineError> {
    let typed_async_source = RefCell::new(None);
    let canonical_typed_source = RefCell::new(None);
    let check_source = |source: &witchy_syntax::source_check::ResolvedSource| {
        let typed = typeck::check_linked_source_semantics_typed(source)?;
        if capture_canonical_typed_source {
            *canonical_typed_source.borrow_mut() = Some(typed);
        }
        let has_async = source.modules().iter().any(|(_, module)| module.items.iter().any(|item| match item {
            witchy_syntax::ast::Item::Function(function) => function.is_async,
            witchy_syntax::ast::Item::Impl(definition) => definition.methods.iter().any(|method| method.is_async),
            witchy_syntax::ast::Item::Type(_) | witchy_syntax::ast::Item::Trait(_)
            | witchy_syntax::ast::Item::Const { .. } | witchy_syntax::ast::Item::TypeAlias { .. }
            | witchy_syntax::ast::Item::Comptime(_) => false,
        }));
        if has_async {
            let projection = source.typed_async_projection().map_err(SourceLinkError::Link)?;
            let typed = typeck::annotate_typed_async_projection(projection.module)
                .map_err(|error| SourceLinkError::Source(SourceCheckError::new(error.message)))?;
            *typed_async_source.borrow_mut() = Some(typed);
        }
        Ok(())
    };
    let linked = match user_modules {
        Some(user_modules) => {
            linker::link_with_user_modules_with_mode_and_origins_and_source_check(
                modules,
                entry,
                expand,
                user_modules,
                mode,
                check_source,
            )
        }
        None => linker::link_with_mode_and_origins_and_source_check(
            modules,
            entry,
            expand,
            mode,
            check_source,
        ),
    }?;
    if let Some(module_owners) = &module_owners {
        module_owners.validate_module_names(linked.module_names.iter().cloned())?;
    }
    Ok((CheckedModule {
        linked,
        module_owners,
        typed_async_source: typed_async_source.into_inner(),
        canonical_source: None,
    }, canonical_typed_source.into_inner()))
}

fn checked_source_digest(sources: &[(String, Vec<u8>)]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"witchy.checked-source-bundle.v1");
    for (name, bytes) in sources {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    *hasher.finalize().as_bytes()
}

fn attach_canonical_source(
    mut checked: CheckedModule,
    typed: typeck::TypedModule,
    source_bundle_digest: [u8; 32],
    proper_tail_manifest: ProperTailSiteManifest,
    callable_witness: CanonicalCallableWitness,
) -> Result<CheckedModule, PipelineError> {
    checked.canonical_source = Some(CanonicalCheckedSource {
        typed,
        source_witness: CheckedSourceWitness {
            source_bundle_digest,
            proper_tail_manifest_digest: proper_tail_manifest.digest(),
            callable_abi_manifest_digest: callable_witness.abi_manifest.digest(),
        },
        proper_tail_manifest,
        callable_witness,
    });
    Ok(checked)
}

/// Turn the typechecker-owned monomorphization result into the exact canonical
/// lowering input.  The trait transform supplies identities, but its output is
/// never trusted directly: the transformed module is immediately checked and
/// the witness rejects any generated identity without a concrete checked body
/// and signature.
fn canonicalize_callable_instances(
    typed: typeck::TypedModule,
) -> Result<(typeck::TypedModule, CanonicalCallableWitness), PipelineError> {
    let (module, generic_instances) =
        crate::traits::lower_for_wasm_with_specializations(typed.into_module()).into_parts();
    let typed = typeck::annotate_checked(module)?;
    let callable_witness = bind_canonical_callable_witness(&typed, generic_instances)?;
    Ok((typed, callable_witness))
}

fn bind_canonical_callable_witness(
    typed: &typeck::TypedModule,
    generic_instances: BTreeMap<String, crate::traits::LogicalSpecializationIdentity>,
) -> Result<CanonicalCallableWitness, PipelineError> {
    let abi_manifest = canonical_abi::derive_manifest(typed).map_err(|error| {
        PipelineError::Type(TypeError { message: error.to_string() })
    })?;
    let functions = typed
        .module()
        .items
        .iter()
        .filter_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) => Some(function.name.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut signatures = BTreeMap::new();
    for name in &functions {
        if let Some(signature) = typed.table().function_type(name) {
            signatures.insert((*name).to_string(), signature);
        }
    }
    for name in generic_instances.keys() {
        if !functions.contains(name.as_str()) || !signatures.contains_key(name) {
            return Err(PipelineError::Type(TypeError {
                message: format!(
                    "canonical generic specialization `{name}` lacks an exact checked concrete body or signature"
                ),
            }));
        }
    }
    Ok(CanonicalCallableWitness {
        signatures,
        generic_instances,
        abi_manifest,
    })
}

/// Parse exact source bytes, type-check the resulting linked module, and mint
/// the only source witness accepted by canonical SafeWIR lowering.
///
/// The old AST-based APIs remain available for diagnostics and legacy
/// consumers, but deliberately do not mint this witness. The source bytes are
/// hashed before any AST lowering or formatting, so the identity is not based
/// on a debug rendering or a caller-supplied digest.
pub fn link_checked_from_source_bundle(
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
) -> Result<CheckedModule, PipelineError> {
    link_checked_source_bundle_with(sources, entry, expand, None, None)
}

fn link_checked_source_bundle_with(
    mut sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: Option<&HashSet<String>>,
    module_owners: Option<AuthenticatedModuleOwners>,
) -> Result<CheckedModule, PipelineError> {
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    let source_bundle_digest = checked_source_digest(&sources);
    let modules = sources
        .iter()
        .map(|(name, bytes)| {
            let source = std::str::from_utf8(bytes).map_err(|error| PipelineError::Link(LinkError {
                message: format!("source module `{name}` is not UTF-8: {error}"),
                location: None,
            }))?;
            let module = witchy_syntax::parser::parse_module(source).map_err(|error| {
                PipelineError::Link(LinkError {
                    message: format!("source module `{name}` failed to parse: {error}"),
                    location: None,
                })
            })?;
            Ok((name.clone(), module))
        })
        .collect::<Result<Vec<_>, PipelineError>>()?;
    let (checked, typed_source) = link_checked_with(
        modules,
        entry,
        expand,
        LinkMode::Production,
        user_modules,
        true,
        module_owners,
    )?;
    let typed_source = typed_source.ok_or_else(|| {
        PipelineError::Type(TypeError {
            message: "canonical checked link did not return its typed source".to_string(),
        })
    })?;
    let (typed_source, callable_witness) = canonicalize_callable_instances(typed_source)?;
    let proper_tail_manifest = typed_source
        .proper_tail_manifest()
        .map_err(|error| PipelineError::Type(TypeError { message: error.to_string() }))?;
    let proper_tail_manifest =
        proper_tail_manifest.for_canonical_functions(typed_source.module());
    attach_canonical_source(
        checked,
        typed_source,
        source_bundle_digest,
        proper_tail_manifest,
        callable_witness,
    )
}

/// Source-bearing production link with loader-authenticated package ownership.
/// The source bytes, ownership catalog, typed module, and tail obligations are
/// bound in one checked product; parsed AST inputs cannot mint this authority.
pub fn link_checked_authenticated_from_source_bundle_with_user_modules(
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
    module_owners: AuthenticatedModuleOwners,
) -> Result<CheckedModule, PipelineError> {
    link_checked_source_bundle_with(
        sources,
        entry,
        expand,
        Some(user_modules),
        Some(module_owners),
    )
}

/// Link with the injected compile-time expander, then type-check the result.
pub fn link_checked(
    modules: Vec<(String, Module)>,
    entry: &str,
    expand: ComptimeExpander,
) -> Result<CheckedModule, PipelineError> {
    link_checked_with(modules, entry, expand, LinkMode::Production, None, false, None)
        .map(|(checked, _)| checked)
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
        false,
        Some(module_owners),
    )
    .map(|(checked, _)| checked)
}

/// Link a test source graph under test syntax policy, then type-check it.
///
/// This is intentionally task-shaped rather than exposing `LinkMode` through
/// production front ends. Test-driver synthesis must recheck its entry-only
/// rewrite through [`check_test_driver_module`].
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
        false,
        None,
    )
    .map(|(checked, _)| checked)
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
        false,
        Some(module_owners),
    )
    .map(|(checked, _)| checked)
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
    fn checked_link_matches_the_legacy_lowered_projection() {
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
    fn canonical_source_binds_rechecked_generic_callable_instances() {
        let checked = link_checked_from_source_bundle(
            vec![(
                "main".to_string(),
                b"fn identity(value: a) -> a:\n    value\n\nfn main() -> Int:\n    identity(1)\n".to_vec(),
            )],
            "main",
            no_expand,
        )
        .expect("generic source checks");
        let canonical = checked.canonical_source().expect("canonical source");
        let instance = canonical
            .typed_module()
            .module()
            .items
            .iter()
            .filter_map(|item| match item {
                witchy_syntax::ast::Item::Function(function)
                    if function.name.contains("identity") && function.name != "main.identity" =>
                {
                    Some(function.name.as_str())
                }
                _ => None,
            })
            .next()
            .expect("checked concrete identity instance");
        assert!(canonical.generic_specialization(instance).is_some());
        assert!(canonical.callable_signature(instance).is_some());
        assert!(canonical.callable_signature("main.identity").is_none());
    }

    #[test]
    fn canonical_source_refuses_specialization_without_checked_body() {
        let module = parse(
            "fn identity(value: a) -> a:\n    value\n\nfn main() -> Int:\n    identity(1)\n",
        );
        let (module, mut instances) =
            crate::traits::lower_for_wasm_with_specializations(module).into_parts();
        let typed = typeck::annotate_checked(module).expect("lowered generic source rechecks");
        let identity = instances
            .values()
            .next()
            .expect("generic instance identity")
            .clone();
        instances.insert("forged_generic_instance".to_string(), identity);
        let error = bind_canonical_callable_witness(&typed, instances)
            .expect_err("a specialization without a checked body must refuse");
        assert!(error.to_string().contains("forged_generic_instance"));
    }

    #[test]
    fn canonical_source_witness_binds_callable_abi_manifest_digest() {
        let checked = link_checked_from_source_bundle(
            vec![(
                "main".to_string(),
                b"fn apply(value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    apply(1)\n".to_vec(),
            )],
            "main",
            no_expand,
        )
        .expect("source checks");
        let canonical = checked.canonical_source().expect("canonical source");
        assert_ne!(canonical.callable_abi_manifest().digest(), [0; 32]);
        assert_eq!(
            canonical.source_witness().callable_abi_manifest_digest(),
            canonical.callable_abi_manifest().digest()
        );
    }

    #[test]
    fn checked_module_retains_lossless_async_surface_metadata() {
        let checked = link_checked(
            vec![(
                "main".to_string(),
                parse(
                    "async fn increment(value: Int) -> Int:\n    let value = task.done(value).await\n    value + 1\n\nfn main(console: Console):\n    console.print(\"ready\")\n",
                ),
            )],
            "main",
            no_expand,
        )
        .expect("checked async source");

        let surface = checked.async_surface();
        let main = surface
            .modules
            .iter()
            .find(|module| module.module == "main")
            .expect("main source surface retained");
        assert_eq!(main.items.len(), 1);
        assert_eq!(surface.bindings.len(), 2, "entry and continuation are bound");
    }

    #[test]
    fn checked_module_preserves_mutually_exclusive_async_operation_paths() {
        let checked = link_checked(
            vec![(
                "main".to_string(),
                parse(
                    "async fn choose(flag: Bool) -> Int:\n    if flag:\n        task.done(1).await\n    else:\n        task.done(2).await\n\nfn main(console: Console):\n    console.print(\"ready\")\n",
                ),
            )],
            "main",
            no_expand,
        )
        .expect("checked branching async source");

        let inventory = checked.source_async_control_flow();
        let callable = inventory.callables.first().expect("async callable retained");
        let awaits = callable
            .operations
            .iter()
            .filter(|operation| {
                operation.kind == crate::async_control_flow::SourceAsyncOperationKind::Await
            })
            .collect::<Vec<_>>();
        assert_eq!(awaits.len(), 2);
        assert_eq!(
            awaits[0].path,
            vec![crate::async_control_flow::ControlPathStep::IfThen(
                crate::async_control_flow::SourceControlId(0),
            )],
        );
        assert_eq!(
            awaits[1].path,
            vec![crate::async_control_flow::ControlPathStep::IfElse(
                crate::async_control_flow::SourceControlId(0),
            )],
        );
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
    fn test_driver_proof_allows_only_checked_entry_replacement() {
        let checked = link_checked(
            vec![(
                "main".to_string(),
                parse("fn helper() -> Int:\n  1\n\nfn main() -> Int:\n  helper()\n"),
            )],
            "main",
            no_expand,
        )
        .expect("source-proved program");
        let mut driver = checked.module().clone();
        driver.items.retain(
            |item| !matches!(item, witchy_syntax::ast::Item::Function(function) if function.name == "main"),
        );
        driver
            .items
            .extend(parse("fn main():\n  Nil\n").items);
        check_test_driver_module(&checked, driver.clone())
            .expect("an entry-only test driver is admitted");

        let replacement = parse("fn helper() -> Int:\n  2\n");
        let replacement_body = match &replacement.items[0] {
            witchy_syntax::ast::Item::Function(function) => function.body.clone(),
            _ => unreachable!("fixture parses a function"),
        };
        let helper = driver
            .items
            .iter_mut()
            .find_map(|item| match item {
                witchy_syntax::ast::Item::Function(function)
                    if function.name.rsplit('.').next() == Some("helper") => Some(function),
                _ => None,
            })
            .expect("linked helper remains present");
        helper.body = replacement_body;
        let error = check_test_driver_module(&checked, driver)
            .expect_err("a generated driver cannot rewrite checked program bodies");
        assert_eq!(
            error.message,
            "compiler test driver may replace only the checked `main` function"
        );
    }

    #[test]
    fn checked_link_does_not_recheck_the_lowered_projection() {
        let source = include_str!("pipeline.rs");
        let checked_link = source
            .split_once("fn link_checked_with(")
            .expect("checked-link implementation exists")
            .1
            .split_once("/// Link with the injected compile-time expander")
            .expect("checked-link implementation has a bounded source section")
            .0;

        assert!(
            checked_link.contains("typeck::check_linked_source_semantics"),
            "checked linking must retain the pre-lowering semantic proof"
        );
        assert!(
            !checked_link.contains("typeck::check("),
            "checked linking must not repeat semantic checking after destructive lowering"
        );
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
    fn checked_link_proves_body_errors_before_production_lowering() {
        let modules = vec![(
            "main".to_string(),
            parse("fn main() -> Int:\n  \"not an int\"\n"),
        )];
        let linked = linker::link(modules.clone(), "main", no_expand).expect("legacy link");
        let old = typeck::check(&linked).expect_err("legacy check must reject the body");
        let new = link_checked(modules, "main", no_expand)
            .expect_err("checked link must reject the body");

        assert_eq!(
            new.to_string(),
            old.to_string()
                .strip_prefix("type error: ")
                .expect("legacy body error has its stage prefix")
        );
        assert_eq!(new.stage(), PipelineStage::Source);
        assert_eq!(new.location(), None);
    }

    #[test]
    fn checked_link_rejects_async_body_semantics_before_source_lowering() {
        let modules = vec![
            (
                "main".to_string(),
                parse("import helper\n\nfn main() -> Int:\n  0\n"),
            ),
            (
                "helper".to_string(),
                parse("async fn bad() -> Int:\n  \"not an int\"\n"),
            ),
        ];
        let error = link_checked(modules, "main", no_expand)
            .expect_err("an invalid async body must fail before source lowering");

        assert_eq!(error.stage(), PipelineStage::Source, "{error}");
        assert!(error.to_string().contains("expected `Int`"), "{error}");
    }

    #[test]
    fn comptime_emitted_body_reenters_semantic_proof_before_source_lowering() {
        fn emit_invalid_body(
            name: &str,
            module: &mut Module,
            _siblings: &[(String, Module)],
        ) -> Result<OriginTable, String> {
            if name == "main" {
                let mut emitted = parse("fn emitted() -> Int:\n  \"not an int\"\n");
                module.items.append(&mut emitted.items);
            }
            Ok(OriginTable::default())
        }

        let modules = vec![("main".to_string(), parse("fn main() -> Int:\n  0\n"))];
        let error = link_checked(modules, "main", emit_invalid_body)
            .expect_err("generated invalid body must re-enter semantic proof");

        assert_eq!(error.stage(), PipelineStage::Source, "{error}");
        assert!(
            error.to_string().contains("function `main.emitted` body"),
            "{error}"
        );
        assert!(error.to_string().contains("expected `Int`"), "{error}");
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
        let pipeline_error = PipelineError::Source(error);
        assert_eq!(pipeline_error.stage(), PipelineStage::Source);
        assert_eq!(
            pipeline_error.location(),
            Some(PipelineLocation {
                module: Some("helper".to_string()),
                line: 2,
            }),
        );
    }

    #[test]
    fn checked_link_rejects_resolved_signature_semantics_before_source_lowering() {
        let modules = vec![
            (
                "main".to_string(),
                parse("import helper\n\nfn main() -> Int:\n  0\n"),
            ),
            (
                "helper".to_string(),
                parse(
                    "type Box(a):\n  Box(a)\n\nasync fn bad(value: Box(Int, String)) -> Int:\n  0\n",
                ),
            ),
        ];
        let error = link_checked(modules, "main", no_expand)
            .expect_err("wrong resolved arity must fail before async lowering");

        assert_eq!(error.stage(), PipelineStage::Source, "{error}");
        assert!(error.to_string().contains("expects 1 type argument"), "{error}");
    }

    #[test]
    fn structured_metadata_preserves_link_and_ownership_stage() {
        let link = PipelineError::Link(LinkError {
            message: "bad import".into(),
            location: Some(witchy_syntax::linker::LinkLocation {
                module: "main".into(),
                line: 7,
            }),
        });
        assert_eq!(link.stage(), PipelineStage::Link);
        assert_eq!(
            link.location(),
            Some(PipelineLocation {
                module: Some("main".into()),
                line: 7,
            }),
        );

        let ownership =
            PipelineError::Ownership(RuntimeTypeError::MissingAuthenticatedModuleOwners);
        assert_eq!(ownership.stage(), PipelineStage::Ownership);
        assert_eq!(ownership.location(), None);
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
