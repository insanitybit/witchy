#!/usr/bin/env bash
set -euo pipefail

print_help() {
    cat <<'USAGE'
Usage: ./scripts/perf-health.sh [options]

Show fast-read contributor-velocity status from local build/test metrics and
recent merge-queue throughput data.

Options:
  --metrics-dir <path>  Metrics root (default: scratch/dev-metrics)
  --since <window>      gate-report window for 24h (default: 24h)
  --json                Print machine-readable summary as JSON.
  --help                Show this help text.
USAGE
}

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

metrics_dir="scratch/dev-metrics"
since="24h"
emit_json=0

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --metrics-dir)
            [ "$#" -ge 2 ] || { echo "perf-health: --metrics-dir needs a path" >&2; exit 2; }
            metrics_dir="$2"
            shift 2
            ;;
        --since)
            [ "$#" -ge 2 ] || { echo "perf-health: --since needs a value" >&2; exit 2; }
            since="$2"
            shift 2
            ;;
        --json)
            emit_json=1
            shift
            ;;
        -h|--help)
            print_help
            exit 0
            ;;
        *)
            echo "perf-health: unknown argument '$1'" >&2
            print_help
            exit 2
            ;;
    esac
done

latest_metrics="$metrics_dir/latest.json"

if command -v jq >/dev/null 2>&1; then
    if [ -f "$latest_metrics" ]; then
        latest_status="$(jq -r 'if (.summary.failed // 1) == 0 then "ok" else "failed" end' "$latest_metrics")"
        if [ "$latest_status" = "ok" ]; then
            build="$(jq -r '.stages[] | select(.name=="build_workspace") | .duration_s // "n/a"' "$latest_metrics")"
            test_compile="$(jq -r '.stages[] | select(.name=="test_compile") | .duration_s // "n/a"' "$latest_metrics")"
            tests="$(jq -r '.stages[] | select(.name=="tests") | .duration_s // "n/a"' "$latest_metrics")"
        else
            build="n/a"
            test_compile="n/a"
            tests="n/a"
        fi
        latest_branch="$(jq -r '.branch // "n/a"' "$latest_metrics")"
        latest_rev="$(jq -r '.git_rev // "n/a"' "$latest_metrics")"
    else
        latest_status="missing"
        build="n/a"
        test_compile="n/a"
        tests="n/a"
        latest_branch="n/a"
        latest_rev="n/a"
    fi

    gate_report="$(./scripts/gate-report.sh --since "$since" --json || true)"
    if [ -n "$gate_report" ]; then
        merged="$(printf '%s' "$gate_report" | jq -r '.throughput.merged_branches // 0')"
        green_gates="$(printf '%s' "$gate_report" | jq -r '.throughput.green_gates // 0')"
        submissions="$(printf '%s' "$gate_report" | jq -r '.throughput.submissions // 0')"
        failed="$(printf '%s' "$gate_report" | jq -r '.outcomes.red // 0 + .outcomes.timeout // 0 + .outcomes.batch_red // 0')"
        p50="$(printf '%s' "$gate_report" | jq -r '.attempt_s.p50 // "n/a"')"
        p90="$(printf '%s' "$gate_report" | jq -r '.attempt_s.p90 // "n/a"')"
    else
        merged=0
        green_gates=0
        submissions=0
        failed=0
        p50="n/a"
        p90="n/a"
    fi

    if [ "$emit_json" -eq 1 ]; then
        printf '{\n'
        printf '  "generated_at": "%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf '  "since": "%s",\n' "$since"
        printf '  "merge_queue": {"submissions":%s,"merged_branches":%s,"green_gates":%s,"failed_attempts":%s,"attempt_p50_s":"%s","attempt_p90_s":"%s"},\n' \
            "$submissions" "$merged" "$green_gates" "$failed" "$p50" "$p90"
        printf '  "latest_local_metrics": {"status":"%s","branch":"%s","git_rev":"%s","build_s":"%s","test_compile_s":"%s","tests_s":"%s"}\n' \
            "$latest_status" "$latest_branch" "$latest_rev" "$build" "$test_compile" "$tests"
        printf '}\n'
        exit 0
    fi

    printf 'Performance health snapshot\n'
    printf '  latest local run: status=%s branch=%s rev=%s\n' "$latest_status" "$latest_branch" "$latest_rev"
    printf '  latest build s: %s\n' "$build"
    printf '  latest test(no-run) s: %s\n' "$test_compile"
    printf '  latest tests s: %s\n' "$tests"
    printf '  merge queue (since %s): submissions=%s merged=%s green_gates=%s failed=%s\n' \
        "$since" "$submissions" "$merged" "$green_gates" "$failed"
    printf '  merge queue attempt latency: p50=%s p90=%s\n' "$p50" "$p90"
else
    echo "perf-health: jq is required for detailed reporting" >&2
    if [ "$emit_json" -eq 1 ]; then
        printf '{"generated_at":"%s","error":"jq required"}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        exit 1
    fi
    if [ -f "$latest_metrics" ]; then
        echo "latest metrics: $latest_metrics"
        sed -n '1,120p' "$latest_metrics"
    else
        echo "latest metrics: not found"
    fi
fi

exit 0
