use witchy_runtime::runtime::{Capabilities, Runtime, comptime::ComptimeLimits};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

fn owners() -> AuthenticatedModuleOwners {
    owners_for("comptime")
}

fn owners_for(module: &str) -> AuthenticatedModuleOwners {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/comptime", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let mut assignments = vec![(module.into(), ModuleLoadIdentity::new(workspace, [module]).unwrap())];
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
fn canonical_dynamic_expression_leaves_are_typed_and_session_bounded() {
    use witchy_syntax::ast::Expr;
    let source = "import meta\nfn main() -> Int:\n    emit_expr(meta.expr_int(42))\n    emit_expr(meta.expr_bool(true))\n    emit_expr(meta.expr_string(\"hello\"))\n    0\n";
    let recipe = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
        vec![("comptime".into(), source.as_bytes().to_vec())], "dynamic-leaves", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners(),
    ).expect("checked expression constructors");
    let artifact = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
        .expect_lowered("admitted expression constructors");
    let first = Runtime::run_comptime(&artifact, limits()).expect("host-owned expression leaves");
    let second = Runtime::run_comptime(&artifact, limits()).expect("fresh deterministic session");
    assert_eq!(first.result, 0);
    assert_eq!(first.heap_leaks, 0);
    assert_eq!(first.expressions, second.expressions);
    assert!(matches!(first.expressions.as_slice(), [a, b, c]
        if matches!(a.expression, Expr::Int(42))
            && matches!(b.expression, Expr::Bool(true))
            && matches!(&c.expression, Expr::Str(value) if value == "hello")));
    assert!(Runtime::run_comptime(&artifact, ComptimeLimits { syntax_nodes: 0, ..limits() }).is_err());
}

#[test]
fn canonical_type_quotation_uses_checked_type_seed_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}import meta\nfn main() -> Int:\n    let ty = quote type:\n        List(Int)\n    42\n");
        let recipe = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
            vec![("comptime".into(), source.into_bytes())], "typed-quotation",
            witchy_interp::comptime::EXPANDER,
            &["comptime".to_owned()].into_iter().collect(), owners(),
        ).expect("checked type quotation");
        let artifact = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
            .expect_lowered("admitted type quotation");
        let output = Runtime::run_comptime(&artifact, limits()).expect("host-owned type seed");
        assert_eq!(output.result, 42);
        assert_eq!(output.heap_leaks, 0);
        assert!(output.items.is_empty() && output.expressions.is_empty());
    }
}

#[test]
fn admitted_comptime_builds_a_record_from_checked_type_syntax() {
    struct ResetOptimization;
    impl Drop for ResetOptimization { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}import meta\ncomptime:\n    let field_ty = quote type:\n        Int\n    let item = meta.record_type(meta.ident(\"Builder\"), [(meta.ident(\"value\"), field_ty)])\n    emit_item(item)\nfn main(console: Console):\n    let built = Builder(42)\n    console.print(\"${{built.value}}\")\n");
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes())], "generator",
            ["generator".to_owned()].into_iter().collect(), owners_for("generator"), limits(),
        ).expect("typed record enters destination checker");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("generated record admits on canonical Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["42"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn admitted_reflected_type_conversion_supplies_checked_record_fields() {
    struct ResetOptimization;
    impl Drop for ResetOptimization { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}import meta\ncomptime:\n    let field_ty = meta.type_expr(meta.TNamed(\"Int\", []))\n    emit_item(meta.record_type(meta.ident(\"Builder\"), [(meta.ident(\"value\"), field_ty)]))\nfn main(console: Console):\n    let value = Builder(42)\n    console.print(\"${{value.value}}\")\n");
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes())], "generator",
            ["generator".to_owned()].into_iter().collect(), owners_for("generator"), limits(),
        ).expect("reflected type syntax enters destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("generated record admits on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["42"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn admitted_typed_parameters_build_a_checked_function_in_both_modes() {
    struct ResetOptimization;
    impl Drop for ResetOptimization { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}import meta\ncomptime:\n    let int_ty = quote type:\n        Int\n    let x = meta.param(meta.ident(\"x\"), int_ty)\n    emit_item(meta.function(true, meta.ident(\"answer\"), [x], Some(int_ty), meta.expr_int(42)))\nfn main(console: Console):\n    console.print(\"${{answer(0)}}\")\n");
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes())], "generator",
            ["generator".to_owned()].into_iter().collect(), owners_for("generator"), limits(),
        ).expect("generated function enters destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("generated function admits on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["42"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn admitted_typed_inherent_method_dispatches_in_both_modes() {
    struct ResetOptimization;
    impl Drop for ResetOptimization { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}import meta\ntype Counter:\n    value: Int\ncomptime:\n    let counter_ty = quote type:\n        Counter\n    let int_ty = quote type:\n        Int\n    let self_param = meta.param_own(meta.ident(\"self\"), counter_ty)\n    let method = meta.function(true, meta.ident(\"answer\"), [self_param], Some(int_ty), meta.expr_int(42))\n    emit_item(meta.inherent_impl(counter_ty, [method]))\nfn main(console: Console):\n    let c = Counter(0)\n    console.print(\"${{c.answer()}}\")\n");
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes())], "generator",
            ["generator".to_owned()].into_iter().collect(), owners_for("generator"), limits(),
        ).expect("generated inherent method enters destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("generated method admits on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["42"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn admitted_expression_composition_preserves_typed_children_in_both_modes() {
    use witchy_syntax::ast::Expr;
    struct ResetOptimization;
    impl Drop for ResetOptimization { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}import meta\nfn main() -> Int:\n    let base = quote expr:\n        p\n    let field = meta.expr_field(base, meta.ident(\"value\"))\n    let record = meta.expr_record(meta.ident(\"Box\"), [(meta.ident(\"value\"), field)])\n    emit_expr(meta.expr_named_call(meta.ident(\"consume\"), [record]))\n    let factory = quote expr:\n        factory\n    emit_expr(meta.expr_call(factory, [meta.expr_int(7)]))\n    emit_expr(meta.expr_try(meta.expr_field(base, meta.ident(\"optional\"))))\n    0\n");
        let recipe = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
            vec![("comptime".into(), source.into_bytes())], "expression",
            witchy_interp::comptime::EXPANDER,
            &["comptime".to_owned()].into_iter().collect(), owners(),
        ).expect("composition is checked as compiler-owned syntax");
        let artifact = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
            .expect_lowered("composition admits on unoptimized Wasm");
        let output = Runtime::run_comptime(&artifact, limits()).expect("compose typed syntax");
        assert_eq!(output.result, 0);
        assert_eq!(output.heap_leaks, 0);
        assert_eq!(output.expressions.len(), 3);
        let Expr::Call { name, args } = &output.expressions[0].expression else { panic!("expected direct call") };
        assert_eq!(name, "consume");
        let [Expr::Record { name, fields, spread: None }] = args.as_slice() else { panic!("expected record") };
        assert_eq!(name, "Box");
        let [(field, Expr::Field { base, field: member })] = fields.as_slice() else { panic!("expected field") };
        assert_eq!((field.as_str(), member.as_str()), ("value", "value"));
        assert!(matches!(base.as_ref(), Expr::Var(name) if name == "p"));
        assert!(matches!(&output.expressions[1].expression, Expr::Apply { func, args }
            if matches!(func.as_ref(), Expr::Var(name) if name == "factory")
            && matches!(args.as_slice(), [Expr::Int(7)])));
        assert!(matches!(&output.expressions[2].expression, Expr::Try(value)
            if matches!(value.as_ref(), Expr::Field { base, field }
                if field == "optional" && matches!(base.as_ref(), Expr::Var(name) if name == "p"))));
    }
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

#[test]
fn canonical_session_executes_only_its_first_original_block() {
    use witchy_types::pipeline::ComptimeSourceSession;
    struct ResetOptimization;
    impl Drop for ResetOptimization { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}fn helper() -> Int:\n    40\ncomptime:\n    let expression = quote expr:\n        40 + 2\n    if helper() == 40:\n        emit_expr(expression)\nfn main() -> Int:\n    99\ncomptime:\n    fail(\"later block must not execute\")\n");
        let session = ComptimeSourceSession::from_source_bundle(
            vec![("generator".into(), source.as_bytes().to_vec())], "generator",
            ["generator".to_owned()].into_iter().collect(), owners_for("generator"),
        ).expect("exact original compiler input");
        for index in [0, 2, 3, usize::MAX] {
            let error = session.check_block(index)
                .expect_err("not an initial block with authenticated empty prior output");
            assert!(error.to_string().contains("first original block"), "{error}");
        }
        let recipe = session.check_block(1)
            .expect("source-selected first block, no interpreted execution of either block");
        let replay = session.check_block(1).unwrap();
        assert_eq!(recipe.canonical_source().source_witness().source_bundle_digest(),
            replay.canonical_source().source_witness().source_bundle_digest());
        let artifact = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
            .expect_lowered("admitted unoptimized original block");
        let output = Runtime::run_comptime(&artifact, limits()).expect("canonical block execution");
        assert_eq!(output.result, 0, "compiler entry returns zero, not original runtime main's99");
        assert_eq!(output.heap_leaks, 0);
        assert_eq!(output.expressions.len(), 1);
        assert_eq!(output.expressions[0].origin.definition_line, 4 + u32::from(!mode.is_empty()));
        let checked = check_emitted(output.expressions.into_iter().next().unwrap()).unwrap();
        let generated = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("ordinary destination source rechecks the returned expression");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&generated, Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["42"], "independent expectation: 40 + 2");
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
        assert!(Runtime::run_comptime(&artifact, ComptimeLimits { fuel: 0, ..limits() }).is_err());
        assert!(Runtime::batch_quick().unwrap().spawn_admitted(&artifact, Capabilities::none(), 64).is_err());
    }
}

#[test]
fn canonical_block_session_binds_raw_source_and_refuses_missing_owner() {
    use witchy_types::pipeline::ComptimeSourceSession;
    let checked = |source: &str| {
        ComptimeSourceSession::from_source_bundle(vec![("generator".into(), source.as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners_for("generator"))
            .unwrap().check_block(0).unwrap()
    };
    let source = "comptime:\n    let _ = 1\nfn main() -> Int:\n    99\n";
    let first = checked(source);
    let changed = checked(&source.replace("99", "100"));
    assert_ne!(first.canonical_source().source_witness().source_bundle_digest(),
        changed.canonical_source().source_witness().source_bundle_digest(),
        "identity binds all raw bytes, including runtime-only source");
    let missing = ComptimeSourceSession::from_source_bundle(vec![("generator".into(), source.as_bytes().to_vec())],
        "generator", ["generator".to_owned()].into_iter().collect(), owners());
    assert!(missing.is_err(), "another module's loader identity cannot substitute for the selected source");

    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/comptime", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let assignments = ["generator", "sibling"].into_iter().map(|name| (
        name.to_owned(), ModuleLoadIdentity::new(workspace.clone(), [name]).unwrap(),
    )).chain(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
    )));
    let sibling = ComptimeSourceSession::from_source_bundle(vec![
        ("generator".into(), b"import sibling\ncomptime:\n    let _ = sibling.value()\n".to_vec()),
        ("sibling".into(), b"comptime:\n    fail(\"must never execute a sibling block\")\npub fn value() -> Int:\n    7\n".to_vec()),
    ], "generator", ["generator".into(), "sibling".into()].into_iter().collect(),
        AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap()).unwrap();
    let error = sibling.check_block(0).expect_err("no injected callback may execute or mutate sibling source");
    assert!(error.to_string().contains("requires authenticated prior expansion"), "{error}");
}

#[test]
fn canonical_block_session_cannot_retarget_the_displaced_entry() {
    use witchy_types::pipeline::ComptimeSourceSession;
    let check = |source: &str, item| {
        ComptimeSourceSession::from_source_bundle(vec![("generator".into(), source.as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners_for("generator"))
            .unwrap().check_block(item)
    };
    for mode in ["", "mode opt\n"] {
        for (source, item) in [
            ("comptime:\n    let _ = main()\n", 0),
            ("comptime:\n    let _ = main()\nfn main() -> Int:\n    99\n", 0),
            ("fn helper() -> Int:\n    main()\ncomptime:\n    let _ = helper()\nfn main() -> Int:\n    99\n", 1),
            ("comptime:\n    let callback = main\n    let _ = callback()\nfn main() -> Int:\n    99\n", 0),
        ] {
            let error = check(&format!("{mode}{source}"), item)
                .expect_err("source entry identity cannot become the compiler entry");
            assert!(error.to_string().contains("comptime.main"), "resolved entry reference: {error}");
        }
        check(&format!("{mode}comptime:\n    let main = fn() -> Int:\n        7\n    let _ = main()\nfn main() -> Int:\n    99\n"), 0)
            .expect("a local callable named main does not reference the displaced declaration");
    }
}
