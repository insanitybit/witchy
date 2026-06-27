---
rfc: 0016
title: Reference-counted memory — RC floor, arena/region/in-place as elision
status: proposed
created: 2026-06-26
superseded-by:
tracking:
---

# RFC-0016: Reference-counted memory — RC floor, arena/region/in-place as elision

## Summary

Make **precise reference counting with reuse (Perceus/FBIP)** the default
reclamation model on the WASM tier. It is *complete* — leak-free with **no cycle
collector** — because witchy's value semantics force the heap to be **acyclic**.
Today's bump allocator, loop watermarks, `region:` blocks, and the `__cap`
in-place token are not a rival memory identity; they are reframed as a single
**RC-elision ladder** sitting on top of one analysis oracle. Each rung *removes*
inc/dec/free traffic the oracle can prove redundant, so the fast paths witchy has
today are recovered exactly, on top of a floor that is finally bounded. This
resolves the open non-goal in [`performance-modes.md`](performance-modes.md)
("pick one memory identity … not both") **for RC, with the arena demoted to an
optimization of it.** Tracing GC is rejected with cause.

The whole design rests on one invariant: **precision is speed, never
correctness.** The RC floor is always sound; every optimization is a pure
consumer of a facts oracle that only ever *deletes* count operations it has
proven dead. A missed fact costs one refcount op; it can never free live data.

## Motivation

The compiled backend is a monotonic bump allocator (`$heap += size` gated by
`ensure_helper`, `src/wir.rs:495`); the only reclamation is the LIFO watermark in
`region:` blocks and loop bodies. A value that escapes every watermark — a
server cache, a session map, a per-request response returned up the stack — is
**never freed**. A `std/server` loop climbs the 1 GiB `StoreLimits` ramp
(`RUN_MEMORY_PAGES = 16384`, `src/main.rs`) and traps. That is the OOM blocking
witchy's headline workload.

The choice is forced by **asymmetric failure**: the arena fails toward *unbounded
growth* (a footgun needing `region:` to defuse); RC fails toward *count-traffic
overhead* (a throughput cost, recoverable by elision). A default must fail toward
"slower," not "OOM."

And witchy is already ~70% of the way there. The `__cap` token
(`list_push_cap_helper`, `src/wir.rs:1069`) is a statically-special-cased "is
this buffer rc==1?" test; the uniqueness pass (`src/analysis.rs`) is already a
borrow/sharing analysis with interprocedural summaries. This RFC generalizes that
{0,1}-at-mutation-sites count into a real per-object word with `dec`-at-last-use,
and unifies every existing and proposed optimization as elision of it.

---

## Design — Part I: the one substrate that enables every optimization

Every optimization below is a **consumer of a single facts oracle**, computed
once over the whole-program module (post-link, post-async-lowering), separating
*analysis* (lattices that monotonically learn) from *transformation* (consumers
that rewrite). This is the "NEXT" consolidation `performance-modes.md` already
calls for; RC is its forcing function and first big client.

For each program point `p` and each heap-typed value `v`, the oracle answers four
queries. **They are not four passes — they are four projections of one
abstract-interpretation fixpoint** over the lowered AST:

| Query | Lattice | What it enables |
|---|---|---|
| `last_use(v, p)` | liveness (backward) | where `$drop v` is emitted |
| `uniq(v, p)` | `Unique ⊐ Shared ⊐ ⊥` | elide the runtime `rc==1` check; enable reuse |
| `confine(v)` | smallest scope `S` containing every reference | region bulk-free; stack allocation |
| `borrow(param)` | `Owned ⊐ Borrowed` | a param that only reads + never escapes ⇒ no dup/drop |

The **soundness floor is RC**: at any point the oracle returns ⊥ (no fact), the
transformer emits the full count operation. Each *positive* fact licenses a
*deletion*. So the oracle can be arbitrarily imprecise and the program stays
correct — it is just slower. This is the exact "self-healing token absorbs
imprecision" property the uniqueness pass already relies on, generalized from
"one extra copy" to "one extra inc/dec." It is why we can ship the floor first and
sharpen the lattice forever after without ever risking a use-after-free.

`mode opt` is the same oracle with the severity dial turned to **error**: where a
normal file silently keeps a count it couldn't prove dead, a mode file refuses to
compile and names the missing fact. Nothing about the analysis changes between
normal and opt — only whether an un-elided op is *tolerated* or *rejected*.

---

## Design — Part II: the RC floor (R0) — how dup/drop/reuse actually work

### Representation

Every heap record gains a **leading `i32` refcount stored at `p-4`**; the pointer
returned by the allocator still points at the tag, so **every existing reader
(`list_at`, `eq_<shape>`, `ts_<shape>` format, `rcopy_<shape>`, and the host
functions in `src/runtime.rs` that read guest memory) is byte-for-byte
unchanged** — only the allocator and the three new helpers touch `p-4`.
(Negative-offset header words already exist: the dict allocator keeps a hidden
word at `p-4`.) Concrete layouts:

```
list:    p-4:[rc] | p:[len][cap][ slot×cap ]      (cap promoted from the shadow
string:  p-4:[rc] | p:[len][ utf8… ]               local into the header so the
record:  p-4:[rc] | p:[tag][ field×n ]             runtime rc==1 reuse path is
dict:    p-4:[rc] | p:[count][ (k,v)×… ]           self-contained — see R2)
```

Scalars (Int/Float/Bool/Duration) live inline in the universal 8-byte slot and
are **never counted**; capability handles are i32 host-table indices and are
**never counted** (authority lives host-side). RC touches pointers only.

### The three helpers

```
$dup(p)   ::  if heap(p): rc[p-4] += 1
$drop(p)  ::  if heap(p): if (rc[p-4] -= 1) == 0 { $rdrop_<shape>(p); $free(p) }
$free(p)  ::  push p onto freelist[size_class(p)]      // no compaction, no relocation
```

`$rdrop_<shape>(p)` is a per-type recursive destructor that `$drop`s each
pointer-typed field, generated by the same shape machinery that already emits
`$rcopy_<shape>`/`$eq_<shape>`. Acyclicity guarantees termination with no mark,
no cycle detection.

### Insertion (the Perceus discipline)

Codegen threads each value through **owned** or **borrowed** contexts and inserts
counts by three rules over the liveness lattice:

1. **Duplicated use** — a value used at `p` that the oracle says is live again
   after `p` (`¬last_use`) is `$dup`'d before the use, so each consumer gets its
   own logical owner.
2. **Last use of an owned value** — `$drop` at exactly `last_use(v,p)`, *not* at
   scope exit. This is what makes RC "garbage free": only live data is retained.
3. **Borrowed value** — never `$dup`/`$drop`'d by the callee; the caller owns it
   (this is R1, and it is *most* of the traffic — see Part IV).

### Reuse (FBIP) — the generalization of the `__cap` token

When `$drop(p)` would bring a cell to `rc==0` and the **same control path next
constructs a cell of the same size class**, the two collapse into an **in-place
overwrite**. Perceus models this with a *reuse token*: `$drop` becomes `$reset`
(returns the cell address if rc hit 0, else null), and the downstream constructor
becomes `$reuse(token){…}` (writes the new fields into the old cell when the token
is non-null, else allocates fresh).

```
// xs : List, last use here, then we build a same-shape list
xs2 = list.push(xs, e)

// floor with reuse:
ru   = $reset(xs)               // rc==1 ⇒ ru = xs; rc>1 ⇒ ru = null (and xs survives)
xs2  = $reuse(ru){ [len+1][cap][ …xs…, e ] }   // ru≠null ⇒ mutate in place; else alloc
```

This is **exactly** what `list_push_cap_helper` does today (in-place when the
spare-capacity token is live, copy-and-re-own when zero) — but derived from the
general mechanism, so it now fires for **every same-shape destruct/construct
pair**: `list.map` over a unique list, `dict` upsert, a tree-node rebalance — not
just the four hand-recognized accumulator shapes. **The hot path gets wider, not
narrower.** `__witchy_reowns` becomes "places `$reset` returned null (rc>1) and we
allocated" — the same O(1)-vs-O(n) oracle, now general.

### Allocation and the free list

`$mk{n}` and the growable allocators **pop the size-class free list before
bumping `$heap`**; `$free` pushes back. `$heap` becomes a high-water mark.
**Bump survives as the empty-list fast path** (one compare-and-bump), so the
common monotone-allocation phase is unchanged, and regions (R4) bypass the free
list entirely. Allocation stays **O(1)**.

### Worked result

The `std/server` request loop now `$drop`s each request's response subgraph at the
loop back-edge (its last use), freeing it into the list, which the next iteration
reuses — **constant live memory, no `region:`, no pause, zero programmer
annotation.** That is the floor's whole job.

---

## Design — Part III: the elision ladder — every optimization, enabled + emitted + why fast

Each rung names its **enabling fact** (the oracle query), the **pass** that
proves it, the **emitted transformation**, and **why it performs well**. The
ladder invariant: *an op is elided only with a positive proof; absence of proof
keeps the op.*

### R1 — Borrow elision (`let`) — the answer to "RC is slow"

- **Enabling fact:** `borrow(param) = Borrowed` — the parameter is only read and
  provably never escapes (returned, stored, captured, mutated).
- **Pass:** typeck `borrow_escape_check` certifies no-escape today; **borrow
  inference** (Counting Immutable Beans) makes it the *default* for params that
  demonstrably only read, via the existing `param_flows_out` summary — so the
  programmer rarely writes `let` to get it.
- **Emitted:** a borrowed argument is passed without `$dup`; the callee emits no
  `$drop`. A read-only traversal does **zero** count traffic.

```
fn contains(let xs: List(Int), x: Int) -> Bool:     // xs borrowed
    for h in xs:                                     // h read, not retained → no dup
        if h == x: return true
    false
// caller: contains(xs, 9)   — no $dup of xs, no $drop in callee. Net RC ops: 0.
```

- **Why it works well:** Beans' entire thesis is that borrowed references keep RC
  traffic low; witchy's `let` *is* the borrowed reference, and witchy already
  enforces its no-escape contract. The dominant call shape — passing a collection
  to a function that reads it — pays nothing. This is the rung that makes the
  RC-floor cost objection evaporate in practice.

### R2 — Reuse / in-place (`rc==1`) — recovering today's mutation speed

- **Enabling fact:** `uniq(v,p) = Unique` (statically rc==1) or, failing that, a
  runtime `rc==1` read.
- **Pass:** the uniqueness pass's `unique_at` gives the static answer; the header
  `rc` gives the dynamic one.
- **Emitted:** `$reset`/`$reuse` as in Part II. When `Unique` is *static*, the
  runtime `rc==1` check is **elided** and the cell is reused unconditionally —
  identical machine code to today's cap-token in-place push. When only dynamic,
  one branch on `rc`.

```
mode opt
fn build(n: Int) -> List(Int):
    var xs = []                 # xs is Unique throughout (never shared)
    for i in range(n):
        xs = list.push(xs, i)   # static rc==1 → in-place, no $reset check, no alloc
    xs                          # O(n) total, zero re-owns
```

- **Why it works well:** the cap token already proves this is competitive
  (4–5.7× Go on string building). Under RC the *same* code runs on the hot path;
  the generalization only *adds* in-place firing for map/filter/tree shapes that
  today allocate. Worst case (a genuinely shared value) is one branch + one
  copy — which is the correct value-semantics behavior anyway.

### R3 — Ownership transfer (`own` / `move`) — threading the count through calls

- **Enabling fact:** `own` param consumes the argument; `may_alias_out` says
  whether the callee hands it back.
- **Pass:** the affine `consumed` check (reuse-after-`move` is already a compile
  error) + the existing `own_abi` summary that today threads the `__cap` token.
- **Emitted:** at `x = f(move x)`, **no `$dup`/`$drop` round-trip at the seam** —
  ownership (and the rc) flows into the callee, reuse fires *inside* it, and the
  count flows back out via the own-ABI's trailing result. FBIP across function
  boundaries.

```
fn push_twice(own xs: List(Int), a: Int, b: Int) -> List(Int):
    xs = list.push(xs, a)       # reuse inside callee (xs is owned ⇒ Unique here)
    list.push(xs, b)
xs = push_twice(move xs, 1, 2)  # no dup/drop across the call; count threads through
```

- **Why it works well:** a multi-function builder pipeline stays O(n) end-to-end
  instead of re-owning per call — the own-ABI already delivers exactly this for
  the cap token; RC inherits it unchanged.

### R4 — Region confinement (`region:`) — RC turned *off* for a proven arena

- **Enabling fact:** `confine(v) ⊆ this region` — every reference to `v` dies
  inside the block (the region outer-assignment rule + the escape lattice).
- **Pass:** typeck's region rule (shipped) + the unified escape lattice.
- **Emitted:** inside a confined region the compiler emits **no `$dup`/`$drop` and
  allocates from the region bump pointer** (no header maintenance); at exit it
  **bulk-resets the watermark**. The single escaping value is `$dup`'d (rc set to
  1) into the parent heap at the existing `$rcopy` copy-out boundary.

```
total = region:                      # everything inside is confined ⇒ zero RC
    var acc = 0
    for line in split(big, "\n"):    # transient splits: no rc, bulk-freed at exit
        acc = acc + parse_int(line)
    acc                              # scalar escapes; nothing to copy out
```

- **Why it works well:** this is the *strongest* elision — Tofte–Talpin regions
  are literally "RC elided where confinement is proven," with the cache locality
  Green Tea shows tracing fights to recover. The region's known failure mode
  (long-lived values leak until the region pops) **cannot bite**, because a value
  that escapes falls to the RC floor and is freed at its true last use. **The
  escape lattice partitions every allocation into confined (no RC) xor escaping
  (RC), so a cell is never double-managed.**

### R5 — Escape-to-stack / SROA — no heap object at all

- **Enabling fact:** `confine(v) ⊆ this function frame` and `v` is a fixed-shape
  record/tuple.
- **Pass:** the escape lattice (the lambda-capture scan + uniqueness already
  compute the inputs).
- **Emitted:** the record is **scalar-replaced into WASM locals** — no
  allocation, **no header, no RC word, no dup/drop at all**. The strongest
  possible outcome: the object never exists on the heap.

```
fn dist(ax: Float, ay: Float, bx: Float, by: Float) -> Float:
    let d = Point(bx - ax, by - ay)    # never escapes → d.x, d.y live in locals
    sqrt(d.x * d.x + d.y * d.y)         # zero heap traffic
```

- **Why it works well:** this is the one lever that changes cache asymptotics for
  short-lived aggregates, and it removes RC entirely for the value — the cheapest
  rung of all. Medium effort, low risk; the escape facts already exist.

### R6 — `mode opt` unlocks — guaranteed, unboxed, header-free

`mode opt` makes R2–R5 **mandatory and checked** (a de-opt is a hard error with
`why_not` provenance), and unlocks optimizations that need a whole-file invariant:

- **Unboxed monomorphized layouts.** `List(Point)` becomes one packed buffer
  `p-4:[rc] | [len][cap][x0,y0,x1,y1,…]` — **one rc for the whole buffer, not per
  element** — base+offset access, cache-dense, SIMD-eligible. Enabled because the
  mode guarantees the element type's representation is statically known.
- **Header elision.** A type the whole-program graph proves is **never shared**
  (a `unique` value) drops the rc word entirely — zero space, zero count.
- **`unique` / `unshared` type qualifier.** Promotes `uniq=Unique` from inference
  to a *checked contract* with a one-way `unique → shared` coercion. The
  qualifier *is* the enforcement: it makes static FIP sound, so reuse is
  guaranteed rather than best-effort.
- **Static FIP (FP²).** A `mode opt` function with `unique` params is **provably
  in-place and zero-allocation** on its hot paths; a violation is a compile error,
  not a silent copy. This is the FP² guarantee made checkable — near-C functional
  code.
- **Destination-passing / returnable slices.** Deferred (returnable slices need
  real lifetime inference); mode-gated when they land.

```
mode opt
fn reverse(unique xs: List(Int)) -> List(Int):   # guaranteed in-place, 0 alloc
    var ys = []
    for x in xs: ys = list.push(ys, x)            # FIP: each cons reuses a destructed cell
    ys
```

- **Why it works well:** the mode is `restrict` for a whole file — give up the
  generality (arbitrary sharing) and the corresponding optimization (in-place
  reuse, flat layout) becomes sound. You opt into a guarantee, and the compiler
  holds you to it.

---

## Design — Part IV: the cost model — why the whole thing performs well

| Operation | Cost | Notes |
|---|---|---|
| `$dup` | load rc, +1, store | no branch |
| `$drop` (rc>1) | load rc, −1, store, branch | falls through |
| `$drop` (rc→0) | + `$rdrop_<shape>` + free-list push | the reclaim, hot in cache at last use |
| `$free` / alloc | O(1) free-list pop or bump | locality preserved |
| borrowed param (R1) | **0** | the dominant call shape |
| reuse (R2/R3) | in-place overwrite | replaces alloc+free |
| region body (R4) | **0** per object + bulk reset | confined scratch |
| stack/SROA (R5) | **0** | no heap object |

The standard objection to RC is dec/inc traffic. The answer is structural, not
hopeful:

- **Read-only traversals pay nothing** (R1) — and they are most calls.
- **Unique mutation pays an in-place overwrite** (R2/R3) — identical to today's
  cap token; the benches it already wins stay won.
- **Confined scratch pays nothing per object** (R4/R5).
- The **only net-new cost** is dup/drop on *escaping, shared, long-lived* data —
  which is **exactly the data that leaks today**, so RC is a strict improvement
  there (bounded memory for a small count cost), never a regression.

Compared to tracing: there is **no mark phase** (Green Tea: ~90% of GC cost, ≥35%
memory-stalled), **no pause**, **no relocation barrier**, and reclamation happens
at last use *when the data is hot in cache*, not in a later graph-flood. Compared
to today's bump-only: identical on the hot paths (R1–R5 recover it), bounded
where today it OOMs.

---

## Design — Part V: parity and verification

Reclamation **may differ between the backends because it is unobservable.** The
interpreter keeps Rust `Drop` on its deep-clone `Value` enum — no rc word, no
Perceus pass. The WASM tier runs the floor + ladder. They agree on every *value*
and *error*; not on the byte at which a cell is reclaimed. What licenses this:
witchy has no finalizers, no `weak`/identity comparison, no reference equality, no
in-language "is this freed / how much memory" query (no `unsafe`, no raw
pointers). The counters below are *exported test hooks*, not language-visible.

Three orthogonal forced-degradation switches, each proving one optimization is
value-neutral; the optimized build must be byte-identical to each:

- `WITCHY_NO_RC_ELISION=1` — full RC, zero elision (the primary elision-soundness
  oracle; analog of `WITCHY_NO_INPLACE`).
- `WITCHY_NO_INPLACE=1` / `WITCHY_NO_REUSE=1` — never reuse, always fresh-allocate
  (existing, retained).
- `WITCHY_NO_FREE=1` — RC ops become no-ops, memory grows, values identical
  (isolates drop-*placement* bugs from elision bugs).

Plus two always-on sanitizers and a suite:

- **Leak oracle:** `__witchy_live_cells` (exported) must read **0** at program
  exit; non-zero ⇒ a missing `$drop`.
- **Double-free trap:** any `rc` decrement below zero traps.
- **Adversarial boundary suite:** aliases dropped around loops, `move` chains,
  closures capturing accumulators, channel sends, and region/RC boundary
  crossings — asserting output (`interp == wasm`) and behavior
  (`__witchy_rc_frees == allocations` at exit).
- Property test: `interp == wasm == wasm-no-elision == wasm-no-reuse ==
  wasm-no-free`.

A wrong elision is a use-after-free or leak — the one class witchy refuses — so it
gets the same three-layer rigor the uniqueness pass already demands.

---

## Design — Part VI: memory as a capability (quota / DoS)

`maximum: None` today means no in-language ceiling. Promote it to a first-class
attenuable **`Memory` capability**:

- A `Memory` handle carries a byte budget (host-side, unforgeable,
  un-widenable); the footprint analyzer (`src/capabilities.rs`) reports it like
  any right, so `coven`'s block-on-widening gate can refuse a package that raises
  its ceiling.
- **Attenuation:** `mem.cap(64.mb)` returns a narrower handle (monotone, like
  `dir.subtree` / `net.only`).
- **Composition with R4:** `region with mem.cap(1.mb):` is a bounded arena —
  watermark gives O(1) bulk free, quota gives a peak ceiling; a request that
  blows its budget fails *that request* (catchable trap, watermark resets), not
  the server.

RC is what makes the bound meaningful: a quota on a monotonically-growing arena is
a delayed trap; a quota on a live working set is a real resource contract.

---

## Design — Part VII: concurrency

One VM, one linear memory, cooperative pure-witchy executor.

- **Non-atomic RC** — yields happen at `await`/back-edges, never mid-`$dup`, so
  the count is a plain `i32`. A major win a threaded runtime could not have.
- **Await-capture becomes a `$dup`, not a copy** — a value live across `await` is
  captured into the continuation closure (a share event the pass *conservatively
  copies* today); under RC it is one count bump, dropped when the continuation
  resumes and consumes it. Strictly cheaper. The existing ordering constraint
  (uniqueness pass runs after async lowering, so it sees continuation captures as
  shares) is exactly what the RC-insertion consumer needs.
- **Per-task reclamation falls out** — a task's transients free at last use within
  the shared heap; a per-task `region`/sub-arena remains an optional throughput
  add-on ("a task is a generation," bulk-free at join). Each `spawn` may carry a
  `Memory` sub-budget.
- **Channel send** is `$dup` if the sender retains, else an ownership transfer
  (no dup/copy) when the sender's binding is dead — the own-ABI at the send site.
- *Future OS-thread parallelism* → per-thread heaps + transfer-on-send (Erlang
  model), preserving non-atomic RC. Out of scope.

---

## Design — Part VIII: staged migration

De-risk first, then the floor, then elision, then unlocks. Each phase keeps both
backends green and parity intact.

- **Phase 0 — instrumentation + free list, NO RC.** Build the leak/double-free
  harness and exported counters as no-ops against today's arena (baseline: zero
  frees). Add the size-classed free list and **route region/loop-watermark frees
  into it** so later non-region allocation reuses scoped garbage. Add the rc word
  at `p-4`, initialized to 1 but never decremented. *Exit:* byte-identical to
  today; mixed-allocation `$heap` bounded. *Honest scope:* this reuses *scoped*
  garbage and de-fragments — it does **not** free the escaping values that cause
  the headline OOM; that needs Phase 1.
- **Phase 1 — the RC floor goes live.** Insert precise `$dup`/`$drop` per Perceus
  last-use; `$drop`→0 frees to the list. **Full counting, zero elision.** *Exit:*
  the server soak runs in constant live memory with no `region:`;
  `__witchy_live_cells == 0` at exit; `interp == wasm == wasm-no-elision` green;
  negative-rc traps. *De-risk fallback:* if precise last-use proves risky, ship
  coarse `drop`-at-scope-exit first (obviously correct, still bounded), then
  tighten.
- **Phase 2 — R1–R3 elision.** Borrow elision + inference; re-key the `__cap`
  helpers on `rc==1` and turn on reuse; thread counts through `own`/`move`. *Only
  after the differential passes* delete the standalone `cap==0` reasoning rc
  subsumes (one cut, no compat layer). *Exit:* in-place benches match today —
  current speed recovered on top of the floor.
- **Phase 3 — R4 + the unified escape lattice.** Consolidate the six escape
  computations into one `Facts` query; region-confined allocations skip RC and
  bulk-free; the lattice partitions confined xor escaping. RC pays for the
  consolidation `performance-modes.md` already scheduled.
- **Phase 4 — `Memory` capability + per-task budgets/sub-arenas.**
- **Phase 5 — R5/R6 unlocks.** Escape→stack/SROA, `unique` qualifier, unboxed +
  header-elided layouts, static FIP. Mode-gated; deferrable.

---

## Design — Part IX: integration (keep / change / delete)

**Keep (recast, not rewritten):** the uniqueness pass + `Summaries` (they *become*
the RC optimizer — `unique_at` ⇒ elide+reuse, `borrows(i)` ⇒ no count, `own_abi`
⇒ count threads through); `let`/`var`/`own`/`move` (Hylo's `let`/`inout`/`sink`,
now also Beans' borrowed/owned distinction); regions + watermark +
`$rcopy_<shape>` (R4, plus a `$dup` at copy-out); `mode opt` + cliff promotion +
transitivity; the bump allocator + `ensure` (now the backing allocator under the
free list).

**Change:** `$mk{n}` writes `rc=1` at `p-4` and pops the free list before bumping;
add `$dup`/`$drop`/`$free`/`$reset`/`$reuse` + per-shape `$rdrop_<shape>` to
`wir_prelude.rs`; promote list `cap` from the shadow local into the header; build
the escape/region lattice as the shared oracle for R4–R5.

**Delete (after the relevant phase proves out at parity):** the bespoke
`cap==0`→reown path subsumed by rc-keyed reuse; the "monotonic growth is
expected" caveat in `spec/architecture.md`; the "pick one memory identity"
non-goal in `performance-modes.md` (resolved here).

---

## Alternatives

- **Tracing GC (ZGC / Go Green Tea / C4).** Rejected. Colored pointers, load/store
  barriers, concurrent relocation, the cache-hostile mark graph-flood — all exist
  to service *large, long-lived, mutable, shared, cyclic* heaps, which witchy's
  value semantics define out of existence. RC on an acyclic heap has no mark phase
  at all, keeps the bump/region locality, and needs no pause. The one transferable
  idea (generational "most objects die young") witchy already realizes
  structurally.
- **Pure arena/region as the floor.** Rejected — regions leak any value forced
  into a long-lived region (the Tofte–Talpin retrospective's known weakness).
  That *is* today's OOM. Regions stay a tier.
- **wasm-gc (engine GC).** Rejected for the default — rewrites the value
  representation (every host reader), boxes data the inline-slot model keeps flat,
  surrenders layout control, to buy a tracing collector witchy doesn't need. May
  revisit only for the browser target.
- **Do nothing.** The server OOM stands; "really solid by default" stays false.

## Drawbacks

- One `i32` header word per heap object + dup/drop traffic — the entire ladder
  exists to drive the traffic to ~today's levels (Lean 4/Koka ship on this bet);
  R6 removes the word on unshared unboxed layouts.
- Short-lived compile-and-exit programs pay inc/dec for memory the OS reclaims at
  exit; mitigated by running them under a `region:` / whole-program escape
  elision, so RC is a floor we optimize *off of*, not a tax.
- Free-list fragmentation — mitigated by power-of-two size classes + regions /
  per-task arenas doing bulk reclaim; measure before considering compaction
  (otherwise refused — no relocation).
- Drop-placement is a use-after-free risk — the one class witchy refuses;
  mitigated by Part V's differentials + sanitizers + the coarse-drop fallback.
- A wide (but shallow) allocator/header change — bounded by the `p-4` placement
  (readers untouched), proven tractable by the `[len]→[len][cap]` surgery already
  absorbed.

## Prior art

- [Perceus](../external-refs/perceus-2021/) — RC + reuse; the floor and the reuse
  token (R0/R2).
- [Counting Immutable Beans](../external-refs/counting-immutable-beans-2019/) —
  borrowed references + borrow inference (R1).
- [FP²: Fully in-Place](../external-refs/fip-fully-in-place-2023/) — the static
  FIP discipline `mode opt` enforces (R6).
- [Mutable Value Semantics](../external-refs/mutable-value-semantics-2022/) — the
  acyclic-heap invariant that makes RC complete; the `let`/`var`/`own` lineage.
- [Region-Based Memory Management](../external-refs/region-based-memory-1997/) —
  regions as confined-case elision (R4); their leak mode is why they can't be the
  floor.
- [Escape Analysis for Java](../external-refs/escape-analysis-java-1999/) — the
  unified escape lattice (R4–R5).
- [C4](../external-refs/c4-concurrent-compacting-2011/),
  [ZGC](../external-refs/zgc-openjdk/),
  [Go Green Tea](../external-refs/go-green-tea-gc-2025/) — the tracing lineage
  rejected in Alternatives.
