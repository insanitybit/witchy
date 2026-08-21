#!/usr/bin/env bash
# Witchy benchmark driver: compares Witchy compiled Wasm vs Go baseline.
#
# Usage:
#   ./bench.sh                 # run fast benchmark sweep across all benchmarks (~3-5s)
#   ./bench.sh fib collatz     # run only specific benchmarks
#   ./bench.sh --quick         # single-pass instant smoke check (<1s)
#   ./bench.sh --full          # full kernel + hyperfine wall-clock suite (~1-2m)
#   ./bench.sh --list          # list available benchmarks
#   ./bench.sh --help          # show this help
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCH_DIR="$ROOT_DIR/benchmarks"
BUILD_DIR="$BENCH_DIR/.build"
WITCHY="${WITCHY:-$ROOT_DIR/target/release/witchy}"

ALL_BENCHES=(
    fib
    loop_sum
    collatz
    mandelbrot
    closure_calls
    list_sum
    dict_count
    binary_trees
    word_count
    expr_eval
    nsieve
    fannkuch
    knucleotide
    record_build
    chan_throughput
    select_fanin
    list_index
)

# Colors (ANSI-C escape literals work reliably across bash, zsh, and macOS terminals)
if [ -t 1 ] || [ "${CLICOLOR_FORCE:-0}" = "1" ]; then
    BOLD=$'\033[1m'
    DIM=$'\033[2m'
    GREEN=$'\033[32m'
    BRIGHT_GREEN=$'\033[1;32m'
    YELLOW=$'\033[33m'
    CYAN=$'\033[36m'
    BRIGHT_CYAN=$'\033[1;36m'
    RED=$'\033[31m'
    BRIGHT_RED=$'\033[1;31m'
    RESET=$'\033[0m'
else
    BOLD=""
    DIM=""
    GREEN=""
    BRIGHT_GREEN=""
    YELLOW=""
    CYAN=""
    BRIGHT_CYAN=""
    RED=""
    BRIGHT_RED=""
    RESET=""
fi

MODE="fast"
RUNS=3
WARMUP=1
TARGETS=()

usage() {
    cat <<EOFU
Usage: ./bench.sh [OPTIONS] [BENCHMARK...]

Options:
  -f, --full     Run full benchmark suite with hyperfine wall-clock timing & update baseline.md
  -q, --quick    Single-sample fast check (<1s)
  -l, --list     List available benchmarks
  -h, --help     Show this help message

Examples:
  ./bench.sh                    # Fast 3-sample sweep of all benchmarks
  ./bench.sh fib collatz        # Run only 'fib' and 'collatz'
  ./bench.sh --full             # Full benchmark with wall-clock timing
  ./bench.sh --quick nsieve     # Quick single-pass check on 'nsieve'
EOFU
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)
            usage
            ;;
        -l|--list)
            printf "Available benchmarks:\n"
            for b in "${ALL_BENCHES[@]}"; do
                printf "  - %s\n" "$b"
            done
            exit 0
            ;;
        -f|--full)
            MODE="full"
            RUNS=8
            WARMUP=2
            shift
            ;;
        -q|--quick)
            MODE="quick"
            RUNS=1
            WARMUP=0
            shift
            ;;
        -*)
            printf "%sError: Unknown option %s%s\n" "$RED" "$1" "$RESET" >&2
            printf "Run ./bench.sh --help for usage.\n" >&2
            exit 1
            ;;
        *)
            TARGETS+=("$1")
            shift
            ;;
    esac
done

if [ ${#TARGETS[@]} -eq 0 ]; then
    TARGETS=("${ALL_BENCHES[@]}")
fi

# 1. Ensure witchy release binary is built
if [ ! -x "$WITCHY" ]; then
    printf "%s==> Building release binary (%s)...%s\n" "$CYAN" "$WITCHY" "$RESET"
    cargo build --release -p witchy
fi

mkdir -p "$BUILD_DIR"

# 2. Build required Go binaries
for b in "${TARGETS[@]}"; do
    if [ ! -f "$BENCH_DIR/${b}.witchy" ]; then
        printf "%sError: Benchmark '%s' not found in benchmarks/%s.witchy%s\n" "$RED" "$b" "$b" "$RESET" >&2
        exit 1
    fi
    if [ -f "$BENCH_DIR/${b}.go" ]; then
        if [ ! -f "$BUILD_DIR/${b}_go" ] || [ "$BENCH_DIR/${b}.go" -nt "$BUILD_DIR/${b}_go" ]; then
            go build -o "$BUILD_DIR/${b}_go" "$BENCH_DIR/${b}.go"
        fi
    fi
done

# Result extraction
result() { grep -v '^bench_ns=' || true; }
kernel_ns() { grep '^bench_ns=' | head -1 | cut -d= -f2 || true; }

min_kernel_ns() {
    local best="" ns
    for _ in $(seq 1 "$RUNS"); do
        ns=$("$@" 2>/dev/null | kernel_ns)
        [ -n "$ns" ] || return 0
        if [ -z "$best" ] || [ "$ns" -lt "$best" ]; then best="$ns"; fi
    done
    echo "$best"
}

if [ "$MODE" = "full" ]; then
    printf "%s==> Running full benchmark suite via benchmarks/run.sh...%s\n" "$CYAN" "$RESET"
    (cd "$BENCH_DIR" && WITCHY="$WITCHY" ./run.sh)
    exit 0
fi

# Fast / Quick mode
printf "\n%s%sWitchy Performance Benchmarks%s %s(%s mode, %s sample(s))%s\n" "$BOLD" "$BRIGHT_CYAN" "$RESET" "$DIM" "$MODE" "$RUNS" "$RESET"
printf "%sKernel: in-program compute clock  |  %s< 1.00x%s beats Go%s\n\n" "$DIM" "$GREEN" "$DIM" "$RESET"

printf "  %-18s %14s %14s %14s    %-8s\n" "Benchmark" "Witchy" "Go" "vs Go" "Status"
printf "  %s\n" "──────────────────────────────────────────────────────────────────────"

total_count=0
pass_count=0
faster_count=0

for b in "${TARGETS[@]}"; do
    total_count=$((total_count + 1))
    
    # Correctness check
    g_out=""
    if [ -f "$BUILD_DIR/${b}_go" ]; then
        g_out=$("$BUILD_DIR/${b}_go" | result)
    fi
    w_out=$("$WITCHY" sandbox "$BENCH_DIR/${b}.witchy" 2>/dev/null | result)

    is_ok=1
    if [ -n "$g_out" ] && [ "$w_out" != "$g_out" ]; then
        is_ok=0
    fi

    # Warm compile cache
    for _ in $(seq 1 "$WARMUP"); do
        "$WITCHY" sandbox "$BENCH_DIR/${b}.witchy" >/dev/null 2>&1 || true
    done

    wns=$(min_kernel_ns "$WITCHY" sandbox "$BENCH_DIR/${b}.witchy")
    gns=""
    if [ -f "$BUILD_DIR/${b}_go" ]; then
        gns=$(min_kernel_ns "$BUILD_DIR/${b}_go")
    fi

    if [ "$is_ok" -eq 1 ]; then
        pass_count=$((pass_count + 1))
        status_disp="${GREEN}OK${RESET}"
    else
        status_disp="${BRIGHT_RED}MISMATCH${RESET}"
    fi

    if [ -n "$wns" ] && [ -n "$gns" ] && [ "$gns" -gt 0 ]; then
        w_ms=$(awk -v ns="$wns" 'BEGIN { printf "%.1f ms", ns / 1000000 }')
        g_ms=$(awk -v ns="$gns" 'BEGIN { printf "%.1f ms", ns / 1000000 }')
        ratio=$(awk -v w="$wns" -v g="$gns" 'BEGIN { printf "%.2f", w / g }')
        ratio_str="${ratio}x"
        
        if awk -v r="$ratio" 'BEGIN { exit (r < 1.00 ? 0 : 1) }'; then
            faster_count=$((faster_count + 1))
            printf "  %-18s %14s %14s %s%14s%s    %s\n" "$b" "$w_ms" "$g_ms" "$BRIGHT_GREEN" "$ratio_str" "$RESET" "$status_disp"
        else
            printf "  %-18s %14s %14s %14s    %s\n" "$b" "$w_ms" "$g_ms" "$ratio_str" "$status_disp"
        fi
    elif [ -n "$wns" ]; then
        w_ms=$(awk -v ns="$wns" 'BEGIN { printf "%.1f ms", ns / 1000000 }')
        printf "  %-18s %14s %14s %14s    %s\n" "$b" "$w_ms" "—" "—" "$status_disp"
    else
        printf "  %-18s %14s %14s %14s    %s\n" "$b" "(wall-only)" "—" "—" "$status_disp"
    fi
done

printf "  %s\n" "──────────────────────────────────────────────────────────────────────"
if [ "$faster_count" -gt 0 ]; then
    printf "  %s%s%d/%d passed%s, %s%d faster than Go%s\n\n" "$BOLD" "$GREEN" "$pass_count" "$total_count" "$RESET" "$BRIGHT_GREEN" "$faster_count" "$RESET"
else
    printf "  %s%s%d/%d passed%s\n\n" "$BOLD" "$GREEN" "$pass_count" "$total_count" "$RESET"
fi
