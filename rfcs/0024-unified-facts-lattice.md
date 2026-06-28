---
rfc: 0024
title: Unified facts / escape lattice
status: proposed
created: 2026-06-28
tracking:
---

# RFC-0024: Unified facts / escape lattice

## Summary

Consolidate the half-dozen disconnected analyses that today answer "does this
value escape this scope?" / "is this binding uniquely owned here?" into **one
`Facts` oracle**: a small set of monotone lattices (uniqueness, escape/
confinement, liveness) computed once over the linked module, with every
optimization re-expressed as a pure *consumer* that queries it. This is the
enabling refactor for the rest of the performance-knob work
([0025](0025-frozen-deep-immutability.md), [0026](0026-unique-qualifier.md),
[0027](0027-packed-layouts-sroa.md)): once the substrate exists, a new knob is
"raise a fact" or "add a consumer," not bespoke plumbing threaded through the
lowerer.

## Motivation

The same question — *what is the smallest scope this value is confined to, and
who else can observe it?* — is currently computed in at least six places, in six
representations:

1. the uniqueness pass's **share events** (storing a value into a structure
   escapes it) — `crates/witchy-lower/src/analysis.rs`
2. **`loop_body_escape_free`** (an escape-free loop body permits a per-iteration
   watermark reset) — codegen
3. **`borrow_escape_check`** (a `let`-borrow parameter may not be returned) —
   typeck
4. the **region outer-assignment rule** (a non-scalar allocated in a `region:`
   may not be assigned to an outer binding) — typeck
5. the **lambda-capture escape scan** (which locals a closure captures) — syntax
6. region copy-out's **watermark short-circuit** (`ptr < wm` ⇒ the value
   predates the region, return it without copying) — codegen `$rcopy`

This is exactly the fragmentation that [ownership-analysis.md](ownership-analysis.md)
already eliminated *once*, when it folded three separate eligibility whitelists
plus an `expr_mentions` helper into a single uniqueness pass. The next
consolidation is the same move at the next level up.

The fragmentation has concrete costs:

- **Every new optimization re-derives escape from scratch.** `frozen`
  (share-by-reference), unboxed layouts (escape→stack), per-task arena resets,
  and returnable views each need an escape answer; without a shared oracle each
  reimplements a slightly different, slightly wrong version.
- **Two async-era consumers have no home.** `await`-capture escape (every value
  live across an `await` is captured into a continuation closure — a share the
  uniqueness pass conservatively copies) and per-task reclamation (the
  `std/task` executor has no per-step reset) are both escape queries with
  nowhere to ask.
- **`mode opt` needs precision the scattered passes can't give.** A mode file
  *errors* on a missed fact instead of silently copying, so the analysis must be
  precise enough to stand alone — a single sharpened lattice is tractable to make
  field- and path-sensitive; six ad-hoc walks are not.

## Design

The shape is textbook abstract interpretation: separate **analysis** (lattices
that monotonically learn more) from **transformation** (consumers that read facts
and rewrite). witchy is unusually suited to it — whole-program after linking,
value semantics (so alias/escape lattices stay tractable), capability purity
(effects are already a lattice).

### The lattices

All three are per-(program-point, binding), join-semilattices, computed by one
worklist pass over the post-link AST. They run **after async lowering** (so
continuation-closure captures appear as the shares they are) and after const
inlining / the AST optimizer.

**Uniqueness** — already exists, lifted verbatim:

```
Unique  ⊑  Shared  ⊑  Dead
```

`Unique` = no live alias; `Shared` = an alias may be observed; `Dead` = moved /
consumed / past last use. Backed at runtime by the self-healing `__cap` token
exactly as today (live = owned slack, zero = next self-assign re-owns) — the
lattice does not change the runtime model, only where the fact is produced.

**Escape / confinement** — the new core. For each value, the *smallest enclosing
scope it is provably confined to*:

```
Frame  ⊑  Loop(n)  ⊑  Region(r)  ⊑  Call  ⊑  Module  ⊑  Escapes
```

- `Frame` — never leaves the current function activation (SROA / stack-allocation
  candidate).
- `Loop(n)` — confined to loop body `n` (watermark-reset candidate).
- `Region(r)` — confined to region `r` (the `region:` copy-out / reclaim story).
- `Call` — may be returned to the immediate caller but no further by this
  function (this is `local unique`'s confinement — see [0026](0026-unique-qualifier.md)).
- `Module` / `Escapes` — stored somewhere globally reachable / passed to an
  unknown callee.

**Liveness** — standard last-use, needed by eager-free and by the `Dead`
transition. Already implicit in `take_kills`; made explicit here.

### The oracle API

One struct, queried by lowering and typeck. Names are illustrative:

```rust
pub struct Facts { /* lattices, indexed by (StmtKey, binding) */ }

impl Facts {
    pub fn uniqueness(&self, at: StmtKey, name: &str) -> Uniqueness;
    pub fn confined_to(&self, at: StmtKey, name: &str) -> Scope;
    pub fn last_use(&self, at: StmtKey, name: &str) -> bool;
    pub fn escapes(&self, at: StmtKey, name: &str) -> bool; // confined_to == Escapes
}
```

Interprocedural propagation stays as it is conceptually: `Summaries::of_module`
already computes `may_alias_out` per parameter as a bottom-up fixpoint; under
this RFC it becomes the *transfer function* for the escape lattice at call sites
(a parameter with `may_alias_out[i] = false` does not raise its argument's escape
level; `arg_live`'s three outcomes — transparent / transport / kill — are reads
of the lattice). Summaries remain context-insensitive (one per function), so a
single annotation on a hot helper still helps every call site.

### The six consumers, rewritten

Each existing computation becomes a thin query. No behavior changes — this is
proven by the differential de-opt framework (`WITCHY_OPT`, [RFC-0030](0030-perf-correctness-infra.md)) plus
the parity suite.

| Today | Becomes |
|---|---|
| uniqueness share events | `facts.uniqueness(..) == Shared` |
| `loop_body_escape_free` | `facts.confined_to(body) ⊑ Loop(n)` for all allocs |
| `borrow_escape_check` (typeck) | `let`-borrow ⇒ assert `confined_to ⊑ Call` |
| region outer-assignment rule | assign target ⇒ assert escape ≠ region-internal |
| lambda-capture escape scan | the capture set *is* the escape transfer at a closure node |
| region watermark short-circuit | `confined_to(v) ⊐ Region(r)` ⇒ value predates `r` |

### Staging

1. **Land the oracle alongside the existing passes**, computing the same facts;
   assert equality against each legacy computation in tests (no consumer switched
   yet). This is the safety net.
2. **Switch consumers one at a time**, deleting the legacy computation as each
   flips, gated by the parity suite + forced-copy mode.
3. **Sharpen** (field- and path-sensitivity) only once consumers are unified —
   this is the precision `mode opt` needs and where a real CFG/SSA would land if
   the structural AST dataflow proves insufficient.

No new inference *engine* is required for steps 1–2: the whole-program
interprocedural backbone, affine enforcement, and escape facts already exist;
this collects them behind one interface.

## Alternatives

- **Do nothing / keep adding bespoke passes.** Each new knob
  ([0025](0025-frozen-deep-immutability.md)–[0027](0027-packed-layouts-sroa.md))
  reimplements escape. The drift between six escape notions is already a latent
  source of soundness bugs (e.g. an escape the lambda scan sees but the region
  rule doesn't); more consumers make it worse. Rejected.
- **Jump straight to CFG/SSA.** Tempting, but it couples the consolidation (low
  risk, code-deleting) to a representation change (high risk). Stage it: unify
  first on the existing structural dataflow, add SSA only if precision demands.
- **A full effect system instead of escape lattices.** witchy already has one
  effect lattice that matters — capabilities — and it stays separate. Folding
  escape into a general effect calculus is more machinery than the problem needs.

## Drawbacks

- A central oracle is a central point of failure: a bug in the lattice is a bug
  in *every* optimization at once. Mitigated by step 1 (run alongside, assert
  equality) and the forced-copy differential oracle, which together make any
  divergence loud.
- The query API risks becoming a god-object. Keep it to the three lattices above;
  resist adding consumers' private state to it.
- Precision work (step 3) is genuinely open-ended; this RFC deliberately scopes
  to consolidation and lists sharpening as follow-on, not commitment.

## Prior art

- [ownership-analysis.md](ownership-analysis.md) — the prior consolidation
  (three whitelists → one pass) this RFC generalizes.
- [performance-modes.md](performance-modes.md) §"The unified escape/fact analysis
  framework" — proposes exactly this; this RFC is its detailed form.
- Abstract interpretation (Cousot & Cousot): the analysis/transformation split.
- Perceus (Koka) and Lean 4 FBIP: facts-driven reuse, the model for "optimization
  as a pure consumer of an ownership oracle."

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
