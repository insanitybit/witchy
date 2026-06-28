---
rfc: 0030
title: Performance & correctness infrastructure — one opt lever, differential de-opt, deterministic counters
status: proposed
created: 2026-06-28
tracking:
---

# RFC-0030: Performance & correctness infrastructure

## Summary

The foundation that must exist **before** any of the performance work
([0024]–[0029] + RC [0016]) is implemented, so the system stays correct and
measurable as it grows. Four pieces: (1) a **single `WITCHY_OPT` lever** that
turns individual optimizations on or off as a list, replacing the proliferation
of `WITCHY_NO_*` env vars; (2) a **differential de-opt framework** that, for
every optimization in the registry, asserts the program produces byte-identical
output with that optimization off — soundness made systematic; (3) **deterministic
white-box counters** (`witchy stats`) so performance claims ("0 copies here", "0
heap allocations in this opt fn", "constant memory") are *unit tests*, not flaky
benchmarks; (4) **gated benchmarking** — wall-time, peak memory, and soak/eviction
legs wired into the green gate, plus checked-heap coverage for RC. This is the
**gating step 0** of [0029]'s sequencing: no feature ships without its de-opt
toggle and its counter assertions.

[0016]: 0016-reference-counted-memory.md
[0024]: 0024-unified-facts-lattice.md
[0029]: 0029-performance-tier-contract.md

## Motivation

The performance program adds optimizations that each have a "fast path" and a
"correct fallback": in-place mutation vs copy, unboxed vs boxed layout, confined
view vs materialized copy, elided RC vs full RC. Two things must be true of every
one of them, forever:

- **It changes no observable behavior** ([0029]'s non-goal). The only proof that
  holds up is differential: run with the optimization off, run with it on, assert
  identical output on both backends — the same method the uniqueness pass already
  uses via `WITCHY_NO_INPLACE`.
- **It actually does what it claims.** A wall-clock benchmark is too noisy to
  prove "this accumulation did zero copies" (the project already has a flaky
  perf test); the claim has to be a *deterministic count*.

Today this exists for exactly one optimization (`WITCHY_NO_INPLACE`) and one
counter family (`WITCHY_REGION_STATS`/`__region_copy_bytes`). Scaling that to a
dozen optimizations by adding `WITCHY_NO_PACKED`, `WITCHY_NO_VIEWS`,
`WITCHY_NO_SROA`, … is the wrong shape: N env vars, N ad-hoc test paths, no way to
sweep them all. One lever + one counter command + one sweep is the right shape,
and building it first is what keeps the whole program falsifiable.

## Design

### 1. `WITCHY_OPT` — the single optimization lever

One environment variable controls which optimizations are active. It affects
**only performance, never observable behavior** — every setting must produce
identical output (the parity invariant; this is what the framework in §2 checks).

**Grammar.** A comma-separated list. The base is **all optimizations on** unless
the first token is `none`; subsequent `-<opt>` removes and `+<opt>`/`<opt>` adds:

```sh
WITCHY_OPT=all              # default when unset — everything on (production)
WITCHY_OPT=none            # everything off — the canonical de-opt reference oracle
WITCHY_OPT=-inplace        # all optimizations except in-place mutation
WITCHY_OPT=-unbox,-views   # all except packed layouts and confined views
WITCHY_OPT=none,inplace    # ONLY in-place (allowlist from nothing)
```

`WITCHY_OPT=none` is the reference semantics: pure value semantics, boxed
everywhere, copy at every boundary, full RC, no view/SROA/region reclaim. It is
the slowest run and the ground truth every other setting is diffed against.

**The registry** (each name is one optimization, independently toggleable):

| name | optimization | off ⇒ | owning RFC |
|---|---|---|---|
| `inplace` | uniqueness-driven in-place mutation | copy-per-update | [ownership-analysis.md](ownership-analysis.md) |
| `views` | confined zero-copy borrows | materialize to a copy | [0028] |
| `sroa` | non-escaping aggregates in locals | heap-allocate | [0027] |
| `unbox` | packed/unboxed layouts | uniform boxed slot | [0027] |
| `rc-elide` | RC inc/dec/free elision to the floor | full RC every op | [0016] |
| `region` | region / loop-watermark reclamation | no early reclaim | [regions.md](regions.md) |
| `fold` | AST const-fold + propagation | evaluate at runtime | [performance-modes.md](performance-modes.md) |
| `wasm-opt` | Binaryen post-pass | skip it | [spec/performance.md](../spec/performance.md) |
| `direct-call` | direct call when target is statically known | `call_indirect` | [spec/performance.md](../spec/performance.md) |

[0027]: 0027-packed-layouts-sroa.md
[0028]: 0028-ergonomic-mutable-value-semantics.md

Codegen-only names (`unbox`, `wasm-opt`, `direct-call`) are no-ops on the
interpreter, which is the oracle regardless. Adding an optimization to the program
means **adding one row here**, not a new env var.

**Migration (break, don't deprecate).** `WITCHY_NO_INPLACE` becomes
`WITCHY_OPT=-inplace`; `WITCHY_WASM_OPT=1` becomes `WITCHY_OPT=wasm-opt` (off by
default, so it stays opt-in); the old vars are removed in one cut, no alias.
`WITCHY_REGION_STATS` folds into the counters of §3, not into `WITCHY_OPT` (it is
observability, not a lever).

### 2. The differential de-opt framework

One harness, driven by the registry, makes soundness systematic:

```text
for each program P in the differential corpus:
    base = run(P, WITCHY_OPT=all)
    assert run(P, WITCHY_OPT=none) == base          # full de-opt
    for each opt O in registry:
        assert run(P, WITCHY_OPT=-O) == base         # each optimization is invisible
    assert run(P, WITCHY_OPT=all, backend=wasm) == run(P, WITCHY_OPT=all, backend=interp)
```

- Generalizes the existing forced-copy oracle from one optimization to all of
  them with no new env vars.
- A new optimization is **not done** until it is in the registry and passes
  `-O == all`. This is the gate clause [0029] step 0 enforces.
- **Mode-opt equivalence** rides the same harness: an `opt`-mode program and its
  normal-mode twin must produce identical output (a mode changes implementation,
  never behavior).

### 3. Deterministic white-box counters — `witchy stats`

A `witchy stats <prog>` subcommand (and a `testing` hook for in-language tests)
emits **deterministic** counts, so performance is asserted, not timed:

- `allocs` — heap objects allocated
- `copies` — value copies (the boundary tax)
- `rc_inc` / `rc_dec` / `frees` — RC traffic (once [0016] lands)
- `peak_heap` — high-water mark
- `region_copy_bytes` / `reclaims` — the existing region counters, generalized

These are exact and backend-stable (they count operations, not nanoseconds), so a
test can assert:

```text
assert stats(accumulate_loop).copies == 0          # in-place worked
assert stats(opt_centroid).allocs == 0             # SROA + packed worked
assert stats(server_soak).peak_heap < BUDGET       # never-OOM holds
```

This is what turns [0029]'s scorecard from a hope into a regression suite, and it
is immune to the flakiness that wall-clock benchmarks suffer.

### 4. Gated benchmarking, soak tests, and checked-heap for RC

- **Wire `bench/` into the green gate.** `bench/run.sh` already diffs against
  `bench/BASELINE.md`; add a `--bench` leg to `scripts/check.sh --full` so a
  wall-time regression fails loudly. Keep timing benchmarks for *trend*, counters
  (§3) for *correctness of the optimization*.
- **Memory + soak legs.** Track peak RSS, and add a soak/eviction corpus (a cache
  that grows and evicts, a long-lived server loop) asserting **bounded memory** —
  the [0016] never-OOM floor, checked via `peak_heap`.
- **Per-tier benchmarks.** The same program in normal and `opt` mode, asserting
  both the contract ratio (`opt` meaningfully faster) and byte-identical output.
- **Checked-heap coverage for RC.** Extend [0023](0023-checked-heap.md)'s redzones
  / shadow to assert the RC invariants when [0016] adds `free()`: refcount == 1 at
  every in-place site, use-after-free on drop. Run the differential fuzzer under
  `WITCHY_HEAP_CHECK` over the de-opt sweep.

## Alternatives

- **One env var per optimization** (`WITCHY_NO_PACKED`, …). Rejected: N vars, N
  test paths, no sweep, and the combinatorics of "all but one" become unmanageable.
  One list-valued lever is strictly simpler and enables the registry sweep.
- **A compile-time flag / build profile instead of a runtime env var.** Rejected
  for the differential harness: toggling at runtime lets one built artifact be
  diffed across settings; a rebuild per setting is slower and risks the two builds
  differing for reasons other than the optimization.
- **Wall-clock benchmarks as the correctness check.** Rejected: too noisy to prove
  "zero copies"; timing is for trend, counters are for correctness.
- **Build the infra alongside each feature instead of first.** Rejected — that is
  how a feature ships subtly wrong or a regression slips in unmeasured. The point
  of [0029] step 0 is that the net exists before the trapeze.

## Drawbacks

- The counters (§3) are instrumentation that must itself stay correct and not
  perturb the thing it measures; gate them behind `witchy stats` / a test hook so
  production runs never carry them.
- A single lever with a grammar is a small parser to get right (base + `±`);
  mitigated by keeping the grammar tiny and unit-testing it directly.
- Maintaining `bench/BASELINE.md` is ongoing toil and baselines drift across
  machines; mitigated by treating timing as trend (counters are the hard gate) and
  recording the host in the baseline.
- Codegen-only toggles being interpreter no-ops means the interpreter can't catch
  a codegen-only optimization's bug by itself — but the wasm-vs-interp diff in §2
  does, which is the existing parity guarantee.

## Prior art

- `WITCHY_NO_INPLACE` (the forced-copy oracle) and `WITCHY_REGION_STATS` — the
  existing one-optimization, one-counter precedents this generalizes.
- [0023](0023-checked-heap.md) — the memory-safety harness extended here for RC.
- LLVM `-O`/`-mllvm` pass control and `-stats`; rustc `-Z` flags — single
  controlled surface for toggling and measuring passes.
- Koka/Lean Perceus reuse counts; allocation-count testing in GC literature — the
  deterministic-counter approach to proving an allocation optimization fired.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
