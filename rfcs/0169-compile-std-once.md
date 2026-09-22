---
rfc: 0169
title: Compile the standard library once, not once per program
status: proposed
created: 2026-09-22
related: 0168
tracking:
---

# RFC-0169: Compile the standard library once

## Summary

The compiler is whole-program: every compile re-links, re-type-checks,
re-lowers, re-checks and re-encodes the parts of `std` the program uses. For
async programs that is the std executor, and it dominates. This RFC makes the
standard library a unit that is compiled once per compiler build and reused,
the way `rustc` compiles `std` into a library artifact once rather than into
every crate. It stages the work so each step is measurable and each step keeps
admission sound.

## Motivation

Measured on `master` at `a7ce84829`, `witchy check examples/channels/src/channels.witchy`
(23 lines of user code), SafeWIR verdict store warm:

| stage | time |
|---|---|
| frontend: link, type check, monomorphize | 1.5-1.6s |
| backend: layouts 0.25s, lowering 0.41s, initial check 0.37s, final validation + admission + encoding ~1.5s | 2.6s |

A two-line program without async takes a few milliseconds.

The lowered program has 233 functions, 3,071 blocks and 34,888 operations. One
function -- the executor's state machine -- holds 2,202 blocks and 27,879
operations, 80% of the program. Every backend pass walks it, and the SafeWIR
checker's cost grows faster than linearly in function size (earlier
measurements put the two quadratic paths near n^1.8). With a cold verdict
store, after any `witchy-wir` edit, checking that one function takes ~30s.

That function is the same in every program. The verdict store already relies
on this: its key writes raw function and signature ids, and it hits across
programs, so the executor's ids are stable between programs. Only the verdict
is reused today; everything that produces the function, and everything done
with it after checking, is redone.

The cost is concentrated in tests. The seven async examples were 133 of the
205 seconds of the example oracle sweep; dozens of conformance tests compile
async programs; and the slowest tests in the gate are async programs that
happen to absorb the cold check.

## The hazard

A cache here must never be keyed on less than everything that shapes its
output. The failure mode is not an admission failure: SafeWIR checking would
still pass a stale lowering, because stale-but-safe code is safe. The program
would simply compute something other than what its source says. Every stage
below therefore either:

- reuses an artifact keyed by a digest that provably covers its inputs, or
- recomputes and compares, in a verification mode the test suite runs, before
  the reuse is trusted.

## Design

### The unit: a precompiled standard library

At build time or first use, compile `std` (at least `chan`, `task`,
`future` and their dependencies) into a **std artifact**: the checked, lowered
and validated SafeWIR for every std function that does not depend on a user
type parameter, plus their layouts and encoded Wasm bodies. The artifact is
keyed by the compiler's own identity (the digests `witchy-wir`'s `build.rs`
already computes, widened to every crate that shapes lowering, and the
embedded std source). A different compiler never reads another compiler's
artifact.

Generic std functions instantiated at user types are still monomorphized per
program. The executor evidently does not depend on user types: its verdict-store
key, which includes its full body and every id it names, is the same across
programs. Phase 0 confirms this across a wider corpus before anything relies
on it.

### How a program uses it

The frontend treats std declarations from the artifact as already checked:
their signatures and types are loaded, not re-inferred. Lowering emits only
the program's own functions and generic instantiations, and references std
functions by their stable ids. Admission validates the program's functions and
the call boundaries into std, and splices the artifact's pre-encoded bodies
into the output module.

## Plan

Each phase is independently measurable and leaves `master` releasable.

**Phase 0: prove the ceiling.** Measure what each stage costs for std
functions versus user functions on the async corpus, so each later phase has a
target. Confirm that std function ids and layouts are stable across a
representative set of programs, not just the examples.

**Phase 1: reuse the backend for std functions, with verification.** Cache
the lowered, validated and encoded output of each std function that is not
instantiated at a user type, keyed by the canonical serialization the verdict
store already uses (function body, callee signatures, imports, layouts,
closure table, tail obligations) plus the compiler identity. Add
`WITCHY_STD_CACHE=verify`, which recomputes every hit and fails on any
difference. The gate runs the full suite in verify mode before the cache is
trusted, and a nightly job keeps running it. This removes most of the 2.6s
backend without touching the frontend.

**Phase 2: split the executor.** Independently of caching, break the 2,202-block
state machine into smaller functions. Because checking cost is superlinear,
this cuts the cold check well below 30s even when the cache is cold, and it
shrinks every backend pass. This is async-lowering and std-runtime work and
coordinates with the RFC-0167 track.

**Phase 3: skip the frontend for std.** Load std declarations and types from
the artifact instead of re-linking and re-type-checking them, removing most of
the 1.5s frontend. This needs the typed std to be serializable, and is the
largest change.

**Phase 4: ship the artifact with the toolchain.** Build it once per release
alongside `witchyc` (RFC-0168), so no user and no test process pays for it.

## Acceptance

- The full workspace suite passes with `WITCHY_STD_CACHE=verify`, with zero
  mismatches, before the cache is enabled by default.
- A warm async compile of `examples/channels` drops from ~4s CPU to under
  1s after Phases 1 and 3.
- The cold SafeWIR check of the executor drops below 5s after Phase 2.
- The artifact key includes the digest of every crate that shapes lowering and
  admission, not only `witchy-wir`. (The current verdict-store stamp covers
  only `witchy-wir/src`; an adversarial review found the checker also reads
  `witchy-syntax` types, so this phase fixes that gap too.)

## Alternatives

- **A per-process cache of lowered functions without a verification mode.**
  Faster to build, but a key that misses one input fails silently and
  produces wrong programs. Rejected.
- **Multi-module Wasm linking.** Keep std as a separate Wasm module and link
  at instantiation. It would change the admission model and the ABI and
  requires cross-module ownership rules. Out of scope; the artifact is spliced
  into a single admitted module instead.
- **Only split the executor.** Worth doing on its own merits (Phase 2), but
  it makes the recompilation cheaper rather than removing it.

## Open questions

1. Which std functions are genuinely independent of user types? The executor
   is; generic collection functions are not. Phase 0 answers this with data.
2. Does admission need the full std function graph to validate a program's
   calls into std, or only the signatures and contracts? The verdict store's
   contract summaries suggest the latter.
3. Where does the artifact live for tests: per checkout (like the verdict
   store) or per compiler build under `target/`?
