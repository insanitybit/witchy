#!/usr/bin/env bash
# RFC-0153 matched control/candidate measurement matrix.
#
# Records two warmups, twelve counterbalanced kernel pairs, output hashes, and
# three byte-identical sequence-counter payloads for each workload.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

CONTROL_WITCHY=${CONTROL_WITCHY:-target/release/witchy}
CANDIDATE_WITCHY=${CANDIDATE_WITCHY:-$CONTROL_WITCHY}
CONTROL_OPT=${CONTROL_OPT:-all}
CANDIDATE_OPT=${CANDIDATE_OPT:-all}
LABEL=${LABEL:-rfc0153}
OUT_DIR=${OUT_DIR:-benchmarks/.build/rfc0153-${LABEL}-$(date -u +%Y%m%d-%H%M%S)}
WORKLOADS=${WORKLOADS:-"fannkuch list_index"}
RUNS=${RUNS:-12}
WARMUP=${WARMUP:-2}
COUNTERS=(
    list_header_loads checked_indexed_loads checked_indexed_stores
    cursorized_indexed_accesses sequence_bounds_checks_coalesced
    sequence_forwarded_loads sequence_small_loops_unrolled
)

[[ $RUNS == 12 ]] || { echo "RFC-0153 requires RUNS=12" >&2; exit 2; }
[[ $WARMUP == 2 ]] || { echo "RFC-0153 requires WARMUP=2" >&2; exit 2; }
for witchy in "$CONTROL_WITCHY" "$CANDIDATE_WITCHY"; do
    [[ -x $witchy ]] || { echo "missing executable: $witchy" >&2; exit 2; }
done
mkdir -p "$OUT_DIR"

kernel_ns() { awk -F= '/^bench_ns=/{ print $2; exit }'; }
result() { grep -v '^bench_ns=' || true; }
sha256() { shasum -a 256 | awk '{print $1}'; }
run_program() { WITCHY_OPT="$2" "$1" sandbox "$3" 2>/dev/null; }
deterministic_stats() {
    WITCHY_OPT="$2" "$1" stats "$3" 2>/dev/null |
        awk -v wanted=" ${COUNTERS[*]} " '$1 != "" && index(wanted, " " $1 " ") { print $1 "\t" $2 }'
}

printf '%s\n' \
    'rfc=0153' "label=$LABEL" "revision=$(git rev-parse HEAD)" \
    "host=$(uname -srvm)" "control_opt=$CONTROL_OPT" "candidate_opt=$CANDIDATE_OPT" \
    "runs=$RUNS" "warmup=$WARMUP" "workloads=$WORKLOADS" \
    "control_sha256=$(shasum -a 256 "$CONTROL_WITCHY" | awk '{print $1}')" \
    "candidate_sha256=$(shasum -a 256 "$CANDIDATE_WITCHY" | awk '{print $1}')" \
    >"$OUT_DIR/metadata.txt"
printf 'workload,mode,sample,kernel_ns\n' >"$OUT_DIR/timing.csv"
printf 'workload,mode,run,counter,value\n' >"$OUT_DIR/counters.csv"
printf 'workload,source_sha256,control_output_sha256,candidate_output_sha256\n' >"$OUT_DIR/correctness.csv"

for workload in $WORKLOADS; do
    source="benchmarks/${workload}.witchy"
    [[ -f $source ]] || { echo "missing workload: $source" >&2; exit 2; }
    control_output=$(run_program "$CONTROL_WITCHY" "$CONTROL_OPT" "$source" | result)
    candidate_output=$(run_program "$CANDIDATE_WITCHY" "$CANDIDATE_OPT" "$source" | result)
    [[ -n $control_output && $control_output == "$candidate_output" ]] || {
        echo "correctness mismatch for $workload" >&2; exit 1;
    }
    printf '%s,%s,%s,%s\n' "$workload" "$(shasum -a 256 "$source" | awk '{print $1}')" \
        "$(printf '%s' "$control_output" | sha256)" "$(printf '%s' "$candidate_output" | sha256)" \
        >>"$OUT_DIR/correctness.csv"

    for mode in control candidate; do
        if [[ $mode == control ]]; then binary=$CONTROL_WITCHY; opt=$CONTROL_OPT; else binary=$CANDIDATE_WITCHY; opt=$CANDIDATE_OPT; fi
        for _ in $(seq 1 "$WARMUP"); do run_program "$binary" "$opt" "$source" >/dev/null; done
        for run in 1 2 3; do
            deterministic_stats "$binary" "$opt" "$source" >"$OUT_DIR/${workload}.${mode}.counter-${run}.tsv"
            WITCHY_OPT="$opt" "$binary" stats "$source" >"$OUT_DIR/${workload}.${mode}.counter-${run}.raw"
            if [[ $run != 1 ]] && ! cmp -s "$OUT_DIR/${workload}.${mode}.counter-1.tsv" "$OUT_DIR/${workload}.${mode}.counter-${run}.tsv"; then
                echo "non-deterministic RFC-0153 counters: $workload/$mode" >&2; exit 1
            fi
            awk -F '\t' -v workload="$workload" -v mode="$mode" -v run="$run" \
                '{ print workload "," mode "," run "," $1 "," $2 }' \
                "$OUT_DIR/${workload}.${mode}.counter-${run}.tsv" >>"$OUT_DIR/counters.csv"
        done
    done

    for sample in $(seq 1 "$RUNS"); do
        if (( sample % 2 )); then order='control candidate'; else order='candidate control'; fi
        for mode in $order; do
            if [[ $mode == control ]]; then binary=$CONTROL_WITCHY; opt=$CONTROL_OPT; else binary=$CANDIDATE_WITCHY; opt=$CANDIDATE_OPT; fi
            ns=$(run_program "$binary" "$opt" "$source" | kernel_ns)
            [[ -n $ns ]] || { echo "missing kernel clock: $workload" >&2; exit 1; }
            printf '%s,%s,%s,%s\n' "$workload" "$mode" "$sample" "$ns" >>"$OUT_DIR/timing.csv"
        done
    done
done

echo "wrote RFC-0153 matrix artifact: $OUT_DIR" >&2
