#!/usr/bin/env bash
# Map changed files to the focused checks worth running BEFORE a merge-queue
# submission (the full gate still runs there — this is the fast pre-flight).
#
#   ./scripts/test-for-paths.sh                    # diff of HEAD vs master
#   ./scripts/test-for-paths.sh <file>...          # explicit paths
#   ./scripts/test-for-paths.sh --run              # print AND run the commands
#   ./scripts/test-for-paths.sh --focused          # focused crate-only checks (no check.sh --fast fallback)
#   ./scripts/test-for-paths.sh --focused --run    # run focused crate-only checks
#   ./scripts/test-for-paths.sh --edit-loop        # cheapest named probes when available
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
focused=0
edit_loop=0
gate_nextest=0
paths=()
for arg in "$@"; do
    case "$arg" in
        --run) run=1 ;;
        --focused) focused=1 ;;
        --edit-loop) edit_loop=1; focused=1 ;;
        --gate-nextest) gate_nextest=1 ;;
        -h | --help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) paths+=("$arg") ;;
    esac
done
explicit_paths="${#paths[@]}"
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

# Accumulate commands, deduped, in priority order. Early probes are kept ahead
# of the broader Rust shard: they catch cheap structural failures before a
# multi-minute build/test cycle reaches the same assertion.
cmds=()
early_cmds=()
canonical_native_batch=()
add() { local c="$1"; local x; for x in ${cmds[0]+"${cmds[@]}"}; do [ "$x" = "$c" ] && return; done; cmds+=("$c"); }
add_early() { local c="$1"; local x; for x in ${early_cmds[0]+"${early_cmds[@]}"}; do [ "$x" = "$c" ] && return; done; early_cmds+=("$c"); }

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
#
# MEASURED COST OF THAT UNION (2026-08-27), and why it has not been fixed:
# `-p witchy` makes cargo BUILD all 24 of the package's test binaries, while
# the expression then runs only the ~15 the diff selected. Timed back to back
# after `touch src/lib.rs`: building all 24 takes 63s, building just the
# rfc0* 15 takes 24s -- so a compiler-stage gate spends ~39s per attempt
# compiling and linking binaries it will filter out at runtime, against a
# ~166s median gate.
#
# The obvious fix (emit `--test rfc0NNN` instead of `-p witchy`) is WRONG for
# the reason above, and the failure is silent: measured on the same tree,
# `nextest list -p witchy-lower -p witchy` reports 2174 tests, while
# `-p witchy-lower -p witchy --test rfc0101` reports 2. The AND drops
# witchy-lower entirely and the gate would go green having skipped 2,172
# tests. Do not narrow the build this way.
#
# The only correct route is to split the tests stage into TWO nextest
# invocations (one per package scope) so neither needs `--test` to cross a
# package boundary. That is a real redesign: check.sh's fail-fast
# (--max-fail=1:immediate), the workspace test budget, the preflight replay
# and the serial test-groups all currently assume a single invocation.
gate_pkgs=()
gate_tests=()
gate_need_examples=0
gate_need_stdlib_docs=0
gate_workspace=0
corpus_impact=0
gate_example_mods=()
gate_witchy_tests=()
gate_need_conformance=0
gate_need_std_roots=0
broad_e2e=0
add_pkg() {
    local p="$1" x
    for x in ${gate_pkgs[0]+"${gate_pkgs[@]}"}; do [ "$x" = "$p" ] && return; done
    gate_pkgs+=("$p")
}
add_gate_test() {
    local t="$1" x
    # `case` globs treat `*` as matching `/`, so the generic `tests/*.rs`
    # route can receive a nested module such as `tests/browser/shim.rs`.
    # Nested modules are not Cargo integration-test binary names. Fail safe to
    # the workspace instead of emitting an invalid `binary(browser/shim)`
    # nextest selector that prevents the serialized gate from starting.
    case "$t" in
        */*) gate_workspace=1; return ;;
    esac
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
    # RFC conformance binaries adjudicate semantics ACROSS the pipeline, which
    # is precisely what a single-crate selection cannot see. Master broke on
    # 2026-08-27 because an async-lowering change in witchy-types gated green
    # over 1,068 tests in 15.4s without ever running rfc0130_generator, whose
    # owned-frame census the change invalidated. They are cheap -- most average
    # about a second -- so a compiler-stage change always runs them.
    if [ "$gate_need_conformance" -eq 1 ]; then
        need_witchy=1
        expr="${expr:+$expr or }binary_id(/^witchy::rfc0/)"
        # Same argument, one binary further. `semantic_conformance` holds
        # independent expected results -- the adjudication a backend-agreement
        # test cannot give -- but it is a module of `witchy::misc`, which a
        # single-crate selection never reaches. A canonical-checker change
        # therefore gated green without it: the component that decides whether
        # every program is admitted was judged without the suite that says what
        # the programs should do. Four tests.
        expr="${expr:+$expr or }test(/^semantic_conformance::/)"
        # And one module family further: `example_tests::rfc0*` pins language
        # contracts (what an RFC admits and REFUSES) as examples inside the
        # `witchy` bin, which this selection otherwise never reaches because
        # the whole example matrix is 36% of the suite. On 2026-09-22 two
        # landed view-carrier changes in witchy-types/-lower/-wir gated green
        # and left master red on exactly two of these refusal pins. ~33 tests,
        # ~30s summed, none over 5s.
        expr="${expr:+$expr or }test(/^example_tests::rfc0/)"
    fi
    if [ "$gate_need_stdlib_docs" -eq 1 ]; then
        need_witchy=1
        expr="${expr:+$expr or }test(stdlib_docs_are_current)"
    fi
    # One 0.2s test. Its binary is witchy-types' lib test, which the gate's
    # warm target already holds unless witchy-types itself changed, and then
    # the crate is selected anyway.
    if [ "$gate_need_std_roots" -eq 1 ]; then
        case " $args " in
            *" -p witchy-types "*) ;;
            *) args="${args:+$args }-p witchy-types" ;;
        esac
        expr="${expr:+$expr or }test(=std_pruning::tests::pruning_roots_cover_every_compiler_std_literal)"
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
    # `-p witchy` makes cargo BUILD all 24 of the package's test binaries even
    # when the expression runs tests from ONE of them. Measured 2026-08-27: a
    # docs gate spent 7m14s compiling to reach "Starting 1 test across 24
    # binaries", while holding the serialized gate lock against 21 queued
    # branches.
    #
    # `commands`, `lsp`, `diagnostic_golden_tests`, and `example_tests` (with
    # `stdlib_docs_are_current` inside it) are declared in src/main.rs, which IS
    # the bin target. They live in `witchy::bin/witchy` structurally, not by
    # coincidence. Adding `--bin witchy` builds that one binary instead of 24.
    #
    # Narrow ONLY when `-p witchy` stands alone. `--bin`/`--test` AND with `-p`
    # rather than unioning (see the note at the top of this file): measured on
    # one tree, `-p witchy-lower -p witchy` lists 2174 tests while adding
    # `--test rfc0101` lists 2. A crate package or the rfc0* conformance
    # binaries name targets outside the bin, so either disables the narrowing.
    #
    # `add_witchy_test` also takes free-form names (including from stdin), and
    # those may live in any binary. Narrow only the module families pinned to
    # main.rs above.
    local witchy_tests_all_bin=1 gwt
    for gwt in ${gate_witchy_tests[0]+"${gate_witchy_tests[@]}"}; do
        case "$gwt" in
            commands::* | lsp* | diagnostic_golden_tests* | example_tests*) ;;
            *) witchy_tests_all_bin=0 ;;
        esac
    done
    if [ "$need_witchy" -eq 1 ] \
        && [ "${#gate_pkgs[@]}" -eq 0 ] \
        && [ "${#gate_tests[@]}" -eq 0 ] \
        && [ "$gate_need_conformance" -eq 0 ] \
        && [ "$witchy_tests_all_bin" -eq 1 ] \
        && { [ "$gate_need_examples" -eq 1 ] \
             || [ "${#gate_example_mods[@]}" -gt 0 ] \
             || [ "$gate_need_stdlib_docs" -eq 1 ] \
             || [ "${#gate_witchy_tests[@]}" -gt 0 ]; }; then
        args="$args --bin witchy"
    fi
    # NAME EVERY TARGET instead of building whole packages.
    #
    # `-p witchy` builds every root test binary even when the expression runs
    # a handful of them. Narrowing with `--test` alone is UNSOUND -- target
    # filters are global, so `--test misc` also drops each crate package's own
    # lib and integration tests (measured: 803 listed tests fell to 160 and
    # still reported green). But the filters UNION: naming every needed target
    # across every selected package keeps the exact set. Measured 2026-09-21 on
    # a typeck change: 803 listed tests either way, nothing missing, and the
    # build 60.6 -> 50.8 CPU s (-16%), best of 3 alternating.
    #
    # Fail SAFE to the whole-package form whenever the target set is not
    # provably complete from the filesystem:
    #   - the bin target is needed (example_tests, stdlib docs, `--bin`)
    #   - a crate has a bin or explicit [[test]]/[[bin]]/auto* targets, which
    #     `--lib` + `tests/*.rs` would silently miss
    #   - a package directory cannot be resolved
    local name_targets=0 targets="" pkg dir file
    if [ "$need_witchy" -eq 1 ] && [ "${#gate_pkgs[@]}" -gt 0 ] \
        && [ "$gate_need_examples" -eq 0 ] \
        && [ "${#gate_example_mods[@]}" -eq 0 ] \
        && [ "$gate_need_stdlib_docs" -eq 0 ] \
        && [ "${#gate_witchy_tests[@]}" -eq 0 ] \
        && [[ " $args " != *" --bin "* ]]; then
        name_targets=1
        for pkg in "${gate_pkgs[@]}"; do
            dir="crates/$pkg"
            if [ ! -f "$dir/Cargo.toml" ] || [ -e "$dir/src/main.rs" ] || [ -d "$dir/src/bin" ] \
                || grep -qE '^\[\[(test|bin|bench|example)\]\]|^auto(tests|bins)' "$dir/Cargo.toml"; then
                name_targets=0
                break
            fi
            for file in "$dir"/tests/*.rs; do
                [ -f "$file" ] || continue
                file="${file##*/}"
                targets="$targets --test ${file%.rs}"
            done
        done
    fi
    if [ "$name_targets" -eq 1 ]; then
        # Name only targets that exist in a SELECTED package. Cargo rejects the
        # whole invocation for an unknown one ("no test target named ..."),
        # which is how a branch touching typeck.rs and scripts/merge-queue.sh
        # would have gone red. The queue binaries live in merge-queue-tests,
        # which this stage never selects and already excludes (they run in the
        # isolated queue-infra shard), so they are skipped. Anything else that
        # is not a root `tests/<name>.rs` falls back to the whole-package form.
        for t in ${gate_tests[0]+"${gate_tests[@]}"}; do
            case "$t" in
                merge_queue | test_for_paths) continue ;;
            esac
            if [ -f "tests/$t.rs" ]; then
                targets="$targets --test $t"
            else
                name_targets=0
                break
            fi
        done
    fi
    if [ "$name_targets" -eq 1 ]; then
        if [ "$gate_need_conformance" -eq 1 ]; then
            # `--bin witchy` carries `example_tests::rfc0*` (see above).
            targets="$targets --bin witchy --test misc"
            for file in tests/rfc0*.rs; do
                [ -f "$file" ] || continue
                file="${file##*/}"
                targets="$targets --test ${file%.rs}"
            done
        fi
        args="$args --lib$targets"
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
all_paths_have_early=1
canonical_rewrite=0
canonical_rewrite_paths_only=1
canonical_wasmir_tail=0
canonical_build_recipe=0
canonical_crypto=0
canonical_fixture_rewrite=0
canonical_fixture_rewrite_paths_only=1
registry_replay=0
registry_replay_paths_only=1
registry_replay_contract_surface=0
for p in "${paths[@]}"; do
    path_has_early=0
    case "$p" in
        crates/witchy-canonical-source/src/* | \
        crates/witchy-lower/src/canonical* | \
        crates/witchy-types/src/canonical* | \
        crates/witchy-wir/src/canonical*)
            canonical_rewrite=1 ;;
        tests/misc/rfc0165_oracle.rs | \
        tests/misc/rfc0165_build_env_project.rs | \
        tests/misc/rfc0165_build_read_project.rs) ;;
        tests/misc/rfc0165_*.rs)
            canonical_rewrite=1 ;;
    esac
    case "$p" in
        crates/witchy-wir/src/canonical_wasmir.rs)
            canonical_wasmir_tail=1 ;;
    esac
    case "$p" in
        crates/witchy-lower/src/canonical_build_read.rs | \
        crates/witchy-types/src/pipeline/build_recipe.rs | \
        crates/witchy-wir/src/canonical_interpreter/build_read.rs | \
        crates/witchy-wir/src/canonical_safewir/build_read.rs)
            canonical_build_recipe=1
            canonical_rewrite=1 ;;
    esac
    case "$p" in
        crates/witchy-lower/src/canonical_crypto.rs | \
        crates/witchy-runtime/src/runtime/host/crypto*.rs | \
        crates/witchy-wir/src/canonical_interpreter/crypto*.rs | \
        crates/witchy-wir/src/canonical_safewir/crypto.rs | \
        tests/misc/rfc0165_crypto*.rs)
            canonical_crypto=1
            canonical_rewrite=1 ;;
    esac
    case "$p" in
        crates/witchy-interp/src/* | \
        crates/witchy-lower/src/* | \
        crates/witchy-runtime/src/* | \
        crates/witchy-runtime/tests/* | \
        crates/witchy-test-host/src/* | \
        crates/witchy-types/src/* | \
        crates/witchy-wir/src/* | \
        spec/wasm-abi.md | \
        src/* | \
        tests/misc.rs | \
        tests/misc/*.rs | \
        rfcs/* | \
        wiki/* | \
        bugs/* | \
        docs/* | \
        external-refs/* | \
        scratch/* | \
        security-eval/*) ;;
        *) canonical_rewrite_paths_only=0 ;;
    esac
    # witchy-types' std-pruning root census scans the Rust sources of these
    # crates for std name literals ("meta.TypeHole"). Any edit to them can add
    # one, and a crate-scoped selection never reaches witchy-types: d92c12c45
    # added a literal in witchy-interp, gated green, and left master red on it.
    case "$p" in
        crates/witchy-interp/*.rs | crates/witchy-lower/*.rs | \
        crates/witchy-runtime/*.rs | crates/witchy-syntax/*.rs | \
        crates/witchy-types/*.rs | crates/witchy-wir/*.rs | src/*.rs)
            gate_need_std_roots=1 ;;
    esac
    case "$p" in
        src/source/package_inputs/registry.rs | \
        src/source/package_inputs/registry_replay_tests.rs | \
        src/source/package_inputs/registry_verify.witchy)
            registry_replay=1 ;;
    esac
    case "$p" in
        rfcs/0165-acceptance-ledger.md | \
        src/source.rs | \
        src/source/package_inputs* | \
        src/trusted_publisher/source_inputs.rs | \
        tests/misc/rfc0165_packaged_source.rs) ;;
        *) registry_replay_paths_only=0 ;;
    esac
    case "$p" in
        tests/misc/rfc0165_packaged_source.rs)
            registry_replay_contract_surface=1 ;;
    esac
    case "$p" in
        crates/witchy-interp/src/interpreter/runners.rs | \
        crates/witchy-runtime/src/runtime/console_fixtures.rs | \
        crates/witchy-test-host/src/lib.rs | \
        crates/witchy-types/src/pipeline/test_driver/console_fixture.rs | \
        crates/witchy-types/src/pipeline/test_driver/console_fixture/arguments.rs | \
        src/commands/test_runner/canonical_console_fixture.rs)
            canonical_fixture_rewrite=1 ;;
    esac
    case "$p" in
        crates/witchy-interp/src/interpreter/runners.rs | \
        crates/witchy-runtime/src/runtime/console_fixtures.rs | \
        crates/witchy-test-host/src/lib.rs | \
        crates/witchy-types/src/pipeline/test_driver/console_fixture.rs | \
        crates/witchy-types/src/pipeline/test_driver/console_fixture/arguments.rs | \
        scripts/rfc0165-deferred-tests.txt | \
        src/commands/test_runner/canonical_console_fixture.rs) ;;
        *) canonical_fixture_rewrite_paths_only=0 ;;
    esac
    case "$p" in
        web/witchy-runtime/*.mjs)
            add "find web/witchy-runtime -type f -name '*.mjs' -exec node --check {} \\;" ;;
    esac
    case "$p" in
        src/source.rs | \
        src/source/package_inputs* | \
        src/trusted_publisher/source_inputs.rs | \
        tests/misc/rfc0165_packaged_source.rs)
            add_early "cargo nextest run --test misc -E 'test(=rfc0165_packaged_source::packaged_source_rejects_custom_owner_and_unsafe_input_paths)'" ;;
    esac
    case "$p" in
        src/commands/compile.rs | \
        src/commands/project_resolver_tests.rs | \
        src/source.rs | src/source/package_inputs.rs)
            add_early "cargo nextest run --test cli_subcommands -E 'test(=compile_accepts_complete_authenticated_package_owners)'" ;;
        src/source/build_recipe_tests.rs)
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^source::build_recipe_tests::/)'"
            path_has_early=1 ;;
        src/source/build_net_recipe_tests.rs)
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^source::build_recipe_tests::build_net_recipe_tests::/)'"
            path_has_early=1 ;;
        src/source/build_env_recipe_tests.rs)
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^source::build_recipe_tests::build_env_recipe_tests::/)'"
            path_has_early=1 ;;
        src/source/build_exec_recipe_tests.rs)
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^source::build_recipe_tests::build_exec_recipe_tests::/)'"
            path_has_early=1 ;;
        crates/witchy-runtime/src/runtime/host/build_read.rs | \
        crates/witchy-runtime/src/runtime/host/build_read_tests.rs)
            add_early "cargo nextest run -p witchy-runtime --features native --lib -E 'test(/^runtime::host::build_read_tests::/) or test(=runtime::host::closure_owner::tests::build_read_captures_authenticate_kind_session_projection_and_cleanup)'"
            add_early "cargo nextest run -p witchy --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature)'"
            path_has_early=1 ;;
        crates/witchy-runtime/src/runtime/host/build_env.rs | \
        crates/witchy-runtime/src/runtime/host/build_env_tests.rs)
            add_early "cargo nextest run -p witchy-runtime --features native --lib -E 'test(/^runtime::host::build_env_tests::/) or test(=runtime::host::closure_owner::tests::build_env_captures_reauthenticate_root_and_optional_response_and_cleanup)'"
            add_early "cargo nextest run -p witchy --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature)'"
            path_has_early=1 ;;
        tests/misc/rfc0103_build_step_confinement.rs)
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0103_build_step_confinement::/)'"
            path_has_early=1 ;;
        tests/misc/rfc0165_build_read_project.rs)
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0165_build_read_project::/)'"
            path_has_early=1 ;;
        tests/misc/rfc0165_build_env_project.rs)
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0165_build_env_project::/)'"
            path_has_early=1 ;;
        crates/witchy-types/src/pipeline/test_driver/integration.rs | \
        src/commands/test_runner/canonical_integration.rs)
            add_early "cargo nextest run -p witchy --features witchy-wir/native --bin witchy --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature) or test(=rfc0077_integration_grants::source_integration_recipe_runs_selected_console_capture_and_reports_abort) or test(/^commands::test_runner::canonical_integration_tests::/)'"
            path_has_early=1 ;;
        crates/witchy-types/src/pipeline/test_driver.rs | \
        crates/witchy-types/src/pipeline/test_driver/*.rs | \
        src/commands/test_runner/canonical_bare.rs | \
        src/commands/test_runner/canonical_console_fixture.rs)
            add_early "cargo nextest run --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature)'"
            path_has_early=1 ;;
        tests/misc/rfc0165_oracle.rs)
            add_early "WITCHY_ORACLE_EXAMPLE=calc cargo nextest run -p witchy --features witchy-wir/native --test misc --ignore-default-filter -E 'test(=rfc0165_oracle::oracle_agrees_on_the_admitted_examples_shard_3)'"
            path_has_early=1 ;;
        tests/misc/rfc0165_owner_adapters.rs)
            # This admission-boundary assertion previously failed only after
            # the full workspace stage. Keep the exact raw-Wasm rejection
            # probe in the edit loop; the complete adapter family remains in
            # the landing selection below.
            add_early "cargo nextest run -p witchy --features witchy-wir/native --test misc --ignore-default-filter -E 'test(=rfc0165_owner_adapters::server_workers_reject_raw_wasm_even_with_genuine_entry_metadata)'"
            path_has_early=1 ;;
        tests/misc/rfc0077_integration_grants.rs)
            # The async source-integration cases each take about 166s and run
            # in the complete landing selection. Keep the edit loop on the
            # remaining grant, isolation, and synchronous recipe assertions.
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0077_integration_grants::/) and not test(/^rfc0077_integration_grants::source_integration_async_recipe_/)'"
            path_has_early=1 ;;
        tests/misc/semantic_conformance.rs)
            add_early "cargo nextest run --test misc -E 'test(=semantic_conformance::independent_expectation_rejects_a_shared_semantic_mutation)'"
            path_has_early=1 ;;
        src/commands/embedded_pm.rs)
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^commands::embedded_pm::tests::/)'"
            path_has_early=1 ;;
        tests/cli_subcommands.rs)
            # Keep the edit loop on the recent gate-red output contract plus
            # one sandbox authority and one fetch-root admission guard. The
            # complete cli_subcommands target remains in landing.
            add_early "cargo nextest run --test cli_subcommands -E 'test(=source_nil_main_int_output_is_not_eaten_as_exit_code) or test(=sandbox_wasm_requires_explicit_dir_grant) or test(=fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact)'"
            path_has_early=1 ;;
        crates/witchy-wir/tests/owned_slot_child_storage.rs)
            add_early "cargo nextest run -p witchy-wir --features native --test owned_slot_child_storage -E 'test(=owned_slots_require_managed_child_storage_before_consuming_move)'"
            path_has_early=1 ;;
        crates/witchy-wir/src/wir_prelude.rs | \
        crates/witchy-wir/examples/abi_catalog.rs | \
        tests/misc/wasm_abi_catalog.rs | \
        spec/wasm-abi.md)
            add_early "cargo nextest run --test misc -E 'test(wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature) or test(wasm_abi_catalog::public_spec_contains_the_generated_complete_catalog) or test(wasm_abi_catalog::capability_imports_name_their_authority_family) or test(wasm_abi_catalog::precompiled_runner_authority_rows_are_cataloged)'"
            path_has_early=1 ;;
    esac
    case "$p" in
        crates/witchy-wir/src/canonical_safewir/check/memory.rs)
            add_early "cargo nextest run -p witchy-wir --features native --lib -E 'test(constant_prefix_publication_rejects_gaps_and_missing_children)'"
            path_has_early=1 ;;
        crates/witchy-lower/src/canonical_list_update.rs | \
        crates/witchy-lower/src/canonical_pipeline.rs)
            add_early "cargo nextest run -p witchy-lower --features witchy-wir/native --lib -E 'test(canonical_pipeline::tests::list_)'"
            add_early "cargo nextest run -p witchy --features witchy-wir/native --test misc --ignore-default-filter -E 'test(rfc0165_admitted_launch::admitted_aggregates_render_structurally_as_the_interpreter_prints_them)'"
            path_has_early=1 ;;
        crates/witchy-lower/tests/codegen.rs)
            add_early "cargo nextest run -p witchy-lower --test codegen -E 'test(=tests::grantful_build_primitives_compile_to_build_imports_only)'"
            path_has_early=1 ;;
        tests/misc/rfc0165_admitted_launch.rs)
            add_early "cargo nextest run -p witchy --features witchy-wir/native --test misc --ignore-default-filter -E 'test(rfc0165_admitted_launch::admitted_aggregates_render_structurally_as_the_interpreter_prints_them)'"
            path_has_early=1 ;;
    esac
    case "$p" in
        crates/witchy-canonical-source/*)
            any_rust=1
            gate_need_conformance=1
            # The checked-source contract (RFC-0170) sits under the checker,
            # SafeWIR and lowering; each of them observes its judgments.
            add_pkg witchy-canonical-source
            add_pkg witchy-types
            add_pkg witchy-wir
            add_pkg witchy-lower
            add "cargo nextest run -p witchy-canonical-source -p witchy-types"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-types/*)
            any_rust=1
            gate_need_conformance=1
            add_pkg witchy-types
            # Downstream compiler crates consume this one, and their own tests
            # (e.g. witchy-lower's canonical_pipeline admission tests) observe a
            # change here that this crate's tests cannot. bd239ce55 changed the
            # checker in witchy-wir, gated green without witchy-lower's tests,
            # and left master red on one of them.
            add_pkg witchy-wir
            add_pkg witchy-lower
            add "cargo nextest run -p witchy-types"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-lower/*)
            any_rust=1
            gate_need_conformance=1
            add_pkg witchy-lower
            add "./scripts/nextest-admitted.sh witchy-lower -- cargo nextest run -p witchy-lower --features witchy-wir/native"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --features witchy-wir/native -E 'test(/^example_tests::/)'"
            ;;
        crates/witchy-wir/*)
            any_rust=1
            gate_need_conformance=1
            add_pkg witchy-wir
            # Downstream compiler crates consume this one, and their own tests
            # (e.g. witchy-lower's canonical_pipeline admission tests) observe a
            # change here that this crate's tests cannot. bd239ce55 changed the
            # checker in witchy-wir, gated green without witchy-lower's tests,
            # and left master red on one of them.
            add_pkg witchy-lower
            add_pkg witchy-runtime
            add "cargo nextest run -p witchy-wir --features witchy-wir/native"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --features witchy-wir/native -E 'test(/^example_tests::/)'"
            ;;
        crates/witchy-runtime/*)
            any_rust=1
            gate_need_conformance=1
            add_pkg witchy-runtime
            add "cargo nextest run -p witchy-runtime"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'"
            ;;
        crates/witchy-syntax/*)
            any_rust=1
            gate_need_conformance=1
            add_pkg witchy-syntax
            # Downstream compiler crates consume this one, and their own tests
            # (e.g. witchy-lower's canonical_pipeline admission tests) observe a
            # change here that this crate's tests cannot. bd239ce55 changed the
            # checker in witchy-wir, gated green without witchy-lower's tests,
            # and left master red on one of them.
            add_pkg witchy-types
            add_pkg witchy-wir
            add_pkg witchy-lower
            add "cargo nextest run -p witchy-syntax"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-interp/* | crates/witchy-caps/*)
            any_rust=1
            gate_need_conformance=1
            add_pkg witchy-interp
            add_pkg witchy-caps
            add "cargo nextest run -p witchy-interp -p witchy-caps"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'" ;;
        src/main.rs | src/cli.rs | src/source.rs)
            gate_workspace=1
            add "cargo check -p witchy --all-targets"
            add "cargo clippy -p witchy --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^(checked_cli_pipeline_tests|cli::|runtime_parity_tests|source::tests|test_mode_link_tests)::/)'"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test cli_subcommands"
            add "cargo nextest run -p witchy-syntax" ;;
        src/example_tests.rs)
            any_rust=1
            gate_need_examples=1
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^example_tests::/)'" ;;
        src/example_tests/network.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^example_tests::network::/)'"
            path_has_early=1 ;;
        src/example_tests/*)
            any_rust=1
            _et="${p##*/}"
            add_example_mod "${_et%.rs}"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'" ;;
        src/commands/web.rs | src/commands/web/*)
            any_rust=1
            add_witchy_test 'commands::web'
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^commands::web/)'" ;;
        src/commands/dispatch.rs)
            any_rust=1
            add_early "cargo nextest run --test cli_subcommands -E 'test(=fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact)'"
            add_witchy_test 'commands::'
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^commands::/)'" ;;
        src/commands/*)
            any_rust=1
            add_witchy_test 'commands::'
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^commands::/)'" ;;
        src/lsp.rs | src/lsp_tests.rs)
            any_rust=1
            add_witchy_test 'lsp'
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^lsp/)'" ;;
        src/diagnostic_golden_tests.rs | src/snapshots/*)
            any_rust=1
            add_witchy_test 'diagnostic_golden_tests'
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^diagnostic_golden_tests/)'" ;;
        src/lib.rs | src/artifact.rs | src/stats.rs | src/idp.rs | \
        src/trusted_publisher.rs | src/capabilities_tests.rs)
            any_rust=1
            add_pkg witchy
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -p witchy --lib" ;;
        src/bin/*)
            any_rust=1
            add_gate_test misc
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc" ;;
        crates/* | src/*)
            any_rust=1
            gate_workspace=1 ;;
        std/*.witchy)
            gate_need_examples=1
            gate_need_stdlib_docs=1
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(stdlib_docs_are_current)'"
            add "\"\${CARGO_TARGET_DIR:-target}/debug/witchy\" fmt --check std/*.witchy" ;;
        README.md)
            corpus_impact=1
            # The legacy documentation sweep still executes the frozen source
            # interpreter and is deferred until the playground/docs runner uses
            # admitted Wasm. Keep the gate impact without selecting it here.
            ;;
        examples/* | book/*)
            corpus_impact=1
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'"
            ;;
        projects/grimoire/* | projects/coven/* | projects/coven-web/* | projects/glamour/* | projects/docs/*)
            add "find projects -type f -path '*/src/*.witchy' -exec \"\${CARGO_TARGET_DIR:-target}/debug/witchy\" fmt --check {} +"
            broad_e2e=1
            add "./scripts/check.sh --e2e" ;;
        web/witchy-runtime/glamour-*.mjs | \
        web/witchy-runtime/heap-reset.test.mjs | \
        web/witchy-runtime/highlighter.test.mjs | \
        web/witchy-runtime/user-cap-export.test.mjs)
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test glamour -E 'test(/^dom::/)'" ;;
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
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test browser -E 'test(/^shim::/)'" ;;
        web/witchy-runtime/encoding-abi.test.mjs)
            add_gate_test browser
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test browser -E 'test(/^encoding::/)'" ;;
        web/witchy-runtime/import-catalog.test.mjs)
            add_gate_test misc
            add "cargo nextest run --test misc -E 'test(/^wasm_abi_catalog::/)'" ;;
        web/witchy-runtime/witchy-runtime.mjs)
            add_gate_test browser
            add_gate_test misc
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test browser --test glamour --test misc -E 'binary(browser) or (binary(glamour) and test(/^dom::/)) or (binary(misc) and test(/^wasm_abi_catalog::/))'" ;;
        web/*)
            # Playground / highlighter JS is not a Rust surface. Keep a
            # focused browser binary rather than fail-safe --workspace.
            add_gate_test browser
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test browser" ;;
        tools/merge-queue-tests/Cargo.toml | \
        tools/merge-queue-tests/tests/merge_queue.rs | \
        tools/merge-queue-tests/tests/test_for_paths.rs | \
        tools/merge-queue-tests/tests/merge_queue/*.rs | \
        tools/merge-queue-tests/tests/merge_queue/**/*.rs)
            add_gate_test merge_queue
            add "./scripts/check.sh --queue-infra" ;;
        tools/merge-queue-tests/tests/worktree.rs | \
        tools/merge-queue-tests/tests/worktree/*.rs)
            add_gate_test worktree
            add "cargo nextest run -p merge-queue-tests --test worktree" ;;
        tools/repo-tests/Cargo.toml | \
        tools/repo-tests/tests/repo_hygiene.rs | \
        tools/repo-tests/tests/repo_hygiene/*.rs)
            add_pkg repo-tests
            add "cargo nextest run -p repo-tests --test repo_hygiene" ;;
        tests/test_for_paths.rs)
            # Keep deleted-path routing narrow during the one-time move into
            # the standalone tooling package.
            add_gate_test test_for_paths
            add "cargo nextest run -p merge-queue-tests --test test_for_paths" ;;
        tests/worktree.rs | tests/worktree/*.rs)
            add_gate_test worktree
            add "cargo nextest run -p merge-queue-tests --test worktree" ;;
        tests/glamour/dom.rs)
            any_rust=1
            add "cargo nextest run --test glamour -E 'test(=dom::every_node_test_driver_is_wired_to_a_rust_test)'" ;;
        tests/misc/url_components.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^url_components::/)'"
            path_has_early=1 ;;
        tests/misc/canonical_net_fixture.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^canonical_net_fixture::/)'"
            path_has_early=1 ;;
        tests/misc/http_url_fallible.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^http_url_fallible::/)'"
            path_has_early=1 ;;
        tests/misc/rfc0087_uniform_var.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0087_uniform_var::/)'"
            path_has_early=1 ;;
        tests/misc/rfc0087_migration_census.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0087_migration_census::/)'"
            path_has_early=1 ;;
        tests/misc/architecture.rs | \
        tests/misc/example_source_paths.rs | \
        tests/misc/release_packaging.rs | \
        tests/misc/rfc_status_hygiene.rs | \
        tests/misc/spec_freshness.rs)
            # Keep deleted-path routing narrow during the one-time move into
            # the compiler-independent repository-contract package.
            add_pkg repo-tests
            add "cargo nextest run -p repo-tests --test repo_hygiene" ;;
        tests/misc/rfc0152_async_region_formation.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0152_async_region_formation::/)'"
            path_has_early=1 ;;
        tests/misc/rfc0152_semantic_matrix.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0152_semantic_matrix::/)'"
            path_has_early=1 ;;
        tests/misc/rfc0167_mode_interop.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(=rfc0167_mode_interop::reference_free_normal_opt_chain_preserves_modes_and_aliased_snapshot_without_opts)'"
            path_has_early=1 ;;
        tests/support/*.rs)
            # The shared test-helper module. `mod support;` is declared by
            # EXACTLY two integration binaries -- e2e and rfc0092 --
            # so a change here cannot reach any other target, and falling
            # through to the whole workspace charged it for all of them: one
            # such presubmit on 2026-09-21 ran the workspace-minus-e2e stage
            # for 2,604s. Verify the consumer list before narrowing further:
            #   grep -rn "mod support" --include='*.rs' tests/ | grep -v "^tests/support/"
            broad_e2e=1
            add "./scripts/check.sh --e2e"
            add_gate_test rfc0092
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test rfc0092" ;;
        tests/e2e.rs)
            gate_workspace=1
            broad_e2e=1
            add "./scripts/check.sh --e2e" ;;
        tests/e2e/*.rs)
            any_rust=1
            add_gate_test e2e
            test_name="${p##*/}"
            test_name="${test_name%.rs}"
            case "$test_name" in
                '' | *[!A-Za-z0-9_]*)
                    broad_e2e=1
                    add "./scripts/check.sh --e2e" ;;
                *)
                    add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --features e2e-tests --test e2e -E 'test(/^${test_name}::/)'" ;;
            esac
            ;;
        tests/misc.rs | tests/misc/*)
            any_rust=1
            add_gate_test misc
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc" ;;
        tests/*.rs)
            any_rust=1
            test_name="${p#tests/}"
            test_name="${test_name%.rs}"
            # A DELETED integration test has no binary left to run, and
            # selecting one by name fails the whole plan. What a deletion can
            # actually invalidate is the root-integration budget census, so
            # run that instead.
            if [ ! -f "$p" ]; then
                add_pkg repo-tests
                add_early "cargo nextest run -p repo-tests --test repo_hygiene -E 'test(architecture::root_integration_test_binaries_stay_within_budget)'"
                path_has_early=1
                continue
            fi
            # Shell `case` globs allow `*` to cross `/`. Normalize nested
            # modules only when their sibling root integration target exists;
            # otherwise retain the fail-safe broad selection below.
            case "$test_name" in
                */*)
                    root_test_name="${test_name%%/*}"
                    if [ -f "tests/$root_test_name.rs" ]; then
                        test_name="$root_test_name"
                    else
                        gate_workspace=1
                        add "./scripts/check.sh --fast"
                        continue
                    fi
                    ;;
            esac
            if ! git cat-file -e "master:$p" 2>/dev/null; then
                add_pkg repo-tests
                add_early "cargo nextest run -p repo-tests --test repo_hygiene -E 'test(architecture::root_integration_test_binaries_stay_within_budget)'"
                path_has_early=1
            fi
            add_gate_test "$test_name"
            printf -v test_name_q '%q' "$test_name"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test $test_name_q" ;;
        Cargo.toml | Cargo.lock | build.rs | .cargo/* | rust-toolchain | rust-toolchain.toml)
            gate_workspace=1
            add_early "python3 scripts/check-stage-dependencies.py"
            path_has_early=1 ;;
        .github/workflows/* | .github/actions/* | .github/zizmor.yml)
            add "./scripts/zizmor.sh --quiet --no-progress --persona=pedantic .github/workflows"
            add "./scripts/workflow-isolation-check.py .github/workflows" ;;
        scripts/zizmor.sh | scripts/workflow-isolation-check.py)
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "./scripts/zizmor.sh --quiet --no-progress --persona=pedantic .github/workflows"
            add "./scripts/workflow-isolation-check.py .github/workflows" ;;
        scripts/nextest-list-wrapper.sh | scripts/test-nextest-list-wrapper.sh)
            gate_workspace=1
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "./scripts/test-nextest-list-wrapper.sh"
            add "./scripts/check.sh --queue-infra" ;;
        .config/nextest.toml)
            gate_workspace=1
            add_early "cargo nextest run -p merge-queue-tests --test test_for_paths --ignore-default-filter -E 'test(=nextest_config_change_runs_one_parse_smoke)'"
            path_has_early=1 ;;
        scripts/gate-report.sh)
            add_early "bash -n scripts/gate-report.sh"
            add_early "cargo nextest run -p merge-queue-tests --test merge_queue --ignore-default-filter -E 'test(=gate_behavior::state_and_gate::gate_report_is_read_only_and_aggregates_batches_failures_and_phases)'"
            path_has_early=1 ;;
        scripts/check.sh | \
        scripts/merge-queue.sh | \
        scripts/state-paths.sh)
            add_gate_test merge_queue
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/worktree-status.sh)
            add_gate_test worktree
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "cargo nextest run -p merge-queue-tests --test worktree"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/rfc-status.sh)
            add_gate_test worktree
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "cargo nextest run -p merge-queue-tests --test worktree" ;;
        scripts/worktree-warm.sh)
            add_gate_test worktree
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "cargo nextest run -p merge-queue-tests --test worktree"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/worktree-create.sh)
            add_gate_test worktree
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "cargo nextest run -p merge-queue-tests --test worktree" ;;
        scripts/check-spec-freshness.sh)
            add_pkg repo-tests
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "./scripts/check-spec-freshness.sh"
            add "cargo nextest run -p repo-tests --test repo_hygiene -E 'test(spec_freshness::)'" ;;
        scripts/release-package.sh | scripts/release-checksums.sh | scripts/pinned-binaryen.py)
            add_pkg repo-tests
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "cargo nextest run -p repo-tests --test repo_hygiene -E 'test(release_packaging::)'" ;;
        scripts/gate-test-cost.py | scripts/test-gate-test-cost.py)
            add "python3 scripts/test-gate-test-cost.py" ;;
        scripts/check-stage-dependencies.py | spec/stage-dependencies.json)
            add_pkg repo-tests
            add_early "python3 scripts/check-stage-dependencies.py"
            add_early "cargo nextest run -p repo-tests --test repo_hygiene -E 'test(architecture::stage_crates_follow_the_declared_dependency_dag)'"
            path_has_early=1 ;;
        scripts/agent-check.sh | scripts/test-agent-check-qos.sh)
            add_gate_test merge_queue
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "./scripts/test-agent-check-qos.sh" ;;
        scripts/submit-with-presubmit.sh | scripts/test-submit-with-presubmit.sh)
            add_gate_test merge_queue
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "./scripts/test-submit-with-presubmit.sh" ;;
        scripts/canonical-native-edit.sh | scripts/test-canonical-native-edit.sh)
            add_gate_test test_for_paths
            add "bash -n scripts/canonical-native-edit.sh scripts/test-canonical-native-edit.sh"
            add "./scripts/test-canonical-native-edit.sh" ;;
        scripts/test-for-paths.sh | scripts/test-path-receipts.sh | scripts/test-impact.py | \
        scripts/nextest-admitted.sh | scripts/test-nextest-admitted.sh)
            add_gate_test test_for_paths
            add_gate_test merge_queue
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "./scripts/test-path-receipts.sh"
            add "./scripts/test-nextest-admitted.sh"
            add "cargo nextest run -p merge-queue-tests --test test_for_paths"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/bin/*)
            # Extensionless shims (`cargo`, `witchy`). They have no suffix, so
            # the scripts/*.sh rule below never matched them and nothing
            # syntax-checked them at all.
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done" ;;
        scripts/*.sh)
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done" ;;
        justfile)
            add "just --list" ;;
        spec/wasm-abi.md)
            # The ABI catalog checks selected above validate this generated
            # contract document and its native implementations. The generic
            # documentation sweep is deferred, so adding it here selects zero
            # runnable tests and prevents an otherwise green ABI presubmit.
            add_gate_test misc ;;
        spec/stdlib.md)
            echo "WARNING: spec/stdlib.md is GENERATED — edit std/*.witchy doc-comments instead" >&2
            gate_need_stdlib_docs=1
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(stdlib_docs_are_current)'" ;;
        spec/* | CONTRIBUTING.md)
            # documentation_examples_are_valid walks README, CONTRIBUTING,
            # spec/, and book/src. A spec-only edit cannot change crate
            # tests; running the workspace would only re-prove master.
            add_witchy_test 'example_tests::example_sweeps::documentation_examples_are_valid'
            # Do not turn the explicitly deferred source-interpreter parity
            # sweep into a zero-test admitted command.
            ;;
        rfcs/*)
            add_pkg repo-tests
            add "cargo nextest run -p repo-tests --test repo_hygiene -E 'test(rfc_status_hygiene::)'" ;;
        *.md | bugs/* | wiki/*)
            : ;; # prose only — but book/README witchy blocks are covered above
    esac
    [ "$path_has_early" -eq 1 ] || all_paths_have_early=0
done

# A direct edit to the registry replay test module has no packaged-source
# contract path to trigger the specialized route. Enable that route only for
# the standalone file; mixed packaged-source edits retain their broader source
# ownership probes.
if [ "${#paths[@]}" -eq 1 ] \
    && [ "${paths[0]}" = "src/source/package_inputs/registry_replay_tests.rs" ]; then
    registry_replay_contract_surface=1
fi

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

# Nested `tests/e2e/*.rs` files are modules in the single Cargo target named
# `e2e`; a path such as `--test e2e/offline_pm` names no target. When another
# changed path already selected the complete e2e shard, drop the narrower
# module invocation it subsumes so a successful broad run is not followed by
# duplicate discovery and execution.
if [ "$broad_e2e" -eq 1 ]; then
    remaining=()
    for c in ${cmds[0]+"${cmds[@]}"}; do
        case "$c" in
            # Match on the target and filter, NOT on the whole prefix: the
            # command also carries `--features e2e-tests` (the e2e binary is
            # behind a non-default feature), and pinning the old exact prefix
            # here silently stopped subsuming it.
            *"--test e2e -E 'test(/^"*) ;;
            *) remaining+=("$c") ;;
        esac
    done
    cmds=("${remaining[@]}")
fi

# Queue infrastructure keeps its complete 69-test process/locking corpus in the
# landing shard. The edit loop runs a representative cross-section that covers
# admission, singleton coordination, dependency cycles, reload, sweep ordering,
# receipts, reporting, state paths, and check.sh command construction.
if [ "$edit_loop" -eq 1 ]; then
    for i in "${!cmds[@]}"; do
        if [ "${cmds[$i]}" = "./scripts/check.sh --queue-infra" ]; then
            cmds[$i]="./scripts/check.sh --queue-infra-edit"
        fi
    done
fi

# Any Rust change means the fast gate already runs every workspace nextest test.
# Drop narrower nextest selections it subsumes, but retain independent shards
# such as WASM, e2e, and Witchy source formatting.
if [ "$edit_loop" -eq 1 ] && [ "${#paths[@]}" -eq 1 ]; then
    case "${paths[0]}" in
        tests/misc/rfc0165_fetch.rs)
            # A direct edit to the Fetch fixture can run its complete three-test
            # module. Mixed compiler/RFC edits keep the broader checkpoint below.
            cmds=("./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0165_fetch::/)'" ) ;;
        tests/misc/rfc0165_messages.rs)
            cmds=("./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0165_messages::/)'" ) ;;
        tests/misc/rfc0165_corpus.rs)
            cmds=("./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0165_corpus::/)'" ) ;;
        tests/misc/rfc0165_project_run.rs)
            cmds=("./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/^rfc0165_project_run::/)'" ) ;;
        tests/misc/rfc0165_owner_adapters.rs)
            cmds=("cargo nextest run -p witchy --features witchy-wir/native --test misc --ignore-default-filter -E 'test(=rfc0165_owner_adapters::server_workers_reject_raw_wasm_even_with_genuine_entry_metadata)'") ;;
        tests/misc/rfc0167_mode_interop.rs)
            cmds=("./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(=rfc0167_mode_interop::reference_free_normal_opt_chain_preserves_modes_and_aliased_snapshot_without_opts)'") ;;
        *) ;;
    esac
fi
if [ "$edit_loop" -eq 1 ] && [ "${#cmds[@]}" -gt 0 ] \
    && [ "${#paths[@]}" -eq 1 ] \
    && { [ "${paths[0]}" = "tests/misc/rfc0165_fetch.rs" ] \
        || [ "${paths[0]}" = "tests/misc/rfc0165_messages.rs" ] \
        || [ "${paths[0]}" = "tests/misc/rfc0165_corpus.rs" ] \
        || [ "${paths[0]}" = "tests/misc/rfc0165_project_run.rs" ] \
        || [ "${paths[0]}" = "tests/misc/rfc0165_owner_adapters.rs" ]; }; then
    : # Keep the exact direct-file module route selected above.
elif [ "$edit_loop" -eq 1 ] && [ "$registry_replay" -eq 1 ] \
    && [ "$registry_replay_paths_only" -eq 1 ] \
    && [ "$registry_replay_contract_surface" -eq 1 ]; then
    # Closed registry replay spans source capture, signature verification,
    # restored ownership, and execution. Keep the standalone edit loop on
    # representative replay, substitution, ownership, admission, path, and
    # locked-replay contracts. The two additional authority variants remain in
    # the complete serialized landing selection.
    cmds=(
        "cargo nextest run --test misc -E 'test(=rfc0165_packaged_source::packaged_source_rejects_custom_owner_and_unsafe_input_paths)'"
        "cargo nextest run --test cli_subcommands -E 'test(=compile_accepts_complete_authenticated_package_owners)'"
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --features coven-tests --bin witchy -E 'test(=source::package_inputs::registry_replay_tests::pinned_vendor_package_replays_and_executes_after_original_sources_are_removed) or test(=source::package_inputs::registry_replay_tests::pinned_vendor_replay_rejects_substituted_record_source_pin_and_coordinate) or test(=source::package_inputs::registry_replay_tests::pinned_vendor_project_owner_flags_are_assertions_not_registry_authority) or test(=source::package_inputs::registry_replay_tests::pinned_vendor_valid_source_b_with_wasm_a_rejects_rehashed_envelope_before_execution) or test(=source::package_inputs::registry_replay_tests::pinned_vendor_explicit_path_dependency_wins_over_registry_lock_and_directory_name) or test(=source::package_inputs::registry_replay_tests::pinned_vendor_offline_replay_uses_locked_version_without_manifest_reresolution)'"
    )
elif [ "$edit_loop" -eq 1 ] && [ "$canonical_fixture_rewrite" -eq 1 ] \
    && [ "$canonical_fixture_rewrite_paths_only" -eq 1 ]; then
    # Console-fixture plumbing spans the checked driver, interpreter, host,
    # native runtime, and CLI. Combining those paths used to select seven
    # commands, including every example, every command test, and the Wasm
    # shard. Keep the edit loop on the three contracts the slice changes; the
    # complete crate, example, command, and Wasm coverage remains in landing.
    cmds=(
        "cargo nextest run --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature)'"
        "cargo nextest run -p witchy-test-host -p witchy-runtime -E 'test(/^(tests::|runtime::console_fixtures::tests::)/)'"
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^commands::test_runner::canonical_console_fixture_tests::/)'"
    )
elif [ "$edit_loop" -eq 1 ] && [ "$canonical_rewrite" -eq 1 ] \
    && [ "$canonical_rewrite_paths_only" -eq 1 ]; then
    # A cross-stage canonical compiler edit previously expanded to crate suites,
    # the complete example matrix twice, all misc tests, Wasm, check, and
    # clippy. That is a checkpoint workload and discouraged exact-tip receipts.
    # Documentation accompanying those compiler files adds no executable test
    # surface, so it must not turn the same edit back into a checkpoint. Keep
    # the edit loop to three scoped runs from one native union build, retaining
    # load-bearing ABI, authority, ownership, bounds, admission, and independent
    # semantic assertions. Standalone runtime feature profiles remain separate.
    # The serialized landing selection is unchanged.
    canonical_build_recipe_wir_expr=""
    canonical_build_recipe_lower_target=""
    canonical_build_recipe_lower_expr=""
    canonical_build_recipe_app_expr=""
    canonical_crypto_wir_expr=""
    canonical_crypto_app_expr=""
    if [ "$canonical_build_recipe" -eq 1 ]; then
        canonical_build_recipe_wir_expr=" or test(canonical_safewir::build_read::tests::build_env_catalog_authenticates_root_and_keeps_presence_internal) or test(canonical_interpreter::build_read::tests::build_env_oracle_keeps_denied_unset_empty_and_private_sessions_distinct)"
        canonical_build_recipe_lower_target=" --test codegen"
        canonical_build_recipe_lower_expr=" or test(=tests::grantful_build_primitives_compile_to_build_imports_only)"
        canonical_build_recipe_app_expr=" or test(/^source::build_recipe_tests::/) or test(/^source::build_env_recipe_tests::/) or test(/^rfc0103_build_step_confinement::/) or test(/^rfc0165_build_read_project::/) or test(/^rfc0165_build_env_project::/)"
    fi
    canonical_wasmir_tail_expr=""
    if [ "$canonical_wasmir_tail" -eq 1 ]; then
        # The two mutation stress tests are 25.8s and 25.6s last finishers in
        # the current scoped gate. Keep their admission assertions in the
        # package-wide landing selection, but run the cheaper bounds/nullable
        # semantic sentinel in the edit loop alongside the other admission
        # probes. This changes frequency, never coverage at landing.
        canonical_wasmir_tail_expr=" or test(=canonical_wasmir::tests::packed_scalar_collection_oob_guard_traps_in_wasmtime) or test(=canonical_wasmir::tests::share_at_max_refcount_traps_before_destination_or_refcount_write)"
    fi
    if [ "$canonical_crypto" -eq 1 ]; then
        canonical_crypto_wir_expr=" or test(canonical_interpreter::crypto_rsa_tests::)"
        canonical_crypto_app_expr=" or test(/^rfc0165_crypto(_rsa)?::/)"
    fi
    canonical_native_wir_expr="test(canonical_safewir::build_read::tests::build_read_catalog_separates_root_authority_from_bounded_responses) or test(canonical_safewir::check::tests::scalar_checker_refuses_non_total_backend_combinations) or test(boolean_reaches::) or test(constant_prefix_publication_rejects_gaps_and_missing_children) or test(scalar_move_range_is_bounds_checked_and_admitted) or test(scalar_clone_range_is_ownership_safe_and_admitted) or test(dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot) or test(canonical_capabilities_lower_only_to_externref) or test(canonical_interpreter::capability_tests::a_token_of_another_kind_cannot_serve_a_dir_operation) or test(=validated_safewir_has_one_checker_owned_construction_surface) or test(canonical_wasm_encoding_is_private_and_requires_final_authority)${canonical_wasmir_tail_expr}${canonical_build_recipe_wir_expr}${canonical_crypto_wir_expr}"
    canonical_native_lower_expr="test(canonical_pipeline::tests::list_string_)${canonical_build_recipe_lower_expr}"
    canonical_native_app_expr="test(wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature) or test(wasm_abi_catalog::public_spec_contains_the_generated_complete_catalog) or test(wasm_abi_catalog::capability_imports_name_their_authority_family) or test(wasm_abi_catalog::precompiled_runner_authority_rows_are_cataloged) or test(rfc0077_integration_grants::source_integration_recipe_runs_selected_console_capture_and_reports_abort) or test(rfc0087_uniform_var::list_pop_projected_coordinates_and_nested_payloads_on_three_legs) or test(rfc0165_admitted_launch::admitted_entry_without_a_console_grant_is_refused_before_it_runs) or test(rfc0165_admitted_launch::admitted_guard_near_misses_trap_exactly_at_the_boundary) or test(rfc0165_admitted_launch::admitted_aggregates_render_structurally_as_the_interpreter_prints_them) or test(rfc0165_owner_adapters::server_pool_oracle::/) or test(=rfc0165_owner_adapters::server_workers_reject_raw_wasm_even_with_genuine_entry_metadata) or test(=semantic_conformance::independent_expectation_rejects_a_shared_semantic_mutation) or test(/^commands::embedded_pm::tests::/) or test(/^commands::test_runner::canonical_integration_tests::/) or test(/^rfc0165_enum_equality::/) or test(=rfc0165_oracle::admitted_observer_is_not_called_on_refusal) or test(=rfc0165_oracle::oracle_agrees_on_the_admitted_examples_shard_3) or test(/^example_tests::rfc0/)${canonical_build_recipe_app_expr}${canonical_crypto_app_expr}"
    canonical_native_batch=("$canonical_native_wir_expr" "$canonical_native_lower_expr" "$canonical_native_app_expr" "$canonical_build_recipe")
    cmds=(
        "cargo nextest run -p witchy-wir --features native --lib --test rfc0165_route_gate -E '${canonical_native_wir_expr}'"
        "cargo nextest run -p witchy-lower --features witchy-wir/native --lib${canonical_build_recipe_lower_target} -E '${canonical_native_lower_expr}'"
        "WITCHY_ORACLE_EXAMPLE=calc cargo nextest run -p witchy --features witchy-wir/native --bin witchy --test misc --ignore-default-filter -E '${canonical_native_app_expr}' && ./scripts/nextest-admitted.sh witchy -- cargo nextest run --test cli_subcommands -E 'test(=fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact)'"
    )
    if [ "$canonical_build_recipe" -eq 1 ]; then
        cmds+=("cargo nextest run -p witchy-runtime --features native --lib -E 'test(/^runtime::host::build_read_tests::/) or test(/^runtime::host::build_env_tests::/) or test(=runtime::host::closure_owner::tests::build_read_captures_authenticate_kind_session_projection_and_cleanup) or test(=runtime::host::closure_owner::tests::build_env_captures_reauthenticate_root_and_optional_response_and_cleanup)'")
    fi
    if [ "$canonical_crypto" -eq 1 ]; then
        cmds+=("cargo nextest run -p witchy-runtime --features native --lib -E 'test(/^runtime::host::crypto_rsa_slices_tests::/)'")
    fi
elif [ "$edit_loop" -eq 1 ] && [ "$all_paths_have_early" -eq 1 ] \
    && [ "${#early_cmds[@]}" -gt 0 ]; then
    cmds=("${early_cmds[@]}")
elif [ "$any_rust" -eq 1 ] && [ "$focused" -eq 0 ]; then
    remaining=()
    for c in ${cmds[0]+"${cmds[@]}"}; do
        case "$c" in
            "./scripts/nextest-admitted.sh "*" -- cargo nextest run -p "* | \
            "./scripts/nextest-admitted.sh "*" -- cargo nextest run --test "* | \
            "./scripts/nextest-admitted.sh "*" -- cargo nextest run --bin "* | \
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'" | \
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run --features witchy-wir/native -E 'test(/^example_tests::/)'" | \
            "cargo nextest run -p "* | \
            "cargo nextest run --test "* | \
            "cargo nextest run --bin "* | \
            "cargo nextest run -E 'test(/^example_tests::/)'" | \
            "cargo nextest run --features witchy-wir/native -E 'test(/^example_tests::/)'" | \
            "cargo nextest run -E 'test(stdlib_docs_are_current)'" | \
            "cargo check -p witchy --all-targets" | \
            "cargo clippy -p witchy --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use") ;;
            *) remaining+=("$c") ;;
        esac
    done
    cmds=(${early_cmds[0]+"${early_cmds[@]}"} "./scripts/check.sh --fast" ${remaining[0]+"${remaining[@]}"})
else
    cmds=(${early_cmds[0]+"${early_cmds[@]}"} ${cmds[0]+"${cmds[@]}"})
fi

# A complete binary/package run strictly includes a filtered run with the same
# command prefix. Combined paths can independently select both (for example, a
# census generator selects all of `misc` while its snapshot test selects one
# module), so keep only the broad command instead of compiling and executing
# the same tests twice.
full_queue_infra=0
for c in ${cmds[0]+"${cmds[@]}"}; do
    [ "$c" != "./scripts/check.sh --queue-infra" ] || full_queue_infra=1
done
subsumed_cmds=()
for c in ${cmds[0]+"${cmds[@]}"}; do
    # The full queue shard runs both merge-queue-tests binaries with
    # --ignore-default-filter. Its smaller --queue-infra-edit counterpart does
    # not, so edit-loop selections deliberately retain direct tooling probes.
    if [ "$full_queue_infra" -eq 1 ]; then
        case "$c" in
            "cargo nextest run -p merge-queue-tests "*) continue ;;
        esac
    fi
    unfiltered="${c%% -E *}"
    subsumed=0
    if [ "$unfiltered" != "$c" ]; then
        for broad in ${cmds[0]+"${cmds[@]}"}; do
            if [ "$broad" = "$unfiltered" ]; then
                subsumed=1
                break
            fi
        done
    fi
    [ "$subsumed" -eq 1 ] || subsumed_cmds+=("$c")
done
cmds=(${subsumed_cmds[0]+"${subsumed_cmds[@]}"})

# Cargo builds a complete selected test target before nextest applies `-E`.
# Combined paths can select several exact probes from that same target (for
# example project-resolver and CLI edits both select `cli_subcommands`). Running
# those commands separately paid the identical optimized compiler graph twice:
# on 2026-09-24 the first one-test build took 32m26s under host contention and
# the immediately following three-test command started rebuilding `witchy`.
# Coalesce only commands whose entire prefix before `-E` is byte-identical.
# This deliberately keeps direct security probes separate from
# nextest-admitted.sh, and keeps distinct features, packages, targets and
# environment assignments in separate builds.
coalesced_cmds=()
coalesced_prefixes=()
coalesced_exprs=()
for c in ${cmds[0]+"${cmds[@]}"}; do
    prefix="${c%% -E *}"
    case "$c" in
        *" -E '"*"'") ;;
        *) coalesced_cmds+=("$c"); coalesced_prefixes+=(""); coalesced_exprs+=(""); continue ;;
    esac
    # A compound shell command owns more than one execution policy. Leave it
    # intact rather than treating text after its first expression as a filter.
    case "$c" in
        *"' && "*) coalesced_cmds+=("$c"); coalesced_prefixes+=(""); coalesced_exprs+=(""); continue ;;
    esac
    expr="${c#* -E \'}"
    expr="${expr%\'}"
    merged=0
    for i in "${!coalesced_cmds[@]}"; do
        if [ "${coalesced_prefixes[$i]}" = "$prefix" ]; then
            coalesced_exprs[$i]="(${coalesced_exprs[$i]}) or ($expr)"
            coalesced_cmds[$i]="$prefix -E '${coalesced_exprs[$i]}'"
            merged=1
            break
        fi
    done
    if [ "$merged" -eq 0 ]; then
        coalesced_cmds+=("$c")
        coalesced_prefixes+=("$prefix")
        coalesced_exprs+=("$expr")
    fi
done
cmds=(${coalesced_cmds[0]+"${coalesced_cmds[@]}"})

if [ "${#cmds[@]}" -eq 0 ]; then
    echo "test-for-paths: prose-only change — no focused checks needed (still submit through the queue)"
    exit 0
fi

if [ "${#canonical_native_batch[@]}" -gt 0 ]; then
    echo "canonical native: one union build; first three selections reuse fresh nextest metadata (runtime profiles stay separate)"
fi
echo "focused checks for this change (run before 'merge-queue.sh submit'):"
for c in "${cmds[@]}"; do echo "  $c"; done

if [ "$run" -eq 1 ]; then
    run_started_at="$SECONDS"
    first_command=0
    if [ "${#canonical_native_batch[@]}" -gt 0 ]; then
        run_sha="$(git rev-parse HEAD)"
        ./scripts/canonical-native-edit.sh "${canonical_native_batch[@]}"
        first_command=3
    fi
    for ((i=first_command; i<${#cmds[@]}; i++)); do
        c="${cmds[$i]}"
        printf '\n\033[1;34m==> %s\033[0m\n' "$c"
        bash -c "$c"
    done
    if [ "${#canonical_native_batch[@]}" -gt 0 ] && [ "$(git rev-parse HEAD)" != "$run_sha" ]; then
        echo "canonical native: HEAD changed during checks; no receipt" >&2
        exit 1
    fi
    printf '\n\033[1;32mall focused checks green\033[0m\n'
    # Only a path set derived from the complete branch diff can certify the
    # current tip. Explicit paths may cover only part of the change. The
    # recorder independently requires a clean tree and treats write failures
    # as advisory, so validation success never depends on local queue state.
    if [ "$explicit_paths" -eq 0 ]; then
        receipt_shard="paths"
        [ "$edit_loop" -eq 0 ] || receipt_shard="edit"
        ./scripts/record-shard-receipt.sh "$receipt_shard" "$((SECONDS - run_started_at))" || true
    fi
fi
