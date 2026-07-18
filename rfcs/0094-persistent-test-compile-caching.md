---
rfc: 0094
title: Persistent test-compile caching (std expansion across test processes)
status: implemented
created: 2026-07-15
superseded-by:
tracking: >
  Implemented 2026-07-15. Bundled std modules that round-trip through the
  AST serializer are cached after records lowering and comptime expansion.
  Test processes share BLAKE3-validated exact-AST
  artifacts keyed by exact test-executable identity and ASLR-independent
  expander identity; every cache failure falls back to ordinary expansion.
related:
  - "0093 (diff-scoped merge gate — this is the next gate-time lever)"
  - "0063 (intrinsic catalogs — the cache landed after the active catalog edits cleared)"
---

# RFC-0094: Persistent test-compile caching

The cache implementation is in [`crates/witchy-syntax/src/linker.rs`](../crates/witchy-syntax/src/linker.rs),
with behavior coverage in [`tests/misc/rfc0094_persistent_std_cache.rs`](../tests/misc/rfc0094_persistent_std_cache.rs).

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

## Decision

Implement the disk-backed expansion cache under
`target/witchy-testcache/v1/`. The process-local cache remains the first tier.
On its miss, a cargo test executable reads an exact-AST artifact for the
bundled std module; on a disk miss it performs the existing parse, records
lowering, and comptime expansion, then writes the artifact atomically.

The namespace binds all of:

- the test executable's Cargo filename, byte length, and nanosecond mtime;
- the expander function's offset from a compiler anchor, which cancels ASLR but
  keeps alternate test expanders separate;
- the cache format version and std module name.

The payload is the complete prepared `Module` encoded with Serde/Postcard. It
preserves compiler-only state that canonical source cannot represent, including
derived-type flags, generated-impl origins, lowered intrinsics, and exact source
locations. It carries a BLAKE3 integrity envelope and an 8 MiB size ceiling.
Missing, truncated, corrupt, undecodable, oversized, or unwritable entries are
ordinary misses. Concurrent writers use per-process temporary files and atomic
rename; losing a race is harmless because every writer produces the same
validated artifact.

The cache activates only for executables under Cargo's `debug/deps` or
`release/deps` layout. Installed CLI binaries retain process-local caching; a
user-facing persistent compiler cache needs its own location and lifecycle
decision. Tests may redirect the root with `WITCHY_TEST_STDLIB_CACHE_DIR`.

The pre-implementation estimate was that example-test frontend CPU could fall
by roughly 200s across the full matrix. The focused measurement below is
smaller per process, so that estimate is now a hypothesis rather than a claim;
only serialized coordinator gates may establish the aggregate effect.

## Soundness constraints

- The cache key MUST include compiler identity (binary mtime+size at
  minimum, ideally a build hash): a stale artifact after a compiler change
  is a silent parity catastrophe. wasmtime's cache sets the precedent.
- Cache must be advisory: corrupt/missing entries recompile.
- Both backends must consume identical linked input (the cache serves the
  shared frontend, not either backend's lowering) or cache per-backend.

## Verification (2026-07-15)

`rfc0094_persistent_std_cache` spawns the same test executable three times. The
first process populates a semver artifact, the second proves a hit by leaving
its mtime and bytes unchanged, and the third repairs a deliberately corrupted
entry before linking and typechecking succeeds. This directly covers process
identity, integrity fallback, and exact linked-AST reconstruction.

Three clean cold/warm pairs of
`example_tests::stdlib_properties::semver_roundtrips` measured a minimum 2.59s
real / 2.32s user cold and 2.47s real / 2.21s user warm (4.6% real, 4.7% user).
The exact-AST cache contained 17 modules and used 140 KiB. This validates a
repeatable process-level saving but is not a whole-gate claim; the coordinator's
serialized gate remains the aggregate measurement.

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
