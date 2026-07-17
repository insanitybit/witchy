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
