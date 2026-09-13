//! Production BuildOut/BuildRead recipe v2. Raw source and loader ownership are inputs;
//! no AST, selected ordinal, generated body, or claimed digest is accepted.
use super::*;
use witchy_syntax::ast::{Convention, Expr, Item, Stmt, Type};

const VERSION: u32 = 2;
const WRAPPER: &str = "witchy_build_recipe_entry";

#[derive(Debug)]
pub struct BuildRecipeIdentity {
    original: [u8; 32],
    derived: [u8; 32],
    declaration: String,
    ordinal: u64,
    parameter_kinds: Vec<u8>,
}

impl BuildRecipeIdentity {
    pub fn original_source_digest(&self) -> [u8; 32] { self.original }
    pub fn derived_source_digest(&self) -> [u8; 32] { self.derived }
    pub fn selected_declaration(&self) -> &str { &self.declaration }
    pub fn declaration_ordinal(&self) -> u64 { self.ordinal }
    pub fn recipe_version(&self) -> u32 { VERSION }
    /// Ordered canonical host kinds, derived from the original checked signature.
    pub fn parameter_kinds(&self) -> &[u8] { &self.parameter_kinds }
}

#[derive(Debug)]
pub struct CheckedBuildRecipe {
    original: CheckedModule,
    checked: CheckedModule,
    identity: BuildRecipeIdentity,
}

impl CheckedBuildRecipe {
    pub fn original_module(&self) -> &CheckedModule { &self.original }
    pub fn canonical_source(&self) -> &CanonicalCheckedSource {
        self.checked.canonical_source.as_ref().expect("sealed build recipe has source")
    }
    pub fn identity(&self) -> &BuildRecipeIdentity { &self.identity }
}

fn refused(message: impl Into<String>) -> PipelineError {
    PipelineError::Type(typeck::TypeError { message: message.into() })
}

pub fn check_build_recipe_from_source_bundle(
    mut sources: Vec<(String, Vec<u8>)>, entry: &str, expand: ComptimeExpander,
    user_modules: &HashSet<String>, owners: AuthenticatedModuleOwners,
) -> Result<CheckedBuildRecipe, PipelineError> {
    sources.sort_by(|a, b| a.0.cmp(&b.0));
    if sources.iter().any(|(name, _)| name.is_empty())
        || sources.windows(2).any(|pair| pair[0].0 == pair[1].0)
        || !sources.iter().any(|(name, _)| name == entry)
    {
        return Err(refused("build recipe requires unique raw module names and its entry"));
    }
    let mut modules = sources.iter().map(|(name, bytes)| {
        let source = std::str::from_utf8(bytes).map_err(|error| refused(format!("build source `{name}` is not UTF-8: {error}")))?;
        let module = witchy_syntax::parser::parse_module(source)
            .map_err(|error| refused(format!("build source `{name}` failed to parse: {error}")))?;
        Ok((name.clone(), module))
    }).collect::<Result<Vec<_>, PipelineError>>()?;
    // Check the actual source before creating anything. Loader ownership, source
    // policy and even an unused source main must pass ordinary production checks.
    let (original, original_typed) = link_checked_with(modules.clone(), entry, expand,
        LinkMode::Production, Some(user_modules), true, Some(owners.clone()))?;
    let original_typed = original_typed.ok_or_else(|| refused("original build check returned no typed source"))?;
    let (_, module) = modules.iter_mut().find(|(name, _)| name == entry)
        .ok_or_else(|| refused("build entry source missing"))?;
    if module.items.iter().any(|item| matches!(item, Item::Function(f) if f.name == WRAPPER)) {
        return Err(refused("build source collides with the compiler recipe entry"));
    }
    let targets = module.items.iter().enumerate().filter_map(|(index, item)| match item {
        Item::Function(f) if f.name == "build" => Some((index, f)), _ => None,
    }).collect::<Vec<_>>();
    let [(ordinal, target)] = targets.as_slice() else {
        return Err(refused("build recipe requires exactly one `build` declaration in its own entry source; imported and missing targets are unsupported"));
    };
    if target.is_async || target.is_gen || target.comptime_only || !target.bounds.is_empty()
        || target.params.is_empty() || target.params.len() > usize::from(u8::MAX)
        || target.params.iter().any(|parameter| parameter.ty.is_none()
            || parameter.default.is_some() || parameter.convention != Convention::Let)
    {
        return Err(refused("build recipe supports default-convention BuildOut/BuildRead parameters; async, generic, defaulted and other build capability families are unsupported"));
    }
    let parameters = target.params.clone();
    let ordinal = u64::try_from(*ordinal).map_err(|_| refused("build declaration ordinal overflow"))?;
    let declaration = format!("{entry}.build");
    // Consult the original semantic signature, not the spelling of raw parameter
    // annotations. The same ordered capability ABI is checked again after the
    // compiler wrapper is linked; neither check reuses the other's witnesses.
    let original_signature = original_typed.table().function_type(&declaration)
        .ok_or_else(|| refused("original build signature is not closed and checked"))?;
    let Type::Fn(original_parameters, _, conventions, _) = original_signature.unqualified() else {
        return Err(refused("original build declaration is not callable"));
    };
    if original_parameters.len() != parameters.len()
        || conventions.iter().any(|convention| *convention != Convention::Let)
    {
        return Err(refused("original build parameter ABI differs from its declaration"));
    }
    let parameter_kinds = original_parameters.iter().map(|parameter| match parameter.unqualified() {
        Type::Named(name, args) if name == "BuildOut" && args.is_empty() => Ok(9u8),
        Type::Named(name, args) if name == "BuildRead" && args.is_empty() => Ok(12u8),
        _ => Err(refused("build recipe requires exact checked BuildOut/BuildRead parameters; other providers are unsupported")),
    }).collect::<Result<Vec<_>, _>>()?;
    let mut parameter_abi = Vec::new();
    parameter_abi.extend_from_slice(&(parameter_kinds.len() as u32).to_le_bytes());
    for kind in &parameter_kinds {
        parameter_abi.extend_from_slice(&[*kind, 1]); // private Write or Read right
    }
    let original_digest = checked_source_digest(&sources, entry);
    let mut hash = blake3::Hasher::new();
    hash.update(b"witchy.checked-build-recipe.production.build-io\0");
    hash.update(&VERSION.to_le_bytes());
    hash.update(&original_digest);
    hash.update(&(entry.len() as u64).to_le_bytes());
    hash.update(entry.as_bytes());
    hash.update(&ordinal.to_le_bytes());
    hash.update(&(declaration.len() as u64).to_le_bytes());
    hash.update(declaration.as_bytes());
    hash.update(&parameter_abi);
    let identity = BuildRecipeIdentity { original: original_digest, derived: *hash.finalize().as_bytes(), declaration, ordinal, parameter_kinds };

    // A build capability is intentionally NOT legal in ordinary main. A fixed
    // compiler wrapper is checked as an ordinary callable; only the sealed
    // recipe's entry selector lets admission expose it as the launch target.
    let mut generated = witchy_syntax::parser::parse_module(
        "fn witchy_build_recipe_entry() -> Nil:\n    Nil\n",
    ).map_err(|error| refused(format!("invalid compiler build recipe: {error}")))?;
    let Item::Function(wrapper) = &mut generated.items[0] else { return Err(refused("invalid build wrapper")); };
    wrapper.params = parameters;
    wrapper.line = 0;
    wrapper.body.stmts = vec![Stmt::Expr(Expr::Call {
        name: "build".into(), args: wrapper.params.iter().map(|parameter| Expr::Var(parameter.name.clone())).collect(),
    })];
    wrapper.body.lines = vec![0];
    for index in (0..module.items.len()).rev() {
        if matches!(&module.items[index], Item::Function(f) if f.name == "main") {
            module.items.remove(index);
            if index < module.item_lines.len() { module.item_lines.remove(index); }
        }
    }
    module.items.extend(generated.items);
    module.item_lines.push(0);
    let (checked, typed) = link_checked_with(modules, entry, expand, LinkMode::Production,
        Some(user_modules), true, Some(owners))?;
    let typed = typed.ok_or_else(|| refused("build recipe check returned no typed source"))?;
    let entry_name = format!("{entry}.{WRAPPER}");
    let wrapper = typed.module().items.iter().find_map(|item| match item {
        Item::Function(f) if f.name == entry_name => Some(f), _ => None,
    }).ok_or_else(|| refused("checked build recipe entry missing"))?;
    let [Stmt::Expr(Expr::Call { name, args })] = wrapper.body.stmts.as_slice() else {
        return Err(refused("checked build recipe call shape changed"));
    };
    if name != &identity.declaration || args.len() != identity.parameter_kinds.len()
        || wrapper.params.len() != args.len()
        || args.iter().zip(&wrapper.params).zip(&identity.parameter_kinds).any(|((argument, parameter), kind)| {
            !matches!(argument, Expr::Var(name) if name == &parameter.name)
                || !matches!((typed.table().type_of(argument), kind),
                    (Some(typeck::Ty::BuildOut), 9) | (Some(typeck::Ty::BuildRead), 12))
        })
        || !matches!(wrapper.ret.as_ref(), Some(Type::Named(name, args)) if name == "Nil" && args.is_empty())
    {
        return Err(refused("build recipe requires exact checked BuildOut/BuildRead parameters in source order and Nil result; other providers are unsupported"));
    }
    let mut checked = finish_canonical_source(checked, typed, identity.derived)?;
    let source = checked.canonical_source.as_mut().ok_or_else(|| refused("build recipe source missing"))?;
    let abi = source.closed_callable_abi(&entry_name)
        .ok_or_else(|| refused("build wrapper missing from fresh callable census"))?;
    let CanonicalTargetPolicy::ClosedLocal(target) = abi.target_policy else {
        return Err(refused("build wrapper is not a checked closed local callable"));
    };
    if source.callable_witness.runtime_functions.get(target.function_ordinal as usize) != Some(&entry_name) {
        return Err(refused("build wrapper callable census binding differs"));
    }
    // Version 2 is a BUILD launch contract, not a main's ordinary host-demand
    // payload. It binds the compiler-selected declaration, its exact freshly
    // checked ABI/census ordinal and ordered Write/Read roots. Ordinary
    // raw-byte launch readers that understand only v1 must refuse it.
    let name_len = u32::try_from(entry_name.len()).map_err(|_| refused("build entry name overflow"))?;
    let mut payload = vec![2, 2]; // launch version, ordered BuildOut/BuildRead recipe
    payload.extend_from_slice(&name_len.to_le_bytes());
    payload.extend_from_slice(entry_name.as_bytes());
    payload.extend_from_slice(&target.function_ordinal.to_le_bytes());
    payload.extend_from_slice(&abi.key.as_bytes());
    payload.extend_from_slice(&parameter_abi);
    source.source_witness.launch_contract_digest = launch_contract_digest(&payload);
    source.launch_contract_payload = payload;
    source.build_entry = Some(entry_name);
    Ok(CheckedBuildRecipe { original, checked, identity })
}
