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
        source.replace("Packet(Int, Int)", "Packet(Int, String)")
            .replace("Packet(i, i + 1)", "Packet(i, \"payload\")")
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

#[test]
fn packet_region_retains_actor_completion_clock_and_original_awaits() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let source = include_str!("../../../../benchmarks/async_general/packet3.witchy").replace("__MESSAGES__", "32");
    let checked = super::source::checked_module(&source).expect("checked receiver completion clock");
    let view = checked.canonical_source().expect("canonical completion clock").view();
    let receipt = view.scalar_region().expect("source-derived timed actor companion");
    assert_eq!(receipt.covered_awaits().len(), 3);
    assert!(receipt.validate_checked_source(view.module(), view.task_await_proofs(), view.source_witness()));
    let fallback = source.replace("clock.now_monotonic()", "clock.now()");
    let checked = super::source::checked_module(&fallback).expect("ordinary nonmonotonic clock source");
    assert!(checked.canonical_source().unwrap().view().scalar_region().is_none());
}

#[test]
fn packet_region_checks_mixed_scalar_codecs_and_source_branches() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let source = include_str!("../../../../benchmarks/async_general/packet_bool.witchy").replace("__MESSAGES__", "32");
    let reversed = source.replace("Packet(Int, Bool)", "Packet(Bool, Int)")
        .replace("Packet(i, i % 2 == 0)", "Packet(i % 2 == 0, i)")
        .replace("Packet(left, right)", "Packet(right, left)");
    for source in [source, reversed] {
        let checked = super::source::checked_module(&source).expect("checked mixed scalar source");
        let view = checked.canonical_source().unwrap().view();
        let receipt = view.scalar_region().expect("derived scalar codecs and if paths");
        assert_eq!(receipt.covered_awaits().len(), 3);
        assert!(receipt.validate_checked_source(view.module(), view.task_await_proofs(), view.source_witness()));
    }
}

#[test]
fn packet_region_composes_an_exact_checked_static_send_forwarder() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let source = include_str!("../../../../benchmarks/async_general/helper_packet3.witchy").replace("__MESSAGES__", "32");
    let checked = super::source::checked_module(&source).expect("checked packet forwarding composition");
    let view = checked.canonical_source().expect("canonical forwarding source").view();
    let receipt = view.scalar_region().expect("source facts select existing packet stream surrogate");
    assert_eq!(receipt.kind(), witchy_canonical_source::scalar_region::RegionKind::ForwardedPacketScope);
    assert_eq!(receipt.covered_awaits().len(), 4);
    assert!(receipt.validate_checked_source(view.module(), view.task_await_proofs(), view.source_witness()));
    let input = checked.typed_async_lowering_input().expect("actual typed static call authority");
    assert!(input.static_calls.iter().any(|call| call.arguments.len() == 2 && input.static_call_is_acyclic(call)));
    witchy_types::pipeline::assert_packet_forwarding_rejects_changed_binding(&checked);
}

#[test]
fn packet_region_forwarding_falls_back_for_unsupported_helper_semantics() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    let source = include_str!("../../../../benchmarks/async_general/helper_packet3.witchy").replace("__MESSAGES__", "8");
    for changed in [
        source.replace("    chan.send(tx, packet).await", "    let ignored = 0\n    chan.send(tx, packet).await"),
        source.replace("    chan.send(tx, packet).await", "    chan.send(tx, packet).await\n    let ignored = 0"),
        source.replace("chan.send(tx, packet)", "chan.send(tx, Packet(0, 0, 0))"),
        source.replace("packet: Packet):", "packet: Packet, ignored: Int = 0):"),
        source.replace("async fn send_one(tx: Sender(Packet), packet: Packet):", "async fn send_one(packet: Packet, tx: Sender(Packet)):")
            .replace("send_one(tx, Packet(i, i + 1, i + 2))", "send_one(Packet(i, i + 1, i + 2), tx)"),
        source.replace("    var i = 0\n    while i < n:",
            "    let send_one = fn(tx: Sender(Packet), packet: Packet): chan.send(tx, packet)\n    var i = 0\n    while i < n:"),
    ] {
        for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
            witchy_syntax::opt::set_for_tests(Some(options));
            let checked = super::source::checked_module(&changed).expect("unsupported helper keeps ordinary source acceptance");
            assert!(checked.canonical_source().expect("ordinary checked source").view().scalar_region().is_none(), "{changed}");
        }
    }
}
