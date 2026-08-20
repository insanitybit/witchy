//! Closure-free compiled executor for the first all-scalar channel graph.
//!
//! The typed carrier plan is the selection authority. This slice recognizes a
//! single spawned counter producer feeding a folding receiver by state edges and
//! WIR data flow, never by source/module/function spelling. Rejected graphs keep
//! the ordinary Task/Step scheduler unchanged.

use witchy_types::suspension_carrier::{
    CarrierLane, ScalarExecutorPlan, ScalarExecutorState, ScalarTransition,
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
}

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
    let Some(terminal_args) = terminal_arguments(
        &terminal_function,
        fold.console,
        fold.accumulator,
    ) else {
        return false;
    };
    let Some(main) = module.funcs.iter_mut().find(|function| function.name == "main") else {
        return false;
    };
    if main.ret.len() != 1 || main.ret[0].kind() != Kind::I32 {
        return false;
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
    main.body = vec![
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
                    // Producer slot: Send publishes one Int and advances the
                    // compiler-owned scalar frame lane.
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
                        value: binary(
                            BinOp::Add,
                            Kind::I64,
                            E::GetLocal(ACCUMULATOR.into()),
                            E::GetLocal(MESSAGE.into()),
                        ),
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
    ];
    true
}

fn recognize<'a>(
    plan: &'a ScalarExecutorPlan,
    module: &WirModule,
) -> Option<ScalarChannelFold<'a>> {
    let spawn = plan.states.iter().find(|state| {
        state.transitions.iter().any(|transition| matches!(transition, ScalarTransition::Spawn { .. }))
    })?;
    let child = spawn.transitions.iter().find_map(|transition| match transition {
        ScalarTransition::Spawn { child, .. } => Some(*child),
        _ => None,
    })?;
    let producer_entry = plan.states.get(child)?;
    let producer_send = plan.states.iter().find(|state| {
        state.source_callable == producer_entry.source_callable
            && state.transitions.iter().any(|transition| matches!(transition, ScalarTransition::ChannelSend { .. }))
            && state.transitions.contains(&ScalarTransition::Done)
    })?;
    let receive = plan.states.iter().find(|state| {
        state.transitions.iter().any(|transition| matches!(transition, ScalarTransition::ChannelReceive { .. }))
    })?;
    if !plan.states.iter().any(|state| {
        state.source_callable == receive.source_callable
            && state.transitions.iter().any(|transition| matches!(transition, ScalarTransition::Join { .. }))
    }) || !plan.states.iter().any(|state| {
        state.transitions.iter().any(|transition| matches!(transition, ScalarTransition::ChannelOpen { .. }))
    }) {
        return None;
    }

    let terminal = plan.states.iter().find(|state| {
        state.source_callable == receive.source_callable
            && state.transitions == [ScalarTransition::Done]
    })?;
    let console = terminal.slots.iter().find(|slot| slot.lanes == [CarrierLane::ExternRef])?;
    let accumulator = receive.slots.iter().find(|slot| {
        slot.lanes == [CarrierLane::I64]
            && terminal.slots.iter().any(|terminal_slot| terminal_slot.name == slot.name)
    })?;
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
    if !producer_shape_is_counter(
        plan,
        module,
        &producer_entry.source_callable,
        &producer_send.function,
        &counter.name,
        &limit.name,
    ) {
        return None;
    }
    let Some(producer_lambda) = closure_function(module, &producer_entry.function) else {
        return None;
    };
    let Some(producer_start) = find_local_constant(&producer_lambda.body, &counter.name) else {
        return None;
    };
    let Some(accumulator_start) = initial_local_before_call(
        module,
        &receive.function,
        &accumulator.name,
    ) else {
        return None;
    };

    Some(ScalarChannelFold {
        console: &console.name,
        accumulator: &accumulator.name,
        terminal,
        producer_start,
        producer_limit,
        accumulator_start,
    })
}

fn producer_shape_is_counter(
    plan: &ScalarExecutorPlan,
    module: &WirModule,
    source: &str,
    send_function: &str,
    counter: &str,
    limit: &str,
) -> bool {
    let states = plan.states.iter().filter(|state| state.source_callable == source);
    let mut compares = false;
    let mut sends_counter = false;
    let mut increments = false;
    for state in states {
        let Some(function) = function(module, &state.function) else { return false };
        visit_seq(&function.body, &mut |expression| match expression {
            E::Binary { op: BinOp::Lt, kind: Kind::I64, lhs, rhs }
                if local(lhs, counter) && local(rhs, limit) => compares = true,
            E::Binary { op: BinOp::Add, kind: Kind::I64, lhs, rhs }
                if local(lhs, counter) && matches!(rhs.as_ref(), E::ConstI64(1)) => increments = true,
            E::Call { func, args }
                if call_family(func, "chan.send") && args.iter().any(|arg| local(arg, counter)) =>
            {
                sends_counter = true;
            }
            _ => {}
        });
    }
    compares && sends_counter && increments && function(module, send_function).is_some()
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
) -> Option<Vec<E>> {
    function.params.iter().map(|parameter| {
        if parameter.name == console {
            Some(E::GetLocal(console.into()))
        } else if parameter.name == accumulator {
            Some(E::GetLocal("__witchy_scalar_accumulator".into()))
        } else if parameter.name.starts_with("__await") || parameter.name.ends_with("__cap") {
            zero(parameter.ty.kind())
        } else {
            None
        }
    }).collect()
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
