#!/usr/bin/env bash
# Compare explicitly built compiler binaries; never swap source or rebuild.
# Both executables read the SAME workload sources and WITCHY_STD_DIR. This is
# a compiler implementation comparison, not a comparison of std revisions.
#
# Usage:
#   ./scripts/compile-bench.sh --binary /path/to/candidate
#   ./scripts/compile-bench.sh --binary /path/to/candidate --against-binary /path/to/baseline
#   ./scripts/compile-bench.sh --rounds 5 --only async --output-dir state/compile-samples
#
# real/stdlib measure frontend checking; corpus/async measure full emit-wat.
# Cold verification is the default. For warm-cache runs, explicitly set
# WITCHY_SAFEWIR_CHECK_CACHE=on and WITCHY_SAFEWIR_CHECK_CACHE_DIR.
# One priming pair precedes alternating measured pairs. Raw samples are kept;
# medians do not eliminate host contention. Failed runs are never results.
set -euo pipefail
cd "$(dirname "$0")/.."
rounds=3
binary="target/debug/witchy"
against=""
only=""
output=""
while [ $# -gt 0 ]; do
    case "$1" in
        --binary) binary="${2:?--binary needs a path}"; shift 2 ;;
        --against-binary) against="${2:?--against-binary needs a path}"; shift 2 ;;
        --against) echo 'compile-bench: source-swapping --against was removed; build the ref in isolation and use --against-binary PATH' >&2; exit 2 ;;
        --rounds) rounds="${2:?--rounds needs a count}"; shift 2 ;;
        --only) only="${2:?--only needs a workload name}"; shift 2 ;;
        --output-dir) output="${2:?--output-dir needs a path}"; shift 2 ;;
        --help|-h) sed -n '2,/^set -euo pipefail$/{/^set -euo pipefail$/d; s/^# \{0,1\}//; p; }' "$0"; exit 0 ;;
        *) echo "compile-bench: unknown argument '$1'" >&2; exit 2 ;;
    esac
done
case "$rounds" in ''|*[!0-9]*) echo 'compile-bench: rounds must be a positive integer' >&2; exit 2 ;; esac
[ "$rounds" -gt 0 ] 2>/dev/null || { echo 'compile-bench: rounds must be a positive integer' >&2; exit 2; }
case "$only" in ''|real|stdlib|corpus|async) ;; *) echo "compile-bench: unknown workload '$only'" >&2; exit 2 ;; esac
executables=("$binary")
[ -z "$against" ] || executables+=("$against")
for executable in "${executables[@]}"; do
    [ -x "$executable" ] && [ ! -d "$executable" ] || {
        echo "compile-bench: missing executable $executable; build it explicitly first" >&2; exit 2;
    }
done
if [ -z "$output" ]; then
    mkdir -p state
    output="$(mktemp -d "$PWD/state/compile-bench.XXXXXX")"
else
    # Never mix old successful samples with a new failed run.
    mkdir "$output" || { echo 'compile-bench: output directory must be new' >&2; exit 2; }
fi
export WITCHY_STD_DIR="${WITCHY_STD_DIR:-$PWD/std}"
export WITCHY_SAFEWIR_CHECK_CACHE="${WITCHY_SAFEWIR_CHECK_CACHE:-off}"
export RUST_MIN_STACK=16777216
export LC_ALL=C
{
    printf 'source_tip=%s\nbinary=%s\nagainst=%s\nstd=%s\nverdict_cache=%s\n' \
        "$(git rev-parse HEAD)" "$binary" "$against" "$WITCHY_STD_DIR" "$WITCHY_SAFEWIR_CHECK_CACHE"
    git status --short
    uptime
    for executable in "${executables[@]}"; do openssl dgst -sha256 "$executable"; done
} > "$output/metadata.txt"
printf 'round\tvariant\tcpu_s\twall_s\n' > "$output/samples.tsv"
workloads=(
    'real:check:projects/grimoire/src/gm_core.witchy'
    'stdlib:check:std/json.witchy'
    'corpus:emit-wat:tests/rfc0165/corpus/025-aggregates_with_string_payloads_construct_match_and_propagate.witchy'
    'async:emit-wat:benchmarks/compile_async.witchy'
)
timed() {
    local variant="$1" round="$2" command="$3" source="$4" executable="$binary" status
    [ "$variant" != baseline ] || executable="$against"
    if /usr/bin/time -p "$executable" "$command" "$source" \
        > "$output/$name-$variant-$round.stdout" 2> "$output/$name-$variant-$round.stderr"; then
        :
    else
        status=$?
        echo "compile-bench: $name $variant failed (exit $status); raw evidence: $output" >&2
        cat "$output/$name-$variant-$round.stderr" >&2
        return "$status"
    fi
    if [ "$round" -gt 0 ]; then
        awk -v r="$round" -v v="$name/$variant" \
            '/^real [0-9]/{w=$2} /^user [0-9]/{u=$2} /^sys [0-9]/{s=$2; found=1}
             END{if (!found) exit 1; printf "%s\t%s\t%.2f\t%s\n",r,v,u+s,w}' \
            "$output/$name-$variant-$round.stderr" >> "$output/samples.tsv"
    fi
}
median() {
    awk -v v="$1" '$2==v {print $3}' "$output/samples.tsv" | sort -n |
        awk '{a[NR]=$1} END{if (NR%2) print a[(NR+1)/2]; else printf "%.3f",(a[NR/2]+a[NR/2+1])/2}'
}
printf 'Raw evidence: %s\n' "$output"
printf '%-8s %-10s %12s %12s\n' workload command candidate_cpu baseline_cpu
for entry in "${workloads[@]}"; do
    name="${entry%%:*}"; rest="${entry#*:}"; command="${rest%%:*}"; source="${rest#*:}"
    [ -z "$only" ] || [ "$only" = "$name" ] || continue
    [ -f "$source" ] || { echo "compile-bench: missing workload $source" >&2; exit 2; }
    round=0
    while [ "$round" -le "$rounds" ]; do
        variants=(candidate)
        if [ -n "$against" ]; then
            if (( round % 2 )); then variants=(baseline candidate); else variants=(candidate baseline); fi
        fi
        for variant in "${variants[@]}"; do timed "$variant" "$round" "$command" "$source"; done
        if [ -n "$against" ] && ! cmp -s "$output/$name-candidate-$round.stdout" "$output/$name-baseline-$round.stdout"; then
            echo "compile-bench: $name output mismatch; raw evidence: $output" >&2; exit 1
        fi
        round=$((round + 1))
    done
    baseline_cpu='-'
    [ -z "$against" ] || baseline_cpu="$(median "$name/baseline")"
    printf '%-8s %-10s %12s %12s\n' "$name" "$command" "$(median "$name/candidate")" "$baseline_cpu"
done
printf 'CPU seconds, median of %s; inspect raw spread and host load before attributing changes.\n' "$rounds"
