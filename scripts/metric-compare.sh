#!/usr/bin/env bash
# Compare two successful build-test-metrics JSON snapshots.
set -euo pipefail

usage() {
    echo "usage: scripts/metric-compare.sh [--json] <before.json> <after.json>" >&2
}

emit_json=0
if [ "${1:-}" = "--json" ]; then
    emit_json=1
    shift
fi
if [ "$#" -ne 2 ]; then
    usage
    exit 2
fi

before=$1
after=$2
command -v jq >/dev/null 2>&1 || {
    echo "metric-compare: jq is required" >&2
    exit 2
}

for file in "$before" "$after"; do
    [ -f "$file" ] || { echo "metric-compare: missing snapshot '$file'" >&2; exit 2; }
    if [ "$(jq -r '(.summary.failed // 1)' "$file")" -ne 0 ]; then
        echo "metric-compare: refusing failed snapshot '$file'" >&2
        exit 2
    fi
done

before_rev=$(jq -r '.git_rev // "n/a"' "$before")
after_rev=$(jq -r '.git_rev // "n/a"' "$after")
stages='build_workspace compile_workspace test_compile tests'

if [ "$emit_json" -eq 1 ]; then
    printf '{"before_rev":"%s","after_rev":"%s","stages":{' "$before_rev" "$after_rev"
else
    printf 'metric comparison: %s -> %s\n' "$before_rev" "$after_rev"
fi

first=1
for stage in $stages; do
    before_s=$(jq -r --arg stage "$stage" '.stages[] | select(.name == $stage) | .duration_s' "$before")
    after_s=$(jq -r --arg stage "$stage" '.stages[] | select(.name == $stage) | .duration_s' "$after")
    if [ -z "$before_s" ] || [ -z "$after_s" ] || [ "$before_s" = "null" ] || [ "$after_s" = "null" ]; then
        echo "metric-compare: stage '$stage' is missing from one snapshot" >&2
        exit 2
    fi
    speedup=$(awk -v before="$before_s" -v after="$after_s" 'BEGIN { if (after > 0) printf "%.3f", before / after; else print "n/a" }')
    improvement=$(awk -v before="$before_s" -v after="$after_s" 'BEGIN { if (before > 0) printf "%.1f", (before - after) * 100 / before; else print "n/a" }')
    if [ "$emit_json" -eq 1 ]; then
        [ "$first" -eq 1 ] || printf ','
        printf '"%s":{"before_s":%s,"after_s":%s,"speedup":%s,"improvement_pct":%s}' \
            "$stage" "$before_s" "$after_s" "$speedup" "$improvement"
    else
        printf '  %-16s before=%ss after=%ss speedup=%sx improvement=%s%%\n' \
            "$stage" "$before_s" "$after_s" "$speedup" "$improvement"
    fi
    first=0
done

if [ "$emit_json" -eq 1 ]; then
    printf '}\n'
fi
