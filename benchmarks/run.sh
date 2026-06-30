#!/usr/bin/env bash
# Benchmark harness: compares witchy's compiled (WASM/wasmtime) tier against
# equivalent Go.
#
# Each benchmark is <name>.witchy and a <name>.go computing the SAME result;
# the harness asserts the backends agree before timing, so we never benchmark a
# program that is silently wrong.
#
# Every benchmark runs on wasm + go: the in-place linear-update paths and the
# dict hash index make the collection benchmarks first-class wasm citizens
# (they used to be O(n^2) under the copying ops and were skipped).
#
# Usage:  ./run.sh                 # wasm vs go
#         WITH_INTERP=1 ./run.sh   # also time the interpreter
set -euo pipefail
cd "$(dirname "$0")"

WITCHY="${WITCHY:-../target/release/witchy}"
ALL_BENCHES=(fib loop_sum collatz mandelbrot closure_calls list_sum dict_count binary_trees word_count expr_eval nsieve fannkuch knucleotide record_build chan_throughput)
WARMUP="${WARMUP:-2}"
RUNS="${RUNS:-8}"
BUILD=.build
mkdir -p "$BUILD"

if [ ! -x "$WITCHY" ]; then
    echo "error: witchy binary not found at $WITCHY (run: cargo build --release)" >&2
    exit 1
fi

echo "building Go baselines..."
for b in "${ALL_BENCHES[@]}"; do
    go build -o "$BUILD/${b}_go" "${b}.go"
done

echo
echo "correctness (backends must agree):"
ok=1
for b in "${ALL_BENCHES[@]}"; do
    g=$("$BUILD/${b}_go")
    w=$("$WITCHY" sandbox "${b}.witchy" 2>/dev/null | tail -1)
    if [ "$w" = "$g" ]; then
        printf "  %-12s OK   %s\n" "$b" "$g"
    else
        printf "  %-12s MISMATCH  wasm=%s go=%s\n" "$b" "$w" "$g"
        ok=0
    fi
done
[ "$ok" = 1 ] || { echo "aborting: outputs disagree" >&2; exit 1; }

echo
echo "timing (warmup=$WARMUP runs=$RUNS)..."
for b in "${ALL_BENCHES[@]}"; do
    cmds=(-n "witchy-wasm" "$WITCHY sandbox ${b}.witchy")
    if [ "${WITH_INTERP:-0}" = 1 ]; then
        cmds+=(-n "witchy-interp" "$WITCHY ${b}.witchy")
    fi
    cmds+=(-n "go" "$BUILD/${b}_go")
    hyperfine -w "$WARMUP" -r "$RUNS" --export-json "$BUILD/${b}.json" \
        "${cmds[@]}" >/dev/null 2>&1 || echo "  (hyperfine failed for $b)" >&2
done

python3 summarize.py "${ALL_BENCHES[@]}"
