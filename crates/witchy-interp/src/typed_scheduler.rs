//! Deterministic operation-level scheduler for RFC-0161 typed executions.
//!
//! The scheduler owns only execution IDs and ready-queue order. Frames,
//! waits, queues, and operation commits remain owned by `TypedExecution` and
//! `TypedQueueRuntime`; this prevents a second scheduler from growing inside
//! an adapter or benchmark fixture.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::typed_async::{KernelTurn, TypedExecution};
use crate::typed_host::{HostCompletion, HostDelivery, HostOperationId, HostSubmission};
use crate::typed_queue::{QueuePayload, QueueRuntimeResult, QueueSelectResult, TypedQueue, TypedQueueRuntime};
use witchy_types::async_program::{AsyncBackendBinding, AsyncProgram};
use witchy_types::typed_execution::OperationSiteId;

/// One operation-level scheduler observation. The sequence is assigned when
/// an execution turn is admitted, making traces independent of hash-map order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerEvent {
    pub sequence: u64,
    pub execution: u64,
    pub turn: KernelTurn,
}

/// Canonical external-event admission result. Payload values are deliberately
/// absent; only the typed operation outcome belongs in a cross-backend trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionResult {
    Blocked,
    Committed,
    Closed,
    Stale,
    Cancelled,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionEvent {
    pub sequence: u64,
    pub operation: OperationSiteId,
    pub generation: u64,
    pub result: AdmissionResult,
}

/// The single deterministic scheduler used by the typed interpreter path.
#[derive(Debug, Default)]
pub struct TypedScheduler {
    runtime: TypedQueueRuntime,
    ready: VecDeque<u64>,
    next_round: VecDeque<u64>,
    queued: BTreeSet<u64>,
    next_sequence: u64,
    admission_sequence: u64,
    admissions: Vec<AdmissionEvent>,
}

impl TypedScheduler {
    pub fn new() -> Self { Self::default() }

    pub fn runtime(&self) -> &TypedQueueRuntime { &self.runtime }
    pub fn runtime_mut(&mut self) -> &mut TypedQueueRuntime { &mut self.runtime }

    pub fn install_queue(&mut self, queue: TypedQueue) -> Result<(), String> {
        self.runtime.install_queue(queue)
    }

    /// Install one execution in the next runnable round. IDs are the only
    /// scheduler payload; the typed frame stays inside the runtime execution.
    pub fn install_execution(&mut self, id: u64, execution: TypedExecution) -> Result<(), String> {
        self.runtime.install_execution(id, execution)?;
        self.enqueue_next(id)
    }

    /// Bind one canonical program shape and install it in the same scheduler
    /// used by queue and host admissions. The backend binding is mandatory so
    /// closure-layout drift cannot silently reach the runtime.
    pub fn install_program_execution(
        &mut self,
        program: &AsyncProgram,
        shape: witchy_types::typed_execution::ExecutionShapeId,
        execution: u64,
        binding: &AsyncBackendBinding,
    ) -> Result<(), String> {
        program.validate()?;
        let canonical = program.execution_shapes.iter().find(|candidate| candidate.id == shape)
            .ok_or_else(|| format!("async program has no execution shape {shape:?}"))?;
        if binding.shape != shape {
            return Err("async backend binding shape does not match installed program shape".into());
        }
        for queue in &canonical.queues {
            if !self.runtime.has_queue(queue.id) {
                self.install_queue(TypedQueue::new(queue.clone()))?;
            }
        }
        self.install_execution(execution, TypedExecution::from_async_shape(canonical.clone(), binding)?)
    }

    pub fn ready_len(&self) -> usize { self.ready.len() + self.next_round.len() }
    pub fn admission_trace(&self) -> &[AdmissionEvent] { &self.admissions }

    /// Run the snapshot of the current ready queue. Continued and yielded
    /// executions enter the next round in admission order; blocked and
    /// terminal executions do not re-enter until an explicit wake/commit.
    pub fn run_round(&mut self) -> Result<Vec<SchedulerEvent>, String> {
        self.run_round_selected(&BTreeMap::new())
    }

    /// Run one deterministic round with explicit transition indices for
    /// branching typed states. The selector is supplied by the typed backend;
    /// omitting it for a branching state is an error, never an implicit choice
    /// of transition zero.
    pub fn run_round_selected(&mut self, selections: &BTreeMap<u64, usize>) -> Result<Vec<SchedulerEvent>, String> {
        if self.ready.is_empty() {
            std::mem::swap(&mut self.ready, &mut self.next_round);
        }
        let round_len = self.ready.len();
        let mut events = Vec::with_capacity(round_len);
        for _ in 0..round_len {
            let execution = self.ready.pop_front().expect("round length was bounded");
            self.queued.remove(&execution);
            let turn = match selections.get(&execution) {
                Some(index) => self.runtime.execution_mut_turn_selected(execution, *index)?,
                None => self.runtime.turn_execution(execution)?,
            };
            let event = SchedulerEvent { sequence: self.reserve_sequence()?, execution, turn };
            if matches!(turn, KernelTurn::Continued(_) | KernelTurn::BecameReady(_)) {
                self.enqueue_next(execution)?;
            }
            events.push(event);
        }
        Ok(events)
    }

    /// Admit turns produced by a queue/host operation commit. The queue
    /// runtime has already applied the matching operation edge; this method
    /// only transfers runnable IDs into the deterministic next-round queue.
    pub fn admit_turns(&mut self, turns: &[(u64, KernelTurn)]) -> Result<(), String> {
        for (execution, turn) in turns {
            if matches!(turn, KernelTurn::Continued(_) | KernelTurn::BecameReady(_)) {
                self.enqueue_next(*execution)?;
            }
        }
        Ok(())
    }

    pub fn admit_queue_result(&mut self, result: &QueueRuntimeResult) -> Result<(), String> {
        if let QueueRuntimeResult::Committed { turns, .. } = result {
            self.admit_turns(turns)?;
        }
        Ok(())
    }

    pub fn admit_select_result(&mut self, result: &QueueSelectResult) -> Result<(), String> {
        if let QueueSelectResult::Committed { turns, .. } = result {
            self.admit_turns(turns)?;
        }
        Ok(())
    }

    /// Submit a typed send through the scheduler-owned queue runtime and
    /// admit every execution made runnable by the commit.
    pub fn submit_send(
        &mut self,
        execution: u64,
        generation: u64,
        payload: QueuePayload,
    ) -> Result<QueueRuntimeResult, String> {
        let operation = self.operation_id(execution)?;
        let result = match self.runtime.submit_send(execution, generation, payload) {
            Ok(result) => result,
            Err(error) => {
                self.record_admission(operation, generation, AdmissionResult::Error(error.clone()))?;
                return Err(error);
            }
        };
        self.record_admission(operation, generation, queue_result(&result))?;
        self.admit_queue_result(&result)?;
        Ok(result)
    }

    pub fn submit_receive(
        &mut self,
        execution: u64,
        generation: u64,
    ) -> Result<QueueRuntimeResult, String> {
        let operation = self.operation_id(execution)?;
        let result = match self.runtime.submit_receive(execution, generation) {
            Ok(result) => result,
            Err(error) => {
                self.record_admission(operation, generation, AdmissionResult::Error(error.clone()))?;
                return Err(error);
            }
        };
        self.record_admission(operation, generation, queue_result(&result))?;
        self.admit_queue_result(&result)?;
        Ok(result)
    }

    pub fn submit_select(
        &mut self,
        execution: u64,
        generation: u64,
        send_payloads: &std::collections::BTreeMap<u32, QueuePayload>,
    ) -> Result<QueueSelectResult, String> {
        let operation = self.operation_id(execution)?;
        let result = match self.runtime.submit_select(execution, generation, send_payloads) {
            Ok(result) => result,
            Err(error) => {
                self.record_admission(operation, generation, AdmissionResult::Error(error.clone()))?;
                return Err(error);
            }
        };
        self.record_admission(operation, generation, select_result(&result))?;
        self.admit_select_result(&result)?;
        Ok(result)
    }

    pub fn close_queue(&mut self, queue: witchy_types::typed_execution::QueueShapeId) -> Result<QueueRuntimeResult, String> {
        let result = self.runtime.close_queue(queue)?;
        self.admit_queue_result(&result)?;
        Ok(result)
    }

    pub fn submit_host(
        &mut self,
        execution: u64,
        generation: u64,
        host: HostOperationId,
    ) -> Result<HostSubmission, String> {
        let operation = self.operation_id(execution)?;
        self.runtime.submit_host(execution, generation, host.0)?;
        self.record_admission(operation, generation, AdmissionResult::Blocked)?;
        Ok(HostSubmission::Blocked)
    }

    pub fn complete_host<P>(
        &mut self,
        host: HostOperationId,
        generation: u64,
        payload: P,
    ) -> Result<Option<HostCompletion<P>>, String> {
        let Some((execution, operation, turn)) = self.runtime.complete_host(host.0, generation)? else {
            return Ok(None);
        };
        self.record_admission(operation, generation, AdmissionResult::Committed)?;
        self.admit_turns(&[(execution, turn)])?;
        Ok(Some(HostCompletion {
            turn,
            delivery: HostDelivery { execution, operation, payload },
        }))
    }

    pub fn request_host_cancellation(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let operation = self.runtime.execution(execution).and_then(|execution| execution.waiting_operation().ok().map(|(operation, _)| operation));
        let cancelled = self.runtime.request_host_cancellation(execution, generation)?;
        if cancelled {
            if let Some(operation) = operation { self.record_admission(operation, generation, AdmissionResult::Cancelled)?; }
            self.enqueue_next(execution)?;
        }
        Ok(cancelled)
    }

    /// Remove the exact generation's queue registrations before entering the
    /// execution's declared cleanup state. A successful request is admitted
    /// in the next deterministic round.
    pub fn request_cancellation(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let operation = self.runtime.execution(execution).and_then(|execution| execution.waiting_operation().ok().map(|(operation, _)| operation));
        let requested = self.runtime.request_cancellation(execution, generation)?;
        if requested {
            if let Some(operation) = operation { self.record_admission(operation, generation, AdmissionResult::Cancelled)?; }
            self.enqueue_next(execution)?;
        }
        Ok(requested)
    }

    /// Wake a matching typed wait and enqueue the execution for the next
    /// round. The operation identity is checked by the kernel before enqueue.
    pub fn wake(&mut self, execution: u64, operation: OperationSiteId) -> Result<(), String> {
        self.runtime.wake_execution(execution, operation)?;
        self.enqueue_next(execution)
    }

    fn enqueue_next(&mut self, execution: u64) -> Result<(), String> {
        if !self.queued.insert(execution) {
            return Ok(());
        }
        self.next_round.push_back(execution);
        Ok(())
    }

    fn reserve_sequence(&mut self) -> Result<u64, String> {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.checked_add(1)
            .ok_or_else(|| "typed scheduler transition sequence overflow".to_string())?;
        Ok(sequence)
    }

    fn operation_id(&self, execution: u64) -> Result<OperationSiteId, String> {
        self.runtime.execution(execution)
            .ok_or_else(|| "typed scheduler has no such execution".to_string())?
            .waiting_operation().map(|(operation, _)| operation)
    }

    fn record_admission(&mut self, operation: OperationSiteId, generation: u64, result: AdmissionResult) -> Result<(), String> {
        let sequence = self.admission_sequence;
        self.admission_sequence = self.admission_sequence.checked_add(1)
            .ok_or_else(|| "typed scheduler admission sequence overflow".to_string())?;
        self.admissions.push(AdmissionEvent { sequence, operation, generation, result });
        Ok(())
    }
}

fn queue_result(result: &QueueRuntimeResult) -> AdmissionResult {
    match result {
        QueueRuntimeResult::Blocked => AdmissionResult::Blocked,
        QueueRuntimeResult::Committed { deliveries, .. } if deliveries.iter().any(|delivery| matches!(delivery.completion, crate::typed_queue::QueueCompletion::SendClosed { .. } | crate::typed_queue::QueueCompletion::ReceiveClosed)) => AdmissionResult::Closed,
        QueueRuntimeResult::Committed { .. } => AdmissionResult::Committed,
    }
}

fn select_result(result: &QueueSelectResult) -> AdmissionResult {
    match result {
        QueueSelectResult::Blocked => AdmissionResult::Blocked,
        QueueSelectResult::Committed { deliveries, .. } if deliveries.iter().any(|delivery| matches!(delivery.completion, crate::typed_queue::QueueCompletion::SendClosed { .. } | crate::typed_queue::QueueCompletion::ReceiveClosed)) => AdmissionResult::Closed,
        QueueSelectResult::Committed { .. } => AdmissionResult::Committed,
    }
}
