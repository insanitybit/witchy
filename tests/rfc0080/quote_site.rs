//! Quote location is parser-owned metadata, independent of evaluator order.

#[test]
fn malformed_quote_site_is_refused_before_using_payload() {
    use witchy::ast::{Expr, Item};
    let source = "import meta\ncomptime fn generated() -> meta.ItemSyntax:\n    quote item:\n        fn answer() -> Int:\n            42\ncomptime:\n    emit_item(generated())\n";
    for line in [0, -1, i64::from(u32::MAX) + 1] {
        let mut module = witchy::parser::parse_module(source).expect("parse typed quote");
        let mut changed = 0;
        for item in &mut module.items {
            if let Item::Function(function) = item {
                witchy::ast::visit::visit_block_mut::<()>(&mut function.body, &mut |expression| {
                    if let Expr::Call { name, args } = expression
                        && name == witchy_syntax::intrinsics::COMPILER_QUOTE_ITEM {
                        *args.last_mut().expect("parser-issued site") = Expr::Int(line);
                        changed += 1;
                    }
                    Ok(())
                }).unwrap();
            }
        }
        assert_eq!(changed, 1);
        let error = witchy_interp::comptime::expand_compile_time("main", &mut module, &[])
            .expect_err("invalid quote-site metadata cannot borrow evaluator line state");
        assert!(error.contains("compiler quotation source line is out of range"), "{error}");
    }
}

#[test]
fn quotation_origin_is_lexical_not_previous_branch_execution() {
    for (mode, condition) in [("", "true"), ("", "false"), ("mode opt\n", "true"), ("mode opt\n", "false")] {
        let source = r#"import meta
comptime fn generated() -> meta.ItemSyntax:
    let pair = (if CONDITION:
        1
    else:
        0
    , quote expr:
        42
    )
    let (_, expression) = pair
    quote item:
        fn answer() -> Int:
            ${expression}
comptime:
    emit_item(generated())
fn main() -> Int:
    answer()
"#.replace("CONDITION", condition);
        let source = format!("{mode}{source}");
        let quote_line = if mode.is_empty() { 7 } else { 8 };
        let mut module = witchy::parser::parse_module(&source).expect("parse origin witness");
        assert_eq!(module.compiler_expr_syntax[0].definition_line, quote_line);
        let origins = witchy_interp::comptime::expand_compile_time("main", &mut module, &[])
            .expect("expand source-backed quotation");
        let holes = origins.nodes().iter().flat_map(|node| &node.origin.hole_ancestry).collect::<Vec<_>>();
        assert!(!holes.is_empty(), "actual emitted syntax retains hole provenance");
        for hole in holes {
            assert_eq!(hole.definition.start.line, quote_line, "{mode:?} {condition}: definition is the quote site, never branch line4/6");
        }
    }
}

#[test]
fn identical_quote_payloads_keep_distinct_site_origins() {
    struct ResetOptimization;
    impl Drop for ResetOptimization {
        fn drop(&mut self) { witchy::opt::set_for_tests(None); }
    }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    let source = "import meta\ncomptime fn generated() -> meta.ItemSyntax:\n    let a = quote expr:\n        42\n    let b = quote expr:\n        42\n    quote item:\n        fn answer() -> Int:\n            ${a} + ${b}\ncomptime:\n    emit_item(generated())\nfn main() -> Int:\n    answer()\n";
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}{source}");
        let mut module = witchy::parser::parse_module(&source).expect("parse distinct quote sites");
        assert_eq!(module.compiler_expr_syntax.len(), 2);
        assert_eq!(module.compiler_expr_syntax[0].handle, module.compiler_expr_syntax[1].handle,
            "identical payloads retain deduplication independent of site identity");
        let origins = witchy_interp::comptime::expand_compile_time("main", &mut module, &[]).expect("expand typed quotes");
        let lines = origins.nodes().iter().flat_map(|node| &node.origin.hole_ancestry)
            .map(|hole| hole.definition.start.line).collect::<std::collections::BTreeSet<_>>();
        let offset = u32::from(!mode.is_empty());
        assert_eq!(lines, [3 + offset, 5 + offset].into_iter().collect());
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), source.into_bytes())], "main", witchy_interp::comptime::EXPANDER,
        ).expect("generated AST re-enters ordinary source checking");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admitted unoptimized generated expression");
        let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
            print_int: true, quiet: true, ..witchy::runtime::Capabilities::none()
        }, 64).expect("admitted generated program");
        vm.run().expect("execute generated program");
        assert_eq!(vm.output(), ["84"], "42 + 42 is independently expected");
        assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
    }
}
