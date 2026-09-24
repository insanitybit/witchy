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
    Send { wait: QueueWait, payload: QueuePayload },
    Receive(QueueWait),
}

/// Typed operation completion supplied to the frame adapter before the kernel
/// follows its already-typed result edge.  `arm` identifies heterogeneous
/// select result layouts without introducing a selected-result wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueCompletion {
    SendAccepted,
    SendClosed { payload: QueuePayload },
    ReceivePayload { payload: QueuePayload },
    ReceiveClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueDelivery {
    pub execution: u64,
    pub operation: OperationSiteId,
    pub arm: Option<u32>,
    pub completion: QueueCompletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSend {
    wait: QueueWait,
    payload: QueuePayload,
    sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingReceive {
    wait: QueueWait,
    sequence: u64,
}

/// One typed queue shape with bounded registrations and deterministic FIFO
/// waiter order. Dynamic capacity means unbounded logical growth; physical
/// chunk allocation belongs to a later value-storage adapter.
#[derive(Debug, Clone)]
pub struct TypedQueue {
    shape: QueueShape,
    payloads: VecDeque<QueuePayload>,
    send_waiters: VecDeque<PendingSend>,
    receive_waiters: VecDeque<PendingReceive>,
    next_wait_sequence: u64,
    closed: bool,
}

/// Interpreter adapter which joins the generic queue protocol to canonical
/// typed executions. It transports opaque payload identities to receive result
/// lanes and commits only the wait record that owns the matching operation.
#[derive(Debug, Default)]
pub struct TypedQueueRuntime {
    queues: BTreeMap<QueueShapeId, TypedQueue>,
    executions: BTreeMap<u64, TypedExecution>,
    host_waits: BTreeMap<u64, HostWait>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostWait {
    execution: u64,
    operation: OperationSiteId,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueRuntimeResult {
    Blocked,
    Committed {
        turns: Vec<(u64, KernelTurn)>,
        deliveries: Vec<QueueDelivery>,
    },
}

/// Admission result for one source-ordered typed select.  The caller supplies
/// payload identities only for send arms; endpoint identity and direction are
/// recovered from the waiting execution's typed arm table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueSelectResult {
    Blocked,
    Committed {
        arm: u32,
        turns: Vec<(u64, KernelTurn)>,
        deliveries: Vec<QueueDelivery>,
    },
}

impl TypedQueue {
    pub fn new(shape: QueueShape) -> Self {
        Self {
            shape,
            payloads: VecDeque::new(),
            send_waiters: VecDeque::new(),
            receive_waiters: VecDeque::new(),
            next_wait_sequence: 0,
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
            return QueueSend::Committed { receiver: Some(receiver.wait) };
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
            let sequence = self.reserve_wait_sequence()?;
            self.send_waiters.push_back(PendingSend { wait, payload, sequence });
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
            let sequence = self.reserve_wait_sequence()?;
            self.receive_waiters.push_back(PendingReceive { wait, sequence });
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
        if let Some(index) = self.receive_waiters.iter().position(|pending| pending.wait == wait) {
            self.receive_waiters.remove(index);
            return true;
        }
        false
    }

    /// Close rejects new sends, wakes every installed waiter in global FIFO order, and
    /// leaves buffered payloads available for ordinary receive/drain.
    pub fn close(&mut self) -> Vec<QueueCloseWake> {
        if self.closed { return Vec::new(); }
        self.closed = true;
        let mut wakes = Vec::with_capacity(self.send_waiters.len() + self.receive_waiters.len());
        while let Some(sender) = self.send_waiters.pop_front() {
            wakes.push((sender.sequence, QueueCloseWake::Send { wait: sender.wait, payload: sender.payload }));
        }
        while let Some(receiver) = self.receive_waiters.pop_front() {
            wakes.push((receiver.sequence, QueueCloseWake::Receive(receiver.wait)));
        }
        wakes.sort_by_key(|(sequence, _)| *sequence);
        wakes.into_iter().map(|(_, wake)| wake).collect()
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
            || self.receive_waiters.iter().any(|pending| pending.wait == wait)
        {
            return Err("execution already has this queue wait registration".into());
        }
        Ok(())
    }

    fn reserve_wait_sequence(&mut self) -> Result<u64, String> {
        let sequence = self.next_wait_sequence;
        self.next_wait_sequence = self.next_wait_sequence.checked_add(1)
            .ok_or_else(|| "typed queue wait sequence overflow".to_string())?;
        Ok(sequence)
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
    pub fn has_queue(&self, id: QueueShapeId) -> bool { self.queues.contains_key(&id) }

    /// Advance one ready execution through its canonical operation-level turn.
    /// Scheduler code uses this narrow entry point so it cannot bypass the
    /// runtime's execution ownership map.
    pub(crate) fn turn_execution(&mut self, id: u64) -> Result<KernelTurn, String> {
        self.executions.get_mut(&id)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .turn()
    }

    pub(crate) fn execution_mut_turn_selected(&mut self, id: u64, transition: usize) -> Result<KernelTurn, String> {
        self.executions.get_mut(&id)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .turn_selected(transition)
    }

    /// Wake exactly the operation named by an execution's single wait record.
    /// Queue adapters still validate generation and queue identity before
    /// calling this method.
    pub(crate) fn wake_execution(&mut self, id: u64, operation: OperationSiteId) -> Result<(), String> {
        self.executions.get_mut(&id)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .wake(operation)
    }

    /// Register one host operation in the same execution registry as queue
    /// operations. The host token is opaque; generation claims the exact wait.
    pub(crate) fn submit_host(&mut self, execution: u64, generation: u64, host: u64) -> Result<(), String> {
        if self.host_waits.contains_key(&host) {
            return Err("host operation already has a typed wait registration".into());
        }
        let (operation, kind) = self.executions.get(&execution)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .waiting_operation()?;
        if kind != ExecutionOperation::Host {
            return Err("execution wait is not a typed host operation".into());
        }
        if self.host_waits.values().any(|wait| wait.execution == execution) {
            return Err("execution already has a typed host wait registration".into());
        }
        self.host_waits.insert(host, HostWait { execution, operation, generation });
        Ok(())
    }

    /// Complete an exact host wait and return the operation-level turn. Stale
    /// or duplicate host notifications are harmless and return `None`.
    pub(crate) fn complete_host(&mut self, host: u64, generation: u64) -> Result<Option<(u64, OperationSiteId, KernelTurn)>, String> {
        let Some(wait) = self.host_waits.get(&host).copied() else { return Ok(None) };
        if wait.generation != generation { return Ok(None); }
        self.host_waits.remove(&host);
        let turn = self.executions.get_mut(&wait.execution)
            .ok_or_else(|| "typed host wait execution vanished".to_string())?
            .commit_operation(OperationCommit::Ready { operation: wait.operation })?;
        Ok(Some((wait.execution, wait.operation, turn)))
    }

    pub(crate) fn request_host_cancellation(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let Some((host, wait)) = self.host_waits.iter()
            .find(|(_, wait)| wait.execution == execution && wait.generation == generation)
            .map(|(host, wait)| (*host, *wait)) else { return Ok(false) };
        self.host_waits.remove(&host);
        self.executions.get_mut(&wait.execution)
            .ok_or_else(|| "typed host wait execution vanished".to_string())?
            .request_cancellation()
    }

    pub fn submit_send(&mut self, execution: u64, generation: u64, payload: QueuePayload) -> Result<QueueRuntimeResult, String> {
        let (operation, queue) = self.waiting_operation(execution, ExecutionOperation::ChannelSend)?;
        let wait = QueueWait { execution, operation, generation, arm: None };
        let result = self.queue_mut(queue)?.register_send(wait, payload)?;
        match result {
            QueueSend::Blocked => Ok(QueueRuntimeResult::Blocked),
            QueueSend::Closed => Ok(QueueRuntimeResult::Committed {
                turns: vec![(execution, self.commit_wait(wait, queue, ExecutionOperation::ChannelSend)?)],
                deliveries: vec![QueueDelivery {
                    execution,
                    operation,
                    arm: None,
                    completion: QueueCompletion::SendClosed { payload },
                }],
            }),
            QueueSend::Committed { receiver } => {
                let mut turns = vec![(execution, self.commit(execution, operation)?)];
                let mut deliveries = vec![QueueDelivery {
                    execution,
                    operation,
                    arm: None,
                    completion: QueueCompletion::SendAccepted,
                }];
                if let Some(receiver) = receiver {
                    self.ensure_wait(receiver, queue, ExecutionOperation::ChannelReceive)?;
                    turns.push((receiver.execution, self.commit(receiver.execution, receiver.operation)?));
                    deliveries.push(QueueDelivery {
                        execution: receiver.execution,
                        operation: receiver.operation,
                        arm: None,
                        completion: QueueCompletion::ReceivePayload { payload },
                    });
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
            QueueReceive::Closed => Ok(QueueRuntimeResult::Committed {
                turns: vec![(execution, self.commit_wait(wait, queue, ExecutionOperation::ChannelReceive)?)],
                deliveries: vec![QueueDelivery {
                    execution,
                    operation,
                    arm: None,
                    completion: QueueCompletion::ReceiveClosed,
                }],
            }),
            QueueReceive::Payload { payload, sender } => {
                let mut turns = vec![(execution, self.commit(execution, operation)?)];
                let mut deliveries = vec![QueueDelivery {
                    execution,
                    operation,
                    arm: None,
                    completion: QueueCompletion::ReceivePayload { payload },
                }];
                if let Some(sender) = sender {
                    self.ensure_wait(sender, queue, ExecutionOperation::ChannelSend)?;
                    turns.push((sender.execution, self.commit(sender.execution, sender.operation)?));
                    deliveries.push(QueueDelivery {
                        execution: sender.execution,
                        operation: sender.operation,
                        arm: None,
                        completion: QueueCompletion::SendAccepted,
                    });
                }
                Ok(QueueRuntimeResult::Committed { turns, deliveries })
            }
        }
    }

    /// Linearize explicit channel close and complete every installed wait in
    /// global registration order. Buffered payloads remain queued for later
    /// receive/drain operations.
    pub fn close_queue(&mut self, queue: QueueShapeId) -> Result<QueueRuntimeResult, String> {
        let wakes = self.queue_mut(queue)?.close();
        let mut ordinary = Vec::new();
        let mut selects = BTreeMap::<(u64, OperationSiteId, u64), (usize, QueueWait, QueueCompletion)>::new();
        for (index, wake) in wakes.into_iter().enumerate() {
            let (wait, direction, completion) = match wake {
                QueueCloseWake::Send { wait, payload } => (
                    wait,
                    ExecutionOperation::ChannelSend,
                    QueueCompletion::SendClosed { payload },
                ),
                QueueCloseWake::Receive(wait) => (wait, ExecutionOperation::ChannelReceive, QueueCompletion::ReceiveClosed),
            };
            if let Some(arm) = wait.arm {
                let key = (wait.execution, wait.operation, wait.generation);
                match selects.get(&key) {
                    Some((_, selected, _)) if selected.arm.is_some_and(|selected_arm| selected_arm <= arm) => {}
                    _ => { selects.insert(key, (index, wait, completion)); }
                }
            } else {
                ordinary.push((index, wait, direction, completion));
            }
        }
        let mut selected = ordinary;
        for (index, wait, completion) in selects.into_values() {
            let direction = match completion {
                QueueCompletion::SendClosed { .. } => ExecutionOperation::ChannelSend,
                QueueCompletion::ReceiveClosed => ExecutionOperation::ChannelReceive,
                QueueCompletion::SendAccepted | QueueCompletion::ReceivePayload { .. } => unreachable!("close has no success completion"),
            };
            selected.push((index, wait, direction, completion));
        }
        selected.sort_by_key(|(index, _, _, _)| *index);
        let mut turns = Vec::with_capacity(selected.len());
        let mut deliveries = Vec::with_capacity(selected.len());
        for (_, wait, direction, completion) in selected {
            if wait.arm.is_some() { self.cancel_select_losers(wait)?; }
            turns.push((wait.execution, self.commit_wait(wait, queue, direction)?));
            deliveries.push(QueueDelivery {
                execution: wait.execution,
                operation: wait.operation,
                arm: wait.arm,
                completion,
            });
        }
        Ok(QueueRuntimeResult::Committed { turns, deliveries })
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
                SelectAdmission::Send(QueueSend::Closed, payload) => {
                    self.cancel_waits(&registered)?;
                    return Ok(QueueSelectResult::Committed {
                        arm,
                        turns: vec![(execution, self.commit_wait(wait, queue, ExecutionOperation::ChannelSend)?)],
                        deliveries: vec![QueueDelivery {
                            execution,
                            operation,
                            arm: Some(arm),
                            completion: QueueCompletion::SendClosed { payload },
                        }],
                    });
                }
                SelectAdmission::Receive(QueueReceive::Closed) => {
                    self.cancel_waits(&registered)?;
                    return Ok(QueueSelectResult::Committed {
                        arm,
                        turns: vec![(execution, self.commit_wait(wait, queue, ExecutionOperation::ChannelReceive)?)],
                        deliveries: vec![QueueDelivery {
                            execution,
                            operation,
                            arm: Some(arm),
                            completion: QueueCompletion::ReceiveClosed,
                        }],
                    });
                }
                SelectAdmission::Send(QueueSend::Committed { receiver }, payload) => {
                    self.cancel_waits(&registered)?;
                    let mut turns = vec![(execution, self.commit_wait(wait, queue, ExecutionOperation::ChannelSend)?)];
                    let mut deliveries = vec![QueueDelivery {
                        execution,
                        operation,
                        arm: Some(arm),
                        completion: QueueCompletion::SendAccepted,
                    }];
                    if let Some(receiver) = receiver {
                        turns.push((receiver.execution, self.commit_wait(receiver, queue, ExecutionOperation::ChannelReceive)?));
                        deliveries.push(QueueDelivery {
                            execution: receiver.execution,
                            operation: receiver.operation,
                            arm: receiver.arm,
                            completion: QueueCompletion::ReceivePayload { payload },
                        });
                    }
                    return Ok(QueueSelectResult::Committed { arm, turns, deliveries });
                }
                SelectAdmission::Receive(QueueReceive::Payload { payload, sender }) => {
                    self.cancel_waits(&registered)?;
                    let mut turns = vec![(execution, self.commit_wait(wait, queue, ExecutionOperation::ChannelReceive)?)];
                    let mut deliveries = vec![QueueDelivery {
                        execution,
                        operation,
                        arm: Some(arm),
                        completion: QueueCompletion::ReceivePayload { payload },
                    }];
                    if let Some(sender) = sender {
                        turns.push((sender.execution, self.commit_wait(sender, queue, ExecutionOperation::ChannelSend)?));
                        deliveries.push(QueueDelivery {
                            execution: sender.execution,
                            operation: sender.operation,
                            arm: sender.arm,
                            completion: QueueCompletion::SendAccepted,
                        });
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

    /// Apply a cooperative cancellation request to an execution that is parked
    /// on this queue runtime.  The generation must claim every installed wait
    /// link first; only then can the execution enter its declared cleanup
    /// state.  This makes wake-versus-cancel deterministic and prevents a
    /// later queue event from observing a cancelled select arm.
    pub fn request_cancellation(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let cancellable = self.executions.get(&execution)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?;
        if !cancellable.has_cancellation_transition() {
            return Err("execution shape has no cancellation transition".into());
        }
        if !matches!(cancellable.status(), crate::typed_async::KernelStatus::Waiting(_)) {
            return self.executions.get_mut(&execution)
                .expect("execution was checked above")
                .request_cancellation();
        }
        // Host waits live in this same execution registry but have no queue
        // link. Claiming the exact host generation here keeps scheduler-level
        // cancellation uniform across queue and capability I/O operations.
        if self.host_waits.values().any(|wait| wait.execution == execution) {
            return self.request_host_cancellation(execution, generation);
        }
        let select = cancellable.waiting_select_arms();
        let claimed = match select {
            Ok((operation, arms)) => {
                let mut claimed = 0_usize;
                for (arm, _, queue) in &arms {
                    claimed += usize::from(self.queue_mut(*queue)?.cancel(QueueWait {
                        execution,
                        operation,
                        generation,
                        arm: Some(*arm),
                    }));
                }
                if claimed == 0 { return Ok(false); }
                if claimed != arms.len() {
                    return Err("cancellation found a partially installed select wait record".into());
                }
                true
            }
            Err(_) => {
                let (operation, queue, _) = self.waiting_operation_any(execution)?;
                self.queue_mut(queue)?.cancel(QueueWait { execution, operation, generation, arm: None })
            }
        };
        if !claimed { return Ok(false); }
        self.executions.get_mut(&execution)
            .expect("execution was checked above")
            .request_cancellation()
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

    fn cancel_select_losers(&mut self, winner: QueueWait) -> Result<(), String> {
        let winning_arm = winner.arm.ok_or_else(|| "select winner is missing an arm identity".to_string())?;
        let (operation, arms) = self.executions.get(&winner.execution)
            .ok_or_else(|| "typed queue runtime has no such execution".to_string())?
            .waiting_select_arms()?;
        if operation != winner.operation { return Err("select close wake has a different operation identity".into()); }
        for (arm, _, queue) in arms {
            if arm != winning_arm {
                self.queue_mut(queue)?.cancel(QueueWait {
                    execution: winner.execution,
                    operation,
                    generation: winner.generation,
                    arm: Some(arm),
                });
            }
        }
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
