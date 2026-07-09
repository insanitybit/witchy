# BUG-396: `std/chan` structured join/cancel sequencing still recurses over large fan-outs

Severity: MED
Status: FIXED
Verified: 2026-07-09 fixed on branch fix/chan-join-cancel-indexed
Component: `std/chan`, `std/task`, structured concurrency helpers, RFC-0032 channel ladder

## Problem

Historical problem: `std/chan` presents `scope`, `gather`, `par_map`, and `par_reduce` as the
ergonomic structured-concurrency layer: users hand over a list of jobs and the
library handles spawning, joining, and result collection. The implementation has
been partially repaired since this bug was filed: the spawn/build/receive paths
now use accumulator-style helpers and no longer rebuild result lists with
post-recursive `list.concat`.

The remaining gap was the public structured layer's sequencing path. `join_all`
and `cancel_all` depended on the recursive `for_each` helper, so large fan-outs
still carried call-stack sensitivity in the join/cancel phase even after the
collection helpers were cleaned up.

That made a release-facing headline API feel fragile. A user who maps over a
large list should hit a deliberate resource limit or run iteratively; they should
not depend on interpreter/wasm call-stack depth in the middle of a structured
helper whose docs describe it as the safe default.

## Evidence

- `std/task.witchy` still exposes the generic `task.for_each` helper, but
  `std/chan` structured join/cancel no longer routes through it.
- `std/chan.witchy` implements `spawn_all`, `recv_n`, `recv_each`, and
  `par_build` through accumulator/index helpers.
- `std/chan.witchy` now implements `join_all` through `join_all_from(hs, i)`.
- `std/chan.witchy` now implements `cancel_all` through `cancel_all_from(hs, i)`.
- `std/chan.witchy` builds `scope`, `gather`, `par_map`, `par_reduce`, and
  `race_n` on those indexed sequencing paths.
- `spec/stdlib.md:155-169` describes these APIs as structured concurrency and
  the ergonomic default for data parallelism, without warning about stack-depth
  limits.

Regression:

```sh
CARGO_TARGET_DIR=target-codex-chan cargo test channel_structured_join_cancel_indexed_fanouts_backends_agree -- --nocapture
```

This is distinct from BUG-254, which tracks duplicated task/chan executor and
combinator bodies. Deduplicating the executor surface would still leave these
public helpers recursive unless their implementation strategy changes. It is
also distinct from BUG-366, which tracks recursion in `std/iter`; this bug is
about the channel structured-concurrency API.

## Fix direction

Fixed by rewriting the channel helper layer around indexed sequencing helpers,
matching the style already used by the accumulator paths:

- `join_all` and `cancel_all` no longer call the generic recursive `for_each`.
- Keep the documented ordering contracts: `gather` returns completion order,
  while `par_map` and `par_reduce` preserve input order.
- Focused tests cover structured join and cancel fan-outs on both backends
  without turning the normal suite into a heap-capacity stress test.
