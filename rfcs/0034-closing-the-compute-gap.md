# RFC-0034 — Closing the compute gap: codegen & runtime performance levers

- Status: proposed
- Predecessors: [RFC-0029](0029-performance-tier-contract.md) (perf tier contract),
  [RFC-0033](0033-place-based-uniqueness.md) (in-place through user types),
  [spec/performance.md](../spec/performance.md) (the perf thesis)

## Motivation

The memory half of witchy's performance thesis is largely won: value semantics +
uniqueness-driven in-place + arena reclaim beat the GC languages on
allocation-heavy throughput (the string builder is ~3.9× faster than Go; the
record-graph `expr_eval` benchmark is 0.54×, i.e. witchy is ~2× *faster*). The
*compute* half is not. Current standing vs Go on `benchmarks/` + `bench/`
(Apple Silicon, release, AOT cache warm), **with process startup stripped out**:

| workload | vs Go (startup-stripped) | the gap is |
|---|---|---|
| memory / string-builder / record-graph | **0.5–1.0× (witchy ahead/tied)** | the memory model — already won |
| simple mixed compute (`cpu`) | ~1.5× | mostly *was* startup |
| numeric kernels (`mandelbrot`, `fannkuch`, `nsieve`) | ~1.8–2.6× | **codegen** |
| closures / indirect calls (`closure_calls`) | ~3.1× | **closure ABI** |
| parallel CPU map (`parmap`) | ~1.4× | per-call VM spin-up + per-thread codegen |
| cold start (`hello`) | ~15 ms vs ~4 ms | process + instantiate (AOT already done) |

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

### L1 — Integrate Binaryen `wasm-opt` as an AOT-cached compile pass — ✅ SHIPPED
Done (`runtime.rs` `binaryen_optimize`, run in `build_module`'s cold path only, so
warm runs deserialize the optimized native via the AOT cache and pay nothing; the
cache key includes the Binaryen flag; optional + graceful if `wasm-opt` is absent).
**Measured win (release, warm):** mandelbrot 95.9 → 53.4 ms (~1.8×, now Go parity),
record_build 40.8 → 29.5 ms (~1.4×), nsieve 39.3 → 33.1 ms (~1.2×). Sound: 104
examples + 15 benchmarks byte-identical with Binaryen on vs off.

The single biggest codegen win Cranelift cannot give. Run real `wasm-opt -O2/-O3`
(GVN, aggressive inlining, DCE, local CSE, local/memory coalescing) on the wasm
witchy emits, *before* Cranelift compiles it. It is slow — but it runs **once at
compile time and the result is AOT-serialized into the existing module cache**, so
runtime pays nothing (this is exactly the `Module::deserialize` path in
`crates/witchy-runtime/src/runtime.rs`).
- **Targets:** the numeric ~1.8–2.6× and, partly, closures.
- **Shape:** `binaryen-rs` crate (in-process, no external binary) or shell out to a
  vendored, hash-pinned `wasm-opt`. Gate behind the existing `WasmOpt` lever
  (replace the in-house peephole, or run after it). Sandbox-safe: `wasm-opt`
  preserves module semantics, and the runtime still validates before instantiate.
- **Risk:** medium — a build/runtime dependency; must stay deterministic for the
  content-hash cache key. Verify with the differential oracle (output invariant
  under every `WITCHY_OPT` setting on both backends) + `bench/run.sh` vs baseline.

### L2 — Bounds-check elision
witchy emits an explicit `index < len` check on list access (logical bounds, not
wasm linear-memory bounds — guard pages don't help). In a loop over `0..len` with
an induction-variable index, the check is provably redundant. Prove in-range
indices statically (loop-induction / range analysis over the lowering) and drop the
check.
- **Targets:** numeric/array loops (`nsieve`, `fannkuch`, list traversal) — the
  steady-state gap Binaryen alone won't fully close (it doesn't know our slot ABI
  or that `len` is monotone in the loop).
- **Shape:** a new `WITCHY_OPT` lever (`bounds-elide`), default-on once proven,
  consuming the same uniqueness/escape analysis machinery. Conservative by default:
  elide ONLY when proven; a miss is a kept check, never an unsound access.
- **Risk:** medium — it removes a safety check, so soundness is paramount. Same
  discipline as RFC-0033: default-deny, differential oracle as the gate, an
  adversarial test corpus (off-by-one, mutated length, aliased index).

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
  counter — instead `example_tests::devirtualizes_single_bound_closure_call` asserts
  the emitted WAT is `call $__lamw` under the default and `call_indirect` under
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

1. **Measurement** — add the mutable-accumulation + channel benches (cheap, makes
   the rest provable).
2. **L4 + L5 + L3** — the tractable runtime/codegen wins (pooling, par_map pool,
   closure devirtualization); infrastructure is half-built for each.
3. **L1 (Binaryen)** — the big codegen lever; one dependency, AOT-cached.
4. **L2 (bounds-check elision)** — the highest-value *compiler* work; schedule
   deliberately, gate on the differential oracle.
5. **L6** — representation/SIMD/proposals as the target and need mature.

Expected envelope if L1–L3 land: numeric ~2× → ~1.3×, closures → near parity,
parallel → near goroutine cost, startup unchanged-but-already-AOT. The deliberate
sandbox cost remains, by design.

## Invariants (every lever)

- **Sandbox preserved** — nothing reaches around the VM boundary; capability model
  untouched.
- **Parity preserved** — output identical under every `WITCHY_OPT` setting on BOTH
  backends (the differential sweep), which is also the soundness gate for the
  check-removing levers (L2) and the ABI-changing ones (L1/L3).
- **Counters, not vibes** — each lever ships a `stats`/`bench` number proving it
  fired and didn't regress.
