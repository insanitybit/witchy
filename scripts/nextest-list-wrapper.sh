#!/usr/bin/env bash
# Serialize nextest's macOS list phase. Nextest invokes this wrapper with the
# test binary followed by its `--list` arguments.
set -euo pipefail

[ "$#" -gt 0 ] || { echo "nextest-list-wrapper: missing test binary" >&2; exit 2; }

# New nextest versions expose one NEXTEST_RUN_ID to every list process. Older
# versions do not, but all wrappers still share the nextest runner as PPID.
# Either key is per-run, so a killed gate cannot block a later gate.
lock="${TMPDIR:-/tmp}/witchy-nextest-list-${NEXTEST_RUN_ID:-$PPID}.lock"
while ! mkdir "$lock" 2>/dev/null; do
    sleep 0.05
done

cleanup() {
    rmdir "$lock" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

"$@"
