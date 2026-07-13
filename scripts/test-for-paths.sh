#!/usr/bin/env bash
# Map changed files to the focused checks worth running BEFORE a merge-queue
# submission (the full gate still runs there — this is the fast pre-flight).
#
#   ./scripts/test-for-paths.sh                    # diff of HEAD vs master
#   ./scripts/test-for-paths.sh <file>...          # explicit paths
#   ./scripts/test-for-paths.sh --run              # print AND run the commands
#
# The mapping is deliberately coarse: nextest filters by crate/binary, not by
# guessing individual test names. A rule firing means "this area is cheap
# enough to check and plausibly affected", not "only these tests can break".
set -euo pipefail
cd "$(dirname "$0")/.."

run=0
paths=()
for arg in "$@"; do
    case "$arg" in
        --run) run=1 ;;
        -h | --help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) paths+=("$arg") ;;
    esac
done
if [ "${#paths[@]}" -eq 0 ]; then
    while IFS= read -r p; do paths+=("$p"); done < <(git diff --name-only master...HEAD; git diff --name-only)
fi
if [ "${#paths[@]}" -eq 0 ]; then
    echo "test-for-paths: no changed files vs master (and no paths given)" >&2
    exit 0
fi

# Accumulate commands, deduped, in priority order.
cmds=()
add() { local c="$1"; local x; for x in ${cmds[0]+"${cmds[@]}"}; do [ "$x" = "$c" ] && return; done; cmds+=("$c"); }

any_rust=0
for p in "${paths[@]}"; do
    case "$p" in
        crates/witchy-types/*)
            any_rust=1
            add "cargo nextest run -p witchy-types"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-lower/* | crates/witchy-wir/* | crates/witchy-runtime/*)
            any_rust=1
            add "cargo nextest run -p witchy-lower -p witchy-wir -p witchy-runtime"
            add "cargo nextest run -E 'test(/^example_tests::/)'"     # the parity matrix
            add "./scripts/check.sh --wasm" ;;
        crates/witchy-syntax/*)
            any_rust=1
            add "cargo nextest run -p witchy-syntax"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-interp/* | crates/witchy-caps/*)
            any_rust=1
            add "cargo nextest run -p witchy-interp -p witchy-caps"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/* | src/*)
            any_rust=1 ;;
        std/*.witchy)
            add "cargo nextest run -E 'test(/^example_tests::/)'"
            add "cargo nextest run -E 'test(stdlib_docs_are_current)'"
            add "./target/debug/witchy fmt --check std/*.witchy" ;;
        README.md | examples/* | book/*)
            add "cargo nextest run -E 'test(/^example_tests::/)'"
            # A book/example change can flip a block's browser-runnability (e.g.
            # add a Console-only-footprint program that uses std/vm's worker ops —
            # runnable on native, but the browser shim can't instantiate it). The
            # --wasm shard rebuilds the browser wasm and runs the runnable-book
            # validator, catching that false Run button pre-submit.
            add "./scripts/check.sh --wasm" ;;
        projects/pm/* | projects/coven/* | projects/coven-web/* | projects/glamour/* | projects/docs/*)
            add "find projects -type f -path '*/src/*.witchy' -exec ./target/debug/witchy fmt --check {} +"
            add "./scripts/check.sh --e2e" ;;
        tests/e2e.rs)
            add "./scripts/check.sh --e2e" ;;
        scripts/*.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done" ;;
        spec/stdlib.md)
            echo "WARNING: spec/stdlib.md is GENERATED — edit std/*.witchy doc-comments instead" >&2
            add "cargo nextest run -E 'test(stdlib_docs_are_current)'" ;;
        *.md | rfcs/* | bugs/* | wiki/*)
            : ;; # prose only — but book/README witchy blocks are covered above
    esac
done
# Any Rust change ⇒ the fast gate belongs in the list, first.
if [ "$any_rust" -eq 1 ]; then
    cmds=("./scripts/check.sh --fast" ${cmds[0]+"${cmds[@]}"})
fi

if [ "${#cmds[@]}" -eq 0 ]; then
    echo "test-for-paths: prose-only change — no focused checks needed (still submit through the queue)"
    exit 0
fi

echo "focused checks for this change (run before 'merge-queue.sh submit'):"
for c in "${cmds[@]}"; do echo "  $c"; done

if [ "$run" -eq 1 ]; then
    for c in "${cmds[@]}"; do
        printf '\n\033[1;34m==> %s\033[0m\n' "$c"
        bash -c "$c"
    done
    printf '\n\033[1;32mall focused checks green\033[0m\n'
fi
