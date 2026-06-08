#!/usr/bin/env bash
# Benchmark harness: compares witchy's native (Rust/LLVM) and compiled
# (WASM/wasmtime) backends — and optionally its tree-walking interpreter —
# against equivalent Go for each CPU-bound benchmark.
#
# Each benchmark exists as <name>.witchy and <name>.go computing the SAME result;
# the harness asserts every backend agrees before timing, so we never benchmark a
# program that is silently wrong.
#
# Usage:  ./run.sh                 # native + wasm vs go
#         WITH_INTERP=1 ./run.sh   # also time the interpreter (slow)
set -euo pipefail
cd "$(dirname "$0")"

WITCHY="${WITCHY:-../target/release/witchy}"
BENCHES=(fib loop_sum collatz mandelbrot)
WARMUP="${WARMUP:-2}"
RUNS="${RUNS:-8}"
BUILD=.build
mkdir -p "$BUILD"

if [ ! -x "$WITCHY" ]; then
    echo "error: witchy binary not found at $WITCHY (run: cargo build --release)" >&2
    exit 1
fi

echo "building Go baselines and witchy-native binaries..."
for b in "${BENCHES[@]}"; do
    go build -o "$BUILD/${b}_go" "${b}.go"
    "$WITCHY" native -o "$BUILD/${b}_native" "${b}.witchy"
done

echo
echo "correctness (every backend must agree):"
ok=1
for b in "${BENCHES[@]}"; do
    g=$("$BUILD/${b}_go")
    w=$("$WITCHY" sandbox "${b}.witchy" 2>/dev/null | tail -1)
    n=$("$BUILD/${b}_native")
    if [ "$w" = "$g" ] && [ "$n" = "$g" ]; then
        printf "  %-12s OK   %s\n" "$b" "$g"
    else
        printf "  %-12s MISMATCH  native=%s wasm=%s go=%s\n" "$b" "$n" "$w" "$g"
        ok=0
    fi
done
[ "$ok" = 1 ] || { echo "aborting: outputs disagree" >&2; exit 1; }

echo
echo "timing (warmup=$WARMUP runs=$RUNS)..."
for b in "${BENCHES[@]}"; do
    cmds=(-n "witchy-native" "$BUILD/${b}_native"
          -n "witchy-wasm"   "$WITCHY sandbox ${b}.witchy"
          -n "go"            "$BUILD/${b}_go")
    if [ "${WITH_INTERP:-0}" = 1 ]; then
        cmds+=(-n "witchy-interp" "$WITCHY ${b}.witchy")
    fi
    hyperfine -w "$WARMUP" -r "$RUNS" --export-json "$BUILD/${b}.json" \
        "${cmds[@]}" >/dev/null 2>&1 || echo "  (hyperfine failed for $b)" >&2
done

python3 summarize.py "${BENCHES[@]}"
