#!/bin/bash
# Bound nextest's macOS list phase. Nextest invokes this wrapper with the test
# binary followed by its `--list` arguments.
set -euo pipefail

[ "${1:-}" != "--validate-ignore-policy" ] || {
    cd "${2:-.}"
    # Keep the gate self-contained on clean CI hosts: use find+grep rather than
    # adding ripgrep as a new prerequisite merely for this source-policy check.
    ignore_lines="$(find src crates tests -type f -name '*.rs' \
        -exec grep -nH -E '^[[:space:]]*#\[ignore([^]]*)?\]' {} + || true)"
    ignore_count="$(printf '%s\n' "$ignore_lines" | awk 'NF { n += 1 } END { print n + 0 }')"
    ignore_paths="$(printf '%s\n' "$ignore_lines" | awk -F: 'NF { print $1 }' | sort -u)"
    expected_paths="$(printf '%s\n' src/example_tests.rs src/stats.rs | sort)"
    if [ "$ignore_count" -ne 2 ] || [ "$ignore_paths" != "$expected_paths" ] \
        || ! awk '/^[[:space:]]*#\[ignore([^]]*)?\]/{ armed=1; next } armed { if ($0 ~ /^[[:space:]]*fn binary_path_coverage_report\(/) found=1; armed=0 } END { exit !found }' src/example_tests.rs \
        || ! awk '/^[[:space:]]*#\[ignore([^]]*)?\]/{ armed=1; next } armed { if ($0 ~ /^[[:space:]]*fn chan_throughput_bounded_by_rc_floor\(/) found=1; armed=0 } END { exit !found }' src/stats.rs; then
        echo "nextest-list-wrapper: ignored-test policy changed; update the audited names before discovery can skip the second cold exec" >&2
        printf '%s\n' "$ignore_lines" >&2
        exit 1
    fi
    exit 0
}

[ "$#" -gt 0 ] || { echo "nextest-list-wrapper: missing test binary" >&2; exit 2; }

# New nextest versions expose one NEXTEST_RUN_ID to every list process. Older
# versions do not, but all wrappers still share the nextest runner as PPID.
# Either key is per-run, so a killed gate cannot block a later gate.
root="${TMPDIR:-/tmp}/witchy-nextest-list-${NEXTEST_RUN_ID:-$PPID}"
# Freshly linked test binaries are I/O/codesign bound on macOS. Four distinct
# cold launches stretched to ~4x the single-launch time in a production gate,
# with no aggregate throughput gain, while one-wide discovery later developed
# a >100s no-progress tail. Default to two as the bounded middle ground; the
# override remains available for controlled retuning.
jobs="${WITCHY_NEXTEST_LIST_JOBS:-2}"
runner_pid="$PPID"
binary_name="$(basename "$1")"
normal_done="$root/normal-done-$binary_name"
normal_owner="$root/normal-owner-$binary_name"
normal_output="$root/normal-output-$binary_name"
ignored=0
for arg in "$@"; do
    [ "$arg" = "--ignored" ] && ignored=1
done
progress_file="${WITCHY_GATE_PROGRESS_FILE:-}"
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

mark_progress() {
    [ -n "$progress_file" ] && touch "$progress_file" 2>/dev/null || true
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
# the same time. Libtest's ordinary list includes ignored tests but does not
# identify them, so nextest normally cold-executes every freshly linked ~100 MB
# binary twice just to learn that almost all ignored lists are empty. Capture
# the ordinary output and derive the ignored output from the two audited ignored
# names instead. check.sh validates that source policy before invoking nextest;
# a future un-audited `#[ignore]` makes the gate fail rather than silently
# changing coverage. The ignored waiter holds NO global slot.
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
    awk '
        $0 == "example_tests::binary_path_coverage_report: test" ||
        $0 == "stats::tests::chan_throughput_bounded_by_rc_floor: test"
    ' "$normal_output"
    rm -f "$normal_done" "$normal_output" 2>/dev/null || true
    exit 0
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
            mark_progress
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
        # Ignored discovery exits above after consuming the cached list.
        :
    fi
    mark_progress
    # Deliberately do NOT rmdir "$root": reaping the shared root is what races
    # concurrent starters into the ENOENT spin above. The root is keyed by the
    # per-run NEXTEST_RUN_ID, so at most one empty dir per suite run lingers in
    # TMPDIR — the OS reaps it with the rest of the tmp dir.
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

record_progress "$1"
command_status=0
"$@" >"$normal_output" || command_status=$?
cat "$normal_output"
exit "$command_status"
