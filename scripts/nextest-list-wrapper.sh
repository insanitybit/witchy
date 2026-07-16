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

# `kill -0` returns EPERM for a live process across some managed-sandbox
# boundaries. That is evidence the PID exists, not evidence that its slot is
# stale. Only ESRCH-like failures are reclaimable.
pid_is_alive() {
    local error
    if error="$(kill -0 "$1" 2>&1)"; then
        return 0
    fi
    case "$error" in
        *"Operation not permitted"* | *"operation not permitted"* | *"not permitted"*)
            return 0
            ;;
    esac
    return 1
}

# Failure to create the per-run root means the performance guard is unavailable.
# Run this one list command directly rather than spinning forever.
# One line per binary into the check.sh progress channel (stage_heartbeat
# turns growth into liveness-resetting log lines; nextest swallows wrapper
# stdout/stderr, so a file is the only visible channel).
record_progress() {
    [ -n "${WITCHY_LIST_PROGRESS_FILE:-}" ] || return 0
    printf '%s\n' "$(basename "$1")" >>"$WITCHY_LIST_PROGRESS_FILE" 2>/dev/null || true
}

if ! mkdir -p "$root" 2>/dev/null; then
    echo "nextest-list-wrapper: cannot create slot root; proceeding unbounded" >&2
    record_progress "$1"
    exec "$@"
fi

slot=""
while [ -z "$slot" ]; do
    i=1
    while [ "$i" -le "$jobs" ]; do
        candidate="$root/$i"
        # The symlink creation atomically claims the slot and records its owner.
        # A directory plus a later owner file has a crash window between those
        # operations that can leak an ownerless slot forever.
        if ln -s "$$" "$candidate" 2>/dev/null; then
            slot="$candidate"
            break
        fi
        owner="$(readlink "$candidate" 2>/dev/null || true)"
        if [ -n "$owner" ] && ! pid_is_alive "$owner"; then
            # Concurrent reclaimers are harmless: one removes the dead slot
            # and the next atomic symlink race still has exactly one winner.
            rm -f "$candidate" 2>/dev/null || true
            continue
        fi
        i=$((i + 1))
    done
    [ -n "$slot" ] && break
    sleep 0.05
done

cleanup() {
    rm -f "$slot" 2>/dev/null || true
    # Deliberately do NOT rmdir "$root": reaping the shared root is what races
    # concurrent starters into the ENOENT spin above. The root is keyed by the
    # per-run NEXTEST_RUN_ID, so at most one empty dir per suite run lingers in
    # TMPDIR — the OS reaps it with the rest of the tmp dir.
}
trap cleanup EXIT INT TERM

record_progress "$1"
"$@"
