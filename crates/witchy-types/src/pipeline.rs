//! The checked boundary between the front end and later compiler stages.
//!
//! Linking performs expansion and source name resolution, then invokes the
//! complete semantic checker over an internal runtime projection while the
//! authoritative expanded source remains intact. Only that proof can enter
//! destructive source lowering.

mod build_recipe;
pub use build_recipe::{BuildRecipeIdentity, CheckedBuildRecipe, check_build_recipe_from_source_bundle};

mod test_driver;
pub use test_driver::{CheckedTestSuite, TestDriverIdentity, check_bare_test_driver, check_console_fixture_test_driver, check_integration_test_driver, link_checked_test_suite_from_source_bundle};

mod static_evaluation;
pub use static_evaluation::{CheckedStaticEvaluationRecipe, link_checked_static_evaluation_from_source_bundle};

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use std::fmt;

use witchy_syntax::ast::{Module, Type};
use witchy_syntax::linker::{
    self, ComptimeExpander, LinkError, LinkMode, LinkedModule, SourceCheckError,
    SourceLinkError,
};
use witchy_syntax::origin::OriginTable;
use witchy_syntax::type_resolve::ResolvedDeclarations;
use witchy_caps::capabilities;

use crate::runtime_type::{
    AuthenticatedModuleOwners, DeclarationKind, RuntimeDeclarationCatalog, RuntimeTypeError,
    RuntimeTypePlan,
};
use crate::canonical_abi::{self, CanonicalCallableAbi, CanonicalCallableAbiManifest, CanonicalTargetPolicy};
use crate::canonical_closure::{ClosureWitnessError, ClosureWitnessManifest};
use crate::precompiled_declarations::PrecompiledDeclarationEnvironment;
use crate::optimization_readiness::OptimizationReadiness;
use crate::typeck::{self, ProperTailSiteManifest, TypeError};

/// The structural `()` spelling is canonical; legacy names remain accepted
/// while old source is migrated.
pub(super) fn is_unit_type(ty: &Type) -> bool {
    match ty.unqualified() {
        Type::Tuple(items) => items.is_empty(),
        Type::Named(name, arguments) => {
            arguments.is_empty() && matches!(name.as_str(), "Nil" | "Unit")
        }
        _ => false,
    }
}

/// The executable callable inventory produced by the canonical checked
/// frontend.  Generic templates deliberately have no entry: a direct call may
/// target only a concrete function whose body and signature were rechecked
/// after monomorphization.
#[derive(Debug, PartialEq)]
struct CanonicalCallableWitness {
    signatures: BTreeMap<String, witchy_syntax::ast::Type>,
    dictionary_comparators: Vec<(witchy_syntax::ast::Type, String)>,
    generic_instances: BTreeMap<String, crate::traits::LogicalSpecializationIdentity>,
    abi_manifest: CanonicalCallableAbiManifest,
    closed_abis: BTreeMap<String, CanonicalCallableAbi>,
    runtime_functions: Vec<String>,
    /// Obligations this witness could not bind as authority; see
    /// [`CanonicalObligation`].
    obligations: BTreeSet<CanonicalObligation>,
}

/// Something the frontend proved present in a valid checked program that
/// canonical v1 cannot bind as authority (RFC-0165).
///
/// An obligation is never dropped and never a type error: the program stays
/// checked, the set is sorted and digest-bound into the source witness, and
/// canonical lowering refuses a program whose set is non-empty, naming each
/// entry. Forged or corrupted witness entries are not obligations; they refuse
/// where they are detected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CanonicalObligation {
    /// A generated specialization outside the entry scope that could not be
    /// discarded because its exact checked body was not available. It is not
    /// a canonical runtime function and carries no signature or ABI here.
    Specialization(String),
    /// The canonical closure witness could not authenticate every lambda in
    /// the program; the payload is the witness's own diagnostic.
    ClosureWitness(String),
    /// The canonical callable ABI manifest could not encode every runtime
    /// declaration; the payload is the manifest derivation's diagnostic.
    CallableAbi(String),
}

impl CanonicalObligation {
    fn tag(&self) -> u8 {
        match self {
            Self::Specialization(_) => 0,
            Self::ClosureWitness(_) => 1,
            Self::CallableAbi(_) => 2,
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Specialization(detail)
            | Self::ClosureWitness(detail)
            | Self::CallableAbi(detail) => detail,
        }
    }
}

impl fmt::Display for CanonicalObligation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Specialization(name) => write!(f, "generic specialization `{name}`"),
            Self::ClosureWitness(detail) => write!(f, "closure witness: {detail}"),
            Self::CallableAbi(detail) => write!(f, "callable ABI: {detail}"),
        }
    }
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
    closure_witness_digest: [u8; 32],
    launch_contract_digest: [u8; 32],
    obligations_digest: [u8; 32],
    precompiled_declarations_digest: [u8; 32],
}

impl CheckedSourceWitness {
    /// Production uses the exact raw bundle identity. A compiler-owned test
    /// driver uses its domain-separated recipe-derived identity; that driver's
    /// TestDriverIdentity separately retains the original raw bundle identity.
    pub fn source_bundle_digest(&self) -> [u8; 32] {
        self.source_bundle_digest
    }

    /// Digest of the sorted [`CanonicalObligation`] set this checked source
    /// recorded. Bound into SafeWIR authority so the set cannot be edited
    /// between checked-source construction and canonical admission.
    pub fn obligations_digest(&self) -> [u8; 32] {
        self.obligations_digest
    }

    pub fn proper_tail_manifest_digest(&self) -> [u8; 32] {
        self.proper_tail_manifest_digest
    }

    pub fn callable_abi_manifest_digest(&self) -> [u8; 32] {
        self.callable_abi_manifest_digest
    }

    pub fn closure_witness_digest(&self) -> [u8; 32] {
        self.closure_witness_digest
    }

    pub fn launch_contract_digest(&self) -> [u8; 32] {
        self.launch_contract_digest
    }

    /// Digest of the authenticated external declaration environment joined to
    /// this exact source link. The empty environment has its own canonical
    /// digest; it is omitted only from the historical bundle-input hash for
    /// backward-compatible empty-source identities.
    pub fn precompiled_declarations_digest(&self) -> [u8; 32] {
        self.precompiled_declarations_digest
    }

    pub fn verifies_closure_witness(&self, manifest: &ClosureWitnessManifest) -> bool {
        self.closure_witness_digest == manifest.digest()
    }
}

/// The canonical source product consumed by SafeWIR lowering.
///
/// Its fields are private so the typed module, source witness, and proper-tail
/// manifest can only be paired by the canonical source-bearing frontend. The
/// AST-only checked-link APIs never mint this product.
#[derive(Debug, PartialEq)]
pub struct CanonicalCheckedSource {
    // Minted only by the source-backed compiler Console fixture recipe.
    console_fixture: bool,
    // Only a source-backed build recipe may select a non-main launch entry.
    build_entry: Option<String>,
    typed: typeck::TypedModule,
    optimization_readiness: OptimizationReadiness,
    dynamic_runtime_plan: Option<RuntimeTypePlan>,
    source_witness: CheckedSourceWitness,
    proper_tail_manifest: ProperTailSiteManifest,
    closure_witness: ClosureWitnessManifest,
    callable_witness: CanonicalCallableWitness,
    launch_contract_payload: Vec<u8>,
    obligations: BTreeSet<CanonicalObligation>,
    precompiled_declarations: PrecompiledDeclarationEnvironment,
}

impl CanonicalCheckedSource {
    pub fn is_console_fixture(&self) -> bool { self.console_fixture }

    pub fn entry_declaration(&self) -> &str { self.build_entry.as_deref().unwrap_or("main") }
    pub fn is_build_recipe(&self) -> bool { self.build_entry.is_some() }

    /// Dynamic metadata for this exact checked source and its authenticated
    /// declaration owners. This does not authorize dynamic AST expansion.
    #[doc(hidden)]
    pub fn dynamic_runtime_plan(&self) -> Option<&RuntimeTypePlan> {
        self.dynamic_runtime_plan.as_ref()
    }

    /// Borrow the exact typed module retained for canonical lowering.
    #[doc(hidden)]
    pub fn typed_module(&self) -> &typeck::TypedModule {
        &self.typed
    }

    /// Checked RFC-0167 facts available to a future optimizer. This carrier
    /// records effects, frozen access, and explicit discard barriers without
    /// enabling any optimization.
    #[doc(hidden)]
    pub fn optimization_readiness(&self) -> &OptimizationReadiness {
        &self.optimization_readiness
    }

    /// Borrow the exact loan graph checked for the retained canonical AST.
    /// Canonical consumers must use this product rather than re-walking the
    /// source or treating an absent graph as an empty one.
    #[doc(hidden)]
    pub fn loan_facts(&self) -> Result<&crate::loans::LoanFacts, typeck::CheckedLoanFactsError> {
        self.typed.loan_facts()
    }

    /// Borrow the exact access-signature and place graph checked for the
    /// retained canonical AST. Consumers must not rebuild this side product.
    #[doc(hidden)]
    pub fn access_facts(&self) -> Result<&crate::access::CheckedAccessFacts, typeck::CheckedAccessFactsError> {
        self.typed.access_facts()
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

    #[doc(hidden)]
    pub fn closure_witness(&self) -> &ClosureWitnessManifest {
        &self.closure_witness
    }

    /// Return the exact rechecked callable signature for a final executable
    /// declaration.  This is intentionally not a TypeTable name lookup: the
    /// map is sealed with the canonical typed module and excludes templates.
    #[doc(hidden)]
    pub fn callable_signature(&self, name: &str) -> Option<&witchy_syntax::ast::Type> {
        self.callable_witness.signatures.get(name)
    }

    /// Typed comparator roots derived from this source's retained trait
    /// declarations and included in its rechecked callable inventory.
    #[doc(hidden)]
    pub fn dictionary_key_comparators(&self) -> &[(witchy_syntax::ast::Type, String)] {
        &self.callable_witness.dictionary_comparators
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

    /// Return the source ABI for this exact concrete runtime declaration.
    #[doc(hidden)]
    pub fn closed_callable_abi(&self, name: &str) -> Option<&CanonicalCallableAbi> {
        self.callable_witness.closed_abis.get(name)
    }

    #[doc(hidden)]
    pub fn closed_callable_signature(&self, ordinal: u32) -> Option<&witchy_syntax::ast::Type> {
        let name = self.callable_witness.runtime_functions.get(ordinal as usize)?;
        self.callable_witness.signatures.get(name)
    }

    /// Return the exact retained source declaration for a closed callable.
    /// Unlike the checked callable signature, this preserves source loan
    /// spelling needed by independent ABI correspondence.
    #[doc(hidden)]
    pub fn closed_callable_declaration(
        &self,
        ordinal: u32,
    ) -> Option<&witchy_syntax::ast::Function> {
        let name = self.callable_witness.runtime_functions.get(ordinal as usize)?;
        self.typed.module().items.iter().find_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) if &function.name == name => {
                Some(function)
            }
            _ => None,
        })
    }

    /// Exact source-derived metadata that canonical admission must carry into
    /// the artifact before final SafeWIR validation.
    #[doc(hidden)]
    pub fn launch_contract_payload(&self) -> &[u8] {
        &self.launch_contract_payload
    }

    /// What this checked source could not bind as canonical authority.
    /// Canonical lowering must refuse a program whose set is non-empty;
    /// nothing else may consume these entries.
    #[doc(hidden)]
    pub fn obligations(&self) -> &BTreeSet<CanonicalObligation> {
        &self.obligations
    }

    /// The retained authenticated external declaration environment. Lowering
    /// may select an external Wasm target only through these exact records.
    #[doc(hidden)]
    pub fn precompiled_declarations(&self) -> &PrecompiledDeclarationEnvironment {
        &self.precompiled_declarations
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
    canonical_source: Option<CanonicalCheckedSource>,
    identity: Option<TestDriverIdentity>,
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
    /// Only the raw-source, compiler-owned driver recipe can mint this product.
    pub fn canonical_source(&self) -> Option<&CanonicalCheckedSource> {
        self.canonical_source.as_ref()
    }

    pub fn identity(&self) -> Option<&TestDriverIdentity> {
        self.identity.as_ref()
    }

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

    /// Seal one source-addressed typed async input to its exact canonical
    /// execution program.  Consumers must retain this checked product rather
    /// than reconnecting lexical captures or cancellation edges from CPS.
    #[doc(hidden)]
    pub fn checked_async_readiness(
        &self,
        input: &crate::typed_async_lowering::TypedAsyncLoweringInput,
        program: &crate::async_program::AsyncProgram,
    ) -> Result<crate::async_readiness::CheckedAsyncReadiness, crate::async_readiness::AsyncReadinessError> {
        crate::async_readiness::CheckedAsyncReadiness::new(self, input, program)
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
        canonical_source: None,
        identity: None,
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
    let linked = if capture_canonical_typed_source {
        match user_modules {
            Some(user_modules) => linker::link_canonical_with_user_modules_and_origins_and_source_check(
                modules,
                entry,
                expand,
                user_modules,
                mode,
                check_source,
            ),
            None => linker::link_canonical_with_origins_and_source_check(
                modules,
                entry,
                expand,
                mode,
                check_source,
            ),
        }
    } else {
        match user_modules {
            Some(user_modules) => linker::link_with_user_modules_with_mode_and_origins_and_source_check(
                modules, entry, expand, user_modules, mode, check_source,
            ),
            None => linker::link_with_mode_and_origins_and_source_check(
                modules, entry, expand, mode, check_source,
            ),
        }
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

fn checked_source_digest(
    sources: &[(String, Vec<u8>)],
    entry: &str,
    precompiled_declarations: &PrecompiledDeclarationEnvironment,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"witchy.checked-source-bundle.v2");
    // The loader's selected entry is semantic input too. Identical raw
    // bundles selected at different roots must not share source authority.
    hasher.update(&(entry.len() as u64).to_le_bytes());
    hasher.update(entry.as_bytes());
    for (name, bytes) in sources {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    // Preserve the historical empty-environment source identity byte-for-byte.
    // A nonempty environment is an additional authenticated source input, not
    // a loader option that later stages may silently ignore.
    if !precompiled_declarations.is_empty() {
        hasher.update(b"precompiled-declarations\0");
        hasher.update(&precompiled_declarations.digest());
    }
    *hasher.finalize().as_bytes()
}

fn launch_contract_digest(payload: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"witchy.checked-launch-contract.v1");
    hasher.update(&(payload.len() as u64).to_le_bytes());
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

fn obligations_digest(obligations: &BTreeSet<CanonicalObligation>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"witchy.checked-canonical-obligations.v1");
    hasher.update(&(obligations.len() as u64).to_le_bytes());
    for obligation in obligations {
        hasher.update(&[obligation.tag()]);
        let detail = obligation.detail();
        hasher.update(&(detail.len() as u64).to_le_bytes());
        hasher.update(detail.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

fn attach_canonical_source(
    mut checked: CheckedModule,
    typed: typeck::TypedModule,
    source_bundle_digest: [u8; 32],
    proper_tail_manifest: ProperTailSiteManifest,
    closure_witness: ClosureWitnessManifest,
    callable_witness: CanonicalCallableWitness,
    obligations: BTreeSet<CanonicalObligation>,
    dynamic_runtime_plan: Option<RuntimeTypePlan>,
    precompiled_declarations: PrecompiledDeclarationEnvironment,
) -> Result<CheckedModule, PipelineError> {
    let launch_contract_payload = capabilities::launch_contract_payload(checked.module());
    let launch_contract_digest = launch_contract_digest(&launch_contract_payload);
    let optimization_readiness = OptimizationReadiness::from_typed(&typed, source_bundle_digest)
        .map_err(|error| PipelineError::Type(TypeError { message: error.to_string() }))?;
    checked.canonical_source = Some(CanonicalCheckedSource {
        console_fixture: false,
        build_entry: None,
        typed,
        optimization_readiness,
        dynamic_runtime_plan,
        source_witness: CheckedSourceWitness {
            source_bundle_digest,
            proper_tail_manifest_digest: proper_tail_manifest.digest(),
            callable_abi_manifest_digest: callable_witness.abi_manifest.digest(),
            closure_witness_digest: closure_witness.digest(),
            launch_contract_digest,
            obligations_digest: obligations_digest(&obligations),
            precompiled_declarations_digest: precompiled_declarations.digest(),
        },
        proper_tail_manifest,
        closure_witness,
        callable_witness,
        launch_contract_payload,
        obligations,
        precompiled_declarations,
    });
    Ok(checked)
}

/// Turn the typechecker-owned monomorphization result into the exact canonical
/// lowering input.  The trait transform supplies identities, but its output is
/// never trusted directly: the transformed module is immediately checked.
/// Keep all specialization identities and preparation reservations until
/// authenticated existential adapters have joined the executable scope; the
/// final witness then binds the completed checked bodies and signatures.
fn canonicalize_callable_instances<'catalog>(
    typed: typeck::TypedModule,
    dictionary_catalog: Option<&'catalog crate::witness::WitnessCatalog>,
) -> Result<(typeck::TypedModule, crate::canonical_equality::PreparationSession<'catalog>), PipelineError> {
    // The linked semantic proof already prepares generators through
    // witchy_syntax::generators::lower: owned tuple frames and iter.unfold,
    // never replay. Consume that exact typed projection, not checked.module()
    // (the legacy source view), and never accept a detached helper inventory.
    // Preparation must precede specialization and the final recheck below so
    // callable, closure, and tail witnesses all describe the prepared bodies.
    if typed.module().items.iter().any(|item| match item {
        witchy_syntax::ast::Item::Function(function) => function.is_gen,
        witchy_syntax::ast::Item::Impl(definition) => {
            definition.methods.iter().any(|method| method.is_gen)
        }
        _ => false,
    }) {
        return Err(PipelineError::Type(TypeError {
            message: "canonical source projection contains an unprepared generator".into(),
        }));
    }
    let (module, generic_instances) =
        crate::traits::lower_for_wasm_with_specializations(typed.into_module()).into_parts();
    let typed = typeck::annotate_checked(module)?;
    let mut preparation = crate::canonical_equality::PreparationSession::new(
        &typed, dictionary_catalog, generic_instances,
    ).map_err(|message| PipelineError::Type(TypeError { message }))?;
    let typed = preparation.extend(typed)
        .map_err(|message| PipelineError::Type(TypeError { message }))?;
    Ok((typed, preparation))
}

/// Canonical message preparation can retain a private generic dependency that
/// was not executable before the message rewrite. Re-run the same checked
/// callable specialization over that rechecked source, then carry its exact
/// identities into the original reservation session. This is deliberately not
/// a second preparation session: helper-name reservations and prior identities
/// remain authoritative for the final closed preparation below.
fn specialize_message_prepared_callables<'catalog>(
    typed: typeck::TypedModule,
    preparation: &mut crate::canonical_equality::PreparationSession<'catalog>,
) -> Result<typeck::TypedModule, PipelineError> {
    let existing = typed.module().items.iter().filter_map(|item| match item {
        witchy_syntax::ast::Item::Function(function) => Some(function.name.clone()),
        _ => None,
    }).collect::<HashSet<_>>();
    let (mut module, instances) =
        crate::traits::lower_for_wasm_with_specializations(typed.into_module()).into_parts();
    let mut last_function_item = HashMap::new();
    for (index, item) in module.items.iter().enumerate() {
        let witchy_syntax::ast::Item::Function(function) = item else { continue };
        if last_function_item.insert(function.name.clone(), index).is_some() {
            if !existing.contains(&function.name)
                || !instances.contains_key(&function.name)
                || !preparation.has_specialization_instance(&function.name)
            {
                return Err(PipelineError::Type(TypeError {
                    message: format!(
                        "post-message specialization produced an unauthenticated duplicate callable `{}`",
                        function.name,
                    ),
                }));
            }
        }
    }
    // `lower_for_wasm_with_specializations` restarts its internal memo. Its
    // output therefore repeats a prior generated declaration before appending
    // the same logical specialization. Keep the final rechecked declaration,
    // whose name resolution matches the checked table, only after confirming
    // that the existing reservation and new logical identity are both present.
    module.items = module.items.into_iter().enumerate().filter(|(index, item)| match item {
        witchy_syntax::ast::Item::Function(function) => last_function_item[&function.name] == *index,
        _ => true,
    }).map(|(_, item)| item).collect();
    let typed = typeck::annotate_checked(module)?;
    preparation
        .merge_specialization_instances(instances)
        .map_err(|message| PipelineError::Type(TypeError { message }))?;
    Ok(typed)
}

/// Whether a declaration claims a checked source-owned namespace. This is
/// also used to reject forged specialization identities without bodies;
/// membership alone does not make an imported declaration an execution root.
///
/// The linker leaves the entry module's `main` unqualified and qualifies its
/// other declarations with the entry module name (`app.helper`); every other
/// module's declarations are qualified with their own name. An unlinked module
/// (tests, direct binder callers) has no entry and everything is in scope.
/// Actual executable membership is the shared `canonical_scope` judgment
/// below. A program's own `main` must never be admitted away (see
/// `BUG-canonical-entry-scope-drops-bare-main.md`).
pub fn is_canonical_entry_function(linked_entry: Option<&str>, name: &str) -> bool {
    linked_entry.is_none_or(|entry| {
        name == "main"
            || name.starts_with(&format!("{entry}."))
            || is_user_module_qualified(name)
    })
}

/// The canonical scope of a linked module: the entry scope (`main` and the
/// selected entry module's existing exported declarations) plus every function reachable
/// from it by name, transitively, through standard-library bodies and
/// monomorphized instances. Reachability is by exact declaration name after
/// monomorphization has renamed call sites to their instances, so a program
/// carries exactly the library code it references and nothing it does not; a
/// reachable body outside the compact subset still refuses precisely at
/// lowering. The scope is one judgment shared by the proper-tail manifest, the
/// callable witness, the layouts, and the lowering, so their ordinals agree.
pub fn canonical_scope(module: &Module) -> std::sync::Arc<BTreeSet<String>> {
    // No cache: a fingerprint over function names and statement counts once
    // served two different programs of the same shape the same scope (a
    // method call resolved to a std function in one and not the other), so
    // callers that need the scope for many lookups take the set once.
    let entry = module.linked_entry.as_deref();
    let functions = module
        .items
        .iter()
        .filter_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) => Some((function.name.as_str(), function)),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut scope = BTreeSet::new();
    let entry_prefix = entry.map(|entry| format!("{entry}."));
    let mut pending = functions
        .keys()
        // Imported declarations are dependencies, not additional entry roots.
        // Keep the established entry-module export surface (including private
        // compiler recipe entries), and keep unlinked/library checks closed
        // over all their declarations. Neither types nor lowering support
        // participate in this decision.
        .filter(|name| entry_prefix.as_ref().is_none_or(|prefix|
            **name == "main" || name.starts_with(prefix)))
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let seeds = pending.iter().cloned().collect::<BTreeSet<_>>();
    while let Some(name) = pending.pop() {
        let Some(function) = functions.get(name.as_str()) else { continue };
        // A generic template reached by name has no concrete body to lower;
        // only its monomorphized instances (which call sites name after
        // specialization) join the scope. The entry scope keeps its templates
        // as before, so its ordinals do not move.
        if !seeds.contains(&name) && function_has_free_signature_variables(function) {
            continue;
        }
        // A call named like a compiler intrinsic (`string.chars`, `list.push`)
        // resolves to the compiler, never to the standard-library wrapper of
        // the same name; that wrapper's body is not part of the program.
        if !seeds.contains(&name) && name == "vm.serve" { continue; }
        if !seeds.contains(&name)
            && matches!(
                witchy_syntax::intrinsics::lookup(&name),
                Some(spec)
                    if !matches!(
                        spec.lowering,
                        witchy_syntax::intrinsics::IntrinsicLowering::SourceFunction
                    )
            )
        {
            continue;
        }
        if !scope.insert(name.clone()) {
            continue;
        }
        let _: Result<(), ()> = witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            let callee = match expression {
                witchy_syntax::ast::Expr::Call { name, .. } | witchy_syntax::ast::Expr::Var(name) => Some(name),
                _ => None,
            };
            if let Some(callee) = callee
                && functions.contains_key(callee.as_str())
                && !scope.contains(callee)
            {
                pending.push(callee.clone());
            }
            Ok(())
        });
    }
    std::sync::Arc::new(scope)
}

/// A declaration qualified by a user library module (`strutil.shout`) is part
/// of the checked program: the linker qualifies every non-entry module's
/// declarations with that module's name, and only the standard library and
/// the playground modules are outside canonical scope by policy. Mangled
/// specializations and methods carry no `.` and never match here.
fn is_user_module_qualified(name: &str) -> bool {
    let Some((module, rest)) = name.split_once('.') else {
        return false;
    };
    // A mangled trait-impl or specialization (`Show__duration.DurationParseError__show`)
    // can carry a `.` inside a type segment; its `__` separators mark it as a
    // mangled name, never a module qualifier.
    !module.is_empty()
        && !rest.is_empty()
        && !module.contains("__")
        && !rest.contains("__")
        && !witchy_syntax::linker::STD_MODULES.contains(&module)
        && !witchy_syntax::linker::PLAYGROUND_MODULES.contains(&module)
}

fn bind_canonical_callable_witness(
    typed: &typeck::TypedModule,
    generic_instances: BTreeMap<String, crate::traits::LogicalSpecializationIdentity>,
) -> Result<CanonicalCallableWitness, PipelineError> {
    let linked_entry = typed.module().linked_entry.as_deref();
    let scope = canonical_scope(typed.module());
    let in_entry_scope = |name: &str| scope.contains(name);
    let checked_function_names = typed
        .module()
        .items
        .iter()
        .filter_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) => Some(function.name.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    // Monomorphization currently records a whole-module fixed point. Drop
    // only identities with an exact checked concrete body that canonical
    // reachability proves is outside the entry scope. Missing-body identities
    // remain below so forged or incomplete witness entries still refuse.
    let generic_instances = generic_instances
        .into_iter()
        .filter(|(name, _)| {
            !checked_function_names.contains(name.as_str()) || scope.contains(name)
        })
        .collect::<BTreeMap<_, _>>();
    let runtime_functions = typed
        .module()
        .items
        .iter()
        .filter_map(|item| match item {
            witchy_syntax::ast::Item::Function(function)
                if in_entry_scope(&function.name)
                    && !function_has_free_signature_variables(function) => {
                    Some(function.name.clone())
                }
            _ => None,
        })
        .collect::<Vec<_>>();
    let functions = runtime_functions.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut signatures = BTreeMap::new();
    for name in &runtime_functions {
        let signature = canonical_abi::checked_declaration_signature(typed, name).map_err(|error| {
            PipelineError::Type(TypeError { message: error.to_string() })
        })?;
        signatures.insert(name.clone(), signature);
    }
    // Generated instances split along the same entry scope. An instance inside
    // the scope is a direct call target: it must carry an exact rechecked body
    // and signature, and anything else is a forged or corrupted entry that
    // refuses here. An instance outside the scope with an exact checked body
    // was proven unreachable above and is omitted; an outside-scope identity
    // without that body remains a sorted, digest-bound obligation and fails at
    // canonical lowering instead of turning a valid checked program into a
    // type error.
    let mut obligations = BTreeSet::new();
    for name in generic_instances.keys() {
        if functions.contains(name.as_str()) && signatures.contains_key(name) {
            continue;
        }
        // The forged judgment stays name-based: an instance claiming the entry
        // scope's namespace without an exact checked body is a forgery, whether
        // or not anything reaches it.
        if in_entry_scope(name) || is_canonical_entry_function(linked_entry, name) {
            return Err(PipelineError::Type(TypeError {
                message: format!(
                    "canonical generic specialization `{name}` lacks an exact checked concrete body or signature"
                ),
            }));
        }
        obligations.insert(CanonicalObligation::Specialization(name.clone()));
    }
    // The manifest encodes every runtime declaration's ABI shape. A shape the
    // canonical encoding does not cover yet (a rights-qualified capability
    // type such as `Dir[Read]`, an unresolved nominal) is a v1 limit on a
    // program the checker accepted, so it is an obligation, and the witness
    // then binds no callable at all: an empty manifest and no closed ABIs,
    // so nothing downstream can consume a partial inventory as authority.
    let abi_manifest = match canonical_abi::derive_manifest(typed, &runtime_functions) {
        Ok(manifest) => manifest,
        Err(error) => {
            obligations.insert(CanonicalObligation::CallableAbi(error.to_string()));
            return Ok(CanonicalCallableWitness {
                signatures,
                dictionary_comparators: Vec::new(),
                generic_instances,
                abi_manifest: CanonicalCallableAbiManifest::empty(),
                closed_abis: BTreeMap::new(),
                runtime_functions,
                obligations,
            });
        }
    };
    let closed_abis = runtime_functions
        .iter()
        .enumerate()
        .map(|(ordinal, name)| {
            let abi = abi_manifest.entries().iter().find(|entry| {
                matches!(entry.target_policy, CanonicalTargetPolicy::ClosedLocal(target)
                    if target.function_ordinal == ordinal as u32)
            }).cloned().ok_or_else(|| PipelineError::Type(TypeError {
                message: format!("canonical runtime function `{name}` lacks a closed callable ABI"),
            }))?;
            Ok((name.clone(), abi))
        })
        .collect::<Result<BTreeMap<_, _>, PipelineError>>()?;
    Ok(CanonicalCallableWitness {
        signatures,
        dictionary_comparators: Vec::new(),
        generic_instances,
        abi_manifest,
        closed_abis,
        runtime_functions,
        obligations,
    })
}

/// Generic templates are compile-time inputs to monomorphization, not runtime
/// call targets. Concrete generated instances have no free variables and stay
/// in the manifest, where the checks below require their exact checked body and
/// signature.
pub(crate) fn function_has_free_signature_variables(function: &witchy_syntax::ast::Function) -> bool {
    let mut variables = Vec::new();
    for parameter in &function.params {
        if let Some(ty) = &parameter.ty {
            witchy_syntax::ast::collect_type_vars(ty, &mut variables);
        }
    }
    if let Some(result) = &function.ret {
        witchy_syntax::ast::collect_type_vars(result, &mut variables);
    }
    !variables.is_empty()
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
    link_checked_source_bundle_with(sources, entry, expand, None, None, None)
}

/// Link source with declarations recovered solely from authenticated
/// precompiled-library sidecars. The caller cannot pass a parallel source
/// signature: `environment` materializes the sealed scalar declarations and
/// is included in the checked-source witness before lowering may observe it.
pub fn link_checked_from_source_bundle_with_precompiled_declarations(
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    environment: PrecompiledDeclarationEnvironment,
) -> Result<CheckedModule, PipelineError> {
    link_checked_source_bundle_with(sources, entry, expand, None, None, Some(environment))
}

fn link_checked_source_bundle_with(
    mut sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: Option<&HashSet<String>>,
    module_owners: Option<AuthenticatedModuleOwners>,
    precompiled_declarations: Option<PrecompiledDeclarationEnvironment>,
) -> Result<CheckedModule, PipelineError> {
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    let environment = precompiled_declarations.unwrap_or_default();
    let source_bundle_digest = checked_source_digest(&sources, entry, &environment);
    let mut modules = sources
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
    modules.extend(environment.modules().map_err(|message| PipelineError::Link(LinkError {
        message,
        location: None,
    }))?);
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
    finish_canonical_source(checked, typed_source, source_bundle_digest, environment)
}

// Both production source and compiler-owned driver source arrive here only
// after complete source checking. Every witness is derived afresh below.
fn finish_canonical_source(
    checked: CheckedModule,
    typed_source: typeck::TypedModule,
    source_bundle_digest: [u8; 32],
    precompiled_declarations: PrecompiledDeclarationEnvironment,
) -> Result<CheckedModule, PipelineError> {
    // Semantic checking has already erased traits from its typed projection.
    // The same checked link retains the resolved pre-erasure declarations;
    // capture their witnesses here, never from the later canonical AST.
    let witness_catalog = crate::witness::WitnessCatalog::from_module(checked.module());
    let (mut typed_source, mut preparation) =
        canonicalize_callable_instances(typed_source, Some(&witness_catalog))?;
    // Only declarations actually removed by the following checked preparation
    // may lose their specialization metadata. An already missing body must
    // still reach the final binder's fail-closed identity checks.
    let before_preparation = typed_source.module().items.iter().filter_map(|item| match item {
        witchy_syntax::ast::Item::Function(function) => Some(function.name.clone()),
        _ => None,
    }).collect::<HashSet<_>>();
    // Ownership is taken only from this checked link, never from a catalog
    // supplied at the plan's use site or a cache keyed by source shape.
    let dynamic_runtime_plan = if checked.module_owners.is_some() {
        let runtime_catalog = checked.runtime_declaration_catalog()?;
        typed_source = crate::canonical_messages::prepare(typed_source, Some(&runtime_catalog))
            .map_err(|message| PipelineError::Type(TypeError { message }))?;
        let plan = crate::existential::build_dynamic_runtime_plan(
            &typed_source,
            &witness_catalog,
            &runtime_catalog,
        ).map_err(|message| PipelineError::Type(TypeError { message }))?;
        // The plan never leaves this checked-source construction route before
        // preparation. Generated bodies are rechecked before any final witness
        // is bound; original source bytes and loader ownership stay unchanged.
        typed_source = crate::existential::prepare_closed_dynamic(
            typed_source, &witness_catalog, &runtime_catalog, &plan,
        ).map_err(|message| PipelineError::Type(TypeError { message }))?;
        Some(plan)
    } else {
        typed_source = crate::canonical_messages::prepare(typed_source, None)
            .map_err(|message| PipelineError::Type(TypeError { message }))?;
        crate::existential::build_dynamic_runtime_plan_inner(
            &typed_source,
            &witness_catalog,
            None,
        ).map_err(|message| PipelineError::Type(TypeError { message }))?;
        None
    };

    typed_source = specialize_message_prepared_callables(typed_source, &mut preparation)?;

    let (prepared_traits, _) = crate::existential::prepare_closed_traits(
        typed_source, &witness_catalog,
    ).map_err(|message| PipelineError::Type(TypeError { message }))?;
    // Closed Dynamic/trait dispatch adds real calls to adapters authenticated
    // by the retained catalog. Their bodies were not necessarily executable
    // roots before that rewrite. Extend the SAME preparation sessions before
    // deriving any final witness; never restart prefix-collision reservations.
    typed_source = preparation.extend(prepared_traits)
        .map_err(|message| PipelineError::Type(TypeError { message }))?;
    let (mut instances, dictionary_comparators) = preparation.into_metadata();
    let retained = typed_source.module().items.iter().filter_map(|item| match item {
        witchy_syntax::ast::Item::Function(function) => Some(function.name.clone()),
        _ => None,
    }).collect::<HashSet<_>>();
    instances.retain(|name, _| !before_preparation.contains(name) || retained.contains(name));
    let mut callable_witness = bind_canonical_callable_witness(&typed_source, instances)?;
    callable_witness.dictionary_comparators = dictionary_comparators;

    let proper_tail_manifest = typed_source
        .proper_tail_manifest()
        .map_err(|error| PipelineError::Type(TypeError { message: error.to_string() }))?;
    let proper_tail_manifest =
        proper_tail_manifest.for_canonical_functions(typed_source.module(), &callable_witness.runtime_functions);
    let mut obligations = callable_witness.obligations.clone();
    // A closure the v1 witness cannot authenticate (a capture without an exact
    // checked type, a lambda the table did not type, a borrowed capture) is a
    // canonical limit on a checked program, so it is recorded as an obligation
    // and the manifest binds no lambda at all. An internal inconsistency of
    // the witness itself is not a limit and stays a type error.
    let closure_witness = match typed_source.canonical_closure_witness() {
        Ok(manifest) => manifest,
        Err(error) if error.is_canonical_obligation() => {
            obligations.insert(CanonicalObligation::ClosureWitness(error.to_string()));
            ClosureWitnessManifest::default()
        }
        Err(error) => {
            return Err(PipelineError::Type(TypeError { message: error.to_string() }));
        }
    }
    .bind_source_bundle_digest(source_bundle_digest);
    attach_canonical_source(
        checked,
        typed_source,
        source_bundle_digest,
        proper_tail_manifest,
        closure_witness,
        callable_witness,
        obligations,
        dynamic_runtime_plan,
        precompiled_declarations,
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
    link_checked_authenticated_from_source_bundle_with_precompiled_declarations(
        sources,
        entry,
        expand,
        user_modules,
        module_owners,
        PrecompiledDeclarationEnvironment::default(),
    )
}

/// Authenticated source-bearing link with sealed separately compiled scalar
/// declarations. The environment is already derived from verified package
/// artifacts; no import-site source declaration can join this route.
pub fn link_checked_authenticated_from_source_bundle_with_precompiled_declarations(
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
    module_owners: AuthenticatedModuleOwners,
    environment: PrecompiledDeclarationEnvironment,
) -> Result<CheckedModule, PipelineError> {
    link_checked_source_bundle_with(
        sources,
        entry,
        expand,
        Some(user_modules),
        Some(module_owners),
        Some(environment),
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

    #[test]
    fn canonical_source_excludes_ast_fold_while_legacy_link_keeps_it() {
        struct ResetOptimizationOverride;
        impl Drop for ResetOptimizationOverride {
            fn drop(&mut self) {
                witchy_syntax::opt::set_for_tests(None);
            }
        }

        fn result_expression(module: &witchy_syntax::ast::Module) -> &witchy_syntax::ast::Expr {
            let function = module.items.iter().find_map(|item| match item {
                witchy_syntax::ast::Item::Function(function)
                    if function.name == "main.main" || function.name == "main" => Some(function),
                _ => None,
            }).expect("linked main function");
            match function.body.stmts.last().expect("main result") {
                witchy_syntax::ast::Stmt::Expr(expression) => expression,
                statement => panic!("expected expression result, got {statement:?}"),
            }
        }

        let _reset = ResetOptimizationOverride;
        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
        let source = b"fn main() -> Int:\n    2 + 3\n";

        let canonical = link_checked_from_source_bundle(
            vec![("main".into(), source.to_vec())],
            "main",
            no_expand,
        ).expect("canonical source links without pre-SafeWIR folding");
        assert!(matches!(
            result_expression(canonical.module()),
            witchy_syntax::ast::Expr::Binary {
                op: witchy_syntax::ast::BinOp::Add,
                lhs,
                rhs,
            } if matches!(lhs.as_ref(), witchy_syntax::ast::Expr::Int(2))
                && matches!(rhs.as_ref(), witchy_syntax::ast::Expr::Int(3))
        ));

        let legacy = link_checked(
            vec![("main".into(), parse(std::str::from_utf8(source).unwrap()))],
            "main",
            no_expand,
        ).expect("legacy checked link retains configured folding");
        assert_eq!(result_expression(legacy.module()), &witchy_syntax::ast::Expr::Int(5));
    }

    #[test]
    fn canonical_test_driver_link_retains_test_mode() {
        let library = parse(
            "sealed type Version:\n    Version(Int)\n\npub fn value(version: Version) -> Int:\n    match version:\n        Version(value) -> value\n",
        );
        let test_driver = parse(
            "import sealed_lib\n\nfn test_construct() -> Int:\n    sealed_lib.value(sealed_lib.Version(7))\n\nfn main() -> Int:\n    test_construct()\n",
        );
        let user_modules = std::collections::HashSet::from([
            "sealed_lib".to_string(),
            "test_entry".to_string(),
        ]);

        let (_, typed) = link_checked_with(
            vec![("sealed_lib".into(), library), ("test_entry".into(), test_driver)],
            "test_entry",
            no_expand,
            LinkMode::Test,
            Some(&user_modules),
            true,
            None,
        ).expect("canonical test-driver linking retains entry test privileges");
        assert!(typed.is_some(), "canonical test-driver source remains captured");
    }

    #[test]
    fn canonical_comptime_expansion_suppresses_nested_ast_fold() {
        struct ResetOptimizationOverride;
        impl Drop for ResetOptimizationOverride {
            fn drop(&mut self) {
                witchy_syntax::opt::set_for_tests(None);
            }
        }

        fn nested_link_probe(
            name: &str,
            _: &mut witchy_syntax::ast::Module,
            _: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            if name == "main" {
                let nested = witchy_syntax::parser::parse_module(
                    "fn main() -> Int:\n    2 + 3\n",
                ).map_err(|error| error.to_string())?;
                let linked = witchy_syntax::linker::link(
                    vec![("comptime".into(), nested)],
                    "comptime",
                    no_expand,
                ).map_err(|error| error.message)?;
                let expression = linked.items.iter().find_map(|item| match item {
                    witchy_syntax::ast::Item::Function(function)
                        if function.name == "comptime.main" || function.name == "main" => {
                            function.body.stmts.last()
                        }
                    _ => None,
                }).and_then(|statement| match statement {
                    witchy_syntax::ast::Stmt::Expr(expression) => Some(expression),
                    _ => None,
                }).ok_or_else(|| "nested comptime result expression missing".to_string())?;
                if !matches!(expression, witchy_syntax::ast::Expr::Binary {
                    op: witchy_syntax::ast::BinOp::Add,
                    ..
                }) {
                    return Err(format!("canonical comptime nested link ran Fold: {expression:?}"));
                }
            }
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let _reset = ResetOptimizationOverride;
        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
        link_checked_from_source_bundle(
            vec![("main".into(), b"fn main() -> Int:\n    0\n".to_vec())],
            "main",
            nested_link_probe,
        ).expect("canonical expansion suppresses Fold in nested comptime links");
    }

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

    fn dynamic_source_owners(package: &str) -> AuthenticatedModuleOwners {
        let application = PackageCoordinate::new(PackageSource::Workspace, package, "0.1.0")
            .expect("application coordinate");
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0")
            .expect("toolchain coordinate");
        let mut assignments = vec![(
            "main".to_string(),
            ModuleLoadIdentity::new(application, ["main"]).expect("main owner"),
        )];
        assignments.extend(linker::STD_MODULES.iter().map(|module| (
            (*module).to_string(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *module]).expect("std owner"),
        )));
        AuthenticatedModuleOwners::from_loader_assignments(assignments).expect("owners")
    }

    fn dynamic_source(field: &str) -> Vec<(String, Vec<u8>)> {
        vec![("main".to_string(), format!(
            "import dynamic\n\ntype User:\n    value: {field}\n\nfn main() -> Int:\n    let _ = dynamic.runtime_type(User)\n    0\n"
        ).into_bytes())]
    }

    #[test]
    fn canonical_dynamic_preparation_rejects_empty_and_wrong_witness_catalogs() {
        use crate::witness::WitnessCatalog;
        use witchy_syntax::ast::{Item, Type};

        let source = r#"import dynamic
import reflect

trait Named:
    fn name(self) -> String

type Counter:
    value: Int

impl Named for Counter:
    fn name(self) -> String:
        "counter"

impl Reflect for Counter:
    fn reflect(self) -> reflect.Mirror:
        reflect.reflect_one(self.value)

fn main() -> Int:
    let value = dynamic.dynamic(Counter(4))
    let _ = dynamic.implements(value, dynamic.runtime_type(dyn Named))
    0
"#;
        let original = |source: &str| {
            let (checked, typed) = link_checked_with(
                vec![("main".into(), parse(source))], "main", no_expand,
                LinkMode::Production, None, true,
                Some(dynamic_source_owners("example/catalog-binding")),
            ).expect("authenticated original crosses the semantic source boundary");
            let (typed, _) = canonicalize_callable_instances(typed.unwrap(), None)
                .expect("original pre-preparation callable instances");
            (checked, typed)
        };
        let (checked, typed) = original(source);
        let catalog = WitnessCatalog::from_module(checked.module());
        let declarations = checked.runtime_declaration_catalog().unwrap();
        let plan = crate::existential::build_dynamic_runtime_plan(&typed, &catalog, &declarations)
            .expect("correct plan from the checked original");
        let counter = Type::Named("main.Counter".into(), vec![]);
        let named = Type::Dyn("main.Named".into(), vec![]);
        let counter_id = plan.id(&declarations.type_identity(&counter).unwrap()).unwrap();
        let named_id = plan.id(&declarations.type_identity(&named).unwrap()).unwrap();
        assert!(plan.trait_relations().contains(&(counter_id, named_id)));
        assert!(catalog.implements(&counter, &named).unwrap());

        // Each consuming preparation gets a fresh address-keyed table for an
        // unchanged clone of the same checked original, never another source.
        let fresh_original = || typeck::annotate_checked(typed.module().clone()).unwrap();
        let prepared = crate::existential::prepare_closed_dynamic(
            fresh_original(), &catalog, &declarations, &plan,
        ).expect("same-source catalog and authenticated plan still succeed");
        assert!(prepared.module().items.iter().any(|item| matches!(item,
            Item::Type(definition) if definition.name == "$canonical.DynamicPayload")));

        let empty_error = crate::existential::prepare_closed_dynamic(
            fresh_original(), &WitnessCatalog::default(), &declarations, &plan,
        ).expect_err("empty witness catalog must not prepare the checked original");
        assert_eq!(empty_error,
            "runtime trait query references unknown trait `main.Named`");

        // Keep the concrete payload and Reflect impl identical, but change the
        // nominal trait identity. The original plan must not retain a positive
        // Named relation while preparation accepts this OtherNamed catalog.
        let (other, _) = original(&source.replace("Named", "OtherNamed"));
        let wrong_catalog = WitnessCatalog::from_module(other.module());
        assert!(wrong_catalog.implements(&counter, &named).is_err());
        let error = crate::existential::prepare_closed_dynamic(
            fresh_original(), &wrong_catalog, &declarations, &plan,
        ).expect_err("a nonempty catalog with the wrong nominal trait identity must refuse");
        assert_eq!(error,
            "runtime trait query references unknown trait `main.Named`");

        let without_impl = source.replace(
            "impl Named for Counter:\n    fn name(self) -> String:\n        \"counter\"\n\n", "");
        let (other, _) = original(&without_impl);
        let missing_relation_catalog = WitnessCatalog::from_module(other.module());
        let error = crate::existential::prepare_closed_dynamic(
            fresh_original(), &missing_relation_catalog, &declarations, &plan,
        ).expect_err("matching descriptors cannot hide a missing impl relation");
        assert_eq!(error,
            "closed Dynamic runtime plan does not match its checked source and catalogs");
    }

    #[test]
    fn canonical_dynamic_runtime_plan_prepares_reflection_example_witnesses() {
        use crate::runtime_type::RuntimeTypeIdentity;
        use witchy_syntax::ast::{Item, Type};

        // The production comptime evaluator lives in witchy-interp, which
        // depends on this crate. Supply the exact Counter derive output from
        // std/meta.witchy's derive_reflect_source/dynamic_field_body at the
        // existing expander seam; this probe tests preparation, not evaluation.
        fn expand_counter_reflect(
            name: &str,
            module: &mut Module,
            _siblings: &[(String, Module)],
        ) -> Result<OriginTable, String> {
            if name == "main" {
                let mut generated = parse(r#"
impl Reflect for Counter:
    fn reflect(self) -> reflect.Mirror:
        reflect.MRecord("Counter", [("value", reflect.reflect_one(self.value))])

    fn __dynamic_field(self, __name: String) -> Option(reflect.DynamicFieldValue):
        if __name == "value":
            Some(reflect.DynamicFieldValue(__dynamic_descriptor_id(self), __dynamic_descriptor_id(self.value), self.value))
        else:
            None
"#);
                let Item::Impl(implementation) = &mut generated.items[0] else {
                    panic!("Counter derive fixture must emit an impl");
                };
                // Match the existing typed emit_item boundary's impl marker.
                implementation.origin = witchy_syntax::ast::ImplOrigin::CompilerGenerated;
                module.items.append(&mut generated.items);
                module.item_lines.append(&mut generated.item_lines);
            }
            Ok(OriginTable::default())
        }

        let checked = link_checked_authenticated_from_source_bundle_with_user_modules(
            vec![("main".to_string(), include_bytes!(
                "../../../examples/dynamic_reflection/src/dynamic_reflection.witchy"
            ).to_vec())],
            "main",
            expand_counter_reflect,
            &HashSet::from(["main".to_string()]),
            dynamic_source_owners("example/dynamic-reflection"),
        ).expect("authenticated reflection example preparation");
        let canonical = checked.canonical_source().expect("canonical checked source");
        let typed = canonical.typed_module();
        let plan = canonical.dynamic_runtime_plan().expect("retained dynamic plan");
        let catalog = checked.runtime_declaration_catalog().expect("authenticated declarations");
        let counter = Type::Named("main.Counter".into(), vec![]);
        let named = Type::Dyn("main.Named".into(), vec![]);
        let counter_identity = catalog.type_identity(&counter).expect("Counter identity");
        let named_identity = catalog.type_identity(&named).expect("Named identity");
        let counter_id = plan.id(&counter_identity).expect("Counter receiver descriptor");
        let named_id = plan.id(&named_identity).expect("dyn Named descriptor");
        assert_eq!(plan.get(counter_id).unwrap().identity, counter_identity);

        let methods = plan.methods().iter().filter(|method| method.function == "main.add")
            .collect::<Vec<_>>();
        assert_eq!(methods.len(), 1, "exactly one checked add method");
        let add = methods[0];
        assert_eq!(add.name, "add");
        assert_eq!(add.receiver, counter_id);
        assert_eq!(add.receiver_type, counter);
        assert_eq!(add.result, counter_id);
        let facts = crate::access::checked_facts(typed.module(), typed.table())
            .expect("access facts for the exact canonical typed module");
        let signature = facts.declaration("main.add").expect("checked add declaration");
        let callable = catalog.checked_callable_identity_with_authority(signature, typed.module())
            .expect("checked add callable identity");
        assert_eq!(add.callable_identity, callable);
        let RuntimeTypeIdentity::Function { access, .. } = &callable else {
            panic!("add must have a checked function identity");
        };
        assert_eq!(&add.access, access.as_ref());
        let callable_id = plan.id(&callable).expect("checked add callable descriptor");
        assert_eq!(plan.get(callable_id).unwrap().identity, callable);

        assert!(plan.trait_relations().contains(&(counter_id, named_id)),
            "Counter implements Named using the retained pre-erasure witness catalog");
        assert!(!typed.module().items.iter().any(|item| matches!(item, Item::Trait(_) | Item::Impl(_))),
            "the canonical AST cannot supply the erased Named impl");
        let payload = typed.module().items.iter().find_map(|item| match item {
            Item::Type(definition) if definition.name == "$canonical.DynamicPayload" => Some(definition),
            _ => None,
        }).expect("closed owned payload sum");
        assert!(payload.variants.iter().any(|variant| variant.fields == vec![counter.clone()]));
        assert!(payload.variants.iter().any(|variant| variant.fields == vec![Type::Named("Int".into(), vec![])]));
        for variant in &payload.variants {
            let identity = catalog.capability_free_type_identity(&variant.fields[0], typed.module())
                .expect("every payload is transitively capability-free");
            assert!(plan.id(&identity).is_some(), "payload identity belongs to the source plan");
        }
        witchy_syntax::ast::visit::visit_module_exprs::<String>(typed.module(), &mut |expr| {
            assert!(!matches!(expr, witchy_syntax::ast::Expr::ExistentialPack { .. }
                | witchy_syntax::ast::Expr::ExistentialCall { .. }
                | witchy_syntax::ast::Expr::ExistentialUpcast { .. }));
            if let witchy_syntax::ast::Expr::Call { name, .. } = expr {
                assert!(!name.starts_with("__dynamic"), "unprepared operation: {name}");
            }
            Ok(())
        }).expect("ordinary prepared AST");
        let tails = typed.proper_tail_manifest().expect("prepared tails")
            .for_canonical_functions(typed.module(), &canonical.callable_witness.runtime_functions);
        assert_eq!(&tails, canonical.proper_tail_manifest());
        assert_eq!(canonical.source_witness().source_bundle_digest(), checked_source_digest(&[
            ("main".into(), include_bytes!("../../../examples/dynamic_reflection/src/dynamic_reflection.witchy").to_vec()),
        ], "main", &PrecompiledDeclarationEnvironment::default()));
    }

    #[test]
    fn canonical_dynamic_runtime_plan_keeps_capability_arguments_out_of_payloads() {
        use witchy_syntax::ast::{Expr, Item, Type};
        let source = b"import dynamic\n\n@dynamic\npub fn announce(self: Int, console: Console) -> Int:\n    console.print(\"called\")\n    self\n\nfn main(console: Console):\n    let value = dynamic.dynamic(7)\n    let _ = dynamic.call_with(value, \"announce\", [], console)\n";
        let checked = link_checked_source_bundle_with(
            vec![("main".into(), source.to_vec())], "main", no_expand, None,
            Some(dynamic_source_owners("example/dynamic-capability")),
            None,
        ).expect("capability arguments remain statically checked values");
        let canonical = checked.canonical_source().unwrap();
        let typed = canonical.typed_module();
        let plan = canonical.dynamic_runtime_plan().unwrap();
        let method = plan.methods().iter().find(|method| method.function == "main.announce").unwrap();
        assert!(matches!(method.parameters.as_slice(),
            [crate::runtime_type::RuntimeMethodParameterDescriptor::Capability(capability)]
                if capability.ty == Type::Named("Console".into(), vec![])));
        let mut calls = 0;
        witchy_syntax::ast::visit::visit_module_exprs::<String>(typed.module(), &mut |expr| {
            if let Expr::Call { name, args } = expr && name == "main.announce" {
                calls += 1;
                assert_eq!(args.len(), 2);
                assert_eq!(typed.table().type_of(&args[1]).and_then(crate::typeck::ty_to_ast),
                    Some(Type::Named("Console".into(), vec![])), "capability is not boxed or encoded as Int");
            }
            Ok(())
        }).unwrap();
        assert!(calls > 0, "authenticated method branch is generated");
        let payload = typed.module().items.iter().find_map(|item| match item {
            Item::Type(definition) if definition.name == "$canonical.DynamicPayload" => Some(definition),
            _ => None,
        }).unwrap();
        assert!(payload.variants.iter().all(|variant|
            variant.fields == vec![Type::Named("Int".into(), vec![])]));

        for (declaration, receiver) in [("", "Console"), ("type Stash:\n    console: Console\n\n", "Stash")] {
            let source = format!("import dynamic\n\n{declaration}@dynamic\npub fn bad(self: {receiver}) -> Int:\n    0\n\nfn main() -> Int:\n    0\n");
            let error = link_checked_source_bundle_with(
                vec![("main".into(), source.into_bytes())], "main", no_expand, None,
                Some(dynamic_source_owners("example/forbidden-payload")),
                None,
            ).expect_err("direct and transitive capability payloads must refuse");
            assert!(error.to_string().contains("must be capability-free"), "{error}");
        }
    }

    #[test]
    fn canonical_dynamic_runtime_plan_authenticated_and_pure() {
        let checked = link_checked_source_bundle_with(
            dynamic_source("Int"), "main", no_expand, None,
            Some(dynamic_source_owners("example/first")),
            None,
        ).expect("authenticated dynamic source");
        let canonical = checked.canonical_source().expect("canonical source");
        let plan = canonical.dynamic_runtime_plan().expect("authenticated plan");
        let runtime_catalog = checked.runtime_declaration_catalog().expect("catalog");
        let identity = runtime_catalog.descriptor_type_identity(
            &witchy_syntax::ast::Type::Named("main.User".into(), vec![]),
            canonical.typed_module().module(),
        ).expect("authenticated User identity");
        assert!(plan.id(&identity).is_some());
        let before = canonical.typed_module().module().clone();
        let rebuilt = crate::existential::build_dynamic_runtime_plan(
            canonical.typed_module(), &crate::witness::WitnessCatalog::default(), &runtime_catalog,
        ).expect("pure plan rebuild");
        assert_eq!(plan, &rebuilt);
        assert_eq!(canonical.typed_module().module(), &before);
        let error = crate::existential::prepare_closed_dynamic(
            typeck::annotate_checked(before.clone()).unwrap(),
            &crate::witness::WitnessCatalog::default(), &runtime_catalog,
            &crate::runtime_type::RuntimeTypePlan::default(),
        ).expect_err("descriptor-only early return must not accept another plan");
        assert_eq!(error,
            "closed Dynamic runtime plan does not match its checked source and catalogs");
        let mut has_lowered_runtime_type = false;
        witchy_syntax::ast::visit::visit_module_exprs::<String>(&before, &mut |expr| {
            match expr {
                witchy_syntax::ast::Expr::Call { name, .. } => {
                    assert_ne!(name, witchy_syntax::intrinsics::DYNAMIC_RUNTIME_TYPE,
                        "canonical Dynamic preparation must consume compiler-owned type syntax");
                }
                witchy_syntax::ast::Expr::Ctor { name, .. } => {
                    has_lowered_runtime_type |= name == "dynamic.RuntimeType";
                }
                _ => {}
            }
            Ok(())
        }).expect("visit unchanged dynamic operation");
        assert!(has_lowered_runtime_type,
            "descriptor-only Dynamic operation must remain as a lowered runtime descriptor");
    }

    #[test]
    fn canonical_dynamic_plan_excludes_pruned_runtime_type_syntax() {
        use witchy_syntax::ast::Type;

        let application = PackageCoordinate::new(PackageSource::Workspace, "example/pruned-descriptor", "0.1.0")
            .expect("application coordinate");
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0")
            .expect("toolchain coordinate");
        let mut owners = vec![
            ("main".to_string(), ModuleLoadIdentity::new(application.clone(), ["main"]).expect("main owner")),
            ("library".to_string(), ModuleLoadIdentity::new(application, ["library"]).expect("library owner")),
        ];
        owners.extend(linker::STD_MODULES.iter().map(|module| (
            (*module).to_string(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *module]).expect("std owner"),
        )));
        let checked = link_checked_source_bundle_with(
            vec![
                ("main".into(), b"import dynamic\nimport library\n\nfn main() -> Int:\n    let _ = dynamic.runtime_type(Int)\n    0\n".to_vec()),
                ("library".into(), b"import dynamic\n\ntrait Latent:\n    fn descriptor() -> dynamic.RuntimeType:\n        dynamic.runtime_type(String)\n".to_vec()),
            ],
            "main",
            no_expand,
            None,
            Some(AuthenticatedModuleOwners::from_loader_assignments(owners).expect("owners")),
            None,
        ).expect("authenticated descriptor source");
        let canonical = checked.canonical_source().expect("canonical source");
        let plan = canonical.dynamic_runtime_plan().expect("authenticated plan");
        let catalog = checked.runtime_declaration_catalog().expect("catalog");
        let int = Type::Named("Int".into(), vec![]);
        let string = Type::Named("String".into(), vec![]);
        assert!(plan.id(&catalog.descriptor_type_identity(&int, canonical.typed_module().module()).unwrap()).is_some());
        assert!(plan.id(&catalog.descriptor_type_identity(&string, canonical.typed_module().module()).unwrap()).is_none(),
            "a pruned library descriptor request must not join the final Dynamic plan");
        let retained = canonical.typed_module().module().compiler_type_syntax.iter()
            .filter(|syntax| syntax.runtime_identity)
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1, "only the reachable compiler-owned request survives");
        assert_eq!(retained[0].ty, int);
    }

    #[test]
    fn canonical_dynamic_runtime_plan_rejects_missing_owner_and_separates_sources() {
        let error = link_checked_from_source_bundle(dynamic_source("Int"), "main", no_expand)
            .expect_err("dynamic source requires owners");
        assert!(error.to_string().contains("require authenticated runtime declaration ownership"), "{error}");
        let ordinary = link_checked_from_source_bundle(
            vec![("main".into(), b"fn main() -> Int:\n    0\n".to_vec())],
            "main", no_expand,
        ).expect("ordinary ownerless source");
        assert!(ordinary.canonical_source().unwrap().dynamic_runtime_plan().is_none());
        let check = |field, package| link_checked_source_bundle_with(
            dynamic_source(field), "main", no_expand, None, Some(dynamic_source_owners(package)), None,
        ).expect("authenticated source");
        let first = check("Int", "example/first");
        let other_owner = check("Int", "example/second");
        let other_source = check("Float", "example/first");
        let first = first.canonical_source().unwrap();
        let other_owner = other_owner.canonical_source().unwrap();
        let other_source = other_source.canonical_source().unwrap();
        assert_eq!(first.source_witness().source_bundle_digest(), other_owner.source_witness().source_bundle_digest());
        assert_ne!(first.dynamic_runtime_plan(), other_owner.dynamic_runtime_plan());
        assert_ne!(first.source_witness().source_bundle_digest(), other_source.source_witness().source_bundle_digest());
        assert_ne!(first.dynamic_runtime_plan(), other_source.dynamic_runtime_plan());
    }

    #[test]
    fn canonical_generator_preparation_binds_frames_helpers_and_source() {
        use witchy_syntax::ast::{Convention, Expr, Item, Type};

        let bytes = include_bytes!("../../../examples/generators/src/generators.witchy").to_vec();
        let sources = vec![("main".to_string(), bytes.clone())];
        let checked = link_checked_from_source_bundle(sources.clone(), "main", no_expand)
            .expect("existing fib/collatz generator source prepares");
        let canonical = checked.canonical_source().expect("sealed canonical product");
        let typed = canonical.typed_module();
        assert_eq!(canonical.source_witness().source_bundle_digest(), checked_source_digest(&sources, "main", &PrecompiledDeclarationEnvironment::default()));
        let functions = typed.module().items.iter().filter_map(|item| match item {
            Item::Function(function) => Some(function),
            _ => None,
        }).collect::<Vec<_>>();
        assert!(functions.iter().all(|function| !function.is_gen));
        for generator in ["fibs", "collatz"] {
            let helper_name = format!("main.__gen_{generator}");
            let helper = functions.iter().find(|function| function.name == helper_name)
                .expect("prepared resume helper has a checked body");
            assert_eq!(helper.params.len(), 1);
            assert_eq!(helper.params[0].convention, Convention::Own);
            let Some(Type::Tuple(fields)) = &helper.params[0].ty else {
                panic!("resume helper must consume an owned tuple frame");
            };
            let int = Type::Named("Int".into(), vec![]);
            let optional = |ty| Type::Named("Option".into(), vec![ty]);
            // Locals are absent until the first pull initializes them. The
            // Collatz seed yield also retains a one-shot Bool; neither may be
            // flattened into an unconditional Int slot.
            let expected = if generator == "fibs" {
                vec![optional(int.clone()), optional(int.clone()), int]
            } else {
                vec![int.clone(), optional(int.clone()),
                    optional(Type::Named("Bool".into(), vec![])), int]
            };
            assert_eq!(fields, &expected, "exact checked {generator} frame");
            assert_eq!(canonical.callable_signature(&helper_name),
                typed.table().function_type(&helper_name).as_ref());
            assert!(canonical.callable_signature(&helper_name).is_some());
            let wrapper = functions.iter().find(|function| function.name == format!("main.{generator}"))
                .expect("source generator retains its wrapper identity");
            let mut uses_helper = false;
            let mut uses_unfold = false;
            witchy_syntax::ast::visit::visit_block::<String>(&wrapper.body, &mut |expression| {
                match expression {
                    Expr::Var(name) => uses_helper |= name == &helper_name,
                    Expr::Call { name, .. } => {
                        uses_unfold |= name.starts_with("iter.unfold");
                        assert!(!name.contains("from_gen"), "generator preparation must not replay");
                    }
                    _ => {}
                }
                Ok(())
            }).expect("visit prepared wrapper");
            assert!(uses_helper && uses_unfold, "wrapper binds its actual helper through unfold");
        }
        let tails = typed.proper_tail_manifest().expect("prepared tail obligations")
            .for_canonical_functions(typed.module(), &canonical.callable_witness.runtime_functions);
        assert_eq!(&tails, canonical.proper_tail_manifest());
        let closures = typed.canonical_closure_witness().expect("prepared closure obligations")
            .bind_source_bundle_digest(checked_source_digest(&sources, "main", &PrecompiledDeclarationEnvironment::default()));
        assert!(!closures.entries().is_empty(), "unfold's generated callable closures are included");
        assert_eq!(&closures, canonical.closure_witness());
        assert_eq!(bind_canonical_callable_witness(typed, canonical.callable_witness.generic_instances.clone())
            .expect("rederive from the same prepared module"), canonical.callable_witness);

        // Identical prepared shapes do not permit substituting another source.
        let mut other_bytes = bytes;
        other_bytes.extend_from_slice(b"\n// different exact source bundle\n");
        let other = link_checked_from_source_bundle(vec![("main".into(), other_bytes)], "main", no_expand)
            .expect("same generator semantics, different source bytes");
        let other = other.canonical_source().unwrap();
        assert_ne!(canonical.source_witness().source_bundle_digest(), other.source_witness().source_bundle_digest());
        assert!(!canonical.source_witness().verifies_closure_witness(other.closure_witness()));
    }

    #[test]
    fn canonical_generator_preparation_rejects_invalid_semantics_and_replay_shapes() {
        for (source, diagnostic) in [
            ("gen fn bad() -> Iter(Int):\n    yield \"wrong\"\n\nfn main() -> Int:\n    0\n", "Int"),
            ("gen fn bad() -> Iter(Int):\n    while true:\n        while true:\n            yield 1\n\nfn main() -> Int:\n    0\n", "owned resumable frame"),
        ] {
            let error = link_checked_from_source_bundle(
                vec![("main".into(), source.as_bytes().to_vec())], "main", no_expand,
            ).expect_err("invalid generators cannot mint canonical source authority");
            assert!(error.to_string().contains(diagnostic), "{error}");
        }
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
    fn canonical_source_rechecks_nested_concrete_specializations_from_a_generic_local() {
        let checked = link_checked_from_source_bundle(
            vec![(
                "main".to_string(),
                b"trait Reflect:\n    fn reflect(self) -> Int\n\ntype Record(t0):\n    Record(t0)\n\nimpl Reflect for Record(t0):\n    fn reflect(self) -> Int:\n        1\n\nfn reflect_value(value: a) -> Int where a: Reflect:\n    value.reflect()\n\nfn into_value(value: a) -> Int where a: Reflect:\n    reflect_value(value)\n\nfn main() -> Int:\n    into_value(Record(7))\n".to_vec(),
            )],
            "main",
            no_expand,
        )
        .expect("nested concrete generic source checks");
        let canonical = checked.canonical_source().expect("canonical source");
        let functions = canonical
            .typed_module()
            .module()
            .items
            .iter()
            .filter_map(|item| match item {
                witchy_syntax::ast::Item::Function(function) => Some(&function.name),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(functions.iter().any(|name| name.contains("into_value__main_2eRecord")), "{functions:?}");
        assert!(functions.iter().any(|name| name.contains("reflect_value__main_2eRecord")), "{functions:?}");
        assert!(functions.iter().any(|name| name.contains("Reflect__main.Record__reflect__Int")), "{functions:?}");
        assert!(canonical.obligations().is_empty(), "{:?}", canonical.obligations());
    }

    #[test]
    fn canonical_source_omits_unreachable_checked_generic_instances() {
        let mut module = parse(
            "fn identity(value: a) -> a:\n    value\n\nfn main() -> Int:\n    identity(1)\n",
        );
        module.linked_entry = Some("main".to_string());
        let (mut module, mut instances) =
            crate::traits::lower_for_wasm_with_specializations(module).into_parts();
        let identity_name = module
            .items
            .iter()
            .find_map(|item| match item {
                witchy_syntax::ast::Item::Function(function) if function.name.starts_with("identity__") => Some(function.name.clone()),
                _ => None,
            })
            .expect("monomorphization produced the checked identity body");
        let identity_body = module
            .items
            .iter()
            .find_map(|item| match item {
                witchy_syntax::ast::Item::Function(function) if function.name == identity_name => Some(function.clone()),
                _ => None,
            })
            .expect("exact identity body");
        let identity = instances
            .get(&identity_name)
            .expect("identity specialization identity")
            .clone();
        let mut unreachable_body = identity_body;
        unreachable_body.name = "util.unreachable__Int".to_string();
        module.items.push(witchy_syntax::ast::Item::Function(unreachable_body));
        let typed = typeck::annotate_checked(module).expect("rechecked exact unreachable body");
        instances.insert("util.unreachable__Int".to_string(), identity);
        let witness = bind_canonical_callable_witness(&typed, instances)
            .expect("exact checked body outside canonical scope is safely omitted");
        assert!(!witness.runtime_functions.iter().any(|name| name == "util.unreachable__Int"));
        assert!(!witness.generic_instances.contains_key("util.unreachable__Int"));
        assert!(!witness.obligations.iter().any(|obligation| {
            matches!(obligation, CanonicalObligation::Specialization(name) if name == "util.unreachable__Int")
        }));
        assert!(witness.runtime_functions.iter().any(|name| name == &identity_name));
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

    /// A valid checked program that instantiates a library generic at a
    /// program type is outside canonical v1, but that is a lowering refusal,
    /// not a type error. Checked-source construction succeeds and records the
    /// specialization as a digest-bound obligation.
    #[test]
    fn checked_source_records_library_specializations_as_obligations() {
        let checked = link_checked_from_source_bundle(
            vec![
                (
                    "main".to_string(),
                    b"import util\n\nfn main() -> Int:\n    util.identity(1)\n".to_vec(),
                ),
                ("util".to_string(), b"pub fn identity(value: a) -> a:\n    value\n".to_vec()),
            ],
            "main",
            no_expand,
        )
        .expect("a valid program outside the canonical subset still checks");
        let canonical = checked.canonical_source().expect("canonical source");
        // The library instance is reachable from `main`, so it is a runtime
        // function of the canonical scope, not an obligation; the obligation
        // set is empty and its digest is still bound into the witness.
        let obligations = canonical.obligations();
        assert!(obligations.is_empty(), "reachable library instances lower, got {obligations:?}");
        assert!(
            canonical.callable_abi_manifest().entries().len() >= 2,
            "the manifest covers `main` and the reachable instance"
        );
        assert_eq!(
            canonical.source_witness().obligations_digest(),
            obligations_digest(&BTreeSet::new())
        );
    }

    #[test]
    fn bare_main_is_in_the_entry_scope_for_any_entry_module_name() {
        assert!(is_canonical_entry_function(Some("app"), "main"));
        assert!(is_canonical_entry_function(Some("app"), "app.helper"));
        assert!(!is_canonical_entry_function(Some("app"), "list.any"));
        assert!(is_canonical_entry_function(Some("app"), "strutil.shout"), "user library modules are in scope");
        assert!(!is_canonical_entry_function(Some("app"), "glamour.render"), "playground modules are std policy");
        assert!(!is_canonical_entry_function(Some("app"), "__capop.print"), "compiler intrinsics are not declarations");
        assert!(!is_canonical_entry_function(Some("app"), "Show__duration.DurationParseError__show"), "a mangled impl is not module-qualified");
        assert!(!is_canonical_entry_function(Some("app"), "List__any__task_2eSlot"));
        assert!(!is_canonical_entry_function(Some("app"), "app"), "the module is not a function");
        assert!(is_canonical_entry_function(None, "anything"));
    }

    #[test]
    fn canonical_scope_keeps_reachable_source_backed_intrinsics() {
        let mut module = parse("fn bridge():\n    ()\n\nfn main():\n    bridge()\n");
        module.linked_entry = Some("main".to_string());
        for item in &mut module.items {
            if let witchy_syntax::ast::Item::Function(function) = item {
                if function.name == "bridge" {
                    function.name = "task.__channel_send".to_string();
                } else if function.name == "main" {
                    witchy_syntax::ast::visit::visit_block_mut(
                        &mut function.body,
                        &mut |expression| {
                            if let witchy_syntax::ast::Expr::Call { name, .. } = expression
                                && name == "bridge"
                            {
                                *name = "task.__channel_send".to_string();
                            }
                            Ok::<_, ()>(())
                        },
                    )
                    .expect("rename test call");
                }
            }
        }
        assert!(canonical_scope(&module).contains("task.__channel_send"));
    }

    #[test]
    fn canonical_scope_imports_are_dependencies_not_roots() {
        let sources = vec![
            ("entry".into(), b"import library\nfn main() -> Int:\n    library.apply(library.make(4), 3)\n".to_vec()),
            ("library".into(), b"fn add(a: Int, b: Int) -> Int:\n    a + b\npub fn make(offset: Int) -> fn(Int) -> Int:\n    fn(value: Int) -> Int: add(offset, value)\npub fn apply(operation: fn(Int) -> Int, value: Int) -> Int:\n    operation(value)\npub fn unused(fetch: Fetch) -> Int:\n    1\n".to_vec()),
        ];
        let checked = link_checked_from_source_bundle(sources, "entry", no_expand).unwrap();
        let canonical = checked.canonical_source().unwrap();
        let scope = canonical_scope(canonical.typed_module().module());
        for name in ["main", "library.make", "library.apply", "library.add"] {
            assert!(scope.contains(name), "reachable callable {name}");
            assert!(canonical.closed_callable_abi(name).is_some(), "fresh ABI for {name}");
        }
        assert!(!scope.contains("library.unused"));
        assert!(canonical.closed_callable_abi("library.unused").is_none());
        let tails = canonical.typed_module().proper_tail_manifest().unwrap()
            .for_canonical_functions(canonical.typed_module().module(), &canonical.callable_witness.runtime_functions);
        assert_eq!(&tails, canonical.proper_tail_manifest());
        assert!(!canonical.closure_witness().entries().is_empty());

        let mut library = parse("fn unused(fetch: Fetch) -> Int:\n    1\n");
        assert!(canonical_scope(&library).contains("unused"), "unlinked library declarations remain roots");
        library.linked_entry = Some("entry".into());
        if let witchy_syntax::ast::Item::Function(function) = &mut library.items[0] {
            function.name = "entry.unused".into();
        }
        assert!(canonical_scope(&library).contains("entry.unused"), "selected module exports remain roots");
    }

    #[test]
    fn canonical_source_identity_binds_the_selected_entry() {
        let sources = vec![
            ("left".into(), b"fn main() -> Int:\n    1\n".to_vec()),
            ("right".into(), b"fn main() -> Int:\n    2\n".to_vec()),
        ];
        let left = link_checked_from_source_bundle(sources.clone(), "left", no_expand).unwrap();
        let right = link_checked_from_source_bundle(sources, "right", no_expand).unwrap();
        assert_ne!(left.canonical_source().unwrap().source_witness().source_bundle_digest(),
            right.canonical_source().unwrap().source_witness().source_bundle_digest());
    }

    #[test]
    fn canonical_scope_does_not_skip_source_checking_unused_imports() {
        let sources = vec![
            ("entry".into(), b"import library\nfn main() -> Int:\n    1\n".to_vec()),
            ("library".into(), b"pub fn invalid() -> Int:\n    \"not an Int\"\n".to_vec()),
        ];
        assert!(link_checked_from_source_bundle(sources, "entry", no_expand).is_err());
    }

    /// A rights-qualified capability parameter is inside the canonical ABI
    /// (RFC-0165 slice 42): the rights are encoded by name, so the program
    /// binds its manifest and records no callable-ABI obligation.
    #[test]
    fn checked_source_binds_rights_qualified_capability_parameters() {
        let checked = link_checked_from_source_bundle(
            vec![(
                "main".to_string(),
                b"fn peek(dir: Dir[Read]) -> Int:\n    1\n\nfn main(dir: Dir) -> Int:\n    peek(dir)\n".to_vec(),
            )],
            "main",
            no_expand,
        )
        .expect("a valid program with a rights-qualified capability checks");
        let canonical = checked.canonical_source().expect("canonical source");
        assert!(
            !canonical.obligations().iter().any(|obligation| matches!(obligation, CanonicalObligation::CallableAbi(_))),
            "{:?}",
            canonical.obligations()
        );
        assert!(canonical.closed_callable_abi("main").is_some(), "the entry binds its callable ABI");
        assert!(!canonical.callable_abi_manifest().entries().is_empty(), "the inventory is bound");
    }

    /// The entry scope decides strictness: a bodyless instance inside it is a
    /// forged direct call target and refuses; one outside it is an obligation.
    #[test]
    fn forged_entry_scope_specialization_refuses_while_library_names_become_obligations() {
        let mut module = parse(
            "fn identity(value: a) -> a:\n    value\n\nfn main() -> Int:\n    identity(1)\n",
        );
        module.linked_entry = Some("main".to_string());
        let (module, mut instances) =
            crate::traits::lower_for_wasm_with_specializations(module).into_parts();
        let typed = typeck::annotate_checked(module).expect("lowered generic source rechecks");
        let identity = instances
            .values()
            .next()
            .expect("generic instance identity")
            .clone();
        instances.insert("util.forged__Int".to_string(), identity.clone());
        let witness = bind_canonical_callable_witness(&typed, instances.clone())
            .expect("library-scope names are obligations, not type errors");
        assert!(witness.obligations.contains(&CanonicalObligation::Specialization("util.forged__Int".to_string())));
        // The entry's own instance is reachable from `main`: a runtime
        // function, never an obligation.
        assert!(witness.runtime_functions.iter().any(|name| name.starts_with("identity__")), "{:?}", witness.runtime_functions);
        assert!(!witness.obligations.iter().any(|obligation| matches!(
            obligation,
            CanonicalObligation::Specialization(name) if name.starts_with("identity__")
        )));

        instances.insert("main.forged__Int".to_string(), identity);
        let error = bind_canonical_callable_witness(&typed, instances)
            .expect_err("a bodyless specialization inside the entry scope must refuse");
        assert!(error.to_string().contains("main.forged__Int"));
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
    fn canonical_closure_witness_orders_captures_and_keeps_nested_bindings_distinct() {
        let checked = link_checked_from_source_bundle(
            vec![(
                "main".to_string(),
                b"fn factory(seed: Int, a: Int, b: Int):\n    let first = fn():\n        let value = seed\n        fn(): value\n    let second = fn():\n        let value = seed\n        fn(): value\n    let ordered = fn(): a + b\n\nfn main():\n    factory(0, 1, 2)\n".to_vec(),
            )],
            "main",
            no_expand,
        )
        .expect("closure source checks");
        let canonical = checked.canonical_source().expect("canonical source");
        let entries = canonical.closure_witness().entries();
        assert_eq!(entries.len(), 5, "three outer and two inner lambdas");

        let inner = entries
            .iter()
            .filter(|entry| entry.captures().iter().any(|capture| capture.binding().name() == "value"))
            .collect::<Vec<_>>();
        assert_eq!(inner.len(), 2);
        assert_eq!(inner[0].captures()[0].binding().name(), "value");
        assert_eq!(inner[1].captures()[0].binding().name(), "value");
        assert_ne!(
            inner[0].captures()[0].binding(),
            inner[1].captures()[0].binding(),
            "same-spelled nested locals need distinct checked binding identities"
        );

        let ordered = entries
            .iter()
            .find(|entry| entry.captures().iter().any(|capture| capture.binding().name() == "a"))
            .expect("ordered capture lambda");
        assert_eq!(
            ordered
                .captures()
                .iter()
                .map(|capture| capture.binding().name())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn canonical_closure_witness_reconciles_lambda_tail_identity() {
        let checked = link_checked_from_source_bundle(
            vec![(
                "main".to_string(),
                b"fn recur(n: Int) -> Int:\n    if n > 0:\n        recur(n - 1)\n    else:\n        0\n\nfn main():\n    let f = fn(): recur(1)\n".to_vec(),
            )],
            "main",
            no_expand,
        )
        .expect("tail closure source checks");
        let canonical = checked.canonical_source().expect("canonical source");
        let closure = canonical
            .closure_witness()
            .entries()
            .iter()
            .find(|entry| !entry.id().lambda_path.is_empty())
            .expect("lambda witness");
        assert!(
            canonical
                .proper_tail_manifest()
                .sites()
                .iter()
                .any(|site| site.key.callable == *closure.id()),
            "tail obligations inside a lambda must use its real structural identity"
        );
    }

    #[test]
    fn canonical_closure_witness_rejects_borrowed_capture_authentication() {
        let error = link_checked_from_source_bundle(
            vec![(
                "main".to_string(),
                b"mode opt\n\nfn make(xs: List(Int)):\n    let view = &xs\n    let f = fn(): view[0]\n\nfn main():\n    make([])\n".to_vec(),
            )],
            "main",
            no_expand,
        )
        .expect_err("borrowed closure capture must not enter the v1 witness");
        assert!(error.to_string().contains("closure capture"), "{error}");
        // `&List(Int)` has no admitted carrier; that, not a loan escape, is why
        // this capture cannot enter the witness (RFC-0167).
        assert!(error.to_string().contains("no admitted carrier"), "{error}");
    }

    #[test]
    fn canonical_closure_witness_digest_rejects_cross_source_substitution() {
        let make = |source: &[u8]| {
            link_checked_from_source_bundle(
                vec![("main".to_string(), source.to_vec())],
                "main",
                no_expand,
            )
            .expect("closure source checks")
        };
        let first = make(b"fn make(seed: Int):\n    let f = fn(): seed\n\nfn main():\n    make(1)\n");
        let second = make(b"fn make(seed: Float):\n    let f = fn(): seed\n\nfn main():\n    make(1.0)\n");
        let first_source = first.canonical_source().expect("first canonical source");
        let second_source = second.canonical_source().expect("second canonical source");
        assert_ne!(
            first_source.source_witness().closure_witness_digest(),
            second_source.source_witness().closure_witness_digest()
        );
        assert!(!first_source
            .source_witness()
            .verifies_closure_witness(second_source.closure_witness()));
        assert!(first_source
            .source_witness()
            .verifies_closure_witness(first_source.closure_witness()));

        let first_same_bundle = first_source
            .closure_witness()
            .clone()
            .bind_source_bundle_digest([7; 32]);
        let second_same_bundle = second_source
            .closure_witness()
            .clone()
            .bind_source_bundle_digest([7; 32]);
        assert_ne!(
            first_same_bundle.digest(),
            second_same_bundle.digest(),
            "manifest identity must include canonical structure, not only the source bundle digest"
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
            .extend(parse("fn main():\n  ()\n").items);
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
