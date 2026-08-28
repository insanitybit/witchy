//! Deterministic operation-level scheduler for RFC-0161 typed executions.
//!
//! The scheduler owns only execution IDs and ready-queue order. Frames,
//! waits, queues, and operation commits remain owned by `TypedExecution` and
//! `TypedQueueRuntime`; this prevents a second scheduler from growing inside
//! an adapter or benchmark fixture.

use std::collections::{BTreeSet, VecDeque};

use crate::typed_async::{KernelTurn, TypedExecution};
use crate::typed_queue::{QueueRuntimeResult, QueueSelectResult, TypedQueue, TypedQueueRuntime};
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
    use witchy_syntax::ast::Type;
    use witchy_types::typed_execution::{ExecutionEdgeCondition, ExecutionOutcome, ExecutionShapeId,
        ExecutionState, ExecutionStateId, ExecutionTransition, FrameLane, FrameLaneId, LaneOwnership,
        TypedExecutionShape};

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
}
