//! Deterministic operation-level scheduler for RFC-0161 typed executions.
//!
//! The scheduler owns only execution IDs and ready-queue order. Frames,
//! waits, queues, and operation commits remain owned by `TypedExecution` and
//! `TypedQueueRuntime`; this prevents a second scheduler from growing inside
//! an adapter or benchmark fixture.

use std::collections::{BTreeSet, VecDeque};

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

/// The single deterministic scheduler used by the typed interpreter path.
#[derive(Debug, Default)]
pub struct TypedScheduler {
    runtime: TypedQueueRuntime,
    ready: VecDeque<u64>,
    next_round: VecDeque<u64>,
    queued: BTreeSet<u64>,
    next_sequence: u64,
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

    /// Run the snapshot of the current ready queue. Continued and yielded
    /// executions enter the next round in admission order; blocked and
    /// terminal executions do not re-enter until an explicit wake/commit.
    pub fn run_round(&mut self) -> Result<Vec<SchedulerEvent>, String> {
        if self.ready.is_empty() {
            std::mem::swap(&mut self.ready, &mut self.next_round);
        }
        let round_len = self.ready.len();
        let mut events = Vec::with_capacity(round_len);
        for _ in 0..round_len {
            let execution = self.ready.pop_front().expect("round length was bounded");
            self.queued.remove(&execution);
            let turn = self.runtime.turn_execution(execution)?;
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
        let result = self.runtime.submit_send(execution, generation, payload)?;
        self.admit_queue_result(&result)?;
        Ok(result)
    }

    pub fn submit_receive(
        &mut self,
        execution: u64,
        generation: u64,
    ) -> Result<QueueRuntimeResult, String> {
        let result = self.runtime.submit_receive(execution, generation)?;
        self.admit_queue_result(&result)?;
        Ok(result)
    }

    pub fn submit_select(
        &mut self,
        execution: u64,
        generation: u64,
        send_payloads: &std::collections::BTreeMap<u32, QueuePayload>,
    ) -> Result<QueueSelectResult, String> {
        let result = self.runtime.submit_select(execution, generation, send_payloads)?;
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
        self.runtime.submit_host(execution, generation, host.0)?;
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
        self.admit_turns(&[(execution, turn)])?;
        Ok(Some(HostCompletion {
            turn,
            delivery: HostDelivery { execution, operation, payload },
        }))
    }

    pub fn request_host_cancellation(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let cancelled = self.runtime.request_host_cancellation(execution, generation)?;
        if cancelled { self.enqueue_next(execution)?; }
        Ok(cancelled)
    }

    /// Remove the exact generation's queue registrations before entering the
    /// execution's declared cleanup state. A successful request is admitted
    /// in the next deterministic round.
    pub fn request_cancellation(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let requested = self.runtime.request_cancellation(execution, generation)?;
        if requested { self.enqueue_next(execution)?; }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typed_async::KernelStatus;
    use crate::typed_host::HostOperationId;
    use witchy_syntax::ast::Type;
    use witchy_types::async_program::{AsyncBackendBinding, AsyncBackendStateBinding, AsyncExecutionShape, AsyncProgram, AsyncProgramStateId, AsyncStateEdge, FinalStateBinding};
    use witchy_types::typed_async_lowering::FinalCallableIndex;
    use witchy_types::typed_execution::{ExecutionEdgeCondition, ExecutionOutcome, ExecutionShapeId,
        ExecutionState, ExecutionStateId, ExecutionTransition, FrameLane, FrameLaneId, LaneOwnership,
        OperationSite, PayloadOwnership, QueueCapacity, QueueShape, QueueShapeId, TypedExecutionShape};

    #[test]
    fn canonical_program_binding_enters_the_same_execution_kernel() {
        let program = AsyncProgram::new(vec![AsyncExecutionShape {
            id: ExecutionShapeId(0), closure_layout_fingerprint: [3; 32], frame: vec![],
            entry: AsyncProgramStateId(0), result_lanes: vec![], operations: vec![], queues: vec![], cancellation: None,
            states: vec![FinalStateBinding {
                state: AsyncProgramStateId(0), callable: FinalCallableIndex(0), capture_to_lane: vec![],
                edges: vec![AsyncStateEdge { condition: ExecutionEdgeCondition::Always, actions: vec![], outcome: ExecutionOutcome::Complete }],
            }], dynamic_boundaries: vec![],
        }]).expect("program");
        let binding = AsyncBackendBinding {
            shape: ExecutionShapeId(0), closure_layout_fingerprint: [3; 32],
            states: vec![AsyncBackendStateBinding { state: AsyncProgramStateId(0), callable: FinalCallableIndex(0), capture_to_lane: vec![] }],
        };
        let mut scheduler = TypedScheduler::new();
        scheduler.install_program_execution(&program, ExecutionShapeId(0), 44, &binding).expect("bind program");
        assert_eq!(scheduler.run_round().expect("run")[0].turn, KernelTurn::Complete);
    }

    fn linear() -> TypedExecution {
        TypedExecution::new(TypedExecutionShape {
            id: ExecutionShapeId(0),
            entry: ExecutionStateId(0),
            frame: vec![FrameLane { id: FrameLaneId(0), ty: Type::Named("Int".into(), Vec::new()), ownership: LaneOwnership::Copy, initially_initialized: true }],
            result_lanes: vec![FrameLaneId(0)],
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![], outcome: ExecutionOutcome::Continue(ExecutionStateId(1)) }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![], outcome: ExecutionOutcome::Complete }] },
            ],
            operations: vec![],
            queues: vec![],
            cancellation: None,
        }).expect("linear execution")
    }

    fn host_execution() -> TypedExecution {
        TypedExecution::new(TypedExecutionShape {
            id: ExecutionShapeId(2), entry: ExecutionStateId(0), frame: vec![], result_lanes: vec![],
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![], outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(1) } }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![], outcome: ExecutionOutcome::Complete }] },
            ],
            operations: vec![
                witchy_types::typed_execution::OperationSite { id: OperationSiteId(0), kind: witchy_types::typed_execution::ExecutionOperation::Host, input_lanes: vec![], result_lanes: vec![], pending_move_lanes: vec![], fallback_state: Some(ExecutionStateId(0)), queue: None, arms: vec![] },
                witchy_types::typed_execution::OperationSite { id: OperationSiteId(1), kind: witchy_types::typed_execution::ExecutionOperation::Cancellation, input_lanes: vec![], result_lanes: vec![], pending_move_lanes: vec![], fallback_state: None, queue: None, arms: vec![] },
            ], queues: vec![], cancellation: Some(witchy_types::typed_execution::CancellationTransition { operation: OperationSiteId(1), state: ExecutionStateId(1) }),
        }).expect("host execution")
    }

    #[test]
    fn rounds_preserve_fifo_and_defer_continuations() {
        let mut scheduler = TypedScheduler::new();
        scheduler.install_execution(7, linear()).expect("first execution");
        scheduler.install_execution(3, linear()).expect("second execution");
        let first = scheduler.run_round().expect("first round");
        assert_eq!(first.iter().map(|event| event.execution).collect::<Vec<_>>(), vec![7, 3]);
        assert_eq!(first[0].sequence, 0);
        assert_eq!(first[1].sequence, 1);
        assert_eq!(scheduler.ready_len(), 2);
        let second = scheduler.run_round().expect("second round");
        assert_eq!(second.iter().map(|event| event.turn).collect::<Vec<_>>(), vec![KernelTurn::Complete, KernelTurn::Complete]);
        assert_eq!(scheduler.ready_len(), 0);
    }

    #[test]
    fn duplicate_wakes_do_not_duplicate_ready_ids() {
        let mut scheduler = TypedScheduler::new();
        scheduler.install_execution(1, linear()).expect("execution");
        assert_eq!(scheduler.ready_len(), 1);
        // The execution is ready, so a wake is rejected by the kernel rather
        // than creating a duplicate scheduler entry.
        assert!(scheduler.wake(1, OperationSiteId(0)).is_err());
        assert_eq!(scheduler.ready_len(), 1);
        assert_eq!(scheduler.runtime().execution(1).expect("execution").status(), KernelStatus::Ready);
    }

    fn cancellable() -> TypedExecution {
        TypedExecution::new(TypedExecutionShape {
            id: ExecutionShapeId(1),
            entry: ExecutionStateId(0),
            frame: vec![],
            result_lanes: vec![],
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![], outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(0) } }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![], outcome: ExecutionOutcome::Complete }] },
            ],
            operations: vec![
                OperationSite { id: OperationSiteId(0), kind: witchy_types::typed_execution::ExecutionOperation::ChannelReceive, input_lanes: vec![], result_lanes: vec![], pending_move_lanes: vec![], fallback_state: Some(ExecutionStateId(0)), queue: Some(QueueShapeId(0)), arms: vec![] },
                OperationSite { id: OperationSiteId(1), kind: witchy_types::typed_execution::ExecutionOperation::Cancellation, input_lanes: vec![], result_lanes: vec![], pending_move_lanes: vec![], fallback_state: None, queue: None, arms: vec![] },
            ],
            queues: vec![QueueShape { id: QueueShapeId(0), capacity: QueueCapacity::Rendezvous, payload_layout_fingerprint: [0; 32], ownership: PayloadOwnership::Inline }],
            cancellation: Some(witchy_types::typed_execution::CancellationTransition { operation: OperationSiteId(1), state: ExecutionStateId(1) }),
        }).expect("cancellable execution")
    }

    #[test]
    fn cancellation_unlinks_wait_before_cleanup_round() {
        let mut scheduler = TypedScheduler::new();
        scheduler.install_queue(TypedQueue::new(QueueShape { id: QueueShapeId(0), capacity: QueueCapacity::Rendezvous, payload_layout_fingerprint: [0; 32], ownership: PayloadOwnership::Inline })).expect("queue");
        scheduler.install_execution(9, cancellable()).expect("execution");
        assert_eq!(scheduler.run_round().expect("block round")[0].turn, KernelTurn::Blocked(OperationSiteId(0)));
        assert!(matches!(scheduler.submit_receive(9, 1).expect("register receive"), QueueRuntimeResult::Blocked));
        assert!(scheduler.request_cancellation(9, 1).expect("cancel"));
        assert_eq!(scheduler.run_round().expect("cleanup round")[0].turn, KernelTurn::Complete);
        assert_eq!(scheduler.runtime().execution(9).expect("execution").status(), KernelStatus::Complete);
    }

    #[test]
    fn host_completion_is_admitted_by_the_same_scheduler() {
        let mut scheduler = TypedScheduler::new();
        scheduler.install_execution(11, host_execution()).expect("execution");
        assert_eq!(scheduler.run_round().expect("block")[0].turn, KernelTurn::Blocked(OperationSiteId(0)));
        assert_eq!(scheduler.submit_host(11, 5, HostOperationId(7)).expect("submit"), HostSubmission::Blocked);
        let completion = scheduler.complete_host(HostOperationId(7), 5, 42_u32).expect("complete").expect("matching completion");
        assert_eq!(completion.delivery.payload, 42);
        assert_eq!(completion.turn, KernelTurn::Complete);
        assert_eq!(scheduler.ready_len(), 0);
    }
}
