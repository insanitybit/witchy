---
rfc: 0023
title: Checked heap — opt-in instrumentation that makes guest-heap corruption loud
status: implemented
created: 2026-06-28
superseded-by:
tracking:
---

# RFC-0023: Checked heap

The shipped checked-heap shadow and redzone instrumentation are implemented in
[`crates/witchy-runtime/src/runtime.rs`](../crates/witchy-runtime/src/runtime.rs)
and [`crates/witchy-wir/src/wir_helpers`](../crates/witchy-wir/src/wir_helpers),
with runtime-unit coverage in [`crates/witchy-runtime/src/runtime_tests.rs`](../crates/witchy-runtime/src/runtime_tests.rs).

> Code blocks here are intentionally **not** tagged `witchy` (per RFC-0002's
> convention): they are illustrative sketches, not complete programs.

## Summary

An **opt-in** (debug/test) instrumentation mode for the witchy guest heap that makes
*intra-linear-memory* corruption loud: out-of-object writes, use-after-free, reads of
freed/uninitialised memory, and missing-`ensure()` overruns. It poisons redzones
around every allocation and tracks per-object bounds + liveness in a shadow map, then
traps with diagnostics on any access that lands outside a live object. wasmtime already
guarantees a guest cannot escape its linear memory; this supplies the missing half —
confidence that *our own* allocator, ownership analysis, and (RFC-0016) refcounting
never corrupt one object through another. Run under the existing suite and the
differential fuzzer (commit `510e57f`), it turns "is the IR/WASM memory safe?" from an
audit question into a property the test run checks.

## Motivation

"WASM memory safety" is three layers, and only the first is free:

- **(A) Host isolation.** wasmtime bounds-checks *every* linear-memory load/store
  against `memory.size`, so a guest physically cannot read or write outside its own
  linear memory. This is a runtime guarantee, in the TCB. It holds even if the guest
  heap is completely corrupt.
- **(B) Host-function ABI.** Every host function trusts guest-supplied
  pointers/lengths, so each must be robust to a hostile module. This is auditable and
  has been audited (e.g. SEC-033/SEC-034: guest-controlled allocations that aborted the
  host).
- **(C) Intra-guest heap integrity — this RFC.** Does our bump allocator, the
  `ensure()` growth discipline, the uniqueness/in-place-mutation analysis, and
  (RFC-0016) refcounting ever write *past* an object into a neighbour, or touch freed
  memory? wasmtime cannot see this: such a write is in-bounds for the *memory* and
  out-of-bounds only for the *object*.

(C) is where the `int_to_string` OOB lived: a WIR helper wrote and bumped the heap
*without* calling `ensure()`, corrupting a neighbouring object **inside** the linear
memory. wasmtime did not trap (the access was within `memory.size`); it was caught only
because it happened to change visible output near a page boundary. That is the problem:

- **Audits** (and the bugs they fix) are a *history*, not a property.
- The **differential fuzzer** (interpreter, a Rust-memory-safe oracle, vs compiled
  WASM) catches corruption that *changes output*, but silent-but-safe corruption and
  untested paths slip through.
- **RFC-0016** (reference-counted memory) will introduce `free()` — and with it
  use-after-free and double-free, the canonical heap bugs. We want the detector in
  place *before* the foot-guns.

A checked heap is the standard answer (it is what AddressSanitizer is to C): make every
heap access prove it is touching a live object, on every executed path.

## Design

`WITCHY_HEAP_CHECK=1` selects a **checked** lowering/runtime; it is **off by default**
and has **zero cost** when off (the normal codegen is emitted unchanged). When on:

1. **Shadow state (host-side).** A map from guest heap region → `{ start, end,
   alloc_id, live }`, maintained as the allocator hands out and (RFC-0016) frees
   objects. Host-side keeps it out of the guest's observable memory and trivial to
   reason about — the price is a host call per checked access, which a test mode can pay.
2. **Redzones.** Each allocation is padded with poison bytes before and after; the
   shadow marks them never-writable. An off-by-N overrun lands in a redzone.
3. **Access checks.** In checked mode the emitted heap loads/stores route through a
   checked path that asserts the address is within a **live** object's `[start, end)`
   and not a redzone. A violation → a loud trap naming the access and the object:

   ```
   HEAP CHECK: store of 8 bytes at 0x10f4 overruns object #317 [0x10e0,0x10f0)
              (a redzone write — likely a missing ensure() or a wrong field offset)
   ```

4. **Free poisoning (for RFC-0016).** A freed object's shadow flips to `live:false`; a
   later access → use-after-free trap; a second free → double-free trap.
5. **Ownership-analysis validation.** At every in-place-mutation site the analysis
   *claims* the value is unique. In checked mode, assert the object's live alias /
   refcount is actually 1 — empirically falsifying a wrong inference instead of
   trusting it. This is the single highest-value check, because in-place mutation is the
   most dangerous thing the compiler does today.

### Two implementable granularities (ship the first, grow into the second)

- **Redzone + checkpoint sweep (first step).** Poison redzones; the runtime sweeps all
  redzones at allocation / function-return / program-exit and traps if any are touched.
  Small, no per-access instrumentation, and it catches the `int_to_string` overrun class
  across the *whole existing suite* immediately. Cost: coarse timing — you learn at the
  next checkpoint, not at the corrupting store.
- **Per-access shadow (ASan-grade).** Every heap load/store consults the shadow first.
  Exact (pinpoints the store, catches bad *reads* too), but pervasive codegen and slow.
  Add it where diagnostics warrant.

### Where the shadow lives

- **Host-side shadow (recommended for the test mode).** The checker lives in the host;
  a checked variant of the store/load helpers calls it. Simple, no guest-memory layout
  change; one host call per checked access (fine for a CI/test mode).
- **In-guest shadow (a possible future fast mode).** A reserved shadow region of linear
  memory plus emitted check code (true ASan style). Faster — all in wasm, enabling an
  always-on cheap mode — but more codegen. Out of scope for the first cut.

### Integration

The gate becomes: **the full suite + the differential fuzzer pass under
`WITCHY_HEAP_CHECK=1`.** A CI job runs them in checked mode; the differential fuzzer
(`510e57f`) is the *coverage* engine (random heap-heavy programs) and the checked heap
is the *depth* oracle (it catches corruption the fuzzer's output comparison would miss).
The two compose: fuzz for breadth, check for the property.

Touch points: the WIR heap helpers and `ensure()` ([`crates/witchy-wir`](../crates/witchy-wir)), the lowering
that emits heap accesses ([`crates/witchy-lower`](../crates/witchy-lower)), and the runtime heap/shadow
([`crates/witchy-runtime`](../crates/witchy-runtime)). A `checked: bool` already threads through the engine (cf. the
`preempt` flag), so the mode selection has a precedent.

## Alternatives

- **Differential fuzzing alone.** Catches corruption that changes output; misses silent
  corruption and untested paths. Complementary, not a substitute — which is exactly why
  this RFC pairs with it rather than replacing it.
- **Rely on wasmtime.** That is only layer (A): no intra-heap visibility at all.
- **AddressSanitizer on the host binary.** Instruments *host* allocations; the guest's
  linear memory is one opaque wasmtime allocation to ASan, so it sees none of the
  guest's object boundaries.
- **Formal verification of the codegen + allocator.** The strongest guarantee, but far
  heavier than today's bug rate justifies; a checked heap is the pragmatic 80%.

## Drawbacks

- **Complexity.** A second, instrumented lowering/runtime path that must track the
  allocator / ownership / refcount design as it evolves.
- **Perf in checked mode** (host calls per access, or sweeps) — acceptable because it is
  opt-in/test-only, but it bounds cadence to a CI job, not every dev run.
- **Redzone-only timing** is checkpoint-granular (you learn at the sweep, not the
  corrupting store) until the per-access mode lands.
- **It detects, it does not prove.** A green checked run means "no *executed* path
  corrupted the heap," bounded by coverage — hence the pairing with the fuzzer.

## Prior art

- **AddressSanitizer** — shadow memory + redzones + a quarantine for use-after-free.
  The direct model; this is ASan specialised to a single wasm linear-memory heap.
- **Valgrind memcheck** — heavyweight, no compile-time instrumentation; the other point
  on the cost/precision curve.
- **The WASM linear-memory model + wasmtime bounds checks** — layer (A), the half we
  already have.
- **[RFC-0016](./0016-reference-counted-memory.md) (reference-counted memory)** — the design this is built to validate;
  `free()` is what makes a checked heap urgent.
- **The differential fuzzer** ([`tests/differential_fuzz.rs`](../tests/differential_fuzz.rs), commit `510e57f`) — the
  coverage engine this depth-checks.
- witchy's own **`ensure()`** heap-growth discipline — the missing call was the
  `int_to_string` OOB, the motivating bug.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
