# BUG-311: Quiescence release for blocked senders and joiners is now documented

Severity: MED
Status: FIXED
Verified: 2026-07-09 fixed on master b3b39dc0
Component: std/chan.witchy, spec/stdlib.md, book/src/tour-async.md, async/channel executor, rfcs/concurrency-design.md

## Problem

Historical problem: the channel docs promised unconditional blocking, but the
executor deliberately releases blocked senders and joiners during quiescence.
Older docs said a bounded sender blocks when the channel is full and `join`
waits until the task finishes, without naming the quiescence close pass.

The implementation is the RFC-documented design: if every live task is parked
with no progress, the close pass releases parked sends and parked joins. A
bounded send may therefore complete even when the logical capacity is full, and a
join may resume even if the joined task has a continuation that will run later.

## Current status

The shipped docs now describe the actual mechanism:

- `std/chan.witchy` says bounded sends block while the executor can make
  progress, but a quiescence close pass releases a parked send and may
  temporarily exceed logical capacity.
- `std/chan.witchy` says `join`/`join_all` wait while progress is possible, but
  a parked join resumes during quiescence even if the task has a continuation
  that will run afterward.
- `spec/stdlib.md` is regenerated from those comments and publishes the same
  contract.
- `book/src/tour-async.md` explains that parked sends and parked joins resume
  during quiescence.
- `src/example_tests.rs` has `channel_quiescence_close_contract_backends_agree`,
  which verifies bounded send release and join release on both backends.

Regression:

```sh
CARGO_TARGET_DIR=target-codex-chan cargo test channel_quiescence_close_contract_backends_agree -- --nocapture
```

## Evidence

- `std/task.witchy` implements the close pass for `WaitSend` and `WaitJoin`.
- `std/chan.witchy` documents the public channel surface in terms of that close
  pass.
- `spec/stdlib.md` and `book/src/tour-async.md` no longer promise sender
  refcounting or unconditional join blocking.
- The executor behavior agrees on interpreter and compiled wasm in the focused
  parity test.

## Resolution

Fixed via docs-first reconciliation, matching BUG-310. The chosen model is
quiescence-based close/release, not sender refcounting or unconditional
join-blocking. Changing that would be an RFC-level executor redesign.
