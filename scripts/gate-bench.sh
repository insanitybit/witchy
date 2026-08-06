#!/usr/bin/env bash
# Controlled merge-gate benchmark — the ONE way to measure "how long is the
# gate" so successive perf rounds compare like against like. Hand-rolling
# this protocol has produced three rounds of confounded numbers; every trap
# below was hit in practice on 2026-07-15 (see scratch/gate-perf-2026-07-15.md):
#
#   * `touch`-based diff simulation measures NOTHING: sccache returns
#     content-identical compiles instantly, so only relinks run (~11-17s).
#     This script measures the UNCHANGED-CONTENT class — the floor every
#     real gate adds its (content-dependent) compile delta to.
#   * A pre-warm without --workspace leaves the member crates' own test
#     targets on stale fingerprints; the measured gate then "rebuilds the
#     workspace" and reads ~60s slower than reality.
#   * Measurements right after a long build are thermally throttled
#     (observed 2-3x on identical back-to-back runs); we settle first.
#   * A loaded machine (other agents) inflates the run phase arbitrarily;
#     we record 1-min load with the result and warn above a threshold.
#   * The first suite run after a relink pays cold binary-identity-keyed
#     caches. The gate ALWAYS pays this (fresh binary every merge), so the
#     benchmark keeps it — do not "fix" it by pre-running the suite.
#
# Usage: ./scripts/gate-bench.sh [label]
#   Runs: --workspace pre-warm, thermal settle, then the exact merge-gate
#   command (gate env, under the merge-queue lock), and appends one
#   machine-readable line to scratch/gate-bench/results.tsv.
set -euo pipefail
cd "$(dirname "$0")/.."

label="${1:-adhoc}"
settle="${GATE_BENCH_SETTLE:-90}"
out_dir="scratch/gate-bench"
mkdir -p "$out_dir"
log="$out_dir/$(date +%Y%m%d-%H%M%S)-$label.log"

load1="$(sysctl -n vm.loadavg 2>/dev/null | awk '{print $2}' || uptime | sed 's/.*load average[s]*: //' | cut -d, -f1)"
cores="$(sysctl -n hw.ncpu 2>/dev/null || echo '?')"
awk -v l="$load1" -v c="$cores" 'BEGIN { if (c != "?" && l+0 > c*1.5) exit 0; exit 1 }' \
    && printf '\033[1;33mgate-bench: load %s on %s cores — results will be contention-inflated\033[0m\n' "$load1" "$cores"

printf '==> pre-warm (--workspace; REQUIRED — see header)\n'
cargo nextest run --no-run --workspace >/dev/null 2>&1

printf '==> thermal settle (%ss; GATE_BENCH_SETTLE to override)\n' "$settle"
sleep "$settle"

printf '==> gate (env + lock exactly as the coordinator runs it); log: %s\n' "$log"
t0=$(date +%s)
./scripts/merge-queue.sh with-lock -- \
    env CARGO_INCREMENTAL=0 WITCHY_GATE_FUZZ=reduced NEXTEST_STATUS_LEVEL=fail \
    ./scripts/check.sh >"$log" 2>&1
rc=$?
t1=$(date +%s)
total=$((t1 - t0))

strip() { LC_ALL=C sed 's/\x1b\[[0-9;]*m//g'; }
stages="$(strip <"$log" | grep -E '^==> \[' | sed 's/^==> //' | paste -sd';' -)"
run_s="$(strip <"$log" | grep -E 'Summary \[' | tail -1 | sed -E 's/.*Summary \[ *([0-9.]+)s\].*/\1/')"
tests="$(strip <"$log" | grep -E 'Summary \[' | tail -1 | sed -E 's/.* ([0-9]+) tests run.*/\1/')"

printf '\n\033[1;32mgate-bench %s\033[0m total=%ss run=%ss tests=%s load1=%s rc=%s\n' \
    "$label" "$total" "${run_s:-?}" "${tests:-?}" "$load1" "$rc"
printf '  stages: %s\n' "$stages"

printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$label" "$(git rev-parse --short HEAD)" \
    "$total" "${run_s:-?}" "${tests:-?}" "$load1" "$rc" "$stages" \
    >>"$out_dir/results.tsv"
exit "$rc"
