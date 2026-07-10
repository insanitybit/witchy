---
rfc: 0034
title: "Closing the compute gap: codegen & runtime performance levers"
status: implemented
created: 2026-06-30
tracking: "L1–L4 shipped; L5/L6 deferred"
---
# RFC-0034 — Closing the compute gap: codegen & runtime performance levers

- Status: implemented (L1-L4; L5/L6 deferred)
- Predecessors: [RFC-0029](0029-performance-tier-contract.md) (perf tier contract),
  [RFC-0033](0033-place-based-uniqueness.md) (in-place through user types),
  [spec/performance.md](../spec/performance.md) (the perf thesis)

## Motivation

The memory half of witchy's performance thesis is largely won: value semantics +
uniqueness-driven in-place + arena reclaim beat the GC languages on
allocation-heavy throughput (the string builder is ~3.9× faster than Go; the
record-graph `expr_eval` benchmark is 0.54×, i.e. witchy is ~2× *faster*). The
*compute* half is not. Current standing vs Go on `benchmarks/` + `bench/`
(Apple Silicon, release, validated caches warm), **with process startup stripped out**:

| workload | vs Go (startup-stripped) | the gap is |
|---|---|---|
| memory / string-builder / record-graph | **0.5–1.0× (witchy ahead/tied)** | the memory model — already won |
| simple mixed compute (`cpu`) | ~1.5× | mostly *was* startup |
| numeric kernels (`mandelbrot`, `fannkuch`, `nsieve`) | ~1.8–2.6× | **codegen** |
| closures / indirect calls (`closure_calls`) | ~3.1× | **closure ABI** |
| parallel CPU map (`parmap`) | ~1.4× | per-call VM spin-up + per-thread codegen |
| cold start (`hello`) | ~15 ms vs ~4 ms | process + instantiate (module caches already warm) |

Two facts shape what is and isn't worth doing:

- **Cranelift is already maxed.** We set `OptLevel::Speed` (its top speed tier;
  the only alternative trades for size). Cranelift is a *fast* compiler, not a
  maximal optimizer — there is no `-O3`. The Config knob is spent.
- **There is no real wasm optimizer in the pipeline.** The `wasm-opt`
  `WITCHY_OPT` lever is a 276-line in-house WIR peephole (it drops redundant
  slot/kind conversions). Binaryen / the real `wasm-opt` is **not** a dependency.

So the avoidable compute gap is "codegen quality Cranelift's Speed tier leaves on
the table" + "redundant safety checks" + "closure/parallel ABI overhead" — none
of which the existing knobs touch. This RFC enumerates the levers, in priority
order, each preserving the sandbox and twin-backend parity.

## Non-goals (the deliberate part of the gap — do NOT pursue)

- **A native (LLVM) backend.** It would roughly halve the numeric gap and forfeit
  sandbox-by-construction, which is the language's entire value proposition. The
  native backend was already removed on purpose. Off the table.
- **A tracing GC.** [spec/performance.md](../spec/performance.md) rejects it; value
  semantics admits no cycles, so RC ([RFC-0016](0016-reference-counted-memory.md))
  is the reclamation floor.

The compute gap has an *avoidable* part (codegen, checks, ABI — this RFC) and a
*deliberate* part (running sandboxed). We close the first and keep the second.

## Levers (priority order)

### L1 — Integrate Binaryen `wasm-opt` as a cached compile pass — ✅ SHIPPED
Done (`runtime.rs` `binaryen_optimize`, run in `build_module`'s cold path only, so
warm runs load content-bound optimized wasm through safe `Module::new`, then hit
Wasmtime's compilation cache; optional + graceful if `wasm-opt` is absent).
**Measured win (release, warm):** mandelbrot 95.9 → 53.4 ms (~1.8×, now Go parity),
record_build 40.8 → 29.5 ms (~1.4×), nsieve 39.3 → 33.1 ms (~1.2×). Sound: 104
examples + 15 benchmarks byte-identical with Binaryen on vs off.

The single biggest codegen win Cranelift cannot give. Run real `wasm-opt -O2/-O3`
(GVN, aggressive inlining, DCE, local CSE, local/memory coalescing) on the wasm
witchy emits, *before* Cranelift compiles it. It is slow — but successful output is
cached as ordinary wasm in a corruption-detecting envelope. Every hit is validated
through `Module::new`; Wasmtime's own cache safely reuses the compiled native code.
- **Targets:** the numeric ~1.8–2.6× and, partly, closures.
- **Shape:** `binaryen-rs` crate (in-process, no external binary) or shell out to a
  vendored, hash-pinned `wasm-opt`. Gate behind the existing `WasmOpt` lever
  (replace the in-house peephole, or run after it). Sandbox-safe: `wasm-opt`
  preserves module semantics, and the runtime still validates before instantiate.
- **Risk:** medium — a build/runtime dependency; must stay deterministic for the
  content-hash cache key. Verify with the differential oracle (output invariant
  under every `WITCHY_OPT` setting on both backends) + `bench/run.sh` vs baseline.

### L2 — Bounds-check elision — ✅ SHIPPED (conservative slice)
witchy emits an explicit `i < 0 || i >= len` trap guard on every list access (logical
bounds, not wasm linear-memory bounds — guard pages don't help). The `bounds-elide`
lever (default-on) drops it for the one pattern where in-range is provable *by
construction*: inside `for i in 0..list.length(xs)` the compiler-managed counter
satisfies `0 ≤ i < length(xs)`, so `xs[i]` / `list.at(xs, i)` lowers to a direct
unchecked load — which the Binaryen pass (L1) can then fold.
- **Soundness:** elide ONLY when the loop is half-open (`0..=` would let `i == length`),
  `lo ≥ 0`, the bound is *literally* `list.length(xs)` for the indexed `xs`, and both
  `xs` and the loop var are unshadowed and unreassigned in the body (an exhaustive
  `DevirtScan` walk — a rebind of `xs` could make the proven length stale). Any
  deviation keeps the checked access. Verified: a reassigned list, an inclusive range,
  and a *different* (shorter) list indexed by the same counter all stay checked and
  trap/compute identically on both backends; the differential sweep gained an
  elision-firing case (an unsound out-of-range read would diverge from the
  always-checked interpreter oracle). Firing proof:
  `codegen_tests::elides_bounds_check_in_counted_loop` (no `call $list_at` on, checked
  off). **Measured (`benchmarks/list_index`, 10M indexed reads, release, warm):
  33.4 → 22.0 ms = 1.51×.**
- **Honest reach:** the CLBG numeric kernels (`nsieve`, `fannkuch`) *cache* the length
  in a var (`while i < n`) and prove nothing from `0..length(xs)` — proving `n ==
  length(xs)` is non-local, so conservative elision deliberately does NOT touch them.
  The slice reaches *idiomatic* indexed loops, not hand-tuned cached-length ones. A
  follow-up could extend to `while i < list.length(xs)` (needs an induction proof that
  `i` starts ≥ 0 and only increments) — strictly more analysis surface, deferred.
- **Risk:** medium — it removes a safety check, so soundness is paramount; handled by
  default-deny + the differential oracle + the adversarial cases above.

### L3 — Closure devirtualization — ✅ SHIPPED (part a)
Static user calls already lower to a direct `call`; the ~3.1× cost is *closures* —
`call_indirect` through a heap closure record (a table+type-check dispatch the wasm
engine cannot inline through). **(a) devirtualization is shipped:** a closure local
proven bound to exactly one lambda and never reassigned/shadowed is called with a
direct `call $__lamw{i}`, recovering the lifted body's index at compile time instead
of loading it from the closure record's first word at runtime. The env (so any
captures) still flows through unchanged — the closure object is still passed as the
implicit env arg — so this is sound for capturing *and* capture-free closures alike.
- **Shape:** the reserved `direct-call` `WITCHY_OPT` lever is now wired (default-on).
  `begin_unit` computes the eligible locals (`collect_devirt_eligible`: bound by
  exactly one `let`, never reassigned, never re-introduced by a tuple/pattern/`for`/
  param binder — an *exhaustive*, no-wildcard AST walk, so a future syntax node that
  could rebind a name is a compile error, never a silent unsound devirt); the binding
  recorder maps each eligible local to its `$__lamw{i}` index; the two closure-call
  arms emit a direct `call` when the local is in that map.
- **Firing proof:** a call-SHAPE change moves no heap, so there is no `stats`
  counter — instead `codegen_tests::devirtualizes_single_bound_closure_call` asserts
  the emitted wasm calls `$__lamw` directly under the default and `call_indirect` under
  `-direct-call`. Soundness: the differential sweep gained a closure case (a
  single-bound *capturing* `g` that must devirt-with-env + a *reassigned* `f` that
  must stay indirect), invariant across every `WITCHY_OPT` setting on both backends.
- **The real win is downstream:** a direct `call` to a tiny lambda is something the
  Binaryen pass (L1) can *inline*; a `call_indirect` it cannot. So L3 unlocks L1 on
  higher-order code. **Measured (`closure_calls`, 5M calls, release, warm):** default
  (devirt+binaryen) **19.5 ms** vs `-direct-call` 31.8 ms — a **1.63×** speedup that
  closes the gap from **5.06× → 3.11× Go**. The thesis holds: Binaryen *without*
  devirt barely moves it (31.8 ms vs 30.0 ms with Binaryen also off — it cannot inline
  through `call_indirect`); the win appears only when devirt makes the call direct.
- **(b) no-env capture-free closures: deferred.** Dropping the env param for a
  capture-free closure would break the uniform `call_indirect (type $closN)` ABI
  (all closures of arity N share one indirect-call type), so it is only safe *with*
  devirtualization — and then it saves only the closure object's one-time `mk0`
  allocation, not per-call cost. Marginal next to (a); left for later.

### L4 — Pooling instance allocator — ⚠️ IMPLEMENTED opt-in, MEASURED not-beneficial-yet
Set wasmtime `InstanceAllocationStrategy::Pooling`. Reuses pre-reserved instance
slots instead of fresh `mmap`/teardown. Shipped behind `WITCHY_POOL` (off by
default). **Measured (release):** it makes things SLOWER on current workloads —
one-shot `hello` 15.7 → 25.0 ms (1.59×), and even `parmap`'s single fan-out
106 → 113 ms (1.07×). Root cause: each pool slot must reserve witchy's 1 GiB
per-instance memory cap up front, and a non-repeated workload never amortizes
that reservation.
- **When it WILL pay off:** a long-running server (`serve_pool`) where the
  one-time reservation amortizes over many request instances — and/or a smaller
  per-instance memory cap. Neither holds in the current one-shot-dominated suite.
- **Decision:** keep opt-in + documented; do NOT default-enable until there's a
  server/repeated-instance benchmark that shows the win. Pairs with L5.

### L5 — Worker-VM pool for `vm.par_map`
Today each `par_map` spins up a fresh `Store`/`Instance` per OS thread. RFC-0032
already pools warm worker VMs for `serve_pool`; apply the same so fine-grained
parallelism stops paying VM spin-up per call.
- **Targets:** `parmap` ~1.4×, all fine-grained parallel work.
- **Risk:** low–medium — reuse the proven `serve_pool` machinery; ensure per-call
  capability isolation is preserved on a reused VM.

### L6 — Representation & target-feature work (later, higher effort)
Lower priority, larger surface:
- **Representation specialization beyond `unbox`:** the uniform i64 slot costs float
  bit-reinterprets at slot boundaries and 8-byte-everything. Sub-i64 slots for
  `List(Bool)`, struct-of-arrays for hot record-lists.
- **wasm SIMD** for numeric kernels (`mandelbrot`) via Cranelift's SIMD support.
- **Adopt newer wasm proposals as wasmtime matures them:** tail calls
  (recursion/iterators), eventually wasm-GC (could reshape the value model).

## Measurement (do this FIRST)

Several levers above are *guessed*, not measured, because the bench suite doesn't
cover the relevant shapes. Before optimizing, add the missing paired
witchy/Go benchmarks so each lever lands as a tracked number (`bench/run.sh` diffs
against the recorded baseline and fails loudly on regressions):

- a **mutable-accumulation** bench (a `Stack`/builder wrapping a list) — to exercise
  RFC-0033 and L2 against Go;
- **channel ping-pong** and **fan-out throughput** — concurrency beyond `parmap`;
- keep `bench/BASELINE.md` as the reference (do not overwrite with another machine's
  numbers; regenerate on a fixed reference machine in CI).

## Sequencing

1. **Measurement** — ✅ done (mutable-accumulation `record_build` + channel
   `chan_throughput` benches added; `list_index` added with L2).
2. **L4 + L5 + L3** — L4 ✅ measured (pooling not-beneficial-yet, opt-in); L3(a)
   ✅ shipped (closure devirt); L5 not done (lowest value — `parmap` already ~1.4×).
3. **L1 (Binaryen)** — ✅ shipped (the big codegen lever; safely cached).
4. **L2 (bounds-check elision)** — ✅ shipped (conservative `0..list.length(xs)`
   slice; gated on the differential oracle).
5. **L6** — not done; representation/SIMD/proposals as the target and need mature.

**Shipped status:** the three high-ROI compute levers (L1, L2, L3a) are done and
measured. **Measured wins (release, warm):** mandelbrot 95.9 → 53.4 ms (~Go parity)
via L1; closures 5.06× → 3.11× Go via L3; indexed loops 1.51× via L2. L4 is opt-in
(measured not-beneficial on one-shot workloads). What remains is lower-ROI or larger:
L5 (small — `parmap` is already close) and L6 (SIMD / sub-i64 slots / wasm proposals,
a large surface). The single biggest *remaining* performance gap is not in this RFC's
lever list: the async executor OOMs at ~9k channel messages and is ~19× behind a Go
goroutine on `chan_throughput` (the `vm.par_map` path is healthy at ~1.4×) — a
memory-model fix for the cooperative executor, the natural follow-on.

**Investigated, and it is NOT a localized fix (recorded so the next effort skips the
dead-end).** The executor (`std/chan.witchy` `run` + `step_round`/`step_one`, pure
witchy so any fix is parity-safe) rebuilds its `(slots, channels)` state functionally
each round and carries it across `while go:` iterations. Wrapping the per-round
`step_round` call in a `region:` (to reclaim the round's `set_at` intermediates + the
polled tasks' superseded continuation closures) is **inert**: the OOM ceiling stays at
~10k and N=8000 throughput is unchanged (1.01×). The reason is that the dominant
accumulation is NOT the round's internal temporaries but the **carried state copied
OUT of each round** — the new `slots`/`channels` and the producer's continuation chain
must survive to the next iteration, so a region cannot reclaim them, and each
iteration's superseded copy then leaks (the `var` reassignment isn't RC-floor
reclaimed). A real fix must stop rebuilding the carried state per step: mutate
`slots`/`channels` in place (a `var` the uniqueness pass can prove unaliased, or an
explicit in-place scheduler structure) and/or reclaim the per-iteration churn — a
deliberate executor restructure, not a one-liner. Deferred to a dedicated effort.

**Confirmed empirically that NO existing lever fixes it (so the next effort knows the
floor it must build).** The OOM ceiling on `chan_throughput` is ~10–12k messages under
`default`, `WITCHY_OPT=rc-floor`, AND `WITCHY_OPT=all` alike — identical. The dominant
leak is **element-level**: every `poll` replaces a slot's `Task` with its successor
continuation via `list.set_at(slots, i, Active(cont(…)))`, and the displaced `Task`
object becomes unreferenced garbage. `rc-floor` reclaims a *confined var's* old list
*buffer* on reassignment; it does not reclaim a list *element* overwritten by `set_at`,
which is what churns here. So the cure is the **full per-object RC floor** — the known
deferred residual (per-object reclamation of cache-eviction-style garbage), a runtime
subsystem (size-classed free-list + `set_at`/`mk{n}` freeing the displaced pointer),
NOT a `std/chan` edit and NOT any current `WITCHY_OPT` lever. The throughput gap (~19–26×
under the ceiling) is separately architectural: the CPS `and_then` chain allocates a
closure per message and the channel buffer is a `List` nested inside `channels`, which
the flat-confined-var in-place machinery cannot penetrate. **Bottom line: the executor
is blocked on building the per-object RC floor (a major runtime subsystem) and/or
flattening the scheduler's nested data — a dedicated project, proven (region-wrap,
rc-floor, all = all inert) not reachable by a contained, parity-safe change.**

**The fix is specified in [RFC-0035](0035-completing-the-rc-floor.md).** The leak here
is the *decisive* motivation for completing RFC-0016's per-object refcount floor: it is
inter-procedural (element bound in `step_one`, freed by `set_at` in `try_push`), which
proves a static element-liveness can't reach it and the answer is `dec`-at-last-use on a
runtime refcount. This benchmark (`chan_throughput`, heap flat at 40k) is one of that
RFC's two proving workloads.

## Invariants (every lever)

- **Sandbox preserved** — nothing reaches around the VM boundary; capability model
  untouched.
- **Parity preserved** — output identical under every `WITCHY_OPT` setting on BOTH
  backends (the differential sweep), which is also the soundness gate for the
  check-removing levers (L2) and the ABI-changing ones (L1/L3).
- **Counters, not vibes** — each lever ships a `stats`/`bench` number proving it
  fired and didn't regress.

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections.** L1 (cached Binaryen via PATH shell-out — note the
cache key omits the wasm-opt version), L2 (BoundsElide), L3a (DirectCall), and
L4 (pooling, opt-in) all verified shipped. L5 (par_map worker pool) is NOT —
still a fresh VM per chunk (runtime.rs:1494). L6 is contradicted by the shipped
RFC-0005 engine lockdown (SIMD and tail calls disabled). Status line updated to
`implemented (L1-L4; L5/L6 deferred)` (this edit). Two stale claims: the named
firing-proof tests `devirtualizes_single_bound_closure_call` /
`elides_bounds_check_in_counted_loop` (also cited at opt.rs:103) do not exist
anywhere in the repo — likely lost in the codegen.rs decomposition — so only
output-invariance is tested, and the default-on DirectCall/BoundsElide levers
could silently stop firing (BUG-008). The executor postscript says the fix is in
RFC-0035; it should point at RFC-0036.

**Required revisions.** Restore the two shape tests (BUG-008); fix the
opt.rs:103 pointer; retitle status (done in this edit); fix the RFC-0035 →
RFC-0036 pointer.

**Verdict.** Doc revision + restore the missing shape tests (the real gap).
Priority: medium (tests) / low (doc).

## Tracking note — BUG-008 (2026-07-04): shape tests restored

The two firing-proof SHAPE tests for the default-on `direct-call` (L3a) and
`bounds-elide` (L2) levers — cited above and at `opt.rs`'s registry note but
lost in the codegen.rs decomposition — are restored. They live in
`crates/witchy-lower/src/codegen_tests.rs` as
`devirtualizes_single_bound_closure_call` and `elides_bounds_check_in_counted_loop`
(the `example_tests::` pointers above are corrected to `codegen_tests::`). Each
compiles a program under `WITCHY_OPT` with the lever ON and again with it OFF and
asserts the emitted-wasm call shape flips — devirt: a direct `call $__lamw{i}`
(zero `call_indirect`) ON vs `call_indirect` OFF; bounds: no `call $list_at` ON vs
the checked `$list_at` call OFF — so an inverse guard proves the lever itself fires,
not incidental codegen. BUG-008 fixed.
