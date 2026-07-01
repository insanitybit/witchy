---
rfc: 0036
title: Bounding the async executor — ownership-threaded state and recursive drop
status: partially-implemented (NOT a blocker — premise corrected)
created: 2026-07-01
predecessors:
  - "0035 (completing the RC floor — the Perceus dup/drop floor this builds on)"
  - "0034 (closing the compute gap — where the executor leak was first diagnosed)"
  - "0033 (place-based uniqueness — the own-ABI this must extend)"
  - "0016 (reference-counted memory — the reclamation design)"
tracking: "RFC-0035 shipped the per-object RC floor (universal [rc][size] header +
  Perceus dup/drop) and bounds every CONFINED-UNIQUE churn pattern (a set_at / read-out
  / match-on-read loop reclaims to ~roots: 20006 -> 7 live cells). It does NOT bound the
  async executor (chan_throughput: ~1.1M live cells, unbounded). This RFC scopes the one
  remaining feature: making the executor's threaded state reclaimable."
correction: "This RFC was assumed to gate the rc-floor promotion (release==all). MEASUREMENT
  DISPROVED that (2026-07-01): rc-floor is memory-NEUTRAL on the executor — chan_throughput
  live_cells is 1116919 with the lever OFF vs 1116918 ON, i.e. the ~1.1M leak is a PRE-EXISTING
  arena limitation, unchanged by rc-floor. So rc-floor regresses nothing async, and it was
  PROMOTED to default-on (974ccee) without this RFC. The real rc-floor blocker turned out to be
  SEC-037 (an $rc_dup use-after-free), now fixed. This RFC remains a worthwhile SEPARATE perf
  improvement (a channel-heavy program still OOMs ~9k messages), NOT a safety/correctness blocker."
---

# RFC-0036: Bounding the async executor — ownership-threaded state and recursive drop

## Summary

The RC floor ([RFC-0035](0035-completing-the-rc-floor.md)) reclaims churn when the
mutated container is a **confined-unique local accumulator** — `x = list.set_at(x, i, v)`
in the same function, where `x` owns its buffer. Under those conditions `set_at` mutates
in place and the displaced element drops at last use, so a churn loop stays flat
(`__witchy_live_cells` 20006 → 7).

The async executor (`std/task`'s scheduler) does **not** meet those conditions, and no
reclamation strategy inside the current design bounds it. The root cause is **ownership,
not refcounting**: the executor threads its state — `slots: List(Slot(m))` and
`channels: List((List(m), Int))` — as **borrowed** parameters through a tail-recursive
call chain (`run` → `step_round` → `step_one` → `try_push`/`try_pull`/…), and each step
mutates them with `list.set_at(slots, …)` in **return position** inside a tuple
`(slots, channels, Bool)`. Consequences:

1. `slots` is never proven unique inside those functions (it is a borrowed param), so every
   `set_at` takes the **copy** path — O(slots) allocation per step, O(steps) per round.
2. The intermediate copies are owned by nobody who can reclaim them: they are locals in
   tail-recursive frames, borrowed by the next call, then abandoned when the frame is gone.
3. The displaced `Slot`/continuation the `set_at` replaces is not dropped (the copy path
   shares element pointers without a matching dup — RFC-0035 §step-2 excludes it), and even
   when a buffer *is* freed, the drop is **shell-only** so its nested
   `Slot → Task → continuation` children leak.

Measured: chan_throughput at N=8000 leaks ~1.1M live cells; a small N=200 run leaks ~26k,
with rc-floor on ≈ off (it reclaims essentially nothing). `benchmarks/chan_throughput.witchy`
itself documents the cap at ~8–9k messages before an OOB trap and flags the fix as a known
prerequisite.

**This is a design-level gap in the ownership ABI, not a refcount bug.** This RFC records
the three candidate designs, why the existing machinery cannot express the executor's shape,
the recommendation, and the verification gate.

## Why the existing machinery cannot reach it (evidence)

Four probes/attempts, all confirming the gap:

- `own xs` param + `x = bump(move xs, i)`, and `own xs` + `var s = xs; s = set_at(s,…); s`,
  and a `var xs` param (writeback) — **all copy** (reowns = 20000 on a 20k loop). A *direct*
  local self-assign loop is reowns = 1. So in-place `set_at` is recognized **only** for a
  confined *local* accumulator, never an `own`/`var` param.
- An implementation attempt (cap-token transfer at `var s = move own_p`) was **inert**:
  `cur_fn_own_param` is `None` for a function that returns a *derived* value or a *tuple*,
  because the own-ABI ([RFC-0033](0033-place-based-uniqueness.md), `analysis.rs`
  `~348-364`) fires only when the function **returns the single own buffer directly** with
  **one** extra carried cap result. The executor returns `(slots, channels, Bool)` — **two**
  owned buffers threaded through a tuple — which the single-buffer/single-cap own-ABI
  **fundamentally cannot express**.
- The `Summaries` alias facts (`may_alias_out` / `arg_leaks`) conflate "returns the arg"
  with "stores the arg into a fresh buffer", so they cannot drive an init-aliasing or
  returns-fresh decision without over-conservatism (it would kill `var d = dict.insert(dict.new(),k,v)`).

## The three candidate designs

### Design A — multi-buffer own-ABI

Generalize the own-ABI so a function can thread **N** owned buffers through its
signature, each with its own carried cap token, including when they are returned inside a
tuple. Then `fn try_push(own slots, own channels, …) -> (List(Slot(m)), List((…),Int), Bool)`
mutates both in place and returns them with their tokens; the caller threads the tokens on.

- **Pros:** most general; benefits any multi-owned-value function; keeps the executor's
  functional structure; O(1) per step (true in-place, no copies).
- **Cons:** the largest compiler change. The own-ABI signature synthesis, the caller-side
  token threading, and — the hard part — **cap tokens carried *positionally inside a
  returned tuple*** (today one trailing i32 result). Requires deciding how a tuple result
  carries K extra cap words and how the caller destructures them (`let (s2, c2, p) = …`
  must recover s2's and c2's tokens). Touches `analysis.rs` (own-ABI detection over
  tuple returns + derived returns), `assemble_wir_func` (signature), and the call/return
  lowering.

### Design B — rewrite the scheduler to local self-assign accumulators

Restructure `std/task` so `slots`/`channels` live as **local `var` accumulators inside one
loop** and are mutated with self-assign `slots = list.set_at(slots, …)` — the shape the
existing in-place + reclamation machinery already handles. This means collapsing the
tail-recursive `step_round`/`close_round` into `while` loops and inlining (or restructuring)
`step_one`/`try_*` so the mutations land on the loop's local accumulators, not on borrowed
params.

- **Pros:** **no compiler change** — uses RFC-0035/0016 as-is; the win is proven for local
  accumulators. Contained to one stdlib file.
- **Cons:** a large, careful rewrite of subtle async-scheduler logic (fork/open/cancel/join,
  the quiescence close pass), with the risk concentrated in the async semantics rather than
  the compiler. Note the **alias-init hazard** (RFC-0016 fix): a loop that starts
  `var s = slots` where `slots` is a borrowed param must **not** free s's first buffer — so
  Design B still needs the loop's accumulator to be *owned* (an `own`/`move` seed), which
  loops back to a smaller version of the ownership question. The cleanest realization keeps
  the whole scheduler in `run` with `slots`/`channels` as owned locals from the start.

### Design C — copy-path refcounting + recursive `$rdrop`

Keep the executor as-is (copies), but make the copies **memory-bounded**: in
`list_set_cap`'s cold (copy) path, `$rc_dup` every copied element (so both buffers own
them); `$rc_drop` the displaced element; and when a buffer is freed, **recursively**
`$rdrop` its elements. Time stays O(n²) (copies still happen), but MEMORY (`live_cells`)
becomes bounded — which is the DoD metric.

- **Pros:** no executor rewrite; generalizes the RC floor to non-unique containers.
- **Cons:** O(n) dup per copy (doubles per-step cost); needs **element-kind info threaded
  into the type-erased `list_set_cap`** helper (it copies uniform i64 slots and can't tell a
  heap pointer from a scalar); needs **recursive `$rdrop`** which needs **dup-at-construction**
  (an `ADT`/record/tuple build that captures a heap value must dup it, else recursive drop
  underflows) — a broad emission expansion. And it does **not** fix the intermediate-buffer
  ownership problem on its own (Design C reclaims *elements*; the borrowed tail-recursive
  intermediates still need last-use drops the callee can't emit). Realistically Design C only
  works layered on Design A or B.

## Recommendation

**Design B first (owned iterative scheduler), then recursive `$rdrop` (a subset of Design C)
for the nested children — with Design A as the eventual general answer.**

Rationale: Design B needs no unproven compiler change to the *reclamation* path (the highest
UAF risk area), it is the cleanest realization if `run` holds `slots`/`channels` as owned
locals for the whole scheduler, and it makes the win measurable immediately. Recursive
`$rdrop` (construction-dup + a per-type recursive drop) is then needed for the
`Slot → Task → continuation` children regardless of A vs B, so it should be built and gated
next. Design A (multi-buffer own-ABI) is the right *general* mechanism and should be the
north star, but it is the largest change and can follow once B proves the executor bounds.

Whichever path: **the ownership decision must be designed before code is written** — that is
the decision the maintainer owns.

## Definition of done

- `stats::chan_throughput_bounded_by_rc_floor` (today `#[ignore]`, pins the target) passes:
  the executor reclaims per-message garbage to **bounded** `__witchy_live_cells` (`< 500` at
  N=200; flat as N scales; no OOB trap to 40k).
- Both residuals flat: `cache_eviction` (already) **and** chan_throughput.
- The gate holds under the change — **the whole gate, not a subset** (HARD RULE, inherited
  from RFC-0035): the force-copy metamorphic sweep over every example (catches an in-place
  aliasing UAF with NO oracle), the full oracle sweep under `WITCHY_OPT=rc-floor`
  (`every_example_agrees_under_rc_floor`), the `WITCHY_HEAP_CHECK` differential fuzzer under
  `all` (redzone net), and the heap-type-matrix corpus.
- `check.sh --fast` green; rc-floor OFF by default.
- **Never commit an unproven `dec`** — a wrong `dec` is a use-after-free. If a step can't be
  verified against the gate, commit only the verified pieces and report honestly.

## Non-goals

- Bounding executor *time* (Design C leaves it O(n²)); the DoD is memory (`live_cells`).
- Flipping rc-floor on by default (a separate decision, after both residuals bound + a
  full differential sweep proves parity with every other lever).
