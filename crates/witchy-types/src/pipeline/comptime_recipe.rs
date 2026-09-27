//! Explicit source-backed entry for canonical compile-time execution.
//! This does not replace the interpreted expander or admit caller-supplied ASTs.
use super::*;
use crate::runtime_type::PackageSource;
use witchy_syntax::compiler_session::CompilerSessionContract;

mod block_session;
pub use block_session::{ComptimeBlockSite, ComptimeSourceSession, PriorComptimeBlockOutput};

pub(super) fn expansion_free_expander() -> ComptimeExpander {
    ComptimeExpander::with_sibling_request(block_session::require_expansion_free, |_| false)
}

#[derive(Debug)]
pub struct CheckedComptimeRecipe { checked: CheckedModule }

impl CheckedComptimeRecipe {
    pub fn canonical_source(&self) -> &CanonicalCheckedSource {
        self.checked.canonical_source().expect("checked comptime recipe retains source")
    }

    /// Exact source identity carried by the admitted compiler-session launch.
    /// The runtime repeats this identity in its opaque execution outcome.
    pub fn session_source_digest(&self) -> [u8; 32] {
        CompilerSessionContract::decode(self.canonical_source().launch_contract_payload())
            .expect("checked comptime recipe retains its bound compiler session")
            .source_digest()
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
    authenticate_meta(&owners)?;
    let digest = checked_source_digest(&sources, "comptime", &PrecompiledDeclarationEnvironment::default());
    let contract = CompilerSessionContract::for_source(digest, invocation_scope.into())
        .map_err(|message| PipelineError::Type(TypeError { message }))?;
    let checked = link_checked_source_bundle_with_compiler_session(
        sources, "comptime", expand, Some(user_modules), Some(owners), None, Some(&contract),
    )?;
    bind_recipe(checked, &contract)
}

fn authenticate_meta(owners: &AuthenticatedModuleOwners) -> Result<(), PipelineError> {
    let refuse = |message: &str| PipelineError::Type(TypeError { message: message.into() });
    let meta = owners.owner("meta").ok_or_else(|| refuse("comptime recipe lacks authenticated meta owner"))?;
    if meta.package().source() != &PackageSource::Toolchain
        || meta.package().name() != "witchy/std"
        || meta.module_path() != ["std", "meta"]
    { return Err(refuse("comptime recipe requires toolchain witchy/std meta ownership")); }
    Ok(())
}

fn bind_recipe(mut checked: CheckedModule, contract: &CompilerSessionContract) -> Result<CheckedComptimeRecipe, PipelineError> {
    let refuse = |message: &str| PipelineError::Type(TypeError { message: message.into() });
    let source = checked.canonical_source.as_mut().ok_or_else(|| refuse("comptime recipe lacks checked source"))?;
    let Some(Type::Fn(parameters, result, _, _)) = source.callable_witness.signatures.get("main").map(Type::unqualified) else {
        return Err(refuse("comptime recipe requires a checked main"));
    };
    if !parameters.is_empty() || !matches!(result.unqualified(), Type::Named(name, args) if name == "Int" && args.is_empty()) {
        return Err(refuse("initial comptime recipe requires parameterless Int main"));
    }
    let payload = contract.encode();
    super::build_recipe::bind_checked_recipe_launch_contract(source, payload);
    Ok(CheckedComptimeRecipe { checked })
}

/// Compiler-owned typed bridge declarations enter before ordinary resolution
/// and checking. Names select no authority: only the private checked-recipe
/// route installs these bodies, and final imports still require admission.
/// Quotes already have parser-owned intrinsic calls and payloads.
pub(super) fn install_services(module: &mut Module) {
    use witchy_syntax::ast::{Block, Expr, Function, Param, Stmt};
    if !module.imports.iter().any(|name| name == "meta") { module.imports.push("meta".into()); }
    module.items.push(Item::Function(Function {
        line: 0, public: false, pure: false, comptime_only: false,
        attributes: Vec::new(), name: "emit_expr".into(), type_params: Vec::new(),
        params: vec![Param { name: "syntax_value".into(), ty: Some(Type::Named("meta.ExprSyntax".into(), Vec::new())),
            convention: Default::default(), default: None }],
        ret: Some(Type::Tuple(Vec::new())),
        body: Block { stmts: vec![Stmt::Expr(Expr::Call {
            name: witchy_syntax::intrinsics::COMPILER_EMIT_EXPR.into(), args: vec![Expr::Var("syntax_value".into())],
        })], lines: vec![0], region: None },
        bounds: Vec::new(), is_gen: false, is_async: false,
    }));
    module.items.push(Item::Function(Function {
        line: 0, public: false, pure: false, comptime_only: false,
        attributes: Vec::new(), name: "emit_item".into(), type_params: Vec::new(),
        params: vec![Param { name: "syntax_value".into(), ty: Some(Type::Named("meta.ItemSyntax".into(), Vec::new())),
            convention: Default::default(), default: None }],
        ret: Some(Type::Tuple(Vec::new())),
        body: Block { stmts: vec![Stmt::Expr(Expr::Call {
            name: witchy_syntax::intrinsics::COMPILER_EMIT_ITEM.into(), args: vec![Expr::Var("syntax_value".into())],
        })], lines: vec![0], region: None },
        bounds: Vec::new(), is_gen: false, is_async: false,
    }));
}
