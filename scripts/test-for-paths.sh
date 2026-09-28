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
# rfc0* 15 takes 24s -- so a native conformance gate spends ~39s per attempt
# compiling and linking binaries it will filter out at runtime, against a
# ~166s median gate. Front-end-only routes below now stop before this graph.
#
# The obvious fix (emit `--test rfc0NNN` instead of `-p witchy`) is WRONG for
# the reason above, and the failure is silent: measured on the same tree,
# `nextest list -p witchy-lower -p witchy` reports 2174 tests, while
# `-p witchy-lower -p witchy --test rfc0101` reports 2. The AND drops
# witchy-lower entirely and the gate would go green having skipped 2,172
# tests. Do not narrow the build this way.
#
# Mixed lib-test and explicit-target scopes are emitted as two nextest
# partitions below. Each invocation has one globally valid Cargo target mode;
# check.sh runs them inside the same fail-fast stage and gate budget.
gate_pkgs=()
gate_tests=()
gate_need_examples=0
gate_need_stdlib_docs=0
gate_workspace=0
corpus_impact=0
gate_example_mods=()
gate_witchy_tests=()
gate_misc_modules=()
gate_need_conformance=0
gate_need_std_roots=0
gate_pure_wir=0
gate_native_wir=0
# `check.sh` is exercised directly by state_and_gate and preflight_reuse. When
# a diff is confined to the script and its primary fixture, running unrelated
# coordinator/daemon fixtures added 178 seconds to a gate whose compilation
# took 10 seconds. Any third path clears the filter and falls back to the
# complete selected binary.
gate_queue_expr='test(/^gate_behavior::(state_and_gate|preflight_reuse)::/)'
for gate_queue_path in "${paths[@]}"; do
    case "$gate_queue_path" in
        scripts/check.sh | \
        tools/merge-queue-tests/tests/merge_queue/gate_behavior/state_and_gate.rs) ;;
        *) gate_queue_expr=""; break ;;
    esac
done
gate_worktree_expr='test(/^warm::cargo_shim_/)'
gate_cargo_shim_touched=0
for gate_worktree_path in "${paths[@]}"; do
    case "$gate_worktree_path" in
        scripts/bin/cargo) gate_cargo_shim_touched=1 ;;
        tools/merge-queue-tests/tests/worktree/warm.rs) ;;
        *) gate_worktree_expr=""; break ;;
    esac
done
[ "$gate_cargo_shim_touched" -eq 1 ] || gate_worktree_expr=""
gate_lower_contracts_only=1
for gate_lower_path in "${paths[@]}"; do
    case "$gate_lower_path" in
        crates/witchy-lower/tests/compiler_contracts.rs | \
        crates/witchy-lower/tests/compiler_contracts/*) ;;
        *) gate_lower_contracts_only=0; break ;;
    esac
done
add_pkg() {
    local p="$1" x
    for x in ${gate_pkgs[0]+"${gate_pkgs[@]}"}; do [ "$x" = "$p" ] && return; done
    gate_pkgs+=("$p")
}
gate_package_dir() { # gate_package_dir <workspace package>
    case "$1" in
        merge-queue-tests | repo-tests | witchy-snapshot-tools) printf 'tools/%s\n' "$1" ;;
        witchy) printf '.\n' ;;
        *) printf 'crates/%s\n' "$1" ;;
    esac
}
add_gate_test() {
    local t="$1" x
    # `case` globs treat `*` as matching `/`, so the generic `tests/*.rs`
    # route can receive a nested module such as `tests/misc/foo.rs`.
    # Nested modules are not Cargo integration-test binary names. Fail safe to
    # the workspace instead of emitting an invalid `binary(misc/foo)`
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
add_misc_module() {
    local m="$1" x
    for x in ${gate_misc_modules[0]+"${gate_misc_modules[@]}"}; do [ "$x" = "$m" ] && return; done
    gate_misc_modules+=("$m")
}
emit_gate_nextest() {
    local p t args="" expr="" m need_witchy=0
    if [ "$gate_workspace" -eq 1 ]; then
        printf 'WORKSPACE\n'
        return 0
    fi
    # These tests live in a deliberately small integration binary so a
    # structural lowering proof never links the monolithic codegen harness.
    # Mixed diffs fall through to the ordinary package-safe union below.
    if [ "$gate_lower_contracts_only" -eq 1 ]; then
        printf '%s\n' '-p witchy-lower --test compiler_contracts'
        printf '%s\n' 'package(witchy-lower)'
        printf '%s\n' '-p witchy-lower'
        return 0
    fi
    # SafeWIR and the checker/WIR contract are target-neutral compiler
    # surfaces. Their WIR unit checks encode and validate Wasm with wasmparser;
    # they do not need to execute it under Wasmtime or use the native AWS-LC
    # provider. Keep this partition explicit so Cargo does not compile the
    # native-gate integration target, and put downstream compiler packages in
    # a second invocation because Cargo target filters are intersections.
    # Full CI retains the native execution campaign. Any mixed path that owns
    # native WIR evidence falls through to the ordinary fail-safe route.
    # A mixed checked-source/root edit needs the root package's default native
    # feature graph. Selecting the pure WIR partition for the whole union
    # disables that graph and makes root tests fail to compile.
    if [ "$gate_pure_wir" -eq 1 ] && [ "$gate_native_wir" -eq 0 ] \
        && [ "$gate_need_conformance" -eq 0 ] \
        && [[ " ${gate_pkgs[*]} " != *" witchy "* ]]; then
        local pure_expr="" pure_extra="" pure_checks="" pure_pkg
        for pure_pkg in "${gate_pkgs[@]}"; do
            pure_expr="${pure_expr:+$pure_expr or }package($pure_pkg)"
            pure_checks="${pure_checks:+$pure_checks }-p $pure_pkg"
            [ "$pure_pkg" = witchy-wir ] \
                || pure_extra="${pure_extra:+$pure_extra }-p $pure_pkg"
        done
        printf '%s\n' '-p witchy-wir --test unit --test structural_contracts --test rfc0162_mutation_evidence --test rfc0165_route_gate --no-default-features'
        printf '%s\n' "$pure_expr"
        printf '%s\n' "$pure_checks --no-default-features"
        [ -z "$pure_extra" ] || printf '%s\n' "$pure_extra --no-default-features"
        return 0
    fi
    if [ "${#gate_pkgs[@]}" -eq 0 ] && [ "${#gate_tests[@]}" -eq 0 ] \
        && [ "$gate_need_examples" -eq 0 ] && [ "$gate_need_stdlib_docs" -eq 0 ] \
        && [ "${#gate_example_mods[@]}" -eq 0 ] \
        && [ "${#gate_witchy_tests[@]}" -eq 0 ] \
        && [ "${#gate_misc_modules[@]}" -eq 0 ]; then
        printf 'WORKSPACE\n'
        return 0
    fi
    # Pure integration-test selection keeps `--test` args so a single
    # integration-binary change stays one binary, not `-p witchy`.
    if [ "${#gate_tests[@]}" -gt 0 ] && [ "${#gate_pkgs[@]}" -eq 0 ] \
        && [ "$gate_need_examples" -eq 0 ] && [ "$gate_need_stdlib_docs" -eq 0 ] \
        && [ "${#gate_example_mods[@]}" -eq 0 ] \
        && [ "${#gate_witchy_tests[@]}" -eq 0 ] \
        && [ "${#gate_misc_modules[@]}" -eq 0 ]; then
        for t in "${gate_tests[@]}"; do
            args="${args:+$args }--test $t"
        done
        printf '%s\n' "$args"
        [ -z "$gate_queue_expr" ] || printf '%s\n' "$gate_queue_expr"
        [ -z "$gate_worktree_expr" ] || printf '%s\n' "$gate_worktree_expr"
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
    # RFC conformance binaries adjudicate semantics ACROSS the native pipeline,
    # which is precisely what a single-crate selection cannot see. Keep them on
    # stages that emit, admit, or execute Wasm. Syntax/type routes deliberately
    # stop at the checker: the test bodies are cheap, but linking their root
    # targets pulls Wasmtime and AWS-LC. Full CI retains the complete campaign.
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
    # One 0.2s test. Its binary is witchy-types' `typeck` integration test
    # (the crate's lib target is `test = false`), which the gate's warm target
    # already holds unless witchy-types itself changed, and then the crate is
    # selected anyway. The narrowed-target form below names `--test typeck`
    # for it explicitly, since `--lib` no longer reaches it.
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
    for m in ${gate_misc_modules[0]+"${gate_misc_modules[@]}"}; do
        need_witchy=1
        # Nextest prefixes test names with the binary label (for example
        # `witchy::misc `), so anchoring at the module name selects nothing.
        expr="${expr:+$expr or }test(/.*${m}::/)"
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
    # A touched `tests/misc/<module>.rs` needs the misc integration binary, but
    # not every test within it. Keep the target explicit while the expression
    # above selects only the changed modules. This composes with `--bin witchy`
    # when one diff touches both root target families.
    if [ "${#gate_misc_modules[@]}" -gt 0 ] \
        && [ "${#gate_pkgs[@]}" -eq 0 ] \
        && [ "$gate_need_conformance" -eq 0 ]; then
        args="$args --test misc"
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
    #   - a crate has a bin or explicit [[test]]/[[bin]]/auto* targets, which
    #     `--lib` + `tests/*.rs` would silently miss
    #   - a package directory cannot be resolved
    # A mixed set is partitioned: normal lib-test packages use `--lib` plus
    # their named integration tests, while `[lib] test = false` and
    # integration-only packages join the explicitly named root targets. This
    # avoids both empty compiler cfg(test) copies and package-wide root builds.
    # The root bin is safe in the explicit target union: `--bin witchy` joins
    # every selected package's named lib/test targets instead of replacing
    # them. Free-form root test names still fail safe unless they are known
    # main.rs module families.
    local name_targets=0 pkg dir file pkg_mode has_types=0
    local enabled_args="" explicit_args="" enabled_targets="" explicit_targets=""
    local root_targets="" extra_args=""
    local target_pkgs=(${gate_pkgs[0]+"${gate_pkgs[@]}"})
    # The std-roots pin can add witchy-types outside gate_pkgs. It participates
    # in the target-mode proof just like a mapped package; otherwise a runtime
    # package (lib tests enabled) could be mixed unsafely with types (disabled).
    if [ "$gate_need_std_roots" -eq 1 ] && [[ " $args " == *" -p witchy-types "* ]]; then
        for pkg in ${target_pkgs[0]+"${target_pkgs[@]}"}; do
            [ "$pkg" != witchy-types ] || has_types=1
        done
        [ "$has_types" -eq 1 ] || target_pkgs+=(witchy-types)
    fi
    if [ "$need_witchy" -eq 1 ] && [ "${#gate_pkgs[@]}" -gt 0 ] \
        && [ "$witchy_tests_all_bin" -eq 1 ] \
        && [[ " $args " != *" --bin "* ]]; then
        name_targets=1
        for pkg in "${target_pkgs[@]}"; do
            dir="$(gate_package_dir "$pkg")"
            if [ ! -f "$dir/Cargo.toml" ] || [ -e "$dir/src/main.rs" ] || [ -d "$dir/src/bin" ] \
                || grep -qE '^\[\[(test|bin|bench|example)\]\]|^auto(tests|bins)' "$dir/Cargo.toml"; then
                name_targets=0
                break
            fi
            pkg_mode=enabled
            if [ ! -f "$dir/src/lib.rs" ] \
                || sed -n '/^\[lib\]/,/^\[/p' "$dir/Cargo.toml" | grep -qE '^test *= *false'; then
                pkg_mode=explicit
            fi
            if [ "$pkg_mode" = enabled ]; then
                enabled_args="${enabled_args:+$enabled_args }-p $pkg"
            else
                explicit_args="${explicit_args:+$explicit_args }-p $pkg"
            fi
            for file in "$dir"/tests/*.rs; do
                [ -f "$file" ] || continue
                file="${file##*/}"
                if [ "$pkg_mode" = enabled ]; then
                    enabled_targets="$enabled_targets --test ${file%.rs}"
                else
                    explicit_targets="$explicit_targets --test ${file%.rs}"
                fi
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
                root_targets="$root_targets --test $t"
            else
                name_targets=0
                break
            fi
        done
    fi
    if [ "$name_targets" -eq 1 ]; then
        if [ "$gate_need_examples" -eq 1 ] \
            || [ "${#gate_example_mods[@]}" -gt 0 ] \
            || [ "$gate_need_stdlib_docs" -eq 1 ] \
            || [ "${#gate_witchy_tests[@]}" -gt 0 ]; then
            root_targets="$root_targets --bin witchy"
        fi
        if [ "${#gate_misc_modules[@]}" -gt 0 ]; then
            root_targets="$root_targets --test misc"
        fi
        if [ "$gate_need_conformance" -eq 1 ]; then
            # `--bin witchy` carries `example_tests::rfc0*` (see above).
            case " $root_targets " in *" --bin witchy "*) ;; *) root_targets="$root_targets --bin witchy" ;; esac
            case " $root_targets " in *" --test misc "*) ;; *) root_targets="$root_targets --test misc" ;; esac
            for file in tests/rfc0*.rs; do
                [ -f "$file" ] || continue
                file="${file##*/}"
                root_targets="$root_targets --test ${file%.rs}"
            done
        fi
        if [ "$gate_need_std_roots" -eq 1 ] \
            && [[ " $explicit_targets " != *" --test typeck "* ]]; then
            explicit_targets="$explicit_targets --test typeck"
        fi
        if [ -n "$enabled_args" ] && { [ -n "$explicit_args" ] || [ "$need_witchy" -eq 1 ]; }; then
            # Primary: explicit compiler/integration targets plus root
            # conformance. Secondary: packages whose unit tests require the
            # global --lib selector. The inclusion expression is safe for both;
            # unavailable packages simply match nothing in that partition.
            args="$explicit_args"
            [ "$need_witchy" -eq 0 ] || args="${args:+$args }-p witchy"
            args="$args$explicit_targets$root_targets"
            extra_args="$enabled_args --lib$enabled_targets"
        elif [ -n "$enabled_args" ]; then
            args="$enabled_args --lib$enabled_targets$root_targets"
        else
            args="$explicit_args"
            [ "$need_witchy" -eq 0 ] || args="${args:+$args }-p witchy"
            args="$args$explicit_targets$root_targets"
        fi
    fi
    [ -n "$args" ] || { printf 'WORKSPACE\n'; return 0; }
    printf '%s\n' "$args"
    [ -n "$expr" ] && printf '%s\n' "$expr"
    # Third line: compile-check package set (mapped crates only — not the
    # extra -p witchy added so example_tests / integration tests can run).
    # Empty when there are no mapped crates.
    if [ "${#gate_pkgs[@]}" -gt 0 ]; then
        local cargs="" q
        for q in "${gate_pkgs[@]}"; do
            cargs="${cargs:+$cargs }-p $q"
        done
        printf '%s\n' "$cargs"
    fi
    # Fourth line: an optional second target-safe nextest partition. It shares
    # the inclusion expression on line two and the same serialized gate budget.
    [ -z "$extra_args" ] || printf '%s\n' "$extra_args"
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
pure_compiler_edit_paths_only=1
for p in "${paths[@]}"; do
    path_has_early=0
    case "$p" in
        crates/witchy-canonical-source/src/* | crates/witchy-canonical-source/tests/* | \
        crates/witchy-types/src/* | crates/witchy-types/tests/* | \
        crates/witchy-lower/src/* | crates/witchy-lower/tests/* | \
        crates/witchy-wir/src/* | crates/witchy-wir/tests/* | \
        rfcs/* | book/* | spec/language.md | docs/*) ;;
        *) pure_compiler_edit_paths_only=0 ;;
    esac
    case "$p" in
        crates/witchy-canonical-source/src/* | \
        crates/witchy-wir/src/canonical*)
            canonical_rewrite=1 ;;
        tests/misc/rfc0165_build_env_project.rs | \
        tests/misc/rfc0165_build_read_project.rs) ;;
        tests/misc/rfc0165_*.rs)
            canonical_rewrite=1 ;;
    esac
    case "$p" in
        crates/witchy-wir/src/canonical_wasmir.rs | \
        crates/witchy-wir/src/canonical_wasmir/lowering.rs | \
        crates/witchy-wir/src/canonical_wasmir/correspondence.rs | \
        crates/witchy-wir/src/canonical_wasmir/emit.rs | \
        crates/witchy-wir/src/canonical_wasmir/structured.rs | \
        crates/witchy-wir/src/canonical_wasmir/plan.rs | \
        crates/witchy-wir/src/canonical_wasmir/plan_match.rs)
            canonical_wasmir_tail=1 ;;
    esac
    case "$p" in
        crates/witchy-lower/src/canonical_build_read.rs)
            canonical_build_recipe=1 ;;
        crates/witchy-types/src/pipeline/build_recipe.rs)
            canonical_build_recipe=1 ;;
        crates/witchy-wir/src/canonical_safewir/build_read.rs)
            canonical_build_recipe=1
            canonical_rewrite=1 ;;
    esac
    case "$p" in
        crates/witchy-lower/src/canonical_crypto.rs)
            canonical_crypto=1 ;;
        crates/witchy-runtime/src/runtime/host/crypto*.rs | \
        crates/witchy-wir/src/canonical_safewir/crypto.rs)
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
        crates/witchy-interp/src/interpreter/runners.rs | \
        crates/witchy-runtime/src/runtime/console_fixtures.rs | \
        crates/witchy-test-host/src/lib.rs | \
        crates/witchy-types/src/pipeline/test_driver/console_fixture.rs | \
        crates/witchy-types/src/pipeline/test_driver/console_fixture/arguments.rs)
            canonical_fixture_rewrite=1 ;;
    esac
    case "$p" in
        crates/witchy-interp/src/interpreter/runners.rs | \
        crates/witchy-runtime/src/runtime/console_fixtures.rs | \
        crates/witchy-test-host/src/lib.rs | \
        crates/witchy-types/src/pipeline/test_driver/console_fixture.rs | \
        crates/witchy-types/src/pipeline/test_driver/console_fixture/arguments.rs | \
        scripts/rfc0165-deferred-tests.txt) ;;
        *) canonical_fixture_rewrite_paths_only=0 ;;
    esac
    case "$p" in
        .mbx.toml)
            # Build-cache and scheduler policy is exercised by the cargo-shim
            # and worktree-warming fixtures. It cannot alter Witchy semantics,
            # so never fall through to a native workspace build here.
            add_gate_test worktree
            add "cargo nextest run -p merge-queue-tests --test worktree" ;;
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
        crates/witchy-runtime/src/runtime/host/build_read.rs | \
        crates/witchy-runtime/src/runtime/host/build_read_tests.rs)
            add_early "cargo nextest run -p witchy-runtime --features native --test runtime -E 'test(/^runtime::host::build_read::tests::/) or test(=runtime::host::closure_owner::tests::build_read_captures_authenticate_kind_session_projection_and_cleanup)'"
            add_early "cargo nextest run -p witchy --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature)'"
            path_has_early=1 ;;
        crates/witchy-runtime/src/runtime/host/build_env.rs | \
        crates/witchy-runtime/src/runtime/host/build_env_tests.rs)
            add_early "cargo nextest run -p witchy-runtime --features native --test runtime -E 'test(/^runtime::host::build_env::tests::/) or test(=runtime::host::closure_owner::tests::build_env_captures_reauthenticate_root_and_optional_response_and_cleanup)'"
            add_early "cargo nextest run -p witchy --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature)'"
            path_has_early=1 ;;
        crates/witchy-types/src/pipeline/test_driver/integration.rs)
            add_early "cargo nextest run -p witchy --features witchy-wir/native --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature) or test(=rfc0077_integration_grants::source_integration_recipe_runs_selected_console_capture_and_reports_abort)'"
            path_has_early=1 ;;
        crates/witchy-types/src/pipeline/test_driver.rs | \
        crates/witchy-types/src/pipeline/test_driver/*.rs)
            add_early "cargo nextest run --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature)'"
            path_has_early=1 ;;
        tests/misc/rfc0165_owner_adapters.rs)
            # This admission-boundary assertion previously failed only after
            # the full workspace stage. Keep the exact raw-Wasm rejection
            # probe in the edit loop; the complete adapter family remains in
            # the landing selection below.
            add_early "cargo nextest run -p witchy --features witchy-wir/native --test misc --ignore-default-filter -E 'test(=rfc0165_owner_adapters::server_workers_reject_raw_wasm_even_with_genuine_entry_metadata)'"
            path_has_early=1 ;;
        tests/misc/rfc0165_build_read_project.rs | \
        tests/misc/rfc0165_build_env_project.rs)
            # These are nightly-deferred, so the admitted-test wrapper filters
            # them out. Run the focused edit probe directly; the landing gate
            # retains its deferred selection policy.
            _misc="${p##*/}"
            _misc="${_misc%.rs}"
            add_early "cargo nextest run --test misc -E 'test(/.*${_misc}::/)'"
            path_has_early=1 ;;
        tests/misc/rfc0077_integration_grants.rs)
            # The async source-integration cases each take about 166s and run
            # in the complete landing selection. Keep the edit loop on the
            # remaining grant, isolation, and synchronous recipe assertions.
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/.*rfc0077_integration_grants::/) and not test(/.*rfc0077_integration_grants::source_integration_async_recipe_/)'"
            path_has_early=1 ;;
        tests/misc/semantic_conformance.rs)
            add_early "cargo nextest run --test misc -E 'test(=semantic_conformance::independent_expectation_rejects_a_shared_semantic_mutation)'"
            path_has_early=1 ;;
        tests/cli_subcommands.rs)
            # Keep the edit loop on the recent gate-red output contract plus
            # one sandbox authority and one fetch-root admission guard. The
            # complete cli_subcommands target remains in landing.
            add_early "cargo nextest run --test cli_subcommands -E 'test(=source_nil_main_int_output_is_not_eaten_as_exit_code) or test(=sandbox_wasm_requires_explicit_dir_grant) or test(=fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact)'"
            path_has_early=1 ;;
        crates/witchy-wir/tests/structural_contracts/owned_slot_child_storage.rs)
            add_early "cargo nextest run -p witchy-wir --test structural_contracts -E 'test(=owned_slot_child_storage::owned_slots_require_managed_child_storage_before_consuming_move)'"
            path_has_early=1 ;;
        crates/witchy-wir/src/wir_prelude.rs | \
        crates/witchy-wir/examples/abi_catalog.rs | \
        tests/misc/wasm_abi_catalog.rs | \
        spec/wasm-abi.md)
            add_early "cargo nextest run --test misc -E 'test(wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature) or test(wasm_abi_catalog::public_spec_contains_the_generated_complete_catalog) or test(wasm_abi_catalog::capability_imports_name_their_authority_family) or test(wasm_abi_catalog::precompiled_runner_authority_rows_are_cataloged)'"
            path_has_early=1 ;;
    esac
    case "$p" in
        crates/witchy-wir/src/canonical_safewir/check/memory.rs | \
        crates/witchy-wir/src/canonical_safewir/check/memory/*.rs)
            add_early "cargo nextest run -p witchy-wir --no-default-features --test unit -E 'test(constant_prefix_publication_rejects_gaps_and_missing_children)'"
            path_has_early=1 ;;
        crates/witchy-lower/src/canonical_list_update.rs | \
        crates/witchy-lower/src/canonical_pipeline.rs)
            add_early "cargo nextest run -p witchy-lower --features native-test-runtime --test codegen -E 'test(canonical_pipeline::list_)'"
            path_has_early=1 ;;
        crates/witchy-lower/tests/codegen.rs)
            add_early "cargo nextest run -p witchy-lower --test codegen -E 'test(=tests::grantful_build_primitives_compile_to_build_imports_only)'"
            path_has_early=1 ;;
        tests/misc/rfc0165_admitted_launch.rs)
            add_early "cargo nextest run -p witchy --features witchy-wir/native --test misc --ignore-default-filter -E 'test(rfc0165_admitted_launch::admitted_aggregates_render_structurally_as_expected)'"
            path_has_early=1 ;;
    esac
    case "$p" in
        crates/witchy-wir/tests/structural_contracts.rs | \
        crates/witchy-wir/tests/structural_contracts/*)
            any_rust=1
            # These tests inspect SafeWIR and encoded structure only. Selecting
            # their target directly keeps Wasmtime and AWS-LC out of the graph;
            # native execution remains mandatory for WIR source changes.
            add_gate_test structural_contracts
            add "cargo nextest run -p witchy-wir --test structural_contracts"
            ;;
        crates/witchy-canonical-source/*)
            any_rust=1
            gate_pure_wir=1
            # The checked-source contract (RFC-0170) sits under the checker,
            # SafeWIR and lowering; each of them observes its judgments. None
            # of those checks needs the native Wasm engine or crypto provider.
            add_pkg witchy-canonical-source
            add_pkg witchy-types
            add_pkg witchy-wir
            add_pkg witchy-lower
            add "cargo nextest run -p witchy-canonical-source -p witchy-types"
            add "cargo nextest run -p witchy-wir --test unit --test structural_contracts --test rfc0162_mutation_evidence --test rfc0165_route_gate --no-default-features"
            add "./scripts/nextest-admitted.sh witchy-lower -- cargo nextest run -p witchy-lower" ;;
        crates/witchy-types/*)
            any_rust=1
            add_pkg witchy-types
            # Keep ordinary checker edits on the target-neutral front end.
            # Lower/root conformance pulls Wasmtime and AWS-LC before running
            # a test; those cross-pipeline campaigns remain selected by lower,
            # WIR, runtime, canonical-contract, and full-CI changes. The types
            # integration binary carries the checker's executable judgments.
            add "cargo nextest run -p witchy-types"
            ;;
        crates/witchy-lower/tests/compiler_contracts.rs | \
        crates/witchy-lower/tests/compiler_contracts/*)
            any_rust=1
            add_pkg witchy-lower
            add "cargo nextest run -p witchy-lower --test compiler_contracts"
            ;;
        crates/witchy-lower/*)
            any_rust=1
            add_pkg witchy-lower
            # Lowering's structural and checked-source campaigns do not need a
            # native engine. Root conformance is retained for WIR/runtime/root
            # edits and full CI; attaching it here restores Wasmtime and AWS-LC
            # before these fast tests can start.
            add "./scripts/nextest-admitted.sh witchy-lower -- cargo nextest run -p witchy-lower"
            ;;
        crates/witchy-wir/src/canonical_safewir/build_read.rs | \
        crates/witchy-wir/src/canonical_safewir/crypto.rs)
            any_rust=1
            gate_native_wir=1
            gate_need_conformance=1
            add_pkg witchy-wir
            # Downstream compiler crates consume this one, and their own tests
            # (e.g. witchy-lower's canonical_pipeline admission tests) observe a
            # change here that this crate's tests cannot. bd239ce55 changed the
            # checker in witchy-wir, gated green without witchy-lower's tests,
            # and left master red on one of them.
            add_pkg witchy-lower
            # Do not select witchy-runtime's package tests here. The root
            # conformance targets below still compile the production runtime
            # against the changed WIR, while `package(witchy-runtime)` also
            # builds its 174-test cfg(test) copy. Runtime source edits select
            # that package in their own route. Keeping the native host suite
            # off a compiler-only edit is the boundary: WIR correctness is
            # adjudicated by WIR/lower/root conformance, and runtime internals
            # remain covered when they actually change.
            add "cargo nextest run -p witchy-wir --features witchy-wir/native"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'"
            ;;
        crates/witchy-wir/src/canonical_safewir.rs | \
        crates/witchy-wir/src/canonical_safewir/* | \
        crates/witchy-wir/src/safewir_validate.rs)
            any_rust=1
            gate_pure_wir=1
            add_pkg witchy-wir
            add_pkg witchy-lower
            add "cargo nextest run -p witchy-wir --test unit --test structural_contracts --test rfc0162_mutation_evidence --test rfc0165_route_gate --no-default-features"
            add "./scripts/nextest-admitted.sh witchy-lower -- cargo nextest run -p witchy-lower"
            ;;
        crates/witchy-wir/*)
            any_rust=1
            gate_native_wir=1
            gate_need_conformance=1
            add_pkg witchy-wir
            # Downstream compiler crates consume this one, and their own tests
            # (e.g. witchy-lower's canonical_pipeline admission tests) observe a
            # change here that this crate's tests cannot. bd239ce55 changed the
            # checker in witchy-wir, gated green without witchy-lower's tests,
            # and left master red on one of them.
            add_pkg witchy-lower
            # Do not select witchy-runtime's package tests here. The root
            # conformance targets below still compile the production runtime
            # against the changed WIR, while `package(witchy-runtime)` also
            # builds its 174-test cfg(test) copy. Runtime source edits select
            # that package in their own route. Keeping the native host suite
            # off a compiler-only edit is the boundary: WIR correctness is
            # adjudicated by WIR/lower/root conformance, and runtime internals
            # remain covered when they actually change.
            add "cargo nextest run -p witchy-wir --features witchy-wir/native"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'"
            ;;
        crates/witchy-runtime/*)
            any_rust=1
            gate_native_wir=1
            gate_need_conformance=1
            add_pkg witchy-runtime
            add "cargo nextest run -p witchy-runtime"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'"
            ;;
        crates/witchy-syntax/*)
            any_rust=1
            add_pkg witchy-syntax
            # Syntax changes are observed immediately by the checker, without
            # crossing into lower/WIR/root's Wasmtime and AWS-LC graph.
            add_pkg witchy-types
            add "cargo nextest run -p witchy-syntax -p witchy-types"
            ;;
        crates/witchy-interp/*)
            any_rust=1
            gate_need_conformance=1
            add_pkg witchy-interp
            # An interpreter edit cannot change witchy-caps' package tests.
            # Root conformance still compiles the caps production dependency;
            # reserving its feature-isolated test shard for caps edits avoids
            # a second profile build before the native union build.
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'" ;;
        crates/witchy-caps/*)
            any_rust=1
            gate_need_conformance=1
            add_pkg witchy-interp
            add_pkg witchy-caps
            add "cargo nextest run -p witchy-caps"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'" ;;
        src/main.rs | src/cli.rs | src/source.rs)
            gate_workspace=1
            add "cargo check -p witchy --all-targets"
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^(checked_cli_pipeline_tests|cli::|source::tests|test_mode_link_tests)::/)'"
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
        book/*)
            # Book validation is deferred while the compiler is being rebuilt.
            # In particular, prose accompanying a compiler change must not
            # introduce a second, native build into its pure edit loop.
            if [ "$edit_loop" -eq 0 ]; then
                corpus_impact=1
                add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'"
            fi
            ;;
        examples/*)
            corpus_impact=1
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'"
            ;;
        projects/grimoire/* | projects/coven/* | projects/coven-web/* | projects/glamour/* | projects/docs/*)
            # Consumers of the compiler: untested here. Keep the witchy fmt
            # gate, which still covers projects/**/src/*.witchy.
            add "find projects -type f -path '*/src/*.witchy' -exec \"\${CARGO_TARGET_DIR:-target}/debug/witchy\" fmt --check {} +" ;;
        tools/merge-queue-tests/Cargo.toml | \
        tools/merge-queue-tests/tests/merge_queue.rs | \
        tools/merge-queue-tests/tests/merge_queue/*.rs | \
        tools/merge-queue-tests/tests/merge_queue/**/*.rs)
            add_gate_test merge_queue
            add "./scripts/check.sh --queue-infra" ;;
        tools/merge-queue-tests/tests/test_for_paths.rs)
            add_gate_test test_for_paths
            add "./scripts/check.sh --queue-infra" ;;
        tools/merge-queue-tests/tests/worktree/warm.rs)
            add_gate_test worktree
            if [ -n "$gate_worktree_expr" ]; then
                add "cargo nextest run -p merge-queue-tests --test worktree -E '$gate_worktree_expr'"
            else
                add "cargo nextest run -p merge-queue-tests --test worktree"
            fi ;;
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
        tests/misc/rfc0087_migration_census.rs)
            any_rust=1
            add_early "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/.*rfc0087_migration_census::/)'"
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
        tests/misc.rs)
            any_rust=1
            add_gate_test misc
            add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc" ;;
        tests/misc/*)
            any_rust=1
            if [ ! -f "$p" ]; then
                # The module no longer exists, so a name filter would select
                # nothing. Run the complete surviving integration target.
                add_gate_test misc
                add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc"
            else
                _misc="${p##*/}"
                _misc="${_misc%.rs}"
                add_misc_module "$_misc"
                add "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/.*${_misc}::/)'"
            fi ;;
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
                rfc0080/* | rfc0082/* | rfc0130_generator/*)
                    test_name="rfc0080_0130_conformance"
                    ;;
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
            # This binary executes the real edit/receipt workflow and QoS harness;
            # coordinator fixtures substitute agent-check rather than exercise it.
            add_gate_test test_for_paths
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
        scripts/pure-compiler-check.sh | scripts/test-pure-compiler-check.sh)
            add_gate_test test_for_paths
            add "bash -n scripts/pure-compiler-check.sh scripts/test-pure-compiler-check.sh"
            add "bash scripts/test-pure-compiler-check.sh" ;;
        scripts/native-compiler-examples.sh | scripts/test-native-compiler-examples.sh)
            add_gate_test test_for_paths
            add "bash -n scripts/native-compiler-examples.sh scripts/test-native-compiler-examples.sh"
            add "bash scripts/test-native-compiler-examples.sh" ;;
        scripts/test-for-paths.sh | scripts/test-path-receipts.sh | scripts/test-impact.py | \
        scripts/nextest-admitted.sh | scripts/test-nextest-admitted.sh)
            add_gate_test test_for_paths
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "./scripts/test-path-receipts.sh"
            add "./scripts/test-nextest-admitted.sh"
            add "cargo nextest run -p merge-queue-tests --test test_for_paths"
            add "./scripts/check.sh --queue-infra" ;;
        scripts/bin/cargo)
            add_gate_test worktree
            # The shim's hermetic contracts live together in worktree/warm.rs.
            # Keep a shim-only edit off unrelated pool creation/status fixtures.
            add "for f in scripts/*.sh scripts/bin/*; do bash -n \"\$f\"; done"
            add "cargo nextest run -p merge-queue-tests --test worktree -E 'test(/^warm::cargo_shim_/)'" ;;
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
        spec/language.md)
            # The representative census type-checks every Witchy block in
            # this file. The broader interpreter documentation sweep is
            # explicitly deferred, so selecting it yields zero gate tests.
            add_pkg witchy-snapshot-tools
            # The migration corpus remains in the landing selection; it is
            # not a prerequisite to iterating on a compiler/spec change.
            if [ "$edit_loop" -eq 0 ]; then
                add "cargo nextest run -p witchy-snapshot-tools --test rfc0087_migration_census"
            fi
            ;;
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

# The complete example matrix (311 compile-heavy tests) was 100-164s of a
# 200-295s witchy-types edit loop on 2026-09-24. The edit loop runs one fixed
# hash slice of it -- about 1/8, spread across every example module -- and the
# serialized landing gate still runs all of it. WITCHY_EDIT_EXAMPLES=full
# restores the complete matrix locally.
edit_example_slice=hash:1/8
if [ "$edit_loop" -eq 1 ] && [ "${WITCHY_EDIT_EXAMPLES:-slice}" != full ]; then
    for i in "${!cmds[@]}"; do
        case "${cmds[$i]}" in
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'")
                cmds[$i]="./scripts/nextest-admitted.sh witchy -- cargo nextest run --partition $edit_example_slice -E 'test(/^example_tests::/)'" ;;
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^example_tests::/)'")
                cmds[$i]="./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy --partition $edit_example_slice -E 'test(/^example_tests::/)'" ;;
        esac
    done
fi

# Any Rust change means the fast gate already runs every workspace nextest test.
# Drop narrower nextest selections it subsumes, but retain independent shards
# such as Witchy source formatting.
if [ "$edit_loop" -eq 1 ] && [ "${#paths[@]}" -eq 1 ]; then
    case "${paths[0]}" in
        tests/misc/rfc0165_project_run.rs)
            cmds=("./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(/.*rfc0165_project_run::/)'" ) ;;
        tests/misc/rfc0165_owner_adapters.rs)
            cmds=("cargo nextest run -p witchy --features witchy-wir/native --test misc --ignore-default-filter -E 'test(=rfc0165_owner_adapters::server_workers_reject_raw_wasm_even_with_genuine_entry_metadata)'") ;;
        *) ;;
    esac
fi
if [ "$edit_loop" -eq 1 ] && [ "${#cmds[@]}" -gt 0 ] \
    && [ "${#paths[@]}" -eq 1 ] \
    && { [ "${paths[0]}" = "tests/misc/rfc0165_project_run.rs" ] \
        || [ "${paths[0]}" = "tests/misc/rfc0165_owner_adapters.rs" ]; }; then
    : # Keep the exact direct-file module route selected above.
elif [ "$edit_loop" -eq 1 ] && [ "$pure_compiler_edit_paths_only" -eq 1 ] \
    && [ "$gate_pure_wir" -eq 1 ] && [ "$gate_native_wir" -eq 0 ] \
    && [ "$gate_need_conformance" -eq 0 ] && [ "$gate_workspace" -eq 0 ]; then
    # Build the union of target-neutral compiler tests once. Separate crate
    # commands resolve different feature sets and serialize upstream builds
    # before downstream crates can start. The helper preserves the original
    # direct/admitted execution policies against fresh nextest metadata.
    pure_packages=""
    for pure_package in "${gate_pkgs[@]}"; do
        case "$pure_package" in
            witchy-canonical-source | witchy-types | witchy-wir | witchy-lower)
                pure_packages="$pure_packages $pure_package" ;;
        esac
    done
    cmds=("bash ./scripts/pure-compiler-check.sh$pure_packages")
    for pure_package in "${gate_pkgs[@]}"; do
        if [ "$pure_package" = repo-tests ]; then
            cmds+=("cargo nextest run -p repo-tests --test repo_hygiene -E 'test(rfc_status_hygiene::)'")
        fi
    done
elif [ "$edit_loop" -eq 1 ] && [ "$canonical_fixture_rewrite" -eq 1 ] \
    && [ "$canonical_fixture_rewrite_paths_only" -eq 1 ]; then
    # Console-fixture plumbing spans the checked driver, interpreter, host, and
    # native runtime. Combining those paths used to select seven
    # commands, including every example, every command test, and the Wasm
    # shard. Keep the edit loop on the three contracts the slice changes; the
    # complete crate, example, and Wasm coverage remains in landing.
    cmds=(
        "cargo nextest run --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature)'"
        "cargo nextest run -p witchy-test-host -p witchy-runtime -E 'test(/^(tests::|runtime::console_fixtures::tests::)/)'"
    )
elif [ "$edit_loop" -eq 1 ] && [ "$canonical_rewrite" -eq 1 ] \
    && [ "$canonical_rewrite_paths_only" -eq 1 ] \
    && { [ "$gate_pure_wir" -eq 0 ] || [ "$gate_native_wir" -eq 1 ] \
         || [ "$gate_need_conformance" -eq 1 ]; }; then
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
    canonical_build_recipe_lower_expr=""
    canonical_build_recipe_app_expr=""
    canonical_crypto_wir_expr=""
    canonical_crypto_app_expr=""
    if [ "$canonical_build_recipe" -eq 1 ]; then
        canonical_build_recipe_wir_expr=" or test(canonical_safewir::build_read::tests::build_env_catalog_authenticates_root_and_keeps_presence_internal)"
        canonical_build_recipe_lower_expr=" or test(=tests::grantful_build_primitives_compile_to_build_imports_only)"
        canonical_build_recipe_app_expr=" or test(/.*rfc0103_build_step_confinement::/) or test(/.*rfc0165_build_read_project::/) or test(/.*rfc0165_build_env_project::/)"
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
    canonical_native_wir_expr="test(canonical_safewir::build_read::tests::build_read_catalog_separates_root_authority_from_bounded_responses) or test(canonical_safewir::check::tests::scalar_checker_refuses_non_total_backend_combinations) or test(constant_prefix_publication_rejects_gaps_and_missing_children) or test(scalar_move_range_is_bounds_checked_and_admitted) or test(scalar_clone_range_is_ownership_safe_and_admitted) or test(dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot) or test(canonical_capabilities_lower_only_to_externref) or test(=validated_safewir_has_one_checker_owned_construction_surface) or test(canonical_wasm_encoding_is_private_and_requires_final_authority)${canonical_wasmir_tail_expr}${canonical_build_recipe_wir_expr}"
    canonical_native_lower_expr="test(canonical_pipeline::list_string_)${canonical_build_recipe_lower_expr}"
    canonical_native_app_expr="test(wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature) or test(wasm_abi_catalog::public_spec_contains_the_generated_complete_catalog) or test(wasm_abi_catalog::capability_imports_name_their_authority_family) or test(wasm_abi_catalog::precompiled_runner_authority_rows_are_cataloged) or test(rfc0077_integration_grants::source_integration_recipe_runs_selected_console_capture_and_reports_abort) or test(rfc0165_admitted_launch::admitted_entry_without_a_console_grant_is_refused_before_it_runs) or test(rfc0165_admitted_launch::admitted_guard_near_misses_trap_exactly_at_the_boundary) or test(rfc0165_admitted_launch::admitted_aggregates_render_structurally_as_expected) or test(=rfc0165_owner_adapters::server_workers_reject_raw_wasm_even_with_genuine_entry_metadata) or test(=semantic_conformance::independent_expectation_rejects_a_shared_semantic_mutation) or test(/^example_tests::rfc0/)${canonical_build_recipe_app_expr}"
    canonical_native_batch=("$canonical_native_wir_expr" "$canonical_native_lower_expr" "$canonical_native_app_expr" "$canonical_build_recipe")
    cmds=(
        "cargo nextest run -p witchy-wir --features native --test unit --test rfc0165_route_gate -E '${canonical_native_wir_expr}'"
        "cargo nextest run -p witchy-lower --features witchy-wir/native,witchy-lower/native-test-runtime --test codegen -E '${canonical_native_lower_expr}'"
        "WITCHY_ORACLE_EXAMPLE=calc cargo nextest run -p witchy --features witchy-wir/native --bin witchy --test misc --ignore-default-filter -E '${canonical_native_app_expr}' && ./scripts/nextest-admitted.sh witchy -- cargo nextest run --test cli_subcommands -E 'test(=fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact)'"
    )
    if [ "$canonical_build_recipe" -eq 1 ]; then
        cmds+=("cargo nextest run -p witchy-runtime --features native --test runtime -E 'test(/^runtime::host::build_read::tests::/) or test(/^runtime::host::build_env::tests::/) or test(=runtime::host::closure_owner::tests::build_read_captures_authenticate_kind_session_projection_and_cleanup) or test(=runtime::host::closure_owner::tests::build_env_captures_reauthenticate_root_and_optional_response_and_cleanup)'")
    fi
    if [ "$canonical_crypto" -eq 1 ]; then
        cmds+=("cargo nextest run -p witchy-runtime --features native --test runtime -E 'test(/^runtime::host::crypto::rsa_slices_tests::/)'")
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
            "cargo check -p witchy --all-targets") ;;
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

# Current multi-package routes share the frontend's default feature graph.
# Drop a run whose entire package set is already covered by another run; never
# union partially overlapping sets into a new feature-resolution combination.
# Recognize only this closed grammar: flags, filters, targets, wrappers and
# shell policies must retain their independent execution semantics.
plain_package_run_covers() {
    local broad="$1" narrow="$2" token
    local grammar='^cargo nextest run( -p [a-zA-Z0-9_-]+)+$'
    [[ "$broad" =~ $grammar && "$narrow" =~ $grammar ]] || return 1
    for token in ${narrow#cargo nextest run}; do
        [ "$token" = -p ] && continue
        case "$broad " in
            *" -p $token "*) ;;
            *) return 1 ;;
        esac
    done
}
package_cmds=()
for i in ${cmds[0]+"${!cmds[@]}"}; do
    c="${cmds[$i]}"
    subsumed=0
    for j in ${cmds[0]+"${!cmds[@]}"}; do
        [ "$i" != "$j" ] || continue
        broad="${cmds[$j]}"
        if plain_package_run_covers "$broad" "$c"; then
            # Equivalent sets in a different order keep the first command;
            # a strict superset wins regardless of path-selection order.
            if [ "$j" -lt "$i" ] || ! plain_package_run_covers "$c" "$broad"; then
                subsumed=1
                break
            fi
        fi
    done
    [ "$subsumed" -eq 1 ] || package_cmds+=("$c")
done
cmds=(${package_cmds[0]+"${package_cmds[@]}"})

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

# These two native scopes share the same feature graph. Reuse one freshly
# built inventory, keeping the full WIR suite and only partitioning examples.
# Match complete commands, never parse or absorb arbitrary shell policies.
# If lower's complete suite is also selected, give it the same native graph
# while retaining its admitted policy. The pure WIR suite is a semantic subset
# of the native suite: native only adds execution helpers, never different
# checker/admission logic. Pure-only routes retain their portability check;
# a mixed edit does not rebuild that graph to repeat the same assertions.
native_wir_command='cargo nextest run -p witchy-wir --features witchy-wir/native'
pure_wir_command='cargo nextest run -p witchy-wir --test unit --test structural_contracts --test rfc0162_mutation_evidence --test rfc0165_route_gate --no-default-features'
lower_command='./scripts/nextest-admitted.sh witchy-lower -- cargo nextest run -p witchy-lower'
memory_probe="cargo nextest run -p witchy-wir --no-default-features --test unit -E 'test(constant_prefix_publication_rejects_gaps_and_missing_children)'"
native_examples_command="./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'"
native_examples_partition=full
if [ "${WITCHY_EDIT_EXAMPLES:-slice}" != full ]; then
    native_examples_partition="$edit_example_slice"
    native_examples_command="./scripts/nextest-admitted.sh witchy -- cargo nextest run --partition $edit_example_slice -E 'test(/^example_tests::/)'"
fi
have_native_wir=0
have_native_examples=0
native_lower_package=""
for c in ${cmds[0]+"${cmds[@]}"}; do
    [ "$c" != "$native_wir_command" ] || have_native_wir=1
    [ "$c" != "$native_examples_command" ] || have_native_examples=1
    [ "$c" != "$lower_command" ] || native_lower_package=" witchy-lower"
done
if [ "$edit_loop" -eq 1 ] && [ "$have_native_wir" -eq 1 ] && [ "$have_native_examples" -eq 1 ]; then
    native_cmds=()
    native_inserted=0
    for c in "${cmds[@]}"; do
        if [ "$c" = "$native_wir_command" ] || [ "$c" = "$native_examples_command" ] \
            || [ "$c" = "$pure_wir_command" ] || [ "$c" = "$memory_probe" ] \
            || [ "$c" = "$lower_command" ]; then
            if [ "$native_inserted" -eq 0 ]; then
                native_cmds+=("bash ./scripts/native-compiler-examples.sh $native_examples_partition$native_lower_package")
                native_inserted=1
            fi
        else
            native_cmds+=("$c")
        fi
    done
    cmds=("${native_cmds[@]}")
fi

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
