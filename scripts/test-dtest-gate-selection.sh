#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-dtest-selection-XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/dtest" <<'SH'
#!/usr/bin/env bash
cat <<'JSON'
{"changed_files":2,"changed_definitions":3,"tests":[{"name":"tests::alpha","package":"witchy-types","binary":"types_contracts"}],"partitions":[{"package":"witchy-types","binary":"types_contracts","expression":"test(=tests::alpha)","tests":1}],"timings":{"total_ms":12,"index":{"elapsed_ms":7,"git_ms":2,"graph_ms":1,"overlay_hits":1,"overlay_entries":4,"overlay_compacted":0},"selection":{"elapsed_ms":3}}}
JSON
SH
chmod +x "$tmp/dtest"

selected="$(DTEST_BIN="$tmp/dtest" ./scripts/dtest-gate-selection.sh "$tmp" base HEAD)"
jq -e '
    .status == "selected" and
    .selected_tests == 1 and
    .changed_files == 2 and
    .changed_definitions == 3 and
    .packages == ["witchy-types"] and
    .expression == "test(=tests::alpha)" and
    .target_args == "-p witchy-types --test types_contracts" and
    .index_ms == 7 and .index_git_ms == 2 and .index_graph_ms == 1 and
    .overlay_hits == 1 and .overlay_entries == 4 and .overlay_compacted == 0 and
    .selection_ms == 3 and .partitions == 1 and
    .elapsed_ms == 12
' >/dev/null <<<"$selected"

cat >"$tmp/dtest" <<'SH'
#!/usr/bin/env bash
exit 1
SH
fallback="$(DTEST_BIN="$tmp/dtest" ./scripts/dtest-gate-selection.sh "$tmp" base HEAD)"
jq -e '.status == "fallback" and .reason == "dtest selection failed"' \
    >/dev/null <<<"$fallback"

echo "dtest gate selection tests passed"
