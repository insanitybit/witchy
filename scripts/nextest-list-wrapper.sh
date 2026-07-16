#!/bin/bash
# Bound nextest's macOS list phase. Nextest invokes this wrapper with the test
# binary followed by its `--list` arguments.
set -euo pipefail

[ "$#" -gt 0 ] || { echo "nextest-list-wrapper: missing test binary" >&2; exit 2; }

# New nextest versions expose one NEXTEST_RUN_ID to every list process. Older
# versions do not, but all wrappers still share the nextest runner as PPID.
# Either key is per-run, so a killed gate cannot block a later gate.
root="${TMPDIR:-/tmp}/witchy-nextest-list-${NEXTEST_RUN_ID:-$PPID}"
jobs="${WITCHY_NEXTEST_LIST_JOBS:-4}"
case "$jobs" in
    '' | *[!0-9]* | 0)
        echo "nextest-list-wrapper: WITCHY_NEXTEST_LIST_JOBS must be a positive integer" >&2
        exit 2
        ;;
esac
slot=""
while [ -z "$slot" ]; do
    # (Re)create the root EVERY attempt: a finishing peer's cleanup rmdirs an
    # empty root at the same moment a starting wrapper is between its own
    # mkdir -p and its first slot acquisition — with a one-time mkdir that
    # process spins forever on ENOENT slot mkdirs (observed: gate stall-killed
    # idle at the list phase, zero tests started, 20260716-082626 log).
    mkdir -p "$root"
    i=1
    while [ "$i" -le "$jobs" ]; do
        candidate="$root/$i"
        if mkdir "$candidate" 2>/dev/null; then
            slot="$candidate"
            break
        fi
        i=$((i + 1))
    done
    [ -n "$slot" ] && break
    sleep 0.05
done

cleanup() {
    rmdir "$slot" 2>/dev/null || true
    # Deliberately do NOT rmdir "$root": reaping the shared root is what races
    # concurrent starters into the ENOENT spin above. The root is keyed by the
    # per-run NEXTEST_RUN_ID, so at most one empty dir per suite run lingers in
    # TMPDIR — the OS reaps it with the rest of the tmp dir.
}
trap cleanup EXIT INT TERM

"$@"
