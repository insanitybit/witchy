#!/usr/bin/env bash
# shellcheck disable=SC2016
# Focused behavioral regressions for nextest-list-wrapper.sh. Uses system
# shells as fake test binaries; it does not invoke Cargo or nextest.
set -euo pipefail
# The single-quoted `bash -c`/`zsh -c` bodies intentionally expand MARKER and
# PEER in the child shell from the per-command environment below.
cd "$(dirname "$0")/.."

tmp="$(mktemp -d "/private/tmp/witchy-list-wrapper-test-XXXXXX")"
cleanup() {
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

wrapper="$PWD/scripts/nextest-list-wrapper.sh"

# The optimization is fail-closed around the source-level ignored-test policy.
"$wrapper" --validate-ignore-policy "$PWD"

# Persistent discovery is content-addressed. The exact same executable and
# argument vector executes once, while later nextest runs reuse the complete
# validated output and emit an explicit cache-hit record.
fake_list="$tmp/fake-list"
printf '%s\n' \
    '#!/bin/bash' \
    'printf "run\n" >>"$COUNT_FILE"' \
    '[ "${FAIL_DISCOVERY:-0}" -eq 0 ] || exit 7' \
    '[ "${SLOW_DISCOVERY:-0}" -eq 0 ] || sleep 0.2' \
    'printf "%s\n" "cached_test: test"' >"$fake_list"
chmod +x "$fake_list"
cache_dir="$tmp/cache"
telemetry="$tmp/telemetry"
count_file="$tmp/cache-runs"
COUNT_FILE="$count_file" NEXTEST_RUN_ID=cache-miss TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$cache_dir" WITCHY_LIST_TELEMETRY_FILE="$telemetry" \
    "$wrapper" "$fake_list" --list >"$tmp/cache-miss.out"
COUNT_FILE="$count_file" NEXTEST_RUN_ID=cache-hit TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$cache_dir" WITCHY_LIST_TELEMETRY_FILE="$telemetry" \
    "$wrapper" "$fake_list" --list >"$tmp/cache-hit.out"
[ "$(wc -l <"$count_file" | tr -d ' ')" -eq 1 ]
[ "$(cat "$tmp/cache-miss.out")" = "cached_test: test" ]
[ "$(cat "$tmp/cache-hit.out")" = "cached_test: test" ]
grep -q 'event=cache_miss' "$telemetry"
grep -q 'event=cache_hit' "$telemetry"

# Arguments and executable bytes are proof inputs, so either change causes a
# real discovery instead of reusing a merely similar result.
COUNT_FILE="$count_file" NEXTEST_RUN_ID=cache-args TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$cache_dir" \
    "$wrapper" "$fake_list" --list --format terse >/dev/null
[ "$(wc -l <"$count_file" | tr -d ' ')" -eq 2 ]
printf '%s\n' '# cache-key mutation' >>"$fake_list"
COUNT_FILE="$count_file" NEXTEST_RUN_ID=cache-binary TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$cache_dir" \
    "$wrapper" "$fake_list" --list >/dev/null
[ "$(wc -l <"$count_file" | tr -d ' ')" -eq 3 ]

# A failed discovery is never published. Repeating the identical command after
# the transient failure must execute the binary again and can then populate it.
failure_cache="$tmp/failure-cache"
failure_count="$tmp/failure-runs"
set +e
COUNT_FILE="$failure_count" FAIL_DISCOVERY=1 NEXTEST_RUN_ID=cache-failure TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$failure_cache" \
    "$wrapper" "$fake_list" --list >"$tmp/cache-failure.out"
failure_status=$?
set -e
[ "$failure_status" -eq 7 ]
COUNT_FILE="$failure_count" NEXTEST_RUN_ID=cache-recover TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$failure_cache" \
    "$wrapper" "$fake_list" --list >/dev/null
[ "$(wc -l <"$failure_count" | tr -d ' ')" -eq 2 ]

# Cache entries carry their output digest. Corrupting a complete entry makes
# the next wrapper execute and atomically replace it rather than trust it.
corrupt_cache="$tmp/corrupt-cache"
corrupt_count="$tmp/corrupt-runs"
COUNT_FILE="$corrupt_count" NEXTEST_RUN_ID=corrupt-prime TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$corrupt_cache" \
    "$wrapper" "$fake_list" --list >/dev/null
cache_output="$(find "$corrupt_cache" -type f -name output -print -quit)"
[ -n "$cache_output" ]
printf '%s\n' 'poisoned_test: test' >"$cache_output"
COUNT_FILE="$corrupt_count" NEXTEST_RUN_ID=corrupt-recover TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$corrupt_cache" \
    "$wrapper" "$fake_list" --list >"$tmp/corrupt-recover.out"
[ "$(wc -l <"$corrupt_count" | tr -d ' ')" -eq 2 ]
[ "$(cat "$tmp/corrupt-recover.out")" = "cached_test: test" ]

# Concurrent nextest runs share one producer for an identical proof key. The
# waiter consumes the atomically published entry without taking a cold slot.
shared_cache="$tmp/shared-cache"
shared_count="$tmp/shared-runs"
COUNT_FILE="$shared_count" SLOW_DISCOVERY=1 NEXTEST_RUN_ID=shared-a TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$shared_cache" \
    "$wrapper" "$fake_list" --list >"$tmp/shared-a.out" &
shared_a=$!
COUNT_FILE="$shared_count" SLOW_DISCOVERY=1 NEXTEST_RUN_ID=shared-b TMPDIR="$tmp" \
    WITCHY_NEXTEST_LIST_CACHE_DIR="$shared_cache" \
    "$wrapper" "$fake_list" --list >"$tmp/shared-b.out" &
shared_b=$!
wait "$shared_a"
wait "$shared_b"
[ "$(wc -l <"$shared_count" | tr -d ' ')" -eq 1 ]
[ "$(cat "$tmp/shared-a.out")" = "cached_test: test" ]
[ "$(cat "$tmp/shared-b.out")" = "cached_test: test" ]

# Same-binary ignored discovery waits for normal completion, then derives the
# audited ignored names from the cached ordinary output without a second exec.
marker="$tmp/normal-finished"
MARKER="$marker" NEXTEST_RUN_ID=pair TMPDIR="$tmp" WITCHY_NEXTEST_LIST_JOBS=2 \
    "$wrapper" /bin/bash -c 'sleep 0.2; : >"$MARKER"; printf "%s\n" "normal_test: test" "example_tests::binary_path_coverage_report: test"' \
    >"$tmp/normal.out" &
normal_pid=$!
MARKER="$marker" NEXTEST_RUN_ID=pair TMPDIR="$tmp" WITCHY_NEXTEST_LIST_JOBS=2 \
    "$wrapper" /bin/bash -c 'echo "second exec must not run" >&2; exit 99' --ignored \
    >"$tmp/ignored.out" &
ignored_pid=$!
wait "$normal_pid"
wait "$ignored_pid"
[ "$(sed -n '1p' "$tmp/normal.out")" = "normal_test: test" ]
[ "$(sed -n '2p' "$tmp/normal.out")" = "example_tests::binary_path_coverage_report: test" ]
[ "$(cat "$tmp/ignored.out")" = "example_tests::binary_path_coverage_report: test" ]

# A failing ordinary invocation still publishes its cached output and completion
# from its EXIT trap, so the ignored peer cannot deadlock or execute the binary.
set +e
NEXTEST_RUN_ID=failure TMPDIR="$tmp" \
    "$wrapper" /bin/bash -c 'echo "stats::tests::chan_throughput_bounded_by_rc_floor: test"; exit 7' >"$tmp/failing-normal.out" &
normal_pid=$!
NEXTEST_RUN_ID=failure TMPDIR="$tmp" \
    "$wrapper" /bin/bash -c 'exit 99' --ignored \
    >"$tmp/failure-ignored.out" &
ignored_pid=$!
wait "$normal_pid"
normal_status=$?
wait "$ignored_pid"
ignored_status=$?
set -e
[ "$normal_status" -eq 7 ]
[ "$ignored_status" -eq 0 ]
[ "$(cat "$tmp/failure-ignored.out")" = "stats::tests::chan_throughput_bounded_by_rc_floor: test" ]

# An explicit width-1 override serializes distinct cold binaries. Start A first,
# then require B to observe A's completion when it acquires the only slot.
MARKER="$tmp/override-a-started" END="$tmp/override-a-ended" NEXTEST_RUN_ID=override-serial TMPDIR="$tmp" WITCHY_NEXTEST_LIST_JOBS=1 \
    "$wrapper" /bin/bash -c ': >"$MARKER"; sleep 0.2; : >"$END"' >/dev/null &
first_pid=$!
deadline=$((SECONDS + 5))
while [ ! -e "$tmp/override-a-started" ] && [ "$SECONDS" -lt "$deadline" ]; do sleep 0.01; done
[ -e "$tmp/override-a-started" ]
END="$tmp/override-a-ended" NEXTEST_RUN_ID=override-serial TMPDIR="$tmp" WITCHY_NEXTEST_LIST_JOBS=1 \
    "$wrapper" /bin/zsh -c '[ -e "$END" ]' >/dev/null &
second_pid=$!
wait "$first_pid"
wait "$second_pid"

# The default width 2 keeps bounded parallel discovery available.
# Each command waits for the other's start marker; serialization would fail.
MARKER="$tmp/a-started" PEER="$tmp/b-started" NEXTEST_RUN_ID=parallel TMPDIR="$tmp" \
    "$wrapper" /bin/bash -c '
        : >"$MARKER"
        i=0
        while [ ! -e "$PEER" ] && [ "$i" -lt 100 ]; do sleep 0.01; i=$((i + 1)); done
        [ -e "$PEER" ]
    ' >/dev/null &
first_pid=$!
MARKER="$tmp/b-started" PEER="$tmp/a-started" NEXTEST_RUN_ID=parallel TMPDIR="$tmp" \
    "$wrapper" /bin/zsh -c '
        : >"$MARKER"
        i=0
        while [ ! -e "$PEER" ] && [ "$i" -lt 100 ]; do sleep 0.01; i=$((i + 1)); done
        [ -e "$PEER" ]
    ' >/dev/null &
second_pid=$!
wait "$first_pid"
wait "$second_pid"

# An ignored waiter whose nextest parent dies exits instead of becoming an
# orphan. The intermediate shell is the captured parent and intentionally dies.
pid_file="$tmp/parent-death.pid"
WRAPPER="$wrapper" TMP="$tmp" bash -c '
    NEXTEST_RUN_ID=parent-death TMPDIR="$TMP" "$WRAPPER" /bin/true --ignored &
    child=$!
    printf "%s\n" "$child" >"$TMP/parent-death.pid"
    wait "$child"
' &
parent_pid=$!
deadline=$((SECONDS + 5))
while [ ! -s "$pid_file" ] && [ "$SECONDS" -lt "$deadline" ]; do sleep 0.05; done
[ -s "$pid_file" ]
waiter_pid="$(cat "$pid_file")"
# Ensure the wrapper has started and captured the intermediate shell as its
# parent before killing that shell; otherwise macOS may still be launching the
# child and reparent it to launchd before the script reads PPID.
root="$tmp/witchy-nextest-list-parent-death"
deadline=$((SECONDS + 5))
while [ ! -d "$root" ] && [ "$SECONDS" -lt "$deadline" ]; do sleep 0.05; done
[ -d "$root" ]
kill -TERM "$parent_pid"
wait "$parent_pid" 2>/dev/null || true
deadline=$((SECONDS + 5))
while kill -0 "$waiter_pid" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ]; do sleep 0.05; done
if kill -0 "$waiter_pid" 2>/dev/null; then
    echo "ignored waiter survived parent death" >&2
    exit 1
fi

echo "nextest-list-wrapper tests passed"
