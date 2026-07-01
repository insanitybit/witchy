---
rfc: 0035
title: Completing the RC floor — last-use reclamation + the lifetime model for reachable data
status: proposed
created: 2026-06-30
predecessors:
  - "0016 (reference-counted memory — the design this implements)"
  - "0034 (closing the compute gap — the executor leak diagnosed here)"
  - "spec/performance.md (the perf thesis; tracing GC rejected)"
tracking: "Implements the per-object refcount floor RFC-0016 scoped but left planned:
  the last_use(v,p) lattice + dec-at-last-use that bounds generally-escaping garbage.
  RFC-0016 shipped the analysis-driven elision rungs (rc-elide reuse, region/watermark)
  + the free-at-overwrite var floor; this completes it to the full Perceus floor."
---

# RFC-0035: Completing the RC floor — last-use reclamation + the lifetime model for reachable data

## Implementation status (2026-07-01)

**The mechanism has shipped; the executor residual is re-scoped.** Under
`WITCHY_OPT=rc-floor` (opt-in, off by default) the full Perceus discipline is implemented and
verified sound:

- **Universal `[rc][size]` header** — every value-producing allocator (records/ADTs/tuples/
  closures via `mk`, all list/string/dict producers, every host-import wrapper) routes through
  `$rc_alloc`, so any heap object carries a refcount word at `obj-8`. (Region-slide copies and
  the worker-VM `__galloc` are the documented header-less exceptions; the dict index word is
  dict-internal.)
- **`$rc_dup`/`$rc_drop` emission**, per-type gated to provably offset-0 elements
  (String/List/Tuple/closure/record/ADT; Dict/scalar/type-var excluded — a missed dup/drop leaks,
  never frees live): dup at the `list.at` read; drop the displaced element at in-place `set_at`;
  drop read-owned bindings at last use (`rc_owned_bindings`, drop-iff-dup by construction); drop
  the match-on-read scrutinee after the arms (per-depth save-slot pool for nested matches).
- **Gate**: a 13-case heap-type-matrix + channel-executor corpus (`rc_corpus_*`, incl. nested-match
  and nested-`List(List)`), a metamorphic `examples_agree_under_rc_floor` guard (every console
  example must be byte-identical default vs rc-floor), the `WITCHY_HEAP_CHECK` differential fuzzer
  under `all` (1200-program stress, 0 diverge), forced-copy, and `check.sh --fast` all green.
- **Soundness fix found via that example guard**: a pre-existing free-at-overwrite use-after-free
  (`var rest = s; rest = string.substring(rest, …)` in `std/string` — the first reclamation freed a
  buffer aliasing the borrowed param) is fixed by excluding alias-initialized vars from
  free-at-overwrite. rc-floor is now sound across the whole example suite.

This bounds every **confined-unique** churn pattern (set_at / read-out / match-on-read loops).
The drop is **shell-only** (a dropped shell's heap children leak — recursive `$rdrop` is future).

**The async-executor residual is NOT yet bounded, and the reason refines §1's "inter-procedural"
framing.** The blocker is not element liveness but **container uniqueness**: the executor
(`std/task.witchy`) threads `slots`/`channels` as *borrowed* params through a recursive call chain,
mutating them with `list.set_at(slots, …)` in **return position** (never a `slots = set_at(slots,…)`
self-assign). So the analysis never proves the buffer unique → every `set_at` takes the **copy**
path → O(steps) allocation, and the displaced element cannot be dropped (a copy still shares it).
The own-ABI RFC-0016 has enables in-place mutation only for the `p = f(move p)` *accumulator*
pipeline; it does **not** cover `set_at` on an `own` param in return position, nor does an `own`
param's uniqueness propagate to a local (`var s = own_p` still re-owns — measured). Closing
chan_throughput therefore needs a **uniqueness-analysis extension** (own-param uniqueness → locals,
and/or in-place `set_at` on an `own` param) so the executor's copies never happen — plus recursive
`$rdrop` for the nested `Slot→Task→continuation` children. That is a dedicated, UAF-sensitive
increment, tracked separately. The RC floor here is the correct reclamation *floor* for non-unique
churn; it is not a substitute for in-place mutation of the executor's O(n²)-copy schedule.

## Summary

[RFC-0016](0016-reference-counted-memory.md) is the design: **precise reference
counting with reuse (Perceus/FBIP)** as the universal reclamation floor, *complete*
(leak-free, no cycle collector) because witchy's value semantics force an **acyclic
heap**, with every existing optimization (bump, watermark, `region:`, the `__cap`
in-place token) reframed as **RC-elision** rungs that only ever *delete* count
traffic the analysis proves dead. What actually shipped is the lower half: the
analysis-driven elision rungs and the free-at-overwrite var floor (gated
`WITCHY_OPT=rc-elide`/`rc-floor`). The **per-object refcount word with
dec-at-last-use** — the part that reclaims *generally-escaping* garbage — was scoped
in RFC-0016's dev-log but deliberately not shipped, because **no existing oracle fact
yields a sound, bounded `dec`.**

This RFC completes it. Its load-bearing new content:

1. **A second, sharper residual is now pinned: the async executor** (RFC-0034). It
   leaks one continuation `Task` per message and OOMs at ~9–12k. Critically, the
   leak is **inter-procedural** — the element is read in `step_one` and overwritten
   by `set_at` in `try_push` — which *proves* the "keep extending static analysis"
   path is a dead end (a per-function liveness can't see the cross-function death).
2. **The missing analysis is `last_use(v,p)`** — the backward-liveness lattice
   RFC-0016 names as absent. This RFC specifies it. The crucial property:
   **dec-at-last-use is *local and compositional*** (a `$drop` at a use site, plus
   the own-ABI threading RFC-0016 already has), so it **sidesteps exactly the
   inter-procedural wall** that blocks static element-liveness. The refcount *is*
   the liveness, computed at runtime, exactly.
3. **A lifetime model for *reachable* data** (graphs, arenas, caches). RC closes the
   *unreachable*-garbage gap; it does not — and no GC does — close the *reachable*
   leak (an index-arena that never shrinks, a cache that never evicts). This RFC
   makes that boundary explicit and specifies the three first-class tools that bound
   it: `region:` (whole-structure lifetime, shipped), an arena/slotmap free-list
   (a stdlib type), and a separate VM (whole-heap drop, the `par_map` model).

Non-goals unchanged from RFC-0016: tracing GC (rejected — nondeterministic, poison
for parity + sandbox), a cycle collector (unneeded under the acyclic invariant),
and breaking value semantics.

## Motivation: two pinned residuals, and why neither yields to more static analysis

RFC-0016's floor reclaims what its elision rungs can *prove* dead. Two leak classes
are now empirically pinned, and both are *reachable-once-but-dead* garbage the
current facts cannot bound:

- **Cache eviction** (RFC-0016 dev-log): `d = dict.insert(d,k,v); d = dict.remove(d,k)`
  over distinct keys leaks O(n) (`stats::cache_eviction_leaks_without_rc_floor`).
- **The async executor** (RFC-0034): the cooperative scheduler replaces a slot's
  `Task` with its successor continuation every message —
  `slots = list.set_at(slots, i, Active(cont(unit())))` — and the displaced `Task`
  leaks. `chan_throughput` OOMs at ~9–12k messages under `default`,
  `WITCHY_OPT=rc-floor`, and `all` alike (verified inert). Throughput is ~19–26× Go
  on this shape.

The executor case is the decisive one, because it shows *why* the answer must be a
runtime refcount and not a smarter compile-time pass. The element is **read in one
function and overwritten in another**:

```
fn step_one(i, slots, channels) :: match list.at(slots, i):     # <- task bound here
    Active(task) -> match poll(task): Push(ch,msg,cont) -> try_push(i, ch, msg, cont, slots, channels)
fn try_push(i, ch, msg, cont, slots, channels) ::               # <- set_at (the free site) here
    ... (list.set_at(slots, i, Active(cont(unit()))), ...)
```

To free the displaced element *statically*, `try_push` would have to prove the
caller's `task` binding is dead — **inter-procedural element liveness**, strictly
harder than RC and *still* not general. That is the wall. A leak is not acceptable
(it violates witchy's no-GC full-reclamation promise; the OOM trap is that promise
breaking), and patching it per-shape with ever-deeper static analysis does not
generalize — which is the witchy principle inverted. The general, sound mechanism is
the per-object floor RFC-0016 already designed.

## The missing analysis: `last_use(v, p)`

RFC-0016 Part II inserts counts by the Perceus discipline (dup at a duplicated use,
drop at last use, borrowed params untouched) over a `last_use` liveness lattice that
**does not yet exist** in the compiler. This RFC specifies it as the one new analysis
needed to go from the shipped floor to the complete floor:

- **Shape:** a backward (post-order) liveness pass over the lowered, post-link,
  post-async-lowering AST — the same IR the uniqueness pass already consumes. For
  each heap-typed value `v` and program point `p`, `last_use(v,p)` is true iff no use
  of `v` is reachable after `p`. A fourth projection of the one `Facts` oracle
  RFC-0016 Part I describes, alongside `uniq`/`confine`/`borrow`.
- **Inter-procedural via summaries, not whole-program liveness.** The seam is handled
  *compositionally*: a value passed `own` threads its count into the callee (the
  existing `own_abi` summary), a `let`/borrowed param is never counted (R1), an owned
  arg the caller no longer uses is dropped *by ownership transfer at the call*, not by
  reasoning into the callee. So the executor's `task`→`try_push` flow needs **no
  cross-function liveness**: `task` is owned by `step_one`, threaded (or dropped) at
  the `try_push` call per the summary; inside `try_push`, `set_at` on an owned `slots`
  drops the displaced element via the runtime refcount — which is >1 exactly when some
  *live* alias still holds it, and 0 exactly when it is the executor's churn. This is
  the whole point: **the refcount resolves at runtime the aliasing question the static
  pass could not resolve across the seam.**
- **Soundness floor (unchanged from RFC-0016):** where the lattice returns ⊥, emit the
  full count op. A missed `last_use` costs one retained refcount (a later free, never
  a lost one); it can never free live data. Imprecision is slower, never unsound.

This is why RC dissolves the false dichotomy "leak or use-after-free": dec-at-last-use
frees iff the count is 0 (no leak), and *only* iff it is 0 (no UAF). The hard global
property becomes a local op plus a runtime word.

The mechanism beneath it — the `p-4` refcount header, `$dup`/`$drop`/`$free`,
`$rdrop_<shape>`, `$reset`/`$reuse`, the free list, the cost model, parity — is
specified in RFC-0016 Parts II–V and is **not restated here**; this RFC adds only the
`last_use` lattice that drives `$drop` placement and the migration that turns it on.

## The lifetime model for *reachable* data (graphs, arenas, caches)

RC (or any reclamation) only collects the *unreachable*. A distinct leak — and the one
a careful witchy programmer is most likely to hit — is **reachable-but-dead** data:
the slot in an index-arena you tombstone but never reuse, the entry in a `List` you
"removed" by leaving it, a cache that never evicts. **No memory manager fixes this** —
a tracing GC keeps that data just as faithfully as RC, because it is still reachable.
It is the dominant leak class in GC'd languages (listeners, growing maps), and it lives
in the data structure's logic, not the runtime. This RFC makes the boundary explicit
and specifies how witchy bounds it — better than a GC, because lifetime is *named*.

### Graph data is not a heap cycle

A cycle in a *graph* ("A connects to B connects to A") is not a cycle in the *heap*
("object A's field points to B's, B's to A's"). The former is **data** — encoded with
indices/keys into a flat arena — and is fully expressible (and idiomatic, and often
faster: flat, cache-dense, the petgraph/ECS/CSR pattern). The latter is the only thing
RC cannot collect, and value semantics already forbid it (no shared mutable reference
to close the back-edge). witchy already lives this way:

- **`examples/dijkstra`** — *"Nodes are indices; the graph is an edge list of
  `(from, to, weight)`."* A directed graph *with cycles*, zero heap reference cycles.
- **`examples/maze`** — grid `List(String)` + `Dict(Int,Int)` keyed by `row*width+col`.
- **`examples/bst`** — `type Tree: Leaf | Node(Tree, Int, Tree)`: recursive, but a tree
  (children built before parents; no back-edge).

So the acyclic-heap invariant RC depends on is **already honored by real programs** —
RC would be complete for them today, not after a rewrite.

### Three first-class lifetime tools (a spectrum)

The reachable-leak is bounded by *naming the lifetime* of the structure, RC reclaiming
its backing storage at the drop point:

| Tool | Lifetime granularity | Status | Best for |
|---|---|---|---|
| `region:` | the structure dies with a lexical scope | shipped | build-use-discard (load a graph, run an algorithm, return the answer) — whole structure freed at scope exit, O(1) |
| arena / slotmap (free-list) | long-lived, fine-grained add/remove | **new stdlib type** | a churning graph: `remove(handle)` recycles the slot, so the arena grows to the high-water-mark of *concurrent live* nodes, not total-ever-created |
| separate VM | the whole heap dies at once | shipped (`par_map` worker-VM model) | a big, isolated batch computation → serializable result; tear the VM down and its entire linear memory drops — no per-object reclamation at all |

RC underlies all three: at region end / arena reset / VM teardown the backing store is
freed in one shot, while the per-object floor handles the churn *under* the ceiling.
The remaining footgun — a long-lived churning arena with *no* free-list — is closed by
the stdlib slotmap (the data structure owns the reuse) plus the heap-growth signal
`witchy check`/`stats` already surface (a monotonically-growing collection is something
tooling can point at).

## The acyclicity invariant — proven, then guarded

RC-is-complete is not a freebie; it is an **invariant** the language must hold. To form
a reference cycle you must close a back-edge — make A reference B *after* B references A
— which requires observable mutation of shared state, or reference identity you can
mutate through. witchy forbids exactly that: value semantics (mutation produces a new
value; prior references don't observe it), closures capture **by value**, recursion is
**by name** (a code reference, not a heap pointer), construction is bottom-up, and the
in-place optimization fires only when *unobservable*. So the heap graph is a DAG by
construction — physically, not just logically.

This RFC's first obligation is therefore not the header word; it is to (a) state "no
reference cycles" as an explicit language invariant, and (b) **gate future features by
it**: any proposal that introduces shared mutable references, mutable reference
identity, or recursive-closure-by-reference must either preserve acyclicity or
explicitly trigger the fallback below. If the invariant must ever break, the answer
changes to "RC + a local cycle collector (trial-deletion) for the affected structure,
or a region/arena for it" — a deliberate, scoped escape hatch, not the baseline.

## Parity

Unchanged from RFC-0016 Part V: reclamation is **unobservable** (no finalizers, no
`weak`/identity/reference-equality, no in-language memory query), so the interpreter
keeps Rust `Drop` on its deep-clone `Value` and the WASM tier runs the floor; they
agree on every value and error, never on the byte at which a cell is reclaimed. Heap
*exhaustion* (the OOM trap) is a resource limit, not program semantics, and already
differs between the tiers. The leak oracle (`__witchy_live_cells == 0` at exit), the
double-free trap (rc below zero traps), and the `WITCHY_NO_RC_ELISION`/`NO_REUSE`/
`NO_FREE` differentials are RFC-0016's verification contract and apply here verbatim.

## Migration (from the shipped floor)

RFC-0016's Phases 0 and the elision rungs (R2/R4) shipped. This RFC is the rest of
Phase 1 + the analysis it needs, gated so each step is byte-identical with the lever
off (the de-opt sweep validates header invisibility; `WITCHY_OPT=none` stays the exact
pre-RC oracle):

1. **`last_use` lattice** — the backward liveness pass as the fourth `Facts`
   projection. Verified standalone against hand-checked expected drop points before any
   codegen consumes it.
2. **`$drop` insertion + the `p-4` header**, gated on `RcFloor` — per RFC-0016 Part II,
   driven by `last_use`; `$drop`→0 frees to the existing size-classed free list. The
   coarse `drop`-at-scope-exit fallback (RFC-0016 Phase 1) is the de-risk if precise
   last-use proves risky.
3. **Prove on both pinned residuals** — `cache_eviction_*` and a `chan_throughput`
   bound: heap flat at 40k+ messages, `__witchy_live_cells == 0` at exit, `interp ==
   wasm == wasm-no-elision` green, and an **adversarial use-after-free corpus** (an
   element read into a binding live past the overwrite, an aliased element, an element
   returned/stored/channel-sent, region/RC boundary crossings) that must each *retain*
   the count. The corpus is authored *first*, as the gate.
4. **Re-key elision onto the floor** — the shipped `rc-elide`/`__cap` reuse becomes the
   `uniq`-proven elision of the now-real count (RFC-0016 R2/R3); delete the standalone
   `cap==0` reasoning only after the differential passes.
5. **Default-on** once the corpus + soak + differentials hold across the suite — flipped
   from the lever, because the leak is only *fixed* (channel programs stop OOMing by
   default) when the floor is on by default. This is the step that demands the airtight
   bar: there is no opt-in to hide a wrong `dec` behind.
6. **Stdlib slotmap + the `region`/VM lifetime docs** — ship the reachable-leak tools
   and document the spectrum, so "how do I not leak a long-lived graph" has a
   first-class answer.

## Risks

- **A wrong `$drop` is a use-after-free** — the one class witchy refuses. Mitigated by
  the soundness floor (⊥ ⇒ keep the count), the adversarial corpus authored first, the
  always-on leak/double-free sanitizers, the differential sweep, and the coarse-drop
  fallback. Default-on only after all hold.
- **The acyclicity invariant breaking** under a future feature — mitigated by making it
  explicit and a feature gate; the scoped cycle-collector/region escape hatch is the
  contingency, not silent breakage.
- **Count traffic overhead** — the whole elision ladder (R1 borrow elision is most call
  traffic, R2/R3 reuse, R4/R5 confinement) exists to drive it to ~today's levels; the
  only net-new cost is on escaping-shared-long-lived data, which is exactly what leaks
  today, so RC is a strict improvement there.
- **The reachable-leak footgun** — bounded, not eliminated, by the lifetime tools; the
  honest residual that no runtime removes, made visible by named lifetimes + tooling.

## Dev-log

### 2026-06-30 — `last_use` → codegen consumption (migration step 1 + the *elision half* of step 2)

Shipped under `WITCHY_OPT=rc-floor`, off by default, `./scripts/check.sh --fast` green
(1078 tests, clippy `-D warnings`):

- **`last_use` lattice** (step 1): `DropFacts`/`place_drops` in `analysis.rs` — the two
  airtight drop cases (a dead binding; a single use as a *non-leaking* call arg, via
  `Summaries::arg_leaks`), each carrying the free offset from
  `fresh_heap_builtin_offset` so codegen frees the exact `$rc_alloc` region start (0 for
  list/string, 4 for a dict's hidden-index word). 12 standalone tests against
  hand-checked drop points; the soundness guards (same-block binding, region-confined,
  not-escaped/returned/reassigned/captured/parameter) are each pinned by a test.
- **Codegen consumption** (elision half of step 2): `begin_unit` computes `DropFacts`
  into a stack parallel to `facts_stack`; `lower_block` emits `$rc_free(local - offset)`
  after each drop-statement. This is the **RC-elision** free — a statically
  unique-and-dead value returned to the size-classed free list directly, no refcount
  word consulted (RFC-0016 R2). Composes with the already-shipped free-at-overwrite rule.
- **The heap-reset boundary** (a real double-reclaim bug, found + fixed): a watermarked
  loop body (RFC-0030) or `region:` block resets `$heap` per iteration, already
  reclaiming every iteration-local value. An `$rc_free` there is a *double reclaim* — the
  freed block lands on the free list, the watermark rewinds `$heap` below it, and the
  next bump re-hands-out the same address still linked in the free list (a dangling
  free-list chain → out-of-bounds trap, caught in testing). rc-floor now fires only where
  `wm_level == 0`: straight-line code and loops whose body is NOT arena-resettable
  (something escapes, so the watermark is off) — exactly the niche the watermark cannot
  reach.
- **Differential proof**: `rc_floor_last_use_drop_is_differential_and_bounds_the_leak`. A
  dead per-iteration scratch buffer in a non-arena-resettable loop (a dict escapes) leaks
  352 KB by default; under rc-floor the output is identical to the interpreter oracle AND
  the default build, the heap frontier stays flat at 382 bytes, and `__rc_reused_bytes`
  proves the free list recycled every buffer.
- **`cache_eviction`** (residual 1) is already bounded by the shipped free-at-overwrite
  rule (`stats::cache_eviction_bounded_by_rc_floor`, green) — one of the two pins clears
  without the refcount word.

### Remaining: the per-object refcount word (step 2 header + step 3 executor)

The **`chan_throughput`/executor residual is NOT closed**, and it does not yield to
`last_use`. The scheduler's `list.set_at(slots, i, Active(cont(unit())))` displaces a
`Task` element that was read into a binding *in the caller* (`step_one`); freeing it
requires the runtime refcount to answer, at the `set_at`, whether a live alias still
holds the displaced element (count > 1 → keep; 0 → free). This is inherently
all-or-nothing for soundness: a `set_at` that frees the displaced element WITHOUT the
full `$dup`/`$drop` discipline would use-after-free whenever a live alias holds it — the
one class witchy refuses. So the header + `$dup`/`$drop` land *together*, verified
against the adversarial corpus + the `chan_throughput` soak, or not at all.

Header-layout note for that build: `$rc_alloc`'s `[size]` word sits at the returned
pointer − 4, and dicts already use − 4 for their hidden index word, so the refcount needs
a *second* header word — returned pointer − 4 for rc, − 8 for size — a change localized to
`rc_alloc` / `rc_free` / the free-list reuse scan (the per-primitive allocators call
`rc_alloc` and are unaffected). Not started: per the HARD RULE (never commit an unproven
`dec`) it is gated behind a dedicated, fully-verified build, not an autonomous increment.
