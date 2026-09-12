//! Console-output recipe v1. Providers are deliberately not an input here.
//! The original source identity is retained; the separate recipe domain binds
//! its version and the exact selected raw declaration into every fresh witness.
use super::*;
use witchy_syntax::ast::Convention;
use crate::typeck::Ty;

pub fn check_console_fixture_test_driver(
    suite: &CheckedTestSuite, selected: &str, expand: ComptimeExpander,
) -> Result<CheckedTestDriverModule, PipelineError> {
    let prefix = format!("{}.", suite.entry);
    let bare = selected.strip_prefix(&prefix)
        .filter(|name| name.starts_with("test_") && !name.contains('.'))
        .ok_or_else(|| refused("Console fixture target must be a test declared in the entry source"))?;
    let mut modules = parse_sources(&suite.sources)?;
    let (_, entry) = modules.iter_mut().find(|(name, _)| name == &suite.entry)
        .ok_or_else(|| refused("fixture entry source is missing"))?;
    let targets = entry.items.iter().enumerate().filter_map(|(ordinal, item)| match item {
        Item::Function(function) if function.name == bare => Some((ordinal, function)),
        _ => None,
    }).collect::<Vec<_>>();
    if targets.len() != 1 { return Err(refused(format!("unknown or ambiguous entry fixture `{selected}`"))); }
    let (ordinal, target) = targets[0];
    if target.is_async || target.is_gen || target.comptime_only || !target.bounds.is_empty()
        || target.params.is_empty()
        || target.params.iter().any(|parameter| parameter.ty.is_none()
            || parameter.convention != Convention::Let || parameter.default.is_some())
    {
        return Err(refused("Console fixture recipe requires synchronous explicitly typed default-convention parameters; async, generic, defaulted and other conventions are unsupported"));
    }
    let parameters = target.params.clone();
    let declaration_ordinal = u64::try_from(ordinal).map_err(|_| refused("fixture declaration ordinal overflow"))?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"witchy.checked-test-driver.test-mode.console-output\0");
    hash.update(&1u32.to_le_bytes());
    hash.update(&suite.original_source_digest);
    hash.update(&(suite.entry.len() as u64).to_le_bytes());
    hash.update(suite.entry.as_bytes());
    hash.update(&declaration_ordinal.to_le_bytes());
    hash.update(&(selected.len() as u64).to_le_bytes());
    hash.update(selected.as_bytes());
    let identity = TestDriverIdentity {
        original_source_digest: suite.original_source_digest,
        derived_source_digest: *hash.finalize().as_bytes(),
        selected_declaration: selected.to_owned(), declaration_ordinal,
    };
    // Separate from the held bare recipe: this entry explicitly promises Nil.
    let mut generated = witchy_syntax::parser::parse_module("fn main() -> Nil:\n    Nil\n")
        .map_err(|error| refused(format!("invalid compiler Console recipe: {error}")))?;
    let Item::Function(main) = &mut generated.items[0] else {
        return Err(refused("invalid compiler Console recipe entry"));
    };
    main.line = 0;
    main.params = parameters;
    main.body.stmts = vec![Stmt::Expr(Expr::Call {
        name: bare.to_owned(),
        args: main.params.iter().map(|parameter| Expr::Var(parameter.name.clone())).collect(),
    })];
    main.body.lines = vec![0];
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
    let typed = typed.ok_or_else(|| refused("fixture check did not return typed source"))?;
    let main = typed.module().items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == "main" => Some(function), _ => None,
    }).ok_or_else(|| refused("checked Console recipe entry missing"))?;
    let [Stmt::Expr(Expr::Call { name, args })] = main.body.stmts.as_slice() else {
        return Err(refused("checked Console recipe call shape changed"));
    };
    if name != selected || args.len() != main.params.len() {
        return Err(refused("checked Console recipe target or parameter binding changed"));
    }
    for (argument, parameter) in args.iter().zip(&main.params) {
        if !matches!(argument, Expr::Var(name) if name == &parameter.name)
            || !matches!(typed.table().type_of(argument), Some(Ty::Console(rights)) if rights.write)
        {
            return Err(refused("Console fixture recipe requires exact checked Console Write parameters; records and other providers are unsupported"));
        }
    }
    let mut checked = finish_canonical_source(checked, typed, identity.derived_source_digest)?;
    let runtime_catalog = Some(checked.runtime_declaration_catalog()?);
    let mut canonical_source = checked.canonical_source.take()
        .ok_or_else(|| refused("Console recipe canonical source missing"))?;
    canonical_source.console_fixture = true;
    Ok(CheckedTestDriverModule {
        module: checked.linked.module, runtime_catalog,
        canonical_source: Some(canonical_source), identity: Some(identity),
    })
}
