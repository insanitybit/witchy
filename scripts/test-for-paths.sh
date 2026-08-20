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
#
# Crate packages and integration-test binaries used to be mutually exclusive:
# mixing them emitted WORKSPACE because `cargo nextest -p crate --test foo`
# ANDs the filters and drops whichever side is not in that package. The
# union is an inclusion expression (`package(a) or binary(b)`) plus `-p`
# args that cover every selected package. Integration tests live in the
# root `witchy` package, so that union adds `-p witchy` without pulling
# the rest of the workspace.
gate_pkgs=()
gate_tests=()
gate_need_examples=0
gate_need_stdlib_docs=0
gate_workspace=0
corpus_impact=0
gate_example_mods=()
gate_witchy_tests=()
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
add_example_mod() {
    local m="$1" x
    for x in ${gate_example_mods[0]+"${gate_example_mods[@]}"}; do [ "$x" = "$m" ] && return; done
    gate_example_mods+=("$m")
}
add_witchy_test() {
    local m="$1" x
    for x in ${gate_witchy_tests[0]+"${gate_witchy_tests[@]}"}; do [ "$x" = "$m" ] && return; done
    gate_witchy_tests+=("$m")
}
emit_gate_nextest() {
    local p t args="" expr="" m need_witchy=0
    if [ "$gate_workspace" -eq 1 ]; then
        printf 'WORKSPACE\n'
        return 0
    fi
    if [ "${#gate_pkgs[@]}" -eq 0 ] && [ "${#gate_tests[@]}" -eq 0 ] \
        && [ "$gate_need_examples" -eq 0 ] && [ "$gate_need_stdlib_docs" -eq 0 ] \
        && [ "${#gate_example_mods[@]}" -eq 0 ] \
        && [ "${#gate_witchy_tests[@]}" -eq 0 ]; then
        printf 'WORKSPACE\n'
        return 0
    fi
    # Pure integration-test selection keeps `--test` args so a JS-only
    # glamour/browser change stays one binary, not `-p witchy`.
    if [ "${#gate_tests[@]}" -gt 0 ] && [ "${#gate_pkgs[@]}" -eq 0 ] \
        && [ "$gate_need_examples" -eq 0 ] && [ "$gate_need_stdlib_docs" -eq 0 ] \
        && [ "${#gate_example_mods[@]}" -eq 0 ] \
        && [ "${#gate_witchy_tests[@]}" -eq 0 ]; then
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
    for t in ${gate_tests[0]+"${gate_tests[@]}"}; do
        need_witchy=1
        expr="${expr:+$expr or }binary($t)"
    done
    # The full example_tests matrix (prelude std, or src/example_tests.rs)
    # subsumes per-file module partitions. Touched example_tests files still
    # win over an unforced matrix: crate + one module is not every case.
    if [ "$gate_need_examples" -eq 1 ]; then
        need_witchy=1
        expr="${expr:+$expr or }test(/^example_tests::/)"
    elif [ "${#gate_example_mods[@]}" -gt 0 ]; then
        need_witchy=1
        for m in "${gate_example_mods[@]}"; do
            expr="${expr:+$expr or }test(/^example_tests::${m}::/)"
        done
    fi
    if [ "$gate_need_stdlib_docs" -eq 1 ]; then
        need_witchy=1
        expr="${expr:+$expr or }test(stdlib_docs_are_current)"
    fi
    for m in ${gate_witchy_tests[0]+"${gate_witchy_tests[@]}"}; do
        need_witchy=1
        expr="${expr:+$expr or }test(/^${m}/)"
    done
    if [ "$need_witchy" -eq 1 ]; then
        case " $args " in
            *" -p witchy "*) ;;
            *) args="${args:+$args }-p witchy" ;;
        esac
    fi
    [ -n "$args" ] || { printf 'WORKSPACE\n'; return 0; }
    printf '%s\n' "$args"
    [ -n "$expr" ] && printf '%s\n' "$expr"
    # Third line: check/clippy package set (mapped crates only — not the
    # extra -p witchy added so example_tests / integration tests can run).
    # Empty when there are no mapped crates.
    if [ "${#gate_pkgs[@]}" -gt 0 ]; then
        local cargs="" q
        for q in "${gate_pkgs[@]}"; do
            cargs="${cargs:+$cargs }-p $q"
        done
        printf '%s\n' "$cargs"
    fi
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
            add "cargo nextest run -p witchy-types"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-lower/*)
            any_rust=1
            add_pkg witchy-lower
            add "cargo nextest run -p witchy-lower"
            add "cargo nextest run -E 'test(/^example_tests::/)'"
            add "./scripts/check.sh --wasm" ;;
        crates/witchy-wir/*)
            any_rust=1
            add_pkg witchy-wir
            add "cargo nextest run -p witchy-wir"
            add "cargo nextest run -E 'test(/^example_tests::/)'"
            add "./scripts/check.sh --wasm" ;;
        crates/witchy-runtime/*)
            any_rust=1
            add_pkg witchy-runtime
            add "cargo nextest run -p witchy-runtime"
            add "cargo nextest run -E 'test(/^example_tests::/)'"
            add "./scripts/check.sh --wasm" ;;
        crates/witchy-syntax/*)
            any_rust=1
            add_pkg witchy-syntax
            add "cargo nextest run -p witchy-syntax"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-interp/* | crates/witchy-caps/*)
            any_rust=1
            add_pkg witchy-interp
            add_pkg witchy-caps
            add "cargo nextest run -p witchy-interp -p witchy-caps"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        src/main.rs | src/cli.rs | src/source.rs)
            gate_workspace=1
            add "cargo check -p witchy --all-targets"
            add "cargo clippy -p witchy --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use"
            add "cargo nextest run --bin witchy -E 'test(/^(checked_cli_pipeline_tests|cli::|runtime_parity_tests|source::tests|test_mode_link_tests)::/)'"
            add "cargo nextest run --test cli_subcommands"
            add "cargo nextest run -p witchy-syntax" ;;
        src/example_tests.rs)
            any_rust=1
            gate_need_examples=1
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        src/example_tests/*)
            any_rust=1
            _et="${p##*/}"
            add_example_mod "${_et%.rs}"
            add "cargo nextest run -E 'test(/^example_tests::/)'" ;;
        src/commands/web.rs | src/commands/web/*)
            any_rust=1
            add_witchy_test 'commands::web'
            add "cargo nextest run -E 'test(/^commands::web/)'" ;;
        src/commands/*)
            any_rust=1
            add_witchy_test 'commands::'
            add "cargo nextest run -E 'test(/^commands::/)'" ;;
        src/lsp.rs | src/lsp_tests.rs)
            any_rust=1
            add_witchy_test 'lsp'
            add "cargo nextest run -E 'test(/^lsp/)'" ;;
        src/diagnostic_golden_tests.rs | src/snapshots/*)
            any_rust=1
            add_witchy_test 'diagnostic_golden_tests'
            add "cargo nextest run -E 'test(/^diagnostic_golden_tests/)'" ;;
        src/lib.rs | src/artifact.rs | src/stats.rs | src/idp.rs | \
        src/trusted_exe.rs | src/capabilities_tests.rs)
            any_rust=1
            add_pkg witchy
            add "cargo nextest run -p witchy --lib" ;;
        src/bin/*)
            any_rust=1
            add_gate_test misc
            add "cargo nextest run --test misc" ;;
        crates/* | src/*)
            any_rust=1
            gate_workspace=1 ;;
        std/*.witchy)
            gate_need_examples=1
            gate_need_stdlib_docs=1
            add "cargo nextest run -E 'test(/^example_tests::/)'"
            add "cargo nextest run -E 'test(stdlib_docs_are_current)'"
            add "./target/debug/witchy fmt --check std/*.witchy" ;;
        README.md)
            corpus_impact=1
            add "cargo nextest run -E 'test(documentation_examples_are_valid)'" ;;
        examples/* | book/*)
            corpus_impact=1
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
            add_gate_test glamour
            add "cargo nextest run --test glamour -E 'test(/^dom::/)'" ;;
        web/witchy-runtime/abort-message.test.mjs | \
        web/witchy-runtime/playground-examples.test.mjs | \
        web/witchy-runtime/spike.mjs | \
        web/witchy-runtime/witchy-highlight.test.mjs | \
        web/witchy-runtime/witchy-runnable.test.mjs | \
        web/witchy-runtime/witchy-cell-sandbox.test.mjs | \
        web/witchy-highlight.js | \
        web/witchy-cell-sandbox.js | \
        web/witchy-cell-frame.js)
            add_gate_test browser
            add "cargo nextest run --test browser -E 'test(/^shim::/)'" ;;
        web/witchy-runtime/encoding-abi.test.mjs)
            add_gate_test browser
            add "cargo nextest run --test browser -E 'test(/^encoding::/)'" ;;
        web/witchy-runtime/import-catalog.test.mjs)
            add_gate_test misc
            add "cargo nextest run --test misc -E 'test(/^wasm_abi_catalog::/)'" ;;
        web/witchy-runtime/witchy-runtime.mjs)
            add_gate_test browser
            add_gate_test glamour
            add_gate_test misc
            add "cargo nextest run --test browser --test glamour --test misc -E 'binary(browser) or (binary(glamour) and test(/^dom::/)) or (binary(misc) and test(/^wasm_abi_catalog::/))'" ;;
        web/*)
            # Playground / highlighter JS is not a Rust surface. Keep a
            # focused browser binary rather than fail-safe --workspace.
            add_gate_test browser
            add "cargo nextest run --test browser" ;;
        tests/merge_queue.rs | tests/merge_queue/*.rs)
            any_rust=1
            add_gate_test merge_queue
            add "./scripts/check.sh --queue-infra" ;;
        tests/test_for_paths.rs)
            any_rust=1
            add_gate_test test_for_paths
            add "./scripts/check.sh --queue-infra" ;;
        tests/worktree/*.rs)
            any_rust=1
            add_gate_test worktree
            add "cargo nextest run --test worktree" ;;
        tests/e2e.rs)
            gate_workspace=1
            add "./scripts/check.sh --e2e" ;;
        tests/misc.rs | tests/misc/*)
            any_rust=1
            add_gate_test misc
            add "cargo nextest run --test misc" ;;
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
        .config/nextest.toml)
            gate_workspace=1
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/check.sh | \
        scripts/gate-report.sh | \
        scripts/merge-queue.sh | \
        scripts/state-paths.sh)
            add_gate_test merge_queue
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
        scripts/test-for-paths.sh | scripts/test-impact.py)
            add_gate_test test_for_paths
            add_gate_test merge_queue
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done"
            add "cargo nextest run --test test_for_paths"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/*.sh)
            add "for f in scripts/*.sh; do bash -n \"\$f\"; done" ;;
        justfile)
            add "just --list" ;;
        spec/stdlib.md)
            echo "WARNING: spec/stdlib.md is GENERATED — edit std/*.witchy doc-comments instead" >&2
            gate_need_stdlib_docs=1
            add "cargo nextest run -E 'test(stdlib_docs_are_current)'" ;;
        spec/* | CONTRIBUTING.md)
            # documentation_examples_are_valid walks README, CONTRIBUTING,
            # spec/, and book/src. A spec-only edit cannot change crate
            # tests; running the workspace would only re-prove master.
            add_witchy_test 'example_tests::example_sweeps::documentation_examples_are_valid'
            add "cargo nextest run -E 'test(documentation_examples_are_valid)'" ;;
        *.md | rfcs/* | bugs/* | wiki/*)
            : ;; # prose only — but book/README witchy blocks are covered above
    esac
done

# Corpus impact: partition example_tests by what the tests already name
# (std/foo.witchy strings, include_str, // gate-covers: labels, filename
# stems). Prelude std modules fail closed to the full matrix. python3
# missing or a nonempty corpus with no inferred tests also fails closed.
apply_corpus_impact() {
    local line impact
    command -v python3 >/dev/null 2>&1 || { gate_need_examples=1; return 0; }
    [ -f scripts/test-impact.py ] || { gate_need_examples=1; return 0; }
    impact="$(printf '%s\n' "${paths[@]}" | python3 scripts/test-impact.py --example-mods)" || {
        gate_need_examples=1
        return 0
    }
    [ -n "$impact" ] || return 0
    while IFS= read -r line; do
        case "$line" in
            full) gate_need_examples=1; return 0 ;;
            mod\ *) add_example_mod "${line#mod }" ;;
            test\ *) add_witchy_test "${line#test }" ;;
        esac
    done <<EOF
$impact
EOF
}

if [ "$corpus_impact" -eq 1 ]; then
    apply_corpus_impact
    if [ "$gate_need_examples" -eq 0 ] \
        && [ "${#gate_example_mods[@]}" -eq 0 ] \
        && [ "${#gate_witchy_tests[@]}" -eq 0 ]; then
        gate_need_examples=1
    fi
fi

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
