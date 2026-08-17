#!/usr/bin/env bash
# Map changed files to the focused checks worth running BEFORE a merge-queue
# submission (the full gate still runs there — this is the fast pre-flight).
#
#   ./scripts/test-for-paths.sh                    # diff of HEAD vs master
#   ./scripts/test-for-paths.sh <file>...          # explicit paths
#   ./scripts/test-for-paths.sh --run              # print AND run the commands
#   ./scripts/test-for-paths.sh --gate-nextest     # serialized-gate selection:
#                                                 # print WORKSPACE, or a -p/--test
#                                                 # line plus an optional inclusion
#                                                 # expression. Paths from args or stdin.
#
# The mapping is deliberately coarse: nextest filters by crate/binary, not by
# guessing individual test names. A rule firing means "this area is cheap
# enough to check and plausibly affected", not "only these tests can break".
set -euo pipefail
cd "$(dirname "$0")/.."

run=0
gate_nextest=0
paths=()
for arg in "$@"; do
    case "$arg" in
        --run) run=1 ;;
        --gate-nextest) gate_nextest=1 ;;
        -h | --help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) paths+=("$arg") ;;
    esac
done
if [ "${#paths[@]}" -eq 0 ]; then
    if [ "$gate_nextest" -eq 1 ] && [ ! -t 0 ]; then
        while IFS= read -r p; do
            [ -n "$p" ] && paths+=("$p")
        done
    else
        while IFS= read -r p; do paths+=("$p"); done < <(
            git diff --name-only master...HEAD
            git diff --name-only --cached
            git diff --name-only
        )
    fi
fi
if [ "${#paths[@]}" -eq 0 ]; then
    echo "test-for-paths: no changed files vs master (and no paths given)" >&2
    exit 0
fi

# Accumulate commands, deduped, in priority order.
cmds=()
add() { local c="$1"; local x; for x in ${cmds[0]+"${cmds[@]}"}; do [ "$x" = "$c" ] && return; done; cmds+=("$c"); }

# Serialized-gate nextest selection (crate/binary only). Fail-safe to
# WORKSPACE when the diff is broader than the mapped areas.
gate_pkgs=()
gate_tests=()
gate_need_examples=0
gate_need_stdlib_docs=0
gate_workspace=0
add_pkg() {
    local p="$1" x
    for x in ${gate_pkgs[0]+"${gate_pkgs[@]}"}; do [ "$x" = "$p" ] && return; done
    gate_pkgs+=("$p")
}
add_gate_test() {
    local t="$1" x
    for x in ${gate_tests[0]+"${gate_tests[@]}"}; do [ "$x" = "$t" ] && return; done
    gate_tests+=("$t")
}
emit_gate_nextest() {
    local p t args="" expr=""
    if [ "$gate_workspace" -eq 1 ]; then
        printf 'WORKSPACE\n'
        return 0
    fi
    if [ "${#gate_pkgs[@]}" -eq 0 ] && [ "${#gate_tests[@]}" -eq 0 ] \
        && [ "$gate_need_examples" -eq 0 ] && [ "$gate_need_stdlib_docs" -eq 0 ]; then
        printf 'WORKSPACE\n'
        return 0
    fi
    # Mixing integration-test binaries with crate packages is a broader
    # surface than one focused invocation; fail closed to the full suite.
    if [ "${#gate_tests[@]}" -gt 0 ] && [ "${#gate_pkgs[@]}" -gt 0 ]; then
        printf 'WORKSPACE\n'
        return 0
    fi
    if [ "${#gate_tests[@]}" -gt 0 ]; then
        for t in "${gate_tests[@]}"; do
            args="${args:+$args }--test $t"
        done
        printf '%s\n' "$args"
        return 0
    fi
    for p in ${gate_pkgs[0]+"${gate_pkgs[@]}"}; do
        args="${args:+$args }-p $p"
        expr="${expr:+$expr or }package($p)"
    done
    if [ "$gate_need_examples" -eq 1 ] || [ "$gate_need_stdlib_docs" -eq 1 ]; then
        args="${args:+$args }-p witchy"
        [ "$gate_need_examples" -eq 1 ] && expr="${expr:+$expr or }test(/^example_tests::/)"
        [ "$gate_need_stdlib_docs" -eq 1 ] && expr="${expr:+$expr or }test(stdlib_docs_are_current)"
    fi
    [ -n "$args" ] || { printf 'WORKSPACE\n'; return 0; }
    printf '%s\n' "$args"
    [ -n "$expr" ] && printf '%s\n' "$expr"
}

any_rust=0
for p in "${paths[@]}"; do
    case "$p" in
        web/witchy-runtime/*.mjs)
            add "find web/witchy-runtime -type f -name '*.mjs' -exec node --check {} \\;" ;;
    esac
    case "$p" in
        crates/witchy-types/*)
            any_rust=1
            add_pkg witchy-types
            gate_need_examples=1
            add "cargo nextest run -p witchy-types"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-lower/* | crates/witchy-wir/* | crates/witchy-runtime/*)
            any_rust=1
            add_pkg witchy-lower
            add_pkg witchy-wir
            add_pkg witchy-runtime
            gate_need_examples=1
            add "cargo nextest run -p witchy-lower -p witchy-wir -p witchy-runtime"
            add "cargo nextest run -E 'test(/^example_tests::/)'"     # the parity matrix
            add "./scripts/check.sh --wasm" ;;
        crates/witchy-syntax/*)
            any_rust=1
            add_pkg witchy-syntax
            gate_need_examples=1
            add "cargo nextest run -p witchy-syntax"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-interp/* | crates/witchy-caps/*)
            any_rust=1
            add_pkg witchy-interp
            add_pkg witchy-caps
            gate_need_examples=1
            add "cargo nextest run -p witchy-interp -p witchy-caps"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        src/main.rs | src/cli.rs | src/source.rs)
            gate_workspace=1
            add "cargo check -p witchy --all-targets"
            add "cargo clippy -p witchy --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use"
            add "cargo nextest run --bin witchy -E 'test(/^(checked_cli_pipeline_tests|cli::|runtime_parity_tests|source::tests|test_mode_link_tests)::/)'"
            add "cargo nextest run --test cli_subcommands"
            add "cargo nextest run -p witchy-syntax" ;;
        src/example_tests.rs | src/example_tests/*)
            any_rust=1
            gate_need_examples=1
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/* | src/*)
            any_rust=1
            gate_workspace=1 ;;
        std/*.witchy)
            gate_need_examples=1
            gate_need_stdlib_docs=1
            add "cargo nextest run -E 'test(/^example_tests::/)'"
            add "cargo nextest run -E 'test(stdlib_docs_are_current)'"
            add "./target/debug/witchy fmt --check std/*.witchy" ;;
        README.md | examples/* | book/*)
            gate_need_examples=1
            add "cargo nextest run -E 'test(/^example_tests::/)'"
            # A book/example change can flip a block's browser-runnability (e.g.
            # add a Console-only-footprint program that uses std/vm's worker ops —
            # runnable on native, but the browser shim can't instantiate it). The
            # --wasm shard rebuilds the browser wasm and runs the runnable-book
            # validator, catching that false Run button pre-submit.
            add "./scripts/check.sh --wasm" ;;
        projects/grimoire/* | projects/coven/* | projects/coven-web/* | projects/glamour/* | projects/docs/*)
            add "find projects -type f -path '*/src/*.witchy' -exec ./target/debug/witchy fmt --check {} +"
            add "./scripts/check.sh --e2e" ;;
        web/witchy-runtime/glamour-*.mjs | \
        web/witchy-runtime/heap-reset.test.mjs | \
        web/witchy-runtime/highlighter.test.mjs | \
        web/witchy-runtime/user-cap-export.test.mjs)
            add "cargo nextest run --test glamour -E 'test(/^dom::/)'" ;;
        web/witchy-runtime/abort-message.test.mjs | \
        web/witchy-runtime/playground-examples.test.mjs | \
        web/witchy-runtime/spike.mjs | \
        web/witchy-runtime/witchy-highlight.test.mjs | \
        web/witchy-runtime/witchy-runnable.test.mjs | \
        web/witchy-runtime/witchy-cell-sandbox.test.mjs | \
        web/witchy-cell-sandbox.js | \
        web/witchy-cell-frame.js)
            add "cargo nextest run --test browser -E 'test(/^shim::/)'" ;;
        web/witchy-runtime/encoding-abi.test.mjs)
            add "cargo nextest run --test browser -E 'test(/^encoding::/)'" ;;
        web/witchy-runtime/import-catalog.test.mjs)
            add "cargo nextest run --test misc -E 'test(/^wasm_abi_catalog::/)'" ;;
        web/witchy-runtime/witchy-runtime.mjs)
            add "cargo nextest run --test browser --test glamour --test misc -E 'binary(browser) or (binary(glamour) and test(/^dom::/)) or (binary(misc) and test(/^wasm_abi_catalog::/))'" ;;
        tests/merge_queue.rs | tests/merge_queue/*.rs | tests/test_for_paths.rs)
            any_rust=1
            gate_workspace=1
            add "./scripts/check.sh --queue-infra" ;;
        tests/worktree/*.rs)
            any_rust=1
            add_gate_test worktree
            add "cargo nextest run --test worktree" ;;
        tests/e2e.rs)
            gate_workspace=1
            add "./scripts/check.sh --e2e" ;;
        tests/*.rs)
            any_rust=1
            test_name="${p#tests/}"
            test_name="${test_name%.rs}"
            add_gate_test "$test_name"
            printf -v test_name_q '%q' "$test_name"
            add "cargo nextest run --test $test_name_q" ;;
        Cargo.toml | Cargo.lock | build.rs | .cargo/* | rust-toolchain | rust-toolchain.toml)
            gate_workspace=1 ;;
        .github/workflows/* | .github/actions/* | .github/zizmor.yml)
            add "./scripts/zizmor.sh --quiet --no-progress --persona=pedantic .github/workflows" ;;
        scripts/zizmor.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "./scripts/zizmor.sh --quiet --no-progress --persona=pedantic .github/workflows" ;;
        scripts/nextest-list-wrapper.sh | scripts/test-nextest-list-wrapper.sh)
            gate_workspace=1
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "./scripts/test-nextest-list-wrapper.sh"
            add "./scripts/check.sh --queue-infra" ;;
        .config/nextest.toml | \
        scripts/check.sh | \
        scripts/gate-report.sh | \
        scripts/merge-queue.sh | \
        scripts/state-paths.sh)
            gate_workspace=1
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/worktree-status.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "cargo nextest run --test worktree"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/rfc-status.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "cargo nextest run --test worktree" ;;
        scripts/worktree-warm.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "cargo nextest run --test worktree"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/worktree-create.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "cargo nextest run --test worktree" ;;
        scripts/check-spec-freshness.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "./scripts/check-spec-freshness.sh" ;;
        scripts/test-for-paths.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "cargo nextest run --test test_for_paths"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/*.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done" ;;
        justfile)
            add "just --list" ;;
        spec/stdlib.md)
            echo "WARNING: spec/stdlib.md is GENERATED — edit std/*.witchy doc-comments instead" >&2
            add "cargo nextest run -E 'test(stdlib_docs_are_current)'" ;;
        *.md | rfcs/* | bugs/* | wiki/*)
            : ;; # prose only — but book/README witchy blocks are covered above
    esac
done
if [ "$gate_nextest" -eq 1 ]; then
    emit_gate_nextest
    exit 0
fi

# Any Rust change means the fast gate already runs every workspace nextest test.
# Drop narrower nextest selections it subsumes, but retain independent shards
# such as WASM, e2e, and Witchy source formatting.
if [ "$any_rust" -eq 1 ]; then
    remaining=()
    for c in ${cmds[0]+"${cmds[@]}"}; do
        case "$c" in
            "cargo nextest run -p "* | \
            "cargo nextest run --test "* | \
            "cargo nextest run --bin "* | \
            "cargo nextest run -E 'test(/^example_tests::/)'" | \
            "cargo nextest run -E 'test(stdlib_docs_are_current)'" | \
            "cargo check -p witchy --all-targets" | \
            "cargo clippy -p witchy --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use") ;;
            *) remaining+=("$c") ;;
        esac
    done
    cmds=("./scripts/check.sh --fast" ${remaining[0]+"${remaining[@]}"})
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
