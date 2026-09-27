//! Explicit source-backed entry for canonical compile-time execution.
//! This does not replace the interpreted expander or admit caller-supplied ASTs.
use super::*;
use crate::runtime_type::PackageSource;
use witchy_syntax::compiler_session::CompilerSessionContract;

#[derive(Debug)]
pub struct CheckedComptimeRecipe { checked: CheckedModule }

impl CheckedComptimeRecipe {
    pub fn canonical_source(&self) -> &CanonicalCheckedSource {
        self.checked.canonical_source().expect("checked comptime recipe retains source")
    }
}

/// Check exact isolated compiler program bytes with loader-authenticated owners.
/// The existing `comptime` namespace carries compiler syntax typing; it does
/// not grant runtime imports without this separately bound launch contract.
pub fn check_comptime_recipe_from_source_bundle(
    mut sources: Vec<(String, Vec<u8>)>, invocation_scope: &str, expand: ComptimeExpander,
    user_modules: &HashSet<String>, owners: AuthenticatedModuleOwners,
) -> Result<CheckedComptimeRecipe, PipelineError> {
    let refuse = |message: &str| PipelineError::Type(TypeError { message: message.into() });
    sources.sort_by(|a, b| a.0.cmp(&b.0));
    if sources.iter().any(|(name, _)| name.is_empty())
        || sources.windows(2).any(|pair| pair[0].0 == pair[1].0)
        || !sources.iter().any(|(name, _)| name == "comptime")
    { return Err(refuse("comptime recipe requires unique source names and its isolated comptime entry")); }
    let meta = owners.owner("meta").ok_or_else(|| refuse("comptime recipe lacks authenticated meta owner"))?;
    if meta.package().source() != &PackageSource::Toolchain
        || meta.package().name() != "witchy/std"
        || meta.module_path() != ["std", "meta"]
    { return Err(refuse("comptime recipe requires toolchain witchy/std meta ownership")); }
    let digest = checked_source_digest(&sources, "comptime", &PrecompiledDeclarationEnvironment::default());
    let contract = CompilerSessionContract::for_source(digest, invocation_scope.into())
        .map_err(|message| PipelineError::Type(TypeError { message }))?;
    let mut checked = link_checked_source_bundle_with(
        sources, "comptime", expand, Some(user_modules), Some(owners), None,
    )?;
    let source = checked.canonical_source.as_mut().ok_or_else(|| refuse("comptime recipe lacks checked source"))?;
    let Some(Type::Fn(parameters, result, _, _)) = source.callable_witness.signatures.get("main").map(Type::unqualified) else {
        return Err(refuse("comptime recipe requires a checked main"));
    };
    if !parameters.is_empty() || !matches!(result.unqualified(), Type::Named(name, args) if name == "Int" && args.is_empty()) {
        return Err(refuse("initial comptime recipe requires parameterless Int main"));
    }
    let payload = contract.encode();
    source.source_witness.rebind_launch_contract_for_build_recipe(launch_contract_digest(&payload));
    source.launch_contract_payload = payload;
    Ok(CheckedComptimeRecipe { checked })
}
