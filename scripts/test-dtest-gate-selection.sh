#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-dtest-selection-XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/dtest" <<'SH'
#!/usr/bin/env bash
cat <<'JSON'
{"changed_files":2,"changed_definitions":3,"tests":[{"name":"tests::alpha","package":"witchy-types"},{"name":"tests::beta","package":"witchy-lower"}]}
JSON
SH
chmod +x "$tmp/dtest"

selected="$(DTEST_BIN="$tmp/dtest" ./scripts/dtest-gate-selection.sh "$tmp" base HEAD)"
jq -e '
    .status == "selected" and
    .selected_tests == 2 and
    .changed_files == 2 and
    .changed_definitions == 3 and
    .packages == ["witchy-lower", "witchy-types"] and
    .expression == "test(=tests::alpha) or test(=tests::beta)" and
    (.elapsed_ms | type) == "number"
' >/dev/null <<<"$selected"

cat >"$tmp/dtest" <<'SH'
#!/usr/bin/env bash
exit 1
SH
fallback="$(DTEST_BIN="$tmp/dtest" ./scripts/dtest-gate-selection.sh "$tmp" base HEAD)"
jq -e '.status == "fallback" and .reason == "dtest selection failed"' \
    >/dev/null <<<"$fallback"

echo "dtest gate selection tests passed"
