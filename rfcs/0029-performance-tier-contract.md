---
rfc: 0029
title: The performance-tier contract — normal mode, opt mode, and one substrate
status: proposed
created: 2026-06-28
tracking:
---

# RFC-0029: The performance-tier contract

## Summary

The capstone that ties the performance and ergonomics work into one plan. witchy
has exactly two tiers over one analysis substrate, governed by a single rule:
**normal mode optimizes for ergonomics and may run a bit below Go; opt mode
charges real syntax cost and must pay it back with a *categorical* (asymptotic,
not constant-factor) speed win.** This RFC states that contract, the rule that
decides which optimization lives in which tier, how the two tiers compose, how
witchy beats Go without taxing normal code, and the sequencing that makes the
contract true rather than aspirational. It is the spine the other performance
RFCs ([0016], [0024]–[0028]) hang off; it changes no status of theirs, it
organizes them.

[0016]: 0016-reference-counted-memory.md
[0024]: 0024-unified-facts-lattice.md
[0025]: 0025-frozen-deep-immutability.md
[0026]: 0026-unique-qualifier.md
[0027]: 0027-packed-layouts-sroa.md
[0028]: 0028-ergonomic-mutable-value-semantics.md
[0030]: 0030-perf-correctness-infra.md

## Motivation

The performance story is now spread across many documents — the uniqueness pass
([ownership-analysis.md](ownership-analysis.md)), regions
([regions.md](regions.md)), the codegen roadmap ([0017]), the mode design
([performance-modes.md](performance-modes.md)), and the five RFCs [0024]–[0028]
plus RC ([0016]). Each is locally coherent, but there is **no single statement of
the contract** they collectively implement, and without one two failure modes
are live:

[0017]: 0017-codegen-performance.md

- **Opt mode can take on syntax cost without delivering a payoff that justifies
  it.** Today the mode ships its *discipline* (mandatory conventions, cliffs as
  errors — [performance-modes.md](performance-modes.md) buckets 1–2) but not its
  *unlocks* (`packed`, `unique`, `frozen` are proposed). A mode that costs syntax
  and returns "guaranteed, not faster" is a bad trade and erodes trust in the
  feature.
- **Normal mode can be quietly compromised in the name of speed**, or left
  awkward in the name of generality — when in fact normal code should be
  *ergonomic and safe first*, accepting a speed gap, with one hard exception.

The contract below removes both ambiguities by making the tier of every
optimization a consequence of one rule, and by stating what each tier *promises*
the programmer.

## Design

### The tiering rule

> **Normal-mode (default) optimization** — ships iff it needs **no programmer
> promise**: sound on fully-general code, proven opportunistically by the
> analysis, and a miss costs at most a **copy**, never a wrong answer.
>
> **Opt-mode-only optimization** — ships iff it requires **giving up a
> generality** (aliasing freedom, dynamic layout, escape freedom) that only a
> whole-file promise can supply; in exchange a miss is a **compile error**, not a
> copy.

And the corollary that protects the programmer's syntax investment:

> **An opt-mode knob must buy an asymptotic or near-asymptotic win** (cache /
> layout / SIMD, or guaranteed zero-copy), never a mere constant factor. If a
> promise buys only a few percent, it is not worth a syntax cost and stays out of
> the mode.

This is the filter for what is even *allowed* to be opt-only, and it is what your
intuition — "normal can be a good bit slower than Go; opt must be much faster" —
becomes as an enforceable design constraint.

### Tier 0 — the substrate (invisible; identical in both modes)

Runs the same in both tiers; the only difference upward is *severity*, not
mechanism.

- Value semantics, capabilities, parity, deterministic concurrency.
- **One analysis**: the uniqueness pass + the [0024] escape/confinement lattice.
  Best-effort in normal mode (a missed fact → a copy); contract-*checked* in opt
  mode (a missed fact → an error). Same lattice, same code.
- **RC floor ([0016])** for reclamation. *This is a correctness feature, not a
  speed knob* — see the normal-mode carve-out. It is invisible (no syntax) and
  must be present for the normal-mode contract to hold.
- AST const-folding/propagation; AOT module serialization (microsecond cold
  start).

### Tier 1 — normal mode (ergonomics first; a speed gap is acceptable)

The promises normal code makes to the programmer:

| Promise | Mechanism |
|---|---|
| Never forces an annotation | conventions optional; analysis opportunistic |
| Reads like Go/Swift | [0028]: `xs.push(v)`, `for var x`, confined `View`s |
| A miss is a copy, never a bug | self-healing `__cap` token; forced-copy oracle |
| **Never OOMs** | RC floor ([0016]) reclaims escaping / long-lived / evicted values |
| Always safe | capabilities, bounds checks, no data races, determinism |

Representation stays the uniform boxed slot (simple, general). **Speed target: at
or above Go on strings / concurrency / cold start; acceptably below on
struct/numeric.** This is the deliberate trade — bought with zero syntax.

**The one hard exception to "slower is fine":** a long-running normal-mode
program must not leak. "A bit slower" is acceptable; OOM is not "slow," it is
broken. This is why RC sits in tier 0 as a correctness floor and is the single
non-negotiable in the normal-mode tier.

### Tier 2 — opt mode (syntax cost → Rust-class, or it does not ship)

| You pay (the promise) | You get (the categorical win) |
|---|---|
| mandatory conventions; cliffs are errors | guaranteed in-place reuse — no copy fallback |
| `unique` / `local unique` ([0026]) | destination-passing, eager-free — contract, not hope |
| `packed` + SROA ([0027]) | unboxed, cache-dense, SIMD-eligible layout — the asymptotic lever |
| `frozen` ([0025]) | zero-copy sharing across value boundaries |
| transitive (`opt` imports only `opt`; std exempt) | the guarantee holds over the whole reachable graph |

**Speed target: Rust-class on compute/struct; beats GC on concurrency.** Each bit
of syntax maps to a surrendered generality, which maps to a categorical speedup —
enforced by the corollary above.

### How the tiers compose

The normal↔opt boundary is **just a call site with a coercion**, not a protocol
([performance-modes.md](performance-modes.md) already establishes this):

- Because the linker flattens the whole program before analysis, an opt callee is
  never the pessimistic "unknown callee" — its summary is computed across the
  seam like any other.
- **Monomorphization is the bridge**: a generic stdlib function gets a
  `packed`/`unique` specialization at an opt call site and the boxed best-effort
  one at a normal site, chosen per site. So you write the hot 5% in `opt` and the
  rest in normal mode and they interoperate with no glue, and most crossings never
  reshape data.

So adopting opt mode is incremental and local; it never forces a rewrite of the
program around it.

### How witchy beats Go (honest scorecard)

witchy does **not** try to beat Go everywhere in normal mode. It wins on chosen
axes in *both* tiers, ties or lags on struct-numeric in normal mode, wins
decisively in opt mode — and beats Go on safety and ergonomics regardless.

| Axis | Normal mode | Opt mode | Evidence |
|---|---|---|---|
| Cold start | beats Go | beats Go | AOT serialize (measured: microsecond-class) |
| Strings | **~4–5.7× Go** | ≥ that | measured, `bench/BASELINE.md` (single workload) |
| Lists / dicts / compute | parity with Go | ≥ parity | measured, `bench/BASELINE.md` |
| Concurrency throughput | beats GC | beats GC | design (no pauses, bulk free, `frozen`); to be benched |
| Struct / numeric | ~Go or a bit below | **Rust-class** | *target* — requires [0027]; not yet measured |
| Safety (capabilities) | wins | wins | n/a — categorical |
| Long-lived mutable graphs | concede / index-handles + RC | concede | by design ([spec/performance.md](../spec/performance.md) Phase 4) |

Measured today: strings faster, the rest at parity, cold start ahead. Projected
(and gated on the opt unlocks): struct/numeric to Rust-class. The scorecard is
deliberately split into *measured* vs *target* so the contract is not oversold —
the struct-numeric win is a promise the bucket-3 work must still cash.

### Sequencing — the commitments that make the contract true

Order matters, because each tier's promise has a prerequisite:

0. **Infrastructure first ([0030]) — the gate.** The `WITCHY_OPT` differential
   de-opt lever, the deterministic `witchy stats` counters, and gated
   benchmarking/soak tests must exist *before* any optimization below. The rule:
   no feature in steps 1–4 ships until it is in the `WITCHY_OPT` registry (passes
   `-O == all` differentially) **and** its claim is pinned by a counter assertion
   (`copies == 0`, `allocs == 0`, `peak_heap < budget`). This is what keeps the
   system correct and measurable as it grows.
1. **[0028] (ergonomic mutable value semantics).** Cheapest, highest leverage: it
   is the *normal-mode ergonomics floor*. Without it, normal code is awkward and
   users reach for references they do not need.
2. **RC floor ([0016], now `planned`) — ship it.** It is the normal-mode
   *never-OOM* floor (tier 0), not an opt feature; the normal-mode contract does
   not hold until it lands. (The [spec/performance.md](../spec/performance.md)
   positioning has been re-scoped accordingly — long-lived evicting state is in
   scope via RC, not conceded.)
3. **[0024] (unified facts/escape lattice).** The substrate the opt unlocks and
   the confined views consume; land before building more consumers on the six
   scattered escape computations.
4. **[0025]–[0027] (the opt unlocks: `frozen`, `unique`, `packed`+SROA).** These
   are what make opt mode *much faster* and thereby honor the corollary. **Do not
   sell opt mode's syntax cost as a speed feature until at least `packed`+`unique`
   ship** — until then opt mode delivers safety/guarantees, which is buckets 1–2,
   not the full bargain.

## Alternatives

- **No mode at all; one tier.** Either everyone pays Rust-style annotation cost
  always (loses the ergonomic default that is most of witchy's appeal), or no one
  gets the layout/zero-copy wins (caps witchy below Go on struct-numeric forever).
  The two-tier split is precisely how you get both.
- **More than two tiers** (e.g. a middle "hint" mode). Rejected for now: two
  tiers already cover "no promise / full promise"; a partial-promise tier adds
  teaching surface without a clear payoff. Revisit only if a real cluster of
  optimizations wants a middle promise.
- **Let opt mode change semantics** (e.g. different overflow, different
  equality). Rejected, hard: a mode changes *which implementation runs* and
  *whether a de-opt is an error*, never observable behavior — enforced by the
  `WITCHY_OPT` differential de-opt framework ([0030]). This is what keeps parity and
  keeps the mode boundary a coercion rather than a semantic fault line.
- **Drop the normal tier; be a systems language.** Rejected: it abandons the
  Go/Python/Ruby/Swift target set and the capability-secure-scripting niche that
  is witchy's reason to exist.

## Drawbacks

- **Two tiers are two mental models** and a boundary to understand. Mitigated by
  the boundary being a plain call site, by opt mode being strictly additive
  (every normal program is a valid opt-free program), and by `fmt`/`check`
  guiding the transition with the `why_not` provenance the analysis already
  computes.
- **The contract is only as honest as the sequencing is kept.** If opt-mode
  syntax ships before its asymptotic payoff, the corollary is violated in
  practice even if stated here. The sequencing section is therefore a commitment,
  not a suggestion; `bench/BASELINE.md` is the check.
- **A capstone RFC can drift from the implemented reality** like any RFC. Per the
  rfcs/ cardinal rule, the authoritative tier behavior lives in `spec/` and the
  code; this RFC is the decision and the plan, frozen once accepted.

## Prior art

- [performance-modes.md](performance-modes.md) — the bucket model and the
  "restrict for a whole file" framing this RFC turns into a single rule.
- [spec/performance.md](../spec/performance.md) — the measured scoreboard and the
  Go-targeting strategy; this RFC is its organizing contract.
- C `restrict`, Swift `@frozen`, Rust `unsafe` (inverted) — "give up a generality,
  gain an optimization," the precedent for opt mode's bargain.
- Koka/Lean 4 (Perceus) — facts-driven reuse with RC as the sound floor; the
  model for tier 0's RC + analysis-elision.

---

> 2026-06-29: **Implementation status + per-feature insight (living note).**
>
> SHIPPED (parity-safe, differential-swept, fuzzer-validated under `WITCHY_HEAP_CHECK`):
> - **RFC-0030** — the whole gate: single `WITCHY_OPT` lever + 9-entry registry,
>   differential de-opt sweep, `witchy stats` counters, soak, bench leg, and a
>   checked-heap fuzz leg in `check.sh --full`. 4 optimizations wired+validated
>   (`inplace`, `region`, `sroa`, `fold`) plus opt-in `wasm-opt`. Plus `check.sh
>   --fast` commit gate.
> - **RFC-0028** (3/3, → `implemented`) — `for var` write-back iteration,
>   `nodes.push(x)` mutating-method statements, AND confined slice views (feature
>   3): a confined read-only `list.slice` elides its copy and reads through the
>   source via `$list_at_view`/`$list_len_view` (gated `WITCHY_OPT=views`,
>   default-on; `stats::confined_view_elides_the_slice_copy` + sweep). Realized as
>   invisible copy-elision, NOT a new `View` type — see the 0028 change-note.
> - **RFC-0027** (SROA half) — escape-driven scalar replacement of frame-confined
>   records/tuples, read-only AND field-mutated, closure-safe.
> - **RFC-0024** — the escape oracle (`crates/witchy-lower/src/escape.rs`),
>   consumed by SROA and by confined views (`confined_slice_candidates`).
> - **never-OOM** (the goal's normal-mode clause) is demonstrated: 5000-iteration
>   loops with transient allocs + scalar-returning calls run in O(1) heap via the
>   loop-watermark + SROA + in-place machinery.
>
> REMAINING, with the cheapest known implementation path for each:
> - **Confined-view follow-up (0028)** — the slice form shipped; extend the same
>   view machinery to `windows`/`chunks` (they yield `Iter` of windows, a
>   different producer shape), and someday the explicitly-deferred escaping and
>   mutable views. Lower priority than the levers below.
> - **`packed` (0027 other half)** — the only lever changing cache asymptotics;
>   most invasive (every host fn reading `List(<record>)` becomes layout-aware).
>   Mode-gate it.
> - **RC floor (0016)** — from-scratch refcounting; all-or-nothing (no safe
>   partial). LOWER marginal value than first thought: never-OOM is already met
>   for loop/transient patterns; RC's residual win is the long-lived *escaping*
>   heap (caches), which in-place dict/list already bounds by peak size.
> - **`frozen` (0025) / `unique` (0026)** — type-qualifier machinery (parser +
>   typeck), then boundary-copy elision / contract-checked in-place reuse.
>
> Each is a multi-turn lift at the SROA pace; the substrate (lever, sweep, stats,
> escape oracle, fast gate) is in place so each plugs into the proven playbook
> (analysis → gated codegen → differential + counter).
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
