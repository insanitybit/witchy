use witchy_syntax::ast::Convention;

#[test]
fn static_async_call_binds_exact_ordered_source_arguments_and_final_callee_frame() {
    let checked = super::source::checked_module("from task import Task\n\nasync fn helper(left: Int, right: Int) -> Int:\n    left - right\n\nasync fn worker(value: Int) -> Int:\n    let answer = helper(value + 1, value).await\n    answer + 1\n\nfn main():\n    let _ = worker(20)\n").expect("checked static async call");
    let input = checked.typed_async_lowering_input().expect("typed static call input");
    assert_eq!(input.static_calls.len(), 1);
    let call = &input.static_calls[0];
    assert_ne!(call.caller, call.callee);
    assert!(!call.terminal);
    assert!(call.continuation.is_some());
    assert_eq!(call.arguments.len(), 2);
    assert_eq!(call.arguments.iter().map(|argument| argument.ordinal).collect::<Vec<_>>(), [0, 1]);
    assert!(call.arguments[0].expression < call.arguments[1].expression);
    assert_eq!(call.arguments[0].source_bindings, call.arguments[1].source_bindings);
    assert_eq!(call.arguments.iter().map(|argument| argument.callee_parameter).collect::<Vec<_>>(), [0, 1]);
    assert_ne!(call.arguments[0].callee_binding, call.arguments[1].callee_binding);
    assert_eq!(call.captures.len(), 2);
    assert_eq!(call.callee_thunk.item, call.canonical_callee_item);
    assert!(call.captures.iter().all(|capture| capture.ty == call.arguments[capture.parameter as usize].ty));
    assert!(call.arguments.iter().all(|argument| argument.convention == Convention::Let));
    assert!(input.static_call_is_acyclic(call));
    assert_eq!(call.await_proof.source_mode(), call.caller_mode);
    assert_eq!(call.callee_behavior.source_mode(), call.callee_mode);
    assert_eq!(call.callee_behavior.async_source_identity(), Some((call.callee.module.as_str(),
        call.callee.surface.item, call.callee.surface.method)));
    let sites = witchy_types::typed_static_async_call::checked_static_async_call_sites(&checked)
        .expect("checked static call site");
    sites[0].validate_for(&checked).expect("same checked allocation");
    assert_eq!(sites[0].binding(), call);
    let other = super::source::checked_module("fn main():\n    let _ = 0\n").expect("another checked allocation");
    assert_eq!(sites[0].validate_for(&other).expect_err("different checked allocation"),
        "static async call site belongs to a different checked allocation");
}

#[test]
fn static_async_call_retains_tail_completion_and_rejects_reachable_recursion() {
    let checked = super::source::checked_module("from task import Task\n\nasync fn recursive(value: Int) -> Int:\n    recursive(value).await\n\nasync fn worker(value: Int) -> Int:\n    recursive(value).await\n\nfn main():\n    let _ = worker(20)\n").expect("checked recursive async call");
    let input = checked.typed_async_lowering_input().expect("typed recursive call input");
    assert_eq!(input.static_calls.len(), 2);
    for call in &input.static_calls {
        assert!(call.terminal);
        assert!(call.continuation.is_none());
        assert!(!input.static_call_is_acyclic(call));
        let partition = input.callable_input(&call.caller).expect("caller input");
        assert!(!partition.static_call_is_acyclic(call));
    }
}

#[test]
fn static_async_call_partition_keeps_mutual_and_unresolved_downstream_edges() {
    for source in [
        "from task import Task\n\nasync fn first(value: Int) -> Int:\n    second(value).await\n\nasync fn second(value: Int) -> Int:\n    first(value).await\n\nfn main():\n    let _ = first(1)\n",
        "from task import Task\n\nasync fn unknown(task: Task(Int)) -> Int:\n    task.await\n\nasync fn worker(task: Task(Int)) -> Int:\n    unknown(task).await\n\nfn main():\n    let _ = worker(task.done(1))\n",
    ] {
        let checked = super::source::checked_module(source).expect("checked source call graph");
        let input = checked.typed_async_lowering_input().expect("typed source call graph");
        assert!(!input.static_calls.is_empty());
        for call in &input.static_calls {
            assert!(!input.static_call_is_acyclic(call));
            let partition = input.callable_input(&call.caller).expect("caller input");
            assert!(!partition.static_call_is_acyclic(call));
        }
    }
}

#[test]
fn static_async_call_binds_packet_helper_without_payload_shape_recognition() {
    let checked = super::source::checked_module("from chan import Sender\n\ntype Packet:\n    Packet(Int, Int, Int)\n\nasync fn send_one(tx: Sender(Packet), packet: Packet):\n    chan.send(tx, packet).await\n\nasync fn producer(tx: Sender(Packet), n: Int):\n    var i = 0\n    while i < n:\n        send_one(tx, Packet(i, i + 1, i + 2)).await\n        i = i + 1\n\nasync fn main():\n    let (tx, rx) = chan.channel(64).await\n    producer(tx, 1).await\n").expect("checked packet static call");
    let input = checked.typed_async_lowering_input().expect("typed packet call input");
    let call = input.static_calls.iter().find(|call| call.arguments.len() == 2
        && call.arguments[1].source_bindings.len() == 1).expect("packet helper call binding");
    assert_eq!(call.arguments.len(), 2);
    assert!(input.static_call_is_acyclic(call));
    assert_eq!(call.arguments[1].source_bindings.len(), 1);
    assert_eq!(call.result, witchy_syntax::ast::Type::Tuple(Vec::new()));
    assert_ne!(call.callee_entry.0, call.canonical_callee_item);
    assert_eq!(checked.canonical_source().unwrap().view().task_behavior_at(call.canonical_callee_item),
        Some(&call.callee_behavior));
}

#[test]
fn static_async_call_keeps_owning_arguments_and_distinct_execution_modes() {
    let checked = super::source::checked_bundle(vec![
        ("helpers", "mode opt\n\npub async fn helper(own value: Int) -> Int:\n    value + 1\n"),
        ("main", "from helpers import helper\n\nasync fn worker(own value: Int) -> Int:\n    helper(value).await\n\nfn main():\n    let _ = worker(20)\n"),
    ], "main").expect("checked cross-mode owning call");
    let input = checked.typed_async_lowering_input().expect("typed cross-mode owning call");
    assert_eq!(input.static_calls.len(), 1);
    let call = &input.static_calls[0];
    assert!(!call.caller_mode.is_opt());
    assert!(call.callee_mode.is_opt());
    assert_eq!(call.arguments[0].convention, Convention::Own);
    assert_eq!(call.arguments[0].ownership, witchy_types::typed_execution::LaneOwnership::Move);
    assert!(input.static_call_is_acyclic(call));
}

#[test]
fn static_async_call_readiness_rejects_modified_call_metadata() {
    let checked = super::source::checked_module("from task import Task\n\nasync fn helper(value: Int) -> Int:\n    value + 1\n\nasync fn worker(value: Int) -> Int:\n    helper(value).await\n\nfn main():\n    let _ = worker(20)\n").expect("checked call metadata");
    let input = checked.typed_async_lowering_input().expect("typed call metadata");
    let program = witchy_types::async_program::AsyncProgram::new(Vec::new()).expect("empty candidate program");
    for change in 0..3 {
        let mut modified = input.clone();
        match change {
            0 => modified.static_calls.clear(),
            1 => modified.static_calls[0].arguments[0].convention = Convention::Own,
            _ => modified.static_calls[0].canonical_callee_item += 1,
        }
        assert!(matches!(checked.checked_async_readiness(&modified, &program),
            Err(witchy_types::async_readiness::AsyncReadinessError::TypedInputMismatch)));
    }
}

#[test]
fn static_async_call_state_factories_reject_changed_construction_and_decoding() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    let task = witchy_syntax::linker::bundled_source("task").expect("bundled task");
    let source = "from chan import Sender\n\nasync fn send_one(tx: Sender(Int), value: Int):\n    chan.send(tx, value).await\n\nasync fn main():\n    let (tx, rx) = chan.channel(1).await\n    send_one(tx, 7).await\n";
    for (original, changed, name) in [
        ("__send_state_frame(__erase(state), preparing, resuming, finishing)",
            "__send_state_frame(__erase(state), preparing, fn(own encoded): encoded, finishing)", "task.__producer_state_frame"),
        ("Some((ch, msg)) -> ProduceStateFrame((prepared, prepare, resume, finish, ch, msg))",
            "Some((ch, msg)) -> poll(finish(prepared))", "task.__send_state_frame"),
        ("__terminal_send_state_frame(__erase(state), preparing, finishing)",
            "__terminal_send_state_frame(__erase(state), preparing, fn(own encoded): lazy(fn(): finish(__unerase(encoded))))", "task.__terminal_producer_state_frame"),
        ("TerminalProduceStateFrame((prepared, finish, ch, msg))",
            "poll(finish(prepared))", "task.__terminal_send_state_frame"),
    ] {
        let modified = task.replacen(original, changed, 1);
        assert_ne!(modified, task);
        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::none()));
        super::source::checked_bundle_with_std_override(vec![("main", source)], "main", Some(("task", &modified)))
            .expect("ordinary caller keeps changed source implementations");
        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
        let error = super::source::checked_bundle_with_std_override(vec![("main", source)], "main", Some(("task", &modified)))
            .expect_err("state lowering requires exact constructor and adapter implementations");
        assert!(error.contains(&format!("changed scalar scheduler recipe {name}")), "{error}");
    }
}
