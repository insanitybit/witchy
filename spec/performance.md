# Making the WASM tier the only compiled tier — and fast

**Goal:** retire the native (Rust) backend by making compiled-to-WASM witchy
competitive with native code — concretely, matching or beating Go and C# on
the workloads witchy is for. **Sandboxing stays non-negotiable:** every
optimization below preserves the capability model; nothing reaches around the
VM boundary.

## Where that target is winnable (and where it is not)

Wasmtime runs Cranelift-compiled machine code, not an interpreter. Our own
measurements (2026-06-11): a 4M-op arithmetic loop runs in ~16 ms wall
including JIT — the same class as the LLVM-compiled native binary. The gaps
are not "WASM is slow"; they are specific and addressable:

| Workload | Today | Winnable vs Go/C#? |
|---|---|---|
| Compute-bound loops | already native-class | **Yes** — Cranelift + a Binaryen post-pass closes most of the LLVM gap; SIMD where it applies |
| Startup / cold runs | Cranelift cache exists | **Yes, decisively** — AOT-serialized modules instantiate in microseconds; Go/C# pay process + runtime init |
| Allocation-heavy (lists, strings) | **traps OOM** (copy-per-push under a bump arena, O(n²) bytes) | **Yes** — capacity-growth + ownership-driven in-place mutation beats GC throughput; this is Phase 1 |
| Long-running request loops | arena grows until the cap | **Yes** — arena reset points are *faster* than any GC (free bulk reclaim, no pauses) |
| Long-lived, evicting/mutating heaps (caches, indexes, long-lived owned state) | arena alone never reclaims | **In scope via RC** — reference counting ([RFC-0016](../rfcs/0016-reference-counted-memory.md), planned) is the tier-0 reclamation floor that frees escaping/evicted values; witchy has no shared-mutable pointer graphs to chase, so there is no pointer-cycle tail to concede. See [RFC-0029](../rfcs/0029-performance-tier-contract.md) |

The honest summary: witchy should not chase Go by building a *tracing GC*. Its
value semantics + ownership conventions + region-scoped arenas, with reference
counting as the reclamation floor ([RFC-0016](../rfcs/0016-reference-counted-memory.md)),
are an *Erlang-shaped* memory story that beats GC languages on throughput while
also serving the long-lived, evicting state — caches, indexes, servers holding
state — that the general-purpose targets (Go, Python, Ruby, Swift) take for
granted. Two properties make this work without a collector: value semantics
admits no reference cycles, so RC is complete with no tracer; and graphs are
expressed with index-arena handles rather than shared pointers, so even cyclic
*structure* is just integers reclaimed with its arena. The two-tier contract
over this model is [RFC-0029](../rfcs/0029-performance-tier-contract.md).

## Phase 0 — Measure first

A `bench/` suite of paired programs (witchy / Go / C#) run via `hyperfine`,
tracked in CI as numbers, not vibes:

- compute (numeric loops, branchy code)
- list/dict building and traversal (the workload that traps today)
- string building/scanning
- channel ping-pong and fan-out throughput
- HTTP server requests/second (`std/server` vs `net/http` vs ASP.NET minimal)
- cold-start latency (`witchy sandbox` AOT vs `go run`/compiled binary vs `dotnet run`)

`bench/run.sh` runs the paired programs via `hyperfine` and diffs against the
recorded `bench/BASELINE.md`, so regressions fail loudly.

## Phase 1 — Memory model (the current blocker)

1. **Growable lists**: representation becomes `[len][cap][slots…]`, doubling
   on overflow. `push` copies the spine only when `cap` is hit.
   Touches every consumer of the `[len][slots]` layout (at/iterate/equality/
   to_string/message marshaling) — mechanical but wide.
2. **Ownership-driven in-place mutation** — the critical companion. Capacity
   alone cannot fix `xs = list.push(xs, x)` under value semantics (the result must
   be a fresh value if anyone else can observe `xs`). But the conventions
   system already proves uniqueness: when the assignment target IS the pushed
   operand and the binding is an unaliased `var` (the same analysis the native
   backend uses to elide clones), `push` mutates in place. This is the classic
   linear-update optimization, and it is what turns push from O(n) to
   amortized O(1). Same treatment for `insert` on dicts and string-builder
   accumulation (`s = s <> piece`).
3. **Dict growth**: verify the 16-byte-entry table doubles rather than
   rebuilding per insert; apply the same in-place rule.
4. **Arena reset points**: generalize the per-loop-iteration watermark reset
   to other escape-free boundaries — first target: `std/server`'s per-request
   loop. A reset is sound exactly when no value allocated inside the scope
   escapes it; the capability/escape analysis used for `let`-borrows already
   answers this for function boundaries.
5. ~~Checked arithmetic~~ — already resolved the other way: Int overflow
   WRAPS (two's complement) as defined language behavior on both backends
   (`integer_overflow_wraps_like_the_wasm_backend`). No divergence remains,
   and wrapping keeps arithmetic at one instruction.

Exit criterion: the 300k-push benchmark runs in the same order of magnitude
as native Rust, and `std/server` compiled serves indefinitely under load.

**Status 2026-06-11 — PHASE 1 COMPLETE:** items 1–2 landed as one change
(shadow-capacity locals: in-place push + string append, no representation
change); item 3 landed twice (in-place insert, then the hidden-word hash
index: 50k inserts 1.63 s → 10 ms); item 4 landed (loop watermark resets — a
200k-iteration/6 GB-churn soak runs in constant memory); item 5 was already
resolved (overflow wraps by definition on both backends). The 300k-push
bench went from an OOM trap to Go parity.

**Superseded (2026-06-11, later):** item 2's eligibility scan was replaced
wholesale by the **uniqueness pass** ([ownership-analysis.md](../rfcs/ownership-analysis.md)):
share-event/dirty-site analysis with function summaries, so aliases cost one
re-own instead of disqualifying, read-only calls don't break accumulation,
`d = dict.update(…)` upserts and `x = f(move x)` own-ABI pipelines run in place,
and the remaining copy-path cliffs are flagged by `witchy check`/the LSP.

**Scoreboard vs Go (measured, bench/BASELINE.md):** strings 4–5.7× faster;
lists, dicts, compute, and cold start at parity. C# legs ship in the harness
and activate when a dotnet toolchain is present.

## Phase 2 — Codegen quality

1. **Binaryen post-pass** — landed as an opt-in (`WITCHY_OPT=wasm-opt`,
   shell-out, degrades to a no-op without the binary) and then MEASURED:
   at 64M ops the optimized module is no faster (Cranelift Speed already
   emits ~0.6 ns/op for our loop shapes) and the ~50 ms invocation cost
   dominates every benchmark. Verdict: keep the hook for future
   inline-heavy code, but it is NOT a current lever — which also validates
   deferring the wasmer-LLVM engine.
2. **Direct calls over `call_indirect`** when the closure target is statically
   known (the common `list.map(xs, fn(x): …)` shape).
3. **Flatten non-escaping tuples/records into locals** — SHIPPED as escape-driven
   SROA (RFC-0027): a frame-confined record/tuple (used only via field/index
   access, per the `escape` analysis) is scalar-replaced — each field lives in an
   i64-slot local instead of a heap object, for read-only AND field-mutated
   (`p.x = v`) aggregates. Gated by `WITCHY_OPT=sroa`; a 300-iteration confined
   record drops from 6017 to 17 heap bytes. The general optimization knob is the
   single `WITCHY_OPT` lever (RFC-0030), with the differential de-opt sweep and
   `witchy stats` counters as the soundness/effect gates.
3c. **Confined in-place reuse** — SHIPPED (RFC-0016, first reclamation rung). A
   `var` reassigned to a fixed-shape aggregate — a list literal (any length), or a
   record constructor reassigned only to the same constructor — and never used as a
   whole value (the `escape` oracle proves its buffer unaliased) reuses its buffer
   instead of allocating fresh: a record overwrites its field slots; a list
   overwrites the buffer when the new length fits its capacity, else reallocates
   (the buffer ratchets to the max length). So a build-and-drop loop stays O(1) heap
   instead of leaking O(n) (the arena/watermark cannot reclaim a value that escaped
   the loop body and only later died). A self-referential reassignment bails to
   allocation.
   This is the arena/in-place machinery as an RC-elision rung (no refcount word).
   `WITCHY_OPT=rc-elide` (default-on); proven by a bounded-heap `witchy stats`
   counter (O(1) vs O(n)) and the de-opt sweep.
3d. **RC-floor free-at-overwrite** — SHIPPED (RFC-0016, the reclamation floor the
   reuse rung could not reach). A confined, never-aliased `let`/`var` heap local —
   the `escape` oracle's summary-aware `confined_reassigned_vars`: every whole-use is
   a non-leaking call argument (decided by `Summaries::arg_leaks`, so it generalizes
   to user functions) or an element read — that is overwritten by a freshly-allocated
   buffer threading the old one through (`x = f(x, …)`) frees the old buffer into a
   size-classed free-list that the next allocation reuses. This bounds the
   cache-EVICTION case the reuse rung leaks (insert then remove distinct dict keys:
   every `dict.remove` churns a fresh buffer whose dead, uniquely-owned predecessor
   neither the watermark — the dict escapes the iteration — nor the reuse rung —
   reassignment to a builtin result, not a same-shape literal — reclaims). All
   allocations carry a negative-offset `[size]` header (at `ptr-4`; the returned
   object pointer is unchanged, so readers are untouched), so `$rc_free` needs only
   the pointer; `$rc_alloc` scans the free-list, then bumps. ONE mechanism, general
   over operations (no per-method code) and over USER types (every record/tuple/ADT
   funnels through the single `$mkN` allocator, already routed through `$rc_alloc`).
   Opt-in `WITCHY_OPT=rc-floor` (the floor adds a per-object header + free-list
   traffic the default does not pay); proven by the `cache_eviction_bounded_by_rc_floor`
   stats test (off → leaks O(n); on → bounded) plus the `__rc_reused_bytes` counter
   (off 0; on → scales with iterations) and the de-opt sweep. Reclamation currently
   covers the dict allocators and the generic `$mkN` (records/tuples/ADTs); routing
   the list/string primitive allocators through `$rc_alloc` (so their results are
   freeable too) is the remaining bounded extension.
3b. **Packed confined record-lists** — SHIPPED (RFC-0027, packed inferred case).
   A `let xs = [P(..), ..]` of a fixed-scalar record `P`, read only via
   `list.length(xs)` and `list.at(xs, i).field` (the `escape` analysis proves it
   confined), is stored as ONE flat inline buffer — `[count][f0][f1]…]` — instead
   of an N-pointer array to N boxed records, and each `list.at(xs,i).field` lowers
   to a direct slot load (no pointer deref). Reuses the `$mkN` allocator (no new
   heap path). Opt-in `WITCHY_OPT=unbox`; proven by a `witchy stats` heap-drop
   counter (10×2-field list: 4 allocations → 1) and the de-opt sweep.
   The **declared `packed` qualifier** (`type P packed:`) also ships: it makes the
   flat layout a layout CONTRACT rather than a silent best-effort. A `List(P)` of a
   declared-`packed` type packs through this same confined path, and any use the flat
   layout cannot represent — passing/returning/storing the list whole, comparing,
   rendering, `for`-iterating, channel-sending, or flowing into a generic `List(a)` —
   is a clean COMPILE ERROR naming the offending position (never a silent fall-back to
   the boxed layout the programmer declared away). Packability (all fields scalar or
   other `packed`) is enforced at check time. So `packed` guarantees "flat or a loud
   error," confined to one function. Cross-function / host-visible packed layout (an
   ABI that carries the flat representation across boundaries) remains future work;
   today crossing a boundary is the loud error, not silent boxing.
3a. **Zero-copy confined slice views** — SHIPPED (RFC-0028, feature 3). A
   `let w = list.slice(src, lo, hi)` the same `escape` analysis proves confined
   (read only via `list.at`/`list.length`, with `src` never reassigned/mutated nor
   aliased whole) elides the slice COPY: `w` becomes a borrow that reads through
   `src` at an offset (the `$list_at_view`/`$list_len_view` helpers recompute the
   clamped window and trap on the view bound, so reads match the interpreter
   reading the copy). Invisible — no `View` type or new surface, just a faster
   `list.slice`. Gated `WITCHY_OPT=views`; proven by a `witchy stats` heap-drop
   counter and the differential de-opt sweep.
4. **SIMD** (`relaxed-simd` in wasmtime config) for the obvious stdlib loops
   (string compare/search, list scans) — after Phase 0 shows where it pays.

## Phase 3 — Engine configuration (cheap, do early)

All wasmtime-45 features we already ship but don't fully use:

1. `Config::cranelift_opt_level(OptLevel::Speed)` for non-preempt engines.
2. **AOT artifact cache**: `Module::serialize`/`deserialize` keyed by program
   hash (the Cranelift cache exists; serialize skips even the cache lookup
   work and makes `witchy sandbox` cold-start microsecond-class).
3. **Pooling instance allocator** — deferred until profiling shows spawn
   pressure (the measure-first rule; on-demand allocation hasn't appeared in
   any profile yet).

## Phase 4 — Researched options, deliberately deferred

- **wasm-gc proposal** (wasmtime ships a DRC collector): would give real
  reclamation for long-lived heaps by lowering lists/strings/records to GC
  structs/arrays. Deferred: it rewrites the value representation AND every
  host function that reads guest memory (capabilities included), and gives up
  the arena's bulk-free advantage on the workloads we win. Revisit only if a
  flagship use case needs long-lived mutable graphs.
- **Wasmer's LLVM backend** as an optional "release engine": true LLVM
  codegen over the same module. Deferred until Phase 2 numbers exist —
  Binaryen likely closes most of the gap without linking LLVM or maintaining
  a second runtime integration.
- **wizer** (pre-initialized snapshots): superseded by our own start-function
  + AOT serialize combination.

## Crates

| Crate | Use | Phase |
|---|---|---|
| `wasm-opt` (Binaryen bindings) | post-pass optimizer over emitted modules | 2 |
| `wasmtime` 45 (already in) | opt-level, serialize/deserialize AOT, pooling allocator, relaxed-SIMD | 3 |
| `hyperfine` (dev-dependency / CI tool) | benchmark harness vs Go/C# | 0 |
| `wasmer` + `wasmer-compiler-llvm` | optional LLVM engine — only if Binaryen numbers disappoint | 4 |

## Native backend retirement — DONE (2026-06-11, e302f70)

The exit criteria held (Phase 1 complete; the WASM tier at or beyond the
native tier across the suite), so the native backend was removed:
`src/rustgen.rs` deleted, the `witchy native`/`emit-rust` CLI arms dropped,
and the `let`-borrow no-escape contract moved from rustc's borrow checker
into typeck (`borrow_escape_check`) so the documented language rule survives
the backend. The ownership-conventions appendix is rewritten around what the
conventions buy the one compiled tier (provable non-aliasing for in-place
mutation).
