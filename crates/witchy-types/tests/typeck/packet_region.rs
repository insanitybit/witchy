#[test]
fn closed_packet_recipe_is_checked_ordinary_source() {
    let source = include_str!("../../../../benchmarks/chan_scheduler.witchy");
    let checked = super::source::checked_module(source).expect("checked packet source");
    let companion = witchy_types::pipeline::inspect_closed_packet_region(&checked)
        .expect("source-proven packet lane recipe");
    let mut candidate = checked.module().clone();
    candidate.items.push(witchy_syntax::ast::Item::Function(companion));
    witchy_types::typeck::annotate_checked_source(candidate)
        .expect("ordinary type and ownership checks of packet lane recipe");
}

#[test]
fn timed_packet_source_selects_authenticated_lane_region() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    let source = include_str!("../../../../benchmarks/chan_scheduler.witchy");
    for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(options));
        let checked = super::source::checked_module(source).expect("checked timed packet source");
        let view = checked.canonical_source().expect("canonical packet source").view();
        if witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::ClosureElide) {
            let receipt = view.scalar_region().expect("selected packet lane region");
            assert_eq!(receipt.kind(), witchy_canonical_source::scalar_region::RegionKind::PacketScope);
            assert_eq!(receipt.covered_awaits().len(), 3);
            assert!(receipt.validate_checked_source(view.module(), view.task_await_proofs(), view.source_witness()));
        } else {
            assert!(view.scalar_region().is_none());
        }
    }
}

#[test]
fn packet_region_refuses_observable_or_unsupported_payload_shapes() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let source = include_str!("../../../../benchmarks/chan_scheduler.witchy");
    for source in [
        source.replace("type Packet:", "sealed type Packet:"),
        source.replace("Packet(Int, Int)", "Packet(Int, Bool)")
            .replace("Packet(i, i + 1)", "Packet(i, i < n)")
            .replace("sum = sum + left + right", "sum = sum + left"),
        source.replace("Packet(i, i + 1)", "Packet(i, i / (n + 1))"),
        source.replace("Packet(i, i + 1)", "Packet(i, i % (n + 1))"),
        source.replace("    while i < n:", "    let scalar_region_pending_second = 0\n    while i < n:"),
        source.replace("        i = i + 1", "        chan.send(tx, Packet(i, i + 1)).await\n        i = i + 1"),
    ] {
        let checked = super::source::checked_module(&source).expect("ordinary unsupported packet source stays checkable");
        assert!(checked.canonical_source().expect("ordinary canonical source").view().scalar_region().is_none());
    }
}

#[test]
fn packet_receipt_rejects_detached_companion_and_await_inventory() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let checked = super::source::checked_module(include_str!("../../../../benchmarks/chan_scheduler.witchy"))
        .expect("checked packet source");
    let view = checked.canonical_source().expect("canonical packet source").view();
    let receipt = view.scalar_region().expect("packet receipt");
    let mut changed = view.module().clone();
    let witchy_syntax::ast::Item::Function(companion) = &mut changed.items[receipt.companion_item() as usize] else { panic!("packet companion") };
    companion.body.stmts.push(witchy_syntax::ast::Stmt::Expr(witchy_syntax::ast::Expr::Tuple(vec![])));
    companion.body.lines.push(0);
    assert!(!receipt.validate_checked_source(&changed, view.task_await_proofs(), view.source_witness()));
    let mut proofs = view.task_await_proofs().to_vec();
    proofs.remove(receipt.covered_awaits()[0] as usize);
    assert!(!receipt.validate_checked_source(view.module(), &proofs, view.source_witness()));
}

#[test]
fn packet_region_derives_flat_integer_layouts_from_checked_fields() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let source = include_str!("../../../../benchmarks/chan_scheduler.witchy");
    for width in [1, 3, 8, 32] {
        let fields = vec!["Int"; width].join(", ");
        let values = (0..width).map(|lane| format!("i + {lane}")).collect::<Vec<_>>().join(", ");
        let bindings = (0..width).map(|lane| format!("field_{lane}")).collect::<Vec<_>>();
        let source = source.replace("Packet(Int, Int)", &format!("Packet({fields})"))
            .replace("Packet(i, i + 1)", &format!("Packet({values})"))
            .replace("Packet(left, right)", &format!("Packet({})", bindings.join(", ")))
            .replace("sum = sum + left + right", &format!("sum = sum + {}", bindings.join(" + ")));
        let checked = super::source::checked_module(&source).expect("checked flat packet source");
        let view = checked.canonical_source().expect("canonical flat packet source").view();
        let receipt = view.scalar_region().expect("selected source-derived integer layout");
        assert_eq!(receipt.kind(), witchy_canonical_source::scalar_region::RegionKind::PacketScope);
        assert_eq!(receipt.covered_awaits().len(), 3);
        assert!(receipt.validate_checked_source(view.module(), view.task_await_proofs(), view.source_witness()));
        let companion = witchy_types::pipeline::inspect_closed_packet_region(&checked).expect("flat packet companion");
        let mut candidate = checked.module().clone();
        candidate.items.push(witchy_syntax::ast::Item::Function(companion));
        witchy_types::typeck::annotate_checked_source(candidate).expect("ordinary checked flat buffer layout");
    }
}
