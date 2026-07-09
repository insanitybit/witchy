# BUG-310: `chan.recv` quiescence close contract is now documented

Severity: MED
Status: FIXED
Verified: 2026-07-09 fixed on master b3b39dc0
Component: std/chan.witchy, spec/stdlib.md, book/src/tour-async.md, async/channel executor, rfcs/concurrency-design.md

## Problem

Historical problem: the channel docs promised `recv` returned `None` only once
no task could send anymore. The executor's actual model is quiescence-based:
when every live task is parked with no progress, parked receives resume with
`None`. Witchy does not refcount sender values, so a retained `Sender` may still
send after a quiescent close resumes a receiver.

This is deliberate, RFC-documented executor behavior rather than a backend
parity bug. The bug was that release-facing docs described a stronger
sender-liveness contract than Witchy implements.

## Current status

The shipped docs now describe the actual quiescence contract:

- `std/chan.witchy` says parked receives resume with `None` when the executor
  reaches quiescence, and explicitly says "closed" does not mean no `Sender`
  value can ever be used again.
- `spec/stdlib.md` is regenerated from those comments and publishes the same
  contract.
- `book/src/tour-async.md` explains quiescence close, including the fact that
  Witchy does not refcount sender values.
- `src/example_tests.rs` has `channel_quiescence_close_contract_backends_agree`,
  which verifies the recv-with-live-sender behavior on both backends.

Regression:

```sh
CARGO_TARGET_DIR=target-codex-chan cargo test channel_quiescence_close_contract_backends_agree -- --nocapture
```

## Evidence

- `std/task.witchy` implements the quiescent close pass for parked receives.
- `std/chan.witchy` documents `recv` in terms of that close pass.
- `spec/stdlib.md` and `book/src/tour-async.md` no longer promise sender
  refcounting or "no task can send" semantics.
- The focused parity test shows interpreter and compiled wasm agree.

## Resolution

Fixed via docs-first reconciliation. The chosen model is quiescence-based close,
not sender refcounting. Changing that would be an RFC-level executor redesign.
