---
rfc: 0031
title: SIMD acceleration for stdlib hot loops
status: deferred
created: 2026-06-29
tracking: accepted in principle but not scheduled; revive only with a numeric-kernel target and an RFC-0005 SIMD-hardening update
---

# RFC-0031: SIMD acceleration for stdlib hot loops

This RFC remains intentionally deferred: the current performance contract is
tracked in [`spec/performance.md`](../spec/performance.md), while parallelism
trade-offs are recorded in [RFC-0032](0032-multi-core-execution.md). No SIMD
implementation is claimed by this design record.

## Summary

Exploit **data parallelism within a single core** by emitting wasm `v128`
(128-bit SIMD) operations for a handful of byte/word-at-a-time stdlib hot loops —
string compare/search, list scans, and the flat `packed`-layout scans
([0027](0027-packed-layouts-sroa.md)) — instead of the current scalar loops.
This is the one parallelism axis that costs nothing in the twin-backend parity
or capability model: SIMD changes *how fast* a deterministic computation runs,
not *what* it computes. It is the natural complement to the compile-path
parallelism we already get for free (wasmtime compiles module functions across
cores) and the determinism-preserving alternative to true multi-core execution
([0032](0032-multi-core-execution.md)).

## Motivation

A survey of where parallelism could help witchy found:

- **Backend compilation is already parallel** — wasmtime/cranelift compiles a
  module's functions across cores by default, and the optimized wasm is safely cached, so
  there is nothing to win there.
- **Program execution is single-threaded by design** — `spawn`/channels are a
  cooperative executor inside one wasm instance, so a CPU-bound program uses one
  core. Making that multi-core is a determinism-affecting architecture change
  ([0032](0032-multi-core-execution.md)).
- **SIMD is the gap in between**: real speedup, no new core, no determinism cost.

The witchy WASM tier is already compute-competitive with Go on the benchmark
suite (and faster on `strings`, thanks to the memory model), but the byte-wise
stdlib primitives are scalar. The workloads that lean on them — text scanning,
`list` membership/scan, and the `packed` flat-buffer reductions — are exactly the
shape a 16-byte-at-a-time loop accelerates. wasmtime 45 supports the wasm SIMD
proposal; we just don't emit it.

## Design

Two pieces: enable the engine feature, then emit `v128` in the hand-written WIR
helpers for a small, measured set of hot loops.

### Engine

`relaxed-simd` carries **non-deterministic** operations (e.g. fused-multiply-add
rounding, certain min/max NaN behaviors) whose results can differ across hosts —
that is fatal to the parity invariant (the interpreter is the scalar oracle, and
the two backends must agree bit-for-bit). So:

- Enable only the **deterministic SIMD subset** (the base wasm SIMD proposal:
  integer lanes, shuffles, byte compares, `v128.any_true`/`all_true`), NOT
  `relaxed-simd`'s nondeterministic ops, unless an op is proven host-stable.
- The interpreter stays scalar; parity holds because the SIMD lowering computes
  the **same integer result** as the scalar loop (no float reassociation, no
  relaxed rounding). This is enforced by the existing differential sweep.

### Targeted helpers (WIR, `crates/witchy-wir/src/wir_helpers.rs`)

Vectorize the loops where 16 bytes/8 i16s/4 i32s per iteration pays:

- `$str_eq`, `$find_byte` (substring search), `$ascii_case`, `$trim` scans —
  byte compares with `i8x16.eq` + `v128.any_true`, tail handled scalar.
- `list` scans behind `cmp.*` (`member`/`index_of`/`count`) — i32/i64 lane
  compares.
- the `packed` flat-buffer reductions (`list.at(_,i).field` sum/scan loops) —
  the densest win, since the data is already contiguous.

Each is a self-contained helper rewrite with a scalar fallback for the
remainder; no change to calling code or semantics.

### Gating and DoD

SIMD is representation-neutral (identical output), so it does not need a
`WITCHY_OPT` *correctness* toggle the way an allocation optimization does — but to
fit the [0030](0030-perf-correctness-infra.md) contract and keep a clean
differential, add a `WITCHY_OPT=simd` lever (default-on once stable) so the sweep
can run `-simd == +simd == none` and prove byte-identical output, and so the
scalar path stays exercised. The "it fired and helped" proof is a **wall-clock
bench** (`bench/`) rather than a `witchy stats` allocation counter, because SIMD
saves cycles, not bytes — a per-helper microbench (e.g. search over a 1 MB
buffer) on the bench machine, tracked against [`bench/BASELINE.md`](../bench/BASELINE.md).

## Alternatives

- **`wasm-opt` auto-vectorization** instead of hand-written intrinsics. Binaryen
  rarely vectorizes these idioms from scalar wasm, and we'd lose control over the
  deterministic-subset constraint. Hand-written helpers are few and hot.
- **`relaxed-simd` for the float reductions too.** Rejected for now: its
  nondeterminism breaks parity. Revisit only per-op, with a host-stability proof.
- **Do nothing / wait for multi-core** ([0032](0032-multi-core-execution.md)).
  Multi-core is a far larger, determinism-affecting change; SIMD lands value now
  with no such cost.

## Drawbacks

- The interpreter must keep producing the identical scalar result, so any SIMD
  op whose result could differ from the scalar computation is off-limits — this
  rules out the relaxed/float ops and constrains the lowering.
- Tail handling and alignment add code to each helper (a maintenance cost), and
  SIMD wins evaporate on tiny inputs (keep a length threshold before the vector
  path).
- Another `WITCHY_OPT` lever to carry and sweep.

## Prior art

Go and Rust hand-vectorize `bytes`/`memchr`-class routines; wasm SIMD is the
portable 128-bit form. The constraint that matters here — *deterministic* SIMD
only, to preserve a scalar oracle's agreement — mirrors how the
`packed`/`unbox` work kept representation changes output-invariant under the
differential sweep ([0027](0027-packed-layouts-sroa.md)).

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections.** Dead as written: the RFC predates the shipped
RFC-0005 step-7 engine hardening, which explicitly disables SIMD
(`wasm_simd(false)`, runtime.rs:466-467) — and never mentions it. Its string/list
stdlib-helper targets sit where measured performance already meets the tier
targets; the real remaining gap is tight scalar user loops, which needs the
auto-vectorization the RFC itself concedes is unavailable. Its cmp.* targets are
slated for dedup into list.*.

**Required revisions.** Status changed `proposed` → `deferred` (this edit).
Re-scope conditions for reviving it: a numeric-kernel target (not the
string/list helpers), and an explicit negotiation of the `wasm_simd(false)`
hardening decision with RFC-0005.

**Verdict.** Mark deferred. Priority: low.
