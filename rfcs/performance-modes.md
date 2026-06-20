---
status: implemented
note: Imported from docs/ under RFC-0001. Frozen design record — current behavior lives in spec/ and the code.
---

# Performance modes & the unified optimization analysis

witchy gives the programmer **optional** performance knobs — ownership
conventions (`own`/`move`/`let`-borrow), `region:` scopes, and an in-place
machinery that fires automatically when the analysis can prove it safe. Most of
the time you ignore them and your code is still correct, just occasionally
slower than it could be.

A **performance mode** is a per-file opt-in that flips that contract: inside the
file the knobs become *mandatory and enforced*, every silent de-optimization
becomes a loud error, and — over time — the file gains access to optimizations
that are unsound without the guarantees the mode demands.

```witchy
mode opt          // the whole file is held to the fast-path discipline

fn squares(let xs: List(Int)) -> List(Int):
    var out = []
    for x in xs:
        out = list.push(out, x * x)     // in-place push — provably, or it won't compile
    out

fn main(console: Console):
    print(console, __render(list.length(squares([1, 2, 3]))))
```

This document records the design. It builds on
[ownership-analysis.md](ownership-analysis.md) (the uniqueness pass),
[regions.md](regions.md), and [performance.md](performance.md).

## The one substrate

Everything here is one analysis, not a pile of features. Per program point and
binding, the uniqueness pass tracks a lattice — `Unique | Shared | Dead` —
backed by a runtime self-healing `__cap` token (live = owned slack; zero = the
next self-assign copies and thereby re-owns). Every "knob" is a way the
programmer *raises the precision or the guarantee* of that one analysis at a
point, and every call site is a *transfer function* over the lattice.

A performance mode is the **same analysis with the severity dial turned to
"error"**: where normal code silently de-optimizes on a missing fact, a mode
file refuses to compile and names the fix.

## The three buckets of knobs

| Bucket | Where | Status |
|---|---|---|
| **1. Optional everywhere** — `own`/`move`/`let`-borrow, `region:`, the automatic in-place machinery. Ignore them and code is still correct, just slower. | normal + mode | SHIPPED |
| **2. The same knobs, mandatory + enforced** — the cliff *notes* become *errors*; the inferences become *required*. "Every accumulator provably in-place." | mode only | SHIPPED (cliff promotion) |
| **3. Mode-only unlocks** — optimizations that need a whole-file invariant the mode guarantees: unboxed layouts, guaranteed destination-passing/in-place reuse, returnable slices, eager free, `unique` as a usable type. | mode only | PROPOSED |

Bucket 3 is the payoff and the reason a mode is worth having. An optimization is
mode-gated precisely when it needs an invariant the compiler can only assume if
the whole file promised it — normal mode must stay fully general, and each
generality is exactly what makes the corresponding optimization unsound. Drop
the generality, the optimization becomes sound:

| Mode-only knob | Generality given up | Why it's then feasible |
|---|---|---|
| Inline/unboxed layouts (`List(Point)` packed, not pointers) | the uniform 8-byte slot + dynamic handling | layout is statically known per type → base+offset, cache-dense, SIMD |
| Guaranteed in-place reuse / destination-passing | freedom to alias/share the value | uniqueness becomes a *checked contract* → reuse is always sound |
| Returnable slices/views (zero-copy `substring`) | freedom for a borrow to escape | no-escape is mandatory → a view can't outlive its buffer |
| Eager free at last use | waiting for the arena/region watermark | unique + non-escape ⇒ the buffer is dead at last use |
| `unique`/`unshared` as a real type qualifier | nothing — it is only *meaningful* under enforcement | the qualifier *is* the enforcement |

The pattern is well-precedented: C's `restrict` ("I promise no aliasing → now
optimize"), Swift's `@frozen` ("I promise not to add cases → bake in the
layout"), Rust `unsafe` inverted (give up checks → gain operations). A witchy
mode is `restrict` for a whole file, plus the knobs that promise buys.

## The `mode` annotation — SHIPPED

A file opts in with a contextual `mode` directive at the very top, before any
imports or items:

```text
mode opt           // one directive per file, at the very top
```

`mode` is a *contextual* keyword — it is recognized only as a leading directive,
so `mode` stays usable as an ordinary identifier elsewhere. There is one mode:

- **`opt`** — the performance discipline, made mandatory and enforced. Three
  things change versus an ordinary file:
  1. Every accumulation the uniqueness pass would silently revert to the O(n²)
     copying path (a `Cliff`) is a **hard compile error** instead of a check-time
     note. Keep accumulators on the in-place path (use `own`/`let`-borrow to
     certify helper calls don't alias the buffer out, avoid sharing the
     accumulator mid-loop).
  2. Every ownership-relevant parameter — a heap-typed value (List, String, Dict,
     record, …) — must declare an explicit convention: `let` (borrow), `var`
     (mutate + write back), or `own` (consume). Scalars and capabilities are
     exempt.
  3. The mode is **transitive**: an `opt` module may only import other `opt`
     modules. The bundled standard library is the compiler's optimized substrate
     and is exempt, but any *user* module an `opt` module imports must itself be
     `opt` — so the discipline is a guarantee over the whole reachable graph, not
     just one file. Importing a non-`opt` user module is a link error.

Within-file enforcement (cliffs, conventions) is judged on the declaring file's
own functions; the transitive import rule is checked at link time across the
module graph. The error carries the `why_not` provenance the uniqueness pass
already computes, so the fix is actionable.

## Across call sites, and the normal↔opt boundary

The interprocedural summaries (`Summaries::of_module` in `src/analysis.rs`) are
how the knobs compose across calls. Per function per parameter, `may_alias_out`
answers "can a call leave a live whole-alias of this argument?", and `arg_live`
gives three call outcomes:

- **Transparent** — a `let`-borrow param (no-escape, typeck-certified) or an
  `own` param (caller's binding dies on the move): the call does not
  touch the caller's uniqueness. *This is what makes helper calls non-fatal to
  an accumulation chain.*
- **Transport** — an own-ABI callee with `move`: the `__cap` token flows through
  the call, so `x = f(move x)` is O(1) amortized end-to-end.
- **Kill** — the param genuinely leaks, or the callee is unknown: the value
  drops to `Shared`; its next self-assign re-owns (one copy).

Summaries are *context-insensitive* (one per function), so annotating a hot
stdlib helper `let`/`own` helps every call site at once.

**The boundary between a normal file and a `mode` file is just a call site** —
not a special protocol (PROPOSED for the bucket-3 representation work):

- Because the linker flattens everything into one whole-program module before
  analysis, a mode callee is never the "unknown callee" pessimistic case — its
  summary is computed across the seam like any other.
- The marshal at the seam is two separable costs. The **re-own copy** is
  `arg_live` applied at the seam: a value passed `move` while `Unique` is the
  Transport case → zero copy; a genuinely `Shared` value re-owns once (real work
  the normal code already owed). The **representation reshape** is orthogonal:
  scalar-shaped data (`List(Int)`, strings) already shares layout → zero;
  boxed aggregates reshape unless normal mode had already chosen the unboxed
  layout under proven ownership.
- **Monomorphization is the bridge**: a generic stdlib function called from a
  mode site gets a mode specialization (unboxed + must-stay-`Unique`); the
  normal call gets the boxed, best-effort one. Same source, two specializations,
  picked per call site — so most crossings never reshape.

The upshot: the marshal is not an intrinsic boundary tax; it is the cost of
de-optimization, pushed to exactly the points where the value was not already
optimized. "Optimize the marshal away" reduces to "raise the value to `Unique`
upstream with the `own`/`move` that already exist."

## The representation tiers toward near-Rust — PROPOSED

witchy's compiled value model is uniform 8-byte slots with **boxed aggregates**
(a record/tuple/`List(Point)` element is an i32 pointer to a separate heap
object). That uniform-representation choice — simple codegen, one
equality/format/copy story — is the dominant gap to Rust, whose speed is mostly
*data layout*. Ranked by how far each moves toward "near Rust":

1. **Monomorphized *layouts* (unboxing).** Make monomorphization choose a
   representation, not just specialize code: `List(Point)` becomes a packed
   `[len][cap][x0,y0,…]`, access is base+offset, cache-dense, SIMD-eligible.
   The only lever that changes cache asymptotics; the most invasive (touches
   every host fn that reads guest memory, plus the eq/copy/format machinery).
2. **Escape-driven stack allocation / SROA.** Non-escaping records/tuples live
   in WASM locals, never the heap. The escape facts already exist (the
   lambda-capture scan + the uniqueness pass); this is the analog of Rust stack
   allocation. Medium effort, low risk.
3. **`unique`/`unshared` as a surface type.** Promote affine ownership from the
   binding-side `consumed` set (typeck) to a type qualifier with a one-way
   coercion (`unique → shared`). Unlocks guaranteed in-place reuse and
   destination-passing.
4. **Returnable slices/views + lifetimes.** The one item needing genuinely more
   analytical power than the current binary no-escape check (constrained escape
   = lifetime/region inference). Cleanly separable; mode-gate or defer.
5. **Reuse / FBIP (Perceus, Koka/Lean 4), COW, small-string optimization.** A
   different identity (reference-counted reuse) that reaches near-C on
   functional code but costs a refcount word per heap object — mutually
   exclusive with the pure arena/linear bundle. Pick one identity, not both.

## The unified escape/fact analysis framework — PROPOSED

The long-term home for all of the above. Today "does this value escape this
scope?" is computed in several disconnected places, in different
representations:

1. uniqueness-pass **share events** (storing into a structure escapes it)
2. **`loop_body_escape_free`** (escape-free loop → watermark reset)
3. **`borrow_escape_check`** (a `let` borrow may not be returned) — typeck
4. the **region outer-assignment rule** — typeck
5. the **lambda-capture escape scan**
6. region copy-out's **watermark short-circuit** (`ptr < wm` ⇒ parent-side)

This is the same fragmentation `ownership-analysis.md` already consolidated once
for uniqueness (three whitelists + `expr_mentions` → one pass). The next
consolidation is **one escape/region lattice** answering "what is the smallest
scope this value is confined to?", with every reclamation and in-place consumer
querying it.

The clean shape is abstract interpretation: separate **analysis** (lattices that
monotonically "learn more": types, uniqueness, escape, liveness, value/range,
effects/capabilities) from **transformation** (consumers that read facts and
rewrite). One `Facts`-style oracle, optimizations as pure consumers — adding an
optimization becomes "add a consumer," adding power becomes "add/sharpen a
lattice," and the two decouple. witchy is unusually suited to it: whole-program
(post-link), value semantics (tractable alias/escape lattices), capability
purity (effects already a lattice).

Two async-era consumers motivate it concretely (see
[performance.md](performance.md)): **await-capture escape** (every value live
across an `await` is captured into a continuation closure = a share event the
pass conservatively copies) and **per-task reclamation** (the `std/task`
executor has no per-task arena reset; a sound per-step reset is an escape
query). A `scope:` construct, if it lands, is the same escape question with the
scope as the lifetime bound.

### What this needs (and doesn't)

- **No new inference engine for most of it.** The whole-program interprocedural
  backbone, affine enforcement, escape facts, and monomorphization already
  exist.
- **Precision, not power, is the real cost.** The current uniqueness analysis is
  deliberately weak because the self-healing token absorbs imprecision (a miss
  costs one copy). A mode that *errors* removes that safety net, so the static
  analysis must become precise enough to stand alone — likely path- and
  field-sensitivity. A real CFG/SSA is the natural vehicle (there is none today;
  dataflow is structural over the AST), though not strictly required.
- **One genuine power increase**, isolated: lifetimes for returnable slices
  (tier 4). Deferrable.
- **Ordering constraint** (already satisfied): the uniqueness pass must run
  after async lowering, so it sees continuation-closure captures as shares.

## Status & staged plan

- **SHIPPED** — buckets 1–2: the `mode opt` annotation (with transitive imports), cliff
  promotion (silent O(n²) revert → hard error in a mode file), and the
  mandatory-ownership-convention rule (ownership-relevant parameters must be
  `let`/`own`/`var`, so the summaries are declared facts, not inferences).
- **SHIPPED** — the AST optimizer (`src/optimize.rs`): semantics-preserving
  constant folding (wrapping-int / IEEE-float arithmetic, comparisons, boolean
  ops, bitwise, string-literal concatenation, unary literals) **plus local
  constant propagation** (immutable `let`-bound literals are substituted into
  later uses, feeding the folder), run on the single linked module both backends
  consume. String-concat folding through `let` bindings eliminates real runtime
  allocations the WASM backend's Cranelift mid-end cannot see.
- **SHIPPED** — AOT module serialization (`src/runtime.rs`): a warm `witchy
  sandbox` loads a precompiled wasmtime artifact instead of recompiling (Phase 3
  of [performance.md](performance.md)).
- **NEXT** — consolidate the six escape computations into one escape/region
  lattice + `Facts` query API (the lowest-risk, code-deleting first step).
- **THEN** — escape + uniqueness on a shared CFG/SSA (this is where the
  mode-precision lands); the `unique` surface qualifier (tier 3).
- **LATER** — unboxed layouts (tier 1) and SROA (tier 2) as the bucket-3
  unlocks; returnable slices/lifetimes (tier 4) mode-gated.

## Non-goals

- No user-facing borrow checker with lifetimes outside `mode` files — the
  conventions stay the only surface syntax in normal code; lifetimes (if added)
  are confined to mode files so the value-semantics, no-`Pin` ergonomics of
  normal witchy are untouched.
- No semantic change from any mode — a mode only changes *which implementation
  runs* and *whether a de-opt is an error*, never observable behavior, exactly
  like the existing in-place machinery (enforced by the forced-copy differential
  mode).
- Pick one memory identity (arena/linear *or* reuse-counting/FBIP), not both.
