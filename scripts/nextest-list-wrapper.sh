#!/usr/bin/env bash
# Serialize nextest's macOS list phase. Nextest invokes this wrapper with the
# test binary followed by its `--list` arguments.
set -euo pipefail

[ "$#" -gt 0 ] || { echo "nextest-list-wrapper: missing test binary" >&2; exit 2; }

# NEXTEST_RUN_ID is shared by every list process in one run and unique across
# runs, so a killed gate cannot leave a lock that blocks a later gate.
lock="${TMPDIR:-/tmp}/witchy-nextest-list-${NEXTEST_RUN_ID:-$$}.lock"
while ! mkdir "$lock" 2>/dev/null; do
    sleep 0.05
done

cleanup() {
    rmdir "$lock" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

"$@"
