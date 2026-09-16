//! Bare test recipe v3. Raw bytes and loader ownership survive discovery; a
//! fresh parse/check of the compiler-created entry derives ALL driver witnesses.
//! Async tests are driven to completion; parameterized, fixture and integration
//! recipes are separate obligations.
use super::*;
use witchy_syntax::ast::{Expr, Item, Stmt, Type};

mod console_fixture;
pub use console_fixture::check_console_fixture_test_driver;
mod integration;
pub use integration::check_integration_test_driver;

const BARE_RECIPE_VERSION: u32 = 3;

#[derive(Debug)]
pub struct CheckedTestSuite {
    sources: Vec<(String, Vec<u8>)>,
    entry: String,
    user_modules: HashSet<String>,
    checked: CheckedModule,
    original_source_digest: [u8; 32],
}

impl CheckedTestSuite {
    pub fn checked_module(&self) -> &CheckedModule { &self.checked }
    pub fn module(&self) -> &Module { self.checked.module() }
    pub fn original_source_digest(&self) -> [u8; 32] { self.original_source_digest }
}

/// A sealed derivation, not a caller-supplied hash or a digest of an AST render.
#[derive(Debug)]
pub struct TestDriverIdentity {
    recipe_version: u32,
    original_source_digest: [u8; 32],
    derived_source_digest: [u8; 32],
    selected_declaration: String,
    declaration_ordinal: u64,
}

impl TestDriverIdentity {
    pub fn original_source_digest(&self) -> [u8; 32] { self.original_source_digest }
    pub fn derived_source_digest(&self) -> [u8; 32] { self.derived_source_digest }
    pub fn selected_declaration(&self) -> &str { &self.selected_declaration }
    pub fn declaration_ordinal(&self) -> u64 { self.declaration_ordinal }
    pub fn recipe_version(&self) -> u32 { self.recipe_version }
}

fn refused(message: impl Into<String>) -> PipelineError {
    PipelineError::Type(TypeError { message: message.into() })
}

fn parse_sources(sources: &[(String, Vec<u8>)]) -> Result<Vec<(String, Module)>, PipelineError> {
    sources.iter().map(|(name, bytes)| {
        let source = std::str::from_utf8(bytes)
            .map_err(|error| refused(format!("test source `{name}` is not UTF-8: {error}")))?;
        let module = witchy_syntax::parser::parse_module(source)
            .map_err(|error| refused(format!("test source `{name}` failed to parse: {error}")))?;
        Ok((name.clone(), module))
    }).collect()
}

/// Source-bearing discovery retains a checked original suite, not a rewritten
/// main. Test syntax policy and loader ownership cannot escape as production
/// canonical authority through its AST-only checked_module view.
pub fn link_checked_test_suite_from_source_bundle(
    mut sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
    owners: AuthenticatedModuleOwners,
) -> Result<CheckedTestSuite, PipelineError> {
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    if sources.iter().any(|(name, _)| name.is_empty())
        || sources.windows(2).any(|pair| pair[0].0 == pair[1].0)
        || !sources.iter().any(|(name, _)| name == entry)
    {
        return Err(refused("test source bundle requires unique module names and its entry"));
    }
    let original_source_digest = checked_source_digest(&sources, entry);
    let (checked, _) = link_checked_with(
        parse_sources(&sources)?, entry, expand, LinkMode::Test,
        Some(user_modules), false, Some(owners),
    )?;
    Ok(CheckedTestSuite {
        sources, entry: entry.to_owned(), user_modules: user_modules.clone(),
        checked, original_source_digest,
    })
}

/// No user AST, recipe body, ordinal, or digest is accepted. The selected name
/// must resolve to one raw declaration in this suite's OWN entry source. The
/// compiler constructs the only permitted recipe and rechecks it from scratch.
pub fn check_bare_test_driver(
    suite: &CheckedTestSuite,
    selected: &str,
    expand: ComptimeExpander,
) -> Result<CheckedTestDriverModule, PipelineError> {
    let prefix = format!("{}.", suite.entry);
    let bare = selected.strip_prefix(&prefix)
        .filter(|name| name.starts_with("test_") && !name.contains('.'))
        .ok_or_else(|| refused("bare test target must be a test declared in the entry source"))?;
    let mut modules = parse_sources(&suite.sources)?;
    let (_, entry) = modules.iter_mut().find(|(name, _)| name == &suite.entry)
        .ok_or_else(|| refused("test entry source is missing"))?;
    let targets = entry.items.iter().enumerate().filter_map(|(ordinal, item)| match item {
        Item::Function(function) if function.name == bare => Some((ordinal, function)),
        _ => None,
    }).collect::<Vec<_>>();
    if targets.len() != 1 {
        return Err(refused(format!("unknown or ambiguous entry test `{selected}`")));
    }
    let (ordinal, target) = targets[0];
    if target.is_gen || target.comptime_only || !target.params.is_empty()
        || !target.bounds.is_empty()
        || target.ret.as_ref().is_some_and(|ty| !matches!(ty, Type::Named(name, args) if name == "Nil" && args.is_empty()))
    {
        return Err(refused(format!("bare test `{selected}` requires an ordinary or async nullary Nil declaration; generator, parameterized and generic targets are unsupported")));
    }
    let is_async = target.is_async;
    let declaration_ordinal = u64::try_from(ordinal)
        .map_err(|_| refused("test declaration ordinal overflow"))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"witchy.checked-test-driver.test-mode.bare\0");
    hasher.update(&BARE_RECIPE_VERSION.to_le_bytes());
    hasher.update(&suite.original_source_digest);
    hasher.update(&(suite.entry.len() as u64).to_le_bytes());
    hasher.update(suite.entry.as_bytes());
    hasher.update(&declaration_ordinal.to_le_bytes());
    hasher.update(&(selected.len() as u64).to_le_bytes());
    hasher.update(selected.as_bytes());
    let identity = TestDriverIdentity {
        recipe_version: BARE_RECIPE_VERSION,
        original_source_digest: suite.original_source_digest,
        derived_source_digest: *hasher.finalize().as_bytes(),
        selected_declaration: selected.to_owned(), declaration_ordinal,
    };

    // This fixed compiler template is not user input. Use the local source
    // declaration before linking, never a guessed post-link callable spelling.
    let recipe = if is_async {
        "fn main() -> Nil:\n    task.run(Nil)\n"
    } else {
        "fn main() -> Nil:\n    Nil\n"
    };
    let mut generated = witchy_syntax::parser::parse_module(recipe)
        .map_err(|error| refused(format!("invalid compiler test recipe: {error}")))?;
    let Some(Item::Function(main)) = generated.items.iter_mut()
        .find(|item| matches!(item, Item::Function(function) if function.name == "main")) else {
        return Err(refused("invalid compiler test recipe entry"));
    };
    main.line = 0;
    let target_call = Expr::Call { name: bare.to_owned(), args: Vec::new() };
    if is_async {
        let [Stmt::Expr(Expr::Call { args, .. })] = main.body.stmts.as_mut_slice() else {
            return Err(refused("invalid compiler async test recipe"));
        };
        *args = vec![target_call];
    } else {
        main.body.stmts = vec![Stmt::Expr(target_call)];
    }
    main.body.lines = vec![0];
    // Retain source-line alignment while replacing only the source entry main.
    for index in (0..entry.items.len()).rev() {
        if matches!(&entry.items[index], Item::Function(function) if function.name == "main") {
            entry.items.remove(index);
            if index < entry.item_lines.len() { entry.item_lines.remove(index); }
        }
    }
    entry.items.extend(generated.items);
    entry.item_lines.push(0);
    let (checked, typed) = link_checked_with(
        modules, &suite.entry, expand, LinkMode::Test, Some(&suite.user_modules),
        true, suite.checked.module_owners.clone(),
    )?;
    let typed = typed.ok_or_else(|| refused("test recipe check did not return typed source"))?;
    let main = typed.module().items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == "main" => Some(function),
        _ => None,
    }).ok_or_else(|| refused("checked test recipe entry missing"))?;
    let [Stmt::Expr(expression)] = main.body.stmts.as_slice() else {
        return Err(refused("checked test recipe entry shape changed"));
    };
    let invocation = if is_async {
        let Expr::Call { args, .. } = expression else {
            return Err(refused("checked async test driver must run its task"));
        };
        let [invocation] = args.as_slice() else {
            return Err(refused("checked async test driver task binding changed"));
        };
        invocation
    } else {
        expression
    };
    let Expr::Call { name, args } = invocation else {
        return Err(refused("checked test recipe call shape changed"));
    };
    if name != selected || !args.is_empty() {
        return Err(refused("checked test recipe target binding changed"));
    }
    let mut checked = finish_canonical_source(checked, typed, identity.derived_source_digest)?;
    let runtime_catalog = Some(checked.runtime_declaration_catalog()?);
    let canonical_source = checked.canonical_source.take();
    Ok(CheckedTestDriverModule {
        module: checked.linked.module, runtime_catalog, canonical_source,
        identity: Some(identity),
    })
}
