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
            let storage_uses = bind_storage_uses(&region.typed_channels, &region.operations);
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

/// Bind source-level operation descriptions to their physical carrier slots
/// exactly once, while both views are still available. A missing static slot,
/// mismatched direction, or non-direct lane rejects only this metadata capture;
/// the original scheduler path remains selected.
fn bind_storage_uses(
    channels: &[TypedChannelPlan],
    operations: &[ReadyChannelOperation],
) -> Option<Vec<ReadyStorageUse>> {
    operations
        .iter()
        .map(|operation| {
            let endpoints = operation_endpoint_places(&operation.kind)
                .into_iter()
                .map(|(place, direction)| bind_storage_endpoint(channels, place, direction))
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
    place: &ChannelEndpointPlace,
    direction: ChannelEndpointDirection,
) -> Option<ReadyStorageEndpoint> {
    let ChannelEndpointPlace::Static(place) = place else {
        return None;
    };
    let channel = channels.iter().find(|channel| {
        channel.endpoint_place == *place && channel.direction == direction
    })?;
    Some(ReadyStorageEndpoint {
        slot: channel.endpoint_slot,
        lane: channel.endpoint_lane?,
        direction: channel.direction,
        payload_layout: channel.payload_layout.clone(),
        ownership: channel.ownership,
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
