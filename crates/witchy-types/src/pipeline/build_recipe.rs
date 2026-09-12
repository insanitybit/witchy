//! Production BuildOut recipe v1. Raw source and loader ownership are inputs;
//! no AST, selected ordinal, generated body, or claimed digest is accepted.
use super::*;
use witchy_syntax::ast::{Convention, Expr, Item, Stmt, Type};

const VERSION: u32 = 1;
const WRAPPER: &str = "witchy_build_recipe_entry";

#[derive(Debug)]
pub struct BuildRecipeIdentity {
    original: [u8; 32],
    derived: [u8; 32],
    declaration: String,
    ordinal: u64,
}

impl BuildRecipeIdentity {
    pub fn original_source_digest(&self) -> [u8; 32] { self.original }
    pub fn derived_source_digest(&self) -> [u8; 32] { self.derived }
    pub fn selected_declaration(&self) -> &str { &self.declaration }
    pub fn declaration_ordinal(&self) -> u64 { self.ordinal }
    pub fn recipe_version(&self) -> u32 { VERSION }
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
    let (original, _) = link_checked_with(modules.clone(), entry, expand,
        LinkMode::Production, Some(user_modules), false, Some(owners.clone()))?;
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
        || target.params.len() != 1 || target.params[0].ty.is_none()
        || target.params[0].default.is_some() || target.params[0].convention != Convention::Let
    {
        return Err(refused("build recipe supports one default-convention BuildOut parameter; async, generic, defaulted and other build capability families are unsupported"));
    }
    let parameter = target.params[0].clone();
    let ordinal = u64::try_from(*ordinal).map_err(|_| refused("build declaration ordinal overflow"))?;
    let declaration = format!("{entry}.build");
    let original_digest = checked_source_digest(&sources);
    let mut hash = blake3::Hasher::new();
    hash.update(b"witchy.checked-build-recipe.production.build-out\0");
    hash.update(&VERSION.to_le_bytes());
    hash.update(&original_digest);
    hash.update(&(entry.len() as u64).to_le_bytes());
    hash.update(entry.as_bytes());
    hash.update(&ordinal.to_le_bytes());
    hash.update(&(declaration.len() as u64).to_le_bytes());
    hash.update(declaration.as_bytes());
    let identity = BuildRecipeIdentity { original: original_digest, derived: *hash.finalize().as_bytes(), declaration, ordinal };

    // A build capability is intentionally NOT legal in ordinary main. A fixed
    // compiler wrapper is checked as an ordinary callable; only the sealed
    // recipe's entry selector lets admission expose it as the launch target.
    let mut generated = witchy_syntax::parser::parse_module(
        "fn witchy_build_recipe_entry() -> Nil:\n    Nil\n",
    ).map_err(|error| refused(format!("invalid compiler build recipe: {error}")))?;
    let Item::Function(wrapper) = &mut generated.items[0] else { return Err(refused("invalid build wrapper")); };
    wrapper.params = vec![parameter];
    wrapper.line = 0;
    wrapper.body.stmts = vec![Stmt::Expr(Expr::Call {
        name: "build".into(), args: vec![Expr::Var(wrapper.params[0].name.clone())],
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
    if name != &identity.declaration || args.len() != 1
        || !matches!(&args[0], Expr::Var(name) if name == &wrapper.params[0].name)
        || !matches!(typed.table().type_of(&args[0]), Some(typeck::Ty::BuildOut))
        || !matches!(wrapper.ret.as_ref(), Some(Type::Named(name, args)) if name == "Nil" && args.is_empty())
    {
        return Err(refused("build recipe requires exact checked BuildOut parameter and Nil result; other providers are unsupported"));
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
    // checked ABI/census ordinal and its one write-only BuildOut root. Ordinary
    // raw-byte launch readers that understand only v1 must refuse it.
    let name_len = u32::try_from(entry_name.len()).map_err(|_| refused("build entry name overflow"))?;
    let mut payload = vec![2, 1]; // launch version, BuildOut recipe kind
    payload.extend_from_slice(&name_len.to_le_bytes());
    payload.extend_from_slice(entry_name.as_bytes());
    payload.extend_from_slice(&target.function_ordinal.to_le_bytes());
    payload.extend_from_slice(&abi.key.as_bytes());
    payload.extend_from_slice(&[9, 1]); // canonical kind, private write right
    source.source_witness.launch_contract_digest = launch_contract_digest(&payload);
    source.launch_contract_payload = payload;
    source.build_entry = Some(entry_name);
    Ok(CheckedBuildRecipe { original, checked, identity })
}
