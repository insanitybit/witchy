//! Integration recipe v1: real grants are a launch concern, never source proof.
//! Select one declaration from retained entry bytes, generate an explicit Unit
//! entry, and rederive every witness through the ordinary source pipeline.
use super::*;
use crate::typeck::Ty;
use witchy_syntax::ast::Convention;

pub fn check_integration_test_driver(
    suite: &CheckedTestSuite, selected: &str, expand: ComptimeExpander,
) -> Result<CheckedTestDriverModule, PipelineError> {
    let prefix = format!("{}.", suite.entry);
    let bare = selected.strip_prefix(&prefix)
        .filter(|name| name.starts_with("test_") && !name.contains('.'))
        .ok_or_else(|| refused("integration target must be a test declared in the entry source"))?;
    let mut modules = parse_sources(&suite.sources)?;
    let (_, entry) = modules.iter_mut().find(|(name, _)| name == &suite.entry)
        .ok_or_else(|| refused("integration entry source is missing"))?;
    let targets = entry.items.iter().enumerate().filter_map(|(ordinal, item)| match item {
        Item::Function(function) if function.name == bare => Some((ordinal, function)),
        _ => None,
    }).collect::<Vec<_>>();
    if targets.len() != 1 {
        return Err(refused(format!("unknown or ambiguous entry integration test `{selected}`")));
    }
    let (ordinal, target) = targets[0];
    if target.is_gen || target.comptime_only || !target.bounds.is_empty()
        || target.params.iter().any(|parameter| parameter.ty.is_none()
            || parameter.convention != Convention::Let || parameter.default.is_some())
    {
        return Err(refused("integration recipe requires explicitly typed default-convention capability parameters; generators, comptime, generic, defaulted and other conventions are unsupported"));
    }
    let parameters = target.params.clone();
    let asynchronous = target.is_async;
    let declaration_ordinal = u64::try_from(ordinal)
        .map_err(|_| refused("integration declaration ordinal overflow"))?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"witchy.checked-test-driver.test-mode.integration\0");
    hash.update(&1u32.to_le_bytes());
    hash.update(&suite.original_source_digest);
    hash.update(&(suite.entry.len() as u64).to_le_bytes());
    hash.update(suite.entry.as_bytes());
    hash.update(&declaration_ordinal.to_le_bytes());
    hash.update(&(selected.len() as u64).to_le_bytes());
    hash.update(selected.as_bytes());
    let identity = TestDriverIdentity {
        recipe_version: 1, original_source_digest: suite.original_source_digest,
        derived_source_digest: *hash.finalize().as_bytes(),
        selected_declaration: selected.to_owned(), declaration_ordinal,
    };
    let template = if asynchronous {
        "fn main() -> ():\n    task.run(())\n"
    } else {
        "fn main() -> ():\n    ()\n"
    };
    let mut generated = witchy_syntax::parser::parse_module(template)
        .map_err(|error| refused(format!("invalid compiler integration recipe: {error}")))?;
    let Item::Function(main) = &mut generated.items[0] else {
        return Err(refused("invalid compiler integration recipe entry"));
    };
    main.line = 0;
    main.params = parameters;
    let invocation = Expr::Call {
        name: bare.to_owned(),
        args: main.params.iter().map(|parameter| Expr::Var(parameter.name.clone())).collect(),
    };
    if asynchronous {
        // Async lowering imports the authenticated task module and drives this
        // source call. Merely constructing a Task would falsely pass a test.
        let [Stmt::Expr(Expr::Call { args, .. })] = main.body.stmts.as_mut_slice() else {
            return Err(refused("invalid compiler async integration recipe"));
        };
        *args = vec![invocation];
    } else {
        main.body.stmts = vec![Stmt::Expr(invocation)];
    }
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
    let typed = typed.ok_or_else(|| refused("integration check did not return typed source"))?;
    let main = typed.module().items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == "main" => Some(function), _ => None,
    }).ok_or_else(|| refused("checked integration recipe entry missing"))?;
    let [Stmt::Expr(expression)] = main.body.stmts.as_slice() else {
        return Err(refused("checked integration recipe entry shape changed"));
    };
    let invocation = if asynchronous {
        let Expr::Call { args, .. } = expression else {
            return Err(refused("checked async integration driver must run its task"));
        };
        let [invocation] = args.as_slice() else {
            return Err(refused("checked async integration driver task binding changed"));
        };
        invocation
    } else { expression };
    let Expr::Call { name, args } = invocation else {
        return Err(refused("checked integration recipe call shape changed"));
    };
    if name != selected || args.len() != main.params.len() {
        return Err(refused("checked integration recipe target or parameter binding changed"));
    }
    for (argument, parameter) in args.iter().zip(&main.params) {
        if !matches!(argument, Expr::Var(name) if name == &parameter.name)
            || !matches!(typed.table().type_of(argument), Some(Ty::Console(_) | Ty::Dir(_) | Ty::Net(_)))
        {
            return Err(refused("integration recipe requires exact checked Console, Dir or Net parameters; data, records and other capabilities are unsupported"));
        }
    }
    let mut checked = finish_canonical_source(checked, typed, identity.derived_source_digest, PrecompiledDeclarationEnvironment::default())?;
    let runtime_catalog = Some(checked.runtime_declaration_catalog()?);
    let canonical_source = checked.canonical_source.take()
        .ok_or_else(|| refused("integration recipe canonical source missing"))?;
    Ok(CheckedTestDriverModule {
        module: checked.linked.module, runtime_catalog,
        canonical_source: Some(canonical_source), identity: Some(identity),
    })
}
