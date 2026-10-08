#[test]
fn closed_select_companion_is_ordinary_checked_source() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let source = include_str!("../../../../benchmarks/select_fanin.witchy");
    let checked = super::source::checked_module(source).expect("benchmark source");
    let view = checked.canonical_source().expect("canonical source").view();
    let receipt = view.scalar_region().expect("production selected region");
    assert_eq!(receipt.kind(), witchy_canonical_source::scalar_region::RegionKind::SelectDrain);
    assert_eq!(receipt.covered_awaits().len(), 5);
    assert!(receipt.validate_checked_source(view.module(), view.task_await_proofs(), view.source_witness()));
    witchy_types::pipeline::assert_closed_select_receipt_rejects_forgery(&checked);
    let function = witchy_types::pipeline::inspect_closed_select_region(&checked).expect("closed select source recipe");
    witchy_syntax::ast::visit::visit_expr(&witchy_syntax::ast::Expr::Block(function.body.clone()), &mut |expression| {
        assert!(!matches!(expression, witchy_syntax::ast::Expr::Call { name, .. } if name.starts_with("chan.") || name.starts_with("task.")));
        Ok::<_, ()>(())
    }).expect("ordinary hot path has no generic channel/task calls");
    let mut module = checked.module().clone();
    module.items.push(witchy_syntax::ast::Item::Function(function));
    witchy_types::typeck::annotate_checked_source(module).expect("ordinary source/type/ownership checks");
}

#[test]
fn closed_select_refuses_unsafe_arithmetic_scope_escape_and_shadowing() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let source = include_str!("../../../../benchmarks/select_fanin.witchy");
    for changed in [
        source.replace("% 1000000007", "% 0"),
        source.replace("% 1000000007", "/ 1000000007"),
        source.replace("% 1000000007", "% (seen_a + 1)"),
        source.replace("checksum * 257", "checksum << 1"),
        source.replace("    let (a_tx,", "    let select_region_pending_0 = 0\n    let (a_tx,"),
        source.replace("        chan.send(tx, payload).await", "        chan.send(tx, payload).await\n        chan.send(tx, payload).await"),
    ] {
        let changed = if changed.contains("% (seen_a + 1)") {
            changed.replace("checksum = (checksum * 257 + i * 2 + 1) % (seen_a + 1)", "checksum = (checksum * 257 + i * 2 + 1) % 1000000007")
                .replace("checksum = (checksum * 257 + i * 2 + 3) % (seen_a + 1)", "checksum = (checksum * 257 + i * 2 + 3) % 1000000007")
        } else { changed };
        let checked = super::source::checked_module(&changed).expect("ordinary altered source remains valid");
        assert!(checked.canonical_source().unwrap().view().scalar_region().is_none(), "{changed}");
    }
}

#[test]
fn closed_select_refuses_changed_preparation_decoder_and_priority_contracts() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let source = include_str!("../../../../benchmarks/select_fanin.witchy");
    let authentic = super::source::checked_module(source).expect("authentic select source");
    witchy_types::pipeline::assert_closed_select_rejects_changed_contract(&authentic);
    for (module, before, after) in [
        ("chan", "task.__channel_select2(ia, ib, fn(k, o): select2_result(k, o))", "task.__channel_select2(ib, ia, fn(k, o): select2_result(k, o))"),
        ("task", "Pull2Ready(a, b, _) -> [a, b]", "Pull2Ready(a, b, _) -> [b, a]"),
        ("chan", "resume(select2_frame_decode(0, None), base, state)", "resume(select2_frame_decode(1, None), base, state)"),
    ] {
        let original = witchy_syntax::linker::std_source(module).unwrap();
        assert!(original.contains(before));
        let changed = original.replace(before, after);
        match super::source::checked_bundle_with_std_override(vec![("main", source)], "main", Some((module, &changed))) {
            Ok(checked) => assert!(checked.canonical_source().unwrap().view().scalar_region().is_none()),
            Err(error) => assert!(error.contains("requires the exact checked standard select implementation"), "{error}"),
        }
    }
}
