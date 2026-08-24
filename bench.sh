#!/usr/bin/env bash
# Witchy benchmark driver: compares Witchy compiled Wasm vs Go / Node / Ruby / Rust baselines.
#
# Usage:
#   ./bench.sh                 # run fast benchmark sweep across all benchmarks (~3-5s)
#   ./bench.sh fib collatz     # run only specific benchmarks
#   ./bench.sh --quick         # single-pass instant smoke check (<1s)
#   ./bench.sh --full          # full kernel + hyperfine wall-clock suite (~1-2m)
#   ./bench.sh --compare LANG  # compare against specific language (go, node, ruby, rust)
#   ./bench.sh --json FILE     # write the machine-readable artifact to FILE
#   ./bench.sh --list          # list available benchmarks
#   ./bench.sh --help          # show this help
set -euo pipefail

RUBY_CMD=()
if [ -x "/opt/homebrew/opt/ruby/bin/ruby" ]; then
    RUBY_CMD=("/opt/homebrew/opt/ruby/bin/ruby")
elif [ -x "/opt/homebrew/bin/ruby" ]; then
    RUBY_CMD=("/opt/homebrew/bin/ruby")
elif [ -x "/usr/local/opt/ruby/bin/ruby" ]; then
    RUBY_CMD=("/usr/local/opt/ruby/bin/ruby")
elif command -v ruby >/dev/null 2>&1; then
    RUBY_CMD=("$(command -v ruby)")
fi

if [ ${#RUBY_CMD[@]} -gt 0 ]; then
    if "${RUBY_CMD[@]}" --yjit -e "" >/dev/null 2>&1; then
        RUBY_CMD+=("--yjit")
    fi
fi

# Run a command with an optional timeout without aborting the parent script on signal/exit code
run_timed() {
    local timeout_secs="${BENCH_TIMEOUT:-30}"
    if command -v timeout >/dev/null 2>&1; then
        timeout "$timeout_secs" "$@" 2>/dev/null || true
    else
        "$@" 2>/dev/null || true
    fi
}

extract_result() {
    printf "%s\n" "$1" | grep -v '^bench_ns=' || true
}

extract_kernel_ns() {
    printf "%s\n" "$1" | grep '^bench_ns=' | head -1 | cut -d= -f2 || true
}

ORIGINAL_ARGS=("$@")

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCH_DIR="$ROOT_DIR/benchmarks"
BUILD_DIR="$BENCH_DIR/.build"
WITCHY_EXPLICIT=0
if [ "${WITCHY+x}" = "x" ]; then
    WITCHY_EXPLICIT=1
fi
WITCHY="${WITCHY:-${CARGO_TARGET_DIR:-$ROOT_DIR/target}/release/witchy}"

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
    chan_scheduler
    select_fanin
    list_index
    reverse-complement
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
ARTIFACT_JSON=""
PRINT_BUILD_INPUTS=0
COMPARE_LANG=""

usage() {
    cat <<EOFU
Usage: ./bench.sh [OPTIONS] [BENCHMARK...]

Options:
  -f, --full     Run full benchmark suite with hyperfine wall-clock timing & update baseline.md
  -q, --quick    Single-sample fast check (<1s)
  -c, --compare LANG Compare against a specific language (go, node, ruby, rust) instead of Fastest
      --json     Machine-readable artifact path (default: benchmarks/.build/results-MODE.json)
      --print-build-inputs
                 Print compiler build inputs used by stale detection, then exit
  -l, --list     List available benchmarks
  -h, --help     Show this help message

Examples:
  ./bench.sh                    # Fast 3-sample sweep of all benchmarks
  ./bench.sh fib collatz        # Run only 'fib' and 'collatz'
  ./bench.sh --compare node     # Compare Witchy vs Node.js
  ./bench.sh --compare ruby     # Compare Witchy vs Ruby (with YJIT if available)
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
            RUNS=12
            WARMUP=2
            shift
            ;;
        -q|--quick)
            MODE="quick"
            RUNS=1
            WARMUP=0
            shift
            ;;
        --json)
            [ $# -ge 2 ] || { printf "Error: --json requires a path\n" >&2; exit 1; }
            ARTIFACT_JSON="$2"
            shift 2
            ;;
        -c|--compare)
            [ $# -ge 2 ] || { printf "Error: --compare requires a language (go, node, ruby, rust)\n" >&2; exit 1; }
            COMPARE_LANG="$2"
            shift 2
            ;;
        --print-build-inputs)
            PRINT_BUILD_INPUTS=1
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

build_inputs() {
    git -C "$ROOT_DIR" ls-files --cached --others --exclude-standard -- \
        Cargo.toml Cargo.lock build.rs .cargo src crates std projects menus web
}

if [ "$PRINT_BUILD_INPUTS" -eq 1 ]; then
    build_inputs
    exit 0
fi

# 1. Ensure witchy release binary is present and newer than compiler inputs.
stale_input=""
if [ -x "$WITCHY" ]; then
    while IFS= read -r input; do
        if [ -f "$ROOT_DIR/$input" ] && [ "$ROOT_DIR/$input" -nt "$WITCHY" ]; then
            stale_input="$ROOT_DIR/$input"
            break
        fi
    done < <(build_inputs)
fi
if [ "$WITCHY_EXPLICIT" -eq 1 ] && { [ ! -x "$WITCHY" ] || [ -n "$stale_input" ]; }; then
    if [ ! -x "$WITCHY" ]; then
        printf "%sError: explicitly supplied WITCHY is not executable: %s%s\n" "$BRIGHT_RED" "$WITCHY" "$RESET" >&2
    else
        printf "%sError: explicitly supplied WITCHY is stale; newer input: %s%s\n" "$BRIGHT_RED" "$stale_input" "$RESET" >&2
    fi
    exit 1
fi
if [ ! -x "$WITCHY" ] || [ -n "$stale_input" ]; then
    printf "%s==> Building release binary (%s)...%s\n" "$CYAN" "$WITCHY" "$RESET"
    cargo build --release -p witchy
fi
[ -x "$WITCHY" ] || { printf "Error: release build did not produce %s\n" "$WITCHY" >&2; exit 1; }

mkdir -p "$BUILD_DIR"
if [ -z "$ARTIFACT_JSON" ]; then
    if [ "${#TARGETS[@]}" -eq "${#ALL_BENCHES[@]}" ] && [ "${TARGETS[*]}" = "${ALL_BENCHES[*]}" ]; then
        ARTIFACT_JSON="$BUILD_DIR/results-$MODE.json"
    else
        target_slug=$(IFS=-; printf "%s" "${TARGETS[*]}")
        ARTIFACT_JSON="$BUILD_DIR/results-$MODE-$target_slug.json"
    fi
fi
if [[ "$ARTIFACT_JSON" != /* ]]; then
    ARTIFACT_JSON="$ROOT_DIR/$ARTIFACT_JSON"
fi
if [ ${#ORIGINAL_ARGS[@]} -eq 0 ]; then
    BENCH_DRIVER_ARGV=$(printf '%s\034' "$0")
else
    BENCH_DRIVER_ARGV=$(printf '%s\034' "$0" "${ORIGINAL_ARGS[@]}")
fi
export BENCH_DRIVER_ARGV

# 2. Build required binaries
for b in "${TARGETS[@]}"; do
    if [ ! -f "$BENCH_DIR/${b}.witchy" ]; then
        printf "%sError: Benchmark '%s' not found in benchmarks/%s.witchy%s\n" "$RED" "$b" "$b" "$RESET" >&2
        exit 1
    fi
    if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "go" ]; then
        if [ -f "$BENCH_DIR/${b}.go" ] && command -v go >/dev/null 2>&1; then
            if [ ! -f "$BUILD_DIR/${b}_go" ] || [ "$BENCH_DIR/${b}.go" -nt "$BUILD_DIR/${b}_go" ]; then
                go build -o "$BUILD_DIR/${b}_go" "$BENCH_DIR/${b}.go"
            fi
        fi
    fi
    if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "rust" ]; then
        if [ -f "$BENCH_DIR/${b}.rs" ] && command -v rustc >/dev/null 2>&1; then
            if [ ! -f "$BUILD_DIR/${b}_rs" ] || [ "$BENCH_DIR/${b}.rs" -nt "$BUILD_DIR/${b}_rs" ]; then
                regex_flag=()
                deps_dir="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/release/deps"
                if ls "$deps_dir"/libregex-*.rlib >/dev/null 2>&1; then
                    regex_lib=$(ls "$deps_dir"/libregex-*.rlib | head -n 1)
                    regex_flag=(-L "dependency=$deps_dir" --extern "regex=$regex_lib")
                fi
                rustc --edition 2021 -O "${regex_flag[@]}" -o "$BUILD_DIR/${b}_rs" "$BENCH_DIR/${b}.rs"
            fi
        fi
    fi
done

collect_all_kernel_ns() {
    local benchmark="$1" w_output="$2" g_output="$3" n_output="$4" r_output="$5" rs_output="$6"
    : > "$w_output"
    : > "$g_output"
    : > "$n_output"
    : > "$r_output"
    : > "$rs_output"
    for sample in $(seq 1 "$RUNS"); do
        local w_raw="" g_raw="" n_raw="" r_raw="" rs_raw=""
        local w_ns="" g_ns="" n_ns="" r_ns="" rs_ns=""
        
        if [ $((sample % 2)) -eq 1 ]; then
            w_raw=$(run_timed "$WITCHY" sandbox "$BENCH_DIR/${benchmark}.witchy")
            w_ns=$(extract_kernel_ns "$w_raw")
            if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "go" ]; then
                if [ -f "$BUILD_DIR/${benchmark}_go" ]; then
                    g_raw=$(run_timed "$BUILD_DIR/${benchmark}_go")
                    g_ns=$(extract_kernel_ns "$g_raw")
                fi
            fi
        else
            if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "go" ]; then
                if [ -f "$BUILD_DIR/${benchmark}_go" ]; then
                    g_raw=$(run_timed "$BUILD_DIR/${benchmark}_go")
                    g_ns=$(extract_kernel_ns "$g_raw")
                fi
            fi
            w_raw=$(run_timed "$WITCHY" sandbox "$BENCH_DIR/${benchmark}.witchy")
            w_ns=$(extract_kernel_ns "$w_raw")
        fi
        
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "node" ]; then
            if command -v node >/dev/null 2>&1 && [ -f "$BENCH_DIR/${benchmark}.js" ]; then
                n_raw=$(run_timed node "$BENCH_DIR/${benchmark}.js")
                n_ns=$(extract_kernel_ns "$n_raw")
            fi
        fi
        
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "ruby" ]; then
            if [ ${#RUBY_CMD[@]} -gt 0 ] && [ -f "$BENCH_DIR/${benchmark}.rb" ]; then
                r_raw=$(run_timed "${RUBY_CMD[@]}" "$BENCH_DIR/${benchmark}.rb")
                r_ns=$(extract_kernel_ns "$r_raw")
            fi
        fi
        
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "rust" ]; then
            if [ -f "$BUILD_DIR/${benchmark}_rs" ]; then
                rs_raw=$(run_timed "$BUILD_DIR/${benchmark}_rs")
                rs_ns=$(extract_kernel_ns "$rs_raw")
            fi
        fi

        if [ -n "$w_ns" ]; then printf "witchy\t%s\n" "$w_ns" >> "$w_output"; fi
        if [ -n "$g_ns" ]; then printf "go\t%s\n" "$g_ns" >> "$g_output"; fi
        if [ -n "$n_ns" ]; then printf "node\t%s\n" "$n_ns" >> "$n_output"; fi
        if [ -n "$r_ns" ]; then printf "ruby\t%s\n" "$r_ns" >> "$r_output"; fi
        if [ -n "$rs_ns" ]; then printf "rust\t%s\n" "$rs_ns" >> "$rs_output"; fi
    done
}

if [ "$MODE" = "full" ]; then
    printf "%s==> Running full benchmark suite via benchmarks/run.sh...%s\n" "$CYAN" "$RESET"
    (cd "$BENCH_DIR" && WITCHY="$WITCHY" ARTIFACT_JSON="$ARTIFACT_JSON" RUNS="$RUNS" WARMUP="$WARMUP" ./run.sh)
    exit 0
fi

# Fast / Quick mode
printf "\n%s%sWitchy Performance Benchmarks%s %s(%s mode, %s sample(s))%s\n" "$BOLD" "$BRIGHT_CYAN" "$RESET" "$DIM" "$MODE" "$RUNS" "$RESET"
HEAD_FORMAT="  %-18s %14s"
HEAD_ARGS=("Benchmark" "Witchy")
if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "go" ]; then HEAD_FORMAT+=" %14s"; HEAD_ARGS+=("Go"); fi
if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "node" ]; then HEAD_FORMAT+=" %14s"; HEAD_ARGS+=("Node.js"); fi
if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "ruby" ]; then HEAD_FORMAT+=" %14s"; HEAD_ARGS+=("Ruby"); fi
if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "rust" ]; then HEAD_FORMAT+=" %14s"; HEAD_ARGS+=("Rust"); fi
HEAD_FORMAT+=" %14s    %-8s\n"

if [ -n "$COMPARE_LANG" ]; then
    compare_title="vs $(tr '[:lower:]' '[:upper:]' <<< ${COMPARE_LANG:0:1})${COMPARE_LANG:1}"
    printf "%sKernel: in-program compute clock  |  %s< 1.00x%s beats %s baseline%s\n\n" "$DIM" "$GREEN" "$DIM" "$COMPARE_LANG" "$RESET"
    HEAD_ARGS+=("$compare_title" "Status")
    printf "$HEAD_FORMAT" "${HEAD_ARGS[@]}"
else
    printf "%sKernel: in-program compute clock  |  %s< 1.00x%s beats fastest baseline%s\n\n" "$DIM" "$GREEN" "$DIM" "$RESET"
    HEAD_ARGS+=("vs Fastest" "Status")
    printf "$HEAD_FORMAT" "${HEAD_ARGS[@]}"
fi

LINE_DASHES=""
for ((i=0; i<${#HEAD_ARGS[@]}*14 + 11; i++)); do LINE_DASHES+="─"; done
printf "  %s\n" "$LINE_DASHES"

total_count=0
pass_count=0
faster_count=0

for b in "${TARGETS[@]}"; do
    total_count=$((total_count + 1))
    
    # Correctness check: compare Witchy output against the active comparator or available baseline
    ref_out=""
    has_ref=0

    if [ -n "$COMPARE_LANG" ]; then
        case "$COMPARE_LANG" in
            go)
                if [ -f "$BUILD_DIR/${b}_go" ]; then
                    ref_out=$(extract_result "$("$BUILD_DIR/${b}_go" 2>/dev/null || true)")
                    has_ref=1
                fi
                ;;
            node)
                if command -v node >/dev/null 2>&1 && [ -f "$BENCH_DIR/${b}.js" ]; then
                    ref_out=$(extract_result "$(node "$BENCH_DIR/${b}.js" 2>/dev/null || true)")
                    has_ref=1
                fi
                ;;
            ruby)
                if [ ${#RUBY_CMD[@]} -gt 0 ] && [ -f "$BENCH_DIR/${b}.rb" ]; then
                    ref_out=$(extract_result "$("${RUBY_CMD[@]}" "$BENCH_DIR/${b}.rb" 2>/dev/null || true)")
                    has_ref=1
                fi
                ;;
            rust)
                if [ -f "$BUILD_DIR/${b}_rs" ]; then
                    ref_out=$(extract_result "$("$BUILD_DIR/${b}_rs" 2>/dev/null || true)")
                    has_ref=1
                fi
                ;;
        esac
    fi

    if [ "$has_ref" -eq 0 ]; then
        if [ -f "$BUILD_DIR/${b}_go" ]; then
            ref_out=$(extract_result "$("$BUILD_DIR/${b}_go" 2>/dev/null || true)")
            has_ref=1
        elif command -v node >/dev/null 2>&1 && [ -f "$BENCH_DIR/${b}.js" ]; then
            ref_out=$(extract_result "$(node "$BENCH_DIR/${b}.js" 2>/dev/null || true)")
            has_ref=1
        elif [ ${#RUBY_CMD[@]} -gt 0 ] && [ -f "$BENCH_DIR/${b}.rb" ]; then
            ref_out=$(extract_result "$("${RUBY_CMD[@]}" "$BENCH_DIR/${b}.rb" 2>/dev/null || true)")
            has_ref=1
        elif [ -f "$BUILD_DIR/${b}_rs" ]; then
            ref_out=$(extract_result "$("$BUILD_DIR/${b}_rs" 2>/dev/null || true)")
            has_ref=1
        fi
    fi

    w_raw=$("$WITCHY" sandbox "$BENCH_DIR/${b}.witchy" 2>/dev/null || true)
    w_out=$(extract_result "$w_raw")
    printf "%s" "$w_out" > "$BUILD_DIR/${b}.result"
    if [ "$has_ref" -eq 1 ]; then
        printf "%s" "$ref_out" > "$BUILD_DIR/${b}.go.result"
    else
        printf "%s" "$w_out" > "$BUILD_DIR/${b}.go.result"
    fi

    is_ok=1
    if [ "$has_ref" -eq 1 ]; then
        if [ "$w_out" != "$ref_out" ]; then
            is_ok=0
        fi
    else
        if [ -z "$w_raw" ]; then
            is_ok=0
        fi
    fi

    # Warm compile cache
    for ((warm = 0; warm < WARMUP; warm++)); do
        "$WITCHY" sandbox "$BENCH_DIR/${b}.witchy" >/dev/null 2>&1 || true
        if [ -f "$BUILD_DIR/${b}_go" ]; then
            "$BUILD_DIR/${b}_go" >/dev/null 2>&1 || true
        fi
    done

    collect_all_kernel_ns "$b" "$BUILD_DIR/${b}.witchy.kernel.tsv" "$BUILD_DIR/${b}.go.kernel.tsv" "$BUILD_DIR/${b}.node.kernel.tsv" "$BUILD_DIR/${b}.ruby.kernel.tsv" "$BUILD_DIR/${b}.rust.kernel.tsv"
    
    wns=""
    if [ -s "$BUILD_DIR/${b}.witchy.kernel.tsv" ]; then
        wns=$(awk -F '\t' 'NR == 1 || $2 < best { best=$2 } END { print best }' "$BUILD_DIR/${b}.witchy.kernel.tsv")
    fi
    gns=""
    if [ -s "$BUILD_DIR/${b}.go.kernel.tsv" ]; then
        gns=$(awk -F '\t' 'NR == 1 || $2 < best { best=$2 } END { print best }' "$BUILD_DIR/${b}.go.kernel.tsv")
    fi
    nns=""
    if [ -s "$BUILD_DIR/${b}.node.kernel.tsv" ]; then
        nns=$(awk -F '\t' 'NR == 1 || $2 < best { best=$2 } END { print best }' "$BUILD_DIR/${b}.node.kernel.tsv")
    fi
    rns=""
    if [ -s "$BUILD_DIR/${b}.ruby.kernel.tsv" ]; then
        rns=$(awk -F '\t' 'NR == 1 || $2 < best { best=$2 } END { print best }' "$BUILD_DIR/${b}.ruby.kernel.tsv")
    fi
    rsns=""
    if [ -s "$BUILD_DIR/${b}.rust.kernel.tsv" ]; then
        rsns=$(awk -F '\t' 'NR == 1 || $2 < best { best=$2 } END { print best }' "$BUILD_DIR/${b}.rust.kernel.tsv")
    fi

    if [ "$is_ok" -eq 1 ]; then
        pass_count=$((pass_count + 1))
        status_disp="${GREEN}OK${RESET}"
    else
        status_disp="${BRIGHT_RED}MISMATCH${RESET}"
    fi

    w_ms="—"; g_ms="—"; n_ms="—"; r_ms="—"; rs_ms="—"; ratio_str="—"
    
    if [ -n "$wns" ]; then w_ms=$(awk -v ns="$wns" 'BEGIN { printf "%.1f ms", ns / 1000000 }'); fi
    if [ -n "$gns" ]; then g_ms=$(awk -v ns="$gns" 'BEGIN { printf "%.1f ms", ns / 1000000 }'); fi
    if [ -n "$nns" ]; then n_ms=$(awk -v ns="$nns" 'BEGIN { printf "%.1f ms", ns / 1000000 }'); fi
    if [ -n "$rns" ]; then r_ms=$(awk -v ns="$rns" 'BEGIN { printf "%.1f ms", ns / 1000000 }'); fi
    if [ -n "$rsns" ]; then rs_ms=$(awk -v ns="$rsns" 'BEGIN { printf "%.1f ms", ns / 1000000 }'); fi

    if [ -n "$COMPARE_LANG" ]; then
        if [ "$COMPARE_LANG" = "go" ]; then fastest_ns=$gns
        elif [ "$COMPARE_LANG" = "node" ]; then fastest_ns=$nns
        elif [ "$COMPARE_LANG" = "ruby" ]; then fastest_ns=$rns
        elif [ "$COMPARE_LANG" = "rust" ]; then fastest_ns=$rsns
        else fastest_ns=""
        fi
    else
        fastest_ns=$(awk -v g="$gns" -v n="$nns" -v r="$rns" -v rs="$rsns" '
            BEGIN {
                min = ""
                if (g != "" && (min == "" || g < min)) min = g
                if (n != "" && (min == "" || n < min)) min = n
                if (r != "" && (min == "" || r < min)) min = r
                if (rs != "" && (min == "" || rs < min)) min = rs
                print min
            }
        ')
    fi

    ratio_color=""
    if [ -n "$wns" ] && [ -n "$fastest_ns" ]; then
        if awk -v f="$fastest_ns" 'BEGIN { exit (f > 0 ? 0 : 1) }'; then
            ratio=$(awk -v w="$wns" -v f="$fastest_ns" 'BEGIN { printf "%.2f", w / f }')
            ratio_str="${ratio}x"
            if awk -v r="$ratio" 'BEGIN { exit (r < 1.00 ? 0 : 1) }'; then
                faster_count=$((faster_count + 1))
                ratio_color="$BRIGHT_GREEN"
            fi
        fi
    fi

    ROW_FORMAT="  %-18s %14s"
    ROW_ARGS=("$b")
    
    if [ -z "$wns" ]; then
        ROW_ARGS+=("(wall-only)")
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "go" ]; then ROW_FORMAT+=" %14s"; ROW_ARGS+=("—"); fi
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "node" ]; then ROW_FORMAT+=" %14s"; ROW_ARGS+=("—"); fi
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "ruby" ]; then ROW_FORMAT+=" %14s"; ROW_ARGS+=("—"); fi
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "rust" ]; then ROW_FORMAT+=" %14s"; ROW_ARGS+=("—"); fi
        
        ROW_FORMAT+=" %14s    %s\n"
        ROW_ARGS+=("—" "$status_disp")
    else
        ROW_ARGS+=("$w_ms")
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "go" ]; then ROW_FORMAT+=" %14s"; ROW_ARGS+=("$g_ms"); fi
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "node" ]; then ROW_FORMAT+=" %14s"; ROW_ARGS+=("$n_ms"); fi
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "ruby" ]; then ROW_FORMAT+=" %14s"; ROW_ARGS+=("$r_ms"); fi
        if [ -z "$COMPARE_LANG" ] || [ "$COMPARE_LANG" = "rust" ]; then ROW_FORMAT+=" %14s"; ROW_ARGS+=("$rs_ms"); fi
        
        ROW_FORMAT+=" %s%14s%s    %s\n"
        ROW_ARGS+=("$ratio_color" "$ratio_str" "$RESET" "$status_disp")
    fi
    printf "$ROW_FORMAT" "${ROW_ARGS[@]}"
done

printf "  %s\n" "$LINE_DASHES"
if [ "$faster_count" -gt 0 ]; then
    printf "  %s%s%d/%d passed%s, %s%d faster than baseline%s\n\n" "$BOLD" "$GREEN" "$pass_count" "$total_count" "$RESET" "$BRIGHT_GREEN" "$faster_count" "$RESET"
else
    printf "  %s%s%d/%d passed%s\n\n" "$BOLD" "$GREEN" "$pass_count" "$total_count" "$RESET"
fi

SUMMARIZE_ARGS=(
    --artifact "$ARTIFACT_JSON"
    --mode "$MODE"
    --witchy "$WITCHY"
    --warmup "$WARMUP"
    --runs "$RUNS"
    --build-dir "$BUILD_DIR"
)
if [ -n "$COMPARE_LANG" ]; then
    SUMMARIZE_ARGS+=(--compare "$COMPARE_LANG")
fi

python3 "$BENCH_DIR/summarize.py" "${SUMMARIZE_ARGS[@]}" "${TARGETS[@]}" >/dev/null || {
    printf "%sWarning: benchmarks/summarize.py exited with an error%s\n" "$YELLOW" "$RESET" >&2
}
printf "  machine-readable artifact: %s\n" "$ARTIFACT_JSON"
[ "$pass_count" -eq "$total_count" ] || exit 1
