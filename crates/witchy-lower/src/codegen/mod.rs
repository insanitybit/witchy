//! The compiled backend's public entry points. Every program crosses the
//! canonical SafeWIR pipeline (`crate::canonical_pipeline`): a checked module
//! is lowered, checked, and admitted to Wasm, or refused.

use std::fmt;

use witchy_syntax::ast::Module;

mod glamour_metadata;
pub use glamour_metadata::{
    checked_glamour_island_execution_module, checked_glamour_islands,
    checked_glamour_worker_execution_module,
    checked_glamour_static_evaluation_module,
    checked_glamour_templates, CheckedGlamourApplication, GlamourBrowserPolicyMetadata, GlamourIslandMetadata, GlamourTemplateAttributeMetadata,
    GlamourTemplateMetadata, GlamourTemplateNodeMetadata, GlamourTemplateOriginMetadata,
    GlamourHostPortMetadata, GlamourMappedWorkMetadata, GlamourTemplateSlotMetadata, GlamourWorkerTaskMetadata,
    GlamourWorkMetadata,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CodegenError {
    pub message: String,
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "codegen error: {}", self.message)
    }
}

impl std::error::Error for CodegenError {}

#[derive(Debug, Clone, PartialEq)]
pub struct UnsupportedLowering {
    pub message: String,
}

impl fmt::Display for UnsupportedLowering {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported lowering: {}", self.message)
    }
}

/// The explicit result of crossing the checked-module to Wasm boundary.
///
/// `Unsupported` is reserved for source constructs that are valid but do not
/// yet have a capability-correct compiled lowering. Broken compiler output and
/// failed admission are `Rejected`, never disguised as a miss.
#[must_use]
#[derive(Debug, Clone, PartialEq)]
pub enum LoweringOutcome<T> {
    Lowered(T),
    Unsupported(UnsupportedLowering),
    Rejected(CodegenError),
}

impl<T> LoweringOutcome<T> {
    pub fn expect_lowered(self, message: &str) -> T {
        match self {
            Self::Lowered(value) => value,
            Self::Unsupported(reason) => panic!("{message}: {reason}"),
            Self::Rejected(error) => panic!("{message}: {error}"),
        }
    }

    pub fn expect_rejected(self, message: &str) -> CodegenError {
        match self {
            Self::Rejected(error) => error,
            Self::Lowered(_) => panic!("{message}: lowering succeeded"),
            Self::Unsupported(reason) => panic!("{message}: {reason}"),
        }
    }

    pub fn expect_unsupported(self, message: &str) -> UnsupportedLowering {
        match self {
            Self::Unsupported(reason) => reason,
            Self::Lowered(_) => panic!("{message}: lowering succeeded"),
            Self::Rejected(error) => panic!("{message}: {error}"),
        }
    }
}

fn unsupported<T>(message: impl Into<String>) -> LoweringOutcome<T> {
    LoweringOutcome::Unsupported(UnsupportedLowering { message: message.into() })
}

/// Compile a module through the canonical SafeWIR production boundary.
///
/// The returned bytes are the exact bytes held by `AdmittedCanonicalArtifact`.
/// Valid checked programs outside the supported subset fail closed as
/// `Unsupported`; malformed compiler output or failed admission is `Rejected`.
pub fn compile_checked_module_artifact(
    checked: &witchy_types::pipeline::CheckedModule,
) -> LoweringOutcome<witchy_wir::AdmittedCanonicalArtifact> {
    canonical_artifact_outcome(crate::canonical_pipeline::admit_checked_module(checked))
}

/// Run an observer on the exact production-validated graph and its admitted
/// artifact. Refusal never calls the observer; ordinary compilation shares the
/// same initial/final validation, backend correspondence and byte matcher.
pub fn compile_checked_module_artifact_with_observer<R>(
    checked: &witchy_types::pipeline::CheckedModule,
    observe: impl FnOnce(
        &witchy_wir::canonical_safewir::ValidatedSafeWir,
        &witchy_wir::AdmittedCanonicalArtifact,
    ) -> R,
) -> LoweringOutcome<(witchy_wir::AdmittedCanonicalArtifact, R)> {
    canonical_artifact_outcome(
        crate::canonical_pipeline::admit_checked_module_with_observer(checked, observe),
    )
}

fn canonical_artifact_outcome<T>(
    result: Result<T, crate::canonical_pipeline::CanonicalPipelineError>,
) -> LoweringOutcome<T> {
    match result {
        Ok(artifact) => LoweringOutcome::Lowered(artifact),
        Err(crate::canonical_pipeline::CanonicalPipelineError::Layout(error)) => {
            unsupported(error.to_string())
        }
        Err(crate::canonical_pipeline::CanonicalPipelineError::Lowering(error)) => {
            unsupported(error.to_string())
        }
        Err(error) => LoweringOutcome::Rejected(CodegenError {
            message: error.to_string(),
        }),
    }
}

pub fn compile_checked_module_binary(
    checked: &witchy_types::pipeline::CheckedModule,
) -> LoweringOutcome<Vec<u8>> {
    match compile_checked_module_artifact(checked) {
        LoweringOutcome::Lowered(artifact) => {
            LoweringOutcome::Lowered(artifact.wasm().to_vec())
        }
        LoweringOutcome::Unsupported(reason) => LoweringOutcome::Unsupported(reason),
        LoweringOutcome::Rejected(error) => LoweringOutcome::Rejected(error),
    }
}

/// Compile a generated test driver that was proved to differ from its checked
/// source program only at `main`. Keeping this boundary distinct prevents the
/// post-link rewrite from impersonating a production `CheckedModule`.
pub fn compile_checked_test_driver_artifact(
    checked: &witchy_types::pipeline::CheckedTestDriverModule,
) -> LoweringOutcome<witchy_wir::AdmittedCanonicalArtifact> {
    let Some(source) = checked.canonical_source() else {
        return unsupported("canonical test drivers require retained raw sources and a compiler-owned recipe; AST-only, fixture and integration drivers are unsupported");
    };
    canonical_artifact_outcome(crate::canonical_pipeline::admit_checked_source(source))
}

/// Only the sealed production source recipe can select a build launch entry.
pub fn compile_checked_build_recipe_artifact(
    checked: &witchy_types::pipeline::CheckedBuildRecipe,
) -> LoweringOutcome<witchy_wir::AdmittedCanonicalArtifact> {
    canonical_artifact_outcome(crate::canonical_pipeline::admit_checked_source(checked.canonical_source()))
}

/// Explicit, zero-pass canonical execution path for a checked compiler recipe.
pub fn compile_checked_comptime_recipe_artifact(
    recipe: &witchy_types::pipeline::CheckedComptimeRecipe,
) -> LoweringOutcome<witchy_wir::AdmittedCanonicalArtifact> {
    canonical_artifact_outcome(crate::canonical_pipeline::admit_unoptimized_checked_source(recipe.canonical_source()))
}

/// A compiler-rewritten Glamour application and generated adapter that were
/// checked together against one authenticated source authority.
///
/// Fields and construction are private so a raw `Module` cannot reach the
/// production encoder by impersonating a compiler rewrite.
#[derive(Debug)]
pub struct CheckedGlamourExecutable {
    #[allow(dead_code)]
    checked: witchy_types::pipeline::CheckedEvaluationModule,
}

/// Check and bind one compiler-generated adapter to the exact authenticated
/// application rewrite that selected it.
pub fn check_glamour_island_executable(
    application: &CheckedGlamourApplication<'_>,
    generated: &Module,
) -> Result<CheckedGlamourExecutable, CodegenError> {
    let authority = application.authority();
    if !generated.modes.is_empty()
        || !generated.imports.is_empty()
        || !generated.from_imports.is_empty()
        || !generated.import_lines.is_empty()
        || generated.linked_entry.is_some()
        || !generated.compiler_item_syntax.is_empty()
        || !generated.compiler_expr_syntax.is_empty()
        || !generated.compiler_type_syntax.is_empty()
        || !generated.compiler_pattern_syntax.is_empty()
        || !generated.compiler_stmt_syntax.is_empty()
        || !generated.compiler_block_syntax.is_empty()
    {
        return Err(CodegenError {
            message: "compiler-generated Glamour island adapter contains unsupported module metadata"
                .into(),
        });
    }
    let mut module = application.module().clone();
    module.items.extend(generated.items.clone());
    module.item_lines.clear();
    let checked = witchy_types::pipeline::check_compiler_evaluation_module(authority, module)
        .map_err(|error| CodegenError {
            message: format!(
                "compiler-generated executable failed semantic checking before lowering: {error}"
            ),
        })?;
    Ok(CheckedGlamourExecutable { checked })
}

/// Compile only a compiler-generated Glamour executable that crossed
/// [`check_glamour_island_executable`].
pub fn compile_checked_glamour_island_execution_binary(
    _executable: &CheckedGlamourExecutable,
) -> LoweringOutcome<Vec<u8>> {
    unsupported("Glamour island execution is not yet supported by canonical SafeWIR")
}
