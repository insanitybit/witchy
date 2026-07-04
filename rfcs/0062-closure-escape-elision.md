---
rfc: 0062
title: Closure escape elision — closures that don't escape don't allocate
status: proposed
created: 2026-07-04
tracking:
---

# RFC-0062: Closure escape elision — closures that don't escape don't allocate

## Summary

Extend the existing escape/uniqueness analysis to closure environments, so
that closures fall into three tiers: **non-escaping** closures allocate
nothing (captures stay in locals; the body is called directly or inlined),
**escaping-but-unique** closure environments are reused in place, and only
**escaping, shared** closures pay a heap allocation. Today every closure
creation allocates its environment unconditionally; the call side is already
optimized (the `DirectCall` lever), the allocation side is not.

## Motivation

Closure creation always heap-allocates an environment record
(`crates/witchy-lower/src/codegen/mod.rs:133` — env pointer as implicit first
param; `crates/witchy-lower/src/analysis.rs:857` — captures taken at
creation), even when the closure provably never survives the expression that
uses it. The dominant closure pattern in witchy is exactly that ephemeral
shape:

- combinator arguments: `list.keep(xs, fn(x): x > 0)`, every `iter` chain
  stage, `list.update_at(xs, i, fn(v): v + 1)`
- the RFC-0050 Part 2 eta-expansions (`xs.map(list.length)` lowers to a
  synthesized lambda)
- comparator/key functions passed to sorts and folds

Each of these allocates (and, with the rc floor, later drops) an environment
whose entire life is one callee invocation. The cost is pure overhead relative
to what the operation requires — passing the captured values as extra
arguments — and it taxes precisely the combinator style the stdlib and `iter`
direction encourage.

This is the general-mechanism answer to closure cost. The async executor's
closure problem is *not* solved here and is out of scope: its closures escape
by construction into the task structure, which is why RFC-0059 changes that
representation instead of optimizing it. The two RFCs are complementary: 0059
removes manufactured escaping closures; this RFC makes the remaining,
ordinary, non-escaping ones free.

## Design

One classification, produced by the same analysis family that drives in-place
collections (the escape/uniqueness oracle), applied to each closure creation
site:

1. **Non-escaping** — the closure value is consumed within the creating
   scope: passed only to callees whose corresponding parameter does not
   escape it (per the existing per-function summaries), never stored into a
   structure, never returned, never crossing a slot boundary that outlives
   the scope. Lowering: no environment record; captures are threaded as
   additional arguments to a lambda-lifted function, and the call site (which
   `DirectCall` machinery already resolves for single-bound closure vars)
   becomes a direct call. The optimizer may then inline as usual.
2. **Escaping, unique** — the environment is heap-allocated but uniquely
   owned per the ownership conventions; re-creation in a loop may reuse the
   record in place (same mechanism as unique collection reuse; no new
   `*_cap`-style per-case helpers).
3. **Escaping, shared** — today's behavior, unchanged.

Constraints and properties:

- **General, not per-method** (the CLAUDE.md rule): the classification keys on
  the escape summaries of *callee parameters*, not on a list of blessed
  functions. `list.keep`'s predicate parameter is non-escaping because its
  summary says so, not because `keep` is special-cased.
- **Default-deny**: any construct the analysis cannot prove (captured `var`
  mutated after the call, closures crossing `chan.spawn`, storage into any
  heap value, slot-boundary crossings without a summary) classifies as
  escaping. Wrong answers must be impossible, not unlikely.
- **Lever + proof** (RFC-0030 contract): ships behind a `WITCHY_OPT` lever
  with firing-proof shape tests from day one — asserting the lowered shape
  (no env allocation; direct call with threaded captures) for a canonical
  non-escaping site, and the conservative shape for a canonical escaping one.
  BUG-008 is the cautionary tale this requirement encodes.
- **Parity**: allocation strategy is unobservable in program output; the
  interpreter needs no change. Heap-statistics tests that count allocations
  must tolerate the lever (as they do for existing levers), and the
  differential sweep runs with the lever on and off.
- **Interaction with RFC-0050 Part 2**: eta-expanded module functions are the
  best case (empty or tiny environments, never escaping) and should classify
  as tier 1 without special handling — a good acceptance test that the
  mechanism is genuinely general.

## Definition of done

1. Shape tests proving tier-1 firing (and non-firing where escape is real).
2. `benchmarks/closure_calls` plus a new combinator-chain benchmark
   (`iter` pipeline over a large list) show the win, kernel-timed; numbers
   recorded in the RFC's tracking note.
3. Full differential sweep and heap-check fuzz green with the lever in the
   sweep matrix; no new `*_cap` helpers or per-method recognizers added.

## Alternatives

- **Inlining only** (let a general inliner dissolve closures): witchy's
  optimizer does not have a general inliner today, and inlining alone doesn't
  help closures passed through non-inlined stdlib entry points; parameter
  escape summaries do.
- **Arena/stack allocation of environments** (allocate but cheaply): smaller
  win, still pays creation cost per element in hot loops, and adds a second
  allocation discipline; rejected in favor of not allocating.
- **Do nothing**: combinator style remains taxed; the stdlib's own direction
  (lazy `iter`, `keep`/`map`/`fold` everywhere) keeps paying it.

## Drawbacks

- The lambda-lifted calling convention (captures as extra args) is a second
  closure ABI inside the compiler; the classification must be stable across
  the lowering pipeline so a site cannot be lifted in one place and expected
  boxed in another. The WIR layer keeps this checkable.
- Escape summaries for higher-order stdlib functions must exist and be
  correct; a too-conservative summary silently forfeits the win (shape tests
  on representative sites are the guard), a wrong one would be a memory bug
  (the heap-check fuzz arm is the guard).

## Prior art

- The repo's own uniqueness/escape oracle (`analysis.rs`) and its convention:
  one general mechanism, no per-method cases (RFC-0016/0033/0051 lineage).
- Lambda lifting (Johnsson) and closure conversion literature; escape
  analysis as used by GC'd languages to stack-allocate non-escaping closures.
- RFC-0059 (removes the executor's manufactured escaping closures; the
  complement of this RFC), RFC-0050 Part 2 (eta-expansion, tier-1 consumer).
