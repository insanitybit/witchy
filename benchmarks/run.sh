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
#         RUNS=12 ./run.sh         # more samples per benchmark
set -euo pipefail
cd "$(dirname "$0")"

WITCHY="${WITCHY:-../target/release/witchy}"
ALL_BENCHES=(fib loop_sum collatz mandelbrot closure_calls list_sum dict_count binary_trees word_count expr_eval nsieve fannkuch knucleotide record_build chan_throughput select_fanin list_index)
# chan_throughput is the async-executor probe (no kernel bracket — an async main);
# it is wall-clock only.
WARMUP="${WARMUP:-2}"
RUNS="${RUNS:-8}"
BUILD=.build
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
    if [ "$w" = "$g" ]; then
        printf "  %-14s OK   %s\n" "$b" "$g"
    else
        printf "  %-14s MISMATCH  wasm=%s go=%s\n" "$b" "$w" "$g"
        ok=0
    fi
done
[ "$ok" = 1 ] || { echo "aborting: outputs disagree" >&2; exit 1; }

# Minimum self-reported kernel-ns over RUNS samples (min = least OS-noise).
min_kernel_ns() {
    local best="" ns
    for _ in $(seq 1 "$RUNS"); do
        ns=$("$@" 2>/dev/null | kernel_ns)
        [ -n "$ns" ] || return 0
        if [ -z "$best" ] || [ "$ns" -lt "$best" ]; then best="$ns"; fi
    done
    echo "$best"
}

echo
echo "kernel timing (min of $RUNS in-program samples)..."
for b in "${ALL_BENCHES[@]}"; do
    # warm witchy's on-disk compile cache so timed runs are artifact hits
    for _ in $(seq 1 "$WARMUP"); do "$WITCHY" sandbox "${b}.witchy" >/dev/null 2>&1 || true; done
    wns=$(min_kernel_ns "$WITCHY" sandbox "${b}.witchy")
    gns=$(min_kernel_ns "$BUILD/${b}_go")
    printf "%s %s\n" "${wns:-NA}" "${gns:-NA}" > "$BUILD/${b}.kernel"
done

echo
echo "wall-clock timing (warmup=$WARMUP runs=$RUNS)..."
for b in "${ALL_BENCHES[@]}"; do
    hyperfine -w "$WARMUP" -r "$RUNS" --export-json "$BUILD/${b}.json" \
        -n "witchy-wasm" "$WITCHY sandbox ${b}.witchy" \
        -n "go" "$BUILD/${b}_go" >/dev/null 2>&1 || echo "  (hyperfine failed for $b)" >&2
done

python3 summarize.py "${ALL_BENCHES[@]}"
