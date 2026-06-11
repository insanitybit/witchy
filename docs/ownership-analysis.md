# The uniqueness pass — one ownership analysis for every fast path

## Status — SHIPPED (2026-06-11: 9a1ffb5 core, 842bac3 own-ABI)

All phases landed, with one design simplification the implementation
surfaced: **the runtime `__cap` token is self-healing** (one copy re-owns),
so no liveness fixpoint is needed at all. The analysis finds *share events*
(statements that can create a live whole-alias → the consumer zeroes the
token there, path-sensitively, because kills attach to the statement that
shares) and *dirty sites* (self-assigns whose RHS embeds a share → forced
zero token). Everything else — including "alias dies before the loop" — is
the token's job: the kill costs one re-own copy, not a disqualification.

Shipped beyond the plan: the own-ABI (`x = f(move x)` pipelines carry the
token across calls — Phase 2's stretch goal), the `__witchy_reowns` counter
(tests assert copy *counts*, not timings), and the consumption check (facts
are keyed by AST identity; a cloned subtree is a loud compile error).
Deviations, recorded: the loop-watermark scan was already semantic
(escape-based), so it stays; the interpreter's in-place guard is
evaluation-order-based (its values are fully owned — aliasing cannot occur),
so its `expr_mentions` check is already exact and stays.


Today the compiled tier's in-place machinery (linear update, loop watermark
resets, region copy-out) runs on three separate **syntactic whitelists**: a
variable is eligible only if every use of it matches a recognized pattern, and
one unrecognized read disqualifies it forever. The fallback is always
semantically correct (copying), but it is a *silent O(n²) cliff* that ends in
a memory-cap trap. The interpreter's in-place path has a fourth, slightly
different scan (`expr_mentions`).

This plan replaces all of them with **one dataflow analysis** that computes,
per program point, whether a binding's buffer is *uniquely owned* — and makes
every miss loud.

## The soundness model (what "unique" means here)

Witchy has value semantics; the backends pass pointers and preserve the
semantics through the **owned-slack rule**: a function may mutate a buffer in
place only if *it allocated that buffer itself* (tracked today by the shadow
`__cap` locals — a received value arrives with `cap = 0`, so the first append
copies). The analysis generalizes this:

> A binding `x` is **Unique** at point *p* iff the buffer it denotes was
> allocated (or `own`-transferred) under this binding, and no other *live*
> binding, field, or escaped structure can observe it at *p*.

Mutating a Unique binding in place is unobservable. Everything else copies.
**The analysis never changes semantics — only which implementation runs.**
That invariant is what makes the project safe to build aggressively.

## The analysis

A per-function, flow-sensitive pass over the lowered AST (post records/
traits/sugar — the form both backends consume), exposed as a queryable
side-table so the AST needs no node IDs:

```rust
pub struct Facts { /* keyed by (stmt path, name) */ }
impl Facts {
    fn unique_at(&self, path: &StmtPath, name: &str) -> bool;
    fn loop_body_escape_free(&self, path: &StmtPath) -> bool;
    fn why_not(&self, path: &StmtPath, name: &str) -> Option<Disqualifier>; // diagnostics
}
pub fn analyze(func: &Function, summaries: &Summaries) -> Facts;
```

- **Lattice** per (point, binding): `Unique` | `Shared(reason)` | `Dead`.
- **Liveness**: a standard backward pass. The headline win over the current
  scan: `let alias = xs` only kills `xs`'s uniqueness *while `alias` is
  live*. Alias-dies-before-the-loop is the most common false
  disqualification today.
- **Alias events**: `let y = x` (shared while both live); storing `x` into a
  structure (list/tuple/record/dict/closure capture/message/actor field)
  escapes it; `region`/`retain` boundaries are transparent (the region rules
  already forbid the dangerous writes at check time).
- **Loops**: fixpoint over `while`/`for` bodies (two passes suffice for this
  lattice: anything live across the back-edge stays live).

## Interprocedural summaries (the part the conventions were built for)

A bottom-up pass over the call graph (SCCs collapse conservatively) computes
per function, per parameter:

| Summary fact | Meaning | Source of truth |
|---|---|---|
| `borrows(i)` | param `i` cannot escape the call | typeck's `let` no-escape rule — already enforced |
| `consumes(i)` | callee owns the buffer | the `own`/`sink` convention |
| `returns_alias(i)` | the return value may alias param `i` | computed (e.g. `fn id(xs): xs`) |
| `escapes(i)` | param `i` may be stored beyond the call | computed |

Unlocks, in order of value:

1. **Calls stop being kill-everything.** Passing `x` to a function whose
   summary says `!returns_alias && !escapes` leaves `x` Unique. This alone
   recovers most real-world disqualifications (helpers that read, format,
   validate).
2. **`x = f(move x)` becomes in-place.** When `f` `consumes(0)` and
   `returns_alias(0)`, the whole call chain is a linear pipeline. The `own`
   ABI grows a slack word (the callee receives the caller's `__cap`, returns
   the new one — multi-value returns are already in use), so an owned buffer
   keeps its capacity *across calls*. This is the payoff for `own`/`move`
   existing in the language.
3. **Recursion degrades gracefully**: an SCC member gets the conservative
   summary; the fallback is the copying path, never an error.

## Consumers (all four, one source of truth)

1. **Linear update** (codegen): `unique_at` replaces `push_eligible_vars`/
   `scan_push_*`. The self-assign *shapes* stay (they pick the in-place
   helper); the *eligibility* comes from Facts. Strictly wider, never
   narrower: the old whitelist is a subset of what the dataflow proves.
2. **Loop watermark resets** (codegen): `loop_body_escape_free` replaces
   `loop_arena_resettable`'s ad-hoc scan.
3. **Region copy-out / Phase 4 destination inference** (codegen): the region
   tail's uniqueness is exactly the precondition for routing its allocations
   to the parent lane. Still gated on `__region_copy_bytes` showing volume.
4. **The interpreter's in-place assign**: `try_inplace_assign`'s
   `expr_mentions` guard is replaced by the same Facts — both backends then
   share one definition of "fast", not just one definition of "correct".

## Diagnostics — the cliff becomes loud (ships FIRST)

`why_not` provenance feeds a check-time note (and the LSP):

```
note: `xs` is rebuilt by copy on every iteration of this loop — O(n²)
  --> sim.witchy:14    xs = push(xs, step(xs, i))
  because `xs` is passed to `step`, which may retain or return it
  (declare `step`'s parameter `let` to certify it only reads)
```

Notes, not errors; perf-shape warnings must never block a build. The
reporting channel lands in Phase 0 against the *existing* scans, so the value
arrives before the dataflow does, and the new analysis inherits tests that
pin exact messages.

## Verification — the risk is silent corruption, treat it as such

A wrong Unique conclusion mutates a buffer someone can see: silent
wrongness, the one class witchy refuses. Three layers:

1. **Forced-copy differential mode**: `WITCHY_NO_INPLACE=1` compiles with
   every optimization off (the copying paths ARE the semantics). CI runs the
   example sweep + test corpus both ways and diffs output. Any divergence is
   an analysis soundness bug surfacing loudly.
2. **Adversarial alias suite**: aliases created/dropped around loops, return-
   aliasing helpers, `own` chains, closures capturing accumulators, actor
   fields, region/tail interactions — each asserting both the *output* and
   (where the optimization should fire) the *behavior* (flat memory, the
   `__region_copy_bytes`-style counters).
3. **Fuzzing**: extend the existing proptest generator toward alias-heavy
   programs; the property is interp == wasm == wasm-forced-copy.

## Phases

- **Phase 0 — loud cliff.** `why_not`-style notes from the *current* scans
  (they already compute disqualifiers, then discard them); `witchy check`
  + LSP surface them. Exit: the O(n²) revert is visible at check time.
- **Phase 1 — the dataflow core.** Liveness + alias lattice, `Facts` API,
  intraprocedural only (calls still kill, minus `let`-borrow calls, which
  typeck already certifies). Swap in as the eligibility source for all four
  consumers. Exit: alias-dies-before-loop and read-after-loop shapes go
  in-place on both backends; forced-copy differential green; suite green.
- **Phase 2 — summaries.** Bottom-up `returns_alias`/`escapes`; calls stop
  killing; `own`-ABI slack transfer; `x = f(move x)` pipelines in-place.
  Exit: a multi-function builder pipeline runs O(n) end-to-end, proven by a
  bench and the memory counters.
- **Phase 3 — consolidation.** Delete `scan_push_*`, `loop_arena_resettable`,
  `expr_mentions`-guard; region Phase 4 reevaluated against the counter with
  uniqueness available. Exit: one analysis, zero bespoke scans, docs updated
  (architecture.md + the performance appendix).

## Non-goals

- **No user-facing borrow checker.** The conventions stay the only surface
  syntax; this is compiler-internal inference. A program that ignores every
  annotation still runs — at worst it copies, and now it *says so*.
- **No semantic change of any kind** — enforced by the forced-copy mode.
- **No cross-module summaries in v1**: the linker flattens to one module
  before codegen, so the call graph is already whole-program; true separate
  compilation is a later problem.
