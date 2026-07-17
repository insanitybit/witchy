#!/bin/bash
# Bound nextest's macOS list phase. Nextest invokes this wrapper with the test
# binary followed by its `--list` arguments.
set -euo pipefail

[ "$#" -gt 0 ] || { echo "nextest-list-wrapper: missing test binary" >&2; exit 2; }

# New nextest versions expose one NEXTEST_RUN_ID to every list process. Older
# versions do not, but all wrappers still share the nextest runner as PPID.
# Either key is per-run, so a killed gate cannot block a later gate.
root="${TMPDIR:-/tmp}/witchy-nextest-list-${NEXTEST_RUN_ID:-$PPID}"
# Freshly linked test binaries are I/O/codesign bound on macOS. Four distinct
# cold launches stretched to ~4x the single-launch time in a production gate,
# with no aggregate throughput gain, while unrelated process startup also
# stalled. Serialize by default; the override remains available for controlled
# retuning and for environments where cold launch is not the bottleneck.
jobs="${WITCHY_NEXTEST_LIST_JOBS:-1}"
runner_pid="$PPID"
binary_name="$(basename "$1")"
normal_done="$root/normal-done-$binary_name"
normal_owner="$root/normal-owner-$binary_name"
ignored=0
for arg in "$@"; do
    [ "$arg" = "--ignored" ] && ignored=1
done
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

# Nextest launches each binary's ordinary and `--ignored` discovery passes at
# the same time. On macOS that makes both processes pay the cold first-exec
# codesign/page-in cost for the same freshly linked ~100 MB binary. Let the
# ordinary pass warm that binary before its ignored pass starts. The ignored
# waiter deliberately holds NO global slot, so other binaries still discover
# up to `$jobs`-wide and a wave of waiters cannot deadlock the slot pool.
#
# The two passes are still both executed and their output is unchanged. This is
# required for correctness: libtest's ordinary list includes ignored tests but
# does not identify them, so nextest needs the second output to mark them.
if [ "$ignored" -eq 1 ]; then
    while [ ! -e "$normal_done" ]; do
        # A SIGKILL cannot run the ordinary wrapper's cleanup. Its owner symlink
        # lets the ignored peer fail closed instead of waiting forever.
        owner="$(readlink "$normal_owner" 2>/dev/null || true)"
        if [ -n "$owner" ] && ! pid_is_alive "$owner"; then
            echo "nextest-list-wrapper: ordinary list process died for $binary_name" >&2
            exit 1
        fi
        # If nextest itself exits, no process remains that can consume this
        # output. Stop promptly rather than leaking an orphaned waiter.
        if ! pid_is_alive "$runner_pid"; then
            echo "nextest-list-wrapper: nextest parent exited while waiting for $binary_name" >&2
            exit 1
        fi
        sleep 0.05
    done
else
    # One ordinary pass exists per binary and run. A symlink records its PID so
    # an untrappable death remains distinguishable from a slow healthy pass.
    ln -s "$$" "$normal_owner" 2>/dev/null || true
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
    if [ -n "$slot" ]; then
        rm -f "$slot" 2>/dev/null || true
    fi
    if [ "$ignored" -eq 0 ]; then
        # Publish completion before removing the owner: waiters check the done
        # marker first, so they can never mistake normal cleanup for SIGKILL.
        : >"$normal_done" 2>/dev/null || true
        rm -f "$normal_owner" 2>/dev/null || true
    else
        # The pair is complete. Avoid leaving a stale done marker if an older
        # nextest without NEXTEST_RUN_ID eventually reuses the same parent PID.
        rm -f "$normal_done" 2>/dev/null || true
    fi
    # Deliberately do NOT rmdir "$root": reaping the shared root is what races
    # concurrent starters into the ENOENT spin above. The root is keyed by the
    # per-run NEXTEST_RUN_ID, so at most one empty dir per suite run lingers in
    # TMPDIR — the OS reaps it with the rest of the tmp dir.
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

record_progress "$1"
"$@"
