//! WIR body capture for the future typed ready-channel executor.
//!
//! This module deliberately does not select a runtime path.  It establishes the
//! boundary that a later executor needs: a proved channel transition owns the
//! final task-building WIR node, while every earlier node remains the ordinary
//! state prefix and must execute exactly once before a direct probe or fallback.

use witchy_types::suspension_carrier::{
    ChannelEndpointDirection, ChannelEndpointPlace, ChannelMessageOperand, ChannelOwnershipEnvelope,
    ChannelPayloadLayout, ReadyChannelOperation, ReadyChannelOperationKind, SuspensionCarrierCatalog,
    TypedChannelPlan,
};
use witchy_wir::wir::{WirExpr, WirModule, WirNode, WirSeq};

/// A compiler-owned state body split at its generated terminal task boundary.
///
/// `prefix` is cloned verbatim from the lowered WIR function.  A later emitter
/// may execute it, but may never rediscover or replay it from source.  Keeping
/// the original `terminal` node lets the first direct executor retain an exact
/// scheduler fallback until it can lower the transition itself.
#[derive(Debug, Clone)]
pub(super) struct ReadyStateWir {
    pub state: usize,
    pub path_ordinal: usize,
    pub function: String,
    pub prefix: WirSeq,
    pub terminal: WirNode,
    pub operations: Vec<ReadyChannelOperation>,
    /// Physical carrier references for every operation in `operations`, in
    /// transition order. `None` is an explicit local rejection: later
    /// scheduler-context lowering must retain this captured ordinary fallback,
    /// rather than recover a channel from a source name or an erased `__Msg`
    /// value.
    pub storage_uses: Option<Vec<ReadyStorageUse>>,
}

/// One channel operation's exact carrier references. `Open` has no endpoint
/// to probe in its input state and therefore has an empty `endpoints` list;
/// every send, receive, and fixed select lists its endpoint slots in source
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadyStorageUse {
    pub transition_ordinal: usize,
    pub endpoints: Vec<ReadyStorageEndpoint>,
}

/// A direct typed-ring endpoint identified only by its physical suspension
/// carrier location. The representation and ownership facts travel with that
/// location, so a later emitter cannot accidentally reinterpret erased
/// scheduler storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadyStorageEndpoint {
    pub slot: usize,
    pub lane: usize,
    pub direction: ChannelEndpointDirection,
    pub payload_layout: ChannelPayloadLayout,
    pub ownership: ChannelOwnershipEnvelope,
    /// The checked message type is retained solely as a promotion guard.  A
    /// scalar i64 lane is shared by Int, Bool, and Duration, but the first
    /// scheduler bridge accepts only Int; layout alone is not enough.
    pub message_type: String,
}

/// One compiler-owned typed-ring allocation edge. The key is a lowered
/// suspension boundary, never a source name: an emitter creates storage when
/// this exact open transition runs, then uses the runtime channel id carried by
/// the ordinary `(Sender, Receiver)` result to address that domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadyTypedStorageDomain {
    pub opening_state: usize,
    pub opening_path_ordinal: usize,
    pub opening_transition_ordinal: usize,
    pub resume_state: usize,
    /// The source capacity expression is retained only when it is a literal or
    /// a stable carrier operand. A computed capacity must keep the ordinary
    /// task construction so a typed allocator can never evaluate it twice.
    pub capacity: ChannelMessageOperand,
    pub payload_layout: ChannelPayloadLayout,
    pub ownership: ChannelOwnershipEnvelope,
}

/// Extract WIR bodies only for individually formed, unguarded ready regions.
///
/// Formation has already rejected boxed state, dynamic endpoint/payload, and
/// guarded task construction.  This pass adds the independent WIR requirement:
/// the generated state must have one final value-producing task construction.
/// It refuses all other bodies rather than guessing a boundary from a nested
/// call, which could duplicate effects during a ready probe.
pub(super) fn extract(
    carrier: &SuspensionCarrierCatalog,
    module: &WirModule,
) -> Vec<ReadyStateWir> {
    carrier
        .ready_channel_regions()
        .regions
        .into_iter()
        .filter_map(|region| {
            let function = module
                .funcs
                .iter()
                .find(|function| function.name == region.state.function)?;
            let (terminal, prefix) = split_terminal_task_node(&function.body)?;
            let storage_uses = bind_storage_uses(&region.typed_channels, &prefix, &region.operations);
            Some(ReadyStateWir {
                state: region.id,
                path_ordinal: region.path_ordinal,
                function: function.name.clone(),
                prefix,
                terminal,
                operations: region.operations,
                storage_uses,
            })
        })
        .collect()
}

/// Freeze typed channel allocation edges after WIR capture. The later
/// scheduler-context emitter consumes this descriptor alongside the captured
/// terminal, so it can install typed storage and still execute that terminal
/// exactly once on the first blocking operation.
pub(super) fn storage_domains(states: &[ReadyStateWir]) -> Vec<ReadyTypedStorageDomain> {
    let mut domains = states
        .iter()
        .flat_map(|state| {
            state.operations.iter().filter_map(move |operation| {
                let ReadyChannelOperationKind::Open {
                    resume_target,
                    capacity,
                    payload_layout,
                    ownership,
                } = &operation.kind else {
                    return None;
                };
                Some(ReadyTypedStorageDomain {
                    opening_state: state.state,
                    opening_path_ordinal: state.path_ordinal,
                    opening_transition_ordinal: operation.transition_ordinal,
                    resume_state: *resume_target,
                    capacity: capacity.clone(),
                    payload_layout: payload_layout.clone(),
                    ownership: *ownership,
                })
            })
        })
        .collect::<Vec<_>>();
    domains.sort_by_key(|domain| {
        (
            domain.opening_state,
            domain.opening_path_ordinal,
            domain.opening_transition_ordinal,
        )
    });
    domains
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadyI64Call {
    Open,
    Send,
    Receive,
}

/// Select the narrow Int bridge only at independently proved ready boundaries.
///
/// This does not replace the scheduler: `task.run` materializes a typed ring
/// before every ordinary operation, and each typed bridge itself falls back on
/// its first blocking operation.  A rejected region therefore continues to
/// call the generic public bridge exactly as before.
pub(super) fn rewrite_i64_ready_calls(states: &[ReadyStateWir], module: &mut WirModule) -> usize {
    let Some(open) = resolved_call(module, "chan.__rfc0152_channel_i64") else { return 0 };
    let Some(send) = resolved_call(module, "chan.__rfc0152_send_i64") else { return 0 };
    let Some(receive) = resolved_call(module, "chan.__rfc0152_recv_i64") else { return 0 };

    let mut rewritten = 0;
    for state in states {
        let Some(selection) = i64_selection(state) else { continue };
        let (family, replacement) = match selection {
            ReadyI64Call::Open => ("chan.channel", &open),
            ReadyI64Call::Send => ("chan.send", &send),
            ReadyI64Call::Receive => ("chan.recv", &receive),
        };
        let count = rewrite_function_call(module, &state.function, family, replacement);
        if count == 1 {
            rewritten += 1;
            continue;
        }
        // An async entry's Open is represented by its task.run/lazy wrapper,
        // not by a direct call in the root state. Follow the generated closure
        // code edge from that captured terminal into its table target. This is
        // still a WIR identity, not a recovered source name.
        if selection == ReadyI64Call::Open {
            let targets = closure_targets(&state.terminal, module);
            let snapshot = module.funcs.clone();
            let nested = targets.iter().map(|target| {
                rewrite_function_call(module, target, family, replacement)
            }).sum::<usize>();
            if nested == 1 {
                rewritten += 1;
            } else {
                module.funcs = snapshot;
            }
        }
    }
    rewritten
}

fn rewrite_function_call(module: &mut WirModule, name: &str, family: &str, replacement: &str) -> usize {
    let Some(function) = module.funcs.iter_mut().find(|function| function.name == name) else {
        return 0;
    };
    let original = function.body.clone();
    let count = rewrite_call_seq(&mut function.body, family, replacement);
    if count != 1 {
        function.body = original;
    }
    count
}

fn closure_targets(node: &WirNode, module: &WirModule) -> Vec<String> {
    let mut codes = Vec::new();
    collect_closure_codes_node(node, &mut codes);
    let Some(table) = &module.table else { return Vec::new() };
    codes.into_iter().filter_map(|code| table.funcs.get(code as usize).cloned()).collect()
}

fn collect_closure_codes_node(node: &WirNode, out: &mut Vec<i32>) {
    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
            for node in body { collect_closure_codes_node(node, out); }
        }
        WirNode::If { cond, then_, els, .. } => {
            collect_closure_codes_expr(cond, out);
            for node in then_ { collect_closure_codes_node(node, out); }
            for node in els { collect_closure_codes_node(node, out); }
        }
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } | WirNode::Drop(value)
        | WirNode::Do(value) | WirNode::Push(value) | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => collect_closure_codes_expr(value, out),
        _ => {}
    }
}

fn collect_closure_codes_expr(expression: &WirExpr, out: &mut Vec<i32>) {
    match expression {
        WirExpr::StructNew { struct_id: 0, args } => {
            if let Some(WirExpr::ConstI32(code)) = args.first() { out.push(*code); }
            for arg in args { collect_closure_codes_expr(arg, out); }
        }
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } | WirExpr::StructNew { args, .. }
        | WirExpr::ArrayNewFixed { items: args, .. } | WirExpr::Vector { args, .. } => {
            for arg in args { collect_closure_codes_expr(arg, out); }
        }
        WirExpr::ToSlot(value, _) | WirExpr::FromSlot(value, _) | WirExpr::Unary { arg: value, .. }
        | WirExpr::Convert { arg: value, .. } | WirExpr::MemoryGrow(value) | WirExpr::ArrayLen(value)
        | WirExpr::RefIsNull(value) | WirExpr::Load { ptr: value, .. } | WirExpr::Load8U { ptr: value, .. }
        | WirExpr::StructGet { base: value, .. } | WirExpr::RefCast { value, .. }
        | WirExpr::RefCastNullable { value, .. } => collect_closure_codes_expr(value, out),
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            place.for_each_wir_expr(|value| collect_closure_codes_expr(value, out));
        }
        WirExpr::Binary { lhs, rhs, .. } | WirExpr::ArrayNew { value: lhs, len: rhs, .. }
        | WirExpr::ArrayGet { array: lhs, index: rhs, .. } => {
            collect_closure_codes_expr(lhs, out);
            collect_closure_codes_expr(rhs, out);
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for arg in args { collect_closure_codes_expr(arg, out); }
            collect_closure_codes_expr(index, out);
        }
        WirExpr::Control(node) => collect_closure_codes_node(node, out),
        WirExpr::Seq(nodes) => for node in nodes { collect_closure_codes_node(node, out); },
        _ => {}
    }
}

fn i64_selection(state: &ReadyStateWir) -> Option<ReadyI64Call> {
    let [operation] = state.operations.as_slice() else { return None };
    match &operation.kind {
        ReadyChannelOperationKind::Open { capacity, payload_layout, ownership, .. }
            if !matches!(capacity, ChannelMessageOperand::Dynamic)
                && *payload_layout == ChannelPayloadLayout::Scalar(witchy_types::suspension_carrier::CarrierLane::I64)
                && *ownership == ChannelOwnershipEnvelope::InlineValue => Some(ReadyI64Call::Open),
        ReadyChannelOperationKind::Send(operation)
            // A literal Int is independently typed; a static payload may use
            // the same bridge only after its sender endpoint has been bound
            // through a direct carrier lane or an explicit tuple projection.
            if (matches!(operation.message, Some(ChannelMessageOperand::Int(_)))
                || (matches!(operation.message, Some(ChannelMessageOperand::Static(_)))
                    && state_endpoint_is_int(state, state.operations[0].transition_ordinal)))
                && operation.payload_layout == ChannelPayloadLayout::Scalar(witchy_types::suspension_carrier::CarrierLane::I64)
                && operation.ownership == ChannelOwnershipEnvelope::InlineValue => Some(ReadyI64Call::Send),
        ReadyChannelOperationKind::Receive(operation)
            if operation.payload_layout == ChannelPayloadLayout::Scalar(witchy_types::suspension_carrier::CarrierLane::I64)
                && operation.ownership == ChannelOwnershipEnvelope::InlineValue
                && state_endpoint_is_int(state, state.operations[0].transition_ordinal) => Some(ReadyI64Call::Receive),
        _ => None,
    }
}

fn state_endpoint_is_int(state: &ReadyStateWir, transition_ordinal: usize) -> bool {
    state.storage_uses.as_ref().and_then(|uses| {
        uses.iter().find(|use_| use_.transition_ordinal == transition_ordinal)
    }).is_some_and(|use_| {
        use_.endpoints.len() == 1 && use_.endpoints[0].message_type == "Int"
    })
}

fn resolved_call(module: &WirModule, family: &str) -> Option<String> {
    module.funcs.iter().find_map(|function| {
        call_family(&function.name, family).then(|| function.name.clone())
    })
}

fn call_family(name: &str, family: &str) -> bool {
    name == family || name.strip_prefix(family).is_some_and(|suffix| suffix.starts_with("__"))
}

fn rewrite_call_seq(sequence: &mut WirSeq, family: &str, replacement: &str) -> usize {
    sequence.iter_mut().map(|node| rewrite_call_node(node, family, replacement)).sum()
}

fn rewrite_call_node(node: &mut WirNode, family: &str, replacement: &str) -> usize {
    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => rewrite_call_seq(body, family, replacement),
        WirNode::If { cond, then_, els, .. } => rewrite_call_expr(cond, family, replacement)
            + rewrite_call_seq(then_, family, replacement) + rewrite_call_seq(els, family, replacement),
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } | WirNode::Drop(value)
        | WirNode::Do(value) | WirNode::Push(value) | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => rewrite_call_expr(value, family, replacement),
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => rewrite_call_expr(ptr, family, replacement) + rewrite_call_expr(value, family, replacement),
        WirNode::SafeStore { place, value, .. } | WirNode::SafeStore8 { place, value } => {
            let mut rewritten = 0;
            place.for_each_wir_expr_mut(|expr| {
                rewritten += rewrite_call_expr(expr, family, replacement);
            });
            rewritten + rewrite_call_expr(value, family, replacement)
        }
        WirNode::MemoryCopy { dest, src, len } | WirNode::MemoryFill { dest, value: src, len }
        | WirNode::ArraySet { array: dest, index: src, value: len, .. } => rewrite_call_expr(dest, family, replacement) + rewrite_call_expr(src, family, replacement) + rewrite_call_expr(len, family, replacement),
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            let mut rewritten = 0;
            destination.for_each_wir_expr_mut(|expr| {
                rewritten += rewrite_call_expr(expr, family, replacement);
            });
            source.for_each_wir_expr_mut(|expr| {
                rewritten += rewrite_call_expr(expr, family, replacement);
            });
            rewritten + rewrite_call_expr(len, family, replacement)
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            let mut rewritten = 0;
            destination.for_each_wir_expr_mut(|expr| {
                rewritten += rewrite_call_expr(expr, family, replacement);
            });
            rewritten
                + rewrite_call_expr(value, family, replacement)
                + rewrite_call_expr(len, family, replacement)
        }
        WirNode::CallStoreMulti { args, .. } => args.iter_mut().map(|arg| rewrite_call_expr(arg, family, replacement)).sum(),
        WirNode::CallIndirectStoreMulti { args, index, .. } => args.iter_mut().map(|arg| rewrite_call_expr(arg, family, replacement)).sum::<usize>() + rewrite_call_expr(index, family, replacement),
        _ => 0,
    }
}

fn rewrite_call_expr(expression: &mut WirExpr, family: &str, replacement: &str) -> usize {
    match expression {
        WirExpr::Call { func, args } => {
            let nested = args.iter_mut().map(|arg| rewrite_call_expr(arg, family, replacement)).sum::<usize>();
            if call_family(func, family) {
                *func = replacement.into();
                nested + 1
            } else { nested }
        }
        WirExpr::ToSlot(value, _) | WirExpr::FromSlot(value, _) | WirExpr::Unary { arg: value, .. }
        | WirExpr::Convert { arg: value, .. } | WirExpr::MemoryGrow(value) | WirExpr::ArrayLen(value)
        | WirExpr::RefIsNull(value) | WirExpr::Load { ptr: value, .. } | WirExpr::Load8U { ptr: value, .. }
        | WirExpr::StructGet { base: value, .. } | WirExpr::RefCast { value, .. }
        | WirExpr::RefCastNullable { value, .. } => rewrite_call_expr(value, family, replacement),
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            let mut rewritten = 0;
            place.for_each_wir_expr_mut(|value| {
                rewritten += rewrite_call_expr(value, family, replacement);
            });
            rewritten
        }
        WirExpr::Binary { lhs, rhs, .. } => rewrite_call_expr(lhs, family, replacement) + rewrite_call_expr(rhs, family, replacement),
        WirExpr::CallHost { args, .. } | WirExpr::StructNew { args, .. } | WirExpr::ArrayNewFixed { items: args, .. }
        | WirExpr::Vector { args, .. } => args.iter_mut().map(|arg| rewrite_call_expr(arg, family, replacement)).sum(),
        WirExpr::CallIndirect { args, index, .. } => args.iter_mut().map(|arg| rewrite_call_expr(arg, family, replacement)).sum::<usize>() + rewrite_call_expr(index, family, replacement),
        WirExpr::Control(node) => rewrite_call_node(node, family, replacement),
        WirExpr::Seq(sequence) => rewrite_call_seq(sequence, family, replacement),
        WirExpr::ArrayNew { value, len, .. } | WirExpr::ArrayGet { array: value, index: len, .. } => rewrite_call_expr(value, family, replacement) + rewrite_call_expr(len, family, replacement),
        _ => 0,
    }
}

/// Bind source-level operation descriptions to their physical carrier slots
/// exactly once, while both views are still available. A missing static slot,
/// mismatched direction, or non-direct lane rejects only this metadata capture;
/// the original scheduler path remains selected.
fn bind_storage_uses(
    channels: &[TypedChannelPlan],
    prefix: &[WirNode],
    operations: &[ReadyChannelOperation],
) -> Option<Vec<ReadyStorageUse>> {
    operations
        .iter()
        .map(|operation| {
            let endpoints = operation_endpoint_places(&operation.kind)
                .into_iter()
                .map(|(place, direction)| bind_storage_endpoint(channels, prefix, place, direction))
                .collect::<Option<Vec<_>>>()?;
            Some(ReadyStorageUse {
                transition_ordinal: operation.transition_ordinal,
                endpoints,
            })
        })
        .collect()
}

fn bind_storage_endpoint(
    channels: &[TypedChannelPlan],
    prefix: &[WirNode],
    place: &ChannelEndpointPlace,
    direction: ChannelEndpointDirection,
) -> Option<ReadyStorageEndpoint> {
    let ChannelEndpointPlace::Static(place) = place else {
        return None;
    };
    let place = projected_endpoint_root(prefix, place).unwrap_or(place);
    let channel = channels.iter().find(|channel| {
        channel.endpoint_place == *place && channel.direction == direction
    })?;
    Some(ReadyStorageEndpoint {
        slot: channel.endpoint_slot,
        lane: channel.endpoint_lane?,
        direction: channel.direction,
        payload_layout: channel.payload_layout.clone(),
        ownership: channel.ownership,
        message_type: channel.message_type.clone(),
    })
}

/// Async lowering may bind `tx`/`rx` by destructuring a direct tuple carrier
/// parameter. Retain that WIR projection boundary: the caller still binds the
/// operation by the source-derived static local, while storage identity stays
/// the original carrier parameter and lane.
fn projected_endpoint_root<'a>(prefix: &'a [WirNode], local: &'a str) -> Option<&'a str> {
    prefix.iter().find_map(|node| match node {
        WirNode::SetLocal {
            local: bound,
            value: WirExpr::StructGet { base, .. },
        } if bound == local => match base.as_ref() {
            WirExpr::GetLocal(root) => Some(root.as_str()),
            _ => None,
        },
        _ => None,
    })
}

fn operation_endpoint_places(
    operation: &ReadyChannelOperationKind,
) -> Vec<(&ChannelEndpointPlace, ChannelEndpointDirection)> {
    match operation {
        ReadyChannelOperationKind::Open { .. } => Vec::new(),
        ReadyChannelOperationKind::Send(send) => {
            vec![(&send.endpoint, ChannelEndpointDirection::Send)]
        }
        ReadyChannelOperationKind::Receive(receive) => {
            vec![(&receive.endpoint, ChannelEndpointDirection::Receive)]
        }
        ReadyChannelOperationKind::ReceiveFrameLoop(receive) => {
            vec![(&receive.channel, ChannelEndpointDirection::Receive)]
        }
        ReadyChannelOperationKind::Select2(select) => vec![
            (&select.first_channel, ChannelEndpointDirection::Receive),
            (&select.second_channel, ChannelEndpointDirection::Receive),
        ],
        ReadyChannelOperationKind::Select2FrameLoop(select) => vec![
            (&select.first_channel, ChannelEndpointDirection::Receive),
            (&select.second_channel, ChannelEndpointDirection::Receive),
        ],
        ReadyChannelOperationKind::Select3(select)
        | ReadyChannelOperationKind::Select4(select) => select
            .endpoints
            .iter()
            .map(|endpoint| (endpoint, ChannelEndpointDirection::Receive))
            .collect(),
        ReadyChannelOperationKind::Select3FrameLoop(select)
        | ReadyChannelOperationKind::Select4FrameLoop(select) => select
            .endpoints
            .iter()
            .map(|endpoint| (endpoint, ChannelEndpointDirection::Receive))
            .collect(),
    }
}

/// The async lowerer writes the task expression as the final statement of a
/// frame function. WIR's normal tail conversion represents that expression as
/// one final `Push` or `Return`; neither case is inferred from arbitrary nested
/// calls.  Any other final node stays on the scheduler.
fn split_terminal_task_node(body: &[WirNode]) -> Option<(WirNode, WirSeq)> {
    let (terminal, prefix) = body.split_last()?;
    let (terminal, mut nested_prefix) = split_terminal_value_node(terminal)?;
    let mut complete_prefix = prefix.to_vec();
    complete_prefix.append(&mut nested_prefix);
    Some((terminal, complete_prefix))
}

/// `lower_expr` represents a source expression with ordered effects as
/// `Push(Seq(...))`.  The generated async terminal is one such expression, so
/// the exact once-only prefix can be nested beneath its outer final `Push`.
/// Peel only that representation and require its inner sequence to finish in
/// another value node.  A generic nested call or control node is never treated
/// as a boundary.
fn split_terminal_value_node(node: &WirNode) -> Option<(WirNode, WirSeq)> {
    match node {
        WirNode::Push(WirExpr::Seq(sequence)) => {
            let (inner_terminal, prefix) = sequence.split_last()?;
            let (terminal, mut nested_prefix) = split_terminal_value_node(inner_terminal)?;
            let mut complete_prefix = prefix.to_vec();
            complete_prefix.append(&mut nested_prefix);
            Some((terminal, complete_prefix))
        }
        WirNode::Push(_) | WirNode::Return(Some(_)) => Some((node.clone(), Vec::new())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_types::suspension_carrier::{
        AwaitChannelOperationPlan, CarrierLane, ChannelMessageOperand,
    };
    use witchy_wir::wir::{WirExpr, WirNode};

    fn endpoint(
        place: &str,
        slot: usize,
        lane: usize,
        direction: ChannelEndpointDirection,
    ) -> TypedChannelPlan {
        TypedChannelPlan {
            state: 4,
            endpoint_slot: slot,
            endpoint_lane: Some(lane),
            endpoint_place: place.into(),
            direction,
            message_type: "Int".into(),
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            ownership: ChannelOwnershipEnvelope::InlineValue,
        }
    }

    fn operation(
        ordinal: usize,
        kind: ReadyChannelOperationKind,
    ) -> ReadyChannelOperation {
        ReadyChannelOperation {
            state: 4,
            transition_ordinal: ordinal,
            kind,
        }
    }

    #[test]
    fn splits_only_a_final_value_boundary() {
        let body = vec![
            WirNode::Do(WirExpr::CallHost { import: "effect".into(), args: vec![] }),
            WirNode::Push(WirExpr::ConstI32(0)),
        ];
        let (terminal, prefix) = split_terminal_task_node(&body).expect("final task value");
        assert!(matches!(terminal, WirNode::Push(WirExpr::ConstI32(0))));
        assert_eq!(prefix.len(), 1, "the effectful prefix is retained verbatim");
        assert!(split_terminal_task_node(&[WirNode::Do(WirExpr::ConstI32(0))]).is_none());
    }

    #[test]
    fn splits_effectful_sequence_inside_final_push() {
        let body = vec![WirNode::Push(WirExpr::Seq(vec![
            WirNode::Do(WirExpr::CallHost { import: "effect".into(), args: vec![] }),
            WirNode::Push(WirExpr::ConstI32(0)),
        ]))];
        let (terminal, prefix) = split_terminal_task_node(&body).expect("nested final task value");
        assert!(matches!(terminal, WirNode::Push(WirExpr::ConstI32(0))));
        assert!(matches!(prefix.as_slice(), [WirNode::Do(WirExpr::CallHost { import, .. })] if import == "effect"));
    }

    #[test]
    fn binds_operations_to_physical_typed_endpoint_lanes() {
        let channels = vec![
            endpoint("tx", 1, 1, ChannelEndpointDirection::Send),
            endpoint("rx", 3, 4, ChannelEndpointDirection::Receive),
        ];
        let send = AwaitChannelOperationPlan {
            endpoint: ChannelEndpointPlace::Static("tx".into()),
            message: Some(ChannelMessageOperand::Static("value".into())),
            result: None,
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            ownership: ChannelOwnershipEnvelope::InlineValue,
            resume_target: 5,
        };
        let receive = AwaitChannelOperationPlan {
            endpoint: ChannelEndpointPlace::Static("rx".into()),
            message: None,
            result: Some(witchy_types::suspension_carrier::ChannelReceiveResult::Discarded),
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            ownership: ChannelOwnershipEnvelope::InlineValue,
            resume_target: 6,
        };
        let uses = bind_storage_uses(
            &channels,
            &[],
            &[
                operation(2, ReadyChannelOperationKind::Send(send)),
                operation(5, ReadyChannelOperationKind::Receive(receive)),
            ],
        )
        .expect("static direct endpoints bind");
        assert_eq!(
            uses,
            vec![
                ReadyStorageUse {
                    transition_ordinal: 2,
                    endpoints: vec![ReadyStorageEndpoint {
                        slot: 1,
                        lane: 1,
                        direction: ChannelEndpointDirection::Send,
                        payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
                        ownership: ChannelOwnershipEnvelope::InlineValue,
                        message_type: "Int".into(),
                    }],
                },
                ReadyStorageUse {
                    transition_ordinal: 5,
                    endpoints: vec![ReadyStorageEndpoint {
                        slot: 3,
                        lane: 4,
                        direction: ChannelEndpointDirection::Receive,
                        payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
                        ownership: ChannelOwnershipEnvelope::InlineValue,
                        message_type: "Int".into(),
                    }],
                },
            ]
        );
    }

    #[test]
    fn rejects_missing_or_erased_storage_bindings() {
        let channels = vec![TypedChannelPlan {
            endpoint_lane: None,
            ..endpoint("rx", 0, 0, ChannelEndpointDirection::Receive)
        }];
        let receive = AwaitChannelOperationPlan {
            endpoint: ChannelEndpointPlace::Static("rx".into()),
            message: None,
            result: Some(witchy_types::suspension_carrier::ChannelReceiveResult::Discarded),
            payload_layout: ChannelPayloadLayout::Scalar(CarrierLane::I64),
            ownership: ChannelOwnershipEnvelope::InlineValue,
            resume_target: 6,
        };
        assert!(bind_storage_uses(
            &channels,
            &[],
            &[operation(0, ReadyChannelOperationKind::Receive(receive))],
        )
        .is_none());
    }

    #[test]
    fn storage_domains_keep_open_identity_and_typed_layout() {
        let states = vec![ReadyStateWir {
            state: 4,
            path_ordinal: 2,
            function: "open".into(),
            prefix: Vec::new(),
            terminal: WirNode::Push(WirExpr::ConstI32(0)),
            operations: vec![operation(
                3,
                ReadyChannelOperationKind::Open {
                    resume_target: 7,
                    capacity: ChannelMessageOperand::Int(8),
                    payload_layout: ChannelPayloadLayout::Packed(vec![
                        CarrierLane::I64,
                        CarrierLane::I64,
                    ]),
                    ownership: ChannelOwnershipEnvelope::InlineValue,
                },
            )],
            storage_uses: Some(Vec::new()),
        }];
        assert_eq!(
            storage_domains(&states),
            vec![ReadyTypedStorageDomain {
                opening_state: 4,
                opening_path_ordinal: 2,
                opening_transition_ordinal: 3,
                resume_state: 7,
                capacity: ChannelMessageOperand::Int(8),
                payload_layout: ChannelPayloadLayout::Packed(vec![
                    CarrierLane::I64,
                    CarrierLane::I64,
                ]),
                ownership: ChannelOwnershipEnvelope::InlineValue,
            }],
        );
    }
}
