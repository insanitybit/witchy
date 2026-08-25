#!/usr/bin/env bash
# RFC-0153 deterministic attribution capture.
#
# This is deliberately a counter/profile companion, not a timing harness:
# promotion timing stays in benchmarks/run.sh, which owns the two-warmup,
# twelve-pair Witchy/Go protocol.  Run this before selecting a mechanism so a
# target cannot be claimed by RFC-0153 from a wall-clock result alone.
#
# Usage:
#   WITCHY=target/release/witchy ./benchmarks/rfc0153_attribution.sh
# Optional: OUT_DIR, WORKLOADS (space-separated benchmark stems), OPT.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

WITCHY=${WITCHY:-target/release/witchy}
OPT=${OPT:-all}
WORKLOADS=${WORKLOADS:-"collatz nsieve fannkuch list_index"}
OUT_DIR=${OUT_DIR:-benchmarks/.build/rfc0153-attribution-$(date -u +%Y%m%d-%H%M%S)}
COUNTERS=(
    list_header_loads checked_indexed_loads checked_indexed_stores
    cursorized_indexed_accesses sequence_bounds_checks_coalesced
    sequence_forwarded_loads sequence_small_loops_unrolled
)

[[ -x $WITCHY ]] || { echo "missing executable: $WITCHY" >&2; exit 2; }
mkdir -p "$OUT_DIR"

sha256() { shasum -a 256 | awk '{print $1}'; }
result() { grep -v '^bench_ns=' || true; }
deterministic_stats() {
    WITCHY_OPT="$OPT" "$WITCHY" stats "$1" 2>/dev/null |
        awk -v wanted=" ${COUNTERS[*]} " '$1 != "" && index(wanted, " " $1 " ") { print $1 "\t" $2 }'
}

printf '%s\n' \
    'rfc=0153' \
    "revision=$(git rev-parse HEAD)" \
    "host=$(uname -srvm)" \
    "witchy=$(cd "$(dirname "$WITCHY")" && pwd)/$(basename "$WITCHY")" \
    "witchy_sha256=$(shasum -a 256 "$WITCHY" | awk '{print $1}')" \
    "opt=$OPT" \
    "workloads=$WORKLOADS" \
    >"$OUT_DIR/metadata.txt"
printf 'workload,source_sha256,output_sha256\n' >"$OUT_DIR/correctness.csv"
printf 'workload,run,counter,value\n' >"$OUT_DIR/counters.csv"

for workload in $WORKLOADS; do
    source="benchmarks/${workload}.witchy"
    [[ -f $source ]] || { echo "missing workload: $source" >&2; exit 2; }
    output=$(WITCHY_OPT="$OPT" "$WITCHY" sandbox "$source" 2>/dev/null | result)
    [[ -n $output ]] || { echo "empty result: $workload" >&2; exit 1; }
    printf '%s,%s,%s\n' "$workload" "$(shasum -a 256 "$source" | awk '{print $1}')" \
        "$(printf '%s' "$output" | sha256)" >>"$OUT_DIR/correctness.csv"

    for run in 1 2 3; do
        deterministic_stats "$source" >"$OUT_DIR/${workload}.counter-${run}.tsv"
        WITCHY_OPT="$OPT" "$WITCHY" stats "$source" >"$OUT_DIR/${workload}.counter-${run}.raw"
        if [[ $run != 1 ]] && ! cmp -s \
            "$OUT_DIR/${workload}.counter-1.tsv" "$OUT_DIR/${workload}.counter-${run}.tsv"; then
            echo "non-deterministic RFC-0153 counters: $workload" >&2
            exit 1
        fi
        awk -F '\t' -v workload="$workload" -v run="$run" \
            '{ print workload "," run "," $1 "," $2 }' \
            "$OUT_DIR/${workload}.counter-${run}.tsv" >>"$OUT_DIR/counters.csv"
    done
done

echo "wrote RFC-0153 attribution artifact: $OUT_DIR" >&2
