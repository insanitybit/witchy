#!/usr/bin/env bash
set -euo pipefail

print_help() {
    cat <<'USAGE'
Usage: ./scripts/build-test-metrics.sh [options]

Capture stage timings for local build, test-compile, and optional full test runs.

Options:
  --target-dir <dir>       Pass CARGO_TARGET_DIR to all measured commands.
  --output-dir <path>      Base directory for metrics artifacts (default: scratch/dev-metrics)
  --label <name>           Label for this run (default: cycle)
  --with-tests             Run full workspace tests after test-compile.
  --json                   Print the generated JSON summary to stdout.
  --help                   Show this help text.
USAGE
}

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

label="cycle"
output_dir="scratch/dev-metrics"
target_dir=""
include_tests=0
emit_json=0

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --target-dir)
            [ "$#" -ge 2 ] || { echo "build-test-metrics: --target-dir needs a directory" >&2; exit 2; }
            target_dir="$2"
            shift 2
            ;;
        --output-dir)
            [ "$#" -ge 2 ] || { echo "build-test-metrics: --output-dir needs a directory" >&2; exit 2; }
            output_dir="$2"
            shift 2
            ;;
        --label)
            [ "$#" -ge 2 ] || { echo "build-test-metrics: --label needs a value" >&2; exit 2; }
            label="$2"
            shift 2
            ;;
        --with-tests)
            include_tests=1
            shift
            ;;
        --json)
            emit_json=1
            shift
            ;;
        -h | --help)
            print_help
            exit 0
            ;;
        *)
            echo "build-test-metrics: unknown argument '$1'" >&2
            print_help
            exit 2
            ;;
    esac
done

mkdir -p "$output_dir"
run_timestamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
git_rev="$(git rev-parse --short HEAD)"
git_branch="$(git branch --show-current 2>/dev/null || echo '<detached>')"
label_sanitized="${label// /_}"
label_sanitized="${label_sanitized//[^A-Za-z0-9._-]/_}"
run_label="$(date +%Y%m%d-%H%M%S)-$label_sanitized"
run_dir="$output_dir/$run_label"
mkdir -p "$run_dir"

stage_names=()
stage_statuses=()
stage_elapsed_s=()

timestamp_seconds() {
    if command -v python3 >/dev/null 2>&1; then
        python3 - <<'PY'
import time
print(f"{time.time():.6f}")
PY
    else
        if date +%s%N >/dev/null 2>&1; then
            date +%s%N
        else
            printf "%s.000000\n" "$(date +%s)"
        fi
    fi
}

duration_seconds() {
    local start="$1"
    local end="$2"
    awk -v start="$start" -v end="$end" 'BEGIN { printf "%.3f", end - start }'
}

run_stage() {
    local name="$1"
    shift
    local log_file="$run_dir/${name}.log"
    local start_time
    local end_time
    local status=0
    local elapsed

    start_time="$(timestamp_seconds)"
    set +e
    if [[ -n "$target_dir" ]]; then
        (export CARGO_TARGET_DIR="$target_dir"; "$@" >"$log_file" 2>&1)
    else
        ("$@" >"$log_file" 2>&1)
    fi
    status=$?
    set -e
    end_time="$(timestamp_seconds)"

    elapsed="$(duration_seconds "$start_time" "$end_time")"
    stage_names+=("$name")
    stage_statuses+=("$status")
    stage_elapsed_s+=("$elapsed")

    printf '%s,%s,%s,%s,%s,%s,%s\n' \
        "$run_timestamp" "$label_sanitized" "$git_rev" "$git_branch" "$name" "$elapsed" "$status" >>"$run_file"

    if [ "$status" -ne 0 ]; then
        echo "build-test-metrics: stage '$name' failed (log: $log_file)" >&2
        return 1
    fi

    return 0
}

run_file="$run_dir/metrics.tsv"
printf 'timestamp,label,git_rev,branch,stage,duration_s,exit_code\n' >"$run_file"

failed=0

run_stage build_workspace cargo build --workspace --all-targets || failed=1
run_stage test_compile cargo test --workspace --no-run || failed=1
if [[ "$include_tests" -eq 1 ]]; then
    run_stage tests cargo test --workspace || failed=1
fi

history_file="$output_dir/metrics.tsv"
if [[ ! -f "$history_file" ]]; then
    printf 'timestamp,label,git_rev,branch,stage,duration_s,exit_code,log\n' >"$history_file"
fi

for idx in "${!stage_names[@]}"; do
    printf '%s,%s,%s,%s,%s,%s,%s,%s\n' \
        "$run_timestamp" "$label_sanitized" "$git_rev" "$git_branch" "${stage_names[$idx]}" \
        "${stage_elapsed_s[$idx]}" "${stage_statuses[$idx]}" "$run_dir/${stage_names[$idx]}.log" \
        >>"$history_file"
done

json_file="$run_dir/metrics.json"
{
    printf '{\n'
    printf '  "generated_at": "%s",\n' "$run_timestamp"
    printf '  "label": "%s",\n' "$label_sanitized"
    printf '  "run_dir": "%s",\n' "$run_dir"
    printf '  "git_rev": "%s",\n' "$git_rev"
    printf '  "branch": "%s",\n' "$git_branch"
    printf '  "target_dir": "%s",\n' "${target_dir:-none}"
    printf '  "stages": [\n'
    for idx in "${!stage_names[@]}"; do
        printf '    {"name":"%s","duration_s":%s,"exit_code":%s,"log":"%s"}%s\n' \
            "${stage_names[$idx]}" "${stage_elapsed_s[$idx]}" "${stage_statuses[$idx]}" \
            "$run_dir/${stage_names[$idx]}.log" \
            "$(if [[ "$idx" -ne "$((${#stage_names[@]}-1))" ]]; then printf ','; fi)"
    done
    printf '  ],\n'
    printf '  "summary": {"failed": %s}\n' "$failed"
    printf '}\n'
} >"$json_file"

cp "$json_file" "$output_dir/latest.json"

if [[ "$emit_json" -eq 1 ]]; then
    cat "$json_file"
fi

if [[ "$failed" -ne 0 ]]; then
    echo "build-test-metrics: one or more stages failed; see $run_dir" >&2
    exit 1
fi

echo "build-test-metrics: metrics written to $json_file" >&2
exit 0
