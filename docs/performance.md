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
| Multicore actor pipelines | single-threaded drain | **Yes** — actors are already isolated VMs; a threaded scheduler gets shared-nothing parallelism with zero GC coordination |
| Long-lived, pointer-chasing mutable heaps | arena never reclaims | **Hard** — this is what Go's GC is genuinely good at. Out of scope until everything above lands; mitigations in Phase 4 |

The honest summary: witchy should not chase Go by building a GC. Its value
semantics + ownership conventions + per-message arenas are an *Erlang-shaped*
memory story that, done properly, beats GC languages on throughput for the
request/message-scoped workloads witchy targets — and loses (by design) on
workloads witchy is not for.

## Phase 0 — Measure first

A `bench/` suite of paired programs (witchy / Go / C#) run via `hyperfine`,
tracked in CI as numbers, not vibes:

- compute (numeric loops, branchy code)
- list/dict building and traversal (the workload that traps today)
- string building/scanning
- actor ping-pong and fan-out throughput
- HTTP server requests/second (`std/server` vs `net/http` vs ASP.NET minimal)
- cold-start latency (`witchy sandbox` AOT vs `go run`/compiled binary vs `dotnet run`)

`witchy --bench` grows a `--vs-baseline` mode that diffs against the last
recorded run, so regressions fail loudly.

## Phase 1 — Memory model (the current blocker)

1. **Growable lists**: representation becomes `[len][cap][slots…]`, doubling
   on overflow. `push` copies the spine only when `cap` is hit.
   Touches every consumer of the `[len][slots]` layout (at/iterate/equality/
   to_string/message marshaling) — mechanical but wide.
2. **Ownership-driven in-place mutation** — the critical companion. Capacity
   alone cannot fix `xs = push(xs, x)` under value semantics (the result must
   be a fresh value if anyone else can observe `xs`). But the conventions
   system already proves uniqueness: when the assignment target IS the pushed
   operand and the binding is an unaliased `var` (the same analysis the native
   backend uses to elide clones), `push` mutates in place. This is the classic
   linear-update optimization, and it is what turns push from O(n) to
   amortized O(1). Same treatment for `insert` on dicts and string-builder
   accumulation (`s = s <> piece`).
3. **Dict growth**: verify the 16-byte-entry table doubles rather than
   rebuilding per insert; apply the same in-place rule.
4. **Arena reset points**: generalize the actors' `__msg_prep` watermark reset
   to other escape-free boundaries — first target: `std/server`'s per-request
   loop. A reset is sound exactly when no value allocated inside the scope
   escapes it; the capability/escape analysis used for `let`-borrows already
   answers this for function boundaries.
5. **Checked arithmetic**: `+`/`-`/`*` trap on Int overflow like the
   interpreter errors (the last known *silent* divergence). ~2 extra
   instructions per op; measure in Phase 0's compute bench, claw back via
   Phase 2 if needed.

Exit criterion: the 300k-push benchmark runs in the same order of magnitude
as native Rust, and `std/server` compiled serves indefinitely under load.

## Phase 2 — Codegen quality

1. **Binaryen post-pass** (`wasm-opt` crate, Rust bindings to Binaryen): run
   `-O2`/`-O3` + `--converge` over the emitted module behind a `--release`
   flag. This buys a mature optimizer (inlining, GVN, const-prop, dead-code,
   local coalescing) for a naive emitter without writing any passes — the
   single highest-leverage item in this phase.
2. **Direct calls over `call_indirect`** when the closure target is statically
   known (the common `list.map(xs, fn(x): …)` shape).
3. **Flatten non-escaping tuples/records into locals** (escape-analysis lite —
   reuse the lambda-capture escape scan).
4. **SIMD** (`relaxed-simd` in wasmtime config) for the obvious stdlib loops
   (string compare/search, list scans) — after Phase 0 shows where it pays.

## Phase 3 — Engine configuration (cheap, do early)

All wasmtime-45 features we already ship but don't fully use:

1. `Config::cranelift_opt_level(OptLevel::Speed)` for non-preempt engines.
2. **AOT artifact cache**: `Module::serialize`/`deserialize` keyed by program
   hash (the Cranelift cache exists; serialize skips even the cache lookup
   work and makes `witchy sandbox` cold-start microsecond-class).
3. **Pooling instance allocator** (`PoolingAllocationConfig`) for actor
   systems — spawn cost drops from mmap-per-VM to slot reuse; matters once
   handlers spawn workers per job.
4. **Threaded actor scheduler**: the drain loop is single-threaded today.
   Actors share nothing (the whole point), so a work-stealing pool over the
   per-actor mailboxes is safe parallelism Go has to buy with GC coordination.
   (`Engine` and the queue are already `Send`; the table's `Option`-take
   pattern extends to per-actor locks.)

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
| `rayon` or std threads | actor scheduler pool | 3 |
| `wasmer` + `wasmer-compiler-llvm` | optional LLVM engine — only if Binaryen numbers disappoint | 4 |

## Native backend retirement

The native tier stays frozen (no new feature work) until Phase 1's exit
criterion holds and Phase 0 shows the WASM tier within ~1.5× of it across the
suite. Then `witchy native`/`emit-rust` are removed (git keeps the history),
and the ownership-conventions appendix is rewritten around what the
conventions buy the WASM tier (clone elision and in-place mutation — the same
knobs, one tier).
