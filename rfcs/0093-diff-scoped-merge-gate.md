---
rfc: 0093
title: Diff-scoped merge gate
status: implemented
created: 2026-07-15
superseded-by:
tracking: >
  Implemented. The merge-queue coordinator classifies each gate batch's diff
  and scopes the pre-merge gate to what the diff can affect: fuzz seeds are
  reduced or skipped off the parity surface (WITCHY_GATE_FUZZ, pre-existing),
  and a diff confined to documentation no test reads skips the heavy stages
  entirely (WITCHY_GATE_SCOPE=docs). Post-merge CI always runs the complete
  suite; --full is never scoped. The gate itself is restructured so tests are
  the sole foreground stage, with clippy and the wasm playground build as
  collected background legs.
related:
  - "0058 (gate exit-code masking — why check.sh must not be piped)"
  - "0070 (release queue — the throughput pressure this answers)"
---

# RFC-0093: Diff-scoped merge gate

## Problem

Every merge, regardless of content, paid the same pre-merge gate: a serial
front of clippy (59-73s warm; 237s observed under contention) and a
redundant standalone binary build (21-174s), then the full test suite. On
2026-07-15 the median single-batch gate was ~450s, with contended gates at
780-940s. Queue throughput is the release push's bottleneck: every branch,
including a pure RFC edit, waited the full price.

## Decision

Scope the pre-merge gate to what the batch diff can affect, with post-merge
CI as the unscoped backstop. Three mechanisms:

1. **Structural (all gates):** `check.sh`'s default mode runs the test suite
   as the only foreground stage. The `witchy` binary is produced by nextest's
   own build (integration tests reference `CARGO_BIN_EXE_witchy`, so cargo
   must rebuild it fresh), the fmt sweep runs after tests against that
   binary, and clippy runs concurrently in a CoW-cloned `target-clippy` dir
   (same-dir cargo invocations serialize on the build lock). Background legs
   are collected — and fail the gate — before green, and are reaped by an
   EXIT trap on any red. `--fast` uses the same overlap.

2. **Fuzz policy (pre-existing, RFC'd here for the record):**
   `WITCHY_GATE_FUZZ` reduces the fixed-seed differential fuzzer to 10 seeds
   when the diff touches the parity surface and skips it when it cannot
   affect parity. Unchanged by this RFC beyond `--no-renames` (below).

3. **Docs-only scope (new):** when EVERY changed path in the batch diff is
   documentation no test or gate stage reads — `rfcs/` (except
   `rfcs/performance-modes.md`, which
   `example_tests::public_sources_do_not_call_legacy_render_intrinsic`
   reads), `wiki/`, `bugs/`, and the gitignored `scratch/`/`security-eval/`
   — the coordinator passes `WITCHY_GATE_SCOPE=docs` and check.sh skips
   tests/clippy/fmt/wasm/book. Such a diff cannot change any stage's
   outcome; the gate would only re-validate the already-gated master tree.

## Soundness

- The classifier diffs with `--no-renames`: rename detection reports only a
  rename's post-image path, so a `git mv` of code into `rfcs/` would
  otherwise classify as docs and bypass the gate (found in adversarial
  review; reproduced, then fixed and verified).
- Classifier greps avoid quiet-first-match (`-q`) forms whose SIGPIPE
  interaction with `set -o pipefail` mis-signals on very large batch diffs,
  and avoid `grep -qv` (divergent exit semantics under grep shims).
- Fail-safe direction everywhere: empty diff, git error, or any path outside
  the tiny safe set → the full gate. `--fast`, `--full`, the shards, and
  standalone runs ignore the scope entirely.
- Post-merge CI (`ci.yml`, on push to master) runs the complete unscoped
  suite, including the 30-seed heap-checked fuzz sweeps.

## Non-goals / rejected

- `CARGO_INCREMENTAL=1` in gates: the global `rustc-wrapper = sccache`
  rejects incremental compiles outright (measured; documented at the env
  site in merge-queue.sh).
- Consolidating the ~34 integration-test binaries: measured relink cost
  after a touch-invalidation is ~17s total (sccache absorbs recompiles,
  links parallelize) — not worth the churn.
- Shrinking long-running resource-proof tests (e.g. the RFC-0090
  5,000,000-transition constant-stack proofs): reducing counts weakens the
  proof margin; the run phase stays CPU-bound and unscoped.

## Measured effect (2026-07-15, scratch/gate-perf-2026-07-15.md)

Controlled like-for-like gate runs: 261s and 356s against a 413-472s
baseline (-25..-42%); the serial front (95-413s observed) is eliminated in
production gates (stage timings show fmt/clippy/wasm collects at 0-1s).
Docs-only gates drop from ~450s to seconds once the coordinator daemon is
restarted onto this code.
