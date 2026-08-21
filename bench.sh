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

# Colors (if terminal)
if [ -t 1 ]; then
    BOLD="\033[1m"
    GREEN="\033[32m"
    YELLOW="\033[33m"
    CYAN="\033[36m"
    RED="\033[31m"
    RESET="\033[0m"
else
    BOLD=""
    GREEN=""
    YELLOW=""
    CYAN=""
    RED=""
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
            echo "Available benchmarks:"
            for b in "${ALL_BENCHES[@]}"; do
                echo "  - $b"
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
            echo "Error: Unknown option $1" >&2
            echo "Run ./bench.sh --help for usage." >&2
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
    echo -e "${CYAN}==> Building release binary ($WITCHY)...${RESET}"
    cargo build --release -p witchy
fi

mkdir -p "$BUILD_DIR"

# 2. Build required Go binaries
for b in "${TARGETS[@]}"; do
    if [ ! -f "$BENCH_DIR/${b}.witchy" ]; then
        echo -e "${RED}Error: Benchmark '$b' not found in benchmarks/${b}.witchy${RESET}" >&2
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
    echo -e "${CYAN}==> Running full benchmark suite via benchmarks/run.sh...${RESET}"
    (cd "$BENCH_DIR" && WITCHY="$WITCHY" ./run.sh)
    exit 0
fi

# Fast / Quick mode
echo -e "${BOLD}Witchy Benchmark Suite ($MODE mode, $RUNS sample(s))${RESET}"
echo -e "${CYAN}Kernel: in-program monotonic compute clock; < 1.00x beats Go${RESET}"
echo

printf "%-16s %15s %15s %15s %10s\n" "Benchmark" "Witchy (ms)" "Go (ms)" "vs Go" "Status"
printf "%-16s %15s %15s %15s %10s\n" "----------------" "---------------" "---------------" "---------------" "----------"

for b in "${TARGETS[@]}"; do
    # Correctness check
    g_out=""
    if [ -f "$BUILD_DIR/${b}_go" ]; then
        g_out=$("$BUILD_DIR/${b}_go" | result)
    fi
    w_out=$("$WITCHY" sandbox "$BENCH_DIR/${b}.witchy" 2>/dev/null | result)

    status="${GREEN}OK${RESET}"
    if [ -n "$g_out" ] && [ "$w_out" != "$g_out" ]; then
        status="${RED}MISMATCH${RESET}"
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

    if [ -n "$wns" ] && [ -n "$gns" ] && [ "$gns" -gt 0 ]; then
        w_ms=$(awk -v ns="$wns" 'BEGIN { printf "%.1f", ns / 1000000 }')
        g_ms=$(awk -v ns="$gns" 'BEGIN { printf "%.1f", ns / 1000000 }')
        ratio=$(awk -v w="$wns" -v g="$gns" 'BEGIN { printf "%.2f", w / g }')
        ratio_str="${ratio}x"
        
        if awk -v r="$ratio" 'BEGIN { exit (r < 1.00 ? 0 : 1) }'; then
            printf "%-16s %12s ms %12s ms   ${GREEN}%8s${RESET}   ${GREEN}%s${RESET}\n" "$b" "$w_ms" "$g_ms" "$ratio_str" "$status"
        else
            printf "%-16s %12s ms %12s ms   %8s   %s\n" "$b" "$w_ms" "$g_ms" "$ratio_str" "$status"
        fi
    elif [ -n "$wns" ]; then
        w_ms=$(awk -v ns="$wns" 'BEGIN { printf "%.1f", ns / 1000000 }')
        printf "%-16s %12s ms %15s   %8s   %s\n" "$b" "$w_ms" "—" "—" "$status"
    else
        printf "%-16s %15s %15s   %8s   %s\n" "$b" "(wall-only)" "—" "—" "$status"
    fi
done

echo
