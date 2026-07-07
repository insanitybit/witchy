# BUG-495: `future.lazy` promises exact-once execution but is replayable

Severity: MED
Status: FIXED
Verified: 2026-07-07 CODE on branch worktree-wt-bug-495-lazy-contract
Component: `std/future`, `std/task`, generated stdlib docs, lazy computation semantics

## Problem

`std/future.lazy` documented a one-shot contract: the thunk runs on first poll and
"then exactly once." The implementation cannot provide that contract with the
current representation.

`Future(a)` is an ordinary value wrapping `fn() -> Poll(a)`, and `poll` just calls
that thunk. `lazy(thunk)` builds a new thunk whose body is `poll(thunk())`. If the
same `Future` value is polled twice, the outer thunk runs twice and calls
`thunk()` twice. The normal `join_all` / `select` driver replaces a `More` result
with its continuation, so the common executor path advances past the `lazy`
wrapper. But the public `poll` API and value semantics still make the original
future replayable.

That is a semantic contract mismatch, not only stale async wording. It is
impossible for the current copyable thunk representation to guarantee
single-entry lazy execution without either mutable memo state, linear/consumed
polling, or a weaker documented contract.

## Fix

The docs now describe the actual model: `Future` and `Task` values are
replayable execution recipes, not memo cells. Work is delayed until polling or
driving reaches the lazy/deferred value, and standard drivers advance through
continuations, but polling/driving the same value again can rerun its thunk.

This deliberately keeps the runtime representation unchanged. Exact-once
semantics would require a different API shape, such as consumed polling or
mutable memo state, and that is larger than a 0.1 stdlib contract cleanup.

## Acceptance

- `std/future.lazy` and `std/future.defer` no longer promise exact-once or
  memoized execution for a replayable `Future` value.
- `std/task.lazy` and the compatibility `std/chan.lazy` facade use the same
  replayable execution-recipe terminology.
- Generated stdlib docs carry the same contract.

## Verification

- `target-codex/debug/witchy fmt --check std/future.witchy`
- `target-codex/debug/witchy fmt --check std/task.witchy`
- `target-codex/debug/witchy fmt --check std/chan.witchy`
- `target-codex/debug/witchy check std/future.witchy`
- `target-codex/debug/witchy check std/task.witchy`
- `target-codex/debug/witchy check std/chan.witchy`
- `CARGO_TARGET_DIR=target-codex CARGO_BUILD_JOBS=1 cargo nextest run -E 'test(all_std_modules_type_check) | test(stdlib_docs_are_current)'`
