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
cache_enabled="${WITCHY_NEXTEST_LIST_CACHE:-1}"
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
case "$cache_enabled" in
    0 | 1) ;;
    *)
        echo "nextest-list-wrapper: WITCHY_NEXTEST_LIST_CACHE must be 0 or 1" >&2
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

sha256_file() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{ print $1 }'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$1" | awk '{ print $NF }'
    else
        return 1
    fi
}

sha256_stream() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 | awk '{ print $1 }'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum | awk '{ print $1 }'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 | awk '{ print $NF }'
    else
        return 1
    fi
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

# Machine-readable discovery telemetry is deliberately separate from the
# one-line-per-binary watchdog channel above. Adding cache completion records to
# the watchdog file would make check.sh over-count binaries and could turn a
# telemetry change into a liveness-policy change.
record_telemetry() { # record_telemetry <event> <elapsed-seconds> [cache-key]
    [ -n "${WITCHY_LIST_TELEMETRY_FILE:-}" ] || return 0
    printf 'schema=1 event=%s binary=%s elapsed_s=%s cache_key=%s\n' \
        "$1" "$binary_name" "$2" "${3:--}" >>"$WITCHY_LIST_TELEMETRY_FILE" 2>/dev/null || true
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

# Persistent discovery reuse is narrower than test-result caching: libtest's
# inventory is a pure function of the executable, logical binary name, and its
# list arguments. Hash those byte-for-byte, plus this wrapper and the audited
# ignored-test policy. A missing hash tool, unwritable cache, malformed entry,
# or failed list command
# simply executes discovery normally; correctness never depends on the cache.
slot=""
cache_lock=""
cache_lock_owned=0
cache_entry=""
cache_key=""
cache_started="$(date +%s)"
cache_schema="witchy-nextest-discovery-v1"
cacheable=0
for arg in "$@"; do
    [ "$arg" = "--list" ] && cacheable=1
done

cleanup() {
    if [ -n "$slot" ]; then
        rm -f "$slot" 2>/dev/null || true
    fi
    if [ "$cache_lock_owned" -eq 1 ] && [ -n "$cache_lock" ] \
        && [ "$(readlink "$cache_lock" 2>/dev/null || true)" = "$$" ]; then
        rm -f "$cache_lock" 2>/dev/null || true
    fi
    if [ "$ignored" -eq 0 ]; then
        # Publish completion before removing the owner: waiters check the done
        # marker first, so they can never mistake normal cleanup for SIGKILL.
        : >"$normal_done" 2>/dev/null || true
        rm -f "$normal_owner" 2>/dev/null || true
    else
        # Ignored discovery exits above after consuming the per-run list.
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

cache_entry_is_valid() { # cache_entry_is_valid <entry-dir> <expected-key>
    local entry="$1" expected="$2" recorded_key recorded_output output_digest
    [ -d "$entry" ] && [ ! -L "$entry" ] || return 1
    [ -f "$entry/output" ] && [ ! -L "$entry/output" ] || return 1
    [ -f "$entry/manifest" ] && [ ! -L "$entry/manifest" ] || return 1
    recorded_key="$(sed -n 's/^key=//p' "$entry/manifest")"
    recorded_output="$(sed -n 's/^output_sha256=//p' "$entry/manifest")"
    [ "$recorded_key" = "$expected" ] && [ -n "$recorded_output" ] || return 1
    output_digest="$(sha256_file "$entry/output" 2>/dev/null || true)"
    [ -n "$output_digest" ] && [ "$output_digest" = "$recorded_output" ]
}

use_cache_entry() { # use_cache_entry <entry-dir> <event>
    cp "$1/output" "$normal_output" 2>/dev/null || return 1
    cat "$normal_output"
    record_telemetry "$2" "$(( $(date +%s) - cache_started ))" "${cache_key:0:16}"
    return 0
}

record_progress "$1"
if [ "$cache_enabled" -eq 1 ] && [ "$cacheable" -eq 1 ]; then
    binary_digest="$(sha256_file "$1" 2>/dev/null || true)"
    wrapper_digest="$(sha256_file "$0" 2>/dev/null || true)"
    if [ -n "$binary_digest" ] && [ -n "$wrapper_digest" ]; then
        cache_key="$({
            printf '%s\0%s\0%s\0%s\0%s\0' \
                "$cache_schema" "$binary_digest" "$binary_name" "$wrapper_digest" \
                'example_tests::binary_path_coverage_report,stats::tests::chan_throughput_bounded_by_rc_floor'
            shift
            for arg in "$@"; do printf '%s\0' "$arg"; done
        } | sha256_stream 2>/dev/null || true)"
        binary_dir="$(cd "$(dirname "$1")" 2>/dev/null && pwd -P || true)"
        cache_root="${WITCHY_NEXTEST_LIST_CACHE_DIR:-${binary_dir:+$binary_dir/.witchy-nextest-list-cache}}"
        if [ -n "$cache_key" ] && [ -n "$cache_root" ] && mkdir -p "$cache_root" 2>/dev/null; then
            cache_entry="$cache_root/$cache_key"
            cache_lock="$cache_root/$cache_key.lock"
            if cache_entry_is_valid "$cache_entry" "$cache_key" \
                && use_cache_entry "$cache_entry" cache_hit; then
                exit 0
            fi

            while [ "$cache_lock_owned" -eq 0 ]; do
                if ln -s "$$" "$cache_lock" 2>/dev/null; then
                    cache_lock_owned=1
                    break
                fi
                if cache_entry_is_valid "$cache_entry" "$cache_key" \
                    && use_cache_entry "$cache_entry" cache_wait_hit; then
                    exit 0
                fi
                owner="$(readlink "$cache_lock" 2>/dev/null || true)"
                if [ -n "$owner" ] && ! pid_is_alive "$owner"; then
                    rm -f "$cache_lock" 2>/dev/null || true
                    continue
                fi
                # Never let a cache producer outlive the nextest run it serves.
                if ! pid_is_alive "$runner_pid"; then
                    echo "nextest-list-wrapper: nextest parent exited while waiting for discovery cache" >&2
                    exit 1
                fi
                sleep 0.05
            done
        fi
    fi
fi

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
command_status=0
"$@" >"$normal_output" || command_status=$?
cat "$normal_output"
if [ "$command_status" -eq 0 ] && [ "$cache_lock_owned" -eq 1 ] && [ -n "$cache_entry" ]; then
    cache_tmp="${cache_entry}.tmp-$$"
    cache_output_digest="$(sha256_file "$normal_output" 2>/dev/null || true)"
    if [ -n "$cache_output_digest" ] && mkdir "$cache_tmp" 2>/dev/null \
        && cp "$normal_output" "$cache_tmp/output" 2>/dev/null; then
        printf 'schema=1\nkey=%s\noutput_sha256=%s\n' \
            "$cache_key" "$cache_output_digest" >"$cache_tmp/manifest"
        if [ -e "$cache_entry" ]; then
            mv "$cache_entry" "${cache_entry}.invalid-$$" 2>/dev/null || true
        fi
        mv "$cache_tmp" "$cache_entry" 2>/dev/null || true
    fi
    record_telemetry cache_miss "$(( $(date +%s) - cache_started ))" "${cache_key:0:16}"
elif [ "$command_status" -eq 0 ]; then
    record_telemetry cache_bypass "$(( $(date +%s) - cache_started ))"
else
    record_telemetry discovery_failed "$(( $(date +%s) - cache_started ))" "${cache_key:0:16}"
fi
exit "$command_status"
