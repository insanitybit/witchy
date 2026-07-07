# BUG-254: `std/chan` duplicates `std/task` combinators and executor bodies

Severity: MED
Status: FIXED
Verified: 2026-07-07 CODE on branch worktree-wt-bug-254-chan-task-dedup
Component: `std/task`, `std/chan`, async/channel stdlib API, RFC-0059 migration

## Problem

`std/chan` now imports the executor-owned types from `std/task`, which is the
right post-RFC-0042 shape:

```witchy
from task import Task, Step, Slot, Handle
```

But it still copies the task combinator surface and a second executor body
instead of delegating to `std/task`. The result is a half-deduplicated
concurrency stack:

- `task.Task`, `task.Step`, `task.Slot`, and `task.Handle` are canonical;
- `chan.done`, `chan.ready_unit`, `chan.yield_now`, `chan.and_then`,
  `chan.map`, `chan.lazy`, `chan.for_each`, `chan.spawn`, `chan.join`,
  `chan.cancel`, and `chan.run` are copy-maintained siblings of the same
  functions in `task`;
- the two modules can drift in behavior, docs, diagnostics, or performance work
  even though they operate on the same imported `Task`/`Step`/`Slot` types.

For a public release, this makes the concurrency story feel unfinished: there is
one executor type, but two apparent homes for the executor API.

## Evidence

- `std/task.witchy` defines `Step(a)`, `Task(a)`, `Handle`, and the executor
  `Slot`.
- `std/chan.witchy` imports the canonical task types from `task`, and explicitly
  says the old duplicate type declarations were replaced.
- Before this fix, `std/task.witchy` and `std/chan.witchy` both defined
  equivalent public task combinators:
  `done`, `ready_unit`, `yield_now`, `and_then`, `map`, `lazy`, `for_each`,
  `spawn`, `join`, and `cancel`.
- Before this fix, both modules carried a round-robin executor loop over `slots`
  and `channels`.
- `rfcs/0059-state-machine-async.md` describes this executor duplication as part
  of the async performance diagnosis.

## Why this matters

Concurrency is a headline feature. A user should not need to learn whether
`task.spawn` and `chan.spawn` are independent APIs, aliases, or historical
duplicates. Maintainers also should not need to patch two `and_then_step` or
executor loops when changing async lowering, cancellation, recursive drop,
diagnostics, or channel buffering.

This is distinct from BUG-064, which tracks forged typed channel endpoints. This
finding is about API and implementation duplication after the type-level
consolidation already happened.

## Fix

`std/task` is now the single owner of the core task combinators, spawn/join/
cancel, and the executor loop. `std/chan` keeps compatibility facades for
channel-centric examples and programs, but those facades delegate to `task.*`.

Channel-specific APIs remain in `std/chan`: `Sender`, `Receiver`, `channel`,
`send`, `recv`, `select`, and the higher-level channel helpers. The copied
executor loop, copied scheduler helpers, and copied `and_then_step` body were
removed from `std/chan`.

## Acceptance

- `std/chan.witchy` has no local implementation of `and_then_step`, scheduler
  slot predicates, ring helpers, readiness search, or the round-robin executor
  loop.
- `chan.done`, `chan.ready_unit`, `chan.yield_now`, `chan.and_then`, `chan.map`,
  `chan.lazy`, `chan.for_each`, `chan.spawn`, `chan.join`, `chan.cancel`, and
  `chan.run` delegate to `task.*`.
- Public docs still expose the compatibility facades, but scheduling behavior
  has one canonical stdlib implementation site: `std/task`.

## Verification

- `CARGO_TARGET_DIR=target-codex CARGO_BUILD_JOBS=1 cargo check --bin witchy`
- `CARGO_TARGET_DIR=target-codex CARGO_BUILD_JOBS=1 cargo build --bin witchy`
- `target-codex/debug/witchy check std/chan.witchy`
- `target-codex/debug/witchy check std/task.witchy`
- `CARGO_TARGET_DIR=target-codex CARGO_BUILD_JOBS=1 cargo nextest run -E 'test(all_std_modules_type_check) | test(stdlib_docs_are_current) | test(async_await_lowers_and_runs_backends_agree) | test(async_with_channels_backends_agree) | test(chan_select_backends_agree) | test(for_await_loop_backends_agree) | test(for_await_over_receiver_backends_agree) | test(future_select_first_wins_backends_agree) | test(rc_corpus_channel_executor_is_stable) | test(rfc0055_one_task_two_message_types_job_answer) | test(rfc0055_two_modules_private_channels_of_different_types) | test(task_and_future_coexist_backends_agree)'`
- `CARGO_TARGET_DIR=target-codex CARGO_BUILD_JOBS=1 cargo clippy --bin witchy -- -D warnings`
