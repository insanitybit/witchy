#!/usr/bin/env bash
# Kernel-clock-only benchmark leg (RFC-0051 gate): min self-reported bench_ns
# over RUNS samples per benchmark, witchy compiled tier only. No Go, no
# hyperfine — this measures witchy-before vs witchy-after.
set -euo pipefail
cd "$(dirname "$0")"
WITCHY="${WITCHY:-../target/release/witchy}"
BENCHES=(fib loop_sum collatz mandelbrot closure_calls list_sum dict_count binary_trees word_count expr_eval nsieve fannkuch knucleotide record_build select_fanin list_index)
WARMUP="${WARMUP:-2}"
RUNS="${RUNS:-8}"
kernel_ns() { grep '^bench_ns=' | head -1 | cut -d= -f2 || true; }
for b in "${BENCHES[@]}"; do
    for _ in $(seq 1 "$WARMUP"); do "$WITCHY" sandbox "${b}.witchy" >/dev/null 2>&1 || true; done
    best=""
    for _ in $(seq 1 "$RUNS"); do
        ns=$("$WITCHY" sandbox "${b}.witchy" 2>/dev/null | kernel_ns) || true
        [ -n "$ns" ] || { best="NA"; break; }
        if [ -z "$best" ] || [ "$ns" -lt "$best" ]; then best="$ns"; fi
    done
    printf "%s %s\n" "$b" "$best"
done
