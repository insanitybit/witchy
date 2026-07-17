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

# Same-binary ignored discovery waits for normal completion. The ignored
# command fails if it starts before the normal command publishes its marker.
marker="$tmp/normal-finished"
MARKER="$marker" NEXTEST_RUN_ID=pair TMPDIR="$tmp" WITCHY_NEXTEST_LIST_JOBS=2 \
    "$wrapper" /bin/bash -c 'sleep 0.2; : >"$MARKER"; echo "normal_test: test"' \
    >"$tmp/normal.out" &
normal_pid=$!
MARKER="$marker" NEXTEST_RUN_ID=pair TMPDIR="$tmp" WITCHY_NEXTEST_LIST_JOBS=2 \
    "$wrapper" /bin/bash -c '[ -e "$MARKER" ]; echo "ignored_test: test"' --ignored \
    >"$tmp/ignored.out" &
ignored_pid=$!
wait "$normal_pid"
wait "$ignored_pid"
[ "$(cat "$tmp/normal.out")" = "normal_test: test" ]
[ "$(cat "$tmp/ignored.out")" = "ignored_test: test" ]

# A failing ordinary invocation still publishes completion from its EXIT trap,
# so the ignored peer runs and the pair cannot deadlock.
set +e
NEXTEST_RUN_ID=failure TMPDIR="$tmp" \
    "$wrapper" /bin/bash -c 'exit 7' >"$tmp/failing-normal.out" &
normal_pid=$!
NEXTEST_RUN_ID=failure TMPDIR="$tmp" \
    "$wrapper" /bin/bash -c 'echo "ignored_test: test"' --ignored \
    >"$tmp/failure-ignored.out" &
ignored_pid=$!
wait "$normal_pid"
normal_status=$?
wait "$ignored_pid"
ignored_status=$?
set -e
[ "$normal_status" -eq 7 ]
[ "$ignored_status" -eq 0 ]
[ "$(cat "$tmp/failure-ignored.out")" = "ignored_test: test" ]

# Different binaries retain the global concurrency allowed by separate keys.
# Each command waits for the other's start marker; serialization would fail.
MARKER="$tmp/a-started" PEER="$tmp/b-started" NEXTEST_RUN_ID=parallel TMPDIR="$tmp" WITCHY_NEXTEST_LIST_JOBS=2 \
    "$wrapper" /bin/bash -c '
        : >"$MARKER"
        i=0
        while [ ! -e "$PEER" ] && [ "$i" -lt 100 ]; do sleep 0.01; i=$((i + 1)); done
        [ -e "$PEER" ]
    ' >/dev/null &
first_pid=$!
MARKER="$tmp/b-started" PEER="$tmp/a-started" NEXTEST_RUN_ID=parallel TMPDIR="$tmp" WITCHY_NEXTEST_LIST_JOBS=2 \
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
