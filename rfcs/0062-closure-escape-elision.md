---
rfc: 0062
title: Closure escape elision — closures that don't escape don't allocate
status: implemented
created: 2026-07-04
tracking: tier-1 implemented and promoted into the default release set; conservative escaping closures remain boxed
---

# RFC-0062: Closure escape elision — closures that don't escape don't allocate

The escape analysis and candidate facts are implemented in
[`crates/witchy-lower/src/escape.rs`](../crates/witchy-lower/src/escape.rs) and
[`crates/witchy-syntax/src/opt.rs`](../crates/witchy-syntax/src/opt.rs).

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
([`crates/witchy-lower/src/codegen/mod.rs:133`](../crates/witchy-lower/src/codegen/mod.rs) — env pointer as implicit first
param; [`crates/witchy-lower/src/analysis.rs:857`](../crates/witchy-lower/src/analysis.rs) — captures taken at
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

- The repo's own uniqueness/escape oracle ([`analysis.rs`](../crates/witchy-lower/src/analysis.rs)) and its convention:
  one general mechanism, no per-method cases (RFC-0016/0033/0051 lineage).
- Lambda lifting (Johnsson) and closure conversion literature; escape
  analysis as used by GC'd languages to stack-allocate non-escaping closures.
- RFC-0059 (removes the executor's manufactured escaping closures; the
  complement of this RFC), RFC-0050 Part 2 (eta-expansion, tier-1 consumer).

## Implementation status (tier 1 shipped and promoted)

Tier 1 is implemented behind the addressable `WITCHY_OPT=closure-elide` pass
and is included in the default production set (`release == all`). It was
promoted after the firing/non-firing shape proofs, differential configuration,
heap-check fuzz arm, and repeated workspace gates had exercised the pass both
alone and in the optimization union. `WITCHY_OPT=-closure-elide` retains the
boxed reference path for differential testing and bisection.

The promotion audit also corrected the high-risk differential configurations
to start from `none` (`none,closure-elide`, and likewise for `rc-floor` and
`unbox`). A bare pass token is additive to `release`; once every pass is
promoted, the former bare-token rows would only duplicate the default instead
of proving each risky pass in isolation.

The 0.1 scope is tier 1: proven non-escaping closures avoid the environment
allocation. Escaping-but-unique closures use the existing RC-floor behavior,
and escaping/shared closures remain boxed. More aggressive closure
specialization or in-place environment reuse is future optimization work, not
an incomplete semantic requirement of this RFC.

**Classification (general, default-deny).** Two facts, both keyed on the
escape/uniqueness oracle — no blessed-function list, no per-method code:

- `escape::only_directly_called(body)` — the non-escape fact: names used ONLY
  as a direct-call callee (`Call` head / `Apply` `Var` func), never as a whole
  value (arg, store, return), never referenced inside a nested closure body (a
  capture). Any other occurrence disqualifies the name.
- `escape::reassigned_names(body)` — the capture-stability guard: eliding the
  env threads captures at the CALL site, not the creation site, so a capture
  reassigned in between (the interpreter snapshots at creation) forces the
  boxed closure.

A `let f = <lambda>` is tier-1 iff it is single-bound (`devirt_ok`, the
existing DirectCall census), in `only_directly_called`, and no capture is
reassigned. Anything unprovable → boxed (tiers 2/3).

**Lowering (three tiers).**

1. *Non-escaping* (tier 1): `lower_lambda_threaded` registers a THREADED lifted
   body `$__lamt{i}` (`CapMode::Threaded`: captures are leading value params, no
   env pointer) and records the capture list in `thread_index`. The `let`
   emits NOTHING — no `mk{n}` env allocation. Each call site (both the
   closure-local `Call` arm and the `Apply` arm) threads the captures from
   their locals into a direct `call $__lamt{i}(cap0.., args)`.
2. *Escaping, unique*: unchanged (RC-floor already reclaims a confined env at
   last use); no new per-case helper added.
3. *Escaping, shared*: unchanged — `mk{n}` env + `call_indirect`/`$__lamw`.

**Firing proof (shape tests, `codegen_tests`).** `elides_nonescaping_closure_env`
asserts the canonical tier-1 site emits NO `mk{n}` and a `$__lamt` direct call
(and, with the lever off, reverts to `mk1` + `$__lamw`); `keeps_env_for_escaping_closure`
asserts a closure passed as an argument KEEPS its `mk1` env under the lever
(default-deny); `elided_closure_matches_boxed_output` asserts identical output
elided vs boxed.

**Benchmarks (kernel-timed, best of 7, release binary).** The win scales with
allocation frequency:

| benchmark | shape | shipping (wasm-opt on) OFF→ON | raw codegen (wasm-opt off) OFF→ON |
|---|---|---|---|
| `closure_capture` | capturing closure created PER ITERATION (5M) | 11.45ms → 3.77ms (**3.0×**) | 23.65ms → 5.10ms (**4.6×**) |
| `closure_calls` | non-capturing, created once | 3.50ms → 3.54ms (~noise) | 4.45ms → 4.31ms (~3%) |
| `closure_pipeline` | map/filter/reduce over 200k, closures created once | 0.19ms → 0.18ms (~noise) | — |

Heap proof (`witchy stats`, region off so allocations accumulate): a 100k-iter
per-iteration-closure loop drops `heap_bytes` from **2,000,069 → 69** with the
lever on — the environment allocation is gone entirely.

The loop-invariant cases (`closure_calls`, `closure_pipeline`) show only a
marginal win because their single env allocation is hoisted out of the loop and
Binaryen's inliner already dissolves the per-call env load in the shipping
pipeline. The dramatic win is the *per-iteration* closure, exactly the
ephemeral shape the RFC targets ("an environment whose entire life is one
callee invocation").

**Parity.** The interpreter is unchanged; allocation strategy is unobservable.
The differential sweep runs `closure-elide` both alone and in the union
(`CONFIGS` in `differential_fuzz.rs`); the heap-checked fuzz (60 programs × 6
configs) is green, exercising escaping closures under the lever (they stay
boxed, matching the interpreter).

**Punted.** Tier-1 does not reach closures that cross a function boundary into
a non-inlined stdlib combinator (`list.map`, `iter.map`/`filter`/`fold`): the
call happens inside the callee via `call_indirect`, and `iter`'s stage closures
are stored into `Iter`/`Step` records (escaping by construction). Eliding those
needs closure specialization / a general inliner, out of scope here (see
*Alternatives*). Tier-2 in-place env reuse is left to the existing RC-floor
reclamation rather than a new closure-specific path.
