---
rfc: 0036
title: Bounding the async executor — ownership-threaded state and recursive drop
status: partially-implemented (Design B landed 2026-07-04; recursive $rdrop remaining — NOT a blocker; blocker precisely located 2026-07-05 = the per-capture move/borrow oracle, see final note)
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
  because the single own-param ABI ([RFC-0033](0033-place-based-uniqueness.md),
  `codegen/mod.rs:543`) fires only when the function **returns the single own buffer directly** with
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
- `check.sh --fast` green. (rc-floor is default-on since 974ccee — see the correction
  header — so the DoD gate is the full sweep *under* rc-floor, not a toggle back to off.)
- **Never commit an unproven `dec`** — a wrong `dec` is a use-after-free. If a step can't be
  verified against the gate, commit only the verified pieces and report honestly.

## Non-goals

- Bounding executor *time* (Design C leaves it O(n²)); the DoD is memory (`live_cells`).
- Flipping rc-floor on by default (a separate decision, after both residuals bound + a
  full differential sweep proves parity with every other lever).

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections.** Diagnosis verified in full: the step_one shape
at std/task.witchy:208; the N=8000 benchmark cap; the #[ignore]d DoD test at
stats.rs:646; shell-only drop at wir_helpers/mod.rs:673; single own-param ABI at
codegen/mod.rs:543. The correction header is accurate — rc-floor IS default-on —
but the comment at opt.rs:150-155 still contradicts the code one line above it,
and a DoD bullet still describes rc-floor as OFF. One structural omission:
RFC-0055's erased-executor work left full scheduler copies in BOTH
std/task.witchy AND std/chan.witchy.

**Required revisions.** (1) Fix the stale "rc-floor OFF" DoD bullet. (2) Design B
must cover both executor copies (task + chan) — or sequence after a task/chan
dedup — otherwise one copy silently keeps the OOM ceiling. (3) Update the
analysis.rs citation.

**Verdict.** Small revision, then implement. The plan (B first → recursive
$rdrop → A as north star) is right and respects the generality rule. Priority:
medium-high — elevated to high the same day by RFC-0059, which adopts Design B
as its Stage 0. The OOM at ~9k messages is a functional ceiling on the
concurrency substrate, i.e. language surface, and the highest-value performance
remainder.

## Implementation note (2026-07-04) — Design B landed; recursive `$rdrop` deferred

**Design B (owning iterative executor): IMPLEMENTED, both copies.** `std/task.witchy`
and `std/chan.witchy` `run` are rewritten from the tail-recursive
`step_round`/`step_one`/`try_*`/`close_*` chain — which threaded `slots`/`channels` as
BORROWED params rebuilt per step through a `(slots, channels, Bool)` tuple return, so
every `set_at` took the copy path — into a single iterative loop whose `slots`/`channels`
are confined-unique local `var` accumulators, mutated in place with self-assign
(`slots = list.set_at(slots, …)` / `list.push(…)`), the exact shape RFC-0035/0016 reclaim
per step. The schedule is unchanged and verified byte-identical on both backends via
`witchy parity` across every concurrency example (for_await/scope/worker_pool/select/
channels/async_tasks/request_reply/conventions). The task/chan DEDUP the review note
offered as an alternative was NOT taken: `std/chan` defines its own erased
`Step`/`Task`/`Slot` and the async lowering selects the executor by import, so sharing
them would touch the RFC-0055 erasure boundary; Design B was applied to each copy.

Effect (chan_throughput DoD, N=200, rc-floor on): live_cells **26569 → 18608**. The
per-message ARRAY churn (formerly O(n^2) abandoned copies) is now bounded; the compiled
benchmark's OOM ceiling moved from ~9k to ~10k messages. The RESIDUAL is a FLAT ~93 live
cells PER MESSAGE (measured constant across N = 50…800), i.e. the per-message CLOSURE
garbage — the `and_then` continuation towers each `await` rebuilds. Design B does not
touch it: shell-only drop frees the `Slot`/`Step` shells, not their `Task`→closure
children.

**Recursive `$rdrop`: NOT shipped this session (deferred, by this RFC's own DoD hard
rule).** Reaching `< 500` live cells requires reclaiming those closure children, i.e.
recursive drop. A SOUND implementation is a large matched-pair change:

- **dup-at-construction** — every ADT/record/tuple/list/closure build that stores a
  statically-offset-0-rc child must `$rc_dup` it, or recursive drop underflows (a wrong
  `dec` = use-after-free). The construction surface is broad; its COMPLETENESS is exactly
  what the full force-copy + heap-type-matrix + fuzz gate exists to prove.
- **drop-time shape** — recursive drop must know which slots are heap children. For
  aggregates the static type at the drop site suffices (a per-type drop). For CLOSURES it
  does NOT: a `Task(fn() -> Step)` field's type does not describe the closure's captures,
  so closure drop needs a DEFUNCTIONALIZED per-code-index drop — a drop table parallel to
  the `$__lamw{i}` call table, each `$__lamdrop{i}` dropping its captures' statically-known
  kinds. The executor's dominant leak IS closures, so this is required, not optional.
- **erased `__Msg` (RFC-0055)** — the executor's `List(__Msg)` buffers carry values of
  OPAQUE kind; recursive drop must LEAVE them undropped (a sound leak), never guess, or a
  heap message payload is a UAF. Sound for the Int-message DoD (scalar); leaky-but-safe
  for heap messages.

Per this RFC's DoD ("Never commit an unproven `dec` — a wrong `dec` is a use-after-free …
commit only the verified pieces and report honestly", and "the WHOLE gate, not a subset
(HARD RULE)"), the recursive-`dec` change was not landed under the reduced validation
available this session — a recursive drop is precisely the change that gate exists to
qualify. Design B, which uses only the existing proven reclamation machinery and cannot
introduce a UAF, is committed; recursive `$rdrop` remains the sole open item for this
RFC's DoD and for RFC-0059 Stage 0. The DoD test
(`stats::chan_throughput_bounded_by_rc_floor`) stays `#[ignore]`d with an updated reason
pinning the ~93-cell/message residual.

## Implementation note (2026-07-05) — blocker located precisely; deferral upheld under the full gate

A full-gate session confirmed and DEEPENED the 2026-07-04 deferral. Two safe fixes
landed; the reclaiming `dec` did not (it would be an unproven use-after-free).

**Drift repaired (safe, verified).** Both `benchmarks/chan_throughput.witchy` and the DoD
test source had drifted to a bare `import chan`, which no longer brings the `Sender` /
`Receiver` TYPES into scope — every working concurrency example uses
`from chan import Receiver, Sender`. The benchmark did not compile ("unknown type
`Sender`") and the DoD test errored before it could measure. Both now use
`from chan import Receiver, Sender`: the benchmark compiles and runs (prints `8000`, both
backends) and the DoD test measures again — **`live_cells` = 18608 at N=200** with rc-floor
on, matching the ~93-cell/message residual. N stays at 8000; the ~10k OOM ceiling is
unchanged (it is the very thing recursive drop would lift, so it cannot move until the
`dec` lands).

**Why the reclaiming `dec` is still blocked — sharper than "matched-pair".** A per-type /
`$__lamdrop{i}` recursive drop plus dup-at-construction is NOT sufficient on its own,
because the two dup policies trade off against each other:

- **always-dup** every heap child at construction is SAFE (never under-counts) but does
  NOT reclaim — the constructor temporaries' `rc=1` is never released, so a recursive drop
  can never reach 0. It is memory-neutral pure overhead (this is the direction that cannot
  UAF, and it is also the direction that fails the DoD).
- **move (no dup) on last use** is what actually reclaims, but it requires proving, per
  construction argument AND per closure capture, that the occurrence is a genuine last use
  (a transfer of ownership) rather than a still-live alias. A wrong move = a shared value
  freed while live = a use-after-free.

The executor's dominant garbage is the `and_then` continuation tower
(`std/chan.witchy` `and_then_step`): each step rebuilds `fn(u): and_then(cont(u), k)`
capturing the params `cont` and `k` — both **closures threaded through a recursive chain**
(single-consumer, linear) — alongside the **erased `__erase(msg)`** payload. An
unconditional `$__lamdrop{i}` that drops every i32 capture would drop the shared
`cont`/`k` (double-free along the chain) and the opaque `__Msg` (a heap-message UAF). So
each capture drop must be conditioned on a **per-capture move/borrow decision**, which is
exactly the **inter-procedural ownership** question: are `and_then` / `and_then_step`'s
params CONSUMED (move — the callee may drop) or BORROWED (the caller retains)? Under
witchy's default `let` (borrow) convention a borrowed param must NOT be dropped by the
callee; only an `own` / last-use-move param may be.

This is precisely the piece the `last_use` oracle (`crates/witchy-lower/src/analysis.rs`,
~lines 1610-1616) **deliberately does not ship unverified**: "the full backward-liveness
drop-at-last-use … MUST discharge two soundness obligations before it can place a drop on
a *used* value — the Perceus dup/move discipline … and inter-procedural escape via
`Summaries::arg_leaks`." Recursive `$rdrop` inherits BOTH obligations, at every child and
every capture. Per this RFC's HARD DoD rule ("Never commit an unproven `dec`"), the
reclaiming `dec` is not landed: `$__lamdrop{i}` cannot be soundly wired until the
move/borrow oracle for construction arguments and closure captures exists.

**Precise scope for the next increment** (same shape, sharper gate). The construction
surface is now mapped and centralized: `lower_aggregate`
(`crates/witchy-lower/src/codegen/mod.rs` ~3349-3368) covers ADT/record/tuple/list, and
`lower_lambda` (~5197-5270) covers closures; the closure code-index registry is
`lambda_wir_funcs` (index = code index) with per-capture kinds in `cap_info`; the drop
sites are mod.rs ~2811 (set_at displaced), ~3295 (read-binding last use), ~3766 (match
scrutinee); the drop-time field kinds come from `adt_variants` / `record_field_types` and
`type_is_offset0_rc`; `__Msg` is `Type::Named("__Msg", …)` (skip — leak-safe).
1. Build the per-argument / per-capture **move-vs-dup oracle** (the deferred `last_use`
   bulk + `Summaries::arg_leaks` for callee-retains, extended to closure captures). This
   is the keystone both dup-at-construction and recursive drop consume.
2. `$__lamdrop{i}` drops ONLY captures the oracle marks owned-by-this-closure; borrowed /
   opaque (`__Msg`) / scalar captures are skipped (leak-safe).
3. Per-type recursive drop for aggregates keyed on the same oracle for their fields.
4. Prove under the WHOLE gate (force-copy metamorphic + heap-type-matrix +
   `WITCHY_HEAP_CHECK` redzone fuzz at substantial seed count + full concurrency parity).
   The DoD test stays `#[ignore]`d (source now compiles; residual 18608) until green.
