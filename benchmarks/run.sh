#!/usr/bin/env bash
# Benchmark harness: compares witchy's compiled (WASM/wasmtime) tier against
# equivalent Go, on TWO clocks:
#
#   kernel — the compute time measured INSIDE the program with a monotonic clock
#            (witchy `now_monotonic`, Go `time.Now`), printed as a trailing
#            `bench_ns=<n>` line. This excludes process start + runtime/wasmtime
#            instantiation, so it isolates codegen quality — the thing the
#            optimizer moves. This is the headline number.
#   wall   — end-to-end hyperfine timing of the whole binary (startup INCLUDED).
#            witchy pays a fixed ~10-20ms runtime-startup tax here that Go does
#            not; the gap between wall and kernel is that tax.
#
# Each benchmark is <name>.witchy and a <name>.go computing the SAME result; the
# harness asserts the backends agree (on the result line, ignoring bench_ns)
# before timing, so we never benchmark a program that is silently wrong.
#
# Usage:  ./run.sh                 # wasm vs go, kernel + wall
#         RUNS=12 ./run.sh         # promotion sample count
set -euo pipefail
cd "$(dirname "$0")"

if [ -z "${BENCH_DRIVER_ARGV:-}" ]; then
    BENCH_DRIVER_ARGV=$(printf '%s\034' "$0" "$@")
    export BENCH_DRIVER_ARGV
fi

WITCHY="${WITCHY:-../target/release/witchy}"
ALL_BENCHES=(
  fasta
    fib
    loop_sum
    collatz
    mandelbrot
    closure_calls
    list_sum
    dict_count
    binary_trees
    word_count
    regex-redux
    expr_eval
    nsieve
    fannkuch
    knucleotide
    record_build
    chan_throughput
    select_fanin
    list_index
)
# chan_throughput is the async-executor probe (no kernel bracket — an async main);
# it is wall-clock only.
WARMUP="${WARMUP:-2}"
RUNS="${RUNS:-12}"
BUILD=.build
ARTIFACT_JSON="${ARTIFACT_JSON:-$BUILD/results-full.json}"
mkdir -p "$BUILD"

if [ ! -x "$WITCHY" ]; then
    echo "error: witchy binary not found at $WITCHY (run: cargo build --release)" >&2
    exit 1
fi

# The result line is everything the program prints EXCEPT the trailing bench_ns.
result() { grep -v '^bench_ns=' || true; }
# The compute-kernel nanoseconds the program self-reported (empty if none).
kernel_ns() { grep '^bench_ns=' | head -1 | cut -d= -f2 || true; }

echo "building Go baselines..."
for b in "${ALL_BENCHES[@]}"; do
    go build -o "$BUILD/${b}_go" "${b}.go"
done

echo
echo "correctness (backends must agree on the result):"
ok=1
for b in "${ALL_BENCHES[@]}"; do
    g=$("$BUILD/${b}_go" | result)
    w=$("$WITCHY" sandbox "${b}.witchy" 2>/dev/null | result)
    printf "%s" "$w" > "$BUILD/${b}.result"
    printf "%s" "$g" > "$BUILD/${b}.go.result"
    if [ -n "$w" ] && [ -n "$g" ] && [ "$w" = "$g" ]; then
        printf "  %-14s OK   %s\n" "$b" "$g"
    else
        printf "  %-14s MISMATCH  wasm=%s go=%s\n" "$b" "$w" "$g"
        ok=0
    fi
done
[ "$ok" = 1 ] || { echo "aborting: outputs disagree" >&2; exit 1; }

# Preserve every self-reported sample. The human report may summarize them,
# but promotion evidence must retain the raw values.
collect_paired_kernel_ns() {
    local benchmark="$1" w_output="$2" g_output="$3" w_ns g_ns
    : > "$w_output"
    : > "$g_output"
    for sample in $(seq 1 "$RUNS"); do
        if [ $((sample % 2)) -eq 1 ]; then
            w_ns=$("$WITCHY" sandbox "${benchmark}.witchy" 2>/dev/null | kernel_ns)
            g_ns=$("$BUILD/${benchmark}_go" 2>/dev/null | kernel_ns)
        else
            g_ns=$("$BUILD/${benchmark}_go" 2>/dev/null | kernel_ns)
            w_ns=$("$WITCHY" sandbox "${benchmark}.witchy" 2>/dev/null | kernel_ns)
        fi
        if [ -n "$w_ns" ]; then
            printf "witchy\t%s\n" "$w_ns" >> "$w_output"
        fi
        if [ -n "$g_ns" ]; then
            printf "go\t%s\n" "$g_ns" >> "$g_output"
        fi
    done
}

echo
echo "kernel timing ($RUNS raw in-program samples)..."
for b in "${ALL_BENCHES[@]}"; do
    # Warm both sides before paired, counterbalanced samples.
    for ((warm = 0; warm < WARMUP; warm++)); do
        "$WITCHY" sandbox "${b}.witchy" >/dev/null 2>&1
        "$BUILD/${b}_go" >/dev/null 2>&1
    done
    collect_paired_kernel_ns "$b" "$BUILD/${b}.witchy.kernel.tsv" "$BUILD/${b}.go.kernel.tsv"
done

echo
echo "wall-clock timing (warmup=$WARMUP runs=$RUNS)..."
for b in "${ALL_BENCHES[@]}"; do
    rm -f "$BUILD/${b}.json"
    hyperfine -w "$WARMUP" -r "$RUNS" --export-json "$BUILD/${b}.json" \
        -n "witchy-wasm" "$WITCHY sandbox ${b}.witchy" \
        -n "go" "$BUILD/${b}_go" >/dev/null
done

python3 summarize.py \
    --artifact "$ARTIFACT_JSON" \
    --mode full \
    --witchy "$WITCHY" \
    --warmup "$WARMUP" \
    --runs "$RUNS" \
    --build-dir "$BUILD" \
    --human baseline.md \
    "${ALL_BENCHES[@]}"
echo "machine-readable artifact: $ARTIFACT_JSON"
