#!/usr/bin/env bash
# RFC-0152 reproducible ready/blocking measurement matrix.
#
# Compare a scheduler control and a candidate built from the same revision.
# The script preserves twelve counterbalanced in-program timing pairs, three
# deterministic counter payloads, source/output hashes, and the full raw
# `witchy stats` output for every matrix row. It deliberately keeps ready and
# blocking workloads separate: no aggregate may average them together.
#
# Usage:
#   CONTROL_WITCHY=target/release/witchy CANDIDATE_WITCHY=target/release/witchy \
#     CONTROL_OPT=-rfc0152 CANDIDATE_OPT=all ./benchmarks/rfc0152_matrix.sh
#
# Optional variables: LABEL, OUT_DIR, WORKLOADS, RUNS (must be 12), WARMUP.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

CONTROL_WITCHY=${CONTROL_WITCHY:-target/release/witchy}
CANDIDATE_WITCHY=${CANDIDATE_WITCHY:-$CONTROL_WITCHY}
CONTROL_OPT=${CONTROL_OPT:-all}
CANDIDATE_OPT=${CANDIDATE_OPT:-all}
LABEL=${LABEL:-rfc0152}
OUT_DIR=${OUT_DIR:-benchmarks/.build/rfc0152-${LABEL}-$(date -u +%Y%m%d-%H%M%S)}
WORKLOADS=${WORKLOADS:-"chan_throughput chan_scheduler select_fanin"}
RUNS=${RUNS:-12}
WARMUP=${WARMUP:-2}

if [[ $RUNS != 12 ]]; then
    echo "RFC-0152 requires exactly 12 paired timing samples (RUNS=12)" >&2
    exit 2
fi
for witchy in "$CONTROL_WITCHY" "$CANDIDATE_WITCHY"; do
    if [[ ! -x $witchy ]]; then
        echo "missing executable: $witchy" >&2
        exit 2
    fi
done

mkdir -p "$OUT_DIR"
COUNTERS=(
    heap_bytes rc_alloc_calls bump_alloc_calls rc_reuse_calls rc_free_calls
    scheduler_slot_transitions scheduler_wait_installs scheduler_wake_claims
    selected_result_materializations
)

kernel_ns() { awk -F= '/^bench_ns=/{ print $2; exit }'; }
result() { grep -v '^bench_ns=' || true; }
sha256() { shasum -a 256 | awk '{print $1}'; }

run_program() {
    local binary=$1 opt=$2 source=$3
    WITCHY_OPT="$opt" "$binary" sandbox "$source" 2>/dev/null
}

deterministic_stats() {
    local binary=$1 opt=$2 source=$3
    WITCHY_OPT="$opt" "$binary" stats "$source" 2>/dev/null |
        awk -v wanted=" ${COUNTERS[*]} " '$1 != "" && index(wanted, " " $1 " ") { print $1 "\t" $2 }'
}

printf '%s\n' \
    "rfc=0152" \
    "label=$LABEL" \
    "revision=$(git rev-parse HEAD)" \
    "host=$(uname -srvm)" \
    "control_binary=$(cd "$(dirname "$CONTROL_WITCHY")" && pwd)/$(basename "$CONTROL_WITCHY")" \
    "candidate_binary=$(cd "$(dirname "$CANDIDATE_WITCHY")" && pwd)/$(basename "$CANDIDATE_WITCHY")" \
    "control_opt=$CONTROL_OPT" \
    "candidate_opt=$CANDIDATE_OPT" \
    "runs=$RUNS" \
    "warmup=$WARMUP" \
    "workloads=$WORKLOADS" \
    >"$OUT_DIR/metadata.txt"

printf 'workload,mode,sample,kernel_ns\n' >"$OUT_DIR/timing.csv"
printf 'workload,mode,run,counter,value\n' >"$OUT_DIR/counters.csv"
printf 'workload,source_sha256,control_output_sha256,candidate_output_sha256\n' >"$OUT_DIR/correctness.csv"

for workload in $WORKLOADS; do
    source="benchmarks/${workload}.witchy"
    [[ -f $source ]] || { echo "missing workload: $source" >&2; exit 2; }

    control_output=$(run_program "$CONTROL_WITCHY" "$CONTROL_OPT" "$source" | result)
    candidate_output=$(run_program "$CANDIDATE_WITCHY" "$CANDIDATE_OPT" "$source" | result)
    if [[ -z $control_output || $control_output != "$candidate_output" ]]; then
        echo "correctness mismatch for $workload" >&2
        exit 1
    fi
    printf '%s,%s,%s,%s\n' "$workload" \
        "$(shasum -a 256 "$source" | awk '{print $1}')" \
        "$(printf '%s' "$control_output" | sha256)" \
        "$(printf '%s' "$candidate_output" | sha256)" \
        >>"$OUT_DIR/correctness.csv"

    for mode in control candidate; do
        if [[ $mode == control ]]; then
            binary=$CONTROL_WITCHY
            opt=$CONTROL_OPT
        else
            binary=$CANDIDATE_WITCHY
            opt=$CANDIDATE_OPT
        fi
        for warmup in $(seq 1 "$WARMUP"); do
            run_program "$binary" "$opt" "$source" >/dev/null
        done
        for run in 1 2 3; do
            deterministic_stats "$binary" "$opt" "$source" >"$OUT_DIR/${workload}.${mode}.counter-${run}.tsv"
            WITCHY_OPT="$opt" "$binary" stats "$source" >"$OUT_DIR/${workload}.${mode}.counter-${run}.raw"
            if [[ $run != 1 ]] && ! cmp -s \
                "$OUT_DIR/${workload}.${mode}.counter-1.tsv" \
                "$OUT_DIR/${workload}.${mode}.counter-${run}.tsv"; then
                echo "non-deterministic RFC-0152 counters: $workload/$mode" >&2
                exit 1
            fi
            awk -F '\t' -v workload="$workload" -v mode="$mode" -v run="$run" \
                '{ print workload "," mode "," run "," $1 "," $2 }' \
                "$OUT_DIR/${workload}.${mode}.counter-${run}.tsv" >>"$OUT_DIR/counters.csv"
        done
    done

    # Counterbalance pair order to avoid a fixed thermal or cache bias.
    for sample in $(seq 1 "$RUNS"); do
        if (( sample % 2 )); then
            order="control candidate"
        else
            order="candidate control"
        fi
        for mode in $order; do
            if [[ $mode == control ]]; then
                binary=$CONTROL_WITCHY
                opt=$CONTROL_OPT
            else
                binary=$CANDIDATE_WITCHY
                opt=$CANDIDATE_OPT
            fi
            output=$(run_program "$binary" "$opt" "$source")
            ns=$(printf '%s\n' "$output" | kernel_ns)
            if [[ -n $ns ]]; then
                printf '%s,%s,%s,%s\n' "$workload" "$mode" "$sample" "$ns" >>"$OUT_DIR/timing.csv"
            else
                # chan_throughput intentionally has no source-level kernel clock.
                printf '%s,%s,%s,wall-only\n' "$workload" "$mode" "$sample" >>"$OUT_DIR/timing.csv"
            fi
        done
    done
done

echo "wrote RFC-0152 matrix artifact: $OUT_DIR" >&2
