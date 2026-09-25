# RFC-0165 development helpers

Throwaway-grade helpers the coverage slices were built with. They are not part
of the gate. Every script runs cargo in the background with a status file so a
tool call never sits on a 10-minute build; poll the status file instead.

Environment they assume (override with `WITCHY_DEV_TARGET`):

- `CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-inner` (warm; a fresh one costs
  a 15-25 minute cold build), `CARGO_BUILD_RUSTC_WRAPPER=""`.
- The worktree root is discovered with `git rev-parse --show-toplevel`.

| script | what it does |
|---|---|
| `build.sh` | `cargo build -p witchy` in the background; `status/build.status` holds `exit=N`; log in `status/build.log`. The binary is `$WITCHY_DEV_TARGET/debug/witchy`. |
| `one-test.sh <filter> [cargo test args]` | one `cargo test --test misc <filter>` run in the background (`status/one.status`, `status/one.log`). |
| `shards.sh edit` | the default inner loop: named root-driver budget, WIR prelude/ABI, prefix-ownership, lowering-publication, and admitted aggregate probes (`status/shards.log`, `status/shards.status`). `edit` is the default argument. The four WIR probes share one `witchy-wir --features native --lib` invocation after exact selection comparison; lowerer and production-root probes remain separate because their feature graphs differ. |
| `shards.sh corpus` | all existing `rfc0165_` misc tests, including every admitted-example oracle partition. Use for family-completion checkpoints. |
| `shards.sh landing` | the previous broad local sequence unchanged: all `rfc0165_` misc tests, ABI catalog, lower canonical, WIR native, types, and runtime native. This is a local checkpoint; the merge queue remains the authoritative landing set. |
| `queue-watch.sh <change-id>` | polls the merge-queue journal in the MAIN checkout for a terminal event on the change and writes it to `status/queue-watch.out`. |

The WIR edit-probe consolidation was measured on 2026-09-12 from three warm
old/new pairs. The old two-process sequence took 2.733s median; the combined
process took 1.449s median, saving 1.284s (47.0%) for this segment. Nextest's
machine-readable listings selected the same four test names before and after
(sorted-set SHA-256 `21cc6ce7ec1f25c8a5267ff2fbc21f311ce55026d125c2e060676476d96b4619`).
Normalized `cargo tree -e features` output was identical. This comparison does
not justify combining the lowerer or production-root commands.

`status/` is created next to the scripts (`scripts/rfc0165-dev/status/`) and is
listed in `.gitignore`.

The admitted-example corpus remains in the coordinator gate. Recent gates ran
all 16 partitions at a two-test concurrency limit, with 243-306s perfect-pack
execution floors and 487-612 summed process-seconds. Moving it out of the
default helper saves an estimated 4-6 minutes per small edit round. That saving
has medium confidence because build overlap and current helper logs vary; no
test, assertion, gate filter, budget, or exclusion changes here.
