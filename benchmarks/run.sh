#!/usr/bin/env bash
# Benchmark harness: compares witchy's native (Rust/LLVM) backend against
# equivalent Go, and — for the CPU benchmarks — its compiled (WASM/wasmtime)
# backend too.
#
# Each benchmark is <name>.witchy and a <name>.go computing the SAME result;
# the harness asserts the backends agree before timing, so we never benchmark a
# program that is silently wrong.
#
# CPU benchmarks run on native + wasm + go. Collection benchmarks (list/dict
# heavy) run on native + go only: the wasm backend's immutable `push`/dict ops
# are O(n^2) at these sizes (a known, separate limitation), so timing it would
# measure that, not the language.
#
# Usage:  ./run.sh                 # native + (wasm for CPU) vs go
#         WITH_INTERP=1 ./run.sh   # also time the interpreter on CPU benchmarks
set -euo pipefail
cd "$(dirname "$0")"

WITCHY="${WITCHY:-../target/release/witchy}"
CPU_BENCHES=(fib loop_sum collatz mandelbrot)
COLL_BENCHES=(list_sum dict_count)
ALL_BENCHES=("${CPU_BENCHES[@]}" "${COLL_BENCHES[@]}")
WARMUP="${WARMUP:-2}"
RUNS="${RUNS:-8}"
BUILD=.build
mkdir -p "$BUILD"

if [ ! -x "$WITCHY" ]; then
    echo "error: witchy binary not found at $WITCHY (run: cargo build --release)" >&2
    exit 1
fi

# Is $1 a CPU benchmark (so wasm participates)?
is_cpu() { for c in "${CPU_BENCHES[@]}"; do [ "$c" = "$1" ] && return 0; done; return 1; }

echo "building Go baselines and witchy-native binaries..."
for b in "${ALL_BENCHES[@]}"; do
    go build -o "$BUILD/${b}_go" "${b}.go"
    "$WITCHY" native -o "$BUILD/${b}_native" "${b}.witchy"
done

echo
echo "correctness (backends must agree):"
ok=1
for b in "${ALL_BENCHES[@]}"; do
    g=$("$BUILD/${b}_go")
    n=$("$BUILD/${b}_native")
    if is_cpu "$b"; then
        w=$("$WITCHY" sandbox "${b}.witchy" 2>/dev/null | tail -1)
    else
        w="$g" # not run on wasm; treat as agreeing for the check
    fi
    if [ "$n" = "$g" ] && [ "$w" = "$g" ]; then
        printf "  %-12s OK   %s\n" "$b" "$g"
    else
        printf "  %-12s MISMATCH  native=%s wasm=%s go=%s\n" "$b" "$n" "$w" "$g"
        ok=0
    fi
done
[ "$ok" = 1 ] || { echo "aborting: outputs disagree" >&2; exit 1; }

echo
echo "timing (warmup=$WARMUP runs=$RUNS)..."
for b in "${ALL_BENCHES[@]}"; do
    cmds=(-n "witchy-native" "$BUILD/${b}_native")
    if is_cpu "$b"; then
        cmds+=(-n "witchy-wasm" "$WITCHY sandbox ${b}.witchy")
        if [ "${WITH_INTERP:-0}" = 1 ]; then
            cmds+=(-n "witchy-interp" "$WITCHY ${b}.witchy")
        fi
    fi
    cmds+=(-n "go" "$BUILD/${b}_go")
    hyperfine -w "$WARMUP" -r "$RUNS" --export-json "$BUILD/${b}.json" \
        "${cmds[@]}" >/dev/null 2>&1 || echo "  (hyperfine failed for $b)" >&2
done

python3 summarize.py "${ALL_BENCHES[@]}"
