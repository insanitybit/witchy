---
rfc: 0094
title: Persistent test-compile caching (std expansion across test processes)
status: proposed
created: 2026-07-15
superseded-by:
tracking: >
  Proposed. The differential test matrix spends ~41% of suite CPU in
  frontend compilation, most of it re-compiling the same std modules in
  every one of ~950 test processes. Extend the compile pipeline's caching
  (per-process linker expansion cache; build-time prelude blob; wasmtime's
  binary-identity-keyed disk cache) into a persistent, compiler-identity-
  keyed artifact cache shared across test processes. Handed off: collides
  with the active RFC-0063 catalog work in the linker/std surface.
related:
  - "0093 (diff-scoped merge gate — this is the next gate-time lever)"
  - "0063 (intrinsic catalogs — owns the linker/std surface today)"
---

# RFC-0094: Persistent test-compile caching

## Measurement (2026-07-15, gate log 20260715-210751, 2089 tests, 1023s CPU)

| category | CPU | tests |
|---|---|---|
| example_tests (differential matrix, excl. property groups) | 422s (41%) | 943 |
| rfc0090_indirect_tail (5M-transition resource proofs) | 313s (31%) | 8 |
| everything else | ~288s (28%) | ~1140 |

The 943 matrix tests average ~0.45s each. Each runs as its own nextest
process and compiles its program from scratch: parse -> typecheck -> lower
-> encode, INCLUDING the std modules the program imports. In-process caches
cannot help across processes. Precedents that prove the shape of the fix:

- The "pulled-std expansion cache" (linker, in-process) took
  `stdlib_properties::semver_roundtrips` from 17.9s to 2.6s — the cost is
  real and it is std re-expansion, not the tests' own programs.
- The pre-compiled PRELUDE BLOB already moves a std subset's compilation to
  build time.
- wasmtime's disk cache is keyed on the `witchy` binary's mtime+size — the
  project already trusts binary-identity-keyed artifact caching.

## Proposal

One of (in increasing ambition):
1. Extend the prelude blob to cover all of std (build-time cost, zero
   run-time std expansion anywhere — also speeds the CLI for users).
2. A disk-backed expansion/lowering cache under `target/witchy-testcache/`,
   keyed (binary mtime+size, blake3 of linked source) — first process per
   gate pays, the other ~950 hit.

Estimated gate effect: example_tests CPU roughly halves (~-200s CPU,
~-20s run wall idle; considerably more under contention). Combined with the
shipped interp/runtime O2 this is the remaining path to a further ~25% cut
in the post-RFC-0093 gate baseline.

## Soundness constraints

- The cache key MUST include compiler identity (binary mtime+size at
  minimum, ideally a build hash): a stale artifact after a compiler change
  is a silent parity catastrophe. wasmtime's cache sets the precedent.
- Cache must be advisory: corrupt/missing entries recompile.
- Both backends must consume identical linked input (the cache serves the
  shared frontend, not either backend's lowering) or cache per-backend.

## Why handed off rather than landed

The linker/std expansion surface is under active RFC-0063 catalog work
(string/math/encoding/list/dict catalogs merged 2026-07-15 alone), and the
expansion cache itself was just introduced there. Two agents in that code
concurrently is how parity bugs happen. Whoever owns the catalog completion
should fold this in; the measurements above are current as of tonight.

## Rejected for this goal (measured 2026-07-15)

- Dependency opt-levels (wasmtime/cranelift at O2): no run-phase effect —
  the pipeline's own caches already amortize cranelift; warm and cold A/B
  probes flat.
- Reducing the rfc0090 5M-transition counts: weakens the constant-stack
  proof margin; forbidden.
- Test-binary consolidation: relinks measured ~17s total, not the cost.
- interp/runtime at O2: measured here (scalar 5M-transition proof
  15.9s -> 12.4s, -22..-29%), then superseded mid-flight by d06a05a1, which
  landed ALL stage crates at O2 plus a hash-map `intrinsics::lookup`
  (-50% interp-bound test CPU) — two agents independently converged on the
  same lever the same evening. The remaining headroom is the frontend
  compile caching this RFC proposes, and per d06a05a1's own notes, arena
  allocation in the interpreter (~30% of samples).
