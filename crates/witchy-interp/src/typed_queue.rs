//! Deterministic typed queue and wait protocol for RFC-0161 executions.
//!
//! This module deliberately stores opaque payload identities only. Value layout
//! and movement remain the execution frame's responsibility; the queue owns
//! ordering, capacity, registration generation, matching, cancellation, and
//! close/drain rules uniformly for every payload type.

use std::collections::VecDeque;

use witchy_types::typed_execution::{OperationSiteId, QueueCapacity, QueueShape};

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

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_types::typed_execution::{PayloadOwnership, QueueCapacity, QueueShapeId};

    fn queue(capacity: QueueCapacity) -> TypedQueue {
        TypedQueue::new(QueueShape {
            id: QueueShapeId(0),
            capacity,
            payload_layout_fingerprint: [3; 32],
            ownership: PayloadOwnership::Inline,
        })
    }

    fn wait(execution: u64, generation: u64) -> QueueWait {
        QueueWait { execution, operation: OperationSiteId(0), generation }
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
}
