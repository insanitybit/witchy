#!/usr/bin/env bash
# Emit a validated dtest selection for the merge coordinator as one JSON value.
# Failures are data: callers retain their existing conservative selector.
set -euo pipefail

root="${1:-.}"
from="${2:-}"
to="${3:-HEAD}"

if [ -z "$from" ]; then
    echo '{"status":"fallback","reason":"missing base revision"}'
    exit 0
fi

dtest_bin="${DTEST_BIN:-}"
if [ -z "$dtest_bin" ]; then
    dtest_bin="$(command -v dtest 2>/dev/null || true)"
fi
if [ -z "$dtest_bin" ] || [ ! -x "$dtest_bin" ]; then
    echo '{"status":"fallback","reason":"dtest unavailable"}'
    exit 0
fi

selection=""
if ! selection="$($dtest_bin --workspace "$root" select --from "$from" --to "$to" \
    --assume-clean --json --compact 2>/dev/null)"; then
    echo '{"status":"fallback","reason":"dtest selection failed"}'
    exit 0
fi

if ! jq -e '
    (.changed_files|type)=="number" and
    (.changed_definitions|type)=="number" and
    (.tests|type)=="array" and
    all(.tests[]; (.name|type)=="string" and (.package|type)=="string") and
    (.partitions|type)=="array" and
    (.timings.index.elapsed_ms|type)=="number" and
    (.timings.selection.elapsed_ms|type)=="number"
' >/dev/null 2>&1 <<<"$selection"; then
    echo '{"status":"fallback","reason":"invalid dtest output"}'
    exit 0
fi

jq -cn --argjson selection "$selection" '
    ($selection.tests | map(.package) | unique) as $packages
    | ($selection.tests | map("test(=" + .name + ")") | join(" or ")) as $expression
    | ($selection.partitions | if length == 1 then .[0] else null end) as $partition
    | {status:(if ($selection.tests|length)==0 then "empty" else "selected" end),
       elapsed_ms:$selection.timings.total_ms,
       index_ms:$selection.timings.index.elapsed_ms,
       index_git_ms:$selection.timings.index.git_ms,
       index_graph_ms:$selection.timings.index.graph_ms,
       overlay_hits:$selection.timings.index.overlay_hits,
       overlay_entries:$selection.timings.index.overlay_entries,
       overlay_compacted:$selection.timings.index.overlay_compacted,
       selection_ms:$selection.timings.selection.elapsed_ms,
       changed_files:$selection.changed_files,
       changed_definitions:$selection.changed_definitions,
       selected_tests:($selection.tests|length),
       partitions:($selection.partitions|length),
       target_args:(if $partition != null and ($partition.package|type)=="string"
                       and ($partition.binary|type)=="string"
                    then "-p " + $partition.package + " --test " + $partition.binary
                    else "" end),
       packages:$packages,
       expression:$expression}
'
