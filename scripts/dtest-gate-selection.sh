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

started="$(python3 -c 'import time; print(time.monotonic_ns() // 1000000)')"
selection=""
if ! selection="$($dtest_bin --workspace "$root" select --from "$from" --to "$to" --json 2>/dev/null)"; then
    elapsed="$(( $(python3 -c 'import time; print(time.monotonic_ns() // 1000000)') - started ))"
    jq -cn --argjson elapsed_ms "$elapsed" \
        '{status:"fallback", reason:"dtest selection failed", elapsed_ms:$elapsed_ms}'
    exit 0
fi
elapsed="$(( $(python3 -c 'import time; print(time.monotonic_ns() // 1000000)') - started ))"

if ! jq -e '
    (.changed_files|type)=="number" and
    (.changed_definitions|type)=="number" and
    (.tests|type)=="array" and
    all(.tests[]; (.name|type)=="string" and (.package|type)=="string")
' >/dev/null 2>&1 <<<"$selection"; then
    jq -cn --argjson elapsed_ms "$elapsed" \
        '{status:"fallback", reason:"invalid dtest output", elapsed_ms:$elapsed_ms}'
    exit 0
fi

jq -cn --argjson selection "$selection" --argjson elapsed_ms "$elapsed" '
    ($selection.tests | map(.package) | unique) as $packages
    | ($selection.tests | map("test(=" + .name + ")") | join(" or ")) as $expression
    | {status:(if ($selection.tests|length)==0 then "empty" else "selected" end),
       elapsed_ms:$elapsed_ms,
       changed_files:$selection.changed_files,
       changed_definitions:$selection.changed_definitions,
       selected_tests:($selection.tests|length),
       packages:$packages,
       expression:$expression}
'
