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
# Acquisition deadline. The slot cap is only a PERFORMANCE bound (cap concurrent
# dyld pressure during discovery); it is never a correctness requirement, so if
# acquisition cannot converge we FAIL OPEN — proceed unbounded rather than spin.
# A hung wrapper stall-kills the whole gate (~26 min occupied, observed
# 20260716-082626); one extra concurrent `--list` does not. The root-reap race
# below is fixed, but this bounds every OTHER cause of a stuck acquisition
# (a leaked slot dir from a SIGKILL'd peer that skipped cleanup, a full or
# read-only TMPDIR, mkdir EINTR loops) to a fixed wall-clock instead of forever.
deadline_secs="${WITCHY_NEXTEST_LIST_ACQUIRE_TIMEOUT:-20}"
case "$deadline_secs" in
    '' | *[!0-9]* ) deadline_secs=20 ;;
esac
start_secs=$(date +%s)
slot=""
while [ -z "$slot" ]; do
    # (Re)create the root EVERY attempt: a finishing peer's cleanup rmdirs an
    # empty root at the same moment a starting wrapper is between its own
    # mkdir -p and its first slot acquisition — with a one-time mkdir that
    # process spins forever on ENOENT slot mkdirs (observed: gate stall-killed
    # idle at the list phase, zero tests started, 20260716-082626 log).
    mkdir -p "$root" 2>/dev/null || true
    i=1
    while [ "$i" -le "$jobs" ]; do
        candidate="$root/$i"
        if mkdir "$candidate" 2>/dev/null; then
            # Stamp ownership so a holder killed without running its trap does
            # not leak one of the bounded slots for the rest of the nextest run.
            echo "$$" >"$candidate/owner" 2>/dev/null || true
            slot="$candidate"
            break
        fi
        owner="$(cat "$candidate/owner" 2>/dev/null || true)"
        if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
            # Concurrent reclaimers are harmless: one removes the dead slot
            # and the next acquisition race still has exactly one winner.
            rm -rf "$candidate" 2>/dev/null || true
            continue
        fi
        i=$((i + 1))
    done
    [ -n "$slot" ] && break
    # Fail open past the deadline: never let a wedged acquisition hang the gate.
    if [ "$(( $(date +%s) - start_secs ))" -ge "$deadline_secs" ]; then
        echo "nextest-list-wrapper: could not acquire a list slot in ${deadline_secs}s; proceeding unbounded" >&2
        break
    fi
    sleep 0.05
done

cleanup() {
    # $slot is empty in the fail-open path (no slot was ever acquired).
    [ -n "$slot" ] && rm -rf "$slot" 2>/dev/null || true
    # Deliberately do NOT rmdir "$root": reaping the shared root is what races
    # concurrent starters into the ENOENT spin above. The root is keyed by the
    # per-run NEXTEST_RUN_ID, so at most one empty dir per suite run lingers in
    # TMPDIR — the OS reaps it with the rest of the tmp dir.
}
trap cleanup EXIT INT TERM

"$@"
