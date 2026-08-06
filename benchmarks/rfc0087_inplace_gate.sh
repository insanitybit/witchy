#!/usr/bin/env bash
# RFC-0087 / RFC-0051 focused performance gate.
#
# This is deliberately a same-binary, same-machine comparison:
#   * optimized (`WITCHY_OPT=all`) must complete every named kernel;
#   * forced copy (`WITCHY_OPT=-inplace`) must preserve output whenever it
#     completes, and must be measurably slower or hit the bounded resource cliff;
#   * an optional REFERENCE snapshot applies RFC-0051's 5% optimized-kernel
#     non-regression threshold without pretending timings are portable between
#     machines.
#
# Usage:
#   ./benchmarks/rfc0087_inplace_gate.sh
#   RUNS=5 REFERENCE=before.tsv ./benchmarks/rfc0087_inplace_gate.sh
#
# The emitted TSV is suitable as the next run's REFERENCE:
#   benchmark<TAB>optimized_ns<TAB>forced_ns_or_status<TAB>forced/optimized
set -euo pipefail

cd "$(dirname "$0")"

WITCHY="${WITCHY:-../target/release/witchy}"
RUNS="${RUNS:-3}"
WARMUP="${WARMUP:-1}"
TIMEOUT_SECONDS="${TIMEOUT_SECONDS:-90}"
REFERENCE="${REFERENCE:-rfc0087_inplace_reference.tsv}"
OUTPUT="${OUTPUT:-.build/rfc0087-inplace-current.tsv}"
BENCHES=(
    word_count
    dict_count
    list_sum
    knucleotide
    list_index
    binary_trees
    expr_eval
)

if [[ ! -x "$WITCHY" ]]; then
    echo "error: witchy binary not found at $WITCHY" >&2
    exit 2
fi
if ! [[ "$RUNS" =~ ^[1-9][0-9]*$ && "$WARMUP" =~ ^[0-9]+$ ]]; then
    echo "error: RUNS must be positive and WARMUP must be non-negative" >&2
    exit 2
fi
if ! [[ "$TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: TIMEOUT_SECONDS must be positive" >&2
    exit 2
fi
if [[ -n "$REFERENCE" && ! -f "$REFERENCE" ]]; then
    echo "error: reference snapshot not found: $REFERENCE" >&2
    exit 2
fi

mkdir -p "$(dirname "$OUTPUT")"

result_text() {
    sed '/^bench_ns=/d'
}

kernel_ns() {
    sed -n 's/^bench_ns=//p' | head -1
}

run_limited() {
    local seconds="$1"
    shift
    perl -e '
        my $seconds = shift @ARGV;
        $SIG{ALRM} = sub { exit 124 };
        alarm $seconds;
        exec @ARGV or exit 127;
    ' "$seconds" "$@"
}

sample() {
    local mode="$1" bench="$2"
    run_limited "$TIMEOUT_SECONDS" \
        env "WITCHY_OPT=$mode" "$WITCHY" sandbox "${bench}.witchy"
}

best_kernel() {
    local mode="$1" bench="$2"
    local best="" output ns status
    for _ in $(seq 1 "$WARMUP"); do
        sample "$mode" "$bench" >/dev/null 2>&1 || true
    done
    for _ in $(seq 1 "$RUNS"); do
        set +e
        output="$(sample "$mode" "$bench" 2>/dev/null)"
        status=$?
        set -e
        if [[ "$status" -ne 0 ]]; then
            echo "status:$status"
            return
        fi
        ns="$(printf '%s\n' "$output" | kernel_ns)"
        if [[ -z "$ns" || ! "$ns" =~ ^[0-9]+$ ]]; then
            echo "status:missing-bench-ns"
            return
        fi
        if [[ -z "$best" || "$ns" -lt "$best" ]]; then
            best="$ns"
        fi
    done
    echo "$best"
}

reference_ns() {
    local bench="$1"
    awk -F '\t' -v bench="$bench" '
        $1 == bench && $2 ~ /^[0-9]+$/ { print $2; exit }
    ' "$REFERENCE"
}

ratio_at_least() {
    local forced="$1" optimized="$2" minimum="$3"
    awk -v forced="$forced" -v optimized="$optimized" -v minimum="$minimum" \
        'BEGIN { exit !(forced / optimized >= minimum) }'
}

within_five_percent() {
    local current="$1" reference="$2"
    awk -v current="$current" -v reference="$reference" \
        'BEGIN { exit !(current <= reference * 1.05) }'
}

has_five_percent_reference_gate() {
    case "$1" in
        list_index | binary_trees | expr_eval) return 0 ;;
        *) return 1 ;;
    esac
}

minimum_ratio() {
    case "$1" in
        word_count | dict_count | list_sum | knucleotide) echo "1.25" ;;
        list_index) echo "1.10" ;;
        binary_trees | expr_eval) echo "1.05" ;;
    esac
}

forced_copy_may_hit_resource_cliff() {
    case "$1" in
        word_count | dict_count | list_sum | knucleotide) return 0 ;;
        *) return 1 ;;
    esac
}

is_expected_resource_cliff() {
    local status="$1" output="$2"
    [[ "$status" -eq 124 ]] && return 0
    printf '%s\n' "$output" |
        grep -Eiq 'out of memory|out of bounds memory access|memory limit|heap[^[:space:]]* (exhaust|full)|allocation[^[:space:]]* fail'
}

printf 'benchmark\toptimized_ns\tforced_copy\tratio\n' > "$OUTPUT"
failed=0

for bench in "${BENCHES[@]}"; do
    set +e
    optimized_output="$(sample all "$bench" 2>&1)"
    optimized_status=$?
    set -e
    if [[ "$optimized_status" -ne 0 ]]; then
        echo "$bench: optimized run failed with status $optimized_status: $optimized_output" >&2
        failed=1
        continue
    fi
    optimized_result="$(printf '%s\n' "$optimized_output" | result_text)"
    optimized="$(best_kernel all "$bench")"
    if [[ ! "$optimized" =~ ^[0-9]+$ ]]; then
        echo "$bench: optimized samples failed ($optimized)" >&2
        failed=1
        continue
    fi

    set +e
    forced_output="$(sample -inplace "$bench" 2>&1)"
    forced_status=$?
    set -e
    if [[ "$forced_status" -eq 0 ]]; then
        forced_result="$(printf '%s\n' "$forced_output" | result_text)"
        if [[ "$forced_result" != "$optimized_result" ]]; then
            echo "$bench: forced-copy output diverged from optimized output" >&2
            failed=1
            continue
        fi
    fi

    if [[ "$forced_status" -eq 0 ]]; then
        forced="$(best_kernel -inplace "$bench")"
        if [[ ! "$forced" =~ ^[0-9]+$ ]]; then
            echo "$bench: forced-copy sampling failed after its parity run completed ($forced)" >&2
            failed=1
            continue
        fi
    else
        forced="status:$forced_status"
    fi
    if [[ "$forced" =~ ^[0-9]+$ ]]; then
        ratio="$(awk -v forced="$forced" -v optimized="$optimized" \
            'BEGIN { printf "%.3f", forced / optimized }')"
        minimum="$(minimum_ratio "$bench")"
        if ! ratio_at_least "$forced" "$optimized" "$minimum"; then
            echo "$bench: in-place path did not show the required firing margin: ratio=$ratio minimum=$minimum" >&2
            failed=1
        fi
        forced_cell="$forced"
    else
        ratio="resource-cliff"
        forced_cell="$forced"
        if ! forced_copy_may_hit_resource_cliff "$bench" \
            || ! is_expected_resource_cliff "$forced_status" "$forced_output"
        then
            echo "$bench: forced-copy run failed for a non-resource reason ($forced): $forced_output" >&2
            failed=1
        fi
    fi

    if [[ -n "$REFERENCE" ]] && has_five_percent_reference_gate "$bench"; then
        reference="$(reference_ns "$bench")"
        if [[ -z "$reference" ]]; then
            echo "$bench: missing optimized reference row in $REFERENCE" >&2
            failed=1
        elif ! within_five_percent "$optimized" "$reference"; then
            change="$(awk -v current="$optimized" -v reference="$reference" \
                'BEGIN { printf "%+.2f%%", (current / reference - 1) * 100 }')"
            echo "$bench: optimized kernel regressed beyond RFC-0051's 5% limit ($change)" >&2
            failed=1
        fi
    fi

    printf '%s\t%s\t%s\t%s\n' \
        "$bench" "$optimized" "$forced_cell" "$ratio" >> "$OUTPUT"
    echo "$bench optimized=$optimized forced=$forced_cell ratio=$ratio"
done

if [[ "$failed" -ne 0 ]]; then
    echo "RFC-0087 in-place performance gate failed; snapshot: $OUTPUT" >&2
    exit 1
fi

echo "RFC-0087 in-place performance gate passed; snapshot: $OUTPUT"
