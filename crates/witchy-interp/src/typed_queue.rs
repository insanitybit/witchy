//! Deterministic typed queue and wait protocol for RFC-0161 executions.
//!
//! This module deliberately stores opaque payload identities only. Value layout
//! and movement remain the execution frame's responsibility; the queue owns
//! ordering, capacity, registration generation, matching, cancellation, and
//! close/drain rules uniformly for every payload type.

use std::collections::{BTreeMap, VecDeque};

use witchy_types::typed_execution::{ExecutionOperation, OperationSiteId, QueueCapacity, QueueShape, QueueShapeId};

use crate::typed_async::{KernelTurn, OperationCommit, TypedExecution};

/// Backend-neutral identity for an already typed payload in an execution lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueuePayload(pub u64);

/// One intrusive execution wait registration. `generation` makes stale
/// notifications/cancellations unclaimable after a wait record is reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueWait {
    pub execution: u64,
    pub operation: OperationSiteId,
    pub generation: u64,
    /// A select arm shares the execution's one wait record but remains a
    /// distinct registration, including when two arms name the same endpoint.
    pub arm: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueSend {
    /// The payload was committed, optionally waking a FIFO receive waiter.
    Committed { receiver: Option<QueueWait> },
    /// The caller must install a send wait record with [`TypedQueue::register_send`].
    Blocked,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueReceive {
    /// A payload committed, optionally waking the FIFO blocked sender whose
    /// payload was promoted into the newly available capacity.
    Payload { payload: QueuePayload, sender: Option<QueueWait> },
    /// The caller must install a receive wait record with [`TypedQueue::register_receive`].
    Blocked,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueCloseWake {
    Send(QueueWait),
    Receive(QueueWait),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSend {
    wait: QueueWait,
    payload: QueuePayload,
}

/// One typed queue shape with bounded registrations and deterministic FIFO
/// waiter order. Dynamic capacity means unbounded logical growth; physical
/// chunk allocation belongs to a later value-storage adapter.
#[derive(Debug, Clone)]
pub struct TypedQueue {
    shape: QueueShape,
    payloads: VecDeque<QueuePayload>,
    send_waiters: VecDeque<PendingSend>,
    receive_waiters: VecDeque<QueueWait>,
    closed: bool,
}

/// Interpreter adapter which joins the generic queue protocol to canonical
/// typed executions. It transports opaque payload identities to receive result
/// lanes and commits only the wait record that owns the matching operation.
#[derive(Debug, Default)]
pub struct TypedQueueRuntime {
    queues: BTreeMap<QueueShapeId, TypedQueue>,
    executions: BTreeMap<u64, TypedExecution>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueRuntimeResult {
    Blocked,
    Closed,
    Committed {
        turns: Vec<(u64, KernelTurn)>,
        deliveries: Vec<(u64, OperationSiteId, QueuePayload)>,
    },
}

/// Admission result for one source-ordered typed select.  The caller supplies
/// payload identities only for send arms; endpoint identity and direction are
/// recovered from the waiting execution's typed arm table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueSelectResult {
    Blocked,
    /// The selected arm observed a closed queue.  RFC-0161's final execution
    /// edge contract must supply the close outcome before this can commit an
    /// execution; the coordinator therefore tears down registrations and
    /// restores the operation fallback rather than inventing a ready edge.
    Closed { arm: u32 },
    Committed {
        arm: u32,
        turns: Vec<(u64, KernelTurn)>,
        deliveries: Vec<(u64, OperationSiteId, QueuePayload)>,
    },
}

impl TypedQueue {
    pub fn new(shape: QueueShape) -> Self {
        Self {
            shape,
            payloads: VecDeque::new(),
            send_waiters: VecDeque::new(),
            receive_waiters: VecDeque::new(),
            closed: false,
        }
    }

    pub fn shape(&self) -> &QueueShape { &self.shape }
    pub fn is_closed(&self) -> bool { self.closed }
    pub fn buffered_len(&self) -> usize { self.payloads.len() }
    pub fn waiting_senders(&self) -> usize { self.send_waiters.len() }
    pub fn waiting_receivers(&self) -> usize { self.receive_waiters.len() }

    /// Probe/commit a send without installing a wait record.
    pub fn send(&mut self, payload: QueuePayload) -> QueueSend {
        if self.closed { return QueueSend::Closed; }
        if let Some(receiver) = self.receive_waiters.pop_front() {
            return QueueSend::Committed { receiver: Some(receiver) };
        }
        if self.has_buffer_capacity() {
            self.payloads.push_back(payload);
            return QueueSend::Committed { receiver: None };
        }
        QueueSend::Blocked
    }

    /// Register a blocked send. Registration reprobes first, so an event that
    /// arrived between an external probe and registration commits immediately.
    pub fn register_send(&mut self, wait: QueueWait, payload: QueuePayload) -> Result<QueueSend, String> {
        self.reject_duplicate(wait)?;
        let result = self.send(payload);
        if result == QueueSend::Blocked {
            self.send_waiters.push_back(PendingSend { wait, payload });
        }
        Ok(result)
    }

    /// Probe/commit a receive without installing a wait record.
    pub fn receive(&mut self) -> QueueReceive {
        if let Some(payload) = self.payloads.pop_front() {
            let sender = self.promote_one_sender();
            return QueueReceive::Payload { payload, sender };
        }
        if let Some(sender) = self.send_waiters.pop_front() {
            return QueueReceive::Payload { payload: sender.payload, sender: Some(sender.wait) };
        }
        if self.closed { QueueReceive::Closed } else { QueueReceive::Blocked }
    }

    /// Register a blocked receive, reprobeing under the same queue ownership
    /// before publishing the wait record.
    pub fn register_receive(&mut self, wait: QueueWait) -> Result<QueueReceive, String> {
        self.reject_duplicate(wait)?;
        let result = self.receive();
        if result == QueueReceive::Blocked {
            self.receive_waiters.push_back(wait);
        }
        Ok(result)
    }

    /// Cancel exactly one installed registration. A stale generation, wrong
    /// operation, or already committed wait cannot affect another execution.
    pub fn cancel(&mut self, wait: QueueWait) -> bool {
        if let Some(index) = self.send_waiters.iter().position(|pending| pending.wait == wait) {
            self.send_waiters.remove(index);
            return true;
        }
        if let Some(index) = self.receive_waiters.iter().position(|pending| *pending == wait) {
            self.receive_waiters.remove(index);
            return true;
        }
        false
    }

    /// Close rejects new sends, wakes every installed waiter in FIFO order, and
    /// leaves buffered payloads available for ordinary receive/drain.
    pub fn close(&mut self) -> Vec<QueueCloseWake> {
        if self.closed { return Vec::new(); }
        self.closed = true;
        let mut wakes = Vec::with_capacity(self.send_waiters.len() + self.receive_waiters.len());
        while let Some(sender) = self.send_waiters.pop_front() {
            wakes.push(QueueCloseWake::Send(sender.wait));
        }
        while let Some(receiver) = self.receive_waiters.pop_front() {
            wakes.push(QueueCloseWake::Receive(receiver));
        }
        wakes
    }

    fn has_buffer_capacity(&self) -> bool {
        match self.shape.capacity {
            QueueCapacity::Rendezvous => false,
            QueueCapacity::Fixed(capacity) => self.payloads.len() < capacity as usize,
            QueueCapacity::Dynamic => true,
        }
    }

    fn promote_one_sender(&mut self) -> Option<QueueWait> {
        if !self.has_buffer_capacity() { return None; }
        let sender = self.send_waiters.pop_front()?;
        self.payloads.push_back(sender.payload);
        Some(sender.wait)
    }

    fn reject_duplicate(&self, wait: QueueWait) -> Result<(), String> {
        if self.send_waiters.iter().any(|pending| pending.wait == wait)
            || self.receive_waiters.iter().any(|pending| *pending == wait)
        {
            return Err("execution already has this queue wait registration".into());
        }
        Ok(())
    }
}

impl TypedQueueRuntime {
    pub fn install_queue(&mut self, queue: TypedQueue) -> Result<(), String> {
        let id = queue.shape.id;
        if self.queues.insert(id, queue).is_some() {
            return Err("typed queue runtime already has this queue shape ID".into());
        }
        Ok(())
    }

    pub fn install_execution(&mut self, id: u64, execution: TypedExecution) -> Result<(), String> {
        if self.executions.insert(id, execution).is_some() {
            return Err("typed queue runtime already has this execution ID".into());
        }
        Ok(())
    }

    pub fn execution(&self, id: u64) -> Option<&TypedExecution> { self.executions.get(&id) }

    pub fn submit_send(&mut self, execution: u64, generation: u64, payload: QueuePayload) -> Result<QueueRuntimeResult, String> {
        let (operation, queue) = self.waiting_operation(execution, ExecutionOperation::ChannelSend)?;
        let wait = QueueWait { execution, operation, generation, arm: None };
        let result = self.queue_mut(queue)?.register_send(wait, payload)?;
        match result {
            QueueSend::Blocked => Ok(QueueRuntimeResult::Blocked),
            QueueSend::Closed => Ok(QueueRuntimeResult::Closed),
            QueueSend::Committed { receiver } => {
                let mut turns = vec![(execution, self.commit(execution, operation)?)];
                let mut deliveries = Vec::new();
                if let Some(receiver) = receiver {
                    self.ensure_wait(receiver, queue, ExecutionOperation::ChannelReceive)?;
                    turns.push((receiver.execution, self.commit(receiver.execution, receiver.operation)?));
                    deliveries.push((receiver.execution, receiver.operation, payload));
                }
                Ok(QueueRuntimeResult::Committed { turns, deliveries })
            }
        }
    }

    pub fn submit_receive(&mut self, execution: u64, generation: u64) -> Result<QueueRuntimeResult, String> {
        let (operation, queue) = self.waiting_operation(execution, ExecutionOperation::ChannelReceive)?;
        let wait = QueueWait { execution, operation, generation, arm: None };
        let result = self.queue_mut(queue)?.register_receive(wait)?;
        match result {
            QueueReceive::Blocked => Ok(QueueRuntimeResult::Blocked),
            QueueReceive::Closed => Ok(QueueRuntimeResult::Closed),
            QueueReceive::Payload { payload, sender } => {
                let mut turns = vec![(execution, self.commit(execution, operation)?)];
                let deliveries = vec![(execution, operation, payload)];
                if let Some(sender) = sender {
                    self.ensure_wait(sender, queue, ExecutionOperation::ChannelSend)?;
                    turns.push((sender.execution, self.commit(sender.execution, sender.operation)?));
                }
                Ok(QueueRuntimeResult::Committed { turns, deliveries })
            }
        }
    }

    pub fn cancel(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let (operation, queue, _) = self.waiting_operation_any(execution)?;
        let wait = QueueWait { execution, operation, generation, arm: None };
        if !self.queue_mut(queue)?.cancel(wait) { return Ok(false); }
        self.executions.get_mut(&execution)
            .expect("execution was checked above")
            .abort_operation(operation)?;
        Ok(true)
    }

    /// Probe/register every arm in source order.  A later ready arm cancels
    /// the exact earlier registrations before committing its declared select
    /// edge, so duplicate endpoints and stale generations cannot wake the
    /// losing arm later.
    pub fn submit_select(
        &mut self,
        execution: u64,
        generation: u64,
        send_payloads: &BTreeMap<u32, QueuePayload>,
    ) -> Result<QueueSelectResult, String> {
        let (operation, arms) = self.executions.get(&execution)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .waiting_select_arms()?;
        if arms.is_empty() { return Err("typed select has no arms".into()); }
        for ordinal in send_payloads.keys() {
            let Some((_, direction, _)) = arms.iter().find(|(arm, _, _)| arm == ordinal) else {
                return Err("select payload names an absent arm".into());
            };
            if *direction != ExecutionOperation::ChannelSend {
                return Err("select payload may be supplied only for a send arm".into());
            }
        }

        let mut registered = Vec::new();
        for (arm, direction, queue) in arms {
            let wait = QueueWait { execution, operation, generation, arm: Some(arm) };
            let admission = match direction {
                ExecutionOperation::ChannelSend => {
                    let payload = send_payloads.get(&arm)
                        .copied().ok_or_else(|| "typed select send arm has no payload binding".to_string())?;
                    SelectAdmission::Send(self.queue_mut(queue)?.register_send(wait, payload)?, payload)
                }
                ExecutionOperation::ChannelReceive => SelectAdmission::Receive(self.queue_mut(queue)?.register_receive(wait)?),
                _ => return Err("typed select arm is not a queue operation".into()),
            };
            match admission {
                SelectAdmission::Send(QueueSend::Blocked, _) | SelectAdmission::Receive(QueueReceive::Blocked) => {
                    registered.push((queue, wait));
                }
                SelectAdmission::Send(QueueSend::Closed, _) | SelectAdmission::Receive(QueueReceive::Closed) => {
                    self.cancel_waits(&registered)?;
                    self.abort_select(execution, operation)?;
                    return Ok(QueueSelectResult::Closed { arm });
                }
                SelectAdmission::Send(QueueSend::Committed { receiver }, payload) => {
                    self.cancel_waits(&registered)?;
                    let mut turns = vec![(execution, self.commit_wait(wait, queue, ExecutionOperation::ChannelSend)?)];
                    let mut deliveries = Vec::new();
                    if let Some(receiver) = receiver {
                        turns.push((receiver.execution, self.commit_wait(receiver, queue, ExecutionOperation::ChannelReceive)?));
                        deliveries.push((receiver.execution, receiver.operation, payload));
                    }
                    return Ok(QueueSelectResult::Committed { arm, turns, deliveries });
                }
                SelectAdmission::Receive(QueueReceive::Payload { payload, sender }) => {
                    self.cancel_waits(&registered)?;
                    let mut turns = vec![(execution, self.commit_wait(wait, queue, ExecutionOperation::ChannelReceive)?)];
                    let deliveries = vec![(execution, operation, payload)];
                    if let Some(sender) = sender {
                        turns.push((sender.execution, self.commit_wait(sender, queue, ExecutionOperation::ChannelSend)?));
                    }
                    return Ok(QueueSelectResult::Committed { arm, turns, deliveries });
                }
            }
        }
        Ok(QueueSelectResult::Blocked)
    }

    /// Cancel all registrations belonging to a select's one execution wait
    /// record.  The generation and every arm identity must still match.
    pub fn cancel_select(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let (operation, arms) = self.executions.get(&execution)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .waiting_select_arms()?;
        let mut cancelled = false;
        for (arm, _, queue) in arms {
            cancelled |= self.queue_mut(queue)?.cancel(QueueWait { execution, operation, generation, arm: Some(arm) });
        }
        if cancelled { self.abort_select(execution, operation)?; }
        Ok(cancelled)
    }

    fn waiting_operation(&self, execution: u64, expected: ExecutionOperation) -> Result<(OperationSiteId, QueueShapeId), String> {
        let (operation, queue, actual) = self.waiting_operation_any(execution)?;
        if actual != expected { return Err("execution wait has a different typed queue operation".into()); }
        Ok((operation, queue))
    }

    fn waiting_operation_any(&self, execution: u64) -> Result<(OperationSiteId, QueueShapeId, ExecutionOperation), String> {
        self.executions.get(&execution)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .waiting_queue_operation()
    }

    fn ensure_wait(&self, wait: QueueWait, queue: QueueShapeId, expected: ExecutionOperation) -> Result<(), String> {
        let (operation, actual_queue, actual) = self.waiting_operation_any(wait.execution)?;
        if operation != wait.operation || actual_queue != queue || actual != expected {
            return Err("queue match does not own the target execution wait record".into());
        }
        Ok(())
    }

    fn ensure_select_wait(&self, wait: QueueWait, queue: QueueShapeId, expected: ExecutionOperation) -> Result<(), String> {
        let arm = wait.arm.ok_or_else(|| "select wait is missing an arm identity".to_string())?;
        let (operation, arms) = self.executions.get(&wait.execution)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .waiting_select_arms()?;
        if operation != wait.operation
            || !arms.iter().any(|(ordinal, actual, actual_queue)| *ordinal == arm && *actual == expected && *actual_queue == queue)
        {
            return Err("queue match does not own the target select arm registration".into());
        }
        Ok(())
    }

    fn commit_wait(&mut self, wait: QueueWait, queue: QueueShapeId, expected: ExecutionOperation) -> Result<KernelTurn, String> {
        match wait.arm {
            Some(arm) => {
                self.ensure_select_wait(wait, queue, expected)?;
                self.executions.get_mut(&wait.execution)
                    .expect("execution was checked above")
                    .commit_operation(OperationCommit::SelectArm { operation: wait.operation, arm })
            }
            None => {
                self.ensure_wait(wait, queue, expected)?;
                self.commit(wait.execution, wait.operation)
            }
        }
    }

    fn cancel_waits(&mut self, waits: &[(QueueShapeId, QueueWait)]) -> Result<(), String> {
        for (queue, wait) in waits { self.queue_mut(*queue)?.cancel(*wait); }
        Ok(())
    }

    fn abort_select(&mut self, execution: u64, operation: OperationSiteId) -> Result<(), String> {
        self.executions.get_mut(&execution)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .abort_operation(operation)
    }

    fn queue_mut(&mut self, id: QueueShapeId) -> Result<&mut TypedQueue, String> {
        self.queues.get_mut(&id).ok_or_else(|| "typed queue runtime has no such queue shape".to_string())
    }

    fn commit(&mut self, execution: u64, operation: OperationSiteId) -> Result<KernelTurn, String> {
        self.executions.get_mut(&execution)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .commit_operation(OperationCommit::Ready { operation })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectAdmission {
    Send(QueueSend, QueuePayload),
    Receive(QueueReceive),
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::ast::Type;
    use witchy_types::typed_execution::{
        ExecutionOutcome, ExecutionShapeId, ExecutionState, ExecutionStateId,
        ExecutionTransition, FrameLane, FrameLaneId, LaneOwnership, OperationSite,
        PayloadOwnership, TypedExecutionShape,
    };

    fn queue(capacity: QueueCapacity) -> TypedQueue {
        queue_with_id(QueueShapeId(0), capacity)
    }

    fn queue_with_id(id: QueueShapeId, capacity: QueueCapacity) -> TypedQueue {
        TypedQueue::new(QueueShape {
            id,
            capacity,
            payload_layout_fingerprint: [3; 32],
            ownership: PayloadOwnership::Inline,
        })
    }

    fn wait(execution: u64, generation: u64) -> QueueWait {
        QueueWait { execution, operation: OperationSiteId(0), generation, arm: None }
    }

    fn execution(kind: ExecutionOperation) -> TypedExecution {
        let shape = TypedExecutionShape {
            id: ExecutionShapeId(0),
            entry: ExecutionStateId(0),
            frame: vec![FrameLane {
                id: FrameLaneId(0),
                ty: Type::Named("Int".into(), Vec::new()),
                ownership: LaneOwnership::Move,
                initially_initialized: true,
            }],
            result_lanes: if kind == ExecutionOperation::ChannelReceive {
                vec![FrameLaneId(0)]
            } else {
                vec![]
            },
            states: vec![
                ExecutionState {
                    id: ExecutionStateId(0),
                    transitions: vec![ExecutionTransition {
                        condition: witchy_types::typed_execution::ExecutionEdgeCondition::Always,
                        actions: vec![],
                        outcome: ExecutionOutcome::Blocked {
                            operation: OperationSiteId(0),
                            retry: ExecutionStateId(1),
                        },
                    }],
                },
                ExecutionState {
                    id: ExecutionStateId(1),
                    transitions: vec![ExecutionTransition {
                        condition: witchy_types::typed_execution::ExecutionEdgeCondition::Always,
                        actions: vec![],
                        outcome: ExecutionOutcome::Complete,
                    }],
                },
            ],
            operations: vec![OperationSite {
                id: OperationSiteId(0),
                kind,
                input_lanes: vec![FrameLaneId(0)],
                result_lanes: if kind == ExecutionOperation::ChannelReceive { vec![FrameLaneId(0)] } else { vec![] },
                pending_move_lanes: vec![],
                fallback_state: Some(ExecutionStateId(0)),
                queue: Some(QueueShapeId(0)),
                arms: vec![],
            }],
            queues: vec![QueueShape {
                id: QueueShapeId(0),
                capacity: QueueCapacity::Rendezvous,
                payload_layout_fingerprint: [3; 32],
                ownership: PayloadOwnership::Inline,
            }],
            cancellation: None,
        };
        let mut execution = TypedExecution::new(shape).expect("valid execution");
        assert_eq!(execution.turn(), Ok(KernelTurn::Blocked(OperationSiteId(0))));
        execution
    }

    fn select_execution(arms: &[(QueueShapeId, ExecutionOperation)]) -> TypedExecution {
        let shape = TypedExecutionShape {
            id: ExecutionShapeId(0),
            entry: ExecutionStateId(0),
            frame: vec![FrameLane {
                id: FrameLaneId(0),
                ty: Type::Named("Int".into(), Vec::new()),
                ownership: LaneOwnership::Move,
                initially_initialized: true,
            }],
            states: vec![
                ExecutionState {
                    id: ExecutionStateId(0),
                    transitions: vec![ExecutionTransition {
                        condition: witchy_types::typed_execution::ExecutionEdgeCondition::Always,
                        actions: vec![],
                        outcome: ExecutionOutcome::Blocked {
                            operation: OperationSiteId(0),
                            retry: ExecutionStateId(1),
                        },
                    }],
                },
                ExecutionState {
                    id: ExecutionStateId(1),
                    transitions: arms.iter().enumerate().map(|(ordinal, _)| ExecutionTransition {
                        condition: witchy_types::typed_execution::ExecutionEdgeCondition::SelectArm {
                            operation: OperationSiteId(0), arm: ordinal as u32,
                        },
                        actions: vec![],
                        outcome: ExecutionOutcome::Complete,
                    }).collect(),
                },
            ],
            operations: vec![OperationSite {
                id: OperationSiteId(0),
                kind: ExecutionOperation::Select,
                input_lanes: vec![],
                result_lanes: vec![],
                pending_move_lanes: vec![],
                fallback_state: Some(ExecutionStateId(0)),
                queue: None,
                arms: arms.iter().enumerate().map(|(ordinal, (queue, operation))| {
                    witchy_types::typed_execution::OperationArm {
                        ordinal: ordinal as u32,
                        operation: *operation,
                        endpoint_lane: FrameLaneId(0),
                        input_lanes: vec![],
                        result_lanes: if *operation == ExecutionOperation::ChannelReceive { vec![FrameLaneId(0)] } else { vec![] },
                        queue: Some(*queue),
                        ownership: PayloadOwnership::Inline,
                    }
                }).collect(),
            }],
            queues: arms.iter().fold(Vec::new(), |mut queues, (id, _)| {
                if !queues.iter().any(|queue: &QueueShape| queue.id == *id) {
                    queues.push(QueueShape {
                        id: *id,
                        capacity: QueueCapacity::Dynamic,
                        payload_layout_fingerprint: [3; 32],
                        ownership: PayloadOwnership::Inline,
                    });
                }
                queues
            }),
            cancellation: None,
        };
        let mut execution = TypedExecution::new(shape).expect("valid select execution");
        assert_eq!(execution.turn(), Ok(KernelTurn::Blocked(OperationSiteId(0))));
        execution
    }

    #[test]
    fn rendezvous_registers_then_commits_to_the_fifo_receiver() {
        let mut queue = queue(QueueCapacity::Rendezvous);
        let receiver = wait(1, 0);
        assert_eq!(queue.register_receive(receiver), Ok(QueueReceive::Blocked));
        assert_eq!(queue.send(QueuePayload(7)), QueueSend::Committed { receiver: Some(receiver) });
        assert_eq!(queue.waiting_receivers(), 0);
    }

    #[test]
    fn fixed_capacity_promotes_one_blocked_sender_after_a_receive() {
        let mut queue = queue(QueueCapacity::Fixed(1));
        assert_eq!(queue.send(QueuePayload(1)), QueueSend::Committed { receiver: None });
        let sender = wait(2, 0);
        assert_eq!(queue.register_send(sender, QueuePayload(2)), Ok(QueueSend::Blocked));
        assert_eq!(queue.receive(), QueueReceive::Payload { payload: QueuePayload(1), sender: Some(sender) });
        assert_eq!(queue.receive(), QueueReceive::Payload { payload: QueuePayload(2), sender: None });
    }

    #[test]
    fn cancellation_is_generation_exact_and_close_drains_buffered_payloads() {
        let mut queue = queue(QueueCapacity::Fixed(2));
        assert_eq!(queue.send(QueuePayload(1)), QueueSend::Committed { receiver: None });
        let receiver = wait(3, 1);
        assert_eq!(queue.register_receive(receiver), Ok(QueueReceive::Payload { payload: QueuePayload(1), sender: None }));
        let blocked = wait(4, 2);
        assert_eq!(queue.register_receive(blocked), Ok(QueueReceive::Blocked));
        assert!(!queue.cancel(wait(4, 1)), "stale generation cannot cancel");
        assert!(queue.cancel(blocked));
        assert!(queue.close().is_empty());
        assert_eq!(queue.send(QueuePayload(2)), QueueSend::Closed);
        assert_eq!(queue.receive(), QueueReceive::Closed);
    }

    #[test]
    fn close_wakes_registered_senders_then_receivers_in_fifo_order() {
        let mut queue = queue(QueueCapacity::Rendezvous);
        let sender = wait(1, 0);
        let receiver = wait(2, 0);
        assert_eq!(queue.register_send(sender, QueuePayload(9)), Ok(QueueSend::Blocked));
        assert_eq!(queue.register_receive(receiver), Ok(QueueReceive::Payload { payload: QueuePayload(9), sender: Some(sender) }));
        let sender = wait(3, 0);
        let receiver = wait(4, 0);
        assert_eq!(queue.register_send(sender, QueuePayload(10)), Ok(QueueSend::Blocked));
        assert_eq!(queue.register_receive(receiver), Ok(QueueReceive::Payload { payload: QueuePayload(10), sender: Some(sender) }));
        let sender = wait(5, 0);
        assert_eq!(queue.register_send(sender, QueuePayload(11)), Ok(QueueSend::Blocked));
        let wakes = queue.close();
        assert_eq!(wakes, vec![QueueCloseWake::Send(sender)]);
    }

    #[test]
    fn dynamic_capacity_never_routes_ready_payloads_through_waiters() {
        let mut queue = queue(QueueCapacity::Dynamic);
        for payload in 0..32 {
            assert_eq!(queue.send(QueuePayload(payload)), QueueSend::Committed { receiver: None });
        }
        assert_eq!(queue.buffered_len(), 32);
        for payload in 0..32 {
            assert_eq!(queue.receive(), QueueReceive::Payload { payload: QueuePayload(payload), sender: None });
        }
        assert_eq!(queue.waiting_senders(), 0);
        assert_eq!(queue.waiting_receivers(), 0);
    }

    #[test]
    fn a_wait_identity_cannot_be_registered_twice() {
        let mut queue = queue(QueueCapacity::Rendezvous);
        let receiver = wait(1, 3);
        assert_eq!(queue.register_receive(receiver), Ok(QueueReceive::Blocked));
        assert!(queue.register_receive(receiver).expect_err("duplicate wait").contains("already has"));
        assert!(queue.cancel(receiver));
        assert_eq!(queue.register_receive(receiver), Ok(QueueReceive::Blocked));
    }

    #[test]
    fn distinct_select_arms_may_register_the_same_execution_wait_on_one_queue() {
        let mut queue = queue(QueueCapacity::Rendezvous);
        let first = QueueWait { execution: 1, operation: OperationSiteId(0), generation: 3, arm: Some(0) };
        let second = QueueWait { arm: Some(1), ..first };
        assert_eq!(queue.register_receive(first), Ok(QueueReceive::Blocked));
        assert_eq!(queue.register_receive(second), Ok(QueueReceive::Blocked));
        assert_eq!(queue.close(), vec![QueueCloseWake::Receive(first), QueueCloseWake::Receive(second)]);
    }

    #[test]
    fn runtime_commits_matching_send_and_receive_operations_by_wait_identity() {
        let mut runtime = TypedQueueRuntime::default();
        runtime.install_queue(queue(QueueCapacity::Rendezvous)).expect("queue");
        runtime.install_execution(1, execution(ExecutionOperation::ChannelReceive)).expect("receiver");
        runtime.install_execution(2, execution(ExecutionOperation::ChannelSend)).expect("sender");
        assert_eq!(runtime.submit_receive(1, 0), Ok(QueueRuntimeResult::Blocked));
        assert_eq!(
            runtime.submit_send(2, 0, QueuePayload(42)),
            Ok(QueueRuntimeResult::Committed {
                turns: vec![(2, KernelTurn::Complete), (1, KernelTurn::Complete)],
                deliveries: vec![(1, OperationSiteId(0), QueuePayload(42))],
            }),
        );
    }

    #[test]
    fn runtime_cancel_requires_the_registered_generation_and_restores_the_execution() {
        let mut runtime = TypedQueueRuntime::default();
        runtime.install_queue(queue(QueueCapacity::Rendezvous)).expect("queue");
        runtime.install_execution(1, execution(ExecutionOperation::ChannelReceive)).expect("receiver");
        assert_eq!(runtime.submit_receive(1, 4), Ok(QueueRuntimeResult::Blocked));
        assert!(!runtime.cancel(1, 3).expect("stale cancel"));
        assert!(runtime.cancel(1, 4).expect("matching cancel"));
        assert_eq!(runtime.execution(1).expect("execution").status(), crate::typed_async::KernelStatus::Ready);
        assert_eq!(runtime.execution(1).expect("execution").state(), ExecutionStateId(0));
    }

    #[test]
    fn select_commits_the_first_ready_arm_in_source_order() {
        let mut runtime = TypedQueueRuntime::default();
        let mut first = queue_with_id(QueueShapeId(0), QueueCapacity::Fixed(1));
        let mut second = queue_with_id(QueueShapeId(1), QueueCapacity::Fixed(1));
        assert_eq!(first.send(QueuePayload(10)), QueueSend::Committed { receiver: None });
        assert_eq!(second.send(QueuePayload(20)), QueueSend::Committed { receiver: None });
        runtime.install_queue(first).expect("first queue");
        runtime.install_queue(second).expect("second queue");
        runtime.install_execution(1, select_execution(&[
            (QueueShapeId(0), ExecutionOperation::ChannelReceive),
            (QueueShapeId(1), ExecutionOperation::ChannelReceive),
        ])).expect("select execution");
        assert_eq!(
            runtime.submit_select(1, 4, &BTreeMap::new()),
            Ok(QueueSelectResult::Committed {
                arm: 0,
                turns: vec![(1, KernelTurn::Complete)],
                deliveries: vec![(1, OperationSiteId(0), QueuePayload(10))],
            }),
        );
    }

    #[test]
    fn select_cancellation_removes_each_arm_only_at_its_generation() {
        let mut runtime = TypedQueueRuntime::default();
        runtime.install_queue(queue(QueueCapacity::Rendezvous)).expect("queue");
        runtime.install_execution(1, select_execution(&[
            (QueueShapeId(0), ExecutionOperation::ChannelReceive),
            (QueueShapeId(0), ExecutionOperation::ChannelReceive),
        ])).expect("select execution");
        assert_eq!(runtime.submit_select(1, 9, &BTreeMap::new()), Ok(QueueSelectResult::Blocked));
        assert!(!runtime.cancel_select(1, 8).expect("stale cancellation"));
        assert!(runtime.cancel_select(1, 9).expect("matching cancellation"));
        assert_eq!(runtime.execution(1).expect("execution").status(), crate::typed_async::KernelStatus::Ready);
        assert_eq!(runtime.execution(1).expect("execution").state(), ExecutionStateId(0));
    }

    #[test]
    fn select_send_commits_the_exact_waiting_peer_and_carries_its_payload() {
        let mut runtime = TypedQueueRuntime::default();
        runtime.install_queue(queue(QueueCapacity::Rendezvous)).expect("queue");
        runtime.install_execution(1, execution(ExecutionOperation::ChannelReceive)).expect("receiver");
        runtime.install_execution(2, select_execution(&[
            (QueueShapeId(0), ExecutionOperation::ChannelSend),
        ])).expect("select execution");
        assert_eq!(runtime.submit_receive(1, 0), Ok(QueueRuntimeResult::Blocked));
        assert_eq!(
            runtime.submit_select(2, 0, &BTreeMap::from([(0, QueuePayload(77))])),
            Ok(QueueSelectResult::Committed {
                arm: 0,
                turns: vec![(2, KernelTurn::Complete), (1, KernelTurn::Complete)],
                deliveries: vec![(1, OperationSiteId(0), QueuePayload(77))],
            }),
        );
    }

    #[test]
    fn select_rejects_missing_send_payload_before_registering_any_arm() {
        let mut runtime = TypedQueueRuntime::default();
        runtime.install_queue(queue(QueueCapacity::Rendezvous)).expect("queue");
        runtime.install_execution(1, select_execution(&[
            (QueueShapeId(0), ExecutionOperation::ChannelSend),
        ])).expect("select execution");
        assert!(runtime.submit_select(1, 0, &BTreeMap::new())
            .expect_err("send payload is mandatory").contains("no payload"));
        assert_eq!(runtime.execution(1).expect("execution").status(), crate::typed_async::KernelStatus::Waiting(OperationSiteId(0)));
    }
}
