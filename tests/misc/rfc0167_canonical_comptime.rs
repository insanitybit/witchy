use witchy_runtime::runtime::{Capabilities, Runtime, comptime::ComptimeLimits};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

fn owners() -> AuthenticatedModuleOwners {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/comptime", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let mut assignments = vec![("comptime".into(), ModuleLoadIdentity::new(workspace, ["comptime"]).unwrap())];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
    )));
    AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap()
}

#[test]
fn canonical_comptime_requires_loader_authenticated_meta_identity() {
    let forged = PackageCoordinate::new(PackageSource::Workspace, "witchy/std", "1").unwrap();
    let owners = AuthenticatedModuleOwners::from_loader_assignments([
        ("comptime".to_owned(), ModuleLoadIdentity::new(forged.clone(), ["comptime"]).unwrap()),
        ("meta".to_owned(), ModuleLoadIdentity::new(forged, ["std", "meta"]).unwrap()),
    ]).unwrap();
    let error = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
        vec![("comptime".into(), b"fn main() -> Int:\n    42\n".to_vec())], "scope", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners,
    ).expect_err("a same-spelled workspace module is not toolchain authority");
    assert!(error.to_string().contains("requires toolchain witchy/std meta ownership"), "{error}");
}

#[test]
fn canonical_comptime_refuses_unmetered_host_services() {
    let source = "fn main() -> Int:\n    let text = \"${1.25}\"\n    if text == \"1.25\":\n        42\n    else:\n        0\n";
    let recipe = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
        vec![("comptime".into(), source.as_bytes().to_vec())], "bounded-service", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners(),
    ).expect("valid compiler-service source");
    let rejection = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
        .expect_rejected("unmetered host computation must not escape deterministic compiler budget");
    assert!(rejection.to_string().contains("no deterministic compiler-session budget contract"), "{rejection}");
}

#[test]
fn canonical_fresh_names_execute_with_session_and_budget_boundaries() {
    let source = "import meta\nfn main() -> Int:\n    let a = meta.fresh(\"x\")\n    let b = meta.fresh(\"x\")\n    if a == b:\n        0\n    else:\n        42\n";
    let recipe = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
        vec![("comptime".into(), source.as_bytes().to_vec())], "a/b", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners(),
    ).expect("source-backed compiler recipe");
    let artifact = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
        .expect_lowered("admitted unoptimized compiler recipe");
    let limits = limits();
    let first = Runtime::run_comptime(&artifact, limits).expect("actual admitted compiler execution");
    let second = Runtime::run_comptime(&artifact, limits).expect("fresh deterministic session");
    assert_eq!(first.result, 42); // distinct issuance ordinals, not an interpreter oracle
    assert_eq!(first.heap_leaks, 0);
    assert_eq!(first.fresh_identifiers.len(), 2);
    assert_eq!(first.fresh_identifiers, ["__witchy_fresh_612f62_0_x", "__witchy_fresh_612f62_1_x"]);
    assert_ne!(first.fresh_identifiers[0], first.fresh_identifiers[1]);
    assert_eq!(first.fresh_identifiers, second.fresh_identifiers);
    assert_eq!(first.fuel_consumed, second.fuel_consumed);
    assert!(first.fuel_consumed > 0);
    assert!(Runtime::batch_quick().unwrap().spawn_admitted(&artifact, Capabilities::none(), 64).is_err());
    assert!(Runtime::batch_quick().unwrap().spawn_admitted_with_external_libraries(&artifact, Capabilities::none(), 64, &[]).is_err());
    assert!(Runtime::run_comptime(&artifact, ComptimeLimits { fuel: 0, ..limits }).is_err());
    assert!(Runtime::run_comptime(&artifact, ComptimeLimits { fresh_identifiers: 1, ..limits }).is_err());
    assert!(Runtime::run_comptime(&artifact, ComptimeLimits { host_output_bytes: 1, ..limits }).is_err());

    // The same valid typed body, without recipe authority, cannot introduce a
    // compiler-only host call by claiming its ABI alone.
    let ordinary = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("comptime".into(), source.as_bytes().to_vec())], "comptime", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners(),
    ).expect("ordinary checked source");
    let rejection = witchy_lower::codegen::compile_checked_module_artifact(&ordinary)
        .expect_rejected("compiler imports require source-bound recipe authority");
    assert!(rejection.to_string().contains("checked comptime launch contract"), "{rejection}");
}

fn limits() -> ComptimeLimits {
    ComptimeLimits { fuel: 2_000_000, memory_pages: 64, fresh_identifiers: 2, host_output_bytes: 4096,
        syntax_nodes: 10_000, syntax_bytes: 100_000, emitted_nodes: 10_000, emitted_bytes: 100_000 }
}

#[test]
fn canonical_quotation_emits_typed_expression_with_lexical_origin() {
    struct ResetOptimization;
    impl Drop for ResetOptimization { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    for mode in ["", "mode opt\n"] {
    let source = format!("{mode}import meta\nfn main() -> Int:\n    let first = quote expr:\n        40 + 2\n    let second = quote expr:\n        40 + 2\n    emit_expr(first)\n    emit_expr(second)\n    0\n");
    let ordinary = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("comptime".into(), source.as_bytes().to_vec())], "comptime", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners()).expect_err("ordinary source paths install no compiler service declarations");
    assert!(ordinary.to_string().contains("unknown function `emit_expr`"), "{ordinary}");
    let recipe = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
        vec![("comptime".into(), source.as_bytes().to_vec())], "expression", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners(),
    ).expect("source-backed typed quotation");
    let artifact = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
        .expect_lowered("admitted unoptimized quotation and emission");
    let first = Runtime::run_comptime(&artifact, limits()).expect("execute canonical quotation");
    let second = Runtime::run_comptime(&artifact, limits()).expect("fresh replay session");
    assert_eq!(first.result, 0);
    assert_eq!(first.heap_leaks, 0);
    assert_eq!(first.expressions.len(), 2);
    assert_eq!(first.expressions, second.expressions);
    assert_eq!(first.fuel_consumed, second.fuel_consumed);
    assert_eq!(first.expressions.iter().map(|value| value.origin.definition_line).collect::<Vec<_>>(),
        [3 + u32::from(!mode.is_empty()), 5 + u32::from(!mode.is_empty())],
        "deduplicated payloads produce distinct fresh instances and lexical origins");
    assert!(first.expressions[0].origin.hole_ancestry.is_empty());
    let checked = check_emitted(first.expressions[0].clone()).expect("typed emitted AST re-enters ordinary source checking");
    let generated = witchy_lower::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted generated AST");
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&generated, Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
    vm.run().expect("execute generated expression");
    assert_eq!(vm.output(), ["42"], "independent arithmetic oracle: 40 + 2");
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    for bounded in [ComptimeLimits { syntax_nodes: 0, ..limits() },
        ComptimeLimits { syntax_bytes: 0, ..limits() }, ComptimeLimits { emitted_nodes: 0, ..limits() },
        ComptimeLimits { emitted_bytes: 0, ..limits() }, ComptimeLimits { fuel: 0, ..limits() }] {
        assert!(Runtime::run_comptime(&artifact, bounded).is_err());
    }
    assert!(Runtime::batch_quick().unwrap().spawn_admitted(&artifact, Capabilities::none(), 64).is_err());
    }
}

thread_local! {
    static EMITTED: std::cell::RefCell<Option<witchy_syntax::compiler_session::seeds::EmittedExpression>> = const { std::cell::RefCell::new(None) };
}

fn check_emitted(emitted: witchy_syntax::compiler_session::seeds::EmittedExpression)
    -> Result<witchy_types::pipeline::CheckedModule, witchy_types::pipeline::PipelineError> {
    use witchy_syntax::{ast::{Item, Stmt}, origin::{ExpansionOrigin, SourceSpan}};
    fn expand(name: &str, module: &mut witchy_syntax::ast::Module, siblings: &[(String, witchy_syntax::ast::Module)])
        -> Result<witchy_syntax::origin::OriginTable, String> {
        let mut origins = witchy_interp::comptime::expand_compile_time(name, module, siblings)?;
        if name == "generated" {
            let emitted = EMITTED.with(|value| value.borrow().clone()).expect("host returned typed payload");
            let index = module.items.iter().position(|item| matches!(item, Item::Function(function) if function.name == "main")).unwrap();
            let Item::Function(function) = &mut module.items[index] else { unreachable!() };
            function.body.stmts = vec![Stmt::Expr(emitted.expression)];
            origins.record_item_tree(name, index, &module.items[index], ExpansionOrigin {
                definition: SourceSpan::line("comptime", emitted.origin.definition_line),
                invocation: SourceSpan::line(name, 2), hole_ancestry: Vec::new(),
            });
        }
        Ok(origins)
    }
    EMITTED.with(|slot| *slot.borrow_mut() = Some(emitted));
    let result = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("generated".into(), b"fn main() -> Int:\n    0\n".to_vec())], "generated",
        witchy_syntax::linker::ComptimeExpander::new(expand));
    EMITTED.with(|slot| *slot.borrow_mut() = None);
    result
}

#[test]
fn canonical_emission_is_not_type_authority_for_its_destination() {
    let source = "import meta\nfn main() -> Int:\n    let expression = quote expr:\n        \"not an Int\"\n    emit_expr(expression)\n    0\n";
    let recipe = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
        vec![("comptime".into(), source.as_bytes().to_vec())], "wrong-type", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners()).expect("quotation is data before insertion");
    let artifact = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe).expect_lowered("admitted quotation");
    let output = Runtime::run_comptime(&artifact, limits()).expect("typed syntax emission");
    assert_eq!(output.expressions.len(), 1);
    let error = check_emitted(output.expressions.into_iter().next().unwrap()).expect_err("String is not Int");
    assert!(error.to_string().contains("Int") && error.to_string().contains("String"), "{error}");
}
