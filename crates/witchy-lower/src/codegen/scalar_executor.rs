//! Closure-free compiled executor for the first all-scalar channel graph.
//!
//! The typed carrier plan is the selection authority. This slice recognizes a
//! single spawned counter producer feeding a folding receiver by state edges and
//! WIR data flow, never by source/module/function spelling. Rejected graphs keep
//! the ordinary Task/Step scheduler unchanged.

use witchy_types::suspension_carrier::{
    CarrierLane, ChannelPayloadLayout, ScalarExecutorPlan, ScalarExecutorState,
    ScalarTransition,
};
use witchy_wir::wir::{
    BinOp, Kind, WirExpr as E, WirFunc, WirLocal, WirModule, WirNode as N, WirTy,
};

#[derive(Debug)]
struct ScalarChannelFold<'a> {
    console: &'a str,
    accumulator: &'a str,
    terminal: &'a ScalarExecutorState,
    producer_start: i64,
    producer_limit: i64,
    accumulator_start: i64,
    payload: ScalarPayload,
}

#[derive(Debug)]
struct EagerRegionPrologue {
    body: Vec<N>,
    locals: Vec<WirLocal>,
}

/// A source-independent description of the values that can cross the first
/// direct one-way channel path.  These are WIR data-flow shapes, not nominal
/// source constructors: a matching record is accepted regardless of its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarPayload {
    Counter,
    CounterAndSuccessor,
}

/// Emit the currently supported direct one-way region from the complete
/// ready-region ABI. This deliberately consumes the same plan as future
/// fixed-select and bounded-wait emitters: the direct path may decline a
/// layout or graph shape, but it must not reconstruct channel typing from
/// erased scheduler values.
pub(super) fn synthesize(plan: &ScalarExecutorPlan, module: &mut WirModule) -> bool {
    let Some(fold) = recognize(plan, module) else {
        return false;
    };
    let Some(terminal_function) = module
        .funcs
        .iter()
        .find(|function| function.name == fold.terminal.function)
        .cloned()
    else {
        return false;
    };
    if terminal_function.ret.len() != 1 {
        return false;
    }
    let Some(main_index) = module.funcs.iter().position(|function| function.name == "main") else {
        return false;
    };
    let Some(prologue) = eager_region_prologue(plan, module, &module.funcs[main_index]) else {
        return false;
    };
    let main = &mut module.funcs[main_index];
    if main.ret.len() != 1 || main.ret[0].kind() != Kind::I32 {
        return false;
    }
    let Some(terminal_args) = terminal_arguments(
        &terminal_function,
        fold.console,
        fold.accumulator,
        &main.params,
        &prologue.body,
    ) else {
        return false;
    };

    for local in prologue.locals {
        if !main.params.iter().chain(main.locals.iter()).any(|known| known.name == local.name) {
            main.locals.push(local);
        }
    }

    const PRODUCER: &str = "__witchy_scalar_producer";
    const LIMIT: &str = "__witchy_scalar_limit";
    const MESSAGE: &str = "__witchy_scalar_message";
    const ACCUMULATOR: &str = "__witchy_scalar_accumulator";
    const LOOP: &str = "__witchy_scalar_dispatch";

    main.locals.extend([
        i64_local(PRODUCER),
        i64_local(LIMIT),
        i64_local(MESSAGE),
        i64_local(ACCUMULATOR),
    ]);
    let mut body = prologue.body;
    body.extend([
        set_i64(PRODUCER, fold.producer_start),
        set_i64(LIMIT, fold.producer_limit),
        set_i64(ACCUMULATOR, fold.accumulator_start),
        N::If {
            cond: binary(
                BinOp::Lt,
                Kind::I64,
                E::GetLocal(PRODUCER.into()),
                E::GetLocal(LIMIT.into()),
            ),
            then_: vec![N::Loop {
                label: LOOP.into(),
                body: vec![
                    // Producer slot: Send publishes exact scalar lanes and
                    // advances the compiler-owned scalar frame lane.  The
                    // direct graph has one proved consumer, so no erased
                    // message envelope or nominal record is materialized.
                    N::SetLocal {
                        local: MESSAGE.into(),
                        value: E::GetLocal(PRODUCER.into()),
                    },
                    N::SetLocal {
                        local: PRODUCER.into(),
                        value: binary(
                            BinOp::Add,
                            Kind::I64,
                            E::GetLocal(PRODUCER.into()),
                            E::ConstI64(1),
                        ),
                    },
                    // Consumer slot: Receive consumes the message and updates
                    // its scalar fold lane in place.
                    N::SetLocal {
                        local: ACCUMULATOR.into(),
                        value: payload_fold_expression(fold.payload),
                    },
                    N::Br {
                        target: LOOP.into(),
                        cond: Some(binary(
                            BinOp::Lt,
                            Kind::I64,
                            E::GetLocal(PRODUCER.into()),
                            E::GetLocal(LIMIT.into()),
                        )),
                    },
                ],
            }],
            els: Vec::new(),
            result: None,
        },
        // Open, Spawn, and Join have no residual allocation in this closed
        // graph. The original terminal state runs once, preserving post-join
        // effects and formatting without re-entering Task/Step polling.
        N::Drop(E::Call {
            func: fold.terminal.function.clone(),
            args: terminal_args,
        }),
        N::Push(E::ConstI32(0)),
    ]);
    main.body = body;
    true
}

fn recognize<'a>(
    plan: &'a ScalarExecutorPlan,
    module: &WirModule,
) -> Option<ScalarChannelFold<'a>> {
    let mut spawns = plan.states.iter().filter(|state| {
        state.transitions.iter().any(|transition| matches!(transition, ScalarTransition::Spawn { .. }))
    });
    let spawn = spawns.next()?;
    // Multiple child graphs require an explicit scheduler/interleaving proof.
    // Do not pick one arbitrary state and accidentally erase the others.
    if spawns.next().is_some() {
        return None;
    }
    let child = spawn.transitions.iter().find_map(|transition| match transition {
        ScalarTransition::Spawn { child, .. } => Some(*child),
        _ => None,
    })?;
    let producer_entry = plan.states.get(child)?;
    let producer_send = plan.states.iter().find(|state| {
        state.source_callable == producer_entry.source_callable
            && state.transitions.iter().any(|transition| matches!(transition, ScalarTransition::ChannelSend(_)))
            && state.transitions.contains(&ScalarTransition::Done)
    })?;
    let mut receives = plan.states.iter().filter(|state| {
        state.transitions.iter().any(|transition| {
            matches!(transition, ScalarTransition::ChannelReceive(_) | ScalarTransition::ChannelReceiveFrameLoop(_))
        })
    });
    let receive = receives.next()?;
    if receives.next().is_some() {
        return None;
    }
    let frame_receive = receive.transitions.iter().any(|transition| {
        matches!(transition, ScalarTransition::ChannelReceiveFrameLoop(_))
    });
    if !frame_receive && !plan.states.iter().any(|state| {
        state.source_callable == receive.source_callable
            && state.transitions.iter().any(|transition| matches!(transition, ScalarTransition::Join { .. }))
    }) || plan.states.iter().filter(|state| {
        state.transitions.iter().any(|transition| matches!(transition, ScalarTransition::ChannelOpen { .. }))
    }).count() != 1 {
        return None;
    }

    let terminal = plan.states.iter().find(|state| {
        state.source_callable == receive.source_callable
            && state.transitions == [ScalarTransition::Done]
    })?;
    let console = terminal.slots.iter().find(|slot| slot.lanes == [CarrierLane::ExternRef])?;
    let accumulator_name = if receive.transitions.iter().any(|transition| {
        matches!(transition, ScalarTransition::ChannelReceiveFrameLoop(_))
    }) {
        frame_accumulator_name(module, receive, terminal)?
    } else {
        receive.slots.iter().find(|slot| {
            slot.lanes == [CarrierLane::I64]
                && terminal.slots.iter().any(|terminal_slot| terminal_slot.name == slot.name)
        })?.name.clone()
    };
    let accumulator = terminal.slots.iter().find(|slot| slot.name == accumulator_name)?;
    let counter = producer_send.slots.iter().find(|slot| {
        slot.lanes == [CarrierLane::I64]
            && !producer_entry.slots.iter().any(|entry| entry.name == slot.name)
    })?;

    let Some(spawn_function) = function(module, &spawn.function) else {
        return None;
    };
    let Some(child_call) = direct_call_args(&spawn_function.body, &producer_entry.function) else {
        return None;
    };
    let Some((limit_index, producer_limit)) = child_call.iter().enumerate().find_map(|(index, argument)| {
        match argument {
            E::ConstI64(value) => Some((index, *value)),
            _ => None,
        }
    }) else {
        return None;
    };
    let Some(limit) = producer_entry.slots.get(limit_index) else {
        return None;
    };
    let payload = producer_payload_shape(
        plan,
        module,
        &producer_entry.source_callable,
        &producer_send.function,
        &counter.name,
        &limit.name,
    )?;
    if !receiver_fold_shape(plan, module, receive, &accumulator.name, payload) {
        return None;
    }
    let Some(producer_lambda) = closure_function(module, &producer_entry.function) else {
        return None;
    };
    let Some(producer_start) = find_local_constant(&producer_lambda.body, &counter.name) else {
        return None;
    };
    let accumulator_start = if receive.transitions.iter().any(|transition| {
        matches!(transition, ScalarTransition::ChannelReceiveFrameLoop(_))
    }) {
        function(module, &receive.function)
            .and_then(|function| find_local_constant(&function.body, &accumulator.name))
    } else {
        initial_local_before_call(module, &receive.function, &accumulator.name)
    };
    let Some(accumulator_start) = accumulator_start else {
        return None;
    };

    Some(ScalarChannelFold {
        console: &console.name,
        accumulator: &accumulator.name,
        terminal,
        producer_start,
        producer_limit,
        accumulator_start,
        payload,
    })
}

fn producer_payload_shape(
    plan: &ScalarExecutorPlan,
    module: &WirModule,
    source: &str,
    send_function: &str,
    counter: &str,
    limit: &str,
) -> Option<ScalarPayload> {
    let states = plan.states.iter().filter(|state| state.source_callable == source);
    let mut compares = false;
    let mut payload = None;
    let mut increments = false;
    for state in states {
        let Some(function) = function(module, &state.function) else { return None };
        visit_seq(&function.body, &mut |expression| match expression {
            E::Binary { op: BinOp::Lt, kind: Kind::I64, lhs, rhs }
                if local(lhs, counter) && local(rhs, limit) => compares = true,
            E::Binary { op: BinOp::Add, kind: Kind::I64, lhs, rhs }
                if local(lhs, counter) && matches!(rhs.as_ref(), E::ConstI64(1)) => increments = true,
            E::Call { func, args } if call_family(func, "chan.send") => {
                let Some(message) = args.get(1) else { return };
                let candidate = if local(message, counter) {
                    Some(ScalarPayload::Counter)
                } else {
                    counter_and_successor(message, counter)
                        .then_some(ScalarPayload::CounterAndSuccessor)
                };
                match (payload, candidate) {
                    (None, Some(candidate)) => payload = Some(candidate),
                    (Some(existing), Some(candidate)) if existing == candidate => {}
                    _ => payload = None,
                }
            }
            _ => {}
        });
    }
    let payload = payload?;
    let layout_matches = !plan.typed_channels.is_empty()
        && plan.typed_channels.iter().all(|channel| {
            matches_payload_layout(payload, &channel.payload_layout)
        })
        && plan.typed_channels.iter().any(|channel| {
            channel.direction == witchy_types::suspension_carrier::ChannelEndpointDirection::Send
        })
        && plan.typed_channels.iter().any(|channel| {
            channel.direction == witchy_types::suspension_carrier::ChannelEndpointDirection::Receive
        });
    (compares && increments && function(module, send_function).is_some() && layout_matches)
        .then_some(payload)
}

fn matches_payload_layout(payload: ScalarPayload, layout: &ChannelPayloadLayout) -> bool {
    match (payload, layout) {
        (ScalarPayload::Counter, ChannelPayloadLayout::Scalar(CarrierLane::I64)) => true,
        (ScalarPayload::CounterAndSuccessor, ChannelPayloadLayout::Packed(lanes)) => {
            lanes.as_slice() == [CarrierLane::I64, CarrierLane::I64]
        }
        _ => false,
    }
}

fn receiver_fold_shape(
    plan: &ScalarExecutorPlan,
    module: &WirModule,
    receive: &ScalarExecutorState,
    accumulator: &str,
    payload: ScalarPayload,
) -> bool {
    let function = if receive.transitions.iter().any(|transition| {
        matches!(transition, ScalarTransition::ChannelReceiveFrameLoop(_))
    }) {
        // The reusable frame's fold update lives in the first generated
        // closure owned by the receive-frame state, not in an ordinary
        // continuation state. The carrier has already proved that this is a
        // compiler-owned frame closure, so recovering that function preserves
        // the old scalar-fold recognizer without scanning arbitrary closures.
        if !receive.transitions.iter().any(|transition| {
            matches!(transition, ScalarTransition::ChannelReceiveFrameLoop(_))
        }) {
            return false;
        }
        let Some(function) = closure_function(module, &receive.function) else { return false };
        function
    } else {
        let Some(resume) = receive.transitions.iter().find_map(|transition| match transition {
            ScalarTransition::ChannelReceive(plan) => Some(plan.resume_target),
            _ => None,
        }) else { return false };
        let Some(state) = plan.states.get(resume) else { return false };
        let Some(function) = function(module, &state.function) else { return false };
        function
    };
    let mut updates = false;
    visit_seq(&function.body, &mut |expression| {
        if let E::Binary { op: BinOp::Add, kind: Kind::I64, .. } = expression {
            updates |= match payload {
                ScalarPayload::Counter => accumulator_plus_local(expression, accumulator),
                ScalarPayload::CounterAndSuccessor => accumulator_plus_two_locals(expression, accumulator),
            };
        }
    });
    updates
}

fn counter_and_successor(expression: &E, counter: &str) -> bool {
    let E::StructNew { args, .. } = expression else { return false };
    args.len() == 2
        && local(&args[0], counter)
        && matches!(args[1], E::Binary { op: BinOp::Add, kind: Kind::I64, ref lhs, ref rhs }
            if local(lhs, counter) && matches!(rhs.as_ref(), E::ConstI64(1)))
}

fn accumulator_plus_local(expression: &E, accumulator: &str) -> bool {
    matches!(expression,
        E::Binary { op: BinOp::Add, kind: Kind::I64, lhs, rhs }
            if accumulator_local(lhs, accumulator) && matches!(rhs.as_ref(), E::GetLocal(_)))
}

fn accumulator_plus_two_locals(expression: &E, accumulator: &str) -> bool {
    matches!(expression,
        E::Binary { op: BinOp::Add, kind: Kind::I64, lhs, rhs }
            if matches!(lhs.as_ref(), E::Binary { op: BinOp::Add, kind: Kind::I64, lhs, rhs }
                if accumulator_local(lhs, accumulator) && matches!(rhs.as_ref(), E::GetLocal(_)))
                && matches!(rhs.as_ref(), E::GetLocal(_)))
}

/// Frame closures receive captured slots under a hygienic `__shadow` spelling.
/// It is still the same compiler-owned carrier local, not a user-selected
/// alias, so accepting this suffix preserves the exact frame binding.
fn accumulator_local(expression: &E, accumulator: &str) -> bool {
    matches!(expression, E::GetLocal(name)
        if name == accumulator || name.starts_with(&format!("{accumulator}__shadow")))
}

fn payload_fold_expression(payload: ScalarPayload) -> E {
    let accumulator = E::GetLocal("__witchy_scalar_accumulator".into());
    let message = E::GetLocal("__witchy_scalar_message".into());
    match payload {
        ScalarPayload::Counter => binary(BinOp::Add, Kind::I64, accumulator, message),
        ScalarPayload::CounterAndSuccessor => binary(
            BinOp::Add,
            Kind::I64,
            binary(BinOp::Add, Kind::I64, accumulator, message.clone()),
            binary(BinOp::Add, Kind::I64, message, E::ConstI64(1)),
        ),
    }
}

fn initial_local_before_call(
    module: &WirModule,
    callee: &str,
    local_name: &str,
) -> Option<i64> {
    module.funcs.iter().find_map(|function| {
        let calls_callee = direct_call_args(&function.body, callee).is_some();
        calls_callee.then(|| find_local_constant(&function.body, local_name)).flatten()
    })
}

fn closure_function<'a>(module: &'a WirModule, owner: &str) -> Option<&'a WirFunc> {
    let owner = function(module, owner)?;
    let mut index = None;
    visit_seq(&owner.body, &mut |expression| {
        if index.is_none() {
            if let E::StructNew { struct_id: 0, args } = expression {
                if let Some(E::ConstI32(code)) = args.first() {
                    index = usize::try_from(*code).ok();
                }
            }
        }
    });
    let name = module.table.as_ref()?.funcs.get(index?)?;
    function(module, name)
}

/// Recover the original scalar frame lane named by the generated step closure.
/// The receive state owns only the receiver and frame root; its captured scalar
/// accumulator is hygienically renamed inside `step`, while the terminal state
/// keeps the canonical slot name used by the synthesized executor.
fn frame_accumulator_name(
    module: &WirModule,
    receive: &ScalarExecutorState,
    terminal: &ScalarExecutorState,
) -> Option<String> {
    receive.transitions.iter().find_map(|transition| match transition {
        ScalarTransition::ChannelReceiveFrameLoop(_) => Some(()),
        _ => None,
    })?;
    let step = closure_function(module, &receive.function)?;
    let mut candidate = None;
    visit_seq(&step.body, &mut |expression| {
        let E::Binary { op: BinOp::Add, kind: Kind::I64, lhs, .. } = expression else { return };
        let E::GetLocal(shadow) = lhs.as_ref() else { return };
        candidate = terminal.slots.iter().find_map(|slot| {
            (slot.lanes == [CarrierLane::I64]
                && shadow.starts_with(&format!("{}__shadow", slot.name)))
                .then(|| slot.name.clone())
        }).or(candidate.take());
    });
    candidate
}

fn find_local_constant(sequence: &[N], local_name: &str) -> Option<i64> {
    for node in sequence {
        match node {
            N::SetLocal { local, value: E::ConstI64(value) } if local == local_name => {
                return Some(*value);
            }
            N::Source { body, .. } | N::Block { body, .. } | N::Loop { body, .. } => {
                if let Some(value) = find_local_constant(body, local_name) {
                    return Some(value);
                }
            }
            N::If { then_, els, .. } => {
                if let Some(value) = find_local_constant(then_, local_name)
                    .or_else(|| find_local_constant(els, local_name))
                {
                    return Some(value);
                }
            }
            N::SetLocal { value, .. } | N::SetGlobal { value, .. }
            | N::Drop(value) | N::Do(value) | N::Push(value) => {
                if let Some(value) = find_local_constant_expr(value, local_name) {
                    return Some(value);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_local_constant_expr(expression: &E, local_name: &str) -> Option<i64> {
    match expression {
        E::Seq(sequence) => find_local_constant(sequence, local_name),
        E::Control(node) => find_local_constant(std::slice::from_ref(node.as_ref()), local_name),
        _ => None,
    }
}

fn direct_call_args<'a>(sequence: &'a [N], callee: &str) -> Option<&'a [E]> {
    for node in sequence {
        match node {
            N::CallStoreMulti { func, args, .. } if call_instance(func, callee) => {
                return Some(args);
            }
            N::Source { body, .. } | N::Block { body, .. } | N::Loop { body, .. } => {
                if let Some(arguments) = direct_call_args(body, callee) {
                    return Some(arguments);
                }
            }
            N::If { then_, els, .. } => {
                if let Some(arguments) = direct_call_args(then_, callee)
                    .or_else(|| direct_call_args(els, callee))
                {
                    return Some(arguments);
                }
            }
            _ => {}
        }
    }
    let mut found = None;
    visit_seq(sequence, &mut |expression| {
        if found.is_none() {
            if let E::Call { func, args } = expression {
                if call_instance(func, callee) {
                    found = Some(args.as_slice());
                }
            }
        }
    });
    found
}

fn function<'a>(module: &'a WirModule, name: &str) -> Option<&'a WirFunc> {
    module.funcs.iter().find(|function| function.name == name)
}

fn terminal_arguments(
    function: &WirFunc,
    console: &str,
    accumulator: &str,
    main_params: &[WirLocal],
    prologue: &[N],
) -> Option<Vec<E>> {
    let initialized = main_params
        .iter()
        .map(|parameter| parameter.name.as_str())
        .chain(prologue.iter().filter_map(|node| match node {
            N::SetLocal { local, .. } => Some(local.as_str()),
            _ => None,
        }))
        .collect::<std::collections::HashSet<_>>();
    function.params.iter().map(|parameter| {
        if parameter.name == console {
            Some(E::GetLocal(console.into()))
        } else if parameter.name == accumulator {
            Some(E::GetLocal("__witchy_scalar_accumulator".into()))
        } else if parameter.name.starts_with("__await") || parameter.name.ends_with("__cap") {
            zero(parameter.ty.kind())
        } else if initialized.contains(parameter.name.as_str()) {
            Some(E::GetLocal(parameter.name.clone()))
        } else {
            None
        }
    }).collect()
}

/// Recover a straight-line, eager prefix from the compiler-owned entry
/// closure.  The suffix is the first task construction and is replaced by the
/// scalar graph, so unsupported control flow or task work stays on the normal
/// scheduler path.
fn eager_region_prologue(
    plan: &ScalarExecutorPlan,
    module: &WirModule,
    main: &WirFunc,
) -> Option<EagerRegionPrologue> {
    // Async lowering wraps the entry state in `task.lazy`, so the eager source
    // prologue is held in that closure rather than directly in Wasm `main`.
    // Extract only its straight-line prefix before the first `task.and_then`.
    // The remaining task construction is replaced by the proved scalar graph.
    let entry = plan.states.iter().find(|state| {
        state.function == "main" && state.source_callable == "main"
    })?;
    let entry = closure_function(module, &entry.function)?;
    let N::Push(E::Seq(sequence)) = entry.body.last()? else {
        // A closed graph with no eager source statements has an entry whose
        // terminal task is emitted directly, and needs no copied prologue.
        return matches!(entry.body.last(), Some(N::Push(E::Call { func, .. })) if call_family(func, "task.and_then"))
            .then_some(EagerRegionPrologue { body: Vec::new(), locals: Vec::new() });
    };
    let (terminal, prefix) = sequence.split_last()?;
    let N::Push(E::Call { func, .. }) = terminal else { return None };
    if !call_family(func, "task.and_then") || !eager_prologue_nodes(prefix) {
        return None;
    }

    let written = prefix
        .iter()
        .filter_map(|node| match node {
            N::SetLocal { local, .. } => Some(local.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    let available = main.params.iter().map(|local| local.name.as_str())
        .chain(written.iter().copied())
        .collect::<std::collections::HashSet<_>>();
    let mut reads = std::collections::HashSet::new();
    visit_seq(prefix, &mut |expression| {
        if let E::GetLocal(local) = expression {
            reads.insert(local.as_str());
        }
    });
    if !reads.is_subset(&available) {
        return None;
    }
    let locals = entry.locals.iter()
        .filter(|local| written.contains(local.name.as_str()))
        .cloned()
        .collect();
    Some(EagerRegionPrologue { body: prefix.to_vec(), locals })
}

fn eager_prologue_nodes(nodes: &[N]) -> bool {
    nodes.iter().all(|node| match node {
        N::SetLocal { value, .. } | N::Drop(value) | N::Do(value) => !contains_task_call(value),
        // `Push` would leave an unconsumed Wasm stack value when spliced before
        // the scalar loop. Structured control is intentionally deferred until
        // it has an explicit result/exit proof.
        _ => false,
    })
}

fn contains_task_call(expression: &E) -> bool {
    let mut contains = false;
    visit_expr(expression, &mut |expression| {
        if let E::Call { func, .. } = expression {
            contains |= func.starts_with("task.") || func.starts_with("chan.");
        }
    });
    contains
}

fn visit_seq<'a>(sequence: &'a [N], visitor: &mut impl FnMut(&'a E)) {
    for node in sequence {
        match node {
            N::Source { body, .. } | N::Block { body, .. } | N::Loop { body, .. } => {
                visit_seq(body, visitor);
            }
            N::If { cond, then_, els, .. } => {
                visit_expr(cond, visitor);
                visit_seq(then_, visitor);
                visit_seq(els, visitor);
            }
            N::SetLocal { value, .. } | N::SetGlobal { value, .. } | N::Drop(value)
            | N::Do(value) | N::Push(value) => visit_expr(value, visitor),
            N::Store { ptr, value, .. } => {
                visit_expr(ptr, visitor);
                visit_expr(value, visitor);
            }
            N::Br { cond: Some(cond), .. } => visit_expr(cond, visitor),
            N::Return(Some(value)) => visit_expr(value, visitor),
            N::StructSet { base, value, .. } => {
                visit_expr(base, visitor);
                visit_expr(value, visitor);
            }
            N::ArraySet { array, index, value, .. } => {
                visit_expr(array, visitor);
                visit_expr(index, visitor);
                visit_expr(value, visitor);
            }
            _ => {}
        }
    }
}

fn visit_expr<'a>(expression: &'a E, visitor: &mut impl FnMut(&'a E)) {
    visitor(expression);
    match expression {
        E::ToSlot(value, _) | E::FromSlot(value, _) | E::Unary { arg: value, .. }
        | E::Convert { arg: value, .. } | E::MemoryGrow(value) | E::ArrayLen(value)
        | E::RefIsNull(value) => visit_expr(value, visitor),
        E::Binary { lhs, rhs, .. } => {
            visit_expr(lhs, visitor);
            visit_expr(rhs, visitor);
        }
        E::Load { ptr, .. } | E::Load8U { ptr, .. } => visit_expr(ptr, visitor),
        E::Call { args, .. } | E::CallHost { args, .. } | E::StructNew { args, .. } => {
            for argument in args { visit_expr(argument, visitor); }
        }
        E::CallIndirect { args, index, .. } => {
            for argument in args { visit_expr(argument, visitor); }
            visit_expr(index, visitor);
        }
        E::Control(node) => visit_seq(std::slice::from_ref(node.as_ref()), visitor),
        E::Seq(sequence) => visit_seq(sequence, visitor),
        E::ArrayNew { value, len, .. } => {
            visit_expr(value, visitor);
            visit_expr(len, visitor);
        }
        E::ArrayNewFixed { items, .. } => {
            for item in items { visit_expr(item, visitor); }
        }
        E::ArrayGet { array, index, .. } => {
            visit_expr(array, visitor);
            visit_expr(index, visitor);
        }
        E::StructGet { base, .. } | E::RefCast { value: base, .. }
        | E::RefCastNullable { value: base, .. } => visit_expr(base, visitor),
        _ => {}
    }
}

/// Conservative proof used by the future Select2 lane-transfer emitter.  An
/// arm continuation may only contain local lane writes, integer expressions,
/// structured control flow, and branches.  Calls (including indirect calls),
/// stores, allocation-producing expressions, and host effects are rejected so
/// the ordinary Task/Step scheduler remains the fallback.
#[allow(dead_code)]
fn scalar_lane_update_sequence(sequence: &[N]) -> bool {
    sequence.iter().all(|node| match node {
        N::Source { body, .. } | N::Block { body, .. } | N::Loop { body, .. } => {
            scalar_lane_update_sequence(body)
        }
        N::SetLocal { value, .. } | N::Drop(value) | N::Do(value) | N::Push(value) => {
            scalar_lane_update_expression(value)
        }
        N::If { cond, then_, els, .. } => {
            scalar_lane_update_expression(cond)
                && scalar_lane_update_sequence(then_)
                && scalar_lane_update_sequence(els)
        }
        N::Br { cond, .. } => cond.as_ref().is_none_or(scalar_lane_update_expression),
        N::Unreachable => true,
        _ => false,
    })
}

#[allow(dead_code)]
fn scalar_lane_update_expression(expression: &E) -> bool {
    match expression {
        E::ConstI32(_) | E::ConstI64(_) | E::ConstF64(_) | E::GetLocal(_) => true,
        E::Binary { lhs, rhs, .. } => {
            scalar_lane_update_expression(lhs) && scalar_lane_update_expression(rhs)
        }
        E::Unary { arg, .. } | E::Convert { arg, .. } => scalar_lane_update_expression(arg),
        E::Seq(sequence) => scalar_lane_update_sequence(sequence),
        E::Control(node) => scalar_lane_update_sequence(std::slice::from_ref(node.as_ref())),
        _ => false,
    }
}

fn call_family(name: &str, family: &str) -> bool {
    name == family
        || name.strip_prefix(family).is_some_and(|suffix| suffix.starts_with("__"))
        || name.rsplit_once('.').is_some_and(|(_, tail)| {
            let expected = family.rsplit('.').next().unwrap_or(family);
            tail == expected || tail.strip_prefix(expected).is_some_and(|suffix| suffix.starts_with("__"))
        })
}

fn call_instance(name: &str, logical: &str) -> bool {
    name == logical
        || name.strip_prefix(logical).is_some_and(|suffix| suffix.starts_with("__phys"))
}

fn local(expression: &E, name: &str) -> bool {
    matches!(expression, E::GetLocal(local) if local == name)
}

fn zero(kind: Kind) -> Option<E> {
    match kind {
        Kind::I32 => Some(E::ConstI32(0)),
        Kind::I64 => Some(E::ConstI64(0)),
        _ => None,
    }
}

fn i64_local(name: &str) -> WirLocal {
    WirLocal { name: name.into(), ty: WirTy::Int }
}

fn set_i64(name: &str, value: i64) -> N {
    N::SetLocal { local: name.into(), value: E::ConstI64(value) }
}

fn binary(op: BinOp, kind: Kind, lhs: E, rhs: E) -> E {
    E::Binary { op, kind, lhs: Box::new(lhs), rhs: Box::new(rhs) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_lane_transfer_accepts_local_integer_updates() {
        let body = vec![N::SetLocal {
            local: "checksum".into(),
            value: binary(
                BinOp::Add,
                Kind::I64,
                E::GetLocal("checksum".into()),
                E::GetLocal("payload".into()),
            ),
        }];
        assert!(scalar_lane_update_sequence(&body));
    }

    #[test]
    fn scalar_lane_transfer_rejects_calls_and_effects() {
        let body = vec![N::Do(E::Call {
            func: "task.and_then".into(),
            args: Vec::new(),
        })];
        assert!(!scalar_lane_update_sequence(&body));
    }
}
