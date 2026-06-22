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
| Long-lived, pointer-chasing mutable heaps | arena never reclaims | **Hard** — this is what Go's GC is genuinely good at. Out of scope until everything above lands; mitigations in Phase 4 |

The honest summary: witchy should not chase Go by building a GC. Its value
semantics + ownership conventions + region-scoped arenas are an *Erlang-shaped*
memory story that, done properly, beats GC languages on throughput for the
request/message-scoped workloads witchy targets — and loses (by design) on
workloads witchy is not for.

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

1. **Binaryen post-pass** — landed as an opt-in (`WITCHY_WASM_OPT=1`,
   shell-out, degrades to a no-op without the binary) and then MEASURED:
   at 64M ops the optimized module is no faster (Cranelift Speed already
   emits ~0.6 ns/op for our loop shapes) and the ~50 ms invocation cost
   dominates every benchmark. Verdict: keep the hook for future
   inline-heavy code, but it is NOT a current lever — which also validates
   deferring the wasmer-LLVM engine.
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
