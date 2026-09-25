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
use crate::canonical_abi;
use witchy_canonical_source::abi::{CanonicalCallableAbi, CanonicalCallableAbiManifest, CanonicalTargetPolicy};
use witchy_canonical_source::closure::ClosureWitnessManifest;
use witchy_canonical_source::source::{
    CanonicalSourceParts, CanonicalSourceView, CheckedAccessQuery, CheckedSourceDigests,
    CheckedSourceWitness, PrecompiledExport,
};
use crate::precompiled_declarations::PrecompiledDeclarationEnvironment;
use crate::optimization_readiness::OptimizationReadiness;
use crate::typeck::{self, ProperTailSiteManifest, TypeError};

/// The executable callable inventory produced by the canonical checked
/// frontend.  Generic templates deliberately have no entry: a direct call may
/// target only a concrete function whose body and signature were rechecked
/// after monomorphization.
#[derive(Debug, PartialEq)]
struct CanonicalCallableWitness {
    signatures: BTreeMap<String, witchy_syntax::ast::Type>,
    dictionary_comparators: Vec<(witchy_syntax::ast::Type, String)>,
    generic_instances: BTreeMap<String, witchy_canonical_source::specialization::LogicalSpecializationIdentity>,
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
    // Derived from `typed`, which never changes after construction. Every
    // canonical stage reads it; one shared map also lets the aggregate shape
    // memo (keyed by the map's address) span all of them.
    type_definitions: BTreeMap<String, witchy_syntax::ast::TypeDef>,
    // The precompiled environment as the Wasm imports it binds; derived once
    // from `precompiled_declarations`, which never changes after construction.
    precompiled_exports: Vec<PrecompiledExport>,
    slice_carriers: Vec<witchy_canonical_source::carrier::CarrierElement>,
}

impl CanonicalCheckedSource {
    /// The read-only projection SafeWIR and the Wasm emitter consume. This is
    /// the only construction site of [`CanonicalSourceView`].
    pub fn view(&self) -> CanonicalSourceView<'_> {
        CanonicalSourceView::from_checked_frontend(CanonicalSourceParts {
            module: self.typed.module(),
            type_definitions: &self.type_definitions,
            source_witness: &self.source_witness,
            closure_witness: &self.closure_witness,
            signatures: &self.callable_witness.signatures,
            generic_instances: &self.callable_witness.generic_instances,
            abi_manifest: &self.callable_witness.abi_manifest,
            closed_abis: &self.callable_witness.closed_abis,
            runtime_functions: &self.callable_witness.runtime_functions,
            launch_contract_payload: &self.launch_contract_payload,
            build_entry: self.build_entry.as_deref(),
            access: self.typed.access_facts().ok().map(|facts| facts as &dyn CheckedAccessQuery),
            loans_checked: self.typed.loan_facts().is_ok(),
            precompiled_exports: &self.precompiled_exports,
            slice_carriers: &self.slice_carriers,
        })
    }

    /// The declared types of the typed module, keyed by bare and
    /// entry-qualified name (see `witchy_canonical_source::aggregates::type_definitions`).
    pub fn type_definitions(&self) -> &BTreeMap<String, witchy_syntax::ast::TypeDef> {
        &self.type_definitions
    }

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
    ) -> Option<&witchy_canonical_source::specialization::LogicalSpecializationIdentity> {
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
    let _link = crate::std_pruning::enter_link();
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
    let type_definitions = witchy_canonical_source::aggregates::type_definitions(typed.module());
    let precompiled_exports = precompiled_declarations.libraries().flat_map(|library| {
        library.exports().map(move |(export, _)| PrecompiledExport {
            library_alias: library.alias().to_owned(),
            export: export.to_owned(),
            library_binding_digest: library.binding_digest(),
            wasm_sha256: library.wasm_sha256(),
            sidecar_sha256: library.sidecar_sha256(),
        })
    }).collect();
    let mut slice_carriers = typed.view_facts().ok().into_iter()
        .flat_map(|facts| facts.iter())
        .filter_map(|fact| typeck::ty_to_ast(fact.payload()))
        .filter_map(|payload| witchy_canonical_source::carrier::CarrierElement::from_slice_payload_type(&payload))
        .collect::<Vec<_>>();
    slice_carriers.extend(callable_witness.signatures.values().flat_map(
        witchy_canonical_source::carrier::CarrierElement::in_checked_signature,
    ));
    slice_carriers.sort_by_key(|element| element.identity());
    slice_carriers.dedup();
    checked.canonical_source = Some(CanonicalCheckedSource {
        console_fixture: false,
        build_entry: None,
        typed,
        optimization_readiness,
        dynamic_runtime_plan,
        source_witness: CheckedSourceWitness::mint_for_checked_frontend(CheckedSourceDigests {
            source_bundle_digest,
            proper_tail_manifest_digest: proper_tail_manifest.digest(),
            callable_abi_manifest_digest: callable_witness.abi_manifest.digest(),
            closure_witness_digest: closure_witness.digest(),
            launch_contract_digest,
            obligations_digest: obligations_digest(&obligations),
            precompiled_declarations_digest: precompiled_declarations.digest(),
        }),
        proper_tail_manifest,
        closure_witness,
        callable_witness,
        launch_contract_payload,
        obligations,
        precompiled_declarations,
        type_definitions,
        precompiled_exports,
        slice_carriers,
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
    // Contextual try (`e ? "msg"`) reaches here as the `__try_ctx` intrinsic,
    // which has no lowering: rewrite it by the operand's checked type into the
    // std call it means (`option.ok_or` / `result.map_err`) BEFORE
    // specialization, so those generic helpers are instantiated and the
    // recheck below types the synthesized nodes.
    let (module, _, _) = typed.rewrite_into_module(|table, module| {
        crate::try_context::rewrite_try_ctx_module(module, table)
    });
    let (module, generic_instances) =
        crate::traits::lower_for_wasm_with_specializations(module).into_parts();
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
    generic_instances: BTreeMap<String, witchy_canonical_source::specialization::LogicalSpecializationIdentity>,
) -> Result<CanonicalCallableWitness, PipelineError> {
    let linked_entry = typed.module().linked_entry.as_deref();
    let scope = witchy_canonical_source::scope::canonical_scope(typed.module());
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
                    && !witchy_canonical_source::scope::function_has_free_signature_variables(function) => {
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

/// Public only for the crate's integration tests (explicit module owners).
#[doc(hidden)]
pub fn link_checked_source_bundle_with(
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: Option<&HashSet<String>>,
    module_owners: Option<AuthenticatedModuleOwners>,
    precompiled_declarations: Option<PrecompiledDeclarationEnvironment>,
) -> Result<CheckedModule, PipelineError> {
    // Only reachable standard library is checked (crate::std_pruning). Any
    // failure after a pruned check -- in type checking or in a canonical stage
    // that synthesizes a std call by name -- reruns this route unpruned, so
    // the reported outcome is always the unpruned compiler's.
    let (pruned_result, pruned) = crate::std_pruning::with_pruning(true, || {
        link_checked_source_bundle_route(
            sources.clone(),
            entry,
            expand,
            user_modules,
            module_owners.clone(),
            precompiled_declarations.clone(),
        )
    });
    if pruned_result.is_ok() || !pruned {
        crate::std_pruning::log(if pruned { "pruned-ok" } else { "not-pruned" }.to_string());
        return pruned_result;
    }
    if let Err(error) = &pruned_result {
        crate::std_pruning::log(format!("fallback: {}", error.to_string().lines().next().unwrap_or("")));
    }
    crate::std_pruning::with_pruning(false, || {
        link_checked_source_bundle_route(
            sources,
            entry,
            expand,
            user_modules,
            module_owners,
            precompiled_declarations,
        )
    })
    .0
}

fn link_checked_source_bundle_route(
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
    let messages_rewritten;
    let dynamic_runtime_plan = if checked.module_owners.is_some() {
        let runtime_catalog = checked.runtime_declaration_catalog()?;
        (typed_source, messages_rewritten) =
            crate::canonical_messages::prepare(typed_source, Some(&runtime_catalog))
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
        (typed_source, messages_rewritten) = crate::canonical_messages::prepare(typed_source, None)
            .map_err(|message| PipelineError::Type(TypeError { message }))?;
        crate::existential::build_dynamic_runtime_plan_inner(
            &typed_source,
            &witness_catalog,
            None,
        ).map_err(|message| PipelineError::Type(TypeError { message }))?;
        None
    };

    // Only a message rewrite can retain a callable that the specialization in
    // `canonicalize_callable_instances` has not already reached. Without one,
    // the rerun re-derives the module it is given (no new body, no new
    // identity) at the cost of two whole-module typechecks.
    if messages_rewritten {
        typed_source = specialize_message_prepared_callables(typed_source, &mut preparation)?;
    }

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
